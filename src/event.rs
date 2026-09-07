//! Input mapping: translate a `crossterm` `KeyEvent` into a high-level `Action`
//! for `app::update`. Bindings are context-sensitive — the active `Screen`
//! decides what a key means — so all key knowledge stays in this one place.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, Screen};

/// The `Action` a key press triggers on `screen`, or `None` if it is unbound.
pub fn action_for(key: KeyEvent, screen: &Screen) -> Option<Action> {
    // Ctrl-C always quits, regardless of screen.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }

    if let Some(action) = global_action(key.code) {
        return Some(action);
    }

    match screen {
        Screen::Outstanding | Screen::Completed => match key.code {
            KeyCode::Enter | KeyCode::Char('l') => Some(Action::Open),
            _ => None,
        },
        Screen::Detail(_) => match key.code {
            KeyCode::Enter | KeyCode::Char('l') => Some(Action::Open),
            KeyCode::Char('p') => Some(Action::JumpToParent),
            _ => None,
        },
    }
}

/// Bindings that mean the same thing on every screen.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn ctrl_c_quits_from_any_screen() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            action_for(ctrl_c, &Screen::Detail(crate::model::TaskId(1))),
            Some(Action::Quit),
        );
    }

    #[test]
    fn plain_c_shows_completed_not_quit() {
        assert_eq!(
            action_for(key(KeyCode::Char('c')), &Screen::Outstanding),
            Some(Action::ShowCompleted),
        );
    }

    #[test]
    fn jump_to_parent_is_detail_only() {
        assert_eq!(
            action_for(
                key(KeyCode::Char('p')),
                &Screen::Detail(crate::model::TaskId(1))
            ),
            Some(Action::JumpToParent),
        );
        assert_eq!(
            action_for(key(KeyCode::Char('p')), &Screen::Outstanding),
            None
        );
    }
}
