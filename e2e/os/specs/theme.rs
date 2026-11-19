// SPDX-License-Identifier: Apache-2.0
//! Every surface in both themes (TASK-064, FR-029): the ground, the Rack,
//! the launcher, Monitor and the terminal, each window's title strip
//! following the theme the moment it changes (the fleet's rule: a test pins
//! the bar's colour in both themes). Pictures of each are kept for a look.

use crate::Ctx;
use crate::art;
use crate::stage::{key, move_to};
use crate::sweep::{attr, q, until_attr, until_gone, until_q};
use fjson::Value;
use fui::tokens_gen::{dark, light};
use std::time::{Duration, Instant};

const MON: &str = "WINDOW[data-owner=\"monitor\"]";
const TERM: &str = "WINDOW[data-owner=\"terminal\"]";
const LAUNCHER: &str = "WINDOW[data-owner=\"desktop\"][data-kind=\"Popup\"]";
const RACK: &str = "WINDOW[data-owner=\"desktop\"][data-kind=\"Panel\"]";

/// A node's box on the screen: x, y, w, h.
fn rect(ctx: &mut Ctx, sel: &str) -> Result<(i32, i32, i32, i32), String> {
    let id = until_q(ctx, sel)?;
    let m = ctx.cdp.call("DOM.getBoxModel", Value::obj([("nodeId", id.into())]))?;
    let model = m.get("model").ok_or("no box model")?;
    let n = |k: &str| model.get(k).and_then(Value::as_i64).unwrap_or(0) as i32;
    let x0 = model.get("content").and_then(Value::as_array).and_then(|q| q.first()?.as_i64()).unwrap_or(0) as i32;
    let y0 = model.get("content").and_then(Value::as_array).and_then(|q| q.get(1)?.as_i64()).unwrap_or(0) as i32;
    Ok((x0, y0, n("width"), n("height")))
}

/// Waits up to 10 s for every (x, y) to show its colour; says which didn't.
fn until_pixels(ctx: &mut Ctx, what: &str, want: &[(i32, i32, u32)]) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let shot = ctx.screenshot()?;
        let px = |x: i32, y: i32| {
            let i = (y as usize * shot.w as usize + x as usize) * 3;
            (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32
        };
        let wrong: Vec<String> = want
            .iter()
            .filter(|(x, y, c)| px(*x, *y) != *c)
            .map(|(x, y, c)| format!("({x}, {y}) {:06x} not {c:06x}", px(*x, *y)))
            .collect();
        if wrong.is_empty() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{what}: {}", wrong.join(", ")));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

struct Look {
    name: &'static str,
    bg: u32,
    surface: u32,
    border: u32,
}

const DARK: Look = Look {
    name: "dark",
    bg: dark::COLOR_BG.xrgb(),
    surface: dark::COLOR_SURFACE.xrgb(),
    border: dark::COLOR_BORDER.xrgb(),
};
const LIGHT: Look = Look {
    name: "light",
    bg: light::COLOR_BG.xrgb(),
    surface: light::COLOR_SURFACE.xrgb(),
    border: light::COLOR_BORDER.xrgb(),
};

/// One theme, every surface: the ground, the Rack, each app window brought
/// forward in turn (its strip lit, its surface in the theme), the other's
/// strip dimmed, and the launcher.
fn surfaces(ctx: &mut Ctx, l: &Look) -> Result<(), String> {
    // The pointer out of the way of every sample.
    move_to(ctx, 1275, 5)?;
    let rack = rect(ctx, RACK)?;
    let plate = |y: i32| art::rack(l.name, (rack.2 as u32, rack.3 as u32), rack.2 / 2, y);
    until_pixels(
        ctx,
        &format!("{}: the ground and the Rack", l.name),
        &[(5, 5, art::ground(l.name, 5, 5)), (rack.2 / 2, rack.1, plate(0)), (rack.2 / 2, rack.1 + 6, plate(6))],
    )?;
    for (sel, inside) in [(MON, l.surface), (TERM, l.bg)] {
        let id = until_q(ctx, sel)?;
        let wid = attr(ctx, id, "data-window-id")?.ok_or("no window id")?;
        ctx.cdp.call("Target.activateTarget", Value::obj([("targetId", wid.as_str().into())]))?;
        until_attr(ctx, sel, "data-focused", "true")?;
        let (x, y, w, h) = rect(ctx, sel)?;
        let other = if sel == MON { TERM } else { MON };
        let (ox, oy, ow, _) = rect(ctx, other)?;
        let lit = (x + w * 3 / 4, y + 6);
        let mut want =
            vec![(lit.0, lit.1, art::strip(l.name, true, (x, y, w), 32, lit.0, lit.1)), (x + w - 5, y + h - 5, inside)];
        // The other window's strip, dimmed, where this one doesn't cover it.
        let (sx, sy) = (ox + ow * 3 / 4, oy + 6);
        if !(sx >= x && sx < x + w && sy >= y && sy < y + h) {
            let oy0 = oy;
            want.push((sx, sy, art::strip(l.name, false, (ox, oy0, ow), 32, sx, sy)));
        }
        until_pixels(ctx, &format!("{}: {sel}", l.name), &want)?;
    }
    ctx.keep_shot(&format!("theme-{}", l.name))?;
    // The launcher.
    key(ctx, "MetaLeft", true)?;
    key(ctx, "MetaLeft", false)?;
    let (x, y, w, h) = rect(ctx, LAUNCHER)?;
    until_pixels(ctx, &format!("{}: the launcher", l.name), &[(x, y, l.border), (x + w - 4, y + h - 4, l.surface)])?;
    ctx.keep_shot(&format!("theme-{}-launcher", l.name))?;
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    until_gone(ctx, LAUNCHER)
}

/// `@theme.both`: the ground, the Rack, the launcher, Monitor and the
/// terminal in light, then in dark: each drawn in its theme's tokens, every
/// title strip with them (lit when focused, dimmed when not), the moment the
/// theme changes; pictures kept of both (TASK-064, FR-029).
pub fn both(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up")?;
    ctx.door("monitor open")?;
    ctx.door("terminal open")?;
    until_q(ctx, MON)?;
    until_q(ctx, TERM)?;
    crate::stage::ok(ctx, "stage theme light")?;
    let r = surfaces(ctx, &LIGHT);
    crate::stage::ok(ctx, "stage theme dark")?;
    r?;
    surfaces(ctx, &DARK)?;
    // Both closed, for the specs after.
    for sel in [MON, TERM] {
        let id = q(ctx, sel)?;
        if id != 0 {
            let wid = attr(ctx, id, "data-window-id")?.ok_or("no window id")?;
            ctx.cdp.call("Target.activateTarget", Value::obj([("targetId", wid.as_str().into())]))?;
            until_attr(ctx, sel, "data-focused", "true")?;
            key(ctx, "AltLeft", true)?;
            key(ctx, "F4", true)?;
            key(ctx, "F4", false)?;
            key(ctx, "AltLeft", false)?;
            until_gone(ctx, sel)?;
        }
    }
    Ok(())
}
