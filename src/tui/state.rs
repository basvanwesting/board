//! What the TUI shows, as a pure function of what happened: [`State::step`]
//! takes an event (a key, a resize, the clock, a file change, a result) and
//! gives the next state and the commands for the runtime to run. No IO here,
//! so a trace of random events can test all of it (`trace.rs`).
//!
//! The actions fit the same shape: a key gives a [`Command`], and its
//! result comes back as an [`Event`]. A write ([`Action`]) runs the CLI's
//! function on the worker, one at a time, in the order sent; the state
//! keeps what is out, so it knows when the last one is done.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;

use super::data::{BriefItem, Data, ListEntry};
use super::layout::{self, Areas};
use crate::board::{Shown, waits_suffix};
use crate::brief::{Stamp, Status};
use crate::id::{BacklogId, BriefId, ChoreId, ListId, ListKind};
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

/// What the mouse did, as the state knows it; the runtime maps the
/// terminal's to these and drops the rest (a release, a drag, a move,
/// another button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mouse {
    /// The left button pressed at this cell of the screen.
    Click {
        column: u16,
        row: u16,
    },
    WheelUp,
    WheelDown,
}

/// What a click is on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hit {
    /// A list panel, and the item on the row clicked, if any.
    Panel(Panel, Option<ItemKey>),
    Preview,
    /// The status line, or off the screen.
    Nothing,
}

#[derive(Debug, Clone)]
pub(crate) enum Event {
    Key(Key),
    Mouse(Mouse),
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
    /// The answer to [`Command::Unpushed`]: the briefs with unpushed
    /// commits (`board::Pushes::unpushed`), or why git could not tell.
    Unpushed(Result<BTreeSet<BriefId>, String>),
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
    /// Which briefs have commits above the project repository's upstream:
    /// git, on a thread of its own, off the drawing thread and apart from
    /// the fetch. Asked at each load and about once a minute, so a push the
    /// owner makes shows within the minute, or at once on `r`, without a
    /// board file changing; one at a time.
    Unpushed,
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
    /// `queue <id> --first`: `F` on a queued brief.
    First(BriefId),
    /// `queue <id>`: `Q` on a parked brief, confirmed.
    Queue(BriefId),
    Sync,
}

/// As the command line says it.
impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Done(id) => write!(f, "done {id}"),
            Action::Drop(id) => write!(f, "drop {id}"),
            Action::First(id) => write!(f, "queue {id} --first"),
            Action::Queue(id) => write!(f, "queue {id}"),
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
    Parked,
    Filed,
}

/// A list of rows with a cursor of its own: each tab of Briefs, Chores,
/// Backlog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Open = 0,
    Parked = 1,
    Filed = 2,
    Chores = 3,
    Backlog = 4,
}

impl Source {
    pub const ALL: [Source; 5] = [
        Source::Open,
        Source::Parked,
        Source::Filed,
        Source::Chores,
        Source::Backlog,
    ];

    pub fn panel(self) -> Panel {
        match self {
            Source::Open | Source::Parked | Source::Filed => Panel::Briefs,
            Source::Chores => Panel::Chores,
            Source::Backlog => Panel::Backlog,
        }
    }

    /// The list file the rows are of; `None` for briefs.
    pub fn list(self) -> Option<ListKind> {
        match self {
            Source::Open | Source::Parked | Source::Filed => None,
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
    /// What a brief is shown as (its status, or `unpushed`), for its
    /// colour; `None` for a list row.
    pub status: Option<Shown>,
    /// Where in `text` the `  after <id>` of an item that waits starts, drawn
    /// dim; `text.len()` when it waits on nothing. Part of the text, so the
    /// filter and `Y` see the line as `ls` prints it.
    pub dim_from: usize,
}

impl Row {
    /// The id the row starts with, as the screen spells it (`128`, `c12`):
    /// from the item's key, so a heading or an item without an id has none,
    /// whatever its text looks like.
    pub fn id_text(&self, source: Source) -> Option<String> {
        match &self.key.as_ref()?.ident {
            Ident::Brief(number, _) => Some(number.to_string()),
            Ident::Id(_) => self.id_word(source.list()?),
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
/// A chore's or backlog item's id, a brief's number and slug. `nth` tells
/// apart rows with one ident (a brief number on two files, which lint
/// reports), so the cursor can still reach the second.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ItemKey {
    ident: Ident,
    nth: usize,
}

impl ItemKey {
    /// The number of a list item's id; `None` for a brief.
    pub fn id(&self) -> Option<u32> {
        match self.ident {
            Ident::Id(number) => Some(number),
            Ident::Brief(..) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Ident {
    /// Number and slug: the file name.
    Brief(BriefId, String),
    Id(u32),
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
            Action::Queue(_) => "queue",
            Action::First(_) | Action::Sync => unreachable!("only d, x and Q ask"),
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
    /// Labelled by `unpushed`.
    data: Data,
    /// The last answer to [`Command::Unpushed`].
    unpushed: BTreeSet<BriefId>,
    /// When [`Command::Unpushed`] was last asked, and whether its answer is
    /// still out.
    unpushed_asked: Option<Stamp>,
    unpushed_out: bool,
    width: u16,
    height: u16,
    now: Stamp,
    focus: Panel,
    /// The preview has the keys, for scrolling; `focus` is the panel it
    /// shows.
    in_preview: bool,
    tab: Tab,
    cursors: [Cursor; 5],
    /// Per panel; the three tabs of Briefs share one.
    filters: [Option<String>; 3],
    /// Keys go into the focused panel's filter.
    typing: bool,
    /// `g` was pressed: a second one goes to the top.
    pending_g: bool,
    /// A `y` was the last key, on this item: a `y` now yanks the whole of
    /// it (`yy`). The item, not a flag: a reload between the two keys may
    /// move the cursor onto another one, whose id was never yanked.
    pending_y: Option<ItemKey>,
    help: bool,
    preview: Preview,
    sync: SyncState,
    load_error: Option<String>,
    question: Option<Question>,
    /// The writes out on the worker, oldest first: it runs them in this
    /// order and answers each.
    writes: VecDeque<Action>,
    /// Quit was asked while writes were out: the TUI ends when they have.
    quitting: bool,
    /// `e` was pressed while writes were out: the editor opens on this
    /// item once they are done and the files are read again, unless a key
    /// but Esc came first. A brief's file path with it: the editor opens
    /// on that file only.
    edit_wanted: Option<(Source, ItemKey, Option<BoardPath>)>,
    /// Since the last key.
    messages: Vec<Message>,
}

impl State {
    /// The state before anything is read, and the commands that read it.
    pub fn new(data: Data, width: u16, height: u16, now: Stamp) -> (State, Vec<Command>) {
        let mut state = State {
            data,
            unpushed: BTreeSet::new(),
            unpushed_asked: None,
            unpushed_out: false,
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
            pending_y: None,
            help: false,
            preview: Preview::default(),
            sync: SyncState::default(),
            load_error: None,
            question: None,
            writes: VecDeque::new(),
            quitting: false,
            edit_wanted: None,
            messages: Vec::new(),
        };
        let mut commands = vec![Command::Load];
        commands.extend(state.ask_unpushed());
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
            Event::Mouse(mouse) => self.mouse(mouse),
            Event::Resize { width, height } => {
                self.width = width;
                self.height = height;
                Vec::new()
            }
            Event::Tick(now) => {
                self.now = now;
                let mut commands = self.fetch_if_due();
                let due = self
                    .unpushed_asked
                    .is_none_or(|asked| asked.unix().abs_diff(now.unix()) >= FETCH_EVERY);
                if due {
                    commands.extend(self.ask_unpushed());
                }
                commands
            }
            Event::FilesChanged => {
                let mut commands = vec![Command::Load];
                commands.extend(self.ask_unpushed());
                commands
            }
            Event::Loaded(data) => {
                let mut data = *data;
                data.label(&self.unpushed);
                self.load_error = None;
                self.relabel(data);
                self.loaded()
            }
            Event::Unpushed(answer) => {
                self.unpushed_out = false;
                match answer {
                    Ok(unpushed) => self.unpushed_now(unpushed),
                    // The set stays as it was; the next ask tries again.
                    Err(why) => self.say(why, true),
                }
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
            self.call_off_edit(key == Key::Char('e'));
        }
        let pending_g = std::mem::take(&mut self.pending_g);
        let pending_y = std::mem::take(&mut self.pending_y);
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
        let mut commands = Vec::new();
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
            // Three tabs, Briefs, Parked, Filed: `[` and `]` step through
            // them and stop at the ends.
            Key::Char('[') if self.focus == Panel::Briefs => {
                self.tab = match self.tab {
                    Tab::Open | Tab::Parked => Tab::Open,
                    Tab::Filed => Tab::Parked,
                };
            }
            Key::Char(']') if self.focus == Panel::Briefs => {
                self.tab = match self.tab {
                    Tab::Open => Tab::Parked,
                    Tab::Parked | Tab::Filed => Tab::Filed,
                };
            }
            // `0` is the preview's number in its title, as `1` `2` `3` are the
            // panels': the number names a key that is there (SPEC.md, TUI).
            Key::Char('0') | Key::Enter
                if !self.in_preview && self.cursor(self.source()).key.is_some() =>
            {
                self.in_preview = true;
            }
            Key::Char('/') if !self.in_preview => {
                self.typing = true;
                self.filters[self.focus as usize].get_or_insert_with(String::new);
            }
            Key::Char('d') => self.ask(ListKind::Chores),
            Key::Char('x') => self.ask(ListKind::Backlog),
            Key::Char('F') => commands.extend(self.first()),
            Key::Char('Q') => self.ask_release(),
            Key::Char('e') => commands.extend(self.edit_key()),
            Key::Char('r') => commands.extend(self.run(Action::Sync)),
            // The id at once, as `gg` is read: no timer for a key that can
            // answer now; a `y` as the very next key then takes the whole
            // item instead, and a third starts over.
            Key::Char('y') if pending_y.is_some() && pending_y == self.cursor_key() => {
                commands.extend(self.yank_whole());
            }
            Key::Char('y') => {
                commands.extend(self.yank(false));
                self.pending_y = self.cursor_key();
            }
            Key::Char('Y') => commands.extend(self.yank(true)),
            _ => {}
        }
        commands
    }

    /// An editor waited for opens only while the user still waits for it:
    /// one that opened later would take the keys typed for the TUI (a `d`,
    /// a `J`) as editing commands on the file. Called off by any key but
    /// Esc, which keeps the wait as it keeps everything else, and but `e`
    /// (`again`), which asks anew.
    fn call_off_edit(&mut self, again: bool) {
        if self.edit_wanted.take().is_some() && !again {
            self.say(
                "the editor is called off: a key or click came first; e asks again".to_owned(),
                false,
            );
        }
    }

    /// A click or the wheel (owner, 2026-11-25: mouse capture, as LazyGit
    /// has it). A left click focuses the pane it hits and selects the item
    /// it hits ([`State::hit`]); the preview takes the focus only when `0`
    /// would, with an item under the cursor. One rule: a mouse event is first an unrelated key, then
    /// itself, so no pending state gets a second way out. Where a key is
    /// taken by what waits for it (the filter being typed, a question, help,
    /// a quit waiting for the writes) it does nothing at all: a question is
    /// not answered no by a click beside it. Elsewhere it clears what an
    /// unrelated key clears (the messages, a `g` or `y` waiting for its
    /// second key, an edit waited for).
    fn mouse(&mut self, mouse: Mouse) -> Vec<Command> {
        if self.quitting || self.typing || self.question.is_some() || self.help {
            return Vec::new();
        }
        // Where the click is, on the screen as drawn.
        let hit = match mouse {
            Mouse::Click { column, row } => Some(self.hit(column, row)),
            Mouse::WheelUp | Mouse::WheelDown => None,
        };
        self.messages.clear();
        self.call_off_edit(false);
        self.pending_g = false;
        self.pending_y = None;
        match (mouse, hit) {
            // What `k` and `j` do: capture ends the terminal's own wheel,
            // which some sent as Up and Down.
            (Mouse::WheelUp, _) => self.go_by(-1),
            (Mouse::WheelDown, _) => self.go_by(1),
            (Mouse::Click { .. }, Some(Hit::Panel(panel, item))) => {
                self.focus_on(panel);
                let source = self.source_of(panel);
                let items = self.items(source);
                if let Some((at, key)) = item.and_then(|key| {
                    let at = items.iter().position(|shown| *shown == key)?;
                    Some((at, key))
                }) {
                    let cursor = &mut self.cursors[source as usize];
                    cursor.at = at;
                    cursor.key = Some(key);
                }
            }
            // As `0`: only with an item under the cursor.
            (Mouse::Click { .. }, Some(Hit::Preview)) => {
                if self.cursor(self.source()).key.is_some() {
                    self.in_preview = true;
                }
            }
            (Mouse::Click { .. }, Some(Hit::Nothing) | None) => {}
        }
        Vec::new()
    }

    /// What a left click hits, as the screen draws it: the pane under it
    /// and, for a list panel, the item on the row under it. A border or a
    /// title is the pane's; a heading, a row below the last and the status
    /// line hit no item. By [`State::areas`], the layout the view draws, so
    /// the two cannot disagree.
    fn hit(&self, column: u16, row: u16) -> Hit {
        let at = ratatui::layout::Position::new(column, row);
        let areas = self.areas();
        if let Some(panel) = Panel::ALL
            .into_iter()
            .find(|panel| areas.panel(*panel).contains(at))
        {
            let area = areas.panel(panel);
            let inside = ratatui::layout::Rect::new(
                area.x.saturating_add(1),
                area.y.saturating_add(1),
                area.width.saturating_sub(2),
                area.height.saturating_sub(2),
            );
            let source = self.source_of(panel);
            let item = inside
                .contains(at)
                .then(|| self.offset(source) + usize::from(row - inside.y))
                .and_then(|index| self.rows(source).into_iter().nth(index))
                .and_then(|row| row.key);
            Hit::Panel(panel, item)
        } else if areas.preview.contains(at) {
            Hit::Preview
        } else {
            Hit::Nothing
        }
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

    /// Quits, once the writes that are out have ended: a done or a drop the
    /// user saw on screen is then on the board, not cut off with the
    /// process. Asked a second time while it waits, it quits at once: a
    /// push can stall for as long as the connection does, and a write cut
    /// off is one the next write catches up with (SPEC.md, Sync).
    fn quit(&mut self) -> Vec<Command> {
        if self.quitting {
            return vec![Command::Quit];
        }
        let mut commands = Vec::new();
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

    /// `Q`: asks, on the bottom line as `d` and `x` do, whether to queue the
    /// parked brief under the cursor (owner, 2026-11-26: "Q is fine").
    /// Asked, because nothing takes a brief out of the queue again and `Q`
    /// is one Shift away from `q`. On any other row it does nothing.
    fn ask_release(&mut self) {
        let Some((source, row)) = self.cursor_item() else {
            return;
        };
        if source != Source::Parked {
            return;
        }
        let brief = &self.brief(source, &row).row;
        if brief.status != Some(Status::Parked) {
            return;
        }
        self.question = Some(Question {
            action: Action::Queue(brief.number),
            text: brief.ls_line(),
        });
    }

    /// `F`: the queued brief under the cursor first in the queue, with no
    /// question: a second `F` on another brief undoes it. One of a brief's
    /// two actions, with `Q` (owner, 2026-11-26: "it needs a single key in
    /// the TUI on a brief as well"); queueing a draft stays the planner's, so on any
    /// other brief it says why it does nothing, and on a list item it does
    /// nothing.
    fn first(&mut self) -> Vec<Command> {
        let Some((source, row)) = self.cursor_item() else {
            return Vec::new();
        };
        if source.list().is_some() {
            return Vec::new();
        }
        let brief = &self.brief(source, &row).row;
        if brief.status == Some(Status::Queued) && !brief.unpushed {
            return self.run(Action::First(brief.number));
        }
        let said = format!(
            "F puts a queued brief first; {} is {}",
            brief.number,
            brief.status_word()
        );
        self.say(said, true);
        Vec::new()
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

    /// `e`: the editor on the item under the cursor, once the writes that
    /// are out have ended.
    ///
    /// Decided here: it waits for them, because an editor opened beside a
    /// write reads the file before the write changes it, and saving it
    /// then would put back what the write took out (a done chore).
    fn edit_key(&mut self) -> Vec<Command> {
        let Some((source, row)) = self.cursor_item() else {
            self.say("nothing to edit here".to_owned(), true);
            return Vec::new();
        };
        let path = source
            .list()
            .is_none()
            .then(|| self.brief(source, &row).path.clone());
        let key = row.key.expect("the cursor is on an item");
        if self.writes.is_empty() {
            return self.edit(source, &key, path.as_ref());
        }
        self.edit_wanted = Some((source, key, path));
        self.say(
            "the editor opens once the writes are done; a key but Esc calls it off".to_owned(),
            false,
        );
        Vec::new()
    }

    /// The editor on the brief's file (`path`, the one `e` was pressed
    /// on), or on the item's file.
    fn edit(&mut self, source: Source, key: &ItemKey, path: Option<&BoardPath>) -> Vec<Command> {
        // A brief may have changed tabs meanwhile: pushed, an unpushed one
        // goes to Filed. It is the same file, so a brief's row is followed
        // by its file alone: a brief on two files of one name, one open and
        // one in `briefs/done/` (lint reports it), has a key in each tab,
        // renumbered (`nth`) as one moves, and the other file is not the
        // one `e` was pressed on.
        let sources: &[Source] = match source {
            Source::Open | Source::Parked | Source::Filed => {
                &[Source::Open, Source::Parked, Source::Filed]
            }
            Source::Chores => &[Source::Chores],
            Source::Backlog => &[Source::Backlog],
        };
        let found = sources.iter().copied().find_map(|source| {
            self.rows(source)
                .into_iter()
                .find(|row| match (source.list(), path) {
                    (None, Some(path)) => {
                        row.key.is_some() && self.brief(source, row).path == *path
                    }
                    _ => row.key.as_ref() == Some(key),
                })
                .map(|row| (source, row))
        });
        let Some((source, row)) = found else {
            self.say("the item is gone: nothing to edit".to_owned(), true);
            return Vec::new();
        };
        let path = match source.list() {
            None => self.brief(source, &row).path.clone(),
            Some(kind) => {
                let ListEntry::Item { id, .. } = self.data.list(kind)[row.at].entry else {
                    unreachable!("an item's row");
                };
                BoardPath::Item {
                    project: self.data.project.clone(),
                    id: match kind {
                        ListKind::Chores => {
                            ListId::Chore(ChoreId::new(id).expect("an item's id is past 0"))
                        }
                        ListKind::Backlog => {
                            ListId::Backlog(BacklogId::new(id).expect("an item's id is past 0"))
                        }
                    },
                }
            }
        };
        vec![Command::Edit { path, line: None }]
    }

    fn brief(&self, source: Source, row: &Row) -> &BriefItem {
        match source {
            Source::Open => &self.data.open[row.at],
            Source::Parked => &self.data.parked[row.at],
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

    /// The item under the cursor of the focused panel.
    fn cursor_key(&self) -> Option<ItemKey> {
        self.cursor_item().and_then(|(_, row)| row.key)
    }

    /// `yy`: the whole item, as `board show` prints it, from the same code: a
    /// brief's or a chore's or backlog item's file, as stored. The bottom
    /// line says what was taken, never the text.
    fn yank_whole(&mut self) -> Vec<Command> {
        let Some((source, row)) = self.cursor_item() else {
            return Vec::new();
        };
        let (text, what) = match source.list() {
            None => {
                let brief = self.brief(source, &row);
                (brief.text.clone(), brief.row.number.to_string())
            }
            Some(kind) => {
                let ListEntry::Item { file, .. } = &self.data.list(kind)[row.at].entry else {
                    return Vec::new();
                };
                let name = row.id_word(kind).unwrap_or_else(|| "the item".to_owned());
                (file.clone(), name)
            }
        };
        let text = String::from_utf8_lossy(&text).into_owned();
        let lines = text.lines().count();
        let plural = if lines == 1 { "" } else { "s" };
        self.say(format!("yanked {what}, {lines} line{plural}"), false);
        vec![Command::Yank(text)]
    }

    /// A write's answer. The worker answers in the order sent, so this is
    /// the oldest write out of its kind; looked up all the same.
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
        // Read now, not at the watcher's event, which may come after: what
        // the write changed shows at once.
        let mut commands = vec![Command::Load];
        commands.extend(self.ask_unpushed());
        if self.quitting && self.writes.is_empty() {
            commands.push(Command::Quit);
        }
        commands
    }

    /// Asks which briefs are unpushed, unless an answer is still out: it
    /// answers for now soon enough.
    fn ask_unpushed(&mut self) -> Vec<Command> {
        if self.unpushed_out {
            return Vec::new();
        }
        self.unpushed_out = true;
        self.unpushed_asked = Some(self.now);
        vec![Command::Unpushed]
    }

    /// A new set of unpushed briefs. Not a load: the files are as they
    /// were, so nothing that waits for the load after a write (an edit)
    /// goes on.
    fn unpushed_now(&mut self, unpushed: BTreeSet<BriefId>) {
        if unpushed == self.unpushed {
            return;
        }
        self.unpushed = unpushed;
        let mut data = self.data.clone();
        data.label(&self.unpushed);
        self.relabel(data);
    }

    /// New data, from the files or from a new set of unpushed briefs: the
    /// cursors stay on their items, wherever the rows went.
    fn relabel(&mut self, data: Data) {
        self.data = data;
    }

    /// After a reload: the editor waited for opens.
    fn loaded(&mut self) -> Vec<Command> {
        if self.writes.is_empty()
            && let Some((source, key, path)) = self.edit_wanted.take()
        {
            return self.edit(source, &key, path.as_ref());
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

    /// How many items a tab lists, headings left out: as drawn, and of
    /// all when the panel's filter hides some; `None` for the second when
    /// no filter is set (an empty filter text is none).
    pub fn count(&self, source: Source) -> (usize, Option<usize>) {
        let items = |rows: Vec<Row>| rows.iter().filter(|row| row.key.is_some()).count();
        let shown = items(self.rows(source));
        let filtered = self.filters[source.panel() as usize]
            .as_deref()
            .is_some_and(|filter| !filter.is_empty());
        (shown, filtered.then(|| items(all_rows(&self.data, source))))
    }

    /// The rows of a list as drawn: all of them, less what the panel's
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

    fn areas_of(&self, rows: &[Vec<Row>; 5]) -> Areas {
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
            (Panel::Briefs, Tab::Parked) => Source::Parked,
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

    pub fn question(&self) -> Option<&Question> {
        self.question.as_ref()
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

fn cursor_row(cursor: &Cursor, rows: &[Row]) -> Option<usize> {
    let key = cursor.key.as_ref()?;
    rows.iter().position(|row| row.key.as_ref() == Some(key))
}

/// Every row of a list as the data has it, before a filter.
fn all_rows(data: &Data, source: Source) -> Vec<Row> {
    let mut rows = match source {
        Source::Open => brief_rows(&data.open),
        Source::Parked => brief_rows(&data.parked),
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
            let text = format!(
                "{:>digits$} {:<9} {}",
                row.number,
                row.status_word(),
                row.slug
            );
            let dim_from = text.len();
            Row {
                text: text + &waits_suffix(row.waits),
                key: Some(ItemKey {
                    ident: Ident::Brief(row.number, row.slug.clone()),
                    nth: 0,
                }),
                at,
                since: brief.started.filter(|_| in_flight),
                status: row.shown(),
                dim_from,
            }
        })
        .collect()
}

fn list_rows(kind: ListKind, data: &Data) -> Vec<Row> {
    data.list(kind)
        .iter()
        .enumerate()
        .map(|(at, line)| match &line.entry {
            ListEntry::Group(name) => {
                let text = format!("## {}", String::from_utf8_lossy(name));
                Row {
                    dim_from: text.len(),
                    text,
                    key: None,
                    at,
                    since: None,
                    status: None,
                }
            }
            ListEntry::Item {
                id, text, waits, ..
            } => {
                let text = String::from_utf8_lossy(text).into_owned();
                Row {
                    key: Some(ItemKey {
                        ident: Ident::Id(*id),
                        nth: 0,
                    }),
                    dim_from: text.len(),
                    text: text + &waits_suffix(*waits),
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
