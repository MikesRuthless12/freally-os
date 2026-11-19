// SPDX-License-Identifier: Apache-2.0
//! The OS's renders as the specs expect them on the screen (TASK-052): drawn
//! from the same `assets/os` with the same fimage routines stage and the
//! desktop draw them with, so a spec can ask for any pixel exactly.
//!
//! Each piece at the scale it's shown at (TASK-090A: the image carries every
//! scale, stage and the desktop draw the one made for theirs):
//!
//! - the ground: the theme's wallpaper, whole, brought down to the screen;
//! - a window's title strip: its render 9-sliced to the frame's width and
//!   the strip's height, over the theme's strip colour;
//! - the Rack: the faceplate's render 9-sliced to the screen's width and the
//!   Rack's height, over the theme's surface.

use fimage::blit::Surface;
use fjson::Value;
use fui::tokens_gen::{dark, light};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// The VM's screen (`cargo xtask run`'s display).
pub const SCREEN: (u32, u32) = (1280, 800);

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/os")
}

/// A piece's height at 100 % (`os.json`'s `h`).
fn height_1x(piece: &str) -> u32 {
    let meta = std::fs::read_to_string(dir().join("os.json")).expect("assets/os/os.json");
    let meta = fjson::parse(&meta).expect("os.json");
    let p = meta.get("pieces").and_then(|ps| ps.get(piece)).unwrap_or_else(|| panic!("os.json has no {piece}"));
    p.get("h").and_then(Value::as_i64).unwrap_or_else(|| panic!("{piece}: no h")) as u32
}

/// The UI scale a 9-sliced piece shown `h` tall is drawn at: the nearest
/// scale the OS offers.
fn pct_for(piece: &str, h: u32) -> u32 {
    ((h * 100 / height_1x(piece) + 12) / 25 * 25).clamp(100, 300)
}

/// A theme's piece at `pct` percent (0: the render's own size, as the
/// wallpaper is carried), as the image carries it (FIMG bytes), each made once.
fn fimg(theme: &str, piece: &str, pct: u32) -> Vec<u8> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    let key = format!("{theme}/{piece}@{pct}");
    let cache = CACHE.get_or_init(Default::default);
    if let Some(b) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return b.clone();
    }
    let meta = std::fs::read_to_string(dir().join("os.json")).expect("assets/os/os.json");
    let meta = fjson::parse(&meta).expect("os.json");
    let p = meta.get("pieces").and_then(|ps| ps.get(piece)).unwrap_or_else(|| panic!("os.json has no {piece}"));
    let scale = p.get("scale").and_then(Value::as_i64).unwrap_or(4) as u32;
    let s: Vec<u32> = p
        .get("slice")
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_i64)
        .map(|v| v as u32)
        .collect();
    let slice = [0, 1, 2, 3].map(|i| s.get(i).copied().unwrap_or(0));
    let png = std::fs::read(dir().join(theme).join(format!("{piece}.png"))).unwrap_or_else(|e| panic!("{key}: {e}"));
    let at = if pct == 0 { scale * 100 } else { pct };
    let bytes = fimage::fimg::from_png(&png, at, scale, slice).unwrap_or_else(|e| panic!("{key}: {e}"));
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key, bytes.clone());
    bytes
}

fn tokens(theme: &str) -> (u32, u32) {
    if theme == "light" {
        (light::COLOR_SURFACE.xrgb(), light::COLOR_SURFACE_2.xrgb())
    } else {
        (dark::COLOR_SURFACE.xrgb(), dark::COLOR_SURFACE_2.xrgb())
    }
}

/// `piece` 9-sliced into `w × h` over `under`, as XRGB, at the scale it's
/// shown at.
fn nine(theme: &str, piece: &str, w: u32, h: u32, under: u32) -> Vec<u32> {
    let bytes = fimg(theme, piece, pct_for(piece, h));
    let src = fimage::fimg::parse(&bytes).expect("a FIMG");
    let mut px = vec![under; (w * h) as usize];
    Surface { px: &mut px, width: w, height: h, stride: w }.nine(&src, 0, 0, w, h);
    px.into_iter().map(|p| p & 0xFF_FFFF).collect()
}

/// The ground's pixel at (x, y) on the VM's screen.
pub fn ground(theme: &str, x: i32, y: i32) -> u32 {
    static WALLS: OnceLock<Mutex<HashMap<String, Vec<u32>>>> = OnceLock::new();
    let walls = WALLS.get_or_init(Default::default);
    let mut walls = walls.lock().unwrap_or_else(|p| p.into_inner());
    let wall = walls.entry(theme.to_string()).or_insert_with(|| {
        let bytes = fimg(theme, "wallpaper", 0);
        let src = fimage::fimg::parse(&bytes).expect("a FIMG");
        fimage::blit::cover(&src, SCREEN.0, SCREEN.1).into_iter().map(|p| p & 0xFF_FFFF).collect()
    });
    wall[(y as u32 * SCREEN.0 + x as u32) as usize]
}

/// A title strip's pixel at screen (x, y): the window's frame at
/// (`fx`, `fy`), `fw` wide, its strip `sh` tall; lit when `focused`.
pub fn strip(theme: &str, focused: bool, (fx, fy, fw): (i32, i32, i32), sh: u32, x: i32, y: i32) -> u32 {
    let (surface, surface2) = tokens(theme);
    let (piece, under) = if focused { ("title_strip_focused", surface2) } else { ("title_strip", surface) };
    nine(theme, piece, fw as u32, sh, under)[((y - fy) as u32 * fw as u32 + (x - fx) as u32) as usize]
}

/// The Rack's pixel at (x, y) inside it (the Rack `w` wide, `h` tall).
pub fn rack(theme: &str, (w, h): (u32, u32), x: i32, y: i32) -> u32 {
    nine(theme, "rack_faceplate", w, h, tokens(theme).0)[(y as u32 * w + x as u32) as usize]
}
