// SPDX-License-Identifier: Apache-2.0
//! The inventory (TASK-063, FR-040): every control a window showed during
//! the run, and every one a spec worked (a click landed on it, or keys went
//! to it), each named `owner ROLE id` (the test id, or the name without
//! one). Each setup writes its own; after a full run `xtask cdp` fails the
//! leg on any control shown and never worked, unless `e2e/os/accepted.json`
//! names it with a reason.

use fjson::Value;
use std::collections::BTreeSet;

#[derive(Debug, Default)]
pub struct Inventory {
    pub shown: BTreeSet<String>,
    pub worked: BTreeSet<String>,
}

/// A control's name, from cdpd's description of it.
pub fn name(v: &Value) -> Option<String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
    let id = if s("testid").is_empty() { s("label") } else { s("testid") };
    (!s("owner").is_empty() && !id.is_empty()).then(|| format!("{} {} {}", s("owner"), s("role"), id))
}

impl Inventory {
    pub fn to_json(&self) -> Value {
        let list = |set: &BTreeSet<String>| Value::Arr(set.iter().map(|s| Value::from(s.as_str())).collect());
        Value::obj([("shown", list(&self.shown)), ("worked", list(&self.worked))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_is_named_by_owner_role_and_test_id_or_name() {
        let v = |owner: &str, testid: &str, label: &str| {
            Value::obj([
                ("owner", owner.into()),
                ("role", "BUTTON".into()),
                ("testid", testid.into()),
                ("label", label.into()),
            ])
        };
        assert_eq!(name(&v("desktop", "rack-monitor", "Monitor")).as_deref(), Some("desktop BUTTON rack-monitor"));
        assert_eq!(name(&v("monitor", "", "Sort by CPU")).as_deref(), Some("monitor BUTTON Sort by CPU"));
        assert_eq!(name(&v("", "x", "y")), None, "no window, no name");
        assert_eq!(name(&Value::Null), None);
        let mut inv = Inventory::default();
        inv.shown.insert("a".into());
        inv.worked.insert("a".into());
        assert_eq!(inv.to_json().to_json(), r#"{"shown":["a"],"worked":["a"]}"#);
    }
}
