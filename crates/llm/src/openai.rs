use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
use tracing::error;

use super::provider::{Provider, ProviderConfig};
use super::types::*;

/// OpenAI provider implementation
pub struct OpenAiProvider {
    config: ProviderConfig,
    client: Client,
}

impl OpenAiProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn default_model(&self) -> &str {
        &self.config.model
    }

    async fn list_models(&self) -> Result<Vec<String>, String> {
        let api_key = self
            .config
            .api_key
            .as_ref()
            .ok_or_else(|| {
                error!("OpenAI API key not configured");
                "API key required for OpenAI".to_string()
            })?;

        let resp = self
            .client
            .get("https://api.openai.com/v1/models")
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .await
            .map_err(|e| {
                error!("Failed to connect to OpenAI for model list: {}", e);
                format!("Failed to list models: {}", e)
            })?;

        #[derive(serde::Deserialize)]
        struct ModelInfo {
            id: String,
        }

        #[derive(serde::Deserialize)]
        struct ListResponse {
            data: Vec<ModelInfo>,
        }

        let body: ListResponse = resp
            .json()
            .await
            .map_err(|e| {
                error!("Failed to parse OpenAI model list: {}", e);
                format!("Failed to parse model list: {}", e)
            })?;

        Ok(body.data.into_iter().map(|m| m.id).collect())
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, String> {
        let api_key = self
            .config
            .api_key
            .as_ref()
            .ok_or_else(|| {
                error!("OpenAI API key not configured");
                "API key required for OpenAI".to_string()
            })?;

        let openai_req = serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "temperature": request.temperature.unwrap_or(0.7),
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "tools": request.tools,
            "tool_choice": request.tool_choice,
        });

        let resp = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&openai_req)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to connect to OpenAI for chat: {}", e);
                format!("OpenAI request failed: {}", e)
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("OpenAI returned error {}: {}", status, body);
            return Err(format!("OpenAI error {}: {}", status, body));
        }

        let response: ChatResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        Ok(response)
    }

    fn chat_stream<'a>(
        &'a self,
        _request: ChatRequest,
    ) -> BoxFuture<'a, Result<Pin<Box<dyn Stream<Item = Result<ChatChunk, String>> + Send>>, String>> {
        Box::pin(async { Err("OpenAI streaming not yet implemented".to_string()) })
    }
}
