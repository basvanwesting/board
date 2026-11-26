//! What the TUI shows of a project, read from its files at each reload.

use std::collections::BTreeSet;

use crate::board::{BriefRow, arrange, item_waits};
use crate::brief::Stamp;
use crate::id::{BriefId, ItemId, ListKind};
use crate::item::Row;
use crate::project::{BoardPath, BriefFile, Project, ProjectName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Data {
    pub project: ProjectName,
    /// In the board's order: in flight first (SPEC.md, TUI).
    pub open: Vec<BriefItem>,
    pub filed: Vec<BriefItem>,
    pub chores: Vec<ListLine>,
    pub backlog: Vec<ListLine>,
}

/// A brief with what its row and its preview show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefItem {
    pub row: BriefRow,
    /// Where the file is: what `e` opens.
    pub path: BoardPath,
    /// The `started:` stamp: in flight since, for a started or blocked
    /// brief. A resume keeps the first start (see `Transition::stamps`).
    pub started: Option<Stamp>,
    /// The whole file, as the preview shows it.
    pub text: Vec<u8>,
}

/// A row `ls` shows of a kind (`board::ListRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLine {
    pub entry: ListEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEntry {
    /// A chores' group, as its name.
    Group(Vec<u8>),
    /// An item: its id (its file's name), its line as `ls` prints it, and
    /// the item as `show` prints it, which the preview shows and `yy`
    /// yanks.
    Item {
        id: u32,
        text: Vec<u8>,
        shown: Vec<u8>,
        /// Whether its file has a body.
        body: bool,
        /// What it still waits on: its row ends with `  after <id>`.
        waits: Option<ItemId>,
    },
}

impl BriefItem {
    /// Its row, and whether its file is in `briefs/done/`.
    fn row_in_done(&mut self) -> (&mut BriefRow, bool) {
        let in_done = matches!(self.path, BoardPath::Brief { filed: true, .. });
        (&mut self.row, in_done)
    }

    fn of(project: &Project, file: &BriefFile) -> BriefItem {
        BriefItem {
            row: BriefRow::of(project, file),
            path: file.path(project.name()),
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
    /// The files as read; no brief is `unpushed` until [`Data::label`]
    /// is given the set, which comes from git and not from the files.
    pub fn of(project: &Project) -> Data {
        let name = project.name();
        let briefs = project
            .briefs()
            .iter()
            .map(|file| BriefItem::of(project, file));
        let (open, filed) = arrange(briefs, &BTreeSet::new(), BriefItem::row_in_done);
        Data {
            project: name.clone(),
            open,
            filed,
            chores: list_lines(project, ListKind::Chores),
            backlog: list_lines(project, ListKind::Backlog),
        }
    }

    /// The briefs labelled anew by the set of briefs with unpushed commits,
    /// in the tabs and the order that gives: an unpushed brief is in the
    /// Briefs tab, the other filed ones in Filed.
    pub fn label(&mut self, unpushed: &BTreeSet<BriefId>) {
        let briefs = std::mem::take(&mut self.open)
            .into_iter()
            .chain(std::mem::take(&mut self.filed));
        (self.open, self.filed) = arrange(briefs, unpushed, BriefItem::row_in_done);
    }

    /// The board before its first load.
    pub fn empty(project: ProjectName) -> Data {
        Data::of(&Project::empty(project))
    }

    pub fn list(&self, kind: ListKind) -> &[ListLine] {
        match kind {
            ListKind::Chores => &self.chores,
            ListKind::Backlog => &self.backlog,
        }
    }
}

/// The rows `ls` shows of a kind (`board::list_rows`).
fn list_lines(project: &Project, kind: ListKind) -> Vec<ListLine> {
    let items = project.list(kind);
    items
        .rows()
        .into_iter()
        .map(|row| ListLine {
            entry: match row {
                Row::Group(name) => ListEntry::Group(name.to_vec()),
                Row::Item(listed) => ListEntry::Item {
                    id: listed.number,
                    text: listed.item.line(items.kind(), listed.number),
                    shown: listed.item.shown(items.kind(), listed.number),
                    body: !listed.item.body().is_empty(),
                    waits: item_waits(project, kind, listed.number),
                },
            },
        })
        .collect()
}

/// The data of the state's and the view's tests, without files.
#[cfg(test)]
impl BriefItem {
    /// At its path in `project`, from its number and slug; filed when its
    /// status says so.
    pub fn sample(
        project: &ProjectName,
        row: BriefRow,
        started: Option<Stamp>,
        text: Vec<u8>,
    ) -> BriefItem {
        let name = crate::project::BriefName::parse(&format!("{}-{}.md", row.number, row.slug))
            .expect("a brief's file name");
        BriefItem {
            path: BoardPath::Brief {
                project: project.clone(),
                name,
                filed: row.status == Some(crate::brief::Status::Filed),
            },
            row,
            started,
            text,
        }
    }
}

#[cfg(test)]
impl ListLine {
    /// The rows as given, an item's id read off its first word (`c12`),
    /// without a body: what a load gives of files that list so.
    pub fn numbered(kind: ListKind, rows: Vec<crate::board::ListRow>) -> Vec<ListLine> {
        use crate::board::ListRow;
        rows.into_iter()
            .map(|row| ListLine {
                entry: match row {
                    ListRow::Group(name) => ListEntry::Group(name),
                    ListRow::Item(text) => {
                        let word = text.split(|b| *b == b' ').next().unwrap_or(&text);
                        let id = kind.parse_id(word).expect("a test row starts with its id");
                        let shown = [text.as_slice(), b"\n"].concat();
                        ListEntry::Item {
                            id,
                            text,
                            shown,
                            body: false,
                            waits: None,
                        }
                    }
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{ListRow, Snapshot};
    use crate::item::Item;

    #[test]
    fn a_kind_s_rows_are_ls_s_with_what_show_prints() {
        let mut project = Project::empty(ProjectName::parse("shop").unwrap());
        for (number, text) in [
            (1, "id: c1\ngroup: home\n\n# one\n\nmore of c1\n"),
            (2, "id: c2\ngroup: away\ncheck: z\n\n# two\n"),
        ] {
            project
                .list_mut(ListKind::Chores)
                .insert(number, Item::parse(text.as_bytes()));
        }
        let data = Data::of(&project);
        let rows: Vec<ListRow> = data
            .chores
            .iter()
            .map(|line| match &line.entry {
                ListEntry::Group(name) => ListRow::Group(name.clone()),
                ListEntry::Item { text, .. } => ListRow::Item(text.clone()),
            })
            .collect();
        assert_eq!(rows, Snapshot::of(&project, &BTreeSet::new()).chores);
        let shown: Vec<(u32, &[u8], bool)> = data
            .chores
            .iter()
            .filter_map(|line| match &line.entry {
                ListEntry::Item {
                    id, shown, body, ..
                } => Some((*id, shown.as_slice(), *body)),
                ListEntry::Group(_) => None,
            })
            .collect();
        assert_eq!(
            shown,
            [
                (2, &b"c2 two. Check: z\n"[..], false),
                (1, &b"c1 one\n\nmore of c1\n"[..], true),
            ]
        );
    }
}
