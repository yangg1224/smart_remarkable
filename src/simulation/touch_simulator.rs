use super::{parse_duration, SimulationConfig, TouchEvent, TouchEventFile};
use crate::cancellation::GhostwriterCancellation;
use crate::touch::TriggerCorner;
use anyhow::Result;
use log::{debug, info};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::{sleep, timeout};

/// Touch simulator that replaces real device touch detection
/// with scripted events or programmatic triggers
pub struct TouchSimulator {
    config: SimulationConfig,
    trigger_corner: TriggerCorner,
    events: Vec<TouchEvent>,
    start_time: Instant,
    manual_triggers: Arc<Mutex<Vec<TriggerCorner>>>,
    event_index: usize,
}

impl TouchSimulator {
    pub fn new(config: SimulationConfig, trigger_corner: TriggerCorner) -> Result<Self> {
        let events = if let Some(ref events_file) = config.touch_events_file {
            Self::load_events_from_file(events_file)?
        } else {
            Vec::new()
        };

        info!("TouchSimulator initialized with {} scripted events", events.len());
        if let Some(auto_delay) = config.auto_trigger_delay {
            info!("Auto-trigger enabled with {:?} delay", auto_delay);
        }

        Ok(Self {
            config,
            trigger_corner,
            events,
            start_time: Instant::now(),
            manual_triggers: Arc::new(Mutex::new(Vec::new())),
            event_index: 0,
        })
    }

    /// Load touch events from JSON file
    fn load_events_from_file(file_path: &str) -> Result<Vec<TouchEvent>> {
        let content = std::fs::read_to_string(file_path)?;
        let event_file: TouchEventFile = serde_json::from_str(&content)?;
        info!("Loaded {} touch events from {}", event_file.touch_events.len(), file_path);
        Ok(event_file.touch_events)
    }

    /// Get handle for manually triggering events (for web API)
    pub fn get_manual_trigger_handle(&self) -> Arc<Mutex<Vec<TriggerCorner>>> {
        Arc::clone(&self.manual_triggers)
    }

    /// Add a manual trigger (used by web API)
    pub fn add_manual_trigger(&self, corner: TriggerCorner) {
        if let Ok(mut triggers) = self.manual_triggers.lock() {
            triggers.push(corner);
            debug!("Added manual trigger for corner: {:?}", corner);
        }
    }

    /// Wait for next trigger event (simulated)
    pub async fn wait_for_trigger(&mut self, cancellation: &GhostwriterCancellation) -> Result<()> {
        info!("TouchSimulator waiting for trigger (corner: {:?})", self.trigger_corner);

        loop {
            // Check for cancellation (only main token, not execution cycles)
            if cancellation.should_cancel_main() {
                return Err(anyhow::anyhow!("Touch waiting cancelled"));
            }

            // Check for manual triggers first (highest priority)
            if let Ok(mut triggers) = self.manual_triggers.lock() {
                if let Some(triggered_corner) = triggers.pop() {
                    if triggered_corner as u8 == self.trigger_corner as u8 {
                        info!("Manual trigger matched current corner: {:?}", self.trigger_corner);
                        return Ok(());
                    } else {
                        debug!("Manual trigger for {:?} ignored, waiting for {:?}", triggered_corner, self.trigger_corner);
                    }
                }
            }

            // Check for scripted events
            if let Some(event) = self.get_next_scripted_event() {
                let event_corner = TriggerCorner::from_string(&event.corner)?;
                if event_corner as u8 == self.trigger_corner as u8 {
                    info!("Scripted trigger matched: {} at {}", event.corner, event.timestamp);
                    if let Some(desc) = &event.description {
                        info!("Event description: {}", desc);
                    }
                    self.event_index += 1;
                    return Ok(());
                } else {
                    debug!("Scripted trigger for {} ignored, waiting for {:?}", event.corner, self.trigger_corner);
                    self.event_index += 1;
                }
            }

            // Check for auto-trigger
            if let Some(auto_delay) = self.config.auto_trigger_delay {
                let elapsed = self.start_time.elapsed();
                if elapsed >= auto_delay {
                    info!("Auto-trigger activated after {:?}", elapsed);
                    self.start_time = Instant::now(); // Reset for next auto-trigger
                    return Ok(());
                }
            }

            // Sleep briefly to avoid busy-waiting and allow cancellation checking
            let sleep_result = timeout(Duration::from_millis(100), sleep(Duration::from_millis(100))).await;
            if sleep_result.is_err() {
                // Timeout means we should check conditions again
                continue;
            }
        }
    }

    /// Get the next scripted event if its time has come
    fn get_next_scripted_event(&self) -> Option<&TouchEvent> {
        if self.event_index >= self.events.len() {
            return None;
        }

        let event = &self.events[self.event_index];
        let event_time = match parse_duration(&event.timestamp) {
            Ok(duration) => duration,
            Err(_) => {
                debug!("Invalid timestamp in event: {}", event.timestamp);
                return None;
            }
        };

        let elapsed = self.start_time.elapsed();
        if elapsed >= event_time {
            Some(event)
        } else {
            None
        }
    }

    /// Update the trigger corner (called when config changes)
    pub fn set_trigger_corner(&mut self, new_corner: TriggerCorner) {
        debug!("TouchSimulator trigger corner changed from {:?} to {:?}", self.trigger_corner, new_corner);
        self.trigger_corner = new_corner;
    }

    /// Simulate a tap at middle bottom (for progress indication)
    pub async fn tap_middle_bottom(&self) -> Result<()> {
        debug!("TouchSimulator: Simulating tap at middle bottom");
        // In simulation mode, this is just logged
        Ok(())
    }
}
