use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;

use crate::types::*;

/// Configuration for creating a provider
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// API endpoint URL
    pub endpoint: String,
    /// API key (optional)
    pub api_key: Option<String>,
    /// Default model
    pub model: String,
    /// Extra parameters
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Core trait that all LLM providers must implement
#[async_trait]
pub trait Provider: Send + Sync {
    /// Get the provider name
    fn name(&self) -> &str;

    /// Get the default model
    fn default_model(&self) -> &str;

    /// List available models
    async fn list_models(&self) -> Result<Vec<String>, String>;

    /// Send a chat completion request (non-streaming)
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, String>;

    /// Send a chat completion request (streaming)
    fn chat_stream<'a>(
        &'a self,
        request: ChatRequest,
    ) -> BoxFuture<'a, Result<Pin<Box<dyn Stream<Item = Result<ChatChunk, String>> + Send>>, String>>;
}

/// Factory for creating provider instances
pub struct ProviderFactory;

impl ProviderFactory {
    /// Create a provider from a type string
    pub fn create(provider_type: &str, config: ProviderConfig) -> Result<Arc<dyn Provider>, String> {
        match provider_type {
            "ollama" => {
                let provider = crate::ollama::OllamaProvider::new(config);
                Ok(Arc::new(provider))
            }
            "openai" => {
                let provider = crate::openai::OpenAiProvider::new(config);
                Ok(Arc::new(provider))
            }
            "anthropic" => {
                let provider = crate::anthropic::AnthropicProvider::new(config);
                Ok(Arc::new(provider))
            }
            _ => Err(format!("Unknown provider type: {}", provider_type)),
        }
    }
}
