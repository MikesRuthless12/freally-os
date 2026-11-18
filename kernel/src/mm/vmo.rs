// SPDX-License-Identifier: Apache-2.0
//! Virtual memory objects (TASK-025): pages of memory a process can map, read,
//! write and pass to another process. Pages are allocated and zeroed up front;
//! a contiguous VMO (DMA, TASK-040) is one physical run.

use super::frames::{self, FRAME};
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use fcore_abi::FErr;
use fcore_abi::layout::DIRECT_MAP;

/// The largest VMO: 1 GiB.
pub const MAX_SIZE: u64 = 1 << 30;

pub struct Vmo {
    pub size: u64,
    pub contiguous: bool,
    pages: SpinLock<Vec<u64>>,
}

impl Vmo {
    pub fn new(size: u64, contiguous: bool) -> Result<Arc<Vmo>, FErr> {
        if size == 0 || size > MAX_SIZE {
            return Err(FErr::InvalidArgs);
        }
        let n = size.div_ceil(FRAME);
        let mut pages = Vec::new();
        pages.try_reserve(n as usize).map_err(|_| FErr::NoMemory)?;
        if contiguous {
            let base = frames::alloc_contig(n).ok_or(FErr::NoMemory)?;
            // SAFETY: the run is ours; the direct map covers it.
            unsafe { core::ptr::write_bytes((base + DIRECT_MAP) as *mut u8, 0, (n * FRAME) as usize) };
            pages.extend((0..n).map(|i| base + i * FRAME));
        } else {
            for _ in 0..n {
                match frames::alloc_zeroed() {
                    Some(p) => pages.push(p),
                    None => {
                        for p in pages {
                            frames::free(p);
                        }
                        return Err(FErr::NoMemory);
                    }
                }
            }
        }
        Ok(Arc::new(Vmo { size, contiguous, pages: SpinLock::new(pages) }))
    }

    /// A VMO over existing physical memory the kernel already owns (the image, BootInfo).
    /// Its frames are never freed by the VMO.
    pub fn wrap(phys: u64, size: u64) -> Arc<Vmo> {
        let n = size.div_ceil(FRAME);
        let pages = (0..n).map(|i| ((phys & !(FRAME - 1)) + i * FRAME) | WRAPPED).collect();
        Arc::new(Vmo { size, contiguous: true, pages: SpinLock::new(pages) })
    }

    /// The physical page backing byte `off`.
    pub fn page(&self, off: u64) -> Option<u64> {
        self.pages.lock().get((off / FRAME) as usize).map(|p| p & !WRAPPED)
    }

    pub fn read(&self, off: u64, buf: &mut [u8]) -> Result<(), FErr> {
        self.copy(off, buf.len() as u64, |page_ptr, at, chunk| {
            // SAFETY: inside one page of this VMO.
            unsafe { core::ptr::copy_nonoverlapping(page_ptr, buf.as_mut_ptr().add(at), chunk) }
        })
    }

    pub fn write(&self, off: u64, data: &[u8]) -> Result<(), FErr> {
        self.copy(off, data.len() as u64, |page_ptr, at, chunk| {
            // SAFETY: inside one page of this VMO.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(at), page_ptr, chunk) }
        })
    }

    fn copy(&self, off: u64, len: u64, mut f: impl FnMut(*mut u8, usize, usize)) -> Result<(), FErr> {
        if off.checked_add(len).is_none_or(|e| e > self.size) {
            return Err(FErr::InvalidArgs);
        }
        let mut done = 0u64;
        while done < len {
            let at = off + done;
            let page = self.page(at).ok_or(FErr::InvalidArgs)?;
            let in_page = at % FRAME;
            let chunk = (FRAME - in_page).min(len - done);
            f((page + DIRECT_MAP + in_page) as *mut u8, done as usize, chunk as usize);
            done += chunk;
        }
        Ok(())
    }
}

/// Marks a wrapped page: not the VMO's to free.
const WRAPPED: u64 = 1;

impl Drop for Vmo {
    fn drop(&mut self) {
        let pages = core::mem::take(&mut *self.pages.lock());
        if pages.first().is_some_and(|p| p & WRAPPED != 0) {
            return;
        }
        if self.contiguous {
            if let Some(&base) = pages.first() {
                frames::free_contig(base, pages.len() as u64);
            }
        } else {
            for p in pages {
                frames::free(p);
            }
        }
    }
}

ktest!(vmo_reads_back_what_was_written_and_frees_its_frames, {
    let before = frames::free_count();
    {
        let v = Vmo::new(3 * FRAME + 100, false)?;
        let data: alloc::vec::Vec<u8> = (0..7000u32).map(|i| i as u8).collect();
        v.write(5000, &data)?;
        let mut back = alloc::vec![0u8; 7000];
        v.read(5000, &mut back)?;
        if back != data {
            return Err("a VMO read back something else");
        }
        if v.write(3 * FRAME + 50, &[0u8; 100]).is_ok() {
            return Err("a write past the end succeeded");
        }
    }
    if frames::free_count() != before { Err("a dropped VMO kept its frames") } else { Ok(()) }
});
