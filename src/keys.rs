//! Keyboard chord → action mapping. v0.1.

use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum Action {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Yank,
    Refresh,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    StartSearch,
    StartPost,
    StartThreadReply,
    StartReact,
    InputChar(char),
    InputBackspace,
    SubmitText,
    CancelText,
}

pub fn handle(key: KeyEvent, app: &App) -> Option<Action> {
    let m = key.modifiers;

    // Text-entry mode (search query or post buffer) — characters
    // route to the buffer, special chords still work.
    let in_text = app.post_mode.is_some() || app.active().data.search_mode;

    // Always-on quit chord.
    if matches!(key.code, KeyCode::Char('c')) && m.contains(KeyModifiers::CONTROL) {
        return Some(Action::Quit);
    }

    // Submit / cancel for text-entry.
    if in_text {
        return match key.code {
            KeyCode::Esc => Some(Action::CancelText),
            KeyCode::Enter if app.active().data.search_mode => Some(Action::SubmitText),
            KeyCode::Char('s') if m.contains(KeyModifiers::CONTROL) => Some(Action::SubmitText),
            KeyCode::Backspace => Some(Action::InputBackspace),
            KeyCode::Char(c) => Some(Action::InputChar(c)),
            _ => None,
        };
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Enter => Some(Action::Enter),
        KeyCode::Char('y') => Some(Action::Yank),
        KeyCode::Char('/') => Some(Action::StartSearch),
        KeyCode::Char('p') => Some(Action::StartPost),
        KeyCode::Char('T') => Some(Action::StartThreadReply),
        KeyCode::Char('R') => Some(Action::StartReact),
        KeyCode::Char('r') => Some(Action::Refresh),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char(c @ '1'..='9') => Some(Action::SwitchTab((c as u8 - b'1') as usize)),
        _ => None,
    }
}

pub fn apply(action: Action, app: &mut App) -> bool {
    match action {
        Action::Quit => return true,
        Action::Up => app.move_selection(-1),
        Action::Down => app.move_selection(1),
        Action::PageUp => app.move_selection(-10),
        Action::PageDown => app.move_selection(10),
        Action::Home => app.move_selection(-(i32::MAX as isize)),
        Action::End => app.move_selection(i32::MAX as isize),
        Action::Enter => app.enter(),
        Action::Yank => app.yank(),
        Action::Refresh => app.refresh_active(),
        Action::NextTab => {
            let next = (app.active_tab + 1) % app.tabs.len();
            app.switch_tab(next);
        }
        Action::PrevTab => {
            let prev = if app.active_tab == 0 {
                app.tabs.len() - 1
            } else {
                app.active_tab - 1
            };
            app.switch_tab(prev);
        }
        Action::SwitchTab(i) => app.switch_tab(i),
        Action::StartSearch => app.start_search(),
        Action::StartPost => app.start_post(),
        Action::StartThreadReply => app.start_thread_reply(),
        Action::StartReact => {
            // v0.1: liking is the most common; pick by sub-key in a
            // follow-up. Status hint shows the picker keys.
            app.status =
                "react: l=like h=heart L=laugh s=surprised d=sad a=angry · Esc cancel".into();
            // The next character pressed is interpreted in ui::run via
            // a tiny one-shot read; for simplicity, v0.1 reacts with
            // 'like' on `R` so the action is always self-contained.
            // Follow-up will wire the picker key. For now, immediate
            // like:
            app.react("like");
        }
        Action::InputChar(c) => {
            app.input_char(c);
        }
        Action::InputBackspace => {
            app.input_backspace();
        }
        Action::SubmitText => {
            if app.post_mode.is_some() {
                app.submit_post();
            } else if app.active().data.search_mode {
                app.submit_search();
            }
        }
        Action::CancelText => {
            if app.post_mode.is_some() {
                app.cancel_post();
            } else if app.active().data.search_mode {
                app.cancel_search();
            }
        }
    }
    false
}
