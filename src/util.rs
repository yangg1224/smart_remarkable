use anyhow::Result;
use dotenv;
use image::GrayImage;
use log::{debug, info};
use resvg::render;
use resvg::tiny_skia::Pixmap;
use resvg::usvg;
use resvg::usvg::{fontdb, Options, Tree};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use crate::device::DeviceModel;
use crate::embedded_assets::{get_answer_font_data, get_uinput_module_data};

pub type OptionMap = HashMap<String, String>;

/// Whether xochitl's UI is currently rendered 180° rotated relative to the
/// panel/framebuffer (user holding the device flipped). Detected from each
/// screenshot (see Screenshot::detect_ui_rotated). When set, screenshots are
/// normalized to user space, and all synthetic pen/touch coordinates are
/// mirrored at the injection boundary so taps hit the UI elements the user
/// actually sees and ink reads upright to the user.
static UI_ROTATED_180: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn ui_rotated_180() -> bool {
    UI_ROTATED_180.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_ui_rotated_180(rotated: bool) {
    if rotated != ui_rotated_180() {
        info!("UI rotation state changed: rotated_180={}", rotated);
    }
    UI_ROTATED_180.store(rotated, std::sync::atomic::Ordering::Relaxed);
}

/// Map user-space virtual coordinates to panel-space virtual coordinates
/// (identity when the UI is not rotated). Involution, so it converts panel
/// reads back to user space too.
pub fn maybe_rot180_virtual((x, y): (i32, i32)) -> (i32, i32) {
    if ui_rotated_180() {
        (767 - x, 1023 - y)
    } else {
        (x, y)
    }
}

/// Build a font database with system fonts plus the bundled answer fonts
/// (see embedded_assets::get_answer_font_data), so drawn answers render with
/// a consistent, legible style regardless of what's installed on the device.
pub fn build_fontdb() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    for font_data in get_answer_font_data() {
        db.load_font_data(font_data);
    }
    db
}

pub fn svg_to_bitmap(svg_data: &str, width: u32, height: u32) -> Result<Vec<Vec<bool>>> {
    let mut opt = Options::default();
    opt.fontdb = Arc::new(build_fontdb());

    let tree = match Tree::from_str(svg_data, &opt) {
        Ok(tree) => tree,
        Err(e) => {
            info!("Error parsing SVG: {}. Using fallback SVG.", e);
            let fallback_svg = format!(
                r#"<svg width='{width}' height='{height}' xmlns='http://www.w3.org/2000/svg'><text x='100' y='900' font-family='Noto Sans' font-size='24'>ERROR!</text></svg>"#
            );
            Tree::from_str(&fallback_svg, &opt)?
        }
    };

    let mut pixmap = Pixmap::new(width, height).unwrap();
    // Scale transform so the SVG fills the requested bitmap size (not just its intrinsic size)
    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let transform = usvg::Transform::from_scale(scale_x, scale_y);
    render(&tree, transform, &mut pixmap.as_mut());

    let bitmap = pixmap
        .pixels()
        .chunks(width as usize)
        .map(|row| row.iter().map(|p| p.alpha() > 128).collect())
        .collect();

    Ok(bitmap)
}

/// Same as svg_to_bitmap but with configurable alpha threshold.
pub fn svg_to_bitmap_threshold(svg_data: &str, width: u32, height: u32, threshold: u8) -> Result<Vec<Vec<bool>>> {
    let mut opt = Options::default();
    opt.fontdb = Arc::new(build_fontdb());

    let tree = match Tree::from_str(svg_data, &opt) {
        Ok(tree) => tree,
        Err(e) => {
            info!("Error parsing SVG: {}. Using fallback SVG.", e);
            let fallback_svg = format!(
                r#"<svg width='{width}' height='{height}' xmlns='http://www.w3.org/2000/svg'><text x='100' y='900' font-family='Noto Sans' font-size='24'>ERROR!</text></svg>"#
            );
            Tree::from_str(&fallback_svg, &opt)?
        }
    };

    let mut pixmap = Pixmap::new(width, height).unwrap();
    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let transform = usvg::Transform::from_scale(scale_x, scale_y);
    render(&tree, transform, &mut pixmap.as_mut());

    let bitmap = pixmap
        .pixels()
        .chunks(width as usize)
        .map(|row| row.iter().map(|p| p.alpha() > threshold).collect())
        .collect();

    Ok(bitmap)
}

/// Decode a generated image (PNG/JPEG) into an ink bitmap for skeleton
/// tracing: grayscale, downscale so the longest side is at most `max_dim`
/// (thinning cost grows with area, and the pen's virtual canvas is only
/// 768x1024 anyway), then threshold dark pixels to ink. White background
/// with black line art is assumed, as requested from the image model.
pub fn image_to_ink_bitmap(image_bytes: &[u8], max_dim: u32) -> Result<Vec<Vec<bool>>> {
    let img = image::load_from_memory(image_bytes)?;
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();

    let scale = (max_dim as f32 / w.max(h) as f32).min(1.0);
    let gray = if scale < 1.0 {
        let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
        image::imageops::resize(&gray, nw, nh, image::imageops::FilterType::Triangle)
    } else {
        gray
    };

    let (w, h) = gray.dimensions();
    // Threshold on the dark side of mid-gray so anti-aliased edges don't
    // fatten the strokes before thinning.
    let bitmap: Vec<Vec<bool>> = (0..h).map(|y| (0..w).map(|x| gray.get_pixel(x, y).0[0] < 128).collect()).collect();

    let ink: usize = bitmap.iter().map(|row| row.iter().filter(|&&b| b).count()).sum();
    debug!("image_to_ink_bitmap: {}x{} bitmap, {} ink pixels", w, h, ink);
    Ok(bitmap)
}

/// Upscale a small base64 PNG (e.g. a cropped lasso selection of a few
/// hundred pixels) so the image-generation model gets enough resolution to
/// read the sketch. Returns the input unchanged if it is already at least
/// `min_dim` on its longest side, or on any decode error.
pub fn upscale_png_b64(b64: &str, min_dim: u32) -> String {
    use base64::prelude::*;
    let upscaled = || -> Result<String> {
        let bytes = BASE64_STANDARD.decode(b64)?;
        let img = image::load_from_memory(&bytes)?;
        let (w, h) = (img.width(), img.height());

        // Whiten the background: the crop of a lassoed region carries the
        // marquee's gray fill (~rgb 194); the image model reads the sketch
        // better as black ink on clean white.
        let mut gray = img.to_luma8();
        for p in gray.pixels_mut() {
            if p.0[0] > 150 {
                p.0[0] = 255;
            }
        }
        let img = image::DynamicImage::ImageLuma8(gray);

        let resized = if w.max(h) >= min_dim {
            img
        } else {
            let scale = min_dim as f32 / w.max(h) as f32;
            img.resize(
                (w as f32 * scale) as u32,
                (h as f32 * scale) as u32,
                image::imageops::FilterType::Lanczos3,
            )
        };
        let mut png = std::io::Cursor::new(Vec::new());
        resized.write_to(&mut png, image::ImageFormat::Png)?;
        debug!("upscale_png_b64: {}x{} -> {}x{}", w, h, resized.width(), resized.height());
        Ok(BASE64_STANDARD.encode(png.into_inner()))
    };
    upscaled().unwrap_or_else(|e| {
        info!("upscale_png_b64 failed ({}), sending original", e);
        b64.to_string()
    })
}

/// Same as svg_to_bitmap but returns alpha values (0-255) instead of boolean.
pub fn svg_to_alpha_bitmap(svg_data: &str, width: u32, height: u32) -> Result<Vec<Vec<u8>>> {
    let mut opt = Options::default();
    opt.fontdb = Arc::new(build_fontdb());

    let tree = match Tree::from_str(svg_data, &opt) {
        Ok(tree) => tree,
        Err(e) => {
            info!("Error parsing SVG: {}. Using fallback SVG.", e);
            let fallback_svg = format!(
                r#"<svg width='{width}' height='{height}' xmlns='http://www.w3.org/2000/svg'><text x='100' y='900' font-family='Noto Sans' font-size='24'>ERROR!</text></svg>"#
            );
            Tree::from_str(&fallback_svg, &opt)?
        }
    };

    let mut pixmap = Pixmap::new(width, height).unwrap();
    let svg_size = tree.size();
    let scale_x = width as f32 / svg_size.width();
    let scale_y = height as f32 / svg_size.height();
    let transform = usvg::Transform::from_scale(scale_x, scale_y);
    render(&tree, transform, &mut pixmap.as_mut());

    let alpha_bitmap = pixmap
        .pixels()
        .chunks(width as usize)
        .map(|row| row.iter().map(|p| p.alpha()).collect())
        .collect();

    Ok(alpha_bitmap)
}

/// Rewrite an SVG so that its inked content fits inside `rect` (virtual
/// 768x1024 coordinates). The content's bounding box is found by rasterizing,
/// then the whole SVG is wrapped in a uniform scale + translate that maps the
/// ink bbox into the rect, centered. Used by select mode to place the answer
/// into the box the user chose.
/// Scan a rasterized bitmap for its ink bounding box, in bitmap-pixel units.
fn bitmap_ink_bbox(bitmap: &[Vec<bool>]) -> Option<(f32, f32, f32, f32)> {
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for (y, row) in bitmap.iter().enumerate() {
        for (x, &pixel) in row.iter().enumerate() {
            if pixel {
                min_x = min_x.min(x as u32);
                min_y = min_y.min(y as u32);
                max_x = max_x.max(x as u32);
                max_y = max_y.max(y as u32);
            }
        }
    }
    if min_x > max_x {
        return None;
    }
    Some((min_x as f32, min_y as f32, (max_x - min_x + 1) as f32, (max_y - min_y + 1) as f32))
}

pub fn fit_svg_to_rect(svg_data: &str, rect: crate::touch::Rect) -> Result<String> {
    const CANVAS_W: f32 = 768.0;
    const CANVAS_H: f32 = 1024.0;

    // Find the ink bounding box in canvas coordinates
    let bitmap = svg_to_bitmap(svg_data, CANVAS_W as u32, CANVAS_H as u32)?;
    let Some((bbox_x, bbox_y, bbox_w, bbox_h)) = bitmap_ink_bbox(&bitmap) else {
        info!("fit_svg_to_rect: SVG has no visible content, leaving unchanged");
        return Ok(svg_data.to_string());
    };

    // The bitmap was rendered with the SVG's intrinsic size scaled to the
    // canvas; apply the same normalization inside the wrapper so inner
    // coordinates line up with the bbox we just measured.
    let mut opt = Options::default();
    opt.fontdb = Arc::new(build_fontdb());
    let tree = Tree::from_str(svg_data, &opt)?;
    let norm_x = CANVAS_W / tree.size().width();
    let norm_y = CANVAS_H / tree.size().height();

    // Uniform scale so the ink bbox fits in the rect. Cap the upscale so a
    // one-word answer doesn't balloon, anchor at the top of the rect (the
    // rect may extend to the page bottom), and center horizontally.
    const MAX_UPSCALE: f32 = 2.5;
    let scale = (rect.w as f32 / bbox_w).min(rect.h as f32 / bbox_h).min(MAX_UPSCALE);
    let tx = rect.x as f32 - bbox_x * scale + (rect.w as f32 - bbox_w * scale) / 2.0;
    let ty = rect.y as f32 - bbox_y * scale;

    // Clamp so scaled content can never be pushed off the physical canvas,
    // regardless of how wide/tall the model's content turned out to be —
    // don't rely on the model respecting the line-length guidance either.
    let tx = tx.clamp(0.0, (CANVAS_W - bbox_w * scale).max(0.0));
    let ty = ty.clamp(0.0, (CANVAS_H - bbox_h * scale).max(0.0));

    // Extract the inner content of the <svg> element
    let open_start = svg_data
        .find("<svg")
        .ok_or_else(|| anyhow::anyhow!("No <svg> element found"))?;
    let open_end = svg_data[open_start..]
        .find('>')
        .map(|i| open_start + i + 1)
        .ok_or_else(|| anyhow::anyhow!("Malformed <svg> element"))?;
    let close_start = svg_data
        .rfind("</svg>")
        .ok_or_else(|| anyhow::anyhow!("No </svg> closing tag found"))?;
    let inner = &svg_data[open_end..close_start];

    info!(
        "fit_svg_to_rect: bbox ({}, {}, {}, {}) -> rect ({}, {}, {}, {}), scale {:.3}",
        bbox_x, bbox_y, bbox_w, bbox_h, rect.x, rect.y, rect.w, rect.h, scale
    );

    Ok(format!(
        r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><g transform="translate({} {}) scale({} {})">{}</g></svg>"#,
        tx,
        ty,
        scale * norm_x,
        scale * norm_y,
        inner
    ))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Deterministically build an SVG from a list of plain-text lines, one
/// <text> element per line with fixed, non-overlapping placement. This
/// replaces relying on the LLM to compute its own x/y coordinates, which is
/// unenforced and can silently produce overlapping/garbled lines when the
/// model miscounts (see: the "explain option trading" garbled-answer bug).
///
/// Font size/line height shrink for longer answers, so more detail fits
/// without needing as much vertical page space, while staying legible.
const ANSWER_TEXT_X: u32 = 20;

/// Whether text contains a CJK ideograph. Deliberately narrow (Chinese being
/// the only non-Latin script we currently support answers in) rather than a
/// general script-detector.
fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x4E00..=0x9FFF   // CJK Unified Ideographs
            | 0x3400..=0x4DBF // CJK Extension A
            | 0x3000..=0x303F // CJK punctuation
            | 0xFF00..=0xFFEF // Fullwidth forms
        )
    })
}

/// Pick the exact font for a line — a single literal family name, not a
/// fallback list. usvg's fallback resolution is unreliable once the system
/// font database is loaded alongside our embedded fonts (a system CJK font
/// can win over our bundled one for glyph coverage, ignoring declared
/// order), so instead of hoping fallback picks the right font, decide it
/// ourselves: there's only one font registered under each of these exact
/// names, so there's no ambiguity for usvg to resolve.
fn answer_font_family(line: &str) -> &'static str {
    if contains_cjk(line) {
        "Noto Sans SC"
    } else {
        "Patrick Hand"
    }
}

/// (font_size, line_height) for a given number of answer lines: longer
/// answers use a smaller size/tighter spacing so they still fit legibly
/// without needing an ever-taller placement box.
fn answer_line_layout(n_lines: usize) -> (u32, u32) {
    if n_lines > 8 {
        (30, 42)
    } else {
        (40, 60)
    }
}

pub fn build_svg_from_lines(lines: &[String]) -> String {
    let (font_size, line_height) = answer_line_layout(lines.len());
    let first_y = line_height;

    let mut body = String::new();
    for (i, line) in lines.iter().enumerate() {
        let y = first_y + line_height * i as u32;
        let font_family = answer_font_family(line);
        body.push_str(&format!(
            r#"<text x="{ANSWER_TEXT_X}" y="{y}" font-family="{font_family}" font-size="{font_size}" fill="black">{}</text>"#,
            xml_escape(line)
        ));
    }

    format!(r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg">{body}</svg>"#)
}

/// Like fit_svg_to_rect, but for a list of answer lines: computes ONE shared
/// scale/position (from the combined answer's ink bounding box, so sizing is
/// consistent across all lines) and returns one small wrapped SVG per line,
/// in order. Drawing them one at a time with a pause in between makes the
/// answer appear progressively, like it's being handwritten, instead of all
/// at once — and doubles as a "still working" signal while it draws.
pub fn fit_lines_to_rect(lines: &[String], rect: crate::touch::Rect) -> Result<Vec<String>> {
    const CANVAS_W: f32 = 768.0;
    const CANVAS_H: f32 = 1024.0;
    const MAX_UPSCALE: f32 = 2.5;

    if lines.is_empty() {
        return Ok(vec![]);
    }

    let combined = build_svg_from_lines(lines);
    let bitmap = svg_to_bitmap(&combined, CANVAS_W as u32, CANVAS_H as u32)?;
    let Some((bbox_x, bbox_y, bbox_w, bbox_h)) = bitmap_ink_bbox(&bitmap) else {
        info!("fit_lines_to_rect: answer has no visible content");
        return Ok(vec![]);
    };

    // build_svg_from_lines always emits width="768" height="1024" exactly,
    // so (unlike fit_svg_to_rect, which fits arbitrary LLM-authored SVG)
    // there's no intrinsic-size normalization to apply here.
    let scale = (rect.w as f32 / bbox_w).min(rect.h as f32 / bbox_h).min(MAX_UPSCALE);
    let tx = rect.x as f32 - bbox_x * scale + (rect.w as f32 - bbox_w * scale) / 2.0;
    let ty = rect.y as f32 - bbox_y * scale;
    let tx = tx.clamp(0.0, (CANVAS_W - bbox_w * scale).max(0.0));
    let ty = ty.clamp(0.0, (CANVAS_H - bbox_h * scale).max(0.0));

    info!(
        "fit_lines_to_rect: {} lines, bbox ({}, {}, {}, {}) -> rect ({}, {}, {}, {}), scale {:.3}",
        lines.len(),
        bbox_x,
        bbox_y,
        bbox_w,
        bbox_h,
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        scale
    );

    let (font_size, line_height) = answer_line_layout(lines.len());
    let first_y = line_height;

    Ok(lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let y = first_y + line_height * i as u32;
            let font_family = answer_font_family(line);
            let text_el = format!(
                r#"<text x="{ANSWER_TEXT_X}" y="{y}" font-family="{font_family}" font-size="{font_size}" fill="black">{}</text>"#,
                xml_escape(line)
            );
            format!(
                r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><g transform="translate({tx} {ty}) scale({scale} {scale})">{text_el}</g></svg>"#
            )
        })
        .collect())
}

pub fn write_bitmap_to_file(bitmap: &[Vec<bool>], filename: &str) -> Result<()> {
    let width = bitmap[0].len();
    let height = bitmap.len();
    let mut img = GrayImage::new(width as u32, height as u32);

    for (y, row) in bitmap.iter().enumerate() {
        for (x, &pixel) in row.iter().enumerate() {
            img.put_pixel(x as u32, y as u32, image::Luma([if pixel { 0 } else { 255 }]));
        }
    }

    img.save(filename)?;
    info!("Bitmap saved to {}", filename);
    Ok(())
}

pub fn option_or_env(options: &OptionMap, key: &str, env_key: &str) -> String {
    let option = options.get(key);
    if let Some(value) = option {
        value.to_string()
    } else {
        std::env::var(env_key).unwrap().to_string()
    }
}

pub fn option_or_env_fallback(options: &OptionMap, key: &str, env_key: &str, fallback: &str) -> String {
    let option = options.get(key);
    if let Some(value) = option {
        value.to_string()
    } else {
        std::env::var(env_key).unwrap_or_else(|_| fallback.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::touch::Rect;

    #[test]
    fn build_svg_from_lines_never_overlaps_regardless_of_model_input() {
        // Regression test for the garbled-answer bug: previously the LLM
        // computed its own y-coordinates per the prompt spec and could
        // (and did) violate the spacing rule for longer answers. Lines are
        // now placed deterministically in Rust, so overlap is impossible
        // by construction no matter how many lines the model asks for.
        let lines: Vec<String> = (0..8).map(|i| format!("line number {i}")).collect();
        let svg = build_svg_from_lines(&lines);

        // Match the standalone y="..." attribute, not the tail of font-family="..."
        let ys: Vec<u32> = svg
            .match_indices(" y=\"")
            .map(|(idx, _)| {
                let rest = &svg[idx + 4..];
                let end = rest.find('"').unwrap();
                rest[..end].parse::<u32>().unwrap()
            })
            .collect();

        assert_eq!(ys.len(), 8);
        for w in ys.windows(2) {
            assert_eq!(w[1] - w[0], 60, "line spacing must be exactly 60px, got {:?}", ys);
        }
    }

    #[test]
    fn build_svg_from_lines_shrinks_font_for_long_answers() {
        // Beyond 8 lines the renderer should switch to the smaller tier
        // (30px font / 42px spacing) so detailed answers still fit legibly
        // without needing an ever-taller placement box.
        let lines: Vec<String> = (0..14).map(|i| format!("point {i}")).collect();
        let svg = build_svg_from_lines(&lines);

        assert!(svg.contains("font-size=\"30\""), "expected smaller font tier for 14 lines");
        assert!(!svg.contains("font-size=\"40\""));

        let ys: Vec<u32> = svg
            .match_indices(" y=\"")
            .map(|(idx, _)| {
                let rest = &svg[idx + 4..];
                let end = rest.find('"').unwrap();
                rest[..end].parse::<u32>().unwrap()
            })
            .collect();
        assert_eq!(ys.len(), 14);
        for w in ys.windows(2) {
            assert_eq!(w[1] - w[0], 42, "line spacing must be exactly 42px in the smaller tier, got {:?}", ys);
        }
    }

    #[test]
    fn build_svg_from_lines_escapes_xml_special_chars() {
        let svg = build_svg_from_lines(&["5 < 10 & \"quoted\"".to_string()]);
        assert!(svg.contains("5 &lt; 10 &amp; &quot;quoted&quot;"));
        assert!(!svg.contains("< 10")); // raw '<' would break XML parsing
    }

    #[test]
    fn fit_lines_to_rect_returns_one_svg_per_line_with_shared_transform() {
        let lines: Vec<String> = vec!["First point here".to_string(), "Second point here".to_string(), "Third point here".to_string()];
        let rect = Rect { x: 50, y: 400, w: 600, h: 400 };

        let per_line = fit_lines_to_rect(&lines, rect).unwrap();
        assert_eq!(per_line.len(), 3);

        // Every line must share the exact same translate/scale, so they line
        // up consistently on the page even though drawn one at a time.
        let transforms: Vec<&str> = per_line
            .iter()
            .map(|svg| {
                let start = svg.find("translate(").unwrap();
                let end = svg[start..].find(')').unwrap();
                &svg[start..start + end]
            })
            .collect();
        assert_eq!(transforms[0], transforms[1]);
        assert_eq!(transforms[1], transforms[2]);

        // Each fragment draws only its own line's ink, all within the rect
        for svg in &per_line {
            let bitmap = svg_to_bitmap(svg, 768, 1024).unwrap();
            let (min_x, min_y, max_x, max_y) = bitmap_ink_bbox(&bitmap).expect("each line should have visible ink");
            assert!(min_x as i32 >= rect.x - 2);
            assert!(min_y as i32 >= rect.y - 2);
            assert!(max_x as i32 <= rect.x + rect.w + 2);
            assert!(max_y as i32 <= rect.y + rect.h + 2);
        }
    }

    fn ink_bbox(bitmap: &[Vec<bool>]) -> Option<(u32, u32, u32, u32)> {
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &p) in row.iter().enumerate() {
                if p {
                    min_x = min_x.min(x as u32);
                    min_y = min_y.min(y as u32);
                    max_x = max_x.max(x as u32);
                    max_y = max_y.max(y as u32);
                }
            }
        }
        if min_x > max_x {
            None
        } else {
            Some((min_x, min_y, max_x, max_y))
        }
    }

    #[test]
    fn fit_svg_to_rect_places_content_inside_rect() {
        // A rect drawn near the top-left of the full canvas
        let svg = r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><rect x="10" y="10" width="400" height="200" fill="none" stroke="black" stroke-width="4"/></svg>"#;
        let target = Rect { x: 300, y: 500, w: 200, h: 150 };

        let fitted = fit_svg_to_rect(svg, target).unwrap();
        let bitmap = svg_to_bitmap(&fitted, 768, 1024).unwrap();
        let (min_x, min_y, max_x, max_y) = ink_bbox(&bitmap).expect("fitted SVG should have ink");

        // All ink must be inside the target rect (small tolerance for stroke rounding)
        assert!(min_x as i32 >= target.x - 2, "min_x {} outside rect", min_x);
        assert!(min_y as i32 >= target.y - 2, "min_y {} outside rect", min_y);
        assert!(max_x as i32 <= target.x + target.w + 2, "max_x {} outside rect", max_x);
        assert!(max_y as i32 <= target.y + target.h + 2, "max_y {} outside rect", max_y);

        // And it should fill most of the limiting dimension (uniform scale, 400x200 into 200x150)
        let ink_w = max_x - min_x + 1;
        assert!(ink_w >= 190, "ink width {} should nearly fill rect width", ink_w);
    }

    #[test]
    fn fit_svg_to_rect_handles_empty_svg() {
        let svg = r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"></svg>"#;
        let target = Rect { x: 100, y: 100, w: 100, h: 100 };
        let fitted = fit_svg_to_rect(svg, target).unwrap();
        assert_eq!(fitted, svg);
    }
}

pub fn setup_uinput() -> Result<()> {
    debug!("Checking for uinput module");

    // Use DeviceModel to detect the device type
    let device_model = DeviceModel::detect();
    info!("Device model detected: {}", device_model.name());

    if device_model != DeviceModel::RemarkablePaperPro {
        info!("Not a Paper Pro, skipping uinput module check and installation");
        return Ok(());
    }

    // If /dev/uinput already exists, the kernel has uinput built in
    if std::path::Path::new("/dev/uinput").exists() {
        info!("/dev/uinput exists, kernel has uinput built in, skipping module loading");
        return Ok(());
    }

    // Check if uinput module is loaded by looking at the lsmod output
    let output = std::process::Command::new("lsmod").output().expect("Failed to execute lsmod");
    let output_str = std::str::from_utf8(&output.stdout).unwrap();
    if output_str.contains("uinput") {
        debug!("uinput module already loaded");
    } else {
        info!("uinput module not found, installing bundled version");

        let os_info_path = String::from("/etc/os-release");
        if std::path::Path::new(os_info_path.as_str()).exists() {
            dotenv::from_path(os_info_path)?;
        }

        let img_version = std::env::var("IMG_VERSION").unwrap_or_default();

        if img_version.is_empty() {
            return Ok(());
        }

        let short_version = img_version.split('.').take(2).collect::<Vec<&str>>().join(".");

        // let target_module_filename = format!("rmpp/uinput-{short_version}.ko");

        // Use the function from embedded_assets module to get the module data
        let uinput_module_data = get_uinput_module_data(&short_version).unwrap_or_else(|| panic!("Uinput module for version {} not found", short_version));
        let raw_uinput_module_data = uinput_module_data.as_slice();
        let mut uinput_module_file = std::fs::File::create("/tmp/uinput.ko")?;
        uinput_module_file.write_all(raw_uinput_module_data)?;
        uinput_module_file.flush()?;
        drop(uinput_module_file);
        let output = std::process::Command::new("insmod").arg("/tmp/uinput.ko").output()?;
        let output_str = std::str::from_utf8(&output.stderr).unwrap();
        info!("insmod output: {}", output_str);
    }

    Ok(())
}

#[test]
fn render_chinese_font_test_png() {
    // Exercises the real build_svg_from_lines path (system fonts + embedded
    // fonts both loaded, as on the device) to confirm per-line explicit font
    // selection isn't affected by competing system CJK fonts.
    let lines: Vec<String> = vec![
        "Own stock, sell call option".to_string(),
        "这是买入并持有股票".to_string(),
        "Mixed: 卖出call期权 earns 溢价".to_string(),
    ];
    let svg = build_svg_from_lines(&lines);
    let bitmap = svg_to_bitmap(&svg, 768, 1024).unwrap();
    write_bitmap_to_file(&bitmap, "/private/tmp/claude-501/-Users-yang-Downloads-remarkable-app/8697c15e-ba77-4d86-a32d-4eb6ec49b9f8/scratchpad/chinese_font_test.png").unwrap();
}

#[test]
fn answer_font_family_picks_chinese_font_for_cjk_lines() {
    assert_eq!(answer_font_family("Own stock, sell call option"), "Patrick Hand");
    assert_eq!(answer_font_family("这是买入并持有股票"), "Noto Sans SC");
    assert_eq!(answer_font_family("Mixed: 卖出call期权 earns 溢价"), "Noto Sans SC");
}
