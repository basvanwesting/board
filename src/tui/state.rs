//! What the TUI shows, as a pure function of what happened: [`State::step`]
//! takes an event (a key, a resize, the clock, a file change, a result) and
//! gives the next state and the commands for the runtime to run. No IO here,
//! so a trace of random events can test all of it (`trace.rs`).
//!
//! The actions fit the same shape: a key gives a [`Command`], and its
//! result comes back as an [`Event`]. A write ([`Action`]) runs the CLI's
//! function on the worker, one at a time, in the order sent; the state
//! keeps what is out, so it shows a move that is out where it goes and
//! knows when the last one is done.

use std::collections::VecDeque;
use std::fmt;

use super::data::{BriefItem, Data, ListEntry};
use super::layout::{self, Areas};
use super::moves;
use crate::brief::{Stamp, Status};
use crate::id::{BacklogId, BriefId, ChoreId, ListKind};
use crate::list::{self, Move, Side};
use crate::project::BoardPath;
use crate::sync::{Counts, Fetched};

/// Seconds between the starts of two background fetches ("about once a
/// minute", SPEC.md, Sync).
pub(crate) const FETCH_EVERY: u64 = 60;

/// The most messages the bottom line keeps: what came since the last key.
const MESSAGES: usize = 8;

/// A key as the state knows it; the runtime maps the terminal's to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Tab,
    /// Shift-Tab.
    BackTab,
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
    /// The answer to [`Command::Run`]: what the command printed, or its
    /// error.
    Ran(Action, Result<String, String>),
    /// What a write says along the way (`waiting for lock`, a lint
    /// finding), or what went wrong outside the state (a yank).
    Note(String),
    /// The answer to [`Command::Edit`]: the editor has given the terminal
    /// back.
    Edited(Edited),
    /// The terminal gives no more keys, so nothing could quit the TUI.
    InputGone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Edited {
    /// The editor ended without an error.
    Closed,
    /// The editor ran and ended with an error: it may have saved.
    Failed(String),
    /// No editor ran.
    NotStarted(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Read the project's files.
    Load,
    /// The background fetch, on the worker thread.
    Fetch,
    /// A write, on the worker thread after what is out there.
    Run(Action),
    /// `$EDITOR` on a file of the board, at a line (`+N`), with the
    /// terminal given to it.
    Edit {
        path: BoardPath,
        line: Option<usize>,
    },
    /// Onto the clipboard: OSC 52, and `wl-copy` or `pbcopy`.
    Yank(String),
    Quit,
}

/// A write of the TUI: each is a CLI command, run by the function the
/// command runs (SPEC.md, TUI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Done(ChoreId),
    Drop(BacklogId),
    Move(Move),
    Sync,
}

/// As the command line says it.
impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Done(id) => write!(f, "done {id}"),
            Action::Drop(id) => write!(f, "drop {id}"),
            Action::Move(wanted) => write!(f, "move {wanted}"),
            Action::Sync => f.write_str("sync"),
        }
    }
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

    /// The list file the rows are of; `None` for briefs.
    pub fn list(self) -> Option<ListKind> {
        match self {
            Source::Open | Source::Filed => None,
            Source::Chores => Some(ListKind::Chores),
            Source::Backlog => Some(ListKind::Backlog),
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
    /// A brief's status, for its colour; `None` for a list row.
    pub status: Option<Status>,
}

impl Row {
    /// The id the row starts with, as the screen spells it (`128`, `c12`):
    /// from the item's key, so a heading or an item without an id has none,
    /// whatever its text looks like.
    pub fn id_text(&self, source: Source) -> Option<String> {
        match &self.key.as_ref()?.ident {
            Ident::Brief(number, _) => Some(number.to_string()),
            Ident::Id(_) => self.id_word(source.list()?),
            Ident::Text(_) => None,
        }
    }

    /// A list item's id as the CLI spells it (`c12`), from the id the
    /// parser read, not off the text: what `y` yanks, and the preview's
    /// title.
    pub fn id_word(&self, kind: ListKind) -> Option<String> {
        let id = self.key.as_ref()?.id()?;
        Some(format!("{}{id}", kind.prefix()))
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

impl ItemKey {
    /// The number of a list item's id; `None` for a brief and for an item
    /// without an id.
    pub fn id(&self) -> Option<u32> {
        match self.ident {
            Ident::Id(number) => Some(number),
            Ident::Brief(..) | Ident::Text(_) => None,
        }
    }
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

/// A run of `J`/`K` presses on an item: its row stands at `place`, and no
/// command is out yet. One `board move` is sent when the run ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Moving {
    pub source: Source,
    pub item: ItemKey,
    /// Next to this item, which may change before the run ends; never
    /// where the item stood when the run began (that run is over).
    pub place: (Side, ItemKey),
}

/// `d` or `x`, asked on the bottom line: `y` runs the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Question {
    /// Done or drop of the item under the cursor when it was asked.
    pub action: Action,
    /// The item's line, so the question says what it is about.
    pub text: String,
}

impl Question {
    /// As the bottom line asks it: `done c12 Rotate the key…? y/n`, the id
    /// once, as the line starts with it.
    pub fn prompt(&self) -> String {
        let verb = match self.action {
            Action::Done(_) => "done",
            Action::Drop(_) => "drop",
            Action::Move(_) | Action::Sync => unreachable!("only d and x ask"),
        };
        format!("{verb} {}? y/n", self.text)
    }
}

/// A line of the bottom line: a result or a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    pub text: String,
    pub error: bool,
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
    question: Option<Question>,
    moving: Option<Moving>,
    /// The writes out on the worker, oldest first: it runs them in this
    /// order and answers each.
    writes: VecDeque<Action>,
    /// Quit was asked while writes were out: the TUI ends when they have.
    quitting: bool,
    /// `e` was pressed while writes were out: the editor opens on this
    /// item once they are done and the files are read again, unless a key
    /// but Esc came first.
    edit_wanted: Option<(Source, ItemKey)>,
    /// Since the last key.
    messages: Vec<Message>,
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
            question: None,
            moving: None,
            writes: VecDeque::new(),
            quitting: false,
            edit_wanted: None,
            messages: Vec::new(),
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
                let before = std::mem::replace(&mut self.data, *data);
                self.load_error = None;
                self.follow_ids(&before);
                self.loaded()
            }
            Event::LoadFailed(why) => {
                self.load_error = Some(why);
                Vec::new()
            }
            Event::Fetched(fetched) => {
                self.fetched(fetched);
                Vec::new()
            }
            Event::Ran(action, result) => self.ran(&action, result),
            Event::Note(note) => {
                self.say(note, false);
                Vec::new()
            }
            Event::Edited(edited) => self.edited(edited),
            Event::InputGone => {
                if self.quitting {
                    Vec::new()
                } else {
                    self.quit()
                }
            }
        };
        self.settle();
        (self, commands)
    }

    fn key(&mut self, key: Key) -> Vec<Command> {
        // Raw mode turns ctrl-c into a key; it quits from anywhere, as it
        // does everywhere else in a terminal.
        if key == Key::Ctrl('c') {
            return self.quit();
        }
        if self.quitting {
            // Waiting for the writes to end; a second quit does not wait.
            return match key {
                Key::Char('q') => self.quit(),
                _ => Vec::new(),
            };
        }
        // Esc does nothing but its one thing, so the message stays.
        if key != Key::Esc {
            self.messages.clear();
            // An editor waited for opens only while the user still waits
            // for it: one that opened later would take the keys typed for
            // the TUI (a `d`, a `J`) as editing commands on the file. Esc
            // keeps the wait, as it does everything else.
            if self.edit_wanted.take().is_some() && key != Key::Char('e') {
                self.say(
                    "the editor is called off: a key came first; e asks again".to_owned(),
                    false,
                );
            }
        }
        let pending_g = std::mem::take(&mut self.pending_g);
        if self.typing {
            self.type_filter(key);
            return Vec::new();
        }
        // Asked once: `y` is yes, any other key no, Esc too.
        if let Some(question) = self.question.take() {
            return match key {
                Key::Char('y') => self.run(question.action),
                _ => Vec::new(),
            };
        }
        if self.help {
            match key {
                Key::Esc | Key::Char('?') => self.help = false,
                Key::Char('q') => return self.quit(),
                _ => {}
            }
            return Vec::new();
        }
        // A run of `J`/`K` goes on; every other key ends it, and sends its
        // move first, so a key's own command comes after. But Esc: it does
        // nothing else (SPEC.md, TUI), so the move waits for the next key.
        let mut commands = match key {
            Key::Char('J' | 'K') | Key::Esc => Vec::new(),
            _ if self.moving.is_some() => {
                let sent = self.end_moving();
                // A dropped run puts the row back: the cursor's place is
                // where its item is now, for the key to move it from.
                self.settle();
                sent
            }
            _ => Vec::new(),
        };
        let half = isize::try_from((self.page() / 2).max(1)).unwrap_or(isize::MAX);
        match key {
            Key::Char('q') => commands.extend(self.quit()),
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
            // Tab backwards, so from the preview too: `h` only leaves it.
            Key::BackTab => self.focus_on(self.focus.previous()),
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
            Key::Char('d') => self.ask(ListKind::Chores),
            Key::Char('x') => self.ask(ListKind::Backlog),
            Key::Char('J') => self.move_item(true),
            Key::Char('K') => self.move_item(false),
            Key::Char('e') => commands.extend(self.edit_key()),
            Key::Char('r') => commands.extend(self.run(Action::Sync)),
            Key::Char('y') => commands.extend(self.yank(false)),
            Key::Char('Y') => commands.extend(self.yank(true)),
            _ => {}
        }
        commands
    }

    /// Esc leaves the preview, closes help, answers no, or clears a filter,
    /// and does nothing else: the owner hits it by reflex (SPEC.md, TUI).
    /// Help, a question and a filter being typed are handled before this.
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
            Key::Ctrl(_) | Key::Tab | Key::BackTab | Key::Up | Key::Down => {}
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

    /// Quits, once the writes that are out have ended: a move or a done the
    /// user saw on screen is then on the board, not cut off with the
    /// process. Asked a second time while it waits, it quits at once: a
    /// push can stall for as long as the connection does, and a write cut
    /// off is one the next write catches up with (SPEC.md, Sync).
    fn quit(&mut self) -> Vec<Command> {
        if self.quitting {
            return vec![Command::Quit];
        }
        let mut commands = self.end_moving();
        self.question = None;
        self.edit_wanted = None;
        if self.writes.is_empty() {
            commands.push(Command::Quit);
        } else {
            self.quitting = true;
        }
        commands
    }

    /// Sends a write to the worker.
    fn run(&mut self, action: Action) -> Vec<Command> {
        self.writes.push_back(action.clone());
        vec![Command::Run(action)]
    }

    fn say(&mut self, text: String, error: bool) {
        if self.messages.len() == MESSAGES {
            self.messages.remove(0);
        }
        self.messages.push(Message { text, error });
    }

    /// The row under the focused panel's cursor.
    fn cursor_item(&self) -> Option<(Source, Row)> {
        let source = self.source();
        let rows = self.rows(source);
        let row = rows.get(cursor_row(self.cursor(source), &rows)?)?;
        Some((source, row.clone()))
    }

    /// `d` (chores) or `x` (backlog): asks whether to, about the item under
    /// the cursor.
    fn ask(&mut self, kind: ListKind) {
        let Some((source, row)) = self.cursor_item() else {
            return;
        };
        if source.list() != Some(kind) {
            let wrong = match kind {
                ListKind::Chores => "d is done, for a chore",
                ListKind::Backlog => "x is drop, for a backlog item",
            };
            self.say(wrong.to_owned(), true);
            return;
        }
        let action = row
            .key
            .as_ref()
            .and_then(ItemKey::id)
            .and_then(|id| match kind {
                ListKind::Chores => ChoreId::new(id).map(Action::Done),
                ListKind::Backlog => BacklogId::new(id).map(Action::Drop),
            });
        match action {
            Some(action) => {
                self.question = Some(Question {
                    action,
                    text: row.text,
                });
            }
            None => self.no_id(&row),
        }
    }

    fn no_id(&mut self, row: &Row) {
        self.say(
            format!(
                "no id yet, so no command can name it ({}); r gives it one",
                row.text
            ),
            true,
        );
    }

    /// `J` (`down`) or `K`: the item under the cursor one place on, at once
    /// on screen; its move is sent when the run of presses ends.
    ///
    /// A run goes on while the cursor is on its item (the trace checks that
    /// it is): every key that moves the cursor ends the run first.
    fn move_item(&mut self, down: bool) {
        let Some((source, row)) = self.cursor_item() else {
            return;
        };
        let Some(kind) = source.list() else {
            self.say(
                "briefs have no order of their own: J/K move chores and backlog items".to_owned(),
                true,
            );
            return;
        };
        if self.filtered(source) {
            // The neighbours a filter hides are where the item would go.
            self.say("clear the filter (Esc) to move an item".to_owned(), true);
            return;
        }
        let item = row.key.clone().expect("the cursor is on an item");
        let Some(id) = item.id() else {
            self.no_id(&row);
            return;
        };
        let base = self.base_rows(source);
        if moves::unique(&base, id).is_none() {
            self.say(
                format!(
                    "{} is on two lines, so no command can name it; lint says which",
                    name_of(kind, &item)
                ),
                true,
            );
            return;
        }
        let Some(home) = moves::group_of(&base, &item) else {
            return;
        };
        let Some(next) = moves::step(&self.arranged(source), &item, down, home) else {
            return;
        };
        let keys = |rows: &[Row]| -> Vec<Option<ItemKey>> {
            rows.iter().map(|row| row.key.clone()).collect()
        };
        self.moving = if keys(&next) == keys(&base) {
            // Back where it was: there is nothing to send.
            None
        } else {
            moves::place_of(&next, &item).map(|place| Moving {
                source,
                item,
                place,
            })
        };
    }

    /// Ends a run of `J`/`K`: its one `board move`, or a note that it is
    /// dropped when no command can say the place.
    fn end_moving(&mut self) -> Vec<Command> {
        let Some(moving) = &self.moving else {
            return Vec::new();
        };
        let rows = self.arranged(moving.source);
        let kind = moving.source.list().expect("a list item moves");
        let item = moving.item.clone();
        self.moving = None;
        match moves::wanted(&rows, &item, kind) {
            Some(wanted) => self.run(Action::Move(wanted)),
            None => {
                self.say(
                    format!(
                        "dropped the move of {}: neither item next to it has an id to name \
                         the place by; r gives them one",
                        name_of(kind, &item)
                    ),
                    true,
                );
                Vec::new()
            }
        }
    }

    /// `e`: the editor on the item under the cursor, once the writes that
    /// are out have ended.
    ///
    /// Decided here: it waits for them, because an editor opened beside a
    /// write reads the file before the write changes it, and saving it
    /// then would put back what the write took out (a done chore, a move).
    fn edit_key(&mut self) -> Vec<Command> {
        let Some((source, row)) = self.cursor_item() else {
            self.say("nothing to edit here".to_owned(), true);
            return Vec::new();
        };
        let key = row.key.expect("the cursor is on an item");
        if self.writes.is_empty() {
            return self.edit(source, &key);
        }
        self.edit_wanted = Some((source, key));
        self.say(
            "the editor opens once the writes are done; a key but Esc calls it off".to_owned(),
            false,
        );
        Vec::new()
    }

    /// The editor on the brief's file, or on the list file at the item's
    /// line.
    fn edit(&mut self, source: Source, key: &ItemKey) -> Vec<Command> {
        let row = self
            .rows(source)
            .into_iter()
            .find(|row| row.key.as_ref() == Some(key));
        let Some(row) = row else {
            self.say("the item is gone: nothing to edit".to_owned(), true);
            return Vec::new();
        };
        let (path, line) = match source.list() {
            None => (self.brief(source, &row).path.clone(), None),
            Some(kind) => (
                BoardPath::List {
                    project: self.data.project.clone(),
                    kind,
                },
                Some(self.data.list(kind)[row.at].line),
            ),
        };
        vec![Command::Edit { path, line }]
    }

    fn brief(&self, source: Source, row: &Row) -> &BriefItem {
        match source {
            Source::Open => &self.data.open[row.at],
            Source::Filed => &self.data.filed[row.at],
            Source::Chores | Source::Backlog => unreachable!("a brief's row"),
        }
    }

    /// The editor gave the terminal back: whatever it saved is committed
    /// by a sync (SPEC.md, TUI), unless none ran.
    fn edited(&mut self, edited: Edited) -> Vec<Command> {
        match edited {
            Edited::Closed => self.run(Action::Sync),
            Edited::Failed(why) => {
                self.say(why, true);
                self.run(Action::Sync)
            }
            Edited::NotStarted(why) => {
                self.say(why, true);
                Vec::new()
            }
        }
    }

    /// `y`: the item's id; `Y` (`line`): its line as `board ls` prints it.
    fn yank(&mut self, line: bool) -> Vec<Command> {
        let Some((source, row)) = self.cursor_item() else {
            return Vec::new();
        };
        let text = match (source.list(), line) {
            (None, false) => self.brief(source, &row).row.number.to_string(),
            (None, true) => self.brief(source, &row).row.ls_line(),
            (Some(_), true) => row.text.clone(),
            (Some(kind), false) => match row.id_word(kind) {
                Some(id) => id,
                None => {
                    self.no_id(&row);
                    return Vec::new();
                }
            },
        };
        self.say(format!("yanked {text}"), false);
        vec![Command::Yank(text)]
    }

    /// A write's answer. The worker answers in the order sent, so this is
    /// the oldest write out of its kind; looked up all the same, so that a
    /// move is shown where it goes for exactly as long as it is out.
    fn ran(&mut self, action: &Action, result: Result<String, String>) -> Vec<Command> {
        if let Some(at) = self.writes.iter().position(|out| out == action) {
            self.writes.remove(at);
        }
        match result {
            Ok(said) => self.say(said, false),
            Err(why) => self.say(why, true),
        }
        // The write fetched and may have pushed: the counts on the status
        // line are stale, so the next tick fetches.
        self.sync.asked = None;
        // Read now, not at the watcher's event, which may come after: the
        // rows of a move that is no longer out would stand where they were
        // until then.
        let mut commands = vec![Command::Load];
        if self.quitting && self.writes.is_empty() {
            commands.push(Command::Quit);
        }
        commands
    }

    /// After a reload, before anything looks for what it holds: the items
    /// a write gave an id keep their cursor, their preview, their place as
    /// a run's neighbour and the edit waited for on them ([`follow`]).
    fn follow_ids(&mut self, before: &Data) {
        for source in [Source::Chores, Source::Backlog] {
            let kind = source.list().expect("a list");
            let rows = all_rows(&self.data, source);
            let before = all_rows(before, source);
            let keep = |key: &mut ItemKey| {
                if let Some(found) = follow(kind, &rows, &before, key) {
                    *key = found;
                }
            };
            if let Some(key) = &mut self.cursors[source as usize].key {
                keep(key);
            }
            if let Some((shown, key)) = &mut self.preview.shown
                && *shown == source
            {
                keep(key);
            }
            if let Some(moving) = &mut self.moving
                && moving.source == source
            {
                keep(&mut moving.place.1);
            }
            if let Some((wanted, key)) = &mut self.edit_wanted
                && *wanted == source
            {
                keep(key);
            }
        }
    }

    /// After a reload: a run of `J`/`K` whose item or neighbour is gone is
    /// dropped, as is one whose item's id is on two lines now (no command
    /// could name it), and the editor waited for opens.
    fn loaded(&mut self) -> Vec<Command> {
        if let Some(moving) = &self.moving {
            let kind = moving.source.list().expect("a list item moves");
            let rows = self.base_rows(moving.source);
            let has = |key: &ItemKey| rows.iter().any(|row| row.key.as_ref() == Some(key));
            let why = match [&moving.item, &moving.place.1]
                .into_iter()
                .find(|key| !has(key))
            {
                Some(gone) => Some(format!("{} is gone", name_of(kind, gone))),
                None => moving
                    .item
                    .id()
                    .filter(|id| moves::unique(&rows, *id).is_none())
                    .map(|_| "it is on two lines now".to_owned()),
            };
            if let Some(why) = why {
                let item = name_of(kind, &moving.item);
                self.moving = None;
                self.say(format!("dropped the move of {item}: {why}"), true);
            }
        }
        if self.writes.is_empty()
            && let Some((source, key)) = self.edit_wanted.take()
        {
            return self.edit(source, &key);
        }
        Vec::new()
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

    /// The rows of a list as they stand while the moves that are out run:
    /// where the files will have them.
    fn base_rows(&self, source: Source) -> Vec<Row> {
        let mut rows = all_rows(&self.data, source);
        if let Some(kind) = source.list() {
            for action in &self.writes {
                if let Action::Move(wanted) = action
                    && wanted.kind() == kind
                {
                    moves::put_sent(&mut rows, wanted);
                }
            }
        }
        rows
    }

    /// The rows of a list as they stand on screen: the run of `J`/`K`
    /// presses too.
    fn arranged(&self, source: Source) -> Vec<Row> {
        let mut rows = self.base_rows(source);
        if let Some(moving) = &self.moving
            && moving.source == source
        {
            let (side, next_to) = &moving.place;
            moves::put(&mut rows, &moving.item, *side, next_to);
        }
        rows
    }

    /// Whether the panel's filter hides rows: one is set and not empty.
    fn filtered(&self, source: Source) -> bool {
        self.filters[source.panel() as usize]
            .as_deref()
            .is_some_and(|filter| !filter.is_empty())
    }

    /// The rows of a list as drawn: arranged, less what the panel's filter
    /// hides. A heading stays while an item under it does.
    pub fn rows(&self, source: Source) -> Vec<Row> {
        let all = self.arranged(source);
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

    /// Whether the row stands where a move puts it before a file has it:
    /// moved by `J`/`K`, or its move is out.
    pub fn pending(&self, source: Source, row: &Row) -> bool {
        let Some(key) = &row.key else {
            return false;
        };
        let moving = self
            .moving
            .as_ref()
            .is_some_and(|moving| moving.source == source && moving.item == *key);
        let sent = source.list().is_some_and(|kind| {
            self.writes.iter().any(|action| {
                matches!(action, Action::Move(wanted)
                    if wanted.kind() == kind && key.id() == Some(wanted.item().get()))
            })
        });
        moving || sent
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

    pub fn question(&self) -> Option<&Question> {
        self.question.as_ref()
    }

    pub fn moving(&self) -> Option<&Moving> {
        self.moving.as_ref()
    }

    pub fn writes(&self) -> &VecDeque<Action> {
        &self.writes
    }

    pub fn quitting(&self) -> bool {
        self.quitting
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// How a note names a list item: its id, else its text.
fn name_of(kind: ListKind, key: &ItemKey) -> String {
    match &key.ident {
        Ident::Id(number) => format!("{}{number}", kind.prefix()),
        Ident::Text(text) => text.clone(),
        Ident::Brief(number, _) => number.to_string(),
    }
}

/// Where `key`'s item is among `rows`, a list's rows after a reload, in
/// file order: its own row, or, for an item typed by hand that a write has
/// given an id since, the row with that id. `None` when it is gone.
///
/// The key of such an item is its text, and the id changes the text: the
/// sync that commits a hand-typed line (the one after `e`, say) would
/// otherwise make it look gone to whatever holds on to it, though it is
/// still there. A given id is spelled as [`list::with_id`] spells it, the
/// one spelling the merge relies on too, and is one that `before` (the
/// rows before the reload) did not have: `b1 X` that was there all along
/// is not the `X` typed by hand. The `nth` line of one text, with a given
/// id or without, is the `nth` it was: a sync gives every item without an
/// id one at once, in file order.
fn follow(kind: ListKind, rows: &[Row], before: &[Row], key: &ItemKey) -> Option<ItemKey> {
    if rows.iter().any(|row| row.key.as_ref() == Some(key)) {
        return Some(key.clone());
    }
    let Ident::Text(text) = &key.ident else {
        return None;
    };
    let given = |id: u32| {
        !before
            .iter()
            .any(|row| row.key.as_ref().and_then(ItemKey::id) == Some(id))
    };
    let line = format!("- {text}");
    rows.iter()
        .filter_map(|row| {
            let found = row.key.as_ref()?;
            let spells = match found.ident {
                Ident::Text(ref other) => other == text,
                Ident::Id(id) => {
                    given(id)
                        && list::with_id(kind, line.as_bytes(), id)
                            == format!("- {}", row.text).as_bytes()
                }
                Ident::Brief(..) => false,
            };
            spells.then_some(found)
        })
        .nth(key.nth)
        // A line still without an id is another item: this one would have
        // its own key still.
        .filter(|found| found.id().is_some())
        .cloned()
}

fn cursor_row(cursor: &Cursor, rows: &[Row]) -> Option<usize> {
    let key = cursor.key.as_ref()?;
    rows.iter().position(|row| row.key.as_ref() == Some(key))
}

/// Every row of a list as the data has it, before a move or a filter.
fn all_rows(data: &Data, source: Source) -> Vec<Row> {
    let mut rows = match source {
        Source::Open => brief_rows(&data.open),
        Source::Filed => brief_rows(&data.filed),
        Source::Chores => list_rows(ListKind::Chores, data),
        Source::Backlog => list_rows(ListKind::Backlog, data),
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
                status: row.status,
            }
        })
        .collect()
}

fn list_rows(kind: ListKind, data: &Data) -> Vec<Row> {
    data.list(kind)
        .iter()
        .enumerate()
        .map(|(at, line)| match &line.entry {
            ListEntry::Group(name) => Row {
                text: format!("## {}", String::from_utf8_lossy(name)),
                key: None,
                at,
                since: None,
                status: None,
            },
            ListEntry::Item { id, text } => {
                let text = String::from_utf8_lossy(text).into_owned();
                Row {
                    key: Some(ItemKey {
                        ident: id.map_or_else(|| Ident::Text(text.clone()), Ident::Id),
                        nth: 0,
                    }),
                    text,
                    at,
                    since: None,
                    status: None,
                }
            }
        })
        .collect()
}

/// A child of this module, so that it sees the state's insides.
#[cfg(test)]
#[path = "trace.rs"]
mod trace;
