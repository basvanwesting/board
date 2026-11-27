//! One project's board: a method per command.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::after;
use crate::brief::{EntryText, Stamp, Status, Text};
use crate::error::Error;
use crate::git::{Failure, Git, Oid};
use crate::id::{BacklogId, BriefId, ChoreId, ItemId, ListId, ListKind};
use crate::item::{Body, Items, Row};
use crate::lint::{Finding, lint};
use crate::list::{FindError, Line};
use crate::migrate;
use crate::ops::{self, CheckChange, Claim, Commits, Filed, NewBacklog, NewChore, Queueing};
use crate::project::{
    self, BoardPath, BriefFile, Env, Place, Project, ProjectName, QueuePlace, Slug, resolve,
};
use crate::sync::{self, Fetched, Note};

pub struct Board {
    /// Absolute: a path the board prints is one an agent can open from
    /// anywhere.
    root: PathBuf,
    /// Where the command runs: the project repository whose `HEAD` a new
    /// brief is based on, also when `--board` names the project.
    cwd: PathBuf,
    project: ProjectName,
    /// Whether the repository the command runs in is the project's own:
    /// only there do its commits say which filed briefs are unpushed.
    in_project: bool,
    /// Where a write says what happens along the way. A note is passed on
    /// as it comes: `waiting for lock` has to show before the wait. `Send`:
    /// the TUI's writes run on a thread of their own.
    notes: Box<dyn Fn(Note) + Send>,
    /// `--local`: its writes make no fetch and no push (`sync::Run`).
    /// `main` refuses the flag on every command it would not fit, so only
    /// local-first writes get here with it.
    local: bool,
}

impl Board {
    /// Finds the project and the board-data clone; reads nothing yet.
    pub fn open(env: Env, notes: Box<dyn Fn(Note) + Send>) -> Result<Board, Error> {
        let project = resolve(&env)?;
        // Named with `--board`: the checkout may be another project's, or
        // none, and then nothing is labelled.
        let in_project = env.board.is_none()
            || resolve(&Env {
                board: None,
                ..env.clone()
            })
            .is_ok_and(|checkout| checkout == project);
        // A project without a directory is an empty board, so a root that
        // is not there would read as one too: every list empty, lint `ok`,
        // on a machine where the board was never cloned or `BOARD_ROOT` has
        // a typo. That is told apart here, for reads as for writes.
        if !env.root.is_dir() {
            return Err(Error::Setup(format!(
                "no board-data clone at {root}; clone it there once (git clone <url> {root}), \
                 or point BOARD_ROOT at it",
                root = env.root.display()
            )));
        }
        Ok(Board {
            root: env.cwd.join(env.root),
            cwd: env.cwd,
            project,
            in_project,
            notes,
            local: false,
        })
    }

    /// The board whose writes stay on the clone (`--local`).
    pub fn local(self) -> Board {
        Board {
            local: true,
            ..self
        }
    }

    /// `board sync` without `--board`, run in the board-data clone itself:
    /// the write with no command, lint left out (owner, 2026-11-27: "make
    /// board sync the one exception to the refusal in the clone root, and
    /// print no lint there"; lint is per project, and a sync covers the
    /// whole clone). The refusal of a project with a list file of the older
    /// storage is left out with it: it is per project too, and a list a hand
    /// edit left is committed as any write of another project commits it.
    /// `false` when the command does not run in the clone, for
    /// [`Board::open`] to take it from there.
    pub fn sync_in_clone(env: &Env, notes: &dyn Fn(Note)) -> Result<bool, Error> {
        if !matches!(project::locate(env)?, project::Located::Clone(_)) {
            return Ok(false);
        }
        sync::write::<()>(
            &env.cwd.join(&env.root),
            "sync",
            notes,
            sync::Run::Sync { lint: None },
        )?;
        Ok(true)
    }

    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    /// The board with its notes said elsewhere: the TUI's go to its bottom
    /// line, as stderr is its screen.
    pub(crate) fn with_notes(self, notes: Box<dyn Fn(Note) + Send>) -> Board {
        Board { notes, ..self }
    }

    /// The board-data clone, absolute.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The project from the files, refused while it has a list file of the
    /// older storage: every read goes through here.
    pub(crate) fn load(&self) -> Result<Project, Error> {
        let project = Project::load(&self.root, &self.project)?;
        project.refuse_old_lists()?;
        Ok(project)
    }

    /// Converts the project's list files of the older storage to item
    /// files and brings the item files up to date, in one write; the one
    /// command a project with a list file takes.
    pub fn migrate(&self) -> Result<String, Error> {
        let now = now()?;
        let apply = |tx: &mut sync::Tx| {
            let migrated = migrate::migrate(&mut tx.project, now).map_err(|unplaced| {
                let lines: Vec<String> =
                    unplaced.iter().map(|line| format!("\n  {line}")).collect();
                Error::Refused(format!(
                    "migrate: nothing converted; fix these lines by hand and run it again:{}",
                    lines.concat()
                ))
            })?;
            if migrated.is_nothing() {
                // Nothing changed, so nothing is committed under a subject.
                return Ok(("nothing to migrate".to_owned(), String::new()));
            }
            Ok((migrated.to_string(), migrated.to_string()))
        };
        let value = sync::write(
            &self.root,
            "migrate",
            &*self.notes,
            sync::Run::Command {
                project: &self.project,
                kind: sync::Kind::LocalFirst,
                local: self.local,
                apply: &apply,
            },
        )?;
        Ok(value.expect("a write that runs a command gives its value"))
    }

    /// The TUI's background fetch, to run on a thread of its own.
    pub fn fetcher(&self) -> Fetcher {
        Fetcher {
            root: self.root.clone(),
        }
    }

    /// The board as `ls` prints it.
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        let unpushed = self.unpushed()?;
        self.load().map(|project| Snapshot::of(&project, &unpushed))
    }

    /// The briefs with a commit in the project repository that its
    /// upstream does not have ([`Pushes::unpushed`]).
    pub fn unpushed(&self) -> Result<BTreeSet<BriefId>, Error> {
        self.pushes().unpushed()
    }

    /// The question [`Board::unpushed`] asks, apart from the board so a
    /// thread can own it.
    pub fn pushes(&self) -> Pushes {
        Pushes {
            cwd: self.cwd.clone(),
            in_project: self.in_project,
        }
    }

    /// What `show` prints: a brief's file or a chore's or backlog item's
    /// file, as stored, byte for byte.
    ///
    /// An item as its file, as a brief is (owner, 2026-11-26: "I would do
    /// it like briefs do for chores and backlogs as well"), reversing the
    /// line and body of the list files, where an item had no header. The
    /// one-line form stays what a list shows (`ls`, the rows, `Y`).
    pub fn show(&self, id: ItemId) -> Result<Vec<u8>, Error> {
        let project = self.load()?;
        let refused = |why: String| Error::Refused(format!("show {id}: {why}"));
        match id {
            ItemId::Brief(number) => project
                .find_brief(number)
                .map(|file| file.brief.to_bytes())
                .map_err(|why| refused(ops::no_brief(number, why))),
            ItemId::Chore(chore) => {
                item_file(project.list(ListKind::Chores), chore.get()).map_err(refused)
            }
            ItemId::Backlog(backlog) => {
                item_file(project.list(ListKind::Backlog), backlog.get()).map_err(refused)
            }
        }
    }

    /// The findings of this project.
    pub fn lint(&self) -> Result<Vec<Finding>, Error> {
        self.load().map(|project| lint(&project))
    }

    /// Adds a chore under its group; gives its new id.
    pub fn add_chore(&self, new: &NewChore) -> Result<ChoreId, Error> {
        let now = now()?;
        self.refereed("add chore", &|tx| {
            if let Some(target) = new.after {
                tx.require_given("add chore", target)?;
            }
            let id = tx.alloc_chore()?;
            ops::add_chore(&mut tx.project, id, new, now);
            Ok((id, format!("add {id}")))
        })
    }

    /// Adds a backlog item under its group, with its body (empty for
    /// none); gives its new id.
    pub fn add_backlog(&self, new: &NewBacklog) -> Result<BacklogId, Error> {
        let now = now()?;
        self.refereed("add backlog", &|tx| {
            if let Some(target) = new.after {
                tx.require_given("add backlog", target)?;
            }
            let id = tx.alloc_backlog()?;
            ops::add_backlog(&mut tx.project, id, new, now);
            Ok((id, format!("add {id}")))
        })
    }

    /// Changes a chore's or backlog item's title, a chore's check (set or
    /// removed), or both; gives what it did, as `retitled c12`.
    ///
    /// # Panics
    ///
    /// When a check is given for a backlog item, which has none: the
    /// command line refuses that before the board is read.
    pub fn retitle(
        &self,
        id: ListId,
        text: Option<&Line>,
        check: Option<&CheckChange>,
    ) -> Result<String, Error> {
        self.local_first("retitle", &|tx| {
            let retitled = ops::retitle(&mut tx.project, id, text, check)?;
            Ok((retitled, format!("retitle {id}")))
        })
    }

    /// Appends to a chore's or backlog item's body, or replaces it; gives
    /// what it did, as `amended c12`.
    pub fn amend_item(&self, id: ListId, body: &Body, replace: bool) -> Result<String, Error> {
        self.local_first("amend", &|tx| {
            let amended = ops::amend_item(&mut tx.project, id, body, replace)?;
            Ok((amended.clone(), amended))
        })
    }

    /// Deletes chores' files, all or none; gives what it did, as `done c12
    /// c13`.
    pub fn done(&self, ids: &[ChoreId]) -> Result<String, Error> {
        self.local_first("done", &|tx| {
            let done = ops::done(&mut tx.project, ids)?;
            Ok((done.clone(), done))
        })
    }

    /// Deletes backlog items' files, all or none; gives what it did, as
    /// `dropped b41`.
    pub fn drop_items(&self, ids: &[BacklogId]) -> Result<String, Error> {
        self.local_first("drop", &|tx| {
            let dropped = ops::drop_items(&mut tx.project, ids)?;
            Ok((dropped.clone(), dropped))
        })
    }

    /// Puts chores and backlog items into a group, in one commit; gives
    /// what it did, as `grouped c12 b4 under Home`. Allocates nothing, so
    /// it works offline like `done`.
    pub fn group(&self, ids: &[ListId], to: &Line) -> Result<String, Error> {
        self.local_first("group", &|tx| {
            let grouped = ops::group(&mut tx.project, ids, to)?;
            Ok((grouped.clone(), grouped))
        })
    }

    /// Adds the project's next brief, its header written; gives the brief's
    /// path. The base is the commit the project repository is on now.
    pub fn new_brief(&self, slug: &Slug) -> Result<PathBuf, Error> {
        let base = self.project_head()?;
        let now = now()?;
        let path = self.refereed("new brief", &|tx| {
            let number = tx.alloc_brief()?;
            let path = ops::new_brief(&mut tx.project, number, slug, &base, now);
            Ok((path, format!("new brief {number}-{slug}")))
        })?;
        Ok(self.path(&path))
    }

    /// Queues a draft, writing what `asked` gives (the backlog items it
    /// closes, what it waits on, first in the queue), or puts a queued brief
    /// first; gives what it did, as `queued 128`.
    pub fn queue(&self, id: BriefId, asked: Queueing<'_>) -> Result<String, Error> {
        let now = now()?;
        self.local_first("queue", &|tx| {
            if let Some(target) = asked.after {
                tx.require_given(&format!("queue {id}"), target)?;
            }
            let queued = ops::queue(&mut tx.project, id, asked, now)?;
            Ok((queued.clone(), queued))
        })
    }

    /// Parks a written draft; gives what it did, as `parked 128`.
    pub fn park(
        &self,
        id: BriefId,
        closes: Option<&[BacklogId]>,
        after: Option<ItemId>,
    ) -> Result<String, Error> {
        let now = now()?;
        self.local_first("park", &|tx| {
            if let Some(target) = after {
                tx.require_given(&format!("park {id}"), target)?;
            }
            let parked = ops::park(&mut tx.project, id, closes, after, now)?;
            Ok((parked.clone(), parked))
        })
    }

    /// Queues every parked brief in one commit; gives what it did, as
    /// `queued 31, 32`, or `nothing parked` with no commit.
    pub fn queue_parked(&self) -> Result<String, Error> {
        let now = now()?;
        self.local_first("queue --parked", &|tx| {
            let queued = ops::queue_parked(&mut tx.project, now)?;
            Ok((queued.clone(), queued))
        })
    }

    /// Makes an item or a brief wait on another, or on nothing; gives what
    /// it did, as `c3 after c9`. Allocates nothing, so it works offline
    /// like `done`.
    pub fn after(&self, id: ItemId, target: Option<ItemId>) -> Result<String, Error> {
        self.local_first("after", &|tx| {
            if let Some(target) = target {
                tx.require_given(&format!("after {id}"), target)?;
            }
            let said = ops::set_after(&mut tx.project, id, target)?;
            Ok((said.clone(), said))
        })
    }

    /// Claims the first brief of the queue that waits on nothing.
    ///
    /// Only with the remote: a claim is shared like an id, and two clones
    /// claiming one brief offline would each hand it to a worker.
    pub fn next(&self) -> Result<Next, Error> {
        let now = now()?;
        let claim = self.refereed("next", &|tx| {
            tx.require_remote()?;
            let claim = ops::next(&mut tx.project, now)?;
            let subject = match &claim {
                Claim::Claimed(..) => claim.to_string(),
                // Nothing changed, so nothing is committed under a subject.
                Claim::Empty | Claim::Waiting(..) => String::new(),
            };
            Ok((claim, subject))
        })?;
        Ok(match claim {
            Claim::Claimed(_, path) => Next::Claimed(self.path(&path)),
            other => Next::Nothing(other.to_string()),
        })
    }

    /// Blocks a started brief on a question, which goes under
    /// `## Questions`.
    pub fn block(&self, id: BriefId, question: &Text) -> Result<(), Error> {
        let question = entry(question, &format!("block {id}"))?;
        let now = now()?;
        self.local_first("block", &|tx| {
            let blocked = ops::block(&mut tx.project, id, &question, now)?;
            Ok(((), blocked))
        })
    }

    /// Appends an amendment under `## Amendments`; a blocked brief resumes.
    /// Gives what it did, as `amended 128, resumed`.
    pub fn amend(&self, id: BriefId, text: &Text) -> Result<String, Error> {
        let text = entry(text, &format!("amend {id}"))?;
        let now = now()?;
        self.local_first("amend", &|tx| {
            let amended = ops::amend(&mut tx.project, id, &text, now)?;
            Ok((amended, format!("amended {id}")))
        })
    }

    /// Makes the text the started brief's `## Report` and reports it.
    pub fn report(&self, id: BriefId, text: &Text) -> Result<(), Error> {
        let now = now()?;
        self.local_first("report", &|tx| {
            let reported = ops::report(&mut tx.project, id, text, now)?;
            Ok(((), reported))
        })
    }

    /// Files a reported brief: the backlog items it closes are deleted, and
    /// it moves to `briefs/done/`, in one commit.
    ///
    /// The commits are counted before the write, in the repository the
    /// command runs in: they are the worker's, not the board's, and a write
    /// may run its command more than once.
    pub fn file(&self, id: BriefId, check: CommitCheck) -> Result<Filed, Error> {
        let commits = match check {
            CommitCheck::Trailer => self.brief_commits(id)?,
            CommitCheck::Skip => Commits::Skipped,
            CommitCheck::NotBuilt => Commits::NotBuilt,
        };
        let now = now()?;
        self.local_first("file", &|tx| {
            let filed = ops::file(&mut tx.project, id, &commits, now)?;
            Ok((filed, format!("filed {id}")))
        })
    }

    /// The commits of the repository the command runs in that carry the
    /// brief's trailer, `Brief: <id>` on a line of its own (trailing blanks
    /// allowed), reachable from `HEAD`.
    ///
    /// By trailer, not by commit ids noted in the report: a rebase changes
    /// every id and keeps the message (the owner's decision).
    fn brief_commits(&self, number: BriefId) -> Result<Commits, Error> {
        let repository = match Git::at(&self.cwd).toplevel() {
            Ok(repository) => repository,
            // Told by `ops::file` after the status: a brief that is not
            // reported says so first.
            Err(Failure::Said(said)) => {
                return Ok(Commits::NoRepository {
                    at: self.cwd.clone(),
                    said,
                });
            }
            Err(failure @ Failure::NotRun(_)) => return Err(Error::Git(failure.to_string())),
        };
        let git = Git::at(&repository);
        let git_error = |failure: Failure| {
            Error::Git(format!(
                "file {number}: cannot count the commits in {}: {failure}",
                repository.display()
            ))
        };
        // A repository without a commit has none with the trailer; `HEAD`
        // names nothing there, and log would fail on it.
        let count = match git.commit("HEAD").map_err(git_error)? {
            None => 0,
            Some(_) => git
                .messages("HEAD")
                .map_err(git_error)?
                .iter()
                .filter(|message| trailers(message).any(|named| named == number))
                .count(),
        };
        Ok(Commits::Counted { count, repository })
    }

    /// Where a board path is on this machine.
    fn path(&self, path: &BoardPath) -> PathBuf {
        self.root.join(path.to_string())
    }

    /// The commit the repository the command runs in is on: a new brief's
    /// base, read before the write starts.
    fn project_head(&self) -> Result<Oid, Error> {
        let at = self.cwd.display();
        match Git::at(&self.cwd).commit("HEAD") {
            Ok(Some(head)) => Ok(head),
            Ok(None) => Err(Error::Refused(format!(
                "new brief: the repository at {at} has no commit to base the brief on"
            ))),
            Err(Failure::Said(said)) => Err(Error::Usage(format!(
                "new brief: no repository at {at} to base the brief on ({said}); \
                 run it in the project's checkout"
            ))),
            Err(failure @ Failure::NotRun(_)) => Err(Error::Git(failure.to_string())),
        }
    }

    /// The write loop with no command: hand edits committed, and the clone
    /// and the remote on the same commit. Without the remote the commits
    /// stay on the clone and it is [`Error::NeedsRemote`].
    ///
    /// Refused, as every command but `migrate`, for a project with a list
    /// file of the older storage, before and after: one the sync brought in
    /// is said at once.
    pub fn sync(&self) -> Result<(), Error> {
        self.load()?;
        sync::write::<()>(
            &self.root,
            "sync",
            &*self.notes,
            sync::Run::Sync {
                lint: Some(&self.project),
            },
        )?;
        self.load().map(drop)
    }

    /// A command whose result the remote decides: it hands out an id or
    /// claims something shared, so it is refused when it is not pushed
    /// (`sync::Kind::Refereed`). The explicit exception (owner, 2026-11-27:
    /// "local first as default and refereed as explicit exceptions on add,
    /// new, next").
    fn refereed<T>(&self, what: &str, apply: sync::Apply<'_, T>) -> Result<T, Error> {
        self.command(what, sync::Kind::Refereed, apply)
    }

    /// Every other command: it succeeds on the clone when its push fails
    /// twice, and its commit goes out with the next write
    /// (`sync::Kind::LocalFirst`).
    fn local_first<T>(&self, what: &str, apply: sync::Apply<'_, T>) -> Result<T, Error> {
        self.command(what, sync::Kind::LocalFirst, apply)
    }

    /// A write that runs a command; `what` names it in an error. Refused for
    /// a project with a list file of the older storage, in the files and in
    /// the commit it applies to: one can come in with the merge.
    fn command<T>(
        &self,
        what: &str,
        kind: sync::Kind,
        apply: sync::Apply<'_, T>,
    ) -> Result<T, Error> {
        self.load()?;
        let guarded = |tx: &mut sync::Tx| {
            tx.project.refuse_old_lists()?;
            apply(tx)
        };
        let value = sync::write(
            &self.root,
            what,
            &*self.notes,
            sync::Run::Command {
                project: &self.project,
                kind,
                local: self.local,
                apply: &guarded,
            },
        )?;
        Ok(value.expect("a write that runs a command gives its value"))
    }
}

/// Which briefs the project repository the command runs in has commits
/// for above its upstream.
#[derive(Debug, Clone)]
pub struct Pushes {
    cwd: PathBuf,
    /// As the board's: whether this is the project's own checkout.
    in_project: bool,
}

impl Pushes {
    /// The briefs with a commit in the project repository that its
    /// upstream does not have: the filed ones among them are shown as
    /// `unpushed` until the owner pushes. Derived from git each time, never
    /// stored: a stored copy would go stale at every push made without
    /// `board`. One `log` for all of them, after the questions whether
    /// there is an upstream and a commit. None where the command does not
    /// run in the project's own checkout, where its branch has no upstream,
    /// where `HEAD` is detached (which has none either) or has no commit
    /// yet. About this checkout: the one the owner pushes from.
    pub fn unpushed(&self) -> Result<BTreeSet<BriefId>, Error> {
        if !self.in_project {
            return Ok(BTreeSet::new());
        }
        let git = Git::at(&self.cwd);
        let error = |failure: Failure| {
            Error::Git(format!(
                "cannot read the unpushed commits in {}: {failure}",
                self.cwd.display()
            ))
        };
        let Some(upstream) = git.commit("@{upstream}").map_err(error)? else {
            return Ok(BTreeSet::new());
        };
        // A branch without a commit may still name an upstream it fetched.
        if git.commit("HEAD").map_err(error)?.is_none() {
            return Ok(BTreeSet::new());
        }
        let messages = git.messages(&format!("{upstream}..HEAD")).map_err(error)?;
        Ok(messages
            .iter()
            .flat_map(|message| trailers(message))
            .collect())
    }
}

/// The TUI's background job, apart from the [`Board`] so a thread can own
/// it.
pub struct Fetcher {
    root: PathBuf,
}

impl Fetcher {
    /// Fetches the remote's `main` and counts the commits each side has,
    /// when no write holds the clone's lock; [`Fetched::Busy`] when one
    /// does. Commits and pushes nothing.
    pub fn fetch(&self) -> Fetched {
        sync::fetch_in_background(&self.root)
    }
}

/// Whether `file` checks that the brief has commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitCheck {
    /// At least one commit with the brief's trailer, or no filing.
    Trailer,
    /// `--no-commits`: the brief changed nothing in the repository.
    Skip,
    /// `--not-built`: no commits looked for, and nothing it closes deleted.
    NotBuilt,
}

/// The time a write stamps, taken once, before the write: every round of it
/// writes the same.
fn now() -> Result<Stamp, Error> {
    Stamp::now()
        .ok_or_else(|| Error::Setup("the clock is outside the years 1970 to 9999".to_owned()))
}

/// The briefs a commit message names: each line that is `Brief: <id>`,
/// blanks after it allowed, the id as `board` writes it (no leading zero).
/// One reading for `file`, which counts a brief's commits, and the
/// `unpushed` label, which looks for them above the upstream, so the two
/// cannot disagree; whole lines, so `Brief: 12` names neither 1 nor 120.
pub(crate) fn trailers(message: &[u8]) -> impl Iterator<Item = BriefId> + '_ {
    message.split(|b| *b == b'\n').filter_map(|line| {
        let id = str::from_utf8(line.strip_prefix(b"Brief: ")?.trim_ascii_end()).ok()?;
        let number: u32 = id.parse().ok()?;
        (number.to_string() == id).then(|| BriefId::new(number))
    })
}

/// The text of a question or an amendment, refused before the write when it
/// would not stay in its section: no state of the board makes it fit.
fn entry(text: &Text, what: &str) -> Result<EntryText, Error> {
    EntryText::new(text.clone()).map_err(|why| Error::Refused(format!("{what}: {why}")))
}

/// The file of the item with this id, as stored, else why the id names
/// none.
fn item_file(items: &Items, number: u32) -> Result<Vec<u8>, String> {
    items
        .get(number)
        .map(crate::item::Item::to_bytes)
        .ok_or_else(|| ops::not_one(items.kind(), number, FindError::Missing))
}

/// What `next` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// The path of the brief it claimed.
    Claimed(PathBuf),
    /// Nothing claimed, as said: `queue empty`, or `queue empty, 2 waiting
    /// on c9` when the queued briefs all wait.
    Nothing(String),
}

/// What the board shows of a project: the briefs, the chores and the
/// backlog by group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub project: ProjectName,
    /// The briefs in `briefs/` and the unpushed ones, in the order of
    /// [`BriefRow::rank`], the queued ones in queue order ([`QueuePlace`]),
    /// then by number.
    pub open: Vec<BriefRow>,
    /// The briefs in `briefs/parked/`, by number.
    pub parked: Vec<BriefRow>,
    /// The briefs in `briefs/done/` that are not unpushed, by number.
    pub filed: Vec<BriefRow>,
    pub chores: Vec<ListRow>,
    pub backlog: Vec<ListRow>,
}

/// A word `ls --status` takes: a status, or the `unpushed` label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Status(Status),
    Unpushed,
}

impl Shown {
    /// The word: `ls` prints it, `--status` takes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Shown::Status(status) => status.as_str(),
            Shown::Unpushed => UNPUSHED,
        }
    }
}

impl FromStr for Shown {
    type Err = UnknownShown;

    fn from_str(s: &str) -> Result<Shown, UnknownShown> {
        if s == UNPUSHED {
            return Ok(Shown::Unpushed);
        }
        s.parse()
            .map(Shown::Status)
            .map_err(|_| UnknownShown(s.to_owned()))
    }
}

/// A word that is neither a status nor `unpushed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownShown(pub String);

impl fmt::Display for UnknownShown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let known = Status::ALL.map(Status::as_str).join(", ");
        write!(f, "`{}` is not a status ({known}, {UNPUSHED})", self.0)
    }
}

impl std::error::Error for UnknownShown {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefRow {
    pub number: BriefId,
    /// `None` for a brief whose status is missing or unknown; lint says
    /// which.
    pub status: Option<Status>,
    pub slug: String,
    /// Filed, with a commit still above the project repository's upstream
    /// ([`Board::unpushed`]): shown among the open briefs as `unpushed`
    /// until the owner pushes. A label, not a status: nothing stores it.
    pub unpushed: bool,
    /// Its place in the queue ([`QueuePlace`]); read only while queued.
    pub queue: QueuePlace,
    /// What it still waits on (`after:`, unsatisfied); `None` for nothing.
    pub waits: Option<ItemId>,
}

impl BriefRow {
    pub(crate) fn of(project: &Project, file: &BriefFile) -> BriefRow {
        let number = file.name.number();
        BriefRow {
            number,
            status: file.status().ok(),
            slug: file.name.slug().to_owned(),
            unpushed: false,
            queue: file.queue_place(),
            waits: after::waits_on(project, ItemId::Brief(number)),
        }
    }

    /// Which list the brief is in: the open ones (in `briefs/`, or filed
    /// and unpushed), the parked ones, the filed ones.
    fn list(&self, place: Place) -> Place {
        match place {
            Place::Done if self.unpushed => Place::Open,
            place => place,
        }
    }

    /// Where the row stands among the briefs: what is in flight first
    /// (started, blocked), then what waits for the planner (reported), then
    /// what waits for the owner's push (unpushed), then what waits for the
    /// worker (queued), then what is still being written. A brief without a
    /// usable status comes last, where it cannot push the brief in flight
    /// out of sight.
    fn rank(&self) -> u8 {
        match self.status {
            _ if self.unpushed => 3,
            Some(Status::Started) => 0,
            Some(Status::Blocked) => 1,
            Some(Status::Reported) => 2,
            Some(Status::Queued) => 4,
            Some(Status::Draft) => 5,
            None => 6,
            Some(Status::Filed) => 7,
            Some(Status::Parked) => 8,
        }
    }

    /// What the row is shown as: its status, or `unpushed`; `None` for a
    /// brief without a usable status.
    pub fn shown(&self) -> Option<Shown> {
        if self.unpushed {
            return Some(Shown::Unpushed);
        }
        self.status.map(Shown::Status)
    }

    /// The word in the status column: `unpushed` for a brief so labelled.
    pub fn status_word(&self) -> &'static str {
        self.shown().map_or("unknown", Shown::as_str)
    }

    /// The row as `board ls` prints it, without the newline; the TUI's `Y`
    /// yanks the same.
    pub fn ls_line(&self) -> String {
        format!(
            "{} {} {}{}",
            self.number,
            self.status_word(),
            self.slug,
            waits_suffix(self.waits)
        )
    }
}

/// A row of a kind's listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    /// A group, as its name: `ls` prints `## <name>` above its items.
    Group(Vec<u8>),
    /// An item, as its line ([`crate::item::Item::line`]): the id is the
    /// first word.
    Item(Vec<u8>),
}

/// Where a brief stands among the open or the filed ones: its rank, its
/// place in the queue while queued, its number.
type BriefOrder = (u8, Option<QueuePlace>, BriefId);

/// What a line or row ends with while its item waits: `  after c9`; empty
/// once the wait is over, when the line is as it was. Two blanks, as the
/// reference `  (126)` is set off: after a chore's check one blank would
/// read as part of it ("Check: it is up after c1").
pub fn waits_suffix(waits: Option<ItemId>) -> String {
    waits
        .map(|target| format!("  after {target}"))
        .unwrap_or_default()
}

/// The label of a filed brief with unpushed commits.
pub const UNPUSHED: &str = "unpushed";

/// Labels the briefs by `unpushed` and splits them into the open ones, the
/// parked ones and the filed ones, in the order the board shows them: by
/// [`BriefRow::rank`], the queued ones in queue order ([`QueuePlace`]),
/// then by number. One order for `ls` and the TUI, which hold a brief as a
/// row or as an item: `row` gives an item's row and where its file is.
/// Parked and filed ones go by number: the place decides their status
/// ([`BriefFile::status`]), so all rank alike.
pub(crate) fn arrange<T>(
    briefs: impl IntoIterator<Item = T>,
    unpushed: &BTreeSet<BriefId>,
    mut row: impl FnMut(&mut T) -> (&mut BriefRow, Place),
) -> Arranged<T> {
    let (mut open, mut parked, mut filed) = (Vec::new(), Vec::new(), Vec::new());
    for mut brief in briefs {
        let (row, place) = row(&mut brief);
        row.unpushed = row.status == Some(Status::Filed) && unpushed.contains(&row.number);
        // Queued briefs in queue order, so `--first` shows; the others by
        // number, as before.
        let queue = (row.status == Some(Status::Queued) && !row.unpushed).then_some(row.queue);
        let key = (row.rank(), queue, row.number);
        match row.list(place) {
            Place::Open => open.push((key, brief)),
            Place::Parked => parked.push((key, brief)),
            Place::Done => filed.push((key, brief)),
        }
    }
    let sorted = |mut briefs: Vec<(BriefOrder, T)>| -> Vec<T> {
        briefs.sort_by_key(|(key, _)| *key);
        briefs.into_iter().map(|(_, brief)| brief).collect()
    };
    Arranged {
        open: sorted(open),
        parked: sorted(parked),
        filed: sorted(filed),
    }
}

/// The briefs as [`arrange`] splits them.
pub(crate) struct Arranged<T> {
    pub(crate) open: Vec<T>,
    pub(crate) parked: Vec<T>,
    pub(crate) filed: Vec<T>,
}

impl Snapshot {
    pub fn of(project: &Project, unpushed: &BTreeSet<BriefId>) -> Snapshot {
        let rows = project
            .briefs()
            .iter()
            .map(|file| (BriefRow::of(project, file), file.place));
        let arranged = arrange(rows, unpushed, |(row, place)| (row, *place));
        let rows =
            |briefs: Vec<(BriefRow, Place)>| briefs.into_iter().map(|(row, _)| row).collect();
        Snapshot {
            project: project.name().clone(),
            open: rows(arranged.open),
            parked: rows(arranged.parked),
            filed: rows(arranged.filed),
            chores: list_rows(project, ListKind::Chores),
            backlog: list_rows(project, ListKind::Backlog),
        }
    }

    /// The briefs `ls` lists: the open ones, or those it shows with one
    /// word. `filed` is the briefs in `briefs/done/` that are not
    /// `unpushed`, `parked` those in `briefs/parked/`: neither is among the
    /// open ones, so a parked brief is out of the planner's queue checks.
    pub fn briefs(&self, shown: Option<Shown>) -> impl Iterator<Item = &BriefRow> {
        let rows = match shown {
            Some(Shown::Status(Status::Filed)) => &self.filed,
            Some(Shown::Status(Status::Parked)) => &self.parked,
            Some(_) | None => &self.open,
        };
        rows.iter().filter(move |row| match shown {
            None | Some(Shown::Status(Status::Filed | Status::Parked)) => true,
            Some(Shown::Unpushed) => row.unpushed,
            Some(Shown::Status(status)) => !row.unpushed && row.status == Some(status),
        })
    }

    pub fn list(&self, kind: ListKind) -> &[ListRow] {
        match kind {
            ListKind::Chores => &self.chores,
            ListKind::Backlog => &self.backlog,
        }
    }
}

fn list_rows(project: &Project, kind: ListKind) -> Vec<ListRow> {
    let items = project.list(kind);
    items
        .rows()
        .into_iter()
        .map(|row| match row {
            Row::Group(name) => ListRow::Group(name.to_vec()),
            Row::Item(listed) => {
                let mut line = listed.item.line(kind, listed.number);
                line.extend_from_slice(
                    waits_suffix(item_waits(project, kind, listed.number)).as_bytes(),
                );
                ListRow::Item(line)
            }
        })
        .collect()
}

/// What the item `number` of `kind` still waits on.
pub(crate) fn item_waits(project: &Project, kind: ListKind, number: u32) -> Option<ItemId> {
    let id = ListId::of(kind, number).expect("an item's id is past 0");
    after::waits_on(project, ItemId::from(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brief::Brief;
    use crate::project::BoardPath;
    use proptest::prelude::*;

    fn shop() -> ProjectName {
        ProjectName::parse("shop").unwrap()
    }

    fn brief(path: &str, text: &str) -> BriefFile {
        match BoardPath::parse(&format!("shop/{path}")) {
            Some(BoardPath::Brief { name, place, .. }) => BriefFile {
                name,
                place,
                brief: Brief::parse(text.as_bytes()),
            },
            _ => panic!("{path} is no brief path"),
        }
    }

    fn project(briefs: Vec<BriefFile>) -> Project {
        Project {
            briefs,
            ..Project::empty(shop())
        }
    }

    fn listed<'a>(rows: impl Iterator<Item = &'a BriefRow>) -> Vec<String> {
        rows.map(BriefRow::ls_line).collect()
    }

    #[test]
    fn open_briefs_come_in_the_order_of_who_has_to_act() {
        let snapshot = Snapshot::of(
            &project(vec![
                brief("briefs/1-draft.md", "status: draft\n"),
                brief("briefs/2-queued.md", "status: queued\n"),
                brief("briefs/3-reported.md", "status: reported\n"),
                brief("briefs/4-blocked.md", "status: blocked\n"),
                brief("briefs/5-started.md", "status: started\n"),
                brief("briefs/6-no-status.md", "# no header\n"),
                brief("briefs/7-typo.md", "status: strated\n"),
                brief("briefs/8-filed-by-hand.md", "status: filed\n"),
                brief("briefs/9-started.md", "status: started\n"),
                brief("briefs/done/10-old.md", "# no header\n"),
                brief("briefs/done/007-older.md", "status: started\n"),
            ]),
            &BTreeSet::new(),
        );
        assert_eq!(
            listed(snapshot.briefs(None)),
            [
                "5 started started",
                "9 started started",
                "4 blocked blocked",
                "3 reported reported",
                "2 queued queued",
                "1 draft draft",
                "6 unknown no-status",
                "7 unknown typo",
                "8 unknown filed-by-hand",
            ]
        );
        assert_eq!(
            listed(snapshot.briefs(Some(Shown::Status(Status::Started)))),
            ["5 started started", "9 started started"]
        );
        assert_eq!(
            listed(snapshot.briefs(Some(Shown::Status(Status::Filed)))),
            ["7 filed older", "10 filed old"]
        );
        assert_eq!(
            listed(snapshot.briefs(Some(Shown::Status(Status::Blocked)))).len(),
            1
        );
        assert_eq!(listed(snapshot.briefs(Some(Shown::Unpushed))).len(), 0);
    }

    /// A filed brief with unpushed commits is among the open ones, after
    /// what waits for the planner and before what waits for the worker; an
    /// open brief in the set is not labelled (only a filed one is), and a
    /// number that names no brief changes nothing.
    #[test]
    fn an_unpushed_brief_is_listed_with_the_open_ones() {
        let snapshot = Snapshot::of(
            &project(vec![
                brief("briefs/2-queued.md", "status: queued\n"),
                brief("briefs/3-reported.md", "status: reported\n"),
                brief("briefs/5-started.md", "status: started\n"),
                brief("briefs/done/10-old.md", "status: filed\n"),
                brief("briefs/done/12-newer.md", "status: filed\n"),
                brief("briefs/done/120-other.md", "status: filed\n"),
            ]),
            &[2, 12, 99].map(BriefId::new).into(),
        );
        assert_eq!(
            listed(snapshot.briefs(None)),
            [
                "5 started started",
                "3 reported reported",
                "12 unpushed newer",
                "2 queued queued"
            ]
        );
        assert_eq!(
            listed(snapshot.briefs(Some(Shown::Unpushed))),
            ["12 unpushed newer"]
        );
        assert_eq!(
            listed(snapshot.briefs(Some(Shown::Status(Status::Filed)))),
            ["10 filed old", "120 filed other"]
        );
        assert_eq!(
            listed(snapshot.briefs(Some(Shown::Status(Status::Queued)))),
            ["2 queued queued"]
        );
    }

    #[test]
    fn a_trailer_names_a_brief_on_a_line_of_its_own() {
        let named = |message: &str| -> Vec<u32> {
            trailers(message.as_bytes()).map(BriefId::get).collect()
        };
        assert_eq!(named("Fix\n\nBrief: 12\n"), [12]);
        assert_eq!(named("Brief: 12 \t\r\nBrief: 3"), [12, 3]);
        for not_one in [
            "Brief: 012",
            "Brief:12",
            " Brief: 12",
            "Brief: 12a",
            "Brief: 1 2",
            "See Brief: 12",
            "Brief: ",
        ] {
            assert_eq!(named(not_one), Vec::<u32>::new(), "{not_one:?}");
        }
    }

    #[test]
    fn a_kind_lists_its_groups_by_name_and_its_items_by_id() {
        let mut project = Project::empty(ProjectName::parse("shop").unwrap());
        for (number, text) in [
            (1, "id: c1\ngroup: Work\n\n# one\n"),
            (2, "id: c2\ngroup: Home\ncheck: x\nfrom: 126\n\n# two.\n"),
            (3, "id: c3\ngroup: Home\n\n# three\n\nbody\n"),
            (4, "id: c4\ngroup: Home\n\n# four\n"),
        ] {
            project
                .list_mut(ListKind::Chores)
                .insert(number, crate::item::Item::parse(text.as_bytes()));
        }
        assert_eq!(
            list_rows(&project, ListKind::Chores),
            [
                ListRow::Group(b"Home".to_vec()),
                ListRow::Item(b"c2 two. Check: x  (126)".to_vec()),
                ListRow::Item(b"c3 three".to_vec()),
                ListRow::Item(b"c4 four".to_vec()),
                ListRow::Group(b"Work".to_vec()),
                ListRow::Item(b"c1 one".to_vec()),
            ]
        );
        project.list_mut(ListKind::Backlog).insert(
            41,
            crate::item::Item::parse(b"id: b41\ngroup: Default\nfrom: 126\n\n# Later\n"),
        );
        assert_eq!(
            list_rows(&project, ListKind::Backlog),
            [
                ListRow::Group(b"Default".to_vec()),
                ListRow::Item(b"b41 Later  (from 126)".to_vec())
            ]
        );
    }

    fn any_status_line() -> impl Strategy<Value = &'static str> {
        prop::sample::select(vec![
            "status: draft\n",
            "status: queued\n",
            "status: started\n",
            "status: blocked\n",
            "status: reported\n",
            "status: filed\n",
            "status: other\n",
            "",
        ])
    }

    proptest! {
        /// Every brief is on the board once, in one of the two lists, and
        /// each status lists exactly its briefs.
        #[test]
        fn every_brief_is_listed_once(
            briefs in prop::collection::btree_map(
                (0u32..40, any::<bool>()),
                any_status_line(),
                0..12,
            ),
            unpushed in prop::collection::btree_set(0u32..40, 0..6),
        ) {
            let files: Vec<BriefFile> = briefs
                .iter()
                .map(|((number, filed), status)| {
                    let done = if *filed { "done/" } else { "" };
                    brief(&format!("briefs/{done}{number}-x.md"), status)
                })
                .collect();
            let set: BTreeSet<BriefId> = unpushed.iter().copied().map(BriefId::new).collect();
            let snapshot = Snapshot::of(&project(files), &set);

            let numbers = |rows: Vec<&BriefRow>| -> Vec<u32> {
                rows.iter().map(|row| row.number.get()).collect()
            };
            let mut open = numbers(snapshot.briefs(None).collect());
            let filed = numbers(snapshot.briefs(Some(Shown::Status(Status::Filed))).collect());
            // Open: outside `briefs/done/`, or in it and unpushed.
            let expected = |want: bool| -> Vec<u32> {
                briefs
                    .keys()
                    .filter(|(n, filed)| (!*filed || unpushed.contains(n)) != want)
                    .map(|(n, _)| *n)
                    .collect()
            };
            prop_assert_eq!(&filed, &expected(true));
            let labelled = numbers(snapshot.briefs(Some(Shown::Unpushed)).collect());
            let want: Vec<u32> = briefs
                .keys()
                .filter(|(n, filed)| *filed && unpushed.contains(n))
                .map(|(n, _)| *n)
                .collect();
            let mut got = labelled.clone();
            got.sort_unstable();
            prop_assert_eq!(got, want);

            // In board order: by rank, then by number.
            let keys: Vec<(u8, u32)> = snapshot
                .briefs(None)
                .map(|row| (row.rank(), row.number.get()))
                .collect();
            prop_assert!(keys.is_sorted(), "{:?}", keys);
            open.sort_unstable();
            prop_assert_eq!(&open, &expected(false));

            // The statuses and `unpushed` split the open briefs; what none
            // of them lists is shown as unknown.
            let mut by_status = labelled.len();
            for status in Status::ALL {
                let rows: Vec<&BriefRow> = snapshot.briefs(Some(Shown::Status(status))).collect();
                prop_assert!(rows.iter().all(|row| row.status == Some(status) && !row.unpushed));
                if status != Status::Filed {
                    by_status += rows.len();
                }
            }
            let unknown = snapshot.briefs(None).filter(|row| row.status.is_none()).count();
            prop_assert_eq!(by_status + unknown, open.len());
            prop_assert!(snapshot.open.iter().all(|row| row.status != Some(Status::Filed) || row.unpushed));
            prop_assert!(snapshot.filed.iter().all(|row| row.status == Some(Status::Filed) && !row.unpushed));
        }
    }
}
