//! A chore's or backlog item's file, and a kind's items as the board lists
//! them.
//!
//! ```text
//! <project>/chores/c12.md            <project>/backlog/b41.md
//!
//! id: c12
//! group: Deploy                      a chore's, required
//! order: 3000                        the place in its group, or the backlog
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
//! its own, and order and group are keys on the item, as metadata. The file
//! is named by its id only: a slug would rename the file at every retitle,
//! and the merge of a rename is not worth it (owner).

use std::collections::BTreeMap;

use crate::brief::Stamp;
use crate::header::Header;
use crate::id::{ItemId, ListKind};
use crate::list::{FindError, Line, Move, Side};
use crate::raw;

/// The gap between two neighbours' orders that a new item, or an item put
/// first or last, keeps: room for many moves halfway between before a
/// renumbering.
pub(crate) const STEP: i64 = 1000;

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

/// Why an item's `order:` is no place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadOrder {
    Missing,
    /// Not an integer, as found.
    NotANumber(String),
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
        if let Some(group) = new.group {
            header.set("group", group.as_str());
        }
        header.set("order", &new.order.to_string());
        if let Some(check) = new.check {
            header.set("check", check.as_str());
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

    pub fn order(&self) -> Result<i64, BadOrder> {
        let value = self.get("order").ok_or(BadOrder::Missing)?;
        str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| BadOrder::NotANumber(String::from_utf8_lossy(value).into_owned()))
    }

    /// A chore's group; `None` without one.
    pub fn group(&self) -> Option<&[u8]> {
        self.get("group")
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

    /// The item as `show` prints it: its line, and when it has a body a
    /// blank line and the body as the file has it. The TUI's preview and
    /// `yy` show the same, from this code.
    pub fn shown(&self, kind: ListKind, number: u32) -> Vec<u8> {
        let mut shown = self.line(kind, number);
        shown.push(b'\n');
        if !self.body.is_empty() {
            shown.push(b'\n');
            shown.extend_from_slice(&self.body);
            if !self.body.ends_with(b"\n") {
                shown.push(b'\n');
            }
        }
        shown
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
    pub group: Option<&'a Line>,
    pub order: i64,
    pub check: Option<&'a Line>,
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

    /// The group a chore is listed under: its `group:`, the empty name
    /// without one (lint reports it). A backlog item has none.
    fn group_of<'a>(&self, item: &'a Item) -> &'a [u8] {
        match self.kind {
            ListKind::Chores => item.group().unwrap_or_default(),
            ListKind::Backlog => b"",
        }
    }

    /// The place an item is listed at: an order that is no number goes
    /// last (lint reports it), ties by id.
    fn place(item: &Item) -> i64 {
        item.order().unwrap_or(i64::MAX)
    }

    /// The items in the board's order: by group (chores, by name, byte
    /// order), then by `order`, ties by id. A group exists while a chore
    /// names it.
    pub fn listed(&self) -> Vec<Listed<'_>> {
        let mut listed: Vec<Listed<'_>> = self
            .files
            .iter()
            .map(|(number, item)| Listed {
                number: *number,
                item,
            })
            .collect();
        listed.sort_by(|a, b| {
            (self.group_of(a.item), Self::place(a.item), a.number).cmp(&(
                self.group_of(b.item),
                Self::place(b.item),
                b.number,
            ))
        });
        listed
    }

    /// The groups and items as `ls` lists them: a `## group` row above each
    /// group's chores; the backlog has no groups.
    pub fn rows(&self) -> Vec<Row<'_>> {
        let mut rows = Vec::new();
        let mut group: Option<&[u8]> = None;
        for listed in self.listed() {
            if self.kind == ListKind::Chores {
                let this = self.group_of(listed.item);
                if group != Some(this) {
                    rows.push(Row::Group(this));
                    group = Some(this);
                }
            }
            rows.push(Row::Item(listed));
        }
        rows
    }

    /// The order a new item gets: past the last of its group (a chore) or
    /// of the backlog, by [`STEP`].
    pub(crate) fn next_order(&self, group: Option<&[u8]>) -> i64 {
        self.files
            .values()
            .filter(|item| self.kind == ListKind::Backlog || item.group() == group)
            .filter_map(|item| item.order().ok())
            .max()
            .map_or(STEP, |last| last.saturating_add(STEP))
    }

    /// Moves an item next to another of its kind: gives the ids whose files
    /// changed, none when the item already stands there.
    ///
    /// Only the moved item's file is written: its `order` goes halfway
    /// between its new neighbours in their group, [`STEP`] before the first
    /// or after the last, and a chore takes the target's group. When the
    /// neighbours leave no integer between them, the group (or the backlog)
    /// is renumbered by [`STEP`] in the same change. Rejected: an index file
    /// of the order, which every move of any item would write.
    pub(crate) fn move_item(&mut self, wanted: &Move) -> Result<Vec<u32>, MoveError> {
        let (item, target) = (wanted.item().get(), wanted.target().get());
        self.find(item).map_err(MoveError::Item)?;
        self.find(target).map_err(MoveError::Target)?;
        let before: Vec<u32> = self.listed().iter().map(|listed| listed.number).collect();
        let mut order: Vec<u32> = before.iter().copied().filter(|n| *n != item).collect();
        let at = order
            .iter()
            .position(|n| *n == target)
            .expect("the target is listed");
        let at = match wanted.side() {
            Side::Before => at,
            Side::After => at + 1,
        };
        order.insert(at, item);
        let group = self.group_of(&self.files[&target]).to_vec();
        let same_group = self.group_of(&self.files[&item]) == group.as_slice();
        if order == before && same_group {
            return Ok(Vec::new());
        }
        let in_group = |n: &u32| self.group_of(&self.files[n]) == group.as_slice() || *n == item;
        let group_order: Vec<u32> = order.iter().copied().filter(in_group).collect();
        let at = group_order
            .iter()
            .position(|n| *n == item)
            .expect("the item is in its new group");
        let order_of = |n: Option<&u32>| n.map(|n| self.files[n].order());
        let place = match (
            order_of(at.checked_sub(1).map(|above| &group_order[above])),
            order_of(group_order.get(at + 1)),
        ) {
            (Some(Ok(above)), Some(Ok(below))) => {
                let mid = above + (below - above) / 2;
                (above < mid && mid < below).then_some(mid)
            }
            (Some(Ok(above)), None) => above.checked_add(STEP),
            (None, Some(Ok(below))) => below.checked_sub(STEP),
            // Alone in its group, as the target is in it: not reached.
            (None, None) => Some(STEP),
            // A neighbour without a number: the renumbering gives it one.
            (Some(Err(_)), _) | (_, Some(Err(_))) => None,
        };
        let moved = self.files.get_mut(&item).expect("found above");
        if self.kind == ListKind::Chores && !same_group {
            moved.set("group", &String::from_utf8_lossy(&group));
        }
        match place {
            Some(place) => {
                moved.set("order", &place.to_string());
                Ok(vec![item])
            }
            None => {
                let mut changed = vec![item];
                for (rank, n) in group_order.iter().enumerate() {
                    let place = STEP * (i64::try_from(rank).unwrap_or(i64::MAX / STEP) + 1);
                    let file = self.files.get_mut(n).expect("listed");
                    if file.order() != Ok(place) {
                        file.set("order", &place.to_string());
                        if *n != item {
                            changed.push(*n);
                        }
                    }
                }
                changed.sort_unstable();
                Ok(changed)
            }
        }
    }
}

/// A row of a kind's listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row<'a> {
    Group(&'a [u8]),
    Item(Listed<'a>),
}

/// Why `move` moved nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveError {
    Item(FindError),
    Target(FindError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{BacklogId, ChoreId, ListId};
    use proptest::prelude::*;

    fn line(text: &str) -> Line {
        Line::new(text).unwrap()
    }

    #[test]
    fn a_file_reads_back_byte_for_byte() {
        for text in [
            "id: c1\ngroup: Home\norder: 1000\ncreated: x\n\n# Title\n",
            "id: b1\norder: 1\n\n# T\n\nbody\n\n```sql\nselect 1;\n```\n## not a section\n",
            "id: b1\r\norder: 1\r\n\r\n# T\r\n\r\nbody\r\n",
            "no header at all\n",
            "id: b2\n# no blank line\n",
            "id: b3\norder: 1\n\n# T",
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
        let item = Item::parse(
            b"id: c1\ngroup: Home\norder: -3\ncheck:\nfrom: 126\n\n#  Do it \n\n\nkept\n",
        );
        assert_eq!(item.title(), Some(&b"Do it"[..]));
        // One blank line sets the body off; a second is the body's.
        assert_eq!(item.body(), b"\nkept\n");
        assert_eq!(item.order(), Ok(-3));
        assert_eq!(item.group(), Some(&b"Home"[..]));
        assert_eq!(item.check(), None);
        assert_eq!(item.from(), Some(&b"126"[..]));
        assert_eq!(Item::parse(b"id: b1\n\nno title\n").title(), None);
        assert_eq!(
            Item::parse(b"order: x\n").order(),
            Err(BadOrder::NotANumber("x".to_owned()))
        );
        assert_eq!(Item::parse(b"id: b1\n").order(), Err(BadOrder::Missing));
    }

    #[test]
    fn the_line_and_show_are_what_ls_and_show_print() {
        let chore = Item::parse(b"id: c12\ngroup: g\norder: 1\ncheck: x\nfrom: 126\n\n# Do it\n");
        assert_eq!(
            chore.line(ListKind::Chores, 12),
            b"c12 Do it. Check: x  (126)"
        );
        let ends = Item::parse(b"id: c3\ngroup: g\norder: 1\ncheck: x\n\n# Done?\n");
        assert_eq!(ends.line(ListKind::Chores, 3), b"c3 Done? Check: x");
        let plain = Item::parse(b"id: c4\ngroup: g\norder: 1\n\n# Simple\n");
        assert_eq!(plain.line(ListKind::Chores, 4), b"c4 Simple");
        let backlog = Item::parse(b"id: b41\norder: 1\nfrom: c3\n\n# Later\n\nwhy\n\n## more\n");
        assert_eq!(backlog.line(ListKind::Backlog, 41), b"b41 Later  (from c3)");
        assert_eq!(
            backlog.shown(ListKind::Backlog, 41),
            b"b41 Later  (from c3)\n\nwhy\n\n## more\n"
        );
        assert_eq!(plain.shown(ListKind::Chores, 4), b"c4 Simple\n");
        let open = Item::parse(b"id: b1\norder: 1\n\n# T\n\nno newline");
        assert_eq!(open.shown(ListKind::Backlog, 1), b"b1 T\n\nno newline\n");
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
            group: Some(&line("Deploy")),
            order: 3000,
            check: Some(&line("select 1")),
            from: Some(ItemId::Brief(crate::id::BriefId::new(126))),
            created,
            title: &line("Check the states"),
            body: &Body::new(b""),
        });
        assert_eq!(
            item.to_bytes(),
            b"id: c12\ngroup: Deploy\norder: 3000\ncheck: select 1\nfrom: 126\n\
              created: 2026-11-25T14:08:18Z\n\n# Check the states\n"
        );
    }

    #[test]
    fn edits_keep_a_file_without_final_line_breaks_well_formed() {
        let mut item = Item::parse(b"id: b1\norder: 1\n\n# T");
        item.append_body(&Body::new(b"x"));
        assert_eq!(item.to_bytes(), b"id: b1\norder: 1\n\n# T\n\nx\n");
        let mut item = Item::parse(b"id: b1\norder: 1");
        item.set("order", "2");
        item.set_title(&line("New"));
        assert_eq!(item.to_bytes(), b"id: b1\norder: 2\n\n# New\n");
    }

    // ---- the order of a kind's items ----

    #[derive(Debug, Clone)]
    enum Op {
        /// A chore into group `0..3` (or a backlog item).
        Add(u8),
        /// The item at index, next to the one at the other index.
        Move(usize, bool, usize),
        Delete(usize),
        /// Every order set to its rank (1, 2, 3, …) by hand: no gap left
        /// anywhere, so the next move into one renumbers.
        Squash,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (0u8..3).prop_map(Op::Add),
            5 => (any::<usize>(), any::<bool>(), any::<usize>()).prop_map(|(a, s, b)| Op::Move(a, s, b)),
            1 => any::<usize>().prop_map(Op::Delete),
            1 => Just(Op::Squash),
        ]
    }

    fn list_id(kind: ListKind, number: u32) -> ListId {
        match kind {
            ListKind::Chores => ListId::Chore(ChoreId::new(number).unwrap()),
            ListKind::Backlog => ListId::Backlog(BacklogId::new(number).unwrap()),
        }
    }

    proptest! {
        /// After any adds, moves, deletes and hand renumberings, the
        /// listing is what a plain list says (groups by name, the items of
        /// each in the order the moves put them), and no two items of one
        /// group share an order.
        #[test]
        fn the_listing_is_a_plain_list_s(
            chores in any::<bool>(),
            ops in prop::collection::vec(op(), 0..60),
        ) {
            let kind = if chores { ListKind::Chores } else { ListKind::Backlog };
            let mut items = Items::new(kind);
            // The model: the items in listing order, each with its group.
            let mut model: Vec<(u32, u8)> = Vec::new();
            let mut next = 1;
            for op in ops {
                match op {
                    Op::Add(group) => {
                        let group = if chores { group } else { 0 };
                        let name = format!("g{group}");
                        let order = items.next_order(Some(name.as_bytes()));
                        let mut text = format!("id: x\norder: {order}\n\n# t\n");
                        if chores {
                            text = format!("group: {name}\n{text}");
                        }
                        items.insert(next, Item::parse(text.as_bytes()));
                        let at = model.iter().rposition(|(_, g)| *g <= group).map_or(0, |at| at + 1);
                        model.insert(at, (next, group));
                        next += 1;
                    }
                    Op::Move(from, after, to) if model.len() > 1 => {
                        let (item, target) = (model[from % model.len()].0, model[to % model.len()].0);
                        if item == target {
                            continue;
                        }
                        let side = if after { Side::After } else { Side::Before };
                        let wanted = Move::new(list_id(kind, item), side, list_id(kind, target)).unwrap();
                        items.move_item(&wanted).unwrap();
                        let moved = model.remove(model.iter().position(|(n, _)| *n == item).unwrap());
                        let at = model.iter().position(|(n, _)| *n == target).unwrap();
                        let group = model[at].1;
                        model.insert(if after { at + 1 } else { at }, (moved.0, group));
                    }
                    Op::Move(..) => {}
                    Op::Delete(at) if !model.is_empty() => {
                        let (number, _) = model.remove(at % model.len());
                        items.remove(number);
                    }
                    Op::Delete(_) => {}
                    Op::Squash => {
                        for (rank, (number, _)) in model.iter().enumerate() {
                            items.get_mut(*number).unwrap().set("order", &(rank + 1).to_string());
                        }
                    }
                }
                let listed: Vec<u32> = items.listed().iter().map(|listed| listed.number).collect();
                let want: Vec<u32> = model.iter().map(|(n, _)| *n).collect();
                prop_assert_eq!(listed, want);
                let mut seen = std::collections::BTreeSet::new();
                for listed in items.listed() {
                    let key = (listed.item.group().map(<[u8]>::to_vec), listed.item.order().unwrap());
                    prop_assert!(seen.insert(key), "an order twice in one group");
                }
            }
        }
    }
}
