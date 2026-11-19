// SPDX-License-Identifier: Apache-2.0
//! Settings ▸ Update (TASK-110, FR-050): what this computer runs, an update
//! waiting for a restart, *Switch back*, and *Install from a file*: the
//! packages found (`/system/updates`, Home ▸ Downloads), one chosen and
//! inspected (its version, who signed it, its notes), installed with a
//! meter, then *Restart now*. The updater does the work; this decides what's
//! said and which keys there are.

use alloc::string::String;
use alloc::vec::Vec;
use fwire::proto::updater as up;

/// Where packages are looked for: the system's own folder, and the
/// account's Downloads (`{home}` is the account's home).
pub const FOLDERS: [&str; 2] = ["/system/updates", "{home}/Downloads"];

/// A slot's state, as conductor's A/B record numbers them.
pub const EMPTY: u8 = 0;
pub const GOOD: u8 = 1;
pub const TRIAL: u8 = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    /// The updater's answer; `Err`: why there's none (no disk in this build).
    pub state: Option<Result<up::State, String>>,
    /// The packages found (paths).
    pub files: Vec<String>,
    pub chosen: Option<usize>,
    /// The chosen package, inspected, or why it was refused.
    pub info: Option<Result<up::Info, String>>,
    /// Bytes sent of how many, while installing.
    pub installing: Option<(u64, u64)>,
    /// What the last key did.
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Restart,
    SwitchBack,
    File(usize),
    Install,
}

fn slot_name(s: u8) -> &'static str {
    if s == 0 { "A" } else { "B" }
}

/// A file's name, from its path.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl View {
    /// The panel's lines, in order.
    pub fn lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        match &self.state {
            None => {}
            Some(Err(why)) => v.push(why.clone()),
            Some(Ok(s)) => {
                v.push(alloc::format!("This computer runs Freally OS {}, from slot {}.", s.running, slot_name(s.slot)));
                match (s.other_state, s.other_next) {
                    (TRIAL, true) => {
                        v.push(alloc::format!("{} is installed: it starts when you restart.", s.other_version))
                    }
                    (GOOD, true) => v.push(alloc::format!("Freally OS {} starts when you restart.", s.other_version)),
                    (GOOD, false) if !s.other_version.is_empty() => v.push(alloc::format!(
                        "Freally OS {} is on this computer too: you can switch back to it.",
                        s.other_version
                    )),
                    _ => {}
                }
            }
        }
        if let Some((sent, total)) = self.installing {
            v.push(alloc::format!("Installing: {} %", sent * 100 / total.max(1)));
        }
        v
    }

    /// The keys, in order: (label, test id, what it does).
    pub fn keys(&self) -> Vec<(String, String, Act)> {
        let mut v = Vec::new();
        let Some(Ok(s)) = &self.state else { return v };
        if self.installing.is_some() {
            return v;
        }
        if s.other_next && matches!(s.other_state, TRIAL | GOOD) {
            v.push((String::from("Restart now"), String::from("settings-update-restart"), Act::Restart));
        } else if s.other_state == GOOD && !s.other_version.is_empty() {
            v.push((
                alloc::format!("Switch back to {}", s.other_version),
                String::from("settings-update-switch-back"),
                Act::SwitchBack,
            ));
        }
        for (i, f) in self.files.iter().enumerate() {
            v.push((String::from(file_name(f)), alloc::format!("settings-update-file-{}", file_name(f)), Act::File(i)));
        }
        if let Some(Ok(info)) = &self.info {
            v.push((alloc::format!("Install {}", info.version), String::from("settings-update-install"), Act::Install));
        }
        v
    }

    /// What the chosen package is, or why it can't go on.
    pub fn chosen_words(&self) -> Option<String> {
        match self.info.as_ref()? {
            Ok(i) => Some(alloc::format!(
                "Freally OS {} · signed with the {} key · {:.1} MB",
                i.version,
                i.key,
                i.size as f64 / 1e6
            )),
            Err(why) => Some(why.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(other_state: u8, other_version: &str, other_next: bool) -> up::State {
        up::State {
            running: String::from("0.4.0"),
            slot: 0,
            other_state,
            other_version: other_version.into(),
            other_next,
        }
    }

    fn info() -> up::Info {
        up::Info {
            version: String::from("0.4.1"),
            oldest: String::new(),
            notes: String::from("# 0.4.1"),
            size: 202_000_000,
            key: String::from("dev"),
        }
    }

    fn ids(v: &View) -> Vec<String> {
        v.keys().into_iter().map(|k| k.1).collect()
    }

    #[test]
    fn a_fresh_computer_offers_its_packages_then_install() {
        let mut v = View {
            state: Some(Ok(state(EMPTY, "", false))),
            files: alloc::vec![String::from("/system/updates/e2e-0.4.1.fup")],
            ..Default::default()
        };
        assert_eq!(v.lines(), ["This computer runs Freally OS 0.4.0, from slot A."]);
        assert_eq!(ids(&v), ["settings-update-file-e2e-0.4.1.fup"]);
        v.chosen = Some(0);
        v.info = Some(Ok(info()));
        assert_eq!(ids(&v), ["settings-update-file-e2e-0.4.1.fup", "settings-update-install"]);
        assert_eq!(v.chosen_words().unwrap(), "Freally OS 0.4.1 · signed with the dev key · 202.0 MB");
        v.installing = Some((50, 200));
        assert!(v.keys().is_empty(), "no keys while it installs");
        assert_eq!(v.lines()[1], "Installing: 25 %");
    }

    #[test]
    fn an_update_waiting_offers_restart_and_a_good_other_slot_switch_back() {
        let v = View { state: Some(Ok(state(TRIAL, "0.4.1", true))), ..Default::default() };
        assert_eq!(v.lines()[1], "0.4.1 is installed: it starts when you restart.");
        assert_eq!(ids(&v), ["settings-update-restart"]);
        let v = View { state: Some(Ok(state(GOOD, "0.4.0", false))), ..Default::default() };
        assert_eq!(ids(&v), ["settings-update-switch-back"]);
        assert_eq!(v.keys()[0].0, "Switch back to 0.4.0");
        let v = View { state: Some(Ok(state(GOOD, "0.4.0", true))), ..Default::default() };
        assert_eq!(ids(&v), ["settings-update-restart"], "switched back: restart to finish");
    }

    #[test]
    fn no_updater_or_a_refused_package_says_why() {
        let v = View { state: Some(Err(String::from("Updates need a disk."))), ..Default::default() };
        assert_eq!(v.lines(), ["Updates need a disk."]);
        assert!(v.keys().is_empty());
        let v = View {
            state: Some(Ok(state(EMPTY, "", false))),
            files: alloc::vec![String::from("/x/bad.fup")],
            chosen: Some(0),
            info: Some(Err(String::from("This update was changed after it was signed."))),
            ..Default::default()
        };
        assert_eq!(ids(&v), ["settings-update-file-bad.fup"], "no Install for a refused package");
        assert_eq!(v.chosen_words().unwrap(), "This update was changed after it was signed.");
    }
}
