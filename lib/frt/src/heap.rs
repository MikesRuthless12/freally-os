// SPDX-License-Identifier: Apache-2.0
//! The user heap: size classes (16 B – 2 KiB) carved from 64 KiB chunks, and
//! a mapping of its own for anything larger, unmapped when it's freed (a few
//! small ones kept for the next block of their size: a call's reply buffer
//! comes and goes with every call). Each chunk is a VMO mapped read/write
//! (never executable) into the process's own address space through `vm_map`.

use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use fcore_abi::{perm, well_known};

const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
const CHUNK: usize = 64 * 1024;

/// Freed large blocks kept for reuse: at most this many, each this big or less.
const SPARES: usize = 8;
const SPARE_MAX: usize = 64 * 1024;

struct State {
    free: [*mut u8; 8],
    /// Freed large blocks and their lengths (0: an empty slot).
    spare: [(*mut u8, usize); SPARES],
}

/// Single-threaded for now (Phase 1 programs have one thread); a futex lock
/// arrives with multi-threaded services.
struct Heap(UnsafeCell<State>);

// SAFETY: see above: one thread per process until the lock exists.
unsafe impl Sync for Heap {}

#[global_allocator]
static HEAP: Heap =
    Heap(UnsafeCell::new(State { free: [core::ptr::null_mut(); 8], spare: [(core::ptr::null_mut(), 0); SPARES] }));

/// Maps `bytes` (rounded up) of fresh zeroed memory.
fn map(bytes: usize) -> *mut u8 {
    let len = bytes.next_multiple_of(4096) as u64;
    let Ok(vmo) = sys::vmo_create(len) else { return core::ptr::null_mut() };
    let r = sys::vm_map(well_known::SELF_ASPACE, vmo, 0, len, 0, perm::READ | perm::WRITE);
    // The mapping keeps the VMO's pages; the handle isn't needed any more.
    let _ = sys::handle_close(vmo);
    r.map_or(core::ptr::null_mut(), |a| a as *mut u8)
}

// SAFETY: memory comes from mappings this allocator made; freed memory returns
// to its class's list, and a large block is unmapped (or kept as a spare).
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let need = l.size().max(l.align());
        // SAFETY: single-threaded (see above).
        let s = unsafe { &mut *self.0.get() };
        let Some(c) = CLASSES.iter().position(|&s| s >= need) else {
            if l.align() > 4096 {
                return core::ptr::null_mut();
            }
            let len = l.size().next_multiple_of(4096);
            if let Some(slot) = s.spare.iter_mut().find(|(_, n)| *n == len) {
                let (p, _) = core::mem::replace(slot, (core::ptr::null_mut(), 0));
                return p;
            }
            return map(len);
        };
        if s.free[c].is_null() {
            let base = map(CHUNK);
            if base.is_null() {
                return base;
            }
            let size = CLASSES[c];
            for i in (0..CHUNK / size).rev() {
                // SAFETY: inside the chunk just mapped.
                unsafe {
                    let obj = base.add(i * size);
                    *(obj as *mut *mut u8) = s.free[c];
                    s.free[c] = obj;
                }
            }
        }
        let obj = s.free[c];
        // SAFETY: a free object's first word links the list.
        s.free[c] = unsafe { *(obj as *mut *mut u8) };
        obj
    }

    unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
        let need = l.size().max(l.align());
        // SAFETY: as above.
        let s = unsafe { &mut *self.0.get() };
        match CLASSES.iter().position(|&s| s >= need) {
            Some(c) => {
                // SAFETY: `ptr` came from this class.
                unsafe { *(ptr as *mut *mut u8) = s.free[c] };
                s.free[c] = ptr;
            }
            None => {
                let len = l.size().next_multiple_of(4096);
                if len <= SPARE_MAX
                    && let Some(slot) = s.spare.iter_mut().find(|(_, n)| *n == 0)
                {
                    *slot = (ptr, len);
                    return;
                }
                // Its own mapping, from `map`: given back whole.
                let _ = sys::vm_unmap(well_known::SELF_ASPACE, ptr as u64, len as u64);
            }
        }
    }
}
