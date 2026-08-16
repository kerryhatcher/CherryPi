//! CherryPi TUI - terminal user interface
//!
//! Built with ratatui and crossterm, provides:
//! - Chat panel with scrolling history
//! - Input area with multi-line support
//! - File viewer panel
//! - Status bar
//! - Plugin management view

pub mod app;
pub mod ui;
pub mod input;
pub mod theme;

pub use app::{App, DisplayMessage, MessageKind, poll_plugin_install};
