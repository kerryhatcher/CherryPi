use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{Stream, StreamExt};
use reqwest::Client;
use std::pin::Pin;
use tracing::{debug, error, info};

use super::provider::{Provider, ProviderConfig};
use super::types::*;

/// Ollama provider implementation
pub struct OllamaProvider {
    config: ProviderConfig,
    client: Client,
}

impl OllamaProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    fn chat_endpoint(&self) -> String {
        format!("{}/api/chat", self.config.endpoint.trim_end_matches('/'))
    }

    fn tags_endpoint(&self) -> String {
        format!("{}/api/tags", self.config.endpoint.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn default_model(&self) -> &str {
        &self.config.model
    }

    async fn list_models(&self) -> Result<Vec<String>, String> {
        let resp = self
            .client
            .get(&self.tags_endpoint())
            .send()
            .await
            .map_err(|e| {
                error!("Failed to connect to Ollama for model list: {}", e);
                format!("Failed to list models: {}", e)
            })?;

        #[derive(serde::Deserialize)]
        struct ModelInfo {
            name: String,
        }

        #[derive(serde::Deserialize)]
        struct ListResponse {
            models: Vec<ModelInfo>,
        }

        let body: ListResponse = resp
            .json()
            .await
            .map_err(|e| {
                error!("Failed to parse Ollama model list response: {}", e);
                format!("Failed to parse model list: {}", e)
            })?;

        Ok(body.models.into_iter().map(|m| m.name).collect())
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, String> {
        debug!("Ollama chat request: model={}", request.model);

        let mut ollama_req = serde_json::json!({
            "model": request.model,
            "messages": request.messages,
            "stream": false,
            "options": {
                "temperature": request.temperature.unwrap_or(0.7),
                "num_predict": request.max_tokens.unwrap_or(4096),
            }
        });

        // Pass tools if present
        if let Some(ref tools) = request.tools {
            ollama_req["tools"] = serde_json::to_value(tools).unwrap_or_default();
        }

        let resp = self
            .client
            .post(&self.chat_endpoint())
            .json(&ollama_req)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to connect to Ollama for chat: {}", e);
                format!("Ollama request failed: {}", e)
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Ollama returned error {}: {}", status, body);
            return Err(format!("Ollama error {}: {}", status, body));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| {
                error!("Failed to parse Ollama chat response: {}", e);
                format!("Failed to parse response: {}", e)
            })?;

        info!("Ollama response received");

        // Convert Ollama response to standard ChatResponse
        let content = body["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        // Parse tool calls from Ollama response if present
        let tool_calls = body["message"]["tool_calls"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|tc| ToolCallMsg {
                        id: tc["id"].as_str().unwrap_or("").to_string(),
                        call_type: "function".to_string(),
                        function: FunctionCallMsg {
                            name: tc["function"]["name"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
                            arguments: match &tc["function"]["arguments"] {
                            // Ollama native format: arguments is a JSON object
                            obj @ serde_json::Value::Object(_) => obj.clone(),
                            // OpenAI format: arguments is a JSON string
                            serde_json::Value::String(s) => {
                                serde_json::from_str(s).unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
                            }
                            _ => serde_json::Value::Object(serde_json::Map::new()),
                        },
                        },
                    })
                    .collect()
            });

        let finish_reason = if tool_calls.is_some() {
            Some("tool_calls".to_string())
        } else {
            Some("stop".to_string())
        };

        let response = ChatResponse {
            id: uuid::Uuid::new_v4().to_string(),
            model: request.model,
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text(content),
                    name: None,
                    tool_calls,
                    tool_call_id: None,
                },
                finish_reason,
            }],
            usage: Usage {
                prompt_tokens: body["prompt_eval_count"].as_u64().unwrap_or(0) as u32,
                completion_tokens: body["eval_count"].as_u64().unwrap_or(0) as u32,
                total_tokens: (body["prompt_eval_count"].as_u64().unwrap_or(0)
                    + body["eval_count"].as_u64().unwrap_or(0))
                    as u32,
            },
        };

        Ok(response)
    }

    fn chat_stream<'a>(
        &'a self,
        request: ChatRequest,
    ) -> BoxFuture<'a, Result<Pin<Box<dyn Stream<Item = Result<ChatChunk, String>> + Send>>, String>> {
        Box::pin(async move {
            debug!("Ollama streaming chat request: model={}", request.model);

            let ollama_req = serde_json::json!({
                "model": request.model,
                "messages": request.messages,
                "stream": true,
                "options": {
                    "temperature": request.temperature.unwrap_or(0.7),
                    "num_predict": request.max_tokens.unwrap_or(4096),
                }
            });

            let resp = self
                .client
                .post(&self.chat_endpoint())
                .json(&ollama_req)
                .send()
                .await
                .map_err(|e| {
                    error!("Failed to connect to Ollama for streaming chat: {}", e);
                    format!("Ollama stream request failed: {}", e)
                })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                error!("Ollama streaming error {}: {}", status, body);
                return Err(format!("Ollama error {}: {}", status, body));
            }

            let model = request.model.clone();
            let stream = resp
                .bytes_stream()
                .map(move |result| {
                    result
                        .map_err(|e| format!("Stream error: {}", e))
                        .and_then(|bytes| {
                            let line = String::from_utf8_lossy(&bytes);
                            let chunk: serde_json::Value = serde_json::from_str(&line)
                                .map_err(|e| format!("JSON parse error: {}", e))?;

                            let content = chunk["message"]["content"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();

                            let done = chunk["done"].as_bool().unwrap_or(false);

                            Ok(ChatChunk {
                                id: uuid::Uuid::new_v4().to_string(),
                                model: model.clone(),
                                choices: vec![ChunkChoice {
                                    index: 0,
                                    delta: ChunkDelta {
                                        role: None,
                                        content: Some(content),
                                        tool_calls: None,
                                    },
                                    finish_reason: if done {
                                        Some("stop".to_string())
                                    } else {
                                        None
                                    },
                                }],
                            })
                        })
                });

            Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = Result<ChatChunk, String>> + Send>>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that the JSON we send to Ollama is always valid.
    #[test]
    fn test_request_json_validity() {
        // Simulate a conversation with tool calls (the exact scenario that fails)
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: MessageContent::Text("You are CherryPi, an AI coding agent.".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".into(),
                content: MessageContent::Text("list the files in the cwd".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: MessageContent::Text("".into()),
                name: None,
                tool_calls: Some(vec![ToolCallMsg {
                    id: "call_abc123".into(),
                    call_type: "function".into(),
                    function: FunctionCallMsg {
                        name: "bash".into(),
                        arguments: serde_json::json!({"command": "ls -la"}),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: MessageContent::Text("total 96\ndrwxrwxr-x 4 kwhatcher ...".into()),
                name: None,
                tool_calls: None,
                tool_call_id: Some("call_abc123".into()),
            },
        ];

        let tools = vec![ToolDef {
            def_type: "function".into(),
            function: FunctionDef {
                name: "bash".into(),
                description: "Execute a bash command".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Bash command to execute"},
                        "timeout": {"type": "number", "description": "Timeout in seconds (optional)"}
                    },
                    "required": ["command"]
                }),
            },
        }];

        // Build the request exactly like the chat() method does
        let mut ollama_req = serde_json::json!({
            "model": "llama3.2",
            "messages": messages,
            "stream": false,
            "options": {
                "temperature": 0.7,
                "num_predict": 4096,
            }
        });

        ollama_req["tools"] = serde_json::to_value(&tools).unwrap_or_default();

        // Serialize to string
        let json_str = serde_json::to_string_pretty(&ollama_req).unwrap();
        println!("=== JSON being sent to Ollama ===\n{}\n=== END ===", json_str);

        // Verify it's valid JSON
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&json_str);
        assert!(parsed.is_ok(), "JSON is invalid: {}", parsed.unwrap_err());

        // Also verify it round-trips correctly
        let parsed = parsed.unwrap();
        assert_eq!(parsed["model"], "llama3.2");
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 4);
        assert!(parsed["tools"].is_array());
    }
}
