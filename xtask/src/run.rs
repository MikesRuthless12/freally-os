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
/// The host's TLS test server, which QEMU's 10.0.2.100:443 reaches (TASK-180).
pub const TLS_PORT: u16 = 4560;

#[derive(Clone, Debug)]
pub struct VmOptions {
    pub flavour: Flavour,
    pub name: String,
    pub headless: bool,
    /// QEMU's TCG instead of KVM (the Docker leg, where there's no KVM).
    pub tcg: bool,
    /// Added to every port, so two VMs can run side by side.
    pub port_offset: u16,
    /// The NIC on a test network of its own (TASK-176): a TAP device, and a
    /// DHCP and DNS server on it (`e2e/os/net/testnet.py`), not QEMU's.
    pub testnet: bool,
    /// The test network gives DNS over IPv6 alone (RDNSS, TASK-176): its own
    /// tap and addresses, the lease naming no DNS server.
    pub testnet6: bool,
    /// What the test update host serves (`fup::online_www`), for the CDP runner.
    pub www: Option<PathBuf>,
    pub memory_mb: u32,
    pub cpus: u32,
    /// Test only: flip one bit of this byte in the VM's copy of the image, to
    /// prove the bootloader refuses it (`@boot.integrity`). Never touches the build.
    pub tamper_byte: Option<usize>,
    /// Test only: boot this image instead of the flavour's own (`@boot.seal`: a release
    /// bootloader must refuse a dev-signed image).
    pub image_override: Option<PathBuf>,
    /// Test only: no PS/2 controller (`@input.absent`).
    pub no_ps2: bool,
    /// Slot B's image (`@boot.fallback`, `@boot.trial`); without one, the ESP has none.
    pub slot_b: Option<PathBuf>,
    /// The probe image (TASK-069) instead of the flavour's own.
    pub probe: bool,
    /// A stand-in display EDID on the ESP (`\FREALLY\EDID.BIN`): a VM's
    /// firmware gives none (TASK-071; dev and e2e bootloaders read it).
    pub edid: Option<Vec<u8>>,
    /// Boot options on the ESP (`\FREALLY\CMDLINE.TXT`, dev and e2e only:
    /// `log=screen`, TASK-074).
    pub cmdline: Option<String>,
    /// The GPT layout (TASK-091): the ESP folder made into a whole disk
    /// (`disk.img`: ESP, FREALLY_SYSTEM_A and _B, FREALLY_DATA) on NVMe,
    /// instead of the files layout's virtual FAT drive.
    pub gpt: bool,
    /// The GPT disk kept as the last start left it (the crash test's cold
    /// restarts), instead of made again from the ESP folder.
    pub keep_disk: bool,
    /// An account carried in before the first boot (TASK-106): its bundle
    /// taken from this disk (another VM's), for this user.
    pub carry: Option<(PathBuf, String)>,
    /// The update packages `@update.*` use, put in /system/updates (TASK-110).
    pub updates: bool,
    /// A network card of this QEMU model (`virtio-net-pci`, `e1000`, `e1000e`)
    /// on QEMU's user network (TASK-172), else none. `--nic <model>`.
    pub nic: Option<String>,
    /// QEMU's CPU model instead of `host` (KVM): `host,-tsc-deadline` is a
    /// CPU without TSC-deadline, as VirtualBox shows a guest (TASK-113).
    pub cpu: Option<String>,
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
            no_ps2: false,
            slot_b: None,
            probe: false,
            edid: None,
            cmdline: None,
            gpt: false,
            keep_disk: false,
            carry: None,
            updates: false,
            nic: None,
            testnet: false,
            testnet6: false,
            www: None,
            cpu: None,
        }
    }

    pub fn from_args(args: &[String]) -> Result<VmOptions, String> {
        let flavour = pack::flavour_arg(args)?;
        let arg = |n: &str| crate::util::flag(args, n).map(String::from);
        let mut o = VmOptions::new(flavour, &arg("--vm").unwrap_or_else(|| flavour.name().to_string()));
        o.headless = args.iter().any(|a| a == "--headless");
        o.probe = args.iter().any(|a| a == "--probe");
        o.gpt = args.iter().any(|a| a == "--gpt");
        o.tcg = args.iter().any(|a| a == "--tcg") || !Path::new("/dev/kvm").exists();
        o.image_override = arg("--image").map(PathBuf::from);
        o.nic = arg("--nic");
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

    /// The firmware's variables (made once from OVMF_VARS, then kept).
    pub fn vars(&self) -> PathBuf {
        self.dir().join("OVMF_VARS_4M.fd")
    }

    /// A copy of the variables a good boot left, started from later.
    pub fn settled_vars(&self) -> PathBuf {
        self.dir().join("OVMF_VARS_4M.settled")
    }

    pub fn com1_log(&self) -> PathBuf {
        self.dir().join("com1.log")
    }

    pub fn port(&self, com: u16) -> u16 {
        COM1_PORT + (com - 1) + self.port_offset
    }

    /// The test network's TAP device and its /24 (`frt2`, `10.77.2`), by the port offset.
    pub fn testnet_names(&self) -> (String, String) {
        let n = self.port_offset / 100;
        if self.testnet6 {
            (format!("fr6t{n}"), format!("10.78.{n}"))
        } else {
            (format!("frt{n}"), format!("10.77.{n}"))
        }
    }

    /// The port a spec's TLS server listens on, for this VM (TASK-180).
    pub fn tls_port(&self) -> u16 {
        TLS_PORT + self.port_offset
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
    // Slot B only when asked for: a leftover would boot as soon as the record said so.
    match &o.slot_b {
        Some(b) => copy(b, &esp.join("FREALLY/SYSTEM_B.FSI"))?,
        None => {
            let _ = std::fs::remove_file(esp.join("FREALLY/SYSTEM_B.FSI"));
        }
    }
    // The stand-in EDID only when asked for (a leftover would change the mode).
    match &o.edid {
        Some(e) => std::fs::write(esp.join("FREALLY/EDID.BIN"), e).map_err(|e| e.to_string())?,
        None => {
            let _ = std::fs::remove_file(esp.join("FREALLY/EDID.BIN"));
        }
    }
    match &o.cmdline {
        Some(c) => std::fs::write(esp.join("FREALLY/CMDLINE.TXT"), c).map_err(|e| e.to_string())?,
        None => {
            let _ = std::fs::remove_file(esp.join("FREALLY/CMDLINE.TXT"));
        }
    }
    let vars = o.vars();
    // One stocked with updates (`@update.*` write slot B and its record
    // themselves) starts from the variables its first good boot left (slot A
    // good, B empty, the firmware's boot entry made): fresh ones would do,
    // but QEMU's firmware misses an NVMe disk far more often with them.
    let settled = o.settled_vars();
    if o.updates && settled.exists() {
        copy(&settled, &vars)?;
    // A VM with a slot B starts from fresh firmware variables: its A/B record
    // is rebuilt on the first boot, so no earlier run decides how this one
    // starts. So does one whose store is filling (`vars_full`).
    } else if !vars.exists() || o.slot_b.is_some() || o.updates || std::fs::read(&vars).is_ok_and(|b| vars_full(&b)) {
        copy(Path::new(OVMF_VARS), &vars)?;
    }
    Ok(esp)
}

/// An update VM's variables kept once its first boot is good: conductor up
/// from slot A, the record as a first boot makes it.
fn settle(o: &VmOptions) {
    if !o.updates || o.settled_vars().exists() {
        return;
    }
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(60) {
        let log = std::fs::read(o.com1_log()).unwrap_or_default();
        if String::from_utf8_lossy(&log).contains("conductor: started from slot A (Good") {
            let _ = std::fs::copy(o.vars(), o.settled_vars());
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Whether OVMF's variable store (its first 256 KiB; unwritten flash is
/// 0xFF) is more than half written. OVMF rewrites its console variables on
/// every boot, and with the store near full (89 % once) a warm reset hung
/// before its boot entry (TASK-099's crash test, after about 190 boots).
fn vars_full(fd: &[u8]) -> bool {
    const STORE: usize = 0x40000;
    let store = &fd[..fd.len().min(STORE)];
    store.iter().rposition(|&b| b != 0xFF).is_some_and(|end| end > STORE / 2)
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("can't copy {} to {}: {e}", from.display(), to.display()))
}

/// The QEMU command for a VM whose ESP folder is `esp`.
pub fn qemu(o: &VmOptions, esp: &Path) -> Command {
    let dir = o.dir();
    let mut c = Command::new("qemu-system-x86_64");
    c.args(["-name", &format!("freally-{}", o.name)]);
    c.args(["-machine", if o.no_ps2 { "q35,i8042=off" } else { "q35" }]);
    if o.tcg {
        c.args(["-accel", "tcg", "-cpu", "max"]);
    } else {
        c.args(["-accel", "kvm", "-cpu", o.cpu.as_deref().unwrap_or("host")]);
    }
    c.args(["-m", &o.memory_mb.to_string(), "-smp", &o.cpus.to_string()]);
    // The RTC on local time, as Windows keeps it on the OMEN: the Rack's clock
    // reads the RTC as the time (time zones come with Settings).
    c.args(["-rtc", "base=localtime"]);
    c.arg("-drive").arg(format!("if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}"));
    c.arg("-drive").arg(format!("if=pflash,format=raw,unit=1,file={}", o.vars().display()));
    if o.gpt {
        // The whole disk on NVMe (TASK-091), as a laptop's SSD.
        c.arg("-drive").arg(format!("if=none,id=disk0,format=raw,file={}", dir.join("disk.img").display()));
        c.args(["-device", "nvme,serial=FREALLY0,drive=disk0"]);
    } else {
        // Read-only: the bootloader never writes the ESP, and a writable virtual FAT
        // disk stalled a fresh VM's firmware on its first boot.
        c.arg("-drive").arg(format!("if=virtio,format=raw,readonly=on,file=fat:{}", esp.display()));
    }
    // A network card on QEMU's user network, restricted: the guest reaches
    // QEMU's own gateway, DHCP and DNS, never the host's network or beyond.
    // Two ways out, for the tests: 10.0.2.100:7, the echo port, is handed to
    // `cat` on this machine (TCP, TASK-177), and 10.0.2.100:443 to the TLS
    // test server a spec starts on this machine's `tls_port` (TASK-180).
    match &o.nic {
        Some(model) if o.testnet => {
            let (tap, _) = o.testnet_names();
            c.args(["-netdev", &format!("tap,id=n0,ifname={tap},script=no,downscript=no")]);
            c.arg("-device").arg(format!("{model},netdev=n0"));
        }
        Some(model) => {
            let tls = format!("guestfwd=tcp:10.0.2.100:443-cmd:nc -N 127.0.0.1 {}", o.tls_port());
            c.args(["-netdev", &format!("user,id=n0,restrict=on,guestfwd=tcp:10.0.2.100:7-cmd:cat,{tls}")]);
            c.arg("-device").arg(format!("{model},netdev=n0"));
        }
        None => {
            c.args(["-nic", "none"]);
        }
    }
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
        // Nothing reads QEMU's terminal in a headless run: no stdin (an
        // inherited terminal could stall a fresh VM's firmware). Its monitor
        // is a socket beside QMP's (which cdp-host holds): `info registers -a`
        // there shows where each CPU is when a run stalls.
        c.args(["-display", "none"]);
        c.arg("-monitor").arg(format!("unix:{},server=on,wait=off", o.dir().join("hmp.sock").display()));
        c.stdin(std::process::Stdio::null());
    }
    c
}

/// Builds the flavour, lays out the VM and starts QEMU (not waiting for it).
pub fn start(o: &VmOptions) -> Result<Child, String> {
    let built = if o.probe { build::build_probe()? } else { build::build(o.flavour)? };
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
    if o.gpt && !(o.keep_disk && o.dir().join("disk.img").exists()) {
        crate::image::disk_from_esp(&esp, &o.dir().join("disk.img"), crate::image::DATA_MIB)?;
        if let Some((from, user)) = &o.carry {
            crate::carry::carry(from, user, &o.dir().join("disk.img"))?;
        }
        if o.updates {
            crate::fup::stock(&o.dir().join("disk.img"), o.flavour)?;
        }
    }
    let _ = std::fs::remove_file(o.qmp_socket());
    if o.testnet {
        testnet_up(o)?;
    }
    qemu(o, &esp).spawn().map_err(|e| format!("can't start QEMU: {e}"))
}

/// The VM's test network (TASK-176): its TAP device, made once with the
/// host's address on it, and the DHCP and DNS server, started once and left
/// for the leg's later VMs on the same device (it ends with xtask).
fn testnet_up(o: &VmOptions) -> Result<(), String> {
    let (tap, prefix) = o.testnet_names();
    if !Path::new(&format!("/sys/class/net/{tap}")).exists() {
        let made = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "ip tuntap add dev {tap} mode tap && ip addr add {prefix}.1/24 dev {tap} && ip link set {tap} up{}",
                    // IPv6's DNS server on the tap (no DAD: nothing else is on it).
                    if o.testnet6 {
                        format!(" && ip -6 addr add fd78:{}::1/64 dev {tap} nodad", o.port_offset / 100)
                    } else {
                        String::new()
                    }
                ),
            ])
            .status()
            .map_err(|e| e.to_string())?;
        if !made.success() {
            return Err(format!("can't make the test network's {tap} (root, /dev/net/tun?)"));
        }
    }
    let server: std::net::SocketAddr = format!("{prefix}.1:53").parse().map_err(|_| "a bad prefix")?;
    let up = || std::net::TcpStream::connect_timeout(&server, std::time::Duration::from_millis(200)).is_ok();
    if up() {
        return Ok(());
    }
    Command::new("python3")
        .arg(util::repo_root().join("e2e/os/net/testnet.py"))
        .args([&tap, &prefix])
        .args(o.testnet6.then_some("--v6"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("can't start the test network's server: {e}"))?;
    let start = std::time::Instant::now();
    while !up() {
        if start.elapsed() > std::time::Duration::from_secs(10) {
            return Err(format!("the test network's server never listened on {server}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

/// As [`start_built`], and for a GPT disk on NVMe waits for the bootloader's
/// first line: OVMF (QEMU 10.2) misses an NVMe disk on some cold starts, more
/// with fresh variables ("No bootable option", before any of our code runs;
/// TASK-092A), so such a start is made again, up to three times, and said.
pub fn start_booted(o: &VmOptions, built: &Built) -> Result<Child, String> {
    if !o.gpt {
        return start_built(o, built);
    }
    let mut o = o.clone();
    for attempt in 1..=3 {
        let mut child = start_built(&o, built)?;
        let start = std::time::Instant::now();
        loop {
            let log = std::fs::read(o.com1_log()).unwrap_or_default();
            let text = String::from_utf8_lossy(&log);
            if text.contains("freally-boot ") {
                settle(&o);
                return Ok(child);
            }
            if text.contains("BdsDxe: No bootable option") || start.elapsed() > std::time::Duration::from_secs(45) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        println!("run: the firmware didn't find the NVMe disk (OVMF; TASK-092A): QEMU again ({attempt} of 3)");
        o.keep_disk = true;
    }
    Err(String::from("the firmware didn't find the NVMe disk on three cold starts"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_firmware_store_past_half_written_is_full() {
        let mut fd = vec![0xFFu8; 540_672];
        fd[..100].fill(0x11);
        assert!(!vars_full(&fd), "a fresh store");
        fd[..0x40000 * 2 / 5].fill(0x22);
        assert!(!vars_full(&fd), "40 %");
        fd[..0x40000 * 3 / 5].fill(0x33);
        assert!(vars_full(&fd), "60 %");
        // What follows the store (the fault-tolerant write areas) isn't counted.
        let mut fd = vec![0xFFu8; 540_672];
        fd[0x41000..].fill(0);
        assert!(!vars_full(&fd));
    }
}
