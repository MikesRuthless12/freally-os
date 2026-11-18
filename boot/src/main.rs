// SPDX-License-Identifier: Apache-2.0
//! freally-boot: Freally OS's own UEFI bootloader (`\EFI\BOOT\BOOTX64.EFI`).
//!
//! It reads `\FREALLY\SYSTEM_A.FSI`, verifies all of it (the Ed25519 signature
//! over the header, then every block through the Merkle tree, then every file's
//! hash: `docs/fsi-format.md`), and only then shows *Freally OS vX.Y.Z*. A
//! changed or damaged image gets the integrity screen instead, and nothing from
//! it runs. There's no flag, variable or key that skips a check.
//!
//! Phase 0 stops at the title. Phase 1 loads Freally Core from the verified
//! image, exits boot services and jumps (TASK-018).
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
mod keys;
mod read;
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
const TRUSTED: &[[u8; 32]] = &[keys::STAGING];
#[cfg(not(feature = "flavour-release"))]
const TRUSTED: &[[u8; 32]] = &[keys::DEV, keys::STAGING];
#[cfg(feature = "flavour-release")]
const KEY_NAMES: &[&str] = &["staging"];
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
    Verified { version: [u8; 32], note: &'static str },
    Refused(&'static str),
}

/// Checks the image the way `docs/fsi-format.md` says, plus the flavour rule.
fn check(image: &[u8]) -> Outcome {
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
            Outcome::Verified { version: v.header.version, note }
        }
        Err(e) => {
            say!("freally-boot: ERROR image refused: {} ({:?})", e.describe(), e);
            Outcome::Refused(e.describe())
        }
    }
}

#[unsafe(export_name = "efi_main")]
extern "efiapi" fn efi_main(image: Handle, st: *mut SystemTable) -> Status {
    let mut stamps = BootStamps { b0: rdtsc(), ..BootStamps::default() };
    // SAFETY: the firmware passes a valid system table and keeps it valid while
    // boot services run, which is all of this bootloader's life in Phase 0.
    let st = unsafe { &*st };
    let bs = unsafe { &*st.boot_services };
    console::init(st.con_out);
    // The firmware's watchdog would reset a machine that stays on our screen.
    (bs.set_watchdog_timer)(0, 0, 0, null_mut());
    say!("freally-boot {} ({})", fcore_abi::VERSION, FLAVOUR.name());

    // The display, the EDID and the memory map (the kernel's, from Phase 1).
    let display = unsafe { video::gop(bs) };
    if let Some((_, d)) = display {
        say!("freally-boot: display {}x{}, stride {}, pixel format {}", d.w, d.h, d.stride, d.format);
    }
    let mut edid: *mut c_void = null_mut();
    if !is_error((bs.locate_protocol)(&EDID_ACTIVE_GUID, null_mut(), &mut edid)) && !edid.is_null() {
        // SAFETY: the firmware's EDID Active protocol.
        say!("freally-boot: EDID {} bytes", unsafe { (*(edid as *mut EdidActive)).size_of_edid });
    }
    let (mut map_size, mut key, mut desc_size, mut desc_version) = (0usize, 0usize, 0usize, 0u32);
    let st_map = (bs.get_memory_map)(&mut map_size, null_mut(), &mut key, &mut desc_size, &mut desc_version);
    if st_map == BUFFER_TOO_SMALL && desc_size > 0 {
        say!("freally-boot: memory map {} regions", map_size / desc_size);
    }

    // Read, then verify.
    // SAFETY: boot services are running and `image` is our own handle.
    let outcome = match unsafe { read::read_image(bs, image, read::SYSTEM_A) } {
        Ok(bytes) => {
            stamps.b1 = rdtsc();
            let hdr = fsi::Header::parse(bytes).map(|h| h.version);
            say!(
                "freally-boot: read SYSTEM_A.FSI, {} bytes, header version {}",
                bytes.len(),
                hdr.as_ref().map_or("?", |v| core::str::from_utf8(&v[..v.iter().position(|&b| b == 0).unwrap_or(32)]).unwrap_or("?"))
            );
            let o = check(bytes);
            stamps.b2 = rdtsc();
            o
        }
        Err(why) => {
            say!("freally-boot: ERROR {why}");
            Outcome::Refused(why)
        }
    };
    say!(
        "freally-boot: read {} ticks, verified {} ticks",
        stamps.b1.saturating_sub(stamps.b0),
        stamps.b2.saturating_sub(stamps.b1)
    );

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
        Outcome::Verified { version, note } => {
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
    }
    // SAFETY: `gop` and `d` come from the firmware's current mode.
    unsafe { video::present(gop, d, canvas.px) };
    halt();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    say!("freally-boot: ERROR panic: {info}");
    halt();
}
