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
//!   (a reload, a filter, a resize, a tab, a move), unless a key moved it;
//!   one that moved lands where its key says; an item typed by hand that a
//!   sync gave an id is the same item, here and in T10 and T12;
//! - T4 every command comes from the event that gives it, Esc gives none,
//!   navigation none of its own (the move its key ends, as any other key
//!   but Esc ends it), and a file change is one load;
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
//! - T10 a run of `J`/`K` ends in exactly one `board move`, the one that
//!   puts the item where the screen showed it, or is dropped with a note
//!   (its item or neighbour gone after a reload, or no neighbour with an id
//!   to name), or came back where it began; it is kept across a reload
//!   that keeps both, and while it runs the cursor is on its item;
//! - T11 the state knows the writes out, in order; quit waits for them
//!   (a second quit does not), and comes when the last one has answered;
//! - T12 the editor never opens while a write is out, and on the item `e`
//!   was pressed on; one waited for opens at the load after the last
//!   write, unless its item is gone, and any key but Esc calls it off
//!   (only `e` asks again).

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
    chores: Vec<ChoreLine>,
    /// Id, and a variant of the text: the same item edited.
    backlog: Vec<(Option<u32>, u8)>,
}

#[derive(Debug, Clone)]
enum ChoreLine {
    Group(u8),
    Item(Option<u32>, u8),
    /// `c<id><tab>…`: an item without an id, to the CLI (`list::parse_line`).
    Tabbed(u32, u8),
}

#[derive(Debug, Clone)]
enum Step {
    Key(Key),
    Resize(u16, u16),
    /// The clock, this many seconds on.
    Tick(u64),
    FilesChanged,
    Load(Files),
    /// The files as the last load had them: what a write that changed
    /// another project's files, or nothing, gives.
    LoadSame,
    /// The files as the last load had them, with an id given to every item
    /// typed by hand ([`assigned`]): what a sync gives.
    LoadAssigned,
    LoadFailed,
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
/// adds some, and a hand-typed duplicate comes up.
fn files() -> impl Strategy<Value = Files> {
    let chore = prop_oneof![
        1 => (0u8..3).prop_map(ChoreLine::Group),
        4 => (prop::option::weighted(0.85, 1u32..8), 0u8..2)
            .prop_map(|(id, variant)| ChoreLine::Item(id, variant)),
        1 => (1u32..8, 0u8..2).prop_map(|(id, variant)| ChoreLine::Tabbed(id, variant)),
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
        Char('2'),
        Char('3'),
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
        3 => Just(Step::LoadAssigned),
        10 => (0u64..90).prop_map(Step::Tick),
        4 => Just(Step::FilesChanged),
        8 => answer().prop_map(Step::Answer),
        8 => any::<bool>().prop_map(Step::Wrote),
        3 => (0u8..3).prop_map(Step::Edited),
        2 => Just(Step::Note),
        3 => (60u16..=200, 10u16..=50).prop_map(|(w, h)| Step::Resize(w, h)),
        1 => (0u16..60, 0u16..10).prop_map(|(w, h)| Step::Resize(w, h)),
        2 => Just(Step::LoadFailed),
        1 => Just(Step::InputGone),
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

/// A run of `J`/`K` in a list, sometimes with a reload or Esc in it, and
/// the key that ends it.
fn moved() -> impl Strategy<Value = Vec<Step>> {
    use Key::{Char, Esc};
    let press = prop_oneof![
        8 => prop::sample::select(vec![Char('J'), Char('K')]).prop_map(Step::Key),
        1 => Just(Step::Key(Esc)),
        1 => Just(Step::LoadSame),
        1 => Just(Step::LoadAssigned),
        1 => files().prop_map(Step::Load),
    ];
    (
        prop::sample::select(vec!['2', '3']),
        0usize..4,
        prop::collection::vec(press, 1..8),
        prop::sample::select(vec![
            Char('j'),
            Char('k'),
            Char('r'),
            Char('e'),
            Char('y'),
            Char('d'),
            Char('?'),
            Char('1'),
            Char('q'),
        ]),
    )
        .prop_map(|(panel, down, presses, end)| {
            let mut steps = vec![Step::Key(Char(panel))];
            steps.extend((0..down).map(|_| Step::Key(Char('j'))));
            steps.extend(presses);
            steps.push(Step::Key(end));
            steps
        })
}

/// A run of `J`/`K` that puts a backlog item next to one typed by hand,
/// and a sync that gives that one an id while the run goes on: random
/// files and keys seldom line the three up. Esc first, enough to close
/// whatever is open, and the cursor from the top.
fn moved_next_to_hand_typed() -> impl Strategy<Value = Vec<Step>> {
    use Key::{Char, Esc};
    (
        files(),
        0u8..2,
        prop::sample::select(vec![Char('j'), Char('r'), Char('q'), Char('1')]),
    )
        .prop_map(|(mut files, variant, end)| {
            // b1 on one line only, so a command can name it.
            files.backlog.retain(|(id, _)| *id != Some(1));
            files
                .backlog
                .splice(0..0, [(None, variant), (Some(1), variant)]);
            let mut steps = vec![Step::Load(files)];
            steps.extend([Esc, Esc, Esc, Esc, Esc].map(Step::Key));
            steps.extend([Char('3'), Char('g'), Char('g'), Char('j'), Char('K')].map(Step::Key));
            steps.extend([Step::LoadAssigned, Step::Key(end)]);
            steps
        })
}

/// `e` on a backlog item typed by hand while a sync is out: the editor
/// waits for the load after it, which gives the item an id.
fn edited_hand_typed() -> impl Strategy<Value = Vec<Step>> {
    use Key::{Char, Esc};
    (files(), 0u8..2, any::<bool>(), 0u8..3).prop_map(|(mut files, variant, ok, how)| {
        files.backlog.insert(0, (None, variant));
        let mut steps = vec![Step::Load(files)];
        steps.extend([Esc, Esc, Esc, Esc, Esc].map(Step::Key));
        steps.extend([Char('3'), Char('g'), Char('g'), Char('r'), Char('e')].map(Step::Key));
        steps.extend([Step::Wrote(ok), Step::LoadAssigned, Step::Edited(how)]);
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
        prop::sample::select(vec![Step::LoadSame, Step::LoadAssigned]),
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

fn trace() -> impl Strategy<Value = (Files, Vec<Step>)> {
    let steps = prop_oneof![
        30 => step().prop_map(|step| vec![step]),
        1 => filter_typed(),
        3 => asked(),
        4 => moved(),
        1 => edit_waits(),
        1 => moved_next_to_hand_typed(),
        1 => edited_hand_typed(),
        1 => previewed(),
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

/// An item's id as its line starts with it: the line of one without an id,
/// with `<id> ` in front, as a sync gives one.
fn spelled(prefix: char, id: Option<u32>) -> String {
    id.map(|id| format!("{prefix}{id} ")).unwrap_or_default()
}

/// The files after a sync gave every item without an id one, in file
/// order, from past the highest; the lines whose id was typed with a tab
/// are left as they are, for brevity.
fn assigned(files: &Files) -> Files {
    let mut files = files.clone();
    let mut next = files
        .chores
        .iter()
        .filter_map(|line| match line {
            ChoreLine::Item(id, _) => *id,
            ChoreLine::Tabbed(id, _) => Some(*id),
            ChoreLine::Group(_) => None,
        })
        .max()
        .unwrap_or(0);
    for line in &mut files.chores {
        if let ChoreLine::Item(id @ None, _) = line {
            next += 1;
            *id = Some(next);
        }
    }
    let mut next = files
        .backlog
        .iter()
        .filter_map(|(id, _)| *id)
        .max()
        .unwrap_or(0);
    for (id, _) in &mut files.backlog {
        if id.is_none() {
            next += 1;
            *id = Some(next);
        }
    }
    files
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
        chores: ListLine::numbered(
            ListKind::Chores,
            files
                .chores
                .iter()
                .map(|line| match line {
                    ChoreLine::Group(group) => {
                        ListRow::Group(format!("group {group}").into_bytes())
                    }
                    ChoreLine::Item(id, variant) => ListRow::Item(
                        format!("{}chore v{variant}. Check: it is done", spelled('c', *id))
                            .into_bytes(),
                    ),
                    ChoreLine::Tabbed(id, variant) => ListRow::Item(
                        format!("c{id}\tchore v{variant}. Check: it is done").into_bytes(),
                    ),
                })
                .collect(),
        ),
        backlog: ListLine::numbered(
            ListKind::Backlog,
            files
                .backlog
                .iter()
                .map(|(id, variant)| {
                    ListRow::Item(
                        format!("{}backlog item ü v{variant}", spelled('b', *id)).into_bytes(),
                    )
                })
                .collect(),
        ),
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
    esc_question: u32,
    esc_help: u32,
    esc_preview: u32,
    /// Esc left the preview over a filtered panel: the filter stays.
    esc_preview_filtered: u32,
    /// Esc closed help over the preview: the preview stays.
    esc_help_over_preview: u32,
    esc_filter: u32,
    /// Esc during a run of `J`/`K`: the run goes on.
    esc_moving: u32,
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
    moves_sent: u32,
    /// A move sent whose item went past a heading, into another group.
    moved_to_other_group: u32,
    /// A move sent while another write was out.
    moved_behind_a_write: u32,
    dropped_on_reload: u32,
    dropped_unnamed: u32,
    kept_on_reload: u32,
    /// A run kept across a reload whose sync gave its neighbour an id.
    kept_next_to_followed: u32,
    came_back: u32,
    edits: u32,
    /// An edit that waited for the writes out.
    edits_waited: u32,
    /// An edit waited for on an item that the load after gave an id.
    edit_followed: u32,
    /// A key but Esc or `e` called off an edit waited for.
    edit_called_off: u32,
    /// A cursor kept on an item a reload gave an id.
    followed: u32,
    /// An action key on a line whose id was typed with a tab: no id, to
    /// the CLI.
    on_tab_typed_id: u32,
    yanks: u32,
    syncs: u32,
    wrote_ok: u32,
    wrote_failed: u32,
    /// A quit that waited for writes, and the quit that came after them.
    quit_waited: u32,
    quit_after_writes: u32,
    quit_forced: u32,
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
            on_duplicate,
            esc_typing,
            esc_question,
            esc_help,
            esc_preview,
            esc_preview_filtered,
            esc_help_over_preview,
            esc_filter,
            esc_moving,
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
            moves_sent,
            moved_to_other_group,
            moved_behind_a_write,
            dropped_on_reload,
            dropped_unnamed,
            kept_on_reload,
            kept_next_to_followed,
            came_back,
            edits,
            edits_waited,
            edit_followed,
            edit_called_off,
            followed,
            on_tab_typed_id,
            yanks,
            syncs,
            wrote_ok,
            wrote_failed,
            quit_waited,
            quit_after_writes,
            quit_forced
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
}

impl Oracle {
    /// A TUI just started: its first fetch is out.
    fn new(now: u64) -> Oracle {
        Oracle {
            fetch_out: true,
            fetch_last: Some(now),
            writes: VecDeque::new(),
            edits: 0,
        }
    }
}

/// A TUI started at `now` on these files.
fn start(files: &Files, now: u64) -> Result<(State, Oracle), TestCaseError> {
    let (state, commands) = State::new(Data::empty(shop()), 160, 40, stamp(now));
    prop_assert_eq!(commands, vec![Command::Load, Command::Fetch]);
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
            Step::LoadAssigned => {
                last = assigned(&last);
                Event::Loaded(Box::new(data(&last)))
            }
            Step::LoadFailed => Event::LoadFailed("cannot read shop/chores.md".to_owned()),
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
    }
    Ok(depth)
}

/// T1, T2, T7, T8, and what T9 to T11 need of every state.
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
    // T10: a run is on the item under the focused cursor, with no filter
    // hiding its neighbours; a question waits alone.
    if let Some(moving) = &state.moving {
        prop_assert_eq!(state.source(), moving.source);
        prop_assert_eq!(state.cursor(moving.source).key.as_ref(), Some(&moving.item));
        prop_assert!(!state.filtered(moving.source));
        prop_assert!(!state.typing && !state.help && state.question.is_none());
    }
    if state.question.is_some() {
        prop_assert!(!state.typing && !state.help && !state.quitting);
    }
    // T11: quitting waits for something.
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
            state.moving.clone(),
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

/// The oracle's id of a row: its item's, when on that row only.
fn named(rows: &[Row], at: usize) -> Option<u32> {
    let id = rows.get(at)?.key.as_ref()?.id()?;
    (rows
        .iter()
        .filter(|row| row.key.as_ref().and_then(ItemKey::id) == Some(id))
        .count()
        == 1)
        .then_some(id)
}

/// The id the CLI reads on a list row's line.
fn cli_id(kind: ListKind, row: &Row) -> Option<u32> {
    match crate::list::parse_line(kind, format!("- {}", row.text).as_bytes()) {
        crate::list::Entry::Item { id, .. } => id,
        _ => None,
    }
}

/// The oracle's account of an item after a reload from `before` to
/// `after`: its own key, or, for one typed by hand, the `nth` row in file
/// order that spells its text, with no id in front or one that was not
/// there before, when that row has an id. `None` when it is gone.
fn followed(source: Source, before: &State, after: &State, key: &ItemKey) -> Option<ItemKey> {
    let rows = all_rows(&after.data, source);
    if rows.iter().any(|row| row.key.as_ref() == Some(key)) {
        return Some(key.clone());
    }
    let kind = source.list()?;
    let Ident::Text(text) = &key.ident else {
        return None;
    };
    let old: Vec<u32> = all_rows(&before.data, source)
        .iter()
        .filter_map(|row| row.key.as_ref()?.id())
        .collect();
    let found = rows
        .iter()
        .filter_map(|row| row.key.as_ref().map(|key| (key, &row.text)))
        .filter(|(found, shown)| match found.id() {
            Some(id) => !old.contains(&id) && **shown == format!("{}{id} {text}", kind.prefix()),
            None => *shown == text,
        })
        .nth(key.nth)?
        .0;
    found.id().map(|_| found.clone())
}

/// The oracle's `board move`: the item's row taken out and put back next
/// to the target's.
fn moved_by(rows: &[Row], wanted: &Move) -> Vec<Option<ItemKey>> {
    let mut keys = keys(rows);
    let find = |keys: &[Option<ItemKey>], number: u32| {
        keys.iter()
            .position(|key| key.as_ref().and_then(ItemKey::id) == Some(number))
    };
    let Some(from) = find(&keys, wanted.item().get()) else {
        return keys;
    };
    let item = keys.remove(from);
    let Some(target) = find(&keys, wanted.target().get()) else {
        return keys;
    };
    let at = match wanted.side() {
        Side::Before => target,
        Side::After => target + 1,
    };
    keys.insert(at, item);
    keys
}

/// T3 to T6, T9 to T12: what holds of every step.
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

    let sent: Vec<&Move> = commands
        .iter()
        .filter_map(|command| match command {
            Command::Run(Action::Move(wanted)) => Some(wanted),
            _ => None,
        })
        .collect();

    // T3
    let moved = match key {
        Some(key) if navigates(key) && plain && !before.in_preview => Some((before.source(), key)),
        _ => None,
    };
    for source in Source::ALL {
        let Some(mut item) = before.cursor(source).key.clone() else {
            continue;
        };
        // An item a sync gave an id is the same item.
        if let Event::Loaded(_) = event
            && let Some(found) = followed(source, before, after, &item)
            && found != item
        {
            depth.followed += 1;
            item = found;
        }
        let item = &item;
        let items = after.items(source);
        match moved {
            Some((moved, navigation)) if moved == source => {
                // The key ended a run first: it moves the cursor from where
                // the run's move, sent or dropped, leaves the item.
                let at = match &before.moving {
                    Some(moving) if moving.source == source => {
                        let rows = before.base_rows(source);
                        let order = match sent.first() {
                            Some(wanted) => moved_by(&rows, wanted),
                            None => keys(&rows),
                        };
                        order
                            .iter()
                            .flatten()
                            .position(|key| key == item)
                            .expect("the run's item is there")
                    }
                    _ => before.cursor(source).at,
                };
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

    // T4: each command from its event.
    let pressed = |c: char| plain && key == Some(Key::Char(c));
    for command in commands {
        match command {
            Command::Load => {
                prop_assert!(matches!(event, Event::FilesChanged | Event::Ran(..)));
            }
            Command::Fetch => prop_assert!(matches!(event, Event::Tick(_))),
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
            // T9 and T10, below.
            Command::Run(Action::Done(_) | Action::Drop(_) | Action::Move(_)) => {}
            Command::Edit { .. } => {
                prop_assert!(pressed('e') || matches!(event, Event::Loaded(_)));
            }
            Command::Yank(text) => {
                prop_assert!(pressed('y') || pressed('Y'), "yank after {event:?}");
                if pressed('y')
                    && let Some((source, row)) = before.cursor_item()
                    && let Some(kind) = source.list()
                {
                    // The id the CLI reads on the line, as the line has it.
                    prop_assert_eq!(kind.parse_id(text.as_bytes()), cli_id(kind, &row));
                    prop_assert!(cli_id(kind, &row).is_some());
                    prop_assert!(row.text.starts_with(&format!("{text} ")), "{:?}", row);
                }
                depth.yanks += 1;
            }
        }
    }
    if ['d', 'x', 'y', 'J', 'K'].into_iter().any(pressed)
        && let Some((source, row)) = before.cursor_item()
        && let Some(kind) = source.list()
        && cli_id(kind, &row).is_none()
        && row
            .text
            .split_whitespace()
            .next()
            .is_some_and(|word| kind.parse_id(word.as_bytes()).is_some())
    {
        depth.on_tab_typed_id += 1;
    }
    match event {
        Event::Key(Key::Esc) => prop_assert!(commands.is_empty(), "{commands:?} after Esc"),
        Event::Key(key) if navigates(*key) => {
            prop_assert!(commands.len() == sent.len(), "{commands:?} after {key:?}");
        }
        Event::FilesChanged => prop_assert_eq!(commands, [Command::Load].as_slice()),
        // A write's answer reads the files at once: the rows of a move no
        // longer out stand where the files have them.
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
    // A run's move comes before the command of the key that ended it.
    if let Some(at) = commands
        .iter()
        .position(|command| matches!(command, Command::Run(Action::Move(_))))
    {
        prop_assert_eq!(at, 0, "{:?}", commands);
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
        if before.moving.is_some() {
            depth.esc_moving += 1;
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
            Action::Move(_) | Action::Sync => panic!("asked {question:?}"),
        };
        prop_assert!(pressed(ask), "asked {question:?} after {event:?}");
        let (source, row) = before.cursor_item().expect("asked about an item");
        prop_assert_eq!(source.list(), Some(kind));
        let number = match question.action {
            Action::Done(id) => id.get(),
            Action::Drop(id) => id.get(),
            Action::Move(_) | Action::Sync => unreachable!(),
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

    // T10: a run ends in one move, the one shown, or a note, or at home.
    prop_assert!(sent.len() <= 1, "{commands:?}");
    match &before.moving {
        None => prop_assert!(sent.is_empty(), "{commands:?} with no run"),
        Some(moving) => {
            let kind = moving.source.list().expect("a list item moves");
            let goes_on = after
                .moving
                .as_ref()
                .is_some_and(|now| now.source == moving.source && now.item == moving.item);
            let shown = before.arranged(moving.source);
            let dropped = after.messages.iter().any(|message| {
                message.text.starts_with(&format!(
                    "dropped the move of {}",
                    name_of(kind, &moving.item)
                ))
            });
            if let Event::Loaded(_) = event {
                let rows = after.base_rows(moving.source);
                // Kept while a command can name its item, next to its
                // neighbour.
                let at = rows
                    .iter()
                    .position(|row| row.key.as_ref() == Some(&moving.item));
                let nameable = at.is_some_and(|at| named(&rows, at).is_some());
                // A neighbour a sync gave an id is the same neighbour.
                let next_to = followed(moving.source, before, after, &moving.place.1);
                prop_assert_eq!(goes_on, nameable && next_to.is_some());
                if goes_on {
                    let place = &after.moving.as_ref().expect("goes on").place;
                    prop_assert_eq!(Some(&place.1), next_to.as_ref());
                    if place.1 != moving.place.1 {
                        depth.kept_next_to_followed += 1;
                    }
                    depth.kept_on_reload += 1;
                } else {
                    prop_assert!(dropped);
                    depth.dropped_on_reload += 1;
                }
            }
            if goes_on {
                prop_assert!(sent.is_empty(), "{commands:?} while the run goes on");
                prop_assert!(
                    matches!(key, None | Some(Key::Char('J' | 'K') | Key::Esc)),
                    "{key:?} did not end the run"
                );
            } else if let Some(wanted) = sent.first() {
                prop_assert_eq!(Some(wanted.item().get()), moving.item.id());
                // The move that puts the item where the screen showed it.
                prop_assert_eq!(
                    moved_by(&before.base_rows(moving.source), wanted),
                    keys(&shown)
                );
                depth.moves_sent += 1;
                if !before.writes.is_empty() {
                    depth.moved_behind_a_write += 1;
                }
                let base = before.base_rows(moving.source);
                if moves::group_of(&base, &moving.item) != moves::group_of(&shown, &moving.item) {
                    depth.moved_to_other_group += 1;
                }
            } else if matches!(key, Some(Key::Char('J' | 'K'))) && after.moving.is_none() {
                depth.came_back += 1;
            } else if !matches!(event, Event::Loaded(_)) {
                // No command can name the place: neither neighbour.
                prop_assert!(dropped, "the run of {moving:?} ended without a word");
                let at = shown
                    .iter()
                    .position(|row| row.key.as_ref() == Some(&moving.item))
                    .expect("the item is shown");
                prop_assert!(named(&shown, at + 1).is_none());
                prop_assert!(
                    at.checked_sub(1)
                        .is_none_or(|above| named(&shown, above).is_none())
                );
                depth.dropped_unnamed += 1;
            }
        }
    }
    // Sent once: what is out is not still being moved.
    if let Some(wanted) = sent.first() {
        prop_assert!(
            after
                .moving
                .as_ref()
                .is_none_or(|now| now.item.id() != Some(wanted.item().get()))
        );
    }

    // T11: the state knows what is out, as the worker does.
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

    // T12: the editor, on what `e` was pressed on, with no write out; an
    // edit waited for opens at the load after the last write, unless its
    // item is gone (or a filter hides it).
    if let (Event::Loaded(_), Some((source, item))) = (event, &before.edit_wanted)
        && after.writes.is_empty()
    {
        let there = followed(*source, before, after, item).is_some_and(|found| {
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
    // T12: a key but Esc calls a wait off; only `e` asks for a new one, on
    // the item under the cursor.
    if let Some(key) = key
        && key != Key::Esc
        && let Some((source, item)) = &after.edit_wanted
    {
        prop_assert!(pressed('e'), "{:?} kept the wait for {:?}", key, item);
        let (on, row) = before.cursor_item().expect("e on an item");
        prop_assert_eq!((on, row.key.as_ref()), (*source, Some(item)));
    }
    if before.edit_wanted.is_some() && key.is_some_and(|key| key != Key::Esc && !pressed('e')) {
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
                let (source, item) = before.edit_wanted.clone().expect("an edit was waited for");
                // An item a sync gave an id is the same item.
                let found = match event {
                    Event::Loaded(_) => followed(source, before, after, &item),
                    _ => None,
                }
                .unwrap_or_else(|| item.clone());
                if found != item {
                    depth.edit_followed += 1;
                }
                (source, found)
            };
            let row = after
                .rows(source)
                .into_iter()
                .find(|row| row.key.as_ref() == Some(&item))
                .expect("the item is there");
            match source.list() {
                None => {
                    prop_assert_eq!(path, &after.brief(source, &row).path);
                    prop_assert_eq!(*line, None);
                }
                Some(kind) => {
                    prop_assert_eq!(
                        path,
                        &BoardPath::List {
                            project: shop(),
                            kind
                        }
                    );
                    prop_assert_eq!(*line, Some(after.data.list(kind)[row.at].line));
                }
            }
            depth.edits += 1;
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
    let reached = [
        ("steps", depth.steps > 64 * 30),
        ("kept_on_moved_item", depth.kept_on_moved_item > 0),
        ("item_gone", depth.item_gone > 0),
        ("on_duplicate", depth.on_duplicate > 0),
        ("esc_typing", depth.esc_typing > 0),
        ("esc_question", depth.esc_question > 0),
        ("esc_help", depth.esc_help > 0),
        ("esc_preview", depth.esc_preview > 0),
        ("esc_preview_filtered", depth.esc_preview_filtered > 0),
        ("esc_help_over_preview", depth.esc_help_over_preview > 0),
        ("esc_filter", depth.esc_filter > 0),
        ("esc_moving", depth.esc_moving > 0),
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
        ("moves_sent", depth.moves_sent > 0),
        ("moved_to_other_group", depth.moved_to_other_group > 0),
        ("moved_behind_a_write", depth.moved_behind_a_write > 0),
        ("dropped_on_reload", depth.dropped_on_reload > 0),
        ("dropped_unnamed", depth.dropped_unnamed > 0),
        ("kept_on_reload", depth.kept_on_reload > 0),
        ("kept_next_to_followed", depth.kept_next_to_followed > 0),
        ("came_back", depth.came_back > 0),
        ("edits", depth.edits > 0),
        ("edits_waited", depth.edits_waited > 0),
        ("edit_followed", depth.edit_followed > 0),
        ("edit_called_off", depth.edit_called_off > 0),
        ("followed", depth.followed > 0),
        ("on_tab_typed_id", depth.on_tab_typed_id > 0),
        ("yanks", depth.yanks > 0),
        ("syncs", depth.syncs > 0),
        ("wrote_ok", depth.wrote_ok > 0),
        ("wrote_failed", depth.wrote_failed > 0),
        ("quit_waited", depth.quit_waited > 0),
        ("quit_after_writes", depth.quit_after_writes > 0),
        ("quit_forced", depth.quit_forced > 0),
    ];
    let missed: Vec<&str> = reached
        .iter()
        .filter(|(_, reached)| !reached)
        .map(|(name, _)| *name)
        .collect();
    assert!(missed.is_empty(), "not reached: {missed:?} in {depth:?}");
}
