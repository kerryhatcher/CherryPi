use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use super::manifest::StatusBarConfig;

/// Context passed to status bar commands that opt into stdin_context.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusContext {
    pub model: ModelInfo,
    pub workspace: WorkspaceInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub display_name: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceInfo {
    pub current_dir: String,
}

/// A cached status bar segment from a plugin
#[derive(Debug, Clone)]
pub struct StatusSegment {
    /// Plugin name this segment comes from
    pub plugin_name: String,
    /// Optional label (from manifest)
    pub label: Option<String>,
    /// The rendered text
    pub text: String,
    /// Whether the last poll was successful
    pub healthy: bool,
}

/// Internal entry tracking one registered status bar.
struct Entry {
    config: StatusBarConfig,
    cache: StatusSegment,
    last_poll: Instant,
}

/// Manages polling of status bar commands from plugins.
///
/// Each plugin can declare a `status_bar` in its manifest with a shell command.
/// The manager runs each command on its configured refresh interval and caches
/// the output. The TUI calls `segments()` each frame to get the latest.
pub struct StatusBarManager {
    entries: Vec<Entry>,
}

impl Default for StatusBarManager {
    fn default() -> Self { Self::new() }
}

impl StatusBarManager {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Register status bars from loaded plugins
    pub fn register_from_plugins(
        &mut self,
        plugins: &[&super::loader::LoadedPlugin],
    ) {
        for plugin in plugins {
            if !plugin.enabled {
                continue;
            }
            for sb in &plugin.components.status_bars {
                // Expand ${PLUGIN_ROOT} in the command
                let expanded_cmd = sb.command.replace(
                    "${PLUGIN_ROOT}",
                    &plugin.root.to_string_lossy(),
                );
                let mut config = sb.clone();
                config.command = expanded_cmd;

                debug!(
                    "Registered status bar '{}' from plugin '{}': {} ({}ms, stdin_context={})",
                    sb.label.as_deref().unwrap_or("unnamed"),
                    plugin.name,
                    config.command,
                    config.refresh_ms,
                    config.stdin_context,
                );

                self.entries.push(Entry {
                    config,
                    cache: StatusSegment {
                        plugin_name: plugin.name.clone(),
                        label: sb.label.clone(),
                        text: String::new(),
                        healthy: true,
                    },
                    last_poll: Instant::now() - Duration::from_secs(3600), // force first poll
                });
            }
        }
    }

    /// Call each frame. Polls commands whose refresh interval has elapsed.
    ///
    /// `ctx` is piped as JSON to stdin for plugins with `stdin_context: true`.
    pub fn tick(&mut self, ctx: &StatusContext) {
        let now = Instant::now();
        let ctx_json = serde_json::to_string(ctx).unwrap_or_default();

        for entry in &mut self.entries {
            let elapsed = now.duration_since(entry.last_poll);
            let interval = Duration::from_millis(entry.config.refresh_ms);

            if elapsed < interval {
                continue;
            }
            entry.last_poll = now;

            let output = if entry.config.stdin_context {
                Self::poll_command_with_stdin(&entry.config.command, &ctx_json)
            } else {
                Self::poll_command(&entry.config.command)
            };

            match output {
                Ok(text) => {
                    entry.cache.text = text.trim().to_string();
                    entry.cache.healthy = true;
                }
                Err(e) => {
                    warn!(
                        "Status bar command '{}' failed: {}",
                        entry.config.command, e
                    );
                    entry.cache.text = "(error)".to_string();
                    entry.cache.healthy = false;
                }
            }
        }
    }

    /// Get all current status segments for rendering
    pub fn segments(&self) -> Vec<&StatusSegment> {
        self.entries.iter().map(|e| &e.cache).collect()
    }

    fn poll_command(command: &str) -> Result<String, String> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|e| format!("failed to execute: {}", e))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.is_empty() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                Err(stderr.trim().to_string())
            }
        }
    }

    fn poll_command_with_stdin(command: &str, stdin_data: &str) -> Result<String, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn: {}", e))?;

        // Write context JSON to stdin, then close it so the child sees EOF
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(stdin_data.as_bytes());
            // stdin is dropped here, closing the pipe
        }

        let output = child
            .wait_with_output()
            .map_err(|e| format!("failed to wait: {}", e))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.is_empty() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                Err(stderr.trim().to_string())
            }
        }
    }
}
