//! The write loop: every command that changes the board runs through
//! [`write`], under the clone's lock, on what the remote has.
//!
//! ```text
//! lock     .git/board.lock
//! check    a clone on main with origin and a commit, not shallow; no changed board file
//!          a stale .git/board.index.lock removed
//! round    at most 4:
//!            fetch; M = main, U = origin/main
//!            any failed T found in U -> the command landed; it is not run again
//!            X = U, or M when M is ahead of U
//!            T = the command's commit on X (built in the object store), else X
//!            T == U -> done;  push T -> done, or the next round
//! advance  main fast-forwards to T
//! lint     this project's files -> notes
//! ```
//!
//! A command is a closure over the project loaded from `X`, so a round can
//! run it again on fresh state: after a failed push the command's commit is
//! dropped and the command applied anew to what the remote has then. An id
//! is allocated on the fetched remote tip and printed only after the push
//! that publishes it, so no id is ever renumbered.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

use crate::error::Error;
use crate::git::{Failure, Git, Oid};
use crate::id::{BacklogId, ChoreId, ListKind};
use crate::lint::{Finding, lint};
use crate::list::{Entry, IdsExhausted, parse_line};
use crate::project::{BoardPath, Project, ProjectName};
use crate::raw;

/// Pushes per write. An agent's command has to end rather than retry
/// without limit, and an id not yet published may not be printed; four
/// failed pushes are given up on even with a working fetch.
const ROUNDS: u8 = 4;

/// What a write says along the way, on stderr, each as `board: <note>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// Another write on this clone holds the lock; this one waits for it.
    LockWait,
    /// A push failed; round `round` of the four starts over from a fetch.
    Retry { round: u8 },
    /// The change is on the remote but the files of the clone were not
    /// updated: git's reason.
    CheckoutBehind(String),
    /// A lint finding in the project, after the write.
    Lint(Finding),
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Note::LockWait => f.write_str("waiting for lock"),
            Note::Retry { round } => write!(f, "retry {round}/{ROUNDS}"),
            Note::CheckoutBehind(said) => write!(f, "local files not updated: {said}"),
            Note::Lint(finding) => write!(f, "lint: {finding}"),
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

    fn alloc(&mut self, kind: ListKind) -> Result<u32, Error> {
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
/// id of a line" has one definition.
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
    unchanged(root, &git)?;
    // git's lock on the tool's index, left by a command killed in `commit`:
    // only the tool writes that index, and only under the clone's lock, so
    // a lock file found now is stale. Left there, it fails every later
    // write on the clone.
    remove_if_there(&root.join(".git").join("board.index.lock"))?;

    let mut apply = apply;
    // The result of the command on the last `T` built.
    let mut outcome: Option<Result<T, Error>> = None;
    // Every `T` whose push failed, with its run's result: each may have
    // reached the remote all the same.
    let mut failed: Vec<(Oid, Option<Result<T, Error>>)> = Vec::new();
    let mut last_failure = None;
    let mut done = None;
    for round in 1..=ROUNDS {
        if let Err(failure) = git.fetch() {
            return Err(unreached(what, !failed.is_empty(), &failure));
        }
        let (main, upstream) = tips(&git)?;
        // A push can reach the remote and still report failure (a lost
        // answer). Running the command again would then apply it twice.
        if let Some(run) = landed(&git, &mut failed, &upstream)? {
            outcome = run;
            apply = None;
        }
        let base = integrate(root, &git, &main, &upstream)?;
        let tip = match apply {
            None => base,
            Some(apply) => {
                let mut tx = Tx {
                    project: Project::load_commit(root, project, &base)?,
                    git: &git,
                    base: base.clone(),
                };
                match apply(&mut tx) {
                    Ok((value, subject)) => {
                        outcome = Some(Ok(value));
                        commit(
                            root,
                            &git,
                            &base,
                            &tx.project,
                            &format!("{project}: {subject}"),
                        )?
                    }
                    Err(refusal @ Error::Refused(_)) => {
                        outcome = Some(Err(refusal));
                        base
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        if tip == upstream {
            done = Some(tip);
            break;
        }
        match git.push(&tip) {
            Ok(()) => {
                done = Some(tip);
                break;
            }
            Err(failure) => {
                failed.push((tip, outcome.take()));
                last_failure = Some(failure);
                if round < ROUNDS {
                    notes(Note::Retry { round: round + 1 });
                }
            }
        }
    }
    let tip = match done {
        Some(tip) => tip,
        None => {
            // The last push may have landed too: one more fetch to know.
            if let Err(failure) = git.fetch() {
                return Err(unreached(what, true, &failure));
            }
            let (_, upstream) = tips(&git)?;
            let Some(run) = landed(&git, &mut failed, &upstream)? else {
                let failure = last_failure.expect("a failed push says why");
                return Err(Error::NeedsRemote(format!(
                    "{what}: not pushed after {ROUNDS} attempts ({failure}); nothing was changed"
                )));
            };
            outcome = run;
            upstream
        }
    };
    advance(&git, &tip, notes)?;
    // The write is done whatever lint finds, and a project that cannot be
    // read now was read a moment ago; lint is a note, never the result.
    if let Ok(project) = Project::load(root, project) {
        for finding in lint(&project) {
            notes(Note::Lint(finding));
        }
    }
    outcome.transpose()
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

/// Refuses a clone with a board file changed by hand: the command applies to
/// a commit, so the change would take no part, and the fast-forward could
/// not bring the file up to date.
///
/// The way out it names puts the commit on the remote's main: a file saved
/// during a push leaves `main` behind a remote that holds the push, and a
/// commit on that `main` alone would diverge from it, which a write refuses
/// too.
fn unchanged(root: &Path, git: &Git) -> Result<(), Error> {
    let changed: Vec<String> = git
        .changed_paths()
        .map_err(git_error)?
        .into_iter()
        .filter(|path| BoardPath::parse(path).is_some())
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    Err(Error::Setup(format!(
        "board files changed by hand in {} ({}); a write does not commit them yet: \
         commit them with git on top of the remote's main first \
         (git stash -u; git pull --ff-only; git stash pop; git commit), or restore them",
        root.display(),
        changed.join(", ")
    )))
}

/// `main` and the fetched `origin/main`.
fn tips(git: &Git) -> Result<(Oid, Oid), Error> {
    let main = git.commit("refs/heads/main").map_err(git_error)?;
    let upstream = git.commit("refs/remotes/origin/main").map_err(git_error)?;
    match (main, upstream) {
        (Some(main), Some(upstream)) => Ok((main, upstream)),
        _ => Err(Error::Git(
            "main or origin/main has no commit after the fetch".to_owned(),
        )),
    }
}

/// The run whose failed push is in `upstream` after all, taken out of
/// `failed`. Every failed `T` is asked, not only the last: a push given up
/// on can still finish on the remote after the next round's fetch, and that
/// round's own `T` is then refused. At most one can land: each is pushed as
/// a fast-forward, and none is built on another.
fn landed<R>(git: &Git, failed: &mut Vec<(Oid, R)>, upstream: &Oid) -> Result<Option<R>, Error> {
    for at in 0..failed.len() {
        if git
            .is_ancestor(&failed[at].0, upstream)
            .map_err(git_error)?
        {
            return Ok(Some(failed.swap_remove(at).1));
        }
    }
    Ok(None)
}

/// The commit a command applies to: the remote's, or the clone's when it is
/// ahead. A clone and a remote that both moved on are refused.
fn integrate(root: &Path, git: &Git, main: &Oid, upstream: &Oid) -> Result<Oid, Error> {
    if git.is_ancestor(main, upstream).map_err(git_error)? {
        Ok(upstream.clone())
    } else if git.is_ancestor(upstream, main).map_err(git_error)? {
        Ok(main.clone())
    } else {
        Err(Error::Setup(format!(
            "main of {} and the remote's main have both moved on; \
             a write does not merge them yet",
            root.display()
        )))
    }
}

/// The command's commit: `base` with the project's changed files, built in
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
    let index = root.join(".git").join("board.index");
    git.read_tree(&index, base).map_err(git_error)?;
    let files = project
        .changed
        .iter()
        .map(|path| {
            let bytes = project
                .bytes(path)
                .expect("a changed file is the project's");
            Ok((
                path.to_string(),
                git.hash_object(&bytes).map_err(git_error)?,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    git.update_index(&index, &files).map_err(git_error)?;
    let tree = git.write_tree(&index).map_err(git_error)?;
    remove_if_there(&index)?;
    git.commit_tree(&tree, base, message).map_err(git_error)
}

fn remove_if_there(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::io("remove", path, source)),
    }
}

/// Moves `main` and the files to `tip`. git refuses rather than overwrite
/// a file saved since the check; the change is on the remote by then, so
/// that is a note and the next write brings the files up.
fn advance(git: &Git, tip: &Oid, notes: &dyn Fn(Note)) -> Result<(), Error> {
    let main = git.commit("refs/heads/main").map_err(git_error)?;
    if main.as_ref() == Some(tip) {
        return Ok(());
    }
    if let Err(failure) = git.fast_forward(tip) {
        notes(Note::CheckoutBehind(failure.to_string()));
    }
    Ok(())
}

/// The remote was not reached; after a failed push, whether that push
/// landed is not known.
fn unreached(what: &str, after_push: bool, failure: &Failure) -> Error {
    Error::NeedsRemote(if after_push {
        format!(
            "{what}: the remote was not reached after a failed push ({failure}); the change may have landed"
        )
    } else {
        format!("{what}: needs the remote, which was not reached ({failure})")
    })
}

fn git_error(failure: Failure) -> Error {
    Error::Git(format!("git in the board-data clone: {failure}"))
}
