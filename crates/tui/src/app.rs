use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;
use tracing::{error, warn};

use cherrypi_agent::agent::Agent;
use cherrypi_agent::context::AgentContext;
use cherrypi_core::config::Config;
use cherrypi_core::event::ToolCallEvent;
use cherrypi_core::message::MessageRole;
use cherrypi_core::{CherryPiPaths, ChatLogger};
use cherrypi_llm::provider::{ProviderConfig, ProviderFactory};
use cherrypi_plugins::{StatusBarManager, StatusContext};

use super::theme::Theme;

use ratatui::widgets::ListState;
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TokioJoinHandle;

/// A registered slash command
#[derive(Debug, Clone)]
pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub usage: &'static str,
}

pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo { name: "/help",    description: "Show help",                  usage: "/help" },
    CommandInfo { name: "/quit",    description: "Exit CherryPi",              usage: "/quit" },
    CommandInfo { name: "/exit",    description: "Exit CherryPi",              usage: "/exit" },
    CommandInfo { name: "/clear",   description: "Clear chat history",         usage: "/clear" },
    CommandInfo { name: "/new",     description: "Start a new conversation",  usage: "/new" },
    CommandInfo { name: "/plugins", description: "Show plugin manager",        usage: "/plugins" },
    CommandInfo { name: "/models",  description: "List available models",      usage: "/models" },
    CommandInfo { name: "/model",  description: "Switch to a different model", usage: "/model <name>" },
    CommandInfo { name: "/copy",   description: "Copy last response",          usage: "/copy" },
    CommandInfo { name: "/chat",   description: "Return to chat view",         usage: "/chat" },
    CommandInfo { name: "/fff-health", description: "Show FFF file-index status",  usage: "/fff-health" },
    CommandInfo { name: "/fff-rescan", description: "Force FFF to rescan files",   usage: "/fff-rescan" },
    CommandInfo { name: "/plugin-add", description: "Install a crate and register as a CherryPi plugin", usage: "/plugin-add <crate-name>" },
    CommandInfo { name: "/plugin-remove", description: "Remove an installed plugin", usage: "/plugin-remove <plugin-name>" },
];

/// Application state
pub struct App {
    /// The agent. Shared so a turn can run on its own handle while the TUI keeps
    /// redrawing (spinner, live tool calls). Lock only when the agent is idle or
    /// from the processing future; never hold a guard across another lock.
    pub agent: std::sync::Arc<std::sync::Mutex<Agent>>,
    /// Current input text
    pub input: String,
    /// Chat messages (for display)
    pub messages: Vec<DisplayMessage>,
    /// Whether the agent is currently processing
    pub is_processing: bool,
    /// Status message
    pub status_message: String,
    /// Current view mode
    pub view_mode: ViewMode,
    /// Color theme
    pub theme: Theme,
    /// Scroll position in chat
    pub chat_scroll: usize,
    /// Whether to quit
    pub should_quit: bool,
    /// Last activity time
    pub last_activity: Instant,
    /// Plugin list
    pub plugin_list: Vec<String>,
    /// Available models
    pub available_models: Vec<String>,
    /// Current model
    pub current_model: String,
    /// Selected index in list views (model selector, plugin manager)
    pub list_selection: usize,
    /// Chat transcript logger
    pub chat_logger: ChatLogger,
    /// Persistent list state for model selector
    pub model_list_state: ListState,
    /// Persistent list state for plugin manager
    pub plugin_list_state: ListState,
    /// Cancel flag for aborting in-flight LLM requests
    pub cancel_flag: Arc<AtomicBool>,
    /// Spinner frame for processing indicator
    pub spinner_frame: usize,
    /// Filtered command suggestions (when typing a slash command)
    pub suggestions: Vec<CommandInfo>,
    /// Index of the highlighted suggestion
    pub suggestion_index: usize,
    /// History of submitted user inputs (for ↑/↓ recall)
    pub input_history: Vec<String>,
    /// Current position when browsing history; None = editing a fresh draft
    pub history_index: Option<usize>,
    /// Draft saved when entering history browsing, restored on ↓ past newest
    pub input_draft: String,
    /// Per-cwd file where input_history is persisted across restarts
    pub history_file: PathBuf,
    /// Status bar manager for plugin-provided status segments
    pub status_bar_manager: StatusBarManager,
    /// Platform data directory (e.g. ~/.local/share/cherrypi/)
    pub data_dir: PathBuf,
    /// Active plugin installation state, if any
    pub plugin_install: Option<PluginInstall>,
    /// Receiver for tool call events streamed from the agent during processing
    pub tool_call_rx: mpsc::UnboundedReceiver<ToolCallEvent>,
    /// Handle for the background agent processing task (None when idle)
    pub processing_handle: Option<TokioJoinHandle<Result<String, String>>>,
    /// The user input that was submitted for processing (stored for the background task)
    pub pending_input: String,
}

impl App {
    /// Returns true if a plugin install is currently running in the background.
    pub fn is_plugin_installing(&self) -> bool {
        self.plugin_install.is_some()
    }
}


/// A tool call that was executed by the agent, for display in the TUI
#[derive(Debug, Clone)]
pub struct ToolCallDisplay {
    pub tool_name: String,
    pub tool_input: String,
    pub tool_output: String,
    pub is_error: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Kind of a display message — normal text or a tool call box
#[derive(Debug, Clone, Default)]
pub enum MessageKind {
    #[default]
    Normal,
    ToolCall(ToolCallDisplay),
}

/// Display message with rendering metadata
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: MessageRole,
    pub content: String,
    pub kind: MessageKind,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub is_error: bool,
}

/// Current view mode
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewMode {
    Chat,
    FileViewer { path: String, content: String },
    PluginManager,
    ModelSelector,
    Help,
}

impl App {
    /// Create a new application
    pub fn new(config: Config, paths: &CherryPiPaths, chat_logger: ChatLogger) -> Result<Self, String> {
        let provider_config = ProviderConfig {
            endpoint: config.provider.endpoint.clone(),
            api_key: config.provider.api_key.clone(),
            model: config.provider.model.clone(),
            extra: config.provider.extra.clone(),
        };

        let provider = ProviderFactory::create(&config.provider.provider_type, provider_config)?;

        // Create tool call event channel (agent -> TUI)
        let (tool_call_tx, tool_call_rx) = mpsc::unbounded_channel();

        let context = AgentContext::new(config.clone(), provider, tool_call_tx);
        let mut agent = Agent::new(context);

        // Always scan the project-local plugins/ directory
        if let Ok(cwd) = std::env::current_dir() {
            let local_plugins = cwd.join("plugins");
            if local_plugins.is_dir() {
                agent.context_mut().plugin_loader.add_search_dir(local_plugins);
            }
        }

        agent.init_plugins(Some(paths.plugins_dir.clone()));

        // Inject command skills from plugins into the system prompt so the LLM
        // knows how to call plugin-provided CLI tools (e.g. gor for GitHub).
        inject_plugin_commands(agent.context_mut());

        // On startup, check if CherryPi's private cargo bin dir has any
        // installed binaries and add it to the path so the agent can find them.
        let cargo_bin_dir = paths.data_dir.join("cargo").join("bin");
        if cargo_bin_dir.is_dir() {
            agent.context_mut()
                .extra_bin_paths
                .lock()
                .unwrap()
                .push(cargo_bin_dir);
        }

        let plugin_list = agent.context().plugin_loader.list();
        let current_model = config.provider.model.clone();
        // Reuse the agent's shared cancel flag so the FFF grep tool's abort
        // signal stays in sync with the TUI's cancel button.
        let cancel_flag = agent.context().cancel_flag.clone();

        // Initialize status bar manager with plugin status bars
        let mut status_bar_manager = StatusBarManager::new();
        status_bar_manager.register_from_plugins(&agent.context().plugin_loader.enabled_plugins());

        // Resolve a per-cwd input-history file and load any saved entries.
        let cwd_str = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "".to_string());
        let history_file = history_file_path(&paths.history_dir, &cwd_str);
        let input_history = load_history(&history_file);

        Ok(Self {
            agent: std::sync::Arc::new(std::sync::Mutex::new(agent)),
            input: String::new(),
            messages: vec![DisplayMessage {
                role: MessageRole::System,
                content: format!(
                    "CherryPi v{} - AI Coding Agent\nType /help for commands, /quit to exit.\nModel: {}\nProvider: {}",
                    env!("CARGO_PKG_VERSION"),
                    current_model,
                    config.provider.provider_type,
                ),
                kind: MessageKind::Normal,
                timestamp: chrono::Utc::now(),
                is_error: false,
            }],
            is_processing: false,
            status_message: "Ready".to_string(),
            view_mode: ViewMode::Chat,
            theme: Theme::default(),
            chat_scroll: 0,
            should_quit: false,
            last_activity: Instant::now(),
            plugin_list,
            available_models: Vec::new(),
            current_model,
            list_selection: 0,
            chat_logger,
            model_list_state: ListState::default(),
            plugin_list_state: ListState::default(),
            cancel_flag,
            spinner_frame: 0,
            suggestions: Vec::new(),
            suggestion_index: 0,
            input_history,
            history_index: None,
            input_draft: String::new(),
            history_file,
            status_bar_manager,
            data_dir: paths.data_dir.clone(),
            plugin_install: None,
            tool_call_rx,
            processing_handle: None,
            pending_input: String::new(),
        })
    }

    /// Queue user input for processing. Adds the message to chat immediately,
    /// clears the input, and sets the processing flag.
    pub fn start_processing(&mut self) {
        let input = self.input.trim().to_string();
        if input.is_empty() {
            return;
        }

        // Commands are handled separately
        if input.starts_with('/') {
            return;
        }

        self.messages.push(DisplayMessage {
            role: MessageRole::User,
            content: input.clone(),
            kind: MessageKind::Normal,
            timestamp: chrono::Utc::now(),
            is_error: false,
        });
        self.chat_logger.log("user", &input);

        // Record in input history (skip consecutive duplicates)
        if self.input_history.last() != Some(&input) {
            self.input_history.push(input.clone());
            self.save_history();
        }
        self.history_index = None;
        self.input_draft.clear();

        self.input.clear();
        self.pending_input = input.clone();
        self.is_processing = true;
        crate::status::report(crate::status::TurnState::Working);
        self.cancel_flag.store(false, Ordering::SeqCst);
        self.status_message = "Thinking...".to_string();
        self.last_activity = Instant::now();
        self.chat_scroll = usize::MAX;
    }

    /// Drain tool call events from the agent channel into display messages.
    /// Returns the number of new tool call messages added.
    pub fn drain_tool_calls(&mut self) -> usize {
        let mut count = 0;
        while let Ok(tc) = self.tool_call_rx.try_recv() {
            // Pretty-print the JSON input
            let input_display = serde_json::to_string_pretty(&tc.tool_input)
                .unwrap_or_else(|_| format!("{:?}", tc.tool_input));
            let output_display = if tc.tool_output.len() > 2000 {
                format!("{}...\n[truncated, {} bytes total]",
                    &tc.tool_output[..2000], tc.tool_output.len())
            } else {
                tc.tool_output.clone()
            };

            self.messages.push(DisplayMessage {
                role: MessageRole::Tool,
                content: format!(
                    "Tool: {}\n\nInput:\n{}\n\nOutput:\n{}",
                    tc.tool_name, input_display, output_display
                ),
                kind: MessageKind::ToolCall(ToolCallDisplay {
                    tool_name: tc.tool_name,
                    tool_input: input_display,
                    tool_output: output_display,
                    is_error: tc.is_error,
                    timestamp: tc.timestamp,
                }),
                timestamp: tc.timestamp,
                is_error: tc.is_error,
            });
            count += 1;
        }
        if count > 0 {
            self.chat_scroll = usize::MAX;
        }
        count
    }

    /// Cancel in-flight processing
    pub fn cancel_processing(&mut self) {
        if self.is_processing {
            // If a plugin install is in progress, clean it up
            if self.plugin_install.is_some() {
                cancel_plugin_install(self);
                return;
            }
            self.cancel_flag.store(true, Ordering::SeqCst);
            self.is_processing = false;
            self.status_message = "Cancelled".to_string();
            self.messages.push(DisplayMessage {
                role: MessageRole::System,
                content: "Cancelled.".to_string(),
                kind: MessageKind::Normal,
                timestamp: chrono::Utc::now(),
                is_error: false,
            });
        }
    }

    /// Advance the spinner animation
    pub fn tick_spinner(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
    }

    /// Update command suggestions based on current input
    pub fn update_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestion_index = 0;

        if !self.input.starts_with('/') || self.input.len() < 2 {
            return;
        }

        let prefix = self.input.to_lowercase();
        self.suggestions = COMMANDS
            .iter()
            .filter(|c| c.name.to_lowercase().starts_with(&prefix))
            .cloned()
            .collect();

        // If exact match, clear suggestions (no need to suggest what's already typed)
        if self.suggestions.len() == 1 && self.suggestions[0].name == self.input {
            self.suggestions.clear();
        }
    }

    /// Accept the currently highlighted suggestion
    pub fn accept_suggestion(&mut self) {
        if let Some(cmd) = self.suggestions.get(self.suggestion_index) {
            self.input = cmd.name.to_string();
            // If the command takes args, add a space for the user to type them
            if cmd.usage.contains('<') {
                self.input.push(' ');
            }
            self.suggestions.clear();
        }
    }
    pub fn spinner_char(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner_frame]
    }

    /// Cycle to the previous (older) submitted input.
    /// Saves the current draft the first time we enter history browsing.
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                // Entering history: stash current input as the draft
                self.input_draft = self.input.clone();
                let idx = self.input_history.len() - 1;
                self.history_index = Some(idx);
                self.input = self.input_history[idx].clone();
            }
            Some(i) if i > 0 => {
                self.history_index = Some(i - 1);
                self.input = self.input_history[i - 1].clone();
            }
            _ => {} // already at oldest
        }
    }

    /// Cycle to the next (newer) submitted input.
    /// Restores the draft when moving past the newest entry.
    pub fn history_next(&mut self) {
        if let Some(i) = self.history_index {
                if i + 1 < self.input_history.len() {
                    self.history_index = Some(i + 1);
                    self.input = self.input_history[i + 1].clone();
                } else {
                    // Past the newest — restore the saved draft
                    self.history_index = None;
                    self.input = self.input_draft.clone();
                }
        }
    }

    /// Persist the current input history to its per-cwd file.
    /// Best-effort: errors are logged but never surface to the user.
    pub fn save_history(&self) {
        let dir = match self.history_file.parent() {
            Some(d) => d,
            None => return,
        };
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!("Failed to create history dir {}: {}", dir.display(), e);
            return;
        }
        // Cap retained history to keep the file manageable.
        const MAX_HISTORY: usize = 1000;
        let entries: &[String] = if self.input_history.len() > MAX_HISTORY {
            &self.input_history[self.input_history.len() - MAX_HISTORY..]
        } else {
            &self.input_history
        };
        let payload = serde_json::json!({
            "cwd": std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            "entries": entries,
        });
        match serde_json::to_string_pretty(&payload) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&self.history_file, text) {
                    warn!("Failed to write history file {}: {}", self.history_file.display(), e);
                }
            }
            Err(e) => warn!("Failed to serialize input history: {}", e),
        }
    }

    /// Build a StatusContext and tick the status bar manager.
    /// Call this each frame before rendering.
    pub fn tick_status_bar(&mut self) {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".to_string());

        let ctx = StatusContext {
            model: cherrypi_plugins::ModelInfo {
                display_name: self.current_model.clone(),
            },
            workspace: cherrypi_plugins::WorkspaceInfo {
                current_dir: cwd,
            },
            session_id: None,
        };
        self.status_bar_manager.tick(&ctx);
    }

    /// Handle the result of a completed agent run
    pub fn finish_processing(&mut self, result: Result<String, String>) {
        // Drain any tool call events accumulated during processing
        self.drain_tool_calls();
        
        self.is_processing = false;
        self.processing_handle = None;
        self.pending_input.clear();
        crate::status::report(match &result {
            Ok(_) => crate::status::TurnState::Done,
            Err(e) if e == "Cancelled" => crate::status::TurnState::Idle,
            Err(_) => crate::status::TurnState::Error,
        });
        match result {
            Ok(response) => {
                self.chat_logger.log("assistant", &response);
                self.messages.push(DisplayMessage {
                    role: MessageRole::Assistant,
                    content: response,
                    kind: MessageKind::Normal,
                    timestamp: chrono::Utc::now(),
                    is_error: false,
                });
                self.chat_scroll = usize::MAX;
                self.status_message = "Ready".to_string();
            }
            Err(e) => {
                if e != "Cancelled" {
                    error!("Agent processing error: {}", e);
                    self.chat_logger.log("error", &e);
                    self.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: format!("Error: {}", e),
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: true,
                    });
                    self.status_message = "Error occurred".to_string();
                }
            }
        }
        self.last_activity = Instant::now();
    }

    pub async fn handle_command(&mut self, input: &str) {
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let cmd = parts[0].to_lowercase();
        let _args = parts.get(1).unwrap_or(&"");

        match cmd.as_str() {
            "/help" => {
                self.messages.push(DisplayMessage {
                    role: MessageRole::System,
                    content: HELP_TEXT.to_string(),
                    kind: MessageKind::Normal,
                    timestamp: chrono::Utc::now(),
                    is_error: false,
                });
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
            }
            "/clear" => {
                self.messages.clear();
                self.messages.push(DisplayMessage {
                    role: MessageRole::System,
                    content: "Chat cleared.".to_string(),
                    kind: MessageKind::Normal,
                    timestamp: chrono::Utc::now(),
                    is_error: false,
                });
            }
            "/new" => {
                self.agent.lock().unwrap().reset_conversation();
                self.messages.clear();
                self.messages.push(DisplayMessage {
                    role: MessageRole::System,
                    content: "New conversation started.".to_string(),
                    kind: MessageKind::Normal,
                    timestamp: chrono::Utc::now(),
                    is_error: false,
                });
                self.status_message = "Ready".to_string();
            }
            "/plugins" => {
                self.view_mode = ViewMode::PluginManager;
                self.list_selection = 0;
                self.plugin_list = self.agent.lock().unwrap().context().plugin_loader.list();
            }
            "/models" => {
                self.view_mode = ViewMode::ModelSelector;
                self.list_selection = 0;
                self.status_message = "Loading models...".to_string();
                match self.agent.lock().unwrap().context().provider.list_models().await {
                    Ok(models) => {
                        self.available_models = models;
                        if let Some(pos) = self.available_models.iter().position(|m| m == &self.current_model) {
                            self.list_selection = pos;
                        }
                        self.status_message = format!("{} models available | ↑↓ to navigate, Enter to select, Esc to cancel", self.available_models.len());
                    }
                    Err(e) => {
                        error!("Failed to list models: {}", e);
                        self.status_message = format!("Failed to list models: {}", e);
                    }
                }
            }
            "/model" => {
                if let Some(model) = parts.get(1) {
                    self.current_model = model.to_string();
                    self.agent.lock().unwrap().set_model(self.current_model.clone());
                    self.status_message = format!("Model set to: {}", model);
                    self.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: format!("Switched to model: {}", model),
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: false,
                    });
                }
            }
            "/chat" => {
                self.view_mode = ViewMode::Chat;
            }
            "/fff-health" => {
                self.show_fff_health().await;
            }
            "/fff-rescan" => {
                self.trigger_fff_rescan().await;
            }
            "/copy" => {
                let last = self.messages.iter().rev()
                    .find(|m| m.role == MessageRole::Assistant);
                match last {
                    Some(msg) => {
                        match arboard::Clipboard::new() {
                            Ok(mut clipboard) => {
                                match clipboard.set_text(&msg.content) {
                                    Ok(_) => self.status_message = "Copied last response to clipboard".to_string(),
                                    Err(e) => self.status_message = format!("Clipboard error: {}", e),
                                }
                            }
                            Err(e) => self.status_message = format!("Clipboard error: {}", e),
                        }
                    }
                    None => {
                        self.status_message = "No assistant response to copy".to_string();
                    }
                }
            }
            "/plugin-add" => {
                if self.plugin_install.is_some() {
                    self.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: "A plugin installation is already in progress. Press Esc to cancel it first.".to_string(),
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: true,
                    });
                    return;
                }

                let crate_name = parts.get(1).unwrap_or(&"").trim().to_string();
                if crate_name.is_empty() {
                    self.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: "Usage: /plugin-add <crate-name>\nExample: /plugin-add gor-cli".to_string(),
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: true,
                    });
                    return;
                }

                start_plugin_install(self, crate_name);
            }
            "/plugin-remove" => {
                let plugin_name = parts.get(1).unwrap_or(&"").trim();
                if plugin_name.is_empty() {
                    self.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: format!(
                            "Usage: /plugin-remove <plugin-name>\n\nInstalled plugins: {}",
                            self.plugin_list.join(", ")
                        ),
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: true,
                    });
                    return;
                }

                let uninstall_result = self.agent.lock().unwrap().context_mut().plugin_loader.uninstall(plugin_name);
                match uninstall_result {
                    Ok(_) => {
                        self.plugin_list = self.agent.lock().unwrap().context().plugin_loader.list();
                        self.messages.push(DisplayMessage {
                            role: MessageRole::System,
                            content: format!("Plugin '{}' removed.", plugin_name),
                            kind: MessageKind::Normal,
                            timestamp: chrono::Utc::now(),
                            is_error: false,
                        });
                        self.status_message = format!("Plugin '{}' removed", plugin_name);
                    }
                    Err(e) => {
                        self.messages.push(DisplayMessage {
                            role: MessageRole::System,
                            content: format!("Failed to remove plugin '{}': {}", plugin_name, e),
                            kind: MessageKind::Normal,
                            timestamp: chrono::Utc::now(),
                            is_error: true,
                        });
                        self.status_message = "Failed to remove plugin".to_string();
                    }
                }
            }
            _ => {
                self.messages.push(DisplayMessage {
                    role: MessageRole::System,
                    content: format!("Unknown command: {}. Type /help for available commands.", cmd),
                    kind: MessageKind::Normal,
                    timestamp: chrono::Utc::now(),
                    is_error: true,
                });
            }
        }
    }

    pub async fn load_models(&mut self) {
        match self.agent.lock().unwrap().context().provider.list_models().await {
            Ok(models) => {
                self.available_models = models;
            }
            Err(e) => {
                warn!("Failed to load models on startup: {}", e);
                self.status_message = format!("Failed to load models: {}", e);
            }
        }
    }

    /// Show FFF file-index health: scan progress, watcher state, file count.
    async fn show_fff_health(&mut self) {
        let picker = match self.agent.lock().unwrap().context().fff_picker.clone() {
            Some(p) => p,
            None => {
                self.messages.push(DisplayMessage {
                    role: MessageRole::System,
                    content: "FFF is not initialised. grep/find are using subprocess fallbacks.".into(),
                    kind: MessageKind::Normal,
                    timestamp: chrono::Utc::now(),
                    is_error: false,
                });
                return;
            }
        };

        let (scanned, is_scanning, watcher_ready, warmup, live, base) =
            match picker.read() {
                Ok(guard) => match guard.as_ref() {
                    Some(p) => {
                        let prog = p.get_scan_progress();
                        (
                            prog.scanned_files_count,
                            prog.is_scanning,
                            prog.is_watcher_ready,
                            prog.is_warmup_complete,
                            p.live_file_count(),
                            p.base_path().display().to_string(),
                        )
                    }
                    None => {
                        self.messages.push(DisplayMessage {
                            role: MessageRole::System,
                            content: "FFF picker handle is empty.".into(),
                            kind: MessageKind::Normal,
                            timestamp: chrono::Utc::now(),
                            is_error: true,
                        });
                        return;
                    }
                },
                Err(e) => {
                    self.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: format!("Failed to acquire FFF picker lock: {}", e),
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: true,
                    });
                    return;
                }
            };

        let scan_state = if is_scanning { "scanning" } else { "idle" };
        let watcher_state = if watcher_ready { "ready" } else { "warming up" };
        let warmup_state = if warmup { "complete" } else { "pending" };
        self.messages.push(DisplayMessage {
            role: MessageRole::System,
            content: format!(
                "FFF file-index health\n  base:      {}\n  files:     {} live ({} scanned)\n  scan:      {}\n  watcher:   {}\n  warmup:    {}",
                base, live, scanned, scan_state, watcher_state, warmup_state
            ),
            kind: MessageKind::Normal,
            timestamp: chrono::Utc::now(),
            is_error: false,
        });
        self.status_message = "FFF health shown".to_string();
    }

    /// Trigger an async FFF rescan (non-blocking).
    async fn trigger_fff_rescan(&mut self) {
        let picker = self.agent.lock().unwrap().context().fff_picker.clone();
        let frecency = self.agent.lock().unwrap().context().fff_frecency.clone();
        match (picker, frecency) {
            (Some(picker), Some(frecency)) => {
                match picker.trigger_full_rescan_async(&frecency) {
                    Ok(()) => {
                        self.messages.push(DisplayMessage {
                            role: MessageRole::System,
                            content: "FFF rescan triggered.".into(),
                            kind: MessageKind::Normal,
                            timestamp: chrono::Utc::now(),
                            is_error: false,
                        });
                        self.status_message = "FFF rescan started".to_string();
                    }
                    Err(e) => {
                        self.messages.push(DisplayMessage {
                            role: MessageRole::System,
                            content: format!("FFF rescan failed: {}", e),
                            kind: MessageKind::Normal,
                            timestamp: chrono::Utc::now(),
                            is_error: true,
                        });
                    }
                }
            }
            _ => {
                self.messages.push(DisplayMessage {
                    role: MessageRole::System,
                    content: "FFF is not initialised; nothing to rescan.".into(),
                    kind: MessageKind::Normal,
                    timestamp: chrono::Utc::now(),
                    is_error: false,
                });
            }
        }
    }
}

/// Tracks an in-flight plugin installation.
pub struct PluginInstall {
    /// Crate name being installed
    pub crate_name: String,
    /// Background thread handle (waited on in poll_plugin_install)
    thread: Option<JoinHandle<()>>,
    /// Shared progress: lines of cargo output + completion flag
    pub progress: Arc<Mutex<InstallProgress>>,
}

#[derive(Default)]
pub struct InstallProgress {
    /// Cargo stdout/stderr lines captured so far
    pub lines: Vec<String>,
    /// Whether the install has finished
    pub done: bool,
    /// Whether the install succeeded
    pub success: bool,
    /// Final result message (set when done)
    pub result_message: Option<String>,
    /// Path to the installed plugin directory (set after successful plugin copy)
    pub plugin_path: Option<PathBuf>,
    /// Path to the installed binary (set after successful install)
    pub binary_path: Option<PathBuf>,
    /// Actual binary name(s) found in the cargo bin directory after install
    pub installed_binaries: Vec<String>,
}

/// Start installing a plugin in the background.
/// Sets `is_processing` and stores the thread handle for polling.
pub fn start_plugin_install(app: &mut App, crate_name: String) {
    let data_dir = app.data_dir.clone();
    let progress = Arc::new(Mutex::new(InstallProgress::default()));
    let progress_clone = progress.clone();
    let name = crate_name.clone();

    // Prepare cargo home inside CherryPi's data dir
    let cargo_home = data_dir.join("cargo");
    let cargo_bin_dir = cargo_home.join("bin");
    let _ = std::fs::create_dir_all(&cargo_bin_dir);
    let cargo_home_str = cargo_home.to_string_lossy().to_string();

    let log = progress_clone;

    // Snapshot binaries that exist BEFORE install so we can detect new ones later
    let before_bins: std::collections::HashSet<String> = std::fs::read_dir(&cargo_bin_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();

    // Push an initial status message
    app.messages.push(DisplayMessage {
        role: MessageRole::System,
        content: format!("Installing plugin '{}'... (this may take a few minutes)\n", crate_name),
        kind: MessageKind::Normal,
        timestamp: chrono::Utc::now(),
        is_error: false,
    });

    let thread = std::thread::spawn(move || {
        // --- Step 0: note binaries that already exist so we can detect new ones ---
        let before_bins = before_bins;

        // --- Step 1: cargo install (force to ensure fresh download) ---
        let mut child = match std::process::Command::new("cargo")
            .env("CARGO_HOME", &cargo_home_str)
            .args(["install", "--force", &name])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let mut p = log.lock().unwrap();
                p.lines.push(format!("Error: failed to spawn cargo: {}", e));
                p.done = true;
                p.success = false;
                p.result_message = Some(format!("Failed to run cargo install: {}", e));
                return;
            }
        };

        // Read stdout line by line
        if let Some(stdout) = child.stdout.take() {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let mut p = log.lock().unwrap();
                p.lines.push(line);
            }
        }
        // Read stderr line by line (cargo outputs progress here)
        if let Some(stderr) = child.stderr.take() {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut p = log.lock().unwrap();
                p.lines.push(line);
            }
        }

        let status = child.wait().ok();

        // --- Step 2: check result ---
        let success = status.map(|s| s.success()).unwrap_or(false);

        if !success {
            let mut p = log.lock().unwrap();
            p.done = true;
            p.success = false;
            p.result_message = Some(format!("Failed to install crate '{}'", name));
            return;
        }

        // --- Step 3: detect actual binary name(s) installed ---
        let new_bins: Vec<String> = std::fs::read_dir(&cargo_bin_dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().into_string().ok()?;
                        if name.starts_with('.') {
                            return None;
                        }
                        // Only report files, not symlinks to .dotfile dirs etc.
                        e.file_type().ok().filter(|t| t.is_file())?;
                        // Only report binaries that weren't there before
                        if before_bins.contains(&name) {
                            return None;
                        }
                        Some(name)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let bin_path = new_bins.first().map(|b| cargo_bin_dir.join(b));
        let bin_names_str = if new_bins.is_empty() {
            "(unknown)".to_string()
        } else {
            new_bins.join(", ")
        };

        {
            let mut p = log.lock().unwrap();
            p.binary_path = bin_path;
            p.installed_binaries = new_bins.clone();
        }

        // --- Step 4: search for plugin manifest in registry source ---
        let cargo_home = PathBuf::from(&cargo_home_str);
        let plugin_src = find_plugin_in_registry(&name, &cargo_home);

        match plugin_src {
            Some(plugin_dir) => {
                let mut p = log.lock().unwrap();
                p.plugin_path = Some(plugin_dir);
                p.done = true;
                p.success = true;
                p.result_message = Some(format!(
                    "Plugin '{}' installed. Copying plugin files...\n  Binaries: {}/{}",
                    name,
                    cargo_bin_dir.display(),
                    bin_names_str,
                ));
            }
            None => {
                let mut p = log.lock().unwrap();
                p.done = true;
                p.success = true;
                p.result_message = Some(format!(
                    "Crate '{}' installed successfully to {}/{}\n\
                     No CherryPi plugin manifest (.plugin/plugin.json) found.\n\
                     The binary is available for the agent to call via the bash tool.",
                    name,
                    cargo_bin_dir.display(),
                    bin_names_str,
                ));
            }
        }
    });

    app.plugin_install = Some(PluginInstall {
        crate_name,
        thread: Some(thread),
        progress,
    });
    app.is_processing = true;
    app.status_message = "Installing plugin...".to_string();
    app.cancel_flag.store(false, Ordering::SeqCst);
}

/// Poll the current plugin installation.
/// Should be called each frame while `app.is_plugin_installing()` is true.
///
/// Returns `true` if the install is still running, `false` if complete.
pub fn poll_plugin_install(app: &mut App) -> bool {
    let install = match app.plugin_install.as_ref() {
        Some(i) => i,
        None => return false,
    };

    let done = {
        let p = install.progress.lock().unwrap();
        p.done
    };

    if !done {
        return true;
    }

    // Installation finished — take ownership and process result
    let mut install = app.plugin_install.take().unwrap();
    if let Some(handle) = install.thread.take() {
        let _ = handle.join();
    }

    let progress = install.progress.lock().unwrap();

    if progress.success {
        if let Some(ref plugin_dir) = progress.plugin_path {
            let install_result = app.agent.lock().unwrap().context_mut().plugin_loader.install(
                &install.crate_name,
                plugin_dir,
            );
            match install_result {
                Ok(_) => {
                    app.plugin_list = app.agent.lock().unwrap().context().plugin_loader.list();
                    let msg = match progress.result_message.as_deref() {
                        Some(m) => format!("{}\n  Plugin: {}/plugins/{}",
                            m, app.data_dir.display(), install.crate_name),
                        None => format!("Plugin '{}' installed.", install.crate_name),
                    };
                    app.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: msg,
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: false,
                    });
                }
                Err(e) => {
                    app.messages.push(DisplayMessage {
                        role: MessageRole::System,
                        content: format!(
                            "Crate '{}' installed, but plugin setup failed: {}",
                            install.crate_name, e
                        ),
                        kind: MessageKind::Normal,
                        timestamp: chrono::Utc::now(),
                        is_error: true,
                    });
                }
            }
        } else {
            let msg = progress.result_message.as_deref()
                .unwrap_or(&format!("Crate '{}' installed.", install.crate_name))
                .to_string();
            app.messages.push(DisplayMessage {
                role: MessageRole::System,
                content: msg,
                kind: MessageKind::Normal,
                timestamp: chrono::Utc::now(),
                is_error: false,
            });
        }

        app.status_message = format!("Plugin '{}' installed", install.crate_name);

        // Ensure the cargo bin directory is on PATH for future bash calls
        let cargo_bin_dir = app.data_dir.join("cargo").join("bin");
        if cargo_bin_dir.is_dir() {
            app.agent.lock().unwrap().context_mut()
                .extra_bin_paths
                .lock()
                .unwrap()
                .push(cargo_bin_dir);
        }
    } else {
        let msg = progress.result_message.as_deref()
            .unwrap_or("Plugin installation failed.")
            .to_string();
        app.messages.push(DisplayMessage {
            role: MessageRole::System,
            content: msg,
            kind: MessageKind::Normal,
            timestamp: chrono::Utc::now(),
            is_error: true,
        });
        app.status_message = "Install failed".to_string();
    }

    app.is_processing = false;
    false
}

/// Cancel an in-flight plugin install.
pub fn cancel_plugin_install(app: &mut App) {
    if let Some(mut install) = app.plugin_install.take() {
        install.thread.take();
        let mut p = install.progress.lock().unwrap();
        p.done = true;
        p.success = false;
        drop(p);

        app.messages.push(DisplayMessage {
            role: MessageRole::System,
            content: format!("Plugin install '{}' cancelled.", install.crate_name),
            kind: MessageKind::Normal,
            timestamp: chrono::Utc::now(),
            is_error: true,
        });
        app.is_processing = false;
        app.status_message = "Install cancelled".to_string();
        app.cancel_flag.store(true, Ordering::SeqCst);
    }
}

pub const HELP_TEXT: &str = r#"
CherryPi Commands:
  /help          Show this help
  /quit, /exit   Exit CherryPi
  /clear         Clear chat history
  /plugins       Show plugin manager
  /models        List available models
  /model <name>  Switch to a different model
  /copy          Copy last assistant response to clipboard
  /chat          Return to chat view
  /fff-health    Show FFF file-index status
  /fff-rescan    Force FFF to rescan files
  /plugin-add    Install a crate and register as a CherryPi plugin
  /plugin-remove Remove an installed plugin

Keyboard Shortcuts:
  Ctrl+C         Quit
  Ctrl+N         New conversation
  Ctrl+V         Paste from clipboard
  Enter          Send message
  Shift+Enter    New line in input
  Esc            Cancel processing / return to chat
  Tab            Switch view / accept suggestion
  ↑↓             Cycle input history (or command suggestions)
  PageUp/Down    Scroll chat

Copying text:
  Select text with your mouse as usual — the terminal handles
  selection natively. Use Ctrl+Shift+C or right-click to copy
  the selection to your system clipboard.
"#;

const SPINNER_FRAMES: &[&str] = &[
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];

/// Search the cargo registry source cache (inside CherryPi's self-contained cargo home)
/// for a plugin manifest (.plugin/plugin.json) belonging to a crate.
/// Returns the root directory of the matching crate if found.
///
/// Cargo stores sources at: `$CARGO_HOME/registry/src/<hash>/<crate>-<version>/`
/// so we must scan one level deeper for the hash-named intermediate directories.
fn find_plugin_in_registry(crate_name: &str, cargo_home: &Path) -> Option<PathBuf> {
    let registry_base = cargo_home.join("registry").join("src");

    let hash_dirs = std::fs::read_dir(&registry_base).ok()?;
    for hash_entry in hash_dirs.flatten() {
        let hash_dir = hash_entry.path();
        if !hash_dir.is_dir() {
            continue;
        }
        // Each hash directory (e.g. index.crates.io-xxxxx) contains crate source dirs
        let crate_dirs = std::fs::read_dir(&hash_dir).ok()?;
        for crate_entry in crate_dirs.flatten() {
            let crate_dir = crate_entry.path();
            if !crate_dir.is_dir() {
                continue;
            }
            // Look for a subdirectory starting with the crate name
            // e.g. `gor-cli-0.1.0/` starts with `gor-cli`
            let dirname = crate_dir.file_name()?.to_str()?;
            if !dirname.starts_with(crate_name) {
                continue;
            }
            // Check for plugin manifest
            let manifest = crate_dir.join(".plugin").join("plugin.json");
            if manifest.exists() {
                return Some(crate_dir);
            }
        }
    }
    None
}

/// Inject command skills from all enabled plugins into the conversation
/// as a system message so the LLM knows how to use plugin-provided CLI tools.
fn inject_plugin_commands(ctx: &mut cherrypi_agent::context::AgentContext) {
    let commands = ctx.plugin_loader.all_commands();
    if commands.is_empty() {
        return;
    }

    let mut parts = Vec::new();
    parts.push(
        "\n## Available Plugin Commands\n\
         The following CLI tools are installed via plugins and can be called \
         using the `bash` tool. Use them for their specific domains.\n"
            .to_string(),
    );

    for (plugin_name, cmd) in &commands {
        if cmd.disable_model_invocation {
            continue;
        }

        let description = cmd
            .description
            .as_deref()
            .unwrap_or("No description");

        parts.push(format!("\n### {} ({})", cmd.name, plugin_name));
        parts.push(format!("> {}", description));

        // Read the full command markdown content to include usage examples
        if let Ok(content) = std::fs::read_to_string(&cmd.path) {
            // Strip frontmatter if present
            let body = if let Some(end) = content.trim_start().strip_prefix("---") {
                if let Some(end_pos) = end.find("---") {
                    end[end_pos + 3..].trim()
                } else {
                    content.trim()
                }
            } else {
                content.trim()
            };

            if !body.is_empty() {
                parts.push(body.to_string());
            }
        }
    }

    let system_text = parts.join("\n");
    ctx.conversation
        .add_message(cherrypi_core::message::Message::system(&system_text));
}

/// FNV-1a 64-bit hash. Used to derive a stable, collision-resistant
/// filename for a given working directory without pulling in a crate.
fn fnv1a_64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in s.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Build the per-cwd input-history file path: `<history_dir>/input_<hash>.json`.
/// Keyed by the absolute working directory so each project has its own history.
fn history_file_path(history_dir: &Path, cwd: &str) -> PathBuf {
    let hash = fnv1a_64(cwd);
    history_dir.join(format!("input_{:016x}.json", hash))
}

/// Load the saved input history for the current cwd, if any.
/// Returns an empty vec on missing/corrupt files (logged at warn).
fn load_history(path: &PathBuf) -> Vec<String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!("Failed to read history file {}: {}", path.display(), e);
            return Vec::new();
        }
    };
    match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(v) => v
            .get("entries")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            warn!("Corrupt history file {}: {}", path.display(), e);
            Vec::new()
        }
    }
}
