// SPDX-License-Identifier: Apache-2.0
//! The Rack (TASK-059, FR-030, PRD § 8 *Desktop*): a 56 px panel along the
//! screen's foot. On its left, the keys (the owner's choice: the launcher,
//! then Terminal and Monitor), 40 px square in a row with 8 px gaps, each
//! with its LED under it, lit while its app has a window. On its right, the
//! clock in its display well, 24 px from the edge. Every length is at 100 %
//! and scales with the UI.

use alloc::vec::Vec;

/// One key's place, at 100 %: the key itself, and its LED's centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPlace {
    pub x: i32,
    pub y: i32,
    pub size: u32,
    pub led: (i32, i32),
}

pub const HEIGHT: u32 = 56;
pub const KEY: u32 = 40;
pub const GAP: u32 = 8;
/// The keys start this far in, and sit this far down.
pub const INSET: u32 = 8;
pub const TOP: u32 = 3;
/// The clock's well: its padding from the Rack's right edge.
pub const CLOCK_PAD: u32 = 24;

/// The owner's Rack (TASK-049): the launcher, Terminal, Monitor.
pub const APPS: [&str; 2] = ["terminal", "monitor"];

/// Where each of `n` keys goes, at `scale`, left to right; the LED sits
/// centred under its key, in the 13 px below it.
pub fn keys(n: usize, scale: f32) -> Vec<KeyPlace> {
    let px = |v: u32| (v as f32 * scale + 0.5) as i32;
    (0..n)
        .map(|i| {
            let x = px(INSET + i as u32 * (KEY + GAP));
            let y = px(TOP);
            let size = px(KEY) as u32;
            KeyPlace { x, y, size, led: (x + size as i32 / 2, y + size as i32 + px(HEIGHT - TOP - KEY) / 2) }
        })
        .collect()
}

/// The Rack's key under (x, y) (the Rack's own coordinates), if any.
pub fn key_at(places: &[KeyPlace], x: i32, y: i32) -> Option<usize> {
    places.iter().position(|k| x >= k.x && y >= k.y && x < k.x + k.size as i32 && y < k.y + k.size as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_in_a_40_px_row_with_8_px_gaps() {
        let k = keys(3, 1.0);
        assert_eq!((k[0].x, k[0].y, k[0].size), (8, 3, 40));
        assert_eq!(k[1].x, 56);
        assert_eq!(k[2].x, 104);
        // The LED is centred under its key, in the 13 px below it.
        assert_eq!(k[0].led, (28, 3 + 40 + 6));
        assert!(k[0].led.1 < HEIGHT as i32);
    }

    #[test]
    fn they_scale() {
        let k = keys(2, 1.5);
        assert_eq!((k[1].x, k[1].size), (84, 60));
        assert!(k[1].led.1 < (HEIGHT as f32 * 1.5) as i32);
    }

    #[test]
    fn a_press_finds_its_key() {
        let k = keys(3, 1.0);
        assert_eq!(key_at(&k, 60, 20), Some(1));
        assert_eq!(key_at(&k, 50, 20), None, "a gap");
        assert_eq!(key_at(&k, 20, 50), None, "the LED row");
    }
}
