use anyhow::Result;
use base64::prelude::*;
use log::{debug, info};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch, Mutex as TokioMutex};
use tokio::time::{sleep, Duration};

use crate::cancellation::SmartRemarkableCancellation;
use crate::config::Config;
use crate::embedded_assets::load_config;
use crate::keyboard::Keyboard;
use crate::llm_engine::{LLMEngine, ModelExecutionStatus};
use crate::screenshot::Screenshot;
use crate::segmenter::ImageAnalyzer;
use crate::simulation::SimulationConfig;
use crate::touch::{Rect, Touch, TriggerSource};

/// Events that can trigger AI processing
#[derive(Debug, Clone)]
pub enum TriggerEvent {
    /// User touched the trigger corner
    UserTouch { source: TriggerSource },
    /// User touched the trigger corner, then tapped the corners of a
    /// selection box and an answer-placement box (select mode)
    UserSelection {
        selection: Rect,
        placement: Rect,
        source: TriggerSource,
    },
    /// Trigger via web API (for testing/simulation)
    WebTrigger,
}

/// Progress states during AI processing
/// Uses ModelExecutionStatus for LLM operations, plus additional states for the full workflow
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressState {
    /// No processing happening
    Idle,
    /// Waiting for user trigger
    WaitingForTrigger,
    /// Taking screenshot
    TakingScreenshot,
    /// LLM execution state
    LlmState(ModelExecutionStatus),
    /// Processing completed successfully
    Done,
}

/// Message from coordinator to processing task
#[derive(Debug)]
pub struct ProcessingRequest {
    /// The trigger event that started this
    pub trigger: TriggerEvent,
}

/// Communication channels for the coordinator
pub struct CoordinatorChannels {
    /// Send trigger events to coordinator
    pub trigger_tx: mpsc::Sender<TriggerEvent>,
    /// Receive trigger events in coordinator
    pub trigger_rx: mpsc::Receiver<TriggerEvent>,

    /// Broadcast progress state updates
    pub progress_tx: watch::Sender<ProgressState>,
    /// Receive progress state updates
    pub progress_rx: watch::Receiver<ProgressState>,
}

impl CoordinatorChannels {
    pub fn new() -> Self {
        let (trigger_tx, trigger_rx) = mpsc::channel(10);
        let (progress_tx, progress_rx) = watch::channel(ProgressState::Idle);

        Self {
            trigger_tx,
            trigger_rx,
            progress_tx,
            progress_rx,
        }
    }
}

impl Default for CoordinatorChannels {
    fn default() -> Self {
        Self::new()
    }
}

/// Task that waits for triggers and notifies the coordinator
pub async fn trigger_task(
    touch: Arc<tokio::sync::RwLock<Touch>>,
    trigger_tx: mpsc::Sender<TriggerEvent>,
    cancellation: Arc<SmartRemarkableCancellation>,
    no_trigger: bool,
    collect_taps: bool,
) -> Result<()> {
    info!("Trigger task starting");

    loop {
        debug!("Trigger loop looping");

        if no_trigger {
            debug!("No-trigger mode: auto-triggering");
            if trigger_tx
                .send(TriggerEvent::UserTouch {
                    source: TriggerSource::Touch,
                })
                .await
                .is_err()
            {
                info!("Trigger receiver dropped, exiting trigger task");
                break;
            }
            // In no-trigger mode, wait a bit before next auto-trigger or check for cancellation
            tokio::select! {
                _ = sleep(Duration::from_millis(100)) => {
                    if cancellation.should_cancel_main() {
                        info!("Trigger task: cancelled in no-trigger mode");
                        break;
                    }
                }
                _ = async {
                    while !cancellation.should_cancel_main() {
                        sleep(Duration::from_millis(10)).await;
                    }
                } => {
                    info!("Trigger task: cancelled in no-trigger mode");
                    break;
                }
            }
            continue;
        }

        info!("Trigger task: waiting for touch trigger...");

        debug!("Trigger task: about to acquire touch write lock");
        let mut touch_guard = touch.write().await;
        debug!("Trigger task: acquired touch write lock, calling wait_for_trigger");

        match touch_guard.wait_for_trigger(&cancellation).await {
            Ok(()) => {
                debug!("Trigger task: wait_for_trigger returned Ok, touch detected");
                info!("Trigger task: touch detected");

                let source = touch_guard.last_trigger_source();

                // In select mode, collect the selection and placement box corners
                // while we still hold the touch event stream
                let event = if collect_taps && touch_guard.is_real() {
                    match collect_selection(&mut touch_guard, &cancellation, source).await {
                        Ok(event) => event,
                        Err(e) => {
                            if e.to_string().contains("cancelled") {
                                info!("Trigger task: cancelled during selection");
                                return Ok(());
                            }
                            info!("Trigger task: selection failed ({}), ignoring trigger", e);
                            continue;
                        }
                    }
                } else {
                    TriggerEvent::UserTouch { source }
                };

                // Drop the lock before sending the event so processing_task can acquire it
                drop(touch_guard);
                debug!("Trigger task: dropped touch write lock");

                if trigger_tx.send(event).await.is_err() {
                    info!("Trigger receiver dropped, exiting trigger task");
                    break;
                }
                debug!("Trigger task: sent trigger event, continuing loop");

                // Give processing_task a moment to acquire the lock before we loop back
                sleep(Duration::from_millis(50)).await;
            }
            Err(e) => {
                debug!("Trigger task: wait_for_trigger returned Err: {}", e);
                if e.to_string().contains("cancelled") {
                    info!("Trigger task: cancelled (likely config change)");
                    return Ok(()); // Clean exit for restart
                } else {
                    info!("Trigger task: error waiting for trigger: {}", e);
                    return Err(e);
                }
            }
        }
    }

    debug!("Escaped from trigger task loop");

    Ok(())
}

/// Collect the four taps that define the selection box (what to answer)
/// and the placement box (where to draw the answer): two opposite corners each.
async fn collect_selection(
    touch: &mut Touch,
    cancellation: &SmartRemarkableCancellation,
    source: TriggerSource,
) -> Result<TriggerEvent> {
    info!("Select mode: tap two corners of the handwriting to select");
    let sel_a = touch.wait_for_tap(cancellation).await?;
    let sel_b = touch.wait_for_tap(cancellation).await?;
    let selection = Rect::from_corners(sel_a, sel_b);
    info!("Select mode: selection box {:?}; now tap two corners for the answer box", selection);

    let place_a = touch.wait_for_tap(cancellation).await?;
    let place_b = touch.wait_for_tap(cancellation).await?;
    let placement = Rect::from_corners(place_a, place_b);
    info!("Select mode: placement box {:?}", placement);

    Ok(TriggerEvent::UserSelection {
        selection,
        placement,
        source,
    })
}

/// Task that monitors for cancel touch during processing
pub async fn cancel_monitor_task(touch: Arc<tokio::sync::RwLock<Touch>>, cancellation: Arc<SmartRemarkableCancellation>) -> Result<()> {
    info!("Cancel monitor task: starting");

    // Wait for any touch to cancel
    match touch.write().await.wait_for_trigger(&cancellation).await {
        Ok(()) => {
            info!("Cancel monitor task: touch detected, cancelling processing");
            cancellation.cancel_execution();
            Ok(())
        }
        Err(e) => {
            if e.to_string().contains("cancelled") {
                info!("Cancel monitor task: processing completed before touch");
                Ok(())
            } else {
                info!("Cancel monitor task: error: {}", e);
                Err(e)
            }
        }
    }
}

/// Task that displays progress updates on the keyboard
pub async fn progress_task(
    keyboard: Arc<Mutex<Keyboard>>,
    mut progress_rx: watch::Receiver<ProgressState>,
    cancellation: Arc<SmartRemarkableCancellation>,
) -> Result<()> {
    info!("Progress task starting");

    let mut current_state = ProgressState::Idle;
    let cancel_token = cancellation.execution_token();

    loop {
        tokio::select! {
            // Check for cancellation
            _ = cancel_token.cancelled() => {
                info!("Progress task cancelled");
                // Clear any progress display
                if let Ok(mut kb) = keyboard.lock() {
                    let _ = kb.progress_end();
                }
                return Ok(());
            }

            // Watch for progress updates
            result = progress_rx.changed() => {
                if result.is_err() {
                    info!("Progress sender dropped, exiting progress task");
                    break;
                }

                let new_state = progress_rx.borrow().clone();
                if new_state != current_state {
                    current_state = new_state.clone();

                    match &current_state {
                        ProgressState::Idle => {
                            info!("Progress: Idle");
                            if let Ok(mut kb) = keyboard.lock() {
                                let _ = kb.progress_end();
                            }
                        }
                        ProgressState::WaitingForTrigger => {
                            info!("Progress: Waiting for trigger");
                        }
                        ProgressState::TakingScreenshot => {
                            info!("Progress: Taking screenshot...");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::BuildingContext) => {
                            info!("Progress: Building context...");
                            if let Ok(mut kb) = keyboard.lock() {
                                let _ = kb.progress("Thinking");
                            }
                        }
                        ProgressState::LlmState(ModelExecutionStatus::LlmProcessing) => {
                            info!("Progress: Thinking...");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::ProcessingResponse) => {
                            info!("Progress: Processing response...");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::CallingTools) => {
                            info!("Progress: Executing tools...");
                            if let Ok(mut kb) = keyboard.lock() {
                                let _ = kb.progress_end();
                            }
                        }
                        ProgressState::LlmState(ModelExecutionStatus::Done) => {
                            debug!("Progress: LLM Done");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::Error(msg)) => {
                            debug!("Progress: Error - {}", msg);
                        }
                        ProgressState::Done => {
                            debug!("Progress: Done");
                        }
                    }
                }
            }

            // Add dots for thinking state
            _ = sleep(Duration::from_millis(500)) => {
                if matches!(current_state, ProgressState::LlmState(ModelExecutionStatus::LlmProcessing)) {
                    if let Ok(mut kb) = keyboard.lock() {
                        let _ = kb.progress(".");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Pick a box for the answer near a detected selection: directly below it,
/// or above if there is no room. The user can move/resize it afterwards with
/// the native selection tool.
fn auto_placement(sel: Rect) -> Rect {
    const SCREEN_W: i32 = 768;
    const SCREEN_H: i32 = 1024;
    const GAP: i32 = 16;
    const MARGIN: i32 = 10;

    // Give the answer all the space from below the selection to the bottom
    // of the page; fit_svg_to_rect anchors at the top and only uses what the
    // answer needs, so long answers keep a legible size instead of being
    // squeezed into a fixed-height box.
    let w = (sel.w * 3 / 2).clamp(300, SCREEN_W - 2 * MARGIN);
    let x = sel.x.clamp(MARGIN, SCREEN_W - MARGIN - w);
    let below_y = sel.y + sel.h + GAP;
    let space_below = SCREEN_H - MARGIN - below_y;
    let (y, h) = if space_below >= 160 {
        (below_y, space_below)
    } else {
        // No room below: use the space above the selection instead
        (MARGIN, (sel.y - GAP - MARGIN).max(160))
    };
    Rect { x, y, w, h }
}

/// Task that processes a trigger: screenshot → LLM → tool execution
pub async fn processing_task(
    config: Config,
    engine: Arc<TokioMutex<Box<dyn LLMEngine>>>,
    progress_tx: watch::Sender<ProgressState>,
    cancellation: Arc<SmartRemarkableCancellation>,
    touch: Arc<tokio::sync::RwLock<Touch>>,
    selection: Option<(Rect, Rect)>,
    placement_slot: Arc<Mutex<Option<Rect>>>,
    selection_slot: Arc<Mutex<Option<Rect>>>,
    trigger_source: TriggerSource,
) -> Result<()> {
    info!("Processing task: starting");

    // Update progress: taking screenshot
    info!("Setting ProgressState::TakingScreenshot");
    let _ = progress_tx.send(ProgressState::TakingScreenshot);
    tokio::time::sleep(Duration::from_millis(10)).await; // Give progress_task time

    // Take screenshot
    let screenshot_path = config.save_screenshot.clone();
    let mut selection = selection;
    let base64_image = if let Some(input_png) = &config.input_png {
        BASE64_STANDARD.encode(std::fs::read(input_png)?)
    } else {
        let mut screenshot = if config.is_test_mode() {
            let simulation_config = SimulationConfig::from_config(&config);
            Screenshot::new_simulated(simulation_config)?
        } else {
            Screenshot::new()?
        };
        screenshot.take_screenshot()?;
        if let Some(save_screenshot) = &config.save_screenshot {
            info!("Saving screenshot to {}", save_screenshot);
            screenshot.save_image(save_screenshot)?;
        }

        // Select mode without tapped boxes (four-finger trigger): look for the
        // native selection-tool marquee in the screenshot and answer below it
        if selection.is_none() && config.select_mode {
            match screenshot.detect_selection_rect() {
                Some(marquee) => {
                    let placement = auto_placement(marquee);
                    info!("Detected selection marquee {:?}, answering into {:?}", marquee, placement);
                    selection = Some((marquee, placement));
                }
                None => {
                    info!("No selection marquee found; ignoring trigger (select something first)");
                    let _ = progress_tx.send(ProgressState::Done);
                    return Ok(());
                }
            }
        }

        if let Some((selection_rect, _)) = &selection {
            screenshot.base64_cropped(*selection_rect)?
        } else {
            screenshot.base64()?
        }
    };

    // Arm the placement slot so the draw_svg tool scales the answer into
    // the box the user chose, and the selection slot so the Draw button's
    // draw_sketch tool can redraw into the ORIGINAL lassoed box instead
    // (when the model reports the selection was already a drawing).
    if let Some((selection_rect, placement_rect)) = &selection {
        if let Ok(mut slot) = placement_slot.lock() {
            *slot = Some(*placement_rect);
        }
        if let Ok(mut slot) = selection_slot.lock() {
            *slot = Some(*selection_rect);
        }
    }

    if config.no_submit {
        info!("Skipping LLM submission (no_submit mode)");
        let _ = progress_tx.send(ProgressState::Done);
        return Ok(());
    }

    // Tap middle bottom to position cursor for text input (before showing "Thinking")
    if let Err(e) = touch.write().await.tap_middle_bottom().await {
        info!("Failed to tap middle bottom: {}", e);
    }

    // Update progress: building context
    let _ = progress_tx.send(ProgressState::LlmState(ModelExecutionStatus::BuildingContext));
    tokio::time::sleep(Duration::from_millis(10)).await; // Give progress_task time

    // Apply segmentation if requested
    let segmentation_description = if config.apply_segmentation {
        let image_path = config
            .input_png
            .as_ref()
            .or(screenshot_path.as_ref())
            .ok_or_else(|| anyhow::anyhow!("Segmentation requires either input_png or save_screenshot"))?;

        info!("Applying segmentation to {}", image_path);
        let analyzer = ImageAnalyzer::new(0.001, 10); // min_region_size=0.1%, max_regions=10
        match analyzer.analyze_image(image_path) {
            Ok(result) => {
                let description = analyzer.generate_description(&result);
                info!("Segmentation found {} regions", result.regions.len());
                Some(description)
            }
            Err(e) => {
                info!("Segmentation failed: {}, continuing without it", e);
                None
            }
        }
    } else {
        None
    };

    // Load prompt. The Draw button overrides the normal select-mode prompt
    // with prompts/draw.json regardless of --prompt/config.prompt, since it's
    // a distinct action (sketch/refine) from the LLM button's Q&A behavior.
    let prompt_name = if config.select_mode && trigger_source == TriggerSource::DrawButton {
        "draw.json".to_string()
    } else {
        config.prompt.clone()
    };
    let prompt_general_raw = load_config(&prompt_name);
    let prompt_general_json = serde_json::from_str::<serde_json::Value>(prompt_general_raw.as_str())?;
    let mut prompt = prompt_general_json["prompt"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Prompt file '{}' missing required 'prompt' field", prompt_name))?
        .to_string();

    // Add segmentation to prompt if available
    if let Some(seg_desc) = segmentation_description {
        prompt.push_str("\n\nImage Analysis:\n");
        prompt.push_str(&seg_desc);
    }

    // Prepare engine
    let mut engine_guard = engine.lock().await;
    engine_guard.clear_content();
    engine_guard.add_image_content(&base64_image);
    engine_guard.add_text_content(&prompt);

    // Create status callback that wraps model execution status in LlmState
    let progress_tx_clone = progress_tx.clone();
    let status_callback = Some(Box::new(move |status: ModelExecutionStatus| {
        let _ = progress_tx_clone.send(ProgressState::LlmState(status));
    }) as Box<dyn FnMut(ModelExecutionStatus) + Send>);

    // Execute LLM with proper error handling
    info!("Processing task: calling LLM");
    let execution_result = engine_guard.execute(&cancellation, status_callback).await;

    // Write model output if configured
    if let Some(model_output_file) = &config.model_output_file {
        info!("Would write model output to {}", model_output_file);
        // Note: The actual model output would need to be captured from the engine
        // This is a placeholder - the LLMEngine trait would need to expose the raw response
    }

    // Disarm both slots so later non-select runs draw normally
    if let Ok(mut slot) = placement_slot.lock() {
        slot.take();
    }
    if let Ok(mut slot) = selection_slot.lock() {
        slot.take();
    }

    // Handle execution result
    match execution_result {
        Ok(_) => {
            let _ = progress_tx.send(ProgressState::Done);
            info!("Processing task: completed successfully");
            Ok(())
        }
        Err(e) => {
            let error_msg = e.to_string();
            info!("Processing task: LLM error: {}", error_msg);

            // Only send error state if not already cancelled
            if !error_msg.contains("cancelled") && !error_msg.contains("canceled") {
                let _ = progress_tx.send(ProgressState::LlmState(ModelExecutionStatus::Error(error_msg.clone())));
                // Keep error visible for a moment
                sleep(Duration::from_secs(2)).await;
            }

            // Return to idle state
            let _ = progress_tx.send(ProgressState::Idle);
            Err(e)
        }
    }
}
