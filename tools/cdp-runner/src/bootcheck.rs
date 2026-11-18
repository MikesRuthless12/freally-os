// SPDX-License-Identifier: Apache-2.0
//! `cdp-runner --boot-check --endpoint HOST:PORT` (TASK-065): one boot's
//! numbers, read through cdpd once the desktop takes input (S4) and has had
//! 2 s to settle: each stage's time after B0, every process with its
//! description, and the memory in use. One JSON line on stdout; `cargo xtask
//! boot-check` boots five times and judges them.

use cdp_proto::Cdp;
use fjson::Value;
use std::time::{Duration, Instant};

fn metric(m: &Value, name: &str) -> Option<f64> {
    m.get("metrics")?
        .as_array()?
        .iter()
        .find(|x| x.get("name").and_then(Value::as_str) == Some(name))?
        .get("value")?
        .as_f64()
}

fn none() -> Value {
    Value::Obj(Vec::new())
}

/// One boot: S4 on COM1 first (as the specs do: the door is asked nothing
/// while the image is still coming up), then the door.
pub fn run(cdp: &mut Cdp, com1: &std::path::Path) -> Result<Value, String> {
    let start = Instant::now();
    while !std::fs::read_to_string(com1).unwrap_or_default().contains(" S4=") {
        if start.elapsed() > Duration::from_secs(60) {
            return Err(String::from("no S4 on COM1 within 60 s"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    loop {
        let last = match cdp.call("Performance.getMetrics", none()) {
            Ok(m) if metric(&m, "BootMs").is_some_and(|v| v > 0.0) => break,
            Ok(m) => m.to_json(),
            Err(e) => e,
        };
        if start.elapsed() > Duration::from_secs(60) {
            return Err(format!("the desktop never took input (no S4 within 60 s); last: {last}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Idle: what's left running once the boot's work is done.
    std::thread::sleep(Duration::from_secs(2));
    let t = cdp.call("Freally.getBootTimeline", none())?;
    let p = cdp.call("Freally.getProcesses", none())?;
    let m = cdp.call("Performance.getMetrics", none())?;
    Ok(Value::obj([
        ("stages", t.get("stages").cloned().unwrap_or(Value::Arr(Vec::new()))),
        ("processes", p.get("processes").cloned().unwrap_or(Value::Arr(Vec::new()))),
        ("memUsed", Value::Num(metric(&m, "MemUsed").unwrap_or(0.0))),
    ]))
}
