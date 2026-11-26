//! A chore's or backlog item's file, and a kind's items as the board lists
//! them.
//!
//! ```text
//! <project>/chores/c12.md            <project>/backlog/b41.md
//!
//! id: c12
//! group: Deploy                      required, `Default` when none is given
//! check: <one line>                  a chore's, optional
//! from: 126                          optional: the item it came from
//! created: 2026-11-25T14:08:18Z
//!
//! # <title>
//!
//! <body: Markdown, kept byte for byte>
//! ```
//!
//! One file per item, as a brief has, decided by the owner (2026-11-25),
//! reversing the list files of 2026-11-24: a body is Markdown (an SQL check
//! can be copied whole, a list is a list), git merges and shows an item on
//! its own, and the group is a key on the item, as metadata. The file
//! is named by its id only: a slug would rename the file at every retitle,
//! and the merge of a rename is not worth it (owner).
//!
//! A backlog item has a group as a chore has (owner, 2026-11-26: "Make it
//! required, just add "Default" to start with and then the logic is
//! easier"): one rule for both lists, and `Default` is where an item lands
//! that was given none. Neither kind has an order key any more (same day:
//! "these items don't have order"); a file that still has one keeps it as
//! an unknown key until `migrate` strips it.

use std::collections::BTreeMap;

use crate::brief::Stamp;
use crate::header::Header;
use crate::id::{ItemId, ListKind};
use crate::list::{FindError, Line};
use crate::raw;

/// The group an item gets when none is given (`add` without `--group`, an
/// item `migrate` finds without one): an inbox the planner sorts (owner,
/// 2026-11-26: "just add "Default" to start with and then the logic is
/// easier"), for chores and backlog items alike.
pub const DEFAULT_GROUP: &str = "Default";

/// A chore's or backlog item's file, lossless: written back unchanged it
/// gives the bytes read. The parts a command changes (a key, the title, the
/// body) are rewritten; the rest keeps its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    header: Header,
    /// The blank lines between the header and the title, raw.
    gap: Vec<u8>,
    /// The `# ` line, raw; `None` when the file has none.
    title: Option<Vec<u8>>,
    /// The one blank line between the title and the body, raw; empty when
    /// there is no body.
    separator: Vec<u8>,
    /// Everything after: the body, kept byte for byte.
    body: Vec<u8>,
}

impl Item {
    /// Never fails: what a file lacks is lint's to report.
    pub fn parse(bytes: &[u8]) -> Item {
        let (header, rest) = Header::split(bytes);
        let lines: Vec<&[u8]> = raw::lines(rest).collect();
        let blank = |line: &[u8]| raw::is_blank(raw::content(line));
        let gap = lines.iter().take_while(|line| blank(line)).count();
        let (title, after) = match lines.get(gap) {
            Some(line) if raw::content(line).starts_with(b"# ") => (Some(line.to_vec()), gap + 1),
            _ => (None, gap),
        };
        // One blank line sets the body off; a second is the body's own.
        let separator = match lines.get(after) {
            Some(line) if title.is_some() && blank(line) => 1,
            _ => 0,
        };
        let join = |part: &[&[u8]]| part.concat();
        Item {
            header,
            gap: join(&lines[..gap]),
            title,
            separator: join(&lines[after..after + separator]),
            body: join(&lines[after + separator..]),
        }
    }

    /// A new item's file: its keys in the order the board writes them, the
    /// title, and the body when there is one.
    pub(crate) fn new(new: New<'_>) -> Item {
        let mut header = Header::default();
        header.set("id", &new.id.to_string());
        header.set("group", new.group.as_str());
        if let Some(check) = new.check {
            header.set("check", check.as_str());
        }
        if let Some(after) = new.after {
            header.set(crate::after::KEY, &after.to_string());
        }
        if let Some(from) = new.from {
            header.set("from", &from.to_string());
        }
        header.set("created", &new.created.to_string());
        let mut item = Item {
            header,
            gap: b"\n".to_vec(),
            title: None,
            separator: Vec::new(),
            body: Vec::new(),
        };
        item.set_title(new.title);
        item.set_body(new.body);
        item
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.header.to_bytes();
        // A header the tool writes ends its last line; one read without a
        // final terminator is followed by nothing else.
        bytes.extend_from_slice(&self.gap);
        bytes.extend_from_slice(self.title.as_deref().unwrap_or_default());
        bytes.extend_from_slice(&self.separator);
        bytes.extend_from_slice(&self.body);
        bytes
    }

    /// The trimmed value of a key; `None` without it.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.header.get(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &[u8]> {
        self.header.keys()
    }

    /// The title, after `# `, trimmed; `None` for a file without a title
    /// line.
    pub fn title(&self) -> Option<&[u8]> {
        let line = raw::content(self.title.as_deref()?);
        Some(line.strip_prefix(b"# ").unwrap_or(line).trim_ascii())
    }

    /// The body as the file has it; empty for none.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The item's group; `None` without one, and an empty `group:` is
    /// none: it lists under the empty name, as a missing line does.
    pub fn group(&self) -> Option<&[u8]> {
        self.get("group").filter(|group| !group.is_empty())
    }

    /// A chore's check; an empty `check:` is none.
    pub fn check(&self) -> Option<&[u8]> {
        self.get("check").filter(|check| !check.is_empty())
    }

    /// The `from:` reference, as written.
    pub fn from(&self) -> Option<&[u8]> {
        self.get("from").filter(|from| !from.is_empty())
    }

    pub(crate) fn set(&mut self, key: &str, value: &str) {
        self.terminate_header();
        self.header.set(key, value);
    }

    pub(crate) fn remove(&mut self, key: &str) {
        self.header.remove(key);
    }

    /// What the item waits on (`after:`), where a new item has it: after
    /// `id`, `group` and `check`; `None` removes the line.
    pub(crate) fn set_wait(&mut self, target: Option<ItemId>) {
        self.terminate_header();
        match target {
            Some(target) => self.header.set_after(
                crate::after::KEY,
                &target.to_string(),
                &["id", "group", "check"],
            ),
            None => self.header.remove(crate::after::KEY),
        }
    }

    /// Puts the item in `group`: its `group:` line rewritten in place, or,
    /// on an item without one, added right after `id:`, where a new item
    /// has it, rather than at the end of the header as a new key lands.
    pub(crate) fn set_group(&mut self, group: &Line) {
        self.terminate_header();
        self.header.set_after("group", group.as_str(), &["id"]);
    }

    pub(crate) fn set_title(&mut self, title: &Line) {
        let terminator = self.terminator();
        self.terminate_header();
        if self.gap.is_empty() {
            self.gap = terminator.to_vec();
        }
        self.title = Some([b"# ", title.as_str().as_bytes(), terminator].concat());
        if !self.body.is_empty() && self.separator.is_empty() {
            self.separator = terminator.to_vec();
        }
    }

    /// Puts `body` in place of the body; an empty one removes it, and the
    /// file then ends at its title.
    pub(crate) fn set_body(&mut self, body: &Body) {
        self.body = body.0.clone();
        self.separator = if self.body.is_empty() {
            Vec::new()
        } else {
            self.terminator().to_vec()
        };
        self.terminate_title();
    }

    /// Adds `body` after the body there is, set off by one blank line.
    pub(crate) fn append_body(&mut self, body: &Body) {
        if self.body.is_empty() {
            self.set_body(body);
            return;
        }
        let terminator = self.terminator();
        if !self.body.ends_with(b"\n") {
            self.body.extend_from_slice(terminator);
        }
        // One blank line between the two: the end of the old body's own
        // blank lines is the new one's start.
        while self.body.ends_with(b"\n\n") || self.body.ends_with(b"\r\n\r\n") {
            let cut = if self.body.ends_with(b"\r\n") { 2 } else { 1 };
            self.body.truncate(self.body.len() - cut);
        }
        self.body.extend_from_slice(terminator);
        self.body.extend_from_slice(&body.0);
    }

    /// The terminator a line the tool adds gets: the one of the file's
    /// first line.
    fn terminator(&self) -> &'static [u8] {
        raw::terminator_of(self.header.first_line().or(self.title.as_deref()))
    }

    fn terminate_header(&mut self) {
        let terminator = self.terminator();
        if let Some(last) = self.header.last_line_mut()
            && !last.ends_with(b"\n")
        {
            last.extend_from_slice(terminator);
        }
    }

    fn terminate_title(&mut self) {
        let terminator = self.terminator();
        if let Some(title) = &mut self.title
            && !title.ends_with(b"\n")
        {
            title.extend_from_slice(terminator);
        }
    }

    /// The item as `ls` prints it and the TUI shows its row: `c12 <title>.
    /// Check: <check>  (126)` (without the check part when it has none),
    /// `b41 <title>  (from 126)`. The line agents read did not change with
    /// the files: what the list files had on a line is what this gives.
    pub fn line(&self, kind: ListKind, number: u32) -> Vec<u8> {
        let title = self.title().unwrap_or_default();
        let mut line = format!("{}{number} ", kind.prefix()).into_bytes();
        line.extend_from_slice(title);
        if kind == ListKind::Chores
            && let Some(check) = self.check()
        {
            line.extend_from_slice(join(title));
            line.extend_from_slice(b"Check: ");
            line.extend_from_slice(check);
        }
        if let Some(from) = self.from() {
            line.extend_from_slice(match kind {
                ListKind::Chores => b"  (",
                ListKind::Backlog => b"  (from ",
            });
            line.extend_from_slice(from);
            line.push(b')');
        }
        line
    }
}

/// What goes between a chore's title and its `Check:`: a title that ends
/// its own sentence is not given a second full stop.
fn join(title: &[u8]) -> &'static [u8] {
    if title.is_empty() {
        b""
    } else if title.ends_with(b".") || title.ends_with(b"!") || title.ends_with(b"?") {
        b" "
    } else {
        b". "
    }
}

/// A new item, as `add` gives it.
pub(crate) struct New<'a> {
    pub id: ItemId,
    pub group: &'a Line,
    pub check: Option<&'a Line>,
    pub after: Option<ItemId>,
    pub from: Option<ItemId>,
    pub created: Stamp,
    pub title: &'a Line,
    pub body: &'a Body,
}

/// A chore's or backlog item's body as the CLI writes it (`add … --body`,
/// `amend`): Markdown, kept byte for byte, blank lines, code fences and
/// `## ` lines included. It ends with a line break: stdin without a final
/// one gets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body(Vec<u8>);

impl Body {
    pub fn new(text: &[u8]) -> Body {
        if raw::is_blank(text) {
            return Body(Vec::new());
        }
        let mut text = text.to_vec();
        if !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        Body(text)
    }

    /// Whether it has no text: whitespace alone is none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A kind's items, by id: what the files under `chores/` or `backlog/` hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Items {
    kind: ListKind,
    files: BTreeMap<u32, Item>,
}

/// An item as the board lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listed<'a> {
    pub number: u32,
    pub item: &'a Item,
}

impl Items {
    pub fn new(kind: ListKind) -> Items {
        Items {
            kind,
            files: BTreeMap::new(),
        }
    }

    pub fn kind(&self) -> ListKind {
        self.kind
    }

    pub fn get(&self, number: u32) -> Option<&Item> {
        self.files.get(&number)
    }

    pub(crate) fn get_mut(&mut self, number: u32) -> Option<&mut Item> {
        self.files.get_mut(&number)
    }

    pub(crate) fn insert(&mut self, number: u32, item: Item) {
        self.files.insert(number, item);
    }

    pub(crate) fn remove(&mut self, number: u32) -> Option<Item> {
        self.files.remove(&number)
    }

    pub fn numbers(&self) -> impl Iterator<Item = u32> + '_ {
        self.files.keys().copied()
    }

    /// The item with this id, or why there is none: an id is a file name,
    /// so one names at most one item.
    pub fn find(&self, number: u32) -> Result<&Item, FindError> {
        self.get(number).ok_or(FindError::Missing)
    }

    /// The group an item is listed under: its `group:`, the empty name
    /// without one (lint reports it, and no command refuses the item: it
    /// is seen and listed, so a write beside it lands where it is seen).
    fn group_of(item: &Item) -> &[u8] {
        item.group().unwrap_or_default()
    }

    /// The items in the board's order: by group (by name, byte order), then
    /// by id, numerically, which is the order they were made in. A group
    /// exists while an item names it.
    ///
    /// No order of their own (owner, 2026-11-26, reversing the `order` key
    /// of 2026-11-25): "these items don't have order". The grouping is
    /// what places an item.
    pub fn listed(&self) -> Vec<Listed<'_>> {
        let mut listed: Vec<Listed<'_>> = self
            .files
            .iter()
            .map(|(number, item)| Listed {
                number: *number,
                item,
            })
            .collect();
        // Stable, and the map gives them by id.
        listed.sort_by(|a, b| Self::group_of(a.item).cmp(Self::group_of(b.item)));
        listed
    }

    /// The groups and items as `ls` lists them: a `## group` row above each
    /// group's items, for both kinds.
    pub fn rows(&self) -> Vec<Row<'_>> {
        let mut rows = Vec::new();
        let mut group: Option<&[u8]> = None;
        for listed in self.listed() {
            let this = Self::group_of(listed.item);
            if group != Some(this) {
                rows.push(Row::Group(this));
                group = Some(this);
            }
            rows.push(Row::Item(listed));
        }
        rows
    }
}

/// A row of a kind's listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row<'a> {
    Group(&'a [u8]),
    Item(Listed<'a>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ChoreId;

    fn line(text: &str) -> Line {
        Line::new(text).unwrap()
    }

    #[test]
    fn a_file_reads_back_byte_for_byte() {
        for text in [
            "id: c1\ngroup: Home\ncreated: x\n\n# Title\n",
            "id: b1\ngroup: g\n\n# T\n\nbody\n\n```sql\nselect 1;\n```\n## not a section\n",
            "id: b1\r\ngroup: g\r\n\r\n# T\r\n\r\nbody\r\n",
            "no header at all\n",
            "id: b2\n# no blank line\n",
            "id: b3\ngroup: g\n\n# T",
            "",
        ] {
            assert_eq!(
                Item::parse(text.as_bytes()).to_bytes(),
                text.as_bytes(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn title_body_and_keys_read_as_written() {
        let item = Item::parse(b"id: c1\ngroup: Home\ncheck:\nfrom: 126\n\n#  Do it \n\n\nkept\n");
        assert_eq!(item.title(), Some(&b"Do it"[..]));
        // One blank line sets the body off; a second is the body's.
        assert_eq!(item.body(), b"\nkept\n");
        assert_eq!(item.group(), Some(&b"Home"[..]));
        assert_eq!(item.check(), None);
        assert_eq!(item.from(), Some(&b"126"[..]));
        assert_eq!(Item::parse(b"id: b1\n\nno title\n").title(), None);
        assert_eq!(Item::parse(b"id: b1\n").group(), None);
    }

    #[test]
    fn the_line_is_what_ls_prints() {
        let chore = Item::parse(b"id: c12\ngroup: g\ncheck: x\nfrom: 126\n\n# Do it\n");
        assert_eq!(
            chore.line(ListKind::Chores, 12),
            b"c12 Do it. Check: x  (126)"
        );
        let ends = Item::parse(b"id: c3\ngroup: g\ncheck: x\n\n# Done?\n");
        assert_eq!(ends.line(ListKind::Chores, 3), b"c3 Done? Check: x");
        let plain = Item::parse(b"id: c4\ngroup: g\n\n# Simple\n");
        assert_eq!(plain.line(ListKind::Chores, 4), b"c4 Simple");
        let backlog = Item::parse(b"id: b41\ngroup: g\nfrom: c3\n\n# Later\n\nwhy\n\n## more\n");
        assert_eq!(backlog.line(ListKind::Backlog, 41), b"b41 Later  (from c3)");
    }

    #[test]
    fn a_body_is_kept_as_given_and_gets_a_final_line_break() {
        assert_eq!(Body::new(b"a\n\n  b").0, b"a\n\n  b\n");
        assert_eq!(Body::new(b"a\n").0, b"a\n");
        assert!(Body::new(b" \n\t\n").is_empty());
    }

    #[test]
    fn a_new_item_has_its_keys_in_order_and_ends_at_its_title_without_a_body() {
        let created = "2026-11-25T14:08:18Z".parse().unwrap();
        let item = Item::new(New {
            id: ItemId::Chore(ChoreId::new(12).unwrap()),
            group: &line("Deploy"),
            check: Some(&line("select 1")),
            after: None,
            from: Some(ItemId::Brief(crate::id::BriefId::new(126))),
            created,
            title: &line("Check the states"),
            body: &Body::new(b""),
        });
        assert_eq!(
            item.to_bytes(),
            b"id: c12\ngroup: Deploy\ncheck: select 1\nfrom: 126\n\
              created: 2026-11-25T14:08:18Z\n\n# Check the states\n"
        );
    }

    #[test]
    fn edits_keep_a_file_without_final_line_breaks_well_formed() {
        let mut item = Item::parse(b"id: b1\ngroup: g\n\n# T");
        item.append_body(&Body::new(b"x"));
        assert_eq!(item.to_bytes(), b"id: b1\ngroup: g\n\n# T\n\nx\n");
        let mut item = Item::parse(b"id: b1\ngroup: g");
        item.set("check", "2");
        item.set_title(&line("New"));
        assert_eq!(item.to_bytes(), b"id: b1\ngroup: g\ncheck: 2\n\n# New\n");
    }

    // ---- the listing ----

    fn items(kind: ListKind, files: &[(u32, &str)]) -> Items {
        let mut items = Items::new(kind);
        for (number, text) in files {
            items.insert(*number, Item::parse(text.as_bytes()));
        }
        items
    }

    /// By group name, then by id as a number: `c9` before `c10`, whatever
    /// the order key of an old file says, for both kinds.
    #[test]
    fn items_list_by_group_then_by_id() {
        for (kind, p) in [(ListKind::Chores, 'c'), (ListKind::Backlog, 'b')] {
            let files = [
                (
                    10,
                    format!("id: {p}10\ngroup: Home\norder: 1000\n\n# ten\n"),
                ),
                (9, format!("id: {p}9\ngroup: Home\norder: 2000\n\n# nine\n")),
                (2, format!("id: {p}2\ngroup: Away\n\n# two\n")),
                (1, format!("id: {p}1\n\n# none\n")),
            ];
            let files: Vec<(u32, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
            let items = items(kind, &files);
            let listed: Vec<u32> = items.listed().iter().map(|l| l.number).collect();
            assert_eq!(listed, [1, 2, 9, 10], "{kind:?}");
            let groups: Vec<&[u8]> = items
                .rows()
                .into_iter()
                .filter_map(|row| match row {
                    Row::Group(name) => Some(name),
                    Row::Item(_) => None,
                })
                .collect();
            assert_eq!(groups, [&b""[..], b"Away", b"Home"], "{kind:?}");
        }
    }

    #[test]
    fn set_group_rewrites_its_line_or_adds_one_right_after_id() {
        let mut item = Item::parse(b"id: b1\nfrom: 3\ncreated: x\n\n# T\n");
        item.set_group(&line("Later"));
        assert_eq!(
            item.to_bytes(),
            b"id: b1\ngroup: Later\nfrom: 3\ncreated: x\n\n# T\n"
        );
        item.set_group(&line("Now"));
        assert_eq!(
            item.to_bytes(),
            b"id: b1\ngroup: Now\nfrom: 3\ncreated: x\n\n# T\n"
        );
        let mut item = Item::parse(b"id: c1\r\ncreated: x\r\n\r\n# T\r\n");
        item.set_group(&line("G"));
        assert_eq!(
            item.to_bytes(),
            b"id: c1\r\ngroup: G\r\ncreated: x\r\n\r\n# T\r\n"
        );
        let mut open = Item::parse(b"id: c1");
        open.set_group(&line("G"));
        assert_eq!(open.to_bytes(), b"id: c1\ngroup: G\n");
    }
}
