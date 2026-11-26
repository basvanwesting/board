//! The header of a board file: the leading run of `key: value` lines that a
//! brief, a chore and a backlog item all start with. One parser for the
//! three, so a key reads, is rewritten and is kept the same way in each.
//!
//! ```text
//! key = [a-z]+ ":" (" " value | end)
//! ```
//!
//! Lossless: a line is kept with its terminator, a key the tool does not
//! know is a line like any other, and only the line of a key the tool sets
//! changes.

use crate::raw;

/// The leading header lines of a file, raw (terminators included).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Header {
    lines: Vec<Vec<u8>>,
}

/// The key of a header line: `^[a-z]+:( |$)`.
fn key_of(content: &[u8]) -> Option<&[u8]> {
    let colon = content.iter().position(|b| !b.is_ascii_lowercase())?;
    let (key, rest) = content.split_at(colon);
    let after = rest.strip_prefix(b":")?;
    (!key.is_empty() && matches!(after.first(), None | Some(b' '))).then_some(key)
}

impl Header {
    /// The header of `bytes` and what follows it. Never fails: a file that
    /// does not start with a key line has an empty header.
    pub(crate) fn split(bytes: &[u8]) -> (Header, &[u8]) {
        let lines: Vec<Vec<u8>> = raw::lines(bytes)
            .take_while(|line| key_of(raw::content(line)).is_some())
            .map(<[u8]>::to_vec)
            .collect();
        let len: usize = lines.iter().map(Vec::len).sum();
        (Header { lines }, &bytes[len..])
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        self.lines.concat()
    }

    pub(crate) fn first_line(&self) -> Option<&[u8]> {
        self.lines.first().map(Vec::as_slice)
    }

    pub(crate) fn last_line_mut(&mut self) -> Option<&mut Vec<u8>> {
        self.lines.last_mut()
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| key_of(raw::content(line)) == Some(key.as_bytes()))
    }

    /// The trimmed value of the first line with this key. A key written
    /// twice by hand reads as its first line, which is also the one a write
    /// changes.
    pub(crate) fn get(&self, key: &str) -> Option<&[u8]> {
        let line = raw::content(&self.lines[self.position(key)?]);
        Some(line[key.len() + ":".len()..].trim_ascii())
    }

    /// The keys, in file order, each as often as it is written.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &[u8]> {
        self.lines
            .iter()
            .filter_map(|line| key_of(raw::content(line)))
    }

    /// Rewrites the key's line in place, or adds the key at the end.
    pub(crate) fn set(&mut self, key: &str, value: &str) {
        let line = format!("{key}: {value}").into_bytes();
        match self.position(key) {
            Some(at) => {
                let (_, terminator) = raw::split_terminator(&self.lines[at]);
                // A last line without a terminator ends the file; it keeps
                // that.
                self.lines[at] = [line.as_slice(), terminator].concat();
            }
            None => {
                let terminator = raw::terminator_of(self.first_line());
                // Only a header that runs to the end of the file can end
                // without a terminator.
                if let Some(last) = self.lines.last_mut()
                    && !last.ends_with(b"\n")
                {
                    last.extend_from_slice(terminator);
                }
                self.lines.push([line.as_slice(), terminator].concat());
            }
        }
    }

    /// Rewrites the key's line in place, or adds the key right after the
    /// last line of one of the keys `after` (first in the header without
    /// any): where the board writes it in a new file.
    pub(crate) fn set_after(&mut self, key: &str, value: &str, after: &[&str]) {
        if self.position(key).is_some() {
            self.set(key, value);
            return;
        }
        let terminator = raw::terminator_of(self.first_line());
        if let Some(last) = self.lines.last_mut()
            && !last.ends_with(b"\n")
        {
            last.extend_from_slice(terminator);
        }
        let at = self
            .lines
            .iter()
            .rposition(|line| {
                key_of(raw::content(line)).is_some_and(|k| after.iter().any(|a| a.as_bytes() == k))
            })
            .map_or(0, |at| at + 1);
        let line = format!("{key}: {value}").into_bytes();
        self.lines
            .insert(at, [line.as_slice(), terminator].concat());
    }

    /// Deletes every line with this key.
    pub(crate) fn remove(&mut self, key: &str) {
        self.lines
            .retain(|line| key_of(raw::content(line)) != Some(key.as_bytes()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_is_the_leading_key_lines_and_keeps_their_bytes() {
        let (header, rest) = Header::split(b"id: c1\nzz: kept \r\nNot: a key\nx: y\n");
        assert_eq!(header.to_bytes(), b"id: c1\nzz: kept \r\n");
        assert_eq!(rest, b"Not: a key\nx: y\n");
        assert_eq!(header.get("zz"), Some(&b"kept"[..]));
        assert_eq!(header.keys().collect::<Vec<_>>(), [&b"id"[..], b"zz"]);
        let (empty, rest) = Header::split(b"\n# title\n");
        assert_eq!(empty.to_bytes(), b"");
        assert_eq!(rest, b"\n# title\n");
        // `key:value` and `Key:` are not keys.
        assert_eq!(Header::split(b"id:c1\n").0.to_bytes(), b"");
        assert_eq!(Header::split(b"Id: c1\n").0.to_bytes(), b"");
        assert_eq!(Header::split(b"check:\n").0.get("check"), Some(&b""[..]));
    }

    #[test]
    fn set_rewrites_its_line_or_adds_one_and_remove_deletes() {
        let (mut header, _) = Header::split(b"id: c1\r\nother: x\n");
        header.set("id", "c2");
        header.set("group", "Home");
        assert_eq!(header.to_bytes(), b"id: c2\r\nother: x\ngroup: Home\r\n");
        header.remove("other");
        assert_eq!(header.to_bytes(), b"id: c2\r\ngroup: Home\r\n");
        let (mut open, _) = Header::split(b"id: c1");
        open.set("group", "3");
        assert_eq!(open.to_bytes(), b"id: c1\ngroup: 3\n");
        let (mut after, _) = Header::split(b"x: 1\nid: c1\nother: y\n");
        after.set_after("group", "G", &["id"]);
        assert_eq!(after.to_bytes(), b"x: 1\nid: c1\ngroup: G\nother: y\n");
        let (mut first, _) = Header::split(b"other: y\n");
        first.set_after("group", "G", &["id"]);
        assert_eq!(first.to_bytes(), b"group: G\nother: y\n");
        // After the last of the keys there are.
        let (mut many, _) = Header::split(b"id: c1\ngroup: g\nfrom: 1\n");
        many.set_after("after", "c2", &["id", "group", "check"]);
        assert_eq!(many.to_bytes(), b"id: c1\ngroup: g\nafter: c2\nfrom: 1\n");
    }
}
