//! One project's board: a method per command.

use std::path::PathBuf;

use crate::brief::{EntryText, Stamp, Status, Text};
use crate::error::Error;
use crate::git::{Failure, Git, Oid};
use crate::id::{BacklogId, BriefId, ChoreId, ItemId, ListKind};
use crate::lint::{Finding, lint};
use crate::list::{Entry, Line, ListFile};
use crate::ops::{self, Commits, Filed, NewChore};
use crate::project::{BoardPath, BriefFile, Env, Project, ProjectName, Slug, resolve};
use crate::sync::{self, Note};

pub struct Board {
    /// Absolute: a path the board prints is one an agent can open from
    /// anywhere.
    root: PathBuf,
    /// Where the command runs: the project repository whose `HEAD` a new
    /// brief is based on, also when `--board` names the project.
    cwd: PathBuf,
    project: ProjectName,
    /// Where a write says what happens along the way. A note is passed on
    /// as it comes: `waiting for lock` has to show before the wait.
    notes: Box<dyn Fn(Note)>,
}

impl Board {
    /// Finds the project and the board-data clone; reads nothing yet.
    pub fn open(env: Env, notes: Box<dyn Fn(Note)>) -> Result<Board, Error> {
        let project = resolve(&env)?;
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
            notes,
        })
    }

    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    fn load(&self) -> Result<Project, Error> {
        Project::load(&self.root, &self.project)
    }

    /// The board as `ls` prints it.
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.load().map(|project| Snapshot::of(&project))
    }

    /// What `show` prints: a brief's file as it is, open or filed, or an
    /// item's line without the `- `.
    pub fn show(&self, id: ItemId) -> Result<Vec<u8>, Error> {
        let project = self.load()?;
        let refused = |why: String| Error::Refused(format!("show {id}: {why}"));
        match id {
            ItemId::Brief(number) => project
                .find_brief(number)
                .map(|file| file.brief.to_bytes())
                .map_err(|why| refused(ops::no_brief(number, why))),
            ItemId::Chore(chore) => {
                item(project.list(ListKind::Chores), chore.get()).map_err(refused)
            }
            ItemId::Backlog(backlog) => {
                item(project.list(ListKind::Backlog), backlog.get()).map_err(refused)
            }
        }
    }

    /// The findings of this project.
    pub fn lint(&self) -> Result<Vec<Finding>, Error> {
        self.load().map(|project| lint(&project))
    }

    /// Adds a chore under its group; gives its new id.
    pub fn add_chore(&self, new: &NewChore) -> Result<ChoreId, Error> {
        self.command("add chore", &|tx| {
            let id = tx.alloc_chore()?;
            ops::add_chore(&mut tx.project, id, new)?;
            Ok((id, format!("add {id}")))
        })
    }

    /// Adds a backlog item below the others; gives its new id.
    pub fn add_backlog(&self, text: &Line, from: Option<ItemId>) -> Result<BacklogId, Error> {
        self.command("add backlog", &|tx| {
            let id = tx.alloc_backlog()?;
            ops::add_backlog(&mut tx.project, id, text, from);
            Ok((id, format!("add {id}")))
        })
    }

    /// Deletes chores' lines, all or none; gives what it did, as `done c12
    /// c13`.
    pub fn done(&self, ids: &[ChoreId]) -> Result<String, Error> {
        self.command("done", &|tx| {
            let done = ops::done(&mut tx.project, ids)?;
            Ok((done.clone(), done))
        })
    }

    /// Deletes backlog items' lines, all or none; gives what it did, as
    /// `dropped b41`.
    pub fn drop_items(&self, ids: &[BacklogId]) -> Result<String, Error> {
        self.command("drop", &|tx| {
            let dropped = ops::drop_items(&mut tx.project, ids)?;
            Ok((dropped.clone(), dropped))
        })
    }

    /// Adds the project's next brief, its header written; gives the brief's
    /// path. The base is the commit the project repository is on now.
    pub fn new_brief(&self, slug: &Slug) -> Result<PathBuf, Error> {
        let base = self.project_head()?;
        let now = now()?;
        let path = self.command("new brief", &|tx| {
            let number = tx.alloc_brief()?;
            let path = ops::new_brief(&mut tx.project, number, slug, &base, now);
            Ok((path, format!("new brief {number}-{slug}")))
        })?;
        Ok(self.path(&path))
    }

    /// Queues a draft, writing the backlog items it closes when `closes` is
    /// given.
    pub fn queue(&self, id: BriefId, closes: Option<&[BacklogId]>) -> Result<(), Error> {
        let now = now()?;
        self.command("queue", &|tx| {
            let queued = ops::queue(&mut tx.project, id, closes, now)?;
            Ok(((), queued))
        })
    }

    /// Claims the brief queued longest; gives its path, `None` when the queue
    /// is empty.
    ///
    /// Only with the remote: a claim is shared like an id, and two clones
    /// claiming one brief offline would each hand it to a worker.
    pub fn next(&self) -> Result<Option<PathBuf>, Error> {
        let now = now()?;
        let path = self.command("next", &|tx| {
            tx.require_remote()?;
            Ok(match ops::next(&mut tx.project, now)? {
                Some((number, path)) => (Some(path), format!("started {number}")),
                // Nothing changed, so nothing is committed under a subject.
                None => (None, String::new()),
            })
        })?;
        Ok(path.map(|path| self.path(&path)))
    }

    /// Blocks a started brief on a question, which goes under
    /// `## Questions`.
    pub fn block(&self, id: BriefId, question: &Text) -> Result<(), Error> {
        let question = entry(question, &format!("block {id}"))?;
        let now = now()?;
        self.command("block", &|tx| {
            let blocked = ops::block(&mut tx.project, id, &question, now)?;
            Ok(((), blocked))
        })
    }

    /// Appends an amendment under `## Amendments`; a blocked brief resumes.
    /// Gives what it did, as `amended 128, resumed`.
    pub fn amend(&self, id: BriefId, text: &Text) -> Result<String, Error> {
        let text = entry(text, &format!("amend {id}"))?;
        let now = now()?;
        self.command("amend", &|tx| {
            let amended = ops::amend(&mut tx.project, id, &text, now)?;
            Ok((amended, format!("amended {id}")))
        })
    }

    /// Makes the text the started brief's `## Report` and reports it.
    pub fn report(&self, id: BriefId, text: &Text) -> Result<(), Error> {
        let now = now()?;
        self.command("report", &|tx| {
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
        };
        let now = now()?;
        self.command("file", &|tx| {
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
        // names nothing there, and rev-list would fail on it.
        let count = match git.commit("HEAD").map_err(git_error)? {
            None => 0,
            Some(_) => git
                .count_matching_head(&format!("^Brief: {number}[[:space:]]*$"))
                .map_err(git_error)?,
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
    pub fn sync(&self) -> Result<(), Error> {
        sync::write::<()>(&self.root, &self.project, "sync", &*self.notes, None).map(drop)
    }

    /// A write that runs a command; `what` names it in an error.
    fn command<T>(&self, what: &str, apply: sync::Apply<'_, T>) -> Result<T, Error> {
        let value = sync::write(&self.root, &self.project, what, &*self.notes, Some(apply))?;
        Ok(value.expect("a write that runs a command gives its value"))
    }
}

/// Whether `file` checks that the brief has commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitCheck {
    /// At least one commit with the brief's trailer, or no filing.
    Trailer,
    /// `--no-commits`: the brief changed nothing in the repository.
    Skip,
}

/// The time a write stamps, taken once, before the write: every round of it
/// writes the same.
fn now() -> Result<Stamp, Error> {
    Stamp::now()
        .ok_or_else(|| Error::Setup("the clock is outside the years 1970 to 9999".to_owned()))
}

/// The text of a question or an amendment, refused before the write when it
/// would not stay in its section: no state of the board makes it fit.
fn entry(text: &Text, what: &str) -> Result<EntryText, Error> {
    EntryText::new(text.clone()).map_err(|why| Error::Refused(format!("{what}: {why}")))
}

/// The line of the one item with this id, else why the id names no single
/// item.
fn item(list: &ListFile, number: u32) -> Result<Vec<u8>, String> {
    list.find(number)
        .map(|text| [text, b"\n"].concat())
        .map_err(|why| ops::not_one(list.kind(), number, why))
}

/// What the board shows of a project: the briefs, the chores by group, the
/// backlog in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub project: ProjectName,
    /// The briefs outside `briefs/done/`, in the order of [`BriefRow::rank`],
    /// then by number.
    pub open: Vec<BriefRow>,
    /// The briefs in `briefs/done/`, by number.
    pub filed: Vec<BriefRow>,
    pub chores: Vec<ListRow>,
    pub backlog: Vec<ListRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefRow {
    pub number: BriefId,
    /// `None` for a brief whose status is missing or unknown; lint says
    /// which.
    pub status: Option<Status>,
    pub slug: String,
}

impl BriefRow {
    fn of(file: &BriefFile) -> BriefRow {
        BriefRow {
            number: file.name.number(),
            status: file.status().ok(),
            slug: file.name.slug().to_owned(),
        }
    }

    /// Where the row stands among the briefs: what is in flight first
    /// (started, blocked), then what waits for the planner (reported), then
    /// what waits for the worker (queued), then what is still being written.
    /// A brief without a usable status comes last, where it cannot push the
    /// brief in flight out of sight.
    fn rank(&self) -> u8 {
        match self.status {
            Some(Status::Started) => 0,
            Some(Status::Blocked) => 1,
            Some(Status::Reported) => 2,
            Some(Status::Queued) => 3,
            Some(Status::Draft) => 4,
            None => 5,
            Some(Status::Filed) => 6,
        }
    }

    /// The word in the status column.
    pub fn status_word(&self) -> &'static str {
        self.status.map_or("unknown", Status::as_str)
    }
}

/// A line of a list that the board shows: free text between the items is
/// the file's, not the board's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    /// A `## group` heading, as its name.
    Group(Vec<u8>),
    /// An item, as its line without the `- `: the id, when it has one, is
    /// the first word.
    Item(Vec<u8>),
}

impl Snapshot {
    pub fn of(project: &Project) -> Snapshot {
        let rows = |filed: bool| {
            project
                .briefs()
                .iter()
                .filter(move |file| file.filed == filed)
                .map(BriefRow::of)
        };
        let mut open: Vec<BriefRow> = rows(false).collect();
        open.sort_by_key(|row| (row.rank(), row.number));
        let mut filed: Vec<BriefRow> = rows(true).collect();
        filed.sort_by_key(|row| row.number);
        Snapshot {
            project: project.name().clone(),
            open,
            filed,
            chores: list_rows(project.list(ListKind::Chores)),
            backlog: list_rows(project.list(ListKind::Backlog)),
        }
    }

    /// The briefs `ls` lists: the open ones, or those with one status.
    /// `filed` is the briefs in `briefs/done/`.
    pub fn briefs(&self, status: Option<Status>) -> impl Iterator<Item = &BriefRow> {
        let rows = match status {
            Some(Status::Filed) => &self.filed,
            Some(_) | None => &self.open,
        };
        rows.iter()
            .filter(move |row| status.is_none() || row.status == status)
    }

    pub fn list(&self, kind: ListKind) -> &[ListRow] {
        match kind {
            ListKind::Chores => &self.chores,
            ListKind::Backlog => &self.backlog,
        }
    }
}

fn list_rows(list: &ListFile) -> Vec<ListRow> {
    list.entries()
        .filter_map(|entry| match entry {
            Entry::Heading(name) => Some(ListRow::Group(name.to_vec())),
            Entry::Item { text, .. } => Some(ListRow::Item(text.to_vec())),
            Entry::Text(_) => None,
        })
        .collect()
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
            Some(BoardPath::Brief { name, filed, .. }) => BriefFile {
                name,
                filed,
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

    fn shown<'a>(rows: impl Iterator<Item = &'a BriefRow>) -> Vec<String> {
        rows.map(|row| format!("{} {} {}", row.number, row.status_word(), row.slug))
            .collect()
    }

    #[test]
    fn open_briefs_come_in_the_order_of_who_has_to_act() {
        let snapshot = Snapshot::of(&project(vec![
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
        ]));
        assert_eq!(
            shown(snapshot.briefs(None)),
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
            shown(snapshot.briefs(Some(Status::Started))),
            ["5 started started", "9 started started"]
        );
        assert_eq!(
            shown(snapshot.briefs(Some(Status::Filed))),
            ["7 filed older", "10 filed old"]
        );
        assert_eq!(shown(snapshot.briefs(Some(Status::Blocked))).len(), 1);
    }

    #[test]
    fn a_list_shows_its_groups_and_items() {
        let chores = ListFile::parse(
            ListKind::Chores,
            b"# Chores\n\nfree text\n##  Home \n- c1 one. Check: x\n  continued\n- no id\n-\n",
        );
        assert_eq!(
            list_rows(&chores),
            [
                ListRow::Group(b"Home".to_vec()),
                ListRow::Item(b"c1 one. Check: x".to_vec()),
                ListRow::Item(b"no id".to_vec()),
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
        ) {
            let files: Vec<BriefFile> = briefs
                .iter()
                .map(|((number, filed), status)| {
                    let done = if *filed { "done/" } else { "" };
                    brief(&format!("briefs/{done}{number}-x.md"), status)
                })
                .collect();
            let snapshot = Snapshot::of(&project(files));

            let numbers = |rows: Vec<&BriefRow>| -> Vec<u32> {
                rows.iter().map(|row| row.number.get()).collect()
            };
            let mut open = numbers(snapshot.briefs(None).collect());
            let filed = numbers(snapshot.briefs(Some(Status::Filed)).collect());
            let expected = |want: bool| -> Vec<u32> {
                briefs.keys().filter(|(_, filed)| *filed == want).map(|(n, _)| *n).collect()
            };
            prop_assert_eq!(&filed, &expected(true));

            // In board order: by rank, then by number.
            let keys: Vec<(u8, u32)> = snapshot
                .briefs(None)
                .map(|row| (row.rank(), row.number.get()))
                .collect();
            prop_assert!(keys.is_sorted(), "{:?}", keys);
            open.sort_unstable();
            prop_assert_eq!(&open, &expected(false));

            // The statuses split the open briefs; what none of them lists
            // is shown as unknown.
            let mut by_status = 0;
            for status in Status::ALL {
                let rows: Vec<&BriefRow> = snapshot.briefs(Some(status)).collect();
                prop_assert!(rows.iter().all(|row| row.status == Some(status)));
                if status != Status::Filed {
                    by_status += rows.len();
                }
            }
            let unknown = snapshot.briefs(None).filter(|row| row.status.is_none()).count();
            prop_assert_eq!(by_status + unknown, open.len());
            prop_assert!(snapshot.open.iter().all(|row| row.status != Some(Status::Filed)));
            prop_assert!(snapshot.filed.iter().all(|row| row.status == Some(Status::Filed)));
        }
    }
}
