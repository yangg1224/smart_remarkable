//! Simulation module for off-device testing
//!
//! This module provides simulation capabilities that replace device interactions
//! with file-based or API-driven simulation for development and testing.

pub mod interaction_logger;
pub mod screenshot_simulator;
pub mod touch_simulator;

pub use interaction_logger::InteractionLogger;
pub use screenshot_simulator::ScreenshotSimulator;
pub use touch_simulator::TouchSimulator;

use crate::device::DeviceModel;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for simulation mode
#[derive(Debug, Clone)]
pub struct SimulationConfig {
    pub enabled: bool,
    pub device_model: DeviceModel,
    pub touch_events_file: Option<String>,
    pub screenshot_dir: Option<String>,
    pub auto_trigger_delay: Option<Duration>,
    pub interaction_log: Option<String>,
}

impl SimulationConfig {
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            enabled: config.is_test_mode(),
            device_model: config.get_test_device_model().unwrap_or(DeviceModel::Unknown),
            touch_events_file: config.test_touch_events_file.clone(),
            screenshot_dir: config.test_screenshot_dir.clone(),
            auto_trigger_delay: config.test_auto_trigger_delay.map(|s| Duration::from_secs(s as u64)),
            interaction_log: config.test_interaction_log.clone(),
        }
    }
}

/// Touch event for simulation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchEvent {
    pub timestamp: String, // e.g. "0s", "10s", "5.5s"
    pub corner: String,    // e.g. "UR", "LL", "UL", "LR"
    pub description: Option<String>,
}

/// Format for touch events file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchEventFile {
    pub touch_events: Vec<TouchEvent>,
    pub auto_trigger_interval: Option<String>, // e.g. "5s"
}

/// Parse a duration string like "5s", "10.5s", "1.2s"
pub fn parse_duration(duration_str: &str) -> Result<Duration> {
    if let Some(stripped) = duration_str.strip_suffix('s') {
        let seconds: f64 = stripped.parse()?;
        Ok(Duration::from_secs_f64(seconds))
    } else {
        Err(anyhow::anyhow!("Invalid duration format: {}. Use format like '5s' or '10.5s'", duration_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("10.5s").unwrap(), Duration::from_secs_f64(10.5));
        assert_eq!(parse_duration("0.1s").unwrap(), Duration::from_secs_f64(0.1));
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("invalid").is_err());
    }
}
