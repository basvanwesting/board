//! What is wrong with a project's files, for the user to fix by hand.
//!
//! The parsers take anything, so a hand edit never stops the tool. Lint is
//! where the edits that leave an item unusable come up: an id that names two
//! things, an item without its place, a brief no command will move.

use std::collections::BTreeMap;
use std::fmt;

use crate::brief::BadStatus;
use crate::id::{BriefId, ItemId, ListId, ListKind};
use crate::item::BadOrder;
use crate::project::Project;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Relative to the board-data root.
    pub file: String,
    /// From 1, when the finding is about one line.
    pub line: Option<usize>,
    pub problem: Problem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A brief number that another file of the project carries too.
    DuplicateBrief { number: BriefId, other: String },
    /// A brief outside `briefs/done/` without a status a command can move on
    /// from.
    Status(BadStatus),
    /// The header's `id:` is not the number in the file name. The file name
    /// is the one the tool goes by.
    HeaderId { found: String, name: BriefId },
    /// An item's header `id:` is not the id of its file name, which is the
    /// one the tool goes by.
    ItemHeaderId { found: String, name: ListId },
    /// An item's file without a key it needs: `id`, `order`, `created`, and
    /// a chore's `group`.
    MissingKey(&'static str),
    /// An item's `order:` that is no integer: it is listed last.
    BadOrder(String),
    /// An item's file without its `# ` title line.
    NoTitle,
    /// An `.md` file below `briefs/`, `chores/` or `backlog/` that is not
    /// named as the board names its files.
    Misnamed,
    /// A list file of the storage before item files (`chores.md`,
    /// `backlog.md`), which is not read: its items are not listed, synced or
    /// closed until it is converted.
    OldList,
    /// The local version of a brief or item that lost to the remote's in a
    /// merge.
    LocalCopy(ItemId),
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: {}", self.file, self.problem),
            None => write!(f, "{}: {}", self.file, self.problem),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::DuplicateBrief { number, other } => write!(
                f,
                "duplicate brief number {number}, also {other}; give one of the two another number"
            ),
            Problem::Status(BadStatus::Missing) => {
                write!(f, "no `status:` line; {OPEN_STATUSES}")
            }
            Problem::Status(BadStatus::Unknown(found)) => {
                write!(
                    f,
                    "unknown status `{}`; {OPEN_STATUSES}",
                    found.escape_debug()
                )
            }
            Problem::HeaderId { found, name } => write!(
                f,
                "header id `{}` differs from the file name's {name}",
                found.escape_debug()
            ),
            Problem::ItemHeaderId { found, name } => write!(
                f,
                "header id `{}` differs from the file name's {name}",
                found.escape_debug()
            ),
            Problem::MissingKey(key) => write!(f, "no `{key}:` line"),
            Problem::BadOrder(found) => write!(
                f,
                "order `{}` is no integer, so the item is listed last",
                found.escape_debug()
            ),
            Problem::NoTitle => f.write_str("no `# ` title line after the header"),
            Problem::Misnamed => f.write_str(
                "not named as the board names its files (briefs/[done/]<number>-<slug>.md, \
                 chores/c<id>.md, backlog/b<id>.md), so it is not synced",
            ),
            Problem::OldList => f.write_str(
                "a list file of the older storage, which this version does not read: its items \
                 are not listed, synced or closed until they are converted to chores/ and backlog/ files",
            ),
            Problem::LocalCopy(ItemId::Brief(number)) => write!(
                f,
                "unmerged local version of brief {number}; merge it by hand, then delete this file"
            ),
            Problem::LocalCopy(of) => write!(
                f,
                "unmerged local version of {of}; merge it by hand, then delete this file"
            ),
        }
    }
}

const OPEN_STATUSES: &str =
    "a brief outside briefs/done/ is draft, queued, started, blocked or reported";

/// Every finding of the project, in a fixed order: the chores and the
/// backlog items by id, the briefs by number, the misnamed files, the old
/// list files, then the conflict copies.
///
/// A conflict copy is reported on every lint, and so after every write in its
/// project, until it is deleted: the merge says so once, and a local version
/// nobody merges is lost work that nothing else points at.
///
/// A chore without a check is no finding (owner, 2026-11-25: a check is
/// optional, "and then we also don't warn"), nor is a key the tool does not
/// know.
pub fn lint(project: &Project) -> Vec<Finding> {
    let mut findings = Vec::new();
    for kind in ListKind::ALL {
        items(project, kind, &mut findings);
    }
    briefs(project, &mut findings);
    findings.extend(project.misnamed.iter().map(|file| Finding {
        file: file.clone(),
        line: None,
        problem: Problem::Misnamed,
    }));
    findings.extend(project.old_lists.iter().map(|file| Finding {
        file: file.clone(),
        line: None,
        problem: Problem::OldList,
    }));
    findings.extend(project.copies.iter().map(|(file, of)| Finding {
        file: file.clone(),
        line: None,
        problem: Problem::LocalCopy(*of),
    }));
    findings
}

fn items(project: &Project, kind: ListKind, findings: &mut Vec<Finding>) {
    let items = project.list(kind);
    for number in items.numbers() {
        let item = items.get(number).expect("listed");
        let id = list_id(kind, number);
        let file = project.item_path(id).to_string();
        let mut finding = |problem| {
            findings.push(Finding {
                file: file.clone(),
                line: None,
                problem,
            });
        };
        let mut required = vec!["id", "order", "created"];
        if kind == ListKind::Chores {
            required.push("group");
        }
        for key in required {
            if item.get(key).is_none() {
                finding(Problem::MissingKey(key));
            }
        }
        if let Some(found) = item.get("id")
            && found != id.to_string().as_bytes()
        {
            finding(Problem::ItemHeaderId {
                found: String::from_utf8_lossy(found).into_owned(),
                name: id,
            });
        }
        if let Err(BadOrder::NotANumber(found)) = item.order() {
            finding(Problem::BadOrder(found));
        }
        if item.title().is_none() {
            finding(Problem::NoTitle);
        }
    }
}

fn list_id(kind: ListKind, number: u32) -> ListId {
    match kind {
        ListKind::Chores => {
            ListId::Chore(crate::id::ChoreId::new(number).expect("an item's id is past 0"))
        }
        ListKind::Backlog => {
            ListId::Backlog(crate::id::BacklogId::new(number).expect("an item's id is past 0"))
        }
    }
}

fn briefs(project: &Project, findings: &mut Vec<Finding>) {
    let mut first_files: BTreeMap<BriefId, String> = BTreeMap::new();
    for file in &project.briefs {
        let number = file.name.number();
        let path = file.path(&project.name).to_string();
        let mut finding = |problem| {
            findings.push(Finding {
                file: path.clone(),
                line: None,
                problem,
            });
        };
        let first_file = first_files.entry(number).or_insert_with(|| path.clone());
        if *first_file != path {
            finding(Problem::DuplicateBrief {
                number,
                other: first_file.clone(),
            });
        }
        if let Err(bad) = file.status() {
            finding(Problem::Status(bad));
        }
        // A brief without an `id:` line is fine: the file name has the
        // number, and an archived brief has no header at all.
        if let Some(found) = file.brief.get("id")
            && BriefId::parse(found) != Some(number)
        {
            finding(Problem::HeaderId {
                found: String::from_utf8_lossy(found).into_owned(),
                name: number,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brief::Brief;
    use crate::id::{BacklogId, ChoreId};
    use crate::item::Item;
    use crate::project::{BoardPath, BriefFile, ProjectName};

    fn shop() -> Project {
        Project::empty(ProjectName::parse("shop").unwrap())
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

    fn with_briefs(briefs: Vec<BriefFile>) -> Project {
        Project { briefs, ..shop() }
    }

    fn lines(project: &Project) -> Vec<String> {
        lint(project).iter().map(Finding::to_string).collect()
    }

    #[test]
    fn an_empty_project_has_no_findings() {
        assert_eq!(lint(&shop()), []);
    }

    #[test]
    fn an_open_brief_needs_a_status_a_command_can_move_on_from() {
        let project = with_briefs(vec![
            brief("briefs/1-a.md", "id: 1\nstatus: started\n"),
            brief("briefs/2-b.md", "# No header\n"),
            brief("briefs/3-c.md", "id: 3\nstatus: strated\n"),
            brief("briefs/4-d.md", "id: 4\nstatus: filed\n"),
            brief("briefs/5-e.md", "id: 5\nstatus:\n"),
        ]);
        assert_eq!(
            lines(&project),
            [
                "shop/briefs/2-b.md: no `status:` line; \
                 a brief outside briefs/done/ is draft, queued, started, blocked or reported",
                "shop/briefs/3-c.md: unknown status `strated`; \
                 a brief outside briefs/done/ is draft, queued, started, blocked or reported",
                "shop/briefs/4-d.md: unknown status `filed`; \
                 a brief outside briefs/done/ is draft, queued, started, blocked or reported",
                "shop/briefs/5-e.md: unknown status ``; \
                 a brief outside briefs/done/ is draft, queued, started, blocked or reported",
            ]
        );
    }

    #[test]
    fn a_filed_brief_needs_no_header() {
        let project = with_briefs(vec![
            brief(
                "briefs/done/1-archived.md",
                "# An old brief\n\nNo header.\n",
            ),
            brief("briefs/done/2-moved-by-hand.md", "id: 2\nstatus: started\n"),
            brief("briefs/done/3-filed.md", "id: 3\nstatus: filed\n"),
        ]);
        assert_eq!(lint(&project), []);
    }

    #[test]
    fn the_header_id_is_the_number_of_the_file_name() {
        let project = with_briefs(vec![
            brief("briefs/007-a.md", "id: 7\nstatus: draft\n"),
            brief("briefs/8-b.md", "id: 9\nstatus: draft\n"),
            brief("briefs/9-c.md", "id: nine\nstatus: draft\n"),
            brief("briefs/done/10-d.md", "id: 11\n"),
            brief("briefs/done/12-e.md", "status: filed\n"),
        ]);
        assert_eq!(
            lines(&project),
            [
                "shop/briefs/8-b.md: header id `9` differs from the file name's 8",
                "shop/briefs/9-c.md: header id `nine` differs from the file name's 9",
                "shop/briefs/done/10-d.md: header id `11` differs from the file name's 10",
            ]
        );
    }

    #[test]
    fn a_brief_number_is_on_one_file_open_or_filed() {
        let project = with_briefs(vec![
            brief("briefs/1-a.md", "id: 1\nstatus: draft\n"),
            brief("briefs/2-a.md", "id: 2\nstatus: draft\n"),
            brief("briefs/2-b.md", "id: 2\nstatus: draft\n"),
            brief("briefs/done/002-c.md", ""),
            brief("briefs/done/3-a.md", ""),
        ]);
        assert_eq!(
            lines(&project),
            [
                "shop/briefs/2-b.md: duplicate brief number 2, also shop/briefs/2-a.md; \
                 give one of the two another number",
                "shop/briefs/done/002-c.md: duplicate brief number 2, also shop/briefs/2-a.md; \
                 give one of the two another number",
            ]
        );
    }

    #[test]
    fn a_misnamed_file_is_named() {
        let project = Project {
            misnamed: vec!["shop/briefs/notes.md".to_owned()],
            ..shop()
        };
        assert_eq!(
            lines(&project),
            [
                "shop/briefs/notes.md: not named as the board names its files \
                 (briefs/[done/]<number>-<slug>.md, chores/c<id>.md, backlog/b<id>.md), \
                 so it is not synced"
            ]
        );
    }

    #[test]
    fn a_list_file_of_the_older_storage_is_named() {
        let project = Project {
            old_lists: vec!["shop/backlog.md".to_owned()],
            ..shop()
        };
        assert_eq!(
            lines(&project),
            [
                "shop/backlog.md: a list file of the older storage, which this version does not \
                 read: its items are not listed, synced or closed until they are converted to \
                 chores/ and backlog/ files"
            ]
        );
    }

    #[test]
    fn a_conflict_copy_is_named() {
        let project = Project {
            copies: vec![
                (
                    "shop/briefs/done/128-x.md.local2".to_owned(),
                    ItemId::Brief(BriefId::new(128)),
                ),
                (
                    "shop/chores/c3.md.local".to_owned(),
                    ItemId::Chore(ChoreId::new(3).unwrap()),
                ),
            ],
            ..shop()
        };
        assert_eq!(
            lines(&project),
            [
                "shop/briefs/done/128-x.md.local2: unmerged local version of brief 128; \
                 merge it by hand, then delete this file",
                "shop/chores/c3.md.local: unmerged local version of c3; \
                 merge it by hand, then delete this file",
            ]
        );
    }

    fn with_items(chores: &[(u32, &str)], backlog: &[(u32, &str)]) -> Project {
        let mut project = shop();
        for (kind, items) in [(ListKind::Chores, chores), (ListKind::Backlog, backlog)] {
            for (number, text) in items {
                project
                    .list_mut(kind)
                    .insert(*number, Item::parse(text.as_bytes()));
            }
        }
        project
    }

    #[test]
    fn a_well_formed_item_has_no_findings_with_or_without_a_check() {
        let project = with_items(
            &[
                (
                    1,
                    "id: c1\ngroup: Home\norder: 1000\ncreated: x\n\n# Do it\n",
                ),
                // No check, an empty check, a key the tool does not know.
                (
                    2,
                    "id: c2\ngroup: Home\norder: -5\ncheck:\nowner: me\ncreated: x\n\n# Do it\n",
                ),
            ],
            &[(
                1,
                "id: b1\norder: 1000\ncreated: x\ngroup: kept\n\n# Later\n\nbody\n",
            )],
        );
        assert_eq!(lint(&project), []);
    }

    #[test]
    fn an_item_needs_its_keys() {
        let project = with_items(
            &[(1, "id: c1\n\n# Do it\n")],
            &[(1, "order: 1000\n\n# Later\n")],
        );
        assert_eq!(
            lines(&project),
            [
                "shop/chores/c1.md: no `order:` line",
                "shop/chores/c1.md: no `created:` line",
                "shop/chores/c1.md: no `group:` line",
                "shop/backlog/b1.md: no `id:` line",
                "shop/backlog/b1.md: no `created:` line",
            ]
        );
    }

    #[test]
    fn an_item_s_header_id_is_its_file_name_s() {
        let project = with_items(
            &[(1, "id: c2\ngroup: g\norder: 1\ncreated: x\n\n# Do it\n")],
            &[(4, "id: c4\norder: 1\ncreated: x\n\n# Later\n")],
        );
        assert_eq!(
            lines(&project),
            [
                "shop/chores/c1.md: header id `c2` differs from the file name's c1",
                "shop/backlog/b4.md: header id `c4` differs from the file name's b4",
            ]
        );
    }

    #[test]
    fn an_item_s_order_is_an_integer() {
        let project = with_items(&[], &[(1, "id: b1\norder: first\ncreated: x\n\n# Later\n")]);
        assert_eq!(
            lines(&project),
            ["shop/backlog/b1.md: order `first` is no integer, so the item is listed last"]
        );
    }

    #[test]
    fn an_item_has_a_title_line() {
        let project = with_items(
            &[],
            &[(1, "id: b1\norder: 1\ncreated: x\n\nNo title here\n")],
        );
        assert_eq!(
            lines(&project),
            ["shop/backlog/b1.md: no `# ` title line after the header"]
        );
        let _ = BacklogId::new(1);
    }
}
