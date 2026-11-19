// SPDX-License-Identifier: Apache-2.0
//! THE CRISP OS (TASK-090A) on the real screen: at every scale the OS
//! offers, stage and the desktop take the art made for it (each says so),
//! a window's title strip is that art's own pixels, and nothing is drawn
//! larger than it is or from another scale's art (the crisp guard's error
//! on COM1 would fail the spec).

use crate::Ctx;
use crate::art;
use crate::stage::{expect_pixels, ok, until, win};
use std::time::{Duration, Instant};

const SCALES: [u32; 9] = [100, 125, 150, 175, 200, 225, 250, 275, 300];

/// Waits up to 10 s for a COM1 line containing `text`.
fn expect_log(ctx: &Ctx, text: &str) -> Result<(), String> {
    let start = Instant::now();
    while ctx.log_line_with(text).is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("no \"{text}\" on COM1"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub fn scales(ctx: &mut Ctx) -> Result<(), String> {
    let lines = ctx.door("stage open 300 160 203040 Crisp")?;
    let w: u32 = lines
        .iter()
        .find_map(|l| l.strip_prefix("window ").and_then(|v| v.parse().ok()))
        .ok_or(format!("stage open said {lines:?}"))?;
    for pct in SCALES.iter().copied().skip(1).chain([100]) {
        ok(ctx, &format!("stage scale {pct}"))?;
        let strip = (32 * pct).div_ceil(100);
        let f = win(
            &until(ctx, &format!("the strip at {pct} %"), |ws| win(ws, w).is_ok_and(|x| x.h == 160 + strip as i32))?,
            w,
        )?;
        expect_log(ctx, &format!("stage: art at {pct} %"))?;
        expect_log(ctx, &format!("desktop: art at {pct} %"))?;
        // The strip's corner and its middle: the art made for this scale.
        let (cx, cy) = (f.x + f.w - 6, f.y + strip as i32 / 2);
        let (mx, my) = (f.x + f.w / 2, f.y + strip as i32 / 2);
        expect_pixels(
            ctx,
            &[
                (cx, cy, art::strip("dark", true, (f.x, f.y, f.w), strip, cx, cy), "the strip's corner"),
                (mx, my, art::strip("dark", true, (f.x, f.y, f.w), strip, mx, my), "the strip's middle"),
            ],
        )?;
    }
    ok(ctx, &format!("stage close {w}"))
}
