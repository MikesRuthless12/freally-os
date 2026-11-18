// SPDX-License-Identifier: Apache-2.0
//! The launcher (TASK-059, PRD § 8 *Launcher*): `Super` opens a centred
//! well; typing filters the apps (those whose names start with the words
//! first, then those that contain them, each alphabetically); ↑ ↓ move;
//! Enter starts the one chosen; Esc closes. With nothing typed, every app,
//! alphabetically.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// What the launcher shows.
    pub name: String,
    pub description: String,
    /// The service conductor starts for it.
    pub service: String,
}

/// The apps matching `query`, as indices into `apps`.
pub fn filter(apps: &[App], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let mut order: Vec<usize> = (0..apps.len()).collect();
    order.sort_by_key(|&i| apps[i].name.to_lowercase());
    if q.is_empty() {
        return order;
    }
    let name = |i: usize| apps[i].name.to_lowercase();
    let mut out: Vec<usize> = order.iter().copied().filter(|&i| name(i).starts_with(&q)).collect();
    out.extend(order.iter().copied().filter(|&i| !name(i).starts_with(&q) && name(i).contains(&q)));
    out
}

/// What a key did to the launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The words or the choice changed: draw it again.
    Changed,
    /// Start this app (an index into the apps).
    Start(usize),
    /// Esc: close it.
    Close,
    Nothing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launcher {
    pub query: String,
    /// The chosen row among the matches.
    pub selected: usize,
}

/// Key usages the launcher reads.
pub mod key {
    pub const ENTER: u16 = 0x28;
    pub const ESCAPE: u16 = 0x29;
    pub const BACKSPACE: u16 = 0x2A;
    pub const DOWN: u16 = 0x51;
    pub const UP: u16 = 0x52;
}

impl Launcher {
    /// A key going down, with the character it types (0 for none).
    pub fn key(&mut self, apps: &[App], usage: u16, ch: u32) -> Outcome {
        let matches = filter(apps, &self.query);
        match usage {
            key::ESCAPE => return Outcome::Close,
            key::ENTER => {
                return matches.get(self.selected).map_or(Outcome::Nothing, |&i| Outcome::Start(i));
            }
            key::DOWN => {
                if self.selected + 1 < matches.len() {
                    self.selected += 1;
                    return Outcome::Changed;
                }
                return Outcome::Nothing;
            }
            key::UP => {
                if self.selected > 0 {
                    self.selected -= 1;
                    return Outcome::Changed;
                }
                return Outcome::Nothing;
            }
            key::BACKSPACE => {
                if self.query.pop().is_none() {
                    return Outcome::Nothing;
                }
            }
            _ => match char::from_u32(ch).filter(|c| !c.is_control()) {
                Some(c) if self.query.chars().count() < 64 => self.query.push(c),
                _ => return Outcome::Nothing,
            },
        }
        // The words changed: the first match is chosen.
        self.selected = 0;
        Outcome::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn apps() -> Vec<App> {
        ["Terminal", "Monitor", "Files", "Settings", "Font Viewer"]
            .iter()
            .map(|n| App { name: (*n).into(), description: String::new(), service: n.to_lowercase() })
            .collect()
    }

    fn names(apps: &[App], idx: &[usize]) -> Vec<String> {
        idx.iter().map(|&i| apps[i].name.clone()).collect()
    }

    #[test]
    fn everything_alphabetically_then_prefix_then_substring() {
        let a = apps();
        assert_eq!(names(&a, &filter(&a, "")), ["Files", "Font Viewer", "Monitor", "Settings", "Terminal"]);
        assert_eq!(names(&a, &filter(&a, "f")), ["Files", "Font Viewer"]);
        assert_eq!(
            names(&a, &filter(&a, "t")),
            ["Terminal", "Font Viewer", "Monitor", "Settings"],
            "prefix first, then contains, each alphabetically"
        );
        assert_eq!(names(&a, &filter(&a, "MON")), ["Monitor"], "any case");
        assert!(filter(&a, "xyz").is_empty());
    }

    #[test]
    fn typing_moving_choosing_and_closing() {
        let a = apps();
        let mut l = Launcher::default();
        assert_eq!(l.key(&a, 0x17, 't' as u32), Outcome::Changed);
        assert_eq!(l.key(&a, key::DOWN, 0), Outcome::Changed);
        assert_eq!(l.selected, 1);
        assert_eq!(l.key(&a, key::ENTER, 0), Outcome::Start(4), "Font Viewer, the second match");
        assert_eq!(l.key(&a, 0x08, 'e' as u32), Outcome::Changed);
        assert_eq!((l.query.as_str(), l.selected), ("te", 0), "new words choose the first match again");
        assert_eq!(l.key(&a, key::BACKSPACE, 0), Outcome::Changed);
        assert_eq!(l.key(&a, key::UP, 0), Outcome::Nothing, "already at the top");
        l.key(&a, key::BACKSPACE, 0);
        assert_eq!(l.key(&a, key::BACKSPACE, 0), Outcome::Nothing, "nothing left to take");
        assert_eq!(l.key(&a, key::ESCAPE, 0), Outcome::Close);
        let mut none = Launcher { query: "xyz".into(), selected: 0 };
        assert_eq!(none.key(&a, key::ENTER, 0), Outcome::Nothing, "no match, nothing starts");
        let _ = vec![0];
    }
}
