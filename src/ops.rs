//! What each write command does to a project, given the ids it was
//! allocated: no git and no files, so every rule of a command is tested
//! without either. The write loop in `sync.rs` loads the project, runs one of
//! these and commits what it changed.

use crate::brief::{BadStatus, Brief, Stamp, Status, Transition, TransitionError};
use crate::error::Error;
use crate::git::Oid;
use crate::id::{BacklogId, BriefId, ChoreId, ItemId, ListKind};
use crate::list::{FindError, Line, NoCheck, RemoveError};
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
