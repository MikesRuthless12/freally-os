// SPDX-License-Identifier: Apache-2.0
//! The GPT layout (TASK-091, PRD § 3): the disk the bootloader came from, its
//! GUID Partition Table read through the firmware's BlockIo, and each slot's
//! image read straight from its FREALLY_SYSTEM partition. The bootloader
//! prefers it; a drive without it (the files-layout stick) is read as files.
//! Read only: the bootloader writes no disk.

use crate::read::{MAX_IMAGE, MISSING};
use crate::uefi::*;
use core::ffi::c_void;
use core::ptr::null_mut;

/// Our own disk, with Freally OS's GPT layout: its BlockIo and where each
/// slot's partition is (first and last LBA).
pub struct Disk {
    io: *mut BlockIo,
    media_id: u32,
    block: u64,
    pub slots: [Option<(u64, u64)>; 2],
}

/// Pages for the bootloader's own use, freed by [`Pages::drop`]. The
/// firmware sees `LoaderData`; `tag` is what the ledger notes if they're kept.
struct Pages<'a> {
    bs: &'a BootServices,
    addr: u64,
    n: usize,
    tag: u32,
}

impl<'a> Pages<'a> {
    fn new(bs: &'a BootServices, bytes: usize, tag: u32) -> Option<Pages<'a>> {
        let n = bytes.div_ceil(4096);
        let mut addr = 0u64;
        if is_error((bs.allocate_pages)(ALLOCATE_ANY_PAGES, crate::uefi::LOADER_DATA, n, &mut addr)) {
            return None;
        }
        Some(Pages { bs, addr, n, tag })
    }

    fn bytes(&self) -> &'static mut [u8] {
        // SAFETY: `n` pages at `addr` are ours until dropped (or kept by `keep`).
        unsafe { core::slice::from_raw_parts_mut(self.addr as *mut u8, self.n * 4096) }
    }

    /// Kept after the handoff (the image): never freed, and noted in the
    /// ledger (`None` when it's full).
    fn keep(self) -> Option<u64> {
        let a = self.addr;
        let noted = crate::ledger::note(a, self.n as u64, self.tag);
        if !noted {
            return None;
        }
        core::mem::forget(self);
        Some(a)
    }
}

impl Drop for Pages<'_> {
    fn drop(&mut self) {
        (self.bs.free_pages)(self.addr, self.n);
    }
}

/// Reads `buf.len()` bytes (whole blocks) from `lba`, 4 MiB at a time.
///
/// # Safety
/// `d.io` is the firmware's BlockIo, while boot services run.
unsafe fn read(d: &Disk, lba: u64, buf: &mut [u8]) -> bool {
    const PIECE: usize = 4 << 20;
    for (i, piece) in buf.chunks_mut(PIECE).enumerate() {
        let at = lba + (i * PIECE) as u64 / d.block;
        // SAFETY: as this function's; `piece` is whole blocks of our pages.
        let st = unsafe { ((*d.io).read_blocks)(d.io, d.media_id, at, piece.len(), piece.as_mut_ptr() as *mut c_void) };
        if is_error(st) {
            return false;
        }
    }
    true
}

/// The partition GUID of the last HardDrive node of `path` (the partition we
/// were loaded from), and the path's length up to that node.
///
/// # Safety
/// `path` is a firmware device path (nodes up to an End node).
unsafe fn our_partition(path: *const u8) -> Option<(fcore_abi::Guid, usize)> {
    let mut at = 0usize;
    let mut found = None;
    for _ in 0..64 {
        // SAFETY: inside the device path: every node has a 4-byte header.
        let (ty, sub, len) = unsafe {
            let p = path.add(at);
            (*p, *p.add(1), u16::from_le_bytes([*p.add(2), *p.add(3)]) as usize)
        };
        if ty == 0x7F && sub == 0xFF {
            return found;
        }
        if len < 4 {
            return None;
        }
        // Media (4), HardDrive (1), with a GPT signature (type 2).
        if ty == 4 && sub == 1 && len >= 42 {
            // SAFETY: the node is `len` bytes.
            let node = unsafe { core::slice::from_raw_parts(path.add(at), len) };
            if node[41] == 2 {
                found = Some((fgpt::guid_at(node, 24), at));
            }
        }
        at += len;
    }
    None
}

/// Our disk, if it holds Freally OS's GPT layout: the disk whose GPT lists
/// the partition we were loaded from, read through its BlockIo (the
/// primary table, else the backup).
///
/// # Safety
/// Boot services are running; `bs` and `image` are the firmware's and ours.
pub unsafe fn find(bs: &BootServices, image: Handle) -> Option<Disk> {
    let mut li: *mut c_void = null_mut();
    if is_error((bs.handle_protocol)(image, &LOADED_IMAGE_GUID, &mut li)) {
        return None;
    }
    // SAFETY: the firmware returned our LoadedImage.
    let device = unsafe { (*(li as *mut LoadedImage)).device_handle };
    let mut dp: *mut c_void = null_mut();
    if is_error((bs.handle_protocol)(device, &DEVICE_PATH_GUID, &mut dp)) {
        return None;
    }
    // SAFETY: the firmware's device path for our partition.
    let (ours, before) = unsafe { our_partition(dp as *const u8) }?;
    // The disk's path: ours up to the HardDrive node, then an End node.
    let path = Pages::new(bs, before + 4, LOADER_DATA)?;
    let p = path.bytes();
    // SAFETY: `before` bytes of the firmware's path precede its HardDrive node.
    p[..before].copy_from_slice(unsafe { core::slice::from_raw_parts(dp as *const u8, before) });
    p[before..before + 4].copy_from_slice(&[0x7F, 0xFF, 4, 0]);
    let mut cursor = p.as_mut_ptr() as *mut c_void;
    let mut disk: Handle = null_mut();
    if is_error((bs.locate_device_path)(&BLOCK_IO_GUID, &mut cursor, &mut disk)) {
        return None;
    }
    let mut io: *mut c_void = null_mut();
    if is_error((bs.handle_protocol)(disk, &BLOCK_IO_GUID, &mut io)) {
        return None;
    }
    let io = io as *mut BlockIo;
    // SAFETY: the firmware's BlockIo and its media.
    let media = unsafe { &*(*io).media };
    if media.logical_partition != 0 || media.media_present == 0 || !(512..=4096).contains(&media.block_size) {
        return None;
    }
    let (block, last) = (media.block_size as u64, media.last_block);
    let mut d = Disk { io, media_id: media.media_id, block, slots: [None, None] };
    let buf = Pages::new(bs, 4096 + (fgpt::ENTRIES * fgpt::ENTRY_SIZE) as usize, LOADER_DATA)?;
    let b = buf.bytes();
    let (head, rest) = b.split_at_mut(4096);
    for lba in [1, last] {
        // SAFETY: as this function's.
        if !unsafe { read(&d, lba, &mut head[..block as usize]) } {
            continue;
        }
        let Ok(h) = fgpt::Header::parse(&head[..block as usize], lba, last) else { continue };
        let len = (h.entries_len() as u64).next_multiple_of(block) as usize;
        // SAFETY: as this function's.
        if len > rest.len() || !unsafe { read(&d, h.entries_lba, &mut rest[..len]) } {
            continue;
        }
        let Ok(t) = fgpt::Table::new(h, rest) else { continue };
        // Not the disk we came from: no GPT layout to boot.
        t.find(&ours)?;
        d.slots = t.system_slots().map(|p| p.map(|p| (p.first_lba, p.last_lba)));
        return d.slots.iter().any(Option::is_some).then_some(d);
    }
    None
}

/// Slot `s`'s image, from its partition: the FSI header's length (checked
/// against the partition and the largest image), then that many bytes into
/// `IMAGE` pages kept after the handoff. An empty partition is [`MISSING`].
///
/// # Safety
/// As [`find`]'s; `d` came from it.
pub unsafe fn read_slot(bs: &BootServices, d: &Disk, s: usize) -> Result<&'static [u8], &'static str> {
    let (first, last) = d.slots[s].ok_or(MISSING)?;
    let head = Pages::new(bs, 4096, LOADER_DATA).ok_or("There isn’t enough memory to read the system image.")?;
    let h = head.bytes();
    // SAFETY: as this function's.
    if !unsafe { read(d, first, &mut h[..4096]) } {
        return Err("The drive stopped part-way through the system image.");
    }
    if h[..8] != fsi::MAGIC {
        return Err(MISSING);
    }
    let len = u64::from_le_bytes(h[72..80].try_into().unwrap_or([0; 8]));
    let room = (last - first + 1) * d.block;
    if len == 0 || len > MAX_IMAGE || len > room {
        return Err("The system image is empty or too large.");
    }
    let image = Pages::new(bs, len as usize, fcore_abi::memtype::IMAGE)
        .ok_or("There isn’t enough memory to read the system image.")?;
    let whole = len.next_multiple_of(d.block) as usize;
    // SAFETY: as this function's; `whole` fits the pages (a block divides 4096).
    if !unsafe { read(d, first, &mut image.bytes()[..whole]) } {
        return Err("The drive stopped part-way through the system image.");
    }
    let addr = image.keep().ok_or("Freally OS made more allocations than it keeps track of.")?;
    // SAFETY: `len` bytes at `addr` were just read into pages kept until the handoff and after.
    Ok(unsafe { core::slice::from_raw_parts(addr as *const u8, len as usize) })
}
