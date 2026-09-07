//! Rendering. `render(&App, &mut Frame)` draws the active screen as a header
//! line, a body, and a context-sensitive help footer. This module only reads
//! `App`; it never changes state. Selection is drawn with a literal `> ` marker
//! so it is visible in plain-text snapshots.

use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{App, Detail, Screen};
use crate::store::TaskRow;

const SELECTED: &str = "> ";

pub fn render(app: &App, frame: &mut Frame) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(Paragraph::new(header_text(app)), header);

    match app.screen {
        Screen::Outstanding => render_outstanding(app, frame, body),
        Screen::Completed => render_completed(app, frame, body),
        Screen::Detail(_) => render_detail(app, frame, body),
    }

    frame.render_widget(
        Paragraph::new(footer_text(app)).style(Style::new().add_modifier(Modifier::DIM)),
        footer,
    );
}

fn header_text(app: &App) -> String {
    let title = match app.screen {
        Screen::Outstanding => "Outstanding",
        Screen::Completed => "Completed",
        Screen::Detail(_) => "Detail",
    };

    format!("nextup — {title}")
}

fn footer_text(app: &App) -> &'static str {
    match app.screen {
        Screen::Outstanding => "j/k move · enter detail · c completed · q quit",
        Screen::Completed => "j/k move · enter detail · o outstanding · q quit",
        Screen::Detail(_) => {
            "j/k move · enter open subtask · p parent · o outstanding · c completed · esc back"
        }
    }
}

fn render_outstanding(app: &App, frame: &mut Frame, area: Rect) {
    if app.outstanding.is_empty() {
        frame.render_widget(Paragraph::new("No outstanding tasks."), area);
        return;
    }

    let rows = app.outstanding.iter().map(|row| {
        Row::new([
            priority_cell(row.task.priority),
            row.task.status.to_string(),
            title_cell(row),
            local_date(row.task.created_at),
        ])
    });

    let widths = [
        Constraint::Length(3),
        Constraint::Length(12),
        Constraint::Min(20),
        Constraint::Length(10),
    ];

    render_list(
        frame,
        area,
        Table::new(rows, widths).header(Row::new(["pri", "status", "task", "created"])),
        app.outstanding_selected,
    );
}

fn render_completed(app: &App, frame: &mut Frame, area: Rect) {
    if app.completed.is_empty() {
        frame.render_widget(Paragraph::new("Nothing completed yet."), area);
        return;
    }

    let rows = app.completed.iter().map(|row| {
        Row::new([
            row.task.status.to_string(),
            title_cell(row),
            row.task
                .completed_at
                .map(local_date)
                .unwrap_or_else(|| "-".to_string()),
        ])
    });

    let widths = [
        Constraint::Length(12),
        Constraint::Min(20),
        Constraint::Length(10),
    ];

    render_list(
        frame,
        area,
        Table::new(rows, widths).header(Row::new(["status", "task", "completed"])),
        app.completed_selected,
    );
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let Some(detail) = &app.detail else {
        return;
    };

    let [info, subtasks] =
        Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).areas(area);

    frame.render_widget(
        Paragraph::new(info_lines(detail))
            .wrap(Wrap { trim: false })
            .block(Block::new().borders(Borders::BOTTOM)),
        info,
    );

    let descendants = detail.descendants();
    if descendants.is_empty() {
        frame.render_widget(Paragraph::new("(no subtasks)"), subtasks);
        return;
    }

    let rows = descendants.iter().map(|node| {
        let indent = "  ".repeat(node.depth.saturating_sub(1));
        Row::new([
            format!("{indent}{}", node.task.title),
            node.task.status.to_string(),
        ])
    });

    render_list(
        frame,
        subtasks,
        Table::new(rows, [Constraint::Min(20), Constraint::Length(12)]),
        detail.selected,
    );
}

fn info_lines(detail: &Detail) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if !detail.ancestors.is_empty() {
        let trail = detail
            .ancestors
            .iter()
            .map(|task| task.title.clone())
            .collect::<Vec<_>>()
            .join(" / ");
        lines.push(Line::from(format!("in: {trail}")));
    }

    lines.push(Line::from(format!(
        "{}  [{}]  priority {}",
        detail.task.title,
        detail.task.status,
        priority_cell(detail.task.priority),
    )));

    if !detail.task.notes.is_empty() {
        lines.push(Line::from(detail.task.notes.clone()));
    }

    lines
}

/// Render a `Table` with a leading selection-marker column so the highlighted
/// row is visible in a plain-text buffer, not just via style.
fn render_list(frame: &mut Frame, area: Rect, table: Table<'_>, selected: usize) {
    let mut state = TableState::default();
    state.select(Some(selected));

    let table = table
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(SELECTED)
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(table, area, &mut state);
}

fn title_cell(row: &TaskRow) -> String {
    if row.breadcrumb.is_empty() {
        row.task.title.clone()
    } else {
        format!("{}   ({})", row.task.title, row.breadcrumb.join(" / "))
    }
}

fn priority_cell(priority: Option<i64>) -> String {
    priority.map_or_else(|| "-".to_string(), |value| value.to_string())
}

fn local_date(timestamp: DateTime<Utc>) -> String {
    timestamp
        .with_timezone(&Local)
        .format("%Y-%m-%d")
        .to_string()
}
