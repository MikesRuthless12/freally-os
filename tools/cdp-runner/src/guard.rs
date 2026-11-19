// SPDX-License-Identifier: Apache-2.0
//! The guard (TASK-063, FR-040): one shared fixture around every spec.
//!
//! - COM1: any `ERROR` line the spec printed (and didn't expect) fails it,
//!   and any error a service logged before the logger was up (it reaches
//!   COM1 through `debug_log` as `debug_log [pid N] error: …`, dev images).
//! - Through cdpd (e2e images, once the test door is open): any record
//!   logged at error level during the spec, any kernel warning, any new
//!   fault report, any service restarted, unless the spec said it would.

use fjson::Value;

/// COM1's lines from this spec (`log` is what it printed since it began).
pub fn check(log: &str, allowed: &[String]) -> Result<(), String> {
    let unexpected: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("ERROR") || (l.starts_with("debug_log [pid ") && l.contains("] error: ")))
        .filter(|l| !allowed.iter().any(|a| l.contains(a.as_str())))
        .collect();
    if unexpected.is_empty() { Ok(()) } else { Err(format!("the guard: COM1 reported {}", unexpected.join(" | "))) }
}

/// What a spec began with: the log's next record, the last fault, and each
/// service's restarts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Watch {
    pub log: u64,
    pub fault: u64,
    pub restarts: Vec<(String, u64)>,
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Each process's restarts, from `Freally.getProcesses`.
pub fn restarts(processes: &Value) -> Vec<(String, u64)> {
    processes
        .get("processes")
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .map(|p| (s(p, "name").to_string(), p.get("restarts").and_then(Value::as_i64).unwrap_or(0).max(0) as u64))
        .collect()
}

/// The records `Log.read` gave (warning and up) that fail a spec: an error
/// from anyone, a warning from the kernel; not the ones `allowed` names.
pub fn bad_records(entries: &[Value], allowed: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| s(e, "level") == "error" || (s(e, "source") == "kernel" && s(e, "level") == "warning"))
        .filter(|e| !allowed.iter().any(|a| s(e, "text").contains(a.as_str())))
        .map(|e| format!("{}: {}", s(e, "source"), s(e, "text")))
        .collect()
}

/// The services restarted since `before`.
pub fn restarted(before: &[(String, u64)], now: &[(String, u64)]) -> Vec<String> {
    now.iter()
        .filter(|(name, n)| before.iter().find(|(b, _)| b == name).is_some_and(|(_, was)| n > was))
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(source: &str, level: &str, text: &str) -> Value {
        Value::obj([("source", source.into()), ("level", level.into()), ("text", text.into())])
    }

    #[test]
    fn unexpected_errors_fail_and_expected_ones_pass() {
        let log = "freally-boot 0.1.0\nfreally-boot: ERROR image refused: x\n";
        assert!(check(log, &[]).is_err());
        assert!(check(log, &["image refused".to_string()]).is_ok());
        assert!(check("all fine\n", &[]).is_ok());
    }

    #[test]
    fn errors_from_anyone_and_warnings_from_the_kernel_fail() {
        let entries = [
            rec("stage", "warning", "stage: no keyboard or mouse"),
            rec("monitor", "error", "monitor: lost conductor"),
            rec("kernel", "warning", "freally-core: WARNING a slow frame"),
            rec("kernel", "info", "freally-core: scheduler ready"),
        ];
        assert_eq!(
            bad_records(&entries, &[]),
            ["monitor: monitor: lost conductor", "kernel: freally-core: WARNING a slow frame"]
        );
        assert_eq!(bad_records(&entries, &["lost conductor".to_string(), "slow frame".to_string()]).len(), 0);
    }

    #[test]
    fn a_restart_is_a_count_that_grew() {
        let p = |v: &[(&str, u64)]| v.iter().map(|(n, c)| (n.to_string(), *c)).collect::<Vec<_>>();
        let before = p(&[("stage", 0), ("desktop", 1), ("logger", 0)]);
        let now = p(&[("stage", 0), ("desktop", 2), ("logger", 0), ("monitor", 3)]);
        assert_eq!(restarted(&before, &now), ["desktop"], "a service first seen now isn't a restart");
        let procs = Value::obj([(
            "processes",
            Value::Arr(vec![Value::obj([("name", "stage".into()), ("restarts", 2u32.into())])]),
        )]);
        assert_eq!(restarts(&procs), [("stage".to_string(), 2)]);
    }
}
