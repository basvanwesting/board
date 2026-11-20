//! A brief: header lines, then the document.
//!
//! ```text
//! id: 128
//! status: started
//! base: <sha>
//! created: 2026-11-20T16:31:07Z
//! queued: 2026-11-20T16:40:12Z
//! started: 2026-11-20T17:02:11Z
//!
//! # Title
//! ## Task
//! ```

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::id::{BacklogId, BriefId};
use crate::raw;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    Draft,
    Queued,
    Started,
    Blocked,
    Reported,
    Filed,
}

impl Status {
    pub const ALL: [Status; 6] = [
        Status::Draft,
        Status::Queued,
        Status::Started,
        Status::Blocked,
        Status::Reported,
        Status::Filed,
    ];

    /// The word in the `status:` line, and the key of the status' stamp.
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::Queued => "queued",
            Status::Started => "started",
            Status::Blocked => "blocked",
            Status::Reported => "reported",
            Status::Filed => "filed",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = UnknownStatus;

    fn from_str(s: &str) -> Result<Status, UnknownStatus> {
        Status::ALL
            .into_iter()
            .find(|status| status.as_str() == s)
            .ok_or_else(|| UnknownStatus(s.to_owned()))
    }
}

/// A word that is no status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStatus(pub String);

impl fmt::Display for UnknownStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let known = Status::ALL.map(Status::as_str).join(", ");
        write!(f, "`{}` is not a status ({known})", self.0)
    }
}

impl std::error::Error for UnknownStatus {}

/// The moves a command can make on a brief's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transition {
    Queue,
    Claim,
    Block,
    Resume,
    Report,
    File,
}

impl Transition {
    pub const ALL: [Transition; 6] = [
        Transition::Queue,
        Transition::Claim,
        Transition::Block,
        Transition::Resume,
        Transition::Report,
        Transition::File,
    ];

    /// The status after this move, `None` when it is not allowed from `from`.
    ///
    /// The whole state machine is this one match, and it has no `_` arm: a
    /// new status or transition does not compile until its place here is
    /// decided.
    pub const fn apply(self, from: Status) -> Option<Status> {
        use Status::{Blocked, Draft, Filed, Queued, Reported, Started};
        use Transition::{Block, Claim, File, Queue, Report, Resume};
        match (self, from) {
            (Queue, Draft) => Some(Queued),
            (Claim, Queued) => Some(Started),
            (Block, Started) => Some(Blocked),
            (Resume, Blocked) => Some(Started),
            (Report, Started) => Some(Reported),
            (File, Reported) => Some(Filed),
            (Queue, Queued | Started | Blocked | Reported | Filed)
            | (Claim, Draft | Started | Blocked | Reported | Filed)
            | (Block, Draft | Queued | Blocked | Reported | Filed)
            | (Resume, Draft | Queued | Started | Reported | Filed)
            | (Report, Draft | Queued | Blocked | Reported | Filed)
            | (File, Draft | Queued | Started | Blocked | Filed) => None,
        }
    }

    /// Whether the move stamps the key named after the new status.
    ///
    /// Resume does not: it returns to `started`, and `started:` stays the
    /// first start, which is what "in flight for how long" counts from.
    const fn stamps(self) -> bool {
        match self {
            Transition::Queue
            | Transition::Claim
            | Transition::Block
            | Transition::Report
            | Transition::File => true,
            Transition::Resume => false,
        }
    }
}

/// A second in UTC, written `2026-11-20T16:31:07Z`. Years 1970 to 9999, so
/// the text has one width and sorts as the time does.
///
/// Hand-rolled: the tool needs "now" as text and an order on stamps, and a
/// date crate for that is a dependency to vet at every update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp(u64);

const SECONDS_PER_DAY: u64 = 86_400;

impl Stamp {
    /// 9999-12-31T23:59:59Z.
    const MAX: u64 = 253_402_300_799;

    /// From seconds since 1970-01-01T00:00:00Z; `None` past the year 9999.
    pub const fn from_unix(seconds: u64) -> Option<Stamp> {
        if seconds <= Stamp::MAX {
            Some(Stamp(seconds))
        } else {
            None
        }
    }

    pub const fn unix(self) -> u64 {
        self.0
    }

    /// `None` for a time outside the years 1970 to 9999.
    pub fn from_system_time(time: SystemTime) -> Option<Stamp> {
        let since_epoch = time.duration_since(UNIX_EPOCH).ok()?;
        Stamp::from_unix(since_epoch.as_secs())
    }

    /// The clock's second; `None` when the clock is outside the years 1970 to
    /// 9999 (a setup failure, not something to write into a brief).
    pub fn now() -> Option<Stamp> {
        Stamp::from_system_time(SystemTime::now())
    }
}

// The two calendar conversions are Howard Hinnant's `civil_from_days` and
// `days_from_civil` (public domain), on years counted from March so the leap
// day is the last of its year.

/// Days since 1970-01-01 to `(year, month, day)`.
const fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// `(year, month, day)` to days since 1970-01-01; `None` before that day.
/// Takes any two-digit month and day, in the calendar or not: the caller
/// checks the result by converting it back.
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    let year = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = year / 400;
    let year_of_era = year % 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day.checked_sub(1)?;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era).checked_sub(719_468)
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.0 / SECONDS_PER_DAY);
        let second_of_day = self.0 % SECONDS_PER_DAY;
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            second_of_day / 3_600,
            second_of_day / 60 % 60,
            second_of_day % 60
        )
    }
}

/// Text that is not a stamp as the tool writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidStamp(pub String);

impl fmt::Display for InvalidStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a time like 2026-11-20T16:31:07Z",
            self.0.escape_debug()
        )
    }
}

impl std::error::Error for InvalidStamp {}

impl FromStr for Stamp {
    type Err = InvalidStamp;

    /// Reads exactly what `Display` writes, nothing else.
    fn from_str(s: &str) -> Result<Stamp, InvalidStamp> {
        fn read(s: &[u8]) -> Option<Stamp> {
            let s: &[u8; 20] = s.try_into().ok()?;
            let field = |range: std::ops::Range<usize>| {
                s[range].iter().try_fold(0, |n, b| match b {
                    b'0'..=b'9' => Some(n * 10 + u64::from(b - b'0')),
                    _ => None,
                })
            };
            let days = days_from_civil(field(0..4)?, field(5..7)?, field(8..10)?)?;
            let second_of_day = field(11..13)? * 3_600 + field(14..16)? * 60 + field(17..19)?;
            Stamp::from_unix(days * SECONDS_PER_DAY + second_of_day)
        }
        // The fields are not checked one by one. A wrong separator, a month
        // 13, a February 30 or an hour 24 still gives some second, one that
        // prints as different text: printing it back is the one definition
        // of a valid stamp.
        read(s.as_bytes())
            .filter(|stamp| stamp.to_string() == s)
            .ok_or_else(|| InvalidStamp(s.to_owned()))
    }
}

/// Why a brief has no usable status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadStatus {
    /// No `status:` line in the header.
    Missing,
    /// The value as found (shown lossily when it is not UTF-8).
    Unknown(String),
}

/// A move that was not made; the brief is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    BadStatus(BadStatus),
    /// The move is not allowed from this status.
    NotFrom(Status),
}

/// The key of a header line: `^[a-z]+:( |$)`.
fn header_key(content: &[u8]) -> Option<&[u8]> {
    let colon = content.iter().position(|b| !b.is_ascii_lowercase())?;
    let (key, rest) = content.split_at(colon);
    let after = rest.strip_prefix(b":")?;
    (!key.is_empty() && matches!(after.first(), None | Some(b' '))).then_some(key)
}

/// A brief file, lossless: writing it back gives the bytes that were read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// The leading run of header lines, raw (terminator included). A key the
    /// tool does not know is a line like any other here, kept as it is.
    header: Vec<Vec<u8>>,
    /// Everything after the header, untouched.
    body: Vec<u8>,
}

/// The section that runs to the end of the file, whatever lines it holds.
const REPORT: &str = "Report";

/// A `## Name` section of a brief's body: the lines after its heading, up to
/// the next section's heading or the end of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section<'a> {
    /// The heading's text after `## `, trimmed.
    pub name: &'a [u8],
    /// The section's lines, raw, without the heading line.
    pub text: &'a [u8],
}

impl Section<'_> {
    /// Names are matched ignoring ASCII case: `## Out of Scope` is the
    /// section `Out of scope`, as a reader takes it.
    pub fn is_named(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name.as_bytes())
    }

    /// Whether a line of the section is not blank.
    pub fn has_text(&self) -> bool {
        raw::lines(self.text).any(|line| !raw::is_blank(raw::content(line)))
    }
}

impl Brief {
    /// A new brief's file: `id`, `status: draft`, `base`, `created` and one
    /// blank line. The planner writes the rest.
    pub fn new(id: BriefId, base: &str, created: Stamp) -> Brief {
        Brief::parse(
            format!("id: {id}\nstatus: draft\nbase: {base}\ncreated: {created}\n\n").as_bytes(),
        )
    }

    /// Never fails: a file without header lines (an archived brief placed by
    /// hand) is all body.
    pub fn parse(bytes: &[u8]) -> Brief {
        let header: Vec<Vec<u8>> = raw::lines(bytes)
            .take_while(|line| header_key(raw::content(line)).is_some())
            .map(<[u8]>::to_vec)
            .collect();
        let header_len: usize = header.iter().map(Vec::len).sum();
        Brief {
            header,
            body: bytes[header_len..].to_vec(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        [self.header.concat().as_slice(), &self.body].concat()
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.header
            .iter()
            .position(|line| header_key(raw::content(line)) == Some(key.as_bytes()))
    }

    /// The trimmed value of the first header line with this key. A key
    /// written twice by hand reads as its first line, which is also the one
    /// a write changes.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        let line = raw::content(&self.header[self.position(key)?]);
        Some(line[key.len() + ":".len()..].trim_ascii())
    }

    /// Rewrites the key's line in place, or adds the key at the end of the
    /// header. Private: `status` has one writer, [`Brief::transition`].
    fn set(&mut self, key: &str, value: &str) {
        let line = format!("{key}: {value}").into_bytes();
        match self.position(key) {
            Some(at) => {
                let (_, terminator) = raw::split_terminator(&self.header[at]);
                self.header[at] = [line.as_slice(), terminator].concat();
            }
            None => {
                let terminator = raw::terminator_of(self.header.first().map(Vec::as_slice));
                // Only a header that runs to the end of the file can end
                // without a terminator.
                if let Some(last) = self.header.last_mut()
                    && !last.ends_with(b"\n")
                {
                    last.extend_from_slice(terminator);
                }
                self.header.push([line.as_slice(), terminator].concat());
            }
        }
    }

    /// Writes the backlog items the brief resolves, which `file` deletes:
    /// `closes: b41, b52`.
    pub(crate) fn set_closes(&mut self, ids: &[BacklogId]) {
        let ids: Vec<String> = ids.iter().map(BacklogId::to_string).collect();
        self.set("closes", &ids.join(", "));
    }

    /// The `## ` sections of the body, in file order.
    ///
    /// A heading is a line that starts with `## ` outside a code fence (a
    /// line whose first non-blank characters are ```` ``` ````, which opens
    /// or closes one): a brief quotes markdown, and a heading in a quoted
    /// block is not the brief's. `## Report` runs to the end of the file, so
    /// a report may hold `## ` lines of its own; the tool keeps it last.
    pub fn sections(&self) -> Vec<Section<'_>> {
        let body = self.body.as_slice();
        let mut sections = Vec::new();
        // The open section: its name and where its text starts.
        let mut open: Option<(&[u8], usize)> = None;
        let mut fenced = false;
        let mut at = 0;
        for line in raw::lines(body) {
            let start = at;
            at += line.len();
            let content = raw::content(line);
            if content.trim_ascii_start().starts_with(b"```") {
                fenced = !fenced;
                continue;
            }
            let Some(name) = content.strip_prefix(b"## ").filter(|_| !fenced) else {
                continue;
            };
            if let Some((name, from)) = open {
                sections.push(Section {
                    name,
                    text: &body[from..start],
                });
            }
            let name = name.trim_ascii();
            open = Some((name, at));
            if name.eq_ignore_ascii_case(REPORT.as_bytes()) {
                break;
            }
        }
        if let Some((name, from)) = open {
            sections.push(Section {
                name,
                text: &body[from..],
            });
        }
        sections
    }

    /// The first section with this name. A name written twice by hand reads
    /// as its first section, like a header key.
    pub fn section(&self, name: &str) -> Option<Section<'_>> {
        self.sections()
            .into_iter()
            .find(|section| section.is_named(name))
    }

    /// The status the header states. Not a field: it is read off the header
    /// when asked, so it cannot differ from what the file says.
    pub fn status(&self) -> Result<Status, BadStatus> {
        let value = self.get("status").ok_or(BadStatus::Missing)?;
        str::from_utf8(value)
            .ok()
            .and_then(|word| word.parse().ok())
            .ok_or_else(|| BadStatus::Unknown(String::from_utf8_lossy(value).into_owned()))
    }

    /// Makes the move: rewrites `status:` and stamps the key named after the
    /// new status with `now`. The only writer of the status; a move that is
    /// not allowed changes nothing.
    pub fn transition(
        &mut self,
        transition: Transition,
        now: Stamp,
    ) -> Result<Status, TransitionError> {
        let from = self.status().map_err(TransitionError::BadStatus)?;
        let to = transition
            .apply(from)
            .ok_or(TransitionError::NotFrom(from))?;
        self.set("status", to.as_str());
        if transition.stamps() {
            self.set(to.as_str(), &now.to_string());
        }
        Ok(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::Duration;

    fn stamp(text: &str) -> Stamp {
        text.parse().unwrap()
    }

    fn utf8(bytes: &[u8]) -> &str {
        str::from_utf8(bytes).unwrap()
    }

    // ---- status and transitions ----

    #[test]
    fn a_status_reads_back_as_printed() {
        for status in Status::ALL {
            assert_eq!(status.to_string().parse(), Ok(status));
        }
        assert_eq!(
            "Started".parse::<Status>().unwrap_err().to_string(),
            "`Started` is not a status (draft, queued, started, blocked, reported, filed)"
        );
    }

    /// A row or column the lists lack would also be missing from the table
    /// test below; these matches stop compiling when a variant is added.
    #[test]
    fn the_lists_of_variants_are_complete() {
        for (index, status) in Status::ALL.into_iter().enumerate() {
            let expected = match status {
                Status::Draft => 0,
                Status::Queued => 1,
                Status::Started => 2,
                Status::Blocked => 3,
                Status::Reported => 4,
                Status::Filed => 5,
            };
            assert_eq!(index, expected);
        }
        for (index, transition) in Transition::ALL.into_iter().enumerate() {
            let expected = match transition {
                Transition::Queue => 0,
                Transition::Claim => 1,
                Transition::Block => 2,
                Transition::Resume => 3,
                Transition::Report => 4,
                Transition::File => 5,
            };
            assert_eq!(index, expected);
        }
    }

    #[test]
    fn the_transition_table() {
        use Status::{Blocked, Filed, Queued, Reported, Started};
        const NO: Option<Status> = None;
        #[rustfmt::skip]
        let table: [(Transition, [Option<Status>; 6]); 6] = [
            // from:              draft         queued         started         blocked        reported     filed
            (Transition::Queue,  [Some(Queued), NO,            NO,             NO,            NO,          NO]),
            (Transition::Claim,  [NO,           Some(Started), NO,             NO,            NO,          NO]),
            (Transition::Block,  [NO,           NO,            Some(Blocked),  NO,            NO,          NO]),
            (Transition::Resume, [NO,           NO,            NO,             Some(Started), NO,          NO]),
            (Transition::Report, [NO,           NO,            Some(Reported), NO,            NO,          NO]),
            (Transition::File,   [NO,           NO,            NO,             NO,            Some(Filed), NO]),
        ];
        assert_eq!(table.map(|(transition, _)| transition), Transition::ALL);
        for (transition, row) in table {
            for (from, expected) in Status::ALL.into_iter().zip(row) {
                assert_eq!(
                    transition.apply(from),
                    expected,
                    "{transition:?} from {from}"
                );
            }
        }
    }

    fn reachable(from: Status) -> Vec<Status> {
        let mut seen = vec![from];
        let mut next = 0;
        while let Some(status) = seen.get(next).copied() {
            for to in Transition::ALL.into_iter().filter_map(|t| t.apply(status)) {
                if !seen.contains(&to) {
                    seen.push(to);
                }
            }
            next += 1;
        }
        seen
    }

    #[test]
    fn every_brief_can_be_filed_and_a_filed_one_stays_filed() {
        let mut from_draft = reachable(Status::Draft);
        from_draft.sort();
        assert_eq!(from_draft, Status::ALL, "a status no draft can reach");
        for status in Status::ALL {
            assert!(reachable(status).contains(&Status::Filed), "{status}");
        }
        assert_eq!(reachable(Status::Filed), [Status::Filed]);
    }

    // ---- stamp ----

    /// From `date -u -d @<seconds> +%Y-%m-%dT%H:%M:%SZ`.
    const KNOWN: [(u64, &str); 10] = [
        (0, "1970-01-01T00:00:00Z"),
        (68_169_600, "1972-02-29T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (951_868_799, "2000-02-29T23:59:59Z"),
        (1_000_000_000, "2001-09-09T01:46:40Z"),
        (1_709_251_199, "2024-02-29T23:59:59Z"),
        (1_709_251_200, "2024-03-01T00:00:00Z"),
        (1_795_192_267, "2026-11-20T16:31:07Z"),
        (4_107_542_399, "2100-02-28T23:59:59Z"),
        (253_402_300_799, "9999-12-31T23:59:59Z"),
    ];

    #[test]
    fn known_seconds() {
        for (seconds, text) in KNOWN {
            let stamp = Stamp::from_unix(seconds).unwrap();
            assert_eq!(stamp.to_string(), text);
            assert_eq!(text.parse(), Ok(stamp));
            assert_eq!(stamp.unix(), seconds);
        }
        assert_eq!(Stamp::from_unix(253_402_300_800), None);
    }

    #[test]
    fn text_that_is_not_a_stamp() {
        for text in [
            "",
            "2026-11-20",
            "2026-11-20T16:31:07",
            "2026-11-20T16:31:07z",
            "2026-11-20 16:31:07Z",
            "2026-11-20T16:31:07Z ",
            " 2026-11-20T16:31:07Z",
            "2026-11-20T16:31:07+00",
            "2026-11-20T16:31:7Z",
            "2026/11/20T16:31:07Z",
            "2026-13-01T00:00:00Z",
            "2026-00-10T00:00:00Z",
            "2026-11-00T00:00:00Z",
            "2026-11-31T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2100-02-29T00:00:00Z",
            "2026-11-20T24:00:00Z",
            "2026-11-20T23:60:00Z",
            "2026-11-20T23:59:60Z",
            "1969-12-31T23:59:59Z",
            "0000-01-01T00:00:00Z",
            "+026-11-20T16:31:07Z",
            "２０２６-11-20T16:31:07Z",
        ] {
            assert!(text.parse::<Stamp>().is_err(), "{text}");
        }
        assert_eq!(
            "a\nb".parse::<Stamp>().unwrap_err().to_string(),
            "`a\\nb` is not a time like 2026-11-20T16:31:07Z"
        );
    }

    #[test]
    fn the_clock_gives_a_stamp_inside_the_range_only() {
        let at = |seconds| UNIX_EPOCH + Duration::from_secs(seconds);
        assert_eq!(
            Stamp::from_system_time(at(1_795_192_267)),
            Stamp::from_unix(1_795_192_267)
        );
        assert_eq!(
            Stamp::from_system_time(UNIX_EPOCH - Duration::from_secs(1)),
            None
        );
        assert_eq!(Stamp::from_system_time(at(Stamp::MAX + 1)), None);
        assert!(Stamp::now().is_some());
    }

    fn leap(year: u64) -> bool {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    }

    /// The calendar counted out year by year and month by month, as a second
    /// opinion on the closed-form conversion.
    fn counted_out(seconds: u64) -> String {
        let mut days = seconds / 86_400;
        let mut year = 1970;
        while days >= 365 + u64::from(leap(year)) {
            days -= 365 + u64::from(leap(year));
            year += 1;
        }
        let february = 28 + u64::from(leap(year));
        let mut month = 1;
        for length in [31, february, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31] {
            if days < length {
                break;
            }
            days -= length;
            month += 1;
        }
        let (h, m, s) = (seconds / 3_600 % 24, seconds / 60 % 60, seconds % 60);
        format!("{year:04}-{month:02}-{:02}T{h:02}:{m:02}:{s:02}Z", days + 1)
    }

    /// A second on a day where the calendar turns: around new year and the
    /// end of February, in any year. A wrong leap rule shows on a few days
    /// per century only, which seconds drawn evenly would not hit.
    fn turning_second() -> impl Strategy<Value = u64> {
        let day_of_year = prop::sample::select(&[0, 1, 30, 31, 57, 58, 59, 60, 363, 364, 365][..]);
        (1970..=9999u64, day_of_year, 0..86_400u64).prop_map(|(year, day, second)| {
            let days_before: u64 = (1970..year).map(|y| 365 + u64::from(leap(y))).sum();
            ((days_before + day) * 86_400 + second).min(Stamp::MAX)
        })
    }

    fn any_stamp() -> impl Strategy<Value = Stamp> {
        prop_oneof![
            // The years in use, the whole range, the days that go wrong.
            2 => 1_700_000_000..2_000_000_000u64,
            1 => 0..=Stamp::MAX,
            2 => turning_second(),
        ]
        .prop_map(|seconds| Stamp::from_unix(seconds).unwrap())
    }

    proptest! {
        #[test]
        fn a_stamp_prints_the_calendar_date(stamp in any_stamp()) {
            prop_assert_eq!(stamp.to_string(), counted_out(stamp.unix()));
        }

        #[test]
        fn a_stamp_reads_back_as_printed(stamp in any_stamp()) {
            prop_assert_eq!(stamp.to_string().parse(), Ok(stamp));
        }

        #[test]
        fn stamps_sort_as_their_text(a in any_stamp(), b in any_stamp()) {
            prop_assert_eq!(a.cmp(&b), a.to_string().cmp(&b.to_string()));
        }

        /// Whatever reads as a stamp is text the tool would have written.
        #[test]
        fn text_that_reads_as_a_stamp_is_printed_the_same(
            text in "[0-9]{4}-[01][0-9]-[0-3][0-9]T[0-2][0-9]:[0-6][0-9]:[0-6][0-9]Z",
        ) {
            if let Ok(stamp) = text.parse::<Stamp>() {
                prop_assert_eq!(stamp.to_string(), text);
            }
        }
    }

    // ---- header ----

    const NEW: &str = "id: 128\nstatus: draft\nbase: 86743aa\ncreated: 2026-11-20T16:31:07Z\n\n# Title\n\n## Task\n";

    #[test]
    fn the_header_is_the_leading_run_of_key_lines() {
        let brief = Brief::parse(NEW.as_bytes());
        assert_eq!(brief.get("id"), Some(&b"128"[..]));
        assert_eq!(brief.get("base"), Some(&b"86743aa"[..]));
        assert_eq!(brief.get("closes"), None);
        assert_eq!(brief.status(), Ok(Status::Draft));
        assert_eq!(utf8(brief.body()), "\n# Title\n\n## Task\n");
    }

    #[test]
    fn what_ends_the_header() {
        for (bytes, body) in [
            ("status: draft\n\nnote: body\n", "\nnote: body\n"),
            ("status: draft\nstatus:queued\n", "status:queued\n"),
            ("status: draft\nStatus: queued\n", "Status: queued\n"),
            ("status: draft\nmy-key: x\n", "my-key: x\n"),
            ("status: draft\n: x\n", ": x\n"),
            ("status: draft\n status: queued\n", " status: queued\n"),
            ("status: draft\nplain text\n", "plain text\n"),
            ("# Title\nstatus: draft\n", "# Title\nstatus: draft\n"),
            ("", ""),
        ] {
            assert_eq!(
                utf8(Brief::parse(bytes.as_bytes()).body()),
                body,
                "{bytes:?}"
            );
        }
        // A key alone on its line is a header line with an empty value.
        let empty_value = Brief::parse(b"closes:\r\nstatus:\r\n\r\n");
        assert_eq!(empty_value.get("closes"), Some(&b""[..]));
        assert_eq!(utf8(empty_value.body()), "\r\n");
    }

    #[test]
    fn a_status_the_tool_does_not_know_is_reported_as_found() {
        let status = |bytes: &[u8]| Brief::parse(bytes).status();
        assert_eq!(status(b"status:   started  \n"), Ok(Status::Started));
        assert_eq!(status(b"status: started\r\n"), Ok(Status::Started));
        assert_eq!(status(b"id: 1\n"), Err(BadStatus::Missing));
        assert_eq!(status(b"# Title\n"), Err(BadStatus::Missing));
        assert_eq!(
            status(b"status: wip\n"),
            Err(BadStatus::Unknown("wip".into()))
        );
        assert_eq!(status(b"status:\n"), Err(BadStatus::Unknown("".into())));
        assert_eq!(
            status(b"status: Started\n"),
            Err(BadStatus::Unknown("Started".into()))
        );
        assert_eq!(
            status(b"status: \xff\n"),
            Err(BadStatus::Unknown("\u{fffd}".into()))
        );
        // The first of two lines is the one that counts.
        assert_eq!(
            status(b"status: queued\nstatus: filed\n"),
            Ok(Status::Queued)
        );
    }

    #[test]
    fn a_brief_walks_from_draft_to_filed() {
        let mut brief = Brief::parse(NEW.as_bytes());
        let mut walk = |transition, now: &str| brief.transition(transition, stamp(now));
        assert_eq!(
            walk(Transition::Queue, "2026-11-20T16:40:12Z"),
            Ok(Status::Queued)
        );
        assert_eq!(
            walk(Transition::Claim, "2026-11-20T17:02:11Z"),
            Ok(Status::Started)
        );
        assert_eq!(
            walk(Transition::Block, "2026-11-20T17:10:00Z"),
            Ok(Status::Blocked)
        );
        assert_eq!(
            walk(Transition::Resume, "2026-11-20T17:20:00Z"),
            Ok(Status::Started)
        );
        assert_eq!(
            walk(Transition::Block, "2026-11-20T17:30:00Z"),
            Ok(Status::Blocked)
        );
        assert_eq!(
            walk(Transition::Resume, "2026-11-20T17:40:00Z"),
            Ok(Status::Started)
        );
        assert_eq!(
            walk(Transition::Report, "2026-11-20T18:00:00Z"),
            Ok(Status::Reported)
        );
        assert_eq!(
            walk(Transition::File, "2026-11-20T19:00:00Z"),
            Ok(Status::Filed)
        );
        // `started:` is the first start; `blocked:` the latest block.
        assert_eq!(
            utf8(&brief.to_bytes()),
            "id: 128\n\
             status: filed\n\
             base: 86743aa\n\
             created: 2026-11-20T16:31:07Z\n\
             queued: 2026-11-20T16:40:12Z\n\
             started: 2026-11-20T17:02:11Z\n\
             blocked: 2026-11-20T17:30:00Z\n\
             reported: 2026-11-20T18:00:00Z\n\
             filed: 2026-11-20T19:00:00Z\n\
             \n# Title\n\n## Task\n"
        );
    }

    #[test]
    fn a_move_that_is_not_allowed_changes_nothing() {
        let now = stamp("2026-11-20T16:40:12Z");
        let mut draft = Brief::parse(NEW.as_bytes());
        assert_eq!(
            draft.transition(Transition::Report, now),
            Err(TransitionError::NotFrom(Status::Draft))
        );
        assert_eq!(utf8(&draft.to_bytes()), NEW);

        let mut archived = Brief::parse(b"# An old brief\n");
        assert_eq!(
            archived.transition(Transition::File, now),
            Err(TransitionError::BadStatus(BadStatus::Missing))
        );
        let mut unknown = Brief::parse(b"status: wip\n");
        assert_eq!(
            unknown.transition(Transition::Queue, now),
            Err(TransitionError::BadStatus(BadStatus::Unknown("wip".into())))
        );
        assert_eq!(unknown.to_bytes(), b"status: wip\n");
    }

    #[test]
    fn a_new_header_line_takes_the_terminator_of_the_first_line() {
        let now = stamp("2026-11-20T16:40:12Z");
        let mut crlf = Brief::parse(b"id: 1\r\nstatus: draft\r\n\r\nbody\n");
        crlf.transition(Transition::Queue, now).unwrap();
        assert_eq!(
            utf8(&crlf.to_bytes()),
            "id: 1\r\nstatus: queued\r\nqueued: 2026-11-20T16:40:12Z\r\n\r\nbody\n"
        );

        // A header that is the whole file and ends without a terminator.
        let mut bare = Brief::parse(b"status: draft");
        bare.transition(Transition::Queue, now).unwrap();
        assert_eq!(
            utf8(&bare.to_bytes()),
            "status: queued\nqueued: 2026-11-20T16:40:12Z\n"
        );

        // A line rewritten in place keeps its own terminator.
        let mut mixed = Brief::parse(b"id: 1\nqueued: old\r\nstatus: draft");
        mixed.transition(Transition::Queue, now).unwrap();
        assert_eq!(
            utf8(&mixed.to_bytes()),
            "id: 1\nqueued: 2026-11-20T16:40:12Z\r\nstatus: queued"
        );
    }

    #[test]
    fn a_new_brief_is_its_header_and_a_blank_line() {
        let brief = Brief::new(BriefId::new(129), "86743aa8", stamp("2026-11-20T16:31:07Z"));
        assert_eq!(
            utf8(&brief.to_bytes()),
            "id: 129\nstatus: draft\nbase: 86743aa8\ncreated: 2026-11-20T16:31:07Z\n\n"
        );
        assert_eq!(brief.status(), Ok(Status::Draft));
        assert_eq!(brief.sections(), []);
    }

    #[test]
    fn closes_is_written_in_place_or_added() {
        let ids = [BacklogId::new(41).unwrap(), BacklogId::new(52).unwrap()];
        let mut brief = Brief::parse(b"id: 1\nstatus: draft\n\n# T\n");
        brief.set_closes(&ids);
        assert_eq!(
            utf8(&brief.to_bytes()),
            "id: 1\nstatus: draft\ncloses: b41, b52\n\n# T\n"
        );
        brief.set_closes(&ids[1..]);
        assert_eq!(
            utf8(&brief.to_bytes()),
            "id: 1\nstatus: draft\ncloses: b52\n\n# T\n"
        );
    }

    // ---- sections ----

    fn sections(text: &str) -> Vec<(String, String)> {
        Brief::parse(text.as_bytes())
            .sections()
            .iter()
            .map(|s| (utf8(s.name).to_owned(), utf8(s.text).to_owned()))
            .collect()
    }

    fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
            .collect()
    }

    #[test]
    fn a_section_runs_to_the_next_heading() {
        assert_eq!(
            sections(
                "id: 1\n\n# Title\nintro\n## Task\ndo it\n\n##  Design \r\n### Sub\nx\n##Not\n## Last"
            ),
            owned(&[
                ("Task", "do it\n\n"),
                ("Design", "### Sub\nx\n##Not\n"),
                ("Last", ""),
            ])
        );
        assert_eq!(sections("# Title\nno sections\n"), []);
    }

    #[test]
    fn a_heading_in_a_code_fence_is_quoted_text() {
        assert_eq!(
            sections("## Task\n```markdown\n## Design\n```\n## Design\n  ```\n## Not\n"),
            owned(&[
                ("Task", "```markdown\n## Design\n```\n"),
                // An unclosed fence runs to the end.
                ("Design", "  ```\n## Not\n"),
            ])
        );
    }

    #[test]
    fn the_report_runs_to_the_end_of_the_file() {
        assert_eq!(
            sections("## Task\nx\n## report\nran:\n## Checks\n```\n## Task\n"),
            owned(&[
                ("Task", "x\n"),
                ("report", "ran:\n## Checks\n```\n## Task\n")
            ])
        );
    }

    #[test]
    fn a_section_is_found_by_its_name_in_any_case() {
        let brief = Brief::parse(b"## Out Of Scope\n\n## out of scope\nsecond\n## Task\n \t\n");
        let out = brief.section("Out of scope").unwrap();
        // The first of two: it has no text, and the second does not help.
        assert_eq!((out.name, out.has_text()), (&b"Out Of Scope"[..], false));
        assert!(!brief.section("task").unwrap().has_text());
        assert_eq!(brief.section("Design"), None);
        assert!(
            Brief::parse(b"## Design\n\nx")
                .section("design")
                .unwrap()
                .has_text()
        );
    }

    // ---- properties ----

    /// A body line that is likely to matter to sections: headings, fences,
    /// near misses of both.
    fn section_line() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => prop::sample::select(&[
                "## Task", "## task ", "## Design", "## Report", "## REPORT", "## ",
                "```", "  ```rust", "### Sub", "##Task", " ## Task", "# Title",
            ][..])
            .prop_map(String::from),
            2 => "[a-z #`]{0,8}",
        ]
    }

    /// Any body bytes: the lines, `\n` or `\r\n`, the last with or without.
    fn section_body() -> impl Strategy<Value = Vec<u8>> {
        (
            prop::collection::vec((section_line(), any::<bool>()), 0..14),
            any::<bool>(),
        )
            .prop_map(|(lines, ends_terminated)| {
                let count = lines.len();
                let mut bytes = Vec::new();
                for (index, (content, crlf)) in lines.into_iter().enumerate() {
                    bytes.extend_from_slice(content.as_bytes());
                    if index + 1 < count || ends_terminated {
                        bytes.extend_from_slice(if crlf { b"\r\n" } else { b"\n" });
                    }
                }
                bytes
            })
    }

    proptest! {
        /// The sections cover the body from the first heading on, each its
        /// heading line and then its text; a `## ` line outside a fence is a
        /// heading, or in the report; nothing comes after the report.
        #[test]
        fn the_sections_split_the_body_at_its_headings(body in section_body()) {
            let mut bytes = b"id: 1\n\n".to_vec();
            bytes.extend_from_slice(&body);
            let brief = Brief::parse(&bytes);
            let body = brief.body();
            let found = brief.sections();

            // Rebuilt: the part before the first heading, then per section
            // its heading line and its text.
            let lines = raw_lines(body);
            let mut offsets = Vec::new();
            let mut at = 0;
            for line in &lines {
                offsets.push(at);
                at += line.len();
            }
            let mut rest = &body[body.len()..];
            let mut headings = Vec::new();
            if let Some(first) = found.first() {
                // The first heading line ends where the first text starts.
                let text_at = first.text.as_ptr() as usize - body.as_ptr() as usize;
                let line = offsets.iter().rposition(|o| *o < text_at).unwrap();
                rest = &body[offsets[line]..];
            }
            for section in &found {
                let (heading, after) = rest.split_at(rest.iter().position(|b| *b == b'\n').map_or(rest.len(), |n| n + 1));
                let content = content_of(heading);
                prop_assert!(content.starts_with(b"## "), "{:?}", utf8(heading));
                prop_assert_eq!(content[3..].trim_ascii(), section.name);
                prop_assert!(after.starts_with(section.text));
                headings.push(heading.as_ptr() as usize - body.as_ptr() as usize);
                rest = &after[section.text.len()..];
            }
            prop_assert!(rest.is_empty());

            // Every `## ` line outside a fence is a heading, or is in the
            // report, which is last.
            let report = found.iter().position(|s| s.is_named("Report"));
            if let Some(report) = report {
                prop_assert_eq!(report + 1, found.len());
            }
            let report_text_at = report.map(|r| found[r].text.as_ptr() as usize - body.as_ptr() as usize);
            let mut fenced = false;
            for (line, offset) in lines.iter().zip(&offsets) {
                let content = content_of(line);
                if report_text_at.is_some_and(|r| *offset >= r) {
                    break;
                }
                if content.trim_ascii_start().starts_with(b"```") {
                    fenced = !fenced;
                } else if !fenced && content.starts_with(b"## ") {
                    prop_assert!(headings.contains(offset), "{:?} is no heading", utf8(line));
                } else {
                    prop_assert!(!headings.contains(offset));
                }
            }
        }
    }

    /// A header line as hand edits leave it: the tool's keys in any order,
    /// repeated or missing, unknown keys, loose spacing.
    fn header_line() -> impl Strategy<Value = String> {
        const KEYS: [&str; 10] = [
            "id", "base", "created", "closes", "queued", "started", "blocked", "reported", "filed",
            "note",
        ];
        let status = prop_oneof![
            8 => prop::sample::select(&Status::ALL[..]).prop_map(|s| s.to_string()),
            1 => Just("wip".to_owned()),
            1 => Just(String::new()),
        ];
        let other = prop_oneof![
            1 => Just("2026-11-20T16:31:07Z".to_owned()),
            1 => "[a-z0-9 ,]{0,8}",
        ];
        prop_oneof![
            1 => (Just("status"), " {1,2}", status),
            3 => (prop::sample::select(&KEYS[..]), " {0,2}", other),
        ]
        .prop_map(|(key, space, value)| {
            // Without the space only a key alone is a header line.
            if space.is_empty() {
                format!("{key}:")
            } else {
                format!("{key}:{space}{value}")
            }
        })
    }

    fn body_line() -> impl Strategy<Value = String> {
        prop_oneof![
            2 => Just(String::new()),
            2 => "[a-z #`:]{0,12}",
            1 => prop::sample::select(
                &["# Title", "## Task", "## Report", "```", "status: filed", "status:started"][..]
            )
            .prop_map(String::from),
        ]
    }

    /// A brief as bytes: header lines, then body lines, `\n` or `\r\n` or
    /// mixed, the last line with or without a terminator.
    fn brief_bytes() -> impl Strategy<Value = Vec<u8>> {
        let terminator = prop_oneof![
            8 => Just(["\n", "\n"]),
            2 => Just(["\r\n", "\r\n"]),
            1 => Just(["\n", "\r\n"]),
        ];
        (
            prop::collection::vec((header_line(), any::<bool>()), 0..8),
            prop::collection::vec((body_line(), any::<bool>()), 0..6),
            terminator,
            any::<bool>(),
        )
            .prop_map(|(mut lines, body, terminators, ends_terminated)| {
                lines.extend(body);
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

    fn terminator_of(raw_line: &[u8]) -> &[u8] {
        &raw_line[content_of(raw_line).len()..]
    }

    /// The line of the first `key:` in the leading run of header lines, by a
    /// reading of the format that shares nothing with the parser.
    fn header_position(lines: &[Vec<u8>], key: &str) -> (usize, Option<usize>) {
        let is_header = |line: &Vec<u8>| {
            let content = utf8(content_of(line));
            content.split_once(':').is_some_and(|(key, rest)| {
                !key.is_empty()
                    && key.chars().all(|c| c.is_ascii_lowercase())
                    && (rest.is_empty() || rest.starts_with(' '))
            })
        };
        let header_len = lines.iter().take_while(|l| is_header(l)).count();
        let at = lines[..header_len].iter().position(|line| {
            utf8(content_of(line)).split_once(':').map(|(key, _)| key) == Some(key)
        });
        (header_len, at)
    }

    /// A transition changes the `status:` line and one stamp line (none for
    /// a resume), nothing else; one that is not allowed changes nothing.
    fn check_transition(
        bytes: &[u8],
        (allowed, pick): (bool, prop::sample::Index),
        now: Stamp,
    ) -> Result<(), TestCaseError> {
        let mut brief = Brief::parse(bytes);
        let from = brief.status();
        // Drawn blindly, five moves in six are refused; so mostly one that
        // the status allows, when there is one.
        let allowed: Vec<Transition> = Transition::ALL
            .into_iter()
            .filter(|t| allowed && from.clone().is_ok_and(|from| t.apply(from).is_some()))
            .collect();
        let transition = match allowed.len() {
            0 => *pick.get(&Transition::ALL),
            _ => *pick.get(&allowed),
        };
        let to = match brief.transition(transition, now) {
            Ok(to) => to,
            Err(error) => {
                let expected = match from {
                    Ok(from) => TransitionError::NotFrom(from),
                    Err(bad) => TransitionError::BadStatus(bad),
                };
                prop_assert_eq!(error, expected);
                prop_assert_eq!(brief.to_bytes(), bytes);
                return Ok(());
            }
        };
        prop_assert_eq!(transition.apply(from.unwrap()), Some(to));
        prop_assert_eq!(brief.status(), Ok(to));

        let old = raw_lines(bytes);
        let mut new = raw_lines(&brief.to_bytes());
        let (header_len, status_at) = header_position(&old, "status");
        let status_at = status_at.unwrap();
        let status_line = format!("status: {to}").into_bytes();
        let stamp_line = format!("{to}: {now}").into_bytes();

        // What the old lines are expected to become.
        let mut expected = old.clone();
        expected[status_at] = [&status_line[..], terminator_of(&old[status_at])].concat();
        if transition != Transition::Resume {
            prop_assert_eq!(brief.get(to.as_str()).map(utf8), Some(&now.to_string()[..]));
            match header_position(&old, to.as_str()).1 {
                Some(at) => expected[at] = [&stamp_line[..], terminator_of(&old[at])].concat(),
                None => {
                    // A new line at the end of the header. Taken out again,
                    // the rest is the old lines, give or take the terminator
                    // of a header line that ended the file without one.
                    let added = new.remove(header_len);
                    prop_assert_eq!(content_of(&added), &stamp_line[..]);
                    let terminator = match old.first() {
                        Some(first) if first.ends_with(b"\r\n") => &b"\r\n"[..],
                        _ => b"\n",
                    };
                    prop_assert_eq!(terminator_of(&added), terminator);
                    let last = expected.last_mut().unwrap();
                    if header_len == old.len() && terminator_of(last).is_empty() {
                        last.extend_from_slice(terminator);
                    }
                }
            }
        }
        prop_assert_eq!(new, expected);
        Ok(())
    }

    proptest! {
        #[test]
        fn any_bytes_are_written_back_as_read(bytes in any::<Vec<u8>>()) {
            prop_assert_eq!(Brief::parse(&bytes).to_bytes(), bytes);
        }

        #[test]
        fn any_brief_is_written_back_as_read(bytes in brief_bytes()) {
            let brief = Brief::parse(&bytes);
            prop_assert_eq!(brief.to_bytes(), &bytes[..]);
            let (header_len, _) = header_position(&raw_lines(&bytes), "status");
            prop_assert_eq!(brief.body(), raw_lines(&bytes)[header_len..].concat());
        }

        #[test]
        fn a_transition_changes_the_status_line_and_one_stamp_line(
            bytes in brief_bytes(),
            allowed in prop::bool::weighted(0.8),
            pick in any::<prop::sample::Index>(),
            now in any_stamp(),
        ) {
            check_transition(&bytes, (allowed, pick), now)?;
        }
    }
}
