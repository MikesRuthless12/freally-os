// SPDX-License-Identifier: Apache-2.0
//! The kernel heap (TASK-021): slabs for 16 B – 2 KiB, whole frames above.
//!
//! - Each slab class (16, 32 … 2048 bytes) carves 4 KiB frames into objects on
//!   a free list; objects are aligned to their size.
//! - Larger requests take contiguous frames straight from the frame allocator.
//! - Everything lives in the direct map. Live-object counts make leaks visible.

use super::frames::{self, FRAME};
use crate::sync::SpinLock;
use core::alloc::{GlobalAlloc, Layout};
use fcore_abi::layout::DIRECT_MAP;

const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

struct Slab {
    free: [*mut u8; 8],
    live: [u64; 8],
    large_pages: u64,
}

// SAFETY: only reached through the lock.
unsafe impl Send for Slab {}

static SLAB: SpinLock<Slab> = SpinLock::new(Slab { free: [core::ptr::null_mut(); 8], live: [0; 8], large_pages: 0 });

fn class_of(l: &Layout) -> Option<usize> {
    let need = l.size().max(l.align());
    CLASSES.iter().position(|&c| c >= need)
}

pub struct Heap;

// SAFETY: allocations come from frames this allocator owns until dealloc.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if let Some(c) = class_of(&l) {
            let mut s = SLAB.lock();
            if s.free[c].is_null() {
                let Some(frame) = frames::alloc() else { return core::ptr::null_mut() };
                let base = (frame + DIRECT_MAP) as *mut u8;
                let size = CLASSES[c];
                // Thread the new frame's objects onto the free list.
                for i in (0..FRAME as usize / size).rev() {
                    // SAFETY: inside the frame just taken.
                    unsafe {
                        let obj = base.add(i * size);
                        *(obj as *mut *mut u8) = s.free[c];
                        s.free[c] = obj;
                    }
                }
            }
            let obj = s.free[c];
            // SAFETY: `obj` is a free object; its first word links the list.
            s.free[c] = unsafe { *(obj as *mut *mut u8) };
            s.live[c] += 1;
            return obj;
        }
        if l.align() > FRAME as usize {
            return core::ptr::null_mut();
        }
        let pages = (l.size() as u64).div_ceil(FRAME);
        match frames::alloc_contig(pages) {
            Some(p) => {
                SLAB.lock().large_pages += pages;
                (p + DIRECT_MAP) as *mut u8
            }
            None => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
        if let Some(c) = class_of(&l) {
            let mut s = SLAB.lock();
            // SAFETY: `ptr` came from this class; it becomes the list head.
            unsafe { *(ptr as *mut *mut u8) = s.free[c] };
            s.free[c] = ptr;
            s.live[c] -= 1;
            return;
        }
        let pages = (l.size() as u64).div_ceil(FRAME);
        frames::free_contig(ptr as u64 - DIRECT_MAP, pages);
        SLAB.lock().large_pages -= pages;
    }
}

#[global_allocator]
static HEAP: Heap = Heap;

/// Objects alive per class, and large-allocation pages (for leak checks).
pub fn live() -> ([u64; 8], u64) {
    let s = SLAB.lock();
    (s.live, s.large_pages)
}

ktest!(heap_ten_million_random_allocations_then_no_leak, {
    use alloc::vec::Vec;
    let before = live();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut held: Vec<(usize, alloc::boxed::Box<[u8]>)> = Vec::with_capacity(4096);
    for i in 0..10_000_000u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if held.len() < 4096 && (x & 1 == 0 || held.is_empty()) {
            let size = 1 + (x >> 8) as usize % 3000;
            let mut b = alloc::vec![0u8; size].into_boxed_slice();
            b[0] = i as u8;
            b[size - 1] = i as u8;
            held.push((size, b));
        } else {
            let idx = (x >> 16) as usize % held.len();
            let (size, b) = held.swap_remove(idx);
            if b.len() != size || b[0] != b[size - 1] {
                return Err("an allocation's contents changed");
            }
        }
    }
    drop(held);
    if live() != before {
        return Err("objects leaked after ten million allocations");
    }
    Ok(())
});

ktest!(heap_objects_are_aligned_to_their_class, {
    for size in [16usize, 32, 64, 128, 256, 512, 1024, 2048] {
        let l = Layout::from_size_align(size, size).map_err(|_| "layout")?;
        // SAFETY: a fresh allocation, freed at once.
        let p = unsafe { HEAP.alloc(l) };
        if p.is_null() || !(p as usize).is_multiple_of(size) {
            return Err("a misaligned or failed allocation");
        }
        // SAFETY: as above.
        unsafe { HEAP.dealloc(p, l) };
    }
    Ok(())
});
