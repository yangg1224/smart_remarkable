use super::{fail, required_model, status_update, LLMEngine, Tool};
use crate::cancellation::{with_cancellation, SmartRemarkableCancellation};
use crate::util::{option_or_env, option_or_env_fallback, OptionMap};
use anyhow::Result;
use log::debug;
use serde_json::json;
use serde_json::Value as json;

pub struct Google {
    model: String,
    base_url: String,
    api_key: String,
    tools: Vec<Tool>,
    content: Vec<json>,
}

impl Google {
    pub fn add_content(&mut self, content: json) {
        self.content.push(content);
    }

    fn tool_definition_json(tool: &Tool) -> json {
        json!({
            "name": tool.definition["name"],
            "description": tool.definition["description"],
            "parameters": tool.definition["parameters"],
        })
    }
}

#[async_trait::async_trait]
impl LLMEngine for Google {
    fn new(options: &OptionMap) -> Result<Self> {
        let api_key = option_or_env(options, "api_key", "GOOGLE_API_KEY")?;
        let base_url = option_or_env_fallback(options, "base_url", "GOOGLE_BASE_URL", "https://generativelanguage.googleapis.com");
        let model = required_model(options)?;

        Ok(Self {
            model,
            base_url,
            api_key,
            tools: Vec::new(),
            content: Vec::new(),
        })
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
            "text": text,
        }));
    }

    fn add_image_content(&mut self, base64_image: &str) {
        self.add_content(json!({
            "inline_data": {
                "mime_type": "image/png",
                "data": base64_image,
            }
        }));
    }

    fn clear_content(&mut self) {
        self.content.clear();
    }

    async fn execute(&mut self, cancellation: &SmartRemarkableCancellation, mut status_callback: Option<super::StatusCallback>) -> Result<()> {
        let body = json!({
            "contents": [{
                "role": "user",
                "parts": self.content
            }],
            "tools": [{ "function_declarations": self.tools.iter().map(Self::tool_definition_json).collect::<Vec<_>>() }],
            "tool_config": {
                "function_calling_config": {
                    "mode": "ANY"
                }
            }
        });

        debug!("Request: {}", body);

        // Notify that we're building context
        status_update!(status_callback, super::ModelExecutionStatus::BuildingContext);

        // Notify that we're processing with LLM
        status_update!(status_callback, super::ModelExecutionStatus::LlmProcessing);

        // Create async HTTP request with cancellation support
        let request_future = async {
            // Key goes in a header, not the `?key=` query string, so it can't leak into
            // error messages or logs that include the request URL.
            let response = super::http::client()
                .post(format!("{}/v1beta/models/{}:generateContent", self.base_url, self.model))
                .header("x-goog-api-key", self.api_key.as_str())
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(super::http::send_error)?;

            let body_text = super::http::read_body(response).await?;
            let json: json = serde_json::from_str(&body_text)?;
            Ok(json)
        };

        let json: json = with_cancellation(request_future, cancellation).await?;
        debug!("Response: {}", json);

        // Notify that we're processing the response
        status_update!(status_callback, super::ModelExecutionStatus::ProcessingResponse);

        // Gemini may put text (e.g. reasoning) before the function call, so use the
        // first part that actually carries one rather than blindly taking parts[0].
        let tool_call = json["candidates"][0]["content"]["parts"]
            .as_array()
            .and_then(|parts| parts.iter().find(|part| part.get("functionCall").is_some()));

        if let Some(tool_call) = tool_call {
            // Notify that we're calling tools
            status_update!(status_callback, super::ModelExecutionStatus::CallingTools);

            let Some(function_name) = tool_call["functionCall"]["name"].as_str() else {
                return Err(fail(&mut status_callback, "functionCall in response has no name"));
            };
            let function_input = &tool_call["functionCall"]["args"];
            let tool = self.tools.iter_mut().find(|tool| tool.name == function_name);

            if let Some(tool) = tool {
                if let Some(callback) = &mut tool.callback {
                    callback(function_input.clone());
                    // Notify that we're done
                    status_update!(status_callback, super::ModelExecutionStatus::Done);
                    Ok(())
                } else {
                    status_update!(
                        status_callback,
                        super::ModelExecutionStatus::Error("No callback registered for tool".to_string())
                    );
                    Err(anyhow::anyhow!("No callback registered for tool {}", function_name))
                }
            } else {
                status_update!(status_callback, super::ModelExecutionStatus::Error("No tool registered".to_string()));
                Err(anyhow::anyhow!("No tool registered with name {}", function_name))
            }
        } else {
            status_update!(
                status_callback,
                super::ModelExecutionStatus::Error("No tool calls found in response".to_string())
            );
            Err(anyhow::anyhow!("No tool calls found in response"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_engine::test_support::{last_is_error, mock_server, recording_callback};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn engine(base_url: String) -> (Google, Arc<Mutex<Option<json>>>) {
        let options: OptionMap = HashMap::from([
            ("model".to_string(), "test-model".to_string()),
            ("api_key".to_string(), "test-key".to_string()),
            ("base_url".to_string(), base_url),
        ]);
        let mut engine = Google::new(&options).unwrap();
        let got = Arc::new(Mutex::new(None));
        let got_clone = Arc::clone(&got);
        engine.register_tool(
            "draw_text",
            json!({ "name": "draw_text" }),
            Box::new(move |input| *got_clone.lock().unwrap() = Some(input)),
        );
        (engine, got)
    }

    #[tokio::test]
    async fn finds_function_call_after_leading_text_part() {
        let base = mock_server(
            200,
            r#"{"candidates":[{"content":{"parts":[{"text":"thinking..."},{"functionCall":{"name":"draw_text","args":{"text":"hi"}}}]}}]}"#,
        )
        .await;
        let (mut engine, got) = engine(base);
        engine.execute(&SmartRemarkableCancellation::new(), None).await.unwrap();
        assert_eq!(got.lock().unwrap().as_ref().unwrap()["text"], "hi");
    }

    #[tokio::test]
    async fn function_call_without_name_reports_error_instead_of_panicking() {
        let base = mock_server(200, r#"{"candidates":[{"content":{"parts":[{"functionCall":{"args":{}}}]}}]}"#).await;
        let (mut engine, _) = engine(base);
        let (cb, seen) = recording_callback();
        assert!(engine.execute(&SmartRemarkableCancellation::new(), Some(cb)).await.is_err());
        assert!(last_is_error(&seen));
    }
}
