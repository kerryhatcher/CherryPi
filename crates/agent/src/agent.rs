use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, error, info, warn};

use cherrypi_core::event::{Event, ToolCallEvent};
use cherrypi_core::message::Message;
use cherrypi_llm::types::*;
use cherrypi_plugins::hooks::HookContext;
use cherrypi_plugins::WorkspaceInfo;

use super::context::AgentContext;
use super::tools::BuiltinTools;

/// The main agent that orchestrates the conversation loop
pub struct Agent {
    context: AgentContext,
}

impl Agent {
    /// Create a new agent with the given context
    pub fn new(mut context: AgentContext) -> Self {
        // Register built-in tools
        let working_dir = context.working_dir.clone();
        let cancel_flag = context.cancel_flag.clone();
        let extra_bin_paths = Some(context.extra_bin_paths.clone());
        BuiltinTools::register_all(
            &mut context.tool_registry,
            working_dir,
            context.fff_picker.clone(),
            cancel_flag,
            extra_bin_paths,
        );

        // Add system message (use config override or built-in default)
        let system_prompt = context
            .config
            .agent
            .system_prompt
            .clone()
            .unwrap_or_else(|| include_str!("../../core/assets/system_prompt.md").to_string());
        context.conversation.add_message(Message::system(&system_prompt));

        Self { context }
    }

    /// Get a reference to the agent context
    pub fn context(&self) -> &AgentContext {
        &self.context
    }

    /// Get a mutable reference to the agent context
    pub fn context_mut(&mut self) -> &mut AgentContext {
        &mut self.context
    }

    /// Process a user message and return the assistant's response.
    /// Checks `cancel_flag` between turns; returns Err("Cancelled") if set.
    pub async fn process_message(
        &mut self,
        user_input: &str,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> Result<String, String> {
        // Fire UserPromptSubmit event
        let event = Event::UserPromptSubmit {
            prompt: user_input.to_string(),
            timestamp: chrono::Utc::now(),
        };
        self.context.hook_engine.fire(&event, &self.hook_context()).await;

        // Add user message
        self.context
            .conversation
            .add_message(Message::user(user_input));

        // Run the agent loop
        let mut turn = 0;
        let max_turns = self.context.config.agent.max_turns;

        loop {
            turn += 1;

            // Check cancel flag
            if let Some(ref flag) = cancel_flag {
                if flag.load(Ordering::SeqCst) {
                    info!("Agent cancelled by user");
                    return Err("Cancelled".to_string());
                }
            }

            if turn > max_turns {
                return Err(format!("Exceeded maximum turns ({})", max_turns));
            }

            debug!("Agent turn {}/{}", turn, max_turns);

            // Build the chat request
            let request = self.build_chat_request();

            // Send to LLM
            let response = match self.context.provider.chat(request).await {
                Ok(r) => r,
                Err(e) => {
                    error!("LLM provider error on turn {}: {}", turn, e);
                    return Err(e);
                }
            };

            // Process the response
            let choice = &response.choices[0];
            let message = &choice.message;

            // Check for tool calls
            if let Some(tool_calls) = &message.tool_calls {
                debug!("LLM requested {} tool call(s)", tool_calls.len());

                // Add assistant message with tool calls
                let mut assistant_msg = Message::assistant("");
                assistant_msg.tool_calls = Some(
                    tool_calls
                        .iter()
                        .map(|tc| cherrypi_core::message::ToolCallData {
                            id: tc.id.clone(),
                            call_type: "function".to_string(),
                            function: cherrypi_core::message::FunctionCall {
                                name: tc.function.name.clone(),
                                arguments: tc.function.arguments.clone(),
                            },
                        })
                        .collect(),
                );
                self.context.conversation.add_message(assistant_msg);

                // Execute each tool call
                for tc in tool_calls {
                    let tool_name = &tc.function.name;
                    let args = tc.function.arguments.clone();

                    // Fire PreToolUse event
                    let event = Event::PreToolUse {
                        tool_name: tool_name.clone(),
                        tool_input: args.clone(),
                        timestamp: chrono::Utc::now(),
                    };
                    self.context.hook_engine.fire(&event, &self.hook_context()).await;

                    // Execute the tool
                    let tool_call = cherrypi_core::tool::ToolCall {
                        id: tc.id.clone(),
                        name: tool_name.clone(),
                        arguments: args,
                    };

                    let result = self.context.tool_registry.execute(&tool_call).await;

                    let (output, is_error) = match result {
                        Ok(tool_result) => {
                            if tool_result.is_error {
                                warn!("Tool '{}' reported error: {}", tool_name, tool_result.content);
                            }
                            // Fire PostToolUse event
                            let event = Event::PostToolUse {
                                tool_name: tool_name.clone(),
                                tool_output: tool_result.content.clone(),
                                is_error: tool_result.is_error,
                                timestamp: chrono::Utc::now(),
                            };
                            self.context.hook_engine.fire(&event, &self.hook_context()).await;

                            // Add tool result message
                            self.context.conversation.add_message(
                                Message::tool_result(tc.id.clone(), &tool_result.content),
                            );

                            (tool_result.content, tool_result.is_error)
                        }
                        Err(e) => {
                            error!("Tool '{}' execution failed: {}", tool_name, e);
                            // Fire PostToolUseFailure event
                            let event = Event::PostToolUseFailure {
                                tool_name: tool_name.clone(),
                                error: e.clone(),
                                timestamp: chrono::Utc::now(),
                            };
                            self.context.hook_engine.fire(&event, &self.hook_context()).await;

                            self.context.conversation.add_message(
                                Message::tool_result(tc.id.clone(), &format!("Error: {}", e)),
                            );

                            (format!("Error: {}", e), true)
                        }
                    };

                    // Send tool call event to TUI for real-time display
                    let tc_event = ToolCallEvent {
                        tool_name: tool_name.clone(),
                        tool_input: tc.function.arguments.clone(),
                        tool_output: output,
                        is_error,
                        timestamp: chrono::Utc::now(),
                    };
                    if self.context.tool_call_tx.send(tc_event).is_err() {
                        // TUI receiver dropped (e.g. during shutdown) — fine to ignore
                        debug!("Tool call event receiver closed");
                    }
                }

                // Continue the loop for the LLM to process tool results
                continue;
            }

            // No tool calls - this is the final response
            let content = match &message.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            };

            // Add assistant message
            self.context
                .conversation
                .add_message(Message::assistant(&content));

            return Ok(content);
        }
    }

    /// Build a chat request from the current conversation state
    fn build_chat_request(&self) -> ChatRequest {
        let messages: Vec<ChatMessage> = self
            .context
            .conversation
            .messages
            .iter()
            .map(|msg| {
                let content = match &msg.content {
                    cherrypi_core::message::MessageContent::Text(t) => {
                        MessageContent::Text(t.clone())
                    }
                    cherrypi_core::message::MessageContent::Parts(_) => {
                        MessageContent::Text(msg.content.text())
                    }
                };

                ChatMessage {
                    role: msg.role.to_string(),
                    content,
                    name: msg.name.clone(),
                    tool_calls: msg.tool_calls.as_ref().map(|tcs| {
                        tcs.iter()
                            .map(|tc| ToolCallMsg {
                                id: tc.id.clone(),
                                call_type: tc.call_type.clone(),
                                function: FunctionCallMsg {
                                    name: tc.function.name.clone(),
                                    arguments: tc.function.arguments.clone(),
                                },
                            })
                            .collect()
                    }),
                    tool_call_id: msg.tool_call_id.clone(),
                }
            })
            .collect();

        let tools: Vec<ToolDef> = self
            .context
            .tool_registry
            .definitions()
            .iter()
            .map(|t| ToolDef {
                def_type: "function".to_string(),
                function: FunctionDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect();

        ChatRequest {
            model: self.context.config.provider.model.clone(),
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice: Some(ToolChoice::Auto("auto".to_string())),
            max_tokens: Some(self.context.config.provider.max_output_tokens as u32),
            temperature: Some(self.context.config.provider.temperature),
            stop: None,
            stream: false,
        }
    }

    /// Initialize plugins from configured directories.
    /// `default_plugins_dir` is the platform-standard plugin install directory.
    pub fn init_plugins(&mut self, default_plugins_dir: Option<std::path::PathBuf>) {
        if !self.context.config.plugins.enabled {
            return;
        }

        // Add configured plugin directories
        for dir in &self.context.config.plugins.plugin_dirs {
            self.context.plugin_loader.add_search_dir(dir.into());
        }

        // Add the platform-standard plugin directory
        if let Some(plugins_dir) = default_plugins_dir {
            self.context.plugin_loader.add_search_dir(plugins_dir.clone());
            self.context.plugin_loader.set_install_dir(plugins_dir);
        }

        // Discover and load all plugins
        let loaded = self.context.plugin_loader.discover_all();
        info!("Loaded {} plugins: {:?}", loaded.len(), loaded);

        // Register hooks from plugins
        for plugin in self.context.plugin_loader.enabled_plugins() {
            if let Some(ref hooks_config) = plugin.components.hooks {
                self.context.hook_engine.register_from_config(
                    &plugin.name,
                    &plugin.root.to_string_lossy(),
                    hooks_config,
                );
            }

            // Register MCP servers
            if let Some(ref mcp_config) = plugin.components.mcp_servers {
                self.context.mcp_manager.load_from_config(
                    &plugin.name,
                    &plugin.root.to_string_lossy(),
                    mcp_config,
                );
            }
        }

        // Start MCP servers
        self.context.mcp_manager.start_all();
    }

    /// Build a `HookContext` from the current agent state for piping to
    /// hook commands that opt into `stdin_context`.
    fn hook_context(&self) -> HookContext {
        HookContext {
            workspace: WorkspaceInfo {
                current_dir: self
                    .context
                    .working_dir
                    .display()
                    .to_string(),
            },
            session_id: None,
        }
    }
}
