// SPDX-License-Identifier: Apache-2.0
//! Welcome's *Import an account…* (TASK-103, PRD § 8 *Account export /
//! import*): the account files there are to choose from, the password,
//! then a preview (*"Account 'mike', 3 files, 1.2 MB, exported 19 Nov
//! 2026"*), then *Import*. ↑ ↓ choose a file; typing goes to the password;
//! Enter shows the preview, and then imports; Esc goes back.

use crate::key;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    /// Check the file with the password, for its preview.
    Preview,
    /// Import it.
    Import,
    /// Back to the welcome form.
    Back,
    Nothing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Import {
    /// Bundles as accounts names them (`system:mike.facct`).
    pub files: Vec<String>,
    pub chosen: usize,
    pub password: String,
    /// The preview's words, once the password opened the file.
    pub preview: Option<String>,
    pub error: Option<String>,
}

impl Import {
    pub fn new(files: Vec<String>) -> Import {
        Import { files, ..Import::default() }
    }

    /// A file's name as shown: without its `system:` place.
    pub fn shown(name: &str) -> &str {
        name.split_once(':').map_or(name, |(_, n)| n)
    }

    pub fn file(&self) -> Option<&str> {
        self.files.get(self.chosen).map(String::as_str)
    }

    pub fn key(&mut self, usage: u16, ch: u32) -> Outcome {
        match usage {
            key::ESCAPE => Outcome::Back,
            key::UP => self.choose(self.chosen.wrapping_sub(1)),
            key::DOWN => self.choose(self.chosen + 1),
            key::ENTER if self.file().is_none() || self.password.is_empty() => Outcome::Nothing,
            key::ENTER if self.preview.is_some() => Outcome::Import,
            key::ENTER => Outcome::Preview,
            key::BACKSPACE => {
                if self.password.pop().is_none() {
                    return Outcome::Nothing;
                }
                self.edited();
                Outcome::Changed
            }
            _ => match char::from_u32(ch).filter(|c| !c.is_control()) {
                Some(c) if self.password.chars().count() < 128 => {
                    self.password.push(c);
                    self.edited();
                    Outcome::Changed
                }
                _ => Outcome::Nothing,
            },
        }
    }

    /// Another file: the password and the preview start again.
    pub fn choose(&mut self, i: usize) -> Outcome {
        if i >= self.files.len() || i == self.chosen {
            return Outcome::Nothing;
        }
        self.chosen = i;
        self.forget_password();
        self.preview = None;
        self.error = None;
        Outcome::Changed
    }

    /// The password changed: a preview made with the old one goes.
    fn edited(&mut self) {
        self.preview = None;
        self.error = None;
    }

    pub fn forget_password(&mut self) {
        fstd::secret::wipe_str(&mut self.password);
        self.password.clear();
    }
}

/// *"Account 'mike', 3 files, 1.2 MB, exported 19 Nov 2026"*.
pub fn preview_words(username: &str, files: u64, bytes: u64, exported_unix: u64) -> String {
    let (y, m, d) = date(exported_unix);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    alloc::format!(
        "Account '{username}', {files} file{}, {}, exported {d} {} {y}",
        if files == 1 { "" } else { "s" },
        size(bytes),
        MONTHS[(m - 1) as usize]
    )
}

/// A size as people read it: bytes, KB, MB, GB (powers of 1,000).
pub fn size(bytes: u64) -> String {
    match bytes {
        0..1_000 => alloc::format!("{bytes} bytes"),
        1_000..1_000_000 => alloc::format!("{:.1} KB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => alloc::format!("{:.1} MB", bytes as f64 / 1e6),
        _ => alloc::format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

/// The civil date (UTC) of a Unix time: year, month 1–12, day 1–31.
pub fn date(unix: u64) -> (i64, u32, u32) {
    // Howard Hinnant's days-to-civil.
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn dates_and_sizes_read_as_people_write_them() {
        assert_eq!(date(0), (1970, 1, 1));
        assert_eq!(date(951_782_400), (2000, 2, 29));
        assert_eq!(date(1_795_046_400), (2026, 11, 19));
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1_200_000), "1.2 MB");
        assert_eq!(
            preview_words("mike", 3, 1_200_000, 1_795_046_400),
            "Account 'mike', 3 files, 1.2 MB, exported 19 Nov 2026"
        );
        assert_eq!(preview_words("dana", 1, 10, 0), "Account 'dana', 1 file, 10 bytes, exported 1 Jan 1970");
    }

    #[test]
    fn enter_previews_then_imports_and_changes_start_again() {
        let mut i = Import::new(vec!["system:mike.facct".into(), "system:dana.facct".into()]);
        assert_eq!(Import::shown("system:mike.facct"), "mike.facct");
        assert_eq!(i.key(key::ENTER, 0), Outcome::Nothing, "no password yet");
        for c in "secret!!".chars() {
            i.key(0, c as u32);
        }
        assert_eq!(i.key(key::ENTER, 0), Outcome::Preview);
        i.preview = Some("Account 'mike'".into());
        assert_eq!(i.key(key::ENTER, 0), Outcome::Import);
        i.key(key::BACKSPACE, 0);
        assert_eq!(i.preview, None, "the password changed: no preview");
        assert_eq!(i.key(key::DOWN, 0), Outcome::Changed);
        assert_eq!((i.file(), i.password.as_str()), (Some("system:dana.facct"), ""));
        assert_eq!(i.key(key::DOWN, 0), Outcome::Nothing);
        assert_eq!(i.key(key::ESCAPE, 0), Outcome::Back);
        let mut none = Import::new(Vec::new());
        none.key(0, 'x' as u32);
        assert_eq!(none.key(key::ENTER, 0), Outcome::Nothing, "no file to import");
    }
}
