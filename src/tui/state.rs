//! What the TUI shows, as a pure function of what happened: [`State::step`]
//! takes an event (a key, a resize, the clock, a file change, a result) and
//! gives the next state and the commands for the runtime to run. No IO here,
//! so a trace of random events can test all of it (`trace.rs`).
//!
//! The actions (`d`, `x`, `J`/`K`, `e`, `r`, yank) fit the same shape: a key
//! gives a [`Command`] that runs the CLI's function on the worker, and its
//! result comes back as an [`Event`].

use super::data::{BriefItem, Data};
use super::layout::{self, Areas};
use crate::board::ListRow;
use crate::brief::{Stamp, Status};
use crate::id::{BriefId, ListKind};
use crate::sync::{Counts, Fetched};

/// Seconds between the starts of two background fetches ("about once a
/// minute", SPEC.md, Sync).
pub(crate) const FETCH_EVERY: u64 = 60;

/// A key as the state knows it; the runtime maps the terminal's to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub(crate) enum Event {
    Key(Key),
    Resize {
        width: u16,
        height: u16,
    },
    /// The clock, about once a second: the ages on screen, and the fetch
    /// timer.
    Tick(Stamp),
    /// A file under the project's directory changed.
    FilesChanged,
    /// The answer to [`Command::Load`].
    Loaded(Box<Data>),
    /// The answer to [`Command::Load`] when the files could not be read.
    LoadFailed(String),
    /// The answer to [`Command::Fetch`].
    Fetched(Fetched),
    /// The terminal gives no more keys, so nothing could quit the TUI.
    InputGone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Read the project's files.
    Load,
    /// The background fetch, on the worker thread.
    Fetch,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Panel {
    Briefs = 0,
    Chores = 1,
    Backlog = 2,
}

impl Panel {
    pub const ALL: [Panel; 3] = [Panel::Briefs, Panel::Chores, Panel::Backlog];

    fn next(self) -> Panel {
        Panel::ALL[(self as usize + 1) % 3]
    }

    fn previous(self) -> Panel {
        Panel::ALL[(self as usize + 2) % 3]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Open,
    Filed,
}

/// A list of rows with a cursor of its own: each tab of Briefs, Chores,
/// Backlog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Open = 0,
    Filed = 1,
    Chores = 2,
    Backlog = 3,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::Open, Source::Filed, Source::Chores, Source::Backlog];

    pub fn panel(self) -> Panel {
        match self {
            Source::Open | Source::Filed => Panel::Briefs,
            Source::Chores => Panel::Chores,
            Source::Backlog => Panel::Backlog,
        }
    }
}

/// A row of a panel: an item, or a group heading the cursor passes over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// What the row says, and what a filter matches.
    pub text: String,
    /// `None` for a heading.
    pub key: Option<ItemKey>,
    /// Where the row is in its list of the data.
    pub at: usize,
    /// In flight since: a started or blocked brief.
    pub since: Option<Stamp>,
}

impl Row {
    /// The id at the front of a list item's line.
    pub fn id_word(&self) -> Option<&str> {
        match self.key.as_ref()?.ident {
            Ident::Id(_) => self.text.split_whitespace().next(),
            Ident::Brief(..) | Ident::Text(_) => None,
        }
    }
}

/// What makes a row the same item across reloads: the cursor stays on its
/// item, not its row (SPEC.md, TUI).
///
/// The id where there is one; an item typed by hand without one is its
/// text until a write gives it an id. `nth` tells apart rows with one
/// ident (a hand-typed duplicate id, which lint reports), so the cursor can
/// still reach the second.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ItemKey {
    ident: Ident,
    nth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Ident {
    /// Number and slug: the file name.
    Brief(BriefId, String),
    Id(u32),
    Text(String),
}

/// Where a list's cursor is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Cursor {
    /// The item; `None` when the list shows none.
    key: Option<ItemKey>,
    /// Its place among the list's items: where the cursor lands when the
    /// item is gone.
    at: usize,
    /// The first row drawn.
    offset: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Preview {
    /// The item shown; its scroll starts over when another one is.
    shown: Option<(Source, ItemKey)>,
    scroll: usize,
}

/// The background fetch and what it found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SyncState {
    /// A fetch was asked for and has not answered.
    pub in_flight: bool,
    /// When the last fetch was asked for: the timer counts from here.
    pub asked: Option<Stamp>,
    /// When a fetch last succeeded.
    pub fetched: Option<Stamp>,
    pub counts: Option<Counts>,
    /// Why the last fetch failed; gone at the next one that succeeds.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct State {
    data: Data,
    width: u16,
    height: u16,
    now: Stamp,
    focus: Panel,
    /// The preview has the keys, for scrolling; `focus` is the panel it
    /// shows.
    in_preview: bool,
    tab: Tab,
    cursors: [Cursor; 4],
    /// Per panel; the two tabs of Briefs share one.
    filters: [Option<String>; 3],
    /// Keys go into the focused panel's filter.
    typing: bool,
    /// `g` was pressed: a second one goes to the top.
    pending_g: bool,
    help: bool,
    preview: Preview,
    sync: SyncState,
    load_error: Option<String>,
}

impl State {
    /// The state before anything is read, and the commands that read it.
    pub fn new(data: Data, width: u16, height: u16, now: Stamp) -> (State, Vec<Command>) {
        let mut state = State {
            data,
            width,
            height,
            now,
            focus: Panel::Briefs,
            in_preview: false,
            tab: Tab::Open,
            cursors: Default::default(),
            filters: Default::default(),
            typing: false,
            pending_g: false,
            help: false,
            preview: Preview::default(),
            sync: SyncState::default(),
            load_error: None,
        };
        let mut commands = vec![Command::Load];
        // A fetch at once, not a minute on: until one answers, the status
        // line knows nothing of the remote.
        commands.extend(state.fetch_if_due());
        state.settle();
        (state, commands)
    }

    /// The state after `event`, and what the runtime has to run.
    pub fn step(mut self, event: Event) -> (State, Vec<Command>) {
        let commands = match event {
            Event::Key(key) => self.key(key),
            Event::Resize { width, height } => {
                self.width = width;
                self.height = height;
                Vec::new()
            }
            Event::Tick(now) => {
                self.now = now;
                self.fetch_if_due()
            }
            Event::FilesChanged => vec![Command::Load],
            Event::Loaded(data) => {
                self.data = *data;
                self.load_error = None;
                Vec::new()
            }
            Event::LoadFailed(why) => {
                self.load_error = Some(why);
                Vec::new()
            }
            Event::Fetched(fetched) => {
                self.fetched(fetched);
                Vec::new()
            }
            Event::InputGone => vec![Command::Quit],
        };
        self.settle();
        (self, commands)
    }

    fn key(&mut self, key: Key) -> Vec<Command> {
        // Raw mode turns ctrl-c into a key; it quits from anywhere, as it
        // does everywhere else in a terminal.
        if key == Key::Ctrl('c') {
            return vec![Command::Quit];
        }
        let pending_g = std::mem::take(&mut self.pending_g);
        if self.typing {
            self.type_filter(key);
            return Vec::new();
        }
        if self.help {
            match key {
                Key::Esc | Key::Char('?') => self.help = false,
                Key::Char('q') => return vec![Command::Quit],
                _ => {}
            }
            return Vec::new();
        }
        let half = isize::try_from((self.page() / 2).max(1)).unwrap_or(isize::MAX);
        match key {
            Key::Char('q') => return vec![Command::Quit],
            Key::Esc => self.escape(),
            Key::Char('?') => self.help = true,
            Key::Char('g') if pending_g => self.go_to(0),
            Key::Char('g') => self.pending_g = true,
            Key::Char('G') => self.go_to(usize::MAX),
            Key::Char('j') | Key::Down => self.go_by(1),
            Key::Char('k') | Key::Up => self.go_by(-1),
            Key::Ctrl('d') => self.go_by(half),
            Key::Ctrl('u') => self.go_by(-half),
            Key::Char('1') => self.focus_on(Panel::Briefs),
            Key::Char('2') => self.focus_on(Panel::Chores),
            Key::Char('3') => self.focus_on(Panel::Backlog),
            // The preview is to the right of the panels: `h` goes back.
            Key::Char('h') if self.in_preview => self.in_preview = false,
            Key::Char('h') => self.focus_on(self.focus.previous()),
            Key::Char('l') if self.in_preview => {}
            Key::Char('l') | Key::Tab => self.focus_on(self.focus.next()),
            // Two tabs, so either key goes to the other one.
            Key::Char('[' | ']') if self.focus == Panel::Briefs => {
                self.tab = match self.tab {
                    Tab::Open => Tab::Filed,
                    Tab::Filed => Tab::Open,
                };
            }
            Key::Enter if !self.in_preview && self.cursor(self.source()).key.is_some() => {
                self.in_preview = true;
            }
            Key::Char('/') if !self.in_preview => {
                self.typing = true;
                self.filters[self.focus as usize].get_or_insert_with(String::new);
            }
            _ => {}
        }
        Vec::new()
    }

    /// Esc leaves the preview, closes help, or clears a filter, and does
    /// nothing else: the owner hits it by reflex (SPEC.md, TUI). Help and a
    /// filter being typed are handled before this.
    fn escape(&mut self) {
        if self.in_preview {
            self.in_preview = false;
        } else {
            self.filters[self.focus as usize] = None;
        }
    }

    /// A key while the filter is typed. Enter keeps the filter and gives the
    /// keys back; Esc clears it.
    fn type_filter(&mut self, key: Key) {
        let filter = &mut self.filters[self.focus as usize];
        match key {
            Key::Esc => {
                *filter = None;
                self.typing = false;
            }
            Key::Enter => {
                self.typing = false;
                if filter.as_deref() == Some("") {
                    *filter = None;
                }
            }
            Key::Backspace => {
                if let Some(text) = filter {
                    text.pop();
                }
            }
            Key::Char(c) => filter.get_or_insert_with(String::new).push(c),
            Key::Ctrl(_) | Key::Tab | Key::Up | Key::Down => {}
        }
    }

    fn focus_on(&mut self, panel: Panel) {
        self.focus = panel;
        self.in_preview = false;
    }

    /// Moves the cursor, or scrolls the preview, by `by` rows.
    fn go_by(&mut self, by: isize) {
        if self.in_preview {
            self.preview.scroll = self.preview.scroll.saturating_add_signed(by);
            return;
        }
        let source = self.source();
        let items = self.items(source);
        let cursor = &mut self.cursors[source as usize];
        if let Some(last) = items.len().checked_sub(1) {
            cursor.at = cursor.at.saturating_add_signed(by).min(last);
            cursor.key = Some(items[cursor.at].clone());
        }
    }

    /// Puts the cursor on the item at `at` (or the last), or the preview's
    /// scroll there.
    fn go_to(&mut self, at: usize) {
        if self.in_preview {
            self.preview.scroll = at;
            return;
        }
        let source = self.source();
        let items = self.items(source);
        let cursor = &mut self.cursors[source as usize];
        if let Some(last) = items.len().checked_sub(1) {
            cursor.at = at.min(last);
            cursor.key = Some(items[cursor.at].clone());
        }
    }

    /// Starts a fetch when none is out and the last one was asked for a
    /// minute ago. A clock that went back counts as a minute too, or the
    /// fetch would wait until the clock is back where it was.
    fn fetch_if_due(&mut self) -> Vec<Command> {
        let due = self
            .sync
            .asked
            .is_none_or(|asked| asked.unix().abs_diff(self.now.unix()) >= FETCH_EVERY);
        if self.sync.in_flight || !due {
            return Vec::new();
        }
        self.sync.in_flight = true;
        self.sync.asked = Some(self.now);
        vec![Command::Fetch]
    }

    fn fetched(&mut self, fetched: Fetched) {
        self.sync.in_flight = false;
        match fetched {
            // A write holds the lock and fetches itself; the next try is a
            // minute on.
            Fetched::Busy => {}
            Fetched::Ran { fetched, counts } => {
                match fetched {
                    Ok(()) => {
                        self.sync.fetched = Some(self.now);
                        self.sync.error = None;
                    }
                    Err(why) => self.sync.error = Some(why),
                }
                if counts.is_some() {
                    self.sync.counts = counts;
                }
            }
        }
    }

    /// Puts every cursor back on a row that is drawn, after anything that
    /// changed the rows or the screen: on its item when the item is still
    /// shown, else on the item that took its place.
    fn settle(&mut self) {
        let rows = Source::ALL.map(|source| self.rows(source));
        for source in Source::ALL {
            let cursor = &mut self.cursors[source as usize];
            let items: Vec<&ItemKey> = rows[source as usize]
                .iter()
                .filter_map(|row| row.key.as_ref())
                .collect();
            let Some(last) = items.len().checked_sub(1) else {
                *cursor = Cursor::default();
                continue;
            };
            let found = cursor
                .key
                .as_ref()
                .and_then(|key| items.iter().position(|item| *item == key));
            cursor.at = found.unwrap_or(cursor.at.min(last));
            cursor.key = Some(items[cursor.at].clone());
        }
        let areas = self.areas_of(&rows);
        for source in Source::ALL {
            let height = layout::inner_height(areas.panel(source.panel()));
            let rows = &rows[source as usize];
            let cursor = &mut self.cursors[source as usize];
            let Some(row) = cursor_row(cursor, rows) else {
                cursor.offset = 0;
                continue;
            };
            let offset = cursor.offset.min(rows.len().saturating_sub(height));
            cursor.offset = if height == 0 {
                row
            } else if cursor.at == 0 && row < height {
                // The first item shows the heading above it.
                0
            } else if row < offset {
                row
            } else if row >= offset + height {
                row + 1 - height
            } else {
                offset
            };
        }
        let source = self.source();
        let shown = self.cursors[source as usize]
            .key
            .clone()
            .map(|key| (source, key));
        if shown.is_none() {
            self.in_preview = false;
        }
        if shown != self.preview.shown {
            self.preview = Preview { shown, scroll: 0 };
        }
        let max = self
            .preview_content()
            .map_or(0, |preview| preview.max_scroll(areas.preview));
        self.preview.scroll = self.preview.scroll.min(max);
    }

    /// The rows of a list as drawn: the data's, less what the panel's
    /// filter hides. A heading stays while an item under it does.
    pub fn rows(&self, source: Source) -> Vec<Row> {
        let all = all_rows(&self.data, source);
        let Some(filter) = self.filters[source.panel() as usize]
            .as_deref()
            .filter(|filter| !filter.is_empty())
        else {
            return all;
        };
        let filter = filter.to_lowercase();
        let matches = |row: &Row| row.text.to_lowercase().contains(&filter);
        let mut shown = Vec::new();
        let mut heading = None;
        for row in all {
            if row.key.is_none() {
                heading = Some(row);
            } else if matches(&row) {
                shown.extend(heading.take());
                shown.push(row);
            }
        }
        shown
    }

    fn items(&self, source: Source) -> Vec<ItemKey> {
        self.rows(source)
            .into_iter()
            .filter_map(|row| row.key)
            .collect()
    }

    fn areas_of(&self, rows: &[Vec<Row>; 4]) -> Areas {
        let shown = Panel::ALL.map(|panel| rows[self.source_of(panel) as usize].len());
        layout::areas(self.width, self.height, self.focus, shown)
    }

    pub fn areas(&self) -> Areas {
        self.areas_of(&Source::ALL.map(|source| self.rows(source)))
    }

    /// The rows half a page key moves: the focused panel's, or the
    /// preview's.
    fn page(&self) -> usize {
        let areas = self.areas();
        layout::inner_height(if self.in_preview {
            areas.preview
        } else {
            areas.panel(self.focus)
        })
    }

    fn cursor(&self, source: Source) -> &Cursor {
        &self.cursors[source as usize]
    }

    /// The list a panel shows now.
    pub fn source_of(&self, panel: Panel) -> Source {
        match (panel, self.tab) {
            (Panel::Briefs, Tab::Open) => Source::Open,
            (Panel::Briefs, Tab::Filed) => Source::Filed,
            (Panel::Chores, _) => Source::Chores,
            (Panel::Backlog, _) => Source::Backlog,
        }
    }

    /// The list of the focused panel: what the preview shows.
    pub fn source(&self) -> Source {
        self.source_of(self.focus)
    }

    /// The row of the list's cursor among its drawn rows.
    pub fn cursor_row(&self, source: Source) -> Option<usize> {
        cursor_row(self.cursor(source), &self.rows(source))
    }

    pub fn offset(&self, source: Source) -> usize {
        self.cursor(source).offset
    }

    /// What the preview shows: the focused panel's item under its cursor.
    pub fn preview_content(&self) -> Option<layout::Preview> {
        let source = self.source();
        let rows = self.rows(source);
        let row = &rows[cursor_row(self.cursor(source), &rows)?];
        Some(layout::preview(&self.data, source, row))
    }

    pub fn preview_scroll(&self) -> usize {
        self.preview.scroll
    }

    /// The brief in flight: the first started or blocked one.
    pub fn in_flight(&self) -> Option<&BriefItem> {
        self.data
            .open
            .iter()
            .find(|brief| matches!(brief.row.status, Some(Status::Started | Status::Blocked)))
    }

    pub fn data(&self) -> &Data {
        &self.data
    }

    pub fn now(&self) -> Stamp {
        self.now
    }

    pub fn focus(&self) -> Panel {
        self.focus
    }

    pub fn in_preview(&self) -> bool {
        self.in_preview
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn filter(&self, panel: Panel) -> Option<&str> {
        self.filters[panel as usize].as_deref()
    }

    pub fn typing(&self) -> bool {
        self.typing
    }

    pub fn help(&self) -> bool {
        self.help
    }

    pub fn sync(&self) -> &SyncState {
        &self.sync
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }
}

fn cursor_row(cursor: &Cursor, rows: &[Row]) -> Option<usize> {
    let key = cursor.key.as_ref()?;
    rows.iter().position(|row| row.key.as_ref() == Some(key))
}

/// Every row of a list, before a filter.
fn all_rows(data: &Data, source: Source) -> Vec<Row> {
    let mut rows = match source {
        Source::Open => brief_rows(&data.open),
        Source::Filed => brief_rows(&data.filed),
        Source::Chores => list_rows(ListKind::Chores, &data.chores),
        Source::Backlog => list_rows(ListKind::Backlog, &data.backlog),
    };
    // Numbered per ident in list order, so a second row with one id keeps
    // its key while the rows around it change.
    let mut seen: Vec<Ident> = Vec::new();
    for row in &mut rows {
        if let Some(key) = &mut row.key {
            key.nth = seen.iter().filter(|ident| **ident == key.ident).count();
            seen.push(key.ident.clone());
        }
    }
    rows
}

fn brief_rows(briefs: &[BriefItem]) -> Vec<Row> {
    let digits = briefs
        .iter()
        .map(|brief| brief.row.number.to_string().len())
        .max()
        .unwrap_or(0);
    briefs
        .iter()
        .enumerate()
        .map(|(at, brief)| {
            let row = &brief.row;
            let in_flight = matches!(row.status, Some(Status::Started | Status::Blocked));
            Row {
                text: format!(
                    "{:>digits$} {:<9} {}",
                    row.number,
                    row.status_word(),
                    row.slug
                ),
                key: Some(ItemKey {
                    ident: Ident::Brief(row.number, row.slug.clone()),
                    nth: 0,
                }),
                at,
                since: brief.started.filter(|_| in_flight),
            }
        })
        .collect()
}

fn list_rows(kind: ListKind, rows: &[ListRow]) -> Vec<Row> {
    rows.iter()
        .enumerate()
        .map(|(at, row)| match row {
            ListRow::Group(name) => Row {
                text: format!("## {}", String::from_utf8_lossy(name)),
                key: None,
                at,
                since: None,
            },
            ListRow::Item(text) => {
                let id = text
                    .split(|b| b.is_ascii_whitespace())
                    .next()
                    .and_then(|word| kind.parse_id(word));
                let text = String::from_utf8_lossy(text).into_owned();
                Row {
                    key: Some(ItemKey {
                        ident: id.map_or_else(|| Ident::Text(text.clone()), Ident::Id),
                        nth: 0,
                    }),
                    text,
                    at,
                    since: None,
                }
            }
        })
        .collect()
}

/// A child of this module, so that it sees the state's insides.
#[cfg(test)]
#[path = "trace.rs"]
mod trace;
