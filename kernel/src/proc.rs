// SPDX-License-Identifier: Apache-2.0
//! Processes: an address space, a handle table, threads, and the grants that
//! say what the process may ask the kernel for (TASK-024/027).

use crate::mm::aspace::AddressSpace;
use crate::obj::handles::{Handle, Table};
use crate::obj::{Header, Object, ObjectRef, signal};
use crate::sched::{self, ThreadRef};
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use fcore_abi::{FErr, rights, well_known};

pub struct Process {
    pub pid: u32,
    pub name: [u8; 32],
    pub header: Header,
    pub inner: SpinLock<Inner>,
    /// Port ranges `ioport_grant` opened (the scheduler reads them on every switch).
    pub io: SpinLock<Vec<(u16, u16)>>,
}

pub struct Inner {
    pub handles: Table,
    pub aspace: Arc<AddressSpace>,
    pub threads: Vec<ThreadRef>,
    pub exit_code: Option<i64>,
    pub grants: u32,
}

static NEXT_PID: AtomicU32 = AtomicU32::new(1);

impl Process {
    pub fn name(&self) -> &str {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..n]).unwrap_or("?")
    }
}

/// A new, empty process with its own address space; it holds handles to itself
/// and to its address space (well_known::SELF_PROCESS and SELF_ASPACE).
pub fn create(name: &str, grants: u32) -> Result<ObjectRef, FErr> {
    let aspace = AddressSpace::new()?;
    let mut n = [0u8; 32];
    n[..name.len().min(32)].copy_from_slice(&name.as_bytes()[..name.len().min(32)]);
    let p = Arc::new(Object::Process(Process {
        pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
        name: n,
        header: Header::new(),
        inner: SpinLock::new(Inner {
            handles: Table::new(),
            aspace: aspace.clone(),
            threads: Vec::new(),
            exit_code: None,
            grants,
        }),
        io: SpinLock::new(Vec::new()),
    }));
    let me = as_process(&p)?;
    let mut inner = me.inner.lock();
    // The process's handle to itself is weak in spirit: it can't keep itself alive
    // past exit, because exit clears every handle.
    inner.handles.add_at(
        well_known::SELF_PROCESS,
        Handle { object: p.clone(), rights: rights::READ | rights::MANAGE | rights::WAIT },
    )?;
    inner.handles.add_at(
        well_known::SELF_ASPACE,
        Handle { object: Arc::new(Object::AddressSpace(aspace)), rights: rights::MAP | rights::READ | rights::WRITE },
    )?;
    drop(inner);
    Ok(p)
}

pub fn as_process(o: &ObjectRef) -> Result<&Process, FErr> {
    match &**o {
        Object::Process(p) => Ok(p),
        _ => Err(FErr::WrongType),
    }
}

/// The address space a thread's process runs in.
pub fn aspace_of(o: &ObjectRef) -> Option<Arc<AddressSpace>> {
    as_process(o).ok().map(|p| p.inner.lock().aspace.clone())
}

/// The process the current thread belongs to.
pub fn current() -> Option<ObjectRef> {
    sched::current()?.process.lock().clone()
}

/// Ends a process: every handle closed and every thread killed. Its threads
/// unwind and end on their way back to user mode (the caller's included);
/// the last one out tells waiters (`thread_gone`), so "ended" means nothing
/// of it still runs.
pub fn terminate(o: &ObjectRef, code: i64) {
    let Ok(p) = as_process(o) else { return };
    let (threads, handles) = {
        let mut inner = p.inner.lock();
        if inner.exit_code.is_some() {
            return;
        }
        inner.exit_code = Some(code);
        (inner.threads.clone(), core::mem::take(&mut inner.handles))
    };
    // Closed outside the lock: closing runs objects' own clean-up.
    drop(handles);
    if threads.is_empty() {
        p.header.update(0, signal::TERMINATED);
    }
    for t in &threads {
        sched::kill(t);
    }
}

/// A thread of a process ended (from `sched::exit_current`). The last one
/// out ends the process: exit code 0 if nothing set one, every handle closed,
/// waiters told.
pub fn thread_gone(t: &ThreadRef) {
    let Some(o) = t.process.lock().clone() else { return };
    let Ok(p) = as_process(&o) else { return };
    let handles = {
        let mut inner = p.inner.lock();
        inner.threads.retain(|x| !Arc::ptr_eq(x, t));
        if !inner.threads.is_empty() {
            return;
        }
        inner.exit_code.get_or_insert(0);
        core::mem::take(&mut inner.handles)
    };
    drop(handles);
    p.header.update(0, signal::TERMINATED);
}
