// SPDX-License-Identifier: Apache-2.0
//! The login screen's model (TASK-102, PRD § 8 *Login*): the accounts by
//! display name, one chosen; the password well; *Shut down* and *Restart*.
//! ↑ ↓ choose an account, typing goes to the password, Tab moves between
//! the well and the two keys, Enter acts on whichever has the focus.

use crate::key;
use alloc::string::String;
use alloc::vec::Vec;

pub const ERR_WRONG: &str = "That password didn't match.";
pub const ERR_WAIT: &str = "Wait a moment before trying again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Password,
    ShutDown,
    Restart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    /// Log the chosen account in with the password typed.
    Submit,
    ShutDown,
    Restart,
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub username: String,
    pub display_name: String,
}

impl User {
    /// What the list shows: the display name, else the username.
    pub fn shown(&self) -> &str {
        if self.display_name.is_empty() { &self.username } else { &self.display_name }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub users: Vec<User>,
    pub chosen: usize,
    pub password: String,
    pub focus: Focus,
    pub error: Option<&'static str>,
}

impl Login {
    pub fn new(users: Vec<User>) -> Login {
        Login { users, chosen: 0, password: String::new(), focus: Focus::Password, error: None }
    }

    pub fn key(&mut self, usage: u16, ch: u32, shift: bool) -> Outcome {
        match usage {
            key::UP => self.choose(self.chosen.wrapping_sub(1)),
            key::DOWN => self.choose(self.chosen + 1),
            key::TAB => {
                const ORDER: [Focus; 3] = [Focus::Password, Focus::ShutDown, Focus::Restart];
                let at = ORDER.iter().position(|f| *f == self.focus).unwrap_or(0);
                self.focus = ORDER[(at + if shift { 2 } else { 1 }) % 3];
                Outcome::Changed
            }
            key::ENTER => match self.focus {
                Focus::Password if self.password.is_empty() => Outcome::Nothing,
                Focus::Password => Outcome::Submit,
                Focus::ShutDown => Outcome::ShutDown,
                Focus::Restart => Outcome::Restart,
            },
            key::BACKSPACE if self.focus == Focus::Password => {
                if self.password.pop().is_none() {
                    return Outcome::Nothing;
                }
                self.error = None;
                Outcome::Changed
            }
            _ => match char::from_u32(ch).filter(|c| !c.is_control()) {
                Some(c) if self.password.chars().count() < 128 => {
                    self.focus = Focus::Password;
                    self.password.push(c);
                    self.error = None;
                    Outcome::Changed
                }
                _ => Outcome::Nothing,
            },
        }
    }

    /// An account chosen (by ↑ ↓ or a click): the password starts again.
    pub fn choose(&mut self, i: usize) -> Outcome {
        if i >= self.users.len() || i == self.chosen {
            return Outcome::Nothing;
        }
        self.chosen = i;
        self.forget_password();
        self.error = None;
        self.focus = Focus::Password;
        Outcome::Changed
    }

    /// The account chosen.
    pub fn user(&self) -> Option<&User> {
        self.users.get(self.chosen)
    }

    /// The password, overwritten (sent, or given up).
    pub fn forget_password(&mut self) {
        fstd::secret::wipe_str(&mut self.password);
        self.password.clear();
    }

    /// accounts said no: what to say, and the password goes.
    pub fn refused(&mut self, wait: bool) {
        self.error = Some(if wait { ERR_WAIT } else { ERR_WRONG });
        self.forget_password();
        self.focus = Focus::Password;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> Login {
        Login::new(alloc::vec![
            User { username: "mike".into(), display_name: "Mike Weaver".into() },
            User { username: "dana".into(), display_name: String::new() },
        ])
    }

    #[test]
    fn typing_goes_to_the_password_and_enter_logs_in() {
        let mut l = login();
        assert_eq!(l.key(key::ENTER, 0, false), Outcome::Nothing, "no password yet");
        for c in "secret!!".chars() {
            l.key(0, c as u32, false);
        }
        assert_eq!(l.password, "secret!!");
        l.key(key::BACKSPACE, 0, false);
        assert_eq!(l.password, "secret!");
        assert_eq!(l.key(key::ENTER, 0, false), Outcome::Submit);
        l.refused(false);
        assert_eq!((l.error, l.password.as_str()), (Some(ERR_WRONG), ""));
        l.key(0, 'x' as u32, false);
        assert_eq!(l.error, None, "typing again clears the error");
        l.refused(true);
        assert_eq!(l.error, Some(ERR_WAIT));
    }

    #[test]
    fn arrows_choose_and_tab_reaches_the_power_keys() {
        let mut l = login();
        assert_eq!(l.user().map(User::shown), Some("Mike Weaver"));
        l.key(0, 'p' as u32, false);
        assert_eq!(l.key(key::DOWN, 0, false), Outcome::Changed);
        assert_eq!((l.chosen, l.password.as_str()), (1, ""), "another account: the password starts again");
        assert_eq!(l.user().map(User::shown), Some("dana"), "no display name: the username");
        assert_eq!(l.key(key::DOWN, 0, false), Outcome::Nothing, "the last stays chosen");
        assert_eq!(l.key(key::UP, 0, false), Outcome::Changed);
        assert_eq!(l.key(key::UP, 0, false), Outcome::Nothing);
        l.key(key::TAB, 0, false);
        assert_eq!(l.focus, Focus::ShutDown);
        assert_eq!(l.key(key::ENTER, 0, false), Outcome::ShutDown);
        l.key(key::TAB, 0, false);
        assert_eq!(l.key(key::ENTER, 0, false), Outcome::Restart);
        l.key(key::TAB, 0, true);
        assert_eq!(l.focus, Focus::ShutDown);
        l.key(key::TAB, 0, false);
        l.key(key::TAB, 0, false);
        assert_eq!(l.focus, Focus::Password);
        l.key(key::TAB, 0, false);
        l.key(0, 'a' as u32, false);
        assert_eq!((l.focus, l.password.as_str()), (Focus::Password, "a"), "typing takes the focus back");
    }
}
