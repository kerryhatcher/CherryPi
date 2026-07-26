use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::{debug, error, warn};

use cherrypi_core::event::Event;

/// Context piped to hook commands that opt into `stdin_context`.
/// Mirrors `StatusContext` but omits model info (hooks don't call the LLM).
#[derive(Debug, Clone, serde::Serialize)]
pub struct HookContext {
    pub workspace: super::status::WorkspaceInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Hook action types
#[derive(Debug, Clone)]
pub enum HookAction {
    /// Execute a shell command
    Command {
        command: String,
        plugin_root: String,
        stdin_context: bool,
    },
    /// Send a prompt to the LLM
    Prompt {
        prompt: String,
    },
    /// Invoke a sub-agent
    Agent {
        prompt: String,
    },
}

/// A hook rule that matches events and dispatches actions
#[derive(Debug, Clone)]
pub struct HookRule {
    pub event: String,
    pub matcher: Option<String>,
    pub actions: Vec<HookAction>,
}

/// Engine for executing hooks in response to events
pub struct HookEngine {
    rules: Vec<HookRule>,
    timeout: Duration,
}

impl HookEngine {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Set the execution timeout for hook commands
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Register hooks from a plugin's hook configuration
    pub fn register_from_config(
        &mut self,
        _plugin_name: &str,
        plugin_root: &str,
        config: &serde_json::Value,
    ) {
        let hooks = match config.get("hooks") {
            Some(h) => h,
            None => return,
        };

        let events = match hooks.as_object() {
            Some(obj) => obj,
            None => return,
        };

        for (event_name, rules) in events {
            let rules_array = match rules.as_array() {
                Some(arr) => arr,
                None => continue,
            };

            for rule in rules_array {
                let matcher = rule
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(String::from);

                let hook_actions = match rule.get("hooks").and_then(|h| h.as_array()) {
                    Some(actions) => actions,
                    None => continue,
                };

                let mut parsed_actions = Vec::new();
                for action in hook_actions {
                    let action_type = action
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("command");

                    match action_type {
                        "command" => {
                            if let Some(cmd) = action.get("command").and_then(|c| c.as_str()) {
                                let expanded = cmd.replace("${PLUGIN_ROOT}", plugin_root);
                                let stdin_context = action
                                    .get("stdin_context")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                parsed_actions.push(HookAction::Command {
                                    command: expanded,
                                    plugin_root: plugin_root.to_string(),
                                    stdin_context,
                                });
                            }
                        }
                        "prompt" => {
                            if let Some(prompt) = action.get("prompt").and_then(|p| p.as_str()) {
                                parsed_actions.push(HookAction::Prompt {
                                    prompt: prompt.to_string(),
                                });
                            }
                        }
                        "agent" => {
                            if let Some(prompt) = action.get("prompt").and_then(|p| p.as_str()) {
                                parsed_actions.push(HookAction::Agent {
                                    prompt: prompt.to_string(),
                                });
                            }
                        }
                        _ => {
                            warn!("Unknown hook action type: {}", action_type);
                        }
                    }
                }

                if !parsed_actions.is_empty() {
                    let action_count = parsed_actions.len();
                    self.rules.push(HookRule {
                        event: event_name.clone(),
                        matcher: matcher.clone(),
                        actions: parsed_actions,
                    });
                    debug!(
                        "Registered hook: {} (matcher: {:?}, {} actions)",
                        event_name,
                        matcher,
                        action_count
                    );
                }
            }
        }
    }

    /// Fire an event, executing all matching hooks.
    /// `ctx` provides runtime context (workspace dir, session id) piped to
    /// commands that have `stdin_context: true`.
    pub async fn fire(&self, event: &Event, ctx: &HookContext) -> Vec<HookResult> {
        let event_name = event_type_name(event);
        let matcher_context = event_matcher_context(event);

        let mut results = Vec::new();

        for rule in &self.rules {
            if rule.event != event_name {
                continue;
            }

            // Check matcher
            if let Some(ref matcher) = rule.matcher {
                if let Some(ref context) = matcher_context {
                    match regex::Regex::new(matcher) {
                        Ok(re) => {
                            if !re.is_match(context) {
                                continue;
                            }
                        }
                        Err(e) => {
                            error!("Invalid hook matcher regex '{}': {}", matcher, e);
                            continue;
                        }
                    }
                }
            }

            // Execute all actions in this rule
            for action in &rule.actions {
                let result = self.execute_action(action, event, ctx).await;
                results.push(result);
            }
        }

        results
    }

    async fn execute_action(
        &self,
        action: &HookAction,
        _event: &Event,
        ctx: &HookContext,
    ) -> HookResult {
        match action {
            HookAction::Command {
                command,
                stdin_context,
                ..
            } => {
                debug!(
                    "Executing hook command: {} (stdin_context={})",
                    command, stdin_context
                );

                let output = if *stdin_context {
                    Self::run_command_with_stdin(command, ctx)
                } else {
                    Command::new("sh")
                        .arg("-c")
                        .arg(command)
                        .output()
                };

                match output {
                    Ok(out) => {
                        if out.status.success() {
                            HookResult::Success {
                                action: format!("command: {}", command),
                                output: String::from_utf8_lossy(&out.stdout).to_string(),
                            }
                        } else {
                            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                            error!("Hook command '{}' failed: {}", command, stderr);
                            HookResult::Failure {
                                action: format!("command: {}", command),
                                error: stderr,
                            }
                        }
                    }
                    Err(e) => {
                        error!("Hook command '{}' could not execute: {}", command, e);
                        HookResult::Failure {
                            action: format!("command: {}", command),
                            error: e.to_string(),
                        }
                    }
                }
            }
            HookAction::Prompt { prompt } => {
                // Prompts are collected and returned to the agent for processing
                HookResult::Prompt {
                    prompt: prompt.clone(),
                }
            }
            HookAction::Agent { prompt } => {
                // Agent invocations are collected for the agent loop
                HookResult::AgentInvocation {
                    prompt: prompt.clone(),
                }
            }
        }
    }

    /// Run a command with context JSON piped to its stdin.
    /// Mirrors `StatusBarManager::poll_command_with_stdin()`.
    fn run_command_with_stdin(
        command: &str,
        ctx: &HookContext,
    ) -> std::io::Result<std::process::Output> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            let ctx_json = serde_json::to_string(ctx).unwrap_or_default();
            let _ = stdin.write_all(ctx_json.as_bytes());
            // stdin is dropped here, closing the pipe
        }

        child.wait_with_output()
    }
}

/// Result of executing a hook action
#[derive(Debug, Clone)]
pub enum HookResult {
    Success {
        action: String,
        output: String,
    },
    Failure {
        action: String,
        error: String,
    },
    Prompt {
        prompt: String,
    },
    AgentInvocation {
        prompt: String,
    },
}

/// Map Event enum variants to their string names
fn event_type_name(event: &Event) -> &'static str {
    match event {
        Event::SessionStart { .. } => "SessionStart",
        Event::SessionEnd { .. } => "SessionEnd",
        Event::UserPromptSubmit { .. } => "UserPromptSubmit",
        Event::PreToolUse { .. } => "PreToolUse",
        Event::PostToolUse { .. } => "PostToolUse",
        Event::PostToolUseFailure { .. } => "PostToolUseFailure",
        Event::BeforeReadFile { .. } => "BeforeReadFile",
        Event::AfterFileEdit { .. } => "AfterFileEdit",
        Event::BeforeShellExecution { .. } => "BeforeShellExecution",
        Event::AfterShellExecution { .. } => "AfterShellExecution",
        Event::Stop { .. } => "Stop",
        Event::SubagentStart { .. } => "SubagentStart",
        Event::SubagentStop { .. } => "SubagentStop",
        Event::PluginLoaded { .. } => "PluginLoaded",
        Event::PluginUnloaded { .. } => "PluginUnloaded",
    }
}

/// Get the matcher context string for an event
fn event_matcher_context(event: &Event) -> Option<String> {
    match event {
        Event::PreToolUse { tool_name, .. } => Some(tool_name.clone()),
        Event::PostToolUse { tool_name, .. } => Some(tool_name.clone()),
        Event::PostToolUseFailure { tool_name, .. } => Some(tool_name.clone()),
        Event::BeforeReadFile { path, .. } => Some(path.clone()),
        Event::AfterFileEdit { path, .. } => Some(path.clone()),
        Event::BeforeShellExecution { command, .. } => Some(command.clone()),
        Event::AfterShellExecution { command, .. } => Some(command.clone()),
        _ => None,
    }
}
