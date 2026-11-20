//! Where a project's files sit in the board-data repo.

use std::fmt;

use crate::id::{BriefId, ListKind};

/// A project's directory name in the board-data repo:
/// `[A-Za-z0-9][A-Za-z0-9._-]*`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectName(String);

impl ProjectName {
    pub fn parse(name: &str) -> Option<ProjectName> {
        let mut bytes = name.bytes();
        let valid = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
            && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        valid.then(|| ProjectName(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A brief's file name: `<digits>-<one or more chars, no "/">.md`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BriefName {
    number: BriefId,
    file_name: String,
    /// Length of the leading digits; the number alone cannot say, `007` is 7.
    digits: usize,
}

impl BriefName {
    pub fn parse(file_name: &str) -> Option<BriefName> {
        let (digits, rest) = file_name.split_once('-')?;
        let slug = rest.strip_suffix(".md")?;
        if slug.is_empty() || slug.contains('/') {
            return None;
        }
        Some(BriefName {
            number: BriefId::parse(digits.as_bytes())?,
            file_name: file_name.to_owned(),
            digits: digits.len(),
        })
    }

    pub fn number(&self) -> BriefId {
        self.number
    }

    /// The name without its number and `.md`.
    pub fn slug(&self) -> &str {
        &self.file_name[self.digits + "-".len()..self.file_name.len() - ".md".len()]
    }

    pub fn as_str(&self) -> &str {
        &self.file_name
    }
}

/// A path the board owns, relative to `$BOARD_ROOT`:
///
/// ```text
/// <project>/chores.md | <project>/backlog.md
/// <project>/briefs/<brief> | <project>/briefs/done/<brief>
/// ```
///
/// Anything else in the repo is not the board's: never committed by the tool,
/// never merged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BoardPath {
    List {
        project: ProjectName,
        kind: ListKind,
    },
    Brief {
        project: ProjectName,
        name: BriefName,
        /// Under `briefs/done/`.
        filed: bool,
    },
}

impl BoardPath {
    /// The single path grammar: the worktree scan, the commit of hand edits,
    /// the merge and lint all ask here whether a path is the board's.
    ///
    /// It takes `&str` and no bytes: a file name that is not UTF-8 is not the
    /// board's, like any other name outside the grammar, so no caller has to
    /// carry raw bytes into output that agents read. Every path has one
    /// spelling (`Display` gives it back), so a path can serve as a key.
    pub fn parse(path: &str) -> Option<BoardPath> {
        let (project, rest) = path.split_once('/')?;
        let project = ProjectName::parse(project)?;
        if let Some(kind) = ListKind::ALL.into_iter().find(|k| k.file_name() == rest) {
            return Some(BoardPath::List { project, kind });
        }
        let brief = rest.strip_prefix("briefs/")?;
        let (filed, file_name) = match brief.strip_prefix("done/") {
            Some(file_name) => (true, file_name),
            None => (false, brief),
        };
        let name = BriefName::parse(file_name)?;
        Some(BoardPath::Brief {
            project,
            name,
            filed,
        })
    }

    pub fn project(&self) -> &ProjectName {
        match self {
            BoardPath::List { project, .. } | BoardPath::Brief { project, .. } => project,
        }
    }
}

impl fmt::Display for BoardPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoardPath::List { project, kind } => write!(f, "{project}/{}", kind.file_name()),
            BoardPath::Brief {
                project,
                name,
                filed: false,
            } => write!(f, "{project}/briefs/{}", name.as_str()),
            BoardPath::Brief {
                project,
                name,
                filed: true,
            } => write!(f, "{project}/briefs/done/{}", name.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn brief(path: &str) -> Option<(String, u32, String, bool)> {
        match BoardPath::parse(path)? {
            BoardPath::Brief {
                project,
                name,
                filed,
            } => Some((
                project.to_string(),
                name.number().get(),
                name.slug().to_owned(),
                filed,
            )),
            BoardPath::List { .. } => None,
        }
    }

    #[test]
    fn the_four_shapes() {
        let shop = ProjectName::parse("shop").unwrap();
        assert_eq!(
            BoardPath::parse("shop/chores.md"),
            Some(BoardPath::List {
                project: shop.clone(),
                kind: ListKind::Chores
            })
        );
        assert_eq!(
            BoardPath::parse("shop/backlog.md"),
            Some(BoardPath::List {
                project: shop,
                kind: ListKind::Backlog
            })
        );
        assert_eq!(
            brief("tools-shop/briefs/128-some-slug.md"),
            Some(("tools-shop".into(), 128, "some-slug".into(), false))
        );
        assert_eq!(
            brief("shop/briefs/done/007-Old Notes.v2.md"),
            Some(("shop".into(), 7, "Old Notes.v2".into(), true))
        );
    }

    #[test]
    fn not_the_boards() {
        for path in [
            "",
            "chores.md",
            "README.md",
            "shop",
            "shop/",
            "shop/README.md",
            "shop/chores.md/",
            "shop/Chores.md",
            "shop//chores.md",
            "./shop/chores.md",
            "/shop/chores.md",
            ".git/chores.md",
            "-shop/chores.md",
            "sh op/chores.md",
            "shop/sub/chores.md",
            "shop/done/1-a.md",
            "shop/briefs/notes.md",
            "shop/briefs/128.md",
            "shop/briefs/128-.md",
            "shop/briefs/128-a",
            "shop/briefs/128-a.md.local",
            "shop/briefs/-a.md",
            "shop/briefs/a-128.md",
            "shop/briefs/+128-a.md",
            "shop/briefs/4294967296-a.md",
            "shop/briefs/sub/128-a.md",
            "shop/briefs/done/done/128-a.md",
            "shop/briefs/done/128-a/b.md",
        ] {
            assert_eq!(BoardPath::parse(path), None, "{path}");
        }
    }

    #[test]
    fn a_brief_named_done_dash_something_is_open() {
        // "done-…" is no number, so `briefs/done/` can never be read as a brief.
        assert_eq!(brief("shop/briefs/done-1-a.md"), None);
        assert_eq!(
            brief("shop/briefs/1-done.md"),
            Some(("shop".into(), 1, "done".into(), false))
        );
    }

    fn any_board_path() -> impl Strategy<Value = String> {
        let project = "[A-Za-z0-9][A-Za-z0-9._-]{0,8}";
        let brief = "[0-9]{1,9}-[^/]{1,12}\\.md";
        prop_oneof![
            (project, "(chores|backlog)\\.md").prop_map(|(p, f)| format!("{p}/{f}")),
            (project, "(done/)?", brief).prop_map(|(p, d, b)| format!("{p}/briefs/{d}{b}")),
        ]
    }

    /// Near the grammar on purpose, about half of these parse: an extra
    /// `done/`, a slash in the name, a missing number or `.md`.
    fn almost_a_board_path() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z0-9.]{1,3}/(chores|backlog|notes)\\.md/?",
            "[a-z0-9.]{1,3}/briefs/(done/){0,3}[0-9]{0,3}-?[a-z/]{0,2}\\.md",
            "([a-z.]{0,3}/){0,2}(chores\\.md|briefs/|done/|[0-9]{0,2}-[a-z]{0,2}(\\.md)?){0,3}",
        ]
    }

    proptest! {
        #[test]
        fn every_path_of_the_grammar_parses_and_prints_itself(path in any_board_path()) {
            let parsed = BoardPath::parse(&path);
            prop_assert_eq!(parsed.map(|p| p.to_string()), Some(path));
        }

        /// One spelling per path: what parses prints back as it was read, so
        /// two strings never name the same board file.
        #[test]
        fn a_path_that_parses_prints_itself(path in almost_a_board_path()) {
            if let Some(parsed) = BoardPath::parse(&path) {
                prop_assert_eq!(parsed.to_string(), &path[..]);
                // Each part is one path segment.
                let segments = match parsed {
                    BoardPath::List { .. } => 2,
                    BoardPath::Brief { filed: false, .. } => 3,
                    BoardPath::Brief { filed: true, .. } => 4,
                };
                prop_assert_eq!(path.split('/').count(), segments);
            }
        }
    }
}
