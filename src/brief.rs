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

use crate::header::Header;
use crate::id::{BacklogId, BriefId, ItemId, ListKind};
use crate::raw;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    Draft,
    /// A finished brief that waits in `briefs/parked/` for the owner's word
    /// (owner, 2026-11-26); only `queue` moves on from it.
    Parked,
    Queued,
    Started,
    Blocked,
    Reported,
    Filed,
}

impl Status {
    pub const ALL: [Status; 7] = [
        Status::Draft,
        Status::Parked,
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
            Status::Parked => "parked",
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
    Park,
    Queue,
    Claim,
    Block,
    Resume,
    Report,
    File,
}

impl Transition {
    pub const ALL: [Transition; 7] = [
        Transition::Park,
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
        use Status::{Blocked, Draft, Filed, Parked, Queued, Reported, Started};
        use Transition::{Block, Claim, File, Park, Queue, Report, Resume};
        match (self, from) {
            (Park, Draft) => Some(Parked),
            (Queue, Draft | Parked) => Some(Queued),
            (Claim, Queued) => Some(Started),
            (Block, Started) => Some(Blocked),
            (Resume, Blocked) => Some(Started),
            (Report, Started) => Some(Reported),
            (File, Reported) => Some(Filed),
            (Park, Parked | Queued | Started | Blocked | Reported | Filed)
            | (Queue, Queued | Started | Blocked | Reported | Filed)
            | (Claim, Draft | Parked | Started | Blocked | Reported | Filed)
            | (Block, Draft | Parked | Queued | Blocked | Reported | Filed)
            | (Resume, Draft | Parked | Queued | Started | Reported | Filed)
            | (Report, Draft | Parked | Queued | Blocked | Reported | Filed)
            | (File, Draft | Parked | Queued | Started | Blocked | Filed) => None,
        }
    }

    /// Whether the move stamps the key named after the new status.
    ///
    /// Resume does not: it returns to `started`, and `started:` stays the
    /// first start, which is what "in flight for how long" counts from.
    const fn stamps(self) -> bool {
        match self {
            Transition::Park
            | Transition::Queue
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

/// A word of `closes:` that is no backlog id, as found (lossily when it is
/// not UTF-8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotABacklogId(pub String);

/// A brief file, lossless: writing it back gives the bytes that were read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// The leading run of header lines ([`Header`], shared with the chores'
    /// and backlog items' files).
    header: Header,
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

/// Where a section is in a brief's body.
#[derive(Debug, Clone)]
struct Span {
    /// Where its heading line starts.
    heading: usize,
    /// What follows `## ` on the heading line, untrimmed.
    name: std::ops::Range<usize>,
    /// Its lines after the heading line.
    text: std::ops::Range<usize>,
}

impl Span {
    fn name<'a>(&self, body: &'a [u8]) -> &'a [u8] {
        body[self.name.clone()].trim_ascii()
    }

    fn is_named(&self, body: &[u8], name: &str) -> bool {
        self.name(body).eq_ignore_ascii_case(name.as_bytes())
    }
}

/// What a line of a brief's body is to its sections: the one reading of a
/// line, for the sections of a brief and for text a command puts into one.
enum BodyLine {
    /// A line whose first non-blank characters are ```` ``` ````: it opens
    /// or closes a code fence.
    Fence,
    /// A line that starts with `## `: a heading when it is outside a fence.
    Heading,
    Other,
}

impl BodyLine {
    fn of(content: &[u8]) -> BodyLine {
        if content.trim_ascii_start().starts_with(b"```") {
            BodyLine::Fence
        } else if content.starts_with(b"## ") {
            BodyLine::Heading
        } else {
            BodyLine::Other
        }
    }
}

/// Text a command writes into a brief: a question, an amendment, a report.
/// Not blank; blank lines before it and blanks at its end are dropped, as
/// they would only push it away from its heading and the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text(Vec<u8>);

/// Text that is blank or empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyText;

impl fmt::Display for EmptyText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the text is empty")
    }
}

impl std::error::Error for EmptyText {}

impl Text {
    pub fn new(bytes: &[u8]) -> Result<Text, EmptyText> {
        let start = raw::lines(bytes)
            .take_while(|line| raw::is_blank(raw::content(line)))
            .map(<[u8]>::len)
            .sum::<usize>();
        let text = bytes[start..].trim_ascii_end();
        if text.is_empty() {
            Err(EmptyText)
        } else {
            Ok(Text(text.to_vec()))
        }
    }

    /// Without a terminator at the end.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl FromStr for Text {
    type Err = EmptyText;

    fn from_str(s: &str) -> Result<Text, EmptyText> {
        Text::new(s.as_bytes())
    }
}

/// Text that stays in the section it is appended to: a question or an
/// amendment, which other sections may follow.
///
/// A `## ` line outside a code fence would be read as the heading of a
/// section of its own, and a fence the text opens and does not close would
/// make every heading after it quoted text: either way the sections after
/// the entry would change. A report is plain [`Text`]: it is the last
/// section, by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryText(Text);

/// Why a text would not stay in its section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitsSection {
    /// The `## ` line, as it is in the text.
    Heading(String),
    OpenFence,
}

impl fmt::Display for SplitsSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplitsSection::Heading(line) => write!(
                f,
                "the line `{}` outside a code fence would start a section of its own; \
                 use `###` for a heading",
                line.escape_debug()
            ),
            SplitsSection::OpenFence => f.write_str(
                "a code fence is opened and not closed, which would make the sections after it \
                 quoted text",
            ),
        }
    }
}

impl std::error::Error for SplitsSection {}

impl EntryText {
    pub fn new(text: Text) -> Result<EntryText, SplitsSection> {
        let mut fenced = false;
        for line in raw::lines(text.as_bytes()) {
            let content = raw::content(line);
            match BodyLine::of(content) {
                BodyLine::Fence => fenced = !fenced,
                BodyLine::Heading if !fenced => {
                    return Err(SplitsSection::Heading(
                        String::from_utf8_lossy(content).into_owned(),
                    ));
                }
                BodyLine::Heading | BodyLine::Other => {}
            }
        }
        if fenced {
            return Err(SplitsSection::OpenFence);
        }
        Ok(EntryText(text))
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
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
        let (header, body) = Header::split(bytes);
        Brief {
            header,
            body: body.to_vec(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        [self.header.to_bytes().as_slice(), &self.body].concat()
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The trimmed value of the first header line with this key. A key
    /// written twice by hand reads as its first line, which is also the one
    /// a write changes.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.header.get(key)
    }

    /// Rewrites the key's line in place, or adds the key at the end of the
    /// header. Private: `status` has one writer, [`Brief::transition`].
    fn set(&mut self, key: &str, value: &str) {
        self.header.set(key, value);
    }

    /// What the brief waits on (`after:`), after `closes` where the header
    /// has it; `None` removes the line.
    pub(crate) fn set_after(&mut self, target: Option<ItemId>) {
        match target {
            Some(target) => self.header.set_after(
                crate::after::KEY,
                &target.to_string(),
                &["id", "status", "base", "closes"],
            ),
            None => self.header.remove(crate::after::KEY),
        }
    }

    /// Puts the brief first in the queue: `first: <n>`, after `after` and
    /// `closes` where the header has them.
    pub(crate) fn set_first(&mut self, counter: u64) {
        self.header.set_after(
            crate::project::FIRST,
            &counter.to_string(),
            &["id", "status", "base", "closes", crate::after::KEY],
        );
    }

    /// Writes the backlog items the brief resolves, which `file` deletes:
    /// `closes: b41, b52`.
    pub(crate) fn set_closes(&mut self, ids: &[BacklogId]) {
        let ids: Vec<String> = ids.iter().map(BacklogId::to_string).collect();
        self.set("closes", &ids.join(", "));
    }

    /// The backlog items `closes:` names, each once, in its order; none
    /// without the key. The words may be separated by commas, blanks or
    /// both: the line is also written by hand.
    ///
    /// A word that is no backlog id is an error, not skipped: `file` deletes
    /// what this names, and a typo (`b4l`) skipped would leave the item open
    /// without a word.
    pub(crate) fn closes(&self) -> Result<Vec<BacklogId>, NotABacklogId> {
        let Some(value) = self.get("closes") else {
            return Ok(Vec::new());
        };
        let mut ids: Vec<BacklogId> = Vec::new();
        for word in value
            .split(|b| *b == b',' || b.is_ascii_whitespace())
            .filter(|word| !word.is_empty())
        {
            let id = ListKind::Backlog
                .parse_id(word)
                .and_then(BacklogId::new)
                .ok_or_else(|| NotABacklogId(String::from_utf8_lossy(word).into_owned()))?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// The `## ` sections of the body, in file order.
    ///
    /// A heading is a line that starts with `## ` outside a code fence (see
    /// [`BodyLine`]): a brief quotes markdown, and a heading in a quoted
    /// block is not the brief's. `## Report` runs to the end of the file, so
    /// a report may hold `## ` lines of its own; the tool keeps it last.
    pub fn sections(&self) -> Vec<Section<'_>> {
        let body = self.body.as_slice();
        self.spans()
            .into_iter()
            .map(|span| Section {
                name: span.name(body),
                text: &body[span.text],
            })
            .collect()
    }

    /// Where each section is in the body.
    fn spans(&self) -> Vec<Span> {
        let mut spans = Vec::new();
        // The open section, its text running to the end so far.
        let mut open: Option<Span> = None;
        let mut fenced = false;
        let mut at = 0;
        for line in raw::lines(&self.body) {
            let start = at;
            at += line.len();
            let content = raw::content(line);
            match BodyLine::of(content) {
                BodyLine::Fence => {
                    fenced = !fenced;
                    continue;
                }
                BodyLine::Heading if !fenced => {}
                BodyLine::Heading | BodyLine::Other => continue,
            }
            if let Some(mut span) = open.take() {
                span.text.end = start;
                spans.push(span);
            }
            let span = Span {
                heading: start,
                name: start + "## ".len()..start + content.len(),
                text: at..at,
            };
            let is_report = span.is_named(&self.body, REPORT);
            open = Some(span);
            if is_report {
                break;
            }
        }
        if let Some(mut span) = open {
            span.text.end = self.body.len();
            spans.push(span);
        }
        spans
    }

    /// The terminator a line the tool adds gets: the one of the file's first
    /// line, as for a list file.
    fn terminator(&self) -> &'static [u8] {
        let first = self
            .header
            .first_line()
            .or_else(|| raw::lines(&self.body).next());
        raw::terminator_of(first)
    }

    /// Gives the file's last line a terminator when it has none, so a line
    /// can follow it.
    fn terminate_last_line(&mut self, terminator: &[u8]) {
        let last = if self.body.is_empty() {
            self.header.last_line_mut()
        } else {
            Some(&mut self.body)
        };
        if let Some(last) = last
            && !last.ends_with(b"\n")
        {
            last.extend_from_slice(terminator);
        }
    }

    /// Appends `### <now>`, a blank line and the text at the end of the
    /// section `name` (a question, an amendment), and makes the section when
    /// the brief has none: before `## Report`, which runs to the end of the
    /// file, else at the end. Every other section keeps its bytes, but for a
    /// terminator on a last line that had none.
    ///
    /// An entry is set off by a blank line from what comes before it in its
    /// section and from a heading after it; a new section's heading follows
    /// the line before it directly, as a blank line there would be the text
    /// of the section above.
    ///
    /// A brief whose own text leaves a code fence open to the end of the
    /// file gets the entry inside that fence, where no heading is read: the
    /// text is kept, the section is not found again. Not refused: `block`
    /// is the worker's way out, and the fence is the planner's to close.
    pub(crate) fn append_entry(&mut self, name: &str, now: Stamp, text: &EntryText) {
        let terminator = self.terminator();
        let spans = self.spans();
        let target = spans
            .iter()
            .find(|span| span.is_named(&self.body, name))
            .cloned();
        let report = spans.iter().find(|span| span.is_named(&self.body, REPORT));
        let mut insert = Vec::new();
        let mut at = match &target {
            Some(span) => span.text.end,
            None => report.map_or(self.body.len(), |span| span.heading),
        };
        if at == self.body.len() {
            self.terminate_last_line(terminator);
            at = self.body.len();
        }
        if target.is_none() {
            insert.extend_from_slice(format!("## {name}").as_bytes());
            insert.extend_from_slice(terminator);
        }
        // The line before the entry: the section's last, or its heading.
        let before = match &target {
            Some(span) if !span.text.is_empty() => {
                raw::lines(&self.body[span.text.start..at]).last()
            }
            Some(span) => Some(&self.body[span.heading..span.text.start]),
            None => None,
        };
        if before.is_none_or(|line| !raw::is_blank(raw::content(line))) {
            insert.extend_from_slice(terminator);
        }
        insert.extend_from_slice(format!("### {now}").as_bytes());
        insert.extend_from_slice(terminator);
        insert.extend_from_slice(terminator);
        insert.extend_from_slice(text.as_bytes());
        insert.extend_from_slice(terminator);
        if at < self.body.len() {
            insert.extend_from_slice(terminator);
        }
        self.body.splice(at..at, insert);
    }

    /// Makes the text the whole of `## Report`, which is added at the end of
    /// the file when the brief has none. A report runs to the end of the
    /// file, so it may hold `## ` lines of its own.
    pub(crate) fn set_report(&mut self, text: &Text) {
        let terminator = self.terminator();
        let report = self
            .spans()
            .into_iter()
            .find(|span| span.is_named(&self.body, REPORT));
        let from = match report {
            // A heading that ends the file without a terminator.
            Some(span) if span.text.is_empty() && !self.body.ends_with(b"\n") => {
                self.body.extend_from_slice(terminator);
                self.body.len()
            }
            Some(span) => span.text.start,
            None => {
                self.terminate_last_line(terminator);
                self.body
                    .extend_from_slice(format!("## {REPORT}").as_bytes());
                self.body.extend_from_slice(terminator);
                self.body.len()
            }
        };
        self.body.truncate(from);
        self.body.extend_from_slice(text.as_bytes());
        self.body.extend_from_slice(terminator);
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
        self.transition_from(from, transition, now)
    }

    /// The move from `from`, which the place of the file may decide over
    /// what the header says ([`crate::project::BriefFile::transition`]).
    pub(crate) fn transition_from(
        &mut self,
        from: Status,
        transition: Transition,
        now: Stamp,
    ) -> Result<Status, TransitionError> {
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
            "`Started` is not a status (draft, parked, queued, started, blocked, reported, filed)"
        );
    }

    /// A row or column the lists lack would also be missing from the table
    /// test below; these matches stop compiling when a variant is added.
    #[test]
    fn the_lists_of_variants_are_complete() {
        for (index, status) in Status::ALL.into_iter().enumerate() {
            let expected = match status {
                Status::Draft => 0,
                Status::Parked => 1,
                Status::Queued => 2,
                Status::Started => 3,
                Status::Blocked => 4,
                Status::Reported => 5,
                Status::Filed => 6,
            };
            assert_eq!(index, expected);
        }
        for (index, transition) in Transition::ALL.into_iter().enumerate() {
            let expected = match transition {
                Transition::Park => 0,
                Transition::Queue => 1,
                Transition::Claim => 2,
                Transition::Block => 3,
                Transition::Resume => 4,
                Transition::Report => 5,
                Transition::File => 6,
            };
            assert_eq!(index, expected);
        }
    }

    #[test]
    fn the_transition_table() {
        use Status::{Blocked, Filed, Parked, Queued, Reported, Started};
        const NO: Option<Status> = None;
        #[rustfmt::skip]
        let table: [(Transition, [Option<Status>; 7]); 7] = [
            // from:              draft         parked        queued         started         blocked        reported     filed
            (Transition::Park,   [Some(Parked), NO,           NO,            NO,             NO,            NO,          NO]),
            (Transition::Queue,  [Some(Queued), Some(Queued), NO,            NO,             NO,            NO,          NO]),
            (Transition::Claim,  [NO,           NO,           Some(Started), NO,             NO,            NO,          NO]),
            (Transition::Block,  [NO,           NO,           NO,            Some(Blocked),  NO,            NO,          NO]),
            (Transition::Resume, [NO,           NO,           NO,            NO,             Some(Started), NO,          NO]),
            (Transition::Report, [NO,           NO,           NO,            Some(Reported), NO,            NO,          NO]),
            (Transition::File,   [NO,           NO,           NO,            NO,             NO,            Some(Filed), NO]),
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

    #[test]
    fn closes_reads_what_is_written_and_what_a_hand_types() {
        let ids = |text: &str| {
            Brief::parse(text.as_bytes())
                .closes()
                .map(|ids| ids.into_iter().map(BacklogId::get).collect::<Vec<u32>>())
        };
        assert_eq!(ids("id: 1\n\n# T\n"), Ok(vec![]));
        assert_eq!(ids("closes:\n"), Ok(vec![]));
        assert_eq!(ids("closes: b41, b52\n"), Ok(vec![41, 52]));
        assert_eq!(ids("closes: b52 b41,b52,, \r\n"), Ok(vec![52, 41]));
        for (text, word) in [
            ("closes: b41, b4l\n", "b4l"),
            ("closes: c41\n", "c41"),
            ("closes: b041\n", "b041"),
            ("closes: b0\n", "b0"),
            ("closes: 41\n", "41"),
        ] {
            assert_eq!(ids(text), Err(NotABacklogId(word.to_owned())), "{text}");
        }
    }

    proptest! {
        /// What `queue` writes is what `file` reads.
        #[test]
        fn closes_reads_back_what_set_closes_wrote(
            numbers in prop::collection::btree_set(1u32.., 0..6),
        ) {
            let ids: Vec<BacklogId> = numbers.into_iter().filter_map(BacklogId::new).collect();
            let mut brief = Brief::parse(b"id: 1\nstatus: draft\n\n# T\n");
            brief.set_closes(&ids);
            prop_assert_eq!(brief.closes(), Ok(ids));
        }
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

    // ---- text a command writes ----

    fn entry(text: &str) -> EntryText {
        EntryText::new(text.parse().unwrap()).unwrap()
    }

    #[test]
    fn text_is_what_is_not_blank() {
        let text = |bytes: &[u8]| Text::new(bytes).map(|text| utf8(text.as_bytes()).to_owned());
        assert_eq!(text(b"q"), Ok("q".to_owned()));
        assert_eq!(
            text(b" \n\r\n  first\n\n  second \n\n"),
            Ok("  first\n\n  second".to_owned())
        );
        assert_eq!(text(b""), Err(EmptyText));
        assert_eq!(text(b" \n\t\r\n"), Err(EmptyText));
    }

    #[test]
    fn an_entry_may_not_start_a_section_or_leave_a_fence_open() {
        let check = |text: &str| EntryText::new(text.parse().unwrap()).map(drop);
        for fits in [
            "plain",
            "### Sub\nx",
            " ## indented",
            "##tight",
            "```\n## quoted\n```",
            "  ```md\n## quoted\n```\n```\n## \n```",
        ] {
            assert_eq!(check(fits), Ok(()), "{fits}");
        }
        assert_eq!(
            check("x\n## Design\ny"),
            Err(SplitsSection::Heading("## Design".to_owned()))
        );
        assert_eq!(
            check("```\n```\n## \nx"),
            Err(SplitsSection::Heading("## ".to_owned()))
        );
        assert_eq!(check("```\n## quoted"), Err(SplitsSection::OpenFence));
        assert_eq!(
            check("a\r\n## B\r\n").unwrap_err().to_string(),
            "the line `## B` outside a code fence would start a section of its own; \
             use `###` for a heading"
        );
    }

    const AT: &str = "2026-11-20T17:10:00Z";

    fn appended(bytes: &str, name: &str, text: &str) -> String {
        let mut brief = Brief::parse(bytes.as_bytes());
        brief.append_entry(name, stamp(AT), &entry(text));
        String::from_utf8(brief.to_bytes()).unwrap()
    }

    #[test]
    fn an_entry_goes_at_the_end_of_its_section() {
        // Made at the end, then added to.
        let once = appended("id: 1\n\n## Task\nt\n", "Questions", "Why?");
        assert_eq!(
            once,
            format!("id: 1\n\n## Task\nt\n## Questions\n\n### {AT}\n\nWhy?\n")
        );
        assert_eq!(
            appended(&once, "Questions", "And?\n\n"),
            format!("id: 1\n\n## Task\nt\n## Questions\n\n### {AT}\n\nWhy?\n\n### {AT}\n\nAnd?\n")
        );
        // In the middle: set off from the heading after it.
        assert_eq!(
            appended(
                "## Amendments\n### old\nx\n## Questions\nq\n",
                "amendments",
                "a"
            ),
            format!("## Amendments\n### old\nx\n\n### {AT}\n\na\n\n## Questions\nq\n")
        );
        // Before the report, which stays last.
        assert_eq!(
            appended(
                "## Task\nt\n## report\n## Not a section\n",
                "Amendments",
                "a"
            ),
            format!("## Task\nt\n## Amendments\n\n### {AT}\n\na\n\n## report\n## Not a section\n")
        );
    }

    #[test]
    fn an_entry_takes_the_terminator_of_the_first_line() {
        assert_eq!(
            appended("id: 1\r\n\r\n## Task\r\nt", "Questions", "line 1\nline 2"),
            format!(
                "id: 1\r\n\r\n## Task\r\nt\r\n## Questions\r\n\r\n### {AT}\r\n\r\nline 1\nline 2\r\n"
            )
        );
        // A header that is the whole file, without a terminator.
        assert_eq!(
            appended("status: started", "Questions", "q"),
            format!("status: started\n## Questions\n\n### {AT}\n\nq\n")
        );
        // A heading that ends the file without one.
        assert_eq!(
            appended("## Questions", "Questions", "q"),
            format!("## Questions\n\n### {AT}\n\nq\n")
        );
    }

    fn reported(bytes: &str, text: &str) -> String {
        let mut brief = Brief::parse(bytes.as_bytes());
        brief.set_report(&text.parse().unwrap());
        String::from_utf8(brief.to_bytes()).unwrap()
    }

    #[test]
    fn a_report_is_the_whole_of_the_last_section() {
        assert_eq!(
            reported("id: 1\n\n## Task\nt", "Ran:\n## Checks\nok\n"),
            "id: 1\n\n## Task\nt\n## Report\nRan:\n## Checks\nok\n"
        );
        assert_eq!(
            reported("## Task\nt\n## REPORT \nold\n## Old\n", "new"),
            "## Task\nt\n## REPORT \nnew\n"
        );
        assert_eq!(reported("## Report", "new"), "## Report\nnew\n");
        assert_eq!(
            reported("status: started\r\n", "new"),
            "status: started\r\n## Report\r\nnew\r\n"
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
                "## Questions", "## amendments ",
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

    /// Text for a question, an amendment or a report: mostly lines that fit
    /// anywhere, now and then a heading or a fence.
    fn text_bytes() -> impl Strategy<Value = Vec<u8>> {
        let line = prop_oneof![
            6 => "[a-z ]{0,8}",
            1 => prop::sample::select(&["### Sub", "## Checks", "```", " ## x", "  "][..])
                .prop_map(String::from),
        ];
        prop::collection::vec((line, any::<bool>()), 1..5).prop_map(|lines| {
            let mut bytes = Vec::new();
            for (content, crlf) in lines {
                bytes.extend_from_slice(content.as_bytes());
                bytes.extend_from_slice(if crlf { b"\r\n" } else { b"\n" });
            }
            bytes
        })
    }

    /// Whether the body ends inside a code fence, by a reading of the format
    /// that shares nothing with the parser: a heading added at the end is
    /// then quoted text. A brief the planner left that way keeps what is
    /// written after it inside the fence; that is the brief's, not the edit's.
    fn ends_fenced(body: &[u8]) -> bool {
        let mut fenced = false;
        for line in raw_lines(body) {
            let content = utf8(content_of(&line));
            if content.trim_start().starts_with("```") {
                fenced = !fenced;
            } else if !fenced
                && content
                    .strip_prefix("## ")
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case("report"))
            {
                return false;
            }
        }
        fenced
    }

    /// A brief cut into its header, the body before the first section, and
    /// the sections as `(name, text)`.
    type Parts = (Vec<u8>, Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>);

    fn parts(brief: &Brief) -> Parts {
        let body = brief.body();
        let preamble = brief
            .spans()
            .first()
            .map_or(body.len(), |span| span.heading);
        let sections = brief
            .sections()
            .iter()
            .map(|section| (section.name.to_vec(), section.text.to_vec()))
            .collect();
        (brief.header.to_bytes(), body[..preamble].to_vec(), sections)
    }

    /// `new` is `old`, or, for the last part of the file, `old` with the
    /// terminator it lacked.
    fn kept(old: &[u8], new: &[u8], last: bool, terminator: &[u8]) -> bool {
        new == old || (last && !old.ends_with(b"\n") && new == [old, terminator].concat())
    }

    const PREFIXES: [&str; 3] = [
        "",
        "id: 1\nstatus: started\n\n",
        "id: 1\r\nstatus: started\r\n",
    ];

    /// An entry lands at the end of its section, which is made before the
    /// report or at the end when missing; every other byte stays.
    fn check_entry(
        prefix: &str,
        body: &[u8],
        name: &str,
        text: &[u8],
        now: Stamp,
    ) -> Result<(), TestCaseError> {
        let Some(entry) = Text::new(text)
            .ok()
            .and_then(|text| EntryText::new(text).ok())
        else {
            return Ok(());
        };
        if ends_fenced(body) {
            return Ok(());
        }
        let before = Brief::parse(&[prefix.as_bytes(), body].concat());
        let terminator = before.terminator();
        let mut after = before.clone();
        after.append_entry(name, now, &entry);
        let (old_header, old_preamble, old) = parts(&before);
        let (new_header, new_preamble, new) = parts(&after);
        prop_assert_eq!(new_header, old_header);
        prop_assert!(kept(
            &old_preamble,
            &new_preamble,
            old.is_empty(),
            terminator
        ));

        let core = [
            format!("### {now}").as_bytes(),
            terminator,
            terminator,
            entry.as_bytes(),
            terminator,
        ]
        .concat();
        let target = old
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name.as_bytes()));
        let report = old
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(b"Report"));
        let (at, made) = match target {
            Some(at) => (at, false),
            None => (report.unwrap_or(old.len()), true),
        };
        prop_assert_eq!(new.len(), old.len() + usize::from(made));
        let at_end = at + usize::from(!made) == old.len();
        for (index, (old_name, old_text)) in old.iter().enumerate() {
            let last = index + 1 == old.len();
            let moved = if made && index >= at {
                index + 1
            } else {
                index
            };
            let (new_name, new_text) = &new[moved];
            prop_assert_eq!(new_name, old_name);
            if made || index != at {
                prop_assert!(
                    kept(old_text, new_text, last, terminator),
                    "section {} changed",
                    index
                );
                continue;
            }
            // The target: its old text, then the entry, set off by blank
            // lines from a line before it and a heading after it.
            let mut kept_text = old_text.clone();
            if last && !kept_text.is_empty() && !kept_text.ends_with(b"\n") {
                kept_text.extend_from_slice(terminator);
            }
            let blank_before = match raw_lines(&kept_text).last() {
                Some(line) => !content_of(line).iter().all(u8::is_ascii_whitespace),
                None => true,
            };
            let expected = [
                kept_text.as_slice(),
                if blank_before { terminator } else { b"" },
                &core,
                if at_end { b"" } else { terminator },
            ]
            .concat();
            prop_assert_eq!(utf8(new_text), utf8(&expected));
        }
        if made {
            let (new_name, new_text) = &new[at];
            prop_assert_eq!(utf8(new_name), name);
            let expected = [terminator, &core, if at_end { b"" } else { terminator }].concat();
            prop_assert_eq!(utf8(new_text), utf8(&expected));
        }
        Ok(())
    }

    /// The report is the text and the last section; every other byte stays.
    fn check_report(prefix: &str, body: &[u8], text: &[u8]) -> Result<(), TestCaseError> {
        let Ok(text) = Text::new(text) else {
            return Ok(());
        };
        if ends_fenced(body) {
            return Ok(());
        }
        let before = Brief::parse(&[prefix.as_bytes(), body].concat());
        let terminator = before.terminator();
        let mut after = before.clone();
        after.set_report(&text);
        let (old_header, old_preamble, old) = parts(&before);
        let (new_header, new_preamble, new) = parts(&after);
        prop_assert_eq!(new_header, old_header);
        prop_assert!(kept(
            &old_preamble,
            &new_preamble,
            old.is_empty(),
            terminator
        ));
        let report = old
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(b"Report"));
        prop_assert_eq!(new.len(), old.len() + usize::from(report.is_none()));
        let (new_name, new_text) = new.last().unwrap();
        match report {
            Some(at) => prop_assert_eq!(new_name, &old[at].0),
            None => prop_assert_eq!(utf8(new_name), "Report"),
        }
        prop_assert_eq!(new_text, &[text.as_bytes(), terminator].concat());
        let others = report.unwrap_or(old.len());
        for (index, (old_name, old_text)) in old[..others].iter().enumerate() {
            let (new_name, new_text) = &new[index];
            prop_assert_eq!(new_name, old_name);
            prop_assert!(kept(old_text, new_text, index + 1 == old.len(), terminator));
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn an_entry_leaves_every_other_section_as_it_was(
            prefix in prop::sample::select(&PREFIXES[..]),
            body in section_body(),
            name in prop::sample::select(&["Questions", "Amendments"][..]),
            text in text_bytes(),
            now in any_stamp(),
        ) {
            check_entry(prefix, &body, name, &text, now)?;
        }

        #[test]
        fn a_report_leaves_every_other_section_as_it_was(
            prefix in prop::sample::select(&PREFIXES[..]),
            body in section_body(),
            text in text_bytes(),
        ) {
            check_report(prefix, &body, &text)?;
        }
    }
}
