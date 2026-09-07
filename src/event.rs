//! Input mapping: translate a `crossterm` `KeyEvent` into a high-level `Action`
//! for `app::update`. Bindings depend on the active `Screen` and `Mode` — an
//! open overlay changes what keys mean — so all key knowledge stays here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, Mode, Screen};

/// The `Action` a key press triggers, or `None` if it is unbound in this
/// screen/mode.
pub fn action_for(key: KeyEvent, screen: &Screen, mode: Mode) -> Option<Action> {
    match mode {
        Mode::Editing => editing_action(key),
        Mode::Picking => picking_action(key),
        Mode::Confirming => confirming_action(key),
        Mode::Normal => normal_action(key, screen),
    }
}

fn normal_action(key: KeyEvent, screen: &Screen) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }

    if let Some(action) = global_action(key.code) {
        return Some(action);
    }

    match screen {
        Screen::Outstanding | Screen::Completed => list_action(key.code),
        Screen::Detail(_) => match key.code {
            KeyCode::Char('p') => Some(Action::JumpToParent),
            KeyCode::Tab | KeyCode::Char('z') => Some(Action::ToggleFold),
            other => list_action(other),
        },
        Screen::Planning => planning_action(key.code),
        Screen::Today => today_action(key.code),
    }
}

/// Bindings shared by every normal-mode screen.
fn global_action(code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Esc => Some(Action::Back),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::SelectNext),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::SelectPrev),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::SelectFirst),
        KeyCode::End | KeyCode::Char('G') => Some(Action::SelectLast),
        KeyCode::Char('o') => Some(Action::ShowOutstanding),
        KeyCode::Char('c') => Some(Action::ShowCompleted),
        KeyCode::Char('s') => Some(Action::StartPlanning),
        KeyCode::Char('t') => Some(Action::ShowToday),
        _ => None,
    }
}

/// Planning screen: assign the cursor task to a slot, clear it, or commit.
fn planning_action(code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Enter => Some(Action::Confirm),
        KeyCode::Char('1') => Some(Action::AssignSlot(1)),
        KeyCode::Char('2') => Some(Action::AssignSlot(2)),
        KeyCode::Char('3') => Some(Action::AssignSlot(3)),
        KeyCode::Char('0') | KeyCode::Char('x') | KeyCode::Backspace => {
            Some(Action::ClearAssignment)
        }
        _ => None,
    }
}

/// Today screen: quick status change or swap a top-3 pick.
fn today_action(code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Char(' ') => Some(Action::ChangeStatus),
        KeyCode::Char('w') => Some(Action::SwapSlot),
        _ => None,
    }
}

fn confirming_action(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => Some(Action::Confirm),
        KeyCode::Char('n') | KeyCode::Esc => Some(Action::Cancel),
        _ => None,
    }
}

/// Row actions available on any list-bearing screen (both lists and Detail).
fn list_action(code: KeyCode) -> Option<Action> {
    match code {
        KeyCode::Enter | KeyCode::Char('l') => Some(Action::Open),
        KeyCode::Char('n') => Some(Action::NewTask),
        KeyCode::Char('a') => Some(Action::NewSubtask),
        KeyCode::Char('e') => Some(Action::EditTitle),
        KeyCode::Char('E') => Some(Action::EditNotes),
        KeyCode::Char('r') => Some(Action::EditPriority),
        KeyCode::Char(' ') => Some(Action::ChangeStatus),
        KeyCode::Char('d') => Some(Action::DeleteTask),
        _ => None,
    }
}

fn editing_action(key: KeyEvent) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Cancel);
    }

    match key.code {
        KeyCode::Enter => Some(Action::Confirm),
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Left => Some(Action::CursorLeft),
        KeyCode::Right => Some(Action::CursorRight),
        KeyCode::Home => Some(Action::CursorHome),
        KeyCode::End => Some(Action::CursorEnd),
        KeyCode::Char(ch) => Some(Action::InsertChar(ch)),
        _ => None,
    }
}

fn picking_action(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter | KeyCode::Char(' ') => Some(Action::Confirm),
        KeyCode::Esc | KeyCode::Char('q') => Some(Action::Cancel),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::SelectNext),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::SelectPrev),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskId;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn ctrl_c_quits_in_normal_mode() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            action_for(ctrl_c, &Screen::Outstanding, Mode::Normal),
            Some(Action::Quit),
        );
    }

    #[test]
    fn plain_c_shows_completed_in_normal_mode() {
        assert_eq!(
            action_for(key(KeyCode::Char('c')), &Screen::Outstanding, Mode::Normal),
            Some(Action::ShowCompleted),
        );
    }

    #[test]
    fn letters_are_text_while_editing() {
        assert_eq!(
            action_for(key(KeyCode::Char('c')), &Screen::Outstanding, Mode::Editing),
            Some(Action::InsertChar('c')),
        );
        assert_eq!(
            action_for(key(KeyCode::Enter), &Screen::Outstanding, Mode::Editing),
            Some(Action::Confirm),
        );
    }

    #[test]
    fn jkp_navigate_the_picker() {
        assert_eq!(
            action_for(key(KeyCode::Char('j')), &Screen::Outstanding, Mode::Picking),
            Some(Action::SelectNext),
        );
        assert_eq!(
            action_for(key(KeyCode::Char('p')), &Screen::Outstanding, Mode::Picking),
            None,
        );
    }

    #[test]
    fn jump_to_parent_and_fold_are_detail_only() {
        let detail = Screen::Detail(TaskId(1));
        assert_eq!(
            action_for(key(KeyCode::Char('p')), &detail, Mode::Normal),
            Some(Action::JumpToParent),
        );
        assert_eq!(
            action_for(key(KeyCode::Tab), &detail, Mode::Normal),
            Some(Action::ToggleFold),
        );
        assert_eq!(
            action_for(key(KeyCode::Char('p')), &Screen::Outstanding, Mode::Normal),
            None,
        );
    }

    #[test]
    fn s_and_t_open_planning_and_today_from_any_normal_screen() {
        assert_eq!(
            action_for(key(KeyCode::Char('s')), &Screen::Outstanding, Mode::Normal),
            Some(Action::StartPlanning),
        );
        assert_eq!(
            action_for(key(KeyCode::Char('t')), &Screen::Completed, Mode::Normal),
            Some(Action::ShowToday),
        );
    }

    #[test]
    fn number_keys_assign_slots_on_the_planning_screen() {
        assert_eq!(
            action_for(key(KeyCode::Char('2')), &Screen::Planning, Mode::Normal),
            Some(Action::AssignSlot(2)),
        );
        assert_eq!(
            action_for(key(KeyCode::Char('x')), &Screen::Planning, Mode::Normal),
            Some(Action::ClearAssignment),
        );
        assert_eq!(
            action_for(key(KeyCode::Enter), &Screen::Planning, Mode::Normal),
            Some(Action::Confirm),
        );
    }

    #[test]
    fn w_swaps_on_the_today_screen_only() {
        assert_eq!(
            action_for(key(KeyCode::Char('w')), &Screen::Today, Mode::Normal),
            Some(Action::SwapSlot),
        );
        assert_eq!(
            action_for(key(KeyCode::Char('w')), &Screen::Outstanding, Mode::Normal),
            None,
        );
    }

    #[test]
    fn confirming_mode_takes_y_and_n() {
        assert_eq!(
            action_for(key(KeyCode::Char('y')), &Screen::Planning, Mode::Confirming),
            Some(Action::Confirm),
        );
        assert_eq!(
            action_for(key(KeyCode::Char('n')), &Screen::Planning, Mode::Confirming),
            Some(Action::Cancel),
        );
    }
}
