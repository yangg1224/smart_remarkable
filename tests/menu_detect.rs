//! Offline check of the selection-menu delete-button detector against a real
//! device screenshot. Runs only when MENU_IMAGE is set:
//!   MENU_IMAGE=shot.png MENU_SEL="x,y,w,h" cargo test --test menu_detect -- --nocapture

use smart_remarkable::{screenshot::Screenshot, touch::Rect};

#[test]
fn detect_delete_button() {
    let Ok(path) = std::env::var("MENU_IMAGE") else {
        eprintln!("MENU_IMAGE not set, skipping");
        return;
    };
    let sel: Vec<i32> = std::env::var("MENU_SEL")
        .expect("MENU_SEL=x,y,w,h required")
        .split(',')
        .map(|s| s.trim().parse().unwrap())
        .collect();
    let sel = Rect {
        x: sel[0],
        y: sel[1],
        w: sel[2],
        h: sel[3],
    };

    let img = image::open(&path).expect("open image").to_luma8();
    let result = Screenshot::detect_selection_menu_delete_in(&img, sel);
    println!("detected delete button: {:?}", result);
    assert!(result.is_some(), "delete button not found");
}
