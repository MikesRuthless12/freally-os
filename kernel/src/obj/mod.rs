// SPDX-License-Identifier: Apache-2.0
//! Kernel objects and handles (TASK-024).
//!
//! - Every object is reference-counted (`Arc`) and destroyed when the last
//!   handle (and the last internal reference) goes.
//! - A process reaches an object only through a handle in its own table; each
//!   handle carries rights, which `handle_duplicate` can only narrow.
//! - Handle values are random 32-bit numbers (never 0, never reused while live),
//!   so a stale or guessed value almost never names another object.
//! - Every object has a signal word for waits (TASK-028): bits 0–23 are the
//!   kernel's, 24–31 the user's.

pub mod handles;

use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use fcore_abi::FErr;

/// Signals (bits of an object's signal word).
pub mod signal {
    pub const READABLE: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const PEER_CLOSED: u32 = 1 << 2;
    pub const TERMINATED: u32 = 1 << 3;
    pub const SIGNALED: u32 = 1 << 4;
    pub const USER_MASK: u32 = 0xFF00_0000;
}

/// What an object is (`FErr::WrongType` when a call gets the wrong one).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Process,
    Thread,
    AddressSpace,
    Vmo,
    Channel,
    Event,
    Interrupt,
    Resource,
}

/// The parts every object shares: its signal word and who waits on it.
pub struct Header {
    pub signals: SpinLock<u32>,
    pub waiters: SpinLock<Vec<crate::sched::ThreadRef>>,
}

impl Default for Header {
    fn default() -> Self {
        Self::new()
    }
}

impl Header {
    pub const fn new() -> Header {
        Header { signals: SpinLock::new(0), waiters: SpinLock::new(Vec::new()) }
    }

    pub fn get(&self) -> u32 {
        *self.signals.lock()
    }

    /// Clears then sets bits, and wakes every waiter (each rechecks its own condition).
    pub fn update(&self, clear: u32, set: u32) {
        {
            let mut s = self.signals.lock();
            *s = (*s & !clear) | set;
        }
        let waiters: Vec<_> = core::mem::take(&mut *self.waiters.lock());
        for t in waiters {
            crate::sched::wake(&t);
        }
    }
}

/// A kernel object.
pub enum Object {
    Process(crate::proc::Process),
    Thread(crate::sched::ThreadRef),
    AddressSpace(Arc<crate::mm::aspace::AddressSpace>),
    Vmo(Arc<crate::mm::vmo::Vmo>),
    Channel(crate::ipc::ChannelEnd),
    Event(Header),
    /// A hardware range or IRQ a driver may use (TASK-040).
    Resource(Resource),
}

/// What a resource handle permits (devmgr holds the root; drivers get slices).
#[derive(Clone, Copy, Debug)]
pub struct Resource {
    pub kind: ResourceKind,
    pub base: u64,
    pub len: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceKind {
    Root,
    Mmio,
    IoPort,
    Irq,
}

impl Object {
    pub fn kind(&self) -> Kind {
        match self {
            Object::Process(_) => Kind::Process,
            Object::Thread(_) => Kind::Thread,
            Object::AddressSpace(_) => Kind::AddressSpace,
            Object::Vmo(_) => Kind::Vmo,
            Object::Channel(_) => Kind::Channel,
            Object::Event(_) => Kind::Event,
            Object::Resource(_) => Kind::Resource,
        }
    }

    /// The object's signal word, if it can be waited on.
    pub fn header(&self) -> Option<&Header> {
        match self {
            Object::Event(h) => Some(h),
            Object::Channel(c) => Some(c.header()),
            Object::Process(p) => Some(&p.header),
            Object::Thread(t) => Some(&t.header),
            _ => None,
        }
    }
}

pub type ObjectRef = Arc<Object>;

/// Waits are for these kinds only.
pub fn waitable(o: &Object) -> Result<&Header, FErr> {
    o.header().ok_or(FErr::WrongType)
}
