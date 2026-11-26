//! The line grammar of the old list files (`chores.md`, `backlog.md`),
//! kept for reading what they held: the ids ever used, counted from their
//! history (`sync.rs`), and `migrate`'s conversion of a board to item files,
//! which reads each line as the old writer wrote it. And the input the
//! item commands share: a line of text.
//!
//! ```text
//! line    = heading | item | text
//! heading = "## " name                              chores.md groups
//! item    = "- " id (" " rest | end)  |  "- " rest  column 0; rest is not blank
//! text    = anything else, verbatim
//! written: "- c13 <text>. Check: <check>  (126)"    "- b42 <text>  (from 126)"
//! ```

use std::fmt;
use std::str::FromStr;

use crate::id::{ItemId, ListKind};
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

/// An item's text after its id, split before its trailing reference as
/// `add` writes it: blanks, then `(126)` on a chore (the form the check's
/// reading strips, [`has_check`]) or `(from 126)` on a backlog item, naming
/// an id. The reference keeps its blanks, so it is written back as it was;
/// empty when there is none, and a backlog text ending in `(126)` is text.
pub(crate) fn split_reference(kind: ListKind, text: &[u8]) -> (&[u8], &[u8]) {
    let found = text.strip_suffix(b")").and_then(|inside| {
        let open = inside.iter().rposition(|b| *b == b'(')?;
        let id = &inside[open + 1..];
        let id = match kind {
            ListKind::Chores => id,
            ListKind::Backlog => id.strip_prefix(b"from ")?,
        };
        str::from_utf8(id).ok()?.parse::<ItemId>().ok()?;
        let head = text[..open].trim_ascii_end();
        (head.len() < open).then_some(head.len())
    });
    match found {
        Some(at) => text.split_at(at),
        None => (text, &[]),
    }
}

/// Where a chore's check starts, in its line without the reference: at the
/// last `Check:`, as lint reads it, with the check after it. `None` when the
/// line has no check lint would count.
pub(crate) fn split_check(head: &[u8]) -> Option<(usize, &[u8])> {
    if !has_check(head) {
        return None;
    }
    let at = head.windows(CHECK.len()).rposition(|w| w == CHECK)?;
    Some((at, head[at + CHECK.len()..].trim_ascii()))
}

const CHECK: &[u8] = b"Check:";

/// A line continues the item above it when it is indented and not blank.
pub(crate) fn is_continuation(content: &[u8]) -> bool {
    matches!(content.first(), Some(b' ' | b'\t')) && !raw::is_blank(content)
}

/// One line of text from the command line, for an item's title, check or
/// group: trimmed, not empty, without a line break.
///
/// A line break would end a header value or the title line, and the rest
/// would be the body. Trimmed, because blanks at the ends carry nothing and
/// a trailing one would defeat the rule that joins a chore's title and its
/// check on its line.
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
            InvalidLine::LineBreak => {
                "the text has a line break; it is one line (an item's body is given with `--body`)"
            }
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

/// The list holds or once held the id `u32::MAX`: there is no next id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdsExhausted;

#[cfg(test)]
mod tests {
    use super::*;

    const CHORES: ListKind = ListKind::Chores;
    const BACKLOG: ListKind = ListKind::Backlog;

    fn line(text: &str) -> Line {
        Line::new(text).unwrap()
    }

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

    #[test]
    fn an_old_line_splits_into_text_reference_and_check() {
        assert_eq!(
            split_reference(CHORES, b"c1 Do it. Check: x  (126)"),
            (&b"c1 Do it. Check: x"[..], &b"  (126)"[..])
        );
        assert_eq!(
            split_reference(BACKLOG, b"b1 Later  (from c3)"),
            (&b"b1 Later"[..], &b"  (from c3)"[..])
        );
        assert_eq!(
            split_reference(BACKLOG, b"b1 Later  (126)"),
            (&b"b1 Later  (126)"[..], &b""[..])
        );
        assert_eq!(
            split_check(b"c1 Do it. Check: first. Check: second"),
            Some((24, &b"second"[..]))
        );
        assert_eq!(split_check(b"c1 Do it"), None);
        assert!(is_continuation(b"  indented"));
        assert!(!is_continuation(b"   "));
        assert!(!is_continuation(b"- c1 item"));
    }
}
