//! Where everything goes on the screen, and what the preview holds: shared
//! by the state, which keeps cursors and scrolling inside what is drawn,
//! and the view, which draws it.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

use super::data::Data;
use super::state::{Panel, Row, Source};
use super::view::id_style;

/// The left column's share of the width, and its most columns (SPEC.md,
/// TUI: built for a pane of about 200 columns).
const LEFT_PERCENT: u32 = 45;
const LEFT_MAX: u16 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Areas {
    /// Briefs, Chores, Backlog, top to bottom.
    pub panels: [Rect; 3],
    pub preview: Rect,
    pub status: Rect,
}

impl Areas {
    pub fn panel(&self, panel: Panel) -> Rect {
        self.panels[panel as usize]
    }
}

/// The rows a bordered box has inside.
pub(crate) fn inner_height(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(2))
}

pub(crate) fn inner_width(area: Rect) -> u16 {
    area.width.saturating_sub(2)
}

/// The areas for a screen of `width` x `height`, with `rows` rows shown in
/// each panel.
///
/// A panel that is not focused gets the height of its rows, up to a quarter
/// of the column, and the focused one takes what is left: the spare height
/// goes where the cursor is. Decided here: the quarter, so two unfocused
/// panels never take more than half the column, and every panel keeps at
/// least its borders and a row where the screen has them.
pub(crate) fn areas(width: u16, height: u16, focus: Panel, rows: [usize; 3]) -> Areas {
    let body = height.saturating_sub(1);
    let status = Rect::new(0, body, width, height.min(1));
    let left = u16::try_from(u32::from(width) * LEFT_PERCENT / 100)
        .unwrap_or(LEFT_MAX)
        .min(LEFT_MAX);
    let preview = Rect::new(left, 0, width - left, body);
    let cap = (body / 4).max(3);
    let mut heights = rows.map(|n| u16::try_from(n + 2).unwrap_or(u16::MAX).min(cap));
    let others: u16 = Panel::ALL
        .iter()
        .filter(|panel| **panel != focus)
        .map(|panel| heights[*panel as usize])
        .fold(0, u16::saturating_add);
    heights[focus as usize] = body.saturating_sub(others);
    let mut y = 0;
    let panels = heights.map(|wanted| {
        let height = wanted.min(body - y);
        let area = Rect::new(0, y, left, height);
        y += height;
        area
    });
    Areas {
        panels,
        preview,
        status,
    }
}

/// What the preview shows of a row: its title and its text.
pub(crate) struct Preview {
    /// The item's id, cyan as in its panel, and a brief's slug.
    pub title: Line<'static>,
    pub text: Text<'static>,
}

impl Preview {
    /// The text as drawn: wrapped, so a line cut in its panel is whole here.
    pub fn paragraph(&self) -> Paragraph<'static> {
        Paragraph::new(self.text.clone()).wrap(Wrap { trim: false })
    }

    /// How far the text scrolls in an area of this size: to where its last
    /// line is at the bottom.
    pub fn max_scroll(&self, area: Rect) -> usize {
        let width = inner_width(area);
        if width == 0 {
            return 0;
        }
        self.paragraph()
            .line_count(width)
            .saturating_sub(inner_height(area))
    }
}

/// The preview of the row at `at` in the data of `source`: a brief's file,
/// headings highlighted, or a list item's whole line.
///
/// Plain text: no markdown renderer, which is much machinery for a pane
/// briefs are read in (SPEC.md, TUI).
pub(crate) fn preview(data: &Data, source: Source, row: &Row) -> Preview {
    let brief = match source {
        Source::Open => data.open.get(row.at),
        Source::Filed => data.filed.get(row.at),
        Source::Chores | Source::Backlog => None,
    };
    if let Some(brief) = brief {
        let lines: Vec<Line<'static>> = String::from_utf8_lossy(&brief.text)
            .lines()
            .map(|line| {
                // A tab has no width of its own on the screen.
                let line = line.replace('\t', "    ");
                if line.starts_with('#') {
                    Line::styled(
                        line,
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Line::raw(line)
                }
            })
            .collect();
        return Preview {
            title: Line::from(vec![
                Span::styled(brief.row.number.to_string(), id_style()),
                Span::raw(format!(" {}", brief.row.slug)),
            ]),
            text: Text::from(lines),
        };
    }
    let title = source
        .list()
        .and_then(|kind| row.id_word(kind))
        .map(|id| Line::from(Span::styled(id, id_style())))
        .unwrap_or_default();
    Preview {
        title,
        text: Text::raw(row.text.replace('\t', "    ")),
    }
}
