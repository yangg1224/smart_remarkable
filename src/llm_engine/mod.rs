pub mod anthropic;
pub mod google;
pub mod http;
pub mod openai;

use crate::cancellation::SmartRemarkableCancellation;
use anyhow::Result;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum ModelExecutionStatus {
    BuildingContext,
    LlmProcessing,
    ProcessingResponse,
    CallingTools,
    Done,
    Error(String),
}

pub struct Tool {
    pub name: String,
    pub definition: JsonValue,
    pub callback: Option<Box<dyn FnMut(JsonValue) + Send>>,
}

pub type StatusCallback = Box<dyn FnMut(ModelExecutionStatus) + Send>;

macro_rules! status_update {
    ($callback:expr, $status:expr) => {
        if let Some(ref mut cb) = $callback {
            cb($status);
        }
    };
}

pub(crate) use status_update;

/// Report `message` through the status callback (as `ModelExecutionStatus::Error`) and
/// return it as an error, so a malformed model response fails this one request instead
/// of panicking the whole process.
pub(crate) fn fail(status_callback: &mut Option<StatusCallback>, message: impl Into<String>) -> anyhow::Error {
    let message = message.into();
    if let Some(cb) = status_callback {
        cb(ModelExecutionStatus::Error(message.clone()));
    }
    anyhow::anyhow!(message)
}

/// Read the required `model` option.
pub(crate) fn required_model(options: &HashMap<String, String>) -> Result<String> {
    options
        .get("model")
        .map(|m| m.to_string())
        .ok_or_else(|| anyhow::anyhow!("no model configured: pass --model or set `model` in the config"))
}

#[async_trait::async_trait]
pub trait LLMEngine: Send {
    fn new(options: &HashMap<String, String>) -> Result<Self>
    where
        Self: Sized;
    fn register_tool(&mut self, name: &str, definition: JsonValue, callback: Box<dyn FnMut(JsonValue) + Send>);
    fn add_text_content(&mut self, text: &str);
    fn add_image_content(&mut self, base64_image: &str);
    fn clear_content(&mut self);
    async fn execute(&mut self, cancellation: &SmartRemarkableCancellation, status_callback: Option<StatusCallback>) -> Result<()>;
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One-shot HTTP server on 127.0.0.1 that answers any request with `status` and
    /// `body`. Returns the base URL to point an engine at.
    pub async fn mock_server(status: u16, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Read until the end of the request body (headers + Content-Length bytes).
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let len = text[..header_end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buf.len() >= header_end + 4 + len {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            sock.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{}", addr)
    }

    /// A status callback that records every status it is given.
    pub fn recording_callback() -> (super::StatusCallback, Arc<Mutex<Vec<super::ModelExecutionStatus>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        (Box::new(move |s| seen_clone.lock().unwrap().push(s)), seen)
    }

    pub fn last_is_error(seen: &Arc<Mutex<Vec<super::ModelExecutionStatus>>>) -> bool {
        matches!(seen.lock().unwrap().last(), Some(super::ModelExecutionStatus::Error(_)))
    }
}
