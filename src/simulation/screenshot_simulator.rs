use super::SimulationConfig;
use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use log::{debug, info, warn};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Screenshot simulator that cycles through test images
/// instead of taking real device screenshots
pub struct ScreenshotSimulator {
    image_files: Vec<PathBuf>,
    current_index: Arc<Mutex<usize>>,
}

impl ScreenshotSimulator {
    pub fn new(config: SimulationConfig) -> Result<Self> {
        let image_files = if let Some(ref screenshot_dir) = config.screenshot_dir {
            Self::load_image_files(screenshot_dir)?
        } else {
            Vec::new()
        };

        info!("ScreenshotSimulator initialized with {} test images", image_files.len());
        for (i, file) in image_files.iter().enumerate() {
            debug!("  {}: {}", i, file.display());
        }

        Ok(Self {
            image_files,
            current_index: Arc::new(Mutex::new(0)),
        })
    }

    /// Load image files from directory (PNG, JPG, JPEG)
    fn load_image_files(dir_path: &str) -> Result<Vec<PathBuf>> {
        let dir = fs::read_dir(dir_path)?;
        let mut files = Vec::new();

        for entry in dir {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(ext) = path.extension() {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    if matches!(ext_str.as_str(), "png" | "jpg" | "jpeg") {
                        files.push(path);
                    }
                }
            }
        }

        // Sort files for consistent ordering
        files.sort();

        if files.is_empty() {
            warn!("No image files found in directory: {}", dir_path);
        }

        Ok(files)
    }

    /// Get base64 encoded image data from current test image
    pub fn get_base64_image(&self) -> Result<String> {
        if self.image_files.is_empty() {
            return Err(anyhow::anyhow!(
                "No test images available. Use --test-screenshot-dir to specify a directory with PNG/JPG files."
            ));
        }

        let index = {
            let current_index = self.current_index.lock().unwrap();
            *current_index
        };

        let file_path = &self.image_files[index % self.image_files.len()];
        info!("Using test screenshot: {} (index: {})", file_path.display(), index);

        let image_data = fs::read(file_path)?;
        let base64_data = general_purpose::STANDARD.encode(&image_data);

        Ok(base64_data)
    }

    /// Get pixel at virtual coordinate — always returns white in simulation (no UI chrome).
    pub fn get_pixel(&self, _vx: u32, _vy: u32) -> Option<(u8, u8, u8)> {
        Some((255, 255, 255)) // white — palette never open in simulation
    }

    /// Advance to next test image (called manually or automatically)
    pub fn advance_to_next_image(&self) {
        if !self.image_files.is_empty() {
            let mut index = self.current_index.lock().unwrap();
            *index = (*index + 1) % self.image_files.len();
            let next_file = &self.image_files[*index];
            info!("Advanced to next test image: {} (index: {})", next_file.display(), *index);
        }
    }

    /// Set specific image by index (for web API control)
    pub fn set_image_index(&self, new_index: usize) -> Result<()> {
        if self.image_files.is_empty() {
            return Err(anyhow::anyhow!("No test images available"));
        }

        let clamped_index = new_index % self.image_files.len();
        {
            let mut index = self.current_index.lock().unwrap();
            *index = clamped_index;
        }

        let file = &self.image_files[clamped_index];
        info!("Set test image to: {} (index: {})", file.display(), clamped_index);
        Ok(())
    }

    /// Get current image info
    pub fn get_current_image_info(&self) -> Option<(usize, String, usize)> {
        if self.image_files.is_empty() {
            return None;
        }

        let index = {
            let current_index = self.current_index.lock().unwrap();
            *current_index
        };

        let file_path = &self.image_files[index % self.image_files.len()];
        let file_name = file_path.file_name()?.to_string_lossy().to_string();

        Some((index, file_name, self.image_files.len()))
    }

    /// Save simulated screenshot (for compatibility with real Screenshot)
    pub fn save_image(&self, save_path: &str) -> Result<()> {
        if self.image_files.is_empty() {
            return Err(anyhow::anyhow!("No test images available to save"));
        }

        let index = {
            let current_index = self.current_index.lock().unwrap();
            *current_index
        };

        let source_path = &self.image_files[index % self.image_files.len()];
        fs::copy(source_path, save_path)?;
        info!("Copied test image {} to {}", source_path.display(), save_path);
        Ok(())
    }
}
