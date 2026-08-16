use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// A tool call event streamed from the agent to the TUI during processing.
/// This allows the TUI to render tool calls incrementally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEvent {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub tool_output: String,
    pub is_error: bool,
    pub timestamp: DateTime<Utc>,
}

/// Events that flow through the system
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    // Session events
    SessionStart {
        timestamp: DateTime<Utc>,
    },
    SessionEnd {
        timestamp: DateTime<Utc>,
    },

    // User events
    UserPromptSubmit {
        prompt: String,
        timestamp: DateTime<Utc>,
    },

    // Tool events
    PreToolUse {
        tool_name: String,
        tool_input: serde_json::Value,
        timestamp: DateTime<Utc>,
    },
    PostToolUse {
        tool_name: String,
        tool_output: String,
        is_error: bool,
        timestamp: DateTime<Utc>,
    },
    PostToolUseFailure {
        tool_name: String,
        error: String,
        timestamp: DateTime<Utc>,
    },

    // File events
    BeforeReadFile {
        path: String,
        timestamp: DateTime<Utc>,
    },
    AfterFileEdit {
        path: String,
        timestamp: DateTime<Utc>,
    },

    // Shell events
    BeforeShellExecution {
        command: String,
        timestamp: DateTime<Utc>,
    },
    AfterShellExecution {
        command: String,
        exit_code: i32,
        timestamp: DateTime<Utc>,
    },

    // Agent events
    Stop {
        reason: Option<String>,
        timestamp: DateTime<Utc>,
    },
    SubagentStart {
        name: String,
        timestamp: DateTime<Utc>,
    },
    SubagentStop {
        name: String,
        timestamp: DateTime<Utc>,
    },

    // Plugin events
    PluginLoaded {
        name: String,
        version: Option<String>,
        timestamp: DateTime<Utc>,
    },
    PluginUnloaded {
        name: String,
        timestamp: DateTime<Utc>,
    },
}

impl Event {
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Event::SessionStart { timestamp } => *timestamp,
            Event::SessionEnd { timestamp } => *timestamp,
            Event::UserPromptSubmit { timestamp, .. } => *timestamp,
            Event::PreToolUse { timestamp, .. } => *timestamp,
            Event::PostToolUse { timestamp, .. } => *timestamp,
            Event::PostToolUseFailure { timestamp, .. } => *timestamp,
            Event::BeforeReadFile { timestamp, .. } => *timestamp,
            Event::AfterFileEdit { timestamp, .. } => *timestamp,
            Event::BeforeShellExecution { timestamp, .. } => *timestamp,
            Event::AfterShellExecution { timestamp, .. } => *timestamp,
            Event::Stop { timestamp, .. } => *timestamp,
            Event::SubagentStart { timestamp, .. } => *timestamp,
            Event::SubagentStop { timestamp, .. } => *timestamp,
            Event::PluginLoaded { timestamp, .. } => *timestamp,
            Event::PluginUnloaded { timestamp, .. } => *timestamp,
        }
    }
}
