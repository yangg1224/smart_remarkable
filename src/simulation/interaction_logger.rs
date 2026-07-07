use super::SimulationConfig;
use anyhow::Result;
use log::debug;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Logger for simulated interactions, useful for testing and verification
pub struct InteractionLogger {
    log_file: Option<Arc<Mutex<std::fs::File>>>,
    interactions: Arc<Mutex<Vec<InteractionLog>>>,
}

/// Represents a logged interaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionLog {
    pub timestamp: u64,             // Unix timestamp in milliseconds
    pub interaction_type: String,   // e.g. "touch_trigger", "draw_text", "draw_svg", "screenshot"
    pub details: serde_json::Value, // Flexible details specific to interaction type
    pub description: Option<String>,
}

impl InteractionLogger {
    pub fn new(config: SimulationConfig) -> Result<Self> {
        let log_file = if let Some(ref log_path) = config.interaction_log {
            let file = OpenOptions::new().create(true).append(true).open(log_path)?;
            Some(Arc::new(Mutex::new(file)))
        } else {
            None
        };

        if config.interaction_log.is_some() {
            debug!("InteractionLogger initialized with log file: {:?}", config.interaction_log);
        } else {
            debug!("InteractionLogger initialized (memory only, no file output)");
        }

        Ok(Self {
            log_file,
            interactions: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Log a touch trigger event
    pub fn log_touch_trigger(&self, corner: &str, description: Option<&str>) {
        let details = serde_json::json!({
            "corner": corner,
            "trigger_type": "touch"
        });

        self.log_interaction("touch_trigger", details, description);
    }

    /// Log a draw text operation
    pub fn log_draw_text(&self, text: &str, description: Option<&str>) {
        let details = serde_json::json!({
            "text": text,
            "text_length": text.len()
        });

        self.log_interaction("draw_text", details, description);
    }

    /// Log a draw SVG operation
    pub fn log_draw_svg(&self, svg_length: usize, description: Option<&str>) {
        let details = serde_json::json!({
            "svg_length": svg_length,
            "drawing_type": "svg"
        });

        self.log_interaction("draw_svg", details, description);
    }

    /// Log a screenshot operation
    pub fn log_screenshot(&self, source: &str, description: Option<&str>) {
        let details = serde_json::json!({
            "source": source, // e.g. "test_image_1.png", "device_screenshot"
            "operation": "screenshot"
        });

        self.log_interaction("screenshot", details, description);
    }

    /// Log a config change
    pub fn log_config_change(&self, field: &str, old_value: &str, new_value: &str) {
        let details = serde_json::json!({
            "field": field,
            "old_value": old_value,
            "new_value": new_value
        });

        self.log_interaction(
            "config_change",
            details,
            Some(&format!("Config {} changed from {} to {}", field, old_value, new_value)),
        );
    }

    /// Log a generic interaction
    fn log_interaction(&self, interaction_type: &str, details: serde_json::Value, description: Option<&str>) {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;

        let log_entry = InteractionLog {
            timestamp,
            interaction_type: interaction_type.to_string(),
            details,
            description: description.map(|s| s.to_string()),
        };

        // Add to memory log
        if let Ok(mut interactions) = self.interactions.lock() {
            interactions.push(log_entry.clone());
        }

        // Write to file if configured
        if let Some(ref file_mutex) = self.log_file {
            if let Ok(mut file) = file_mutex.lock() {
                let json_line = serde_json::to_string(&log_entry).unwrap_or_else(|_| "{}".to_string());
                if let Err(e) = writeln!(file, "{}", json_line) {
                    debug!("Failed to write to interaction log: {}", e);
                }
                // Ensure data is written immediately
                let _ = file.flush();
            }
        }

        debug!("Logged interaction: {} - {:?}", interaction_type, description);
    }

    /// Get all interactions from memory (for web API)
    pub fn get_interactions(&self) -> Vec<InteractionLog> {
        self.interactions.lock().unwrap().clone()
    }

    /// Get recent interactions (last N)
    pub fn get_recent_interactions(&self, count: usize) -> Vec<InteractionLog> {
        let interactions = self.interactions.lock().unwrap();
        let start_index = if interactions.len() > count { interactions.len() - count } else { 0 };
        interactions[start_index..].to_vec()
    }

    /// Clear interaction history
    pub fn clear_interactions(&self) {
        if let Ok(mut interactions) = self.interactions.lock() {
            interactions.clear();
        }
        debug!("Cleared interaction history");
    }
}
