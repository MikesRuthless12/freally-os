// SPDX-License-Identifier: Apache-2.0
//! Reads the system image from the drive the bootloader itself came from, into
//! `LoaderData` pages. The firmware reads FAT; Freally OS never needs a FAT driver.

use crate::uefi::*;
use core::ffi::c_void;
use core::ptr::null_mut;

/// The largest image the bootloader will read (the GPT slots are 512 MiB).
const MAX_IMAGE: u64 = 512 << 20;

/// `\FREALLY\SYSTEM_A.FSI` and `SYSTEM_B.FSI` as NUL-terminated UTF-16.
pub const SYSTEM_A: &[u16] = &utf16::<22>("\\FREALLY\\SYSTEM_A.FSI");
pub const SYSTEM_B: &[u16] = &utf16::<22>("\\FREALLY\\SYSTEM_B.FSI");

/// The image isn't on the drive (the A/B rules then try the other slot).
pub const MISSING: &str = "There’s no Freally OS system image on this drive.";

const fn utf16<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i] as u16;
        i += 1;
    }
    out
}

/// Reads `path` from the bootloader's own drive.
///
/// # Safety
/// `bs` must be the firmware's boot services and `image` this bootloader's handle,
/// while boot services are still running.
pub unsafe fn read_image(bs: &BootServices, image: Handle, path: &[u16]) -> Result<&'static [u8], &'static str> {
    let mut li: *mut c_void = null_mut();
    if is_error((bs.handle_protocol)(image, &LOADED_IMAGE_GUID, &mut li)) {
        return Err("The firmware didn’t say which drive Freally OS started from.");
    }
    // SAFETY: the firmware returned a LoadedImage protocol for our own handle.
    let device = unsafe { (*(li as *mut LoadedImage)).device_handle };
    let mut sfs: *mut c_void = null_mut();
    if is_error((bs.handle_protocol)(device, &SIMPLE_FILE_SYSTEM_GUID, &mut sfs)) {
        return Err("The firmware can’t read the drive Freally OS started from.");
    }
    let sfs = sfs as *mut SimpleFileSystem;
    let mut root: *mut File = null_mut();
    // SAFETY: `sfs` is the firmware's SimpleFileSystem protocol.
    if is_error(unsafe { ((*sfs).open_volume)(sfs, &mut root) }) {
        return Err("The firmware can’t open the drive Freally OS started from.");
    }
    let mut file: *mut File = null_mut();
    // SAFETY: `root` is an open directory; `path` is NUL-terminated UTF-16.
    let st = unsafe { ((*root).open)(root, &mut file, path.as_ptr(), FILE_MODE_READ, 0) };
    if is_error(st) {
        // SAFETY: `root` is open.
        unsafe { ((*root).close)(root) };
        return Err(MISSING);
    }
    let result = (|| {
        let mut size = 0u64;
        // SAFETY: `file` is open. Setting the position to all ones moves to the end.
        unsafe {
            ((*file).set_position)(file, u64::MAX);
            ((*file).get_position)(file, &mut size);
            ((*file).set_position)(file, 0);
        }
        if size == 0 || size > MAX_IMAGE {
            return Err("The system image is empty or too large.");
        }
        let pages = size.div_ceil(4096) as usize;
        let mut addr = 0u64;
        // Kept after the handoff: conductor loads every service from it.
        if is_error((bs.allocate_pages)(ALLOCATE_ANY_PAGES, fcore_abi::memtype::IMAGE, pages, &mut addr)) {
            return Err("There isn’t enough memory to read the system image.");
        }
        let mut done = 0u64;
        while done < size {
            let mut n = (size - done) as usize;
            // SAFETY: `addr` has `pages` pages; `done + n` never exceeds `size`.
            let st = unsafe { ((*file).read)(file, &mut n, (addr + done) as *mut u8) };
            if is_error(st) || n == 0 {
                return Err("The drive stopped part-way through the system image.");
            }
            done += n as u64;
        }
        // SAFETY: `size` bytes at `addr` were just read into pages we own until exit.
        Ok(unsafe { core::slice::from_raw_parts(addr as *const u8, size as usize) })
    })();
    // SAFETY: both are open.
    unsafe {
        ((*file).close)(file);
        ((*root).close)(root);
    }
    result
}
