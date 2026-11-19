// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask tester-image [--out P] [--check]` (TASK-114, FR-052): the
//! release build on Freally OS's GPT layout, FREALLY_DATA at its smallest
//! (64 MiB), sparse, for testers' virtual machines. A tester makes the disk
//! as big as they like; at its first boot drv-nvme grows FREALLY_DATA to fill
//! it, ffs formats it, and the Welcome screen asks for the first account.
//!
//! `--check` proves that first boot (`@tester.first-boot`): a copy grown to
//! 4 GiB boots in QEMU (a release build says nothing on COM1 past the
//! bootloader, so the screen is kept as the picture of it), stops, and the
//! disk is read back here: the table grown to fill it, FreallyFS formatted
//! at the partition's whole size, ffsck clean.
//!
//! Signed as every release image is today: with the staging key until the
//! key ceremony (TASK-109), then by the release pipeline with the release key.

use crate::image::{self, Disk};
use crate::run::VmOptions;
use crate::{build, util};
use cdp_proto::qmp::Qmp;
use fcore_abi::Flavour;
use fjson::Value;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// FREALLY_DATA's size as shipped: the layout's smallest.
const DATA_MIB: u64 = 64;
/// The most the image file may take on disk (it's sparse).
const MOST: u64 = 1 << 30;
/// The disk `--check` gives the first boot.
const CHECK_GIB: u64 = 4;

pub fn tester_image_cmd(args: &[String]) -> Result<(), String> {
    let out = util::flag(args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| util::target_dir().join(format!("Freally-OS-v{}-tester.img", fcore_abi::VERSION)));
    let built = build::build(Flavour::Release)?;
    let efi = std::fs::read(&built.efi).map_err(|e| e.to_string())?;
    let a = std::fs::read(&built.image).map_err(|e| e.to_string())?;
    image::write_disk(&out, &Disk { efi: &efi, slot_a: &a, slot_b: None, esp_files: Vec::new(), data_mib: DATA_MIB })?;
    let meta = std::fs::metadata(&out).map_err(|e| e.to_string())?;
    let taken = meta.blocks() * 512;
    println!(
        "tester-image: {} ({} MiB long, {} MiB on disk: sparse; release build, staging-signed)",
        out.display(),
        meta.len() >> 20,
        taken >> 20
    );
    if taken > MOST {
        return Err(format!("the tester image takes {} MiB on disk, over 1 GiB", taken >> 20));
    }
    if args.iter().any(|a| a == "--check") {
        check(&out, &built)?;
    }
    Ok(())
}

/// Waits (`within`) for `line` in the VM's COM1 log.
fn wait_line(log: &Path, line: &str, within: Duration) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if std::fs::read(log).is_ok_and(|b| String::from_utf8_lossy(&b).contains(line)) {
            return Ok(());
        }
        if start.elapsed() > within {
            return Err(format!("COM1 never said {line:?}"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `@tester.first-boot`: a copy grown to 4 GiB, booted, read back.
fn check(img: &Path, built: &build::Built) -> Result<(), String> {
    let mut o = VmOptions::new(Flavour::Release, "tester-first-boot");
    o.gpt = true;
    o.keep_disk = true;
    std::fs::create_dir_all(o.dir()).map_err(|e| e.to_string())?;
    let disk = o.dir().join("disk.img");
    std::fs::copy(img, &disk).map_err(|e| format!("{}: {e}", disk.display()))?;
    let f = std::fs::OpenOptions::new().write(true).open(&disk).map_err(|e| e.to_string())?;
    f.set_len(CHECK_GIB << 30).map_err(|e| e.to_string())?;
    drop(f);
    let _ = std::fs::remove_file(o.com1_log());
    // Fresh firmware variables, as a tester's new VM has: QEMU's firmware
    // can miss the disk then, and is started again (as a tester would).
    let mut vm = crate::run::start_booted(&o, built)?;
    let shot = util::target_dir().join("tester-first-boot.png");
    let result = (|| {
        wait_line(&o.com1_log(), "booting slot A", Duration::from_secs(60))?;
        // The services start, ffs formats the grown partition, Welcome draws.
        std::thread::sleep(Duration::from_secs(25));
        let mut q = Qmp::connect(&o.qmp_socket(), Duration::from_secs(20))?;
        let ppm = o.dir().join("first-boot.ppm");
        q.execute("screendump", Some(Value::obj([("filename", ppm.display().to_string().into())])))?;
        let data = std::fs::read(&ppm).map_err(|e| format!("screendump: {e}"))?;
        let (w, h, rgb) = fimage::ppm::parse(&data)?;
        std::fs::write(&shot, fimage::png::encode(w, h, 3, rgb)).map_err(|e| e.to_string())?;
        let log = String::from_utf8_lossy(&std::fs::read(o.com1_log()).unwrap_or_default()).to_string();
        if log.contains("panic") || log.contains("refused") {
            return Err(format!("the first boot went wrong:\n{log}"));
        }
        // Shut down cleanly is the OS's; here the VM just stops, as a power cut would.
        let _ = q.execute("quit", None);
        Ok(())
    })();
    let _ = vm.kill();
    let _ = vm.wait();
    result?;
    let (data_last, mib) = crate::carry::grown_data(&disk)?;
    println!(
        "tester-image: first boot green: FREALLY_DATA grown to block {data_last} ({mib} MiB), FreallyFS whole and clean; the screen: {}",
        shot.display()
    );
    Ok(())
}
