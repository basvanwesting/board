//! The TUI: `board` on a terminal (SPEC.md, TUI).
//!
//! ```text
//! state.rs    what is shown: a pure function (State, Event) -> (State, Vec<Command>)
//! moves.rs    where the rows stand while moves are made or out
//! layout.rs   where it goes, shared by the state and the view
//! view.rs     the state, drawn
//! data.rs     what a load reads from the files
//! outside.rs  the editor and the clipboard helpers
//! trace.rs    random event traces through the state, with its invariants
//! mod.rs      the runtime
//! ```
//!
//! The runtime is the rest, kept thin because the traces do not run it: it
//! owns the terminal, the file watcher, the clock and the worker thread,
//! turns what they see into events and runs the commands the state gives.
//! `tests/tui.rs` runs it whole, on a screen in memory ([`run_headless`]).

mod data;
mod layout;
mod moves;
mod outside;
mod state;
mod view;

use std::any::Any;
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self as terminal, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::{DefaultTerminal, Terminal};

use crate::board::Board;
use crate::brief::Stamp;
use crate::error::Error;
use crate::project::{Project, ProjectName};
use data::Data;
use state::{Action, Command, Event, Key, State};

/// How often the clock ticks: the ages on screen, and the fetch timer.
const TICK: Duration = Duration::from_secs(1);

/// How long the input thread waits for a key before it looks whether the
/// runtime asked it to pause: the most an `e` waits before the editor
/// starts.
const INPUT_POLL: Duration = Duration::from_millis(50);

/// What the worker thread runs, one at a time, so that navigation never
/// waits for the network (SPEC.md, TUI).
enum Job {
    Fetch,
    Run(Action),
}

/// What the loop waits for: the state's events, and the one thing that
/// ends the TUI from outside the state.
enum Message {
    Event(Event),
    /// The worker panicked, with this payload.
    ///
    /// The TUI ends with that panic rather than carry on: ratatui's panic
    /// hook, process-wide, has already given the terminal back (cooked
    /// mode, the main screen) and printed the message there, so frames
    /// drawn after it would land on the user's shell screen with keys
    /// echoed and line-buffered. This message wakes the loop; what keeps
    /// the frames it draws before reading it off the screen is
    /// [`note_panics`].
    WorkerPanicked(Box<dyn Any + Send>),
}

/// Where the loop draws, and what it lends to the editor.
trait Screen {
    fn size(&self) -> Result<(u16, u16), Error>;
    fn draw(&mut self, state: &State) -> Result<(), Error>;
    /// Runs `outside` with the terminal given to it: no frames, no keys
    /// read, the terminal as the shell has it. Taken back after.
    fn lend<T>(&mut self, outside: impl FnOnce() -> T) -> Result<T, Error>;
    /// Bytes for the terminal itself, outside any frame: OSC 52.
    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

/// The project's files, as each load reads them.
struct Files {
    root: PathBuf,
    project: ProjectName,
}

impl Files {
    fn load(&self) -> Event {
        match Project::load(&self.root, &self.project) {
            Ok(project) => Event::Loaded(Box::new(Data::of(&project))),
            Err(error) => Event::LoadFailed(error.to_string()),
        }
    }
}

/// Runs the TUI on the terminal until it quits.
pub fn run(board: Board) -> Result<(), Error> {
    let (messages, inbox) = mpsc::channel();
    let files = Files {
        root: board.root().to_owned(),
        project: board.project().clone(),
    };
    // Watched before the terminal is taken, so that a failure is told as
    // any command's is.
    let pending = Arc::new(AtomicBool::new(false));
    let _watcher = watch(&files, messages.clone(), Arc::clone(&pending))?;
    let (jobs, _worker) = spawn_worker(worker(board, messages.clone()), messages.clone());
    let terminal = ratatui::try_init().map_err(terminal_error)?;
    let panicked = note_panics();
    let input = spawn_input(messages);
    let mut screen = Tty {
        terminal,
        input,
        panicked: Arc::clone(&panicked),
    };
    let result = run_loop(&mut screen, &files, &inbox, &jobs, &pending, &panicked);
    let restored = ratatui::try_restore().map_err(terminal_error);
    // The worker is not waited for: a fetch it runs may take as long as a
    // stalled connection, and quitting should not. An unfinished fetch
    // goes on holding the clone's lock until it ends (`sync`,
    // `fetch_in_background`), so no write starts beside it. A write is
    // waited for by the state, unless the user quit twice.
    result.and(restored)
}

/// The TUI on a screen in memory, with its keys given up front: the
/// runtime of [`run`] whole (worker, watcher, editor, yank), for
/// `tests/tui.rs`. Each char is a key: `\x1b` Esc, `\r` Enter, `\t` Tab,
/// `\x03` ctrl-c. After the last key the input ends, as a closed terminal
/// ends it, so the TUI quits once its writes are done if no key quit it.
///
/// Gives what the TUI wrote to the terminal outside its frames (OSC 52),
/// once the worker has ended: nothing of the TUI runs on after it.
#[doc(hidden)]
pub fn run_headless(board: Board, keys: &str) -> Result<Vec<u8>, Error> {
    let (messages, inbox) = mpsc::channel();
    let files = Files {
        root: board.root().to_owned(),
        project: board.project().clone(),
    };
    let pending = Arc::new(AtomicBool::new(false));
    let watcher = watch(&files, messages.clone(), Arc::clone(&pending))?;
    let (jobs, worker) = spawn_worker(worker(board, messages.clone()), messages.clone());
    for key in keys.chars() {
        let key = match key {
            '\x1b' => Key::Esc,
            '\r' => Key::Enter,
            '\t' => Key::Tab,
            '\x03' => Key::Ctrl('c'),
            c => Key::Char(c),
        };
        let _ = messages.send(Message::Event(Event::Key(key)));
    }
    let _ = messages.send(Message::Event(Event::InputGone));
    drop(messages);
    let Ok(terminal) = Terminal::new(TestBackend::new(160, 40));
    let mut screen = Headless {
        terminal,
        raw: Vec::new(),
    };
    let result = run_loop(
        &mut screen,
        &files,
        &inbox,
        &jobs,
        &pending,
        &AtomicBool::new(false),
    );
    drop((jobs, watcher, inbox));
    if let Err(payload) = worker.join() {
        panic::resume_unwind(payload);
    }
    result.map(|()| screen.raw)
}

fn terminal_error(source: std::io::Error) -> Error {
    Error::Setup(format!("cannot use the terminal: {source}"))
}

/// The terminal, and the input thread that has to stop reading it while
/// the editor has it.
struct Tty {
    terminal: DefaultTerminal,
    input: Sender<Pause>,
    /// [`note_panics`]'s: the terminal is not taken back after a panic.
    panicked: Arc<AtomicBool>,
}

impl Screen for Tty {
    fn size(&self) -> Result<(u16, u16), Error> {
        let size = self.terminal.size().map_err(terminal_error)?;
        Ok((size.width, size.height))
    }

    fn draw(&mut self, state: &State) -> Result<(), Error> {
        self.terminal
            .draw(|frame| view::draw(frame, state))
            .map(drop)
            .map_err(terminal_error)
    }

    fn lend<T>(&mut self, outside: impl FnOnce() -> T) -> Result<T, Error> {
        let (paused, wait) = mpsc::channel();
        // Dropped on every way out: the input thread reads again.
        let (_resume, resumed) = mpsc::channel();
        // An input thread that is gone reads nothing either.
        if self
            .input
            .send(Pause {
                paused,
                resume: resumed,
            })
            .is_ok()
        {
            let _ = wait.recv();
        }
        ratatui::try_restore().map_err(terminal_error)?;
        let value = outside();
        enable_raw_mode().map_err(terminal_error)?;
        execute!(std::io::stdout(), EnterAlternateScreen).map_err(terminal_error)?;
        // A panic while the editor had the terminal (the worker's, say) had
        // ratatui's hook give it back then, and the loop ends the process
        // as soon as it looks, with nothing to give it back again. Looked
        // at after taking it, not before: a hook that runs after the take
        // gives it back itself. Still open is a hook that gave it back
        // before the take and sets the flag after this look, while it
        // prints: the window the frames have too ([`note_panics`]).
        if self.panicked.load(Ordering::SeqCst) {
            ratatui::try_restore().map_err(terminal_error)?;
            return Ok(value);
        }
        // What the editor left on the screen is not the TUI's last frame.
        self.terminal.clear().map_err(terminal_error)?;
        Ok(value)
    }

    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let backend = self.terminal.backend_mut();
        backend
            .write_all(bytes)
            .and_then(|()| backend.flush())
            .map_err(terminal_error)
    }
}

/// A screen in memory: frames drawn as on a terminal, raw bytes kept.
struct Headless {
    terminal: Terminal<TestBackend>,
    raw: Vec<u8>,
}

impl Screen for Headless {
    fn size(&self) -> Result<(u16, u16), Error> {
        let Ok(size) = self.terminal.size();
        Ok((size.width, size.height))
    }

    fn draw(&mut self, state: &State) -> Result<(), Error> {
        let Ok(_) = self.terminal.draw(|frame| view::draw(frame, state));
        Ok(())
    }

    fn lend<T>(&mut self, outside: impl FnOnce() -> T) -> Result<T, Error> {
        Ok(outside())
    }

    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.raw.extend_from_slice(bytes);
        Ok(())
    }
}

/// A flag set once a panic hook has run on any thread: ratatui's, which
/// this one calls first, has then given the terminal back.
///
/// Checked before every frame, because the loop draws for a tick or a
/// queued event before it reads the worker's [`Message::WorkerPanicked`].
/// Set after ratatui's hook, not before: the process may end as soon as it
/// is seen, and the hook has the message to print. A frame already being
/// drawn, or begun while the hook prints, can still land; closing that
/// needs a lock the hook shares with the draw, and a draw that panics
/// while it holds it would deadlock its own hook.
fn note_panics() -> Arc<AtomicBool> {
    let panicked = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&panicked);
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        hook(info);
        flag.store(true, Ordering::SeqCst);
    }));
    panicked
}

fn run_loop(
    screen: &mut impl Screen,
    files: &Files,
    inbox: &Receiver<Message>,
    jobs: &Sender<Job>,
    pending: &AtomicBool,
    panicked: &AtomicBool,
) -> Result<(), Error> {
    let (width, height) = screen.size()?;
    let now = Stamp::now()
        .ok_or_else(|| Error::Setup("the clock is outside the years 1970 to 9999".to_owned()))?;
    let (mut state, mut commands) =
        State::new(Data::empty(files.project.clone()), width, height, now);
    // The answers of what runs here and now (a load, the editor), before
    // any other event.
    let mut answers = VecDeque::new();
    let mut next_tick = Instant::now() + TICK;
    loop {
        for command in commands {
            match command {
                Command::Quit => return Ok(()),
                Command::Load => {
                    // Cleared before the read: a change while it reads is
                    // one more load.
                    pending.store(false, Ordering::SeqCst);
                    answers.push_back(files.load());
                }
                // The worker is gone only after it panicked, and then
                // its message ends the loop.
                Command::Fetch => drop(jobs.send(Job::Fetch)),
                Command::Run(action) => drop(jobs.send(Job::Run(action))),
                Command::Edit { path, line } => {
                    let file = files.root.join(path.to_string());
                    let editor = std::env::var_os("EDITOR");
                    let edited = screen.lend(|| outside::edit(editor.as_deref(), &file, line))?;
                    answers.push_back(Event::Edited(edited));
                    // The terminal may have changed size meanwhile.
                    let (width, height) = screen.size()?;
                    answers.push_back(Event::Resize { width, height });
                }
                Command::Yank(text) => {
                    screen.write_raw(&outside::osc52(&text))?;
                    answers.extend(outside::copy(&text).map(Event::Note));
                }
            }
        }
        if answers.is_empty() {
            if panicked.load(Ordering::SeqCst) {
                // The terminal is the shell's again and the panic said
                // why; the process ends as that panic ends it. Not
                // `resume_unwind`: a panic on the input thread sends no
                // payload, and the worker's may be behind other messages.
                process::exit(101);
            }
            screen.draw(&state)?;
        }
        let event = match answers.pop_front() {
            Some(answer) => answer,
            None if Instant::now() >= next_tick => tick(&mut next_tick, &state),
            None => match inbox.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(Message::Event(event)) => event,
                // Not the hook again: it ran on the worker.
                Ok(Message::WorkerPanicked(payload)) => panic::resume_unwind(payload),
                Err(RecvTimeoutError::Timeout) => tick(&mut next_tick, &state),
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            },
        };
        (state, commands) = state.step(event);
    }
}

/// The clock's event, due now; the next is a [`TICK`] on. A clock that
/// stopped making sense keeps the last time.
fn tick(next_tick: &mut Instant, state: &State) -> Event {
    *next_tick = Instant::now() + TICK;
    Event::Tick(Stamp::now().unwrap_or(state.now()))
}

/// Watches the board-data clone for changes under the project's directory.
///
/// The whole clone, recursively: the project's directory may not exist
/// before its first write. Events in `.git` (the background fetch's own)
/// are outside the project's directory and dropped here. A burst of events
/// (a checkout of five files) is one load: no event is sent while one is
/// waiting for its load.
fn watch(
    files: &Files,
    messages: Sender<Message>,
    pending: Arc<AtomicBool>,
) -> Result<RecommendedWatcher, Error> {
    // Canonical: FSEvents reports paths with every link resolved.
    let root =
        fs::canonicalize(&files.root).map_err(|source| Error::io("read", &files.root, source))?;
    let dir = root.join(files.project.as_str());
    let cannot = |error: notify::Error| {
        Error::Setup(format!(
            "cannot watch {} for changes: {error}",
            root.display()
        ))
    };
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if changes(&result, &dir) && !pending.swap(true, Ordering::SeqCst) {
            let _ = messages.send(Message::Event(Event::FilesChanged));
        }
    })
    .map_err(cannot)?;
    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(cannot)?;
    Ok(watcher)
}

/// Whether a watcher's event may have changed a file below `dir`.
///
/// Not a file opened or read: inotify reports those too, and a load would
/// then cause the next one. A watcher error or a lost event might hide a
/// change, so it reloads.
fn changes(result: &notify::Result<notify::Event>, dir: &Path) -> bool {
    let Ok(event) = result else {
        return true;
    };
    if event.need_rescan() {
        return true;
    }
    let writes = match event.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Any
        | EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Other => true,
    };
    writes && event.paths.iter().any(|path| path.starts_with(dir))
}

/// What the worker runs: the fetch, and each write by the function its
/// CLI command runs, in process (SPEC.md, TUI). The board's notes go to
/// the bottom line, not to stderr, which is the TUI's screen.
fn worker(board: Board, messages: Sender<Message>) -> impl FnMut(Job) -> Event + Send + 'static {
    let board = board.with_notes(Box::new(move |note| {
        let _ = messages.send(Message::Event(Event::Note(note.to_string())));
    }));
    let fetcher = board.fetcher();
    move |job| match job {
        Job::Fetch => Event::Fetched(fetcher.fetch()),
        Job::Run(action) => {
            let result = match &action {
                Action::Done(id) => board.done(&[*id]),
                Action::Drop(id) => board.drop_items(&[*id]),
                Action::Move(wanted) => board.move_item(wanted),
                Action::Sync => board.sync().map(|()| "synced".to_owned()),
            };
            Event::Ran(action, result.map_err(|error| error.to_string()))
        }
    }
}

/// Runs each job with `run` on a thread of its own and sends its answer; a
/// panic is sent too, and is the worker's last message.
fn spawn_worker(
    mut run: impl FnMut(Job) -> Event + Send + 'static,
    messages: Sender<Message>,
) -> (Sender<Job>, JoinHandle<()>) {
    let (jobs, queue) = mpsc::channel();
    let worker = thread::spawn(move || {
        for job in queue {
            let message = match panic::catch_unwind(AssertUnwindSafe(|| run(job))) {
                Ok(event) => Message::Event(event),
                Err(payload) => {
                    let _ = messages.send(Message::WorkerPanicked(payload));
                    return;
                }
            };
            if messages.send(message).is_err() {
                return;
            }
        }
    });
    (jobs, worker)
}

/// The runtime's request to the input thread to stop reading: it says so
/// on `paused`, and reads again once `resume` is closed.
struct Pause {
    paused: Sender<()>,
    resume: Receiver<()>,
}

/// Reads the terminal's keys and sizes on a thread of its own, so that the
/// loop can wait on one channel for everything; pauses when asked to.
///
/// The pause is for the editor of `e`: while this thread waits in `read`,
/// crossterm's event reader is locked and takes the editor's keys. So it
/// waits in `poll`, [`INPUT_POLL`] at a time, and looks for a pause in
/// between: what an `e` waits before the editor starts.
fn spawn_input(messages: Sender<Message>) -> Sender<Pause> {
    let (pauses, requests) = mpsc::channel::<Pause>();
    thread::spawn(move || {
        loop {
            match requests.try_recv() {
                Ok(Pause { paused, resume }) => {
                    let _ = paused.send(());
                    // Ends when the runtime closes it, or is gone.
                    let _ = resume.recv();
                    continue;
                }
                // The runtime is gone.
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {}
            }
            let event = match terminal::poll(INPUT_POLL).and_then(|ready| {
                if ready {
                    terminal::read().map(Some)
                } else {
                    Ok(None)
                }
            }) {
                Ok(None) => continue,
                Ok(Some(terminal::Event::Key(key))) => match key_of(key) {
                    Some(key) => Event::Key(key),
                    None => continue,
                },
                Ok(Some(terminal::Event::Resize(width, height))) => Event::Resize { width, height },
                Ok(Some(_)) => continue,
                // No more keys will come: the TUI could not be quit.
                Err(_) => Event::InputGone,
            };
            let gone = matches!(event, Event::InputGone);
            if messages.send(Message::Event(event)).is_err() || gone {
                return;
            }
        }
    });
    pauses
}

/// The state's key for a terminal's key press; `None` for a key the TUI
/// has no use for.
fn key_of(key: KeyEvent) -> Option<Key> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match key.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Fetched;
    use notify::event::{CreateKind, DataChange, ModifyKind};

    fn event(kind: EventKind, path: &str) -> notify::Result<notify::Event> {
        Ok(notify::Event::new(kind).add_path(PathBuf::from(path)))
    }

    #[test]
    fn a_change_in_the_project_reloads_and_a_read_or_git_does_not() {
        let dir = PathBuf::from("/data/shop");
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Content));
        assert!(changes(&event(modify, "/data/shop/chores.md"), &dir));
        assert!(changes(
            &event(
                EventKind::Create(CreateKind::File),
                "/data/shop/briefs/1-a.md"
            ),
            &dir
        ));
        assert!(changes(
            &event(
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
                "/data/shop/backlog.md"
            ),
            &dir
        ));
        // A load reads the files: were that a change, it would load again.
        for read in [
            AccessKind::Open(AccessMode::Any),
            AccessKind::Read,
            AccessKind::Close(AccessMode::Read),
        ] {
            assert!(!changes(
                &event(EventKind::Access(read), "/data/shop/chores.md"),
                &dir
            ));
        }
        assert!(!changes(&event(modify, "/data/.git/FETCH_HEAD"), &dir));
        assert!(!changes(&event(modify, "/data/other/chores.md"), &dir));
        // `shop` is not the directory of `shop2`.
        assert!(!changes(&event(modify, "/data/shop2/chores.md"), &dir));
        assert!(changes(
            &Err(notify::Error::generic("queue overflow")),
            &dir
        ));
    }

    #[test]
    fn a_worker_answers_its_jobs_until_one_panics_and_then_says_so() {
        let (messages, inbox) = mpsc::channel();
        let mut jobs_run = 0;
        let (jobs, worker) = spawn_worker(
            move |_| {
                jobs_run += 1;
                assert!(jobs_run < 2, "a bug in the second job");
                Event::Fetched(Fetched::Busy)
            },
            messages,
        );
        for _ in 0..3 {
            jobs.send(Job::Fetch).unwrap();
        }
        assert!(matches!(
            inbox.recv(),
            Ok(Message::Event(Event::Fetched(Fetched::Busy)))
        ));
        match inbox.recv() {
            Ok(Message::WorkerPanicked(payload)) => {
                assert_eq!(
                    payload.downcast_ref::<&str>(),
                    Some(&"a bug in the second job")
                );
            }
            Ok(Message::Event(event)) => panic!("{event:?}"),
            Err(error) => panic!("{error}"),
        }
        // Its last message: the third job is not run.
        assert!(inbox.recv().is_err());
        worker.join().unwrap();
    }

    #[test]
    fn keys_map_to_the_states() {
        let press = |code, modifiers| key_of(KeyEvent::new(code, modifiers));
        assert_eq!(
            press(KeyCode::Char('j'), KeyModifiers::NONE),
            Some(Key::Char('j'))
        );
        assert_eq!(
            press(KeyCode::Char('G'), KeyModifiers::SHIFT),
            Some(Key::Char('G'))
        );
        assert_eq!(
            press(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(Key::Ctrl('d'))
        );
        assert_eq!(press(KeyCode::Esc, KeyModifiers::NONE), Some(Key::Esc));
        assert_eq!(
            press(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(Key::BackTab)
        );
        assert_eq!(press(KeyCode::F(1), KeyModifiers::NONE), None);
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_of(release), None);
    }
}
