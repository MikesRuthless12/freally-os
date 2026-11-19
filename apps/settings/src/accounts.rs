// SPDX-License-Identifier: Apache-2.0
//! Settings ▸ Accounts (TASK-104; TASK-103's export and import): the
//! accounts on this machine, and a dialog for each thing done to one:
//! change your password, export your account, and (the Owner) add one,
//! import one, remove one. These act at once, outside OK and Apply.
//!
//! Each dialog is fields, then its key and *Cancel*: Tab and Shift+Tab move
//! between them, typing goes to the focused field, Enter presses the key
//! (or Cancel, focused), Escape closes it. What can be checked here is (the
//! two new passwords alike, eight characters, the username typed again);
//! the rest is accounts' to say, and its words are shown under the fields.

use crate::key;
use alloc::string::String;
use alloc::vec::Vec;
use app_session::import::{Import, preview_words};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Password,
    Export,
    Add,
    Import,
    /// The account to remove.
    Remove(String),
}

impl Kind {
    pub fn title(&self) -> String {
        match self {
            Kind::Password => String::from("Change your password"),
            Kind::Export => String::from("Export your account"),
            Kind::Add => String::from("Add an account"),
            Kind::Import => String::from("Import an account"),
            Kind::Remove(u) => alloc::format!("Remove {u}"),
        }
    }

    /// The words under the title.
    pub fn words(&self) -> &'static str {
        match self {
            Kind::Password => "Your files stay as they are; only the key that opens them changes.",
            Kind::Export => "Your account and your files, sealed with your password, in one file you can carry.",
            Kind::Add => "A Standard account. Your password says it's you adding it.",
            Kind::Import => "An exported account, from a removable disk or System ▸ Imports.",
            Kind::Remove(_) => "Its files go with it, and nothing can open them again.",
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Kind::Password => "settings-password-dialog",
            Kind::Export => "settings-export-dialog",
            Kind::Add => "settings-add-dialog",
            Kind::Import => "settings-import-dialog",
            Kind::Remove(_) => "settings-remove-dialog",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub secret: bool,
    pub id: &'static str,
}

fn field(label: &str, secret: bool, id: &'static str) -> Field {
    Field { label: String::from(label), value: String::new(), secret, id }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    /// Press the dialog's key (its fields checked here first).
    Submit,
    Close,
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub kind: Kind,
    pub fields: Vec<Field>,
    /// A field, then the key (`fields.len()`), then Cancel.
    pub focus: usize,
    /// The field the error is under (`None`: under the keys).
    pub error: Option<(Option<usize>, String)>,
    /// What was done, once it's done (the dialog then offers only *Close*).
    pub done: Option<String>,
    /// Import: the files to choose from, and the preview once opened.
    pub import: Import,
}

impl Dialog {
    pub fn new(kind: Kind, files: Vec<String>) -> Dialog {
        let fields = match &kind {
            Kind::Password => alloc::vec![
                field("Current password", true, "settings-password-current"),
                field("New password", true, "settings-password-new"),
                field("New password, again", true, "settings-password-again"),
            ],
            Kind::Export => alloc::vec![field("Your password", true, "settings-export-password")],
            Kind::Add => alloc::vec![
                field("Username", false, "settings-add-username"),
                field("Display name", false, "settings-add-name"),
                field("Password", true, "settings-add-password"),
                field("Password, again", true, "settings-add-again"),
                field("Your password", true, "settings-add-owner"),
            ],
            Kind::Import => alloc::vec![field("The file's password", true, "settings-import-password")],
            Kind::Remove(u) => alloc::vec![
                field(&alloc::format!("Type {u} to remove it"), false, "settings-remove-confirm"),
                field("Your password", true, "settings-remove-owner"),
            ],
        };
        Dialog { kind, fields, focus: 0, error: None, done: None, import: Import::new(files) }
    }

    /// The key's words.
    pub fn primary(&self) -> &'static str {
        match &self.kind {
            _ if self.done.is_some() => "Close",
            Kind::Password => "Change password",
            Kind::Export => "Export",
            Kind::Add => "Add",
            Kind::Import if self.import.preview.is_none() => "Open",
            Kind::Import => "Import",
            Kind::Remove(_) => "Remove",
        }
    }

    /// How many focus stops: the fields, the key, Cancel (once done: the key).
    fn stops(&self) -> usize {
        if self.done.is_some() { 1 } else { self.fields.len() + 2 }
    }

    pub fn key(&mut self, usage: u16, ch: u32, shift: bool) -> Outcome {
        if self.done.is_some() {
            return match usage {
                key::ENTER | key::ESCAPE | key::SPACE => Outcome::Close,
                _ => Outcome::Nothing,
            };
        }
        let n = self.stops();
        match usage {
            key::ESCAPE => Outcome::Close,
            key::TAB => {
                self.focus = if shift { (self.focus + n - 1) % n } else { (self.focus + 1) % n };
                Outcome::Changed
            }
            key::ENTER if self.focus == self.fields.len() + 1 => Outcome::Close,
            key::ENTER => Outcome::Submit,
            key::SPACE if self.focus == self.fields.len() => Outcome::Submit,
            key::SPACE if self.focus == self.fields.len() + 1 => Outcome::Close,
            // Import: ↑ ↓ choose the file.
            key::UP if self.kind == Kind::Import => self.choose(self.import.chosen.wrapping_sub(1)),
            key::DOWN if self.kind == Kind::Import => self.choose(self.import.chosen + 1),
            key::BACKSPACE => {
                let Some(f) = self.fields.get_mut(self.focus) else { return Outcome::Nothing };
                if f.value.pop().is_none() {
                    return Outcome::Nothing;
                }
                self.edited();
                Outcome::Changed
            }
            _ => {
                let Some(c) = char::from_u32(ch).filter(|c| !c.is_control()) else { return Outcome::Nothing };
                let Some(f) = self.fields.get_mut(self.focus) else { return Outcome::Nothing };
                if f.value.chars().count() >= 128 {
                    return Outcome::Nothing;
                }
                f.value.push(c);
                self.edited();
                Outcome::Changed
            }
        }
    }

    /// Something typed: the error goes; an import's file must be opened again
    /// if its password changed.
    fn edited(&mut self) {
        self.error = None;
        if self.kind == Kind::Import && self.focus == 0 && self.import.preview.take().is_some() {
            self.fields.truncate(1);
        }
    }

    /// Import: another file. The password and the preview start again.
    pub fn choose(&mut self, i: usize) -> Outcome {
        if i >= self.import.files.len() || i == self.import.chosen {
            return Outcome::Nothing;
        }
        self.import.chosen = i;
        self.import.preview = None;
        self.fields.truncate(1);
        self.forget();
        self.focus = 0;
        self.error = None;
        Outcome::Changed
    }

    pub fn value(&self, i: usize) -> &str {
        self.fields.get(i).map_or("", |f| f.value.as_str())
    }

    /// The checks made here before accounts is asked.
    pub fn check(&mut self) -> bool {
        let short = |s: &str| s.chars().count() < 8;
        let problem = match &self.kind {
            Kind::Password if short(self.value(1)) => Some((1, "A password needs at least 8 characters.")),
            Kind::Password if self.value(1) != self.value(2) => Some((2, "The two new passwords aren't the same.")),
            Kind::Add if self.value(0).is_empty() => Some((0, "Give it a username.")),
            Kind::Add if short(self.value(2)) => Some((2, "A password needs at least 8 characters.")),
            Kind::Add if self.value(2) != self.value(3) => Some((3, "The two passwords aren't the same.")),
            Kind::Import if self.import.file().is_none() => Some((0, "There's no account file to import.")),
            Kind::Remove(u) if self.value(0) != u => Some((0, "Type the username exactly as it's shown.")),
            _ => None,
        };
        match problem {
            Some((at, words)) => {
                self.error = Some((Some(at), String::from(words)));
                self.focus = at;
                false
            }
            None if self.fields.iter().any(|f| f.secret && f.value.is_empty()) => {
                let at = self.fields.iter().position(|f| f.secret && f.value.is_empty()).unwrap_or(0);
                self.error = Some((Some(at), String::from("Type the password.")));
                self.focus = at;
                false
            }
            None => true,
        }
    }

    /// Import: the file opened with its password: its preview, and the two
    /// fields the import needs.
    pub fn opened(&mut self, username: &str, files: u64, bytes: u64, exported_unix: u64) {
        self.import.preview = Some(preview_words(username, files, bytes, exported_unix));
        self.fields.truncate(1);
        self.fields.push(field("Username (blank: keep the file's)", false, "settings-import-rename"));
        self.fields.push(field("Your password", true, "settings-import-owner"));
        self.focus = 1;
    }

    /// accounts refused: its words, under the field they're about.
    pub fn refused(&mut self, words: &str) {
        // Changing a password, the one that didn't match is the current one;
        // elsewhere, the last password asked for (yours, or the file's).
        let at = if words.contains("password") && self.kind == Kind::Password {
            Some(0)
        } else if words.contains("password") {
            self.fields.iter().rposition(|f| f.secret)
        } else if words.contains("username") || words.contains("account called") {
            self.fields.iter().position(|f| !f.secret)
        } else {
            None
        };
        if let Some(i) = at {
            self.focus = i;
            if self.fields[i].secret {
                self.fields[i].value.clear();
            }
        }
        self.error = Some((at, String::from(words)));
    }

    /// Done: the passwords forgotten, and the words shown.
    pub fn finished(&mut self, words: String) {
        self.forget();
        self.done = Some(words);
        self.error = None;
        self.focus = 0;
    }

    /// Every password typed, wiped where it lies.
    pub fn forget(&mut self) {
        for f in self.fields.iter_mut().filter(|f| f.secret) {
            fstd::secret::wipe_str(&mut f.value);
            f.value.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(d: &mut Dialog, s: &str) {
        for c in s.chars() {
            d.key(0, c as u32, false);
        }
    }

    #[test]
    fn the_new_passwords_must_match_before_accounts_is_asked() {
        let mut d = Dialog::new(Kind::Password, Vec::new());
        typed(&mut d, "old-password");
        d.key(key::TAB, 0, false);
        typed(&mut d, "new-password");
        d.key(key::TAB, 0, false);
        typed(&mut d, "new-passwerd");
        assert_eq!(d.key(key::ENTER, 0, false), Outcome::Submit);
        assert!(!d.check());
        assert_eq!(d.error, Some((Some(2), String::from("The two new passwords aren't the same."))));
        d.key(key::BACKSPACE, 0, false);
        assert_eq!(d.error, None, "typing clears it");
        d.key(key::BACKSPACE, 0, false);
        d.key(key::BACKSPACE, 0, false);
        typed(&mut d, "ord");
        assert!(d.check());
        d.finished(String::from("Your password is changed."));
        assert!(d.fields.iter().all(|f| f.value.is_empty()), "forgotten");
        assert_eq!(d.primary(), "Close");
        assert_eq!(d.key(key::ENTER, 0, false), Outcome::Close);
    }

    #[test]
    fn tab_walks_the_fields_then_the_key_then_cancel() {
        let mut d = Dialog::new(Kind::Export, Vec::new());
        assert_eq!(d.focus, 0);
        d.key(key::TAB, 0, false);
        assert_eq!(d.focus, 1, "the key");
        d.key(key::TAB, 0, false);
        assert_eq!(d.key(key::ENTER, 0, false), Outcome::Close, "Cancel");
        d.key(key::TAB, 0, true);
        assert_eq!(d.key(key::SPACE, 0, false), Outcome::Submit);
        assert_eq!(d.key(key::ESCAPE, 0, false), Outcome::Close);
    }

    #[test]
    fn remove_asks_for_the_username_exactly() {
        let mut d = Dialog::new(Kind::Remove(String::from("sam")), Vec::new());
        typed(&mut d, "Sam");
        d.key(key::TAB, 0, false);
        typed(&mut d, "owner-pass");
        assert!(!d.check());
        assert_eq!(d.focus, 0);
        d.key(key::BACKSPACE, 0, false);
        d.key(key::BACKSPACE, 0, false);
        d.key(key::BACKSPACE, 0, false);
        typed(&mut d, "sam");
        assert!(d.check());
    }

    #[test]
    fn an_import_opens_first_then_asks_for_the_rest() {
        let mut d =
            Dialog::new(Kind::Import, alloc::vec![String::from("system:a.facct"), String::from("system:b.facct")]);
        assert_eq!(d.primary(), "Open");
        typed(&mut d, "bundle-pass");
        assert!(d.check());
        d.opened("sam", 3, 1_200_000, 1_795_046_400);
        assert_eq!(d.primary(), "Import");
        assert_eq!(d.fields.len(), 3);
        assert_eq!(d.focus, 1);
        assert!(d.import.preview.as_deref().is_some_and(|p| p.contains("'sam'")));
        // Another file: back to its password alone.
        d.key(key::DOWN, 0, false);
        assert_eq!(d.import.chosen, 1);
        assert_eq!((d.fields.len(), d.value(0), d.import.preview.is_none()), (1, "", true));
    }

    #[test]
    fn accounts_words_land_under_their_field() {
        let mut d = Dialog::new(Kind::Add, Vec::new());
        d.refused("There's already an account called sam.");
        assert_eq!(d.error.as_ref().and_then(|e| e.0), Some(0));
        d.fields[4].value = String::from("wrong-one");
        d.refused("That password didn't match.");
        assert_eq!((d.focus, d.value(4)), (4, ""), "the password field, emptied");
        // Changing a password, the one that didn't match is the current one.
        let mut d = Dialog::new(Kind::Password, Vec::new());
        d.focus = 2;
        d.refused("That password didn't match.");
        assert_eq!(d.error.as_ref().and_then(|e| e.0), Some(0));
        assert_eq!(d.focus, 0);
    }
}
