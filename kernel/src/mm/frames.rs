// SPDX-License-Identifier: Apache-2.0
//! Physical memory (TASK-020): one bit per 4 KiB frame over every usable
//! region, kept in the first usable region big enough to hold it.
//!
//! - Frames below 1 MiB stay reserved (the AP trampoline lives there, TASK-036).
//! - Only `Usable` memory is ever handed out: the kernel, the image, BootInfo,
//!   the page tables and every firmware region stay where they are.

use crate::sync::SpinLock;
use fcore_abi::layout::DIRECT_MAP;
use fcore_abi::{MemKind, MemRegion};

pub const FRAME: u64 = 4096;
const LOW_RESERVED: u64 = 1 << 20;

struct Frames {
    bits: *mut u64,
    frames: u64,
    free: u64,
    next: u64,
}

// SAFETY: the bitmap is only reached through the lock.
unsafe impl Send for Frames {}

static FRAMES: SpinLock<Option<Frames>> = SpinLock::new(None);

impl Frames {
    fn word(&self, frame: u64) -> *mut u64 {
        // SAFETY: callers keep `frame` below `self.frames`.
        unsafe { self.bits.add((frame / 64) as usize) }
    }
    fn used(&self, frame: u64) -> bool {
        // SAFETY: as above.
        unsafe { *self.word(frame) & (1 << (frame % 64)) != 0 }
    }
    fn set(&mut self, frame: u64, used: bool) {
        // SAFETY: as above.
        unsafe {
            let w = self.word(frame);
            if used {
                *w |= 1 << (frame % 64);
            } else {
                *w &= !(1 << (frame % 64));
            }
        }
    }
}

/// Builds the bitmap from the memory map.
pub fn init(map: &[MemRegion]) -> Result<(), &'static str> {
    let top =
        map.iter().filter(|r| r.kind == MemKind::Usable).map(|r| r.base + r.len).max().ok_or("no usable memory")?;
    let frames = top / FRAME;
    let bytes = frames.div_ceil(64) * 8;
    let home = map
        .iter()
        .filter(|r| r.kind == MemKind::Usable)
        .find(|r| r.base >= LOW_RESERVED && r.len >= bytes.next_multiple_of(FRAME))
        .ok_or("no room for the frame bitmap")?;
    let bits = (home.base + DIRECT_MAP) as *mut u64;
    // SAFETY: `home` is usable RAM, mapped by the direct map, and holds `bytes`.
    unsafe { core::ptr::write_bytes(bits as *mut u8, 0xFF, bytes as usize) };
    let mut f = Frames { bits, frames, free: 0, next: LOW_RESERVED / FRAME };
    for r in map.iter().filter(|r| r.kind == MemKind::Usable) {
        let first = r.base.div_ceil(FRAME).max(LOW_RESERVED / FRAME);
        let last = (r.base + r.len) / FRAME;
        for fr in first..last {
            f.set(fr, false);
            f.free += 1;
        }
    }
    let home_first = home.base / FRAME;
    for fr in home_first..home_first + bytes.div_ceil(FRAME) {
        if !f.used(fr) {
            f.set(fr, true);
            f.free -= 1;
        }
    }
    *FRAMES.lock() = Some(f);
    Ok(())
}

/// One frame (not zeroed), or `None` when memory is gone.
pub fn alloc() -> Option<u64> {
    alloc_contig(1)
}

/// `n` physically contiguous frames (not zeroed).
pub fn alloc_contig(n: u64) -> Option<u64> {
    let mut g = FRAMES.lock();
    let f = g.as_mut()?;
    if n == 0 || f.free < n {
        return None;
    }
    let start = f.next;
    let mut fr = start;
    let mut wrapped = false;
    loop {
        if fr + n > f.frames {
            if wrapped {
                return None;
            }
            wrapped = true;
            fr = LOW_RESERVED / FRAME;
            continue;
        }
        if wrapped && fr >= start {
            return None;
        }
        // Skip a fully used word quickly when looking for one frame.
        if n == 1 && fr.is_multiple_of(64) {
            // SAFETY: fr < frames.
            if unsafe { *f.word(fr) } == u64::MAX {
                fr += 64;
                continue;
            }
        }
        match (0..n).find(|i| f.used(fr + i)) {
            None => {
                for i in 0..n {
                    f.set(fr + i, true);
                }
                f.free -= n;
                f.next = fr + n;
                return Some(fr * FRAME);
            }
            Some(i) => fr += i + 1,
        }
    }
}

/// One zeroed frame.
pub fn alloc_zeroed() -> Option<u64> {
    let p = alloc()?;
    // SAFETY: the frame is ours; the direct map covers it.
    unsafe { core::ptr::write_bytes((p + DIRECT_MAP) as *mut u8, 0, FRAME as usize) };
    Some(p)
}

/// Returns `n` frames starting at `phys`. Freeing a free frame is a bug: it panics.
pub fn free_contig(phys: u64, n: u64) {
    let mut g = FRAMES.lock();
    let Some(f) = g.as_mut() else { return };
    for i in 0..n {
        let fr = phys / FRAME + i;
        assert!(fr < f.frames && f.used(fr), "a frame was freed twice");
        f.set(fr, false);
    }
    f.free += n;
}

pub fn free(phys: u64) {
    free_contig(phys, 1)
}

/// Takes one particular frame out of the free pool. False if it isn't free
/// (not usable RAM, or already taken).
pub fn reserve(phys: u64) -> bool {
    let mut g = FRAMES.lock();
    let Some(f) = g.as_mut() else { return false };
    let fr = phys / FRAME;
    if fr >= f.frames || f.used(fr) {
        return false;
    }
    f.set(fr, true);
    f.free -= 1;
    true
}

/// Free frames now.
pub fn free_count() -> u64 {
    FRAMES.lock().as_ref().map_or(0, |f| f.free)
}

ktest!(frames_a_million_allocations_lose_nothing, {
    let before = free_count();
    let mut held = [0u64; 1000];
    for _ in 0..1000 {
        for h in held.iter_mut() {
            *h = alloc().ok_or("ran out of frames")?;
        }
        for h in held.iter() {
            free(*h);
        }
    }
    if free_count() != before {
        return Err("free frames changed after a million alloc/free pairs");
    }
    Ok(())
});

ktest!(frames_contiguous_runs_are_contiguous_and_free_again, {
    let before = free_count();
    let p = alloc_contig(64).ok_or("no run of 64 frames")?;
    if p % FRAME != 0 {
        return Err("a run that isn't page-aligned");
    }
    free_contig(p, 64);
    if free_count() != before {
        return Err("the run didn't come back");
    }
    Ok(())
});
