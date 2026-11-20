//! What each write command does to a project, given the ids it was
//! allocated: no git and no files, so every rule of a command is tested
//! without either. The write loop in `sync.rs` loads the project, runs one of
//! these and commits what it changed.

use crate::error::Error;
use crate::id::{BacklogId, ChoreId, ItemId, ListKind};
use crate::list::{FindError, Line, NoCheck, RemoveError};
use crate::project::Project;

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
