pub mod anthropic;
pub mod google;
pub mod openai;

use crate::cancellation::GhostwriterCancellation;
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

#[async_trait::async_trait]
pub trait LLMEngine: Send {
    fn new(options: &HashMap<String, String>) -> Self
    where
        Self: Sized;
    fn register_tool(&mut self, name: &str, definition: JsonValue, callback: Box<dyn FnMut(JsonValue) + Send>);
    fn add_text_content(&mut self, text: &str);
    fn add_image_content(&mut self, base64_image: &str);
    fn clear_content(&mut self);
    async fn execute(&mut self, cancellation: &GhostwriterCancellation, status_callback: Option<StatusCallback>) -> Result<()>;
}
