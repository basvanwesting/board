//! Random interleavings of writes and hand edits on two clones of one
//! remote, with the faults git can make: refused pushes, a remote out of
//! reach, a write of the other clone or a file saved while a push is on its
//! way, a push that landed but was reported failed, one that landed only
//! after the next round's fetch, and a command killed before or after its
//! push.
//!
//! An oracle reads the item files, their conflict copies and the briefs of
//! the three places (clone A's files, clone B's, the remote's `main`) after
//! every step and checks:
//!
//! - I1: one file per id, with that id inside; in a place a token is in
//!   one item's file, and a content token (a body line, a `mark:` value)
//!   in one item;
//! - I2: an id never carries two tokens, in any place or copy, over the
//!   whole trace;
//! - I3: an id that was in a place and then was not never comes back there;
//! - I4: a token that left a place was closed (`done`, `drop`, `file`, a
//!   file deleted by hand): no item is lost but by a delete, and a delete
//!   wins over an edit made on the other side;
//! - I5: no run exits 4, both clones stay on `main`, and nothing the board
//!   does not commit (a misnamed file, a conflict copy) is on the remote;
//! - I6: a brief's status on the remote moves only along the edges of the
//!   state machine and its number keeps its file there; in every place a
//!   number is on one file, open or filed;
//! - I7: a content token is in the file of the item it was written for, or
//!   nowhere;
//! - I8: an item's body in a place or a copy is one a place had before, or
//!   one the step's writes (`add --body`, `amend`, `--replace`, a line
//!   typed or deleted by hand) make of one: one writer's body whole, never
//!   a mix of two clones' writes, no line applied twice;
//! - I9: a content token leaves an item that stays only for a write that
//!   saw it and takes it out (`amend --replace`, the line or the `mark:`
//!   changed by hand), or into a `.local` copy of that item in the clone
//!   whose version lost: an item both sides changed keeps the remote's
//!   version (SPEC.md, Sync);
//! - a conflict note names a copy that is there, of that item, and the item
//!   stays: a delete that won leaves no copy;
//! - every id a command printed is on the remote with its token, and
//!   nothing refused or never pushed is anywhere; a brief `new brief`
//!   printed is on the remote under its number, and the remote has as many
//!   more claimed briefs as `next` printed paths; the line `block`, `amend`
//!   or `report` wrote is in its brief, and one they refused is in none; a
//!   brief `file` printed is in `done/` there, without the items it says it
//!   closed; an item named by a `group` write that no other write met
//!   carries that group where the write was the last change, and a refused
//!   `group` named an id not there; a `group` never needs the remote (no
//!   exit 3);
//! - convergence, at `Quiesce` and at the end: after `sync` on A, B and A,
//!   both clones are on the remote's commit with nothing changed but the
//!   copies and the misnamed files, which stay as they were; every token
//!   added and not closed is there once, and every line written into a
//!   brief, by hand or by a command, is in the brief or in a conflict copy
//!   of the clone that wrote it.
//!
//! Every item's title, every body line and every `mark:` carries a token
//! (`t7`) used once, so a line applied twice, lost, mixed, given two ids or
//! put into another item shows.

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
    /// `add chore|backlog … --body`: an item with a body of two lines.
    AddWithBody {
        backlog: bool,
    },
    /// `amend <c-id|b-id>` of an item of the clone or the remote, picked
    /// when the step runs: these lines after its body, or with `replace`
    /// in its place (none: the body removed).
    Body {
        backlog: bool,
        item: Index,
        replace: bool,
        lines: u8,
    },
    /// A chore of the clone or the remote, picked when the step runs.
    Done(Index),
    /// A backlog item of the clone or the remote, picked when the step runs.
    Drop(Index),
    /// Items of the clone or the remote, chores and backlog items mixed,
    /// picked when the step runs, put into one of [`GROUPS`] by `group`.
    Group {
        items: Vec<(bool, Index)>,
        name: u8,
    },
    /// An item's title or check changed by `retitle`, its token kept: its
    /// text, a chore's check, or both (a backlog item's text only).
    Retitle {
        backlog: bool,
        item: Index,
        text: bool,
        check: bool,
    },
    NewBrief,
    /// A draft of the clone or the remote, picked when the step runs.
    /// `write`: its sections are written by hand first when the clone's file
    /// has none, as the planner does before it queues. `closes`: a backlog
    /// item of the clone or the remote it closes, when there is one.
    Queue {
        pick: Index,
        write: bool,
        closes: Option<Index>,
        /// `--after`: an item of the board of this kind (0 a brief, 1 a chore, 2 a backlog item).
        after: Option<(u8, Index)>,
        first: bool,
    },
    /// `queue <id> --first` on a queued brief of the clone or the remote.
    First(Index),
    /// `park` of a draft of the clone or the remote, its sections written
    /// first, closing a backlog item when there is one.
    Park {
        pick: Index,
        closes: Option<Index>,
    },
    /// `queue <id>` on a parked brief of the clone or the remote.
    Release(Index),
    /// `queue --parked`.
    ReleaseAll,
    /// `after <id> <target>`, or `--none`: an item or a brief of the clone
    /// or the remote, of a kind: 0 a brief, 1 a chore, 2 a backlog item.
    After {
        item: (u8, Index),
        target: Option<(u8, Index)>,
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
    /// A reported brief of the clone or the remote, picked when the step
    /// runs. `trailer`: a commit with its trailer is made in the project
    /// first; else it is filed with `--no-commits`.
    File {
        pick: Index,
        trailer: bool,
    },
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
    /// A line typed at the end of an item's body.
    BodyLine(Index),
    /// A line of an item's body deleted.
    DeleteBodyLine(Index),
    /// An item's `group:` set to another name, or its `mark:`, a key the
    /// tool does not know, set to a fresh token.
    Key { item: Index, group: bool },
    /// An item's file deleted.
    Delete(Index),
    /// A well-named item file with a body line, its id past every id
    /// there is.
    AddFile { backlog: bool },
    /// A file below `chores/` or `backlog/` that is not named as the board
    /// names its files.
    AddMisnamed { backlog: bool },
    /// A line added to a brief's body, after the planner's sections when it
    /// has none yet.
    Brief(BriefPick),
}

#[derive(Debug, Clone)]
enum Fault {
    /// The other clone runs this while the command's first push waits.
    WindowOp(Op),
    /// A file of the clone is saved or deleted while the command's first
    /// push waits.
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
        2 => any::<bool>().prop_map(|backlog| Op::AddWithBody { backlog }),
        2 => (any::<bool>(), any::<Index>(), any::<bool>(), 0u8..3)
            .prop_map(|(backlog, item, replace, lines)| Op::Body { backlog, item, replace, lines }),
        2 => any::<Index>().prop_map(Op::Done),
        2 => any::<Index>().prop_map(Op::Drop),
        3 => (prop::collection::vec((any::<bool>(), any::<Index>()), 1..=3), 0..3u8)
            .prop_map(|(items, name)| Op::Group { items, name }),
        1 => (any::<bool>(), any::<Index>(), any::<bool>(), any::<bool>())
            .prop_map(|(backlog, item, text, check)| Op::Retitle { backlog, item, text, check }),
        2 => Just(Op::NewBrief),
        3 => (any::<Index>(), prop::bool::weighted(0.8), prop::option::of(any::<Index>()))
            .prop_map(|(pick, write, closes)| Op::Queue { pick, write, closes, after: None, first: false }),
        2 => (any::<Index>(), prop::option::of((0..3u8, any::<Index>())), any::<bool>())
            .prop_map(|(pick, after, first)| Op::Queue { pick, write: true, closes: None, after, first }),
        1 => any::<Index>().prop_map(Op::First),
        2 => (any::<Index>(), prop::option::of(any::<Index>()))
            .prop_map(|(pick, closes)| Op::Park { pick, closes }),
        1 => any::<Index>().prop_map(Op::Release),
        1 => Just(Op::ReleaseAll),
        2 => ((0..3u8, any::<Index>()), prop::option::weighted(0.8, (0..3u8, any::<Index>())))
            .prop_map(|(item, target)| Op::After { item, target }),
        2 => Just(Op::Next),
        2 => any::<Index>().prop_map(Op::Block),
        2 => any::<Index>().prop_map(Op::Amend),
        2 => any::<Index>().prop_map(Op::Report),
        2 => (any::<Index>(), prop::bool::weighted(0.8))
            .prop_map(|(pick, trailer)| Op::File { pick, trailer }),
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
        2 => any::<Index>().prop_map(Edit::BodyLine),
        1 => any::<Index>().prop_map(Edit::DeleteBodyLine),
        2 => (any::<Index>(), any::<bool>()).prop_map(|(item, group)| Edit::Key { item, group }),
        2 => any::<Index>().prop_map(Edit::Delete),
        2 => any::<bool>().prop_map(|backlog| Edit::AddFile { backlog }),
        1 => any::<bool>().prop_map(|backlog| Edit::AddMisnamed { backlog }),
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

/// A `group` and a write of the same item, one the step's command and the
/// other the other clone's run in its push's window: picked by one index
/// from lists the two clones mostly share, so mostly the same item. Left
/// to `fault()`, the two seldom meet on one item.
fn met_group() -> impl Strategy<Value = (Op, Op)> {
    (any::<bool>(), any::<Index>(), 0..3u8, any::<bool>(), 0..3u8).prop_map(
        |(backlog, item, name, after, write)| {
            let grouping = Op::Group {
                items: vec![(backlog, item)],
                name,
            };
            let writing = match write {
                0 => Op::Body {
                    backlog,
                    item,
                    replace: after,
                    lines: 1,
                },
                1 => Op::Retitle {
                    backlog,
                    item,
                    text: true,
                    check: after,
                },
                _ => Op::Group {
                    items: vec![(backlog, item)],
                    name: (name + 1) % 3,
                },
            };
            if after {
                (grouping, writing)
            } else {
                (writing, grouping)
            }
        },
    )
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        10 => (who(), op(), fault()).prop_map(|(who, op, fault)| Step::Cmd { who, op, fault }),
        1 => (who(), met_group()).prop_map(|(who, (op, window))| Step::Cmd {
            who,
            op,
            fault: Some(Fault::WindowOp(window)),
        }),
        5 => (who(), edit()).prop_map(|(who, edit)| Step::HandEdit { who, edit }),
        3 => (who(), prop::bool::weighted(0.65)).prop_map(|(who, online)| Step::Net { who, online }),
        2 => who().prop_map(|who| Step::Sync { who }),
        // Up to 11: a write's eight pushes, a lost ack's own push and more,
        // so a trace can still give up while online.
        1 => (0..12u8).prop_map(Step::RejectPushes),
        1 => Just(Step::Quiesce),
    ]
}

/// A hand edit of an item on one clone, the other clone's write or delete
/// of the same item, and the first clone's sync, which merges the two:
/// a copy where both changed the item, a delete that wins where one
/// deleted it. Picked by one index, from lists the clones mostly share.
/// Left to chance, the clones pull too often for their changes to meet.
fn scene() -> impl Strategy<Value = Vec<Step>> {
    (
        who(),
        any::<bool>(),
        any::<Index>(),
        0..3u8,
        0..4u8,
        any::<bool>(),
        0..3u8,
    )
        .prop_map(|(who, backlog, item, edit, write, flag, name)| {
            let edit = match edit {
                0 => Edit::BodyLine(item),
                1 => Edit::Key { item, group: flag },
                _ => Edit::Delete(item),
            };
            let write = match write {
                0 => Op::Body {
                    backlog,
                    item,
                    replace: flag,
                    lines: u8::from(!flag),
                },
                1 => Op::Retitle {
                    backlog,
                    item,
                    text: true,
                    check: flag,
                },
                2 => Op::Group {
                    items: vec![(backlog, item)],
                    name,
                },
                _ if backlog => Op::Drop(item),
                _ => Op::Done(item),
            };
            vec![
                Step::HandEdit { who, edit },
                Step::Cmd {
                    who: who.other(),
                    op: write,
                    fault: None,
                },
                Step::Sync { who },
            ]
        })
}

/// A brief queued to wait on a chore, the queue drained by `next` until it
/// says the rest waits, the chore done, and `next` again; at times put
/// first meanwhile. Left to chance, a wait is seldom what holds the queue,
/// and seldom over before the trace ends.
fn waited() -> impl Strategy<Value = Vec<Step>> {
    (
        who(),
        any::<Index>(),
        any::<Index>(),
        any::<bool>(),
        1..4usize,
    )
        .prop_map(|(who, chore, brief, first, drains)| {
            let cmd = |op| Step::Cmd {
                who,
                op,
                fault: None,
            };
            let mut steps = vec![
                cmd(Op::AddChore { group: true }),
                cmd(Op::NewBrief),
                // A wait set on the draft, then `--after` at the queue.
                cmd(Op::After {
                    item: (0, brief),
                    target: Some((1, chore)),
                }),
                cmd(Op::Queue {
                    pick: brief,
                    write: true,
                    closes: None,
                    after: Some((1, chore)),
                    first,
                }),
            ];
            steps.extend((0..drains).map(|_| cmd(Op::Next)));
            steps.extend([cmd(Op::First(brief)), cmd(Op::Done(chore)), cmd(Op::Next)]);
            steps
        })
}

/// A brief parked to close a backlog item, released with the others by
/// `queue --parked`, claimed, at times blocked and resumed, reported and
/// filed: left to chance, a parked
/// brief is seldom released whole, let alone filed.
fn parked_through() -> impl Strategy<Value = Vec<Step>> {
    (who(), any::<Index>(), any::<bool>(), any::<bool>()).prop_map(|(who, pick, two, blocked)| {
        let cmd = |op| Step::Cmd {
            who,
            op,
            fault: None,
        };
        let mut steps = vec![cmd(Op::AddBacklog), cmd(Op::NewBrief)];
        steps.push(cmd(Op::Park {
            pick,
            closes: Some(pick),
        }));
        if two {
            steps.extend([cmd(Op::NewBrief), cmd(Op::Park { pick, closes: None })]);
        }
        steps.extend([cmd(Op::ReleaseAll), cmd(Op::Next)]);
        // At times the worker stops on a question, and the answer resumes it.
        if blocked {
            steps.extend([cmd(Op::Block(pick)), cmd(Op::Amend(pick))]);
        }
        steps.extend([
            cmd(Op::Report(pick)),
            cmd(Op::File {
                pick,
                trailer: false,
            }),
        ]);
        steps
    })
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            12 => step().prop_map(|step| vec![step]),
            1 => scene(),
            1 => waited(),
            1 => parked_through(),
        ],
        1..=30,
    )
    .prop_map(|scenes| scenes.into_iter().flatten().collect())
}

/// The names `group` puts items under: two the chores are added to, so an
/// item is at times there already, and one no add uses.
const GROUPS: [&str; 3] = ["Home", "Work", "Later"];

/// The item directories, with the prefix of their ids.
const KINDS: [(&str, char); 2] = [("proj/chores", 'c'), ("proj/backlog", 'b')];

/// The directories of a clone the oracle reads.
const DIRS: [&str; 5] = [
    "proj/chores",
    "proj/backlog",
    "proj/briefs",
    "proj/briefs/parked",
    "proj/briefs/done",
];

/// The stamp of a file made by hand: lint wants a `created:`, not a
/// particular one.
const CREATED: &str = "created: 2026-11-25T00:00:00Z";

/// What the planner writes below a brief's header before it queues it.
const SECTIONS: &str = "# By hand\n\n## Task\nt\n## Design\nd\n## Premises\np\n\
                        ## Out of scope\no\n## Verification\nv\n\
                        ## Not doable by the worker\nn\n## Worker protocol\nw\n";

/// The legal moves of a brief's status, as SPEC.md draws them; written out
/// here rather than taken from the tool.
const EDGES: [(&str, &str); 8] = [
    ("draft", "queued"),
    ("draft", "parked"),
    ("parked", "queued"),
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
    /// The file name, without `done/` or `parked/`.
    name: String,
    filed: bool,
    /// In `briefs/parked/`.
    parked: bool,
    /// The value of the first `status:` line of the header.
    status: Option<String>,
    text: String,
}

/// The value of a brief's `after:` header line.
fn after_of(text: &str) -> Option<String> {
    text.lines()
        .take_while(|line| is_key_line(line))
        .find_map(|line| line.strip_prefix("after: "))
        .map(|value| value.trim().to_owned())
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
    let (filed, parked, name) = match (rest.strip_prefix("done/"), rest.strip_prefix("parked/")) {
        (Some(name), _) => (true, false, name),
        (None, Some(name)) => (false, true, name),
        (None, None) => (false, false, rest),
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
        parked,
        status,
        text,
    })
}

/// An item id: `('c', 12)`.
type Id = (char, u32);

/// The id a word is, with `prefix`, as the tool reads it.
fn id_of(prefix: char, word: &str) -> Option<Id> {
    let digits = word.strip_prefix(prefix)?;
    (digits.starts_with(|c: char| ('1'..='9').contains(&c))
        && digits.bytes().all(|b| b.is_ascii_digit()))
    .then(|| digits.parse().ok().map(|n| (prefix, n)))
    .flatten()
}

fn show(id: Id) -> String {
    format!("{}{}", id.0, id.1)
}

fn item_path(id: Id) -> String {
    let (dir, _) = KINDS.iter().find(|(_, p)| *p == id.0).expect("a kind");
    format!("{dir}/{}.md", show(id))
}

/// A header line as the tool reads one: `^[a-z]+:( |$)`.
fn is_key_line(line: &str) -> bool {
    let key = line.bytes().take_while(u8::is_ascii_lowercase).count();
    key > 0 && matches!(&line.as_bytes()[key..], [b':'] | [b':', b' ', ..])
}

/// An item's file as the oracle reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemSeen {
    /// From the file name.
    id: Id,
    /// The value of `id:`.
    header_id: Option<String>,
    /// The title's first word.
    token: String,
    title: String,
    group: Option<String>,
    check: Option<String>,
    /// The value of `mark:`, the key only hand edits write.
    mark: Option<String>,
    /// The tokens of the body's `- <token>` lines, in order.
    body: Vec<String>,
    /// Lines after the title that no write of the trace makes.
    odd: Vec<String>,
    bytes: Vec<u8>,
}

impl ItemSeen {
    /// The tokens written into the item besides its title.
    fn content(&self) -> impl Iterator<Item = &String> {
        self.body.iter().chain(self.mark.iter())
    }
}

fn item_seen(id: Id, bytes: Vec<u8>) -> ItemSeen {
    let text = String::from_utf8(bytes.clone()).expect("the trace writes UTF-8");
    let lines: Vec<&str> = text.lines().collect();
    let header = lines.iter().take_while(|line| is_key_line(line)).count();
    let mut keys: BTreeMap<&str, &str> = BTreeMap::new();
    for line in &lines[..header] {
        let (key, value) = line.split_once(':').expect("a key line");
        keys.entry(key).or_insert(value.trim());
    }
    let after = &lines[header..];
    let title_at = after.iter().position(|line| !line.trim().is_empty());
    let title = title_at
        .and_then(|at| after[at].strip_prefix("# "))
        .unwrap_or_default()
        .to_owned();
    let (mut body, mut odd) = (Vec::new(), Vec::new());
    for line in title_at.map_or(&[][..], |at| &after[at + 1..]) {
        match line.strip_prefix("- ") {
            Some(token) if token.starts_with('t') && !token.contains(' ') => {
                body.push(token.to_owned());
            }
            _ if line.trim().is_empty() => {}
            _ => odd.push((*line).to_owned()),
        }
    }
    let key = |name: &str| keys.get(name).map(|value| (*value).to_owned());
    ItemSeen {
        id,
        header_id: key("id"),
        token: title
            .split(' ')
            .next()
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_owned(),
        title,
        group: key("group"),
        check: key("check"),
        mark: key("mark"),
        body,
        odd,
        bytes,
    }
}

/// A body's lines as stdin gives them to `--body` and `amend`.
fn body_text(tokens: &[String]) -> String {
    tokens.iter().map(|token| format!("- {token}\n")).collect()
}

/// A file's text with a line added at the end of its body.
fn with_body_line(bytes: &[u8], token: &str) -> Vec<u8> {
    let mut text = String::from_utf8(bytes.to_vec()).unwrap();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    // A file that ends at its title gets the blank line that sets a body
    // off.
    if text
        .lines()
        .last()
        .is_some_and(|line| line.starts_with("# "))
    {
        text.push('\n');
    }
    text.push_str(&format!("- {token}\n"));
    text.into_bytes()
}

/// A file's text without the body line of `token`.
fn without_body_line(bytes: &[u8], token: &str) -> Vec<u8> {
    let line = format!("- {token}");
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .filter(|l| *l != line)
        .map(|l| format!("{l}\n"))
        .collect::<String>()
        .into_bytes()
}

/// A file's text with `key:` set to `value`, in its place, or at the end
/// of the header.
fn with_key(bytes: &[u8], key: &str, value: &str) -> Vec<u8> {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let header = lines.iter().take_while(|line| is_key_line(line)).count();
    let new = format!("{key}: {value}");
    match lines[..header]
        .iter()
        .position(|line| line.starts_with(&format!("{key}:")))
    {
        Some(at) => lines[at] = new,
        None => lines.insert(header, new),
    }
    lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .into_bytes()
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
    /// The item's token, and its body's.
    Add(String, Vec<String>),
    /// `amend` of an item: its id, the body's new lines, `--replace`, and
    /// the write as the oracle recorded it, when it knew the item.
    Body(Id, Vec<String>, bool, Option<usize>),
    Close(Id),
    /// The items, each once, in the order given, and the group's name.
    Group(Vec<Id>, String),
    /// The item, and what it holds once retitled: the start of its title,
    /// and a chore's check when one was given.
    Retitle(Id, String, Option<String>),
    /// The slug, a token.
    NewBrief(String),
    /// `queue <n>`, with `--first` or not.
    Queue(u32, bool),
    /// `queue <n> --first`.
    First(u32),
    /// `park <n>`.
    Park(u32),
    /// `queue <n>` of a parked brief.
    Release(u32),
    /// `queue --parked`.
    QueueParked,
    /// `after <id> …`: the id.
    After(String),
    Next,
    /// The brief, and the token of the line the command writes into it.
    Block(u32, String),
    Amend(u32, String),
    Report(u32, String),
    /// The brief, and the backlog ids its `closes:` named when the step
    /// began.
    File(u32, Vec<Id>),
}

impl Subject {
    /// The brief a command writes a line into, the line's token and the
    /// command's word.
    fn brief_line(&self) -> Option<(u32, &str, &'static str)> {
        match self {
            Subject::Block(number, token) => Some((*number, token, "block")),
            Subject::Amend(number, token) => Some((*number, token, "amend")),
            Subject::Report(number, token) => Some((*number, token, "report")),
            Subject::Add(..)
            | Subject::Body(..)
            | Subject::Close(_)
            | Subject::Group(..)
            | Subject::Retitle(..)
            | Subject::NewBrief(_)
            | Subject::Queue(..)
            | Subject::First(_)
            | Subject::Park(_)
            | Subject::Release(_)
            | Subject::QueueParked
            | Subject::After(_)
            | Subject::Next
            | Subject::File(..) => None,
        }
    }

    /// The items whose files the command changes.
    fn items(&self) -> Vec<Id> {
        match self {
            Subject::Body(id, ..) | Subject::Retitle(id, ..) => vec![*id],
            Subject::Group(ids, _) => ids.clone(),
            Subject::Add(..)
            | Subject::Close(_)
            | Subject::NewBrief(_)
            | Subject::Queue(..)
            | Subject::First(_)
            | Subject::Park(_)
            | Subject::Release(_)
            | Subject::QueueParked
            | Subject::After(_)
            | Subject::Next
            | Subject::Block(..)
            | Subject::Amend(..)
            | Subject::Report(..)
            | Subject::File(..) => Vec::new(),
        }
    }

    /// The items the command deletes.
    fn closes(&self) -> Vec<Id> {
        match self {
            Subject::Close(id) => vec![*id],
            Subject::File(_, closes) => closes.clone(),
            Subject::Add(..)
            | Subject::Body(..)
            | Subject::Group(..)
            | Subject::Retitle(..)
            | Subject::NewBrief(_)
            | Subject::Queue(..)
            | Subject::First(_)
            | Subject::Park(_)
            | Subject::Release(_)
            | Subject::QueueParked
            | Subject::After(_)
            | Subject::Next
            | Subject::Block(..)
            | Subject::Amend(..)
            | Subject::Report(..) => Vec::new(),
        }
    }

    /// What the command reads from stdin: the text of `amend` and `report`.
    fn input(&self) -> Option<String> {
        match self {
            Subject::Amend(_, token) | Subject::Report(_, token) => Some(format!("body {token}\n")),
            Subject::Add(_, body) if !body.is_empty() => Some(body_text(body)),
            Subject::Body(_, lines, ..) => Some(body_text(lines)),
            Subject::Block(..)
            | Subject::Add(..)
            | Subject::Close(_)
            | Subject::Group(..)
            | Subject::Retitle(..)
            | Subject::NewBrief(_)
            | Subject::Queue(..)
            | Subject::First(_)
            | Subject::Park(_)
            | Subject::Release(_)
            | Subject::QueueParked
            | Subject::After(_)
            | Subject::Next
            | Subject::File(..) => None,
        }
    }
}

/// A hand edit's change to a clone's files.
#[derive(Debug, Clone)]
enum Change {
    Write(String, Vec<u8>),
    Remove(String),
}

/// Which kind of hand edit, for the depth check.
#[derive(Debug, Clone, Copy)]
enum Hand {
    Body,
    Key,
    Delete,
    Add,
    Misnamed,
    Brief,
}

/// A write of an item's content as a hand edit makes it: the item's token,
/// the content tokens it writes, those it takes out having seen them, and
/// its change of the body.
type HandWrite = (String, Vec<String>, BTreeSet<String>, Option<BodyChange>);

/// What a hand edit does, for the oracle to record once it is made.
#[derive(Debug)]
struct Effect {
    hand: Hand,
    /// The token of an item file added.
    added: Option<String>,
    /// An item file deleted, and its token.
    deleted: Option<(Id, String)>,
    writes: Vec<HandWrite>,
    /// The item whose file changed or was added.
    changed: Option<Id>,
    /// A token written into a brief, and the brief's number.
    brief: Option<(String, u32)>,
    /// A misnamed file made: its path and bytes.
    misnamed: Option<(String, Vec<u8>)>,
}

impl Effect {
    fn new(hand: Hand) -> Effect {
        Effect {
            hand,
            added: None,
            deleted: None,
            writes: Vec::new(),
            changed: None,
            brief: None,
            misnamed: None,
        }
    }
}

/// Whether a write tried all eight rounds and none was pushed: the last
/// retry noted, and the write's commit left on the clone (`not pushed`).
fn given_up_online(run: &Run) -> bool {
    run.stderr.contains("board: retry 8/8") && run.stderr.contains("not pushed")
}

/// Counts of what the trace went through, for the depth check.
#[derive(Debug, Default, Clone, Copy)]
struct Depth {
    commands: u32,
    succeeded: u32,
    retried_then_succeeded: u32,
    raced_in_the_window: u32,
    not_pushed: u32,
    /// Every round's push refused with the remote there: given up online.
    given_up_online: u32,
    refused: u32,
    offline_refused: u32,
    not_pushed_then_pushed: u32,
    /// A write that merged the clone's own commits with the remote's.
    merged: u32,
    window_edit_settled: u32,
    window_delete_of_written: u32,
    brief_conflicts: u32,
    lost_ack_recognised: u32,
    late_landing_recognised: u32,
    kill_after_push_healed: u32,
    briefs_created: u32,
    briefs_queued: u32,
    briefs_claimed: u32,
    /// A claimed brief with an `after:`, its wait over.
    claimed_after_wait: u32,
    /// `next` with only waiting briefs queued.
    queue_waiting: u32,
    briefs_put_first: u32,
    briefs_parked: u32,
    briefs_released: u32,
    /// A `queue --parked` that released briefs, judged.
    all_released: u32,
    /// `after` runs that set or removed a wait.
    waits_set: u32,
    queue_empty: u32,
    briefs_blocked: u32,
    briefs_resumed: u32,
    briefs_reported: u32,
    briefs_filed: u32,
    closed_by_file: u32,
    /// Items a `group` run was judged to have put into its group.
    items_grouped: u32,
    groups_not_pushed: u32,
    /// A `group` and a write of the same item by the other clone, neither
    /// knowing of the other: in one push's window, or met at a merge.
    group_met: u32,
    /// A `group` judged that put a chore and a backlog item into one group.
    grouped_both_kinds: u32,
    /// A `group` whose items were all in the group: nothing to commit.
    grouped_nothing: u32,
    items_retitled: u32,
    /// `add … --body`, `amend`, `amend --replace` with lines and without,
    /// that succeeded.
    bodies_added: u32,
    bodies_appended: u32,
    bodies_replaced: u32,
    bodies_removed: u32,
    /// Conflict notes of an item, each with its `.local` copy.
    local_copies: u32,
    /// An item deleted on one side and changed on the other, gone.
    delete_won: u32,
    /// Content tokens gone for a write that saw them (I9).
    content_dropped: u32,
    /// Content tokens gone into a `.local` copy (I9).
    content_kept_in_copy: u32,
    body_lines_by_hand: u32,
    keys_by_hand: u32,
    files_deleted_by_hand: u32,
    files_added_by_hand: u32,
    misnamed_by_hand: u32,
}

impl Depth {
    fn add(&mut self, other: Depth) {
        self.commands += other.commands;
        self.succeeded += other.succeeded;
        self.retried_then_succeeded += other.retried_then_succeeded;
        self.raced_in_the_window += other.raced_in_the_window;
        self.not_pushed += other.not_pushed;
        self.given_up_online += other.given_up_online;
        self.refused += other.refused;
        self.offline_refused += other.offline_refused;
        self.not_pushed_then_pushed += other.not_pushed_then_pushed;
        self.merged += other.merged;
        self.window_edit_settled += other.window_edit_settled;
        self.window_delete_of_written += other.window_delete_of_written;
        self.brief_conflicts += other.brief_conflicts;
        self.lost_ack_recognised += other.lost_ack_recognised;
        self.late_landing_recognised += other.late_landing_recognised;
        self.kill_after_push_healed += other.kill_after_push_healed;
        self.briefs_created += other.briefs_created;
        self.briefs_queued += other.briefs_queued;
        self.briefs_claimed += other.briefs_claimed;
        self.claimed_after_wait += other.claimed_after_wait;
        self.queue_waiting += other.queue_waiting;
        self.briefs_put_first += other.briefs_put_first;
        self.briefs_parked += other.briefs_parked;
        self.briefs_released += other.briefs_released;
        self.all_released += other.all_released;
        self.waits_set += other.waits_set;
        self.queue_empty += other.queue_empty;
        self.briefs_blocked += other.briefs_blocked;
        self.briefs_resumed += other.briefs_resumed;
        self.briefs_reported += other.briefs_reported;
        self.briefs_filed += other.briefs_filed;
        self.closed_by_file += other.closed_by_file;
        self.items_grouped += other.items_grouped;
        self.groups_not_pushed += other.groups_not_pushed;
        self.group_met += other.group_met;
        self.grouped_both_kinds += other.grouped_both_kinds;
        self.grouped_nothing += other.grouped_nothing;
        self.items_retitled += other.items_retitled;
        self.bodies_added += other.bodies_added;
        self.bodies_appended += other.bodies_appended;
        self.bodies_replaced += other.bodies_replaced;
        self.bodies_removed += other.bodies_removed;
        self.local_copies += other.local_copies;
        self.delete_won += other.delete_won;
        self.content_dropped += other.content_dropped;
        self.content_kept_in_copy += other.content_kept_in_copy;
        self.body_lines_by_hand += other.body_lines_by_hand;
        self.keys_by_hand += other.keys_by_hand;
        self.files_deleted_by_hand += other.files_deleted_by_hand;
        self.files_added_by_hand += other.files_added_by_hand;
        self.misnamed_by_hand += other.misnamed_by_hand;
    }
}

/// What the oracle last saw in one place.
#[derive(Debug, Default)]
struct Place {
    ids: BTreeSet<Id>,
    /// Ids that were here and then were not (I3).
    gone: BTreeSet<Id>,
    /// Item tokens (I4).
    tokens: BTreeSet<String>,
    /// Content tokens (I9).
    content: BTreeSet<String>,
    /// Each item's file.
    files: BTreeMap<Id, Vec<u8>>,
}

/// A write of an item's content, by a command or by hand, as I9 needs it.
#[derive(Debug, Clone)]
struct ContentWrite {
    /// In the order the trace made them.
    id: usize,
    /// The item's token.
    item: String,
    /// The content tokens it may take out, having seen them: every body
    /// line a replace saw, the one line or `mark:` a hand edit changes.
    drops: BTreeSet<String>,
}

/// A write of an item's body, as the oracle expects it to change the body
/// it is applied to (I8).
#[derive(Debug, Clone)]
enum BodyChange {
    /// These lines after the body: `add --body`, `amend`, a line typed by
    /// hand.
    Append(Vec<String>),
    /// These lines in place of the body: `amend --replace` (none: removed),
    /// a file added by hand.
    Replace(Vec<String>),
    /// This line deleted by hand.
    Remove(String),
}

impl BodyChange {
    fn apply(&self, body: &[String]) -> Vec<String> {
        match self {
            BodyChange::Append(lines) => [body, lines].concat(),
            BodyChange::Replace(lines) => lines.clone(),
            BodyChange::Remove(token) => body.iter().filter(|t| *t != token).cloned().collect(),
        }
    }
}

/// What the oracle reads of one place.
#[derive(Debug, Default)]
struct Snapshot {
    items: BTreeMap<Id, ItemSeen>,
    /// Conflict copies: the path, and the copy as read, of its item's id.
    copies: Vec<(String, ItemSeen)>,
    /// Files below `chores/` and `backlog/` that are neither.
    other: Vec<String>,
    briefs: Vec<BriefSeen>,
}

impl Snapshot {
    fn read(files: Vec<(String, Vec<u8>)>) -> Snapshot {
        let mut snapshot = Snapshot::default();
        'files: for (path, bytes) in files {
            for (dir, prefix) in KINDS {
                let Some(name) = path.strip_prefix(&format!("{dir}/")) else {
                    continue;
                };
                if let Some(id) = name.strip_suffix(".md").and_then(|n| id_of(prefix, n)) {
                    snapshot.items.insert(id, item_seen(id, bytes));
                } else if let Some((stem, suffix)) = name.split_once(".md.local")
                    && let Some(id) = id_of(prefix, stem)
                    && suffix.bytes().all(|b| b.is_ascii_digit())
                {
                    snapshot.copies.push((path.clone(), item_seen(id, bytes)));
                } else {
                    snapshot.other.push(path.clone());
                }
                continue 'files;
            }
            let text = String::from_utf8(bytes).expect("the trace writes UTF-8");
            snapshot.briefs.extend(brief_seen(&path, text));
        }
        snapshot
            .briefs
            .sort_by_key(|brief| (brief.number, brief.filed, brief.name.clone()));
        snapshot
    }

    /// An item by its token.
    fn by_token(&self, token: &str) -> Option<&ItemSeen> {
        self.items.values().find(|item| item.token == token)
    }
}

struct Oracle<'w> {
    world: &'w World,
    tokens: u32,
    /// A, B and the remote.
    places: [Place; 3],
    /// Every id ever seen, with its token (I2).
    owners: BTreeMap<Id, String>,
    /// Tokens of items added by a command that succeeded or by hand.
    added: BTreeSet<String>,
    /// Tokens of adds that were refused, not pushed or killed before their
    /// push, and of content a refused command wrote: never anywhere.
    never: BTreeSet<String>,
    /// Tokens of items deleted, by `done`, `drop`, `file` or by hand.
    closed: BTreeSet<String>,
    /// Tokens of a `done`, `drop` or `file` that was killed: closed or not.
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
    /// The highest id given by hand so far, per kind.
    typed: BTreeMap<char, u32>,
    online: [bool; 2],
    /// A clone with commits a write said it did not push.
    unpushed: [bool; 2],
    /// A clone whose command was killed after its push landed: the next
    /// command there has to work.
    healing: [bool; 2],
    /// Per content token, the token of the item it was written for (I7).
    content_owner: BTreeMap<String, String>,
    /// Per content token, the write that made it (I9).
    content_made: BTreeMap<String, usize>,
    writes: Vec<ContentWrite>,
    /// Per item token, every body seen in any place or copy so far (I8).
    bodies_seen: BTreeMap<String, BTreeSet<Vec<String>>>,
    /// The writes of bodies in this step.
    body_changes: Vec<(String, BodyChange)>,
    /// The clone whose command or sync runs: a delete of the remote's item
    /// in its step is its push.
    actor: Option<Who>,
    /// Per clone, the items it changed that its pushes have not carried
    /// yet, and the same at the start of the step: a merge in the step
    /// deletes them only for a delete of the other side.
    pending: [BTreeSet<Id>; 2],
    pending_at_step: [BTreeSet<Id>; 2],
    /// Per clone, the items it deleted and has not pushed, with the
    /// remote's file as it was then.
    deletes_pending: [BTreeMap<Id, Vec<u8>>; 2],
    /// Per clone, the items it regrouped and has not pushed.
    groups_pending: [BTreeSet<Id>; 2],
    /// Per clone, the items the other clone regrouped and pushed since this
    /// one last pulled.
    groups_unseen: [BTreeSet<Id>; 2],
    /// Per clone, the item tokens it closed in this step.
    closed_in_step: [BTreeSet<String>; 2],
    /// Misnamed files made by hand: the clone, the path, the bytes.
    misnamed: Vec<(Who, String, Vec<u8>)>,
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
            content_owner: BTreeMap::new(),
            content_made: BTreeMap::new(),
            writes: Vec::new(),
            bodies_seen: BTreeMap::new(),
            body_changes: Vec::new(),
            actor: None,
            pending: Default::default(),
            pending_at_step: Default::default(),
            deletes_pending: Default::default(),
            groups_pending: Default::default(),
            groups_unseen: Default::default(),
            closed_in_step: Default::default(),
            misnamed: Vec::new(),
            depth: Depth::default(),
        }
    }

    /// A write of `item`'s content, as it will be tried: refused or not, it
    /// may be the change the places show next.
    fn content_write(
        &mut self,
        item: &str,
        tokens: &[String],
        drops: BTreeSet<String>,
        change: Option<BodyChange>,
    ) -> usize {
        let id = self.writes.len();
        for token in tokens {
            self.content_owner.insert(token.clone(), item.to_owned());
            self.content_made.insert(token.clone(), id);
        }
        self.writes.push(ContentWrite {
            id,
            item: item.to_owned(),
            drops,
        });
        if let Some(change) = change {
            self.body_changes.push((item.to_owned(), change));
        }
        id
    }

    /// The body lines of an item a command of `who` sees: the clone's, and
    /// online the remote's, which it pulls. More than it may see (a merge
    /// keeps one of the two), which only lets I9 excuse more.
    fn seen_body(&self, who: Who, id: Id) -> BTreeSet<String> {
        let mut places = vec![who.slot()];
        if self.online[who.slot()] {
            places.push(2);
        }
        places
            .into_iter()
            .flat_map(|place| {
                self.read(place)
                    .items
                    .get(&id)
                    .map(|item| item.body.clone())
                    .unwrap_or_default()
            })
            .collect()
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

    /// The files of a place: 0 and 1 are the clones' files, 2 the remote's
    /// `main`.
    fn read(&self, place: usize) -> Snapshot {
        let files = match place {
            0 | 1 => {
                let machine = if place == 0 {
                    self.world.a()
                } else {
                    self.world.b()
                };
                DIRS.into_iter()
                    .flat_map(|dir| {
                        fs::read_dir(machine.data().join(dir))
                            .into_iter()
                            .flatten()
                            .map(|entry| entry.unwrap())
                            .filter(|entry| entry.file_type().unwrap().is_file())
                            .map(move |entry| {
                                let path = format!("{dir}/{}", entry.file_name().to_string_lossy());
                                (path, fs::read(entry.path()).unwrap())
                            })
                    })
                    .collect()
            }
            _ => self.world.remote_blobs("proj"),
        };
        Snapshot::read(files)
    }

    fn ids_in(&self, place: usize, prefix: char) -> Vec<u32> {
        self.read(place)
            .items
            .keys()
            .filter(|(p, _)| *p == prefix)
            .map(|(_, n)| *n)
            .collect()
    }

    /// The brief files of a place: 0 and 1 are the clones' files, 2 the
    /// remote's `main`.
    fn briefs(&self, place: usize) -> Vec<BriefSeen> {
        self.read(place).briefs
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
                    Subject::Add(token, Vec::new()),
                )
            }
            Op::AddBacklog => {
                let token = self.token();
                (
                    owned(&["add", "backlog", &token]),
                    Subject::Add(token, Vec::new()),
                )
            }
            Op::AddWithBody { backlog } => {
                let token = self.token();
                let body = vec![self.token(), self.token()];
                self.content_write(
                    &token,
                    &body,
                    BTreeSet::new(),
                    Some(BodyChange::Append(body.clone())),
                );
                let mut args = if *backlog {
                    owned(&["add", "backlog", &token])
                } else {
                    owned(&["add", "chore", &token, "--group", "Home", "--check", "x"])
                };
                args.push("--body".to_owned());
                (args, Subject::Add(token, body))
            }
            Op::Body {
                backlog,
                item,
                replace,
                lines,
            } => {
                let prefix = if *backlog { 'b' } else { 'c' };
                let id = pick(self, prefix, item);
                // An append of no line is a usage error: one at least.
                let count = if *replace { *lines } else { (*lines).max(1) };
                let lines: Vec<String> = (0..count).map(|_| self.token()).collect();
                let write = match self.owners.get(&id).cloned() {
                    Some(item) => {
                        let (change, drops) = if *replace {
                            (BodyChange::Replace(lines.clone()), self.seen_body(who, id))
                        } else {
                            (BodyChange::Append(lines.clone()), BTreeSet::new())
                        };
                        Some(self.content_write(&item, &lines, drops, Some(change)))
                    }
                    None => {
                        for token in &lines {
                            self.content_owner
                                .insert(token.clone(), "no item".to_owned());
                        }
                        None
                    }
                };
                let mut args = owned(&["amend", &show(id)]);
                if *replace {
                    args.push("--replace".to_owned());
                }
                (args, Subject::Body(id, lines, *replace, write))
            }
            Op::Done(index) => {
                let id = pick(self, 'c', index);
                (owned(&["done", &show(id)]), Subject::Close(id))
            }
            Op::Drop(index) => {
                let id = pick(self, 'b', index);
                (owned(&["drop", &show(id)]), Subject::Close(id))
            }
            Op::Group { items, name } => {
                let mut ids: Vec<Id> = Vec::new();
                for (backlog, index) in items {
                    let id = pick(self, if *backlog { 'b' } else { 'c' }, index);
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                let name = GROUPS[usize::from(*name)].to_owned();
                let mut args = owned(&["group"]);
                args.extend(ids.iter().map(|id| show(*id)));
                args.extend(["--to".to_owned(), name.clone()]);
                (args, Subject::Group(ids, name))
            }
            Op::Retitle {
                backlog,
                item,
                text,
                check,
            } => {
                let prefix = if *backlog { 'b' } else { 'c' };
                let id = pick(self, prefix, item);
                // The token stays first, so the item is the same one to
                // every invariant; a fresh mark after it is the change.
                let token = self.token_of(who, id).unwrap_or_else(|| self.token());
                let mark = self.token();
                let mut args = owned(&["retitle", &show(id)]);
                let check = (*check && !*backlog).then(|| format!("x {mark}"));
                let text = (*text || check.is_none()).then(|| format!("{token} {mark}"));
                args.extend(text.clone());
                if let Some(check) = &check {
                    args.extend(["--check".to_owned(), check.clone()]);
                }
                let start = text.unwrap_or(token);
                (args, Subject::Retitle(id, start, check))
            }
            Op::NewBrief => {
                let slug = self.token();
                (owned(&["new", "brief", &slug]), Subject::NewBrief(slug))
            }
            Op::Queue {
                pick: draft,
                closes,
                after,
                first,
                ..
            } => {
                // Nothing to pick: a brief that is not there, for a refusal.
                let drafts = self.drafts(who);
                let number = if drafts.is_empty() {
                    999_999
                } else {
                    *draft.get(&drafts)
                };
                let mut args = owned(&["queue", &number.to_string()]);
                // An item the clone or the remote has; none when neither
                // has one.
                if let Some(index) = closes {
                    let (_, item) = pick(self, 'b', index);
                    if item != 999_999 {
                        args.extend(["--closes".to_owned(), format!("b{item}")]);
                    }
                }
                if let Some(target) = after {
                    args.extend(["--after".to_owned(), self.target(who, *target)]);
                }
                if *first {
                    args.push("--first".to_owned());
                }
                (args, Subject::Queue(number, *first))
            }
            Op::Park {
                pick: draft,
                closes,
            } => {
                let drafts = self.drafts(who);
                let number = if drafts.is_empty() {
                    999_999
                } else {
                    *draft.get(&drafts)
                };
                let mut args = owned(&["park", &number.to_string()]);
                if let Some(index) = closes {
                    let (_, item) = pick(self, 'b', index);
                    if item != 999_999 {
                        args.extend(["--closes".to_owned(), format!("b{item}")]);
                    }
                }
                (args, Subject::Park(number))
            }
            Op::Release(index) => {
                let numbers = self.open_with(who, &["parked"]);
                let number = if numbers.is_empty() {
                    999_999
                } else {
                    *index.get(&numbers)
                };
                (
                    owned(&["queue", &number.to_string()]),
                    Subject::Release(number),
                )
            }
            Op::ReleaseAll => (owned(&["queue", "--parked"]), Subject::QueueParked),
            Op::First(index) => {
                let numbers = self.open_with(who, &["queued"]);
                let number = if numbers.is_empty() {
                    999_999
                } else {
                    *index.get(&numbers)
                };
                (
                    owned(&["queue", &number.to_string(), "--first"]),
                    Subject::First(number),
                )
            }
            Op::After { item, target } => {
                let id = self.target(who, *item);
                let mut args = owned(&["after", &id]);
                match target {
                    Some(target) => args.push(self.target(who, *target)),
                    None => args.push("--none".to_owned()),
                }
                (args, Subject::After(id))
            }
            Op::File { pick, trailer } => {
                let numbers = self.open_with(who, &["reported"]);
                // Nothing to pick: a brief that is not there, for a refusal.
                let number = if numbers.is_empty() {
                    999_999
                } else {
                    *pick.get(&numbers)
                };
                let mut args = owned(&["file", &number.to_string()]);
                if *trailer {
                    // The worker's commit of the brief.
                    let machine = self.machine(who);
                    machine.git(
                        &machine.proj(),
                        [
                            "commit",
                            "-q",
                            "--allow-empty",
                            "-m",
                            &format!("work\n\nBrief: {number}"),
                        ],
                    );
                } else {
                    args.push("--no-commits".to_owned());
                }
                (args, Subject::File(number, self.closes_of(who, number)))
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

    /// The backlog ids the `closes:` line of a brief names, in the clone's
    /// file and the remote's.
    fn closes_of(&self, who: Who, number: u32) -> Vec<Id> {
        let ids: BTreeSet<Id> = [who.slot(), 2]
            .into_iter()
            .flat_map(|place| self.briefs(place))
            .filter(|brief| brief.number == number)
            .flat_map(|brief| {
                let closes = brief
                    .text
                    .lines()
                    .take_while(|line| !line.is_empty())
                    .find_map(|line| line.strip_prefix("closes: "))
                    .unwrap_or_default()
                    .to_owned();
                closes
                    .split(", ")
                    .filter_map(|word| id_of('b', word))
                    .collect::<Vec<Id>>()
            })
            .collect();
        ids.into_iter().collect()
    }

    /// The numbers of the open briefs of the clone and the remote that have
    /// one of these statuses.
    /// An id of the clone or the remote of a kind, picked by
    /// `index`: a brief (0), a chore (1), a backlog item (2). Nothing to
    /// pick: an id no item has, for a refusal.
    fn target(&self, who: Who, (kind, index): (u8, Index)) -> String {
        match kind {
            0 => {
                let numbers: BTreeSet<u32> = [who.slot(), 2]
                    .into_iter()
                    .flat_map(|place| self.briefs(place))
                    .map(|brief| brief.number)
                    .collect();
                let numbers: Vec<u32> = numbers.into_iter().collect();
                if numbers.is_empty() {
                    "999999".to_owned()
                } else {
                    index.get(&numbers).to_string()
                }
            }
            _ => {
                let prefix = if kind == 1 { 'c' } else { 'b' };
                let ids: BTreeSet<u32> = self
                    .ids_in(who.slot(), prefix)
                    .into_iter()
                    .chain(self.ids_in(2, prefix))
                    .collect();
                let ids: Vec<u32> = ids.into_iter().collect();
                if ids.is_empty() {
                    format!("{prefix}999999")
                } else {
                    format!("{prefix}{}", index.get(&ids))
                }
            }
        }
    }

    /// What a brief of `place` waits on, as the oracle reads the files
    /// there, written out here rather than taken from the tool: its
    /// `after:` while that names a brief that is not reported or filed, or
    /// an item whose file is there.
    fn waits_on(&self, place: usize, brief: &BriefSeen) -> Option<String> {
        let target = after_of(&brief.text)?;
        let waiting = match target.chars().next() {
            Some(prefix @ ('c' | 'b')) => {
                id_of(prefix, &target).is_some_and(|id| self.read(place).items.contains_key(&id))
            }
            _ => target.parse::<u32>().is_ok_and(|number| {
                self.briefs(place).iter().any(|other| {
                    other.number == number
                        && !other.filed
                        && !matches!(other.status.as_deref(), Some("reported" | "filed"))
                })
            }),
        };
        waiting.then_some(target)
    }

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

    /// A hand edit of `who`'s files: the change, and what the edit does.
    /// `None` when there is nothing to edit. `window`: the edit is made
    /// while the clone's own write runs, which writes these items.
    fn edit(&mut self, who: Who, edit: &Edit, window: Option<&[Id]>) -> Option<(Change, Effect)> {
        let snapshot = self.read(who.slot());
        let items: Vec<&ItemSeen> = snapshot.items.values().collect();
        match edit {
            Edit::BodyLine(index) => {
                if items.is_empty() {
                    return None;
                }
                let item = *index.get(&items);
                let token = self.token();
                let mut effect = Effect::new(Hand::Body);
                effect.writes.push((
                    item.token.clone(),
                    vec![token.clone()],
                    BTreeSet::new(),
                    Some(BodyChange::Append(vec![token.clone()])),
                ));
                effect.changed = Some(item.id);
                let bytes = with_body_line(&item.bytes, &token);
                Some((Change::Write(item_path(item.id), bytes), effect))
            }
            Edit::DeleteBodyLine(index) => {
                let lines: Vec<(&ItemSeen, &String)> = items
                    .iter()
                    .flat_map(|item| item.body.iter().map(move |token| (*item, token)))
                    .collect();
                if lines.is_empty() {
                    return None;
                }
                let (item, token) = *index.get(&lines);
                let mut effect = Effect::new(Hand::Body);
                effect.writes.push((
                    item.token.clone(),
                    Vec::new(),
                    BTreeSet::from([token.clone()]),
                    Some(BodyChange::Remove(token.clone())),
                ));
                effect.changed = Some(item.id);
                let bytes = without_body_line(&item.bytes, token);
                Some((Change::Write(item_path(item.id), bytes), effect))
            }
            Edit::Key { item, group } => {
                if items.is_empty() {
                    return None;
                }
                let item = *item.get(&items);
                let token = self.token();
                let mut effect = Effect::new(Hand::Key);
                effect.changed = Some(item.id);
                let bytes = if *group {
                    // A group no command names: the item leaves the ones
                    // `group` puts it in.
                    with_key(&item.bytes, "group", "Hand")
                } else {
                    effect.writes.push((
                        item.token.clone(),
                        vec![token.clone()],
                        item.mark.iter().cloned().collect(),
                        None,
                    ));
                    with_key(&item.bytes, "mark", &token)
                };
                Some((Change::Write(item_path(item.id), bytes), effect))
            }
            Edit::Delete(index) => {
                if items.is_empty() {
                    return None;
                }
                let item = *index.get(&items);
                let mut effect = Effect::new(Hand::Delete);
                effect.deleted = Some((item.id, item.token.clone()));
                Some((Change::Remove(item_path(item.id)), effect))
            }
            Edit::AddFile { backlog } => {
                let (dir, prefix) = KINDS[usize::from(*backlog)];
                // Past every id seen or given by hand, by more than a trace
                // can allocate meanwhile: a collision is the example test's,
                // not the trace's.
                let seen = self
                    .owners
                    .keys()
                    .filter(|(p, _)| *p == prefix)
                    .map(|(_, n)| *n);
                let typed = self.typed.get(&prefix).copied();
                let n = seen.chain(typed).max().unwrap_or(0) + 100;
                self.typed.insert(prefix, n);
                let (token, line) = (self.token(), self.token());
                let check = if *backlog { "" } else { "check: x\n" };
                let bytes = format!(
                    "id: {prefix}{n}\ngroup: Home\n{check}{CREATED}\n\n# {token}\n\n- {line}\n"
                );
                let mut effect = Effect::new(Hand::Add);
                effect.added = Some(token.clone());
                effect.changed = Some((prefix, n));
                effect.writes.push((
                    token,
                    vec![line.clone()],
                    BTreeSet::new(),
                    Some(BodyChange::Replace(vec![line])),
                ));
                Some((
                    Change::Write(format!("{dir}/{prefix}{n}.md"), bytes.into_bytes()),
                    effect,
                ))
            }
            Edit::AddMisnamed { backlog } => {
                let (dir, _) = KINDS[usize::from(*backlog)];
                let token = self.token();
                let path = format!("{dir}/hand-{token}.md");
                let bytes = format!("# {token}\n").into_bytes();
                let mut effect = Effect::new(Hand::Misnamed);
                effect.misnamed = Some((path.clone(), bytes.clone()));
                Some((Change::Write(path, bytes), effect))
            }
            Edit::Brief(pick) => {
                let briefs = snapshot.briefs;
                let open: Vec<&BriefSeen> = briefs.iter().filter(|brief| !brief.filed).collect();
                // A brief made by hand while the clone's own write runs
                // could take the number that write gives: a duplicate the
                // trace would have typed, like an id that collides.
                let first = open
                    .iter()
                    .find(|brief| !written(brief))
                    .or_else(|| open.first());
                let chosen = match pick {
                    BriefPick::New if window.is_some() => first,
                    BriefPick::New => None,
                    BriefPick::First => first,
                    BriefPick::Any(index) if !open.is_empty() => Some(index.get(&open)),
                    BriefPick::Any(_) => None,
                    BriefPick::Number(number) => {
                        Some(open.iter().find(|brief| brief.number == *number)?)
                    }
                };
                let (path, number, mut lines) = match chosen {
                    // A parked brief is edited by hand in its folder, as a
                    // draft is in `briefs/`.
                    Some(brief) => (
                        format!(
                            "proj/briefs/{}{}",
                            if brief.parked { "parked/" } else { "" },
                            brief.name
                        ),
                        brief.number,
                        brief.text.lines().map(str::to_owned).collect(),
                    ),
                    None if window.is_some() => return None,
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
                let mut effect = Effect::new(Hand::Brief);
                effect.brief = Some((token, number));
                let bytes = lines
                    .iter()
                    .map(|line| format!("{line}\n"))
                    .collect::<String>();
                Some((Change::Write(path, bytes.into_bytes()), effect))
            }
        }
    }

    /// Records a hand edit as made. Its new content was in the clone's files
    /// from then on, seen or not: a write that lost it breaks I9.
    fn made(&mut self, who: Who, effect: Effect) {
        let slot = who.slot();
        match effect.hand {
            Hand::Body => self.depth.body_lines_by_hand += 1,
            Hand::Key => self.depth.keys_by_hand += 1,
            Hand::Delete => self.depth.files_deleted_by_hand += 1,
            Hand::Add => self.depth.files_added_by_hand += 1,
            Hand::Misnamed => self.depth.misnamed_by_hand += 1,
            Hand::Brief => {}
        }
        if let Some(token) = effect.added {
            self.places[slot].tokens.insert(token.clone());
            self.added.insert(token);
        }
        if let Some((id, token)) = effect.deleted {
            self.closed.insert(token.clone());
            self.closed_in_step[slot].insert(token);
            self.pending[slot].remove(&id);
            if let Some(remote) = self.places[2].files.get(&id).cloned() {
                self.deletes_pending[slot].insert(id, remote);
            }
        }
        for (item, tokens, drops, change) in effect.writes {
            self.content_write(&item, &tokens, drops, change);
            self.places[slot].content.extend(tokens);
        }
        if let Some(id) = effect.changed {
            self.pending[slot].insert(id);
        }
        if let Some((token, brief)) = effect.brief {
            self.brief_lines.push((token, who, brief));
        }
        if let Some((path, bytes)) = effect.misnamed {
            self.misnamed.push((who, path, bytes));
        }
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        // A step's body writes stand for all of its checks: the commands a
        // step runs first (to bring a brief or an item there) check the
        // places too, before the write they prepare has run.
        self.body_changes.clear();
        self.closed_in_step = Default::default();
        self.pending_at_step = self.pending.clone();
        self.actor = None;
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
                if let Some((change, effect)) = self.edit(*who, edit, None) {
                    let machine = self.machine(*who);
                    match change {
                        Change::Write(path, bytes) => machine.write(&path, bytes),
                        Change::Remove(path) => fs::remove_file(machine.data().join(path)).unwrap(),
                    }
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
        self.actor = Some(who);
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
        self.depth.given_up_online += u32::from(given_up_online(&run));
        let conflicts = self.notes(who, &run)?;
        match run.code {
            Some(0) => {
                self.depth.succeeded += 1;
                prop_assert_eq!(&run.stdout, "synced\n");
                self.pulled(who, true);
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
        self.conflicts_stay(who, &conflicts, &[])?;
        self.check_everywhere()
    }

    /// A run of `who` got the remote's commits, and pushed all of its own
    /// if `pushed`.
    fn pulled(&mut self, who: Who, pushed: bool) {
        self.groups_unseen[who.slot()].clear();
        if pushed {
            self.pending[who.slot()].clear();
            self.groups_pending[who.slot()].clear();
        }
    }

    /// The items a run noted a conflict of are in the clone: the remote's
    /// version stands, and a delete that won leaves no copy. Not for an item
    /// the run deletes itself.
    fn conflicts_stay(
        &self,
        who: Who,
        conflicts: &[Id],
        closes: &[Id],
    ) -> Result<(), TestCaseError> {
        let snapshot = self.read(who.slot());
        for id in conflicts.iter().filter(|id| !closes.contains(id)) {
            prop_assert!(
                snapshot.items.contains_key(id),
                "a conflict of {:?} noted on {:?}, and the item is gone",
                id,
                who
            );
        }
        Ok(())
    }

    /// For a `queue` that writes first: the planner's sections into the
    /// clone's file of the brief, by hand, when it has none.
    fn write_before_queue(
        &mut self,
        who: Who,
        op: &Op,
        subject: &Subject,
    ) -> Result<(), TestCaseError> {
        let number = match (op, subject) {
            (Op::Queue { write: true, .. }, Subject::Queue(number, _))
            | (Op::Park { .. }, Subject::Park(number)) => number,
            _ => return Ok(()),
        };
        let unwritten = self
            .briefs(who.slot())
            .iter()
            .any(|brief| brief.number == *number && !brief.filed && !written(brief));
        if unwritten
            && let Some((Change::Write(path, bytes), effect)) =
                self.edit(who, &Edit::Brief(BriefPick::Number(*number)), None)
        {
            self.machine(who).write(&path, bytes);
            self.made(who, effect);
        }
        self.check_everywhere()
    }

    /// For an op on an item when the clone and the remote have fewer than
    /// `wanted` of its kind, as mostly they do: the adds that bring them
    /// there, each judged as a command of its own. A second chore goes into
    /// the other group, so that a `group` finds items in either.
    fn prepare_list(
        &mut self,
        who: Who,
        backlog: bool,
        wanted: usize,
    ) -> Result<(), TestCaseError> {
        let prefix = if backlog { 'b' } else { 'c' };
        for _ in 0..wanted {
            let ids: BTreeSet<u32> = self
                .ids_in(who.slot(), prefix)
                .into_iter()
                .chain(self.ids_in(2, prefix))
                .collect();
            if ids.len() >= wanted {
                break;
            }
            let add = if backlog {
                Op::AddBacklog
            } else {
                Op::AddChore {
                    group: ids.is_empty(),
                }
            };
            self.command(who, &add, None)?;
        }
        Ok(())
    }

    /// For `block`, `amend`, `report` and `file` when neither the clone nor
    /// the remote has a brief they take: the commands that bring one there,
    /// as the planner and the worker would have run them, each judged as a
    /// command of its own. Left to chance, a trace seldom gets a brief from
    /// new to started, let alone reported, before it ends. An op on an item
    /// has [`Oracle::prepare_list`].
    fn prepare(&mut self, who: Who, op: &Op) -> Result<(), TestCaseError> {
        let wanted: &[&str] = match op {
            Op::Block(_) | Op::Report(_) => &["started"],
            Op::Amend(_) => &["queued", "started", "blocked"],
            Op::File { .. } => &["reported"],
            Op::Release(_) => &["parked"],
            Op::AddChore { .. }
            | Op::AddBacklog
            | Op::AddWithBody { .. }
            | Op::Done(_)
            | Op::Drop(_)
            | Op::NewBrief
            | Op::Queue { .. }
            | Op::Park { .. }
            | Op::ReleaseAll
            | Op::First(_)
            | Op::After { .. }
            | Op::Next => return Ok(()),
            Op::Group { items, .. } => {
                for backlog in [false, true] {
                    if items.iter().any(|(b, _)| *b == backlog) {
                        self.prepare_list(who, backlog, 2)?;
                    }
                }
                return Ok(());
            }
            Op::Retitle { backlog, .. } | Op::Body { backlog, .. } => {
                return self.prepare_list(who, *backlog, 2);
            }
        };
        // At most new, queue, next and report; fewer when one is refused.
        for _ in 0..4 {
            if !self.open_with(who, wanted).is_empty() {
                break;
            }
            let before = if wanted == ["parked"] {
                if self.drafts(who).is_empty() {
                    Op::NewBrief
                } else {
                    Op::Park {
                        pick: first(),
                        closes: None,
                    }
                }
            } else if !self.open_with(who, &["started"]).is_empty() {
                Op::Report(first())
            } else if !self.open_with(who, &["queued"]).is_empty() {
                Op::Next
            } else if !self.drafts(who).is_empty() {
                // Closing the first item there is, so that `file` closes one.
                Op::Queue {
                    pick: first(),
                    write: true,
                    closes: Some(first()),
                    after: None,
                    first: false,
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
        self.actor = Some(who);
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
                // A replace whose push meets the other clone's write of that
                // body runs again on it, and so sees that write's lines.
                if let (
                    Subject::Body(id, _, true, Some(write)),
                    Subject::Body(window_id, lines, ..),
                ) = (&subject, &window_subject)
                    && id == window_id
                {
                    self.writes[*write].drops.extend(lines.iter().cloned());
                }
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
                // What the command writes: its items.
                let writes: Vec<Id> = subject.items();
                if let Some((change, effect)) = self.edit(who, edit, Some(&writes)) {
                    // git's fast-forward wrote such a file back once.
                    if let Some((id, _)) = &effect.deleted
                        && writes.contains(id)
                    {
                        self.depth.window_delete_of_written += 1;
                    }
                    let script = match change {
                        Change::Write(path, bytes) => {
                            let saved = self.world.dir().join("window-edit");
                            fs::write(&saved, bytes).unwrap();
                            format!(
                                "mkdir -p \"$(dirname {path})\" && cp {} {path} || exit 1\n",
                                quote(&saved.to_string_lossy()),
                                path = quote(&path)
                            )
                        }
                        Change::Remove(path) => format!("rm {} || exit 1\n", quote(&path)),
                    };
                    machine.hook_once("pre-push", &script);
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
        let window_edited = match window_edit {
            Some(effect) if hook_ran => {
                let changed = effect.changed;
                self.made(who, effect);
                Some(changed)
            }
            Some(_) | None => None,
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
            // One clone's `group` and the other's write of the same item, in
            // one push's window: the later runs again on the earlier.
            let grouped = matches!(&subject, Subject::Group(..))
                || matches!(window_subject, Subject::Group(..));
            let window_items = window_subject.items();
            if grouped
                && window_run.code == Some(0)
                && run.code == Some(0)
                && subject.items().iter().any(|id| window_items.contains(id))
            {
                self.depth.group_met += 1;
            }
        }
        self.judge(who, &subject, &run, rejecting, fault, false)?;
        // Saved after the push it waited on: the clone's to push still.
        if let Some(Some(id)) = window_edited {
            self.pending[who.slot()].insert(id);
        }

        // Every path `next` printed is one more claimed brief on the remote,
        // and no brief is claimed twice; a run that ended without knowing
        // whether its push landed may have claimed one more.
        let runs = std::iter::once((&subject, &run))
            .chain(window_run.as_ref().map(|(subject, run)| (subject, run)))
            .filter(|(subject, _)| matches!(subject, Subject::Next));
        let (mut printed, mut unsure) = (0, 0);
        for (_, run) in runs {
            match run.code {
                Some(0) if !run.stdout.starts_with("queue empty") => printed += 1,
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
        self.depth.given_up_online += u32::from(given_up_online(&run));
        if run.code == Some(0) {
            if std::mem::take(&mut self.healing[slot]) {
                self.depth.kill_after_push_healed += 1;
            }
            if matches!(fault, Some(Fault::LostAck)) && hook_ran && !rejecting {
                self.depth.lost_ack_recognised += 1;
            }
            if matches!(fault, Some(Fault::LateLanding))
                && !rejecting
                && run.stderr.contains("retry 3/8")
            {
                self.depth.late_landing_recognised += 1;
            }
            if window_edited.is_some() && run.stderr.contains("board: merged ") {
                self.depth.window_edit_settled += 1;
            }
        }
        if run.code.is_none()
            && matches!(fault, Some(Fault::KillAfterPush))
            && matches!(&subject, Subject::Add(token, _) if self.added.contains(token))
        {
            self.healing[slot] = true;
        }
        self.check_everywhere()
    }

    /// Checks the notes of a run on `who` that ended: each line is a note
    /// the trace can cause, or the one line of a run that failed; a
    /// conflict copy of an item is there, of that item. Gives the items
    /// whose conflict was noted.
    fn notes(&mut self, who: Who, run: &Run) -> Result<Vec<Id>, TestCaseError> {
        let machine = self.machine(who);
        let lines: Vec<&str> = run.stderr.lines().collect();
        let mut conflicts = Vec::new();
        for (at, line) in lines.iter().enumerate() {
            let last = at + 1 == lines.len();
            if line.starts_with("board: merged ") {
                self.depth.merged += 1;
            } else if line.starts_with("board: conflict: brief ") {
                self.depth.brief_conflicts += 1;
            } else if let Some(conflict) = line.strip_prefix("board: conflict: ") {
                let (word, said) = conflict.split_once(' ').expect("conflict: <id> …");
                let id = word
                    .chars()
                    .next()
                    .and_then(|prefix| id_of(prefix, word))
                    .expect("an item's id");
                let path = said
                    .strip_prefix(
                        "changed here and on the remote; the remote version is kept, \
                         yours is saved as ",
                    )
                    .and_then(|path| path.strip_prefix(&format!("{}/", machine.data().display())));
                let copy = path.and_then(|path| machine.file(path));
                prop_assert!(
                    path.is_some_and(|path| path.starts_with(&format!("{}.local", item_path(id))))
                        && copy.as_ref().is_some_and(|copy| {
                            item_seen(id, copy.clone()).header_id == Some(show(id))
                        }),
                    "a conflict note without a copy of {:?} where it says: {:?}",
                    id,
                    run
                );
                self.depth.local_copies += 1;
                let slot = who.slot();
                if self.groups_pending[slot].contains(&id) || self.groups_unseen[slot].contains(&id)
                {
                    self.depth.group_met += 1;
                }
                conflicts.push(id);
            } else if let Some(finding) = line.strip_prefix("board: lint: ") {
                prop_assert!(
                    finding.contains("unmerged local version of ")
                        || finding.contains("not named as the board names its files"),
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
        Ok(conflicts)
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
        let slot = who.slot();
        let offline = !self.online[slot];
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
                    matches!(
                        subject,
                        Subject::Close(_)
                            | Subject::Body(..)
                            | Subject::Group(..)
                            | Subject::Retitle(..)
                            | Subject::Queue(..)
                            | Subject::First(_)
                            | Subject::Park(_)
                            | Subject::Release(_)
                            | Subject::After(_)
                            | Subject::File(..)
                    ) || subject.brief_line().is_some(),
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
                        Subject::Add(..) | Subject::NewBrief(_) | Subject::Next
                    ),
                    "{run:?}"
                );
            }
            None => {}
            Some(_) => prop_assert!(false, "unexpected exit: {run:?}"),
        }
        let conflicts = if run.code.is_some() {
            self.notes(who, run)?
        } else {
            Vec::new()
        };
        // Offline nothing is pushed, though a write that changed nothing (a
        // `group` of items there already) has no commit to note as not
        // pushed.
        let pushed = run.code == Some(0) && !offline && !run.stderr.contains("board: not pushed: ");
        if run.code == Some(0) && !offline {
            self.pulled(who, pushed);
        }
        if run.code == Some(0) && !pushed {
            self.pending[slot].extend(subject.items());
        }
        // Another write of the step may delete the item after this run's
        // merge kept it: the other clone's in the window, or a file saved
        // during the push.
        let met = !window && matches!(fault, Some(Fault::WindowOp(_) | Fault::WindowEdit(_)));
        if !met {
            self.conflicts_stay(who, &conflicts, &subject.closes())?;
        }
        match subject {
            Subject::Add(token, body) => {
                let on_remote: Vec<Id> = self
                    .read(2)
                    .items
                    .values()
                    .filter(|item| item.token == *token)
                    .map(|item| item.id)
                    .collect();
                match run.code {
                    Some(0) => {
                        prop_assert!(!offline, "an add offline exited 0: {run:?}");
                        // The printed id is on the remote with this token.
                        let printed = run.stdout.trim_end();
                        let shown: Vec<String> = on_remote.iter().map(|id| show(*id)).collect();
                        prop_assert_eq!(&shown, &vec![printed.to_owned()], "{:?}", run);
                        self.added.insert(token.clone());
                        if !body.is_empty() {
                            self.depth.bodies_added += 1;
                        }
                        // Written, so in the clone and on the remote: a loss
                        // breaks I9 at once.
                        for place in [slot, 2] {
                            self.places[place].content.extend(body.iter().cloned());
                        }
                    }
                    None if matches!(fault, Some(Fault::KillAfterPush))
                        && !on_remote.is_empty() =>
                    {
                        self.added.insert(token.clone());
                    }
                    Some(_) | None => {
                        self.never.insert(token.clone());
                        self.never.extend(body.iter().cloned());
                    }
                }
            }
            Subject::Body(id, lines, replace, write) => match run.code {
                Some(0) => {
                    prop_assert_eq!(&run.stdout, &format!("amended {}\n", show(*id)));
                    let places = if pushed { vec![slot, 2] } else { vec![slot] };
                    for place in places {
                        self.places[place].content.extend(lines.iter().cloned());
                    }
                    match (replace, lines.is_empty()) {
                        (false, _) => self.depth.bodies_appended += 1,
                        (true, false) => self.depth.bodies_replaced += 1,
                        (true, true) => self.depth.bodies_removed += 1,
                    }
                }
                Some(1) => {
                    let last = run.stderr.lines().last().unwrap_or_default();
                    prop_assert!(
                        last.starts_with(&format!("board: amend {}: ", show(*id)))
                            && (last.ends_with("no such chore")
                                || last.ends_with("no such backlog item")),
                        "{:?}",
                        run
                    );
                    prop_assert!(!self.ids_in(slot, id.0).contains(&id.1), "{:?}", run);
                    self.never.extend(lines.iter().cloned());
                    // Refused: it took nothing out.
                    if let Some(write) = write {
                        self.writes[*write].drops.clear();
                    }
                }
                // Killed: written or not.
                Some(_) | None => {}
            },
            Subject::Close(id) => {
                let token = self.owners.get(id).cloned();
                let here = self.ids_in(slot, id.0).contains(&id.1);
                match run.code {
                    Some(0) => {
                        let past = if id.0 == 'c' { "done" } else { "dropped" };
                        prop_assert_eq!(&run.stdout, &format!("{past} {}\n", show(*id)));
                        prop_assert!(!here, "{id:?} is still in {who:?} after {run:?}");
                        if pushed {
                            prop_assert!(
                                !self.ids_in(2, id.0).contains(&id.1),
                                "{id:?} is still on the remote after {run:?}"
                            );
                        } else if let Some(remote) = self.places[2].files.get(id).cloned() {
                            self.deletes_pending[slot].insert(*id, remote);
                        }
                        self.closed.extend(token.clone());
                        self.closed_in_step[slot].extend(token);
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
            Subject::Group(ids, name) => {
                self.judge_group(who, run, pushed, (ids, name), (fault, window))?;
            }
            Subject::Retitle(id, start, check) => {
                self.judge_retitle(
                    who,
                    run,
                    pushed,
                    (*id, start, check.as_deref()),
                    (fault, window),
                )?;
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
            Subject::Park(number) => match run.code {
                Some(0) => {
                    prop_assert_eq!(&run.stdout, &format!("parked {number}\n"));
                    // Parked, or released since by the other clone's run in
                    // the window, which the retry ran on.
                    let place = if pushed { 2 } else { slot };
                    let found: Vec<BriefSeen> = self
                        .briefs(place)
                        .into_iter()
                        .filter(|brief| brief.number == *number)
                        .collect();
                    prop_assert!(
                        matches!(&found[..], [brief] if (brief.parked
                            && brief.status.as_deref() == Some("parked"))
                            || (window
                                && brief.status.as_deref().is_some_and(|s| reachable("queued", s)))),
                        "{:?} after {:?}",
                        found,
                        run
                    );
                    self.depth.briefs_parked += 1;
                }
                Some(1) => prop_assert!(
                    run.stderr
                        .lines()
                        .last()
                        .is_some_and(|last| last.starts_with(&format!("board: park {number}: "))),
                    "{:?}",
                    run
                ),
                Some(_) | None => {}
            },
            Subject::Release(number) => match run.code {
                Some(0) => {
                    prop_assert!(
                        run.stdout.starts_with(&format!("queued {number}")),
                        "{run:?}"
                    );
                    let place = if pushed { 2 } else { slot };
                    let found: Vec<BriefSeen> = self
                        .briefs(place)
                        .into_iter()
                        .filter(|brief| brief.number == *number)
                        .collect();
                    prop_assert!(
                        matches!(&found[..], [brief] if !brief.parked
                            && brief.status.as_deref().is_some_and(|s| reachable("queued", s))),
                        "{:?} after {:?}",
                        found,
                        run
                    );
                    self.depth.briefs_released += 1;
                }
                Some(1) => prop_assert!(
                    run.stderr
                        .lines()
                        .last()
                        .is_some_and(|last| last.starts_with(&format!("board: queue {number}: "))),
                    "{:?}",
                    run
                ),
                Some(_) | None => {}
            },
            Subject::QueueParked => {
                if run.code != Some(0) {
                    return Ok(());
                }
                prop_assert!(
                    run.stdout == "nothing parked\n" || run.stdout.starts_with("queued "),
                    "{run:?}"
                );
                // One that no other write met leaves `briefs/parked/` empty
                // where it was the last change.
                let saved_after = !pushed && matches!(fault, Some(Fault::WindowEdit(_)));
                if !window && !saved_after {
                    let place = if pushed { 2 } else { slot };
                    let left: Vec<u32> = self
                        .briefs(place)
                        .iter()
                        .filter(|brief| brief.parked)
                        .map(|brief| brief.number)
                        .collect();
                    prop_assert!(left.is_empty(), "{:?} parked after {:?}", left, run);
                    if run.stdout != "nothing parked\n" {
                        self.depth.all_released += 1;
                    }
                }
            }
            Subject::First(number) => match run.code {
                Some(0) => {
                    // Picked as queued; a draft when it ran (the remote's
                    // version kept over a clone's offline queue) is queued
                    // and put first.
                    prop_assert!(
                        run.stdout == format!("{number} first\n")
                            || run.stdout == format!("queued {number}, first\n"),
                        "{run:?}"
                    );
                    self.depth.briefs_put_first += 1;
                }
                Some(1) => prop_assert!(
                    run.stderr
                        .lines()
                        .last()
                        .is_some_and(|last| last.starts_with(&format!("board: queue {number}: "))),
                    "{:?}",
                    run
                ),
                Some(_) | None => {}
            },
            Subject::After(id) => match run.code {
                Some(0) => {
                    prop_assert!(run.stdout.starts_with(&format!("{id} after ")), "{run:?}");
                    self.depth.waits_set += 1;
                }
                Some(1) => prop_assert!(
                    run.stderr
                        .lines()
                        .last()
                        .is_some_and(|last| last.starts_with("board: after")),
                    "{:?}",
                    run
                ),
                Some(_) | None => {}
            },
            Subject::Queue(number, first) => match run.code {
                Some(0) => {
                    // Picked as a draft; queued when it ran (queued on the
                    // clone offline, or by the other clone and brought in by
                    // the write's fetch) it is only put first, as `First`
                    // says the other way round.
                    prop_assert!(
                        run.stdout == format!("queued {number}\n")
                            || run.stdout == format!("queued {number}, first\n")
                            || (*first && run.stdout == format!("{number} first\n")),
                        "{run:?}"
                    );
                    // Queued, or claimed since: a queue run in the window of
                    // a `next` is claimed by its retry.
                    let place = if pushed { 2 } else { slot };
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
                    if run.stdout == format!("{number} first\n") {
                        self.depth.briefs_put_first += 1;
                    } else {
                        self.depth.briefs_queued += 1;
                    }
                }
                Some(1) => {
                    let last = run.stderr.lines().last().unwrap_or_default();
                    prop_assert!(
                        last.starts_with(&format!("board: queue {number}: "))
                            || last.starts_with("board: queue: "),
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
                if run.stdout.starts_with("queue empty") {
                    // In a window the step's command may queue one after, or
                    // satisfy what one waits on.
                    let claimable = remote.iter().any(|brief| {
                        !brief.filed
                            && brief.status.as_deref() == Some("queued")
                            && self.waits_on(2, brief).is_none()
                    });
                    prop_assert!(
                        window || !claimable,
                        "queue empty with a claimable brief on the remote: {:?}",
                        run
                    );
                    if run.stdout != "queue empty\n" {
                        prop_assert!(run.stdout.starts_with("queue empty, "), "{run:?}");
                        self.depth.queue_waiting += 1;
                    }
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
                // No brief is started while what it waits on is not over,
                // as the remote has it after the claim: its push is the
                // last change, and a wait once over stays over.
                prop_assert!(
                    self.waits_on(2, claimed[0]).is_none(),
                    "{} claimed while it waits on {:?}: {:?}",
                    claimed[0].number,
                    self.waits_on(2, claimed[0]),
                    run
                );
                // Never from `briefs/parked/`: the remote had it queued, not
                // parked, before this step, unless the other clone's run in
                // the window released it.
                let released_meanwhile = matches!(
                    fault,
                    Some(Fault::WindowOp(Op::Release(_) | Op::ReleaseAll))
                );
                if !released_meanwhile
                    && let Some((_, Some(was))) = self.remote_briefs.get(&claimed[0].number)
                {
                    prop_assert!(
                        was != "parked",
                        "{} started from briefs/parked/: {:?}",
                        claimed[0].number,
                        run
                    );
                }
                if after_of(&claimed[0].text).is_some() {
                    self.depth.claimed_after_wait += 1;
                }
                self.depth.briefs_claimed += 1;
            }
            Subject::Block(number, token)
            | Subject::Amend(number, token)
            | Subject::Report(number, token) => {
                self.judge_brief_line(who, subject, run, pushed, (*number, token))?;
            }
            Subject::File(number, closes) => {
                self.judge_file(who, run, pushed, *number, closes)?;
            }
        }
        Ok(())
    }

    /// A `retitle` run: what it printed, and the item's new title and check
    /// where the retitle was the last change, as for a `group`; refused only
    /// for an id that is not there.
    fn judge_retitle(
        &mut self,
        who: Who,
        run: &Run,
        pushed: bool,
        (id, start, check): (Id, &str, Option<&str>),
        (fault, window): (Option<&Fault>, bool),
    ) -> Result<(), TestCaseError> {
        match run.code {
            Some(0) => {
                prop_assert_eq!(&run.stdout, &format!("retitled {}\n", show(id)));
                let saved_after = !pushed && matches!(fault, Some(Fault::WindowEdit(_)));
                if !window && !saved_after {
                    // Counted where the item is judged, so the depth test
                    // proves the judging runs.
                    self.depth.items_retitled += 1;
                    let place = if pushed { 2 } else { who.slot() };
                    let snapshot = self.read(place);
                    let item = snapshot.items.get(&id);
                    prop_assert!(
                        item.is_some_and(|item| item.title.starts_with(start)
                            && check.is_none_or(|check| item.check.as_deref() == Some(check))),
                        "{:?} in place {} is {:?} after {:?}",
                        id,
                        place,
                        item,
                        run
                    );
                }
            }
            Some(1) => {
                let last = run.stderr.lines().last().unwrap_or_default();
                prop_assert!(
                    last.starts_with(&format!("board: retitle {}: ", show(id)))
                        && (last.ends_with("no such chore")
                            || last.ends_with("no such backlog item")),
                    "{:?}",
                    run
                );
                prop_assert!(!self.ids_in(who.slot(), id.0).contains(&id.1), "{:?}", run);
            }
            // Killed: retitled or not; a retitle closes nothing.
            Some(_) | None => {}
        }
        Ok(())
    }

    /// The token of an id in the clone or the remote.
    fn token_of(&self, who: Who, id: Id) -> Option<String> {
        [who.slot(), 2].into_iter().find_map(|place| {
            self.read(place)
                .items
                .get(&id)
                .map(|item| item.token.clone())
        })
    }

    /// A `group` run: what it printed, and each item it named in the group
    /// where the write was the last change; refused only for an id that is
    /// not there.
    fn judge_group(
        &mut self,
        who: Who,
        run: &Run,
        pushed: bool,
        (ids, name): (&[Id], &str),
        (fault, window): (Option<&Fault>, bool),
    ) -> Result<(), TestCaseError> {
        match run.code {
            Some(0) => {
                // The ids it changed, in the order given: the ones that
                // were not in the group, which the oracle cannot know when a
                // merge came between.
                let nothing = format!("nothing to group: all are under {name} already\n");
                if run.stdout == nothing {
                    self.depth.grouped_nothing += 1;
                } else {
                    let printed = run
                        .stdout
                        .strip_prefix("grouped ")
                        .and_then(|rest| rest.strip_suffix(&format!(" under {name}\n")));
                    let mut given = ids.iter().map(|id| show(*id));
                    prop_assert!(
                        printed.is_some_and(|printed| printed
                            .split(' ')
                            .all(|word| given.any(|id| id == word))),
                        "{:?} for {:?} under {}",
                        run,
                        ids,
                        name
                    );
                }
                let slot = who.slot();
                for id in ids {
                    if pushed {
                        self.groups_unseen[who.other().slot()].insert(*id);
                    } else {
                        self.groups_pending[slot].insert(*id);
                    }
                }
                if !pushed {
                    self.depth.groups_not_pushed += 1;
                }
                // Where the write was the last change: the remote when
                // pushed, else the clone. Not for a run in a window, which
                // the step's own command comes after, nor where a file saved
                // during a push that failed was merged in after it.
                let saved_after = !pushed && matches!(fault, Some(Fault::WindowEdit(_)));
                if !window && !saved_after {
                    let place = if pushed { 2 } else { slot };
                    let snapshot = self.read(place);
                    for id in ids {
                        let item = snapshot.items.get(id);
                        prop_assert!(
                            item.is_some_and(|item| item.group.as_deref() == Some(name)),
                            "{:?} in place {} is {:?} after {:?}",
                            id,
                            place,
                            item,
                            run
                        );
                        self.depth.items_grouped += 1;
                    }
                    if ids.iter().any(|id| id.0 == 'c') && ids.iter().any(|id| id.0 == 'b') {
                        self.depth.grouped_both_kinds += 1;
                    }
                }
            }
            Some(1) => {
                let last = run.stderr.lines().last().unwrap_or_default();
                let named = ids.iter().find(|id| {
                    last.starts_with(&format!("board: group {}: ", show(**id)))
                        && (last.ends_with("no such chore")
                            || last.ends_with("no such backlog item"))
                });
                prop_assert!(named.is_some(), "{:?} for {:?}", run, ids);
                let id = *named.expect("checked");
                prop_assert!(!self.ids_in(who.slot(), id.0).contains(&id.1), "{:?}", run);
            }
            // Killed: regrouped or not; a `group` closes nothing.
            Some(_) | None => {}
        }
        Ok(())
    }

    /// A `file` run: what it printed, the brief filed in the place it wrote
    /// to, and the items it says it closed gone from there.
    fn judge_file(
        &mut self,
        who: Who,
        run: &Run,
        pushed: bool,
        number: u32,
        closes: &[Id],
    ) -> Result<(), TestCaseError> {
        match run.code {
            Some(0) => {
                let rest = run
                    .stdout
                    .strip_suffix('\n')
                    .and_then(|line| line.strip_prefix(&format!("filed {number} (")));
                let Some((commits, closed)) = rest.and_then(|rest| rest.split_once(')')) else {
                    return Err(TestCaseError::fail(format!("file printed {run:?}")));
                };
                prop_assert!(
                    matches!(commits, "no commits" | "1 commit")
                        || commits
                            .strip_suffix(" commits")
                            .is_some_and(|n| n.parse::<u32>().is_ok_and(|n| n > 1)),
                    "{:?}",
                    run
                );
                let closed: Vec<Id> = match closed.strip_prefix(" closed ") {
                    Some(ids) => ids.split(' ').filter_map(|id| id_of('b', id)).collect(),
                    None => {
                        prop_assert_eq!(closed, "", "{:?}", run);
                        Vec::new()
                    }
                };
                prop_assert!(
                    closed.iter().all(|id| closes.contains(id)),
                    "file closed {:?}, its brief names {:?}: {:?}",
                    closed,
                    closes,
                    run
                );
                let place = if pushed { 2 } else { who.slot() };
                let found: Vec<BriefSeen> = self
                    .briefs(place)
                    .into_iter()
                    .filter(|brief| brief.number == number)
                    .collect();
                prop_assert!(
                    matches!(&found[..], [brief] if brief.filed
                        && brief.status.as_deref() == Some("filed")),
                    "brief {} is not filed after {:?}: {:?}",
                    number,
                    run,
                    found
                );
                let left = self.ids_in(place, 'b');
                for id in &closed {
                    prop_assert!(
                        !left.contains(&id.1),
                        "{:?} is still there after {:?}",
                        id,
                        run
                    );
                    let token = self.owners.get(id).cloned();
                    self.closed.extend(token.clone());
                    self.closed_in_step[who.slot()].extend(token);
                }
                // And none it names is left open unsaid: ids are never
                // reused, so a named id still there is one `file` skipped.
                for id in closes {
                    prop_assert!(
                        !left.contains(&id.1),
                        "{:?}, named by brief {}, is still there after {:?}",
                        id,
                        number,
                        run
                    );
                }
                self.depth.briefs_filed += 1;
                self.depth.closed_by_file += u32::try_from(closed.len()).unwrap();
            }
            Some(1) => {
                let last = run.stderr.lines().last().unwrap_or_default();
                prop_assert!(
                    last.starts_with(&format!("board: file {number}: ")),
                    "{:?}",
                    run
                );
            }
            // Killed: the items it closes are closed or not.
            Some(_) | None => {
                for id in closes {
                    self.maybe_closed.extend(self.owners.get(id).cloned());
                }
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

    /// The invariants in every place.
    fn check_everywhere(&mut self) -> Result<(), TestCaseError> {
        for who in [Who::A, Who::B] {
            prop_assert_eq!(
                self.machine(who).head_ref(),
                "refs/heads/main",
                "I5: {:?} is off main",
                who
            );
        }
        let snapshots = [self.read(0), self.read(1), self.read(2)];
        for (place, snapshot) in snapshots.iter().enumerate() {
            self.check_place(place, snapshot)?;
            self.check_briefs(place, &snapshot.briefs)?;
        }
        for snapshot in &snapshots {
            let copies = snapshot.copies.iter().map(|(_, copy)| copy);
            for item in snapshot.items.values().chain(copies) {
                self.bodies_seen
                    .entry(item.token.clone())
                    .or_default()
                    .insert(item.body.clone());
            }
        }
        Ok(())
    }

    /// I8: the bodies an item may have now: one seen before, or one this
    /// step's writes make of one, in any order. A body that is neither is
    /// a mix of two writers' lines, or a line lost or applied twice.
    fn bodies_allowed(&self, item: &str) -> BTreeSet<Vec<String>> {
        let mut allowed = self.bodies_seen.get(item).cloned().unwrap_or_default();
        allowed.insert(Vec::new());
        let changes: Vec<&BodyChange> = self
            .body_changes
            .iter()
            .filter(|(of, _)| of == item)
            .map(|(_, change)| change)
            .collect();
        for _ in 0..changes.len().min(3) {
            let more: Vec<Vec<String>> = allowed
                .iter()
                .flat_map(|body| changes.iter().map(move |change| change.apply(body)))
                .collect();
            allowed.extend(more);
        }
        allowed
    }

    /// I6, and no brief of a `new brief` that failed.
    fn check_briefs(&mut self, place: usize, briefs: &[BriefSeen]) -> Result<(), TestCaseError> {
        let name = ["A", "B", "the remote"][place];
        let mut numbers = BTreeSet::new();
        for brief in briefs {
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
        for brief in briefs {
            // The place decides, as the tool reads it.
            let status = if brief.filed {
                Some("filed".to_owned())
            } else if brief.parked {
                Some("parked".to_owned())
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

    /// I1 to I5 and I7 to I9 in a place, and nothing that never was
    /// anywhere.
    fn check_place(&mut self, place: usize, snapshot: &Snapshot) -> Result<(), TestCaseError> {
        let name = ["A", "B", "the remote"][place];
        let mut ids = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        let mut content = BTreeSet::new();
        for item in snapshot.items.values() {
            let id = item.id;
            prop_assert_eq!(
                item.header_id.clone(),
                Some(show(id)),
                "I1: the file of {:?} in {} has another id: {:?}",
                id,
                name,
                item
            );
            prop_assert!(
                item.odd.is_empty(),
                "a line no write made, in {:?} in {}: {:?}",
                id,
                name,
                item
            );
            prop_assert!(
                tokens.insert(item.token.clone()),
                "I1: {} in two files of {}",
                item.token,
                name
            );
            prop_assert!(
                !self.never.contains(&item.token),
                "{} was never added, and is in {}",
                item.token,
                name
            );
            prop_assert!(
                !self.places[place].gone.contains(&id),
                "I3: {:?} came back to {}",
                id,
                name
            );
            match self.owners.get(&id) {
                Some(owner) => {
                    prop_assert_eq!(owner, &item.token, "I2: {:?} carries two tokens", id)
                }
                None => {
                    self.owners.insert(id, item.token.clone());
                }
            }
            for token in item.content() {
                prop_assert!(
                    content.insert(token.clone()),
                    "I1: {} in two items of {}",
                    token,
                    name
                );
                prop_assert!(
                    !self.never.contains(token),
                    "{} was never written, and is in {}",
                    token,
                    name
                );
                let written_for = self.content_owner.get(token);
                prop_assert!(
                    written_for == Some(&item.token),
                    "I7: {} is in {:?} in {}, written for {:?}",
                    token,
                    id,
                    name,
                    written_for
                );
            }
            ids.insert(id);
        }
        // I5: nothing the board does not commit.
        if place == 2 {
            prop_assert!(
                snapshot.copies.is_empty() && snapshot.other.is_empty(),
                "I5: a copy or a misnamed file is on the remote: {:?} {:?}",
                snapshot.copies,
                snapshot.other
            );
        }
        for path in &snapshot.other {
            prop_assert!(
                self.misnamed
                    .iter()
                    .any(|(who, made, _)| who.slot() == place && made == path),
                "{} in {} is no file the trace made",
                path,
                name
            );
        }
        for (path, copy) in &snapshot.copies {
            prop_assert_eq!(
                self.owners.get(&copy.id),
                Some(&copy.token),
                "I2: copy {} in {} is of another item",
                path,
                name
            );
            for token in copy.content() {
                prop_assert!(
                    self.content_owner.get(token) == Some(&copy.token),
                    "I7: {} is in copy {} in {}",
                    token,
                    path,
                    name
                );
            }
        }
        // I8: each body, in the files and in the copies, is one seen before
        // or one this step's writes made of one.
        let copies = snapshot.copies.iter().map(|(_, copy)| copy);
        for item in snapshot.items.values().chain(copies) {
            let allowed = self.bodies_allowed(&item.token);
            prop_assert!(
                allowed.contains(&item.body),
                "I8: the body of {:?} in {} is {:?}, not one writer's whole: {:?}",
                item.id,
                name,
                item.body,
                allowed
            );
        }
        // I9: content leaves an item that stays only for a write that saw it
        // and took it out, or into a copy of the clone whose version lost.
        let left: Vec<String> = self.places[place]
            .content
            .difference(&content)
            .cloned()
            .collect();
        for token in left {
            let item = self
                .content_owner
                .get(&token)
                .cloned()
                .expect("content comes from a write");
            let item = &item;
            if !tokens.contains(item) {
                continue;
            }
            let made = self.content_made[&token];
            let dropped = self.writes.iter().any(|write| {
                write.id != made && write.item == *item && write.drops.contains(&token)
            });
            if dropped {
                self.depth.content_dropped += 1;
                continue;
            }
            let in_copy = snapshot
                .copies
                .iter()
                .any(|(_, copy)| copy.token == *item && copy.content().any(|t| *t == token));
            prop_assert!(
                in_copy,
                "I9: {} left {} in {} with no write that took it out and no copy: now {:?}",
                token,
                item,
                name,
                snapshot.by_token(item)
            );
            self.depth.content_kept_in_copy += 1;
        }
        // I4, and the deletes that won over an edit.
        for token in self.places[place].tokens.difference(&tokens) {
            prop_assert!(
                self.closed.contains(token) || self.maybe_closed.contains(token),
                "I4: {} left {} without being closed",
                token,
                name
            );
        }
        let gone: Vec<Id> = self.places[place].ids.difference(&ids).copied().collect();
        for id in gone {
            self.places[place].gone.insert(id);
            let token = self.owners[&id].clone();
            if place < 2 {
                // The clone changed it, and the other side's delete took it
                // at a merge.
                let changed =
                    self.pending[place].contains(&id) || self.pending_at_step[place].contains(&id);
                if changed && !self.closed_in_step[place].contains(&token) {
                    self.depth.delete_won += 1;
                }
                self.pending[place].remove(&id);
            } else {
                // The clone's delete pushed over a change the remote got
                // after it.
                let was = self.places[2].files.get(&id);
                let won = self.actor.is_some_and(|actor| {
                    self.deletes_pending[actor.slot()]
                        .get(&id)
                        .is_some_and(|then| Some(then) != was)
                });
                if won {
                    self.depth.delete_won += 1;
                }
                for pending in &mut self.deletes_pending {
                    pending.remove(&id);
                }
            }
        }
        let seen = &mut self.places[place];
        seen.ids = ids;
        seen.tokens = tokens;
        seen.content = content;
        seen.files = snapshot
            .items
            .iter()
            .map(|(id, item)| (*id, item.bytes.clone()))
            .collect();
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
            self.actor = Some(who);
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
            let conflicts = self.notes(who, &run)?;
            self.pulled(who, true);
            if std::mem::take(&mut self.healing[who.slot()]) {
                self.depth.kill_after_push_healed += 1;
            }
            self.conflicts_stay(who, &conflicts, &[])?;
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
            // Conflict copies and misnamed files are untracked and stay;
            // nothing else may.
            let changed: Vec<String> = machine
                .status()
                .lines()
                .filter(|line| {
                    let untracked = line.strip_prefix("?? ");
                    !untracked.is_some_and(|path| {
                        path.contains(".md.local")
                            || self
                                .misnamed
                                .iter()
                                .any(|(by, made, _)| *by == who && made == path)
                    })
                })
                .map(str::to_owned)
                .collect();
            prop_assert_eq!(changed, Vec::<String>::new(), "{:?} has changed files", who);
        }
        for (who, path, bytes) in &self.misnamed {
            let now = self.machine(*who).file(path);
            prop_assert_eq!(
                now.as_ref(),
                Some(bytes),
                "misnamed {} changed on {:?}",
                path,
                who
            );
        }
        let remote = self.read(2);
        for token in &self.added {
            let count = remote
                .items
                .values()
                .filter(|item| item.token == *token)
                .count();
            if self.closed.contains(token) {
                prop_assert_eq!(count, 0, "{} was closed and is on the remote", token);
            } else if !self.maybe_closed.contains(token) {
                prop_assert_eq!(count, 1, "{} is to be on the remote once", token);
            }
        }
        // A line written into a brief is in that brief, or in a conflict
        // copy the clone that wrote it saved.
        let remote_briefs = remote.briefs;
        for (token, who, number) in &self.brief_lines {
            let line = format!("body {token}");
            let has_line = |text: &str| text.lines().any(|l| l == line);
            let in_brief = remote_briefs
                .iter()
                .any(|brief| brief.number == *number && has_line(&brief.text));
            // A copy is `<brief file name>.local[<n>]`, of the clone's file
            // of that number, whatever the remote's is named, and where the
            // clone had it: a brief the clone filed is in `done/`.
            let machine = self.machine(*who);
            let in_copy = ["proj/briefs", "proj/briefs/parked", "proj/briefs/done"]
                .into_iter()
                .any(|dir| {
                    let dir = machine.data().join(dir);
                    fs::read_dir(&dir).into_iter().flatten().any(|entry| {
                        let entry = entry.unwrap();
                        let file_name = entry.file_name().to_string_lossy().into_owned();
                        entry.file_type().unwrap().is_file()
                            && file_name
                                .split_once(".md.local")
                                .and_then(|(brief, _)| brief_number(&format!("{brief}.md")))
                                == Some(*number)
                            && has_line(&fs::read_to_string(entry.path()).unwrap())
                    })
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
    assert!(depth.given_up_online > 0, "{depth:?}");
    assert!(depth.refused > 0, "{depth:?}");
    assert!(depth.offline_refused > 0, "{depth:?}");
    assert!(depth.not_pushed_then_pushed > 0, "{depth:?}");
    assert!(depth.merged > 0, "{depth:?}");
    assert!(depth.window_edit_settled > 0, "{depth:?}");
    assert!(depth.brief_conflicts > 0, "{depth:?}");
    assert!(depth.lost_ack_recognised > 0, "{depth:?}");
    assert!(depth.late_landing_recognised > 0, "{depth:?}");
    assert!(depth.kill_after_push_healed > 0, "{depth:?}");
    assert!(depth.briefs_created > 0, "{depth:?}");
    assert!(depth.briefs_queued > 0, "{depth:?}");
    assert!(depth.briefs_claimed > 0, "{depth:?}");
    assert!(depth.claimed_after_wait > 0, "{depth:?}");
    assert!(depth.queue_waiting > 0, "{depth:?}");
    assert!(depth.briefs_put_first > 0, "{depth:?}");
    assert!(depth.briefs_parked > 0, "{depth:?}");
    assert!(depth.briefs_released > 0, "{depth:?}");
    assert!(depth.all_released > 0, "{depth:?}");
    assert!(depth.waits_set > 0, "{depth:?}");
    assert!(depth.queue_empty > 0, "{depth:?}");
    assert!(depth.briefs_blocked > 0, "{depth:?}");
    assert!(depth.briefs_resumed > 0, "{depth:?}");
    assert!(depth.briefs_reported > 0, "{depth:?}");
    assert!(depth.briefs_filed > 0, "{depth:?}");
    assert!(depth.closed_by_file > 0, "{depth:?}");
    assert!(depth.items_grouped > 0, "{depth:?}");
    assert!(depth.group_met > 0, "{depth:?}");
    assert!(depth.grouped_both_kinds > 0, "{depth:?}");
    assert!(depth.grouped_nothing > 0, "{depth:?}");
    assert!(depth.items_retitled > 0, "{depth:?}");
    assert!(depth.bodies_added > 0, "{depth:?}");
    assert!(depth.bodies_appended > 0, "{depth:?}");
    assert!(depth.bodies_replaced > 0, "{depth:?}");
    assert!(depth.bodies_removed > 0, "{depth:?}");
    assert!(depth.local_copies > 0, "{depth:?}");
    assert!(depth.delete_won > 0, "{depth:?}");
    assert!(depth.content_dropped > 0, "{depth:?}");
    assert!(depth.content_kept_in_copy > 0, "{depth:?}");
    assert!(depth.body_lines_by_hand > 0, "{depth:?}");
    assert!(depth.keys_by_hand > 0, "{depth:?}");
    assert!(depth.files_deleted_by_hand > 0, "{depth:?}");
    assert!(depth.files_added_by_hand > 0, "{depth:?}");
    assert!(depth.misnamed_by_hand > 0, "{depth:?}");
    assert!(
        depth.succeeded * 10 >= depth.commands * 6,
        "fewer than 60% of the commands succeeded: {depth:?}"
    );
}

// Traces that once failed, written out: proptest's regression file keeps a
// seed, not the steps, and a seed replays another trace once the strategies
// change shape.

/// The `Index` that picks element `at` of `len`. proptest gives `Index` no
/// constructor, so one is drawn until it picks that one.
fn index_at(at: usize, len: usize) -> Index {
    let mut runner = TestRunner::deterministic();
    loop {
        let index = any::<Index>()
            .new_tree(&mut runner)
            .expect("an Index strategy never rejects")
            .current();
        if index.index(len) == at {
            return index;
        }
    }
}

/// The `Index` that picks the first element, whatever the length: one
/// shrunk all the way, to zero.
fn first() -> Index {
    let mut tree = any::<Index>()
        .new_tree(&mut TestRunner::deterministic())
        .expect("an Index strategy never rejects");
    while tree.simplify() {}
    tree.current()
}

/// [`run_trace`] on a world it gives back, for a test to look at after.
fn run_example_in(steps: &[Step]) -> (Depth, World) {
    let world = World::new();
    let depth = {
        let mut oracle = Oracle::new(&world);
        let run = steps
            .iter()
            .try_for_each(|step| oracle.step(step))
            .and_then(|()| oracle.quiesce());
        if let Err(error) = run {
            panic!("{error}");
        }
        oracle.depth
    };
    (depth, world)
}

fn run_example(steps: &[Step]) -> Depth {
    run_example_in(steps).0
}

fn cmd(who: Who, op: Op) -> Step {
    Step::Cmd {
        who,
        op,
        fault: None,
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
            fault: Some(Fault::WindowEdit(Edit::AddFile { backlog: true })),
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
                    closes: None,
                    after: None,
                    first: false,
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
                closes: None,
                after: None,
                first: false,
            },
            fault: Some(Fault::WindowEdit(Edit::Brief(BriefPick::First))),
        },
        Step::RejectPushes(9),
        Step::Cmd {
            who: Who::A,
            op: Op::Next,
            fault: Some(Fault::LostAck),
        },
    ]);
    assert!(depth.briefs_queued > 0, "{depth:?}");
}

/// `group` on a clone that is offline, both kinds, in two runs; meanwhile
/// the other clone adds an item and regroups the same chore. At the sync
/// the backlog item, which only A regrouped, is in A's group; the chore
/// both clones regrouped keeps the remote's group, B's, and A's is its copy.
#[test]
fn a_group_made_offline_meets_the_remote_s() {
    let grouping = |backlog, name| Op::Group {
        items: vec![(backlog, first())],
        name,
    };
    let (depth, world) = run_example_in(&[
        // Two of each, as a `group`'s preparation brings: none is needed
        // offline, where an add is refused.
        cmd(Who::A, Op::AddBacklog),
        cmd(Who::A, Op::AddBacklog),
        cmd(Who::A, Op::AddChore { group: true }),
        cmd(Who::A, Op::AddChore { group: false }),
        Step::Sync { who: Who::B },
        Step::Net {
            who: Who::A,
            online: false,
        },
        // b1 and c1 into Later.
        cmd(Who::A, grouping(true, 2)),
        cmd(Who::A, grouping(false, 2)),
        cmd(Who::B, Op::AddBacklog),
        // c1 into Work.
        cmd(Who::B, grouping(false, 1)),
        Step::Net {
            who: Who::A,
            online: true,
        },
    ]);
    assert!(depth.groups_not_pushed >= 2, "{depth:?}");
    assert!(depth.items_grouped >= 3, "{depth:?}");
    assert!(depth.group_met > 0, "{depth:?}");
    assert!(depth.local_copies > 0, "{depth:?}");
    let group = |file: &str, id: Id| item_seen(id, world.remote_file(file).unwrap()).group;
    assert_eq!(
        group("proj/backlog/b1.md", ('b', 1)).as_deref(),
        Some("Later")
    );
    assert_eq!(
        group("proj/chores/c1.md", ('c', 1)).as_deref(),
        Some("Work")
    );
    assert!(world.a().file("proj/chores/c1.md.local").is_some());
}

/// Both clones write one item's body before either has the other's: A
/// offline, B online, then A syncs. The item keeps the remote's version, B's,
/// whole; A's is its copy, whole; nothing is mixed.
#[test]
fn a_body_written_on_both_clones_keeps_the_remote_s_and_copies_the_clone_s() {
    let amend = |replace, lines| Op::Body {
        backlog: true,
        item: first(),
        replace,
        lines,
    };
    // Tokens in the order made: b1 is t1 with body t2 t3, b2 is t4; A's
    // lines come next, then B's.
    let cases: [(Op, Op, Vec<&str>, Vec<&str>); 3] = [
        (
            amend(false, 2),
            amend(false, 1),
            vec!["t2", "t3", "t7"],
            vec!["t2", "t3", "t5", "t6"],
        ),
        (
            amend(true, 2),
            amend(false, 2),
            vec!["t2", "t3", "t7", "t8"],
            vec!["t5", "t6"],
        ),
        (
            amend(false, 1),
            amend(true, 0),
            vec![],
            vec!["t2", "t3", "t5"],
        ),
    ];
    for (a_writes, b_writes, remote, copy) in cases {
        let (depth, world) = run_example_in(&[
            cmd(Who::A, Op::AddWithBody { backlog: true }),
            cmd(Who::A, Op::AddBacklog),
            Step::Sync { who: Who::B },
            Step::Net {
                who: Who::A,
                online: false,
            },
            cmd(Who::A, a_writes),
            cmd(Who::B, b_writes),
            Step::Net {
                who: Who::A,
                online: true,
            },
            Step::Sync { who: Who::A },
        ]);
        let body = |bytes: Option<Vec<u8>>| item_seen(('b', 1), bytes.unwrap()).body;
        assert_eq!(body(world.remote_file("proj/backlog/b1.md")), remote);
        assert_eq!(body(world.a().file("proj/backlog/b1.md.local")), copy);
        assert!(depth.local_copies > 0, "{depth:?}");
        assert!(depth.content_kept_in_copy > 0, "{depth:?}");
    }
}

/// A body line typed on A while A's own write pushes lands after that
/// push's commit: B, replacing the body meanwhile, never had it, and A's
/// sync keeps B's body and saves A's, with the line, as its copy (a
/// review's case: the oracle must not take the line for pushed with A's
/// write).
#[test]
fn a_body_line_saved_during_a_push_goes_out_with_the_next() {
    let depth = run_example(&[
        cmd(Who::A, Op::AddWithBody { backlog: true }),
        Step::Sync { who: Who::B },
        Step::Cmd {
            who: Who::A,
            op: Op::AddChore { group: false },
            fault: Some(Fault::WindowEdit(Edit::BodyLine(first()))),
        },
        cmd(
            Who::B,
            Op::Body {
                backlog: true,
                item: first(),
                replace: true,
                lines: 1,
            },
        ),
        Step::Sync { who: Who::A },
    ]);
    assert!(depth.local_copies > 0, "{depth:?}");
}

/// An item deleted on one clone and edited on the other is deleted, either
/// way round, without a copy: A's hand delete over B's amend, B's `drop`
/// over A's hand edit.
#[test]
fn a_delete_wins_over_an_edit_either_way_round() {
    let depth = run_example(&[
        cmd(Who::A, Op::AddBacklog),
        cmd(Who::A, Op::AddBacklog),
        Step::Sync { who: Who::B },
        // A deletes b1 by hand; B amends it and pushes; A's sync deletes it
        // on the remote.
        Step::HandEdit {
            who: Who::A,
            edit: Edit::Delete(first()),
        },
        cmd(
            Who::B,
            Op::Body {
                backlog: true,
                item: first(),
                replace: false,
                lines: 1,
            },
        ),
        Step::Sync { who: Who::A },
        // A edits b2 by hand; B drops it; A's sync deletes it on A.
        Step::HandEdit {
            who: Who::A,
            edit: Edit::Key {
                item: first(),
                group: false,
            },
        },
        // B has b1 still, the remote b2 only: the second of the two.
        cmd(Who::B, Op::Drop(index_at(1, 2))),
        Step::Sync { who: Who::A },
    ]);
    assert!(depth.delete_won >= 2, "{depth:?}");
    assert_eq!(depth.local_copies, 0, "{depth:?}");
}
