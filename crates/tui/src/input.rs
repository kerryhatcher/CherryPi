use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, ViewMode, MessageKind};

/// Input events that the TUI can process
#[derive(Debug)]
pub enum InputEvent {
    Key(KeyEvent),
    Tick,
    Resize,
}

/// Handle keyboard input and update app state.
/// Returns true if a command needs async processing.
pub fn handle_input(app: &mut App, key: KeyEvent) -> bool {
    match app.view_mode {
        ViewMode::Chat | ViewMode::Help => handle_chat_input(app, key),
        ViewMode::FileViewer { .. } => handle_file_viewer_input(app, key),
        ViewMode::PluginManager => handle_plugin_manager_input(app, key),
        ViewMode::ModelSelector => handle_model_selector_input(app, key),
    }
}

fn handle_chat_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                app.input.push('\n');
            } else if !app.is_processing {
                if app.input.trim().starts_with('/') {
                    return true; // Signal that a command needs async processing
                }
                app.start_processing();
                return true; // Signal that agent processing should start
            }
        }
        KeyCode::Char(c) => {
            // Ctrl+V or Ctrl+Shift+V = paste from clipboard
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && (c == 'v' || c == 'V')
            {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() {
                        app.input.push_str(&text);
                    }
                }
            } else {
                app.input.push(c);
            }
            // Update slash-command suggestions
            if app.input.starts_with('/') {
                app.update_suggestions();
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
            if app.input.starts_with('/') {
                app.update_suggestions();
            } else {
                app.suggestions.clear();
            }
        }
        KeyCode::Tab => {
            if !app.suggestions.is_empty() {
                app.accept_suggestion();
            } else {
                app.view_mode = ViewMode::PluginManager;
            }
        }
        KeyCode::Esc => {
            app.view_mode = ViewMode::Chat;
        }
        KeyCode::Up => {
            if !app.suggestions.is_empty() {
                if app.suggestion_index > 0 {
                    app.suggestion_index -= 1;
                }
            } else {
                app.history_prev();
            }
        }
        KeyCode::Down => {
            if !app.suggestions.is_empty() {
                if app.suggestion_index + 1 < app.suggestions.len() {
                    app.suggestion_index += 1;
                }
            } else {
                app.history_next();
            }
        }
        KeyCode::PageUp => {
            app.chat_scroll = app.chat_scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            app.chat_scroll = app.chat_scroll.saturating_add(10);
        }
        _ => {}
    }
    false
}

fn handle_file_viewer_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.view_mode = ViewMode::Chat;
        }
        KeyCode::Up => {
            if app.chat_scroll > 0 {
                app.chat_scroll -= 1;
            }
        }
        KeyCode::Down => {
            app.chat_scroll += 1;
        }
        _ => {}
    }
    false
}

fn handle_plugin_manager_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.view_mode = ViewMode::Chat;
        }
        KeyCode::Tab => {
            app.view_mode = ViewMode::ModelSelector;
        }
        KeyCode::Up => {
            if app.list_selection > 0 {
                app.list_selection -= 1;
            }
        }
        KeyCode::Down if app.list_selection + 1 < app.plugin_list.len() => {
            app.list_selection += 1;
        }
        _ => {}
    }
    false
}

fn handle_model_selector_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.view_mode = ViewMode::Chat;
        }
        KeyCode::Tab => {
            app.view_mode = ViewMode::Chat;
        }
        KeyCode::Up => {
            if app.list_selection > 0 {
                app.list_selection -= 1;
            }
        }
        KeyCode::Down => {
            if app.list_selection + 1 < app.available_models.len() {
                app.list_selection += 1;
            }
        }
        KeyCode::Enter if app.list_selection < app.available_models.len() => {
            let model = app.available_models[app.list_selection].clone();
            app.current_model = model.clone();
            app.agent.lock().unwrap().set_model(model.clone());
            app.status_message = format!("Model set to: {}", model);
            app.messages.push(super::app::DisplayMessage {
                role: cherrypi_core::message::MessageRole::System,
                content: format!("Switched to model: {}", model),
                kind: MessageKind::Normal,
                timestamp: chrono::Utc::now(),
                is_error: false,
            });
            app.view_mode = ViewMode::Chat;
        }
        _ => {}
    }
    false
}
