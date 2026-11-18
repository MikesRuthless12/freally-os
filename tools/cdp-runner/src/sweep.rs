// SPDX-License-Identifier: Apache-2.0
//! The DOM helpers specs share, and the sweep (TASK-063, FR-040): every
//! control under a node worked one by one, by its box on the screen with
//! the real mouse, each change read back by the spec's own check.

use crate::Ctx;
use fjson::Value;
use std::time::{Duration, Instant};

fn call(ctx: &mut Ctx, method: &str, params: Value) -> Result<Value, String> {
    ctx.cdp.call(method, params).map_err(|e| format!("{method}: {e}"))
}

/// The node `sel` finds now, from the root (0: none).
pub fn q(ctx: &mut Ctx, sel: &str) -> Result<i64, String> {
    let r = call(ctx, "DOM.querySelector", Value::obj([("nodeId", 1u32.into()), ("selector", sel.into())]))?;
    Ok(r.get("nodeId").and_then(Value::as_i64).unwrap_or(0))
}

pub fn q_all(ctx: &mut Ctx, sel: &str) -> Result<Vec<i64>, String> {
    let r = call(ctx, "DOM.querySelectorAll", Value::obj([("nodeId", 1u32.into()), ("selector", sel.into())]))?;
    Ok(r.get("nodeIds").and_then(Value::as_array).unwrap_or_default().iter().filter_map(Value::as_i64).collect())
}

/// Waits up to 10 s for `sel` to find a node.
pub fn until_q(ctx: &mut Ctx, sel: &str) -> Result<i64, String> {
    let start = Instant::now();
    loop {
        let id = q(ctx, sel)?;
        if id != 0 {
            return Ok(id);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("nothing matched {sel}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Waits up to 10 s for `sel` to find nothing.
pub fn until_gone(ctx: &mut Ctx, sel: &str) -> Result<(), String> {
    let start = Instant::now();
    while q(ctx, sel)? != 0 {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{sel} is still there"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

/// Waits up to 10 s for the node `sel` finds to have `k` = `v`.
pub fn until_attr(ctx: &mut Ctx, sel: &str, k: &str, v: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let id = q(ctx, sel)?;
        if id != 0 && attr(ctx, id, k)?.as_deref() == Some(v) {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            let now = if id == 0 { None } else { attr(ctx, id, k)? };
            return Err(format!("{sel} {k}: {now:?}, not {v:?}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub fn attr(ctx: &mut Ctx, id: i64, k: &str) -> Result<Option<String>, String> {
    let r = call(ctx, "DOM.getAttributes", Value::obj([("nodeId", id.into())]))?;
    let a = r.get("attributes").and_then(Value::as_array).unwrap_or_default();
    Ok(a.chunks(2).find(|p| p[0].as_str() == Some(k)).and_then(|p| p.get(1)?.as_str().map(String::from)))
}

/// A click in the middle of node `id`, with the real mouse.
pub fn click(ctx: &mut Ctx, id: i64) -> Result<(), String> {
    let m = call(ctx, "DOM.getBoxModel", Value::obj([("nodeId", id.into())]))?;
    let quad: Vec<i64> = m
        .get("model")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    let [x0, y0, x1, _, _, y1, ..] = quad[..] else { return Err(format!("no box for node {id}")) };
    crate::stage::move_to(ctx, ((x0 + x1) / 2) as i32, ((y0 + y1) / 2) as i32)?;
    crate::stage::button(ctx, true)?;
    crate::stage::button(ctx, false)
}

/// Letters typed on the real keyboard.
pub fn type_keys(ctx: &mut Ctx, text: &str) -> Result<(), String> {
    for c in text.chars() {
        let code = format!("Key{}", c.to_ascii_uppercase());
        crate::stage::key(ctx, &code, true)?;
        crate::stage::key(ctx, &code, false)?;
    }
    Ok(())
}

/// One control the sweep works: its selector (by test id), and its name.
#[derive(Clone, Debug)]
pub struct Control {
    pub sel: String,
    pub testid: String,
    pub label: String,
}

/// Every control `sel` finds (each must have a test id: the sweep finds it
/// again by it, since working one may redraw the rest).
pub fn controls(ctx: &mut Ctx, scope: &str, sel: &str) -> Result<Vec<Control>, String> {
    let mut out = Vec::new();
    for id in q_all(ctx, &format!("{scope} {sel}"))? {
        let testid = attr(ctx, id, "data-testid")?.ok_or(format!("a control under {scope} has no test id"))?;
        let label = attr(ctx, id, "aria-label")?.unwrap_or_default();
        out.push(Control { sel: format!("{scope} [data-testid=\"{testid}\"]"), testid, label });
    }
    Ok(out)
}

/// The sweep: every control `sel` finds under `scope`, in document order,
/// clicked by its box; `check` then reads the change back (from the tree,
/// the app's door or the real service). Returns how many it worked.
pub fn sweep(
    ctx: &mut Ctx,
    scope: &str,
    sel: &str,
    mut check: impl FnMut(&mut Ctx, &Control) -> Result<(), String>,
) -> Result<usize, String> {
    let list = controls(ctx, scope, sel)?;
    if list.is_empty() {
        return Err(format!("nothing to sweep: {scope} {sel}"));
    }
    for c in &list {
        let id = until_q(ctx, &c.sel)?;
        click(ctx, id)?;
        check(ctx, c).map_err(|e| format!("{} ({}): {e}", c.label, c.testid))?;
    }
    Ok(list.len())
}
