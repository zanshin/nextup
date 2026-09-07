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
}
