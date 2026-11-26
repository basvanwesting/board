//! `board migrate`: a project's `chores.md` and `backlog.md`, the storage
//! before item files, converted to one file per item, and the item files
//! brought up to date (a `group:` on each, no `order:`), in one write.
//!
//! A line is taken apart by the readers of the old list code
//! ([`crate::list`]), so it reads exactly as the old tool read it. The
//! conversion is all or nothing: a line it cannot place is named and
//! nothing is written, and the owner fixes the list by hand (the boards it
//! was made for have none). Rejected: giving a colliding id a new number:
//! an id is how the owner and the briefs refer to an item.

use std::collections::BTreeSet;
use std::fmt;

use crate::brief::Stamp;
use crate::id::{ItemId, ListId, ListKind};
use crate::item::{self, Body, DEFAULT_GROUP, Item};
use crate::list::{Entry, Line, is_continuation, parse_line, split_check, split_reference};
use crate::project::{OldList, Project};
use crate::raw;

/// How many items a migration made from list files, per kind, and how
/// many item files it brought up to date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Migrated {
    /// Whether there were list files to convert.
    pub lists: bool,
    pub chores: usize,
    pub backlog: usize,
    pub updated: usize,
}

impl Migrated {
    /// Nothing was there to do: the write makes no commit.
    pub fn is_nothing(&self) -> bool {
        !self.lists && self.updated == 0
    }
}

impl fmt::Display for Migrated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let updated = format!(
            "updated {}",
            plural(self.updated, "item file", "item files")
        );
        if !self.lists {
            return f.write_str(&updated);
        }
        write!(
            f,
            "migrated {}, {}",
            plural(self.chores, "chore", "chores"),
            plural(self.backlog, "backlog item", "backlog items")
        )?;
        if self.updated > 0 {
            write!(f, "; {updated}")?;
        }
        Ok(())
    }
}

/// A line the conversion cannot place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unplaced {
    pub(crate) path: String,
    /// 1-based.
    pub(crate) line: usize,
    pub(crate) why: &'static str,
}

impl fmt::Display for Unplaced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.why)
    }
}

/// One item as its old line and continuation lines give it.
struct Old {
    id: u32,
    line: usize,
    group: Option<Line>,
    title: Line,
    check: Option<Line>,
    from: Option<ItemId>,
    body: Vec<u8>,
}

/// Converts every list file of the project into item files and marks the
/// lists deleted, and brings the item files there were up to date
/// ([`update`]); nothing changes when a line is unplaced. In a project that
/// has item files already, the list's items join them: a list that came
/// back from a machine on the old binary is folded in.
pub(crate) fn migrate(project: &mut Project, now: Stamp) -> Result<Migrated, Vec<Unplaced>> {
    let mut read = Vec::new();
    let mut unplaced = Vec::new();
    for list in &project.old_lists {
        match read_list(list) {
            Ok(items) => read.push((list.kind, list.path.clone(), items)),
            Err(found) => unplaced.extend(found),
        }
    }
    for (kind, path, items) in &read {
        for old in items {
            if project.list(*kind).get(old.id).is_some() {
                unplaced.push(Unplaced {
                    path: path.clone(),
                    line: old.line,
                    why: "its id has an item file already",
                });
            }
        }
    }
    if !unplaced.is_empty() {
        unplaced.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        return Err(unplaced);
    }
    let mut migrated = Migrated {
        lists: !project.old_lists.is_empty(),
        updated: update(project),
        ..Migrated::default()
    };
    let default = Line::new(DEFAULT_GROUP).expect("a line");
    for (kind, _, items) in read {
        for old in items {
            let id = ListId::of(kind, old.id).expect("a parsed id is past 0");
            let item = Item::new(item::New {
                id: ItemId::from(id),
                // A chore the heading it stood under; the backlog had none.
                group: old.group.as_ref().unwrap_or(&default),
                check: old.check.as_ref(),
                after: None,
                from: old.from,
                created: now,
                title: &old.title,
                body: &Body::new(&old.body),
            });
            project.list_mut(kind).insert(old.id, item);
            project.touched(id);
            match kind {
                ListKind::Chores => migrated.chores += 1,
                ListKind::Backlog => migrated.backlog += 1,
            }
        }
    }
    for list in &mut project.old_lists {
        list.converted = true;
    }
    Ok(migrated)
}

/// Brings the item files up to date; gives how many it changed. An item
/// without a `group:` gets `group: Default`, right after `id:` (an empty
/// one is filled where it stands), and every header `order:` line goes
/// (owner, 2026-11-26: items have no order). The
/// rest of each file stays byte for byte, the body too, whatever lines it
/// has; a file that needs neither is not written.
///
/// Stripped although nothing reads the key any more: migrate writes these
/// files anyway, and a dead key in every file would have to be explained to
/// each planner who edits one by hand.
fn update(project: &mut Project) -> usize {
    let default = Line::new(DEFAULT_GROUP).expect("a line");
    let mut updated = 0;
    for kind in ListKind::ALL {
        let stale: Vec<u32> = project
            .list(kind)
            .numbers()
            .filter(|number| {
                let item = project.list(kind).get(*number).expect("listed");
                item.group().is_none() || item.get("order").is_some()
            })
            .collect();
        for number in stale {
            let id = ListId::of(kind, number).expect("an item's id is past 0");
            let item = project.item_mut(id).expect("listed");
            if item.group().is_none() {
                item.set_group(&default);
            }
            item.remove("order");
            updated += 1;
        }
    }
    updated
}

fn read_list(list: &OldList) -> Result<Vec<Old>, Vec<Unplaced>> {
    let kind = list.kind;
    let mut unplaced = Vec::new();
    let mut refuse = |line: usize, why| {
        unplaced.push(Unplaced {
            path: list.path.clone(),
            line,
            why,
        });
    };
    let mut items: Vec<Old> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut group: Option<Line> = None;
    // Whether the line above is an item or its continuation: only then is an
    // indented line its body, as the old tool's reading of an item's end.
    let mut open = false;
    for (index, raw_line) in raw::lines(&list.bytes).enumerate() {
        let n = index + 1;
        let content = raw::content(raw_line);
        if raw_line.ends_with(b"\r\n") {
            refuse(n, "a CR line end; convert the file to LF line ends");
            open = false;
            continue;
        }
        let entry = parse_line(kind, content);
        match entry {
            Entry::Heading(name) => {
                open = false;
                if kind == ListKind::Backlog {
                    refuse(n, "a heading in backlog.md, which has no groups");
                    continue;
                }
                match str::from_utf8(name)
                    .ok()
                    .and_then(|name| Line::new(name).ok())
                {
                    Some(name) => group = Some(name),
                    None => refuse(n, "a heading that is not UTF-8"),
                }
            }
            Entry::Item { id: None, .. } => {
                open = false;
                refuse(n, "an item without an id");
            }
            Entry::Item { id: Some(id), text } => {
                open = false;
                if kind == ListKind::Chores && group.is_none() {
                    refuse(n, "a chore above the first `## ` heading, so in no group");
                    continue;
                }
                if !seen.insert(id) {
                    refuse(n, "an id an item above has too");
                    continue;
                }
                match item_of(kind, text) {
                    Ok((title, check, from)) => {
                        items.push(Old {
                            id,
                            line: n,
                            group: group.clone(),
                            title,
                            check,
                            from,
                            body: Vec::new(),
                        });
                        open = true;
                    }
                    Err(why) => refuse(n, why),
                }
            }
            Entry::Text(content) if raw::is_blank(content) => open = false,
            Entry::Text(content) if open && is_continuation(content) => {
                let body = &mut items.last_mut().expect("open after an item").body;
                body.extend_from_slice(unindented(content));
                body.push(b'\n');
            }
            Entry::Text(_) => refuse(
                n,
                "free text: neither an item, a heading, a body line right below an item, \
                 nor blank",
            ),
        }
    }
    if unplaced.is_empty() {
        Ok(items)
    } else {
        Err(unplaced)
    }
}

/// A body line without the body's indentation: the two spaces the old tool
/// wrote, or the one character (a space, a tab) of a line typed by hand.
/// The rest is kept byte for byte, so nested indentation stays. A line of
/// one blank before text that starts with blanks reads as two blanks before
/// one less: the two cannot be told apart, and the tool wrote the one.
fn unindented(content: &[u8]) -> &[u8] {
    content.strip_prefix(b"  ").unwrap_or_else(|| &content[1..])
}

/// The title, check and reference of an item's text (after the `- `, its id
/// included), as the old `retitle` took it apart.
fn item_of(
    kind: ListKind,
    text: &[u8],
) -> Result<(Line, Option<Line>, Option<ItemId>), &'static str> {
    // After the id word and its space; the id is the line's (`parse_line`).
    let rest = text
        .iter()
        .position(|b| *b == b' ')
        .map_or(&b""[..], |at| &text[at + 1..]);
    let (head, reference) = split_reference(kind, rest);
    let from = reference_id(reference);
    let (title, check) = match kind {
        ListKind::Chores => match split_check(head) {
            Some((at, check)) => (without_join(&head[..at]), Some(check)),
            None => (head, None),
        },
        ListKind::Backlog => (head, None),
    };
    let line = |bytes: &[u8], what| {
        str::from_utf8(bytes)
            .ok()
            .and_then(|text| Line::new(text).ok())
            .ok_or(what)
    };
    let title = line(title, "an item without a title, or one that is not UTF-8")?;
    let check = check
        .map(|check| line(check, "a check that is not UTF-8"))
        .transpose()?;
    Ok((title, check, from))
}

/// The text before a chore's `Check:` without the join the old writer put
/// there: `. `, or one space after a text that ends in `.`, `!` or `?`.
///
/// A text that ended in `.` and one that did not are written alike
/// (`Do it. Check: …` either way), so the full stop is taken off both: the
/// new writer puts the same join back, and `ls` shows the line as before.
/// A full stop after one of the three is the text's own (`Wait...`): the
/// writer added none there.
fn without_join(before_check: &[u8]) -> &[u8] {
    let text = before_check.trim_ascii_end();
    match text.strip_suffix(b".") {
        Some(rest) if !ends_sentence(rest) => rest,
        Some(_) | None => text,
    }
}

fn ends_sentence(text: &[u8]) -> bool {
    text.ends_with(b".") || text.ends_with(b"!") || text.ends_with(b"?")
}

/// The id of a reference as `split_reference` gives it (`  (126)`,
/// `  (from b41)`), whatever its kind.
fn reference_id(reference: &[u8]) -> Option<ItemId> {
    let inside = reference
        .trim_ascii_start()
        .strip_prefix(b"(")?
        .strip_suffix(b")")?;
    let id = inside.strip_prefix(b"from ").unwrap_or(inside);
    str::from_utf8(id).ok()?.parse().ok()
}
