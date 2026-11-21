//! The state, drawn. Reads the state only: what is where is decided in
//! `layout.rs`, with the state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use super::layout::{self, inner_height};
use super::state::{Panel, Source, State, Tab};

const HELP: [(&str, &str); 12] = [
    ("j k", "down, up"),
    ("gg G", "first, last"),
    ("ctrl-d ctrl-u", "half a page down, up"),
    ("1 2 3", "Briefs, Chores, Backlog"),
    ("h l Tab", "previous, next panel"),
    ("[ ]", "Open, Filed briefs"),
    ("Enter", "into the preview, to scroll; h back"),
    ("/", "filter the panel: Enter keeps it, Esc clears it"),
    ("Esc", "leave the preview, close help, clear the filter"),
    ("?", "help"),
    ("q", "quit"),
    ("", "The board redraws when its files change."),
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

fn draw_panel(frame: &mut Frame, state: &State, panel: Panel, area: Rect) {
    let focused = state.focus() == panel && !state.in_preview();
    let source = state.source_of(panel);
    let mut title = vec![Span::raw(match panel {
        Panel::Briefs => "1 Briefs  ",
        Panel::Chores => "2 Chores",
        Panel::Backlog => "3 Backlog",
    })];
    if panel == Panel::Briefs {
        let tab = |name: &'static str, this: Tab| {
            if state.tab() == this {
                Span::styled(name, Style::new().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(name, Style::new().add_modifier(Modifier::DIM))
            }
        };
        title.extend([
            tab("Open", Tab::Open),
            Span::raw(" · "),
            tab("Filed", Tab::Filed),
        ]);
    }
    if let Some(filter) = state.filter(panel) {
        let typing = state.typing() && state.focus() == panel;
        title.push(Span::styled(
            format!("  /{filter}{}", if typing { "▏" } else { "" }),
            Style::new().fg(Color::Yellow),
        ));
    }
    let block = Block::bordered()
        .title(Line::from(title))
        .border_style(border(focused));
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
                let mut text = format!("{marker}{}", row.text);
                if let Some(since) = row.since {
                    let age = age(now.unix().saturating_sub(since.unix()));
                    let used = Line::raw(text.as_str()).width() + age.chars().count();
                    text.push_str(&" ".repeat(width.saturating_sub(used).max(2)));
                    text.push_str(&age);
                }
                let style = if row.key.is_none() {
                    Style::new().add_modifier(Modifier::BOLD)
                } else if cursor == Some(at) && focused {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::new()
                };
                Line::styled(text, style)
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_preview(frame: &mut Frame, state: &State, area: Rect) {
    let block = Block::bordered().border_style(border(state.in_preview()));
    let Some(preview) = state.preview_content() else {
        frame.render_widget(block, area);
        return;
    };
    let scroll = u16::try_from(state.preview_scroll()).unwrap_or(u16::MAX);
    frame.render_widget(
        preview
            .paragraph()
            .scroll((scroll, 0))
            .block(block.title(preview.title)),
        area,
    );
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
    let hints = if state.typing() {
        "Enter keep  Esc clear "
    } else if state.help() {
        "Esc close  q quit "
    } else if state.in_preview() {
        "j/k scroll  h back  ? help "
    } else {
        "/ filter  Enter preview  ? help  q quit "
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
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("Help").border_style(border(true))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::data::{BriefItem, Data};
    use super::super::state::{Command, Event, Key};
    use super::*;
    use crate::board::{BriefRow, ListRow};
    use crate::brief::{Stamp, Status};
    use crate::id::BriefId;
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
        BriefItem {
            row: BriefRow {
                number: BriefId::new(number),
                status: Some(status),
                slug: slug.to_owned(),
            },
            started: started.map(|at| Stamp::from_unix(at).unwrap()),
            text: format!("id: {number}\nstatus: {status}\n\n{body}").into_bytes(),
        }
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
            chores: vec![
                ListRow::Group(b"deploy".to_vec()),
                ListRow::Item(b"c12 Rotate the key. Check: the old one fails  (126)".to_vec()),
            ],
            backlog: vec![ListRow::Item(b"b41 Cache the prices  (from 126)".to_vec())],
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
                "┌1 Briefs  Open · Filed─────────────────────┐┌128 some-slug────────────────────────────────────────┐",
                "│▶128 started   some-slug                42m││id: 128                                              │",
                "│ 127 reported  other-slug                  ││status: started                                      │",
                "│                                           ││                                                     │",
                "└───────────────────────────────────────────┘│# Title                                              │",
                "┌2 Chores───────────────────────────────────┐│## Task                                              │",
                "│▶c12 Rotate the key. Check: the old one fai││The task.                                            │",
                "└───────────────────────────────────────────┘│## Report                                            │",
                "┌3 Backlog──────────────────────────────────┐│                                                     │",
                "│▶b41 Cache the prices  (from 126)          ││                                                     │",
                "└───────────────────────────────────────────┘└─────────────────────────────────────────────────────┘",
                " shop · fetched 1m ago · 2 unpushed · 128 started 42m       / filter  Enter preview  ? help  q quit",
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
            "└──────────────────────────────────┘│fails  (126)                              │"
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
            lines[0].starts_with("┌1 Briefs  Open · Filed──"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("┌100 old──"), "{}", lines[0]);
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
            .position(|line| line.contains("3 Backlog"))
            .unwrap();
        assert!(
            lines[backlog].starts_with("┌3 Backlog  /zz▏──"),
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
            lines.iter().any(|line| line.contains("┌Help")),
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
