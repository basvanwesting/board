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
//! - every id a command printed or assigned is on the remote with its token,
//!   and nothing refused or never pushed is anywhere;
//! - convergence, at `Quiesce` and at the end: after `sync` on A, B and A,
//!   both clones are on the remote's commit with no board file changed,
//!   every token added and not closed is there once, and every line written
//!   into a brief is in the brief or in a conflict copy of the clone that
//!   wrote it.
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
    /// A line added to the body of brief 1 or 2, made by hand when missing.
    Brief { second: bool },
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
        // Mostly the one brief, so both clones edit it between syncs.
        5 => prop::bool::weighted(0.1).prop_map(|second| Edit::Brief { second }),
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

/// The briefs a hand edit writes into.
const BRIEFS: [&str; 2] = ["proj/briefs/1-hand.md", "proj/briefs/2-hand.md"];

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
}

/// What a hand edit does, for the oracle to record once it is made.
#[derive(Debug, Default)]
struct Effect {
    /// Tokens added to a list.
    added: Vec<String>,
    /// Tokens of lines deleted.
    deleted: Vec<String>,
    /// A token written into a brief, which one.
    brief: Option<(String, usize)>,
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
    /// Lines written into briefs: token, writer, brief.
    brief_lines: Vec<(String, Who, usize)>,
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
        }
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
            Edit::Brief { second } => {
                let brief = usize::from(*second);
                let path = BRIEFS[brief];
                let mut lines = lines_of(path);
                if lines.is_empty() {
                    lines = vec![
                        format!("id: {}", brief + 1),
                        "status: draft".to_owned(),
                        String::new(),
                        "# By hand".to_owned(),
                    ];
                }
                let token = self.token();
                lines.push(format!("body {token}"));
                effect.brief = Some((token, brief));
                (path.to_owned(), lines)
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

    fn command(&mut self, who: Who, op: &Op, fault: Option<&Fault>) -> Result<(), TestCaseError> {
        let machine = self.machine(who);
        let other = self.machine(who.other());
        let rejecting = self.world.rejects_left() > 0;
        let (args, subject) = self.args(who, op);
        let mut window = None;
        let mut window_edit = None;
        match fault {
            None => {}
            Some(Fault::WindowOp(window_op)) => {
                let (window_args, window_subject) = self.args(who.other(), window_op);
                let words: Vec<&str> = window_args.iter().map(String::as_str).collect();
                machine.hook_once(
                    "pre-push",
                    &pre_push::window(&other.script(&words), self.world.dir()),
                );
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

        let output = machine.command().args(&args).output().expect("run board");
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
            self.judge(who.other(), window_subject, window_run, rejecting, None)?;
            if window_run.code == Some(0) && run.code == Some(0) && run.stderr.contains("retry") {
                self.depth.raced_in_the_window += 1;
            }
        }
        self.judge(who, &subject, &run, rejecting, fault)?;

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
    /// records what became of its subject.
    fn judge(
        &mut self,
        who: Who,
        subject: &Subject,
        run: &Run,
        rejecting: bool,
        fault: Option<&Fault>,
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
                prop_assert!(matches!(subject, Subject::Close(_)), "{run:?}");
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
                prop_assert!(matches!(subject, Subject::Add(_)), "{run:?}");
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
        }
        Ok(())
    }

    /// I1 to I5 in every place, and nothing that never was anywhere.
    fn check_everywhere(&mut self) -> Result<(), TestCaseError> {
        for (who, place) in [(Who::A, 0), (Who::B, 1)] {
            prop_assert_eq!(
                self.machine(who).head_ref(),
                "refs/heads/main",
                "I5: {:?} is off main",
                who
            );
            self.check_place(place)?;
        }
        self.check_place(2)
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
        for (token, who, brief) in &self.brief_lines {
            let line = format!("body {token}");
            let in_brief = self
                .world
                .remote_file(BRIEFS[*brief])
                .is_some_and(|bytes| String::from_utf8(bytes).unwrap().lines().any(|l| l == line));
            let machine = self.machine(*who);
            let dir = machine.data().join("proj/briefs");
            let copy_prefix = format!("{}.local", BRIEFS[*brief].rsplit('/').next().unwrap());
            let in_copy = fs::read_dir(&dir).into_iter().flatten().any(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&copy_prefix)
                    && fs::read_to_string(entry.path())
                        .unwrap()
                        .lines()
                        .any(|l| l == line)
            });
            prop_assert!(
                in_brief || in_copy,
                "{} written by {:?} into brief {} is lost",
                token,
                who,
                brief + 1
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
    assert!(
        depth.succeeded * 10 >= depth.commands * 6,
        "fewer than 60% of the commands succeeded: {depth:?}"
    );
}
