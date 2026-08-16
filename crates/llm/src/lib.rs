//! CherryPi LLM - pluggable LLM provider abstraction
//!
//! Provides a trait-based system for connecting to different LLM backends:
//! - Ollama (local, default for MVP)
//! - OpenAI
//! - Anthropic
//! - OpenRouter

pub mod provider;
pub mod ollama;
pub mod openai;
pub mod anthropic;
pub mod types;

pub use provider::{Provider, ProviderConfig, ProviderFactory};
pub use ollama::OllamaProvider;
pub use types::*;
