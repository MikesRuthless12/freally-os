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
        crate::guard::check_scale(src);
        self.draw_part(src, 0, 0, src.width, src.height, x, y, src.width, src.height);
    }

    /// Draws `src` turned about its centre, which lands at (cx, cy): turned
    /// counter-clockwise on screen by the angle whose sine and cosine are
    /// given, sampled bilinearly (premultiplied, so its edges stay clean).
    pub fn turned(&mut self, src: &Fimg, (cx, cy): (f32, f32), sin: f32, cos: f32) {
        crate::guard::check_scale(src);
        let (hw, hh) = (src.width as f32 / 2.0, src.height as f32 / 2.0);
        let reach = hw + hh;
        let (x0, x1) = ((cx - reach) as i32 - 1, (cx + reach) as i32 + 1);
        let (y0, y1) = ((cy - reach) as i32 - 1, (cy + reach) as i32 + 1);
        for ty in y0.max(0)..y1.min(self.height as i32) {
            for tx in x0.max(0)..x1.min(self.width as i32) {
                let (dx, dy) = (tx as f32 + 0.5 - cx, ty as f32 + 0.5 - cy);
                // The point of the picture that lands here: the turn undone.
                let (sx, sy) = (dx * cos - dy * sin + hw - 0.5, dx * sin + dy * cos + hh - 0.5);
                if sx <= -1.0 || sy <= -1.0 || sx >= src.width as f32 || sy >= src.height as f32 {
                    continue;
                }
                let s = bilinear(src, sx, sy);
                if s[3] != 0 {
                    let i = ty as usize * self.stride as usize + tx as usize;
                    self.px[i] = over(self.px[i], s);
                }
            }
        }
    }

    /// Lays `src` at (x, y), 1:1 and clipped to `w × h`, over the surface
    /// with the "screen" blend at `opacity` (of 255): the meter's glass. Never
    /// stretched (TASK-090A): where the box is larger than the picture, the
    /// rest is left as it is (black, under a screen blend, changes nothing).
    pub fn screen(&mut self, src: &Fimg, x: i32, y: i32, w: u32, h: u32, opacity: u8) {
        crate::guard::check_scale(src);
        let (w, h) = (w.min(src.width), h.min(src.height));
        let op = opacity as u32;
        for oy in 0..h {
            let ty = y + oy as i32;
            if ty < 0 || ty >= self.height as i32 {
                continue;
            }
            for ox in 0..w {
                let tx = x + ox as i32;
                if tx < 0 || tx >= self.width as i32 {
                    continue;
                }
                let [b, g, r, _] = src.at(ox, oy);
                let i = ty as usize * self.stride as usize + tx as usize;
                let d = self.px[i];
                // screen(d, s) = d + s − d·s, then mixed in at the opacity.
                let mix = |s: u8, d: u32| d + (s as u32 * (255 - d) / 255) * op / 255;
                self.px[i] = mix(r, d >> 16 & 0xFF) << 16 | mix(g, d >> 8 & 0xFF) << 8 | mix(b, d & 0xFF);
            }
        }
    }

    /// Draws `src` 9-sliced into `w × h` at (x, y): the corners as they are,
    /// the edges stretched along their length, the middle both ways. A box
    /// smaller than the corners squeezes them evenly.
    pub fn nine(&mut self, src: &Fimg, x: i32, y: i32, w: u32, h: u32) {
        crate::guard::check_scale(src);
        // With no insets it's a plain stretch: the guard sees the whole.
        if src.slice == [0; 4] {
            crate::guard::check_size(src, src.width, src.height, w, h);
        }
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

/// `src` sampled between its pixels (premultiplied; transparent outside).
fn bilinear(src: &Fimg, x: f32, y: f32) -> [u8; 4] {
    // Floors without libm: both are above −1.
    let (ix, iy) = ((x + 1.0) as i32 - 1, (y + 1.0) as i32 - 1);
    let (ax, ay) = (x - ix as f32, y - iy as f32);
    let at = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= src.width as i32 || y >= src.height as i32 {
            [0u8; 4]
        } else {
            src.at(x as u32, y as u32)
        }
    };
    let (p00, p10, p01, p11) = (at(ix, iy), at(ix + 1, iy), at(ix, iy + 1), at(ix + 1, iy + 1));
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] as f32 * (1.0 - ax) + p10[c] as f32 * ax;
        let bottom = p01[c] as f32 * (1.0 - ax) + p11[c] as f32 * ax;
        out[c] = (top * (1.0 - ay) + bottom * ay + 0.5) as u8;
    }
    out
}

/// One axis of [`cover`]'s area filter at `num/den` (below 1): for each of
/// the `screen` pixels along it, the first source pixel under it and how
/// much of it and each next one it covers, in units where a source pixel is
/// `2·num` long (a screen pixel, `2·den`). The shown part is centred.
fn taps(screen: u64, size: u64, num: u64, den: u64) -> alloc::vec::Vec<(u32, alloc::vec::Vec<u64>)> {
    let (sp, tp) = (2 * num, 2 * den);
    let start = (size * num).saturating_sub(screen * den);
    (0..screen)
        .map(|i| {
            let (a, b) = (start + i * tp, start + (i + 1) * tp);
            let first = a / sp;
            let weights = (first..b.div_ceil(sp).min(size)).map(|j| b.min((j + 1) * sp) - a.max(j * sp)).collect();
            (first as u32, weights)
        })
        .collect()
}

/// [`cover`] for a screen smaller than the source: each screen pixel the
/// coverage-weighted mean of the source pixels under it.
fn cover_down(src: &Fimg, (w, h): (u64, u64), (iw, ih): (u64, u64), num: u64, den: u64) -> alloc::vec::Vec<u32> {
    let (xt, yt) = (taps(w, iw, num, den), taps(h, ih, num, den));
    let mut out = alloc::vec::Vec::with_capacity((w * h) as usize);
    for (y0, wy) in &yt {
        for (x0, wx) in &xt {
            let (mut acc, mut total) = ([0u64; 3], 0u64);
            for (dy, &a) in wy.iter().enumerate() {
                for (dx, &b) in wx.iter().enumerate() {
                    let p = src.at(x0 + dx as u32, y0 + dy as u32);
                    let k = a * b;
                    for c in 0..3 {
                        acc[c] += p[c] as u64 * k;
                    }
                    total += k;
                }
            }
            let ch = |c: usize| ((acc[c] + total / 2) / total.max(1)).min(255) as u32;
            out.push(0xFF00_0000 | ch(2) << 16 | ch(1) << 8 | ch(0));
        }
    }
    out
}

/// Two insets that fit in `size`, shrunk in proportion if they don't.
fn fit(a: u32, b: u32, size: u32) -> (u32, u32) {
    if a + b <= size {
        return (a, b);
    }
    let a2 = a * size / (a + b).max(1);
    (a2, size - a2)
}

/// `src` (opaque) scaled to cover a `w` × `h` screen, centred, what spills
/// over cropped: XRGB pixels, row by row. All integer, so every build of it
/// (stage's, and the specs' that check it) gives each pixel the same. A
/// screen smaller than the source gets it brought down with an area filter
/// (TASK-090A: each screen pixel the mean of the source under it, so the
/// render stays as crisp as it was drawn); the source's own size comes
/// through untouched; a larger screen is sampled bilinearly, and the crisp
/// guard counts it.
pub fn cover(src: &Fimg, w: u32, h: u32) -> alloc::vec::Vec<u32> {
    let (iw, ih) = (src.width.max(1) as u64, src.height.max(1) as u64);
    let (w64, h64) = (w as u64, h as u64);
    // The scale, screen over source, as num/den: the larger of the two.
    let (num, den) = if w64 * ih >= h64 * iw { (w64, iw) } else { (h64, ih) };
    if num < den {
        return cover_down(src, (w64, h64), (iw, ih), num, den);
    }
    if num > den {
        crate::guard::check_size(src, src.width, src.height, (iw * num / den) as u32, (ih * num / den) as u32);
    }
    // Each screen pixel's centre in the source, in 16.16, then where the
    // shown part of the source starts.
    let axis = |i: u64, screen: u64, size: u64| -> u64 {
        let shown = screen * den * 65536 / num;
        let start = (size * 65536).saturating_sub(shown) / 2;
        let at = start + (2 * i + 1) * den * 65536 / (2 * num);
        at.saturating_sub(32768).min((size - 1) * 65536)
    };
    let xs: alloc::vec::Vec<u64> = (0..w64).map(|x| axis(x, w64, iw)).collect();
    let mut out = alloc::vec::Vec::with_capacity((w * h) as usize);
    for y in 0..h64 {
        let sy = axis(y, h64, ih);
        let (y0, fy) = (sy >> 16, sy & 0xFFFF);
        let y1 = (y0 + 1).min(ih - 1);
        for &sx in &xs {
            let (x0, fx) = (sx >> 16, sx & 0xFFFF);
            let x1 = (x0 + 1).min(iw - 1);
            let p = |x: u64, y: u64| src.at(x as u32, y as u32);
            let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
            let mut rgb = 0u32;
            for (k, shift) in [(0, 0), (1, 8), (2, 16)] {
                let top = a[k] as u64 * (65536 - fx) + b[k] as u64 * fx;
                let bottom = c[k] as u64 * (65536 - fx) + d[k] as u64 * fx;
                let v = (top * (65536 - fy) + bottom * fy + (1 << 31)) >> 32;
                rgb |= (v.min(255) as u32) << shift;
            }
            out.push(0xFF00_0000 | rgb);
        }
    }
    out
}

#[cfg(test)]
mod tests;
