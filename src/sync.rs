//! The write loop: every command that changes the board runs through
//! [`write`], under the clone's lock, on what the remote has.
//!
//! ```text
//! lock     .git/board.lock
//! check    a clone on main with origin and a commit, not shallow;
//!          git's stale .git/index.lock and .git/board.index.lock removed
//! absorb   board files changed by hand -> one commit on main, `board: hand edits`
//! round    at most 4:
//!            fetch; M = main, U = origin/main
//!            any failed T found in U -> the command landed; it is not run again
//!            X = integrate(M, U): U, M, or a merge commit of the two
//!            X = normalise(X): an id for every item without one, every project
//!            T = the command's commit on X (built in the object store), else X
//!            T == U -> done;  push T -> done, or the next round
//! offline  no fetch, or four failed pushes: X = integrate(M, U) and the
//!          command once more, on main only; allocating an id is refused
//! advance  main fast-forwards to T; a file saved meanwhile is absorbed and
//!          merged in, at most 3 times
//! notes    ids assigned (once pushed), commits not pushed, lint
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
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Error;
use crate::git::{Failure, Git, Oid, TreeEntry};
use crate::id::{BacklogId, BriefId, ChoreId, ItemId, ListKind};
use crate::lint::{Finding, lint};
use crate::list::{Entry, IdsExhausted, ListFile, parse_line};
use crate::merge::{LocalCopy, adopt_ids, merge_trees, published_ids, reconcile};
use crate::project::{BoardPath, Project, ProjectName, copy_paths};
use crate::raw;

/// Pushes per write. An agent's command has to end rather than retry
/// without limit, and an id not yet published may not be printed; four
/// failed pushes are given up on even with a working fetch.
const ROUNDS: u8 = 4;

/// Merges of a file saved during the write before the fast-forward is given
/// up on. A save is a person's or an agent's, so a few are plenty; a loop
/// without end would hold the lock against every other write.
const SETTLES: u8 = 3;

/// What a write says along the way, on stderr, each as `board: <note>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// Another write on this clone holds the lock; this one waits for it.
    LockWait,
    /// A push failed; round `round` of the four starts over from a fetch.
    Retry { round: u8 },
    /// The clone's own commits were merged with the remote's; `lists` were
    /// merged line by line.
    Merged { commits: usize, lists: Vec<String> },
    /// The local version of a brief lost to the remote's and was saved at
    /// `copy`.
    Conflict { brief: BriefId, copy: PathBuf },
    /// An item without an id got this one; pushed.
    Assigned { id: ItemId, text: String },
    /// Commits of the clone that are not on the remote: git's reason.
    NotPushed { commits: usize, reason: String },
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
            Note::Retry { round } => write!(f, "retry {round}/{ROUNDS}"),
            Note::Merged { commits: n, lists } => {
                write!(f, "merged {n} local {}", commits(*n))?;
                if !lists.is_empty() {
                    write!(f, " (by line: {})", lists.join(", "))?;
                }
                Ok(())
            }
            Note::Conflict { brief, copy } => write!(
                f,
                "conflict: brief {brief} changed here and on the remote; \
                 the remote version is kept, yours is saved as {}",
                copy.display()
            ),
            Note::Assigned { id, text } => write!(f, "assigned {id}: {text}"),
            Note::NotPushed { commits: n, reason } => {
                write!(f, "not pushed: {n} {} ({reason})", commits(*n))
            }
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
    /// Four pushes failed with the remote reachable.
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

    /// Refuses a command that has to act on what the remote has. Where an id
    /// is allocated this is checked for every command; a command that
    /// allocates none but claims something shared (`next`) asks for it.
    pub(crate) fn require_remote(&self) -> Result<(), Error> {
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
        let mark = history_mark(self.git, &self.base, self.project.name(), kind)?;
        self.project
            .list(kind)
            .next_id(mark)
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

/// The highest id that a list file ever held, in the history of `commit`.
///
/// History is the counter: one past this and past the file's own ids is the
/// next id. A deleted line would otherwise free its id, and a `closes b41`
/// or a note somewhere would then name a new item. Rejected: a counter file,
/// which a hand edit makes stale and which says again what history says.
/// An added or removed line is read by the one list-line parser, so "the
/// id of a line" has one definition. A hand-typed id is in history before
/// any id is allocated: every write commits hand edits first.
fn history_mark(
    git: &Git,
    commit: &Oid,
    project: &ProjectName,
    kind: ListKind,
) -> Result<u32, Error> {
    let path = BoardPath::List {
        project: project.clone(),
        kind,
    };
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
        .max()
        .unwrap_or(0))
}

/// The highest brief number any file ever added under the project's
/// `briefs/` had, in the history of `commit`; `None` when there was none.
///
/// The same rule as list ids, for the same reason: a brief deleted by hand
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
            BoardPath::List { .. } => None,
        })
        .max())
}

/// What a write works with.
struct Ctx<'a> {
    root: &'a Path,
    git: &'a Git,
    project: &'a ProjectName,
    what: &'a str,
    notes: &'a dyn Fn(Note),
    /// Whether the write said it merged: once, however many rounds merge
    /// the same local commits again.
    merge_noted: Cell<bool>,
    /// This write's mark in its command's commit: see [`run_mark`].
    run: String,
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
    /// The ids normalise gave, with their items' text: printed once pushed.
    assigned: Vec<(ItemId, String)>,
}

/// Runs a write: `apply` once per round until its commit is pushed, or no
/// command (`sync`). Gives the value of the run that was pushed, or its
/// refusal. `what` names the command in an error.
pub(crate) fn write<T>(
    root: &Path,
    project: &ProjectName,
    what: &str,
    notes: &dyn Fn(Note),
    apply: Option<Apply<'_, T>>,
) -> Result<Option<T>, Error> {
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
        project,
        what,
        notes,
        merge_noted: Cell::new(false),
        run: run_mark(),
    };

    let is_sync = apply.is_none();
    let mut apply = apply;
    // Every round whose push failed: each may have reached the remote all
    // the same.
    let mut failed: Vec<Built<T>> = Vec::new();
    let mut last_refusal = None;
    let mut offline = None;
    let mut pushed = None;
    for round in 1..=ROUNDS {
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
            built.assigned.splice(0..0, landed.assigned);
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
                last_refusal = Some(failure);
                if round < ROUNDS {
                    notes(Note::Retry { round: round + 1 });
                }
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
        // stays on `main` for the next write to push. Four refused pushes
        // count the same: an agent's command has to end.
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
    match &offline {
        None => {
            for (id, text) in built.assigned {
                notes(Note::Assigned { id, text });
            }
        }
        Some(offline) => {
            if let Some(upstream) = git.commit("refs/remotes/origin/main").map_err(git_error)? {
                let commits = git
                    .count_between(&upstream, &head(&git)?)
                    .map_err(git_error)?;
                if commits > 0 {
                    notes(Note::NotPushed {
                        commits,
                        reason: offline.reason(),
                    });
                }
            }
        }
    }
    // The write is done whatever lint finds, and a project that cannot be
    // read now was read a moment ago; lint is a note, never the result.
    if let Ok(project) = Project::load(root, project) {
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
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            notes(Note::LockWait);
            file.lock()
                .map_err(|source| Error::io("lock", &path, source))?;
        }
        Err(TryLockError::Error(source)) => return Err(Error::io("lock", &path, source)),
    }
    Ok(file)
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
/// staged is committed with them.
fn absorb(git: &Git) -> Result<(), Error> {
    let changed: Vec<String> = git
        .changed_paths()
        .map_err(git_error)?
        .into_iter()
        .filter(|path| BoardPath::parse(path).is_some())
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

/// One round's commit on `main` and `upstream`: integrated, normalised when
/// online, with the command applied.
fn build<T>(
    ctx: &Ctx<'_>,
    main: &Oid,
    upstream: &Oid,
    offline: Option<&Offline>,
    apply: Option<Apply<'_, T>>,
) -> Result<Built<T>, Error> {
    let mut base = integrate(ctx, main, upstream)?;
    let mut assigned = Vec::new();
    if offline.is_none() {
        (base, assigned) = normalise(ctx, base)?;
    }
    let (tip, outcome) = match apply {
        None => (base, None),
        Some(apply) => {
            let mut tx = Tx {
                project: Project::load_commit(ctx.root, ctx.project, &base)?,
                git: ctx.git,
                base: base.clone(),
                what: ctx.what,
                offline,
            };
            match apply(&mut tx) {
                // A command that changed nothing (`next` on an empty queue)
                // makes no commit: an empty one would be pushed and logged
                // for every look at the queue.
                Ok((value, _)) if tx.project.changed.is_empty() => (base, Some(Ok(value))),
                Ok((value, subject)) => (
                    commit(
                        ctx.root,
                        ctx.git,
                        &base,
                        &tx.project,
                        &format!("{}: {subject}\n\nBoard-Run: {}", ctx.project, ctx.run),
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
    Ok(Built {
        tip,
        outcome,
        assigned,
    })
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
    let mut merged = merge.tree;
    if !merge.lines.is_empty() {
        let empty = git.hash_object(b"").map_err(git_error)?;
        // The commits from the base to the remote's tip, read once and only
        // when a base has an item without an id: the ids those commits gave
        // such items make them the same item on all three sides
        // (`published_ids`).
        let mut path: Option<Vec<Oid>> = None;
        for line in &merge.lines {
            let [base, ours, theirs] = [&line.base, &line.ours, &line.theirs].map(|entry| {
                entry
                    .as_ref()
                    .map_or(empty.clone(), |entry| entry.id.clone())
            });
            let mut blobs = git
                .blobs(&[base.clone(), ours.clone(), theirs.clone()])
                .map_err(git_error)?;
            let (mut base, mut theirs) = (base, theirs);
            if id_less(&ListFile::parse(line.kind, &blobs[0]))
                .next()
                .is_some()
            {
                let commits = match &path {
                    Some(commits) => commits,
                    None => path.insert(
                        git.ancestry_path(&base_commit, upstream)
                            .map_err(git_error)?,
                    ),
                };
                let versions = git.file_at(commits, &line.path).map_err(git_error)?;
                let published = published_ids(
                    line.kind,
                    &blobs[0],
                    versions
                        .iter()
                        .map(|version| version.as_deref().unwrap_or_default()),
                );
                if !published.is_empty() {
                    for (at, blob) in [(0, &mut base), (2, &mut theirs)] {
                        blobs[at] = adopt_ids(line.kind, &blobs[at], &published);
                        *blob = git.hash_object(&blobs[at]).map_err(git_error)?;
                    }
                }
            }
            let union = git.union(&ours, &base, &theirs).map_err(git_error)?;
            let bytes = reconcile(line.kind, &blobs[0], &blobs[1], &blobs[2], &union);
            // A list deleted on one side and emptied by the merge is gone;
            // one both sides keep stays, empty.
            if bytes.is_empty() && (line.ours.is_none() || line.theirs.is_none()) {
                merged.remove(&line.path);
            } else {
                let id = git.hash_object(&bytes).map_err(git_error)?;
                merged.insert(line.path.clone(), TreeEntry::file(id));
            }
        }
    }
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
            lists: merge.lines.into_iter().map(|line| line.path).collect(),
        });
    }
    // Before any push of the merge: once it is on the remote, nothing but
    // this copy holds the local version where anyone looks, and a kill
    // right after the push would leave none.
    save_copies(ctx, &merge.copies)?;
    Ok(commit)
}

/// Gives every item without an id, in every project, the next id of its
/// list: one commit per project, `<project>: assign c14 c15`.
///
/// Every project and not only the command's, so the remote never holds an
/// item without an id: such a line is someone's hand edit, and it gets its
/// id at the first write that reaches the remote, whichever project that
/// write is for. Like any allocation, only on the fetched remote tip.
fn normalise(ctx: &Ctx<'_>, commit: Oid) -> Result<(Oid, Vec<(ItemId, String)>), Error> {
    let git = ctx.git;
    let lists: Vec<(ProjectName, ListKind, Oid)> = git
        .tree(&commit)
        .map_err(git_error)?
        .into_iter()
        .filter_map(|(path, entry)| match BoardPath::parse(&path)? {
            BoardPath::List { project, kind } => Some((project, kind, entry.id)),
            BoardPath::Brief { .. } => None,
        })
        .collect();
    let ids: Vec<Oid> = lists.iter().map(|(_, _, id)| id.clone()).collect();
    let blobs = git.blobs(&ids).map_err(git_error)?;
    let mut without: BTreeMap<ProjectName, Vec<ListKind>> = BTreeMap::new();
    for ((project, kind, _), bytes) in lists.into_iter().zip(blobs) {
        if id_less(&ListFile::parse(kind, &bytes)).next().is_some() {
            without.entry(project).or_default().push(kind);
        }
    }
    let mut commit = commit;
    let mut assigned = Vec::new();
    for (name, kinds) in without {
        let mut project = Project::load_commit(ctx.root, &name, &commit)?;
        let mut given = Vec::new();
        for kind in kinds {
            let texts: Vec<String> = id_less(project.list(kind))
                .map(|text| String::from_utf8_lossy(text).into_owned())
                .collect();
            let mark = history_mark(git, &commit, &name, kind)?;
            // A list whose highest id is u32::MAX has no id left to give:
            // its items without one stay so. The id that ended it is in the
            // file or its history, where the user typed it.
            let Ok(numbers) = project.list_mut(kind).assign_ids(mark) else {
                continue;
            };
            for (number, text) in numbers.into_iter().zip(texts) {
                let id = match kind {
                    ListKind::Chores => ItemId::Chore(ChoreId::new(number).expect("past 0")),
                    ListKind::Backlog => ItemId::Backlog(BacklogId::new(number).expect("past 0")),
                };
                given.push((id, text));
            }
        }
        if given.is_empty() {
            continue;
        }
        let ids: Vec<String> = given.iter().map(|(id, _)| id.to_string()).collect();
        commit = self::commit(
            ctx.root,
            git,
            &commit,
            &project,
            &format!("{name}: assign {}", ids.join(" ")),
        )?;
        assigned.extend(given);
    }
    Ok((commit, assigned))
}

/// The texts of a list's items without an id, in file order.
fn id_less(list: &ListFile) -> impl Iterator<Item = &[u8]> {
    list.entries().filter_map(|entry| match entry {
        Entry::Item { id: None, text } => Some(text),
        Entry::Item { id: Some(_), .. } | Entry::Heading(_) | Entry::Text(_) => None,
    })
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
        .changed
        .iter()
        .map(|path| {
            let entry = match project.bytes(path) {
                Some(bytes) => Some(TreeEntry::file(git.hash_object(&bytes).map_err(git_error)?)),
                None => None,
            };
            Ok((path.to_string(), entry))
        })
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

/// Saves the local version of every brief that lost a merge beside the
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
                        brief: copy.brief,
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

fn git_error(failure: Failure) -> Error {
    Error::Git(format!("git in the board-data clone: {failure}"))
}
