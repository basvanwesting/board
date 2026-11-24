//! What each write command does to a project, given the ids it was
//! allocated: no git and no files, so every rule of a command is tested
//! without either. The write loop in `sync.rs` loads the project, runs one of
//! these and commits what it changed.

use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use crate::brief::{
    BadStatus, Brief, EntryText, NotABacklogId, Stamp, Status, Text, Transition, TransitionError,
};
use crate::error::Error;
use crate::git::Oid;
use crate::id::{BacklogId, BriefId, ChoreId, ItemId, ListKind};
use crate::list::{FindError, Line, Move, MoveError, NoCheck, RemoveError};
use crate::project::{BoardPath, BriefFile, BriefName, Project, Slug};

/// The sections a brief needs text in before it is queued: what the worker
/// works from. Their template is the planner skill's, not the tool's (the
/// agents' rules stay where the owner reviews them); the tool holds the
/// names, because a worker that claims a brief without a design or a
/// verification improvises.
const REQUIRED_SECTIONS: [&str; 7] = [
    "Task",
    "Design",
    "Premises",
    "Out of scope",
    "Verification",
    "Not doable by the worker",
    "Worker protocol",
];

/// Adds brief `number`, header only; gives its path.
pub(crate) fn new_brief(
    project: &mut Project,
    number: BriefId,
    slug: &Slug,
    base: &Oid,
    now: Stamp,
) -> BoardPath {
    let file = BriefFile {
        name: BriefName::new(number, slug),
        filed: false,
        brief: Brief::new(number, base.as_str(), now),
    };
    let path = file.path(project.name());
    project.create_brief(file);
    path
}

/// Queues a draft whose required sections hold text, and writes the backlog
/// items it closes when `closes` is given; gives what `queue` prints.
pub(crate) fn queue(
    project: &mut Project,
    number: BriefId,
    closes: Option<&[BacklogId]>,
    now: Stamp,
) -> Result<String, Error> {
    let refused = |why: String| Error::Refused(format!("queue {number}: {why}"));
    let file = project
        .find_brief(number)
        .map_err(|why| refused(no_brief(number, why)))?;
    match file.status() {
        Ok(Status::Draft) => {}
        Ok(status) => return Err(refused(format!("it is {status}, not draft"))),
        Err(bad) => return Err(refused(bad_status(&bad))),
    }
    let empty: Vec<&str> = REQUIRED_SECTIONS
        .into_iter()
        .filter(|name| !file.brief.section(name).is_some_and(|s| s.has_text()))
        .collect();
    if !empty.is_empty() {
        return Err(refused(format!(
            "no text under ## {}; every one of {} needs a line",
            empty.join(", ## "),
            REQUIRED_SECTIONS.join(", ")
        )));
    }
    // An id given twice is one item.
    let closes = closes.map(|ids| {
        let mut unique: Vec<BacklogId> = Vec::with_capacity(ids.len());
        for id in ids {
            if !unique.contains(id) {
                unique.push(*id);
            }
        }
        unique
    });
    for id in closes.iter().flatten() {
        project
            .list(ListKind::Backlog)
            .find(id.get())
            .map_err(|why| {
                refused(format!(
                    "closes {id}: {}",
                    not_one(ListKind::Backlog, id.get(), why)
                ))
            })?;
    }
    let file = project
        .brief_mut(number)
        .map_err(|why| refused(no_brief(number, why)))?;
    if let Some(closes) = &closes {
        file.brief.set_closes(closes);
    }
    file.transition(Transition::Queue, now)
        .map_err(|error| refused(not_moved(&error, Status::Draft)))?;
    Ok(format!("queued {number}"))
}

/// Claims the brief that has waited longest: the earliest `queued:` time,
/// then the lowest number. `None` when nothing is queued.
///
/// A queued brief without a readable `queued:` time (one queued by hand)
/// comes after those with one: it has no place in the order, and the rest
/// keep theirs.
pub(crate) fn next(
    project: &mut Project,
    now: Stamp,
) -> Result<Option<(BriefId, BoardPath)>, Error> {
    let queued_at = |file: &BriefFile| {
        file.brief
            .get("queued")
            .and_then(|value| str::from_utf8(value).ok()?.parse::<Stamp>().ok())
    };
    let first = project
        .briefs()
        .iter()
        .filter(|file| file.status() == Ok(Status::Queued))
        .min_by_key(|file| {
            let at = queued_at(file);
            (at.is_none(), at, file.name.number())
        });
    let Some(number) = first.map(|file| file.name.number()) else {
        return Ok(None);
    };
    let refused = |why: String| Error::Refused(format!("next: {why}"));
    let name = project.name().clone();
    let file = project
        .brief_mut(number)
        .map_err(|why| refused(no_brief(number, why)))?;
    file.transition(Transition::Claim, now)
        .map_err(|error| refused(not_moved(&error, Status::Queued)))?;
    Ok(Some((number, file.path(&name))))
}

/// The tool's sections, each the place of one command's entries.
const QUESTIONS: &str = "Questions";
const AMENDMENTS: &str = "Amendments";

/// Blocks a started brief on a question, appended under `## Questions`.
pub(crate) fn block(
    project: &mut Project,
    number: BriefId,
    question: &EntryText,
    now: Stamp,
) -> Result<String, Error> {
    let refused = |why: String| Error::Refused(format!("block {number}: {why}"));
    let file = started(project, number).map_err(refused)?;
    file.brief.append_entry(QUESTIONS, now, question);
    file.transition(Transition::Block, now)
        .map_err(|error| refused(not_moved(&error, Status::Started)))?;
    Ok(format!("blocked {number}"))
}

/// Appends an amendment under `## Amendments` to a brief the worker has or
/// will have; a blocked brief resumes, as the amendment is the answer it
/// waits for. Gives what `amend` prints.
///
/// A draft is still the planner's to edit, and a reported brief is done:
/// what is decided after a report is the next brief's.
pub(crate) fn amend(
    project: &mut Project,
    number: BriefId,
    text: &EntryText,
    now: Stamp,
) -> Result<String, Error> {
    let refused = |why: String| Error::Refused(format!("amend {number}: {why}"));
    let file = project
        .find_brief(number)
        .map_err(|why| refused(no_brief(number, why)))?;
    let resume = match file.status() {
        Ok(Status::Queued | Status::Started) => false,
        Ok(Status::Blocked) => true,
        Ok(status @ (Status::Draft | Status::Reported | Status::Filed)) => {
            return Err(refused(format!(
                "it is {status}, not queued, started or blocked"
            )));
        }
        Err(bad) => return Err(refused(bad_status(&bad))),
    };
    let file = project
        .brief_mut(number)
        .map_err(|why| refused(no_brief(number, why)))?;
    file.brief.append_entry(AMENDMENTS, now, text);
    if resume {
        file.transition(Transition::Resume, now)
            .map_err(|error| refused(not_moved(&error, Status::Blocked)))?;
        Ok(format!("amended {number}, resumed"))
    } else {
        Ok(format!("amended {number}"))
    }
}

/// Makes the text the started brief's `## Report` and reports it.
pub(crate) fn report(
    project: &mut Project,
    number: BriefId,
    text: &Text,
    now: Stamp,
) -> Result<String, Error> {
    let refused = |why: String| Error::Refused(format!("report {number}: {why}"));
    let file = started(project, number).map_err(refused)?;
    file.brief.set_report(text);
    file.transition(Transition::Report, now)
        .map_err(|error| refused(not_moved(&error, Status::Started)))?;
    Ok(format!("reported {number}"))
}

/// What `file` knows of a brief's commits in the project repository,
/// counted before the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Commits {
    /// `--no-commits`: the brief changed nothing in the repository.
    Skipped,
    /// The commits reachable from `HEAD` of `repository` that carry the
    /// brief's trailer.
    Counted { count: usize, repository: PathBuf },
    /// The command runs at `at`, in no repository: git's reason. Told only
    /// after the brief's status, by `file`.
    NoRepository { at: PathBuf, said: String },
}

/// What `file` did; prints as `filed 128 (3 commits) closed b41 b52`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filed {
    pub number: BriefId,
    /// `None` for a brief filed with `--no-commits`. Never zero: a brief
    /// without a commit is not filed.
    pub commits: Option<NonZeroUsize>,
    /// The backlog items whose lines were deleted.
    pub closed: Vec<BacklogId>,
}

impl fmt::Display for Filed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "filed {} (", self.number)?;
        match self.commits {
            None => f.write_str("no commits")?,
            Some(n) if n.get() == 1 => f.write_str("1 commit")?,
            Some(n) => write!(f, "{n} commits")?,
        }
        f.write_str(")")?;
        if !self.closed.is_empty() {
            let ids: Vec<String> = self.closed.iter().map(BacklogId::to_string).collect();
            write!(f, " closed {}", ids.join(" "))?;
        }
        Ok(())
    }
}

/// Files a reported brief: deletes the backlog lines its `closes:` names,
/// stamps it filed and moves it to `briefs/done/`, all in one commit.
///
/// The commits are checked after the status, so a brief that is not
/// reported says so rather than that it has no commits.
///
/// An item `closes:` names that is gone already (dropped meanwhile, or
/// deleted by hand) is skipped: the brief is what is filed, and the item is
/// closed either way. One on two lines is refused, as everywhere: deleting
/// both would guess which one the planner meant.
pub(crate) fn file(
    project: &mut Project,
    number: BriefId,
    commits: &Commits,
    now: Stamp,
) -> Result<Filed, Error> {
    let refused = |why: String| Error::Refused(format!("file {number}: {why}"));
    let file = project
        .find_brief(number)
        .map_err(|why| refused(no_brief(number, why)))?;
    match file.status() {
        Ok(Status::Reported) => {}
        Ok(status) => return Err(refused(format!("it is {status}, not reported"))),
        Err(bad) => return Err(refused(bad_status(&bad))),
    }
    let commits = match commits {
        Commits::Skipped => None,
        Commits::Counted { count, repository } => {
            Some(NonZeroUsize::new(*count).ok_or_else(|| {
                refused(format!(
                    "no commit reachable from HEAD in {} has the trailer `Brief: {number}`; \
                     the worker puts it on every commit of the brief \
                     (--no-commits files a brief that changed nothing there)",
                    repository.display()
                ))
            })?)
        }
        Commits::NoRepository { at, said } => {
            return Err(Error::Usage(format!(
                "file {number}: no repository at {} to count the brief's commits in ({said}); \
                 run it in the project's checkout, or file with --no-commits",
                at.display()
            )));
        }
    };
    let closes = file.brief.closes().map_err(|NotABacklogId(word)| {
        refused(format!(
            "its `closes:` names `{}`, which is no backlog id; fix the header first",
            word.escape_debug()
        ))
    })?;
    let mut closed = Vec::new();
    for id in closes {
        match project.list(ListKind::Backlog).find(id.get()) {
            Ok(_) => closed.push(id),
            Err(FindError::Missing) => {}
            Err(why @ FindError::Duplicate(_)) => {
                return Err(refused(format!(
                    "closes {id}: {}",
                    not_one(ListKind::Backlog, id.get(), why)
                )));
            }
        }
    }
    // Only a list that loses a line is touched: a brief that closes nothing
    // must not write a backlog file the project does not have.
    if !closed.is_empty() {
        let numbers: Vec<u32> = closed.iter().map(|id| id.get()).collect();
        project
            .list_mut(ListKind::Backlog)
            .remove(&numbers)
            .expect("each id is on one line, checked above");
    }
    let file = project
        .brief_mut(number)
        .map_err(|why| refused(no_brief(number, why)))?;
    file.transition(Transition::File, now)
        .map_err(|error| refused(not_moved(&error, Status::Reported)))?;
    Ok(Filed {
        number,
        commits,
        closed,
    })
}

/// The brief to edit, when it is started; else why not. Nothing is changed
/// before it is known to be started.
fn started(project: &mut Project, number: BriefId) -> Result<&mut BriefFile, String> {
    let file = project
        .find_brief(number)
        .map_err(|why| no_brief(number, why))?;
    match file.status() {
        Ok(Status::Started) => {}
        Ok(status) => {
            return Err(not_moved(
                &TransitionError::NotFrom(status),
                Status::Started,
            ));
        }
        Err(bad) => return Err(bad_status(&bad)),
    }
    project
        .brief_mut(number)
        .map_err(|why| no_brief(number, why))
}

/// Why a brief number names no single brief, as every command that takes one
/// says it.
pub(crate) fn no_brief(number: BriefId, why: FindError) -> String {
    match why {
        FindError::Missing => "no such brief".to_owned(),
        FindError::Duplicate(files) => {
            format!("{number} is on {files} files; fix the duplicate first")
        }
    }
}

/// Why a brief cannot make a move that needs it to be `wanted`.
fn not_moved(error: &TransitionError, wanted: Status) -> String {
    match error {
        TransitionError::NotFrom(status) => format!("it is {status}, not {wanted}"),
        TransitionError::BadStatus(bad) => bad_status(bad),
    }
}

fn bad_status(bad: &BadStatus) -> String {
    match bad {
        BadStatus::Missing => "it has no `status:` line".to_owned(),
        BadStatus::Unknown(found) => format!("its status `{}` is unknown", found.escape_debug()),
    }
}

/// A chore as `add chore` is given it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChore {
    pub text: Line,
    pub group: Line,
    pub check: Line,
    pub from: Option<ItemId>,
}

pub(crate) fn add_chore(project: &mut Project, id: ChoreId, new: &NewChore) -> Result<(), Error> {
    project
        .list_mut(ListKind::Chores)
        .add_chore(id, &new.group, &new.text, &new.check, new.from)
        .map_err(|NoCheck| {
            Error::Refused(format!(
                "add chore: `{}` does not say how to see that the chore is done",
                new.check
            ))
        })
}

pub(crate) fn add_backlog(project: &mut Project, id: BacklogId, text: &Line, from: Option<ItemId>) {
    project
        .list_mut(ListKind::Backlog)
        .add_backlog(id, text, from);
}

/// Deletes the chores' lines, all or none; gives what `done` prints.
pub(crate) fn done(project: &mut Project, ids: &[ChoreId]) -> Result<String, Error> {
    let numbers: Vec<u32> = ids.iter().map(|id| id.get()).collect();
    remove(project, ListKind::Chores, &numbers, ("done", "done"))
}

/// Deletes the backlog items' lines, all or none; gives what `drop` prints.
pub(crate) fn drop_items(project: &mut Project, ids: &[BacklogId]) -> Result<String, Error> {
    let numbers: Vec<u32> = ids.iter().map(|id| id.get()).collect();
    remove(project, ListKind::Backlog, &numbers, ("drop", "dropped"))
}

/// Moves an item next to another of its list; gives what `move` prints, as
/// `moved c12 before c3`.
///
/// An item already where it is asked to be changes no file, so the write
/// makes no commit and the output is the same: what was asked is so.
pub(crate) fn move_item(project: &mut Project, wanted: &Move) -> Result<String, Error> {
    let kind = wanted.kind();
    let refused = |why: String| Error::Refused(format!("move {}: {why}", wanted.item()));
    let mut list = project.list(kind).clone();
    list.move_item(wanted).map_err(|error| match error {
        MoveError::Item(why) => refused(not_one(kind, wanted.item().get(), why)),
        MoveError::Target(why) => refused(format!(
            "{} {}: {}",
            wanted.side(),
            wanted.target(),
            not_one(kind, wanted.target().get(), why)
        )),
    })?;
    if list != *project.list(kind) {
        *project.list_mut(kind) = list;
    }
    Ok(format!("moved {wanted}"))
}

/// `(command, past)`: the command names a refusal, the past tense what was
/// done.
fn remove(
    project: &mut Project,
    kind: ListKind,
    numbers: &[u32],
    (command, past): (&str, &str),
) -> Result<String, Error> {
    // An id given twice is one line to delete and one word of the output.
    let mut unique: Vec<u32> = Vec::with_capacity(numbers.len());
    for number in numbers {
        if !unique.contains(number) {
            unique.push(*number);
        }
    }
    project
        .list_mut(kind)
        .remove(&unique)
        .map_err(|RemoveError { number, why }| {
            Error::Refused(format!(
                "{command} {}{number}: {}",
                kind.prefix(),
                not_one(kind, number, why)
            ))
        })?;
    let ids: Vec<String> = unique
        .iter()
        .map(|number| format!("{}{number}", kind.prefix()))
        .collect();
    Ok(format!("{past} {}", ids.join(" ")))
}

/// Why a list id names no single item, as every command that takes one says
/// it.
pub(crate) fn not_one(kind: ListKind, number: u32, why: FindError) -> String {
    match why {
        FindError::Missing => match kind {
            ListKind::Chores => "no such chore".to_owned(),
            ListKind::Backlog => "no such backlog item".to_owned(),
        },
        FindError::Duplicate(lines) => format!(
            "{}{number} is on {lines} lines; fix the duplicate first",
            kind.prefix()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::ListFile;
    use crate::project::{BoardPath, ProjectName};

    fn project(chores: &str, backlog: &str) -> Project {
        Project {
            chores: ListFile::parse(ListKind::Chores, chores.as_bytes()),
            backlog: ListFile::parse(ListKind::Backlog, backlog.as_bytes()),
            ..Project::empty(ProjectName::parse("shop").unwrap())
        }
    }

    fn chore(number: u32) -> ChoreId {
        ChoreId::new(number).unwrap()
    }

    fn backlog(number: u32) -> BacklogId {
        BacklogId::new(number).unwrap()
    }

    fn line(text: &str) -> Line {
        Line::new(text).unwrap()
    }

    fn changed(project: &Project) -> Vec<String> {
        project.changed.iter().map(BoardPath::to_string).collect()
    }

    fn refusal(result: Result<String, Error>) -> String {
        match result {
            Err(Error::Refused(why)) => why,
            other => panic!("not a refusal: {other:?}"),
        }
    }

    #[test]
    fn done_deletes_every_line_and_names_each_id_once() {
        let mut shop = project("- c1 a. Check: x\n- c2 b. Check: y\n- c3 c. Check: z\n", "");
        assert_eq!(
            done(&mut shop, &[chore(3), chore(1), chore(3)]).unwrap(),
            "done c3 c1"
        );
        assert_eq!(shop.chores.to_bytes(), b"- c2 b. Check: y\n");
        assert_eq!(changed(&shop), ["shop/chores.md"]);
    }

    #[test]
    fn done_and_drop_refuse_all_for_one_id_that_names_no_single_item() {
        let chores = "- c1 a. Check: x\n- c2 b. Check: y\n- c2 again. Check: y\n";
        let backlog_bytes = "- b1 a\n";
        let mut shop = project(chores, backlog_bytes);
        assert_eq!(
            refusal(done(&mut shop, &[chore(1), chore(9)])),
            "done c9: no such chore"
        );
        assert_eq!(
            refusal(done(&mut shop, &[chore(1), chore(2)])),
            "done c2: c2 is on 2 lines; fix the duplicate first"
        );
        assert_eq!(
            refusal(drop_items(&mut shop, &[backlog(4)])),
            "drop b4: no such backlog item"
        );
        assert_eq!(shop.chores.to_bytes(), chores.as_bytes());
        assert_eq!(shop.backlog.to_bytes(), backlog_bytes.as_bytes());
    }

    #[test]
    fn drop_says_dropped() {
        let mut shop = project("", "- b1 a\n- b2 b\n");
        assert_eq!(drop_items(&mut shop, &[backlog(2)]).unwrap(), "dropped b2");
        assert_eq!(shop.backlog.to_bytes(), b"- b1 a\n");
        assert_eq!(changed(&shop), ["shop/backlog.md"]);
    }

    #[test]
    fn adds_go_into_their_list() {
        let mut shop = project("", "");
        add_chore(
            &mut shop,
            chore(4),
            &NewChore {
                text: line("Rotate the key"),
                group: line("Home"),
                check: line("login works"),
                from: None,
            },
        )
        .unwrap();
        add_backlog(&mut shop, backlog(7), &line("Dark mode"), None);
        assert_eq!(
            shop.chores.to_bytes(),
            b"## Home\n- c4 Rotate the key. Check: login works\n"
        );
        assert_eq!(shop.backlog.to_bytes(), b"- b7 Dark mode\n");
        assert_eq!(changed(&shop), ["shop/chores.md", "shop/backlog.md"]);
    }

    fn moving(item: &str, side: crate::list::Side, target: &str) -> Move {
        Move::new(item.parse().unwrap(), side, target.parse().unwrap()).unwrap()
    }

    #[test]
    fn move_puts_a_chore_into_the_group_of_the_item_it_goes_next_to() {
        use crate::list::Side::{After, Before};
        let chores = "## Home\n- c1 a. Check: x\n- c2 b. Check: x\n## Work\n- c3 c. Check: x\n";
        let mut shop = project(chores, "");
        assert_eq!(
            move_item(&mut shop, &moving("c1", After, "c3")).unwrap(),
            "moved c1 after c3"
        );
        assert_eq!(
            shop.chores.to_bytes(),
            b"## Home\n- c2 b. Check: x\n## Work\n- c3 c. Check: x\n- c1 a. Check: x\n"
        );
        assert_eq!(changed(&shop), ["shop/chores.md"]);

        let mut shop = project(chores, "");
        move_item(&mut shop, &moving("c3", Before, "c2")).unwrap();
        assert_eq!(
            shop.chores.to_bytes(),
            b"## Home\n- c1 a. Check: x\n- c3 c. Check: x\n- c2 b. Check: x\n## Work\n"
        );
    }

    #[test]
    fn a_move_to_where_the_item_is_changes_no_file() {
        use crate::list::Side::{After, Before};
        let mut shop = project("", "- b1 a\n- b2 b\n");
        assert_eq!(
            move_item(&mut shop, &moving("b2", After, "b1")).unwrap(),
            "moved b2 after b1"
        );
        move_item(&mut shop, &moving("b1", Before, "b2")).unwrap();
        assert_eq!(shop.backlog.to_bytes(), b"- b1 a\n- b2 b\n");
        assert_eq!(changed(&shop), Vec::<String>::new());
    }

    #[test]
    fn move_refuses_an_id_that_names_no_single_item() {
        use crate::list::Side::{After, Before};
        let backlog_bytes = "- b1 a\n- b2 b\n- b2 again\n";
        let mut shop = project("", backlog_bytes);
        let mut refused_move = |item: &str, side, target: &str| {
            refusal(move_item(&mut shop, &moving(item, side, target)))
        };
        assert_eq!(
            refused_move("b9", Before, "b1"),
            "move b9: no such backlog item"
        );
        assert_eq!(
            refused_move("b1", After, "b9"),
            "move b1: after b9: no such backlog item"
        );
        assert_eq!(
            refused_move("b2", Before, "b1"),
            "move b2: b2 is on 2 lines; fix the duplicate first"
        );
        assert_eq!(
            refused_move("b1", Before, "b2"),
            "move b1: before b2: b2 is on 2 lines; fix the duplicate first"
        );
        assert_eq!(
            refusal(move_item(
                &mut project("- c1 a. Check: x\n", ""),
                &moving("c1", Before, "c2")
            )),
            "move c1: before c2: no such chore"
        );
        assert_eq!(shop.backlog.to_bytes(), backlog_bytes.as_bytes());
        assert_eq!(changed(&shop), Vec::<String>::new());
    }

    // ---- briefs ----

    const SECTIONS: &str = "# Title\n## Task\nt\n## Design\nd\n## Premises\np\n## Out of scope\no\n\
                            ## Verification\nv\n## Not doable by the worker\nn\n## Worker protocol\nw\n";

    fn at(text: &str) -> Stamp {
        text.parse().unwrap()
    }

    fn with_briefs(files: &[(&str, &str)], backlog: &str) -> Project {
        let mut project = project("", backlog);
        for (path, text) in files {
            match BoardPath::parse(&format!("shop/{path}")) {
                Some(BoardPath::Brief { name, filed, .. }) => project.briefs.push(BriefFile {
                    name,
                    filed,
                    brief: Brief::parse(text.as_bytes()),
                }),
                _ => panic!("{path} is no brief path"),
            }
        }
        project
    }

    fn text_of(project: &Project, path: &str) -> String {
        let path = BoardPath::parse(&format!("shop/{path}")).unwrap();
        String::from_utf8(project.bytes(&path).unwrap()).unwrap()
    }

    fn refused<T: std::fmt::Debug>(result: Result<T, Error>) -> String {
        match result {
            Err(Error::Refused(why)) => why,
            other => panic!("not a refusal: {other:?}"),
        }
    }

    #[test]
    fn a_new_brief_is_a_draft_header() {
        let mut shop = project("", "");
        let base = Oid::parse(b"86743aa86743aa86743aa86743aa86743aa86743\n").unwrap();
        let path = new_brief(
            &mut shop,
            BriefId::new(129),
            &"fix-login".parse().unwrap(),
            &base,
            at("2026-11-20T16:31:07Z"),
        );
        assert_eq!(path.to_string(), "shop/briefs/129-fix-login.md");
        assert_eq!(changed(&shop), ["shop/briefs/129-fix-login.md"]);
        assert_eq!(
            text_of(&shop, "briefs/129-fix-login.md"),
            "id: 129\nstatus: draft\nbase: 86743aa86743aa86743aa86743aa86743aa86743\n\
             created: 2026-11-20T16:31:07Z\n\n"
        );
    }

    #[test]
    fn queue_stamps_a_draft_with_text_in_every_required_section() {
        let draft = format!("id: 1\nstatus: draft\n\n{SECTIONS}");
        let mut shop = with_briefs(&[("briefs/1-a.md", &draft)], "- b41 x\n- b52 y\n");
        let closes = [backlog(52), backlog(41), backlog(52)];
        assert_eq!(
            queue(
                &mut shop,
                BriefId::new(1),
                Some(&closes),
                at("2026-11-20T16:40:12Z")
            )
            .unwrap(),
            "queued 1"
        );
        assert_eq!(
            text_of(&shop, "briefs/1-a.md"),
            format!(
                "id: 1\nstatus: queued\ncloses: b52, b41\nqueued: 2026-11-20T16:40:12Z\n\n{SECTIONS}"
            )
        );
        assert_eq!(changed(&shop), ["shop/briefs/1-a.md"]);

        // Without --closes a `closes:` written by hand stays.
        let by_hand = format!("id: 2\nstatus: draft\ncloses: b9\n\n{SECTIONS}");
        let mut shop = with_briefs(&[("briefs/2-b.md", &by_hand)], "");
        queue(&mut shop, BriefId::new(2), None, at("2026-11-20T16:40:12Z")).unwrap();
        assert!(text_of(&shop, "briefs/2-b.md").contains("\ncloses: b9\n"));
    }

    #[test]
    fn queue_names_every_section_without_text() {
        // Case does not matter; a heading in a fence and blank lines are no
        // text; Report runs to the end, so what follows it is no section.
        let draft = "id: 1\nstatus: draft\n\n## TASK\nt\n## design\n \n\
                     ## Premises\n```\n## Out of scope\n```\n## Verification\nv\n\
                     ## Report\n## Not doable by the worker\nn\n## Worker protocol\nw\n";
        let mut shop = with_briefs(&[("briefs/1-a.md", draft)], "");
        assert_eq!(
            refused(queue(
                &mut shop,
                BriefId::new(1),
                None,
                at("2026-11-20T16:40:12Z")
            )),
            "queue 1: no text under ## Design, ## Out of scope, ## Not doable by the worker, \
             ## Worker protocol; every one of Task, Design, Premises, Out of scope, Verification, \
             Not doable by the worker, Worker protocol needs a line"
        );
        assert_eq!(text_of(&shop, "briefs/1-a.md"), draft);
    }

    #[test]
    fn queue_refuses_a_brief_that_is_not_one_draft() {
        let full = |status: &str| format!("status: {status}\n\n{SECTIONS}");
        let mut shop = with_briefs(
            &[
                ("briefs/1-a.md", &full("queued")),
                ("briefs/2-a.md", &full("draft")),
                ("briefs/2-b.md", &full("draft")),
                ("briefs/done/3-a.md", &full("draft")),
                ("briefs/4-a.md", SECTIONS),
                ("briefs/5-a.md", &full("wip")),
                ("briefs/6-a.md", &full("draft")),
            ],
            "- b1 x\n- b1 y\n",
        );
        let now = at("2026-11-20T16:40:12Z");
        let mut queue_one = |number: u32, closes: Option<&[BacklogId]>| {
            refused(queue(&mut shop, BriefId::new(number), closes, now))
        };
        assert_eq!(queue_one(1, None), "queue 1: it is queued, not draft");
        assert_eq!(
            queue_one(2, None),
            "queue 2: 2 is on 2 files; fix the duplicate first"
        );
        assert_eq!(queue_one(3, None), "queue 3: it is filed, not draft");
        assert_eq!(queue_one(4, None), "queue 4: it has no `status:` line");
        assert_eq!(queue_one(5, None), "queue 5: its status `wip` is unknown");
        assert_eq!(queue_one(7, None), "queue 7: no such brief");
        assert_eq!(
            queue_one(6, Some(&[backlog(1)])),
            "queue 6: closes b1: b1 is on 2 lines; fix the duplicate first"
        );
        assert_eq!(
            queue_one(6, Some(&[backlog(2)])),
            "queue 6: closes b2: no such backlog item"
        );
        assert_eq!(changed(&shop), Vec::<String>::new());
    }

    #[test]
    fn next_claims_the_brief_queued_longest() {
        let queued = |stamp: &str| format!("status: queued\nqueued: {stamp}\n");
        let mut shop = with_briefs(
            &[
                ("briefs/1-draft.md", "status: draft\n"),
                ("briefs/2-later.md", &queued("2026-11-20T16:40:13Z")),
                ("briefs/3-by-hand.md", "status: queued\n"),
                ("briefs/4-first.md", &queued("2026-11-20T16:40:12Z")),
                ("briefs/5-tie.md", &queued("2026-11-20T16:40:12Z")),
                ("briefs/done/6-filed.md", &queued("2000-01-01T00:00:00Z")),
                ("briefs/7-started.md", "status: started\n"),
            ],
            "",
        );
        let mut order = Vec::new();
        while let Some((number, path)) = next(&mut shop, at("2026-11-20T17:00:00Z")).unwrap() {
            order.push((number.get(), path.to_string()));
        }
        assert_eq!(
            order,
            [
                (4, "shop/briefs/4-first.md".to_owned()),
                (5, "shop/briefs/5-tie.md".to_owned()),
                (2, "shop/briefs/2-later.md".to_owned()),
                (3, "shop/briefs/3-by-hand.md".to_owned()),
            ]
        );
        assert_eq!(
            text_of(&shop, "briefs/4-first.md"),
            "status: started\nqueued: 2026-11-20T16:40:12Z\nstarted: 2026-11-20T17:00:00Z\n"
        );
    }

    #[test]
    fn next_on_an_empty_queue_changes_nothing() {
        let mut shop = with_briefs(&[("briefs/1-a.md", "status: draft\n")], "");
        assert_eq!(next(&mut shop, at("2026-11-20T17:00:00Z")).unwrap(), None);
        assert_eq!(changed(&shop), Vec::<String>::new());
    }

    #[test]
    fn next_refuses_a_number_on_two_files() {
        let mut shop = with_briefs(
            &[
                ("briefs/1-a.md", "status: queued\n"),
                ("briefs/done/1-b.md", ""),
            ],
            "",
        );
        assert_eq!(
            refused(next(&mut shop, at("2026-11-20T17:00:00Z"))),
            "next: 1 is on 2 files; fix the duplicate first"
        );
    }

    fn entry(text: &str) -> EntryText {
        EntryText::new(text.parse().unwrap()).unwrap()
    }

    const T: &str = "2026-11-20T17:10:00Z";

    #[test]
    fn block_puts_the_question_under_questions_and_stamps_blocked() {
        let mut shop = with_briefs(
            &[(
                "briefs/1-a.md",
                "status: started\nstarted: 2026-11-20T17:00:00Z\n\n## Task\nt\n",
            )],
            "",
        );
        assert_eq!(
            block(&mut shop, BriefId::new(1), &entry("Which db?"), at(T)).unwrap(),
            "blocked 1"
        );
        assert_eq!(
            text_of(&shop, "briefs/1-a.md"),
            format!(
                "status: blocked\nstarted: 2026-11-20T17:00:00Z\nblocked: {T}\n\n\
                 ## Task\nt\n## Questions\n\n### {T}\n\nWhich db?\n"
            )
        );
        assert_eq!(changed(&shop), ["shop/briefs/1-a.md"]);
    }

    #[test]
    fn amend_resumes_a_blocked_brief_and_keeps_the_status_of_others() {
        let mut shop = with_briefs(
            &[
                ("briefs/1-a.md", "status: blocked\n\n## Questions\nq\n"),
                ("briefs/2-a.md", "status: queued\n"),
                ("briefs/3-a.md", "status: started\nstarted: x\n"),
            ],
            "",
        );
        let mut amend_one = |number: u32| {
            amend(
                &mut shop,
                BriefId::new(number),
                &entry("Use sqlite."),
                at(T),
            )
        };
        assert_eq!(amend_one(1).unwrap(), "amended 1, resumed");
        assert_eq!(amend_one(2).unwrap(), "amended 2");
        assert_eq!(amend_one(3).unwrap(), "amended 3");
        assert_eq!(
            text_of(&shop, "briefs/1-a.md"),
            format!(
                "status: started\n\n## Questions\nq\n## Amendments\n\n### {T}\n\nUse sqlite.\n"
            )
        );
        // A resume is no new start.
        assert_eq!(
            text_of(&shop, "briefs/3-a.md"),
            format!("status: started\nstarted: x\n## Amendments\n\n### {T}\n\nUse sqlite.\n")
        );
        assert!(text_of(&shop, "briefs/2-a.md").starts_with("status: queued\n## Amendments\n"));
    }

    #[test]
    fn report_makes_the_report_and_stamps_reported() {
        let mut shop = with_briefs(&[("briefs/1-a.md", "status: started\n\n## Task\nt\n")], "");
        let text: Text = "Ran:\n## Checks\nall green\n".parse().unwrap();
        assert_eq!(
            report(&mut shop, BriefId::new(1), &text, at(T)).unwrap(),
            "reported 1"
        );
        assert_eq!(
            text_of(&shop, "briefs/1-a.md"),
            format!(
                "status: reported\nreported: {T}\n\n## Task\nt\n## Report\nRan:\n## Checks\nall green\n"
            )
        );
    }

    /// Each command from every status it does not take, and on a number that
    /// names no single brief: refused, nothing changed.
    #[test]
    fn block_amend_and_report_refuse_a_brief_in_the_wrong_status() {
        let files: Vec<(String, String)> = ["draft", "queued", "started", "blocked", "reported"]
            .iter()
            .enumerate()
            .map(|(n, status)| {
                (
                    format!("briefs/{}-{status}.md", n + 1),
                    format!("status: {status}\n"),
                )
            })
            .chain([
                ("briefs/done/6-filed.md".to_owned(), String::new()),
                ("briefs/7-none.md".to_owned(), "# no header\n".to_owned()),
                ("briefs/8-a.md".to_owned(), "status: started\n".to_owned()),
                ("briefs/done/8-b.md".to_owned(), String::new()),
            ])
            .collect();
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (&p[..], &t[..])).collect();
        let text: Text = "x".parse().unwrap();
        let mut refusals = Vec::new();
        for number in 1..=9 {
            let id = BriefId::new(number);
            for command in 0..3 {
                // Each on the files as they are: one that goes through
                // changes the status the next one sees.
                let mut shop = with_briefs(&files, "");
                let result = match command {
                    0 => block(&mut shop, id, &entry("x"), at(T)),
                    1 => amend(&mut shop, id, &entry("x"), at(T)),
                    _ => report(&mut shop, id, &text, at(T)),
                };
                if let Err(Error::Refused(why)) = result {
                    refusals.push(why);
                }
            }
        }
        assert_eq!(
            refusals,
            [
                "block 1: it is draft, not started",
                "amend 1: it is draft, not queued, started or blocked",
                "report 1: it is draft, not started",
                "block 2: it is queued, not started",
                "report 2: it is queued, not started",
                "block 4: it is blocked, not started",
                "report 4: it is blocked, not started",
                "block 5: it is reported, not started",
                "amend 5: it is reported, not queued, started or blocked",
                "report 5: it is reported, not started",
                "block 6: it is filed, not started",
                "amend 6: it is filed, not queued, started or blocked",
                "report 6: it is filed, not started",
                "block 7: it has no `status:` line",
                "amend 7: it has no `status:` line",
                "report 7: it has no `status:` line",
                "block 8: 8 is on 2 files; fix the duplicate first",
                "amend 8: 8 is on 2 files; fix the duplicate first",
                "report 8: 8 is on 2 files; fix the duplicate first",
                "block 9: no such brief",
                "amend 9: no such brief",
                "report 9: no such brief",
            ]
        );
        let mut shop = with_briefs(&files, "");
        for number in [1, 5, 6, 7, 8, 9] {
            let id = BriefId::new(number);
            let _ = block(&mut shop, id, &entry("x"), at(T));
            let _ = amend(&mut shop, id, &entry("x"), at(T));
            let _ = report(&mut shop, id, &text, at(T));
        }
        assert_eq!(changed(&shop), Vec::<String>::new());
    }

    fn counted(count: usize) -> Commits {
        Commits::Counted {
            count,
            repository: PathBuf::from("/w/shop"),
        }
    }

    fn paths(project: &Project) -> Vec<String> {
        project
            .changed_paths()
            .iter()
            .map(BoardPath::to_string)
            .collect()
    }

    #[test]
    fn file_closes_the_backlog_items_and_moves_the_brief_to_done() {
        let reported = "id: 1\nstatus: reported\ncloses: b41, b9, b52\n\n# T\n";
        let mut shop = with_briefs(
            &[("briefs/1-a.md", reported)],
            "- b41 a\n  more on a\n- b52 b\n- b60 c\n",
        );
        let filed = file(&mut shop, BriefId::new(1), &counted(3), at(T)).unwrap();
        // b9 was dropped since the brief was queued: skipped.
        assert_eq!(filed.to_string(), "filed 1 (3 commits) closed b41 b52");
        // A closed item goes with its indented lines, as on `drop`.
        assert_eq!(shop.backlog.to_bytes(), b"- b60 c\n");
        assert_eq!(
            paths(&shop),
            [
                "shop/backlog.md",
                "shop/briefs/1-a.md",
                "shop/briefs/done/1-a.md"
            ]
        );
        assert_eq!(
            shop.bytes(&BoardPath::parse("shop/briefs/1-a.md").unwrap()),
            None
        );
        assert_eq!(
            text_of(&shop, "briefs/done/1-a.md"),
            format!("id: 1\nstatus: filed\ncloses: b41, b9, b52\nfiled: {T}\n\n# T\n")
        );
        assert_eq!(
            shop.find_brief(BriefId::new(1)).unwrap().status(),
            Ok(Status::Filed)
        );
    }

    #[test]
    fn file_without_closes_leaves_the_backlog_alone() {
        let mut shop = with_briefs(&[("briefs/1-a.md", "status: reported\n")], "");
        let filed = file(&mut shop, BriefId::new(1), &Commits::Skipped, at(T)).unwrap();
        assert_eq!(filed.to_string(), "filed 1 (no commits)");
        assert_eq!(
            paths(&shop),
            ["shop/briefs/1-a.md", "shop/briefs/done/1-a.md"]
        );

        let mut shop = with_briefs(&[("briefs/2-a.md", "status: reported\n")], "");
        let filed = file(&mut shop, BriefId::new(2), &counted(1), at(T)).unwrap();
        assert_eq!(filed.to_string(), "filed 2 (1 commit)");
    }

    #[test]
    fn file_refuses_what_it_cannot_file_and_changes_nothing() {
        let files = [
            ("briefs/1-a.md", "status: started\n"),
            ("briefs/2-a.md", "status: reported\n"),
            ("briefs/3-a.md", "status: reported\ncloses: b4l\n"),
            ("briefs/4-a.md", "status: reported\ncloses: b1, b2\n"),
            ("briefs/done/5-a.md", "status: filed\n"),
            ("briefs/6-a.md", "status: reported\n"),
            ("briefs/done/6-b.md", ""),
            ("briefs/7-a.md", "# no header\n"),
        ];
        let backlog = "- b1 x\n- b2 y\n- b2 z\n";
        let refusals: Vec<String> = [
            (1, counted(2)),
            (2, counted(0)),
            (3, counted(1)),
            (4, Commits::Skipped),
            (5, counted(1)),
            (6, counted(1)),
            (7, counted(1)),
            (8, counted(1)),
        ]
        .into_iter()
        .map(|(number, commits)| {
            let mut shop = with_briefs(&files, backlog);
            let result = file(&mut shop, BriefId::new(number), &commits, at(T));
            assert_eq!(paths(&shop), Vec::<String>::new(), "{number}");
            refused(result)
        })
        .collect();
        assert_eq!(
            refusals,
            [
                "file 1: it is started, not reported",
                "file 2: no commit reachable from HEAD in /w/shop has the trailer `Brief: 2`; \
                 the worker puts it on every commit of the brief (--no-commits files a brief \
                 that changed nothing there)",
                "file 3: its `closes:` names `b4l`, which is no backlog id; fix the header first",
                "file 4: closes b2: b2 is on 2 lines; fix the duplicate first",
                "file 5: it is filed, not reported",
                "file 6: 6 is on 2 files; fix the duplicate first",
                "file 7: it has no `status:` line",
                "file 8: no such brief",
            ]
        );

        // No repository to count in: told after the status, as usage.
        let nowhere = || Commits::NoRepository {
            at: PathBuf::from("/w"),
            said: "not a git repository".to_owned(),
        };
        let mut shop = with_briefs(&files, backlog);
        assert_eq!(
            refused(file(&mut shop, BriefId::new(1), &nowhere(), at(T))),
            "file 1: it is started, not reported"
        );
        match file(&mut shop, BriefId::new(2), &nowhere(), at(T)) {
            Err(Error::Usage(why)) => assert_eq!(
                why,
                "file 2: no repository at /w to count the brief's commits in \
                 (not a git repository); run it in the project's checkout, or file with \
                 --no-commits"
            ),
            other => panic!("not a usage error: {other:?}"),
        }
        assert_eq!(paths(&shop), Vec::<String>::new());
    }

    #[test]
    fn a_chore_whose_check_says_nothing_is_refused() {
        let mut shop = project("", "");
        let refused = add_chore(
            &mut shop,
            chore(1),
            &NewChore {
                text: line("Rotate the key"),
                group: line("Home"),
                check: line("(126)"),
                from: None,
            },
        );
        assert!(
            matches!(&refused, Err(Error::Refused(why)) if why == "add chore: `(126)` does not say how to see that the chore is done"),
            "{refused:?}"
        );
        assert_eq!(shop.chores.to_bytes(), b"");
    }
}
