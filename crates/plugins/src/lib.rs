//! CherryPi Plugins - Open Plugin Standard implementation
//!
//! Implements the Open Plugin Standard v1.0.0 for discovering, loading,
//! and managing plugins that bundle skills, agents, hooks, MCP servers,
//! LSP servers, and rules.

pub mod manifest;
pub mod discovery;
pub mod loader;
pub mod hooks;
pub mod mcp;
pub mod namespace;
pub mod status;

pub use manifest::PluginManifest;
pub use discovery::PluginDiscovery;
pub use loader::PluginLoader;
pub use hooks::{HookContext, HookEngine};
pub use status::{StatusBarManager, StatusContext, StatusSegment, ModelInfo, WorkspaceInfo};
