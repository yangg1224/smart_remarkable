//! Offline check of the image-generation drawing pipeline: decode an image,
//! threshold to ink, thin, trace, and render the traced strokes back to a
//! PNG for visual inspection. Runs only when TRACE_IMAGE_INPUT is set:
//!   TRACE_IMAGE_INPUT=in.png TRACE_IMAGE_OUTPUT=out.png cargo test --test trace_image -- --nocapture

use smart_remarkable::{skeleton, util::image_to_ink_bitmap};

#[test]
fn trace_image_to_strokes() {
    let Ok(input) = std::env::var("TRACE_IMAGE_INPUT") else {
        eprintln!("TRACE_IMAGE_INPUT not set, skipping");
        return;
    };
    let output = std::env::var("TRACE_IMAGE_OUTPUT").unwrap_or_else(|_| "/tmp/trace_image_out.png".to_string());

    let bytes = std::fs::read(&input).expect("read input image");
    let mut bitmap = image_to_ink_bitmap(&bytes, 1024).expect("threshold image");
    let (h, w) = (bitmap.len(), bitmap[0].len());

    skeleton::thin_zhang_suen(&mut bitmap);
    let raw_paths = skeleton::trace_skeleton(&bitmap);
    let mut paths: Vec<Vec<(f32, f32)>> = raw_paths.iter().map(|p| skeleton::smooth_path(p, 5)).collect();
    let before = paths.len();
    paths.retain(|p| {
        let len: f32 = p
            .windows(2)
            .map(|s| {
                let (dx, dy) = (s[1].0 - s[0].0, s[1].1 - s[0].1);
                (dx * dx + dy * dy).sqrt()
            })
            .sum();
        len >= 4.0
    });
    println!("{}x{} bitmap -> {} paths ({} specks dropped)", w, h, paths.len(), before - paths.len());
    assert!(!paths.is_empty(), "tracing produced no strokes");

    // Render strokes as 1px black lines on white
    let mut img = image::GrayImage::from_pixel(w as u32, h as u32, image::Luma([255u8]));
    for p in &paths {
        for seg in p.windows(2) {
            let (x0, y0, x1, y1) = (seg[0].0, seg[0].1, seg[1].0, seg[1].1);
            let steps = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                if x >= 0.0 && y >= 0.0 && (x as u32) < w as u32 && (y as u32) < h as u32 {
                    img.put_pixel(x as u32, y as u32, image::Luma([0u8]));
                }
            }
        }
    }
    img.save(&output).expect("save output image");
    println!("wrote {}", output);
}
