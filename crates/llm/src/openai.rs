use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
use tracing::error;

use super::provider::{Provider, ProviderConfig};
use super::types::*;

/// OpenAI-compatible provider (OpenAI itself, OpenRouter, ...)
pub struct OpenAiProvider {
    config: ProviderConfig,
    client: Client,
    name: &'static str,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self::with_base_url(config, "openai", "https://api.openai.com/v1")
    }

    /// Build a provider for any OpenAI-compatible API at `base_url` (no trailing slash).
    pub fn with_base_url(config: ProviderConfig, name: &'static str, base_url: &str) -> Self {
        Self {
            config,
            client: Client::new(),
            name,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        self.name
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
            .get(format!("{}/models", self.base_url))
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

        // Shared upstream pools (e.g. OpenRouter free models) return 429 transiently;
        // retry those with a short backoff before giving up.
        const MAX_ATTEMPTS: u32 = 4;
        let mut attempt = 0;
        let resp = loop {
            attempt += 1;
            let resp = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", api_key))
                .json(&openai_req)
                .send()
                .await
                .map_err(|e| {
                    error!("Failed to connect to OpenAI for chat: {}", e);
                    format!("OpenAI request failed: {}", e)
                })?;

            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < MAX_ATTEMPTS {
                let wait = std::time::Duration::from_secs(2u64.pow(attempt));
                error!("{} rate-limited (attempt {}); retrying in {:?}", self.name, attempt, wait);
                tokio::time::sleep(wait).await;
                continue;
            }
            break resp;
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("OpenAI returned error {}: {}", status, body);
            return Err(format!("OpenAI error {}: {}", status, body));
        }

        let mut body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Tool-call replies carry `"content": null`; normalize it so ChatMessage decodes.
        if let Some(choices) = body.get_mut("choices").and_then(|c| c.as_array_mut()) {
            for choice in choices {
                if let Some(content) = choice.pointer_mut("/message/content") {
                    if content.is_null() {
                        *content = serde_json::Value::String(String::new());
                    }
                }
            }
        }

        let response: ChatResponse = serde_json::from_value(body)
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
