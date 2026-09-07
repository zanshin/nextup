//! End-to-end tests for the read-only TUI shell. Each test seeds a throwaway
//! database, then drives the app the way `main` does — key -> `event::action_for`
//! -> `app::update` -> `ui::render` onto a `TestBackend` — and asserts on the
//! rendered text.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nextup::app::{App, Screen};
use nextup::model::{Status, Task, TaskId};
use nextup::{event, store, ui};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rusqlite::Connection;
use tempfile::TempDir;

/// A driven app plus its terminal and backing database.
struct Tui {
    app: App,
    conn: Connection,
    terminal: Terminal<TestBackend>,
    _dir: TempDir,
}

impl Tui {
    fn new(seed: impl FnOnce(&mut Connection)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut conn = store::open(&dir.path().join("nextup.db")).expect("open + migrate");
        seed(&mut conn);

        let app = App::new(&conn).expect("build app");
        let terminal = Terminal::new(TestBackend::new(120, 32)).expect("test terminal");

        Self {
            app,
            conn,
            terminal,
            _dir: dir,
        }
    }

    /// Feed one unmodified key press through the real input pipeline.
    fn press(&mut self, code: KeyCode) -> &mut Self {
        self.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Feed an arbitrary key event (for modifiers).
    fn key(&mut self, key: KeyEvent) -> &mut Self {
        if let Some(action) = event::action_for(key, &self.app.screen, self.app.mode()) {
            self.app.update(action, &mut self.conn).expect("update");
        }

        self
    }

    /// Type a string one character at a time (an editor overlay must be open).
    fn typ(&mut self, text: &str) -> &mut Self {
        for ch in text.chars() {
            self.press(KeyCode::Char(ch));
        }

        self
    }

    /// Render the current state and return the buffer as newline-joined text
    /// with trailing spaces trimmed from each line.
    fn screen(&mut self) -> String {
        self.terminal
            .draw(|frame| ui::render(&self.app, frame))
            .expect("draw");

        let buffer = self.terminal.backend().buffer();
        let area = buffer.area();

        (0..area.height)
            .map(|y| {
                let line: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A small tree plus one finished task:
///
/// ```text
/// Write book
///   Chapter 1            notes: "rough draft"
///     Outline ch1        in_progress
///   Chapter 2
/// Buy groceries
/// Old task               done
/// ```
fn seed_tree(conn: &mut Connection) {
    let book = store::create_task(conn, "Write book", "").unwrap();
    let ch1 = store::create_subtask(conn, book, "Chapter 1", "rough draft").unwrap();
    let outline = store::create_subtask(conn, ch1, "Outline ch1", "").unwrap();
    store::create_subtask(conn, book, "Chapter 2", "").unwrap();
    store::create_task(conn, "Buy groceries", "").unwrap();
    let old = store::create_task(conn, "Old task", "").unwrap();

    store::set_status(conn, outline, Status::InProgress).unwrap();
    store::set_status(conn, old, Status::Done).unwrap();
}

fn line_containing<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not found in:\n{screen}"))
}

/// The list's data rows: everything after the title line and the table header,
/// up to the first blank line. Row 0 is the first task, in render order.
fn data_rows(screen: &str) -> Vec<&str> {
    screen
        .lines()
        .skip(2)
        .take_while(|line| !line.trim().is_empty())
        .collect()
}

/// The currently highlighted data row (the one carrying the `> ` marker).
fn selected_row(screen: &str) -> &str {
    data_rows(screen)
        .into_iter()
        .find(|line| line.starts_with("> "))
        .unwrap_or_else(|| panic!("no selected row in:\n{screen}"))
}

fn task_by_title(conn: &Connection, title: &str) -> Task {
    let id: i64 = conn
        .query_row("SELECT id FROM tasks WHERE title = ?1", [title], |row| {
            row.get(0)
        })
        .unwrap_or_else(|_| panic!("no task titled {title:?}"));

    store::get(conn, TaskId(id)).unwrap().unwrap()
}

#[test]
fn outstanding_screen_lists_tasks_in_daily_order_with_breadcrumbs() {
    let mut tui = Tui::new(seed_tree);
    let screen = tui.screen();

    assert!(screen.contains("nextup — Outstanding"));

    // in_progress sorts above the `new` tasks; the rest keep creation order.
    let rows = data_rows(&screen);
    assert!(rows[0].contains("Outline ch1"), "in_progress task is first");
    assert!(rows[1].contains("Write book"));
    assert!(rows[2].contains("Chapter 1"));
    assert!(rows[3].contains("Chapter 2"));
    assert!(rows[4].contains("Buy groceries"));

    // the subtask carries its ancestor breadcrumb
    assert!(rows[0].contains("(Write book / Chapter 1)"));

    // completed tasks do not appear here
    assert!(!screen.contains("Old task"));
}

#[test]
fn the_selected_row_is_marked_and_the_marker_follows_navigation() {
    let mut tui = Tui::new(seed_tree);

    let screen = tui.screen();
    let rows = data_rows(&screen);
    assert!(rows[0].starts_with("> "), "first row starts selected");
    assert!(!rows[1].starts_with("> "));

    tui.press(KeyCode::Char('j'));
    let screen = tui.screen();
    let rows = data_rows(&screen);
    assert!(!rows[0].starts_with("> "));
    assert!(
        rows[1].starts_with("> "),
        "marker followed the selection down"
    );
    assert!(rows[1].contains("Write book"));
}

#[test]
fn detail_screen_shows_breadcrumb_notes_and_subtree_then_jumps_to_parent() {
    let mut tui = Tui::new(seed_tree);

    // select "Chapter 1" (row index 2) and open it
    tui.press(KeyCode::Char('j'))
        .press(KeyCode::Char('j'))
        .press(KeyCode::Enter);

    let screen = tui.screen();
    assert!(screen.contains("nextup — Detail"));
    assert!(screen.contains("in: Write book"), "ancestor breadcrumb");
    assert!(screen.contains("rough draft"), "notes are shown");
    assert!(
        screen.contains("Outline ch1"),
        "descendant appears in the subtree"
    );

    // jump to the parent — "Write book", which is a root task
    tui.press(KeyCode::Char('p'));
    let screen = tui.screen();

    assert!(screen.contains("nextup — Detail"));
    assert!(!screen.contains("in: "), "a root task has no breadcrumb");
    assert!(screen.contains("Chapter 1"));
    assert!(screen.contains("Chapter 2"));
}

#[test]
fn detail_screen_reports_a_task_with_no_subtasks() {
    let mut tui = Tui::new(seed_tree);

    // "Buy groceries" is the last outstanding row
    tui.press(KeyCode::Char('G')).press(KeyCode::Enter);

    let screen = tui.screen();
    assert!(screen.contains("Buy groceries"));
    assert!(screen.contains("(no subtasks)"));
}

#[test]
fn completed_screen_lists_finished_tasks_and_q_quits() {
    let mut tui = Tui::new(seed_tree);
    tui.press(KeyCode::Char('c'));

    let screen = tui.screen();
    assert!(screen.contains("nextup — Completed"));
    assert!(line_containing(&screen, "Old task").contains("done"));
    assert!(
        !screen.contains("Buy groceries"),
        "outstanding tasks are not here"
    );

    assert!(!tui.app.should_quit);
    tui.press(KeyCode::Char('q'));
    assert!(tui.app.should_quit);
}

#[test]
fn esc_backs_out_of_detail_then_quits_from_outstanding() {
    let mut tui = Tui::new(seed_tree);

    tui.press(KeyCode::Enter);
    assert!(matches!(tui.app.screen, Screen::Detail(_)));

    tui.press(KeyCode::Esc);
    assert!(matches!(tui.app.screen, Screen::Outstanding));
    assert!(!tui.app.should_quit);

    tui.press(KeyCode::Esc);
    assert!(tui.app.should_quit);
}

#[test]
fn empty_database_renders_an_empty_state() {
    let mut tui = Tui::new(|_conn| {});

    assert!(tui.screen().contains("No outstanding tasks."));

    tui.press(KeyCode::Char('c'));
    assert!(tui.screen().contains("Nothing completed yet."));
}

// --- Phase 5: writes from the keyboard ------------------------------------

#[test]
fn create_edit_and_cascade_lifecycle_is_driveable_from_the_keyboard() {
    let mut tui = Tui::new(|_conn| {});

    // create a root task
    tui.press(KeyCode::Char('n'))
        .typ("Ship release")
        .press(KeyCode::Enter);
    assert!(tui.screen().contains("Ship release"));

    // add two subtasks to it (it is the selected row)
    tui.press(KeyCode::Char('a'))
        .typ("Write notes")
        .press(KeyCode::Enter);
    tui.press(KeyCode::Char('a'))
        .typ("Cut tag")
        .press(KeyCode::Enter);

    let screen = tui.screen();
    let rows = data_rows(&screen);
    assert!(rows[0].contains("Ship release"));
    assert!(rows.iter().any(|row| row.contains("Write notes")));
    assert!(rows.iter().any(|row| row.contains("Cut tag")));

    // select "Write notes" and move it to in_progress via the picker
    tui.press(KeyCode::Char('j'));
    assert!(selected_row(&tui.screen()).contains("Write notes"));
    tui.press(KeyCode::Char(' ')) // open picker (current = new)
        .press(KeyCode::Char('j')) // -> in_progress
        .press(KeyCode::Enter);

    // in_progress sorts to the top of Outstanding
    assert!(data_rows(&tui.screen())[0].contains("Write notes"));

    // select the parent and mark it done
    tui.press(KeyCode::Char('g')).press(KeyCode::Char('j'));
    assert!(selected_row(&tui.screen()).contains("Ship release"));
    tui.press(KeyCode::Char(' '));
    for _ in 0..5 {
        tui.press(KeyCode::Char('j')); // new -> done
    }
    tui.press(KeyCode::Enter);

    // the whole subtree cascaded closed
    assert!(tui.screen().contains("No outstanding tasks."));

    tui.press(KeyCode::Char('c'));
    let screen = tui.screen();
    for title in ["Ship release", "Write notes", "Cut tag"] {
        assert!(
            line_containing(&screen, title).contains("done"),
            "{title} is done"
        );
    }

    // cascade stamped one completion time across all three
    let parent = task_by_title(&tui.conn, "Ship release");
    let child_a = task_by_title(&tui.conn, "Write notes");
    let child_b = task_by_title(&tui.conn, "Cut tag");
    assert!(parent.completed_at.is_some());
    assert_eq!(parent.completed_at, child_a.completed_at);
    assert_eq!(parent.completed_at, child_b.completed_at);
}

#[test]
fn editing_a_title_propagates_to_breadcrumbs() {
    let mut tui = Tui::new(seed_tree);

    tui.press(KeyCode::Char('j')); // select "Write book"
    tui.press(KeyCode::Char('e'));
    for _ in 0..40 {
        tui.press(KeyCode::Backspace);
    }
    tui.typ("Write the book").press(KeyCode::Enter);

    let screen = tui.screen();
    assert!(screen.contains("Write the book"));
    assert!(
        !screen.contains("(Write book /"),
        "renamed task no longer appears in children's breadcrumbs",
    );
}

#[test]
fn cancelling_the_editor_changes_nothing() {
    let mut tui = Tui::new(seed_tree);

    tui.press(KeyCode::Char('j')).press(KeyCode::Char('e'));
    tui.typ("ZZZ").press(KeyCode::Esc);

    let screen = tui.screen();
    assert!(!screen.contains("ZZZ"));
    assert!(selected_row(&screen).contains("Write book"));
}

#[test]
fn setting_then_clearing_a_priority() {
    let mut tui = Tui::new(seed_tree);
    tui.press(KeyCode::Char('j')); // "Write book"

    tui.press(KeyCode::Char('r')).typ("2").press(KeyCode::Enter);
    assert!(
        selected_row(&tui.screen())
            .trim_start_matches("> ")
            .starts_with('2'),
        "priority column shows 2",
    );

    // a non-number is rejected and reported
    tui.press(KeyCode::Char('r')).typ("x").press(KeyCode::Enter);
    assert!(tui.screen().contains("priority must be"));
    tui.press(KeyCode::Esc);

    // blank clears it
    tui.press(KeyCode::Char('r'))
        .press(KeyCode::Backspace)
        .press(KeyCode::Enter);
    assert!(
        selected_row(&tui.screen())
            .trim_start_matches("> ")
            .starts_with('-'),
    );
}

#[test]
fn detail_subtree_folds_and_unfolds() {
    let mut tui = Tui::new(seed_tree);

    tui.press(KeyCode::Char('j')).press(KeyCode::Enter); // "Write book" detail
    let screen = tui.screen();
    assert!(screen.contains("Chapter 1"));
    assert!(screen.contains("Outline ch1"));

    // the subtree selection starts on "Chapter 1"; fold it
    tui.press(KeyCode::Tab);
    let screen = tui.screen();
    assert!(
        screen.contains("[+] "),
        "a folded parent shows the [+] marker"
    );
    assert!(!screen.contains("Outline ch1"), "the grandchild is hidden");
    assert!(screen.contains("Chapter 2"), "the sibling stays visible");

    tui.press(KeyCode::Tab);
    assert!(tui.screen().contains("Outline ch1"));
}

#[test]
fn editing_notes_from_the_detail_screen() {
    let mut tui = Tui::new(seed_tree);

    tui.press(KeyCode::Char('G')).press(KeyCode::Enter); // "Buy groceries" detail
    tui.press(KeyCode::Char('E'))
        .typ("milk, eggs")
        .press(KeyCode::Enter);

    assert!(tui.screen().contains("milk, eggs"));
}

#[test]
fn ctrl_c_quits_even_with_an_editor_open() {
    let mut tui = Tui::new(seed_tree);

    tui.press(KeyCode::Char('n')).typ("half typed");
    tui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    // editor cancelled, nothing created, still running
    assert!(tui.app.overlay.is_none());
    assert!(!tui.screen().contains("half typed"));
    assert!(!tui.app.should_quit);
}
