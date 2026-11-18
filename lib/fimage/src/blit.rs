// SPDX-License-Identifier: Apache-2.0
//! Drawing FIMGs on an opaque XRGB surface (a framebuffer, a window's
//! buffer): premultiplied "over", clipped to the surface; and 9-slice, which
//! stretches an image to any size and keeps its corners as drawn.

use crate::fimg::Fimg;

/// An opaque surface: `stride` pixels a row, 0x00RRGGBB each.
#[derive(Debug)]
pub struct Surface<'a> {
    pub px: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

/// `src` (premultiplied BGRA) over `dst` (opaque XRGB).
#[inline]
pub fn over(dst: u32, [b, g, r, a]: [u8; 4]) -> u32 {
    let inv = 255 - a as u32;
    let mix = |s: u8, d: u32| (s as u32 + (d * inv + 127) / 255).min(255);
    mix(r, dst >> 16 & 0xFF) << 16 | mix(g, dst >> 8 & 0xFF) << 8 | mix(b, dst & 0xFF)
}

impl Surface<'_> {
    /// Draws the part of `src` from (sx, sy), `w × h` of it, at (x, y),
    /// stretched to `dw × dh` by nearest sampling (unstretched when equal).
    #[allow(clippy::too_many_arguments)]
    fn draw_part(&mut self, src: &Fimg, sx: u32, sy: u32, w: u32, h: u32, x: i32, y: i32, dw: u32, dh: u32) {
        if w == 0 || h == 0 || dw == 0 || dh == 0 {
            return;
        }
        for oy in 0..dh {
            let ty = y + oy as i32;
            if ty < 0 || ty >= self.height as i32 {
                continue;
            }
            let py = sy + oy * h / dh;
            for ox in 0..dw {
                let tx = x + ox as i32;
                if tx < 0 || tx >= self.width as i32 {
                    continue;
                }
                let s = src.at(sx + ox * w / dw, py);
                let i = ty as usize * self.stride as usize + tx as usize;
                self.px[i] = over(self.px[i], s);
            }
        }
    }

    /// Draws `src` with its top-left corner at (x, y).
    pub fn draw(&mut self, src: &Fimg, x: i32, y: i32) {
        self.draw_part(src, 0, 0, src.width, src.height, x, y, src.width, src.height);
    }

    /// Draws `src` 9-sliced into `w × h` at (x, y): the corners as they are,
    /// the edges stretched along their length, the middle both ways. A box
    /// smaller than the corners squeezes them evenly.
    pub fn nine(&mut self, src: &Fimg, x: i32, y: i32, w: u32, h: u32) {
        let [l, t, r, b] = src.slice.map(u32::from);
        let (l, r) = fit(l, r, w);
        let (t, b) = fit(t, b, h);
        let (sl, st, sr, sb) = (src.slice[0] as u32, src.slice[1] as u32, src.slice[2] as u32, src.slice[3] as u32);
        let (smw, smh) = (src.width - sl - sr, src.height - st - sb);
        let (mw, mh) = (w - l - r, h - t - b);
        let cols = [(0, sl, 0, l), (sl, smw, l, mw), (src.width - sr, sr, l + mw, r)];
        let rows = [(0, st, 0, t), (st, smh, t, mh), (src.height - sb, sb, t + mh, b)];
        for (sy, shh, oy, dh) in rows {
            for (sx, sw, ox, dw) in cols {
                self.draw_part(src, sx, sy, sw, shh, x + ox as i32, y + oy as i32, dw, dh);
            }
        }
    }
}

/// Two insets that fit in `size`, shrunk in proportion if they don't.
fn fit(a: u32, b: u32, size: u32) -> (u32, u32) {
    if a + b <= size {
        return (a, b);
    }
    let a2 = a * size / (a + b).max(1);
    (a2, size - a2)
}

#[cfg(test)]
mod tests;
