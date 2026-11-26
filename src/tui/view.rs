//! The state, drawn. Reads the state only: what is where is decided in
//! `layout.rs`, with the state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::layout::{self, inner_height};
use super::state::{Panel, Row, Source, State, Tab};
use crate::board::Shown;
use crate::brief::Status;

const HELP: [(&str, &str); 21] = [
    ("j k", "down, up"),
    ("wheel", "down, up, as j k"),
    ("gg G", "first, last"),
    ("ctrl-d ctrl-u", "half a page down, up"),
    ("1 2 3", "Briefs, Chores, Backlog"),
    ("h l S-Tab Tab", "previous, next panel"),
    ("click", "focus the pane, select the row clicked"),
    ("[ ]", "the Briefs tabs: Briefs, Filed"),
    ("0 Enter", "into the preview, to scroll; h back"),
    ("/", "filter the panel: Enter keeps it, Esc clears it"),
    ("d", "done: delete the chore, asked first (y/n)"),
    ("x", "drop: delete the backlog item, asked first (y/n)"),
    ("F", "put the queued brief first in the queue"),
    ("e", "edit the brief, or the list at the item, in $EDITOR"),
    ("r", "sync: commit hand edits, fetch, push"),
    ("y Y yy", "yank the id, the line, the whole item"),
    (
        "Esc",
        "leave the preview, close help, answer no, clear the filter",
    ),
    ("?", "help"),
    ("q", "quit, once the writes are done; q again: now"),
    ("", "The board redraws when its files change."),
    ("", "A write runs in the background, one at a time."),
];

pub(crate) fn draw(frame: &mut Frame, state: &State) {
    let screen = frame.area();
    let areas = state.areas();
    for panel in Panel::ALL {
        draw_panel(frame, state, panel, areas.panel(panel).intersection(screen));
    }
    draw_preview(frame, state, areas.preview.intersection(screen));
    draw_status(frame, state, areas.status.intersection(screen));
    if state.help() {
        let body = Rect::new(0, 0, screen.width, areas.status.y).intersection(screen);
        draw_help(frame, body);
    }
}

fn border(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Green)
    } else {
        Style::new()
    }
}

/// Every block's frame: rounded, as LazyGit draws its panels.
fn block(focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border(focused))
}

// Colours are the terminal's 16 named ones only, never RGB or indexed: the
// user's terminal theme decides what they look like, so the board reads on
// a light and a dark theme alike (SPEC.md, TUI).

/// An item's id, in its panel and in the preview's title. Green is focus,
/// yellow the filter, red an error.
pub(super) fn id_style() -> Style {
    Style::new().fg(Color::Cyan)
}

/// A brief's status, by what it asks of the user: blocked is the worker
/// idle on a question, unpushed waits for the owner's push, the one row
/// that asks something of him. Reported asks nothing of him: it waits for
/// the planner's read, which files it (SPEC.md, TUI).
fn status_style(shown: Shown) -> Style {
    match shown {
        Shown::Status(Status::Blocked) => Style::new().fg(Color::Red),
        Shown::Status(Status::Started) => Style::new().fg(Color::Green),
        Shown::Unpushed => Style::new().fg(Color::Yellow),
        Shown::Status(Status::Draft) => Style::new().add_modifier(Modifier::DIM),
        Shown::Status(Status::Reported | Status::Queued | Status::Filed) => Style::new(),
    }
}

/// The `─[1]─` a title starts with: the key that gives the block the keys,
/// green when it has them. One function for the panels and the preview, so
/// the four cannot drift.
fn key_span(key: &str, focused: bool) -> Span<'static> {
    Span::styled(format!("─[{key}]─"), border(focused))
}

/// A panel's title as LazyGit draws it: `─[1]─Briefs - Filed`, the jump key
/// in brackets, then the tabs; the panel's name is its first tab. The tab
/// shown is bold, and green with the border when the panel has the focus.
fn panel_title(state: &State, panel: Panel, focused: bool) -> Line<'static> {
    let (key, tabs): (&str, &[(&'static str, Option<Tab>)]) = match panel {
        Panel::Briefs => (
            "1",
            &[("Briefs", Some(Tab::Open)), ("Filed", Some(Tab::Filed))],
        ),
        Panel::Chores => ("2", &[("Chores", None)]),
        Panel::Backlog => ("3", &[("Backlog", None)]),
    };
    // The block draws its title in the border's style; plain is said
    // outright, or a focused panel's whole title would be green.
    let plain = Style::new().fg(Color::Reset);
    let mut title = vec![key_span(key, focused)];
    for (at, (name, tab)) in tabs.iter().enumerate() {
        if at > 0 {
            title.push(Span::styled(" - ", plain));
        }
        let shown = tab.is_none_or(|tab| state.tab() == tab);
        title.push(if shown {
            Span::styled(
                *name,
                plain.patch(border(focused)).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(*name, plain)
        });
    }
    if let Some(filter) = state.filter(panel) {
        let typing = state.typing() && state.focus() == panel;
        title.push(Span::styled(
            format!("  /{filter}{}", if typing { "▏" } else { "" }),
            Style::new().fg(Color::Yellow),
        ));
    }
    Line::from(title)
}

/// A row's text in spans: its id cyan, and an open brief's status in its
/// colour. Both are found from what the row knows (its key, its status),
/// not read off the text; a filed brief's status stays plain.
fn row_spans(row: &Row, source: Source) -> Vec<Span<'static>> {
    // What the row still waits on, dim: it says why a brief is not taken,
    // and is not what the eye looks up.
    let waits = &row.text[row.dim_from..];
    let mut spans = line_spans(row, source, &row.text[..row.dim_from]);
    if !waits.is_empty() {
        spans.push(Span::styled(
            waits.to_owned(),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    spans
}

/// The row's text before its `  after <id>`: its id cyan, and an open
/// brief's status in its colour.
fn line_spans(row: &Row, source: Source, text: &str) -> Vec<Span<'static>> {
    let lead = text.len() - text.trim_start_matches(' ').len();
    let Some(id) = row.id_text(source).filter(|id| {
        text[lead..]
            .strip_prefix(id.as_str())
            .is_some_and(|after| after.is_empty() || after.starts_with(' '))
    }) else {
        return vec![Span::raw(text.to_owned())];
    };
    let mut rest = &text[lead + id.len()..];
    let mut spans = vec![
        Span::raw(text[..lead].to_owned()),
        Span::styled(id, id_style()),
    ];
    if let Some(status) = row.status.filter(|_| source == Source::Open)
        && let Some(after) = rest
            .strip_prefix(' ')
            .and_then(|after| after.strip_prefix(status.as_str()))
    {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(status.as_str(), status_style(status)));
        rest = after;
    }
    spans.push(Span::raw(rest.to_owned()));
    spans
}

fn draw_panel(frame: &mut Frame, state: &State, panel: Panel, area: Rect) {
    let focused = state.focus() == panel && !state.in_preview();
    let source = state.source_of(panel);
    let block = block(focused).title(panel_title(state, panel, focused));
    let width = usize::from(layout::inner_width(area));
    let rows = state.rows(source);
    let cursor = state.cursor_row(source);
    let now = state.now();
    let lines: Vec<Line> = if rows.is_empty() {
        let empty = match (source, state.filter(panel)) {
            (_, Some(filter)) if !filter.is_empty() => "nothing matches",
            (Source::Open, _) => "no open briefs",
            (Source::Filed, _) => "no filed briefs",
            (Source::Chores, _) => "no chores",
            (Source::Backlog, _) => "no backlog items",
        };
        vec![Line::styled(
            format!(" {empty}"),
            Style::new().add_modifier(Modifier::DIM),
        )]
    } else {
        rows.iter()
            .enumerate()
            .skip(state.offset(source))
            .take(inner_height(area))
            .map(|(at, row)| {
                let marker = if cursor == Some(at) { "▶" } else { " " };
                let mut spans = vec![Span::raw(marker)];
                spans.extend(row_spans(row, source));
                if let Some(since) = row.since {
                    let age = age(now.unix().saturating_sub(since.unix()));
                    let used = Line::from(spans.clone()).width() + age.chars().count();
                    spans.push(Span::raw(" ".repeat(width.saturating_sub(used).max(2))));
                    spans.push(Span::styled(age, Style::new().add_modifier(Modifier::DIM)));
                }
                // On the cursor's row the id and status keep their colour
                // inside the reverse video, as LazyGit's selected line does.
                let style = if row.key.is_none() {
                    Style::new().add_modifier(Modifier::BOLD)
                } else if cursor == Some(at) && focused {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::new()
                };
                Line::from(spans).style(style)
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_preview(frame: &mut Frame, state: &State, area: Rect) {
    // The number names the pane, not its content: it shows with nothing to
    // preview too.
    let focused = state.in_preview();
    let block = block(focused);
    let Some(preview) = state.preview_content() else {
        frame.render_widget(block.title(key_span("0", focused)), area);
        return;
    };
    let scroll = u16::try_from(state.preview_scroll()).unwrap_or(u16::MAX);
    let paragraph = preview.paragraph().scroll((scroll, 0));
    let mut title = preview.title;
    title.spans.insert(0, key_span("0", focused));
    frame.render_widget(paragraph.block(block.title(title)), area);
}

/// How long, the way the status line says it: `<1m`, `42m`, `5h`, `3d`.
pub(crate) fn age(seconds: u64) -> String {
    match seconds {
        0..60 => "<1m".to_owned(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn draw_status(frame: &mut Frame, state: &State, area: Rect) {
    let now = state.now().unix();
    let sync = state.sync();
    let dot = || Span::raw(" · ");
    let mut left = vec![
        Span::raw(" "),
        Span::styled(
            state.data().project.to_string(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        dot(),
        Span::raw(match sync.fetched {
            Some(at) => format!("fetched {} ago", age(now.saturating_sub(at.unix()))),
            None => "not fetched yet".to_owned(),
        }),
    ];
    if let Some(counts) = sync.counts {
        if counts.unpushed > 0 {
            // `unsynced`, not `unpushed`: that word is the label of a brief
            // whose project commits wait for the owner's push; this is the
            // board-data clone, which `r` syncs.
            left.extend([dot(), Span::raw(format!("{} unsynced", counts.unpushed))]);
        }
        if counts.behind > 0 {
            left.extend([dot(), Span::raw(format!("{} behind", counts.behind))]);
        }
    }
    if let Some(brief) = state.in_flight() {
        let mut flight = format!("{} {}", brief.row.number, brief.row.status_word());
        if let Some(started) = brief.started {
            flight.push(' ');
            flight.push_str(&age(now.saturating_sub(started.unix())));
        }
        left.extend([dot(), Span::raw(flight)]);
    }
    let error = Style::new().fg(Color::Red);
    if let Some(why) = &sync.error {
        left.extend([dot(), Span::styled(format!("fetch failed: {why}"), error)]);
    }
    if let Some(why) = state.load_error() {
        left.extend([dot(), Span::styled(format!("cannot read: {why}"), error)]);
    }
    let writes = state.writes();
    if let Some(running) = writes.front() {
        let more = match writes.len() {
            1 => String::new(),
            n => format!(" (+{})", n - 1),
        };
        left.extend([dot(), Span::raw(format!("{running}…{more}"))]);
    }
    if state.quitting() {
        left.extend([
            dot(),
            Span::styled("quitting once the writes are done; q: now", error),
        ]);
    }
    for message in state.messages() {
        let style = if message.error {
            error
        } else {
            Style::new().fg(Color::Yellow)
        };
        left.extend([dot(), Span::styled(message.text.clone(), style)]);
    }
    if let Some(question) = state.question() {
        // The question goes where the hints were, so it is not cut for
        // what the line says before it.
        let ask = Line::styled(
            format!(" {} ", question.prompt()),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        );
        let width = ask.width().min(usize::from(area.width));
        let [left_area, ask_area] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(u16::try_from(width).unwrap_or(u16::MAX)),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(Line::from(left)), left_area);
        frame.render_widget(Paragraph::new(ask), ask_area);
        return;
    }
    let hints = if state.typing() {
        "Enter keep  Esc clear "
    } else if state.help() {
        "Esc close  q quit "
    } else if state.in_preview() {
        "j/k scroll  h back  ? help "
    } else {
        match state.focus() {
            Panel::Briefs => "e edit  y yank  / filter  ? help ",
            Panel::Chores => "d done  e edit  y yank  ? help ",
            Panel::Backlog => "x drop  e edit  y yank  ? help ",
        }
    };
    let left = Line::from(left);
    // What the line says comes before the hints: an error is not cut for
    // them.
    let hints = Line::raw(hints);
    let hints_width = if left.width() + hints.width() <= usize::from(area.width) {
        hints.width()
    } else {
        0
    };
    let [left_area, hints_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(u16::try_from(hints_width).unwrap_or(u16::MAX)),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(
        Paragraph::new(hints.style(Style::new().add_modifier(Modifier::DIM))),
        hints_area,
    );
}

fn draw_help(frame: &mut Frame, body: Rect) {
    let keys = HELP.iter().map(|(keys, _)| keys.len()).max().unwrap_or(0);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(key, what)| Line::raw(format!(" {key:<keys$}  {what}")))
        .collect();
    let width = lines.iter().map(Line::width).max().unwrap_or(0) + 3;
    let [area] = Layout::horizontal([Constraint::Length(u16::try_from(width).unwrap_or(u16::MAX))])
        .flex(ratatui::layout::Flex::Center)
        .areas(body);
    let [area] = Layout::vertical([Constraint::Length(
        u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    )])
    .flex(ratatui::layout::Flex::Center)
    .areas(area);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block(true).title("Help")), area);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::data::{BriefItem, Data, ListLine};
    use super::super::state::{Action, Command, Event, Key, Mouse};
    use super::*;
    use crate::board::{BriefRow, ListRow};
    use crate::brief::{Stamp, Status};
    use crate::id::BriefId;
    use crate::id::{ChoreId, ListKind};
    use crate::project::ProjectName;
    use crate::sync::{Counts, Fetched};

    const NOW: u64 = 1_800_000_000;

    fn brief(
        number: u32,
        status: Status,
        slug: &str,
        started: Option<u64>,
        body: &str,
    ) -> BriefItem {
        BriefItem::sample(
            &ProjectName::parse("shop").unwrap(),
            BriefRow {
                number: BriefId::new(number),
                status: Some(status),
                slug: slug.to_owned(),
                unpushed: false,
                queue: Default::default(),
                waits: None,
            },
            started.map(|at| Stamp::from_unix(at).unwrap()),
            format!("id: {number}\nstatus: {status}\n\n{body}").into_bytes(),
        )
    }

    fn data() -> Data {
        Data {
            project: ProjectName::parse("shop").unwrap(),
            open: vec![
                brief(
                    128,
                    Status::Started,
                    "some-slug",
                    Some(NOW - 42 * 60),
                    "# Title\n## Task\nThe task.\n## Report\n",
                ),
                brief(127, Status::Reported, "other-slug", None, "# Other\n"),
            ],
            filed: vec![brief(100, Status::Filed, "old", None, "# Old\n")],
            chores: ListLine::numbered(
                ListKind::Chores,
                vec![
                    ListRow::Group(b"deploy".to_vec()),
                    ListRow::Item(b"c12 Rotate the key. Check: the old one fails  (126)".to_vec()),
                ],
            ),
            backlog: ListLine::numbered(
                ListKind::Backlog,
                vec![
                    ListRow::Group(b"Default".to_vec()),
                    ListRow::Item(b"b41 Cache the prices  (from 126)".to_vec()),
                ],
            ),
        }
    }

    /// The state after the files are read and a fetch a minute ago found two
    /// commits to push.
    fn state(width: u16, height: u16) -> State {
        let now = |at| Event::Tick(Stamp::from_unix(at).unwrap());
        let (state, _) = State::new(data(), width, height, Stamp::from_unix(NOW - 60).unwrap());
        let events = [
            Event::Loaded(Box::new(data())),
            Event::Fetched(Fetched::Ran {
                fetched: Ok(()),
                counts: Some(Counts {
                    unpushed: 2,
                    behind: 0,
                }),
            }),
            now(NOW),
        ];
        events
            .into_iter()
            .fold(state, |state, event| state.step(event).0)
    }

    fn press(state: State, keys: &str) -> State {
        keys.chars().fold(state, |state, key| {
            let (state, commands) = state.step(Event::Key(Key::Char(key)));
            assert_eq!(commands, Vec::<Command>::new());
            state
        })
    }

    fn screen(state: &State) -> Vec<String> {
        let (width, height) = (state.areas().status.width, state.areas().status.y + 1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_board_draws_as_the_spec_shows_it() {
        assert_eq!(
            screen(&state(100, 12)),
            [
                "╭─[1]─Briefs - Filed────────────────────────╮╭─[0]─128 some-slug───────────────────────────────────╮",
                "│▶128 started   some-slug                42m││id: 128                                              │",
                "│ 127 reported  other-slug                  ││status: started                                      │",
                "│                                           ││                                                     │",
                "╰───────────────────────────────────────────╯│# Title                                              │",
                "╭─[2]─Chores────────────────────────────────╮│## Task                                              │",
                "│▶c12 Rotate the key. Check: the old one fai││The task.                                            │",
                "╰───────────────────────────────────────────╯│## Report                                            │",
                "╭─[3]─Backlog───────────────────────────────╮│                                                     │",
                "│▶b41 Cache the prices  (from 126)          ││                                                     │",
                "╰───────────────────────────────────────────╯╰─────────────────────────────────────────────────────╯",
                " shop · fetched 1m ago · 2 unsynced · 128 started 42m              e edit  y yank  / filter  ? help",
            ]
        );
    }

    #[test]
    fn a_line_cut_in_its_panel_is_whole_in_the_preview() {
        let lines = screen(&press(state(80, 12), "2"));
        // The focused panel takes the spare height, so its heading shows.
        assert_eq!(
            lines[1],
            "│▶128 started   some-slug       42m││c12 Rotate the key. Check: the old one    │"
        );
        assert_eq!(
            lines[2],
            "╰──────────────────────────────────╯│fails  (126)                              │"
        );
        assert_eq!(
            lines[4],
            "│ ## deploy                        ││                                          │"
        );
        assert_eq!(
            lines[5],
            "│▶c12 Rotate the key. Check: the ol││                                          │"
        );
    }

    #[test]
    fn the_filed_tab_shows_the_filed_briefs() {
        let lines = screen(&press(state(100, 12), "]"));
        assert!(
            lines[0].starts_with("╭─[1]─Briefs - Filed──"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("╭─[0]─100 old──"), "{}", lines[0]);
        assert!(
            lines[1].starts_with("│▶100 filed     old  "),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn a_filter_shows_in_the_title_and_says_when_nothing_matches() {
        let lines = screen(&press(state(100, 12), "3/zz"));
        let backlog = lines
            .iter()
            .position(|line| line.contains("[3]─Backlog"))
            .unwrap();
        assert!(
            lines[backlog].starts_with("╭─[3]─Backlog  /zz▏──"),
            "{}",
            lines[backlog]
        );
        assert!(
            lines[backlog + 1].starts_with("│ nothing matches  "),
            "{}",
            lines[backlog + 1]
        );
        assert!(
            lines[11].ends_with("Enter keep  Esc clear"),
            "{}",
            lines[11]
        );
    }

    #[test]
    fn help_shows_over_the_board() {
        let lines = screen(&press(state(100, 20), "?"));
        assert!(
            lines.iter().any(|line| line.contains("╭Help")),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("│ gg G           first, last")),
            "{lines:#?}"
        );
        assert!(lines[19].ends_with("Esc close  q quit"), "{}", lines[19]);
    }

    #[test]
    fn the_status_line_says_what_the_fetch_found() {
        let (state, _) = state(100, 12).step(Event::Fetched(Fetched::Ran {
            fetched: Err("could not resolve host".to_owned()),
            counts: Some(Counts {
                unpushed: 0,
                behind: 3,
            }),
        }));
        assert_eq!(
            screen(&state)[11],
            " shop · fetched 1m ago · 3 behind · 128 started 42m · fetch failed: could not resolve host"
        );
    }

    #[test]
    fn d_asks_on_the_bottom_line_and_the_write_says_there_how_it_went() {
        let asked = press(state(160, 12), "2d");
        let line = &screen(&asked)[11];
        assert!(line.starts_with(" shop · fetched 1m ago"), "{line}");
        assert!(
            line.ends_with(" done c12 Rotate the key. Check: the old one fails  (126)? y/n"),
            "{line}"
        );
        let (running, commands) = asked.step(Event::Key(Key::Char('y')));
        let done = Action::Done(ChoreId::new(12).unwrap());
        assert_eq!(commands, [Command::Run(done.clone())]);
        assert!(screen(&running)[11].contains(" · done c12… "));
        let (ran, _) = running.step(Event::Ran(done, Err("done c12: no chore c12".to_owned())));
        let line = &screen(&ran)[11];
        assert!(line.contains(" · done c12: no chore c12"), "{line}");
        assert!(!line.contains('…'), "{line}");
    }

    /// `J` and `K` are gone with the order of chores and backlog items:
    /// pressed before a key that used to end their run (`j`, `r`), they
    /// send nothing, and the screen, its colours too, is what the key alone
    /// gives. The key after, not the press alone: a run's move went out
    /// with the next key.
    #[test]
    fn j_and_k_upper_case_do_nothing() {
        for panel in ["2", "3"] {
            for (upper, key) in [('J', 'j'), ('K', 'j'), ('J', 'r'), ('K', 'r')] {
                let start = press(mouse_state(100, 16), panel);
                let step = |state: State, key: char| state.step(Event::Key(Key::Char(key)));
                let (pressed, none) = step(start.clone(), upper);
                assert_eq!(none, Vec::<Command>::new());
                let (after, commands) = step(pressed, key);
                let (want, want_commands) = step(start, key);
                assert_eq!(commands, want_commands, "{upper}{key}");
                assert!(
                    commands
                        .iter()
                        .all(|command| *command == Command::Run(Action::Sync)),
                    "{commands:?}"
                );
                assert_eq!(cells(&after), cells(&want), "{upper}{key} in {panel}");
            }
        }
    }

    /// Both lists by group, then by id as a number, on the screen as in
    /// `ls`: `c9` before `c10` although their old order keys say the other
    /// way round.
    #[test]
    fn the_panels_list_by_group_then_by_id() {
        let mut project = crate::project::Project::empty(ProjectName::parse("shop").unwrap());
        for (kind, p) in [(ListKind::Chores, 'c'), (ListKind::Backlog, 'b')] {
            for (number, text) in [
                (
                    10,
                    format!("id: {p}10\ngroup: Home\norder: 1000\n\n# ten\n"),
                ),
                (9, format!("id: {p}9\ngroup: Home\norder: 2000\n\n# nine\n")),
                (2, format!("id: {p}2\ngroup: Away\n\n# two\n")),
            ] {
                project
                    .list_mut(kind)
                    .insert(number, crate::item::Item::parse(text.as_bytes()));
            }
        }
        let data = Data::of(&project);
        let (state, _) = State::new(data, 60, 30, Stamp::from_unix(NOW).unwrap());
        let lines = screen(&state);
        let panel = |title: &str| -> Vec<String> {
            let top = lines.iter().position(|line| line.contains(title)).unwrap();
            lines[top + 1..]
                .iter()
                .take_while(|line| !line.starts_with('╰'))
                .map(|line| line.split('│').nth(1).unwrap().trim_end().to_owned())
                .collect()
        };
        for (title, p) in [("Chores", 'c'), ("Backlog", 'b')] {
            assert_eq!(
                panel(title),
                [
                    " ## Away".to_owned(),
                    format!("▶{p}2 two"),
                    " ## Home".to_owned(),
                    format!(" {p}9 nine"),
                    format!(" {p}10 ten"),
                ],
                "{title}"
            );
        }
    }

    /// A project from files: briefs at their paths, chores and backlog
    /// items by number.
    fn from_files(
        briefs: &[(&str, &str)],
        chores: &[(u32, &str)],
        backlog: &[(u32, &str)],
    ) -> Data {
        use crate::project::{BoardPath, BriefFile, Project};
        let name = ProjectName::parse("shop").unwrap();
        let mut project = Project::empty(name.clone());
        project.briefs = briefs
            .iter()
            .map(
                |(path, text)| match BoardPath::parse(&format!("shop/{path}")) {
                    Some(BoardPath::Brief { name, filed, .. }) => BriefFile {
                        name,
                        filed,
                        brief: crate::brief::Brief::parse(text.as_bytes()),
                    },
                    _ => panic!("{path} is no brief path"),
                },
            )
            .collect();
        for (kind, items) in [(ListKind::Chores, chores), (ListKind::Backlog, backlog)] {
            for (number, text) in items {
                project
                    .list_mut(kind)
                    .insert(*number, crate::item::Item::parse(text.as_bytes()));
            }
        }
        Data::of(&project)
    }

    /// Queued briefs in queue order (one marked first, then by `queued:`
    /// time, the reverse of their numbers here), and what a brief or an
    /// item waits on, dim at the end of its row; gone once the wait is over.
    #[test]
    fn the_queue_shows_in_its_order_and_a_wait_dim() {
        let briefs = [
            (
                "briefs/1-one.md",
                "status: queued\nqueued: 2026-11-26T10:00:03Z\nafter: c9\n",
            ),
            (
                "briefs/2-two.md",
                "status: queued\nqueued: 2026-11-26T10:00:02Z\n",
            ),
            (
                "briefs/3-three.md",
                "status: queued\nqueued: 2026-11-26T10:00:01Z\n",
            ),
            (
                "briefs/4-four.md",
                "status: queued\nqueued: 2026-11-26T10:00:04Z\nfirst: 1\n",
            ),
        ];
        let chores = [
            (9, "id: c9\ngroup: g\n\n# deploy\n"),
            (10, "id: c10\ngroup: g\nafter: c9\n\n# clean\n"),
        ];
        let backlog = [(3, "id: b3\ngroup: g\nafter: 2\n\n# later\n")];
        let data = from_files(&briefs, &chores, &backlog);
        let (state, _) = State::new(data, 100, 40, Stamp::from_unix(NOW).unwrap());
        let lines = screen(&state);
        let rows: Vec<&str> = lines
            .iter()
            .filter_map(|line| line.split('│').nth(1))
            .map(str::trim_end)
            .filter(|row| !row.trim().is_empty())
            .collect();
        assert_eq!(
            rows[..4],
            [
                "▶4 queued    four",
                " 3 queued    three",
                " 2 queued    two",
                " 1 queued    one  after c9",
            ]
        );
        assert!(rows.contains(&" c10 clean  after c9"), "{rows:?}");
        assert!(rows.contains(&"▶b3 later  after 2"), "{rows:?}");
        let buffer = cells(&state);
        for (row, plain) in [
            ("one  after c9", "one"),
            ("clean  after c9", "clean"),
            ("later  after 2", "later"),
        ] {
            let (x, y) = find(&buffer, row);
            let at = x + u16::try_from(plain.len()).unwrap();
            assert!(
                !buffer[(at - 1, y)].modifier.contains(Modifier::DIM),
                "{row}"
            );
            assert!(
                buffer[(at + 1, y)].modifier.contains(Modifier::DIM),
                "{row}"
            );
        }
        // c9 done: nothing shows.
        let data = from_files(&briefs, &chores[1..], &backlog);
        let (state, _) = State::new(data, 100, 40, Stamp::from_unix(NOW).unwrap());
        let lines = screen(&state).join("\n");
        assert!(lines.contains(" 1 queued    one "), "{lines}");
        assert!(!lines.contains("after c9"), "{lines}");
    }

    /// `F` on a queued brief sends one `queue --first`; on another brief it
    /// says why it does not, and on a list item it does nothing.
    #[test]
    fn f_puts_a_queued_brief_first_and_nothing_else() {
        let data = from_files(
            &[
                ("briefs/1-draft.md", "status: draft\n"),
                ("briefs/2-started.md", "status: started\n"),
                (
                    "briefs/3-queued.md",
                    "status: queued\nqueued: 2026-11-26T10:00:00Z\n",
                ),
            ],
            &[(1, "id: c1\ngroup: g\n\n# chore\n")],
            &[(1, "id: b1\ngroup: g\n\n# item\n")],
        );
        let (state, _) = State::new(data, 100, 20, Stamp::from_unix(NOW).unwrap());
        let press_f = |keys: &str| {
            let state = press(state.clone(), keys);
            state.step(Event::Key(Key::Char('F')))
        };
        // Started first, then queued, then the draft.
        let (_, commands) = press_f("j");
        assert_eq!(commands, [Command::Run(Action::First(BriefId::new(3)))]);
        for (keys, note) in [
            ("", Some("F puts a queued brief first; 2 is started")),
            ("jj", Some("F puts a queued brief first; 1 is draft")),
            ("2", None),
            ("3", None),
        ] {
            let (after, commands) = press_f(keys);
            assert_eq!(commands, Vec::<Command>::new(), "{keys:?}");
            let said: Vec<&str> = after.messages().iter().map(|m| m.text.as_str()).collect();
            assert_eq!(said, note.into_iter().collect::<Vec<_>>(), "{keys:?}");
        }
    }

    /// The state drawn, to read the cells' styles.
    fn cells(state: &State) -> ratatui::buffer::Buffer {
        let size = state.areas().status;
        let mut terminal = Terminal::new(TestBackend::new(size.width, size.y + 1)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Where `text` starts on the screen: the first line that has it.
    fn find(buffer: &ratatui::buffer::Buffer, text: &str) -> (u16, u16) {
        let area = buffer.area;
        (0..area.height)
            .find_map(|y| {
                let line: Vec<&str> = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
                (0..line.len()).find_map(|x| {
                    line[x..]
                        .concat()
                        .starts_with(text)
                        .then(|| (u16::try_from(x).unwrap(), y))
                })
            })
            .unwrap_or_else(|| panic!("{text} is not on the screen"))
    }

    /// The fg and whether bold of each cell of `text` where it starts.
    fn styles(buffer: &ratatui::buffer::Buffer, text: &str) -> Vec<(Color, bool)> {
        let (x, y) = find(buffer, text);
        (x..x + u16::try_from(text.chars().count()).unwrap())
            .map(|x| {
                let cell = &buffer[(x, y)];
                (cell.fg, cell.modifier.contains(Modifier::BOLD))
            })
            .collect()
    }

    fn all(text: &str, style: (Color, bool)) -> Vec<(Color, bool)> {
        vec![style; text.chars().count()]
    }

    #[test]
    fn a_panel_title_names_its_key_in_brackets_then_its_tabs() {
        let lines = screen(&state(100, 12));
        assert!(
            lines[0].starts_with("╭─[1]─Briefs - Filed─────"),
            "{}",
            lines[0]
        );
        assert!(lines[5].starts_with("╭─[2]─Chores─────"), "{}", lines[5]);
        assert!(lines[8].starts_with("╭─[3]─Backlog─────"), "{}", lines[8]);
        // Briefs has the focus: its key and its tab shown are green, the
        // other tab plain; the other panels' names are bold, not green.
        let focused = cells(&state(100, 12));
        assert_eq!(
            styles(&focused, "─[1]─"),
            all("─[1]─", (Color::Green, false))
        );
        assert_eq!(
            styles(&focused, "Briefs - "),
            [
                vec![(Color::Green, true); 6],
                all(" - ", (Color::Reset, false)),
            ]
            .concat()
        );
        assert_eq!(
            styles(&focused, "Filed"),
            all("Filed", (Color::Reset, false))
        );
        assert_eq!(
            styles(&focused, "─[2]─"),
            all("─[2]─", (Color::Reset, false))
        );
        assert_eq!(
            styles(&focused, "Chores"),
            all("Chores", (Color::Reset, true))
        );
        // The Filed tab shown, and Chores focused.
        let filed = cells(&press(state(100, 12), "]2"));
        assert_eq!(
            styles(&filed, "Briefs - Filed"),
            [
                all("Briefs - ", (Color::Reset, false)),
                all("Filed", (Color::Reset, true)),
            ]
            .concat()
        );
        assert_eq!(
            styles(&filed, "─[2]─Chores"),
            [
                all("─[2]─", (Color::Green, false)),
                all("Chores", (Color::Green, true)),
            ]
            .concat()
        );
    }

    #[test]
    fn ids_are_cyan_in_every_panel_and_a_heading_has_none() {
        let buffer = cells(&state(100, 12));
        for id in ["▶128 ", " 127 ", "▶c12 ", "▶b41 "] {
            let (x, y) = find(&buffer, id);
            let id_cells = (x + 1..x + 4).map(|x| buffer[(x, y)].fg);
            assert!(id_cells.into_iter().all(|fg| fg == Color::Cyan), "{id}");
            // The cell after the id is not.
            assert_eq!(buffer[(x + 4, y)].fg, Color::Reset, "{id}");
        }
        // The focused panel takes the spare height, so its heading shows;
        // and the preview's title has the id too.
        let buffer = cells(&press(state(100, 12), "2"));
        assert_eq!(
            styles(&buffer, "## deploy"),
            all("## deploy", (Color::Reset, true))
        );
        let (x, y) = find(&buffer, "╭─[0]─c12");
        assert_eq!(buffer[(x + 6, y)].fg, Color::Cyan);
    }

    /// The preview is `[0]` as the panels are `[1]` `[2]` `[3]`: green with
    /// the keys, plain without, and there with nothing to preview.
    #[test]
    fn the_preview_is_titled_0_like_a_panel() {
        let right = |state: &State| {
            let line = screen(state).remove(0);
            line.chars().skip(45).collect::<String>()
        };
        let plain = state(100, 12);
        assert_eq!(
            right(&plain),
            "╭─[0]─128 some-slug───────────────────────────────────╮"
        );
        assert_eq!(
            styles(&cells(&plain), "─[0]─"),
            all("─[0]─", (Color::Reset, false))
        );
        let inside = press(state(100, 12), "0");
        assert_eq!(right(&inside), right(&plain));
        assert_eq!(
            styles(&cells(&inside), "─[0]─"),
            all("─[0]─", (Color::Green, false))
        );
        // A filter that matches nothing: an empty panel, no content.
        let empty = press(state(100, 12), "/zz");
        assert_eq!(
            right(&empty),
            "╭─[0]─────────────────────────────────────────────────╮"
        );
    }

    /// The set of briefs with unpushed commits puts a filed one in the
    /// Briefs tab, after reported and before queued, and back in Filed
    /// once pushed; the cursor stays on its item while rows move.
    #[test]
    fn an_unpushed_brief_is_in_the_briefs_tab_until_pushed() {
        let mut briefs = data();
        briefs
            .open
            .push(brief(131, Status::Queued, "next-up", None, "# Next\n"));
        let (state, _) = state(100, 16).step(Event::Loaded(Box::new(briefs)));
        let state = press(state, "jj");
        let rows = |state: &State| -> Vec<String> {
            screen(state)[1..5]
                .iter()
                .map(|line| {
                    line.chars()
                        .skip(1)
                        .take(30)
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect()
        };
        let (pushing, _) = state.step(Event::Unpushed(Ok([BriefId::new(100)].into())));
        assert_eq!(
            rows(&pushing),
            [
                " 128 started   some-slug",
                " 127 reported  other-slug",
                " 100 unpushed  old",
                "▶131 queued    next-up",
            ]
        );
        let filed = screen(&press(pushing.clone(), "]"));
        assert!(
            !filed.iter().any(|line| line.contains("100 ")),
            "{filed:#?}"
        );
        let (pushed, _) = pushing.step(Event::Unpushed(Ok(BTreeSet::new())));
        assert_eq!(
            rows(&pushed)[..3],
            [
                " 128 started   some-slug",
                " 127 reported  other-slug",
                "▶131 queued    next-up"
            ]
        );
        let filed = screen(&press(pushed, "]"));
        assert!(filed[1].contains("100 filed     old"), "{filed:#?}");
    }

    #[test]
    fn a_status_is_coloured_by_what_it_asks_of_the_user() {
        let mut briefs = data();
        briefs.open.push(brief(
            129,
            Status::Blocked,
            "stuck",
            Some(NOW - 60),
            "# Stuck\n",
        ));
        briefs
            .open
            .push(brief(130, Status::Draft, "idea", None, "# Idea\n"));
        let (state, _) = state(100, 16).step(Event::Loaded(Box::new(briefs)));
        // Brief 100 is filed, with a commit still to push.
        let (state, _) = state.step(Event::Unpushed(Ok([BriefId::new(100)].into())));
        let buffer = cells(&state);
        let fg = |text: &str| {
            let (x, y) = find(&buffer, text);
            buffer[(x, y)].fg
        };
        assert_eq!(fg("blocked "), Color::Red);
        assert_eq!(fg("started "), Color::Green);
        // Reported waits for the planner, not the owner; the push is his.
        assert_eq!(fg("reported "), Color::Reset);
        assert_eq!(fg("unpushed "), Color::Yellow);
        let (x, y) = find(&buffer, "draft ");
        assert!(buffer[(x, y)].modifier.contains(Modifier::DIM));
        // The age is dim, the slug plain.
        let (x, y) = find(&buffer, "42m");
        assert!(buffer[(x, y)].modifier.contains(Modifier::DIM));
        // On the panel row: the preview's title has the slug too, above it.
        assert_eq!(fg("some-slug "), Color::Reset);
        // A filed brief's status stays plain; once pushed, 100 is one.
        let (state, _) = state.step(Event::Unpushed(Ok(BTreeSet::new())));
        let filed = cells(&press(state, "]"));
        let (x, y) = find(&filed, "filed ");
        assert_eq!(filed[(x, y)].fg, Color::Reset);
    }

    /// A list item's preview is its file, as `show` prints it: every line,
    /// in order, header first; its `# ` title a heading as a brief's is, a
    /// header line plain.
    #[test]
    fn an_item_s_preview_is_its_file() {
        const C3: &str = "id: c3\ngroup: Deploy\ncheck: the page loads\nowner: me\nfrom: 126\n\
                          created: 2026-11-26T10:00:00Z\n\n# Rotate the key\n\nsteps:\n\n\
                          ## first\n- a list\n";
        let mut project = crate::project::Project::empty(ProjectName::parse("shop").unwrap());
        for (number, text) in [(3, C3), (4, "id: c4\ngroup: Deploy\n\n# plain")] {
            project
                .list_mut(ListKind::Chores)
                .insert(number, crate::item::Item::parse(text.as_bytes()));
        }
        let data = Data {
            chores: Data::of(&project).chores,
            ..data()
        };
        let (state, _) = State::new(data, 100, 30, Stamp::from_unix(NOW).unwrap());
        let text = |state: &State| -> Vec<String> {
            let preview = state.preview_content().unwrap();
            preview.text.lines.iter().map(ToString::to_string).collect()
        };
        let state = press(state, "2");
        assert_eq!(text(&state), C3.lines().collect::<Vec<_>>());
        let preview = state.preview_content().unwrap();
        let style = |at: usize| preview.text.lines[at].style;
        assert_eq!(preview.text.lines[0].to_string(), "id: c3");
        assert_eq!(style(7), layout::heading_style());
        assert_eq!(style(0), Style::default());
        // The file without a final line break: nothing added.
        assert_eq!(
            text(&press(state, "j")),
            ["id: c4", "group: Deploy", "", "# plain"]
        );
    }

    #[test]
    fn ages_read_in_their_largest_unit() {
        assert_eq!(age(0), "<1m");
        assert_eq!(age(59), "<1m");
        assert_eq!(age(60), "1m");
        assert_eq!(age(3_599), "59m");
        assert_eq!(age(3_600), "1h");
        assert_eq!(age(86_399), "23h");
        assert_eq!(age(86_400), "1d");
    }

    /// The mouse tests' board: two open briefs, chores in two groups, a
    /// backlog long enough to scroll.
    fn mouse_data() -> Data {
        let mut data = data();
        data.chores = ListLine::numbered(
            ListKind::Chores,
            vec![
                ListRow::Group(b"deploy".to_vec()),
                ListRow::Item(b"c12 Rotate the key. Check: the old one fails".to_vec()),
                ListRow::Item(b"c13 Renew the cert. Check: it expires later".to_vec()),
                ListRow::Group(b"home".to_vec()),
                ListRow::Item(b"c14 Water the plants. Check: wet soil".to_vec()),
            ],
        );
        data.backlog = ListLine::numbered(
            ListKind::Backlog,
            std::iter::once(ListRow::Group(b"Default".to_vec()))
                .chain((1..=12).map(|n| ListRow::Item(format!("b{n} item {n}").into_bytes())))
                .collect(),
        );
        data
    }

    fn mouse_state(width: u16, height: u16) -> State {
        let (state, _) = State::new(mouse_data(), width, height, Stamp::from_unix(NOW).unwrap());
        state.step(Event::Loaded(Box::new(mouse_data()))).0
    }

    fn click(state: State, column: u16, row: u16) -> (State, Vec<Command>) {
        state.step(Event::Mouse(Mouse::Click { column, row }))
    }

    fn clicked(state: State, column: u16, row: u16) -> State {
        let (state, commands) = click(state, column, row);
        assert_eq!(commands, Vec::<Command>::new());
        state
    }

    /// The focused pane, and the line of the item under its cursor, as
    /// the state holds them.
    fn on(state: &State) -> (Panel, bool, Option<String>) {
        let source = state.source();
        let row = state
            .cursor_row(source)
            .map(|at| state.rows(source)[at].text.clone());
        (state.focus(), state.in_preview(), row)
    }

    fn on_row(panel: Panel, text: &str) -> (Panel, bool, Option<String>) {
        (panel, false, Some(text.to_owned()))
    }

    // At 100 x 16: the left column is 45 wide, the status line is row 15,
    // and an unfocused panel is 3 high (a quarter of 15). Briefs focused:
    // Briefs rows 0-8, Chores 9-11, Backlog 12-14. Backlog focused: Briefs
    // 0-2, Chores 3-5, Backlog 6-14, its items on rows 7-13. Chores
    // focused: Chores 3-11, its rows deploy, c12, c13, home, c14 on 4-8.

    #[test]
    fn a_click_focuses_the_pane_under_it() {
        let state = mouse_state(100, 16);
        assert_eq!(on(&state), on_row(Panel::Briefs, "128 started   some-slug"));
        // Chores' title, on its top border.
        let state = clicked(state, 10, 9);
        assert_eq!(
            on(&state),
            on_row(
                Panel::Chores,
                "c12 Rotate the key. Check: the old one fails"
            )
        );
        // Backlog's left border, now rows 12-14 again.
        let state = clicked(state, 0, 13);
        assert_eq!(on(&state).0, Panel::Backlog);
        // Briefs, from the preview's left: column 44 is Briefs' right border.
        let state = clicked(state, 44, 1);
        assert_eq!(on(&state).0, Panel::Briefs);
        // The preview, with an item under the cursor: as `0`.
        let state = clicked(state, 70, 5);
        assert_eq!(
            on(&state),
            (
                Panel::Briefs,
                true,
                Some("128 started   some-slug".to_owned())
            )
        );
        // A list pane from the preview leaves it.
        let state = clicked(state, 3, 2);
        assert!(!on(&state).1);
    }

    #[test]
    fn a_click_on_a_row_selects_its_item_and_no_other() {
        let state = press(mouse_state(100, 16), "3");
        assert_eq!(on(&state), on_row(Panel::Backlog, "b1 item 1"));
        // Row 11 is the fourth item row, under the heading on row 7: b4.
        let state = clicked(state, 5, 11);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b4 item 4"));
        // Scrolled to the end: b6 to b12 on rows 7-13.
        let state = press(state, "G");
        assert_eq!(state.offset(state.source()), 6);
        let state = clicked(state, 5, 7);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b6 item 6"));
        let state = clicked(state, 5, 13);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b12 item 12"));
        // From another pane: the row clicked in Chores (rows 3-5 now, its
        // one item row 4 shows c12, scrolled to its cursor) is selected
        // there.
        let state = clicked(state, 5, 4);
        assert_eq!(
            on(&state),
            on_row(
                Panel::Chores,
                "c12 Rotate the key. Check: the old one fails"
            )
        );
        // Chores focused: c14 is on row 8.
        let state = clicked(state, 5, 8);
        assert_eq!(
            on(&state),
            on_row(Panel::Chores, "c14 Water the plants. Check: wet soil")
        );
    }

    #[test]
    fn a_click_on_a_heading_or_below_the_rows_only_focuses() {
        let state = press(mouse_state(100, 16), "2j");
        assert_eq!(
            on(&state),
            on_row(Panel::Chores, "c13 Renew the cert. Check: it expires later")
        );
        // `home` on row 7, `deploy` on row 4, nothing on row 9.
        for row in [7, 4, 9] {
            let state = clicked(state.clone(), 5, row);
            assert_eq!(
                on(&state),
                on_row(Panel::Chores, "c13 Renew the cert. Check: it expires later")
            );
        }
        // From Briefs: a heading focuses Chores, the cursor stays.
        let state = clicked(press(state, "1"), 5, 9);
        assert_eq!(on(&state).0, Panel::Chores);
        let state = clicked(state, 5, 7);
        assert_eq!(
            on(&state),
            on_row(Panel::Chores, "c13 Renew the cert. Check: it expires later")
        );
    }

    #[test]
    fn the_status_line_and_off_the_screen_change_nothing() {
        let state = press(mouse_state(100, 16), "3j");
        for (column, row) in [(5, 15), (80, 15), (100, 3), (5, 16), (u16::MAX, u16::MAX)] {
            let after = clicked(state.clone(), column, row);
            assert_eq!(on(&after), on(&state), "{column},{row}");
            assert_eq!(screen(&after), screen(&state), "{column},{row}");
        }
    }

    #[test]
    fn the_preview_takes_a_click_only_with_an_item() {
        // A filter that hides every brief: no item, no preview.
        let state = press(mouse_state(100, 16), "/zzz");
        let state = state.step(Event::Key(Key::Enter)).0;
        assert_eq!(state.cursor_row(state.source()), None);
        let state = clicked(state, 70, 5);
        assert_eq!(on(&state), (Panel::Briefs, false, None));
    }

    #[test]
    fn the_hit_follows_the_layout_at_another_size() {
        // At 80 x 24: left 36 wide, status row 23, unfocused panels 5 high
        // (a quarter of 23). Briefs focused: Briefs 0-12, Chores 13-17,
        // Backlog 18-22. Chores shows deploy, c12, c13 on rows 14-16.
        let state = mouse_state(80, 24);
        let state = clicked(state, 5, 16);
        assert_eq!(
            on(&state),
            on_row(Panel::Chores, "c13 Renew the cert. Check: it expires later")
        );
        // Chores focused: Briefs 0-3, Chores 4-17, Backlog 18-22; its rows
        // on 5-9, c14 on row 9; backlog's heading, b1 and b2 on rows 19-21.
        let state = clicked(state, 5, 9);
        assert_eq!(
            on(&state),
            on_row(Panel::Chores, "c14 Water the plants. Check: wet soil")
        );
        let state = clicked(state, 5, 21);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b2 item 2"));
        // Backlog focused: Briefs 0-3, Chores 4-8, Backlog 9-22; the
        // heading on 10, b1 on 11.
        let state = clicked(state, 35, 13);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b2 item 2"));
        let state = clicked(state, 1, 11);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b1 item 1"));
        // Column 36 is the preview's border.
        let state = clicked(state, 36, 12);
        assert_eq!(
            on(&state),
            (Panel::Backlog, true, Some("b1 item 1".to_owned()))
        );
    }

    #[test]
    fn the_mouse_does_nothing_where_a_key_is_waited_for() {
        let waits = [
            // The filter being typed.
            press(mouse_state(100, 16), "3/item"),
            // A question.
            press(mouse_state(100, 16), "3x"),
            // Help.
            press(mouse_state(100, 16), "3?"),
        ];
        for state in waits {
            for mouse in [
                Mouse::Click { column: 5, row: 2 },
                Mouse::WheelDown,
                Mouse::WheelUp,
            ] {
                let (after, commands) = state.clone().step(Event::Mouse(mouse));
                assert_eq!(commands, Vec::<Command>::new());
                assert_eq!(screen(&after), screen(&state), "{mouse:?}");
                assert_eq!(
                    (
                        after.typing(),
                        after.question().cloned(),
                        after.help(),
                        on(&after)
                    ),
                    (
                        state.typing(),
                        state.question().cloned(),
                        state.help(),
                        on(&state)
                    )
                );
            }
        }
        // A quit waiting for a write.
        let (state, _) = press(mouse_state(100, 16), "3x").step(Event::Key(Key::Char('y')));
        let (state, commands) = state.step(Event::Key(Key::Char('q')));
        assert_eq!(commands, Vec::<Command>::new());
        assert!(state.quitting());
        for mouse in [Mouse::Click { column: 5, row: 2 }, Mouse::WheelDown] {
            let (after, commands) = state.clone().step(Event::Mouse(mouse));
            assert_eq!(commands, Vec::<Command>::new());
            assert_eq!(on(&after), on(&state));
        }
    }

    #[test]
    fn a_click_clears_a_pending_g() {
        // `g`, a click, `g`: no `gg`.
        let state = press(mouse_state(100, 16), "3Gg");
        let state = clicked(state, 5, 15);
        let state = press(state, "g");
        assert_eq!(on(&state), on_row(Panel::Backlog, "b12 item 12"));
    }

    #[test]
    fn the_wheel_moves_as_j_and_k() {
        let state = press(mouse_state(100, 16), "3");
        let wheel = |state: State, mouse| {
            let (state, commands) = state.step(Event::Mouse(mouse));
            assert_eq!(commands, Vec::<Command>::new());
            state
        };
        let state = wheel(state, Mouse::WheelDown);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b2 item 2"));
        let state = wheel(wheel(state, Mouse::WheelDown), Mouse::WheelUp);
        assert_eq!(on(&state), on_row(Panel::Backlog, "b2 item 2"));
        assert_eq!(
            on(&wheel(state.clone(), Mouse::WheelDown)),
            on(&press(state, "j"))
        );
    }
}
