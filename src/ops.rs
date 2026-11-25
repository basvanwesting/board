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
use crate::id::{BacklogId, BriefId, ChoreId, ItemId, ListId, ListKind};
use crate::item::{self, Body, Item, MoveError};
use crate::list::{FindError, Line, Move};
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
    /// The backlog items whose files were deleted.
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

/// Files a reported brief: deletes the backlog items its `closes:` names,
/// stamps it filed and moves it to `briefs/done/`, all in one commit.
///
/// The commits are checked after the status, so a brief that is not
/// reported says so rather than that it has no commits.
///
/// An item `closes:` names that is gone already (dropped meanwhile, or
/// deleted by hand) is skipped: the brief is what is filed, and the item is
/// closed either way.
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
    let closed: Vec<BacklogId> = closes
        .into_iter()
        .filter(|id| project.list(ListKind::Backlog).get(id.get()).is_some())
        .collect();
    for id in &closed {
        project.list_mut(ListKind::Backlog).remove(id.get());
        project.touched(ListId::Backlog(*id));
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
    /// How to see it is done; optional (owner, 2026-11-25: "some chores are
    /// just that, simple actions. A check would repeat it").
    pub check: Option<Line>,
    pub from: Option<ItemId>,
    /// Empty for none.
    pub body: Body,
}

/// What `retitle --check` asks of a chore's check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckChange {
    Set(Line),
    /// `--check ""`: the chore has none after.
    Remove,
}

/// Adds chore `id` last in its group.
pub(crate) fn add_chore(project: &mut Project, id: ChoreId, new: &NewChore, now: Stamp) {
    let items = project.list_mut(ListKind::Chores);
    let order = items.next_order(Some(new.group.as_str().as_bytes()));
    let item = Item::new(item::New {
        id: ItemId::Chore(id),
        group: Some(&new.group),
        order,
        check: new.check.as_ref(),
        from: new.from,
        created: now,
        title: &new.text,
        body: &new.body,
    });
    items.insert(id.get(), item);
    project.touched(ListId::Chore(id));
}

/// Adds backlog item `id` last in the backlog.
pub(crate) fn add_backlog(
    project: &mut Project,
    id: BacklogId,
    text: &Line,
    from: Option<ItemId>,
    body: &Body,
    now: Stamp,
) {
    let items = project.list_mut(ListKind::Backlog);
    let order = items.next_order(None);
    let item = Item::new(item::New {
        id: ItemId::Backlog(id),
        group: None,
        order,
        check: None,
        from,
        created: now,
        title: text,
        body,
    });
    items.insert(id.get(), item);
    project.touched(ListId::Backlog(id));
}

/// Changes a chore's or backlog item's title, or a chore's check (set or
/// removed), keeping its id, place, reference, body and every other key;
/// gives what `retitle` prints, as `retitled c12`. Without it the planner,
/// which changes single items only through `board`, could change a title
/// only by drop and add: a new id, and the item at the bottom.
pub(crate) fn retitle(
    project: &mut Project,
    id: ListId,
    text: Option<&Line>,
    check: Option<&CheckChange>,
) -> Result<String, Error> {
    let kind = id.kind();
    let item = project.item_mut(id).ok_or_else(|| {
        Error::Refused(format!(
            "retitle {id}: {}",
            not_one(kind, id.get(), FindError::Missing)
        ))
    })?;
    if let Some(text) = text {
        item.set_title(text);
    }
    match check {
        Some(CheckChange::Set(check)) => item.set("check", check.as_str()),
        Some(CheckChange::Remove) => item.remove("check"),
        None => {}
    }
    Ok(format!("retitled {id}"))
}

/// Writes a chore's or backlog item's body: appended after one blank line,
/// or in place of the one it has (`--replace`; empty, it removes the body).
/// A replace is there because the planner changes single items only
/// through `board`, and append-only bodies collect text that is no longer
/// true. Appending is the default: it cannot lose text.
pub(crate) fn amend_item(
    project: &mut Project,
    id: ListId,
    body: &Body,
    replace: bool,
) -> Result<String, Error> {
    let kind = id.kind();
    let item = project.item_mut(id).ok_or_else(|| {
        Error::Refused(format!(
            "amend {id}: {}",
            not_one(kind, id.get(), FindError::Missing)
        ))
    })?;
    if replace {
        item.set_body(body);
    } else {
        item.append_body(body);
    }
    Ok(format!("amended {id}"))
}

/// Deletes the chores' files, all or none; gives what `done` prints.
pub(crate) fn done(project: &mut Project, ids: &[ChoreId]) -> Result<String, Error> {
    let ids: Vec<ListId> = ids.iter().copied().map(ListId::Chore).collect();
    remove(project, &ids, ("done", "done"))
}

/// Deletes the backlog items' files, all or none; gives what `drop` prints.
pub(crate) fn drop_items(project: &mut Project, ids: &[BacklogId]) -> Result<String, Error> {
    let ids: Vec<ListId> = ids.iter().copied().map(ListId::Backlog).collect();
    remove(project, &ids, ("drop", "dropped"))
}

/// Moves an item next to another of its kind; gives what `move` prints, as
/// `moved c12 before c3`. Writes the moved item's file, or the files of its
/// group when that is renumbered ([`Items::move_item`]).
///
/// An item already where it is asked to be changes no file, so the write
/// makes no commit and the output is the same: what was asked is so.
pub(crate) fn move_item(project: &mut Project, wanted: &Move) -> Result<String, Error> {
    let kind = wanted.kind();
    let refused = |why: String| Error::Refused(format!("move {}: {why}", wanted.item()));
    let changed = project
        .list_mut(kind)
        .move_item(wanted)
        .map_err(|error| match error {
            MoveError::Item(why) => refused(not_one(kind, wanted.item().get(), why)),
            MoveError::Target(why) => refused(format!(
                "{} {}: {}",
                wanted.side(),
                wanted.target(),
                not_one(kind, wanted.target().get(), why)
            )),
        })?;
    for number in changed {
        project.touched(list_id(kind, number));
    }
    Ok(format!("moved {wanted}"))
}

fn list_id(kind: ListKind, number: u32) -> ListId {
    match kind {
        ListKind::Chores => ListId::Chore(ChoreId::new(number).expect("an item's id is past 0")),
        ListKind::Backlog => {
            ListId::Backlog(BacklogId::new(number).expect("an item's id is past 0"))
        }
    }
}

/// `(command, past)`: the command names a refusal, the past tense what was
/// done.
fn remove(
    project: &mut Project,
    ids: &[ListId],
    (command, past): (&str, &str),
) -> Result<String, Error> {
    // An id given twice is one file to delete and one word of the output.
    let mut unique: Vec<ListId> = Vec::with_capacity(ids.len());
    for id in ids {
        if !unique.contains(id) {
            unique.push(*id);
        }
    }
    for id in &unique {
        project.list(id.kind()).find(id.get()).map_err(|why| {
            Error::Refused(format!(
                "{command} {id}: {}",
                not_one(id.kind(), id.get(), why)
            ))
        })?;
    }
    for id in &unique {
        project.list_mut(id.kind()).remove(id.get());
        project.touched(*id);
    }
    let ids: Vec<String> = unique.iter().map(ListId::to_string).collect();
    Ok(format!("{past} {}", ids.join(" ")))
}

/// Why a list id names no item, as every command that takes one says it.
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
    use crate::list::Side::{After, Before};
    use crate::project::{BoardPath, ProjectName};

    const CREATED: &str = "2026-11-25T10:00:00Z";

    /// A project with these item files, as `(id, text)`.
    fn project(chores: &[(u32, &str)], backlog: &[(u32, &str)]) -> Project {
        let mut project = Project::empty(ProjectName::parse("shop").unwrap());
        for (kind, items) in [(ListKind::Chores, chores), (ListKind::Backlog, backlog)] {
            for (number, text) in items {
                project
                    .list_mut(kind)
                    .insert(*number, Item::parse(text.as_bytes()));
            }
        }
        project
    }

    fn chore_file(id: u32, group: &str, order: i64, title: &str) -> String {
        format!("id: c{id}\ngroup: {group}\norder: {order}\ncreated: {CREATED}\n\n# {title}\n")
    }

    fn backlog_file(id: u32, order: i64, title: &str) -> String {
        format!("id: b{id}\norder: {order}\ncreated: {CREATED}\n\n# {title}\n")
    }

    /// The file of item `id` (`c1`) as the project has it now; `None` once
    /// deleted.
    fn file_text(project: &Project, id: &str) -> Option<String> {
        let path = project.item_path(id.parse().unwrap());
        project
            .bytes(&path)
            .map(|bytes| String::from_utf8(bytes).unwrap())
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

    fn now() -> Stamp {
        "2026-11-25T14:08:18Z".parse().unwrap()
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
    fn add_writes_an_item_s_file_last_in_its_group() {
        let home = chore_file(1, "Home", 3000, "Old");
        let mut shop = project(&[(1, &home)], &[]);
        let new = |text: &str, group: &str, check: Option<&str>, body: &[u8]| NewChore {
            text: line(text),
            group: line(group),
            check: check.map(line),
            from: None,
            body: Body::new(body),
        };
        add_chore(
            &mut shop,
            chore(4),
            &new("Rotate the key", "Home", Some("login works"), b""),
            now(),
        );
        add_chore(
            &mut shop,
            chore(5),
            &new(
                "Water the plants",
                "Away",
                None,
                b"first\n\n## step\n```sql\nselect 1;\n```",
            ),
            now(),
        );
        assert_eq!(
            file_text(&shop, "c4").unwrap(),
            "id: c4\ngroup: Home\norder: 4000\ncheck: login works\n\
             created: 2026-11-25T14:08:18Z\n\n# Rotate the key\n"
        );
        assert_eq!(
            file_text(&shop, "c5").unwrap(),
            "id: c5\ngroup: Away\norder: 1000\ncreated: 2026-11-25T14:08:18Z\n\n\
             # Water the plants\n\nfirst\n\n## step\n```sql\nselect 1;\n```\n"
        );
        let from = Some(ItemId::Brief(BriefId::new(126)));
        add_backlog(
            &mut shop,
            backlog(7),
            &line("Dark mode"),
            from,
            &Body::new(b""),
            now(),
        );
        assert_eq!(
            file_text(&shop, "b7").unwrap(),
            "id: b7\norder: 1000\nfrom: 126\ncreated: 2026-11-25T14:08:18Z\n\n# Dark mode\n"
        );
        assert_eq!(
            changed(&shop),
            [
                "shop/chores/c4.md",
                "shop/chores/c5.md",
                "shop/backlog/b7.md"
            ]
        );
        assert_eq!(file_text(&shop, "c1").unwrap(), home);
    }

    #[test]
    fn amend_appends_after_one_blank_line_and_replace_takes_the_body_s_place() {
        let bare = backlog_file(1, 1000, "Later");
        let with_body = format!("{}\nfirst\n\n\n", backlog_file(2, 2000, "Body"));
        let mut shop = project(&[], &[(1, &bare), (2, &with_body)]);
        let body = |text: &str| Body::new(text.as_bytes());
        assert_eq!(
            amend_item(&mut shop, "b1".parse().unwrap(), &body("one"), false).unwrap(),
            "amended b1"
        );
        assert_eq!(file_text(&shop, "b1").unwrap(), format!("{bare}\none\n"));
        amend_item(&mut shop, "b1".parse().unwrap(), &body("two\n"), false).unwrap();
        assert_eq!(
            file_text(&shop, "b1").unwrap(),
            format!("{bare}\none\n\ntwo\n")
        );
        amend_item(&mut shop, "b2".parse().unwrap(), &body("more"), false).unwrap();
        assert_eq!(
            file_text(&shop, "b2").unwrap(),
            format!("{}\nfirst\n\nmore\n", backlog_file(2, 2000, "Body"))
        );
        amend_item(
            &mut shop,
            "b2".parse().unwrap(),
            &body("new\n\nwhole"),
            true,
        )
        .unwrap();
        assert_eq!(
            file_text(&shop, "b2").unwrap(),
            format!("{}\nnew\n\nwhole\n", backlog_file(2, 2000, "Body"))
        );
        // Empty stdin with `--replace`: the file ends at its title.
        amend_item(&mut shop, "b2".parse().unwrap(), &body(""), true).unwrap();
        assert_eq!(
            file_text(&shop, "b2").unwrap(),
            backlog_file(2, 2000, "Body")
        );
        assert_eq!(
            refusal(amend_item(
                &mut shop,
                "c9".parse().unwrap(),
                &body("x"),
                false
            )),
            "amend c9: no such chore"
        );
        // A key the tool does not know stays as it is.
        let text = "id: b3\norder: 3\nowner: me\ncreated: x\n\n# T\n";
        let mut shop = project(&[], &[(3, text)]);
        amend_item(&mut shop, "b3".parse().unwrap(), &body("more"), false).unwrap();
        assert_eq!(file_text(&shop, "b3").unwrap(), format!("{text}\nmore\n"));
    }

    #[test]
    fn retitle_changes_the_title_or_the_check_and_keeps_every_other_byte() {
        let text = "id: c1\ngroup: Home\norder: 1000\nowner: me\ncheck: old\n\
                    created: 2026-11-25T10:00:00Z\n\n# Old title\n\nthe body\n";
        let plain = chore_file(2, "Home", 2000, "No check");
        let mut shop = project(&[(1, text), (2, &plain)], &[]);
        assert_eq!(
            retitle(
                &mut shop,
                "c1".parse().unwrap(),
                Some(&line("New title")),
                None
            )
            .unwrap(),
            "retitled c1"
        );
        assert_eq!(
            file_text(&shop, "c1").unwrap(),
            text.replace("# Old title", "# New title")
        );
        let set = CheckChange::Set(line("select 1"));
        retitle(&mut shop, "c1".parse().unwrap(), None, Some(&set)).unwrap();
        assert_eq!(
            file_text(&shop, "c1").unwrap(),
            text.replace("# Old title", "# New title")
                .replace("check: old", "check: select 1")
        );
        retitle(
            &mut shop,
            "c1".parse().unwrap(),
            None,
            Some(&CheckChange::Remove),
        )
        .unwrap();
        assert_eq!(
            file_text(&shop, "c1").unwrap(),
            text.replace("# Old title", "# New title")
                .replace("check: old\n", "")
        );
        // A chore without a check is retitled, and given one.
        retitle(
            &mut shop,
            "c2".parse().unwrap(),
            Some(&line("Still none")),
            None,
        )
        .unwrap();
        assert_eq!(
            file_text(&shop, "c2").unwrap(),
            chore_file(2, "Home", 2000, "Still none")
        );
        retitle(&mut shop, "c2".parse().unwrap(), None, Some(&set)).unwrap();
        assert_eq!(
            file_text(&shop, "c2").unwrap(),
            chore_file(2, "Home", 2000, "Still none")
                .replace("\n\n# Still none", "\ncheck: select 1\n\n# Still none")
        );
        assert_eq!(
            refusal(retitle(
                &mut shop,
                "b3".parse().unwrap(),
                Some(&line("x")),
                None
            )),
            "retitle b3: no such backlog item"
        );
    }

    #[test]
    fn done_and_drop_delete_the_files_and_name_each_id_once() {
        let files: Vec<String> = (1..=3)
            .map(|n| chore_file(n, "Home", 1000 * i64::from(n), "x"))
            .collect();
        let mut shop = project(
            &[(1, &files[0]), (2, &files[1]), (3, &files[2])],
            &[(1, &backlog_file(1, 1, "b"))],
        );
        assert_eq!(
            refusal(done(&mut shop, &[chore(1), chore(9)])),
            "done c9: no such chore"
        );
        assert_eq!(changed(&shop), Vec::<String>::new());
        assert_eq!(
            done(&mut shop, &[chore(3), chore(1), chore(3)]).unwrap(),
            "done c3 c1"
        );
        assert_eq!(
            (file_text(&shop, "c1"), file_text(&shop, "c3")),
            (None, None)
        );
        assert_eq!(file_text(&shop, "c2").unwrap(), files[1]);
        assert_eq!(changed(&shop), ["shop/chores/c1.md", "shop/chores/c3.md"]);
        assert_eq!(
            refusal(drop_items(&mut shop, &[backlog(4)])),
            "drop b4: no such backlog item"
        );
        assert_eq!(drop_items(&mut shop, &[backlog(1)]).unwrap(), "dropped b1");
        assert_eq!(file_text(&shop, "b1"), None);
    }

    fn moving(item: &str, side: crate::list::Side, target: &str) -> Move {
        Move::new(item.parse().unwrap(), side, target.parse().unwrap()).unwrap()
    }

    /// Home: c1 1000, c2 2000, c3 3000; Work: c4 1000.
    fn groups() -> Project {
        let c: Vec<String> = [
            (1, "Home", 1000),
            (2, "Home", 2000),
            (3, "Home", 3000),
            (4, "Work", 1000),
        ]
        .map(|(n, group, order)| chore_file(n, group, order, "x"))
        .into();
        project(&[(1, &c[0]), (2, &c[1]), (3, &c[2]), (4, &c[3])], &[])
    }

    #[test]
    fn a_move_writes_the_moved_item_only() {
        for (wanted, file_after) in [
            // Halfway between its new neighbours.
            (moving("c3", Before, "c2"), chore_file(3, "Home", 1500, "x")),
            // Last: past the last by a step.
            (moving("c1", After, "c3"), chore_file(1, "Home", 4000, "x")),
            // First, into another group: before the first by a step.
            (moving("c4", Before, "c1"), chore_file(4, "Home", 0, "x")),
            // Last in another group.
            (moving("c1", After, "c4"), chore_file(1, "Work", 2000, "x")),
        ] {
            let mut shop = groups();
            assert_eq!(
                move_item(&mut shop, &wanted).unwrap(),
                format!("moved {wanted}")
            );
            let id = wanted.item().to_string();
            assert_eq!(changed(&shop), [format!("shop/chores/{id}.md")], "{wanted}");
            assert_eq!(file_text(&shop, &id).unwrap(), file_after, "{wanted}");
        }
    }

    #[test]
    fn a_move_into_no_gap_renumbers_that_list() {
        let b: Vec<String> = (1..=3)
            .map(|n| backlog_file(n, i64::from(n), "x"))
            .collect();
        let mut shop = project(&[], &[(1, &b[0]), (2, &b[1]), (3, &b[2])]);
        move_item(&mut shop, &moving("b3", Before, "b2")).unwrap();
        assert_eq!(
            changed(&shop),
            [
                "shop/backlog/b1.md",
                "shop/backlog/b2.md",
                "shop/backlog/b3.md"
            ]
        );
        assert_eq!(
            [1, 3, 2].map(|n| file_text(&shop, &format!("b{n}")).unwrap()),
            [
                backlog_file(1, 1000, "x"),
                backlog_file(3, 2000, "x"),
                backlog_file(2, 3000, "x"),
            ]
        );
        // A chores' group is renumbered alone.
        let c: Vec<String> = [(1, "Home", 1), (2, "Home", 2), (3, "Work", 5)]
            .map(|(n, group, order)| chore_file(n, group, order, "x"))
            .into();
        let mut shop = project(&[(1, &c[0]), (2, &c[1]), (3, &c[2])], &[]);
        move_item(&mut shop, &moving("c3", Before, "c2")).unwrap();
        assert_eq!(
            changed(&shop),
            [
                "shop/chores/c1.md",
                "shop/chores/c2.md",
                "shop/chores/c3.md"
            ]
        );
        assert_eq!(
            file_text(&shop, "c3").unwrap(),
            chore_file(3, "Home", 2000, "x")
        );
        assert_eq!(
            file_text(&shop, "c2").unwrap(),
            chore_file(2, "Home", 3000, "x")
        );
    }

    #[test]
    fn a_move_to_where_the_item_is_changes_no_file_and_keeps_unknown_keys() {
        let mut shop = groups();
        assert_eq!(
            move_item(&mut shop, &moving("c2", After, "c1")).unwrap(),
            "moved c2 after c1"
        );
        move_item(&mut shop, &moving("c1", Before, "c2")).unwrap();
        assert_eq!(changed(&shop), Vec::<String>::new());
        let text = "id: b1\norder: 5\nowner: me\ncreated: x\n\n# a\n";
        let mut shop = project(&[], &[(1, text), (2, &backlog_file(2, 9, "b"))]);
        move_item(&mut shop, &moving("b1", After, "b2")).unwrap();
        assert_eq!(
            file_text(&shop, "b1").unwrap(),
            text.replace("order: 5", "order: 1009")
        );
    }

    #[test]
    fn move_refuses_an_id_that_names_no_item() {
        let mut shop = groups();
        assert_eq!(
            refusal(move_item(&mut shop, &moving("c9", Before, "c1"))),
            "move c9: no such chore"
        );
        assert_eq!(
            refusal(move_item(&mut shop, &moving("c1", After, "c9"))),
            "move c1: after c9: no such chore"
        );
        assert_eq!(changed(&shop), Vec::<String>::new());
    }

    // ---- briefs ----

    const SECTIONS: &str = "# Title\n## Task\nt\n## Design\nd\n## Premises\np\n## Out of scope\no\n\
                            ## Verification\nv\n## Not doable by the worker\nn\n## Worker protocol\nw\n";

    fn at(text: &str) -> Stamp {
        text.parse().unwrap()
    }

    /// The briefs, and backlog items with these ids.
    fn with_briefs(files: &[(&str, &str)], backlog: &[u32]) -> Project {
        let items: Vec<(u32, String)> = backlog
            .iter()
            .map(|n| (*n, backlog_file(*n, i64::from(*n), "x")))
            .collect();
        let items: Vec<(u32, &str)> = items.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let mut project = project(&[], &items);
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
        let mut shop = project(&[], &[]);
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
        let mut shop = with_briefs(&[("briefs/1-a.md", &draft)], &[41, 52]);
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
        let mut shop = with_briefs(&[("briefs/2-b.md", &by_hand)], &[]);
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
        let mut shop = with_briefs(&[("briefs/1-a.md", draft)], &[]);
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
            &[1],
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
            &[],
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
        let mut shop = with_briefs(&[("briefs/1-a.md", "status: draft\n")], &[]);
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
            &[],
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
            &[],
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
            &[],
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
        let mut shop = with_briefs(&[("briefs/1-a.md", "status: started\n\n## Task\nt\n")], &[]);
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
                let mut shop = with_briefs(&files, &[]);
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
        let mut shop = with_briefs(&files, &[]);
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
        let mut shop = with_briefs(&[("briefs/1-a.md", reported)], &[41, 52, 60]);
        let filed = file(&mut shop, BriefId::new(1), &counted(3), at(T)).unwrap();
        // b9 was dropped since the brief was queued: skipped.
        assert_eq!(filed.to_string(), "filed 1 (3 commits) closed b41 b52");
        // A closed item's file is deleted, as on `drop`.
        assert_eq!(file_text(&shop, "b41"), None);
        assert_eq!(file_text(&shop, "b52"), None);
        assert_eq!(file_text(&shop, "b60").unwrap(), backlog_file(60, 60, "x"));
        assert_eq!(
            paths(&shop),
            [
                "shop/backlog/b41.md",
                "shop/backlog/b52.md",
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
        let mut shop = with_briefs(&[("briefs/1-a.md", "status: reported\n")], &[]);
        let filed = file(&mut shop, BriefId::new(1), &Commits::Skipped, at(T)).unwrap();
        assert_eq!(filed.to_string(), "filed 1 (no commits)");
        assert_eq!(
            paths(&shop),
            ["shop/briefs/1-a.md", "shop/briefs/done/1-a.md"]
        );

        let mut shop = with_briefs(&[("briefs/2-a.md", "status: reported\n")], &[]);
        let filed = file(&mut shop, BriefId::new(2), &counted(1), at(T)).unwrap();
        assert_eq!(filed.to_string(), "filed 2 (1 commit)");
    }

    #[test]
    fn file_refuses_what_it_cannot_file_and_changes_nothing() {
        let files = [
            ("briefs/1-a.md", "status: started\n"),
            ("briefs/2-a.md", "status: reported\n"),
            ("briefs/3-a.md", "status: reported\ncloses: b4l\n"),
            ("briefs/done/5-a.md", "status: filed\n"),
            ("briefs/6-a.md", "status: reported\n"),
            ("briefs/done/6-b.md", ""),
            ("briefs/7-a.md", "# no header\n"),
        ];
        let backlog = &[1, 2];
        let refusals: Vec<String> = [
            (1, counted(2)),
            (2, counted(0)),
            (3, counted(1)),
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
}
