// SPDX-License-Identifier: Apache-2.0
//! The system image and BootInfo, as the kernel hands them to conductor
//! (handles 5 and 6): mapped read-only. Every file conductor uses is checked
//! against its TOC hash before it's used (`fsi::recheck_file`).

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{BootInfo, FErr, perm, well_known};

pub struct Image {
    bytes: &'static [u8],
    pub flavour: u8,
    pub version: String,
    /// The slot booted (0 = A, 1 = B) and the A/B record as the bootloader left it.
    pub slot: u8,
    pub bootctl: [u8; 32],
    /// The boot options (dev and e2e bootloaders read them: TASK-074).
    pub cmdline: [u8; 256],
    /// The firmware's framebuffer: physical base and size.
    pub framebuffer: (u64, u64),
}

/// Maps `len` bytes of `vmo` read-only into conductor.
fn map(vmo: u32, len: u64) -> Result<&'static [u8], FErr> {
    let len = len.next_multiple_of(4096);
    let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo, 0, len, 0, perm::READ)?;
    // SAFETY: the kernel just mapped `len` readable bytes at `at`; they stay mapped.
    Ok(unsafe { core::slice::from_raw_parts(at as *const u8, len as usize) })
}

impl Image {
    pub fn open() -> Result<Image, FErr> {
        let bi = map(well_known::BOOTINFO_VMO, 4096)?;
        // SAFETY: the kernel copied a BootInfo to offset 0 of this page.
        let info: BootInfo = unsafe { core::ptr::read_unaligned(bi.as_ptr() as *const BootInfo) };
        let all = map(well_known::IMAGE_VMO, info.image.len)?;
        let end = info.image.version.iter().position(|&b| b == 0).unwrap_or(32);
        Ok(Image {
            bytes: &all[..info.image.len as usize],
            flavour: match info.flavour {
                1 => 1,
                2 => 2,
                _ => 0,
            },
            version: String::from_utf8_lossy(&info.image.version[..end]).into_owned(),
            slot: info.image.slot as u8,
            bootctl: info.bootctl,
            cmdline: info.cmdline,
            framebuffer: (info.framebuffer.base, info.framebuffer.size),
        })
    }

    /// A file's bytes, checked against its TOC hash.
    pub fn file(&self, path: &str) -> Result<&'static [u8], String> {
        fsi::recheck_file(self.bytes, path).map(|(_, b)| b).map_err(|e| String::from(e.describe()))
    }

    /// Every path in the TOC.
    pub fn paths(&self) -> Result<Vec<String>, String> {
        Ok(fsi::recheck_toc(self.bytes)
            .map_err(|e| String::from(e.describe()))?
            .map(|e| String::from(e.path()))
            .collect())
    }
}
