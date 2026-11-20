//! Lines as the file has them: bytes in, bytes out.
//!
//! A board file is edited by hand as well as by the tool, so a parser keeps
//! every line's bytes, terminator included, and a writer concatenates them. A
//! file that is not UTF-8 or mixes `\n` with `\r\n` goes through unchanged.

/// Splits after every `\n`. Concatenating the lines gives `bytes` back; only
/// the last line can lack a terminator.
pub(crate) fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split_inclusive(|b| *b == b'\n')
}

/// A raw line as `(content, terminator)`; the terminator is `\n`, `\r\n` or
/// empty.
pub(crate) fn split_terminator(raw: &[u8]) -> (&[u8], &'static [u8]) {
    if let Some(content) = raw.strip_suffix(b"\r\n") {
        (content, b"\r\n")
    } else if let Some(content) = raw.strip_suffix(b"\n") {
        (content, b"\n")
    } else {
        (raw, b"")
    }
}

pub(crate) fn content(raw: &[u8]) -> &[u8] {
    split_terminator(raw).0
}

/// The terminator a new line gets: the one of the file's first line, `\n`
/// when that line has none or the file is empty.
pub(crate) fn terminator_of(first_line: Option<&[u8]>) -> &'static [u8] {
    match first_line.map(split_terminator) {
        Some((_, terminator)) if !terminator.is_empty() => terminator,
        _ => b"\n",
    }
}

pub(crate) fn is_blank(content: &[u8]) -> bool {
    content.iter().all(u8::is_ascii_whitespace)
}
