use super::{status_update, LLMEngine, Tool};
use crate::cancellation::{with_cancellation, GhostwriterCancellation};
use crate::util::{option_or_env, option_or_env_fallback, OptionMap};
use anyhow::Result;
use log::debug;
use serde_json::json;
use serde_json::Value as json;

pub struct Anthropic {
    model: String,
    api_key: String,
    base_url: String,
    tools: Vec<Tool>,
    content: Vec<json>,
    web_search: bool,
    thinking: bool,
    thinking_tokens: u32,
}

impl Anthropic {
    pub fn add_content(&mut self, content: json) {
        self.content.push(content);
    }

    fn tool_definition_json(tool: &Tool) -> json {
        json!({
            "name": tool.definition["name"],
            "description": tool.definition["description"],
            "input_schema": tool.definition["parameters"],
        })
    }
}

#[async_trait::async_trait]
impl LLMEngine for Anthropic {
    fn new(options: &OptionMap) -> Self {
        let api_key = option_or_env(options, "api_key", "ANTHROPIC_API_KEY");
        let base_url = option_or_env_fallback(options, "base_url", "ANTHROPIC_BASE_URL", "https://api.anthropic.com");
        let model = options.get("model").unwrap().to_string();
        let web_search = options.get("web_search").is_some_and(|v| v == "true");
        let thinking = options.get("thinking").is_some_and(|v| v == "true");
        let thinking_tokens = options.get("thinking_tokens").and_then(|v| v.parse::<u32>().ok()).unwrap_or(5000);

        Self {
            model,
            base_url,
            api_key,
            tools: Vec::new(),
            content: Vec::new(),
            web_search,
            thinking,
            thinking_tokens,
        }
    }

    fn register_tool(&mut self, name: &str, definition: json, callback: Box<dyn FnMut(json) + Send>) {
        self.tools.push(Tool {
            name: name.to_string(),
            definition,
            callback: Some(callback),
        });
    }

    fn add_text_content(&mut self, text: &str) {
        self.add_content(json!({
            "type": "text",
            "text": text,
        }));
    }

    fn add_image_content(&mut self, base64_image: &str) {
        self.add_content(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": base64_image
            }
        }));
    }

    fn clear_content(&mut self) {
        self.content.clear();
    }

    async fn execute(&mut self, cancellation: &GhostwriterCancellation, mut status_callback: Option<super::StatusCallback>) -> Result<()> {
        // Notify that we're building context
        // status_update!(status_callback, super::ModelExecutionStatus::BuildingContext);

        let mut tool_definitions = self.tools.iter().map(Self::tool_definition_json).collect::<Vec<_>>();

        // Add web search tool if enabled
        if self.web_search {
            tool_definitions.push(json!({
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": 5
            }));
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": 10000,
            "messages": [{
                "role": "user",
                "content": self.content
            }],
            "tools": tool_definitions,
            "tool_choice": {
                "type": "auto",
                // "disable_parallel_tool_use": true
            }
        });

        // Add thinking configuration if enabled
        if self.thinking {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": self.thinking_tokens
            });
        }

        debug!("Request: {}", body);

        // Notify that we're processing with LLM
        status_update!(status_callback, super::ModelExecutionStatus::LlmProcessing);

        // Create async HTTP request with cancellation support
        let request_future = async {
            let client = reqwest::Client::new();
            let response = client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", self.api_key.as_str())
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await?;

            if !response.status().is_success() {
                return Err(anyhow::anyhow!("API Error: {}", response.status()));
            }

            let body_text = response.text().await?;
            let json: json = serde_json::from_str(&body_text)?;
            Ok(json)
        };

        let json: json = with_cancellation(request_future, cancellation).await?;
        debug!("Response: {}", json);

        // Notify that we're processing the response
        status_update!(status_callback, super::ModelExecutionStatus::ProcessingResponse);

        let content_array = &json["content"];

        // Loop through all content entries
        for content_item in content_array.as_array().unwrap_or(&Vec::new()) {
            let content_type = content_item["type"].as_str().unwrap_or("");

            match content_type {
                "tool_use" => {
                    // Notify that we're calling tools
                    status_update!(status_callback, super::ModelExecutionStatus::CallingTools);
                    let function_name = content_item["name"].as_str().unwrap();
                    let function_input = &content_item["input"];
                    let tool = self.tools.iter_mut().find(|tool| tool.name == function_name);

                    if let Some(tool) = tool {
                        if let Some(callback) = &mut tool.callback {
                            callback(function_input.clone());
                            // Notify that we're done
                            status_update!(status_callback, super::ModelExecutionStatus::Done);
                            return Ok(());
                        } else {
                            status_update!(
                                status_callback,
                                super::ModelExecutionStatus::Error("No callback registered for tool".to_string())
                            );
                            return Err(anyhow::anyhow!("No callback registered for tool {}", function_name));
                        }
                    } else {
                        status_update!(status_callback, super::ModelExecutionStatus::Error("No tool registered".to_string()));
                        return Err(anyhow::anyhow!("No tool registered with name {}", function_name));
                    }
                }
                "thinking" => {
                    if let Some(thinking) = content_item.get("thinking") {
                        debug!("Thinking: {}", thinking);
                    }
                }
                "text" => {
                    if let Some(text) = content_item.get("text") {
                        debug!("Text: {}", text);
                    }
                }
                _ => {
                    debug!("Unknown content type: {}", content_type);
                }
            }
        }

        status_update!(
            status_callback,
            super::ModelExecutionStatus::Error("No tool calls found in response".to_string())
        );
        Err(anyhow::anyhow!("No tool calls found in response"))
    }
}
