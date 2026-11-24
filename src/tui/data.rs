//! What the TUI shows of a project, read from its files at each reload.

use std::collections::BTreeSet;

use crate::board::{BriefRow, arrange};
use crate::brief::Stamp;
use crate::id::{BriefId, ListKind};
use crate::list::{Entry, ListFile};
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

/// A heading or an item of a list file, and the line of the file it is on:
/// where `e` opens the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLine {
    pub entry: ListEntry,
    /// Counted from 1, as an editor's `+N` counts.
    pub line: usize,
}

/// A row `ls` shows of a list (`board::ListRow`), with the id the CLI reads
/// on an item's line: the TUI's actions name the item by it, so it is the
/// parser's (`list::parse_line`), never read off the text a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEntry {
    /// A `## group` heading, as its name.
    Group(Vec<u8>),
    /// An item, as its line without the `- `, and the contents of the
    /// indented lines below it, which are its own: the preview shows them.
    Item {
        id: Option<u32>,
        text: Vec<u8>,
        indented: Vec<Vec<u8>>,
    },
}

impl BriefItem {
    /// Its row, and whether its file is in `briefs/done/`.
    fn row_in_done(&mut self) -> (&mut BriefRow, bool) {
        let in_done = matches!(self.path, BoardPath::Brief { filed: true, .. });
        (&mut self.row, in_done)
    }

    fn of(project: &ProjectName, file: &BriefFile) -> BriefItem {
        BriefItem {
            row: BriefRow::of(file),
            path: file.path(project),
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
            .map(|file| BriefItem::of(name, file));
        let (open, filed) = arrange(briefs, &BTreeSet::new(), BriefItem::row_in_done);
        Data {
            project: name.clone(),
            open,
            filed,
            chores: list_lines(project.list(ListKind::Chores)),
            backlog: list_lines(project.list(ListKind::Backlog)),
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

/// The rows `ls` shows of a list (`board::list_rows`), each with its line.
fn list_lines(list: &ListFile) -> Vec<ListLine> {
    list.entries()
        .enumerate()
        .filter_map(|(index, entry)| {
            let entry = match entry {
                Entry::Heading(name) => ListEntry::Group(name.to_vec()),
                Entry::Item { id, text } => ListEntry::Item {
                    id,
                    text: text.to_vec(),
                    indented: list.indented_below(index).map(<[u8]>::to_vec).collect(),
                },
                Entry::Text(_) => return None,
            };
            Some(ListLine {
                entry,
                line: index + 1,
            })
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
    /// Rows on the lines of a file that holds nothing else, read as a load
    /// reads them: an item's id is the parser's.
    pub fn numbered(kind: ListKind, rows: Vec<crate::board::ListRow>) -> Vec<ListLine> {
        use crate::board::ListRow;
        let mut file = Vec::new();
        for row in rows {
            let (mark, text) = match row {
                ListRow::Group(name) => (&b"## "[..], name),
                ListRow::Item(text) => (&b"- "[..], text),
            };
            assert!(!text.contains(&b'\n'), "one row, one line");
            file.extend_from_slice(mark);
            file.extend_from_slice(&text);
            file.push(b'\n');
        }
        list_lines(&ListFile::parse(kind, &file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{ListRow, Snapshot};

    #[test]
    fn a_list_s_rows_are_ls_s_with_the_lines_they_are_on() {
        let mut project = Project::empty(ProjectName::parse("shop").unwrap());
        project.chores = ListFile::parse(
            ListKind::Chores,
            b"# Chores\n\n## home\n- c1 one. Check: x\n  more of c1\n- by hand. Check: y\n\
              - c2\tid typed with a tab. Check: z\n",
        );
        let data = Data::of(&project);
        let (rows, ids): (Vec<ListRow>, Vec<Option<u32>>) = data
            .chores
            .iter()
            .map(|line| match &line.entry {
                ListEntry::Group(name) => (ListRow::Group(name.clone()), None),
                ListEntry::Item { id, text, .. } => (ListRow::Item(text.clone()), *id),
            })
            .unzip();
        assert_eq!(rows, Snapshot::of(&project, &BTreeSet::new()).chores);
        // As the CLI reads them: `c2<tab>` is no id (`list::parse_line`).
        assert_eq!(ids, [None, Some(1), None, None]);
        let lines: Vec<usize> = data.chores.iter().map(|line| line.line).collect();
        assert_eq!(lines, [3, 4, 6, 7]);
    }
}
