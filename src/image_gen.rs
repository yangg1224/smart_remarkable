use anyhow::{anyhow, Result};
use base64::prelude::*;
use log::{debug, info};
use serde_json::{json, Value};
use std::time::Duration;

/// Google's image-generation model ("nano banana"). Used when --image-model
/// is set; the chat LLM plans the drawing, this model renders it.
pub const DEFAULT_IMAGE_MODEL: &str = "gemini-2.5-flash-image";

/// Appended to every image prompt. The output is traced into single-weight
/// pen strokes by the skeleton pipeline, so anything that isn't a clean
/// black stroke on white (gray tones, stipple, solid fills) traces badly:
/// gray dithers into thousands of speck-strokes and fills skeletonize into
/// noise. Constrain the model to strokes the plotter can actually draw.
const STYLE_SUFFIX: &str = "\n\nStyle requirements (strict): professional concept-art line drawing with confident, flowing strokes; pure black ink lines on a pure white background. Rich in detail: draw full internal linework — contours, panel lines, structural elements, surface features — like a polished industrial-design illustration, never a minimal outline. Line art ONLY: no gray tones, no gradients, no stippling or dotted shading, no solid fills, no color. Sparse parallel hatching lines are allowed for depth, but keep them light and well separated. No text, labels, watermarks, signatures, borders, or background scenery. The drawing will be traced stroke-by-stroke onto an e-ink notebook by a pen plotter, so every mark must be a clean, deliberate stroke.";

pub struct ImageGen {
    model: String,
    base_url: String,
    api_key: String,
}

impl ImageGen {
    pub fn new(model: &str, api_key: Option<&str>, base_url: Option<&str>) -> Result<Self> {
        let api_key = api_key
            .map(str::to_string)
            .or_else(|| std::env::var("GEMINI_API_KEY").ok())
            .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
            .ok_or_else(|| anyhow!("image generation requires an API key: pass --image-api-key or set GEMINI_API_KEY/GOOGLE_API_KEY"))?;
        let base_url = base_url
            .map(str::to_string)
            .or_else(|| std::env::var("GOOGLE_BASE_URL").ok())
            .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string());
        Ok(Self {
            model: model.to_string(),
            base_url,
            api_key,
        })
    }

    /// Generate a line-art image from a prompt, optionally conditioned on an
    /// input image (base64 PNG) for sketch-enhancement mode. Returns the raw
    /// bytes of the generated image (PNG/JPEG, whatever the API returns —
    /// the `image` crate sniffs the format when decoding).
    pub async fn generate(&self, prompt: &str, input_png_b64: Option<&str>) -> Result<Vec<u8>> {
        let mut parts = Vec::new();
        if let Some(img) = input_png_b64 {
            parts.push(json!({
                "inline_data": { "mime_type": "image/png", "data": img }
            }));
        }
        parts.push(json!({ "text": format!("{}{}", prompt, STYLE_SUFFIX) }));

        let body = json!({
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": { "responseModalities": ["TEXT", "IMAGE"] }
        });

        info!("ImageGen: requesting {} image (input image attached: {})", self.model, input_png_b64.is_some());

        let client = reqwest::Client::builder().timeout(Duration::from_secs(180)).build()?;
        let response = client
            .post(format!("{}/v1beta/models/{}:generateContent", self.base_url, self.model))
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;
        if !status.is_success() {
            let detail: String = body_text.chars().take(500).collect();
            return Err(anyhow!("image API error {}: {}", status, detail));
        }

        let json: Value = serde_json::from_str(&body_text)?;
        debug!("ImageGen response keys: {:?}", json.as_object().map(|o| o.keys().collect::<Vec<_>>()));

        let parts = json["candidates"][0]["content"]["parts"]
            .as_array()
            .ok_or_else(|| anyhow!("image API response has no content parts"))?;

        for part in parts {
            let data = part["inlineData"]["data"].as_str().or_else(|| part["inline_data"]["data"].as_str());
            if let Some(data) = data {
                let bytes = BASE64_STANDARD.decode(data)?;
                info!("ImageGen: received {} byte image", bytes.len());
                return Ok(bytes);
            }
        }

        // Surface any text the model returned instead (usually a refusal)
        let text = parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" ");
        Err(anyhow!("image API returned no image; model said: {}", text.chars().take(300).collect::<String>()))
    }
}
