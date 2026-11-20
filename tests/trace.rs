//! Random interleavings of writes and hand edits on two clones of one
//! remote, with the faults git can make: refused pushes, a remote out of
//! reach, a write of the other clone or a file saved while a push is on its
//! way, a push that landed but was reported failed, one that landed only
//! after the next round's fetch, and a command killed before or after its
//! push.
//!
//! An oracle reads the list files of the three places (clone A's files,
//! clone B's, the remote's `main`) after every step and checks:
//!
//! - I1: no id and no token on two lines of a file;
//! - I2: an id never carries two tokens, in any place, over the whole trace;
//! - I3: an id that was in a place and then was not never comes back there;
//! - I4: a token that left a place was closed (`done`, `drop`, a line
//!   deleted by hand); free text never leaves;
//! - I5: the remote has no item without an id, no run exits 4, and both
//!   clones stay on `main`;
//! - I6: a brief's status on the remote moves only along the edges of the
//!   state machine and its number keeps its file there; in every place a
//!   number is on one file, open or filed;
//! - every id a command printed or assigned is on the remote with its token,
//!   and nothing refused or never pushed is anywhere; a brief `new brief`
//!   printed is on the remote under its number, and the remote has as many
//!   more claimed briefs as `next` printed paths; the line `block`, `amend`
//!   or `report` wrote is in its brief, and one they refused is in none;
//! - convergence, at `Quiesce` and at the end: after `sync` on A, B and A,
//!   both clones are on the remote's commit with no board file changed,
//!   every token added and not closed is there once, and every line written
//!   into a brief, by hand or by a command, is in the brief or in a
//!   conflict copy of the clone that wrote it.
//!
//! Every item's text and every free-text line carries a token (`t7`) used
//! once, so a line applied twice, lost or given two ids shows.

mod common;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::process::ExitStatusExt;

use common::{Machine, World, pre_push, quote};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::strategy::ValueTree;
use proptest::test_runner::{
    Config, FileFailurePersistence, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    A,
    B,
}

impl Who {
    fn other(self) -> Who {
        match self {
            Who::A => Who::B,
            Who::B => Who::A,
        }
    }

    fn slot(self) -> usize {
        match self {
            Who::A => 0,
            Who::B => 1,
        }
    }
}

#[derive(Debug, Clone)]
enum Op {
    AddChore {
        group: bool,
    },
    AddBacklog,
    /// A chore of the clone or the remote, picked when the step runs.
    Done(Index),
    /// A backlog item of the clone or the remote, picked when the step runs.
    Drop(Index),
    NewBrief,
    /// A draft of the clone or the remote, picked when the step runs.
    /// `write`: its sections are written by hand first when the clone's file
    /// has none, as the planner does before it queues.
    Queue {
        pick: Index,
        write: bool,
    },
    Next,
    /// A started brief of the clone or the remote, picked when the step
    /// runs.
    Block(Index),
    /// A queued, started or blocked brief of the clone or the remote,
    /// picked when the step runs.
    Amend(Index),
    /// A started brief of the clone or the remote, picked when the step
    /// runs.
    Report(Index),
}

/// Which brief a hand edit writes into.
#[derive(Debug, Clone)]
enum BriefPick {
    /// A new one, numbered by hand one past the clone's highest: on another
    /// clone the tool may give that number meanwhile.
    New,
    /// The clone's open brief with the lowest number that has no sections
    /// yet, else the lowest: what the planner writes next, the same brief on
    /// both clones between syncs.
    First,
    Any(Index),
    /// The clone's brief with this number; never generated, a `queue` step
    /// writes it.
    Number(u32),
}

/// A change made in an editor, to a clone's files.
#[derive(Debug, Clone)]
enum Edit {
    /// An item without an id.
    AddIdLess { backlog: bool },
    /// A line of free text.
    AddText { backlog: bool },
    /// An item with an id typed by hand, past every id there is.
    AddTyped { backlog: bool },
    /// An item's text changed, its token kept.
    Change(Index),
    /// An item's line moved within its file.
    Move(Index, Index),
    /// An item's line deleted.
    Delete(Index),
    /// A line added to a brief's body, after the planner's sections when it
    /// has none yet.
    Brief(BriefPick),
}

#[derive(Debug, Clone)]
enum Fault {
    /// The other clone runs this while the command's first push waits.
    WindowOp(Op),
    /// A file of the clone is saved while the command's first push waits.
    /// A delete or a move hits an item without an id that the write
    /// publishes, added by hand before the write when the clone has none.
    WindowEdit(Edit),
    LostAck,
    /// The first push lands only after the second round's fetch.
    LateLanding,
    KillBeforePush,
    KillAfterPush,
}

#[derive(Debug, Clone)]
enum Step {
    Cmd {
        who: Who,
        op: Op,
        fault: Option<Fault>,
    },
    HandEdit {
        who: Who,
        edit: Edit,
    },
    /// The clone's remote is reachable, or not.
    Net {
        who: Who,
        online: bool,
    },
    /// The remote refuses the next pushes, this many.
    RejectPushes(u8),
    Sync {
        who: Who,
    },
    Quiesce,
}

fn who() -> impl Strategy<Value = Who> {
    prop_oneof![Just(Who::A), Just(Who::B)]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => any::<bool>().prop_map(|group| Op::AddChore { group }),
        3 => Just(Op::AddBacklog),
        2 => any::<Index>().prop_map(Op::Done),
        2 => any::<Index>().prop_map(Op::Drop),
        2 => Just(Op::NewBrief),
        3 => (any::<Index>(), prop::bool::weighted(0.8))
            .prop_map(|(pick, write)| Op::Queue { pick, write }),
        2 => Just(Op::Next),
        2 => any::<Index>().prop_map(Op::Block),
        2 => any::<Index>().prop_map(Op::Amend),
        2 => any::<Index>().prop_map(Op::Report),
    ]
}

fn brief_pick() -> impl Strategy<Value = BriefPick> {
    prop_oneof![
        1 => Just(BriefPick::New),
        6 => Just(BriefPick::First),
        2 => any::<Index>().prop_map(BriefPick::Any),
    ]
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        3 => any::<bool>().prop_map(|backlog| Edit::AddIdLess { backlog }),
        1 => any::<bool>().prop_map(|backlog| Edit::AddText { backlog }),
        1 => any::<bool>().prop_map(|backlog| Edit::AddTyped { backlog }),
        2 => any::<Index>().prop_map(Edit::Change),
        2 => (any::<Index>(), any::<Index>()).prop_map(|(from, to)| Edit::Move(from, to)),
        2 => any::<Index>().prop_map(Edit::Delete),
        5 => brief_pick().prop_map(Edit::Brief),
    ]
}

fn fault() -> impl Strategy<Value = Option<Fault>> {
    prop_oneof![
        8 => Just(None),
        2 => op().prop_map(|op| Some(Fault::WindowOp(op))),
        3 => edit().prop_map(|edit| Some(Fault::WindowEdit(edit))),
        1 => Just(Some(Fault::LostAck)),
        1 => Just(Some(Fault::LateLanding)),
        1 => Just(Some(Fault::KillBeforePush)),
        2 => Just(Some(Fault::KillAfterPush)),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        10 => (who(), op(), fault()).prop_map(|(who, op, fault)| Step::Cmd { who, op, fault }),
        5 => (who(), edit()).prop_map(|(who, edit)| Step::HandEdit { who, edit }),
        3 => (who(), prop::bool::weighted(0.65)).prop_map(|(who, online)| Step::Net { who, online }),
        2 => who().prop_map(|who| Step::Sync { who }),
        1 => (0..6u8).prop_map(Step::RejectPushes),
        1 => Just(Step::Quiesce),
    ]
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(step(), 1..=30)
}

/// The list files, with the prefix of their ids.
const FILES: [(&str, char); 2] = [("proj/chores.md", 'c'), ("proj/backlog.md", 'b')];

/// What the planner writes below a brief's header before it queues it.
const SECTIONS: &str = "# By hand\n\n## Task\nt\n## Design\nd\n## Premises\np\n\
                        ## Out of scope\no\n## Verification\nv\n\
                        ## Not doable by the worker\nn\n## Worker protocol\nw\n";

/// The legal moves of a brief's status, as SPEC.md draws them; written out
/// here rather than taken from the tool.
const EDGES: [(&str, &str); 6] = [
    ("draft", "queued"),
    ("queued", "started"),
    ("started", "blocked"),
    ("blocked", "started"),
    ("started", "reported"),
    ("reported", "filed"),
];

/// Whether a brief may move from `from` to `to` in any number of moves.
fn reachable(from: &str, to: &str) -> bool {
    let mut seen = vec![from];
    let mut at = 0;
    while let Some(status) = seen.get(at).copied() {
        for (a, b) in EDGES {
            if a == status && !seen.contains(&b) {
                seen.push(b);
            }
        }
        at += 1;
    }
    seen.contains(&to)
}

/// A brief file as the oracle reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BriefSeen {
    number: u32,
    /// The file name, without `done/`.
    name: String,
    filed: bool,
    /// The value of the first `status:` line of the header.
    status: Option<String>,
    text: String,
}

/// Whether the planner's sections are in the brief.
fn written(brief: &BriefSeen) -> bool {
    brief.text.lines().any(|line| line == "## Task")
}

/// A brief of the path grammar: `<digits>-<name>.md`.
fn brief_number(file_name: &str) -> Option<u32> {
    let (digits, rest) = file_name.split_once('-')?;
    (!digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && rest.len() > ".md".len()
        && rest.ends_with(".md"))
    .then(|| digits.parse().ok())
    .flatten()
}

fn brief_seen(path: &str, text: String) -> Option<BriefSeen> {
    let rest = path.strip_prefix("proj/briefs/")?;
    let (filed, name) = match rest.strip_prefix("done/") {
        Some(name) => (true, name),
        None => (false, rest),
    };
    if name.contains('/') {
        return None;
    }
    let number = brief_number(name)?;
    let status = text
        .lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| line.strip_prefix("status: "))
        .map(str::to_owned);
    Some(BriefSeen {
        number,
        name: name.to_owned(),
        filed,
        status,
        text,
    })
}

/// A list id: `('c', 12)`.
type Id = (char, u32);

/// One line of a list file as the oracle reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// `- [<id> ]<token>[ …]`
    Item { id: Option<Id>, token: String },
    /// `note <token>`
    Text { token: String },
}

impl Line {
    fn token(&self) -> &str {
        match self {
            Line::Item { token, .. } | Line::Text { token } => token,
        }
    }
}

/// The id a word is in the file of `prefix`, as the tool reads it.
fn id_of(prefix: char, word: &str) -> Option<Id> {
    let digits = word.strip_prefix(prefix)?;
    (digits.starts_with(|c: char| ('1'..='9').contains(&c))
        && digits.bytes().all(|b| b.is_ascii_digit()))
    .then(|| digits.parse().ok().map(|n| (prefix, n)))
    .flatten()
}

fn parse(prefix: char, bytes: &[u8]) -> Vec<Line> {
    let text = String::from_utf8(bytes.to_vec()).expect("the trace writes UTF-8");
    let token = |word: Option<&str>| word.unwrap_or_default().trim_end_matches('.').to_owned();
    text.lines()
        .filter_map(|line| {
            if let Some(item) = line.strip_prefix("- ") {
                let mut words = item.split(' ');
                let first = words.next();
                Some(match first.and_then(|word| id_of(prefix, word)) {
                    Some(id) => Line::Item {
                        id: Some(id),
                        token: token(words.next()),
                    },
                    None => Line::Item {
                        id: None,
                        token: token(first),
                    },
                })
            } else {
                line.strip_prefix("note ").map(|rest| Line::Text {
                    token: token(rest.split(' ').next()),
                })
            }
        })
        .collect()
}

/// What one `board` run did.
#[derive(Debug)]
struct Run {
    /// `None` when killed.
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// What a step's command is about: the token it adds, or the id it closes.
#[derive(Debug, Clone)]
enum Subject {
    Add(String),
    Close(Id),
    /// The slug, a token.
    NewBrief(String),
    Queue(u32),
    Next,
    /// The brief, and the token of the line the command writes into it.
    Block(u32, String),
    Amend(u32, String),
    Report(u32, String),
}

impl Subject {
    /// The brief a command writes a line into, the line's token and the
    /// command's word.
    fn brief_line(&self) -> Option<(u32, &str, &'static str)> {
        match self {
            Subject::Block(number, token) => Some((*number, token, "block")),
            Subject::Amend(number, token) => Some((*number, token, "amend")),
            Subject::Report(number, token) => Some((*number, token, "report")),
            Subject::Add(_)
            | Subject::Close(_)
            | Subject::NewBrief(_)
            | Subject::Queue(_)
            | Subject::Next => None,
        }
    }

    /// What the command reads from stdin: the text of `amend` and `report`.
    fn input(&self) -> Option<String> {
        match self {
            Subject::Amend(_, token) | Subject::Report(_, token) => Some(format!("body {token}\n")),
            Subject::Block(..)
            | Subject::Add(_)
            | Subject::Close(_)
            | Subject::NewBrief(_)
            | Subject::Queue(_)
            | Subject::Next => None,
        }
    }
}

/// What a hand edit does, for the oracle to record once it is made.
#[derive(Debug, Default)]
struct Effect {
    /// Tokens added to a list.
    added: Vec<String>,
    /// Tokens of lines deleted.
    deleted: Vec<String>,
    /// A token written into a brief, and the brief's number.
    brief: Option<(String, u32)>,
    /// A line deleted or moved while its id is being published.
    published: bool,
}

/// Counts of what the trace went through, for the depth check.
#[derive(Debug, Default, Clone, Copy)]
struct Depth {
    commands: u32,
    succeeded: u32,
    retried_then_succeeded: u32,
    raced_in_the_window: u32,
    not_pushed: u32,
    refused: u32,
    offline_refused: u32,
    not_pushed_then_pushed: u32,
    merged_by_line: u32,
    assigned: u32,
    window_edit_settled: u32,
    published_line_edited: u32,
    brief_conflicts: u32,
    lost_ack_recognised: u32,
    late_landing_recognised: u32,
    kill_after_push_healed: u32,
    briefs_created: u32,
    briefs_queued: u32,
    briefs_claimed: u32,
    queue_empty: u32,
    briefs_blocked: u32,
    briefs_resumed: u32,
    briefs_reported: u32,
}

impl Depth {
    fn add(&mut self, other: Depth) {
        self.commands += other.commands;
        self.succeeded += other.succeeded;
        self.retried_then_succeeded += other.retried_then_succeeded;
        self.raced_in_the_window += other.raced_in_the_window;
        self.not_pushed += other.not_pushed;
        self.refused += other.refused;
        self.offline_refused += other.offline_refused;
        self.not_pushed_then_pushed += other.not_pushed_then_pushed;
        self.merged_by_line += other.merged_by_line;
        self.assigned += other.assigned;
        self.window_edit_settled += other.window_edit_settled;
        self.published_line_edited += other.published_line_edited;
        self.brief_conflicts += other.brief_conflicts;
        self.lost_ack_recognised += other.lost_ack_recognised;
        self.late_landing_recognised += other.late_landing_recognised;
        self.kill_after_push_healed += other.kill_after_push_healed;
        self.briefs_created += other.briefs_created;
        self.briefs_queued += other.briefs_queued;
        self.briefs_claimed += other.briefs_claimed;
        self.queue_empty += other.queue_empty;
        self.briefs_blocked += other.briefs_blocked;
        self.briefs_resumed += other.briefs_resumed;
        self.briefs_reported += other.briefs_reported;
    }
}

/// What the oracle last saw in one place.
#[derive(Debug, Default)]
struct Place {
    ids: BTreeSet<Id>,
    /// Ids that were here and then were not (I3).
    gone: BTreeSet<Id>,
    tokens: BTreeSet<String>,
}

struct Oracle<'w> {
    world: &'w World,
    tokens: u32,
    /// A, B and the remote.
    places: [Place; 3],
    /// Every id ever seen, with its token (I2).
    owners: BTreeMap<Id, String>,
    /// Tokens added to a list by a command that succeeded or by hand.
    added: BTreeSet<String>,
    /// Tokens of adds that were refused, not pushed or killed before their
    /// push: never anywhere.
    never: BTreeSet<String>,
    /// Tokens whose line was deleted, by `done`, `drop` or by hand.
    closed: BTreeSet<String>,
    /// Tokens of a `done` or `drop` that was killed: closed or not.
    maybe_closed: BTreeSet<String>,
    /// Lines written into briefs: token, writer, brief number.
    brief_lines: Vec<(String, Who, u32)>,
    /// Per brief number on the remote, its file name and the status last
    /// seen (I6).
    remote_briefs: BTreeMap<u32, (String, Option<String>)>,
    /// Slugs of `new brief` runs that were refused or never pushed: on no
    /// file anywhere.
    briefs_never: BTreeSet<String>,
    /// Tokens of lines `block`, `amend` or `report` were refused to write:
    /// in no brief anywhere.
    lines_never: BTreeSet<String>,
    /// The highest id typed by hand so far, per list.
    typed: BTreeMap<char, u32>,
    online: [bool; 2],
    /// A clone with commits a write said it did not push.
    unpushed: [bool; 2],
    /// A clone whose command was killed after its push landed: the next
    /// command there has to work.
    healing: [bool; 2],
    depth: Depth,
}

impl<'w> Oracle<'w> {
    fn new(world: &'w World) -> Oracle<'w> {
        Oracle {
            world,
            tokens: 0,
            places: Default::default(),
            owners: BTreeMap::new(),
            added: BTreeSet::new(),
            never: BTreeSet::new(),
            closed: BTreeSet::new(),
            maybe_closed: BTreeSet::new(),
            brief_lines: Vec::new(),
            remote_briefs: BTreeMap::new(),
            briefs_never: BTreeSet::new(),
            lines_never: BTreeSet::new(),
            typed: BTreeMap::new(),
            online: [true; 2],
            unpushed: [false; 2],
            healing: [false; 2],
            depth: Depth::default(),
        }
    }

    fn machine(&self, who: Who) -> Machine<'w> {
        match who {
            Who::A => self.world.a(),
            Who::B => self.world.b(),
        }
    }

    fn token(&mut self) -> String {
        self.tokens += 1;
        format!("t{}", self.tokens)
    }

    /// The lines of a place's list files: 0 and 1 are the clones' files, 2
    /// the remote's `main`.
    fn read(&self, place: usize) -> Vec<(usize, Vec<Line>)> {
        FILES
            .iter()
            .enumerate()
            .map(|(file, (path, prefix))| {
                let bytes = match place {
                    0 => self.world.a().file(path),
                    1 => self.world.b().file(path),
                    _ => self.world.remote_file(path),
                };
                (file, parse(*prefix, &bytes.unwrap_or_default()))
            })
            .collect()
    }

    fn ids_in(&self, place: usize, prefix: char) -> Vec<u32> {
        self.read(place)
            .into_iter()
            .flat_map(|(_, lines)| lines)
            .filter_map(|line| match line {
                Line::Item {
                    id: Some((p, n)), ..
                } if p == prefix => Some(n),
                Line::Item { .. } | Line::Text { .. } => None,
            })
            .collect()
    }

    /// The brief files of a place: 0 and 1 are the clones' files, 2 the
    /// remote's `main`.
    fn briefs(&self, place: usize) -> Vec<BriefSeen> {
        let files: Vec<(String, String)> = match place {
            0 | 1 => {
                let machine = if place == 0 {
                    self.world.a()
                } else {
                    self.world.b()
                };
                ["proj/briefs", "proj/briefs/done"]
                    .into_iter()
                    .flat_map(|dir| {
                        let entries = fs::read_dir(machine.data().join(dir));
                        entries
                            .into_iter()
                            .flatten()
                            .map(|entry| entry.unwrap())
                            .filter(|entry| entry.file_type().unwrap().is_file())
                            .map(move |entry| {
                                format!("{dir}/{}", entry.file_name().to_string_lossy())
                            })
                    })
                    .map(|path| {
                        let text = String::from_utf8(machine.file(&path).unwrap()).unwrap();
                        (path, text)
                    })
                    .collect()
            }
            _ => self
                .world
                .remote_files("proj/briefs")
                .into_iter()
                .map(|path| {
                    let text = String::from_utf8(self.world.remote_file(&path).unwrap()).unwrap();
                    (path, text)
                })
                .collect(),
        };
        let mut briefs: Vec<BriefSeen> = files
            .into_iter()
            .filter_map(|(path, text)| brief_seen(&path, text))
            .collect();
        briefs.sort_by_key(|brief| (brief.number, brief.filed, brief.name.clone()));
        briefs
    }

    /// The numbers of the drafts of the clone and the remote, the ones the
    /// clone has written when there are any: the planner queues what it
    /// wrote, and a refusal for an empty section is the rarer case.
    fn drafts(&self, who: Who) -> Vec<u32> {
        let drafts: Vec<(u32, bool)> = [who.slot(), 2]
            .into_iter()
            .flat_map(|place| {
                self.briefs(place)
                    .into_iter()
                    .map(move |brief| (place, brief))
            })
            .filter(|(_, brief)| !brief.filed && brief.status.as_deref() == Some("draft"))
            .map(|(place, brief)| (brief.number, place != 2 && written(&brief)))
            .collect();
        let pick = |only_written: bool| -> BTreeSet<u32> {
            drafts
                .iter()
                .filter(|(_, written)| *written || !only_written)
                .map(|(number, _)| *number)
                .collect()
        };
        let written = pick(true);
        let numbers = if written.is_empty() {
            pick(false)
        } else {
            written
        };
        numbers.into_iter().collect()
    }

    /// How many briefs of the remote were claimed: started or past it.
    fn claimed_on_remote(&self) -> usize {
        self.briefs(2)
            .iter()
            .filter(|brief| {
                brief.filed
                    || brief
                        .status
                        .as_deref()
                        .is_some_and(|status| reachable("started", status))
            })
            .count()
    }

    /// The command line for an op of `who`, on what the clone and the
    /// remote have now.
    fn args(&mut self, who: Who, op: &Op) -> (Vec<String>, Subject) {
        let pick = |oracle: &Self, prefix: char, index: &Index| -> Id {
            let ids: BTreeSet<u32> = oracle
                .ids_in(who.slot(), prefix)
                .into_iter()
                .chain(oracle.ids_in(2, prefix))
                .collect();
            let ids: Vec<u32> = ids.into_iter().collect();
            // Nothing to pick: an id that no item has, for a refusal.
            let number = if ids.is_empty() {
                999_999
            } else {
                *index.get(&ids)
            };
            (prefix, number)
        };
        let owned = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect();
        match op {
            Op::AddChore { group } => {
                let token = self.token();
                let group = if *group { "Home" } else { "Work" };
                (
                    owned(&["add", "chore", &token, "--group", group, "--check", "x"]),
                    Subject::Add(token),
                )
            }
            Op::AddBacklog => {
                let token = self.token();
                (owned(&["add", "backlog", &token]), Subject::Add(token))
            }
            Op::Done(index) => {
                let id = pick(self, 'c', index);
                (owned(&["done", &format!("c{}", id.1)]), Subject::Close(id))
            }
            Op::Drop(index) => {
                let id = pick(self, 'b', index);
                (owned(&["drop", &format!("b{}", id.1)]), Subject::Close(id))
            }
            Op::NewBrief => {
                let slug = self.token();
                (owned(&["new", "brief", &slug]), Subject::NewBrief(slug))
            }
            Op::Queue { pick, .. } => {
                // Nothing to pick: a brief that is not there, for a refusal.
                let drafts = self.drafts(who);
                let number = if drafts.is_empty() {
                    999_999
                } else {
                    *pick.get(&drafts)
                };
                (
                    owned(&["queue", &number.to_string()]),
                    Subject::Queue(number),
                )
            }
            Op::Next => (owned(&["next"]), Subject::Next),
            Op::Block(pick) | Op::Amend(pick) | Op::Report(pick) => {
                let statuses: &[&str] = match op {
                    Op::Amend(_) => &["queued", "started", "blocked"],
                    _ => &["started"],
                };
                let numbers = self.open_with(who, statuses);
                // Nothing to pick: a brief that is not there, for a refusal.
                let number = if numbers.is_empty() {
                    999_999
                } else {
                    *pick.get(&numbers)
                };
                let token = self.token();
                let line = format!("body {token}");
                let (args, subject) = match op {
                    Op::Block(_) => (
                        owned(&["block", &number.to_string(), &line]),
                        Subject::Block(number, token),
                    ),
                    Op::Amend(_) => (
                        owned(&["amend", &number.to_string()]),
                        Subject::Amend(number, token),
                    ),
                    _ => (
                        owned(&["report", &number.to_string()]),
                        Subject::Report(number, token),
                    ),
                };
                (args, subject)
            }
        }
    }

    /// The numbers of the open briefs of the clone and the remote that have
    /// one of these statuses.
    fn open_with(&self, who: Who, statuses: &[&str]) -> Vec<u32> {
        let numbers: BTreeSet<u32> = [who.slot(), 2]
            .into_iter()
            .flat_map(|place| self.briefs(place))
            .filter(|brief| {
                !brief.filed
                    && brief
                        .status
                        .as_deref()
                        .is_some_and(|status| statuses.contains(&status))
            })
            .map(|brief| brief.number)
            .collect();
        numbers.into_iter().collect()
    }

    /// A hand edit of `who`'s files: the file and its new bytes, and what
    /// the edit does. `None` when there is nothing to edit. `window`: the
    /// edit is made while the clone's own write runs.
    fn edit(&mut self, who: Who, edit: &Edit, window: bool) -> Option<(String, Vec<u8>, Effect)> {
        let machine = self.machine(who);
        let mut effect = Effect::default();
        let list = |backlog: bool| FILES[usize::from(backlog)];
        let lines_of = |path: &str| -> Vec<String> {
            String::from_utf8(machine.file(path).unwrap_or_default())
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        };
        let suffix = |prefix: char| if prefix == 'c' { ". Check: x" } else { "" };
        let (path, lines) = match edit {
            Edit::AddIdLess { backlog }
            | Edit::AddText { backlog }
            | Edit::AddTyped { backlog } => {
                let (path, prefix) = list(*backlog);
                let token = self.token();
                let line = match edit {
                    Edit::AddIdLess { .. } => format!("- {token}{}", suffix(prefix)),
                    Edit::AddText { .. } => format!("note {token}"),
                    _ => {
                        // Past every id seen or typed, by more than a trace
                        // can allocate meanwhile: a collision is the example
                        // test's, not the trace's.
                        let seen = self
                            .owners
                            .keys()
                            .filter(|(p, _)| *p == prefix)
                            .map(|(_, n)| *n);
                        let typed = self.typed.get(&prefix).copied();
                        let n = seen.chain(typed).max().unwrap_or(0) + 100;
                        self.typed.insert(prefix, n);
                        format!("- {prefix}{n} {token}{}", suffix(prefix))
                    }
                };
                effect.added.push(token);
                let mut lines = lines_of(path);
                lines.push(line);
                (path.to_owned(), lines)
            }
            Edit::Change(index) | Edit::Move(index, _) | Edit::Delete(index) => {
                // Items of the clone. An item without an id whose id is
                // being published: its token on the remote with an id (a
                // kill after the push left the clone behind), or any in a
                // window. A delete or a move picks one of those when there
                // is one, as the merge has to read them by the published
                // id and nothing else gets there. A change of one keeps the
                // change under a new id, and a close of the published id on
                // the other clone does not win over it: the residue
                // `adopt_ids` names, left out.
                let remote: BTreeSet<String> = self
                    .read(2)
                    .into_iter()
                    .flat_map(|(_, lines)| lines)
                    .map(|line| line.token().to_owned())
                    .collect();
                let mut candidates = Vec::new();
                let mut publishing = Vec::new();
                for (file, (path, prefix)) in FILES.iter().enumerate() {
                    for (at, line) in lines_of(path).iter().enumerate() {
                        match parse(*prefix, line.as_bytes()).first() {
                            Some(Line::Item { id: Some(_), .. }) => candidates.push((file, at)),
                            Some(Line::Item { id: None, token })
                                if window || remote.contains(token) =>
                            {
                                publishing.push((file, at));
                            }
                            Some(Line::Item { id: None, .. }) => candidates.push((file, at)),
                            Some(Line::Text { .. }) | None => {}
                        }
                    }
                }
                if !publishing.is_empty() && !matches!(edit, Edit::Change(_)) {
                    candidates = publishing;
                    effect.published = true;
                }
                if candidates.is_empty() {
                    return None;
                }
                let (file, at) = *index.get(&candidates);
                let (path, prefix) = FILES[file];
                let mut lines = lines_of(path);
                let token = parse(prefix, lines[at].as_bytes())[0].token().to_owned();
                match edit {
                    Edit::Change(_) => {
                        let mark = self.token();
                        lines[at] = format!("{} {mark}", lines[at]);
                    }
                    Edit::Move(_, to) => {
                        let line = lines.remove(at);
                        lines.insert(to.index(lines.len() + 1), line);
                    }
                    _ => {
                        lines.remove(at);
                        effect.deleted.push(token);
                    }
                }
                (path.to_owned(), lines)
            }
            Edit::Brief(pick) => {
                let briefs = self.briefs(who.slot());
                let open: Vec<&BriefSeen> = briefs.iter().filter(|brief| !brief.filed).collect();
                // A brief made by hand while the clone's own write runs
                // could take the number that write gives: a duplicate the
                // trace would have typed, like an id that collides.
                let first = open
                    .iter()
                    .find(|brief| !written(brief))
                    .or_else(|| open.first());
                let chosen = match pick {
                    BriefPick::New if window => first,
                    BriefPick::New => None,
                    BriefPick::First => first,
                    BriefPick::Any(index) if !open.is_empty() => Some(index.get(&open)),
                    BriefPick::Any(_) => None,
                    BriefPick::Number(number) => {
                        Some(open.iter().find(|brief| brief.number == *number)?)
                    }
                };
                let (path, number, mut lines) = match chosen {
                    Some(brief) => (
                        format!("proj/briefs/{}", brief.name),
                        brief.number,
                        brief.text.lines().map(str::to_owned).collect(),
                    ),
                    None if window => return None,
                    None => {
                        let number = briefs.iter().map(|brief| brief.number).max().unwrap_or(0) + 1;
                        (
                            format!("proj/briefs/{number}-hand.md"),
                            number,
                            vec![
                                format!("id: {number}"),
                                "status: draft".to_owned(),
                                String::new(),
                            ],
                        )
                    }
                };
                if chosen.is_none_or(|brief| !written(brief)) {
                    lines.extend(SECTIONS.lines().map(str::to_owned));
                }
                let token = self.token();
                lines.push(format!("body {token}"));
                effect.brief = Some((token, number));
                (path, lines)
            }
        };
        let bytes = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        Some((path, bytes.into_bytes(), effect))
    }

    /// Whether the clone's files have an item without an id.
    fn has_id_less(&self, who: Who) -> bool {
        self.read(who.slot())
            .into_iter()
            .flat_map(|(_, lines)| lines)
            .any(|line| matches!(line, Line::Item { id: None, .. }))
    }

    /// Records a hand edit as made. Its new lines were in the clone's files
    /// from then on, seen or not: a write that lost them breaks I4.
    fn made(&mut self, who: Who, effect: Effect) {
        self.places[who.slot()]
            .tokens
            .extend(effect.added.iter().cloned());
        self.added.extend(effect.added);
        self.closed.extend(effect.deleted);
        if effect.published {
            self.depth.published_line_edited += 1;
        }
        if let Some((token, brief)) = effect.brief {
            self.brief_lines.push((token, who, brief));
        }
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        match step {
            Step::RejectPushes(pushes) => {
                self.world.reject_pushes(u32::from(*pushes));
                Ok(())
            }
            Step::Quiesce => self.quiesce(),
            Step::Net { who, online } => {
                self.machine(*who).set_online(*online);
                self.online[who.slot()] = *online;
                Ok(())
            }
            Step::HandEdit { who, edit } => {
                if let Some((path, bytes, effect)) = self.edit(*who, edit, false) {
                    self.machine(*who).write(&path, bytes);
                    self.made(*who, effect);
                }
                self.check_everywhere()
            }
            Step::Sync { who } => self.sync(*who),
            Step::Cmd { who, op, fault } => self.command(*who, op, fault.as_ref()),
        }
    }

    /// `board sync` on a clone outside `Quiesce`: exit 0 online, 3 without
    /// the remote or with every push refused.
    fn sync(&mut self, who: Who) -> Result<(), TestCaseError> {
        let rejecting = self.world.rejects_left() > 0;
        let output = self
            .machine(who)
            .command()
            .arg("sync")
            .output()
            .expect("run board");
        let run = Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        };
        self.depth.commands += 1;
        match run.code {
            Some(0) => {
                self.depth.succeeded += 1;
                prop_assert_eq!(&run.stdout, "synced\n");
            }
            Some(3) => {
                self.depth.not_pushed += 1;
                prop_assert!(
                    !self.online[who.slot()] || rejecting,
                    "sync exit 3 with the remote there: {run:?}"
                );
            }
            _ => prop_assert!(false, "unexpected exit of sync: {run:?}"),
        }
        self.notes(who, &run)?;
        self.check_everywhere()
    }

    /// For a `queue` that writes first: the planner's sections into the
    /// clone's file of the brief, by hand, when it has none.
    fn write_before_queue(
        &mut self,
        who: Who,
        op: &Op,
        subject: &Subject,
    ) -> Result<(), TestCaseError> {
        let (Op::Queue { write: true, .. }, Subject::Queue(number)) = (op, subject) else {
            return Ok(());
        };
        let unwritten = self
            .briefs(who.slot())
            .iter()
            .any(|brief| brief.number == *number && !brief.filed && !written(brief));
        if unwritten
            && let Some((path, bytes, effect)) =
                self.edit(who, &Edit::Brief(BriefPick::Number(*number)), false)
        {
            self.machine(who).write(&path, bytes);
            self.made(who, effect);
        }
        self.check_everywhere()
    }

    /// For `block`, `amend` and `report` when neither the clone nor the
    /// remote has a brief they take: the commands that bring one there, as
    /// the planner and the worker would have run them, each judged as a
    /// command of its own. Left to chance, a trace seldom gets a brief from
    /// new to started before it ends.
    fn prepare(&mut self, who: Who, op: &Op) -> Result<(), TestCaseError> {
        let wanted: &[&str] = match op {
            Op::Block(_) | Op::Report(_) => &["started"],
            Op::Amend(_) => &["queued", "started", "blocked"],
            Op::AddChore { .. }
            | Op::AddBacklog
            | Op::Done(_)
            | Op::Drop(_)
            | Op::NewBrief
            | Op::Queue { .. }
            | Op::Next => return Ok(()),
        };
        // At most new, queue and next; fewer when one is refused.
        for _ in 0..3 {
            if !self.open_with(who, wanted).is_empty() {
                break;
            }
            let before = if !self.open_with(who, &["queued"]).is_empty() {
                Op::Next
            } else if !self.drafts(who).is_empty() {
                Op::Queue {
                    pick: first(),
                    write: true,
                }
            } else {
                Op::NewBrief
            };
            self.command(who, &before, None)?;
        }
        Ok(())
    }

    fn command(&mut self, who: Who, op: &Op, fault: Option<&Fault>) -> Result<(), TestCaseError> {
        let machine = self.machine(who);
        let other = self.machine(who.other());
        self.prepare(who, op)?;
        if let Some(Fault::WindowOp(window_op)) = fault {
            self.prepare(who.other(), window_op)?;
        }
        let rejecting = self.world.rejects_left() > 0;
        let (args, subject) = self.args(who, op);
        self.write_before_queue(who, op, &subject)?;
        let mut window = None;
        let mut window_edit = None;
        match fault {
            None => {}
            Some(Fault::WindowOp(window_op)) => {
                let (window_args, window_subject) = self.args(who.other(), window_op);
                self.write_before_queue(who.other(), window_op, &window_subject)?;
                let words: Vec<&str> = window_args.iter().map(String::as_str).collect();
                let script = match window_subject.input() {
                    Some(input) => {
                        let file = self.world.dir().join("window-input");
                        fs::write(&file, input).unwrap();
                        other.script_with_input(&words, &file)
                    }
                    None => other.script(&words),
                };
                machine.hook_once("pre-push", &pre_push::window(&script, self.world.dir()));
                window = Some(window_subject);
            }
            Some(Fault::WindowEdit(edit)) => {
                if let Edit::Move(index, _) | Edit::Delete(index) = edit
                    && !self.has_id_less(who)
                    && let Some((path, bytes, effect)) = self.edit(
                        who,
                        &Edit::AddIdLess {
                            backlog: index.index(2) == 1,
                        },
                        false,
                    )
                {
                    machine.write(&path, bytes);
                    self.made(who, effect);
                }
                if let Some((path, bytes, effect)) = self.edit(who, edit, true) {
                    let saved = self.world.dir().join("window-edit");
                    fs::write(&saved, bytes).unwrap();
                    machine.hook_once(
                        "pre-push",
                        &format!(
                            "mkdir -p \"$(dirname {path})\" && cp {} {path} || exit 1\n",
                            quote(&saved.to_string_lossy()),
                            path = quote(&path)
                        ),
                    );
                    window_edit = Some(effect);
                }
            }
            Some(Fault::LostAck) => machine.hook_once("pre-push", pre_push::LOST_ACK),
            Some(Fault::LateLanding) => {
                machine.hook("pre-push", &pre_push::late_landing(self.world.dir()));
            }
            Some(Fault::KillBeforePush) => {
                machine.hook_once("pre-push", pre_push::KILL_BEFORE_PUSH)
            }
            Some(Fault::KillAfterPush) => machine.hook_once("pre-push", pre_push::KILL_AFTER_PUSH),
        }

        let claimed_before = self.claimed_on_remote();
        let mut command = machine.command();
        command.args(&args);
        if let Some(input) = subject.input() {
            let file = self.world.dir().join("input");
            fs::write(&file, input).unwrap();
            command.stdin(fs::File::open(&file).unwrap());
        }
        let output = command.output().expect("run board");
        // A one-shot hook that is gone has run.
        let hook_ran = !machine.has_hook("pre-push");
        machine.unhook("pre-push");
        // A late landing whose second push never came.
        match fs::remove_file(self.world.dir().join(pre_push::LATE_LANDING_FILE)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove {}: {error}", pre_push::LATE_LANDING_FILE),
        }
        let run = Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        };
        prop_assert!(
            run.code.is_some() || output.status.signal() == Some(9),
            "{run:?}"
        );
        let killable = matches!(fault, Some(Fault::KillBeforePush | Fault::KillAfterPush));
        prop_assert!(
            run.code.is_some() || killable,
            "killed without a kill: {run:?}"
        );
        let window_edit_made = match window_edit {
            Some(effect) if hook_ran => {
                self.made(who, effect);
                true
            }
            Some(_) | None => false,
        };

        // The other clone's run, when the hook ran.
        let window_run = match window {
            Some(window_subject) => {
                match fs::read_to_string(self.world.dir().join("window.code")) {
                    Ok(code) => {
                        let read = |name: &str| {
                            let path = self.world.dir().join(name);
                            let text = fs::read_to_string(&path).unwrap();
                            fs::remove_file(path).unwrap();
                            text
                        };
                        let run = Run {
                            code: Some(code.trim().parse().unwrap()),
                            stdout: read("window.out"),
                            stderr: read("window.err"),
                        };
                        fs::remove_file(self.world.dir().join("window.code")).unwrap();
                        Some((window_subject, run))
                    }
                    Err(_) => None,
                }
            }
            None => None,
        };

        if let Some((window_subject, window_run)) = &window_run {
            self.judge(
                who.other(),
                window_subject,
                window_run,
                rejecting,
                None,
                true,
            )?;
            if window_run.code == Some(0) && run.code == Some(0) && run.stderr.contains("retry") {
                self.depth.raced_in_the_window += 1;
            }
        }
        self.judge(who, &subject, &run, rejecting, fault, false)?;

        // Every path `next` printed is one more claimed brief on the remote,
        // and no brief is claimed twice; a run that ended without knowing
        // whether its push landed may have claimed one more.
        let runs = std::iter::once((&subject, &run))
            .chain(window_run.as_ref().map(|(subject, run)| (subject, run)))
            .filter(|(subject, _)| matches!(subject, Subject::Next));
        let (mut printed, mut unsure) = (0, 0);
        for (_, run) in runs {
            match run.code {
                Some(0) if run.stdout != "queue empty\n" => printed += 1,
                Some(0 | 1) => {}
                Some(_) | None => unsure += 1,
            }
        }
        let claimed = self.claimed_on_remote() as i64 - claimed_before as i64;
        prop_assert!(
            (printed..=printed + unsure).contains(&claimed),
            "{} briefs claimed on the remote, {} paths printed: {:?}",
            claimed,
            printed,
            run
        );

        // Depth: what this command went through.
        let slot = who.slot();
        if run.code == Some(0) {
            if std::mem::take(&mut self.healing[slot]) {
                self.depth.kill_after_push_healed += 1;
            }
            if matches!(fault, Some(Fault::LostAck)) && hook_ran && !rejecting {
                self.depth.lost_ack_recognised += 1;
            }
            if matches!(fault, Some(Fault::LateLanding))
                && !rejecting
                && run.stderr.contains("retry 3/4")
            {
                self.depth.late_landing_recognised += 1;
            }
            if window_edit_made && run.stderr.contains("board: merged ") {
                self.depth.window_edit_settled += 1;
            }
        }
        if run.code.is_none()
            && matches!(fault, Some(Fault::KillAfterPush))
            && matches!(&subject, Subject::Add(token) if self.added.contains(token))
        {
            self.healing[slot] = true;
        }
        self.check_everywhere()
    }

    /// Checks the notes of a run on `who` that ended: each line is a note
    /// the trace can cause, or the one line of a run that failed; an id
    /// assigned is on the remote with its token.
    fn notes(&mut self, who: Who, run: &Run) -> Result<(), TestCaseError> {
        let lines: Vec<&str> = run.stderr.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            let last = at + 1 == lines.len();
            if let Some(assigned) = line.strip_prefix("board: assigned ") {
                self.depth.assigned += 1;
                let (id, text) = assigned.split_once(": ").expect("assigned <id>: <text>");
                let prefix = id.chars().next().unwrap();
                let id = id_of(prefix, id).expect("an id");
                let token = text.split(' ').next().unwrap().trim_end_matches('.');
                let file = if prefix == 'c' { 0 } else { 1 };
                let on_remote = self.read(2).remove(file).1;
                prop_assert!(
                    on_remote.contains(&Line::Item {
                        id: Some(id),
                        token: token.to_owned()
                    }),
                    "assigned {:?} is not on the remote with {}: {:?}",
                    id,
                    token,
                    run
                );
            } else if line.starts_with("board: merged ") {
                if line.contains("(by line: ") {
                    self.depth.merged_by_line += 1;
                }
            } else if line.starts_with("board: conflict: brief ") {
                self.depth.brief_conflicts += 1;
            } else if line.starts_with("board: lint: ") {
                prop_assert!(
                    line.contains("unmerged local version of brief"),
                    "a lint finding the trace does not make: {run:?}"
                );
            } else if line.starts_with("board: retry ") {
            } else if line.starts_with("board: not pushed: ") {
                self.unpushed[who.slot()] = true;
            } else {
                prop_assert!(
                    last && run.code != Some(0),
                    "a line the trace does not expect: {line:?} in {run:?}"
                );
            }
        }
        let pushed_all = run.code == Some(0) && !run.stderr.contains("board: not pushed: ");
        if pushed_all && self.online[who.slot()] && std::mem::take(&mut self.unpushed[who.slot()]) {
            self.depth.not_pushed_then_pushed += 1;
        }
        Ok(())
    }

    /// Checks one run of `who` against the places after the step, and
    /// records what became of its subject. `window`: the run was made while
    /// the step's command pushed, which went on after it.
    fn judge(
        &mut self,
        who: Who,
        subject: &Subject,
        run: &Run,
        rejecting: bool,
        fault: Option<&Fault>,
        window: bool,
    ) -> Result<(), TestCaseError> {
        self.depth.commands += 1;
        let offline = !self.online[who.slot()];
        match run.code {
            Some(0) => {
                self.depth.succeeded += 1;
                if run.stderr.contains("retry") {
                    self.depth.retried_then_succeeded += 1;
                }
            }
            Some(1) => {
                self.depth.refused += 1;
                prop_assert!(
                    matches!(subject, Subject::Close(_) | Subject::Queue(_))
                        || subject.brief_line().is_some(),
                    "{run:?}"
                );
            }
            Some(3) => {
                self.depth.not_pushed += 1;
                if offline {
                    self.depth.offline_refused += 1;
                }
                prop_assert!(
                    offline || rejecting,
                    "exit 3 with the remote taking pushes: {run:?}"
                );
                prop_assert!(
                    matches!(
                        subject,
                        Subject::Add(_) | Subject::NewBrief(_) | Subject::Next
                    ),
                    "{run:?}"
                );
            }
            None => {}
            Some(_) => prop_assert!(false, "unexpected exit: {run:?}"),
        }
        if run.code.is_some() {
            self.notes(who, run)?;
        }
        let pushed = run.code == Some(0) && !run.stderr.contains("board: not pushed: ");
        match subject {
            Subject::Add(token) => {
                let on_remote: Vec<Id> = self
                    .read(2)
                    .into_iter()
                    .flat_map(|(_, lines)| lines)
                    .filter(|line| line.token() == token)
                    .filter_map(|line| match line {
                        Line::Item { id, .. } => id,
                        Line::Text { .. } => None,
                    })
                    .collect();
                match run.code {
                    Some(0) => {
                        prop_assert!(!offline, "an add offline exited 0: {run:?}");
                        // The printed id is on the remote with this token.
                        let printed = run.stdout.trim_end();
                        let shown: Vec<String> =
                            on_remote.iter().map(|(p, n)| format!("{p}{n}")).collect();
                        prop_assert_eq!(&shown, &vec![printed.to_owned()], "{:?}", run);
                        self.added.insert(token.clone());
                    }
                    None if matches!(fault, Some(Fault::KillAfterPush))
                        && !on_remote.is_empty() =>
                    {
                        self.added.insert(token.clone());
                    }
                    Some(_) | None => {
                        self.never.insert(token.clone());
                    }
                }
            }
            Subject::Close(id) => {
                let token = self.owners.get(id).cloned();
                let here = self.ids_in(who.slot(), id.0).contains(&id.1);
                match run.code {
                    Some(0) => {
                        let past = if id.0 == 'c' { "done" } else { "dropped" };
                        prop_assert_eq!(&run.stdout, &format!("{past} {}{}\n", id.0, id.1));
                        prop_assert!(!here, "{id:?} is still in {who:?} after {run:?}");
                        if pushed {
                            prop_assert!(
                                !self.ids_in(2, id.0).contains(&id.1),
                                "{id:?} is still on the remote after {run:?}"
                            );
                        }
                        self.closed.extend(token);
                    }
                    Some(1) => {
                        prop_assert!(
                            run.stderr.ends_with("no such chore\n")
                                || run.stderr.ends_with("no such backlog item\n"),
                            "{:?}",
                            run
                        );
                        prop_assert!(!here, "{id:?} is in {who:?} after {run:?}");
                    }
                    Some(_) | None => self.maybe_closed.extend(token),
                }
            }
            Subject::NewBrief(slug) => {
                let name_end = format!("-{slug}.md");
                let on_remote: Vec<BriefSeen> = self
                    .briefs(2)
                    .into_iter()
                    .filter(|brief| brief.name.ends_with(&name_end))
                    .collect();
                match run.code {
                    Some(0) => {
                        prop_assert!(!offline, "a new brief offline exited 0: {run:?}");
                        // The printed path is the clone's, of the brief on
                        // the remote under its number.
                        let dir = format!("{}/proj/briefs/", self.machine(who).data().display());
                        let printed = run
                            .stdout
                            .strip_prefix(&dir)
                            .and_then(|name| name.strip_suffix('\n'));
                        let names: Vec<&str> =
                            on_remote.iter().map(|brief| brief.name.as_str()).collect();
                        prop_assert_eq!(
                            names,
                            printed.into_iter().collect::<Vec<_>>(),
                            "{:?}",
                            run
                        );
                        prop_assert!(
                            on_remote.iter().all(|brief| !brief.filed
                                && brief.text.starts_with(&format!("id: {}\n", brief.number))),
                            "{:?}",
                            on_remote
                        );
                        self.depth.briefs_created += 1;
                    }
                    None if matches!(fault, Some(Fault::KillAfterPush))
                        && !on_remote.is_empty() => {}
                    Some(_) | None => {
                        self.briefs_never.insert(slug.clone());
                    }
                }
            }
            Subject::Queue(number) => match run.code {
                Some(0) => {
                    prop_assert_eq!(&run.stdout, &format!("queued {number}\n"));
                    // Queued, or claimed since: a queue run in the window of
                    // a `next` is claimed by its retry.
                    let place = if pushed { 2 } else { who.slot() };
                    let status: Vec<Option<String>> = self
                        .briefs(place)
                        .into_iter()
                        .filter(|brief| brief.number == *number)
                        .map(|brief| brief.status)
                        .collect();
                    prop_assert!(
                        matches!(&status[..], [Some(s)] if reachable("queued", s)),
                        "{:?} after {:?}",
                        status,
                        run
                    );
                    self.depth.briefs_queued += 1;
                }
                Some(1) => {
                    let last = run.stderr.lines().last().unwrap_or_default();
                    prop_assert!(
                        last.starts_with(&format!("board: queue {number}: ")),
                        "{:?}",
                        run
                    );
                }
                Some(_) | None => {}
            },
            Subject::Next => {
                if run.code != Some(0) {
                    return Ok(());
                }
                let remote = self.briefs(2);
                if run.stdout == "queue empty\n" {
                    // In a window the step's command may queue one after.
                    prop_assert!(
                        window
                            || !remote.iter().any(|brief| {
                                !brief.filed && brief.status.as_deref() == Some("queued")
                            }),
                        "queue empty with a queued brief on the remote: {:?}",
                        run
                    );
                    self.depth.queue_empty += 1;
                    return Ok(());
                }
                let dir = format!("{}/proj/briefs/", self.machine(who).data().display());
                let name = run
                    .stdout
                    .strip_prefix(&dir)
                    .and_then(|name| name.strip_suffix('\n'));
                let claimed: Vec<&BriefSeen> = remote
                    .iter()
                    .filter(|brief| Some(brief.name.as_str()) == name && !brief.filed)
                    .collect();
                prop_assert!(
                    claimed.len() == 1
                        && claimed[0]
                            .status
                            .as_deref()
                            .is_some_and(|s| reachable("started", s)),
                    "next printed a brief that is not started on the remote: {:?}",
                    run
                );
                self.depth.briefs_claimed += 1;
            }
            Subject::Block(number, token)
            | Subject::Amend(number, token)
            | Subject::Report(number, token) => {
                self.judge_brief_line(who, subject, run, pushed, (*number, token))?;
            }
        }
        Ok(())
    }

    /// A `block`, `amend` or `report` run: what it printed, and its line in
    /// the brief of the place it wrote to, with a status the command can
    /// have left (or one the brief moved on to since, by the other run of
    /// the step).
    fn judge_brief_line(
        &mut self,
        who: Who,
        subject: &Subject,
        run: &Run,
        pushed: bool,
        (number, token): (u32, &str),
    ) -> Result<(), TestCaseError> {
        let (_, _, word) = subject.brief_line().expect("a brief line");
        match run.code {
            Some(0) => {
                let (printed, after) = match subject {
                    Subject::Block(..) => {
                        self.depth.briefs_blocked += 1;
                        (format!("blocked {number}\n"), "blocked")
                    }
                    Subject::Report(..) => {
                        self.depth.briefs_reported += 1;
                        (format!("reported {number}\n"), "reported")
                    }
                    _ => {
                        if run.stdout == format!("amended {number}, resumed\n") {
                            self.depth.briefs_resumed += 1;
                            (run.stdout.clone(), "started")
                        } else {
                            (format!("amended {number}\n"), "queued")
                        }
                    }
                };
                prop_assert_eq!(&run.stdout, &printed);
                let place = if pushed { 2 } else { who.slot() };
                let line = format!("body {token}");
                let found: Vec<BriefSeen> = self
                    .briefs(place)
                    .into_iter()
                    .filter(|brief| brief.number == number && !brief.filed)
                    .collect();
                prop_assert!(
                    matches!(&found[..], [brief] if brief.text.lines().any(|l| l == line)
                        && brief.status.as_deref().is_some_and(|s| reachable(after, s))),
                    "{} {} is not in brief {}, {}: {:?} after {:?}",
                    word,
                    token,
                    number,
                    after,
                    found,
                    run
                );
                self.brief_lines.push((token.to_owned(), who, number));
            }
            Some(1) => {
                let last = run.stderr.lines().last().unwrap_or_default();
                prop_assert!(
                    last.starts_with(&format!("board: {word} {number}: ")),
                    "{:?}",
                    run
                );
                self.lines_never.insert(token.to_owned());
            }
            // Killed: written or not.
            Some(_) | None => {}
        }
        Ok(())
    }

    /// I1 to I6 in every place, and nothing that never was anywhere.
    fn check_everywhere(&mut self) -> Result<(), TestCaseError> {
        for (who, place) in [(Who::A, 0), (Who::B, 1)] {
            prop_assert_eq!(
                self.machine(who).head_ref(),
                "refs/heads/main",
                "I5: {:?} is off main",
                who
            );
            self.check_place(place)?;
            self.check_briefs(place)?;
        }
        self.check_place(2)?;
        self.check_briefs(2)
    }

    /// I6, and no brief of a `new brief` that failed.
    fn check_briefs(&mut self, place: usize) -> Result<(), TestCaseError> {
        let name = ["A", "B", "the remote"][place];
        let briefs = self.briefs(place);
        let mut numbers = BTreeSet::new();
        for brief in &briefs {
            prop_assert!(
                numbers.insert(brief.number),
                "I6: brief {} is on two files of {}: {:?}",
                brief.number,
                name,
                briefs
            );
            prop_assert!(
                !self
                    .briefs_never
                    .iter()
                    .any(|slug| brief.name.ends_with(&format!("-{slug}.md"))),
                "{} was never made, and is in {}",
                brief.name,
                name
            );
            let refused = brief.text.lines().find(|line| {
                line.strip_prefix("body ")
                    .is_some_and(|token| self.lines_never.contains(token))
            });
            prop_assert!(
                refused.is_none(),
                "{:?} was refused, and is in brief {} of {}",
                refused,
                brief.number,
                name
            );
        }
        if place != 2 {
            return Ok(());
        }
        for brief in &briefs {
            let status = if brief.filed {
                Some("filed".to_owned())
            } else {
                brief.status.clone()
            };
            prop_assert!(
                status.as_deref().is_some_and(|s| reachable("draft", s)),
                "I6: brief {} on the remote has no status: {:?}",
                brief.number,
                brief
            );
            if let Some((was_name, was)) = self.remote_briefs.get(&brief.number) {
                prop_assert_eq!(
                    was_name,
                    &brief.name,
                    "I6: brief {} changed its file on the remote",
                    brief.number
                );
                let (was, now) = (was.as_deref().unwrap(), status.as_deref().unwrap());
                prop_assert!(
                    reachable(was, now),
                    "I6: brief {} went from {} to {} on the remote",
                    brief.number,
                    was,
                    now
                );
            }
            self.remote_briefs
                .insert(brief.number, (brief.name.clone(), status));
        }
        for number in self.remote_briefs.keys() {
            prop_assert!(numbers.contains(number), "brief {} left the remote", number);
        }
        Ok(())
    }

    fn check_place(&mut self, place: usize) -> Result<(), TestCaseError> {
        let name = ["A", "B", "the remote"][place];
        let mut ids = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        for (file, lines) in self.read(place) {
            let mut file_ids = BTreeSet::new();
            let mut file_tokens = BTreeSet::new();
            for line in &lines {
                let token = line.token().to_owned();
                prop_assert!(
                    file_tokens.insert(token.clone()),
                    "I1: {} twice in file {} of {}",
                    token,
                    file,
                    name
                );
                prop_assert!(
                    !self.never.contains(&token),
                    "{} was never added, and is in {}",
                    token,
                    name
                );
                if let Line::Item { id, .. } = line {
                    match id {
                        Some(id) => {
                            prop_assert!(file_ids.insert(*id), "I1: {:?} twice in {}", id, name);
                            prop_assert!(
                                !self.places[place].gone.contains(id),
                                "I3: {:?} came back to {}",
                                id,
                                name
                            );
                            match self.owners.get(id) {
                                Some(owner) => prop_assert_eq!(
                                    owner,
                                    &token,
                                    "I2: {:?} carries two tokens",
                                    id
                                ),
                                None => {
                                    self.owners.insert(*id, token.clone());
                                }
                            }
                        }
                        None => prop_assert!(
                            place != 2,
                            "I5: an item without an id on the remote: {:?}",
                            line
                        ),
                    }
                }
            }
            ids.extend(file_ids);
            tokens.extend(file_tokens);
        }
        let seen = &mut self.places[place];
        for id in seen.ids.difference(&ids) {
            seen.gone.insert(*id);
        }
        for token in seen.tokens.difference(&tokens) {
            prop_assert!(
                self.closed.contains(token) || self.maybe_closed.contains(token),
                "I4: {} left {} without being closed",
                token,
                name
            );
        }
        seen.ids = ids;
        seen.tokens = tokens;
        Ok(())
    }

    /// Faults off, both clones online; `sync` on A, B and A brings both to
    /// the remote's commit.
    fn quiesce(&mut self) -> Result<(), TestCaseError> {
        self.world.reject_pushes(0);
        for who in [Who::A, Who::B] {
            self.machine(who).set_online(true);
            self.online[who.slot()] = true;
        }
        for who in [Who::A, Who::B, Who::A] {
            let machine = self.machine(who);
            machine.unhook("pre-push");
            let output = machine.command().arg("sync").output().expect("run board");
            let run = Run {
                code: output.status.code(),
                stdout: String::from_utf8(output.stdout).unwrap(),
                stderr: String::from_utf8(output.stderr).unwrap(),
            };
            prop_assert_eq!(
                (run.code, run.stdout.as_str()),
                (Some(0), "synced\n"),
                "{:?}",
                run
            );
            self.notes(who, &run)?;
            if std::mem::take(&mut self.healing[who.slot()]) {
                self.depth.kill_after_push_healed += 1;
            }
            self.check_everywhere()?;
        }
        let remote_main = self.world.remote_main();
        for who in [Who::A, Who::B] {
            let machine = self.machine(who);
            prop_assert_eq!(
                machine.main(),
                remote_main.clone(),
                "{:?} is not on the remote's commit",
                who
            );
            // Conflict copies are untracked and stay; nothing else may.
            let changed: Vec<String> = machine
                .status()
                .lines()
                .filter(|line| !line.starts_with("?? ") || !line.contains(".md.local"))
                .map(str::to_owned)
                .collect();
            prop_assert_eq!(changed, Vec::<String>::new(), "{:?} has changed files", who);
        }
        let remote: Vec<String> = self
            .read(2)
            .into_iter()
            .flat_map(|(_, lines)| lines)
            .map(|line| line.token().to_owned())
            .collect();
        for token in &self.added {
            let count = remote.iter().filter(|t| *t == token).count();
            if self.closed.contains(token) {
                prop_assert_eq!(count, 0, "{} was closed and is on the remote", token);
            } else if !self.maybe_closed.contains(token) {
                prop_assert_eq!(count, 1, "{} is to be on the remote once", token);
            }
        }
        // A line written into a brief is in that brief, or in a conflict
        // copy the clone that wrote it saved.
        let remote_briefs = self.briefs(2);
        for (token, who, number) in &self.brief_lines {
            let line = format!("body {token}");
            let has_line = |text: &str| text.lines().any(|l| l == line);
            let in_brief = remote_briefs
                .iter()
                .any(|brief| brief.number == *number && has_line(&brief.text));
            // A copy is `<brief file name>.local[<n>]`, of the clone's file
            // of that number, whatever the remote's is named.
            let machine = self.machine(*who);
            let dir = machine.data().join("proj/briefs");
            let in_copy = fs::read_dir(&dir).into_iter().flatten().any(|entry| {
                let entry = entry.unwrap();
                let file_name = entry.file_name().to_string_lossy().into_owned();
                file_name
                    .split_once(".md.local")
                    .and_then(|(brief, _)| brief_number(&format!("{brief}.md")))
                    == Some(*number)
                    && has_line(&fs::read_to_string(entry.path()).unwrap())
            });
            prop_assert!(
                in_brief || in_copy,
                "{} written by {:?} into brief {} is lost",
                token,
                who,
                number
            );
        }
        Ok(())
    }
}

/// Runs one trace in a fresh world; gives how deep it went.
fn run_trace(steps: &[Step]) -> Result<Depth, TestCaseError> {
    let world = World::new();
    let mut oracle = Oracle::new(&world);
    for step in steps {
        oracle.step(step)?;
    }
    oracle.quiesce()?;
    Ok(oracle.depth)
}

fn config(cases: u32) -> Config {
    // `PROPTEST_CASES` wins, as it does for `proptest!` tests.
    let cases = if std::env::var_os("PROPTEST_CASES").is_some() {
        Config::default().cases
    } else {
        cases
    };
    Config {
        cases,
        max_shrink_iters: 64,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "proptest-regressions/trace.txt",
        ))),
        ..Config::default()
    }
}

fn report(result: Result<(), TestError<Vec<Step>>>) {
    if let Err(error) = result {
        panic!("{error}");
    }
}

#[test]
fn every_trace_keeps_the_invariants() {
    let mut runner = TestRunner::new(config(32));
    report(runner.run(&steps(), |steps| run_trace(&steps).map(drop)));
}

/// A trace that never reaches a mechanism proves nothing about it: a fixed
/// seed, and every mechanism has to come up.
#[test]
fn the_traces_get_deep() {
    let depth = RefCell::new(Depth::default());
    let mut runner = TestRunner::new_with_rng(
        Config {
            failure_persistence: None,
            ..config(24)
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );
    report(runner.run(&steps(), |steps| {
        let reached = run_trace(&steps)?;
        depth.borrow_mut().add(reached);
        Ok(())
    }));
    let depth = depth.into_inner();
    assert!(depth.retried_then_succeeded > 0, "{depth:?}");
    assert!(depth.raced_in_the_window > 0, "{depth:?}");
    assert!(depth.not_pushed > 0, "{depth:?}");
    assert!(depth.refused > 0, "{depth:?}");
    assert!(depth.offline_refused > 0, "{depth:?}");
    assert!(depth.not_pushed_then_pushed > 0, "{depth:?}");
    assert!(depth.merged_by_line > 0, "{depth:?}");
    assert!(depth.assigned > 0, "{depth:?}");
    assert!(depth.window_edit_settled > 0, "{depth:?}");
    assert!(depth.published_line_edited > 0, "{depth:?}");
    assert!(depth.brief_conflicts > 0, "{depth:?}");
    assert!(depth.lost_ack_recognised > 0, "{depth:?}");
    assert!(depth.late_landing_recognised > 0, "{depth:?}");
    assert!(depth.kill_after_push_healed > 0, "{depth:?}");
    assert!(depth.briefs_created > 0, "{depth:?}");
    assert!(depth.briefs_queued > 0, "{depth:?}");
    assert!(depth.briefs_claimed > 0, "{depth:?}");
    assert!(depth.queue_empty > 0, "{depth:?}");
    assert!(depth.briefs_blocked > 0, "{depth:?}");
    assert!(depth.briefs_resumed > 0, "{depth:?}");
    assert!(depth.briefs_reported > 0, "{depth:?}");
    assert!(
        depth.succeeded * 10 >= depth.commands * 6,
        "fewer than 60% of the commands succeeded: {depth:?}"
    );
}

// Traces that once failed, written out: proptest's regression file keeps a
// seed, not the steps, and a seed replays another trace once the strategies
// change shape.

/// The `Index` that picks the first element. proptest gives `Index` no
/// constructor, so one is shrunk all the way, to zero.
fn first() -> Index {
    let mut tree = any::<Index>()
        .new_tree(&mut TestRunner::deterministic())
        .expect("an Index strategy never rejects");
    while tree.simplify() {}
    tree.current()
}

fn run_example(steps: &[Step]) -> Depth {
    match run_trace(steps) {
        Ok(depth) => depth,
        Err(error) => panic!("{error}"),
    }
}

/// A brief made by hand, then edited again while the write that commits it
/// pushes.
#[test]
fn a_brief_saved_during_the_push_that_commits_it() {
    run_example(&[
        Step::HandEdit {
            who: Who::A,
            edit: Edit::Brief(BriefPick::New),
        },
        Step::Cmd {
            who: Who::A,
            op: Op::AddChore { group: false },
            fault: Some(Fault::WindowEdit(Edit::Brief(BriefPick::First))),
        },
    ]);
}

/// Both clones make brief 1 by hand, one edits it again during a push, and
/// the other pushes its own brief 1 and is killed after the push.
#[test]
fn one_brief_number_made_by_hand_on_both_clones() {
    let depth = run_example(&[
        Step::HandEdit {
            who: Who::B,
            edit: Edit::Brief(BriefPick::New),
        },
        Step::HandEdit {
            who: Who::A,
            edit: Edit::Brief(BriefPick::New),
        },
        Step::Cmd {
            who: Who::A,
            op: Op::AddChore { group: false },
            fault: Some(Fault::WindowEdit(Edit::Brief(BriefPick::First))),
        },
        Step::Cmd {
            who: Who::A,
            op: Op::Drop(first()),
            fault: Some(Fault::WindowEdit(Edit::AddIdLess { backlog: true })),
        },
        Step::Cmd {
            who: Who::B,
            op: Op::AddChore { group: false },
            fault: Some(Fault::KillAfterPush),
        },
    ]);
    assert!(depth.kill_after_push_healed > 0, "{depth:?}");
}

/// The other clone claims with `next` while `queue` pushes.
#[test]
fn next_runs_while_queue_pushes() {
    for write in [false, true] {
        let depth = run_example(&[
            Step::HandEdit {
                who: Who::A,
                edit: Edit::Brief(BriefPick::New),
            },
            Step::Cmd {
                who: Who::A,
                op: Op::AddChore { group: false },
                fault: Some(Fault::WindowOp(Op::Done(first()))),
            },
            Step::Cmd {
                who: Who::B,
                op: Op::Queue {
                    pick: first(),
                    write,
                },
                fault: Some(Fault::WindowOp(Op::Next)),
            },
        ]);
        assert!(depth.briefs_queued > 0, "write: {write}, {depth:?}");
    }
}

/// A brief edited during the push of its `queue`, then `next` with every
/// push refused and the last one landed unheard.
#[test]
fn next_after_a_queue_whose_brief_was_saved_during_the_push() {
    let depth = run_example(&[
        Step::HandEdit {
            who: Who::A,
            edit: Edit::Brief(BriefPick::New),
        },
        Step::Cmd {
            who: Who::A,
            op: Op::Queue {
                pick: first(),
                write: false,
            },
            fault: Some(Fault::WindowEdit(Edit::Brief(BriefPick::First))),
        },
        Step::RejectPushes(5),
        Step::Cmd {
            who: Who::A,
            op: Op::Next,
            fault: Some(Fault::LostAck),
        },
    ]);
    assert!(depth.briefs_queued > 0, "{depth:?}");
}
