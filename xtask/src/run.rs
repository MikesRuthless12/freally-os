// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask run`: builds a flavour, lays out the files-layout ESP folder,
//! and starts QEMU (KVM by default) with the serial ports the flavour has, a QMP
//! socket and `isa-debug-exit`. No network device: nothing can phone home.
//!
//! VM folders live in `~/.freally-vms/<name>/`, each with its own writable copy
//! of `OVMF_VARS` (made once), so UEFI variables persist per VM.

use crate::build::{self, Built};
use crate::doctor::{OVMF_CODE, OVMF_VARS};
use crate::{pack, util};
use fcore_abi::Flavour;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// The serial ports, as TCP ports on 127.0.0.1: COM1 log, COM2 Brain, COM3 CDP test door.
pub const COM1_PORT: u16 = 4551;

#[derive(Clone, Debug)]
pub struct VmOptions {
    pub flavour: Flavour,
    pub name: String,
    pub headless: bool,
    /// QEMU's TCG instead of KVM (the Docker leg, where there's no KVM).
    pub tcg: bool,
    /// Added to every port, so two VMs can run side by side.
    pub port_offset: u16,
    pub memory_mb: u32,
    pub cpus: u32,
    /// Test only: flip one bit of this byte in the VM's copy of the image, to
    /// prove the bootloader refuses it (`@boot.integrity`). Never touches the build.
    pub tamper_byte: Option<usize>,
    /// Test only: boot this image instead of the flavour's own (`@boot.seal`: a release
    /// bootloader must refuse a dev-signed image).
    pub image_override: Option<PathBuf>,
}

impl VmOptions {
    pub fn new(flavour: Flavour, name: &str) -> VmOptions {
        VmOptions {
            flavour,
            name: name.into(),
            headless: false,
            tcg: false,
            port_offset: 0,
            memory_mb: 1024,
            cpus: 2,
            tamper_byte: None,
            image_override: None,
        }
    }

    pub fn from_args(args: &[String]) -> Result<VmOptions, String> {
        let flavour = pack::flavour_arg(args)?;
        let arg = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
        let mut o = VmOptions::new(flavour, &arg("--vm").unwrap_or_else(|| flavour.name().to_string()));
        o.headless = args.iter().any(|a| a == "--headless");
        o.tcg = args.iter().any(|a| a == "--tcg") || !Path::new("/dev/kvm").exists();
        o.image_override = arg("--image").map(PathBuf::from);
        if let Some(b) = arg("--tamper") {
            o.tamper_byte = Some(b.parse().map_err(|_| "--tamper takes a byte offset")?);
        }
        if let Some(p) = arg("--port-offset") {
            o.port_offset = p.parse().map_err(|_| "--port-offset takes a number")?;
        }
        Ok(o)
    }

    pub fn dir(&self) -> PathBuf {
        util::vms_dir().join(&self.name)
    }

    pub fn qmp_socket(&self) -> PathBuf {
        self.dir().join("qmp.sock")
    }

    pub fn com1_log(&self) -> PathBuf {
        self.dir().join("com1.log")
    }

    pub fn port(&self, com: u16) -> u16 {
        COM1_PORT + (com - 1) + self.port_offset
    }
}

/// Makes the VM folder and its ESP from a build. Returns the ESP folder.
pub fn lay_out(o: &VmOptions, built: &Built) -> Result<PathBuf, String> {
    let dir = o.dir();
    let esp = dir.join("esp");
    for sub in ["EFI/BOOT", "FREALLY"] {
        std::fs::create_dir_all(esp.join(sub)).map_err(|e| e.to_string())?;
    }
    copy(&built.efi, &esp.join("EFI/BOOT/BOOTX64.EFI"))?;
    copy(&built.image, &esp.join("FREALLY/SYSTEM_A.FSI"))?;
    let vars = dir.join("OVMF_VARS_4M.fd");
    if !vars.exists() {
        copy(Path::new(OVMF_VARS), &vars)?;
    }
    Ok(esp)
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("can't copy {} to {}: {e}", from.display(), to.display()))
}

/// The QEMU command for a VM whose ESP folder is `esp`.
pub fn qemu(o: &VmOptions, esp: &Path) -> Command {
    let dir = o.dir();
    let mut c = Command::new("qemu-system-x86_64");
    c.args(["-name", &format!("freally-{}", o.name)]);
    c.args(["-machine", "q35"]);
    if o.tcg {
        c.args(["-accel", "tcg", "-cpu", "max"]);
    } else {
        c.args(["-accel", "kvm", "-cpu", "host"]);
    }
    c.args(["-m", &o.memory_mb.to_string(), "-smp", &o.cpus.to_string()]);
    c.arg("-drive").arg(format!("if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}"));
    c.arg("-drive").arg(format!("if=pflash,format=raw,unit=1,file={}", dir.join("OVMF_VARS_4M.fd").display()));
    c.arg("-drive").arg(format!("format=raw,file=fat:rw:{}", esp.display()));
    c.args(["-nic", "none"]);
    c.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    c.arg("-qmp").arg(format!("unix:{},server=on,wait=off", o.qmp_socket().display()));

    // COM1 always carries the firmware's console (and, in dev, the kernel's log),
    // mirrored to com1.log for the guard. COM2 is the Brain (dev), COM3 the test door (e2e).
    let socket = |id: &str, port: u16, log: Option<&Path>| {
        let mut s = format!("socket,id={id},host=127.0.0.1,port={port},server=on,wait=off");
        if let Some(l) = log {
            s.push_str(&format!(",logfile={}", l.display()));
        }
        s
    };
    let com1 = o.com1_log();
    let _ = std::fs::remove_file(&com1);
    c.arg("-chardev").arg(socket("com1", o.port(1), Some(&com1)));
    c.args(["-serial", "chardev:com1"]);
    match o.flavour {
        Flavour::Dev => {
            c.arg("-chardev").arg(socket("com2", o.port(2), None));
            c.args(["-serial", "chardev:com2"]);
        }
        Flavour::E2e => {
            c.args(["-serial", "null"]);
            c.arg("-chardev").arg(socket("com3", o.port(3), None));
            c.args(["-serial", "chardev:com3"]);
        }
        Flavour::Release => {}
    }
    if o.headless {
        c.args(["-display", "none"]);
    }
    c
}

/// Builds the flavour, lays out the VM and starts QEMU (not waiting for it).
pub fn start(o: &VmOptions) -> Result<Child, String> {
    let built = build::build(o.flavour)?;
    start_built(o, &built)
}

pub fn start_built(o: &VmOptions, built: &Built) -> Result<Child, String> {
    let esp = lay_out(o, built)?;
    if let Some(img) = &o.image_override {
        copy(img, &esp.join("FREALLY/SYSTEM_A.FSI"))?;
    }
    if let Some(byte) = o.tamper_byte {
        tamper(&esp.join("FREALLY/SYSTEM_A.FSI"), byte)?;
    }
    let _ = std::fs::remove_file(o.qmp_socket());
    qemu(o, &esp).spawn().map_err(|e| format!("can't start QEMU: {e}"))
}

pub fn run_cmd(args: &[String]) -> Result<(), String> {
    let o = VmOptions::from_args(args)?;
    let mut child = start(&o)?;
    println!(
        "run: {} in {} ({}); COM1 127.0.0.1:{} (log {}), QMP {}",
        o.flavour.name(),
        o.dir().display(),
        if o.tcg { "TCG" } else { "KVM" },
        o.port(1),
        o.com1_log().display(),
        o.qmp_socket().display()
    );
    let status = child.wait().map_err(|e| e.to_string())?;
    // isa-debug-exit: the guest writing v exits with (v << 1) | 1.
    match status.code() {
        Some(0) | Some(1) | None => Ok(()),
        Some(c) => Err(format!("QEMU exited with {c}")),
    }
}

/// Flips the low bit of `byte` in the file at `path` (the VM's copy only).
pub fn tamper(path: &Path, byte: usize) -> Result<(), String> {
    let mut data = std::fs::read(path).map_err(|e| e.to_string())?;
    let b = data.get_mut(byte).ok_or_else(|| format!("the image is shorter than byte {byte}"))?;
    *b ^= 1;
    std::fs::write(path, data).map_err(|e| e.to_string())
}
