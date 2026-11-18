// SPDX-License-Identifier: Apache-2.0
//! Per-process handle tables (TASK-024).

use super::ObjectRef;
use crate::rand;
use alloc::collections::BTreeMap;
use fcore_abi::{FErr, rights};

/// A handle: the object and what the holder may do with it.
#[derive(Clone)]
pub struct Handle {
    pub object: ObjectRef,
    pub rights: u32,
}

/// The most handles one process may hold.
pub const MAX_HANDLES: usize = 65_536;

#[derive(Default)]
pub struct Table {
    map: BTreeMap<u32, Handle>,
}

impl Table {
    pub const fn new() -> Table {
        Table { map: BTreeMap::new() }
    }

    /// Adds a handle; returns its value (random, non-zero, unused).
    pub fn add(&mut self, h: Handle) -> Result<u32, FErr> {
        if self.map.len() >= MAX_HANDLES {
            return Err(FErr::NoMemory);
        }
        loop {
            let v = rand::u32();
            if v == 0 {
                continue;
            }
            if let alloc::collections::btree_map::Entry::Vacant(e) = self.map.entry(v) {
                e.insert(h);
                return Ok(v);
            }
        }
    }

    /// Adds a handle at a fixed value (handle 1, conductor's channel, at process start).
    pub fn add_at(&mut self, value: u32, h: Handle) -> Result<(), FErr> {
        if self.map.contains_key(&value) {
            return Err(FErr::AlreadyExists);
        }
        self.map.insert(value, h);
        Ok(())
    }

    /// The handle `v`, if it has every right in `need`.
    pub fn get(&self, v: u32, need: u32) -> Result<&Handle, FErr> {
        let h = self.map.get(&v).ok_or(FErr::BadHandle)?;
        if h.rights & need != need {
            return Err(FErr::AccessDenied);
        }
        Ok(h)
    }

    pub fn remove(&mut self, v: u32) -> Result<Handle, FErr> {
        self.map.remove(&v).ok_or(FErr::BadHandle)
    }

    /// A new handle to the same object with `rights` (which may only narrow).
    pub fn duplicate(&mut self, v: u32, rights: u32) -> Result<u32, FErr> {
        let h = self.get(v, rights::DUPLICATE)?;
        if rights & !h.rights != 0 {
            return Err(FErr::AccessDenied);
        }
        let dup = Handle { object: h.object.clone(), rights };
        self.add(dup)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Every handle, closed (the process is going away).
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

ktest!(handles_closed_handles_are_bad_and_rights_only_narrow, {
    use super::{Header, Object};
    use alloc::sync::Arc;
    let mut t = Table::new();
    let ev = Arc::new(Object::Event(Header::new()));
    let all = rights::DUPLICATE | rights::WAIT | rights::SIGNAL;
    let h = t.add(Handle { object: ev, rights: all })?;
    let narrow = t.duplicate(h, rights::WAIT).map_err(|_| "duplicate failed")?;
    if t.get(narrow, rights::SIGNAL).err() != Some(FErr::AccessDenied) {
        return Err("a narrowed handle kept a right it lost");
    }
    if t.duplicate(narrow, rights::WAIT).err() != Some(FErr::AccessDenied) {
        return Err("a handle without DUPLICATE was duplicated");
    }
    if t.duplicate(h, rights::WAIT | rights::MANAGE).err() != Some(FErr::AccessDenied) {
        return Err("duplicate widened rights");
    }
    t.remove(h).map_err(|_| "remove")?;
    if t.get(h, 0).err() != Some(FErr::BadHandle) {
        return Err("a closed handle still works");
    }
    if h == 0 || narrow == 0 || h == narrow {
        return Err("handle values aren't distinct and non-zero");
    }
    Ok(())
});
