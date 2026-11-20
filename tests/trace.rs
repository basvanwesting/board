//! Random interleavings of writes on two clones of one remote, with the
//! faults git can make: refused pushes, a write of the other clone while a
//! push is on its way, a push that landed but was reported failed, one that
//! landed only after the next round's fetch, and a command killed before or
//! after its push.
//!
//! An oracle reads the list files of the three places (clone A's files,
//! clone B's, the remote's `main`) after every step and checks:
//!
//! - I1: no id and no token on two lines of a file;
//! - I2: an id never carries two tokens, in any place, over the whole trace;
//! - every id a command printed is on the remote with the command's token,
//!   every item that was added is there once until closed, and nothing that
//!   was refused, never pushed or closed is there;
//! - convergence, at `Quiesce` and at the end: after `sync` on A, B and A,
//!   both clones are on the remote's commit with nothing changed.
//!
//! Every item's text is a token (`t7`) used once, so a line applied twice or
//! an id given to two items shows.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::ExitStatusExt;

use common::{Machine, World, pre_push};
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
}

#[derive(Debug, Clone)]
enum Op {
    AddChore {
        group: bool,
    },
    AddBacklog,
    /// A chore on the remote, picked when the step runs.
    Done(Index),
    /// A backlog item on the remote, picked when the step runs.
    Drop(Index),
}

#[derive(Debug, Clone)]
enum Fault {
    /// The other clone runs this while the command's first push waits.
    WindowOp(Op),
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
    /// The remote refuses the next pushes, this many.
    RejectPushes(u8),
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

fn fault() -> impl Strategy<Value = Option<Fault>> {
    prop_oneof![
        6 => Just(None),
        2 => op().prop_map(|op| Some(Fault::WindowOp(op))),
        1 => Just(Some(Fault::LostAck)),
        1 => Just(Some(Fault::LateLanding)),
        1 => Just(Some(Fault::KillBeforePush)),
        1 => Just(Some(Fault::KillAfterPush)),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        12 => (who(), op(), fault()).prop_map(|(who, op, fault)| Step::Cmd { who, op, fault }),
        1 => (0..6u8).prop_map(Step::RejectPushes),
        1 => Just(Step::Quiesce),
    ]
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(step(), 1..=30)
}

const FILES: [&str; 2] = ["proj/chores.md", "proj/backlog.md"];

/// What became of an item's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// On the remote, once.
    Live,
    /// Never on the remote: refused, not pushed, or killed before its push.
    Never,
    /// Done or dropped: no longer on the remote.
    Closed,
}

/// A list id: `('c', 12)`.
type Id = (char, u32);

/// One item line as the oracle reads it: `- <id> <token>[. Check: x]`.
fn items(bytes: &[u8]) -> Result<Vec<(Id, String)>, TestCaseError> {
    let text = String::from_utf8(bytes.to_vec()).expect("the trace writes UTF-8");
    let mut items = Vec::new();
    for line in text.lines() {
        let Some(item) = line.strip_prefix("- ") else {
            continue;
        };
        let mut words = item.split(' ');
        let word = words.next().unwrap_or_default();
        let prefix = word.chars().next().unwrap_or(' ');
        let number = word.get(1..).and_then(|digits| digits.parse::<u32>().ok());
        prop_assert!(
            matches!(prefix, 'c' | 'b') && number.is_some(),
            "an item without an id: {line:?}"
        );
        let token = words.next().unwrap_or_default().trim_end_matches('.');
        items.push(((prefix, number.unwrap()), token.to_owned()));
    }
    Ok(items)
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
    Close(String),
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
        self.lost_ack_recognised += other.lost_ack_recognised;
        self.late_landing_recognised += other.late_landing_recognised;
        self.kill_after_push_healed += other.kill_after_push_healed;
    }
}

struct Oracle<'w> {
    world: &'w World,
    tokens: u32,
    fates: BTreeMap<String, Fate>,
    /// Every id ever seen, with its token (I2).
    owners: BTreeMap<Id, String>,
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
            fates: BTreeMap::new(),
            owners: BTreeMap::new(),
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

    /// The items of the remote's `main`.
    fn remote(&self) -> Result<Vec<(Id, String)>, TestCaseError> {
        let mut all = Vec::new();
        for file in FILES {
            all.extend(items(&self.world.remote_file(file).unwrap_or_default())?);
        }
        Ok(all)
    }

    /// The command line for an op, on what the remote has now.
    fn args(&mut self, op: &Op) -> Result<(Vec<String>, Subject), TestCaseError> {
        let remote = self.remote()?;
        let pick = |prefix: char, index: &Index| -> String {
            let ids: Vec<u32> = remote
                .iter()
                .filter(|((p, _), _)| *p == prefix)
                .map(|((_, n), _)| *n)
                .collect();
            // Nothing to pick: an id that no item has, for a refusal.
            let number = if ids.is_empty() {
                999
            } else {
                *index.get(&ids)
            };
            format!("{prefix}{number}")
        };
        let owned = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect();
        Ok(match op {
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
                let id = pick('c', index);
                (owned(&["done", &id]), Subject::Close(id))
            }
            Op::Drop(index) => {
                let id = pick('b', index);
                (owned(&["drop", &id]), Subject::Close(id))
            }
        })
    }

    fn token(&mut self) -> String {
        self.tokens += 1;
        format!("t{}", self.tokens)
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        match step {
            Step::RejectPushes(pushes) => {
                self.world.reject_pushes(u32::from(*pushes));
                Ok(())
            }
            Step::Quiesce => self.quiesce(),
            Step::Cmd { who, op, fault } => self.command(*who, op, fault.as_ref()),
        }
    }

    fn command(&mut self, who: Who, op: &Op, fault: Option<&Fault>) -> Result<(), TestCaseError> {
        let machine = self.machine(who);
        let other = self.machine(who.other());
        let rejecting = self.world.rejects_left() > 0;
        let (args, subject) = self.args(op)?;
        let mut window = None;
        match fault {
            None => {}
            Some(Fault::WindowOp(window_op)) => {
                let (window_args, window_subject) = self.args(window_op)?;
                let words: Vec<&str> = window_args.iter().map(String::as_str).collect();
                machine.hook_once(
                    "pre-push",
                    &pre_push::window(&other.script(&words), self.world.dir()),
                );
                window = Some(window_subject);
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

        let remote = self.remote()?;
        self.observe(&remote)?;
        if let Some((window_subject, window_run)) = &window_run {
            self.judge(window_subject, window_run, rejecting, None, &remote)?;
            if window_run.code == Some(0) && run.code == Some(0) && run.stderr.contains("retry") {
                self.depth.raced_in_the_window += 1;
            }
        }
        self.judge(&subject, &run, rejecting, fault, &remote)?;

        // Depth: what this command went through.
        let slot = match who {
            Who::A => 0,
            Who::B => 1,
        };
        if run.code == Some(0) {
            if std::mem::take(&mut self.healing[slot]) {
                self.depth.kill_after_push_healed += 1;
            }
            if matches!(fault, Some(Fault::LostAck)) && !rejecting {
                self.depth.lost_ack_recognised += 1;
            }
            if matches!(fault, Some(Fault::LateLanding))
                && !rejecting
                && run.stderr.contains("retry 3/4")
            {
                self.depth.late_landing_recognised += 1;
            }
        }
        if run.code.is_none()
            && matches!(fault, Some(Fault::KillAfterPush))
            && matches!(&subject, Subject::Add(token) if self.fates.get(token) == Some(&Fate::Live))
        {
            self.healing[slot] = true;
        }
        self.check_everywhere(&remote)
    }

    /// Checks one run against the remote after the step, and records what
    /// became of its subject.
    fn judge(
        &mut self,
        subject: &Subject,
        run: &Run,
        rejecting: bool,
        fault: Option<&Fault>,
        remote: &[(Id, String)],
    ) -> Result<(), TestCaseError> {
        self.depth.commands += 1;
        match run.code {
            Some(0) => {
                self.depth.succeeded += 1;
                if run.stderr.contains("retry") {
                    self.depth.retried_then_succeeded += 1;
                }
                for line in run.stderr.lines() {
                    prop_assert!(line.starts_with("board: retry "), "{run:?}");
                }
            }
            Some(1) => self.depth.refused += 1,
            Some(3) => {
                self.depth.not_pushed += 1;
                prop_assert!(rejecting, "exit 3 with the remote taking pushes: {run:?}");
            }
            None => {}
            Some(_) => prop_assert!(false, "unexpected exit: {run:?}"),
        }
        match subject {
            Subject::Add(token) => {
                let lines: Vec<&Id> = remote
                    .iter()
                    .filter(|(_, t)| t == token)
                    .map(|(id, _)| id)
                    .collect();
                let fate = match run.code {
                    Some(0) => {
                        // The printed id is on the remote with this token.
                        let printed = run.stdout.trim_end();
                        let shown: Vec<String> =
                            lines.iter().map(|(p, n)| format!("{p}{n}")).collect();
                        prop_assert_eq!(&shown, &vec![printed.to_owned()], "{:?}", run);
                        Fate::Live
                    }
                    None if matches!(fault, Some(Fault::KillAfterPush)) && !lines.is_empty() => {
                        Fate::Live
                    }
                    Some(_) | None => Fate::Never,
                };
                self.fates.insert(token.clone(), fate);
            }
            Subject::Close(id) => {
                let key: Id = (id.as_bytes()[0] as char, id[1..].parse().unwrap());
                let present = remote.iter().any(|(other, _)| *other == key);
                match run.code {
                    Some(0) => {
                        let past = if key.0 == 'c' { "done" } else { "dropped" };
                        prop_assert_eq!(&run.stdout, &format!("{past} {id}\n"));
                    }
                    Some(1) => prop_assert!(
                        run.stderr.ends_with("no such chore\n")
                            || run.stderr.ends_with("no such backlog item\n"),
                        "{:?}",
                        run
                    ),
                    Some(_) | None => {}
                }
                if run.code == Some(0) || run.code == Some(1) {
                    prop_assert!(!present, "{id} is still on the remote after {run:?}");
                }
                if !present && let Some(token) = self.owners.get(&key) {
                    self.fates.insert(token.clone(), Fate::Closed);
                }
            }
        }
        Ok(())
    }

    /// I1 and I2 in a place.
    fn observe(&mut self, items: &[(Id, String)]) -> Result<(), TestCaseError> {
        for (id, token) in items {
            match self.owners.get(id) {
                Some(owner) => prop_assert_eq!(owner, token, "I2: {:?} carries two tokens", id),
                None => {
                    self.owners.insert(*id, token.clone());
                }
            }
        }
        Ok(())
    }

    fn check_place(
        &mut self,
        files: [Option<Vec<u8>>; 2],
        place: &str,
    ) -> Result<(), TestCaseError> {
        for bytes in files {
            let items = items(&bytes.unwrap_or_default())?;
            let mut ids = BTreeMap::new();
            let mut tokens = BTreeMap::new();
            for (id, token) in &items {
                prop_assert!(
                    ids.insert(*id, ()).is_none(),
                    "I1: {:?} twice in {}",
                    id,
                    place
                );
                prop_assert!(
                    tokens.insert(token.clone(), ()).is_none(),
                    "I1: {} twice in {}",
                    token,
                    place
                );
            }
            self.observe(&items)?;
        }
        Ok(())
    }

    fn check_everywhere(&mut self, remote: &[(Id, String)]) -> Result<(), TestCaseError> {
        for who in [Who::A, Who::B] {
            let machine = self.machine(who);
            self.check_place(FILES.map(|file| machine.file(file)), &format!("{who:?}"))?;
        }
        let world = self.world;
        self.check_place(FILES.map(|file| world.remote_file(file)), "the remote")?;
        for (token, fate) in &self.fates {
            let count = remote.iter().filter(|(_, t)| t == token).count();
            match fate {
                Fate::Live => prop_assert_eq!(count, 1, "{} is to be on the remote once", token),
                Fate::Never | Fate::Closed => {
                    prop_assert_eq!(count, 0, "{} ({:?}) is on the remote", token, fate);
                }
            }
        }
        Ok(())
    }

    /// The remote takes pushes again; `sync` on A, B and A brings both
    /// clones to the remote's commit.
    fn quiesce(&mut self) -> Result<(), TestCaseError> {
        self.world.reject_pushes(0);
        for who in [Who::A, Who::B, Who::A] {
            let machine = self.machine(who);
            machine.unhook("pre-push");
            let output = machine.command().arg("sync").output().expect("run board");
            prop_assert_eq!(
                (output.status.code(), &output.stdout[..], &output.stderr[..]),
                (Some(0), &b"synced\n"[..], &b""[..])
            );
            if std::mem::take(&mut self.healing[who as usize]) {
                self.depth.kill_after_push_healed += 1;
            }
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
            prop_assert_eq!(machine.status(), "", "{:?} has changed files", who);
        }
        let remote = self.remote()?;
        self.check_everywhere(&remote)
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
            ..config(12)
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
    assert!(depth.lost_ack_recognised > 0, "{depth:?}");
    assert!(depth.late_landing_recognised > 0, "{depth:?}");
    assert!(depth.kill_after_push_healed > 0, "{depth:?}");
    assert!(
        depth.succeeded * 10 >= depth.commands * 6,
        "fewer than 60% of the commands succeeded: {depth:?}"
    );
}
