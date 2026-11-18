// SPDX-License-Identifier: Apache-2.0
//! Composing (TASK-057, FR-028): the screen is built in a back buffer, only
//! where something changed (the damage), then those rectangles alone are
//! copied to the framebuffer (write-combining).
//!
//! In the damaged area: the ground, then every window bottom to top. A normal
//! window is its title strip and its surface (premultiplied, over the theme's
//! surface colour), inside rounded corners and a 1 px border, anti-aliased;
//! the strip carries its title (cut with "…" when long) and the close
//! button's "×". Focused, the strip is the raised surface with the title in
//! the text colour; unfocused, the plain surface with the title muted. All of
//! it in the theme's tokens: switching theme is composing again. The strip's
//! rendered art replaces the flat strip when TASK-052's render lands.

use crate::windows::{Rect, Window, WindowId, Windows};
use alloc::string::String;
use alloc::vec::Vec;
use ffont::atlas::Target;
use fui::text::Fonts;
use fui::theme::{TextStyle, Theme, Tok};
use fui::tokens_os;

/// The screen as stage builds it: opaque 0x00RRGGBB.
#[derive(Debug)]
pub struct Back {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

impl Back {
    pub fn new(w: u32, h: u32) -> Back {
        Back { w, h, px: alloc::vec![0; (w * h) as usize] }
    }

    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }
}

/// A window's surface: premultiplied BGRA, one u32 a pixel (0xAARRGGBB).
pub trait Pixels {
    fn size(&self) -> (u32, u32);
    /// Copies row `y` from column `x` into `out`.
    fn row(&self, y: u32, x: u32, out: &mut [u32]);
}

impl Pixels for (u32, u32, &[u32]) {
    fn size(&self) -> (u32, u32) {
        (self.0, self.1)
    }
    fn row(&self, y: u32, x: u32, out: &mut [u32]) {
        let at = (y * self.0 + x) as usize;
        out.copy_from_slice(&self.2[at..at + out.len()]);
    }
}

/// A surface in memory the client shares and may be writing as stage reads:
/// read with volatile loads (a torn frame at worst, never undefined).
#[derive(Debug)]
pub struct Shared {
    ptr: *const u32,
    w: u32,
    h: u32,
}

impl Shared {
    /// # Safety
    /// `ptr` must point at `w × h` readable u32s that stay mapped while this lives.
    pub unsafe fn new(ptr: *const u32, w: u32, h: u32) -> Shared {
        Shared { ptr, w, h }
    }
}

impl Pixels for Shared {
    fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
    fn row(&self, y: u32, x: u32, out: &mut [u32]) {
        let base = (y * self.w + x) as usize;
        for (i, o) in out.iter_mut().enumerate() {
            // SAFETY: inside the w × h pixels `new` was promised.
            *o = unsafe { core::ptr::read_volatile(self.ptr.add(base + i)) };
        }
    }
}

/// Premultiplied `s` (0xAARRGGBB) over opaque `d` (0x00RRGGBB).
#[inline]
pub fn over(d: u32, s: u32) -> u32 {
    let a = s >> 24;
    if a == 255 {
        return s & 0xFF_FFFF;
    }
    if a == 0 {
        return d;
    }
    let inv = 255 - a;
    let ch = |sh: u32| ((s >> sh & 0xFF) + ((d >> sh & 0xFF) * inv + 127) / 255).min(255) << sh;
    ch(16) | ch(8) | ch(0)
}

/// `c` laid over `d` with coverage `k` (0..=1).
#[inline]
fn mix(d: u32, c: u32, k: f32) -> u32 {
    if k >= 1.0 {
        return c;
    }
    if k <= 0.0 {
        return d;
    }
    let ch = |sh: u32| {
        let (a, b) = ((d >> sh & 0xFF) as f32, (c >> sh & 0xFF) as f32);
        ((a + (b - a) * k + 0.5) as u32).min(255) << sh
    };
    ch(16) | ch(8) | ch(0)
}

fn sqrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v / 2.0 } else { 1.0 };
    for _ in 0..12 {
        x = 0.5 * (x + v / x);
    }
    x
}

/// Signed distance from a pixel's centre (px, py), relative to a w × h box's
/// top-left, to the box with corners of radius r: negative inside.
fn rounded(px: u32, py: u32, w: u32, h: u32, r: f32) -> f32 {
    let (hx, hy) = (w as f32 / 2.0, h as f32 / 2.0);
    let qx = (px as f32 + 0.5 - hx).abs() - (hx - r);
    let qy = (py as f32 + 0.5 - hy).abs() - (hy - r);
    let outside = sqrt(qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0));
    outside + qx.max(qy).min(0.0) - r
}

/// How the windows look: the theme and the decoration's measures, in pixels.
pub struct Look<'a, 'f> {
    pub theme: Theme,
    pub fonts: &'a mut Fonts<'f>,
}

impl Look<'_, '_> {
    fn radius(&self) -> f32 {
        tokens_os::WINDOW_RADIUS as f32 * self.theme.scale
    }
    fn pad(&self) -> i32 {
        self.theme.px(tokens_os::WINDOW_PAD) as i32
    }
    fn rgb(&self, t: Tok) -> u32 {
        self.theme.colour(t).xrgb()
    }
}

/// The back buffer seen through a clip, for the text drawer.
struct Clip<'b> {
    back: &'b mut Back,
    clip: Rect,
}

impl Target for Clip<'_> {
    fn size(&self) -> (usize, usize) {
        (self.back.w as usize, self.back.h as usize)
    }
    fn get(&self, x: usize, y: usize) -> u32 {
        self.back.px[y * self.back.w as usize + x]
    }
    fn set(&mut self, x: usize, y: usize, rgb: u32) {
        if self.clip.contains(x as i32, y as i32) {
            self.back.px[y * self.back.w as usize + x] = rgb;
        }
    }
}

/// Builds `area` of the screen in `back`. `surface` gives each window's
/// pixels (none yet: the theme's surface colour shows).
pub fn compose<'s>(
    back: &mut Back,
    area: Rect,
    windows: &Windows,
    surface: &dyn Fn(WindowId) -> Option<&'s dyn Pixels>,
    look: &mut Look,
) {
    let area = area.intersect(&back.rect());
    if area.is_empty() {
        return;
    }
    let ground = look.rgb(Tok::Bg);
    for y in area.y..area.bottom() {
        let at = (y as u32 * back.w) as usize;
        back.px[at + area.x as usize..at + area.right() as usize].fill(ground);
    }
    let focused = windows.focused();
    let mut row = Vec::new();
    for w in windows.stack() {
        let f = w.frame.intersect(&area);
        if f.is_empty() {
            continue;
        }
        let px = surface(w.id);
        let s = windows.surface_rect(w);
        let deco = w.decorated();
        let is_focused = focused == Some(w.id);
        let strip_rgb = look.rgb(if is_focused { Tok::Surface2 } else { Tok::Surface });
        let base = look.rgb(Tok::Surface);
        let border = look.rgb(Tok::Border);
        let r = look.radius();
        for y in f.y..f.bottom() {
            let out = (y as u32 * back.w) as usize;
            // This row of the surface, if the row is the surface's.
            let in_surface = y >= s.y && y < s.bottom();
            let (sx0, sx1) = (f.x.max(s.x), f.right().min(s.right()));
            if in_surface && sx1 > sx0 {
                row.resize((sx1 - sx0) as usize, 0);
                match px {
                    Some(p) if p.size() == (w.w, w.h) => p.row((y - s.y) as u32, (sx0 - s.x) as u32, &mut row),
                    _ => row.fill(0),
                }
            }
            for x in f.x..f.right() {
                let content = if deco && y < s.y {
                    strip_rgb
                } else if in_surface && x >= sx0 && x < sx1 {
                    let p = row[(x - sx0) as usize];
                    if deco { over(base, p) } else { over(back.px[out + x as usize], p) }
                } else {
                    base
                };
                let d = &mut back.px[out + x as usize];
                if !deco {
                    *d = content;
                    continue;
                }
                let dist = rounded((x - w.frame.x) as u32, (y - w.frame.y) as u32, w.frame.w, w.frame.h, r);
                let fill = (0.5 - dist).clamp(0.0, 1.0);
                if fill <= 0.0 {
                    continue;
                }
                let inner = (0.5 - (dist + 1.0)).clamp(0.0, 1.0);
                // The 1 px border just inside the edge, then the window inside it.
                let c = mix(border, content, inner / fill);
                *d = mix(*d, c, fill);
            }
        }
        if deco {
            decorate(back, w, windows, area, is_focused, look);
        }
    }
}

/// The title and the close button's "×".
fn decorate(back: &mut Back, w: &Window, windows: &Windows, area: Rect, focused: bool, look: &mut Look) {
    let (Some(strip), Some(close)) = (windows.strip_rect(w), windows.close_rect(w)) else { return };
    let pad = look.pad();
    let (px, line, _) = look.theme.text(TextStyle::Body);
    let title_rgb = look.rgb(if focused { Tok::Text } else { Tok::Text3 });
    let close_rgb = look.rgb(if focused { Tok::Text2 } else { Tok::Text3 });
    let room = (close.x - strip.x - 2 * pad).max(0) as u32;
    let title = fit(look.fonts, look.theme, &w.title, room);
    let chain = look.fonts.chain(TextStyle::Body);
    let (up, down, _) = chain[0].metrics(px);
    let baseline = strip.y + (strip.h as i32 - line as i32) / 2 + (line as i32 - (up + down)) / 2 + up;
    let title_clip = Rect::new(strip.x + pad, strip.y, room, strip.h).intersect(&area);
    let mut t = Clip { back, clip: title_clip };
    ffont::text::draw(&mut t, chain, px, strip.x + pad, baseline, &title, title_rgb);
    let xw = ffont::text::measure(chain, px, "×").max(0);
    t.clip = close.intersect(&area);
    ffont::text::draw(&mut t, chain, px, close.x + (close.w as i32 - xw) / 2, baseline, "×", close_rgb);
}

/// `title`, cut with "…" to fit `room` pixels.
fn fit(fonts: &mut Fonts, theme: Theme, title: &str, room: u32) -> String {
    if fonts.measure(theme, TextStyle::Body, title).0 <= room {
        return String::from(title);
    }
    let mut chars: Vec<char> = title.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cut: String = chars.iter().collect::<String>() + "…";
        if fonts.measure(theme, TextStyle::Body, &cut).0 <= room {
            return cut;
        }
    }
    String::new()
}

/// Copies `area` of the back buffer to the framebuffer; `format` 0 is BGRX
/// (as the back buffer), 1 RGBX.
///
/// # Safety
/// `fb` must point at a mapped framebuffer of `back.h` rows of `stride` pixels.
pub unsafe fn present(back: &Back, area: Rect, fb: *mut u32, stride: u32, format: u32) {
    let area = area.intersect(&back.rect());
    for y in area.y..area.bottom() {
        let from = &back.px[(y as u32 * back.w + area.x as u32) as usize..][..area.w as usize];
        // SAFETY: the row lies inside the framebuffer (the caller's promise).
        let to = unsafe { fb.add((y as u32 * stride + area.x as u32) as usize) };
        if format == 0 {
            // SAFETY: as above; the back buffer and the framebuffer never overlap.
            unsafe { core::ptr::copy_nonoverlapping(from.as_ptr(), to, from.len()) };
        } else {
            for (i, &p) in from.iter().enumerate() {
                let swapped = (p & 0xFF00) | (p >> 16 & 0xFF) | (p & 0xFF) << 16;
                // SAFETY: as above.
                unsafe { core::ptr::write_volatile(to.add(i), swapped) };
            }
        }
    }
}

#[cfg(test)]
mod tests;
