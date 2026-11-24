//! `chores.md` and `backlog.md`: a lossless parse and the edits of the
//! commands (add, remove, move, assign ids).
//!
//! ```text
//! line    = heading | item | text
//! heading = "## " name                              chores.md groups
//! item    = "- " id (" " rest | end)  |  "- " rest  column 0; rest is not blank
//! text    = anything else, verbatim
//! written: "- c13 <text>. Check: <check>  (126)"    "- b42 <text>  (from 126)"
//! ```

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::id::{BacklogId, ChoreId, ItemId, ListId, ListKind};
use crate::raw;

/// What one line of a list file is. `content` is the line without its
/// terminator.
///
/// The one list-line parser: the file, the scan of a file's history for ids
/// ever used and the merge all read a line through it, so there is one
/// definition of "the id of a line".
pub fn parse_line(kind: ListKind, content: &[u8]) -> Entry<'_> {
    if let Some(name) = content.strip_prefix(b"## ")
        && !raw::is_blank(name)
    {
        return Entry::Heading(name.trim_ascii());
    }
    if let Some(text) = content.strip_prefix(b"- ")
        && !raw::is_blank(text)
    {
        // The id is the whole first word, and a word ends at a space only:
        // `c12:` or `c12<tab>` is text of an item without an id, never a
        // second spelling of c12.
        let word = text.split(|b| *b == b' ').next().unwrap_or(text);
        return Entry::Item {
            id: kind.parse_id(word),
            text,
        };
    }
    Entry::Text(content)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry<'a> {
    /// A `## name` line, with the name trimmed.
    Heading(&'a [u8]),
    /// A `- …` line at column 0. `text` is everything after the `- `, the id
    /// included: an item is shown and copied with its id.
    Item { id: Option<u32>, text: &'a [u8] },
    /// Anything else, with its content: free text, a blank line, an indented
    /// line, `- ` alone.
    Text(&'a [u8]),
}

/// Whether a chore says how to see that it is done: non-blank text after the
/// last `Check:`, not counting a trailing `(<id>)` (the `--from` suffix).
pub fn has_check(item_text: &[u8]) -> bool {
    const MARK: &[u8] = b"Check:";
    let Some(at) = item_text.windows(MARK.len()).rposition(|w| w == MARK) else {
        return false;
    };
    let check = item_text[at + MARK.len()..].trim_ascii();
    !without_source(check).is_empty()
}

fn without_source(check: &[u8]) -> &[u8] {
    let source = check
        .strip_suffix(b")")
        .and_then(|rest| rest.iter().rposition(|b| *b == b'(').map(|at| (at, rest)))
        .filter(|(at, rest)| {
            str::from_utf8(&rest[at + 1..]).is_ok_and(|id| id.parse::<ItemId>().is_ok())
        });
    match source {
        Some((at, _)) => check[..at].trim_ascii_end(),
        None => check,
    }
}

/// A line continues the item above it when it is indented and not blank.
pub(crate) fn is_continuation(content: &[u8]) -> bool {
    matches!(content.first(), Some(b' ' | b'\t')) && !raw::is_blank(content)
}

/// One line of text from the command line, to become (part of) a list line:
/// trimmed, not empty, without a line break.
///
/// A line break would split the item over two lines of the file, and the
/// second would be free text. Trimmed, because blanks at the ends carry
/// nothing and a trailing one would defeat the rule that joins a chore's text
/// and its check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line(String);

impl Line {
    pub fn new(text: &str) -> Result<Line, InvalidLine> {
        let text = text.trim();
        if text.is_empty() {
            Err(InvalidLine::Empty)
        } else if text.contains(['\n', '\r']) {
            Err(InvalidLine::LineBreak)
        } else {
            Ok(Line(text.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Line {
    type Err = InvalidLine;

    fn from_str(s: &str) -> Result<Line, InvalidLine> {
        Line::new(s)
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidLine {
    Empty,
    LineBreak,
}

impl fmt::Display for InvalidLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            InvalidLine::Empty => "the text is empty",
            InvalidLine::LineBreak => "the text has a line break; an item is one line",
        })
    }
}

impl std::error::Error for InvalidLine {}

/// Why an id does not name one item of a list file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindError {
    Missing,
    /// On this many lines (two or more).
    Duplicate(usize),
}

/// The first id of a `remove` that does not name one item; nothing was
/// removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveError {
    pub number: u32,
    pub why: FindError,
}

/// The list holds or once held the id `u32::MAX`: there is no next id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdsExhausted;

/// The check would not read as one once written: it is only `(126)`, which
/// reads as the `--from` suffix, or it ends in `Check:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoCheck;

/// Which side of the other item `move` puts an item on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Before,
    After,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Before => "before",
            Side::After => "after",
        })
    }
}

/// What `move` is asked: an item, and the other item of its list it goes
/// next to. Two items of one list, so no file can be asked to hold an item
/// of the other and no item is put next to itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    item: ListId,
    side: Side,
    target: ListId,
}

impl Move {
    pub fn new(item: ListId, side: Side, target: ListId) -> Result<Move, BadMove> {
        if item.kind() != target.kind() {
            Err(BadMove::OtherList { item, target })
        } else if item == target {
            Err(BadMove::Itself(item))
        } else {
            Ok(Move { item, side, target })
        }
    }

    pub fn kind(&self) -> ListKind {
        self.item.kind()
    }

    pub fn item(&self) -> ListId {
        self.item
    }

    pub fn side(&self) -> Side {
        self.side
    }

    pub fn target(&self) -> ListId {
        self.target
    }
}

/// As `move` prints it: `c12 before c3`.
impl fmt::Display for Move {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.item, self.side, self.target)
    }
}

/// Why two ids make no [`Move`], whatever the files hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadMove {
    OtherList { item: ListId, target: ListId },
    Itself(ListId),
}

impl fmt::Display for BadMove {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = |id: &ListId| match id.kind() {
            ListKind::Chores => "a chore",
            ListKind::Backlog => "a backlog item",
        };
        match self {
            BadMove::OtherList { item, target } => write!(
                f,
                "{item} is {} and {target} {}; an item moves within its own list",
                what(item),
                what(target)
            ),
            BadMove::Itself(item) => write!(f, "{item} cannot go next to itself"),
        }
    }
}

impl std::error::Error for BadMove {}

/// The id of a [`Move`] that does not name one item; nothing was moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveError {
    Item(FindError),
    Target(FindError),
}

/// A list file as its lines, each with its bytes as read.
///
/// What a line is (heading, item, its id) is not stored: it is read off the
/// bytes when asked, so the answer cannot go stale after an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListFile {
    kind: ListKind,
    /// Raw lines, terminator included. Each ends in `\n`, except possibly the
    /// last.
    lines: Vec<Vec<u8>>,
}

impl ListFile {
    /// Never fails: what is no heading and no item is text, kept as it is.
    pub fn parse(kind: ListKind, bytes: &[u8]) -> ListFile {
        ListFile {
            kind,
            lines: raw::lines(bytes).map(<[u8]>::to_vec).collect(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.lines.concat()
    }

    pub fn kind(&self) -> ListKind {
        self.kind
    }

    /// One entry per line, in file order: the n-th is line n + 1.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = Entry<'_>> + ExactSizeIterator {
        self.lines.iter().map(|line| self.entry(line))
    }

    fn entry<'a>(&self, raw_line: &'a [u8]) -> Entry<'a> {
        parse_line(self.kind, raw::content(raw_line))
    }

    /// The item lines as `(index, id, text)`.
    fn items(&self) -> impl DoubleEndedIterator<Item = (usize, Option<u32>, &[u8])> {
        self.entries()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                Entry::Item { id, text } => Some((index, id, text)),
                Entry::Heading(_) | Entry::Text(_) => None,
            })
    }

    fn is_heading(&self, index: usize) -> bool {
        matches!(self.entry(&self.lines[index]), Entry::Heading(_))
    }

    /// The one item with this id, as `(index, text)`.
    fn item(&self, number: u32) -> Result<(usize, &[u8]), FindError> {
        let mut found = self
            .items()
            .filter(|(_, id, _)| *id == Some(number))
            .map(|(index, _, text)| (index, text));
        match (found.next(), found.count()) {
            (None, _) => Err(FindError::Missing),
            (Some(item), 0) => Ok(item),
            (Some(_), more) => Err(FindError::Duplicate(more + 1)),
        }
    }

    /// The text of the one item with this id (the line without `- `).
    pub fn find(&self, number: u32) -> Result<&[u8], FindError> {
        self.item(number).map(|(_, text)| text)
    }

    /// The one item with this id: its text, and the contents of the
    /// indented lines that continue it, which are its own.
    pub fn find_with_indented(&self, number: u32) -> Result<(&[u8], Vec<&[u8]>), FindError> {
        self.item(number)
            .map(|(index, text)| (text, self.indented_below(index).collect()))
    }

    /// The contents of the indented lines that continue the item at `index`.
    pub(crate) fn indented_below(&self, index: usize) -> impl Iterator<Item = &[u8]> {
        self.lines[index + 1..self.after_item(index)]
            .iter()
            .map(|line| raw::content(line))
    }

    /// The highest id on an item line; 0 for none.
    pub fn max_id(&self) -> u32 {
        self.items().filter_map(|(_, id, _)| id).max().unwrap_or(0)
    }

    /// The next free id: one past the highest of this file and
    /// `history_mark`, the highest id the file's history ever held.
    ///
    /// The file's own ids are counted here and not left to the caller's
    /// mark, so no caller can hand out an id the file already holds.
    pub fn next_id(&self, history_mark: u32) -> Result<u32, IdsExhausted> {
        self.max_id()
            .max(history_mark)
            .checked_add(1)
            .ok_or(IdsExhausted)
    }

    /// Adds a chore after the last item of the first group named `group`,
    /// directly under the heading when the group is empty; a group that does
    /// not exist is appended at the end of the file. `id` comes from
    /// [`ListFile::next_id`].
    ///
    /// # Panics
    ///
    /// When this is not a chores file.
    pub fn add_chore(
        &mut self,
        id: ChoreId,
        group: &Line,
        text: &Line,
        check: &Line,
        from: Option<ItemId>,
    ) -> Result<(), NoCheck> {
        assert_eq!(self.kind, ListKind::Chores, "a chore goes into chores.md");
        // A text that ends its own sentence is not given a second full stop.
        let join = if text.as_str().ends_with(['.', '!', '?']) {
            " "
        } else {
            ". "
        };
        // Asked of the check as it will stand in the line, by the function
        // lint asks, so the tool never writes a chore its own lint reports.
        // Without the source suffix: a check must not need it to count.
        let check = format!("Check: {check}");
        if !has_check(check.as_bytes()) {
            return Err(NoCheck);
        }
        let mut item = format!("- {id} {text}{join}{check}");
        if let Some(from) = from {
            item.push_str(&format!("  ({from})"));
        }
        let item = item.into_bytes();

        let heading = (0..self.lines.len()).find(|index| {
            self.entry(&self.lines[*index]) == Entry::Heading(group.as_str().as_bytes())
        });
        match heading {
            Some(heading) => {
                let end = (heading + 1..self.lines.len())
                    .find(|index| self.is_heading(*index))
                    .unwrap_or(self.lines.len());
                let last_item = self
                    .items()
                    .map(|(index, ..)| index)
                    .rfind(|index| (heading + 1..end).contains(index));
                let at = match last_item {
                    Some(last_item) => self.after_item(last_item),
                    None => heading + 1,
                };
                self.insert(at, vec![item]);
            }
            None => {
                let heading = format!("## {group}").into_bytes();
                // A blank line sets the new group apart, where there is
                // something to set it apart from.
                let separated = self
                    .lines
                    .last()
                    .is_none_or(|last| raw::is_blank(raw::content(last)));
                let block = if separated {
                    vec![heading, item]
                } else {
                    vec![Vec::new(), heading, item]
                };
                self.insert(self.lines.len(), block);
            }
        }
        Ok(())
    }

    /// Adds a backlog item after the last item, at the end of the file when
    /// there is none: line order is the priority order and a new item is the
    /// lowest. `id` comes from [`ListFile::next_id`].
    ///
    /// # Panics
    ///
    /// When this is not a backlog file.
    pub fn add_backlog(&mut self, id: BacklogId, text: &Line, from: Option<ItemId>) {
        assert_eq!(
            self.kind,
            ListKind::Backlog,
            "a backlog item goes into backlog.md"
        );
        let mut item = format!("- {id} {text}");
        if let Some(from) = from {
            item.push_str(&format!("  (from {from})"));
        }
        let at = match self.items().next_back() {
            Some((last_item, ..)) => self.after_item(last_item),
            None => self.lines.len(),
        };
        self.insert(at, vec![item.into_bytes()]);
    }

    /// The index after an item and the indented lines that continue it, so a
    /// new item does not come between another item and its continuation.
    fn after_item(&self, item: usize) -> usize {
        (item + 1..self.lines.len())
            .find(|index| !is_continuation(raw::content(&self.lines[*index])))
            .unwrap_or(self.lines.len())
    }

    /// Inserts lines given without terminator. They take the terminator of
    /// the file's first line; a last line without one gets it when lines are
    /// put after it.
    fn insert(&mut self, at: usize, contents: Vec<Vec<u8>>) {
        let terminator = raw::terminator_of(self.lines.first().map(Vec::as_slice));
        if at == self.lines.len()
            && let Some(last) = self.lines.last_mut()
            && !last.ends_with(b"\n")
        {
            last.extend_from_slice(terminator);
        }
        self.lines.splice(
            at..at,
            contents.into_iter().map(|mut line| {
                line.extend_from_slice(terminator);
                line
            }),
        );
    }

    /// Deletes the items with these ids, all or none: every id must sit on
    /// exactly one line. An item goes with the indented lines that continue
    /// it: they are its own, and left behind they would continue whichever
    /// item ends up above them.
    pub fn remove(&mut self, numbers: &[u32]) -> Result<(), RemoveError> {
        let indexes = numbers
            .iter()
            .map(|number| match self.item(*number) {
                Ok((index, _)) => Ok(index),
                Err(why) => Err(RemoveError {
                    number: *number,
                    why,
                }),
            })
            .collect::<Result<BTreeSet<usize>, RemoveError>>()?;
        for index in indexes.into_iter().rev() {
            let end = self.after_item(index);
            self.lines.drain(index..end);
        }
        Ok(())
    }

    /// Puts the item directly before the target's line, or directly after
    /// the target and the indented lines that continue it, so it never comes
    /// between an item and its continuation. In a chores file it is then in
    /// the target's group: no heading is between the two.
    ///
    /// The item's indented lines go with it: they are its own, as for
    /// `remove`, and left behind they would continue whichever item ends up
    /// above them. The merge of a sync keeps them under the item as well
    /// (`merge::reconcile`).
    ///
    /// Nothing else changes, except that a line without a terminator (the
    /// file's last) gets one when a line comes after it. An item that is
    /// where it is asked to be is left as it is.
    ///
    /// # Panics
    ///
    /// When the move is not of this file's list.
    pub fn move_item(&mut self, wanted: &Move) -> Result<(), MoveError> {
        assert_eq!(self.kind, wanted.kind(), "an item moves within its list");
        let (from, _) = self.item(wanted.item.get()).map_err(MoveError::Item)?;
        let (target, _) = self.item(wanted.target.get()).map_err(MoveError::Target)?;
        let end = self.after_item(from);
        let at = match wanted.side {
            Side::Before => target,
            Side::After => self.after_item(target),
        };
        // The target is an item, not a continuation, so `at` is never inside
        // the block: at its edges the block is in place already.
        if at == from || at == end {
            return Ok(());
        }
        let terminator = raw::terminator_of(self.lines.first().map(Vec::as_slice));
        let mut block: Vec<Vec<u8>> = self.lines.drain(from..end).collect();
        let at = if at > from { at - block.len() } else { at };
        // The block was at the end and is no longer: a line follows it.
        if let Some(last) = block.last_mut()
            && !last.ends_with(b"\n")
        {
            last.extend_from_slice(terminator);
        }
        if at == self.lines.len()
            && let Some(last) = self.lines.last_mut()
            && !last.ends_with(b"\n")
        {
            last.extend_from_slice(terminator);
        }
        self.lines.splice(at..at, block);
        Ok(())
    }

    /// Gives every item without an id the next free one, in file order, and
    /// returns them; all or none. See [`ListFile::next_id`] for
    /// `history_mark`.
    pub fn assign_ids(&mut self, history_mark: u32) -> Result<Vec<u32>, IdsExhausted> {
        let without_id: Vec<usize> = self
            .items()
            .filter(|(_, id, _)| id.is_none())
            .map(|(index, ..)| index)
            .collect();
        if without_id.is_empty() {
            return Ok(Vec::new());
        }
        let first = self.next_id(history_mark)?;
        // The last id has to exist as well, or none is given.
        let last = u32::try_from(without_id.len() - 1)
            .ok()
            .and_then(|more| first.checked_add(more))
            .ok_or(IdsExhausted)?;
        let ids: Vec<u32> = (first..=last).collect();
        for (index, id) in without_id.into_iter().zip(&ids) {
            self.lines[index] = with_id(self.kind, &self.lines[index], *id);
        }
        Ok(ids)
    }
}

/// The line of an item without an id, given `id`: the id goes in after the
/// `- `, and nothing else changes. The one spelling of an assigned id, so
/// the merge can tell such a line from the line it was.
pub fn with_id(kind: ListKind, line: &[u8], id: u32) -> Vec<u8> {
    let mut line = line.to_vec();
    line.splice(2..2, format!("{}{id} ", kind.prefix()).into_bytes());
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const CHORES: ListKind = ListKind::Chores;
    const BACKLOG: ListKind = ListKind::Backlog;

    fn line(text: &str) -> Line {
        Line::new(text).unwrap()
    }

    fn chore_id(number: u32) -> ChoreId {
        ChoreId::new(number).unwrap()
    }

    fn backlog_id(number: u32) -> BacklogId {
        BacklogId::new(number).unwrap()
    }

    fn utf8(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn chores(bytes: &str) -> ListFile {
        ListFile::parse(CHORES, bytes.as_bytes())
    }

    fn backlog(bytes: &str) -> ListFile {
        ListFile::parse(BACKLOG, bytes.as_bytes())
    }

    fn written(file: &ListFile) -> String {
        utf8(&file.to_bytes())
    }

    // ---- the line grammar ----

    #[test]
    fn items_with_an_id() {
        let item = |id, text: &'static str| Entry::Item {
            id: Some(id),
            text: text.as_bytes(),
        };
        assert_eq!(parse_line(CHORES, b"- c12 Do it"), item(12, "c12 Do it"));
        assert_eq!(parse_line(CHORES, b"- c12"), item(12, "c12"));
        assert_eq!(parse_line(CHORES, b"- c12 "), item(12, "c12 "));
        assert_eq!(parse_line(BACKLOG, b"- b41 Later"), item(41, "b41 Later"));
        assert_eq!(
            parse_line(CHORES, b"- c4294967295 x"),
            item(u32::MAX, "c4294967295 x")
        );
    }

    #[test]
    fn items_without_an_id() {
        for content in [
            "- Do it",
            "- c012 leading zero",
            "- c0 zero",
            "- b41 the other list's id",
            "- c4294967296 overflow",
            "- c12: punctuation is part of the word",
            "- c12\ttab is no separator",
            "-  c12 two spaces",
            "- C12 upper case",
        ] {
            assert_eq!(
                parse_line(CHORES, content.as_bytes()),
                Entry::Item {
                    id: None,
                    text: &content.as_bytes()[2..]
                },
                "{content}"
            );
        }
    }

    #[test]
    fn headings_and_text() {
        assert_eq!(parse_line(CHORES, b"## Home"), Entry::Heading(b"Home"));
        assert_eq!(
            parse_line(CHORES, b"##  Home office  "),
            Entry::Heading(b"Home office")
        );
        assert_eq!(parse_line(BACKLOG, b"## Later"), Entry::Heading(b"Later"));
        for content in [
            "",
            "   ",
            "- ",
            "-   ",
            "-",
            "-c12 no space",
            " - c12 indented",
            "\t- c12 indented",
            "  continuation",
            "* c12 other bullet",
            "##",
            "## ",
            "##Home",
            "# Title",
            "### Sub",
            " ## indented",
            "free text",
        ] {
            assert_eq!(
                parse_line(CHORES, content.as_bytes()),
                Entry::Text(content.as_bytes()),
                "{content:?}"
            );
        }
    }

    #[test]
    fn a_chore_has_a_check_when_text_follows_the_last_check() {
        for with_check in [
            "c1 Do it. Check: the page loads",
            "c1 Do it. Check: the page loads  (126)",
            "c1 Do it. Check:x",
            "c1 Check: first. Check: second",
            "c1 Do it. Check: see (the log)",
            "c1 Do it. Check: see b41 (b41)",
            "c1 Do it. Check: (not an id)",
        ] {
            assert!(has_check(with_check.as_bytes()), "{with_check}");
        }
        for without_check in [
            "c1 Do it",
            "c1 Do it. Check:",
            "c1 Do it. Check:   ",
            "c1 Do it. Check:  (126)",
            "c1 Do it. Check: (c12)",
            "c1 Do it. Check: (b41)  ",
            "c1 Do it. Check: yes. Check:",
            "c1 Do it. check: lower case",
            "c1 Do it. Check : spaced",
        ] {
            assert!(!has_check(without_check.as_bytes()), "{without_check}");
        }
    }

    #[test]
    fn a_line_of_input_is_trimmed_and_single() {
        assert_eq!(line("  Do it \t").as_str(), "Do it");
        assert_eq!(Line::new(""), Err(InvalidLine::Empty));
        assert_eq!(Line::new(" \n "), Err(InvalidLine::Empty));
        assert_eq!(Line::new("a\nb"), Err(InvalidLine::LineBreak));
        assert_eq!(Line::new("a\rb"), Err(InvalidLine::LineBreak));
    }

    // ---- reads ----

    #[test]
    fn find_tells_missing_from_duplicate() {
        let file = chores("## Home\n- c1 one\n- c2 two\n- c2 two again\n- c2 thrice\n- no id\n");
        assert_eq!(file.find(1), Ok(&b"c1 one"[..]));
        assert_eq!(file.find(2), Err(FindError::Duplicate(3)));
        assert_eq!(file.find(3), Err(FindError::Missing));
        assert_eq!(file.max_id(), 2);
        assert_eq!(chores("free text\n").max_id(), 0);
    }

    #[test]
    fn the_next_id_is_past_the_file_and_past_the_history() {
        let file = chores("- c7 x\n- c3 y\n");
        assert_eq!(file.next_id(0), Ok(8));
        assert_eq!(file.next_id(7), Ok(8));
        assert_eq!(file.next_id(20), Ok(21));
        assert_eq!(file.next_id(u32::MAX), Err(IdsExhausted));
        assert_eq!(chores("- c4294967295 x\n").next_id(0), Err(IdsExhausted));
        assert_eq!(chores("").next_id(0), Ok(1));
    }

    // ---- add chore ----

    #[test]
    fn a_chore_is_written_as_text_check_and_source() {
        let mut file = chores("## Home\n");
        let home = line("Home");
        file.add_chore(
            chore_id(13),
            &home,
            &line("Rotate the key"),
            &line("login works"),
            Some(ItemId::Brief(crate::BriefId::new(126))),
        )
        .unwrap();
        file.add_chore(chore_id(14), &home, &line("Done."), &line("a"), None)
            .unwrap();
        file.add_chore(chore_id(15), &home, &line("Really?"), &line("b"), None)
            .unwrap();
        file.add_chore(chore_id(16), &home, &line("Now!"), &line("c"), None)
            .unwrap();
        assert_eq!(
            written(&file),
            "## Home\n\
             - c13 Rotate the key. Check: login works  (126)\n\
             - c14 Done. Check: a\n\
             - c15 Really? Check: b\n\
             - c16 Now! Check: c\n"
        );
    }

    #[test]
    fn a_chore_goes_after_the_last_item_of_the_first_group_of_that_name() {
        let mut file = chores(
            "# Chores\n\n## Home\n- c1 one\n\nnotes\n- c2 two\n\n## Work\n- c3 three\n\n## Home\n- c4 four\n",
        );
        file.add_chore(chore_id(5), &line("Home"), &line("new"), &line("x"), None)
            .unwrap();
        assert_eq!(
            written(&file),
            "# Chores\n\n## Home\n- c1 one\n\nnotes\n- c2 two\n- c5 new. Check: x\n\n## Work\n- c3 three\n\n## Home\n- c4 four\n"
        );
    }

    #[test]
    fn a_chore_goes_directly_under_the_heading_of_an_empty_group() {
        let mut file = chores("## Home\n\nnotes\n## Work\n- c3 three\n");
        file.add_chore(chore_id(5), &line("Home"), &line("new"), &line("x"), None)
            .unwrap();
        assert_eq!(
            written(&file),
            "## Home\n- c5 new. Check: x\n\nnotes\n## Work\n- c3 three\n"
        );
    }

    #[test]
    fn a_new_chore_does_not_come_between_an_item_and_its_continuation() {
        let mut file = chores("## Home\n- c1 one\n  more about one\n\ttabbed\n\n## Work\n");
        file.add_chore(chore_id(2), &line("Home"), &line("new"), &line("x"), None)
            .unwrap();
        assert_eq!(
            written(&file),
            "## Home\n- c1 one\n  more about one\n\ttabbed\n- c2 new. Check: x\n\n## Work\n"
        );
    }

    #[test]
    fn a_missing_group_is_appended_after_a_blank_line() {
        let mut file = chores("## Home\n- c1 one\n");
        file.add_chore(chore_id(2), &line("Work"), &line("new"), &line("x"), None)
            .unwrap();
        assert_eq!(
            written(&file),
            "## Home\n- c1 one\n\n## Work\n- c2 new. Check: x\n"
        );
        // A group name is matched whole and with its case.
        file.add_chore(chore_id(3), &line("work"), &line("new"), &line("x"), None)
            .unwrap();
        assert!(written(&file).ends_with("- c2 new. Check: x\n\n## work\n- c3 new. Check: x\n"));
    }

    #[test]
    fn a_missing_group_needs_no_blank_line_when_nothing_is_above_it() {
        let mut empty = chores("");
        empty
            .add_chore(chore_id(1), &line("Home"), &line("new"), &line("x"), None)
            .unwrap();
        assert_eq!(written(&empty), "## Home\n- c1 new. Check: x\n");

        let mut blank_end = chores("# Chores\n\n");
        blank_end
            .add_chore(chore_id(1), &line("Home"), &line("new"), &line("x"), None)
            .unwrap();
        assert_eq!(
            written(&blank_end),
            "# Chores\n\n## Home\n- c1 new. Check: x\n"
        );
    }

    #[test]
    fn new_lines_take_the_terminator_of_the_first_line() {
        let mut crlf = chores("## Home\r\n- c1 one");
        crlf.add_chore(chore_id(2), &line("Home"), &line("new"), &line("x"), None)
            .unwrap();
        crlf.add_chore(chore_id(3), &line("Work"), &line("new"), &line("y"), None)
            .unwrap();
        assert_eq!(
            written(&crlf),
            "## Home\r\n- c1 one\r\n- c2 new. Check: x\r\n\r\n## Work\r\n- c3 new. Check: y\r\n"
        );

        let mut unterminated = backlog("- b1 one");
        unterminated.add_backlog(backlog_id(2), &line("two"), None);
        assert_eq!(written(&unterminated), "- b1 one\n- b2 two\n");
    }

    #[test]
    fn a_chore_whose_check_reads_as_the_source_suffix_is_refused() {
        let mut file = chores("## Home\n");
        let from = Some(ItemId::Brief(crate::BriefId::new(126)));
        for check in ["(126)", "(c12)", "Check:", "see Check: (b41)"] {
            for from in [None, from] {
                assert_eq!(
                    file.add_chore(chore_id(1), &line("Home"), &line("t"), &line(check), from),
                    Err(NoCheck),
                    "{check}"
                );
            }
        }
        assert_eq!(written(&file), "## Home\n");
    }

    #[test]
    #[should_panic(expected = "a chore goes into chores.md")]
    fn a_chore_is_not_added_to_the_backlog() {
        let _ = backlog("").add_chore(chore_id(1), &line("g"), &line("t"), &line("c"), None);
    }

    // ---- add backlog ----

    #[test]
    fn a_backlog_item_goes_after_the_last_item() {
        let mut file = backlog("# Backlog\n- b1 one\n  detail\n\nnotes at the end\n");
        file.add_backlog(backlog_id(2), &line("two"), None);
        file.add_backlog(
            backlog_id(3),
            &line("three"),
            Some(ItemId::Chore(chore_id(7))),
        );
        assert_eq!(
            written(&file),
            "# Backlog\n- b1 one\n  detail\n- b2 two\n- b3 three  (from c7)\n\nnotes at the end\n"
        );
    }

    #[test]
    fn a_backlog_item_goes_to_the_end_of_a_file_without_items() {
        let mut file = backlog("# Backlog\n\nnothing yet");
        file.add_backlog(backlog_id(1), &line("one"), None);
        assert_eq!(written(&file), "# Backlog\n\nnothing yet\n- b1 one\n");

        let mut empty = backlog("");
        empty.add_backlog(backlog_id(1), &line("one"), None);
        assert_eq!(written(&empty), "- b1 one\n");
    }

    // ---- remove ----

    #[test]
    fn remove_deletes_the_item_with_its_indented_lines() {
        let mut file = chores(
            "## Home\n- c1 one\n  more about one\n\t- and a list\n\n- c2 two\n- c3 three\n  of three",
        );
        assert_eq!(file.remove(&[3, 1]), Ok(()));
        // The heading above and the item below stay, and so does the blank
        // line after c1's lines.
        assert_eq!(written(&file), "## Home\n\n- c2 two\n");
    }

    #[test]
    fn an_item_is_found_with_its_indented_lines() {
        let file = chores("- c1 one\n  more\n    deeper\n\n  free\n- c2 two\n");
        assert_eq!(
            file.find_with_indented(1),
            Ok((&b"c1 one"[..], vec![&b"  more"[..], b"    deeper"]))
        );
        assert_eq!(file.find_with_indented(2), Ok((&b"c2 two"[..], vec![])));
    }

    #[test]
    fn remove_is_all_or_nothing() {
        let before = "- c1 one\n- c2 two\n- c2 two again\n";
        let mut file = chores(before);
        assert_eq!(
            file.remove(&[1, 9]),
            Err(RemoveError {
                number: 9,
                why: FindError::Missing
            })
        );
        assert_eq!(
            file.remove(&[1, 2]),
            Err(RemoveError {
                number: 2,
                why: FindError::Duplicate(2)
            })
        );
        assert_eq!(written(&file), before);
        // The same id twice is one line to delete.
        assert_eq!(file.remove(&[1, 1]), Ok(()));
        assert_eq!(written(&file), "- c2 two\n- c2 two again\n");
    }

    // ---- move ----

    fn list_id(kind: ListKind, number: u32) -> ListId {
        match kind {
            ListKind::Chores => ListId::Chore(chore_id(number)),
            ListKind::Backlog => ListId::Backlog(backlog_id(number)),
        }
    }

    fn moving(kind: ListKind, item: u32, side: Side, target: u32) -> Move {
        Move::new(list_id(kind, item), side, list_id(kind, target)).unwrap()
    }

    #[test]
    fn two_ids_make_a_move_when_they_are_two_items_of_one_list() {
        let (c1, c2, b1) = (list_id(CHORES, 1), list_id(CHORES, 2), list_id(BACKLOG, 1));
        assert!(Move::new(c1, Side::Before, c2).is_ok());
        let other = Move::new(c1, Side::After, b1).unwrap_err();
        assert_eq!(
            other,
            BadMove::OtherList {
                item: c1,
                target: b1
            }
        );
        assert_eq!(
            other.to_string(),
            "c1 is a chore and b1 a backlog item; an item moves within its own list"
        );
        let itself = Move::new(b1, Side::Before, b1).unwrap_err();
        assert_eq!(itself.to_string(), "b1 cannot go next to itself");
        assert_eq!(
            moving(BACKLOG, 3, Side::After, 1).to_string(),
            "b3 after b1"
        );
    }

    #[test]
    fn a_moved_item_takes_its_continuation_and_skips_the_target_s() {
        let before = "## Home\n- c1 one\n  about one\n- c2 two\n  about two\n\tand more\n\nnotes\n";
        let mut file = chores(before);
        file.move_item(&moving(CHORES, 1, Side::After, 2)).unwrap();
        assert_eq!(
            written(&file),
            "## Home\n- c2 two\n  about two\n\tand more\n- c1 one\n  about one\n\nnotes\n"
        );
        file.move_item(&moving(CHORES, 1, Side::Before, 2)).unwrap();
        assert_eq!(written(&file), before);
    }

    #[test]
    fn a_line_without_a_terminator_gets_one_when_a_line_follows_it() {
        let mut file = backlog("- b1 one\r\n- b2 two\r\n- b3 three");
        file.move_item(&moving(BACKLOG, 3, Side::Before, 1))
            .unwrap();
        assert_eq!(written(&file), "- b3 three\r\n- b1 one\r\n- b2 two\r\n");

        let mut file = backlog("- b1 one\n- b2 two");
        file.move_item(&moving(BACKLOG, 1, Side::After, 2)).unwrap();
        assert_eq!(written(&file), "- b2 two\n- b1 one\n");
    }

    #[test]
    fn an_item_where_it_is_asked_to_be_stays() {
        let before = "- b1 one\n  more\n- b2 two\n- b3 three";
        for wanted in [
            moving(BACKLOG, 2, Side::After, 1),
            moving(BACKLOG, 1, Side::Before, 2),
            moving(BACKLOG, 3, Side::After, 2),
        ] {
            let mut file = backlog(before);
            file.move_item(&wanted).unwrap();
            assert_eq!(written(&file), before, "{wanted}");
        }
    }

    #[test]
    fn move_is_refused_for_an_id_on_no_line_or_on_two() {
        let before = "- c1 a\n- c2 b\n- c2 c\n";
        let mut file = chores(before);
        assert_eq!(
            file.move_item(&moving(CHORES, 9, Side::Before, 1)),
            Err(MoveError::Item(FindError::Missing))
        );
        assert_eq!(
            file.move_item(&moving(CHORES, 2, Side::Before, 1)),
            Err(MoveError::Item(FindError::Duplicate(2)))
        );
        assert_eq!(
            file.move_item(&moving(CHORES, 1, Side::After, 9)),
            Err(MoveError::Target(FindError::Missing))
        );
        assert_eq!(
            file.move_item(&moving(CHORES, 1, Side::After, 2)),
            Err(MoveError::Target(FindError::Duplicate(2)))
        );
        assert_eq!(written(&file), before);
    }

    #[test]
    #[should_panic(expected = "an item moves within its list")]
    fn a_chore_is_not_moved_in_the_backlog() {
        let _ = backlog("- b1 a\n- b2 b\n").move_item(&moving(CHORES, 1, Side::Before, 2));
    }

    // ---- assign ids ----

    #[test]
    fn items_without_an_id_get_the_next_ones_in_file_order() {
        let mut file =
            chores("## Home\n- first\n- c7 has one\n- c012 not an id\r\n-  spaced\n- \n");
        assert_eq!(file.assign_ids(9), Ok(vec![10, 11, 12]));
        assert_eq!(
            written(&file),
            "## Home\n- c10 first\n- c7 has one\n- c11 c012 not an id\r\n- c12  spaced\n- \n"
        );
        assert_eq!(file.assign_ids(0), Ok(vec![]));
    }

    #[test]
    fn assigning_past_the_last_id_assigns_none() {
        let before = "- b4294967294 x\n- one\n- two\n";
        let mut file = backlog(before);
        assert_eq!(file.assign_ids(0), Err(IdsExhausted));
        assert_eq!(written(&file), before);

        let mut one_left = backlog("- b4294967294 x\n- one\n");
        assert_eq!(one_left.assign_ids(0), Ok(vec![u32::MAX]));
        assert_eq!(one_left.assign_ids(0), Ok(vec![]));
    }

    // ---- properties ----

    fn kind() -> impl Strategy<Value = ListKind> {
        prop_oneof![Just(CHORES), Just(BACKLOG)]
    }

    const GROUPS: [&str; 3] = ["Home", "Work", "Home office"];

    /// One line of a hand-edited list file, without terminator: every shape
    /// of the grammar, ids from a small range so they repeat, a few group
    /// names so headings repeat.
    fn content() -> impl Strategy<Value = String> {
        let words = "[a-z]{1,6}( [a-z]{1,6}){0,2}";
        let suffix = prop_oneof![
            4 => Just(""),
            1 => Just(". Check: it works"),
            1 => Just("  (126)"),
            1 => Just(". Check: it works  (from b3)"),
            1 => Just(". Check:"),
        ];
        prop_oneof![
            6 => ("[bc]", 1..12u32, words, suffix.clone())
                .prop_map(|(p, n, w, s)| format!("- {p}{n} {w}{s}")),
            1 => ("[bc]", 1..12u32).prop_map(|(p, n)| format!("- {p}{n}")),
            3 => (words, suffix).prop_map(|(w, s)| format!("- {w}{s}")),
            1 => ("[bc]", 0..12u32, words).prop_map(|(p, n, w)| format!("- {p}0{n} {w}")),
            3 => prop::sample::select(&GROUPS[..]).prop_map(|g| format!("## {g}")),
            1 => prop::sample::select(&GROUPS[..]).prop_map(|g| format!("##  {g} ")),
            2 => words.prop_map(|w| format!("  {w}")),
            1 => words.prop_map(|w| format!("\t{w}")),
            2 => words.prop_map(String::from),
            3 => prop::sample::select(&["", "  ", "- ", "-", "##", "## ", "# Title"][..])
                .prop_map(String::from),
        ]
    }

    /// A list file as bytes: mostly `\n`, sometimes `\r\n`, mixed now and
    /// then, with or without a terminator on the last line.
    fn file_bytes() -> impl Strategy<Value = Vec<u8>> {
        file_of(content())
    }

    /// A list file for `move`: more indented lines below items, as the move
    /// has to place an item's block whole and past the target's.
    fn move_file_bytes() -> impl Strategy<Value = Vec<u8>> {
        file_of(prop_oneof![
            2 => content(),
            1 => "[a-z]{1,6}".prop_map(|w| format!("  {w}")),
        ])
    }

    /// Lines from `line`, joined as [`file_bytes`] says.
    fn file_of(line: impl Strategy<Value = String>) -> impl Strategy<Value = Vec<u8>> {
        let terminator = prop_oneof![
            8 => Just(["\n", "\n"]),
            2 => Just(["\r\n", "\r\n"]),
            1 => Just(["\n", "\r\n"]),
        ];
        (
            prop::collection::vec((line, any::<bool>()), 0..12),
            terminator,
            any::<bool>(),
        )
            .prop_map(|(lines, terminators, ends_terminated)| {
                let count = lines.len();
                let mut bytes = Vec::new();
                for (index, (content, second)) in lines.into_iter().enumerate() {
                    bytes.extend_from_slice(content.as_bytes());
                    if index + 1 < count || ends_terminated {
                        bytes.extend_from_slice(terminators[usize::from(second)].as_bytes());
                    }
                }
                bytes
            })
    }

    fn input_line() -> impl Strategy<Value = Line> {
        prop_oneof![
            4 => "[a-z]{1,6}( [a-z]{1,6}){0,2}[.!?]?",
            1 => "[ -~]{1,12}",
            1 => "\\PC{1,8}",
        ]
        .prop_filter_map("a line is not blank", |text| Line::new(&text).ok())
    }

    /// A check as typed, now and then one the tool must refuse: text that
    /// would read as the source suffix or leave `Check:` with nothing after.
    fn check_line() -> impl Strategy<Value = Line> {
        prop_oneof![
            6 => input_line(),
            1 => "\\([bc]?[1-9][0-9]{0,2}\\)".prop_map(|check| line(&check)),
            1 => "([a-z]{1,4} )?Check:".prop_map(|check| line(&check)),
        ]
    }

    fn source() -> impl Strategy<Value = Option<ItemId>> {
        prop::option::of(prop_oneof![
            any::<u32>().prop_map(|n| ItemId::Brief(crate::BriefId::new(n))),
            (1..=u32::MAX).prop_map(|n| ItemId::Chore(chore_id(n))),
            (1..=u32::MAX).prop_map(|n| ItemId::Backlog(backlog_id(n))),
        ])
    }

    fn raw_lines(bytes: &[u8]) -> Vec<Vec<u8>> {
        bytes
            .split_inclusive(|b| *b == b'\n')
            .map(<[u8]>::to_vec)
            .collect()
    }

    fn content_of(raw_line: &[u8]) -> &[u8] {
        let line = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
        line.strip_suffix(b"\r").unwrap_or(line)
    }

    fn file_terminator(lines: &[Vec<u8>]) -> &'static [u8] {
        match lines.first() {
            Some(first) if first.ends_with(b"\r\n") => b"\r\n",
            _ => b"\n",
        }
    }

    fn is_item(kind: ListKind, raw_line: &[u8]) -> bool {
        matches!(parse_line(kind, content_of(raw_line)), Entry::Item { .. })
    }

    fn is_heading(raw_line: &[u8]) -> bool {
        matches!(parse_line(CHORES, content_of(raw_line)), Entry::Heading(_))
    }

    fn id_of(kind: ListKind, raw_line: &[u8]) -> Option<u32> {
        match parse_line(kind, content_of(raw_line)) {
            Entry::Item { id, .. } => id,
            Entry::Heading(_) | Entry::Text(_) => None,
        }
    }

    /// Edit locality: `after` is `before` with lines put in at one place and
    /// nothing else touched, except that a last line without a terminator
    /// gets one when the new lines follow it. The new lines end as the
    /// file's first line does. Returns the place and the new lines.
    fn inserted(before: &[u8], after: &[u8]) -> Result<(usize, Vec<Vec<u8>>), TestCaseError> {
        let (old, new) = (raw_lines(before), raw_lines(after));
        prop_assert!(new.len() > old.len(), "no line was added");
        let added = new.len() - old.len();
        let terminator = file_terminator(&old);
        let untouched =
            (0..=old.len()).find(|at| new[..*at] == old[..*at] && new[at + added..] == old[*at..]);
        let at = match untouched {
            Some(at) => at,
            None => {
                let mut terminated = old.clone();
                let last = terminated.last_mut().expect("an empty file is a prefix");
                prop_assert!(
                    !last.ends_with(b"\n"),
                    "a line other than the new ones changed"
                );
                last.extend_from_slice(terminator);
                prop_assert_eq!(&new[..old.len()], &terminated[..]);
                old.len()
            }
        };
        let block = new[at..at + added].to_vec();
        for line in &block {
            prop_assert_eq!(&line[content_of(line).len()..], terminator);
        }
        Ok((at, block))
    }

    /// The new item at `lines[at]` is the last item of `lines[start..end]`
    /// (a group, or the file) and sits directly below the item above it and
    /// that item's continuation lines; with no item above, at `empty`.
    fn placed_last(
        kind: ListKind,
        lines: &[Vec<u8>],
        (start, end): (usize, usize),
        at: usize,
        empty: usize,
    ) -> Result<(), TestCaseError> {
        let continues = |line: &Vec<u8>| is_continuation(content_of(line));
        prop_assert!(!lines[at + 1..end].iter().any(|l| is_item(kind, l)));
        match lines[start..at].iter().rposition(|l| is_item(kind, l)) {
            Some(above) => {
                prop_assert!(lines[start + above + 1..at].iter().all(continues));
                prop_assert!(!lines.get(at + 1).is_some_and(continues));
            }
            None => prop_assert_eq!(at, empty),
        }
        Ok(())
    }

    fn check_add_backlog(
        bytes: &[u8],
        mark: u32,
        text: &Line,
        from: Option<ItemId>,
    ) -> Result<(), TestCaseError> {
        let mut file = ListFile::parse(BACKLOG, bytes);
        let id = file.next_id(mark).unwrap();
        file.add_backlog(backlog_id(id), text, from);
        let after = file.to_bytes();

        let (at, new) = inserted(bytes, &after)?;
        prop_assert_eq!(new.len(), 1);
        let suffix = from.map(|f| format!("  (from {f})")).unwrap_or_default();
        let expected = format!("b{id} {text}{suffix}");
        prop_assert_eq!(utf8(content_of(&new[0])), format!("- {expected}"));
        prop_assert_eq!(file.find(id), Ok(expected.as_bytes()));

        // Lowest priority: below every item, at the end of a file without.
        let lines = raw_lines(&after);
        placed_last(BACKLOG, &lines, (0, lines.len()), at, lines.len() - 1)
    }

    fn check_add_chore(
        bytes: &[u8],
        mark: u32,
        group: &Line,
        (text, check): (&Line, &Line),
        from: Option<ItemId>,
    ) -> Result<(), TestCaseError> {
        let mut file = ListFile::parse(CHORES, bytes);
        let id = file.next_id(mark).unwrap();
        // Refused when the check would not read as one, and only then.
        let refused = file.add_chore(chore_id(id), group, text, check, from) == Err(NoCheck);
        let check_alone = format!("Check: {check}");
        prop_assert_eq!(refused, !has_check(check_alone.as_bytes()));
        if refused {
            prop_assert_eq!(file.to_bytes(), bytes);
            return Ok(());
        }
        let after = file.to_bytes();
        let (at, new) = inserted(bytes, &after)?;

        let join = if text.as_str().ends_with(['.', '!', '?']) {
            " "
        } else {
            ". "
        };
        let suffix = from.map(|f| format!("  ({f})")).unwrap_or_default();
        let expected = format!("c{id} {text}{join}Check: {check}{suffix}");
        let item = new.last().unwrap();
        prop_assert_eq!(utf8(content_of(item)), format!("- {expected}"));
        prop_assert_eq!(file.find(id), Ok(expected.as_bytes()));
        prop_assert!(has_check(expected.as_bytes()));

        let named = |line: &Vec<u8>| {
            parse_line(CHORES, content_of(line)) == Entry::Heading(group.as_str().as_bytes())
        };
        let old = raw_lines(bytes);
        let lines = raw_lines(&after);
        let item_at = at + new.len() - 1;
        if old.iter().any(named) {
            // Into the first group of that name, as its last item, directly
            // under the heading when the group has none.
            prop_assert_eq!(new.len(), 1);
            let heading = lines.iter().position(named).unwrap();
            let end = (heading + 1..lines.len())
                .find(|index| is_heading(&lines[*index]))
                .unwrap_or(lines.len());
            prop_assert!((heading + 1..end).contains(&item_at));
            placed_last(CHORES, &lines, (heading + 1, end), item_at, heading + 1)
        } else {
            // A new group at the end of the file, set apart by a blank line
            // unless the file is empty or ends in one.
            prop_assert_eq!(item_at + 1, lines.len());
            let heading = format!("## {group}").into_bytes();
            let open_end = old
                .last()
                .is_some_and(|last| !content_of(last).iter().all(u8::is_ascii_whitespace));
            let block: Vec<&[u8]> = new.iter().map(|line| content_of(line)).collect();
            if open_end {
                prop_assert_eq!(&block[..2], &[b"", &heading[..]][..]);
                prop_assert_eq!(block.len(), 3);
            } else {
                prop_assert_eq!(&block[..1], &[&heading[..]][..]);
                prop_assert_eq!(block.len(), 2);
            }
            prop_assert!(named(&lines[item_at - 1]));
            Ok(())
        }
    }

    fn check_remove(kind: ListKind, bytes: &[u8], numbers: &[u32]) -> Result<(), TestCaseError> {
        let mut file = ListFile::parse(kind, bytes);
        let old = raw_lines(bytes);
        let count = |number: u32| {
            old.iter()
                .filter(|line| id_of(kind, line) == Some(number))
                .count()
        };
        match file.remove(numbers) {
            Ok(()) => {
                prop_assert!(numbers.iter().all(|number| count(*number) == 1));
                // An item removed takes the indented lines below it: no
                // indented line outlives its item or moves under another.
                let mut gone = false;
                let kept = old
                    .iter()
                    .filter(|line| {
                        gone = match id_of(kind, line) {
                            Some(id) => numbers.contains(&id),
                            None => gone && is_continuation(content_of(line)),
                        };
                        !gone
                    })
                    .cloned()
                    .collect::<Vec<_>>()
                    .concat();
                prop_assert_eq!(file.to_bytes(), kept);
            }
            Err(RemoveError { number, why }) => {
                prop_assert!(numbers.contains(&number));
                match why {
                    FindError::Missing => prop_assert_eq!(count(number), 0),
                    FindError::Duplicate(lines) => {
                        prop_assert!(lines > 1);
                        prop_assert_eq!(count(number), lines);
                    }
                }
                prop_assert_eq!(file.to_bytes(), bytes);
            }
        }
        Ok(())
    }

    /// The heading above a line, by index; `None` above the first.
    fn group_of(lines: &[Vec<u8>], at: usize) -> Option<usize> {
        lines[..at].iter().rposition(|line| is_heading(line))
    }

    /// Move locality: the item's line and the indented lines below it are
    /// taken out and put in whole at one place, directly before the target or
    /// directly after the target and its indented lines, and every other line
    /// keeps its bytes and its order. The only byte that changes is a
    /// terminator, given to the one line that had none (the last) when it is
    /// no longer last; the new terminator is the first line's.
    fn check_move(
        kind: ListKind,
        bytes: &[u8],
        (item, side, target): (u32, Side, u32),
    ) -> Result<(), TestCaseError> {
        let mut file = ListFile::parse(kind, bytes);
        let old = raw_lines(bytes);
        let why = |number: u32| match old
            .iter()
            .filter(|line| id_of(kind, line) == Some(number))
            .count()
        {
            0 => Some(FindError::Missing),
            1 => None,
            lines => Some(FindError::Duplicate(lines)),
        };
        let result = file.move_item(&moving(kind, item, side, target));
        let refusal = why(item)
            .map(MoveError::Item)
            .or_else(|| why(target).map(MoveError::Target));
        if let Some(refusal) = refusal {
            prop_assert_eq!(result, Err(refusal));
            prop_assert_eq!(file.to_bytes(), bytes);
            return Ok(());
        }
        prop_assert_eq!(result, Ok(()));
        let new = raw_lines(&file.to_bytes());
        prop_assert_eq!(new.len(), old.len());

        let line_of = |lines: &[Vec<u8>], number: u32| {
            lines
                .iter()
                .position(|line| id_of(kind, line) == Some(number))
                .unwrap()
        };
        let from = line_of(&old, item);
        let len = 1 + old[from + 1..]
            .iter()
            .take_while(|line| is_continuation(content_of(line)))
            .count();
        let at = line_of(&new, item);
        // The old line each new line is: the block taken out and put in.
        let mut order: Vec<usize> = (0..old.len()).collect();
        let block: Vec<usize> = order.drain(from..from + len).collect();
        order.splice(at..at, block);
        let terminator = file_terminator(&old);
        for (index, was) in order.iter().enumerate() {
            let mut expected = old[*was].clone();
            if !expected.ends_with(b"\n") && index + 1 < new.len() {
                expected.extend_from_slice(terminator);
            }
            prop_assert_eq!(&new[index], &expected, "line {}", index + 1);
        }

        let next_to = line_of(&new, target);
        match side {
            Side::Before => prop_assert_eq!(at + len, next_to),
            Side::After => {
                prop_assert!(next_to < at);
                prop_assert!(
                    new[next_to + 1..at]
                        .iter()
                        .all(|line| is_continuation(content_of(line)))
                );
                prop_assert!(
                    !new.get(at + len)
                        .is_some_and(|line| is_continuation(content_of(line)))
                );
            }
        }
        prop_assert_eq!(group_of(&new, at), group_of(&new, next_to));
        Ok(())
    }

    fn check_assign_ids(kind: ListKind, bytes: &[u8], mark: u32) -> Result<(), TestCaseError> {
        let mut file = ListFile::parse(kind, bytes);
        let highest = file.max_id().max(mark);
        let assigned = file.assign_ids(mark).unwrap();
        let after = file.to_bytes();

        // Fresh, and in file order.
        let expected: Vec<u32> = (highest + 1..).take(assigned.len()).collect();
        prop_assert_eq!(&assigned, &expected);

        let (old, new) = (raw_lines(bytes), raw_lines(&after));
        prop_assert_eq!(old.len(), new.len());
        let mut fresh = assigned.iter();
        for (old, new) in old.iter().zip(&new) {
            match parse_line(kind, content_of(old)) {
                Entry::Item { id: None, .. } => {
                    // Taking the new word out gives the line back.
                    let word = format!("{}{} ", kind.prefix(), fresh.next().unwrap());
                    prop_assert_eq!(new, &[b"- ", word.as_bytes(), &old[2..]].concat());
                }
                Entry::Item { id: Some(_), .. } | Entry::Heading(_) | Entry::Text(_) => {
                    prop_assert_eq!(new, old);
                }
            }
        }
        prop_assert_eq!(fresh.next(), None);
        // Each new id is on one line, and no item is left without.
        for id in &assigned {
            prop_assert!(file.find(*id).is_ok());
        }
        prop_assert!(
            new.iter()
                .all(|line| !is_item(kind, line) || id_of(kind, line).is_some())
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn any_bytes_are_written_back_as_read(kind in kind(), bytes in any::<Vec<u8>>()) {
            prop_assert_eq!(ListFile::parse(kind, &bytes).to_bytes(), bytes);
        }

        #[test]
        fn any_list_file_is_written_back_as_read(kind in kind(), bytes in file_bytes()) {
            let file = ListFile::parse(kind, &bytes);
            prop_assert_eq!(file.to_bytes(), &bytes[..]);
            prop_assert_eq!(file.entries().len(), raw_lines(&bytes).len());
        }

        #[test]
        fn add_backlog_inserts_one_line_and_touches_nothing_else(
            bytes in file_bytes(),
            mark in 0..20u32,
            text in input_line(),
            from in source(),
        ) {
            check_add_backlog(&bytes, mark, &text, from)?;
        }

        #[test]
        fn add_chore_inserts_its_lines_and_touches_nothing_else(
            bytes in file_bytes(),
            mark in 0..20u32,
            group in prop_oneof![
                4 => prop::sample::select(&GROUPS[..]).prop_map(line),
                1 => input_line(),
            ],
            text in input_line(),
            check in check_line(),
            from in source(),
        ) {
            check_add_chore(&bytes, mark, &group, (&text, &check), from)?;
        }

        #[test]
        fn remove_deletes_exactly_the_lines_of_the_ids_or_nothing(
            kind in kind(),
            bytes in file_bytes(),
            picks in prop::collection::vec((any::<prop::sample::Index>(), 1..14u32), 0..4),
        ) {
            // Mostly ids the file has (on one line or on several), now and
            // then one it may lack.
            let ids: Vec<u32> = raw_lines(&bytes).iter().filter_map(|l| id_of(kind, l)).collect();
            let numbers: Vec<u32> = picks
                .into_iter()
                .map(|(pick, other)| if ids.is_empty() || other > 11 { other } else { *pick.get(&ids) })
                .collect();
            check_remove(kind, &bytes, &numbers)?;
        }

        #[test]
        fn assign_ids_gives_each_item_without_an_id_a_fresh_one_and_touches_nothing_else(
            kind in kind(),
            bytes in file_bytes(),
            mark in 0..20u32,
        ) {
            check_assign_ids(kind, &bytes, mark)?;
        }
    }

    proptest! {
        // A line without a terminator moved, or moved after, needs the
        // file's last line to be the item's or the target's: a few cases in
        // a thousand. At 256 cases a broken terminator rule passed half the
        // runs; at this count it fails every run of 20.
        #![proptest_config(ProptestConfig::with_cases(4096))]

        #[test]
        fn move_puts_the_item_s_lines_next_to_the_target_and_touches_nothing_else(
            kind in kind(),
            bytes in move_file_bytes(),
            picks in (
                prop::option::weighted(0.75, any::<prop::sample::Index>()),
                prop::option::weighted(0.75, any::<prop::sample::Index>()),
                1..14u32,
            ),
            any_count in prop::bool::weighted(0.2),
            before in any::<bool>(),
        ) {
            // Mostly two ids on one line each, the moves that go through;
            // now and then ids on any number of lines, or one that no line
            // has (13). `None` picks the last item, as often the file's last
            // line: a line without a terminator, moved or moved after.
            let ids: Vec<u32> = raw_lines(&bytes).iter().filter_map(|l| id_of(kind, l)).collect();
            let once: Vec<u32> = ids
                .iter()
                .filter(|n| ids.iter().filter(|m| m == n).count() == 1)
                .copied()
                .collect();
            let pool = if once.len() >= 2 && !any_count { &once } else { &ids };
            let (item, target, other) = picks;
            let pick = |index: Option<prop::sample::Index>| match (index, pool.last()) {
                (_, None) => other,
                (Some(index), Some(_)) => *index.get(pool),
                (None, Some(last)) => *last,
            };
            let (item, target) = (pick(item), if other > 12 { other } else { pick(target) });
            let side = if before { Side::Before } else { Side::After };
            if item == target {
                prop_assert_eq!(
                    Move::new(list_id(kind, item), side, list_id(kind, target)),
                    Err(BadMove::Itself(list_id(kind, item)))
                );
            } else {
                check_move(kind, &bytes, (item, side, target))?;
            }
        }
    }
}
