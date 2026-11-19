// SPDX-License-Identifier: Apache-2.0
//! One Files window's state (TASK-105, FR-049): the places (Home, Bin),
//! the tools, and the folder's contents, folders first. Its keys:
//!
//! - Tab and Shift+Tab: the places (one stop, ↑ ↓), each tool, the list;
//! - in the list: ↑ ↓ Home End choose; Enter opens a folder; Backspace goes
//!   up (never above the home); F2 renames; Delete puts it in the bin;
//!   Ctrl+Shift+N makes a new folder and names it;
//! - naming: typing edits, Enter keeps it, Escape leaves it as it was.
//!
//! Everything it does to the disk goes through `Disk`, so the tests drive
//! it whole on a tree in RAM.

use crate::bin;
use crate::disk::{Disk, Said, join, split, valid_name};
use crate::key;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Home,
    Bin,
}

pub const PLACES: [Place; 2] = [Place::Home, Place::Bin];

impl Place {
    pub fn name(self) -> &'static str {
        match self {
            Place::Home => "Home",
            Place::Bin => "Bin",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Place::Home => "files-place-home",
            Place::Bin => "files-place-bin",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    List,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Up,
    NewFolder,
    Rename,
    Delete,
    Restore,
    Empty,
    View,
}

impl Tool {
    pub fn label(self, view: View) -> &'static str {
        match self {
            Tool::Up => "Up",
            Tool::NewFolder => "New folder",
            Tool::Rename => "Rename",
            Tool::Delete => "Delete",
            Tool::Restore => "Restore",
            Tool::Empty => "Empty the bin",
            Tool::View if view == View::List => "Grid",
            Tool::View => "List",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Tool::Up => "files-up",
            Tool::NewFolder => "files-new-folder",
            Tool::Rename => "files-rename",
            Tool::Delete => "files-delete",
            Tool::Restore => "files-restore",
            Tool::Empty => "files-empty",
            Tool::View => "files-view",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub modified_ns: u64,
    /// In the bin: its slot, and where it came from.
    pub binned: Option<(u64, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Places,
    Tool(Tool),
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Browser {
    pub home: String,
    pub place: Place,
    /// The folder shown (in the home).
    pub dir: String,
    pub entries: Vec<Entry>,
    pub chosen: Option<usize>,
    pub focus: Focus,
    pub view: View,
    /// The chosen entry's name as it's being typed (a rename, or a new
    /// folder's name).
    pub naming: Option<String>,
    /// What went wrong last, in words.
    pub error: Option<String>,
}

impl Browser {
    pub fn new(home: &str) -> Browser {
        Browser {
            home: String::from(home),
            place: Place::Home,
            dir: String::from(home),
            entries: Vec::new(),
            chosen: None,
            focus: Focus::List,
            view: View::List,
            naming: None,
            error: None,
        }
    }

    /// The tools this place offers, in order.
    pub fn tools(&self) -> Vec<Tool> {
        match self.place {
            Place::Home => alloc::vec![Tool::Up, Tool::NewFolder, Tool::Rename, Tool::Delete, Tool::View],
            Place::Bin => alloc::vec![Tool::Restore, Tool::Empty, Tool::View],
        }
    }

    pub fn enabled(&self, t: Tool) -> bool {
        match t {
            Tool::Up => self.place == Place::Home && self.dir != self.home,
            Tool::Rename | Tool::Delete | Tool::Restore => self.chosen.is_some() && self.naming.is_none(),
            Tool::Empty => !self.entries.is_empty(),
            Tool::NewFolder | Tool::View => self.naming.is_none(),
        }
    }

    pub fn entry(&self) -> Option<&Entry> {
        self.chosen.and_then(|i| self.entries.get(i))
    }

    /// The chosen entry's path.
    pub fn path(&self) -> Option<String> {
        self.entry().map(|e| join(&self.dir, &e.name))
    }

    /// The folder (or the bin) read again; the same entry chosen if it's
    /// still there.
    pub fn refresh(&mut self, d: &mut dyn Disk) {
        let was = self.entry().map(|e| e.name.clone());
        self.entries = match self.place {
            Place::Bin => bin::list(d, &self.home)
                .into_iter()
                .map(|b| Entry { name: b.name, dir: b.dir, size: 0, modified_ns: 0, binned: Some((b.id, b.from)) })
                .collect(),
            Place::Home => {
                let mut v: Vec<Entry> = d
                    .list(&self.dir)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(n, _)| !n.starts_with('.'))
                    .map(|(name, dir)| {
                        let m = d.meta(&join(&self.dir, &name)).unwrap_or_default();
                        Entry { name, dir, size: m.size, modified_ns: m.modified_ns, binned: None }
                    })
                    .collect();
                v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                v
            }
        };
        self.chosen = match was {
            Some(n) => self.entries.iter().position(|e| e.name == n),
            None => None,
        }
        .or(if self.entries.is_empty() { None } else { Some(0) });
    }

    fn done(&mut self, d: &mut dyn Disk, r: Result<(), Said>) -> Outcome {
        self.error = r.err();
        self.refresh(d);
        Outcome::Changed
    }

    pub fn go(&mut self, d: &mut dyn Disk, place: Place) -> Outcome {
        self.place = place;
        self.dir = self.home.clone();
        self.chosen = None;
        self.naming = None;
        self.error = None;
        self.refresh(d);
        Outcome::Changed
    }

    /// Into folder `name`.
    pub fn open(&mut self, d: &mut dyn Disk, name: &str) -> Outcome {
        self.dir = join(&self.dir, name);
        self.chosen = None;
        self.error = None;
        self.refresh(d);
        Outcome::Changed
    }

    pub fn up(&mut self, d: &mut dyn Disk) -> Outcome {
        if self.place != Place::Home || self.dir == self.home {
            return Outcome::Nothing;
        }
        let (parent, name) = split(&self.dir);
        let name = String::from(name);
        self.dir = String::from(parent);
        self.refresh(d);
        self.chosen = self.entries.iter().position(|e| e.name == name).or(self.chosen);
        Outcome::Changed
    }

    /// A tool pressed.
    pub fn press(&mut self, d: &mut dyn Disk, t: Tool) -> Outcome {
        if !self.enabled(t) {
            return Outcome::Nothing;
        }
        match t {
            Tool::Up => self.up(d),
            Tool::NewFolder => self.new_folder(d),
            Tool::Rename => {
                self.naming = self.entry().map(|e| e.name.clone());
                self.focus = Focus::List;
                Outcome::Changed
            }
            Tool::Delete => {
                let Some(p) = self.path() else { return Outcome::Nothing };
                let r = bin::delete(d, &self.home, &p).map(|_| ());
                self.done(d, r)
            }
            Tool::Restore => {
                let Some((id, _)) = self.entry().and_then(|e| e.binned.clone()) else { return Outcome::Nothing };
                let r = bin::restore(d, &self.home, id).map(|_| ());
                self.done(d, r)
            }
            Tool::Empty => {
                let r = bin::empty(d, &self.home).map(|_| ());
                self.done(d, r)
            }
            Tool::View => {
                self.view = if self.view == View::List { View::Grid } else { View::List };
                Outcome::Changed
            }
        }
    }

    /// "New folder" (or "New folder 2" …) made, chosen, and being named.
    pub fn new_folder(&mut self, d: &mut dyn Disk) -> Outcome {
        let mut name = String::from("New folder");
        let mut n = 2;
        while d.exists(&join(&self.dir, &name)) {
            name = alloc::format!("New folder {n}");
            n += 1;
        }
        if let Err(e) = d.mkdir(&join(&self.dir, &name)) {
            self.error = Some(e);
            return Outcome::Changed;
        }
        self.refresh(d);
        self.chosen = self.entries.iter().position(|e| e.name == name);
        self.naming = Some(name);
        self.focus = Focus::List;
        self.error = None;
        Outcome::Changed
    }

    /// The name typed kept: the chosen entry renamed.
    fn keep_name(&mut self, d: &mut dyn Disk) -> Outcome {
        let (Some(to), Some(e)) = (self.naming.clone(), self.entry().cloned()) else { return Outcome::Nothing };
        let to = String::from(to.trim());
        if to == e.name {
            self.naming = None;
            return Outcome::Changed;
        }
        if let Err(w) = valid_name(&to) {
            self.error = Some(String::from(w));
            return Outcome::Changed;
        }
        let target = join(&self.dir, &to);
        if d.exists(&target) {
            self.error = Some(alloc::format!("There's already something called {to} here."));
            return Outcome::Changed;
        }
        match d.rename(&join(&self.dir, &e.name), &target) {
            Ok(()) => {
                self.naming = None;
                self.error = None;
                self.refresh(d);
                self.chosen = self.entries.iter().position(|x| x.name == to);
            }
            Err(w) => self.error = Some(w),
        }
        Outcome::Changed
    }

    /// `from` (anywhere in the home) moved into folder `to_dir`.
    pub fn move_into(&mut self, d: &mut dyn Disk, from: &str, to_dir: &str) -> Outcome {
        let (from_dir, name) = split(from);
        let r = if from_dir == to_dir {
            Ok(())
        } else if to_dir == from || to_dir.starts_with(&alloc::format!("{from}/")) {
            Err(String::from("A folder can't go inside itself."))
        } else if d.exists(&join(to_dir, name)) {
            Err(alloc::format!("There's already something called {name} there."))
        } else {
            d.rename(from, &join(to_dir, name))
        };
        self.done(d, r)
    }

    fn step(&mut self, by: isize) -> Outcome {
        if self.entries.is_empty() {
            return Outcome::Nothing;
        }
        let last = self.entries.len() as isize - 1;
        let to = match self.chosen {
            None => 0,
            Some(i) => (i as isize + by).clamp(0, last) as usize,
        };
        if Some(to) == self.chosen {
            return Outcome::Nothing;
        }
        self.chosen = Some(to);
        Outcome::Changed
    }

    fn stops(&self) -> Vec<Focus> {
        let mut v = alloc::vec![Focus::Places];
        v.extend(self.tools().into_iter().filter(|&t| self.enabled(t)).map(Focus::Tool));
        v.push(Focus::List);
        v
    }

    pub fn key(&mut self, d: &mut dyn Disk, usage: u16, ch: u32, ctrl: bool, shift: bool) -> Outcome {
        if let Some(name) = self.naming.as_mut() {
            return match usage {
                key::ENTER => self.keep_name(d),
                key::ESCAPE => {
                    self.naming = None;
                    self.error = None;
                    Outcome::Changed
                }
                key::BACKSPACE => {
                    name.pop();
                    Outcome::Changed
                }
                _ => match char::from_u32(ch).filter(|c| !c.is_control()) {
                    Some(c) if name.len() < 255 => {
                        name.push(c);
                        Outcome::Changed
                    }
                    _ => Outcome::Nothing,
                },
            };
        }
        if usage == key::TAB {
            let stops = self.stops();
            let at = stops.iter().position(|&f| f == self.focus).unwrap_or(0);
            let n = stops.len();
            self.focus = stops[if shift { (at + n - 1) % n } else { (at + 1) % n }];
            return Outcome::Changed;
        }
        if ctrl && shift && usage == key::N {
            return if self.place == Place::Home { self.new_folder(d) } else { Outcome::Nothing };
        }
        match self.focus {
            Focus::Places => match usage {
                key::UP | key::DOWN => {
                    let to = if self.place == Place::Home { Place::Bin } else { Place::Home };
                    self.go(d, to)
                }
                _ => Outcome::Nothing,
            },
            Focus::Tool(t) => match usage {
                key::ENTER | key::SPACE => self.press(d, t),
                _ => Outcome::Nothing,
            },
            Focus::List => match usage {
                key::UP => self.step(-1),
                key::DOWN => self.step(1),
                key::HOME => self.step(isize::MIN / 2),
                key::END => self.step(isize::MAX / 2),
                key::ENTER => match self.entry().cloned() {
                    Some(e) if e.dir && self.place == Place::Home => self.open(d, &e.name),
                    _ => Outcome::Nothing,
                },
                key::BACKSPACE => self.up(d),
                key::F2 if self.place == Place::Home => self.press(d, Tool::Rename),
                key::DELETE if self.place == Place::Home => self.press(d, Tool::Delete),
                _ => Outcome::Nothing,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::Mem;

    fn disk() -> Mem {
        Mem::with(&[
            "/home/",
            "/home/mike/",
            "/home/mike/zeta.txt",
            "/home/mike/Alpha/",
            "/home/mike/Alpha/inner.txt",
            "/home/mike/beta.txt",
            "/home/mike/.hidden",
        ])
    }

    fn names(b: &Browser) -> Vec<&str> {
        b.entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn typed(b: &mut Browser, d: &mut Mem, s: &str) {
        for c in s.chars() {
            b.key(d, 0, c as u32, false, false);
        }
    }

    #[test]
    fn folders_first_then_names_and_hidden_left_out() {
        let mut d = disk();
        let mut b = Browser::new("/home/mike");
        b.refresh(&mut d);
        assert_eq!(names(&b), ["Alpha", "beta.txt", "zeta.txt"]);
        assert_eq!(b.chosen, Some(0));
    }

    #[test]
    fn enter_opens_and_backspace_goes_up_but_never_above_home() {
        let mut d = disk();
        let mut b = Browser::new("/home/mike");
        b.refresh(&mut d);
        b.key(&mut d, key::ENTER, 0, false, false);
        assert_eq!((b.dir.as_str(), names(&b)), ("/home/mike/Alpha", alloc::vec!["inner.txt"]));
        assert!(b.enabled(Tool::Up));
        b.key(&mut d, key::BACKSPACE, 0, false, false);
        assert_eq!(b.dir, "/home/mike");
        assert_eq!(b.entry().map(|e| e.name.as_str()), Some("Alpha"), "the folder it came from is chosen");
        assert_eq!(b.key(&mut d, key::BACKSPACE, 0, false, false), Outcome::Nothing);
    }

    #[test]
    fn rename_with_f2_checks_the_name() {
        let mut d = disk();
        let mut b = Browser::new("/home/mike");
        b.refresh(&mut d);
        b.key(&mut d, key::DOWN, 0, false, false);
        b.key(&mut d, key::F2, 0, false, false);
        assert_eq!(b.naming.as_deref(), Some("beta.txt"));
        for _ in 0..8 {
            b.key(&mut d, key::BACKSPACE, 0, false, false);
        }
        typed(&mut b, &mut d, "zeta.txt");
        b.key(&mut d, key::ENTER, 0, false, false);
        assert_eq!(b.error.as_deref(), Some("There's already something called zeta.txt here."));
        for _ in 0..8 {
            b.key(&mut d, key::BACKSPACE, 0, false, false);
        }
        typed(&mut b, &mut d, "a/b");
        b.key(&mut d, key::ENTER, 0, false, false);
        assert_eq!(b.error.as_deref(), Some("A name can't have a / in it."));
        for _ in 0..3 {
            b.key(&mut d, key::BACKSPACE, 0, false, false);
        }
        typed(&mut b, &mut d, "gamma.txt");
        b.key(&mut d, key::ENTER, 0, false, false);
        assert_eq!((b.naming.as_deref(), b.error.as_deref()), (None, None));
        assert_eq!(names(&b), ["Alpha", "gamma.txt", "zeta.txt"]);
        assert_eq!(b.entry().map(|e| e.name.as_str()), Some("gamma.txt"));
        // Escape leaves a name as it was.
        b.key(&mut d, key::F2, 0, false, false);
        typed(&mut b, &mut d, "x");
        b.key(&mut d, key::ESCAPE, 0, false, false);
        assert!(d.exists("/home/mike/gamma.txt"));
    }

    #[test]
    fn a_new_folder_is_named_as_its_made() {
        let mut d = disk();
        let mut b = Browser::new("/home/mike");
        b.refresh(&mut d);
        b.key(&mut d, key::N, 0, true, true);
        assert_eq!(b.naming.as_deref(), Some("New folder"));
        b.key(&mut d, key::ENTER, 0, false, false);
        b.press(&mut d, Tool::NewFolder);
        assert_eq!(b.naming.as_deref(), Some("New folder 2"));
        for _ in 0.."New folder 2".len() {
            b.key(&mut d, key::BACKSPACE, 0, false, false);
        }
        typed(&mut b, &mut d, "Taxes");
        b.key(&mut d, key::ENTER, 0, false, false);
        assert!(d.exists("/home/mike/Taxes") && d.exists("/home/mike/New folder"));
    }

    #[test]
    fn delete_goes_to_the_bin_and_restore_brings_it_back() {
        let mut d = disk();
        let mut b = Browser::new("/home/mike");
        b.refresh(&mut d);
        b.key(&mut d, key::END, 0, false, false);
        b.key(&mut d, key::DELETE, 0, false, false);
        assert_eq!(names(&b), ["Alpha", "beta.txt"]);
        b.go(&mut d, Place::Bin);
        assert_eq!(names(&b), ["zeta.txt"]);
        assert_eq!(b.tools(), [Tool::Restore, Tool::Empty, Tool::View]);
        b.press(&mut d, Tool::Restore);
        assert!(b.entries.is_empty() && !b.enabled(Tool::Empty));
        b.go(&mut d, Place::Home);
        assert_eq!(names(&b), ["Alpha", "beta.txt", "zeta.txt"]);
    }

    #[test]
    fn a_move_into_a_folder_and_never_into_itself() {
        let mut d = disk();
        let mut b = Browser::new("/home/mike");
        b.refresh(&mut d);
        b.move_into(&mut d, "/home/mike/beta.txt", "/home/mike/Alpha");
        assert_eq!(names(&b), ["Alpha", "zeta.txt"]);
        assert!(d.exists("/home/mike/Alpha/beta.txt"));
        b.move_into(&mut d, "/home/mike/Alpha", "/home/mike/Alpha");
        assert_eq!(b.error.as_deref(), Some("A folder can't go inside itself."));
        d.write("/home/mike/inner.txt", b"x").unwrap();
        b.move_into(&mut d, "/home/mike/inner.txt", "/home/mike/Alpha");
        assert_eq!(b.error.as_deref(), Some("There's already something called inner.txt there."));
    }

    #[test]
    fn tab_walks_the_places_the_tools_that_work_and_the_list() {
        let mut d = disk();
        let mut b = Browser::new("/home/mike");
        b.refresh(&mut d);
        b.focus = Focus::Places;
        let mut seen = Vec::new();
        for _ in 0..6 {
            b.key(&mut d, key::TAB, 0, false, false);
            seen.push(b.focus);
        }
        // Up is dark in the home: not a stop.
        assert_eq!(
            seen,
            [
                Focus::Tool(Tool::NewFolder),
                Focus::Tool(Tool::Rename),
                Focus::Tool(Tool::Delete),
                Focus::Tool(Tool::View),
                Focus::List,
                Focus::Places
            ]
        );
        b.key(&mut d, key::DOWN, 0, false, false);
        assert_eq!(b.place, Place::Bin);
    }
}
