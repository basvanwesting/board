//! The TUI: `board` on a terminal (SPEC.md, TUI).
//!
//! ```text
//! state.rs   what is shown: a pure function (State, Event) -> (State, Vec<Command>)
//! layout.rs  where it goes, shared by the state and the view
//! view.rs    the state, drawn
//! data.rs    what a load reads from the files
//! trace.rs   random event traces through the state, with its invariants
//! mod.rs     the runtime
//! ```
//!
//! The runtime is the rest, kept thin because the traces do not run it: it
//! owns the terminal, the file watcher, the clock and the worker thread,
//! turns what they see into events and runs the commands the state gives.

mod data;
mod layout;
mod state;
mod view;

use std::any::Any;
use std::collections::VecDeque;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self as terminal, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::board::Board;
use crate::brief::Stamp;
use crate::error::Error;
use data::Data;
use state::{Command, Event, Key, State};

/// How often the clock ticks: the ages on screen, and the fetch timer.
const TICK: Duration = Duration::from_secs(1);

/// What the worker thread runs, one at a time, so that navigation never
/// waits for the network (SPEC.md, TUI).
enum Job {
    Fetch,
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

/// Runs the TUI on the terminal until it quits.
pub fn run(board: &Board) -> Result<(), Error> {
    let (messages, inbox) = mpsc::channel();
    // Watched before the terminal is taken, so that a failure is told as
    // any command's is.
    let pending = Arc::new(AtomicBool::new(false));
    let _watcher = watch(board, messages.clone(), Arc::clone(&pending))?;
    let fetcher = board.fetcher();
    let jobs = spawn_worker(
        move |job| match job {
            Job::Fetch => Event::Fetched(fetcher.fetch()),
        },
        messages.clone(),
    );
    let mut terminal = ratatui::try_init().map_err(terminal_error)?;
    let panicked = note_panics();
    spawn_input(messages);
    let result = run_loop(&mut terminal, board, &inbox, &jobs, &pending, &panicked);
    let restored = ratatui::try_restore().map_err(terminal_error);
    // The worker is not waited for: a fetch it runs may take as long as a
    // stalled connection, and quitting should not. An unfinished fetch
    // goes on holding the clone's lock until it ends (`sync`,
    // `fetch_in_background`), so no write starts beside it.
    result.and(restored)
}

fn terminal_error(source: std::io::Error) -> Error {
    Error::Setup(format!("cannot use the terminal: {source}"))
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
    terminal: &mut DefaultTerminal,
    board: &Board,
    inbox: &Receiver<Message>,
    jobs: &Sender<Job>,
    pending: &AtomicBool,
    panicked: &AtomicBool,
) -> Result<(), Error> {
    let size = terminal.size().map_err(terminal_error)?;
    let now = Stamp::now()
        .ok_or_else(|| Error::Setup("the clock is outside the years 1970 to 9999".to_owned()))?;
    let (mut state, mut commands) = State::new(
        Data::empty(board.project().clone()),
        size.width,
        size.height,
        now,
    );
    // The answers of what runs here and now (a load), before any other
    // event.
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
                    answers.push_back(load(board));
                }
                // The worker is gone only after it panicked, and then
                // its message ends the loop.
                Command::Fetch => drop(jobs.send(Job::Fetch)),
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
            terminal
                .draw(|frame| view::draw(frame, &state))
                .map_err(terminal_error)?;
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

fn load(board: &Board) -> Event {
    match board.load() {
        Ok(project) => Event::Loaded(Box::new(Data::of(&project))),
        Err(error) => Event::LoadFailed(error.to_string()),
    }
}

/// Watches the board-data clone for changes under the project's directory.
///
/// The whole clone, recursively: the project's directory may not exist
/// before its first write. Events in `.git` (the background fetch's own)
/// are outside the project's directory and dropped here. A burst of events
/// (a checkout of five files) is one load: no event is sent while one is
/// waiting for its load.
fn watch(
    board: &Board,
    messages: Sender<Message>,
    pending: Arc<AtomicBool>,
) -> Result<RecommendedWatcher, Error> {
    // Canonical: FSEvents reports paths with every link resolved.
    let root =
        fs::canonicalize(board.root()).map_err(|source| Error::io("read", board.root(), source))?;
    let dir = root.join(board.project().as_str());
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

/// Runs each job with `run` on a thread of its own and sends its answer; a
/// panic is sent too, and is the worker's last message.
fn spawn_worker(
    mut run: impl FnMut(Job) -> Event + Send + 'static,
    messages: Sender<Message>,
) -> Sender<Job> {
    let (jobs, queue) = mpsc::channel();
    thread::spawn(move || {
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
    jobs
}

/// Reads the terminal's keys and sizes on a thread of its own, so that the
/// loop can wait on one channel for everything.
///
/// Decision for `e` (SPEC.md, TUI: it suspends the TUI for `$EDITOR`),
/// which comes with the write actions: this thread has to stop reading
/// first. `read` blocks with crossterm's event reader locked, so while it
/// waits it takes the editor's keys and blocks any other reader; the
/// change is `event::poll` with a timeout here, plus a pause the runtime
/// asks for and waits on before it gives the terminal away. Not built
/// before `e` exists: nothing would pause it, and nothing could test it.
fn spawn_input(messages: Sender<Message>) {
    thread::spawn(move || {
        loop {
            let event = match terminal::read() {
                Ok(terminal::Event::Key(key)) => match key_of(key) {
                    Some(key) => Event::Key(key),
                    None => continue,
                },
                Ok(terminal::Event::Resize(width, height)) => Event::Resize { width, height },
                Ok(_) => continue,
                // No more keys will come: the TUI could not be quit.
                Err(_) => Event::InputGone,
            };
            let gone = matches!(event, Event::InputGone);
            if messages.send(Message::Event(event)).is_err() || gone {
                return;
            }
        }
    });
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
    use std::path::PathBuf;

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
        let jobs = spawn_worker(
            move |Job::Fetch| {
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
        assert_eq!(press(KeyCode::F(1), KeyModifiers::NONE), None);
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key_of(release), None);
    }
}
