// SPDX-License-Identifier: Apache-2.0
//! freally-boot: Freally OS's own UEFI bootloader (`\EFI\BOOT\BOOTX64.EFI`).
//!
//! It reads `\FREALLY\SYSTEM_A.FSI`, verifies all of it (the Ed25519 signature
//! over the header, then every block through the Merkle tree, then every file's
//! hash: `docs/fsi-format.md`), and only then shows *Freally OS vX.Y.Z*. A
//! changed or damaged image gets the integrity screen instead, and nothing from
//! it runs. There's no flag, variable or key that skips a check.
//!
//! Then it loads Freally Core from the verified image, exits boot services and
//! jumps to it (TASK-018, `handoff.rs`).
#![no_std]
#![no_main]

#[cfg(not(any(feature = "flavour-release", feature = "flavour-dev", feature = "flavour-e2e")))]
compile_error!("build the bootloader through `cargo xtask build --flavour release|dev|e2e`");
#[cfg(any(
    all(feature = "flavour-release", feature = "flavour-dev"),
    all(feature = "flavour-release", feature = "flavour-e2e"),
    all(feature = "flavour-dev", feature = "flavour-e2e"),
))]
compile_error!("exactly one flavour");

#[macro_use]
mod console;
mod gpt;
mod handoff;
mod keys;
mod read;
mod slots;
mod uefi;
mod video;

use core::ffi::c_void;
use core::ptr::null_mut;
use fcore_abi::{BootStamps, Flavour};
use uefi::*;

#[cfg(feature = "flavour-release")]
const FLAVOUR: Flavour = Flavour::Release;
#[cfg(feature = "flavour-dev")]
const FLAVOUR: Flavour = Flavour::Dev;
#[cfg(feature = "flavour-e2e")]
const FLAVOUR: Flavour = Flavour::E2e;

/// The keys this bootloader trusts. A release bootloader trusts only the staging
/// key until `v0.8.0`, then only the release key; dev and e2e trust dev and staging.
#[cfg(feature = "flavour-release")]
const RELEASE_TRUST: [[u8; 32]; 1] = [match keys::RELEASE {
    Some(k) => k,
    None => keys::STAGING,
}];
#[cfg(feature = "flavour-release")]
const TRUSTED: &[[u8; 32]] = &RELEASE_TRUST;
#[cfg(not(feature = "flavour-release"))]
const TRUSTED: &[[u8; 32]] = &[keys::DEV, keys::STAGING];
#[cfg(feature = "flavour-release")]
const KEY_NAMES: &[&str] = &[if keys::RELEASE.is_some() { "release" } else { "staging" }];
#[cfg(not(feature = "flavour-release"))]
const KEY_NAMES: &[&str] = &["dev", "staging"];

fn rdtsc() -> u64 {
    // SAFETY: RDTSC has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn halt() -> ! {
    loop {
        // SAFETY: waits for the next interrupt; the screen stays as painted.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

/// What the screen will say.
enum Outcome {
    Verified {
        version: [u8; 32],
        note: &'static str,
        image: &'static [u8],
        kernel: Option<&'static [u8]>,
    },
    Refused(&'static str),
    /// Neither slot can start (FR-003): the panic screen, with both slots' states.
    NoSlot,
}

/// Checks the image the way `docs/fsi-format.md` says, plus the flavour rule.
fn check(image: &'static [u8]) -> Outcome {
    match fsi::verify(image, TRUSTED) {
        Ok(v) => {
            if FLAVOUR == Flavour::Release && v.header.flavour != Flavour::Release as u16 {
                return Outcome::Refused("This is a development image; this computer runs only release images.");
            }
            say!(
                "freally-boot: verified Freally OS {} ({}), signed by the {} key, {} files",
                v.header.version_str(),
                Flavour::from_u32(v.header.flavour as u32).map_or("?", |f| f.name()),
                KEY_NAMES[v.key_index],
                v.toc().count()
            );
            let note = match KEY_NAMES[v.key_index] {
                "dev" => "Every block checked. Signed by the dev key.",
                _ => "Every block checked. Signed by the staging key.",
            };
            Outcome::Verified { version: v.header.version, note, image, kernel: v.file("/system/kernel") }
        }
        Err(e) => {
            say!("freally-boot: ERROR image refused: {} ({:?})", e.describe(), e);
            Outcome::Refused(e.describe())
        }
    }
}

#[unsafe(export_name = "efi_main")]
extern "efiapi" fn efi_main(image_handle: Handle, st: *mut SystemTable) -> Status {
    let mut stamps = BootStamps { b0: rdtsc(), ..BootStamps::default() };
    // SAFETY: the firmware passes a valid system table and keeps it valid while
    // boot services run, which is all of this bootloader's life in Phase 0.
    let st = unsafe { &*st };
    let bs = unsafe { &*st.boot_services };
    console::init(st.con_out);
    // The firmware's watchdog would reset a machine that stays on our screen.
    (bs.set_watchdog_timer)(0, 0, 0, null_mut());
    say!("freally-boot {} ({})", fcore_abi::VERSION, FLAVOUR.name());

    // The EDID, the display in its own mode (TASK-071), and the memory map
    // (the kernel's, from Phase 1).
    let mut edid: *mut c_void = null_mut();
    let mut edid_bytes: Option<(*const u8, usize)> = None;
    if !is_error((bs.locate_protocol)(&EDID_ACTIVE_GUID, null_mut(), &mut edid)) && !edid.is_null() {
        // SAFETY: the firmware's EDID Active protocol.
        let e = unsafe { &*(edid as *mut EdidActive) };
        say!("freally-boot: EDID {} bytes", e.size_of_edid);
        if !e.edid.is_null() && e.size_of_edid > 0 {
            edid_bytes = Some((e.edid as *const u8, e.size_of_edid as usize));
        }
    }
    // Dev and e2e only, compiled out of release: a VM's firmware gives no
    // EDID, so a stand-in on the drive lets the specs prove the native mode
    // and the scale (TASK-071).
    #[cfg(not(feature = "flavour-release"))]
    if edid_bytes.is_none()
        // SAFETY: boot services are running; `image_handle` is ours.
        && let Ok(b) = unsafe { read::read_image(bs, image_handle, read::TEST_EDID) }
        && (128..=512).contains(&b.len())
    {
        say!("freally-boot: no EDID from the firmware; using the stand-in EDID.BIN ({} bytes)", b.len());
        edid_bytes = Some((b.as_ptr(), b.len()));
    }
    // Boot options (dev and e2e only, compiled out of release: TASK-074).
    #[allow(unused_mut)]
    let mut cmdline = [0u8; 256];
    #[cfg(not(feature = "flavour-release"))]
    // SAFETY: boot services are running; `image_handle` is ours.
    if let Ok(b) = unsafe { read::read_image(bs, image_handle, read::CMDLINE) } {
        cmdline = fcore_abi::cmdline::from_text(b);
        let n = cmdline.iter().position(|&c| c == 0).unwrap_or(256);
        say!("freally-boot: boot options: {}", core::str::from_utf8(&cmdline[..n]).unwrap_or("?"));
    }
    // SAFETY: the firmware's EDID bytes, `n` of them, while boot services run.
    let preferred = edid_bytes
        .and_then(|(p, n)| fedid::parse(unsafe { core::slice::from_raw_parts(p, n) }).ok())
        .and_then(|e| e.preferred)
        .map(|m| (m.w, m.h));
    let mut gop_modes = [0u32; fcore_abi::GOP_MODES_MAX];
    if let Some((g, _)) = unsafe { video::gop(bs) } {
        // SAFETY: boot services are running; `g` is the firmware's GOP.
        gop_modes = unsafe { video::native_mode(bs, g, preferred) };
    }
    let display = unsafe { video::gop(bs) };
    if let Some((_, d)) = display {
        say!("freally-boot: display {}x{}, stride {}, pixel format {}", d.w, d.h, d.stride, d.format);
    }
    let (mut map_size, mut key, mut desc_size, mut desc_version) = (0usize, 0usize, 0usize, 0u32);
    let st_map = (bs.get_memory_map)(&mut map_size, null_mut(), &mut key, &mut desc_size, &mut desc_version);
    if st_map == BUFFER_TOO_SMALL && desc_size > 0 {
        say!("freally-boot: memory map {} regions", map_size / desc_size);
    }
    // The ACPI 2.0+ RSDP, for the kernel (BootInfo.acpi_rsdp, Phase 1).
    // SAFETY: the firmware's configuration table has `number_of_table_entries` entries.
    let tables = unsafe { core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries) };
    let mut acpi_rsdp = 0u64;
    if let Some(t) = tables.iter().find(|t| t.vendor_guid == ACPI_20_TABLE_GUID) {
        acpi_rsdp = t.vendor_table as u64;
        say!("freally-boot: ACPI 2.0 RSDP at {:#x}", acpi_rsdp);
    }

    // Which slot (A/B, TASK-045), then read and verify its image.
    // SAFETY: boot services are running and `image` is our own handle.
    let chosen = unsafe { slots::choose(st, bs, image_handle) };
    let (slot, mut ctl) = match &chosen {
        Ok(c) => (c.slot, c.ctl),
        Err(ctl) => (0, *ctl),
    };
    let outcome = match chosen.map(|c| c.image) {
        Err(_) => Outcome::NoSlot,
        Ok(Ok(bytes)) => {
            stamps.b1 = rdtsc();
            let hdr = fsi::Header::parse(bytes).map(|h| h.version);
            say!(
                "freally-boot: read SYSTEM_{}.FSI, {} bytes, header version {}",
                fbootctl::slot_name(slot),
                bytes.len(),
                hdr.as_ref().map_or("?", |v| core::str::from_utf8(&v[..v.iter().position(|&b| b == 0).unwrap_or(32)])
                    .unwrap_or("?"))
            );
            let o = check(bytes);
            stamps.b2 = rdtsc();
            o
        }
        Ok(Err(why)) => {
            say!("freally-boot: ERROR {why}");
            Outcome::Refused(why)
        }
    };
    // A rebuilt record learns the version slot A holds, once it's verified.
    if let Outcome::Verified { version, .. } = &outcome
        && ctl.slot[slot as usize].version() == ""
    {
        let end = version.iter().position(|&b| b == 0).unwrap_or(32);
        ctl.slot[slot as usize].version = fbootctl::version_bytes(core::str::from_utf8(&version[..end]).unwrap_or(""));
        // SAFETY: boot services are running.
        unsafe { slots::save(st.runtime_services, &ctl) };
    }
    say!(
        "freally-boot: read {} ticks, verified {} ticks",
        stamps.b1.saturating_sub(stamps.b0),
        stamps.b2.saturating_sub(stamps.b1)
    );

    // Load Freally Core from the verified image (images before Phase 1 have none).
    let mut outcome = outcome;
    let mut kernel = None;
    if let Outcome::Verified { kernel: Some(elf), .. } = &outcome {
        match handoff::load_kernel(bs, elf) {
            Ok(k) => {
                say!("freally-boot: Freally Core loaded, {} bytes, handing over", elf.len());
                kernel = Some(k);
            }
            Err(why) => {
                say!("freally-boot: ERROR {why}");
                outcome = Outcome::Refused(why);
            }
        }
    }

    // Paint. Nothing is printed after this, or the console would draw over it.
    let Some((gop, d)) = display else {
        say!("freally-boot: ERROR no graphics output; the console is all there is");
        halt();
    };
    let pages = (d.w * d.h * 4).div_ceil(4096);
    let mut addr = 0u64;
    if is_error((bs.allocate_pages)(ALLOCATE_ANY_PAGES, LOADER_DATA, pages, &mut addr)) {
        say!("freally-boot: ERROR no memory for the screen");
        halt();
    }
    // SAFETY: `pages` pages at `addr` are ours, enough for d.w × d.h u32 pixels.
    let px = unsafe { core::slice::from_raw_parts_mut(addr as *mut u32, d.w * d.h) };
    let mut canvas = fscreen::Canvas { px, w: d.w, h: d.h };
    match &outcome {
        Outcome::Verified { version, note, .. } => {
            let end = version.iter().position(|&b| b == 0).unwrap_or(32);
            let v = core::str::from_utf8(&version[..end]).unwrap_or("?");
            say!("freally-boot: showing the title");
            console::hide_cursor();
            fscreen::title(&mut canvas, v, note);
        }
        Outcome::Refused(why) => {
            say!("freally-boot: showing the integrity screen");
            console::hide_cursor();
            fscreen::integrity(&mut canvas, why);
        }
        Outcome::NoSlot => {
            say!("freally-boot: showing the panic screen: neither slot can start");
            console::hide_cursor();
            let states = slots::states(&ctl);
            fscreen::panic(
                &mut canvas,
                &[
                    "Neither copy of Freally OS on this drive can start.",
                    states.as_str(),
                    "Install Freally OS again from a USB stick to carry on.",
                ],
            );
        }
    }
    // SAFETY: `gop` and `d` come from the firmware's current mode.
    unsafe { video::present(gop, d, canvas.px) };

    // Hand over (TASK-018). It returns only if something failed before ExitBootServices.
    if let (Some(k), Outcome::Verified { version, image, .. }) = (kernel, &outcome) {
        let facts = handoff::Facts {
            framebuffer: handoff::framebuffer(&d, d.fb_base, d.fb_size),
            edid: edid_bytes,
            acpi_rsdp,
            image,
            version: *version,
            stamps,
            slot,
            bootctl: ctl.encode(),
            gop_modes,
            cmdline,
            // SAFETY: the system table's own vendor string.
            firmware_vendor: unsafe { handoff::vendor_ascii(st.firmware_vendor) },
        };
        // SAFETY: boot services are running; `st` and `image` are the firmware's.
        let why = unsafe { handoff::handoff(st, image_handle, &k, facts) };
        fscreen::integrity(&mut canvas, why);
        // SAFETY: as above.
        unsafe { video::present(gop, d, canvas.px) };
    }
    halt();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    say!("freally-boot: ERROR panic: {info}");
    halt();
}
