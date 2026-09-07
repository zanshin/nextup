//! Terminal-independent application state: the `Screen` enum, the `App` struct,
//! and `update` — the single entry point that advances state in response to an
//! `Action`. No `ratatui` or `crossterm` types leak in here, so the whole thing
//! is exercised straight from tests.

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::model::{Task, TaskId};
use crate::store::{self, SubtreeNode, TaskRow};

/// Which screen the user is looking at. `Detail` carries the task it is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Outstanding,
    Completed,
    Detail(TaskId),
}

/// A high-level, input-independent request to change state. `event` maps key
/// presses onto these; `update` is the only thing that interprets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Back,
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    Open,
    JumpToParent,
    ShowOutstanding,
    ShowCompleted,
}

/// The loaded contents of the Detail screen: the task itself, its ancestor chain
/// (root first) for the breadcrumb, and its subtree (`subtree[0]` is the task).
#[derive(Debug, Clone)]
pub struct Detail {
    pub task: Task,
    pub ancestors: Vec<Task>,
    pub subtree: Vec<SubtreeNode>,
    pub selected: usize,
}

impl Detail {
    /// The subtree without the task itself — the rows the subtask list renders.
    pub fn descendants(&self) -> &[SubtreeNode] {
        self.subtree.get(1..).unwrap_or(&[])
    }
}

/// All application state. Screen data is reloaded from the database whenever the
/// active screen changes, so views never drift from storage.
pub struct App {
    pub screen: Screen,
    pub should_quit: bool,

    pub outstanding: Vec<TaskRow>,
    pub outstanding_selected: usize,

    pub completed: Vec<TaskRow>,
    pub completed_selected: usize,

    pub detail: Option<Detail>,
}

impl App {
    /// Build the app and load the two list screens. Starts on Outstanding.
    pub fn new(conn: &Connection) -> Result<Self> {
        let mut app = App {
            screen: Screen::Outstanding,
            should_quit: false,
            outstanding: Vec::new(),
            outstanding_selected: 0,
            completed: Vec::new(),
            completed_selected: 0,
            detail: None,
        };

        app.load_outstanding(conn)?;
        app.load_completed(conn)?;

        Ok(app)
    }

    /// Advance state for one action. Reloads whatever the new screen needs.
    pub fn update(&mut self, action: Action, conn: &Connection) -> Result<()> {
        match action {
            Action::Quit => self.should_quit = true,

            Action::Back => match self.screen {
                Screen::Outstanding => self.should_quit = true,
                _ => self.go_to(Screen::Outstanding, conn)?,
            },

            Action::ShowOutstanding => self.go_to(Screen::Outstanding, conn)?,
            Action::ShowCompleted => self.go_to(Screen::Completed, conn)?,

            Action::SelectNext => self.move_selection(1),
            Action::SelectPrev => self.move_selection(-1),
            Action::SelectFirst => self.set_selection(0),
            Action::SelectLast => self.set_selection(usize::MAX),

            Action::Open => self.open_selected(conn)?,
            Action::JumpToParent => self.jump_to_parent(conn)?,
        }

        Ok(())
    }

    fn go_to(&mut self, screen: Screen, conn: &Connection) -> Result<()> {
        match screen {
            Screen::Outstanding => {
                self.load_outstanding(conn)?;
                self.screen = Screen::Outstanding;
            }
            Screen::Completed => {
                self.load_completed(conn)?;
                self.screen = Screen::Completed;
            }
            Screen::Detail(id) => self.load_detail(conn, id)?,
        }

        Ok(())
    }

    fn load_outstanding(&mut self, conn: &Connection) -> Result<()> {
        self.outstanding = store::outstanding_ordered(conn)?;
        self.outstanding_selected = clamp_index(self.outstanding_selected, self.outstanding.len());

        Ok(())
    }

    fn load_completed(&mut self, conn: &Connection) -> Result<()> {
        self.completed = store::completed_ordered(conn)?;
        self.completed_selected = clamp_index(self.completed_selected, self.completed.len());

        Ok(())
    }

    fn load_detail(&mut self, conn: &Connection, id: TaskId) -> Result<()> {
        let task = store::get(conn, id)?
            .with_context(|| format!("opening detail for missing task {id}"))?;
        let ancestors = store::ancestors(conn, id)?;
        let subtree = store::subtree(conn, id)?;

        self.detail = Some(Detail {
            task,
            ancestors,
            subtree,
            selected: 0,
        });
        self.screen = Screen::Detail(id);

        Ok(())
    }

    fn open_selected(&mut self, conn: &Connection) -> Result<()> {
        let target = match self.screen {
            Screen::Outstanding => self
                .outstanding
                .get(self.outstanding_selected)
                .map(|r| r.task.id),
            Screen::Completed => self
                .completed
                .get(self.completed_selected)
                .map(|r| r.task.id),
            Screen::Detail(_) => self
                .detail
                .as_ref()
                .and_then(|d| d.descendants().get(d.selected))
                .map(|node| node.task.id),
        };

        if let Some(id) = target {
            self.load_detail(conn, id)?;
        }

        Ok(())
    }

    fn jump_to_parent(&mut self, conn: &Connection) -> Result<()> {
        let parent = self.detail.as_ref().and_then(|d| d.task.parent_id);

        if let Some(parent) = parent {
            self.load_detail(conn, parent)?;
        }

        Ok(())
    }

    fn move_selection(&mut self, delta: i32) {
        let (len, selected) = self.active_list_mut();
        if len == 0 {
            *selected = 0;
            return;
        }

        let max = (len - 1) as i32;
        *selected = (*selected as i32 + delta).clamp(0, max) as usize;
    }

    fn set_selection(&mut self, target: usize) {
        let (len, selected) = self.active_list_mut();
        *selected = clamp_index(target, len);
    }

    /// `(row count, &mut selection index)` for whatever list the active screen
    /// shows. Detail with no loaded data reports an empty list.
    fn active_list_mut(&mut self) -> (usize, &mut usize) {
        match self.screen {
            Screen::Outstanding => (self.outstanding.len(), &mut self.outstanding_selected),
            Screen::Completed => (self.completed.len(), &mut self.completed_selected),
            Screen::Detail(_) => match &mut self.detail {
                Some(detail) => {
                    let len = detail.subtree.len().saturating_sub(1);
                    (len, &mut detail.selected)
                }
                None => (0, &mut self.outstanding_selected),
            },
        }
    }
}

/// Clamp `index` to the last valid slot of a list of `len`, or 0 when empty.
fn clamp_index(index: usize, len: usize) -> usize {
    index.min(len.saturating_sub(1))
}
