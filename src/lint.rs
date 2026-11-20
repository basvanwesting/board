//! What is wrong with a project's files, for the user to fix by hand.
//!
//! The parsers take anything, so a hand edit never stops the tool. Lint is
//! where the edits that leave an item unusable come up: an id that names two
//! things, a chore nobody can see is done, a brief no command will move.

use std::collections::BTreeMap;
use std::fmt;

use crate::brief::BadStatus;
use crate::id::{BriefId, ListKind};
use crate::list::{Entry, has_check};
use crate::project::{BoardPath, Project};

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
    /// A list id that an earlier line of the file carries too.
    DuplicateId {
        kind: ListKind,
        number: u32,
        first_line: usize,
    },
    /// A brief number that another file of the project carries too.
    DuplicateBrief { number: BriefId, other: String },
    /// A chore that does not say how to see it is done.
    NoCheck,
    /// A brief outside `briefs/done/` without a status a command can move on
    /// from.
    Status(BadStatus),
    /// The header's `id:` is not the number in the file name. The file name
    /// is the one the tool goes by.
    HeaderId { found: String, name: BriefId },
    /// An `.md` file below `briefs/` that is not at
    /// `briefs/[done/]<number>-<slug>.md`.
    Misnamed,
    /// The local version of a brief that lost to the remote's in a merge.
    LocalCopy(BriefId),
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
            Problem::DuplicateId {
                kind,
                number,
                first_line,
            } => write!(
                f,
                "duplicate id {}{number}, first on line {first_line}; \
                 delete the id on one of the lines and that line gets a new one",
                kind.prefix()
            ),
            Problem::DuplicateBrief { number, other } => write!(
                f,
                "duplicate brief number {number}, also {other}; give one of the two another number"
            ),
            Problem::NoCheck => {
                f.write_str("chore without a check; say after `Check:` how to see that it is done")
            }
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
            Problem::Misnamed => f.write_str(
                "not at briefs/[done/]<number>-<slug>.md, so it is not a brief and not synced",
            ),
            Problem::LocalCopy(number) => write!(
                f,
                "unmerged local version of brief {number}; merge it by hand, then delete this file"
            ),
        }
    }
}

const OPEN_STATUSES: &str =
    "a brief outside briefs/done/ is draft, queued, started, blocked or reported";

/// Every finding of the project, in a fixed order: `chores.md`, `backlog.md`,
/// the briefs by number, the misnamed files, then the conflict copies.
///
/// A conflict copy is reported on every lint, and so after every write in its
/// project, until it is deleted: the merge says so once, and a local version
/// nobody merges is lost work that nothing else points at.
pub fn lint(project: &Project) -> Vec<Finding> {
    let mut findings = Vec::new();
    for kind in ListKind::ALL {
        list(project, kind, &mut findings);
    }
    briefs(project, &mut findings);
    findings.extend(project.misnamed.iter().map(|file| Finding {
        file: file.clone(),
        line: None,
        problem: Problem::Misnamed,
    }));
    findings.extend(project.copies.iter().map(|(file, number)| Finding {
        file: file.clone(),
        line: None,
        problem: Problem::LocalCopy(*number),
    }));
    findings
}

fn list(project: &Project, kind: ListKind, findings: &mut Vec<Finding>) {
    let file = BoardPath::List {
        project: project.name.clone(),
        kind,
    }
    .to_string();
    let mut first_lines: BTreeMap<u32, usize> = BTreeMap::new();
    for (index, entry) in project.list(kind).entries().enumerate() {
        let line = index + 1;
        let Entry::Item { id, text } = entry else {
            continue;
        };
        let mut finding = |problem| {
            findings.push(Finding {
                file: file.clone(),
                line: Some(line),
                problem,
            });
        };
        if let Some(number) = id {
            let first_line = *first_lines.entry(number).or_insert(line);
            if first_line != line {
                finding(Problem::DuplicateId {
                    kind,
                    number,
                    first_line,
                });
            }
        }
        match kind {
            ListKind::Chores if !has_check(text) => finding(Problem::NoCheck),
            ListKind::Chores | ListKind::Backlog => {}
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
    use crate::list::{FindError, ListFile};
    use crate::project::{BriefFile, ProjectName};
    use proptest::prelude::*;

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
    fn every_line_after_the_first_with_an_id_is_a_duplicate() {
        let project = Project {
            chores: ListFile::parse(
                ListKind::Chores,
                b"## Home\n- c2 a. Check: x\n\n- c1 b. Check: x\n- c2 c. Check: x\n- c2 d. Check: x\n",
            ),
            backlog: ListFile::parse(ListKind::Backlog, b"- b2 a\n- b2 b\n- c2 other list\n"),
            ..shop()
        };
        assert_eq!(
            lines(&project),
            [
                "shop/chores.md:5: duplicate id c2, first on line 2; \
                 delete the id on one of the lines and that line gets a new one",
                "shop/chores.md:6: duplicate id c2, first on line 2; \
                 delete the id on one of the lines and that line gets a new one",
                "shop/backlog.md:2: duplicate id b2, first on line 1; \
                 delete the id on one of the lines and that line gets a new one",
            ]
        );
    }

    #[test]
    fn a_chore_needs_a_check_and_a_backlog_item_does_not() {
        let project = Project {
            chores: ListFile::parse(
                ListKind::Chores,
                b"# Chores\n## Home\n- c1 Do it\n- no id either. Check:\n  Check: on a continuation line\n- c2 Do it. Check: the page loads  (126)\n",
            ),
            backlog: ListFile::parse(ListKind::Backlog, b"- b1 Later\n"),
            ..shop()
        };
        assert_eq!(
            lines(&project),
            [
                "shop/chores.md:3: chore without a check; say after `Check:` how to see that it is done",
                "shop/chores.md:4: chore without a check; say after `Check:` how to see that it is done",
            ]
        );
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
                "shop/briefs/notes.md: not at briefs/[done/]<number>-<slug>.md, \
                 so it is not a brief and not synced"
            ]
        );
    }

    #[test]
    fn a_conflict_copy_is_named() {
        let project = Project {
            copies: vec![(
                "shop/briefs/done/128-x.md.local2".to_owned(),
                BriefId::new(128),
            )],
            ..shop()
        };
        assert_eq!(
            lines(&project),
            [
                "shop/briefs/done/128-x.md.local2: unmerged local version of brief 128; \
                 merge it by hand, then delete this file"
            ]
        );
    }

    /// Few distinct ids and checks, so that duplicates and missing checks are
    /// the common case.
    fn list_text() -> impl Strategy<Value = String> {
        let line = prop_oneof![
            "- [bc][1-4]( [a-z]{1,3})?(\\. Check:( [a-z]{1,3})?)?",
            "- [a-z]{1,3}",
            "## [A-Z][a-z]{0,2}",
            "( |  [a-z]{1,3}|[a-z]{1,3})?",
        ];
        prop::collection::vec(line, 0..12).prop_map(|lines| lines.join("\n"))
    }

    fn list_kind() -> impl Strategy<Value = ListKind> {
        prop_oneof![Just(ListKind::Chores), Just(ListKind::Backlog)]
    }

    proptest! {
        /// Lint and the commands agree on what a duplicate is: an id has a
        /// duplicate finding exactly when a command would refuse it as
        /// sitting on several lines, one finding per line after the first,
        /// each on a line that carries the id.
        #[test]
        fn an_id_is_reported_as_duplicate_exactly_when_it_cannot_be_found(
            kind in list_kind(),
            text in list_text(),
        ) {
            let file = ListFile::parse(kind, text.as_bytes());
            let project = match kind {
                ListKind::Chores => Project { chores: file.clone(), ..shop() },
                ListKind::Backlog => Project { backlog: file.clone(), ..shop() },
            };
            let findings = lint(&project);
            let entries: Vec<Entry<'_>> = file.entries().collect();
            for number in 1..=4 {
                let reported: Vec<(usize, usize)> = findings
                    .iter()
                    .filter_map(|finding| match finding.problem {
                        Problem::DuplicateId { kind: of, number: n, first_line }
                            if of == kind && n == number =>
                        {
                            Some((finding.line.unwrap(), first_line))
                        }
                        _ => None,
                    })
                    .collect();
                match file.find(number) {
                    Ok(_) | Err(FindError::Missing) => prop_assert_eq!(&reported, &[]),
                    Err(FindError::Duplicate(lines)) => prop_assert_eq!(reported.len(), lines - 1),
                }
                for (line, first_line) in reported {
                    prop_assert!(first_line < line);
                    for at in [line, first_line] {
                        prop_assert!(
                            matches!(entries[at - 1], Entry::Item { id: Some(n), .. } if n == number),
                            "line {} does not carry the id", at
                        );
                    }
                }
            }
        }

        /// A chore is reported exactly when it has no check; the line number
        /// is the line's.
        #[test]
        fn the_chores_without_a_check_are_the_ones_reported(
            kind in list_kind(),
            text in list_text(),
        ) {
            let file = ListFile::parse(kind, text.as_bytes());
            let project = match kind {
                ListKind::Chores => Project { chores: file.clone(), ..shop() },
                ListKind::Backlog => Project { backlog: file.clone(), ..shop() },
            };
            let reported: Vec<usize> = lint(&project)
                .iter()
                .filter(|finding| finding.problem == Problem::NoCheck)
                .map(|finding| finding.line.unwrap())
                .collect();
            let expected: Vec<usize> = file
                .entries()
                .enumerate()
                .filter(|(_, entry)| {
                    kind == ListKind::Chores
                        && matches!(entry, Entry::Item { text, .. } if !has_check(text))
                })
                .map(|(index, _)| index + 1)
                .collect();
            prop_assert_eq!(reported, expected);
        }
    }
}
