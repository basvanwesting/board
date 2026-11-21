//! What the TUI shows of a project, read from its files at each reload.

use crate::board::{BriefRow, ListRow, Snapshot, briefs_in_order};
use crate::brief::Stamp;
use crate::project::{BriefFile, Project, ProjectName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    pub project: ProjectName,
    /// In the board's order: in flight first (SPEC.md, TUI).
    pub open: Vec<BriefItem>,
    pub filed: Vec<BriefItem>,
    pub chores: Vec<ListRow>,
    pub backlog: Vec<ListRow>,
}

/// A brief with what its row and its preview show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefItem {
    pub row: BriefRow,
    /// The `started:` stamp: in flight since, for a started or blocked
    /// brief. A resume keeps the first start (see `Transition::stamps`).
    pub started: Option<Stamp>,
    /// The whole file, as the preview shows it.
    pub text: Vec<u8>,
}

impl BriefItem {
    fn of(file: &BriefFile) -> BriefItem {
        BriefItem {
            row: BriefRow::of(file),
            started: file
                .brief
                .get("started")
                .and_then(|value| str::from_utf8(value).ok())
                .and_then(|value| value.parse().ok()),
            text: file.brief.to_bytes(),
        }
    }
}

impl Data {
    pub fn of(project: &Project) -> Data {
        let briefs = |filed: bool| -> Vec<BriefItem> {
            briefs_in_order(project, filed)
                .into_iter()
                .map(BriefItem::of)
                .collect()
        };
        let Snapshot {
            project: name,
            chores,
            backlog,
            ..
        } = Snapshot::of(project);
        Data {
            project: name,
            open: briefs(false),
            filed: briefs(true),
            chores,
            backlog,
        }
    }

    /// The board before its first load.
    pub fn empty(project: ProjectName) -> Data {
        Data::of(&Project::empty(project))
    }
}
