//! The state, drawn. Reads the state only: what is where is decided in
//! `layout.rs`, with the state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::layout::{self, inner_height};
use super::state::{Panel, Row, Source, State, Tab};
use crate::brief::Status;

const HELP: [(&str, &str); 19] = [
    ("j k", "down, up"),
    ("gg G", "first, last"),
    ("ctrl-d ctrl-u", "half a page down, up"),
    ("1 2 3", "Briefs, Chores, Backlog"),
    ("h l S-Tab Tab", "previous, next panel"),
    ("[ ]", "the Briefs tabs: Briefs, Filed"),
    ("0 Enter", "into the preview, to scroll; h back"),
    ("/", "filter the panel: Enter keeps it, Esc clears it"),
    ("d", "done: delete the chore, asked first (y/n)"),
    ("x", "drop: delete the backlog item, asked first (y/n)"),
    (
        "J K",
        "move the item down, up; sent at the next key but Esc",
    ),
    ("e", "edit the brief, or the list at the item, in $EDITOR"),
    ("r", "sync: commit hand edits, fetch, push"),
    ("y Y", "yank the id, the line"),
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
/// yellow the filter and a pending move, red an error.
pub(super) fn id_style() -> Style {
    Style::new().fg(Color::Cyan)
}

/// A brief's status, by what it asks of the user: blocked is the worker
/// idle on a question, reported waits to be filed.
fn status_style(status: Status) -> Style {
    match status {
        Status::Blocked => Style::new().fg(Color::Red),
        Status::Started => Style::new().fg(Color::Green),
        Status::Reported => Style::new().fg(Color::Yellow),
        Status::Draft => Style::new().add_modifier(Modifier::DIM),
        Status::Queued | Status::Filed => Style::new(),
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
    let text = row.text.as_str();
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
                // Where a move puts it, before the file has it: yellow as a
                // whole, so its id and status are not coloured.
                let pending = state.pending(source, row);
                let mut spans = vec![Span::raw(marker)];
                if pending {
                    spans.push(Span::raw(row.text.clone()));
                } else {
                    spans.extend(row_spans(row, source));
                }
                if let Some(since) = row.since {
                    let age = age(now.unix().saturating_sub(since.unix()));
                    let used = Line::from(spans.clone()).width() + age.chars().count();
                    spans.push(Span::raw(" ".repeat(width.saturating_sub(used).max(2))));
                    spans.push(if pending {
                        Span::raw(age)
                    } else {
                        Span::styled(age, Style::new().add_modifier(Modifier::DIM))
                    });
                }
                // On the cursor's row the id and status keep their colour
                // inside the reverse video, as LazyGit's selected line does.
                let mut style = if row.key.is_none() {
                    Style::new().add_modifier(Modifier::BOLD)
                } else if cursor == Some(at) && focused {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::new()
                };
                if pending {
                    style = style.fg(Color::Yellow).add_modifier(Modifier::ITALIC);
                }
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
            left.extend([dot(), Span::raw(format!("{} unpushed", counts.unpushed))]);
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
    } else if state.moving().is_some() {
        "J/K move  any key but Esc sends it "
    } else if state.in_preview() {
        "j/k scroll  h back  ? help "
    } else {
        match state.focus() {
            Panel::Briefs => "e edit  y yank  / filter  ? help ",
            Panel::Chores => "d done  J/K move  e edit  ? help ",
            Panel::Backlog => "x drop  J/K move  e edit  ? help ",
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::data::{BriefItem, Data, ListLine};
    use super::super::state::{Action, Command, Event, Key};
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
                vec![ListRow::Item(b"b41 Cache the prices  (from 126)".to_vec())],
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
                " shop · fetched 1m ago · 2 unpushed · 128 started 42m              e edit  y yank  / filter  ? help",
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

    #[test]
    fn a_moved_row_shows_where_it_goes_and_that_it_is_pending() {
        let mut two = data();
        two.backlog = ListLine::numbered(
            ListKind::Backlog,
            vec![
                ListRow::Item(b"b41 Cache the prices".to_vec()),
                ListRow::Item(b"b42 Drop the old api".to_vec()),
            ],
        );
        let (state, _) = state(100, 14).step(Event::Loaded(Box::new(two)));
        let moved = press(state, "3J");
        let size = moved.areas().status;
        let mut terminal = Terminal::new(TestBackend::new(size.width, size.y + 1)).unwrap();
        terminal.draw(|frame| draw(frame, &moved)).unwrap();
        let buffer = terminal.backend().buffer();
        let row = |id: &str| {
            (0..size.y)
                .find(|y| {
                    (0..size.width)
                        .map(|x| buffer[(x, *y)].symbol())
                        .collect::<String>()
                        .contains(id)
                })
                .unwrap()
        };
        // In the panel: the cursor's row, and the other one's.
        let (moved_row, other_row) = (row("│▶b41"), row("│ b42"));
        assert_eq!(moved_row, other_row + 1);
        assert_eq!(buffer[(2, moved_row)].fg, Color::Yellow);
        // The other row's id is cyan: the pending row is yellow as a whole.
        assert_eq!(buffer[(3, moved_row)].fg, Color::Yellow);
        assert_eq!(buffer[(2, other_row)].fg, Color::Cyan);
        assert!(screen(&moved)[13].ends_with("J/K move  any key but Esc sends it"));
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
        let (state, _) = state(100, 14).step(Event::Loaded(Box::new(briefs)));
        let buffer = cells(&state);
        let fg = |text: &str| {
            let (x, y) = find(&buffer, text);
            buffer[(x, y)].fg
        };
        assert_eq!(fg("blocked "), Color::Red);
        assert_eq!(fg("started "), Color::Green);
        assert_eq!(fg("reported "), Color::Yellow);
        let (x, y) = find(&buffer, "draft ");
        assert!(buffer[(x, y)].modifier.contains(Modifier::DIM));
        // The age is dim, the slug plain.
        let (x, y) = find(&buffer, "42m");
        assert!(buffer[(x, y)].modifier.contains(Modifier::DIM));
        // On the panel row: the preview's title has the slug too, above it.
        assert_eq!(fg("some-slug "), Color::Reset);
        // A filed brief's status stays plain.
        let filed = cells(&press(state, "]"));
        let (x, y) = find(&filed, "filed ");
        assert_eq!(filed[(x, y)].fg, Color::Reset);
    }

    /// A list item's preview: its line, then its indented lines without the
    /// indentation they share; a deeper one keeps the rest of its own.
    #[test]
    fn an_item_s_preview_has_its_indented_lines() {
        let mut project = crate::project::Project::empty(ProjectName::parse("shop").unwrap());
        project.backlog = crate::list::ListFile::parse(
            ListKind::Backlog,
            b"- b41 Cache the prices\n    why: slow\n    - a list\n      - nested\n\n  free\n- b42 plain\n",
        );
        let data = Data {
            backlog: Data::of(&project).backlog,
            ..data()
        };
        let (state, _) = State::new(data, 100, 12, Stamp::from_unix(NOW).unwrap());
        let text = |state: &State| -> Vec<String> {
            let preview = state.preview_content().unwrap();
            preview.text.lines.iter().map(ToString::to_string).collect()
        };
        let state = press(state, "3");
        assert_eq!(
            text(&state),
            [
                "b41 Cache the prices",
                "why: slow",
                "- a list",
                "  - nested"
            ]
        );
        assert_eq!(text(&press(state, "j")), ["b42 plain"]);
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
}
