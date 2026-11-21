//! Random event traces through the TUI's state (SPEC.md, TUI, Tests).
//!
//! A trace is what the runtime can send: keys, resizes, the clock, file
//! changes, loads of files that change between them, a load that fails, and
//! fetch results, each for a fetch that was asked for, as the worker answers
//! only those. After every step:
//!
//! - T1 every list's cursor is on an item the list shows, or on nothing when
//!   it shows none;
//! - T2 each panel's cursor row is drawn: at its offset or below, above the
//!   panel's bottom;
//! - T3 a cursor stays on its item while the item is shown, whatever came
//!   (a reload, a filter, a resize, a tab), unless a key moved it; one that
//!   moved lands where its key says;
//! - T4 Esc and navigation start no command, nor does any key but quit; a
//!   file change is one load;
//! - T5 Esc does one thing (stops a filter being typed, or closes help, or
//!   leaves the preview, or clears the focused panel's filter) and nothing
//!   else;
//! - T6 a fetch is asked for only with none out and a minute after the
//!   last, and always when one is due;
//! - T7 the preview shows the focused panel's item, and its scroll stays
//!   inside the text;
//! - T8 the state draws at its size without a panic.

use std::cell::RefCell;

use proptest::prelude::*;
use proptest::test_runner::{
    Config, FileFailurePersistence, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::super::data::{BriefItem, Data};
use super::super::layout::inner_height;
use super::super::view;
use super::*;
use crate::board::BriefRow;
use crate::project::ProjectName;

#[derive(Debug, Clone)]
struct Files {
    /// Number, status, lines of text past the header.
    open: Vec<(u32, Option<Status>, u8)>,
    filed: Vec<(u32, u8)>,
    chores: Vec<ChoreLine>,
    /// Id, and a variant of the text: the same item edited.
    backlog: Vec<(Option<u32>, u8)>,
}

#[derive(Debug, Clone)]
enum ChoreLine {
    Group(u8),
    Item(Option<u32>, u8),
}

#[derive(Debug, Clone)]
enum Step {
    Key(Key),
    Resize(u16, u16),
    /// The clock, this many seconds on.
    Tick(u64),
    FilesChanged,
    Load(Files),
    LoadFailed,
    /// The worker's answer to the fetch that is out; skipped when none is.
    Answer(Answer),
    InputGone,
}

#[derive(Debug, Clone)]
enum Answer {
    Busy,
    Ran { ok: bool, counts: Option<(u8, u8)> },
}

fn status() -> impl Strategy<Value = Option<Status>> {
    prop::sample::select(vec![
        Some(Status::Draft),
        Some(Status::Queued),
        Some(Status::Started),
        Some(Status::Blocked),
        Some(Status::Reported),
        None,
    ])
}

/// Few numbers and ids, so that a reload keeps some items, drops some and
/// adds some, and a hand-typed duplicate comes up.
fn files() -> impl Strategy<Value = Files> {
    let chore = prop_oneof![
        1 => (0u8..3).prop_map(ChoreLine::Group),
        4 => (prop::option::weighted(0.85, 1u32..8), 0u8..2)
            .prop_map(|(id, variant)| ChoreLine::Item(id, variant)),
    ];
    (
        prop::collection::vec((1u32..10, status(), 0u8..60), 0..8),
        prop::collection::vec((1u32..10, 0u8..10), 0..4),
        prop::collection::vec(chore, 0..14),
        prop::collection::vec((prop::option::weighted(0.85, 1u32..12), 0u8..2), 0..30),
    )
        .prop_map(|(open, filed, chores, backlog)| Files {
            open,
            filed,
            chores,
            backlog,
        })
}

fn key() -> impl Strategy<Value = Key> {
    use Key::{Backspace, Char, Ctrl, Down, Enter, Esc, Tab, Up};
    prop::sample::select(vec![
        Char('j'),
        Char('j'),
        Char('j'),
        Char('k'),
        Char('k'),
        Char('g'),
        Char('g'),
        Char('G'),
        Ctrl('d'),
        Ctrl('u'),
        Char('1'),
        Char('2'),
        Char('3'),
        Char('h'),
        Char('l'),
        Tab,
        Char('['),
        Char(']'),
        Enter,
        Enter,
        Esc,
        Esc,
        Char('/'),
        Char('?'),
        Char('q'),
        Backspace,
        Up,
        Down,
        // Filter text: matches many rows, some, or none.
        Char('c'),
        Char('s'),
        Char('x'),
        Char('z'),
        Ctrl('c'),
    ])
}

fn answer() -> impl Strategy<Value = Answer> {
    prop_oneof![
        1 => Just(Answer::Busy),
        3 => (any::<bool>(), prop::option::weighted(0.8, (0u8..4, 0u8..4)))
            .prop_map(|(ok, counts)| Answer::Ran { ok, counts }),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        60 => key().prop_map(Step::Key),
        10 => files().prop_map(Step::Load),
        10 => (0u64..90).prop_map(Step::Tick),
        4 => Just(Step::FilesChanged),
        8 => answer().prop_map(Step::Answer),
        3 => (60u16..=200, 10u16..=50).prop_map(|(w, h)| Step::Resize(w, h)),
        1 => (0u16..60, 0u16..10).prop_map(|(w, h)| Step::Resize(w, h)),
        2 => Just(Step::LoadFailed),
        1 => Just(Step::InputGone),
    ]
}

/// A filter typed whole and kept: `/`, a short text, Enter. Typed key by
/// key from [`key`], a filter takes in every navigation key that follows
/// `/` and hardly ever still matches, so what comes after a kept filter
/// (the preview opened over it, Esc there) would not come up.
fn filter_typed() -> impl Strategy<Value = Vec<Step>> {
    prop::sample::select(vec!["1", "2", "3", "v0", "v1", "s", "e", "b", "c", "zz"]).prop_map(
        |text| {
            let mut steps = vec![Step::Key(Key::Char('/'))];
            steps.extend(text.chars().map(|c| Step::Key(Key::Char(c))));
            steps.push(Step::Key(Key::Enter));
            steps
        },
    )
}

fn trace() -> impl Strategy<Value = (Files, Vec<Step>)> {
    let steps = prop_oneof![
        30 => step().prop_map(|step| vec![step]),
        1 => filter_typed(),
    ];
    (
        files(),
        prop::collection::vec(steps, 1..120).prop_map(|steps| steps.concat()),
    )
}

fn shop() -> ProjectName {
    ProjectName::parse("shop").unwrap()
}

fn stamp(seconds: u64) -> Stamp {
    Stamp::from_unix(seconds).unwrap()
}

/// The data a load of these files gives.
fn data(files: &Files) -> Data {
    let brief = |number: u32, status: Option<Status>, lines: u8| {
        let row = BriefRow {
            number: BriefId::new(number),
            status,
            slug: format!("s{number}"),
        };
        let mut text = format!(
            "id: {number}\nstatus: {}\n\n# Brief {number}\n## Task\n",
            row.status_word()
        );
        for line in 0..lines {
            text.push_str(&format!(
                "line {line} of a brief that runs on long enough to wrap in a narrow preview\n"
            ));
        }
        BriefItem {
            started: matches!(status, Some(Status::Started | Status::Blocked))
                .then(|| stamp(900_000)),
            row,
            text: text.into_bytes(),
        }
    };
    Data {
        project: shop(),
        open: files
            .open
            .iter()
            .map(|(number, status, lines)| brief(*number, *status, *lines))
            .collect(),
        filed: files
            .filed
            .iter()
            .map(|(number, lines)| brief(*number, Some(Status::Filed), *lines))
            .collect(),
        chores: files
            .chores
            .iter()
            .map(|line| match line {
                ChoreLine::Group(group) => ListRow::Group(format!("group {group}").into_bytes()),
                ChoreLine::Item(Some(id), variant) => ListRow::Item(
                    format!("c{id} chore {id} v{variant}. Check: it is done").into_bytes(),
                ),
                ChoreLine::Item(None, variant) => {
                    ListRow::Item(format!("by hand {variant}. Check: x").into_bytes())
                }
            })
            .collect(),
        backlog: files
            .backlog
            .iter()
            .map(|(id, variant)| match id {
                Some(id) => ListRow::Item(format!("b{id} backlog item v{variant}").into_bytes()),
                None => ListRow::Item(format!("by hand ü {variant}").into_bytes()),
            })
            .collect(),
    }
}

fn fetched(answer: &Answer) -> Fetched {
    match answer {
        Answer::Busy => Fetched::Busy,
        Answer::Ran { ok, counts } => Fetched::Ran {
            fetched: if *ok {
                Ok(())
            } else {
                Err("could not resolve host".to_owned())
            },
            counts: counts.map(|(unpushed, behind)| Counts {
                unpushed: usize::from(unpushed),
                behind: usize::from(behind),
            }),
        },
    }
}

/// Counts of what the traces went through, for the depth check.
#[derive(Debug, Default, Clone, Copy)]
struct Depth {
    steps: u32,
    /// A reload kept a cursor on its item, at another place in the list.
    kept_on_moved_item: u32,
    /// A reload took a cursor's item away, and the cursor went to another.
    item_gone: u32,
    /// A cursor on the second row of one ident.
    on_duplicate: u32,
    esc_typing: u32,
    esc_help: u32,
    esc_preview: u32,
    /// Esc left the preview over a filtered panel: the filter stays.
    esc_preview_filtered: u32,
    /// Esc closed help over the preview: the preview stays.
    esc_help_over_preview: u32,
    esc_filter: u32,
    esc_nothing: u32,
    /// A filter hid rows of a list with a cursor.
    filtered: u32,
    fetches: u32,
    busy: u32,
    fetch_failed: u32,
    preview_scrolled: u32,
    panel_scrolled: u32,
    filed_tab: u32,
    quits: u32,
}

impl Depth {
    fn add(&mut self, other: Depth) {
        self.steps += other.steps;
        self.kept_on_moved_item += other.kept_on_moved_item;
        self.item_gone += other.item_gone;
        self.on_duplicate += other.on_duplicate;
        self.esc_typing += other.esc_typing;
        self.esc_help += other.esc_help;
        self.esc_preview += other.esc_preview;
        self.esc_preview_filtered += other.esc_preview_filtered;
        self.esc_help_over_preview += other.esc_help_over_preview;
        self.esc_filter += other.esc_filter;
        self.esc_nothing += other.esc_nothing;
        self.filtered += other.filtered;
        self.fetches += other.fetches;
        self.busy += other.busy;
        self.fetch_failed += other.fetch_failed;
        self.preview_scrolled += other.preview_scrolled;
        self.panel_scrolled += other.panel_scrolled;
        self.filed_tab += other.filed_tab;
        self.quits += other.quits;
    }
}

/// The oracle's own account of the fetch, apart from the state's.
struct Fetches {
    out: bool,
    last: Option<u64>,
}

fn run_trace(first: &Files, steps: &[Step]) -> Result<Depth, TestCaseError> {
    let mut depth = Depth::default();
    let mut now = 1_000_000;
    let (state, commands) = State::new(Data::empty(shop()), 160, 40, stamp(now));
    prop_assert_eq!(commands, vec![Command::Load, Command::Fetch]);
    let mut fetches = Fetches {
        out: true,
        last: Some(now),
    };
    let (mut state, _) = state.step(Event::Loaded(Box::new(data(first))));
    check(&state, &mut depth)?;
    for step in steps {
        let event = match step {
            Step::Key(key) => Event::Key(*key),
            Step::Resize(width, height) => Event::Resize {
                width: *width,
                height: *height,
            },
            Step::Tick(by) => {
                now += by;
                Event::Tick(stamp(now))
            }
            Step::FilesChanged => Event::FilesChanged,
            Step::Load(files) => Event::Loaded(Box::new(data(files))),
            Step::LoadFailed => Event::LoadFailed("cannot read shop/chores.md".to_owned()),
            Step::Answer(answer) => {
                if !fetches.out {
                    continue;
                }
                fetches.out = false;
                match answer {
                    Answer::Busy => depth.busy += 1,
                    Answer::Ran { ok: false, .. } => depth.fetch_failed += 1,
                    Answer::Ran { ok: true, .. } => {}
                }
                Event::Fetched(fetched(answer))
            }
            Step::InputGone => Event::InputGone,
        };
        let before = state.clone();
        let (after, commands) = state.step(event.clone());
        judge(
            &before,
            &event,
            &after,
            &commands,
            &mut fetches,
            now,
            &mut depth,
        )?;
        check(&after, &mut depth)?;
        depth.steps += 1;
        state = after;
    }
    Ok(depth)
}

/// T1, T2, T7, T8: what holds of every state.
fn check(state: &State, depth: &mut Depth) -> Result<(), TestCaseError> {
    for source in Source::ALL {
        let items = state.items(source);
        let cursor = state.cursor(source);
        if items.is_empty() {
            prop_assert!(cursor.key.is_none(), "{source:?}: {cursor:?} on no item");
        } else {
            prop_assert!(cursor.at < items.len(), "{source:?}: {cursor:?}");
            prop_assert_eq!(cursor.key.as_ref(), Some(&items[cursor.at]), "{:?}", source);
            if cursor.key.as_ref().is_some_and(|key| key.nth > 0) {
                depth.on_duplicate += 1;
            }
            if all_rows(&state.data, source).len() > state.rows(source).len() {
                depth.filtered += 1;
            }
        }
    }
    let areas = state.areas();
    for panel in Panel::ALL {
        let source = state.source_of(panel);
        let height = inner_height(areas.panel(panel));
        let offset = state.offset(source);
        if let (Some(row), true) = (state.cursor_row(source), height > 0) {
            prop_assert!(
                offset <= row && row < offset + height,
                "{panel:?}: row {row} not drawn from {offset} in {height}"
            );
        }
        if offset > 0 {
            depth.panel_scrolled += 1;
        }
    }
    let source = state.source();
    prop_assert_eq!(
        &state.preview.shown,
        &state.cursor(source).key.clone().map(|key| (source, key))
    );
    let max = state
        .preview_content()
        .map_or(0, |preview| preview.max_scroll(areas.preview));
    prop_assert!(
        state.preview.scroll <= max,
        "scroll {} past {max}",
        state.preview.scroll
    );
    if state.preview.scroll > 0 {
        depth.preview_scrolled += 1;
    }
    prop_assert!(!state.in_preview || state.preview.shown.is_some());
    if state.tab == Tab::Filed && state.cursor(Source::Filed).key.is_some() {
        depth.filed_tab += 1;
    }
    let mut terminal = Terminal::new(TestBackend::new(state.width, state.height)).unwrap();
    terminal.draw(|frame| view::draw(frame, state)).unwrap();
    Ok(())
}

/// A key that moves the focused list's cursor, or scrolls the preview.
fn navigates(key: Key) -> bool {
    matches!(
        key,
        Key::Char('j' | 'k' | 'g' | 'G') | Key::Up | Key::Down | Key::Ctrl('d' | 'u')
    )
}

/// Where a navigation key puts the cursor that was at `at` among `items`.
fn landing(before: &State, key: Key, at: usize, items: usize) -> usize {
    let last = items - 1;
    let half = (before.page() / 2).max(1);
    match key {
        Key::Char('j') | Key::Down => (at + 1).min(last),
        Key::Char('k') | Key::Up => at.saturating_sub(1),
        Key::Ctrl('d') => (at + half).min(last),
        Key::Ctrl('u') => at.saturating_sub(half),
        Key::Char('G') => last,
        Key::Char('g') if before.pending_g => 0,
        _ => at,
    }
}

/// The parts of the state Esc may not touch but the one it is for.
fn untouched(state: &State) -> impl PartialEq + std::fmt::Debug {
    (
        (state.focus, state.in_preview, state.tab, state.help),
        (state.filters.clone(), state.typing),
        (state.width, state.height, state.now),
        (
            state.data.clone(),
            state.sync.clone(),
            state.load_error.clone(),
        ),
    )
}

/// T3, T4, T5, T6: what holds of every step.
fn judge(
    before: &State,
    event: &Event,
    after: &State,
    commands: &[Command],
    fetches: &mut Fetches,
    now: u64,
    depth: &mut Depth,
) -> Result<(), TestCaseError> {
    // T3
    let moved = match event {
        Event::Key(key)
            if navigates(*key) && !before.typing && !before.help && !before.in_preview =>
        {
            Some((before.source(), *key))
        }
        _ => None,
    };
    for source in Source::ALL {
        let Some(key) = &before.cursor(source).key else {
            continue;
        };
        let items = after.items(source);
        match moved {
            Some((moved, navigation)) if moved == source => {
                let at = before.cursor(source).at;
                let want = landing(before, navigation, at, items.len());
                prop_assert_eq!(
                    after.cursor(source).at,
                    want,
                    "{:?} after {:?}",
                    source,
                    navigation
                );
            }
            _ => match items.iter().position(|item| item == key) {
                Some(at) => {
                    prop_assert_eq!(after.cursor(source).key.as_ref(), Some(key), "{:?}", source);
                    if matches!(event, Event::Loaded(_)) && at != before.cursor(source).at {
                        depth.kept_on_moved_item += 1;
                    }
                }
                None => {
                    if matches!(event, Event::Loaded(_)) && after.cursor(source).key.is_some() {
                        depth.item_gone += 1;
                    }
                }
            },
        }
    }

    // T4
    let fetch = commands
        .iter()
        .filter(|command| **command == Command::Fetch)
        .count();
    match event {
        Event::Key(key) => {
            let quits = *key == Key::Ctrl('c') || (*key == Key::Char('q') && !before.typing);
            if quits {
                depth.quits += 1;
            }
            let want = if quits {
                vec![Command::Quit]
            } else {
                Vec::new()
            };
            prop_assert_eq!(commands, want.as_slice(), "after {:?}", key);
        }
        Event::FilesChanged => prop_assert_eq!(commands, [Command::Load].as_slice()),
        Event::InputGone => prop_assert_eq!(commands, [Command::Quit].as_slice()),
        Event::Tick(_) => prop_assert_eq!(commands.len(), fetch),
        Event::Resize { .. } | Event::Loaded(_) | Event::LoadFailed(_) | Event::Fetched(_) => {
            prop_assert!(commands.is_empty(), "{commands:?} after {event:?}");
        }
    }

    // T5
    if let Event::Key(Key::Esc) = event {
        let mut want = before.clone();
        let focus = before.focus as usize;
        if before.typing {
            want.typing = false;
            want.filters[focus] = None;
            depth.esc_typing += 1;
        } else if before.help {
            want.help = false;
            depth.esc_help += 1;
            if before.in_preview {
                depth.esc_help_over_preview += 1;
            }
        } else if before.in_preview {
            want.in_preview = false;
            depth.esc_preview += 1;
            if before.filters[focus].is_some() {
                depth.esc_preview_filtered += 1;
            }
        } else if before.filters[focus].is_some() {
            want.filters[focus] = None;
            depth.esc_filter += 1;
        } else {
            depth.esc_nothing += 1;
        }
        prop_assert_eq!(untouched(after), untouched(&want));
    }

    // T6
    if let Event::Tick(_) = event {
        let due = !fetches.out && fetches.last.is_none_or(|last| now - last >= FETCH_EVERY);
        prop_assert_eq!(
            fetch,
            usize::from(due),
            "out {}, last {:?}, now {}",
            fetches.out,
            fetches.last,
            now
        );
        if due {
            fetches.out = true;
            fetches.last = Some(now);
            depth.fetches += 1;
        }
    }
    Ok(())
}

fn config(cases: u32) -> Config {
    // `PROPTEST_CASES` wins, as it does for `proptest!` tests.
    let cases = if std::env::var_os("PROPTEST_CASES").is_some() {
        Config::default().cases
    } else {
        cases
    };
    Config {
        cases,
        max_shrink_iters: 1024,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "proptest-regressions/tui.txt",
        ))),
        ..Config::default()
    }
}

fn report(result: Result<(), TestError<(Files, Vec<Step>)>>) {
    if let Err(error) = result {
        panic!("{error}");
    }
}

#[test]
fn every_trace_keeps_the_invariants() {
    let mut runner = TestRunner::new(config(256));
    report(runner.run(&trace(), |(first, steps)| {
        run_trace(&first, &steps).map(drop)
    }));
}

/// A trace that never reaches a mechanism proves nothing about it: a fixed
/// seed, and every mechanism has to come up.
#[test]
fn the_traces_get_deep() {
    let depth = RefCell::new(Depth::default());
    let mut runner = TestRunner::new_with_rng(
        Config {
            failure_persistence: None,
            ..config(64)
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );
    report(runner.run(&trace(), |(first, steps)| {
        depth.borrow_mut().add(run_trace(&first, &steps)?);
        Ok(())
    }));
    let depth = depth.into_inner();
    assert!(depth.steps > 64 * 30, "{depth:?}");
    assert!(depth.kept_on_moved_item > 0, "{depth:?}");
    assert!(depth.item_gone > 0, "{depth:?}");
    assert!(depth.on_duplicate > 0, "{depth:?}");
    assert!(depth.esc_typing > 0, "{depth:?}");
    assert!(depth.esc_help > 0, "{depth:?}");
    assert!(depth.esc_preview > 0, "{depth:?}");
    assert!(depth.esc_preview_filtered > 0, "{depth:?}");
    assert!(depth.esc_help_over_preview > 0, "{depth:?}");
    assert!(depth.esc_filter > 0, "{depth:?}");
    assert!(depth.esc_nothing > 0, "{depth:?}");
    assert!(depth.filtered > 0, "{depth:?}");
    assert!(depth.fetches > 0, "{depth:?}");
    assert!(depth.busy > 0, "{depth:?}");
    assert!(depth.fetch_failed > 0, "{depth:?}");
    assert!(depth.preview_scrolled > 0, "{depth:?}");
    assert!(depth.panel_scrolled > 0, "{depth:?}");
    assert!(depth.filed_tab > 0, "{depth:?}");
    assert!(depth.quits > 0, "{depth:?}");
}
