// SPDX-License-Identifier: Apache-2.0
//! Reduce Motion's saver: the F still and dimmed in the middle of black.
//! Drawn once; nothing moves.

use super::{Frame, Saver, Sprite};

pub struct Still {
    f: Sprite,
}

/// The F's light, of 256.
pub const DIM: u32 = 110;

impl Still {
    pub fn new(h: u32, f: &Sprite) -> Still {
        let fh = (h / 3).max(8);
        Still { f: f.scaled((f.w * fh / f.h.max(1)).max(8), fh) }
    }
}

impl Saver for Still {
    fn frame(&mut self, f: &mut Frame, _dt_ms: u32) {
        f.px.fill(0);
        let (x, y) = ((f.w as i32 - self.f.w as i32) / 2, (f.h as i32 - self.f.h as i32) / 2);
        self.f.draw(f, x, y, DIM);
    }
}
