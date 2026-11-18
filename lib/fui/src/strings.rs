// SPDX-License-Identifier: Apache-2.0
//! String catalogs (TASK-055): the interface's words live in
//! `assets/strings/<lang>.txt`, never in code. Nodes name a key; the catalog
//! gives the words. English first; the 18 languages follow (FR-073).
//!
//! The format, one entry a line: `key = words`. `#` starts a comment line;
//! blank lines are nothing. Keys are `[a-z0-9.-]`.

use alloc::collections::BTreeMap;
use alloc::string::String;

#[derive(Debug, Default)]
pub struct Catalog {
    words: BTreeMap<String, String>,
}

impl Catalog {
    /// A catalog from its text; an unreadable line is an error naming it.
    pub fn parse(text: &str) -> Result<Catalog, String> {
        let mut words = BTreeMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| alloc::format!("line {}: no `=`", n + 1))?;
            let k = k.trim();
            if k.is_empty()
                || !k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            {
                return Err(alloc::format!("line {}: a bad key {k:?}", n + 1));
            }
            if words.insert(String::from(k), String::from(v.trim())).is_some() {
                return Err(alloc::format!("line {}: {k} again", n + 1));
            }
        }
        Ok(Catalog { words })
    }

    /// The words for `key`. A key the catalog lacks is a bug: it fails a
    /// debug assertion, and release builds show the key itself, bracketed,
    /// so it's seen and reported rather than hidden.
    pub fn get(&self, key: &str) -> String {
        match self.words.get(key) {
            Some(w) => w.clone(),
            None => {
                debug_assert!(false, "no string for {key}");
                alloc::format!("[{key}]")
            }
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.words.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.words.keys().map(String::as_str)
    }
}
