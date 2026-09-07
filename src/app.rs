//! Terminal-independent application state: the `Screen` enum, the `App` struct,
//! modal overlays (line editor, status picker, task picker, confirm prompt), and
//! `update` — the single entry point that advances state in response to an
//! `Action`. No `ratatui` or `crossterm` types leak in here, so the whole thing
//! is exercised from tests.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rusqlite::Connection;

use crate::model::{Status, Task, TaskId};
use crate::planning::{self, Candidate, Plan};
use crate::store::{self, SubtreeNode, TaskRow};

/// Which screen the user is looking at. `Detail` carries the task it is showing;
/// the transient `Planning` / `Today` state lives in `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Outstanding,
    Completed,
    Detail(TaskId),
    Planning,
    Today,
}

/// The interaction mode, derived from whichever overlay is open. `event` uses it
/// to decide how to read a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Editing,
    Picking,
    Confirming,
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
    ToggleFold,

    NewTask,
    NewSubtask,
    EditTitle,
    EditNotes,
    EditPriority,
    ChangeStatus,

    StartPlanning,
    ShowToday,
    AssignSlot(u8),
    ClearAssignment,
    SwapSlot,

    InsertChar(char),
    Backspace,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,

    /// Apply the open overlay (save the edit / pick the status).
    Confirm,
    /// Dismiss the open overlay with no change.
    Cancel,
}

/// The loaded contents of the Detail screen: the task, its ancestor chain (root
/// first) for the breadcrumb, its subtree (`subtree[0]` is the task), the set of
/// collapsed nodes, and the selected row within the visible subtree.
#[derive(Debug, Clone)]
pub struct Detail {
    pub task: Task,
    pub ancestors: Vec<Task>,
    pub subtree: Vec<SubtreeNode>,
    pub collapsed: HashSet<TaskId>,
    pub selected: usize,
}

/// One row of the visible (fold-aware) subtree list.
pub struct VisibleNode<'a> {
    pub node: &'a SubtreeNode,
    pub has_children: bool,
    pub collapsed: bool,
}

impl Detail {
    /// The subtree without the task itself.
    fn descendants(&self) -> &[SubtreeNode] {
        self.subtree.get(1..).unwrap_or(&[])
    }

    /// The descendant rows actually shown, skipping anything beneath a collapsed
    /// node, each tagged with whether it has children and whether it is folded.
    pub fn visible_descendants(&self) -> Vec<VisibleNode<'_>> {
        let all = self.descendants();
        let mut visible = Vec::new();
        let mut hidden_below: Option<usize> = None;

        for (index, node) in all.iter().enumerate() {
            if let Some(limit) = hidden_below {
                if node.depth > limit {
                    continue;
                }
                hidden_below = None;
            }

            let has_children = all
                .get(index + 1)
                .is_some_and(|next| next.depth > node.depth);
            let collapsed = self.collapsed.contains(&node.task.id);
            visible.push(VisibleNode {
                node,
                has_children,
                collapsed,
            });

            if collapsed && has_children {
                hidden_below = Some(node.depth);
            }
        }

        visible
    }

    pub fn visible_len(&self) -> usize {
        self.visible_descendants().len()
    }

    fn selected_visible_id(&self) -> Option<TaskId> {
        self.visible_descendants()
            .get(self.selected)
            .map(|visible| visible.node.task.id)
    }
}

/// A single-line text editor for creating and renaming tasks. `cursor` is a byte
/// offset kept on a `char` boundary.
#[derive(Debug, Clone)]
pub struct Editor {
    pub target: EditTarget,
    pub label: String,
    pub buffer: String,
    pub cursor: usize,
}

/// What an `Editor`'s committed text should become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    NewRootTask,
    NewSubtaskOf(TaskId),
    Title(TaskId),
    Notes(TaskId),
    Priority(TaskId),
}

impl Editor {
    fn new(target: EditTarget, label: &str, initial: &str) -> Self {
        Editor {
            target,
            label: label.to_string(),
            buffer: initial.to_string(),
            cursor: initial.len(),
        }
    }

    fn insert(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some(prev) = self.buffer[..self.cursor].chars().next_back() {
            let start = self.cursor - prev.len_utf8();
            self.buffer.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    fn move_left(&mut self) {
        if let Some(prev) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= prev.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.buffer[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }
}

/// The status-picker overlay: which task it targets and the highlighted option
/// (an index into `Status::ALL`).
#[derive(Debug, Clone)]
pub struct StatusPicker {
    pub target: TaskId,
    pub selected: usize,
}

/// What a chosen task from the `TaskPicker` should be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPickPurpose {
    /// Swap the chosen task into this top-3 slot of today's plan.
    SwapSlot(u8),
}

/// A pick-one-task overlay (a scrollable list of candidate tasks).
#[derive(Debug, Clone)]
pub struct TaskPicker {
    pub prompt: String,
    pub candidates: Vec<TaskRow>,
    pub selected: usize,
    pub purpose: TaskPickPurpose,
}

/// A deferred action awaiting a yes/no confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAction {
    CommitPlan,
}

/// A yes/no confirmation prompt guarding a destructive action.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub prompt: String,
    pub action: PendingAction,
}

/// The modal currently on top of a screen, if any.
#[derive(Debug, Clone)]
pub enum Overlay {
    Editor(Editor),
    StatusPicker(StatusPicker),
    TaskPicker(TaskPicker),
    Confirm(Confirm),
}

/// The in-progress state of the Planning screen: the ordered candidate list, the
/// three chosen slots, and the cursor.
#[derive(Debug, Clone)]
pub struct PlanningState {
    pub date: NaiveDate,
    pub candidates: Vec<Candidate>,
    pub picks: [Option<TaskId>; 3],
    pub selected: usize,
    /// Whether a plan already existed for `date` when planning began (drives the
    /// overwrite confirmation).
    pub has_existing_plan: bool,
}

impl PlanningState {
    /// The slot (1..=3) `id` currently occupies, if any.
    pub fn slot_of(&self, id: TaskId) -> Option<u8> {
        (1..=3).find(|&n| self.picks[(n - 1) as usize] == Some(id))
    }
}

/// The loaded Today screen: the committed plan for `date` (or `None`), plus the
/// cursor.
#[derive(Debug, Clone)]
pub struct TodayState {
    pub date: NaiveDate,
    pub plan: Option<Plan>,
    pub selected: usize,
}

impl TodayState {
    fn len(&self) -> usize {
        self.plan.as_ref().map_or(0, |plan| plan.items.len())
    }
}

/// All application state. Screen data is reloaded from the database after every
/// navigation and every write, so views never drift from storage.
pub struct App {
    pub screen: Screen,
    pub should_quit: bool,

    /// The local date treated as "today" for planning. Fixed for the process.
    pub today: NaiveDate,

    pub outstanding: Vec<TaskRow>,
    pub outstanding_selected: usize,

    pub completed: Vec<TaskRow>,
    pub completed_selected: usize,

    pub detail: Option<Detail>,
    pub planning: Option<PlanningState>,
    pub todays_plan: Option<TodayState>,
    pub overlay: Option<Overlay>,

    /// A one-shot message shown in the footer until the next key press.
    pub status_message: Option<String>,
}

impl App {
    /// Build the app and load the two list screens. Starts on Outstanding.
    /// `today` is the local date planning treats as the current day.
    pub fn new(conn: &Connection, today: NaiveDate) -> Result<Self> {
        let mut app = App {
            screen: Screen::Outstanding,
            should_quit: false,
            today,
            outstanding: Vec::new(),
            outstanding_selected: 0,
            completed: Vec::new(),
            completed_selected: 0,
            detail: None,
            planning: None,
            todays_plan: None,
            overlay: None,
            status_message: None,
        };

        app.load_outstanding(conn)?;
        app.load_completed(conn)?;

        Ok(app)
    }

    /// The interaction mode implied by the open overlay.
    pub fn mode(&self) -> Mode {
        match self.overlay {
            None => Mode::Normal,
            Some(Overlay::Editor(_)) => Mode::Editing,
            Some(Overlay::StatusPicker(_)) | Some(Overlay::TaskPicker(_)) => Mode::Picking,
            Some(Overlay::Confirm(_)) => Mode::Confirming,
        }
    }

    /// Advance state for one action. Any write is followed by a reload of the
    /// current views.
    pub fn update(&mut self, action: Action, conn: &mut Connection) -> Result<()> {
        self.status_message = None;

        match self.mode() {
            Mode::Editing => self.update_editing(action, conn)?,
            Mode::Picking => self.update_picking(action, conn)?,
            Mode::Confirming => self.update_confirming(action, conn)?,
            Mode::Normal => self.update_normal(action, conn)?,
        }

        Ok(())
    }

    fn update_normal(&mut self, action: Action, conn: &mut Connection) -> Result<()> {
        match action {
            Action::Quit => self.should_quit = true,

            Action::Back => match self.screen {
                Screen::Outstanding => self.should_quit = true,
                _ => {
                    self.planning = None;
                    self.go_to(Screen::Outstanding, conn)?;
                }
            },

            Action::ShowOutstanding => self.go_to(Screen::Outstanding, conn)?,
            Action::ShowCompleted => self.go_to(Screen::Completed, conn)?,

            Action::SelectNext => self.move_selection(1),
            Action::SelectPrev => self.move_selection(-1),
            Action::SelectFirst => self.set_selection(0),
            Action::SelectLast => self.set_selection(usize::MAX),

            Action::Open => self.open_selected(conn)?,
            Action::JumpToParent => self.jump_to_parent(conn)?,
            Action::ToggleFold => self.toggle_fold(),

            Action::NewTask => {
                self.overlay = Some(Overlay::Editor(Editor::new(
                    EditTarget::NewRootTask,
                    "New task",
                    "",
                )));
            }
            Action::NewSubtask => self.begin_new_subtask(),
            Action::EditTitle => self.begin_edit_title(conn)?,
            Action::EditNotes => self.begin_edit_notes(conn)?,
            Action::EditPriority => self.begin_edit_priority(conn)?,
            Action::ChangeStatus => self.begin_change_status(conn)?,

            Action::StartPlanning => self.start_planning(conn)?,
            Action::ShowToday => self.show_today(conn)?,
            Action::AssignSlot(slot) => self.assign_slot(slot),
            Action::ClearAssignment => self.clear_assignment(),
            Action::SwapSlot => self.begin_swap(conn)?,

            // `Confirm` only reaches normal mode from the Planning screen's
            // commit key.
            Action::Confirm if matches!(self.screen, Screen::Planning) => {
                self.confirm_planning(conn)?;
            }

            _ => {}
        }

        Ok(())
    }

    fn update_editing(&mut self, action: Action, conn: &mut Connection) -> Result<()> {
        let Some(Overlay::Editor(editor)) = &mut self.overlay else {
            return Ok(());
        };

        match action {
            Action::InsertChar(ch) => editor.insert(ch),
            Action::Backspace => editor.backspace(),
            Action::CursorLeft => editor.move_left(),
            Action::CursorRight => editor.move_right(),
            Action::CursorHome => editor.move_home(),
            Action::CursorEnd => editor.move_end(),
            Action::Cancel => self.overlay = None,
            Action::Confirm => self.commit_editor(conn)?,
            _ => {}
        }

        Ok(())
    }

    fn update_picking(&mut self, action: Action, conn: &mut Connection) -> Result<()> {
        // Navigation only touches the picker; do it without disturbing `self`.
        match (&mut self.overlay, action) {
            (Some(Overlay::StatusPicker(picker)), Action::SelectNext) => {
                picker.selected = (picker.selected + 1).min(Status::ALL.len() - 1);
                return Ok(());
            }
            (Some(Overlay::StatusPicker(picker)), Action::SelectPrev) => {
                picker.selected = picker.selected.saturating_sub(1);
                return Ok(());
            }
            (Some(Overlay::TaskPicker(picker)), Action::SelectNext) => {
                let last = picker.candidates.len().saturating_sub(1);
                picker.selected = (picker.selected + 1).min(last);
                return Ok(());
            }
            (Some(Overlay::TaskPicker(picker)), Action::SelectPrev) => {
                picker.selected = picker.selected.saturating_sub(1);
                return Ok(());
            }
            _ => {}
        }

        match action {
            Action::Cancel => self.overlay = None,
            Action::Confirm => match self.overlay.take() {
                Some(Overlay::StatusPicker(picker)) => {
                    store::set_status(conn, picker.target, Status::ALL[picker.selected])?;
                    self.reload(conn)?;
                }
                Some(Overlay::TaskPicker(picker)) => {
                    if let Some(chosen) = picker
                        .candidates
                        .get(picker.selected)
                        .map(|row| row.task.id)
                    {
                        match picker.purpose {
                            TaskPickPurpose::SwapSlot(slot) => {
                                if let Err(err) =
                                    planning::swap_pick(conn, self.today, slot, chosen)
                                {
                                    self.status_message = Some(err.to_string());
                                }
                            }
                        }
                    }
                    self.show_today(conn)?;
                }
                _ => {}
            },
            _ => {}
        }

        Ok(())
    }

    fn update_confirming(&mut self, action: Action, conn: &mut Connection) -> Result<()> {
        match action {
            Action::Cancel => self.overlay = None,
            Action::Confirm => {
                if let Some(Overlay::Confirm(confirm)) = self.overlay.take() {
                    match confirm.action {
                        PendingAction::CommitPlan => self.commit_plan(conn)?,
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    // --- daily planning -----------------------------------------------------

    /// Enter the Planning screen for `today`, pre-filling slots 1/2/3 from any
    /// plan already committed for the day.
    fn start_planning(&mut self, conn: &Connection) -> Result<()> {
        let candidates = planning::candidates(conn, self.today)?;
        let existing = planning::plan_for(conn, self.today)?;

        let mut picks: [Option<TaskId>; 3] = [None; 3];
        if let Some(plan) = &existing {
            for item in &plan.items {
                if (1..=3).contains(&item.priority) {
                    picks[(item.priority - 1) as usize] = Some(item.row.task.id);
                }
            }
        }

        self.planning = Some(PlanningState {
            date: self.today,
            candidates,
            picks,
            selected: 0,
            has_existing_plan: existing.is_some(),
        });
        self.screen = Screen::Planning;

        Ok(())
    }

    /// Load and show the committed plan for `today` (may be empty / absent).
    fn show_today(&mut self, conn: &Connection) -> Result<()> {
        let plan = planning::plan_for(conn, self.today)?;
        let selected = self.todays_plan.as_ref().map_or(0, |state| state.selected);
        let len = plan.as_ref().map_or(0, |plan| plan.items.len());

        self.todays_plan = Some(TodayState {
            date: self.today,
            plan,
            selected: clamp_index(selected, len),
        });
        self.screen = Screen::Today;

        Ok(())
    }

    fn assign_slot(&mut self, slot_number: u8) {
        if !(1..=3).contains(&slot_number) {
            return;
        }
        let Some(state) = &mut self.planning else {
            return;
        };
        let Some(candidate) = state.candidates.get(state.selected) else {
            return;
        };

        let id = candidate.row.task.id;
        let index = (slot_number - 1) as usize;

        if state.picks[index] == Some(id) {
            state.picks[index] = None; // pressing the same number again unassigns
            return;
        }

        for pick in &mut state.picks {
            if *pick == Some(id) {
                *pick = None; // moved out of whatever slot it was in
            }
        }
        state.picks[index] = Some(id);
    }

    fn clear_assignment(&mut self) {
        let Some(state) = &mut self.planning else {
            return;
        };
        let Some(candidate) = state.candidates.get(state.selected) else {
            return;
        };

        let id = candidate.row.task.id;
        for pick in &mut state.picks {
            if *pick == Some(id) {
                *pick = None;
            }
        }
    }

    /// Commit today's picks, or raise the overwrite confirmation first if a plan
    /// already existed.
    fn confirm_planning(&mut self, conn: &mut Connection) -> Result<()> {
        let has_existing = self
            .planning
            .as_ref()
            .is_some_and(|state| state.has_existing_plan);

        if has_existing {
            self.overlay = Some(Overlay::Confirm(Confirm {
                prompt: "Re-plan today? This replaces the current plan.".to_string(),
                action: PendingAction::CommitPlan,
            }));
        } else {
            self.commit_plan(conn)?;
        }

        Ok(())
    }

    fn commit_plan(&mut self, conn: &mut Connection) -> Result<()> {
        let Some(state) = &self.planning else {
            return Ok(());
        };
        let date = state.date;
        let picks = state.picks;

        match planning::commit_session(conn, date, picks) {
            Ok(_) => {
                self.planning = None;
                self.show_today(conn)?;
            }
            Err(err) => self.status_message = Some(err.to_string()),
        }

        Ok(())
    }

    /// Open a task picker to swap the selected top-3 item out of today's plan.
    fn begin_swap(&mut self, conn: &Connection) -> Result<()> {
        let (slot, top_three) = {
            let Some(state) = &self.todays_plan else {
                return Ok(());
            };
            let Some(plan) = &state.plan else {
                return Ok(());
            };
            let Some(item) = plan.items.get(state.selected) else {
                return Ok(());
            };
            if item.priority > 3 {
                self.status_message = Some("only slots 1-3 can be swapped".to_string());
                return Ok(());
            }

            let top_three: Vec<TaskId> = plan
                .items
                .iter()
                .filter(|item| item.priority <= 3)
                .map(|item| item.row.task.id)
                .collect();

            (item.priority as u8, top_three)
        };

        let candidates: Vec<TaskRow> = store::outstanding_ordered(conn)?
            .into_iter()
            .filter(|row| !top_three.contains(&row.task.id))
            .collect();

        if candidates.is_empty() {
            self.status_message = Some("no other outstanding tasks to swap in".to_string());
            return Ok(());
        }

        self.overlay = Some(Overlay::TaskPicker(TaskPicker {
            prompt: format!("Swap into slot {slot}"),
            candidates,
            selected: 0,
            purpose: TaskPickPurpose::SwapSlot(slot),
        }));

        Ok(())
    }

    // --- overlay openers ---------------------------------------------------

    fn begin_new_subtask(&mut self) {
        if let Some(parent) = self.action_target() {
            self.overlay = Some(Overlay::Editor(Editor::new(
                EditTarget::NewSubtaskOf(parent),
                "New subtask",
                "",
            )));
        }
    }

    fn begin_edit_title(&mut self, conn: &Connection) -> Result<()> {
        if let Some(task) = self.target_task(conn)? {
            self.overlay = Some(Overlay::Editor(Editor::new(
                EditTarget::Title(task.id),
                "Title",
                &task.title,
            )));
        }

        Ok(())
    }

    fn begin_edit_notes(&mut self, conn: &Connection) -> Result<()> {
        if let Some(task) = self.target_task(conn)? {
            self.overlay = Some(Overlay::Editor(Editor::new(
                EditTarget::Notes(task.id),
                "Notes",
                &task.notes,
            )));
        }

        Ok(())
    }

    fn begin_edit_priority(&mut self, conn: &Connection) -> Result<()> {
        if let Some(task) = self.target_task(conn)? {
            let initial = task
                .priority
                .map(|value| value.to_string())
                .unwrap_or_default();
            self.overlay = Some(Overlay::Editor(Editor::new(
                EditTarget::Priority(task.id),
                "Priority (blank to clear)",
                &initial,
            )));
        }

        Ok(())
    }

    fn begin_change_status(&mut self, conn: &Connection) -> Result<()> {
        if let Some(task) = self.target_task(conn)? {
            let selected = Status::ALL
                .iter()
                .position(|status| *status == task.status)
                .unwrap_or(0);
            self.overlay = Some(Overlay::StatusPicker(StatusPicker {
                target: task.id,
                selected,
            }));
        }

        Ok(())
    }

    fn commit_editor(&mut self, conn: &mut Connection) -> Result<()> {
        let Some(Overlay::Editor(editor)) = &self.overlay else {
            return Ok(());
        };

        let text = editor.buffer.clone();
        let trimmed = text.trim();

        match editor.target {
            EditTarget::NewRootTask => {
                if trimmed.is_empty() {
                    self.status_message = Some("a task needs a title".to_string());
                    return Ok(());
                }
                store::create_task(conn, trimmed, "")?;
            }
            EditTarget::NewSubtaskOf(parent) => {
                if trimmed.is_empty() {
                    self.status_message = Some("a subtask needs a title".to_string());
                    return Ok(());
                }
                store::create_subtask(conn, parent, trimmed, "")?;
            }
            EditTarget::Title(id) => {
                if trimmed.is_empty() {
                    self.status_message = Some("a task needs a title".to_string());
                    return Ok(());
                }
                let task = load_task(conn, id)?;
                store::update_fields(conn, id, trimmed, &task.notes, task.priority)?;
            }
            EditTarget::Notes(id) => {
                let task = load_task(conn, id)?;
                store::update_fields(conn, id, &task.title, text.trim_end(), task.priority)?;
            }
            EditTarget::Priority(id) => {
                let priority = match parse_priority(trimmed) {
                    Ok(value) => value,
                    Err(message) => {
                        self.status_message = Some(message);
                        return Ok(());
                    }
                };
                let task = load_task(conn, id)?;
                store::update_fields(conn, id, &task.title, &task.notes, priority)?;
            }
        }

        self.overlay = None;
        self.reload(conn)
    }

    // --- navigation + loading -------------------------------------------------

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
            Screen::Detail(id) => self.open_detail(conn, id)?,
            // Planning / Today are entered through their own helpers.
            Screen::Planning | Screen::Today => self.screen = screen,
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

    /// Enter the Detail screen fresh: selection reset, nothing folded.
    fn open_detail(&mut self, conn: &Connection, id: TaskId) -> Result<()> {
        let task = load_task(conn, id)?;

        self.detail = Some(Detail {
            task,
            ancestors: store::ancestors(conn, id)?,
            subtree: store::subtree(conn, id)?,
            collapsed: HashSet::new(),
            selected: 0,
        });
        self.screen = Screen::Detail(id);

        Ok(())
    }

    /// Re-read the Detail screen after a write, keeping selection and folds.
    fn reload_detail(&mut self, conn: &Connection, id: TaskId) -> Result<()> {
        let Some(task) = store::get(conn, id)? else {
            self.detail = None;
            return self.go_to(Screen::Outstanding, conn);
        };

        let ancestors = store::ancestors(conn, id)?;
        let subtree = store::subtree(conn, id)?;

        if let Some(detail) = &mut self.detail {
            let present: HashSet<TaskId> = subtree.iter().map(|node| node.task.id).collect();
            detail.task = task;
            detail.ancestors = ancestors;
            detail.subtree = subtree;
            detail.collapsed.retain(|id| present.contains(id));
            detail.selected = clamp_index(detail.selected, detail.visible_len());
        } else {
            self.detail = Some(Detail {
                task,
                ancestors,
                subtree,
                collapsed: HashSet::new(),
                selected: 0,
            });
        }

        Ok(())
    }

    /// Reload every view that could have changed after a write.
    fn reload(&mut self, conn: &Connection) -> Result<()> {
        self.load_outstanding(conn)?;
        self.load_completed(conn)?;

        match self.screen {
            Screen::Detail(id) => self.reload_detail(conn, id)?,
            Screen::Today => self.show_today(conn)?,
            _ => {}
        }

        Ok(())
    }

    fn open_selected(&mut self, conn: &Connection) -> Result<()> {
        let target = match self.screen {
            Screen::Outstanding => self
                .outstanding
                .get(self.outstanding_selected)
                .map(|row| row.task.id),
            Screen::Completed => self
                .completed
                .get(self.completed_selected)
                .map(|row| row.task.id),
            Screen::Detail(_) => self.detail.as_ref().and_then(Detail::selected_visible_id),
            // Planning / Today have no drill-in from `Enter`.
            Screen::Planning | Screen::Today => None,
        };

        if let Some(id) = target {
            self.open_detail(conn, id)?;
        }

        Ok(())
    }

    fn jump_to_parent(&mut self, conn: &Connection) -> Result<()> {
        let parent = self
            .detail
            .as_ref()
            .and_then(|detail| detail.task.parent_id);

        if let Some(parent) = parent {
            self.open_detail(conn, parent)?;
        }

        Ok(())
    }

    fn toggle_fold(&mut self) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        let Some(id) = detail.selected_visible_id() else {
            return;
        };

        if !detail.collapsed.remove(&id) {
            detail.collapsed.insert(id);
        }

        detail.selected = clamp_index(detail.selected, detail.visible_len());
    }

    // --- targeting -------------------------------------------------------------

    /// The task id an editing/status action operates on: the selected list row,
    /// the task shown on Detail, or the selected candidate / plan item.
    fn action_target(&self) -> Option<TaskId> {
        match self.screen {
            Screen::Outstanding => self
                .outstanding
                .get(self.outstanding_selected)
                .map(|row| row.task.id),
            Screen::Completed => self
                .completed
                .get(self.completed_selected)
                .map(|row| row.task.id),
            Screen::Detail(id) => Some(id),
            Screen::Planning => self
                .planning
                .as_ref()
                .and_then(|state| state.candidates.get(state.selected))
                .map(|candidate| candidate.row.task.id),
            Screen::Today => self
                .todays_plan
                .as_ref()
                .and_then(|state| {
                    state
                        .plan
                        .as_ref()
                        .and_then(|plan| plan.items.get(state.selected))
                })
                .map(|item| item.row.task.id),
        }
    }

    fn target_task(&self, conn: &Connection) -> Result<Option<Task>> {
        match self.action_target() {
            Some(id) => Ok(Some(load_task(conn, id)?)),
            None => Ok(None),
        }
    }

    // --- selection ----------------------------------------------------------

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

    /// `(row count, &mut selection index)` for the active screen's list. A screen
    /// with no loaded data reports an empty list.
    fn active_list_mut(&mut self) -> (usize, &mut usize) {
        match self.screen {
            Screen::Outstanding => (self.outstanding.len(), &mut self.outstanding_selected),
            Screen::Completed => (self.completed.len(), &mut self.completed_selected),
            Screen::Detail(_) => match &mut self.detail {
                Some(detail) => {
                    let len = detail.visible_len();
                    (len, &mut detail.selected)
                }
                None => (0, &mut self.outstanding_selected),
            },
            Screen::Planning => match &mut self.planning {
                Some(state) => (state.candidates.len(), &mut state.selected),
                None => (0, &mut self.outstanding_selected),
            },
            Screen::Today => match &mut self.todays_plan {
                Some(state) => {
                    let len = state.len();
                    (len, &mut state.selected)
                }
                None => (0, &mut self.outstanding_selected),
            },
        }
    }
}

fn load_task(conn: &Connection, id: TaskId) -> Result<Task> {
    store::get(conn, id)?.with_context(|| format!("task {id} no longer exists"))
}

/// Parse the priority editor's text: blank clears it, otherwise a positive
/// integer.
fn parse_priority(text: &str) -> std::result::Result<Option<i64>, String> {
    if text.is_empty() {
        return Ok(None);
    }

    match text.parse::<i64>() {
        Ok(value) if value >= 1 => Ok(Some(value)),
        _ => Err("priority must be a whole number of 1 or more".to_string()),
    }
}

/// Clamp `index` to the last valid slot of a list of `len`, or 0 when empty.
fn clamp_index(index: usize, len: usize) -> usize {
    index.min(len.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(initial: &str) -> Editor {
        Editor::new(EditTarget::NewRootTask, "x", initial)
    }

    #[test]
    fn editor_inserts_at_the_cursor_and_backspaces() {
        let mut ed = editor("");
        for ch in "abc".chars() {
            ed.insert(ch);
        }
        ed.move_left();
        ed.insert('X');
        assert_eq!(ed.buffer, "abXc");

        ed.backspace();
        assert_eq!(ed.buffer, "abc");
        assert_eq!(ed.cursor, 2);
    }

    #[test]
    fn editor_cursor_moves_are_bounded() {
        let mut ed = editor("hi");
        ed.move_right();
        ed.move_right();
        ed.move_right();
        assert_eq!(ed.cursor, 2);

        ed.move_home();
        ed.move_left();
        assert_eq!(ed.cursor, 0);

        ed.move_end();
        assert_eq!(ed.cursor, 2);
    }

    #[test]
    fn priority_text_parses_blank_and_positive_integers_only() {
        assert_eq!(parse_priority(""), Ok(None));
        assert_eq!(parse_priority("3"), Ok(Some(3)));
        assert!(parse_priority("0").is_err());
        assert!(parse_priority("-1").is_err());
        assert!(parse_priority("two").is_err());
    }
}
