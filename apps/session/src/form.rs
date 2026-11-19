// SPDX-License-Identifier: Apache-2.0
//! The welcome screen's form (TASK-102, PRD § 8 *Welcome*): Name, Username
//! (filled in from the name until it's typed into), Password, Password
//! again. Enter moves to the next field and, from the last, makes the
//! account; each mistake is said under its own field.

use crate::key;
use alloc::string::String;

pub const NAME: usize = 0;
pub const USERNAME: usize = 1;
pub const PASSWORD: usize = 2;
pub const AGAIN: usize = 3;
pub const FIELDS: usize = 4;

pub const LABELS: [&str; FIELDS] = ["Name", "Username", "Password", "Password again"];
/// Each field's test id (the sweep finds them by it).
pub const IDS: [&str; FIELDS] = ["welcome-name", "welcome-username", "welcome-password", "welcome-again"];

pub const ERR_NAME: &str = "Type your name.";
pub const ERR_NAME_LONG: &str = "A name is at most 64 characters.";
pub const ERR_USERNAME: &str = "Usernames start with a letter and use a–z, 0–9, - or _.";
pub const ERR_SHORT: &str = "A password needs at least 8 characters.";
pub const ERR_MATCH: &str = "The passwords don't match.";

/// The longest each field takes, in characters.
const LONGEST: [usize; FIELDS] = [64, 32, 128, 128];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Something to draw again.
    Changed,
    /// Every field is right: make the account.
    Create,
    Nothing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pub fields: [String; FIELDS],
    pub focus: usize,
    pub errors: [Option<&'static str>; FIELDS],
    /// The username was typed into: the name no longer fills it in.
    pub username_typed: bool,
}

/// A username made from a name: lowercase, spaces as `-`, only a–z, 0–9,
/// `-` and `_`, starting with a letter, at most 32.
pub fn username_from(name: &str) -> String {
    let mut u = String::new();
    for c in name.trim().chars() {
        let c = c.to_ascii_lowercase();
        match c {
            'a'..='z' => u.push(c),
            '0'..='9' | '-' | '_' if !u.is_empty() => u.push(c),
            ' ' if !u.is_empty() && !u.ends_with('-') => u.push('-'),
            _ => {}
        }
    }
    while u.ends_with('-') {
        u.pop();
    }
    u.truncate(32);
    u
}

impl Form {
    pub fn new() -> Form {
        Form::default()
    }

    /// A key: `usage` as stage gives it, `ch` the character it typed (0 for
    /// none), with Shift held.
    pub fn key(&mut self, usage: u16, ch: u32, shift: bool) -> Outcome {
        match usage {
            key::ENTER => {
                if self.focus + 1 < FIELDS {
                    self.focus += 1;
                    Outcome::Changed
                } else {
                    self.submit()
                }
            }
            key::TAB if shift => self.move_focus(-1),
            key::TAB | key::DOWN => self.move_focus(1),
            key::UP => self.move_focus(-1),
            key::BACKSPACE => {
                if self.fields[self.focus].pop().is_none() {
                    return Outcome::Nothing;
                }
                self.edited();
                Outcome::Changed
            }
            _ => match char::from_u32(ch).filter(|c| !c.is_control()) {
                Some(c) if self.fields[self.focus].chars().count() < LONGEST[self.focus] => {
                    self.fields[self.focus].push(c);
                    self.edited();
                    Outcome::Changed
                }
                _ => Outcome::Nothing,
            },
        }
    }

    /// A field clicked.
    pub fn focus_on(&mut self, field: usize) -> Outcome {
        if field >= FIELDS || field == self.focus {
            return Outcome::Nothing;
        }
        self.focus = field;
        Outcome::Changed
    }

    fn move_focus(&mut self, by: isize) -> Outcome {
        self.focus = (self.focus as isize + by).rem_euclid(FIELDS as isize) as usize;
        Outcome::Changed
    }

    /// The focused field changed: its error goes, and the name fills the
    /// username until that's typed into.
    fn edited(&mut self) {
        self.errors[self.focus] = None;
        match self.focus {
            NAME if !self.username_typed => {
                self.fields[USERNAME] = username_from(&self.fields[NAME]);
                self.errors[USERNAME] = None;
            }
            USERNAME => self.username_typed = !self.fields[USERNAME].is_empty(),
            _ => {}
        }
    }

    /// *Create account*: every field checked; the first wrong one focused.
    pub fn submit(&mut self) -> Outcome {
        let name = self.fields[NAME].trim();
        self.errors = [
            if name.is_empty() {
                Some(ERR_NAME)
            } else if name.chars().count() > 64 {
                Some(ERR_NAME_LONG)
            } else {
                None
            },
            (!svc_accounts::valid_username(&self.fields[USERNAME])).then_some(ERR_USERNAME),
            svc_accounts::check_password(&self.fields[PASSWORD]).err().map(|_| ERR_SHORT),
            (self.fields[AGAIN] != self.fields[PASSWORD]).then_some(ERR_MATCH),
        ];
        match self.errors.iter().position(Option::is_some) {
            Some(i) => {
                self.focus = i;
                Outcome::Changed
            }
            None => Outcome::Create,
        }
    }

    /// The passwords, overwritten (after they've been sent, or given up).
    pub fn forget_passwords(&mut self) {
        for f in [PASSWORD, AGAIN] {
            fstd::secret::wipe_str(&mut self.fields[f]);
            self.fields[f].clear();
        }
    }
}

/// B0 → S4 in seconds, from conductor's boot timeline (stage 2 is B0, 14 S4).
pub fn started_in(stages: &[(u8, u64)]) -> Option<f64> {
    let at = |id: u8| stages.iter().find(|(s, _)| *s == id).map(|(_, ns)| *ns);
    Some(at(14)?.saturating_sub(at(2)?) as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(f: &mut Form, s: &str) {
        for c in s.chars() {
            f.key(0, c as u32, false);
        }
    }

    #[test]
    fn the_name_fills_the_username_until_it_is_typed_into() {
        assert_eq!(username_from("  Mike  Weaver "), "mike-weaver");
        assert_eq!(username_from("9 Lives Cat"), "lives-cat");
        assert_eq!(username_from("Zoë O'Neil"), "zo-oneil");
        assert_eq!(username_from(&"a".repeat(40)).len(), 32);
        let mut f = Form::new();
        typed(&mut f, "Dana K");
        assert_eq!(f.fields[USERNAME], "dana-k");
        f.key(key::TAB, 0, false);
        f.key(key::BACKSPACE, 0, false);
        typed(&mut f, "z");
        assert_eq!(f.fields[USERNAME], "dana-z");
        f.key(key::TAB, 0, true);
        typed(&mut f, "!");
        assert_eq!(f.fields[USERNAME], "dana-z", "typed into: the name no longer fills it");
    }

    #[test]
    fn enter_moves_on_and_from_the_last_field_checks_everything() {
        let mut f = Form::new();
        assert_eq!(f.key(key::ENTER, 0, false), Outcome::Changed);
        assert_eq!(f.focus, USERNAME);
        f.focus = AGAIN;
        assert_eq!(f.key(key::ENTER, 0, false), Outcome::Changed, "errors, not an account");
        assert_eq!(f.errors, [Some(ERR_NAME), Some(ERR_USERNAME), Some(ERR_SHORT), None]);
        assert_eq!(f.focus, NAME, "the first wrong field is focused");
        typed(&mut f, "Mike");
        assert_eq!(f.errors[NAME], None, "typing clears a field's error");
        f.focus = PASSWORD;
        typed(&mut f, "correct-horse");
        f.focus = AGAIN;
        typed(&mut f, "correct-hors");
        assert_eq!(f.key(key::ENTER, 0, false), Outcome::Changed);
        assert_eq!(f.errors, [None, None, None, Some(ERR_MATCH)]);
        typed(&mut f, "e");
        assert_eq!(f.key(key::ENTER, 0, false), Outcome::Create);
        f.forget_passwords();
        assert!(f.fields[PASSWORD].is_empty() && f.fields[AGAIN].is_empty());
    }

    #[test]
    fn focus_wraps_and_fields_stop_at_their_length() {
        let mut f = Form::new();
        f.key(key::UP, 0, false);
        assert_eq!(f.focus, AGAIN);
        f.key(key::DOWN, 0, false);
        assert_eq!(f.focus, NAME);
        typed(&mut f, &"x".repeat(70));
        assert_eq!(f.fields[NAME].chars().count(), 64);
        assert_eq!(f.key(0, 0x08, false), Outcome::Nothing, "a control character types nothing");
        assert_eq!(f.focus_on(PASSWORD), Outcome::Changed);
        assert_eq!(f.focus_on(PASSWORD), Outcome::Nothing);
    }

    #[test]
    fn the_boot_time_is_b0_to_s4() {
        assert_eq!(started_in(&[(2, 100_000_000), (14, 1_300_000_000)]), Some(1.2));
        assert_eq!(started_in(&[(2, 0)]), None);
    }
}
