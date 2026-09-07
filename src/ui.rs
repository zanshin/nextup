//! Rendering. `render(&App, &mut Frame)` draws the active screen as a header
//! line, a body, and a context-sensitive footer, then any open overlay (the
//! line editor or the status picker) as a centred popup. This module only reads
//! `App`. Selection is drawn with a literal `> ` marker so it shows in
//! plain-text snapshots.

use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap,
};

use crate::app::{App, Detail, Editor, Overlay, Screen, StatusPicker};
use crate::model::Status;
use crate::store::TaskRow;

const SELECTED: &str = "> ";
const UNSELECTED: &str = "  ";

pub fn render(app: &App, frame: &mut Frame) {
    let full = frame.area();

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(full);

    frame.render_widget(Paragraph::new(header_text(app)), header);

    match app.screen {
        Screen::Outstanding => render_outstanding(app, frame, body),
        Screen::Completed => render_completed(app, frame, body),
        Screen::Detail(_) => render_detail(app, frame, body),
    }

    frame.render_widget(footer_widget(app), footer);

    match &app.overlay {
        Some(Overlay::Editor(editor)) => render_editor(editor, frame, full),
        Some(Overlay::StatusPicker(picker)) => render_status_picker(picker, frame, full),
        None => {}
    }
}

fn header_text(app: &App) -> String {
    let title = match app.screen {
        Screen::Outstanding => "Outstanding",
        Screen::Completed => "Completed",
        Screen::Detail(_) => "Detail",
    };

    format!("nextup — {title}")
}

fn footer_widget(app: &App) -> Paragraph<'_> {
    if let Some(message) = &app.status_message {
        return Paragraph::new(message.as_str()).style(Style::new().add_modifier(Modifier::BOLD));
    }

    let help = if app.overlay.is_some() {
        "enter confirm · esc cancel"
    } else {
        match app.screen {
            Screen::Outstanding | Screen::Completed => {
                "j/k move · enter detail · n new · a subtask · e/E title/notes · r priority · space status · o/c switch · q quit"
            }
            Screen::Detail(_) => {
                "j/k move · enter open · p parent · tab fold · n/a new/subtask · e/E title/notes · r priority · space status · esc back"
            }
        }
    };

    Paragraph::new(help).style(Style::new().add_modifier(Modifier::DIM))
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

    let visible = detail.visible_descendants();
    if visible.is_empty() {
        frame.render_widget(Paragraph::new("(no subtasks)"), subtasks);
        return;
    }

    let rows = visible.iter().map(|entry| {
        let indent = "  ".repeat(entry.node.depth.saturating_sub(1));
        let marker = match (entry.has_children, entry.collapsed) {
            (true, true) => "[+] ",
            (true, false) => "[-] ",
            (false, _) => "",
        };

        Row::new([
            format!("{indent}{marker}{}", entry.node.task.title),
            entry.node.task.status.to_string(),
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
/// row shows in a plain-text buffer, not just via style.
fn render_list(frame: &mut Frame, area: Rect, table: Table<'_>, selected: usize) {
    let mut state = TableState::default();
    state.select(Some(selected));

    let table = table
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol(SELECTED)
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(table, area, &mut state);
}

fn render_editor(editor: &Editor, frame: &mut Frame, area: Rect) {
    let popup = centered_rect(60, 5, area);
    frame.render_widget(Clear, popup);

    let block = Block::bordered().title(format!(" {} ", editor.label));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let [input, _gap, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(Paragraph::new(editor.buffer.as_str()), input);
    frame.render_widget(
        Paragraph::new("enter save · esc cancel").style(Style::new().add_modifier(Modifier::DIM)),
        hint,
    );

    let cursor_col = editor.buffer[..editor.cursor].chars().count() as u16;
    let max_col = input.x + input.width.saturating_sub(1);
    frame.set_cursor_position(((input.x + cursor_col).min(max_col), input.y));
}

fn render_status_picker(picker: &StatusPicker, frame: &mut Frame, area: Rect) {
    let height = (Status::ALL.len() + 2) as u16;
    let popup = centered_rect(30, height, area);
    frame.render_widget(Clear, popup);

    let block = Block::bordered().title(" Status ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line<'static>> = Status::ALL
        .iter()
        .enumerate()
        .map(|(index, status)| {
            let marker = if index == picker.selected {
                SELECTED
            } else {
                UNSELECTED
            };
            Line::from(format!("{marker}{status}"))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);

    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
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
