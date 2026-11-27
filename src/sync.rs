//! The write loop: every command that changes the board runs through
//! [`write`], under the clone's lock, on what the remote has.
//!
//! ```text
//! lock     .git/board.lock
//! check    a clone on main with origin and a commit, not shallow;
//!          git's stale .git/index.lock and .git/board.index.lock removed
//! absorb   board files changed by hand -> one commit on main, `board: hand edits`
//! round    at most 8 (refereed, sync) or 2 (local first, the second at once):
//!            fetch; M = main, U = origin/main
//!            any failed T found in U -> the command landed; it is not run again
//!            X = integrate(M, U): U, M, or a merge commit of the two
//!            T = the command's commit on X (built in the object store), else X
//!            T == U -> done;  push T -> done, or the next round,
//!            after a random wait when the push lost a race
//! offline  no fetch, or the last round's push failed and the one more fetch
//!          finds none landed: X = integrate(M, U) and the command once
//!          more, on main only; allocating an id is refused (in a local-first
//!          write always)
//! advance  main fast-forwards to T; a file saved meanwhile is absorbed and
//!          merged in, at most 3 times
//! notes    commits not pushed, lint
//! ```
//!
//! A command is a closure over the project loaded from `X`, so a round can
//! run it again on fresh state: after a failed push the command's commit is
//! dropped and the command applied anew to what the remote has then. An id
//! is allocated on the fetched remote tip and printed only after the push
//! that publishes it, so no id is ever renumbered.
//!
//! `main` moves only by an absorb commit or a fast-forward, and every `T` has
//! `main` as an ancestor, so what the clone published is always in its
//! ancestry: after a kill between push and fast-forward the next write sees
//! "behind" and catches up, where rewritten commits would read as "diverged"
//! and merge the same hand edit a second time.

use std::cell::Cell;

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::Error;
use crate::git::{Failure, Git, Oid, TreeEntry};
use crate::id::{BacklogId, BriefId, ChoreId, ItemId, ListKind};
use crate::lint::{Finding, lint};
use crate::list::{Entry, IdsExhausted, parse_line};
use crate::merge::{LocalCopy, merge_trees};
use crate::project::{BoardPath, Project, ProjectName, copy_paths, is_old_list};
use crate::raw;

/// Pushes per refereed write and per sync. An agent's command has to end
/// rather than retry without limit, and an id not yet published may not be
/// printed; eight failed pushes are given up on even with a working fetch.
/// Eight, where there were four (owner, 2026-11-25): with the waits of
/// [`backoff`] the rounds are independent tries against a writer that keeps
/// pushing, and enough of them get one through; the waits add at most
/// 11.75 s to the rounds' own fetches and pushes.
const ROUNDS: u8 = 8;

/// Pushes per local-first write: the first, and one retry at once after it
/// fails (owner, 2026-11-27: "local first when the sync is low risk later
/// anyway and can be delayed"). With one file per item a commit left on
/// the clone merges cleanly later, so a lost race costs nothing but the
/// delay, and the next write that pushes carries it.
const LOCAL_ROUNDS: u8 = 2;

/// How a command's write meets the remote, stated where the command is
/// defined (`Board::refereed`, `Board::local_first`), not found out while
/// it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The remote decides: an id (`add`, `new brief`) or a claim (`next`)
    /// is good only once pushed, so it is refused when [`ROUNDS`] pushes
    /// fail.
    Refereed,
    /// The clone decides, the default: [`LOCAL_ROUNDS`] pushes, and when
    /// both fail the commit stays on the clone and the command succeeds.
    LocalFirst,
}

/// What a write runs.
///
/// The project goes with what needs it, the command's commit and the lint
/// notes, so a sync in the board-data clone itself, which has no project
/// (owner, 2026-11-27: "make board sync the one exception to the refusal
/// in the clone root"), cannot name one: no made-up name reaches a commit
/// or a note. Rejected: a second write function for that sync (the loop
/// twice).
pub(crate) enum Run<'a, T> {
    /// No command: the clone and the remote brought together, then `lint`
    /// linted; `None` in the clone itself. [`ROUNDS`] rounds, as a refereed
    /// write: it is what publishes the commits local-first writes left on
    /// the clone, so giving up after two would defeat it.
    Sync { lint: Option<&'a ProjectName> },
    /// A command of `project`'s.
    Command {
        project: &'a ProjectName,
        kind: Kind,
        apply: Apply<'a, T>,
    },
}

/// Merges of a file saved during the write before the fast-forward is given
/// up on. A save is a person's or an agent's, so a few are plenty; a loop
/// without end would hold the lock against every other write.
const SETTLES: u8 = 3;

/// What a write says along the way, on stderr, each as `board: <note>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// Another write on this clone holds the lock; this one waits for it.
    LockWait,
    /// A push failed; round `round` of the write's `of` starts over from a
    /// fetch, after a wait when the push lost a race.
    Retry { round: u8, of: u8 },
    /// The clone's own commits were merged with the remote's.
    Merged { commits: usize },
    /// The local version of a brief or an item lost to the remote's and
    /// was saved at `copy`.
    Conflict { of: ItemId, copy: PathBuf },
    /// Commits of the clone that are not on the remote: git's reason. A
    /// failure: the remote was not reached, or a refereed write or a sync
    /// gave up.
    NotPushed { commits: usize, reason: String },
    /// A local-first write's commits left on the clone with the remote
    /// reached: a normal end, they go out with the next write.
    NotPushedYet { commits: usize, reason: String },
    /// The change is on the remote but the files of the clone were not
    /// updated: git's reason.
    CheckoutBehind(String),
    /// A lint finding in the project, after the write.
    Lint(Finding),
}

fn commits(n: usize) -> &'static str {
    if n == 1 { "commit" } else { "commits" }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Note::LockWait => f.write_str("waiting for lock"),
            Note::Retry { round, of } => write!(f, "retry {round}/{of}"),
            Note::Merged { commits: n } => write!(f, "merged {n} local {}", commits(*n)),
            Note::Conflict { of, copy } => {
                match of {
                    ItemId::Brief(number) => write!(f, "conflict: brief {number}")?,
                    ItemId::Chore(_) | ItemId::Backlog(_) => write!(f, "conflict: {of}")?,
                }
                write!(
                    f,
                    " changed here and on the remote; \
                     the remote version is kept, yours is saved as {}",
                    copy.display()
                )
            }
            Note::NotPushed { commits: n, reason } => {
                write!(f, "not pushed: {n} {} ({reason})", commits(*n))
            }
            Note::NotPushedYet { commits: n, reason } => write!(
                f,
                "not pushed yet: {n} {}, they go out with the next write ({reason})",
                commits(*n)
            ),
            Note::CheckoutBehind(said) => write!(f, "local files not updated: {said}"),
            Note::Lint(finding) => write!(f, "lint: {finding}"),
        }
    }
}

/// Why a write did not reach the remote, with git's reason.
enum Offline {
    /// The fetch failed before any push.
    Unreached(Failure),
    /// The fetch failed after a push that failed: the push may have landed.
    UnreachedAfterPush(Failure),
    /// Every round's push failed with the remote reachable.
    Refused(Failure),
}

impl Offline {
    fn reason(&self) -> String {
        match self {
            Offline::Unreached(failure)
            | Offline::UnreachedAfterPush(failure)
            | Offline::Refused(failure) => failure.to_string(),
        }
    }

    /// Why the command needed the remote and did without, as `<what>: …`
    /// goes on.
    fn message(&self, what: &str) -> String {
        match self {
            Offline::Unreached(failure) => {
                format!("{what}: needs the remote, which was not reached ({failure})")
            }
            Offline::UnreachedAfterPush(failure) => format!(
                "{what}: the remote was not reached after a failed push ({failure}); \
                 the change may have landed"
            ),
            Offline::Refused(failure) => {
                format!("{what}: not pushed after {ROUNDS} attempts ({failure})")
            }
        }
    }
}

/// A command: its edit of the project, its value and the subject of its
/// commit, or a refusal. It may run more than once, once per round.
pub(crate) type Apply<'a, T> = &'a dyn Fn(&mut Tx) -> Result<(T, String), Error>;

/// One run of a command, on the project as the commit `base` has it.
pub(crate) struct Tx<'a> {
    pub(crate) project: Project,
    git: &'a Git,
    base: Oid,
    what: &'a str,
    /// Set when the run is on `main` only.
    offline: Option<&'a Offline>,
    kind: Kind,
}

impl Tx<'_> {
    pub(crate) fn alloc_chore(&mut self) -> Result<ChoreId, Error> {
        let number = self.alloc(ListKind::Chores)?;
        Ok(ChoreId::new(number).expect("a next id is past 0"))
    }

    pub(crate) fn alloc_backlog(&mut self) -> Result<BacklogId, Error> {
        let number = self.alloc(ListKind::Backlog)?;
        Ok(BacklogId::new(number).expect("a next id is past 0"))
    }

    /// The next brief number of the project: one past every number among
    /// its brief files and every one ever added under its `briefs/`.
    pub(crate) fn alloc_brief(&mut self) -> Result<BriefId, Error> {
        self.require_remote()?;
        let mark = brief_mark(self.git, &self.base, self.project.name())?;
        let highest = self
            .project
            .briefs()
            .iter()
            .map(|file| file.name.number().get())
            .chain(mark)
            .max();
        match highest {
            None => Ok(BriefId::new(1)),
            Some(highest) => highest.checked_add(1).map(BriefId::new).ok_or_else(|| {
                Error::Refused(format!(
                    "{}: brief {highest} has been used, the highest number there is; \
                     no number is left",
                    self.what
                ))
            }),
        }
    }

    /// Refuses a wait on `target` when no such id was ever given on the
    /// board: past the highest brief number or item id ever used, as
    /// history counts them for numbering. Here and not in lint: only a
    /// write reads history, under the lock. On the clone's history, so it
    /// works offline; an id given meanwhile on another machine is refused
    /// until the next fetch.
    /// `what` names the refusal, as `queue 5` or `after c1`.
    pub(crate) fn require_given(&self, what: &str, target: ItemId) -> Result<(), Error> {
        let name = self.project.name();
        let (number, highest) = match target {
            ItemId::Brief(number) => (
                number.get(),
                self.project
                    .briefs()
                    .iter()
                    .map(|file| file.name.number().get())
                    .chain(brief_mark(self.git, &self.base, name)?)
                    .max(),
            ),
            ItemId::Chore(_) | ItemId::Backlog(_) => {
                let kind = match target {
                    ItemId::Chore(_) => ListKind::Chores,
                    _ => ListKind::Backlog,
                };
                let number = match target {
                    ItemId::Chore(id) => id.get(),
                    ItemId::Backlog(id) => id.get(),
                    ItemId::Brief(_) => unreachable!("matched above"),
                };
                (
                    number,
                    [
                        self.project.list(kind).numbers().max(),
                        item_mark(self.git, &self.base, name, kind)?,
                        list_mark(self.git, &self.base, name, kind)?,
                    ]
                    .into_iter()
                    .flatten()
                    .max(),
                )
            }
        };
        if highest.is_some_and(|highest| number <= highest) {
            return Ok(());
        }
        Err(Error::Refused(format!(
            "{what}: {target} was never given on this board, so nothing can wait on it"
        )))
    }

    /// Refuses a command that has to act on what the remote has. Where an id
    /// is allocated this is checked for every command; a command that
    /// allocates none but claims something shared (`next`) asks for it.
    ///
    /// In a local-first write it is always refused: such a write ends
    /// unpushed after two rounds as a success, so an id or a claim from it
    /// could be printed and never published. Checked here, where every id
    /// is allocated, at run time rather than by the types: a second `Tx`
    /// type and `Apply` for refereed commands would double the write's
    /// signatures for four callers, and the test of each refereed command
    /// against a refusing remote fails at once if one is declared wrong.
    pub(crate) fn require_remote(&self) -> Result<(), Error> {
        if self.kind == Kind::LocalFirst {
            return Err(Error::Refused(format!(
                "{}: needs the remote, so its write must be refereed, not local first",
                self.what
            )));
        }
        match self.offline {
            Some(offline) => Err(Error::NeedsRemote(offline.message(self.what))),
            None => Ok(()),
        }
    }

    /// The next id of a list, on the fetched remote tip only: another clone
    /// may hand out the same id meanwhile, and it is the push that decides
    /// which one has it. Refused here rather than per command, so no command
    /// can forget.
    fn alloc(&mut self, kind: ListKind) -> Result<u32, Error> {
        self.require_remote()?;
        let name = self.project.name();
        let highest = [
            self.project.list(kind).numbers().max(),
            item_mark(self.git, &self.base, name, kind)?,
            list_mark(self.git, &self.base, name, kind)?,
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);
        highest
            .checked_add(1)
            .ok_or(IdsExhausted)
            .map_err(|IdsExhausted| {
                Error::Refused(format!(
                    "add {}: {}{} has been used, the highest id there is; no id is left",
                    match kind {
                        ListKind::Chores => "chore",
                        ListKind::Backlog => "backlog",
                    },
                    kind.prefix(),
                    u32::MAX
                ))
            })
    }
}

/// The highest id any item file ever added under the project's `chores/` or
/// `backlog/` had, in the history of `commit`; `None` when there was none.
///
/// History is the counter: one past this, past the files there now and
/// past [`list_mark`] is the next id. A deleted item would otherwise free
/// its id, and a `closes b41` or a note somewhere would then name a new
/// item. Rejected: a counter file, which a hand edit makes stale and which
/// says again what history says. A path is read by the one path grammar.
fn item_mark(
    git: &Git,
    commit: &Oid,
    project: &ProjectName,
    kind: ListKind,
) -> Result<Option<u32>, Error> {
    let dir = format!("{project}/{}", kind.dir_name());
    let added = git
        .added_files(commit, &dir)
        .map_err(|failure| Error::Git(format!("cannot read the history of {dir}: {failure}")))?;
    Ok(added
        .iter()
        .filter_map(|path| match BoardPath::parse(path)? {
            BoardPath::Item { id, .. } if id.kind() == kind => Some(id.get()),
            BoardPath::Item { .. } | BoardPath::Brief { .. } => None,
        })
        .max())
}

/// The highest id the kind's old list file (`chores.md`, `backlog.md`) ever
/// held, in the history of `commit`; `None` when it held none.
///
/// A board converted to item files (`migrate`) has the ids of its items done
/// before the conversion only there. An added or removed line is read by
/// the one list-line parser, so "the id of a line" has one definition.
fn list_mark(
    git: &Git,
    commit: &Oid,
    project: &ProjectName,
    kind: ListKind,
) -> Result<Option<u32>, Error> {
    let path = format!("{project}/{}", kind.file_name());
    let patches = git
        .file_history(commit, &path.to_string())
        .map_err(|failure| Error::Git(format!("cannot read the history of {path}: {failure}")))?;
    Ok(raw::lines(&patches)
        .filter_map(|line| {
            let content = raw::content(line);
            // `+- c12 …` and `-- c12 …`; a header (`--- a/…`) parses as text.
            let line = content
                .strip_prefix(b"+")
                .or_else(|| content.strip_prefix(b"-"))?;
            match parse_line(kind, line) {
                Entry::Item { id, .. } => id,
                Entry::Heading(_) | Entry::Text(_) => None,
            }
        })
        .max())
}

/// The highest brief number any file ever added under the project's
/// `briefs/` had, in the history of `commit`; `None` when there was none.
///
/// The same rule as item ids, for the same reason: a brief deleted by hand
/// would otherwise free its number, and `file` on the brief that gets it
/// next would count the old brief's `Brief:` commits. A path is read by the
/// one path grammar. Not followed across a rename of the project's
/// directory: a brief deleted before such a rename frees its number.
fn brief_mark(git: &Git, commit: &Oid, project: &ProjectName) -> Result<Option<u32>, Error> {
    let dir = format!("{project}/briefs");
    let added = git
        .added_files(commit, &dir)
        .map_err(|failure| Error::Git(format!("cannot read the history of {dir}: {failure}")))?;
    Ok(added
        .iter()
        .filter_map(|path| match BoardPath::parse(path)? {
            BoardPath::Brief { name, .. } => Some(name.number().get()),
            BoardPath::Item { .. } => None,
        })
        .max())
}

/// What a write works with.
struct Ctx<'a> {
    root: &'a Path,
    git: &'a Git,
    what: &'a str,
    notes: &'a dyn Fn(Note),
    /// Whether the write said it merged: once, however many rounds merge
    /// the same local commits again.
    merge_noted: Cell<bool>,
    /// This write's mark in its command's commit: see [`run_mark`].
    run: String,
    kind: Kind,
}

/// A mark no other write has, for the message of the command's commit.
///
/// A commit is its parent, tree, identity, second and message, so two
/// clones of one user running the same command on the same remote tip in
/// the same second build the same commit. The landed check would then take
/// the other clone's push for its own, and both would print its result:
/// two workers told `next` gave each of them the same brief. The mark makes
/// "my commit is on the remote" mean this write's. Rejected: a committer per
/// clone, which is configuration every clone would have to get right.
fn run_mark() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    // Keyed from the operating system's randomness at every process start.
    let random = RandomState::new().hash_one((nanos, std::process::id()));
    format!("{random:016x}")
}

/// One round's commit and what it holds.
struct Built<T> {
    tip: Oid,
    /// The command's result; `None` without a command.
    outcome: Option<Result<T, Error>>,
}

/// Runs a write: its command once per round until its commit is pushed,
/// or no command (`sync`). Gives the value of the run that was pushed, or
/// its refusal; for a local-first write not pushed, the value of its run
/// on the clone. `what` names the command in an error.
pub(crate) fn write<T>(
    root: &Path,
    what: &str,
    notes: &dyn Fn(Note),
    run: Run<'_, T>,
) -> Result<Option<T>, Error> {
    write_waiting(root, what, notes, run, &std::thread::sleep)
}

/// The wait after a write's `race`th lost race (1 to 7, whatever round it
/// fell on), before its retry: a random part of a ceiling that doubles per
/// lost race, `random` spread over it.
///
/// A retry made the instant a push is refused can fall in step with a
/// writer that pushes on a steady cycle: when a round takes about as long as
/// that cycle, every retry starts at the same losing point of it, and every
/// round is lost, not most (the case measured: one session's 107 writes in
/// eight minutes). A random wait makes each round an independent try. The
/// first lost race is retried at once (owner, 2026-11-25): one collision is
/// the common case and is best retried at once, and the retries after it
/// start at a random point, which is what breaks the step. Counted in lost
/// races, not rounds: a round lost otherwise (a declined push, a lost
/// answer) says nothing about a writer in step, so a first collision after
/// one is still retried at once. Rejected: a fixed wait (two
/// starved writers would stay in step with each other); a time budget
/// instead of a count (the rounds bound it already); a wait after a failed
/// fetch or a declined push (neither is a lost race, and a wait helps
/// nothing there).
fn backoff(race: u8, random: u64) -> Duration {
    // Ceilings after lost race 1, 2, … in milliseconds: none, then 0.25 s
    // doubling, then flat.
    const CEILINGS: [u64; 7] = [0, 250, 500, 1000, 2000, 4000, 4000];
    let ceiling = CEILINGS[usize::from(race.saturating_sub(1)).min(CEILINGS.len() - 1)];
    Duration::from_millis(random % (ceiling + 1))
}

/// A random value for [`backoff`], fresh per call.
fn random() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    RandomState::new().hash_one((nanos, std::process::id()))
}

/// [`write`] with the wait of a retry after a lost race given: a test
/// records it instead of sleeping. The clone's lock stays held across the
/// wait, as a round assumes it holds `main`: other writes on this machine
/// queue behind it for as long, at most the waits' 11.75 s in all, the
/// accepted cost.
fn write_waiting<T>(
    root: &Path,
    what: &str,
    notes: &dyn Fn(Note),
    run: Run<'_, T>,
    wait: &dyn Fn(Duration),
) -> Result<Option<T>, Error> {
    let (kind, apply, lint_of) = match run {
        Run::Sync { lint } => (Kind::Refereed, None, lint),
        Run::Command {
            project,
            kind,
            apply,
        } => (kind, Some((project, apply)), Some(project)),
    };
    let rounds = match kind {
        Kind::Refereed => ROUNDS,
        Kind::LocalFirst => LOCAL_ROUNDS,
    };
    let git = Git::at(root);
    let _lock = lock(root, notes)?;
    check(root, &git)?;
    // git's locks on the clone's index and the tool's, left by a git killed
    // with the command that ran it: every git that takes them in this clone
    // runs under the clone's lock, which is this write's now, so a lock file
    // found now is stale. Left there, it fails every later write.
    for stale in ["index.lock", "board.index.lock"] {
        remove_if_there(&root.join(".git").join(stale))?;
    }
    absorb(&git)?;
    let ctx = Ctx {
        root,
        git: &git,
        what,
        notes,
        merge_noted: Cell::new(false),
        run: run_mark(),
        kind,
    };

    let is_sync = apply.is_none();
    let mut apply = apply;
    // Every round whose push failed: each may have reached the remote all
    // the same.
    let mut failed: Vec<Built<T>> = Vec::new();
    // Lost races so far, for the wait of the next: see [`backoff`].
    let mut races = 0;
    let mut last_refusal = None;
    let mut offline = None;
    let mut pushed = None;
    for round in 1..=rounds {
        if let Err(failure) = git.fetch() {
            offline = Some(if failed.is_empty() {
                Offline::Unreached(failure)
            } else {
                Offline::UnreachedAfterPush(failure)
            });
            break;
        }
        let (main, upstream) = tips(&git)?;
        // A push can reach the remote and still report failure (a lost
        // answer). Running the command again would then apply it twice.
        let landed = landed(&git, &mut failed, &upstream)?;
        if landed.is_some() {
            apply = None;
        }
        let mut built = build(&ctx, &main, &upstream, None, apply)?;
        if let Some(landed) = landed {
            // This round only catches up with what came after the landed
            // push; the command's result and ids are that push's.
            built.outcome = landed.outcome;
        }
        if built.tip == upstream {
            pushed = Some(built);
            break;
        }
        match git.push(&built.tip) {
            Ok(()) => {
                pushed = Some(built);
                break;
            }
            Err(failure) => {
                failed.push(built);
                if round < rounds {
                    notes(Note::Retry {
                        round: round + 1,
                        of: rounds,
                    });
                    if failure.lost_race() {
                        races += 1;
                        wait(backoff(races, random()));
                    }
                }
                last_refusal = Some(failure);
            }
        }
    }
    if pushed.is_none() && offline.is_none() {
        // The last push may have landed too: one more fetch to know.
        match git.fetch() {
            Err(failure) => offline = Some(Offline::UnreachedAfterPush(failure)),
            Ok(()) => {
                let (_, upstream) = tips(&git)?;
                match landed(&git, &mut failed, &upstream)? {
                    Some(mut run) => {
                        run.tip = upstream;
                        pushed = Some(run);
                    }
                    None => {
                        offline = Some(Offline::Refused(
                            last_refusal.expect("a failed push says why"),
                        ));
                    }
                }
            }
        }
    }
    let built = match pushed {
        Some(built) => built,
        // The remote is out of reach: the command runs once more, on the
        // clone's own commits and what was last fetched, and its commit
        // stays on `main` for the next write to push. A write's last
        // refused push counts the same: an agent's command has to end.
        None => {
            let offline = offline.as_ref().expect("a write not pushed is offline");
            let main = head(&git)?;
            let upstream = git
                .commit("refs/remotes/origin/main")
                .map_err(git_error)?
                .unwrap_or_else(|| main.clone());
            build(&ctx, &main, &upstream, Some(offline), apply)?
        }
    };

    advance(&ctx, built.tip, offline.is_none())?;
    if let Some(offline) = &offline
        && let Some(upstream) = git.commit("refs/remotes/origin/main").map_err(git_error)?
    {
        let commits = git
            .count_between(&upstream, &head(&git)?)
            .map_err(git_error)?;
        if commits > 0 {
            let reason = offline.reason();
            notes(match (kind, offline) {
                (Kind::LocalFirst, Offline::Refused(_)) => Note::NotPushedYet { commits, reason },
                _ => Note::NotPushed { commits, reason },
            });
        }
    }
    // The write is done whatever lint finds, and a project that cannot be
    // read now was read a moment ago; lint is a note, never the result.
    if let Some(project) = lint_of
        && let Ok(project) = Project::load(root, project)
    {
        for finding in lint(&project) {
            notes(Note::Lint(finding));
        }
    }
    match offline {
        Some(offline) if is_sync => Err(Error::NeedsRemote(offline.message(what))),
        Some(_) | None => built.outcome.transpose(),
    }
}

/// Waits for the clone's lock, saying so first when another write holds it.
///
/// `flock`: the kernel frees it when the holder dies, so a killed command
/// leaves no lock behind. git children do not inherit it (Rust opens files
/// close-on-exec), so a push left running by a killed command is like a
/// push from another machine, and a hook that starts `board` cannot
/// deadlock on its parent's lock.
fn lock(root: &Path, notes: &dyn Fn(Note)) -> Result<File, Error> {
    if let Some(file) = try_lock(root)? {
        return Ok(file);
    }
    notes(Note::LockWait);
    let (path, file) = lock_file(root)?;
    file.lock()
        .map_err(|source| Error::io("lock", &path, source))?;
    Ok(file)
}

/// The clone's lock when no write holds it, else `None`, without waiting.
fn try_lock(root: &Path) -> Result<Option<File>, Error> {
    let (path, file) = lock_file(root)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(source)) => Err(Error::io("lock", &path, source)),
    }
}

fn lock_file(root: &Path) -> Result<(PathBuf, File), Error> {
    let path = root.join(".git").join("board.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => not_a_clone(root, "it has no .git directory"),
            _ => Error::io("open", &path, source),
        })?;
    Ok((path, file))
}

/// What the TUI's background fetch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// A write held the clone's lock, so nothing ran.
    Busy,
    Ran {
        /// Why the fetch failed: git's reason, or why the lock could not be
        /// taken.
        fetched: Result<(), String>,
        /// `main` against `origin/main` as last fetched; `None` when either
        /// is missing or git could not count.
        counts: Option<Counts>,
    },
}

/// The commits of the clone that are not on the remote, and the other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub unpushed: usize,
    pub behind: usize,
}

/// Fetches `origin`'s `main` and counts, under the clone's lock; skipped
/// when a write holds it.
///
/// Under the lock, so every git process in the clone runs under it, as a
/// write's do. Skipped rather than waited for: the write fetches itself, and
/// a fetch queued behind it would hold up the next write. Only a fetch: a
/// commit or a push on a timer would publish a hand edit half made, and the
/// owner wants commits on meaningful transitions only (SPEC.md, Sync).
pub(crate) fn fetch_in_background(root: &Path) -> Fetched {
    let lock = match try_lock(root) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Fetched::Busy,
        Err(error) => {
            return Fetched::Ran {
                fetched: Err(error.to_string()),
                counts: None,
            };
        }
    };
    let git = Git::at(root);
    // Held by git too: the TUI does not wait for this fetch when it quits.
    let fetched = git
        .fetch_holding(&lock)
        .map_err(|failure| failure.to_string());
    // Counted after a failed fetch too: what is not pushed matters most
    // when the remote is out of reach.
    let counts = || -> Result<Option<Counts>, Failure> {
        let (Some(main), Some(upstream)) = (
            git.commit("refs/heads/main")?,
            git.commit("refs/remotes/origin/main")?,
        ) else {
            return Ok(None);
        };
        Ok(Some(Counts {
            unpushed: git.count_between(&upstream, &main)?,
            behind: git.count_between(&main, &upstream)?,
        }))
    };
    Fetched::Ran {
        fetched,
        counts: counts().ok().flatten(),
    }
}

fn not_a_clone(root: &Path, why: &str) -> Error {
    Error::Setup(format!(
        "{root} is not a board-data clone the board can write to: {why}; \
         clone the board-data repo there once (git clone <url> {root})",
        root = root.display()
    ))
}

/// The clone the loop works on: on `main`, with `origin`, a commit and its
/// whole history (history is what the next id is counted from, and a
/// shallow clone would hand out ids again).
fn check(root: &Path, git: &Git) -> Result<(), Error> {
    if git.head_ref().map_err(git_error)?.as_deref() != Some("refs/heads/main") {
        return Err(not_a_clone(root, "HEAD is not on main"));
    }
    if !git.has_origin().map_err(git_error)? {
        return Err(not_a_clone(root, "it has no remote origin"));
    }
    if git.commit("refs/heads/main").map_err(git_error)?.is_none() {
        return Err(not_a_clone(root, "main has no commit"));
    }
    if root.join(".git").join("shallow").exists() {
        return Err(not_a_clone(
            root,
            "it is a shallow clone, and the next id is counted from the whole history",
        ));
    }
    Ok(())
}

/// Commits the board files changed by hand, as one commit on `main`.
///
/// Every write does this, not only `sync`: a command applies to a commit
/// merged with the remote's, so an edit left in the files would take no part
/// in it, and the fast-forward to the result could not bring that file up
/// to date. Only the board's paths (the one grammar decides): a conflict
/// copy, a note or a misnamed brief stays out, and nothing else that is
/// staged is committed with them. And a list file of the older storage:
/// `migrate` converts the commit, so a list fixed by hand after a refusal
/// has to be in it.
fn absorb(git: &Git) -> Result<(), Error> {
    let changed: Vec<String> = git
        .changed_paths()
        .map_err(git_error)?
        .into_iter()
        .filter(|path| BoardPath::parse(path).is_some() || is_old_list(path))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    git.commit_paths(&changed, "board: hand edits")
        .map_err(git_error)
}

fn head(git: &Git) -> Result<Oid, Error> {
    git.commit("refs/heads/main")
        .map_err(git_error)?
        .ok_or_else(|| Error::Git("main has no commit".to_owned()))
}

/// `main` and the fetched `origin/main`.
fn tips(git: &Git) -> Result<(Oid, Oid), Error> {
    let main = head(git)?;
    let upstream = git
        .commit("refs/remotes/origin/main")
        .map_err(git_error)?
        .ok_or_else(|| Error::Git("origin/main has no commit after the fetch".to_owned()))?;
    Ok((main, upstream))
}

/// The round whose failed push is in `upstream` after all, taken out of
/// `failed`. Every failed `T` is asked, not only the last: a push given up
/// on can still finish on the remote after the next round's fetch, and that
/// round's own `T` is then refused. At most one can land: each is pushed as
/// a fast-forward, and none is built on another.
fn landed<T>(
    git: &Git,
    failed: &mut Vec<Built<T>>,
    upstream: &Oid,
) -> Result<Option<Built<T>>, Error> {
    for at in 0..failed.len() {
        if git
            .is_ancestor(&failed[at].tip, upstream)
            .map_err(git_error)?
        {
            return Ok(Some(failed.swap_remove(at)));
        }
    }
    Ok(None)
}

/// One round's commit on `main` and `upstream`: integrated, with the
/// command applied.
fn build<T>(
    ctx: &Ctx<'_>,
    main: &Oid,
    upstream: &Oid,
    offline: Option<&Offline>,
    apply: Option<(&ProjectName, Apply<'_, T>)>,
) -> Result<Built<T>, Error> {
    let base = integrate(ctx, main, upstream)?;
    let (tip, outcome) = match apply {
        None => (base, None),
        Some((project, apply)) => {
            let mut tx = Tx {
                project: Project::load_commit(ctx.root, project, &base)?,
                git: ctx.git,
                base: base.clone(),
                what: ctx.what,
                offline,
                kind: ctx.kind,
            };
            match apply(&mut tx) {
                // A command that changed nothing (`next` on an empty queue)
                // makes no commit: an empty one would be pushed and logged
                // for every look at the queue.
                Ok((value, _)) if !tx.project.has_changes() => (base, Some(Ok(value))),
                Ok((value, subject)) => (
                    commit(
                        ctx.root,
                        ctx.git,
                        &base,
                        &tx.project,
                        &format!("{project}: {subject}\n\nBoard-Run: {}", ctx.run),
                    )?,
                    Some(Ok(value)),
                ),
                Err(refusal @ (Error::Refused(_) | Error::NeedsRemote(_))) => {
                    (base, Some(Err(refusal)))
                }
                Err(error) => return Err(error),
            }
        }
    };
    Ok(Built { tip, outcome })
}

/// The commit that has both `main` and `upstream`: one of the two when it
/// already has the other, else a merge commit of them, `upstream` first.
///
/// A merge commit and no rebase: the clone's commits stay as they are, so a
/// clone killed after its push still finds them in the remote's ancestry and
/// does not merge them again. The cost is merge commits in history where
/// both sides changed. See `merge.rs` for what is merged how.
fn integrate(ctx: &Ctx<'_>, main: &Oid, upstream: &Oid) -> Result<Oid, Error> {
    let git = ctx.git;
    if git.is_ancestor(main, upstream).map_err(git_error)? {
        return Ok(upstream.clone());
    }
    if git.is_ancestor(upstream, main).map_err(git_error)? {
        return Ok(main.clone());
    }
    let base_commit = git.merge_base(main, upstream).map_err(git_error)?;
    let tree = |commit: &Oid| git.tree(commit).map_err(git_error);
    let ours = tree(upstream)?;
    let merge = merge_trees(&tree(&base_commit)?, &ours, &tree(main)?);
    let merged = merge.tree;
    let mut changes: Vec<(String, Option<TreeEntry>)> = ours
        .keys()
        .filter(|path| !merged.contains_key(*path))
        .map(|path| (path.clone(), None))
        .collect();
    changes.extend(
        merged
            .iter()
            .filter(|(path, entry)| ours.get(*path) != Some(entry))
            .map(|(path, entry)| (path.clone(), Some(entry.clone()))),
    );
    let tree = write_tree(ctx.root, git, upstream, &changes)?;
    let commit = git
        .commit_tree(&tree, &[upstream, main], "board: merge")
        .map_err(git_error)?;
    if !ctx.merge_noted.replace(true) {
        (ctx.notes)(Note::Merged {
            commits: git.count_between(upstream, main).map_err(git_error)?,
        });
    }
    // Before any push of the merge: once it is on the remote, nothing but
    // this copy holds the local version where anyone looks, and a kill
    // right after the push would leave none.
    save_copies(ctx, &merge.copies)?;
    Ok(commit)
}

/// A command's commit: `base` with the project's changed files, built in
/// an index of the tool's own. The clone's index and files are not
/// touched, so a file saved by hand meanwhile stays as it is, and a commit
/// that is never pushed leaves nothing but an unreferenced object.
fn commit(
    root: &Path,
    git: &Git,
    base: &Oid,
    project: &Project,
    message: &str,
) -> Result<Oid, Error> {
    let files = project
        .changed_paths()
        .iter()
        .map(|path| {
            let entry = match project.bytes(path) {
                Some(bytes) => Some(TreeEntry::file(git.hash_object(&bytes).map_err(git_error)?)),
                None => None,
            };
            Ok((path.to_string(), entry))
        })
        .chain(
            // The list files `migrate` converted: deleted in the same commit
            // that adds their items.
            project
                .old_lists
                .iter()
                .filter(|list| list.converted)
                .map(|list| Ok((list.path.clone(), None))),
        )
        .collect::<Result<Vec<_>, Error>>()?;
    let tree = write_tree(root, git, base, &files)?;
    git.commit_tree(&tree, &[base], message).map_err(git_error)
}

/// The tree of `base` with `changes`, built in the tool's own index.
fn write_tree(
    root: &Path,
    git: &Git,
    base: &Oid,
    changes: &[(String, Option<TreeEntry>)],
) -> Result<Oid, Error> {
    let index = root.join(".git").join("board.index");
    git.read_tree(&index, base).map_err(git_error)?;
    git.update_index(&index, changes).map_err(git_error)?;
    let tree = git.write_tree(&index).map_err(git_error)?;
    remove_if_there(&index)?;
    Ok(tree)
}

fn remove_if_there(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::io("remove", path, source)),
    }
}

/// Saves the local version of every brief and item that lost a merge beside the
/// remote's: `<path>.local`, `.local2`, … A copy with the same bytes that
/// is there already (a round that merges again) is enough.
fn save_copies(ctx: &Ctx<'_>, copies: &[LocalCopy<TreeEntry>]) -> Result<(), Error> {
    for copy in copies {
        let bytes = ctx
            .git
            .blobs(std::slice::from_ref(&copy.blob.id))
            .map_err(git_error)?
            .remove(0);
        for candidate in copy_paths(&copy.path) {
            let file = ctx.root.join(&candidate);
            match fs::read(&file) {
                Ok(existing) if existing == bytes => break,
                Ok(_) => {}
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    if let Some(dir) = file.parent() {
                        fs::create_dir_all(dir)
                            .map_err(|source| Error::io("create", dir, source))?;
                    }
                    fs::write(&file, &bytes).map_err(|source| Error::io("write", &file, source))?;
                    (ctx.notes)(Note::Conflict {
                        of: copy.of,
                        copy: file,
                    });
                    break;
                }
                Err(source) => return Err(Error::io("read", &file, source)),
            }
        }
    }
    Ok(())
}

/// Moves `main` and the files to `tip`, which has `main` as an ancestor.
///
/// git refuses rather than overwrite a file saved since the absorb. That
/// save is then absorbed and merged with `tip`, from the exact base (`main`),
/// so it is neither lost nor reverted, and the fast-forward is tried again.
/// Rejected: `reset --hard` or `checkout -f`, which throw the save away.
/// Still refused (a file outside the board that `tip` changes), a pushed
/// `tip` is a note and the next write brings the files up; an unpushed one
/// is only in the object store, so the command is not applied.
fn advance(ctx: &Ctx<'_>, tip: Oid, pushed: bool) -> Result<(), Error> {
    let git = ctx.git;
    let mut tip = tip;
    for settle in 0..=SETTLES {
        if head(git)? == tip {
            return Ok(());
        }
        // git does not refuse for a file deleted since the absorb: it takes
        // it for one left alone and writes `tip`'s version back, so the
        // delete would be lost. Such a delete is absorbed and merged first.
        // A delete in the instant between this look and the fast-forward is
        // still lost.
        if settle < SETTLES && deleted_since_absorb(ctx)? {
            absorb(git)?;
            tip = integrate(ctx, &head(git)?, &tip)?;
            continue;
        }
        let Err(failure) = git.fast_forward(&tip) else {
            return Ok(());
        };
        if settle == SETTLES {
            if pushed {
                (ctx.notes)(Note::CheckoutBehind(failure.to_string()));
                return Ok(());
            }
            return Err(Error::Git(format!(
                "{}: the files of {} could not be updated ({failure}); the command was not applied",
                ctx.what,
                ctx.root.display()
            )));
        }
        absorb(git)?;
        tip = integrate(ctx, &head(git)?, &tip)?;
    }
    unreachable!("the last settle returns")
}

/// Whether a board file that `main` holds is gone from the work tree.
fn deleted_since_absorb(ctx: &Ctx<'_>) -> Result<bool, Error> {
    Ok(ctx
        .git
        .changed_paths()
        .map_err(git_error)?
        .iter()
        .any(|path| {
            (BoardPath::parse(path).is_some() || is_old_list(path)) && !ctx.root.join(path).exists()
        }))
}

fn git_error(failure: Failure) -> Error {
    Error::Git(format!("git in the board-data clone: {failure}"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::process::Command;

    use super::*;

    /// The ceilings after lost races 1 to 7, whatever their rounds (owner,
    /// 2026-11-25): the first retried at once, then 0.25 s doubling to 4 s.
    const CEILINGS: [(u8, u64); 7] = [
        (1, 0),
        (2, 250),
        (3, 500),
        (4, 1000),
        (5, 2000),
        (6, 4000),
        (7, 4000),
    ];

    #[test]
    fn the_wait_is_random_within_a_ceiling_that_doubles_per_lost_race() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for (race, ceiling) in CEILINGS {
            let ceiling = Duration::from_millis(ceiling);
            // The ceiling itself is reached: it is the ceiling, not below.
            assert_eq!(backoff(race, ceiling.as_millis() as u64), ceiling);
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..200 {
                // xorshift: many values, the same on every run.
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let wait = backoff(race, state);
                assert!(wait <= ceiling, "race {race}: {wait:?}");
                seen.insert(wait);
            }
            assert_eq!(seen.len() > 1, !ceiling.is_zero(), "race {race}: {seen:?}");
        }
        assert_eq!(ROUNDS, 8);
    }

    /// A remote and a clone of it in a temp dir, for [`write_waiting`].
    ///
    /// The library's git inherits this process's environment, so the
    /// developer's global configuration reaches it; the clone's own
    /// configuration fixes what a write here depends on (the identity of its
    /// commits, where its hooks are).
    struct Clone {
        dir: tempfile::TempDir,
    }

    impl Clone {
        fn new() -> Clone {
            let clone = Clone {
                dir: tempfile::tempdir().unwrap(),
            };
            let dir = clone.dir.path();
            git(dir, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
            git(dir, &["init", "-q", "-b", "main", "data"]);
            let data = clone.data();
            let hooks = data.join(".git").join("hooks");
            for (key, value) in [
                ("user.name", "t"),
                ("user.email", "t@example.org"),
                ("core.hooksPath", hooks.to_str().unwrap()),
            ] {
                git(&data, &["config", key, value]);
            }
            git(&data, &["remote", "add", "origin", "../remote.git"]);
            git(&data, &["commit", "-q", "--allow-empty", "-m", "start"]);
            git(&data, &["push", "-q", "-u", "origin", "main"]);
            clone
        }

        fn data(&self) -> PathBuf {
            self.dir.path().join("data")
        }

        /// Every push of the clone's loses a race, `races` times: its
        /// `pre-push` hook first puts a commit of its own on the remote's
        /// `main`, on what the clone fetched.
        fn lose_races(&self, races: usize) {
            self.pushes(&vec!["race"; races]);
        }

        /// What the clone's pushes meet, one word per push, then nothing:
        /// `race`, a commit of the hook's own put on the remote's `main`
        /// first, on what the clone fetched; `fail`, the push failed before
        /// it leaves.
        fn pushes(&self, words: &[&str]) {
            let plan = self.dir.path().join("pushes");
            fs::write(
                &plan,
                words.iter().map(|w| format!("{w}\n")).collect::<String>(),
            )
            .unwrap();
            hook(
                &self.data().join(".git/hooks/pre-push"),
                &format!(
                    "plan={plan}\n\
                     word=$(head -n 1 \"$plan\")\n\
                     tail -n +2 \"$plan\" >\"$plan.next\"\n\
                     mv \"$plan.next\" \"$plan\"\n\
                     case $word in\n\
                     fail) exit 1 ;;\n\
                     race)\n\
                     \ttip=$(git rev-parse refs/remotes/origin/main)\n\
                     \tnew=$(git commit-tree -p \"$tip\" -m race \"$tip^{{tree}}\")\n\
                     \tgit push -q --no-verify origin \"$new:refs/heads/main\" >/dev/null 2>&1 ;;\n\
                     esac\n",
                    plan = plan.display()
                ),
            );
        }

        /// A `sync` of a hand edit, with the waits it asked for.
        fn sync(&self, line: &str) -> (Result<(), Error>, Vec<Duration>, Vec<Note>) {
            let data = self.data();
            fs::create_dir_all(data.join("shop/backlog")).unwrap();
            fs::write(
                data.join("shop/backlog/b1.md"),
                format!("id: b1\ngroup: Default\ncreated: x\n\n# {line}\n"),
            )
            .unwrap();
            let waits = RefCell::new(Vec::new());
            let notes = RefCell::new(Vec::new());
            let shop = ProjectName::parse("shop").unwrap();
            let ran = write_waiting::<()>(
                &data,
                "sync",
                &|note| notes.borrow_mut().push(note),
                Run::Sync { lint: Some(&shop) },
                &|wait| waits.borrow_mut().push(wait),
            );
            (ran.map(drop), waits.into_inner(), notes.into_inner())
        }

        fn remote_has(&self, line: &str) -> bool {
            let out = git(
                &self.dir.path().join("remote.git"),
                &["show", "main:shop/backlog/b1.md"],
            );
            out.lines().any(|l| l.ends_with(line))
        }
    }

    /// Setup git, away from the developer's configuration.
    fn git(dir: &Path, args: &[&str]) -> String {
        #[expect(
            clippy::disallowed_methods,
            reason = "a test's setup, not the product's git"
        )]
        let output = Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn hook(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn retries(notes: &[Note]) -> Vec<u8> {
        notes
            .iter()
            .filter_map(|note| match note {
                Note::Retry { round, .. } => Some(*round),
                _ => None,
            })
            .collect()
    }

    /// Each wait within the ceiling of its lost race, in order from the
    /// first.
    fn assert_within_ceilings(waits: &[Duration]) {
        for (wait, (race, ceiling)) in waits.iter().zip(CEILINGS) {
            assert!(
                *wait <= Duration::from_millis(ceiling),
                "race {race}: {wait:?}"
            );
        }
    }

    /// The owner's case: a writer that keeps losing gets through on a later
    /// round. Five lost races, then the sixth push lands; with four rounds
    /// it would have given up.
    #[test]
    fn a_write_that_loses_five_races_gets_through() {
        let clone = Clone::new();
        clone.lose_races(5);
        let (ran, waits, notes) = clone.sync("b1 one");
        assert!(ran.is_ok(), "{ran:?} {notes:?}");
        assert!(clone.remote_has("b1 one"));
        assert_eq!(retries(&notes), [2, 3, 4, 5, 6]);
        assert_eq!(waits.len(), 5, "{waits:?}");
        assert_within_ceilings(&waits);
    }

    /// Eight lost races: a wait before each of the seven retries, in order,
    /// the first of them zero; then the write is given up on.
    #[test]
    fn eight_lost_races_wait_before_every_retry_and_give_up() {
        let clone = Clone::new();
        clone.lose_races(8);
        let (ran, waits, notes) = clone.sync("b1 one");
        assert!(
            matches!(&ran, Err(Error::NeedsRemote(why)) if why.contains("not pushed after 8 attempts")),
            "{ran:?}"
        );
        assert_eq!(retries(&notes), [2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(waits.len(), 7, "{waits:?}");
        assert_eq!(waits[0], Duration::ZERO);
        assert_within_ceilings(&waits);
    }

    /// The wait goes by the write's lost races, not its rounds: a first
    /// lost race after a failed push is still retried at once, the second
    /// waits.
    #[test]
    fn the_first_lost_race_is_retried_at_once_whatever_the_round() {
        let clone = Clone::new();
        clone.pushes(&["fail", "fail", "race", "race"]);
        let (ran, waits, notes) = clone.sync("b1 one");
        assert!(ran.is_ok(), "{ran:?} {notes:?}");
        assert_eq!(retries(&notes), [2, 3, 4, 5]);
        assert_eq!(waits.len(), 2, "{waits:?}");
        assert_eq!(waits[0], Duration::ZERO);
        assert_within_ceilings(&waits);
    }

    /// An id or a claim in a local-first write is refused before anything
    /// is committed, however the remote answers: such a write could end on
    /// the clone and print an id nobody else knows of.
    #[test]
    fn a_local_first_write_refuses_an_id_and_the_remote_s_check() {
        let clone = Clone::new();
        let notes = RefCell::new(Vec::new());
        type Asks = fn(&mut Tx) -> Result<(), Error>;
        let asks: [(&str, Asks); 4] = [
            ("add chore", |tx| tx.alloc_chore().map(drop)),
            ("add backlog", |tx| tx.alloc_backlog().map(drop)),
            ("new brief", |tx| tx.alloc_brief().map(drop)),
            ("next", |tx| tx.require_remote()),
        ];
        for (what, ask) in asks {
            let apply = |tx: &mut Tx| ask(tx).map(|()| ((), "x".to_owned()));
            let ran = write_waiting(
                &clone.data(),
                what,
                &|note| notes.borrow_mut().push(note),
                Run::Command {
                    project: &ProjectName::parse("shop").unwrap(),
                    kind: Kind::LocalFirst,
                    apply: &apply,
                },
                &|_| (),
            );
            assert!(
                matches!(&ran, Err(Error::Refused(why)) if why.contains("must be refereed")),
                "{what}: {ran:?}"
            );
        }
        assert_eq!(git(&clone.data(), &["log", "--format=%s"]), "start\n");
    }

    /// No race lost, no wait: a remote whose hook declines every push is
    /// retried at once, and a remote out of reach goes offline at once.
    #[test]
    fn a_declined_push_and_a_failed_fetch_ask_for_no_wait() {
        let clone = Clone::new();
        hook(
            &clone.dir.path().join("remote.git/hooks/pre-receive"),
            "exit 1\n",
        );
        let (ran, waits, notes) = clone.sync("b1 one");
        assert!(
            matches!(&ran, Err(Error::NeedsRemote(why)) if why.contains("hook declined")),
            "{ran:?}"
        );
        assert_eq!(retries(&notes), [2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(waits, []);

        let clone = Clone::new();
        git(
            &clone.data(),
            &["remote", "set-url", "origin", "../gone.git"],
        );
        let (ran, waits, notes) = clone.sync("b1 one");
        assert!(matches!(ran, Err(Error::NeedsRemote(_))), "{ran:?}");
        assert_eq!(retries(&notes), []);
        assert_eq!(waits, []);
    }
}
