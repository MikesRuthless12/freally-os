// SPDX-License-Identifier: Apache-2.0
//! The account's bin (FR-049): a deleted file or folder isn't gone, it's
//! moved to `<home>/.bin/<n>/<its name>`, and `<home>/.bin/<n>.from` says
//! the folder it came from. *Restore* puts it back (beside anything that
//! took its name meanwhile: "name (restored)"); *Empty the bin* removes
//! everything in it for good. The bin is in the home, so it's sealed with it.

use crate::disk::{Disk, Said, join, split};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const BIN: &str = ".bin";

/// Something in the bin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binned {
    /// Its place in the bin (`<n>`).
    pub id: u64,
    pub name: String,
    pub dir: bool,
    /// The folder it was deleted from.
    pub from: String,
}

fn bin_of(home: &str) -> String {
    join(home, BIN)
}

/// Every numbered slot in the bin.
fn slots(d: &mut dyn Disk, home: &str) -> Vec<u64> {
    d.list(&bin_of(home))
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, dir)| *dir)
        .filter_map(|(n, _)| n.parse().ok())
        .collect()
}

/// `path` (in `home`) moved to the bin. Its slot.
pub fn delete(d: &mut dyn Disk, home: &str, path: &str) -> Result<u64, Said> {
    let bin = bin_of(home);
    if path == home || path == bin || path.starts_with(&alloc::format!("{bin}/")) {
        return Err(String::from("That can't go in the bin."));
    }
    if !d.exists(&bin) {
        d.mkdir(&bin)?;
    }
    let id = slots(d, home).into_iter().max().map_or(1, |m| m + 1);
    let slot = join(&bin, &id.to_string());
    let (from, name) = split(path);
    d.mkdir(&slot)?;
    if let Err(e) = d.rename(path, &join(&slot, name)) {
        let _ = d.remove(&slot);
        return Err(e);
    }
    d.write(&alloc::format!("{slot}.from"), from.as_bytes())?;
    Ok(id)
}

/// What's in the bin, newest first.
pub fn list(d: &mut dyn Disk, home: &str) -> Vec<Binned> {
    let bin = bin_of(home);
    let mut ids = slots(d, home);
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids.into_iter()
        .filter_map(|id| {
            let slot = join(&bin, &id.to_string());
            let (name, dir) = d.list(&slot).ok()?.into_iter().next()?;
            let from = d
                .read(&alloc::format!("{slot}.from"))
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_else(|| String::from(home));
            Some(Binned { id, name, dir, from })
        })
        .collect()
}

/// Slot `id` put back where it came from (its folder made again if it's
/// gone). Where it went.
pub fn restore(d: &mut dyn Disk, home: &str, id: u64) -> Result<String, Said> {
    let item = list(d, home).into_iter().find(|b| b.id == id).ok_or(String::from("That isn't in the bin."))?;
    let slot = join(&bin_of(home), &id.to_string());
    let mut to_dir = item.from.clone();
    // The folder it came from: inside the home, or the home itself.
    if !(to_dir == home || to_dir.starts_with(&alloc::format!("{home}/"))) {
        to_dir = String::from(home);
    }
    if !d.exists(&to_dir) {
        to_dir = String::from(home);
    }
    let mut to = join(&to_dir, &item.name);
    let mut n = 1;
    while d.exists(&to) {
        let suffix = if n == 1 { String::from(" (restored)") } else { alloc::format!(" (restored {n})") };
        to = join(&to_dir, &alloc::format!("{}{suffix}", item.name));
        n += 1;
    }
    d.rename(&join(&slot, &item.name), &to)?;
    d.remove(&slot)?;
    d.remove(&alloc::format!("{slot}.from"))?;
    Ok(to)
}

/// Everything in the bin, gone for good. How many items.
pub fn empty(d: &mut dyn Disk, home: &str) -> Result<usize, Said> {
    let bin = bin_of(home);
    let n = list(d, home).len();
    if d.exists(&bin) {
        for (name, _) in d.list(&bin)? {
            d.remove_all(&join(&bin, &name))?;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::Mem;

    const HOME: &str = "/home/mike";

    fn disk() -> Mem {
        Mem::with(&["/home/", "/home/mike/", "/home/mike/Notes/", "/home/mike/Notes/a.txt", "/home/mike/b.txt"])
    }

    #[test]
    fn deleted_goes_to_the_bin_and_comes_back() {
        let mut d = disk();
        assert_eq!(delete(&mut d, HOME, "/home/mike/Notes/a.txt"), Ok(1));
        assert_eq!(delete(&mut d, HOME, "/home/mike/Notes"), Ok(2));
        assert!(!d.exists("/home/mike/Notes"));
        let l = list(&mut d, HOME);
        assert_eq!(
            l.iter().map(|b| (b.id, b.name.as_str(), b.dir)).collect::<Vec<_>>(),
            [(2, "Notes", true), (1, "a.txt", false)]
        );
        assert_eq!(l[1].from, "/home/mike/Notes");
        // a.txt's folder is gone: it comes back to the home.
        assert_eq!(restore(&mut d, HOME, 1), Ok(String::from("/home/mike/a.txt")));
        assert_eq!(restore(&mut d, HOME, 2), Ok(String::from("/home/mike/Notes")));
        assert!(list(&mut d, HOME).is_empty());
        assert!(d.exists("/home/mike/a.txt") && d.exists("/home/mike/Notes"));
    }

    #[test]
    fn a_name_taken_meanwhile_is_kept() {
        let mut d = disk();
        delete(&mut d, HOME, "/home/mike/b.txt").unwrap();
        d.write("/home/mike/b.txt", b"new").unwrap();
        assert_eq!(restore(&mut d, HOME, 1), Ok(String::from("/home/mike/b.txt (restored)")));
        assert_eq!(d.read("/home/mike/b.txt").unwrap(), b"new");
    }

    #[test]
    fn emptied_for_good() {
        let mut d = disk();
        delete(&mut d, HOME, "/home/mike/Notes").unwrap();
        delete(&mut d, HOME, "/home/mike/b.txt").unwrap();
        assert_eq!(empty(&mut d, HOME), Ok(2));
        assert!(list(&mut d, HOME).is_empty());
        assert!(!d.items.keys().any(|k| k.contains("a.txt")), "{:?}", d.items.keys());
    }

    #[test]
    fn the_home_and_the_bin_itself_stay() {
        let mut d = disk();
        assert!(delete(&mut d, HOME, HOME).is_err());
        delete(&mut d, HOME, "/home/mike/b.txt").unwrap();
        assert!(delete(&mut d, HOME, "/home/mike/.bin").is_err());
        assert!(delete(&mut d, HOME, "/home/mike/.bin/1").is_err());
    }
}
