// SPDX-License-Identifier: Apache-2.0
//! The user heap: size classes (16 B – 2 KiB) carved from 64 KiB chunks, and
//! whole chunks for anything larger. Each chunk is a VMO mapped read/write (never
//! executable) into the process's own address space through `vm_map`.

use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use fcore_abi::{perm, well_known};

const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
const CHUNK: usize = 64 * 1024;

struct State {
    free: [*mut u8; 8],
}

/// Single-threaded for now (Phase 1 programs have one thread); a futex lock
/// arrives with multi-threaded services.
struct Heap(UnsafeCell<State>);

// SAFETY: see above: one thread per process until the lock exists.
unsafe impl Sync for Heap {}

#[global_allocator]
static HEAP: Heap = Heap(UnsafeCell::new(State { free: [core::ptr::null_mut(); 8] }));

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
// to its class's list (large blocks are kept mapped for reuse in a later phase).
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let need = l.size().max(l.align());
        let Some(c) = CLASSES.iter().position(|&s| s >= need) else {
            return if l.align() <= 4096 { map(l.size()) } else { core::ptr::null_mut() };
        };
        // SAFETY: single-threaded (see above).
        let s = unsafe { &mut *self.0.get() };
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
        if let Some(c) = CLASSES.iter().position(|&s| s >= need) {
            // SAFETY: as above.
            let s = unsafe { &mut *self.0.get() };
            // SAFETY: `ptr` came from this class.
            unsafe { *(ptr as *mut *mut u8) = s.free[c] };
            s.free[c] = ptr;
        }
    }
}
