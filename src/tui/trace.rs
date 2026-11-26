//! Random event traces through the TUI's state (SPEC.md, TUI, Tests).
//!
//! A trace is what the runtime can send: keys, resizes, the clock, file
//! changes, loads of files that change between them, a load that fails, a
//! write's note, and the answers of the worker and the editor, each to a
//! command that was given, in the order given, as the runtime answers only
//! those. A quit ends the TUI, and the trace starts it again. After every
//! step:
//!
//! - T1 every list's cursor is on an item the list shows, or on nothing when
//!   it shows none;
//! - T2 each panel's cursor row is drawn: at its offset or below, above the
//!   panel's bottom;
//! - T3 a cursor stays on its item while the item is shown, whatever came
//!   (a reload, a filter, a resize, a tab), unless a key moved it;
//!   one that moved lands where its key says;
//! - T4 every command comes from the event that gives it, Esc gives none,
//!   navigation none, `J`/`K` none (no key of the TUI's since items lost
//!   their order), and a file change is one load;
//! - T5 Esc does one thing (stops a filter being typed, or answers no, or
//!   closes help, or leaves the preview, or clears the focused panel's
//!   filter) and nothing else;
//! - T6 a fetch is asked for only with none out and a minute after the
//!   last, and always when one is due; a write's answer makes one due;
//! - T7 the preview shows the focused panel's item, and its scroll stays
//!   inside the text;
//! - T8 the state draws at its size without a panic;
//! - T9 `d`/`x` never run unconfirmed: a done or a drop is run only by `y`
//!   to a question, which only `d` (on a chore) or `x` (on a backlog item)
//!   asks, about the item under the cursor, by the id the CLI reads on
//!   its line;
//! - T10 the state knows the writes out, in order; quit waits for them
//!   (a second quit does not), and comes when the last one has answered;
//! - T11 the editor never opens while a write is out, and on the item `e`
//!   was pressed on; one waited for opens at the load after the last
//!   write, unless its item is gone, and any key but Esc calls it off
//!   (only `e` asks again);
//! - T12 a click lands where the screen drew what it hit: it focuses the
//!   pane drawn under it and selects the item drawn on its row; the wheel
//!   moves as `j`/`k`; either is an unrelated key first (it calls off an
//!   edit waited for, clears a pending `g` or `y`), and does
//!   nothing at all while the filter is typed, a question is asked, help
//!   is open or a quit waits. Where a click lands comes from the frame
//!   drawn before it (the panes' boxes found by their `[n]` titles and
//!   corners), not from the state's own hit test.

use std::cell::RefCell;
use std::collections::VecDeque;

use proptest::prelude::*;
use proptest::test_runner::{
    Config, FileFailurePersistence, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::super::data::{BriefItem, Data, ListLine};
use super::super::layout::inner_height;
use super::super::view;
use super::*;
use crate::board::{BriefRow, ListRow};
use crate::project::ProjectName;

#[derive(Debug, Clone)]
struct Files {
    /// Number, status, lines of text past the header.
    open: Vec<(u32, Option<Status>, u8)>,
    filed: Vec<(u32, u8)>,
    /// Group, id, and a variant of the text: the same item edited (2: no
    /// check). An id given twice is one item, the first.
    chores: Vec<(u8, u32, u8)>,
    /// Group, id, and a variant of the text. An id given twice is one item.
    backlog: Vec<(u8, u32, u8)>,
}

#[derive(Debug, Clone)]
enum Step {
    Key(Key),
    Mouse(Mouse),
    Resize(u16, u16),
    /// The clock, this many seconds on.
    Tick(u64),
    FilesChanged,
    Load(Files),
    /// The files as the last load had them: what a write that changed
    /// another project's files, or nothing, gives.
    LoadSame,
    LoadFailed,
    /// The answer to `Command::Unpushed` that is out: these briefs have
    /// unpushed commits, which puts the filed ones among them in the
    /// Briefs tab; `None` when git could not tell. Skipped when none is
    /// out.
    Unpushed(Option<Vec<u32>>),
    /// The worker's answer to the fetch that is out; skipped when none is.
    Answer(Answer),
    /// The worker's answer to the oldest write out, a success or not;
    /// skipped when none is.
    Wrote(bool),
    /// The editor's answer to an edit that is out; skipped when none is.
    Edited(u8),
    Note,
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
/// adds some, and a brief number on two files comes up.
fn files() -> impl Strategy<Value = Files> {
    (
        prop::collection::vec((1u32..10, status(), 0u8..60), 0..8),
        prop::collection::vec((1u32..10, 0u8..10), 0..4),
        prop::collection::vec((0u8..3, 1u32..12, 0u8..3), 0..14),
        prop::collection::vec((0u8..3, 1u32..14, 0u8..2), 0..30),
    )
        .prop_map(|(open, filed, chores, backlog)| Files {
            open,
            filed,
            chores,
            backlog,
        })
}

fn key() -> impl Strategy<Value = Key> {
    use Key::{BackTab, Backspace, Char, Ctrl, Down, Enter, Esc, Tab, Up};
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
        Char('2'),
        Char('3'),
        Char('3'),
        Char('h'),
        Char('l'),
        Tab,
        BackTab,
        Char('['),
        Char(']'),
        Char('0'),
        Enter,
        Enter,
        Esc,
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
        Char('z'),
        // The actions.
        Char('d'),
        Char('x'),
        Char('y'),
        Char('y'),
        Char('n'),
        Char('J'),
        Char('J'),
        Char('K'),
        Char('K'),
        Char('e'),
        Char('r'),
        Char('Y'),
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
        8 => files().prop_map(Step::Load),
        3 => Just(Step::LoadSame),
        10 => (0u64..90).prop_map(Step::Tick),
        4 => Just(Step::FilesChanged),
        8 => answer().prop_map(Step::Answer),
        8 => any::<bool>().prop_map(Step::Wrote),
        3 => (0u8..3).prop_map(Step::Edited),
        2 => Just(Step::Note),
        3 => (60u16..=200, 10u16..=50).prop_map(|(w, h)| Step::Resize(w, h)),
        1 => (0u16..60, 0u16..10).prop_map(|(w, h)| Step::Resize(w, h)),
        2 => Just(Step::LoadFailed),
        4 => prop::option::weighted(0.9, prop::collection::vec(1u32..10, 0..4)).prop_map(Step::Unpushed),
        1 => Just(Step::InputGone),
        // Anywhere, off the screen too: the sizes go up to 200 x 50.
        6 => prop_oneof![
            4 => (0u16..220, 0u16..56).prop_map(|(column, row)| Mouse::Click { column, row }),
            1 => Just(Mouse::WheelUp),
            1 => Just(Mouse::WheelDown),
        ]
        .prop_map(Step::Mouse),
    ]
}

/// A filter typed whole and kept: `/`, a short text, Enter, and at times
/// the preview opened over it and left by Esc. Typed key by key from
/// [`key`], a filter takes in every navigation key that follows `/` and
/// hardly ever still matches, so what comes after a kept filter would not
/// come up.
fn filter_typed() -> impl Strategy<Value = Vec<Step>> {
    (
        prop::sample::select(vec!["1", "2", "3", "v0", "v1", "s", "e", "b", "c", "zz"]),
        any::<bool>(),
    )
        .prop_map(|(text, preview)| {
            let mut steps = vec![Step::Key(Key::Char('/'))];
            steps.extend(text.chars().map(|c| Step::Key(Key::Char(c))));
            steps.push(Step::Key(Key::Enter));
            if preview {
                steps.extend([Step::Key(Key::Enter), Step::Key(Key::Esc)]);
            }
            steps
        })
}

/// `d` or `x` in its panel, on some item, and an answer: random keys
/// rarely ask in the right panel and then answer.
fn asked() -> impl Strategy<Value = Vec<Step>> {
    use Key::{Char, Esc};
    (
        prop::sample::select(vec![('2', 'd'), ('3', 'x'), ('2', 'x'), ('1', 'd')]),
        0usize..4,
        prop::sample::select(vec![
            Char('y'),
            Char('y'),
            Char('n'),
            Esc,
            Char('j'),
            Char('d'),
        ]),
    )
        .prop_map(|((panel, ask), down, answer)| {
            let mut steps = vec![Step::Key(Char(panel))];
            steps.extend((0..down).map(|_| Step::Key(Char('j'))));
            steps.extend([Step::Key(Char(ask)), Step::Key(answer)]);
            steps
        })
}

/// `e` while a write is out: the editor waits for its answer and the load
/// after it, which may give the item an id. Random steps rarely bring the
/// three in that order.
fn edit_waits() -> impl Strategy<Value = Vec<Step>> {
    (
        prop::sample::select(vec!['1', '2', '3']),
        0usize..4,
        any::<bool>(),
        Just(Step::LoadSame),
        0u8..3,
    )
        .prop_map(|(panel, down, ok, load, how)| {
            let mut steps = vec![Step::Key(Key::Char(panel))];
            steps.extend((0..down).map(|_| Step::Key(Key::Char('j'))));
            steps.extend([
                Step::Key(Key::Char('r')),
                Step::Key(Key::Char('e')),
                Step::Wrote(ok),
                load,
                Step::Edited(how),
            ]);
            steps
        })
}

/// Into the preview, scrolled, at times help over it, and out by Esc: with
/// the action keys among them, random keys seldom stay in the preview.
fn previewed() -> impl Strategy<Value = Vec<Step>> {
    use Key::{Char, Ctrl, Enter, Esc};
    (
        prop::sample::select(vec!['1', '2', '3']),
        prop::collection::vec(
            prop::sample::select(vec![Char('j'), Ctrl('d'), Char('G'), Char('k')]),
            0..4,
        ),
        any::<bool>(),
    )
        .prop_map(|(panel, scrolls, help)| {
            let mut steps = vec![Step::Key(Char(panel)), Step::Key(Enter)];
            steps.extend(scrolls.into_iter().map(Step::Key));
            if help {
                steps.extend([Step::Key(Char('?')), Step::Key(Esc)]);
            }
            steps.push(Step::Key(Esc));
            steps
        })
}

/// `yy` on some item of a panel: a `y` from [`key`] is seldom followed by
/// another with nothing between.
fn yanked_whole() -> impl Strategy<Value = Vec<Step>> {
    use Key::Char;
    (prop::sample::select(vec!['1', '2', '3']), 0usize..4).prop_map(|(panel, down)| {
        let mut steps = vec![Step::Key(Char(panel))];
        steps.extend((0..down).map(|_| Step::Key(Char('j'))));
        steps.extend([Step::Key(Char('y')), Step::Key(Char('y'))]);
        steps
    })
}

fn trace() -> impl Strategy<Value = (Files, Vec<Step>)> {
    let steps = prop_oneof![
        30 => step().prop_map(|step| vec![step]),
        1 => filter_typed(),
        3 => asked(),
        1 => edit_waits(),
        1 => previewed(),
        1 => yanked_whole(),
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
            unpushed: false,
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
        BriefItem::sample(
            &shop(),
            row,
            matches!(status, Some(Status::Started | Status::Blocked)).then(|| stamp(900_000)),
            text.into_bytes(),
        )
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
        chores: ListLine::numbered(ListKind::Chores, chore_rows(&files.chores)),
        backlog: ListLine::numbered(ListKind::Backlog, backlog_rows(&files.backlog)),
    }
}

/// The items whose id has not come before: one file per id.
fn unique<T: Copy>(items: &[T], id: impl Fn(&T) -> u32) -> Vec<T> {
    let mut seen = Vec::new();
    items
        .iter()
        .filter(|item| {
            let first = !seen.contains(&id(item));
            seen.push(id(item));
            first
        })
        .copied()
        .collect()
}

/// A kind's items as `ls` lists them: a group by name, its items by id, a
/// group only while an item names it.
fn grouped_rows(items: &[(u8, u32, u8)], line: impl Fn(u32, u8) -> String) -> Vec<ListRow> {
    let mut items = unique(items, |(_, id, _)| *id);
    items.sort_by_key(|(group, id, _)| (*group, *id));
    let mut rows = Vec::new();
    let mut last = None;
    for (group, id, variant) in items {
        if last != Some(group) {
            rows.push(ListRow::Group(format!("group {group}").into_bytes()));
            last = Some(group);
        }
        rows.push(ListRow::Item(line(id, variant).into_bytes()));
    }
    rows
}

fn chore_rows(chores: &[(u8, u32, u8)]) -> Vec<ListRow> {
    grouped_rows(chores, |id, variant| match variant {
        2 => format!("c{id} chore v2"),
        _ => format!("c{id} chore v{variant}. Check: it is done"),
    })
}

fn backlog_rows(backlog: &[(u8, u32, u8)]) -> Vec<ListRow> {
    grouped_rows(backlog, |id, variant| {
        format!("b{id} backlog item ü v{variant}")
    })
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
    /// A new set of unpushed briefs moved a brief between the tabs.
    relabelled: u32,
    /// A cursor on the second row of one ident.
    on_duplicate: u32,
    esc_typing: u32,
    esc_question: u32,
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
    asked: u32,
    confirmed: u32,
    /// A question answered no by a key that is not Esc.
    declined: u32,
    edits: u32,
    /// An edit that waited for the writes out.
    edits_waited: u32,
    /// A key but Esc or `e` called off an edit waited for.
    edit_called_off: u32,
    yanks: u32,
    /// `yy`: the whole item yanked.
    whole_yanks: u32,
    syncs: u32,
    wrote_ok: u32,
    wrote_failed: u32,
    /// A quit that waited for writes, and the quit that came after them.
    quit_waited: u32,
    quit_after_writes: u32,
    quit_forced: u32,
    /// A click that moved the focus to another pane.
    click_focused: u32,
    /// A click that put a cursor on another item.
    click_selected: u32,
    /// A click into the preview.
    click_previewed: u32,
    /// A click or the wheel while a key is waited for: nothing.
    mouse_waited: u32,
    wheeled: u32,
    /// `J` or `K` on a list item, which does nothing.
    upper_jk: u32,
}

impl Depth {
    fn add(&mut self, other: Depth) {
        macro_rules! sum {
            ($($field:ident),*) => { $(self.$field += other.$field;)* };
        }
        sum!(
            steps,
            kept_on_moved_item,
            item_gone,
            relabelled,
            on_duplicate,
            esc_typing,
            esc_question,
            esc_help,
            esc_preview,
            esc_preview_filtered,
            esc_help_over_preview,
            esc_filter,
            esc_nothing,
            filtered,
            fetches,
            busy,
            fetch_failed,
            preview_scrolled,
            panel_scrolled,
            filed_tab,
            quits,
            asked,
            confirmed,
            declined,
            edits,
            edits_waited,
            edit_called_off,
            yanks,
            whole_yanks,
            syncs,
            wrote_ok,
            wrote_failed,
            quit_waited,
            quit_after_writes,
            quit_forced,
            click_focused,
            click_selected,
            click_previewed,
            mouse_waited,
            wheeled,
            upper_jk
        );
    }
}

/// The oracle's own account of what is out, apart from the state's.
struct Oracle {
    /// A fetch was asked for and has not answered.
    fetch_out: bool,
    /// When the last fetch was asked for; `None` after a write's answer.
    fetch_last: Option<u64>,
    /// The writes sent and not answered, in the order sent.
    writes: VecDeque<Action>,
    /// Edits asked for and not answered.
    edits: usize,
    /// The item the last key, a `y`, yanked the id of.
    y_armed: Option<ItemKey>,
    /// Which briefs are unpushed was asked and has not answered.
    unpushed_out: bool,
    /// When it was last asked.
    unpushed_last: u64,
}

impl Oracle {
    /// A TUI just started: its first fetch is out.
    fn new(now: u64) -> Oracle {
        Oracle {
            fetch_out: true,
            fetch_last: Some(now),
            writes: VecDeque::new(),
            edits: 0,
            y_armed: None,
            unpushed_out: true,
            unpushed_last: now,
        }
    }
}

/// A TUI started at `now` on these files.
fn start(files: &Files, now: u64) -> Result<(State, Oracle), TestCaseError> {
    let (state, commands) = State::new(Data::empty(shop()), 160, 40, stamp(now));
    prop_assert_eq!(
        commands,
        vec![Command::Load, Command::Unpushed, Command::Fetch]
    );
    let (state, commands) = state.step(Event::Loaded(Box::new(data(files))));
    prop_assert!(commands.is_empty());
    Ok((state, Oracle::new(now)))
}

fn run_trace(first: &Files, steps: &[Step]) -> Result<Depth, TestCaseError> {
    let mut depth = Depth::default();
    let mut now = 1_000_000;
    let mut last = first.clone();
    let (mut state, mut oracle) = start(first, now)?;
    check(&state, &mut depth)?;
    for step in steps {
        let event = match step {
            Step::Key(key) => Event::Key(*key),
            Step::Mouse(mouse) => Event::Mouse(*mouse),
            Step::Resize(width, height) => Event::Resize {
                width: *width,
                height: *height,
            },
            Step::Tick(by) => {
                now += by;
                Event::Tick(stamp(now))
            }
            Step::FilesChanged => Event::FilesChanged,
            Step::Load(files) => {
                last = files.clone();
                Event::Loaded(Box::new(data(files)))
            }
            Step::LoadSame => Event::Loaded(Box::new(data(&last))),
            Step::LoadFailed => Event::LoadFailed("cannot read shop/chores/c1.md".to_owned()),
            Step::Unpushed(numbers) => {
                if !oracle.unpushed_out {
                    continue;
                }
                oracle.unpushed_out = false;
                Event::Unpushed(match numbers {
                    Some(numbers) => Ok(numbers.iter().copied().map(BriefId::new).collect()),
                    None => Err("cannot read the unpushed commits".to_owned()),
                })
            }
            Step::Answer(answer) => {
                if !oracle.fetch_out {
                    continue;
                }
                oracle.fetch_out = false;
                match answer {
                    Answer::Busy => depth.busy += 1,
                    Answer::Ran { ok: false, .. } => depth.fetch_failed += 1,
                    Answer::Ran { ok: true, .. } => {}
                }
                Event::Fetched(fetched(answer))
            }
            Step::Wrote(ok) => {
                let Some(action) = oracle.writes.pop_front() else {
                    continue;
                };
                oracle.fetch_last = None;
                if *ok {
                    depth.wrote_ok += 1;
                    Event::Ran(action.clone(), Ok(action.to_string()))
                } else {
                    depth.wrote_failed += 1;
                    Event::Ran(action, Err("refused by the test".to_owned()))
                }
            }
            Step::Edited(how) => {
                if oracle.edits == 0 {
                    continue;
                }
                oracle.edits -= 1;
                Event::Edited(match how {
                    0 => Edited::Closed,
                    1 => Edited::Failed("the editor (vi) exited with 1".to_owned()),
                    _ => Edited::NotStarted("EDITOR is not set".to_owned()),
                })
            }
            Step::Note => Event::Note("waiting for lock".to_owned()),
            Step::InputGone => Event::InputGone,
        };
        let before = state.clone();
        let (after, commands) = state.step(event.clone());
        if matches!(event, Event::Unpushed(_))
            && before.items(Source::Open) != after.items(Source::Open)
        {
            depth.relabelled += 1;
        }
        judge(
            &before,
            &event,
            &after,
            &commands,
            &mut oracle,
            now,
            &mut depth,
        )?;
        depth.steps += 1;
        if commands.contains(&Command::Quit) {
            // The TUI has ended, before it draws this state again; the user
            // starts it again.
            depth.quits += 1;
            (state, oracle) = start(&last, now)?;
        } else {
            state = after;
        }
        check(&state, &mut depth)?;
        // The tabs are the files of the last load, labelled by the last set
        // of unpushed briefs, after every step: a load keeps the label, and
        // a set relabels.
        let mut want = data(&last);
        want.label(&state.unpushed);
        let tabs = |data: &Data| -> [Vec<String>; 2] {
            [&data.open, &data.filed]
                .map(|briefs| briefs.iter().map(|brief| brief.row.ls_line()).collect())
        };
        prop_assert_eq!(tabs(&state.data), tabs(&want));
    }
    Ok(depth)
}

/// T1, T2, T7, T8, and what T9 and T10 need of every state.
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
    // A question waits alone.
    if state.question.is_some() {
        prop_assert!(!state.typing && !state.help && !state.quitting);
    }
    // T10: quitting waits for something.
    prop_assert!(!state.quitting || !state.writes.is_empty());
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
        (state.filters.clone(), state.typing, state.question.clone()),
        (state.width, state.height, state.now),
        (
            state.data.clone(),
            state.sync.clone(),
            state.load_error.clone(),
        ),
        (
            state.writes.clone(),
            state.quitting,
            state.edit_wanted.clone(),
            state.messages.clone(),
        ),
    )
}

/// The keys of rows, in order: what a list shows of its order.
fn keys(rows: &[Row]) -> Vec<Option<ItemKey>> {
    rows.iter().map(|row| row.key.clone()).collect()
}

/// The id the CLI reads on a list row's line.
fn cli_id(kind: ListKind, row: &Row) -> Option<u32> {
    match crate::list::parse_line(kind, format!("- {}", row.text).as_bytes()) {
        crate::list::Entry::Item { id, .. } => id,
        _ => None,
    }
}

/// The oracle's account of an item after a reload: its own key, when the
/// reload kept it. `None` when it is gone.
fn followed(source: Source, after: &State, key: &ItemKey) -> Option<ItemKey> {
    all_rows(&after.data, source)
        .iter()
        .any(|row| row.key.as_ref() == Some(key))
        .then(|| key.clone())
}

/// T3 to T6, T9 to T12: what holds of every step, and that `J`/`K` do
/// nothing.
fn judge(
    before: &State,
    event: &Event,
    after: &State,
    commands: &[Command],
    oracle: &mut Oracle,
    now: u64,
    depth: &mut Depth,
) -> Result<(), TestCaseError> {
    let key = match event {
        Event::Key(key) => Some(*key),
        _ => None,
    };
    let mouse = match event {
        Event::Mouse(mouse) => Some(*mouse),
        _ => None,
    };
    // T12: a mouse event while a key is waited for does nothing at all.
    let waited = before.quitting || before.typing || before.question.is_some() || before.help;
    let acts = mouse.is_some() && !waited;
    if mouse.is_some() && waited {
        prop_assert!(commands.is_empty(), "{commands:?} after {event:?}");
        prop_assert_eq!(untouched(after), untouched(before));
        prop_assert_eq!(&after.cursors, &before.cursors);
        prop_assert_eq!(
            (after.pending_g, &after.pending_y),
            (before.pending_g, &before.pending_y)
        );
        depth.mouse_waited += 1;
    }
    // Where the frame drawn before it shows the click.
    let drawn = match mouse {
        Some(Mouse::Click { column, row }) if acts => drawn_hit(before, column, row),
        _ => Drawn::Unknown,
    };
    let quit_key = matches!(key, Some(Key::Ctrl('c')))
        || (key == Some(Key::Char('q'))
            && (before.quitting || (!before.typing && before.question.is_none())));
    // A key acts as itself: nothing before it takes it.
    let plain = key.is_some()
        && !quit_key
        && !before.quitting
        && !before.typing
        && before.question.is_none()
        && !before.help;
    // `yy`, by the keys this trace sent, not by the state's own reading: a
    // `y` right after a `y` on the same item, with no key between.
    let cursor_key = before.cursor_item().and_then(|(_, row)| row.key);
    let whole = plain
        && key == Some(Key::Char('y'))
        && oracle.y_armed.is_some()
        && oracle.y_armed == cursor_key;
    if key.is_some() {
        oracle.y_armed = (plain && key == Some(Key::Char('y')) && !whole)
            .then(|| cursor_key.clone())
            .flatten();
    }
    if acts {
        oracle.y_armed = None;
    }

    // T3
    let moved = match (key, mouse) {
        (Some(key), _) if navigates(key) && plain && !before.in_preview => {
            Some((before.source(), key))
        }
        // The wheel is `k` and `j`.
        (_, Some(Mouse::WheelUp)) if acts && !before.in_preview => {
            Some((before.source(), Key::Char('k')))
        }
        (_, Some(Mouse::WheelDown)) if acts && !before.in_preview => {
            Some((before.source(), Key::Char('j')))
        }
        _ => None,
    };
    if acts && matches!(mouse, Some(Mouse::WheelUp | Mouse::WheelDown)) {
        depth.wheeled += 1;
    }
    // T12: the click's item, drawn on the row clicked, gets its panel's
    // cursor; the rest of T3 holds for every other list.
    let clicked = match &drawn {
        Drawn::Panel(panel, Some(item)) => Some((before.source_of(*panel), item.clone())),
        _ => None,
    };
    for source in Source::ALL {
        let Some(item) = before.cursor(source).key.clone() else {
            continue;
        };
        let item = &item;
        let items = after.items(source);
        if let Some((on, clicked)) = &clicked
            && *on == source
            && items.contains(clicked)
        {
            prop_assert_eq!(
                after.cursor(source).key.as_ref(),
                Some(clicked),
                "{:?}",
                source
            );
            if clicked != item {
                depth.click_selected += 1;
            }
            continue;
        }
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
            _ => match items.iter().position(|shown| shown == item) {
                Some(at) => {
                    prop_assert_eq!(
                        after.cursor(source).key.as_ref(),
                        Some(item),
                        "{:?}",
                        source
                    );
                    if matches!(event, Event::Loaded(_) | Event::Unpushed(_))
                        && at != before.cursor(source).at
                    {
                        depth.kept_on_moved_item += 1;
                    }
                }
                None => {
                    if matches!(event, Event::Loaded(_) | Event::Unpushed(_))
                        && after.cursor(source).key.is_some()
                    {
                        depth.item_gone += 1;
                    }
                }
            },
        }
    }

    // T4: each command from its event.
    let pressed = |c: char| plain && key == Some(Key::Char(c));
    for command in commands {
        match command {
            Command::Load => {
                prop_assert!(matches!(event, Event::FilesChanged | Event::Ran(..)));
            }
            Command::Fetch => prop_assert!(matches!(event, Event::Tick(_))),
            // At each load and about once a minute: below, with its timing.
            Command::Unpushed => {}
            Command::Quit => {
                prop_assert!(
                    quit_key || matches!(event, Event::InputGone | Event::Ran(..)),
                    "quit after {event:?}"
                );
            }
            Command::Run(Action::Sync) => {
                prop_assert!(
                    pressed('r')
                        || matches!(event, Event::Edited(Edited::Closed | Edited::Failed(_))),
                    "sync after {event:?}"
                );
                depth.syncs += 1;
            }
            // T9, below.
            Command::Run(Action::Done(_) | Action::Drop(_)) => {}
            Command::Edit { .. } => {
                prop_assert!(pressed('e') || matches!(event, Event::Loaded(_)));
            }
            Command::Yank(text) => {
                prop_assert!(pressed('y') || pressed('Y'), "yank after {event:?}");
                // `yy`: the whole item; the trace's items have no body, so
                // an item is its line, a brief its file.
                if pressed('y')
                    && whole
                    && let Some((source, row)) = before.cursor_item()
                {
                    let want = match source.list() {
                        Some(_) => format!("{}\n", row.text),
                        None => {
                            String::from_utf8_lossy(&before.brief(source, &row).text).into_owned()
                        }
                    };
                    prop_assert_eq!(text, &want);
                    depth.whole_yanks += 1;
                } else if pressed('y')
                    && let Some((source, row)) = before.cursor_item()
                    && let Some(kind) = source.list()
                {
                    // The id the CLI reads on the line, as the line has it.
                    prop_assert_eq!(kind.parse_id(text.as_bytes()), cli_id(kind, &row));
                    prop_assert!(cli_id(kind, &row).is_some());
                    prop_assert!(row.text.starts_with(&format!("{text} ")), "{:?}", row);
                } else if pressed('y')
                    && let Some((source, row)) = before.cursor_item()
                {
                    // A brief's id: its number.
                    prop_assert_eq!(text, &before.brief(source, &row).row.number.to_string());
                }
                depth.yanks += 1;
            }
        }
    }
    match event {
        Event::Key(Key::Esc) => prop_assert!(commands.is_empty(), "{commands:?} after Esc"),
        Event::Key(key) if navigates(*key) => {
            prop_assert!(commands.is_empty(), "{commands:?} after {key:?}");
        }
        Event::Key(Key::Char('J' | 'K')) => {
            prop_assert!(commands.is_empty(), "{commands:?} after {event:?}");
        }
        // A click or the wheel gives no command.
        Event::Mouse(_) => {
            prop_assert!(commands.is_empty(), "{commands:?} after {event:?}");
        }
        // The question of what is unpushed goes with it, unless one is out.
        Event::FilesChanged => {
            let ask: &[Command] = if before.unpushed_out {
                &[]
            } else {
                &[Command::Unpushed]
            };
            let want = [&[Command::Load][..], ask].concat();
            prop_assert_eq!(commands, want.as_slice());
        }
        // A write's answer reads the files at once.
        Event::Ran(..) => prop_assert_eq!(commands.first(), Some(&Command::Load)),
        // What the editor saved is committed by a sync, if one ran.
        Event::Edited(Edited::Closed | Edited::Failed(_)) => {
            prop_assert_eq!(commands, [Command::Run(Action::Sync)].as_slice());
        }
        Event::Edited(Edited::NotStarted(_)) => prop_assert!(commands.is_empty()),
        Event::InputGone => prop_assert!(after.quitting || commands.contains(&Command::Quit)),
        Event::Resize { .. } | Event::LoadFailed(_) | Event::Fetched(_) | Event::Note(_) => {
            prop_assert!(commands.is_empty(), "{commands:?} after {event:?}");
        }
        _ => {}
    }
    // `J`/`K`: no key of the TUI's. Pressed as a key, they change what any
    // unrelated key changes (the messages, a pending `g` or `y`, an edit
    // waited for) and nothing of the lists: the rows and the cursors stay.
    if plain && matches!(key, Some(Key::Char('J' | 'K'))) {
        for source in Source::ALL {
            prop_assert_eq!(keys(&after.rows(source)), keys(&before.rows(source)));
        }
        prop_assert_eq!(&after.cursors, &before.cursors);
        prop_assert_eq!(after.writes.len(), before.writes.len());
        if before
            .cursor_item()
            .is_some_and(|(source, _)| source.list().is_some())
        {
            depth.upper_jk += 1;
        }
    }

    // T5
    if let Event::Key(Key::Esc) = event {
        let mut want = before.clone();
        let focus = before.focus as usize;
        if before.quitting {
            // Waiting for the writes: no key but quit does anything.
            depth.esc_nothing += 1;
        } else if before.typing {
            want.typing = false;
            want.filters[focus] = None;
            depth.esc_typing += 1;
        } else if before.question.is_some() {
            want.question = None;
            depth.esc_question += 1;
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
    let fetch = commands
        .iter()
        .filter(|command| **command == Command::Fetch)
        .count();
    if let Event::Tick(_) = event {
        let due = !oracle.fetch_out
            && oracle
                .fetch_last
                .is_none_or(|last| now - last >= FETCH_EVERY);
        prop_assert_eq!(
            fetch,
            usize::from(due),
            "out {}, last {:?}, now {}",
            oracle.fetch_out,
            oracle.fetch_last,
            now
        );
        if due {
            oracle.fetch_out = true;
            oracle.fetch_last = Some(now);
            depth.fetches += 1;
        }
    }

    // The question of what is unpushed: one at a time, at a load and
    // about once a minute, apart from the fetch.
    let asked = commands
        .iter()
        .filter(|command| **command == Command::Unpushed)
        .count();
    prop_assert!(asked <= 1, "{commands:?}");
    if asked == 1 {
        prop_assert!(!oracle.unpushed_out, "asked again while out: {event:?}");
        prop_assert!(matches!(
            event,
            Event::FilesChanged | Event::Ran(..) | Event::Tick(_)
        ));
        oracle.unpushed_out = true;
        oracle.unpushed_last = now;
    } else if let Event::Tick(_) = event {
        prop_assert!(
            oracle.unpushed_out || now - oracle.unpushed_last < FETCH_EVERY,
            "not asked at {now}"
        );
    }

    // T9: a done or a drop only by `y`, to the question that asked it.
    for command in commands {
        if let Command::Run(action @ (Action::Done(_) | Action::Drop(_))) = command {
            prop_assert!(
                key == Some(Key::Char('y'))
                    && !before.quitting
                    && before.question.as_ref().map(|asked| &asked.action) == Some(action),
                "{action:?} after {event:?}, asked {:?}",
                before.question
            );
            prop_assert_eq!(commands.len(), 1);
            depth.confirmed += 1;
        }
    }
    if let Some(question) = &after.question
        && before.question.as_ref() != Some(question)
    {
        let (ask, kind) = match question.action {
            Action::Done(_) => ('d', ListKind::Chores),
            Action::Drop(_) => ('x', ListKind::Backlog),
            Action::Sync => panic!("asked {question:?}"),
        };
        prop_assert!(pressed(ask), "asked {question:?} after {event:?}");
        let (source, row) = before.cursor_item().expect("asked about an item");
        prop_assert_eq!(source.list(), Some(kind));
        let number = match question.action {
            Action::Done(id) => id.get(),
            Action::Drop(id) => id.get(),
            Action::Sync => unreachable!(),
        };
        // The id the CLI reads on the line: the command names this item.
        prop_assert_eq!(cli_id(kind, &row), Some(number), "{:?}", row);
        depth.asked += 1;
    }
    if before.question.is_some()
        && after.question.is_none()
        && !matches!(key, Some(Key::Char('y') | Key::Esc))
        && !quit_key
    {
        depth.declined += 1;
    }

    // T10: the state knows what is out, as the worker does.
    for command in commands {
        match command {
            Command::Run(action) => oracle.writes.push_back(action.clone()),
            Command::Edit { .. } => oracle.edits += 1,
            _ => {}
        }
    }
    prop_assert_eq!(&after.writes, &oracle.writes);
    if commands.contains(&Command::Quit) {
        if oracle.writes.is_empty() {
            if before.quitting {
                depth.quit_after_writes += 1;
            }
        } else {
            prop_assert!(
                before.quitting && quit_key,
                "quit with {:?} out",
                oracle.writes
            );
            depth.quit_forced += 1;
        }
    }
    if before.quitting && matches!(event, Event::Ran(..)) && after.writes.is_empty() {
        prop_assert!(commands.contains(&Command::Quit));
    }
    if after.quitting && !before.quitting {
        depth.quit_waited += 1;
    }

    // T11: the editor, on what `e` was pressed on, with no write out; an
    // edit waited for opens at the load after the last write, unless its
    // item is gone (or a filter hides it).
    if let (Event::Loaded(_), Some((source, item))) = (event, &before.edit_wanted)
        && after.writes.is_empty()
    {
        let there = followed(*source, after, item).is_some_and(|found| {
            after
                .rows(*source)
                .iter()
                .any(|row| row.key == Some(found.clone()))
        });
        let opened = commands
            .iter()
            .any(|command| matches!(command, Command::Edit { .. }));
        prop_assert_eq!(opened, there, "edit of {:?} waited for", item);
    }
    // T11: a key but Esc calls a wait off; only `e` asks for a new one, on
    // the item under the cursor.
    if let Some(key) = key
        && key != Key::Esc
        && let Some((source, item)) = &after.edit_wanted
    {
        prop_assert!(pressed('e'), "{:?} kept the wait for {:?}", key, item);
        let (on, row) = before.cursor_item().expect("e on an item");
        prop_assert_eq!((on, row.key.as_ref()), (*source, Some(item)));
    }
    if acts {
        prop_assert!(after.edit_wanted.is_none(), "{event:?} kept the wait");
    }
    if before.edit_wanted.is_some()
        && (acts || key.is_some_and(|key| key != Key::Esc && !pressed('e')))
    {
        depth.edit_called_off += 1;
    }
    for command in commands {
        if let Command::Edit { path, line } = command {
            prop_assert!(
                oracle.writes.is_empty(),
                "edit with {:?} out",
                oracle.writes
            );
            let (source, item) = if pressed('e') {
                let (source, row) = before.cursor_item().expect("e on an item");
                (source, row.key.expect("an item"))
            } else {
                depth.edits_waited += 1;
                before.edit_wanted.clone().expect("an edit was waited for")
            };
            let row = after
                .rows(source)
                .into_iter()
                .find(|row| row.key.as_ref() == Some(&item))
                .expect("the item is there");
            prop_assert_eq!(*line, None);
            match source.list() {
                None => prop_assert_eq!(path, &after.brief(source, &row).path),
                // The item's own file, by the id on its row.
                Some(kind) => {
                    let id = cli_id(kind, &row).expect("a row with an id");
                    prop_assert_eq!(
                        path.to_string(),
                        format!("shop/{}/{}{id}.md", kind.dir_name(), kind.prefix())
                    );
                }
            }
            depth.edits += 1;
        }
    }

    // T12: the focus goes where the frame drew the click.
    if acts {
        prop_assert!(!after.pending_g && after.pending_y.is_none());
        match &drawn {
            Drawn::Panel(panel, _) => {
                prop_assert_eq!((after.focus, after.in_preview), (*panel, false));
                if *panel != before.focus || before.in_preview {
                    depth.click_focused += 1;
                }
            }
            Drawn::Preview => {
                // As `0`: with an item under the cursor, in the preview;
                // without, the focus stays.
                let item = after.cursor(after.source()).key.is_some();
                prop_assert_eq!(after.focus, before.focus);
                prop_assert_eq!(after.in_preview, before.in_preview || item);
                if item && !before.in_preview {
                    depth.click_previewed += 1;
                }
            }
            Drawn::Nothing => {
                prop_assert_eq!(
                    (after.focus, after.in_preview),
                    (before.focus, before.in_preview)
                );
            }
            Drawn::Unknown if matches!(mouse, Some(Mouse::Click { .. })) => {}
            Drawn::Unknown => {
                // The wheel: the focus stays.
                prop_assert_eq!(
                    (after.focus, after.in_preview),
                    (before.focus, before.in_preview)
                );
            }
        }
    }
    Ok(())
}

/// Where a click lands, read off the frame the state drew.
#[derive(Debug)]
enum Drawn {
    /// A list panel's box, and the item on the row clicked inside it.
    Panel(Panel, Option<ItemKey>),
    Preview,
    /// Outside every box, with all four found.
    Nothing,
    /// A box was not found whole (a screen too small for it): not judged.
    Unknown,
}

/// The pane drawn under a click, by the boxes in the frame: each found by
/// its title (`╭─[1]`, the jump key the view writes) and its corners, not
/// by the state's areas. The row inside a list panel's box is the drawn
/// line: the panel's rows from its offset, as the view draws them.
fn drawn_hit(state: &State, column: u16, row: u16) -> Drawn {
    let mut terminal = Terminal::new(TestBackend::new(state.width, state.height)).unwrap();
    terminal.draw(|frame| view::draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer();
    let (width, height) = (state.width, state.height);
    let cell = |x: u16, y: u16| buffer[(x, y)].symbol().to_owned();
    // The box titled `[key]`: its top-left and bottom-right corners.
    let find = |key: char| -> Option<(u16, u16, u16, u16)> {
        let title = ["╭", "─", "[", &key.to_string(), "]"];
        for y in 0..height {
            for x in 0..width.saturating_sub(4) {
                if (0..5).all(|i| cell(x + i, y) == title[usize::from(i)]) {
                    let right = (x + 1..width).find(|&x| cell(x, y) == "╮")?;
                    let bottom = (y + 1..height).find(|&y| cell(x, y) == "╰")?;
                    return Some((x, y, right, bottom));
                }
            }
        }
        None
    };
    let boxes = ['1', '2', '3', '0'].map(find);
    let inside = |(left, top, right, bottom): (u16, u16, u16, u16)| {
        (left..=right).contains(&column) && (top..=bottom).contains(&row)
    };
    for (panel, found) in Panel::ALL.into_iter().zip(boxes) {
        if let Some(found @ (left, top, right, bottom)) = found
            && inside(found)
        {
            let item = (column > left && column < right && row > top && row < bottom)
                .then(|| {
                    let source = state.source_of(panel);
                    let line = state.offset(source) + usize::from(row - top - 1);
                    state.rows(source).into_iter().nth(line)?.key
                })
                .flatten();
            return Drawn::Panel(panel, item);
        }
    }
    match boxes[3] {
        Some(found) if inside(found) => Drawn::Preview,
        _ if boxes.iter().all(Option::is_some) => Drawn::Nothing,
        _ => Drawn::Unknown,
    }
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
    let reached = [
        ("steps", depth.steps > 64 * 30),
        ("kept_on_moved_item", depth.kept_on_moved_item > 0),
        ("item_gone", depth.item_gone > 0),
        ("relabelled", depth.relabelled > 0),
        ("on_duplicate", depth.on_duplicate > 0),
        ("esc_typing", depth.esc_typing > 0),
        ("esc_question", depth.esc_question > 0),
        ("esc_help", depth.esc_help > 0),
        ("esc_preview", depth.esc_preview > 0),
        ("esc_preview_filtered", depth.esc_preview_filtered > 0),
        ("esc_help_over_preview", depth.esc_help_over_preview > 0),
        ("esc_filter", depth.esc_filter > 0),
        ("esc_nothing", depth.esc_nothing > 0),
        ("filtered", depth.filtered > 0),
        ("fetches", depth.fetches > 0),
        ("busy", depth.busy > 0),
        ("fetch_failed", depth.fetch_failed > 0),
        ("preview_scrolled", depth.preview_scrolled > 0),
        ("panel_scrolled", depth.panel_scrolled > 0),
        ("filed_tab", depth.filed_tab > 0),
        ("quits", depth.quits > 0),
        ("asked", depth.asked > 0),
        ("confirmed", depth.confirmed > 0),
        ("declined", depth.declined > 0),
        ("edits", depth.edits > 0),
        ("edits_waited", depth.edits_waited > 0),
        ("edit_called_off", depth.edit_called_off > 0),
        ("yanks", depth.yanks > 0),
        ("whole_yanks", depth.whole_yanks > 0),
        ("syncs", depth.syncs > 0),
        ("wrote_ok", depth.wrote_ok > 0),
        ("wrote_failed", depth.wrote_failed > 0),
        ("quit_waited", depth.quit_waited > 0),
        ("quit_after_writes", depth.quit_after_writes > 0),
        ("quit_forced", depth.quit_forced > 0),
        ("click_focused", depth.click_focused > 0),
        ("click_selected", depth.click_selected > 0),
        ("click_previewed", depth.click_previewed > 0),
        ("mouse_waited", depth.mouse_waited > 0),
        ("wheeled", depth.wheeled > 0),
        ("upper_jk", depth.upper_jk > 0),
    ];
    let missed: Vec<&str> = reached
        .iter()
        .filter(|(_, reached)| !reached)
        .map(|(name, _)| *name)
        .collect();
    assert!(missed.is_empty(), "not reached: {missed:?} in {depth:?}");
}

/// Where the focus is after each key, and whether in the preview.
fn focus_along(keys: &[Key]) -> Vec<(Panel, bool)> {
    focus_along_in(
        &Files {
            open: vec![(1, None, 1)],
            filed: Vec::new(),
            chores: vec![(0, 1, 0)],
            backlog: vec![(0, 1, 0)],
        },
        keys,
    )
}

fn focus_along_in(files: &Files, keys: &[Key]) -> Vec<(Panel, bool)> {
    let (mut state, _) = start(files, 1_000_000).unwrap();
    let mut seen = Vec::new();
    for key in keys {
        state = state.step(Event::Key(*key)).0;
        seen.push((state.focus(), state.in_preview()));
    }
    seen
}

#[test]
fn shift_tab_walks_the_panels_backwards() {
    assert_eq!(
        focus_along(&[Key::BackTab, Key::BackTab, Key::BackTab]),
        [
            (Panel::Backlog, false),
            (Panel::Chores, false),
            (Panel::Briefs, false),
        ]
    );
}

/// Unlike `h`, which only leaves the preview, Shift-Tab goes on: it mirrors
/// Tab.
#[test]
fn shift_tab_leaves_the_preview_for_the_previous_panel() {
    assert_eq!(
        focus_along(&[Key::Char('2'), Key::Enter, Key::BackTab]),
        [
            (Panel::Chores, false),
            (Panel::Chores, true),
            (Panel::Briefs, false),
        ]
    );
}

/// `0` is Enter's twin: into the preview from each panel, with an item
/// under the cursor.
#[test]
fn zero_enters_the_preview_from_each_panel() {
    use Key::Char;
    assert_eq!(
        focus_along(&[Char('0'), Char('2'), Char('0'), Char('3'), Char('0')]),
        [
            (Panel::Briefs, true),
            (Panel::Chores, false),
            (Panel::Chores, true),
            (Panel::Backlog, false),
            (Panel::Backlog, true),
        ]
    );
}

/// Without an item under the cursor `0` does nothing, as Enter: an empty
/// panel. In the preview it does nothing either.
#[test]
fn zero_needs_an_item_and_does_nothing_in_the_preview() {
    use Key::Char;
    let files = Files {
        open: vec![(1, None, 1)],
        filed: Vec::new(),
        chores: Vec::new(),
        backlog: Vec::new(),
    };
    assert_eq!(
        focus_along_in(&files, &[Char('2'), Char('0'), Char('3'), Char('0')]),
        [
            (Panel::Chores, false),
            (Panel::Chores, false),
            (Panel::Backlog, false),
            (Panel::Backlog, false),
        ]
    );
    assert_eq!(
        focus_along(&[Char('0'), Char('0')]),
        [(Panel::Briefs, true), (Panel::Briefs, true)]
    );
}

/// While a filter is typed, `0` is filter text.
#[test]
fn zero_typed_into_a_filter_is_filter_text() {
    let files = Files {
        open: vec![(1, None, 1)],
        filed: Vec::new(),
        chores: Vec::new(),
        backlog: Vec::new(),
    };
    let (state, _) = start(&files, 1_000_000).unwrap();
    let state = [Key::Char('/'), Key::Char('1'), Key::Char('0')]
        .into_iter()
        .fold(state, |state, key| state.step(Event::Key(key)).0);
    assert_eq!(state.filter(Panel::Briefs), Some("10"));
    assert!(!state.in_preview());
}

/// The yanks each key gives, on a board with brief 1 and two chores: c1,
/// and c2 without a check.
fn yanks_along(keys: &[Key]) -> Vec<Vec<String>> {
    let files = Files {
        open: vec![(1, Some(Status::Queued), 0)],
        filed: Vec::new(),
        chores: vec![(0, 1, 0), (0, 2, 2)],
        backlog: Vec::new(),
    };
    let (mut state, _) = start(&files, 1_000_000).unwrap();
    let mut seen = Vec::new();
    for key in keys {
        let commands;
        (state, commands) = state.step(Event::Key(*key));
        seen.push(
            commands
                .into_iter()
                .filter_map(|command| match command {
                    Command::Yank(text) => Some(text),
                    _ => None,
                })
                .collect(),
        );
    }
    seen
}

/// `y` the id at once; a `y` as the very next key the whole item, a third
/// the id again; any key between starts over.
#[test]
fn yy_yanks_the_whole_item_and_y_still_the_id() {
    use Key::Char;
    let whole = "id: 1\nstatus: queued\n\n# Brief 1\n## Task\n".to_owned();
    let id = "1".to_owned();
    assert_eq!(
        yanks_along(&[Char('y'), Char('y'), Char('y')]),
        [vec![id.clone()], vec![whole.clone()], vec![id.clone()]]
    );
    assert_eq!(
        yanks_along(&[Char('y'), Char('j'), Char('y')]),
        [vec![id.clone()], vec![], vec![id.clone()]]
    );
    assert_eq!(
        yanks_along(&[Char('y'), Key::Esc, Char('y'), Char('Y'), Char('y')]),
        [
            vec![id.clone()],
            vec![],
            vec![id.clone()],
            vec!["1 queued s1".to_owned()],
            vec![id]
        ]
    );
    // A chore: its id, then what `show` prints.
    assert_eq!(
        yanks_along(&[
            Char('2'),
            Char('y'),
            Char('y'),
            Char('j'),
            Char('y'),
            Char('y')
        ]),
        [
            vec![],
            vec!["c1".to_owned()],
            vec!["c1 chore v0. Check: it is done\n".to_owned()],
            vec![],
            vec!["c2".to_owned()],
            vec!["c2 chore v2\n".to_owned()],
        ]
    );
}
