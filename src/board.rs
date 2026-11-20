//! One project's board: a method per command.

use std::path::PathBuf;

use crate::brief::Status;
use crate::error::Error;
use crate::id::{BriefId, ItemId, ListKind};
use crate::lint::{Finding, lint};
use crate::list::{Entry, FindError, ListFile};
use crate::project::{BriefFile, Env, Project, ProjectName, resolve};

pub struct Board {
    root: PathBuf,
    project: ProjectName,
}

impl Board {
    /// Finds the project and the board-data clone; reads nothing yet.
    pub fn open(env: Env) -> Result<Board, Error> {
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
            root: env.root,
            project,
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
            ItemId::Brief(number) => {
                let mut files = project
                    .briefs()
                    .iter()
                    .filter(|file| file.name.number() == number);
                match (files.next(), files.count()) {
                    (None, _) => Err(refused("no such brief".to_owned())),
                    (Some(file), 0) => Ok(file.brief.to_bytes()),
                    (Some(_), more) => Err(refused(format!(
                        "{number} is on {} files; fix the duplicate first",
                        more + 1
                    ))),
                }
            }
            ItemId::Chore(chore) => item(project.list(ListKind::Chores), chore.get())
                .map_err(|why| refused(why.unwrap_or_else(|| "no such chore".to_owned()))),
            ItemId::Backlog(backlog) => item(project.list(ListKind::Backlog), backlog.get())
                .map_err(|why| refused(why.unwrap_or_else(|| "no such backlog item".to_owned()))),
        }
    }

    /// The findings of this project.
    pub fn lint(&self) -> Result<Vec<Finding>, Error> {
        self.load().map(|project| lint(&project))
    }
}

/// The line of the one item with this id. The error is why the id names no
/// single item, `None` when it is on no line at all (the caller has the
/// word for the kind of item).
fn item(list: &ListFile, number: u32) -> Result<Vec<u8>, Option<String>> {
    match list.find(number) {
        Ok(text) => Ok([text, b"\n"].concat()),
        Err(FindError::Missing) => Err(None),
        Err(FindError::Duplicate(lines)) => Err(Some(format!(
            "{}{number} is on {lines} lines; fix the duplicate first",
            list.kind().prefix()
        ))),
    }
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
