//! CherryPi Core - shared types, traits, and data structures

pub mod config;
pub mod message;
pub mod tool;
pub mod event;
pub mod error;
pub mod paths;
pub mod chat_log;

/// Re-export commonly used types
pub use config::Config;
pub use message::{Message, MessageRole, Conversation};
pub use tool::{Tool, ToolCall, ToolResult, ToolRegistry};
pub use event::{Event, ToolCallEvent};
pub use paths::CherryPiPaths;
pub use chat_log::ChatLogger;
