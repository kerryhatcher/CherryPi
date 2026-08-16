use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::Stream;
use reqwest::Client;
use std::pin::Pin;
use tracing::error;

use super::provider::{Provider, ProviderConfig};
use super::types::*;

/// Anthropic provider implementation
pub struct AnthropicProvider {
    config: ProviderConfig,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn default_model(&self) -> &str {
        &self.config.model
    }

    async fn list_models(&self) -> Result<Vec<String>, String> {
        // Anthropic doesn't have a public list models endpoint
        Ok(vec![
            "claude-3-5-sonnet-20241022".to_string(),
            "claude-3-5-haiku-20241022".to_string(),
            "claude-3-opus-20240229".to_string(),
        ])
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, String> {
        let api_key = self
            .config
            .api_key
            .as_ref()
            .ok_or_else(|| {
                error!("Anthropic API key not configured");
                "API key required for Anthropic".to_string()
            })?;

        // Convert messages to Anthropic format
        let system = request
            .messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");

        let messages: Vec<&ChatMessage> = request
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .collect();

        let anthropic_req = serde_json::json!({
            "model": request.model,
            "system": system,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7),
        });

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&anthropic_req)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to connect to Anthropic for chat: {}", e);
                format!("Anthropic request failed: {}", e)
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Anthropic returned error {}: {}", status, body);
            return Err(format!("Anthropic error {}: {}", status, body));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Convert Anthropic response to standard ChatResponse
        let content = body["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let response = ChatResponse {
            id: body["id"].as_str().unwrap_or("").to_string(),
            model: request.model,
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text(content),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(
                    body["stop_reason"]
                        .as_str()
                        .unwrap_or("end_turn")
                        .to_string(),
                ),
            }],
            usage: Usage {
                prompt_tokens: body["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: body["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: (body["usage"]["input_tokens"].as_u64().unwrap_or(0)
                    + body["usage"]["output_tokens"].as_u64().unwrap_or(0))
                    as u32,
            },
        };

        Ok(response)
    }

    fn chat_stream<'a>(
        &'a self,
        _request: ChatRequest,
    ) -> BoxFuture<'a, Result<Pin<Box<dyn Stream<Item = Result<ChatChunk, String>> + Send>>, String>> {
        Box::pin(async { Err("Anthropic streaming not yet implemented".to_string()) })
    }
}
