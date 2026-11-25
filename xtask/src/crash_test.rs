// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask crash-test [--resets=N] [--seed=S] [--planted]` (TASK-099,
//! FR-044): the crash-consistency test. A VM with a whole GPT disk boots an
//! e2e image whose ffs, once mounted, runs the load (transactions whose
//! result can be judged: `services/ffs/src/load.rs`) until the machine is
//! reset. The host resets it (QMP `system_reset`) at a random moment of
//! the load, N times (1,000); after each, the mount runs ffsck and the load
//! is checked. Red on any ffsck finding, a load that isn't one whole
//! transaction, a version that went backwards, or a boot that never got
//! there. `--planted` builds ffs with the data written after the superblock
//! and must be red.
//!
//! A reset keeps every write the disk completed (QEMU's cache outlives the
//! guest), so this is the test of the order of writes; the loss of a
//! disk's unflushed writes is the host tests' (`MemStore::crash`).

use crate::build;
use crate::run::{self, VmOptions};
use cdp_proto::qmp::Qmp;
use fcore_abi::Flavour;
use std::time::{Duration, Instant};

const RESETS: usize = 1000;
/// Every this many resets, a cold one instead: QEMU quits mid-load and
/// starts again on the same disk with fresh firmware variables (OVMF
/// rewrites its console variables at every boot, and a store filled by a
/// few hundred warm boots hung a warm reset).
const COLD_EVERY: usize = 100;
const PORT_OFFSET: u16 = 800;
/// The longest a boot may take to mount, check and start the load.
const BOOT_LIMIT: Duration = Duration::from_secs(90);
/// The longest the firmware may take to start the bootloader.
const FIRMWARE_LIMIT: Duration = Duration::from_secs(30);
/// OVMF (QEMU 10.2) sometimes doesn't find an NVMe disk at all: "No
/// bootable option or device was found", then its boot menu, before any of
/// our code runs. Measured 2026-11-19: 1 to 3 cold starts in 10 with the
/// same disk that booted 10 in 10 on virtio-blk, and a few warm resets in a
/// hundred (more as a run goes on: a warm reset doesn't clear it). Such a
/// boot gets QEMU started again (the disk kept), up to this many times in a
/// row, and is counted; misses after more than 10 % of the resets are red.
const MISSES_IN_A_ROW: usize = 3;

pub fn crash_test_cmd(args: &[String]) -> Result<(), String> {
    let arg = |k: &str| args.iter().find_map(|a| a.strip_prefix(k));
    let resets = arg("--resets=").map(str::parse).transpose().map_err(|e| format!("--resets: {e}"))?.unwrap_or(RESETS);
    let seed = match arg("--seed=") {
        Some(s) => s.parse().map_err(|e| format!("--seed: {e}"))?,
        None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64),
    };
    let planted = args.iter().any(|a| a == "--planted");
    let said = run(resets, seed, planted)?;
    println!("crash-test: {said}");
    Ok(())
}

/// xorshift64: the moments of the resets, from the seed (printed, so a red
/// run can be repeated).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// What one boot said, from its part of COM1.
#[derive(Debug, PartialEq, Eq)]
enum Boot {
    /// Still booting.
    Waiting,
    /// The firmware never started the bootloader.
    Stalled,
    /// drv-nvme couldn't get the controller to answer (TASK-092A).
    NoController,
    /// The load checked whole at this version and is running again.
    Running {
        version: u64,
        ffsck_ran: bool,
    },
    Broken(String),
}

/// Reads a boot's part of the log.
fn judge(part: &str) -> Boot {
    let mut version = None;
    let mut ffsck_ran = false;
    for line in part.lines() {
        if line.contains("ffs: crash-load: BROKEN") || line.contains("ffs crashed") || line.contains("panic") {
            return Boot::Broken(line.trim().to_string());
        }
        if let Some(what) = line.split("ffs: ffsck: ").nth(1)
            && !what.starts_with("clean")
        {
            return Boot::Broken(format!("ffsck: {what}"));
        }
        // A bring-up the driver gave up on (one it tried again is said with "asking again").
        if line.contains("drv-nvme: command") && line.contains("never completed") && !line.contains("asking again") {
            return Boot::NoController;
        }
        if line.contains("wasn't unmounted: ffsck ran") {
            ffsck_ran = true;
        }
        if line.contains("ffs: crash-load: fresh") {
            version = Some(0);
        }
        if let Some(rest) = line.split("ffs: crash-load: version ").nth(1) {
            version = rest.split(' ').next().and_then(|v| v.parse().ok());
        }
        if line.contains("ffs: crash-load: running") {
            return match version {
                Some(version) => Boot::Running { version, ffsck_ran },
                None => Boot::Broken("the load ran without saying what it found".into()),
            };
        }
    }
    Boot::Waiting
}

/// Waits for the boot that starts at `from` in the log to finish judging.
fn next_boot(log: &std::path::Path, from: usize) -> Result<(Boot, usize), String> {
    let start = Instant::now();
    loop {
        let mut text = std::fs::read(log).map_err(|e| format!("{}: {e}", log.display()))?;
        // Whole lines only: the last may still be being written.
        text.truncate(text.iter().rposition(|&b| b == b'\n').map_or(0, |n| n + 1).max(from));
        let part = String::from_utf8_lossy(text.get(from..).unwrap_or_default());
        match judge(&part) {
            Boot::Waiting if part.contains("BdsDxe: No bootable option") => return Ok((Boot::Stalled, text.len())),
            Boot::Waiting if start.elapsed() > FIRMWARE_LIMIT && !part.contains("freally-boot ") => {
                return Ok((Boot::Stalled, text.len()));
            }
            Boot::Waiting if start.elapsed() < BOOT_LIMIT => std::thread::sleep(Duration::from_millis(50)),
            Boot::Waiting => {
                let tail: Vec<&str> = part.lines().rev().take(12).collect();
                return Err(format!(
                    "a boot never reached the load in {} s; its last lines:\n{}",
                    BOOT_LIMIT.as_secs(),
                    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                ));
            }
            b => return Ok((b, text.len())),
        }
    }
}

/// The VM under test: QEMU, its QMP, and how far its log has been read.
struct Machine<'a> {
    o: VmOptions,
    built: &'a build::Built,
    child: std::process::Child,
    q: Qmp,
    at: usize,
    /// The firmware's variables once the run's first boot was good: a cold
    /// start takes these (its boot entry already made), not fresh ones.
    settled: Option<std::path::PathBuf>,
    /// The firmware didn't find the disk; drv-nvme couldn't start the
    /// controller (TASK-092A).
    misses: (usize, usize),
}

impl<'a> Machine<'a> {
    fn start(o: VmOptions, built: &'a build::Built) -> Result<Machine<'a>, String> {
        let child = run::start_built(&o, built)?;
        let q = Qmp::connect(&o.qmp_socket(), Duration::from_secs(20))?;
        Ok(Machine { o, built, child, q, at: 0, settled: None, misses: (0, 0) })
    }

    fn vars(&self) -> std::path::PathBuf {
        self.o.vars()
    }

    /// What a stalled boot shows, before its VM goes: conductor's services
    /// as its test door says them (no answer: conductor, or everything,
    /// stopped), each CPU's registers (QEMU's `info registers -a`), then
    /// three times a second apart each service's CPU time (the door's `ps`)
    /// and where each CPU is, so who runs and who waits shows. The kernel and
    /// the programs are stripped: an unstripped build of the same commit
    /// names the RIPs (the builds are reproducible).
    fn stalled(&mut self) -> String {
        let mut out = format!("the test door's services:\n{}", self.door("services"));
        // What the logger holds, whether or not COM1 showed it: its last lines
        // say whether the drivers' lines reached it (no answer: the logger
        // itself is stuck).
        let log = self.door("log 300");
        let tail: Vec<&str> = log.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
        out.push_str(&format!("\nthe logger's last records:\n{}", tail.join("\n")));
        out.push_str(&format!("\nthe CPUs:\n{}", self.registers()));
        // The last report showed the OS well (the load running, the logger
        // taking every line) and COM1's log silent: QEMU's side of COM1.
        out.push_str(&format!("\nQEMU's character devices:\n{}", self.monitor("info chardev")));
        let port = self.o.port(1);
        let ss = std::process::Command::new("ss")
            .args(["-tnpo", &format!("( sport = :{port} or dport = :{port} )")])
            .output();
        out.push_str(&format!(
            "\nTCP on COM1's port {port}:\n{}",
            ss.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_else(|e| e.to_string())
        ));
        for _ in 0..3 {
            std::thread::sleep(Duration::from_secs(1));
            let rips: Vec<String> = self
                .registers()
                .lines()
                .filter(|l| l.starts_with("CPU#") || l.starts_with("RIP="))
                .map(String::from)
                .collect();
            out.push_str(&format!("\na second on:\n{}\n{}", self.door("ps"), rips.join("\n")));
        }
        out
    }

    /// The test door's answer to `cmd` (or why there was none).
    fn door(&self, cmd: &str) -> String {
        use std::io::{BufRead, Write};
        let lines = std::net::TcpStream::connect(("127.0.0.1", self.o.port(3))).and_then(|mut s| {
            s.set_read_timeout(Some(Duration::from_secs(3)))?;
            s.write_all(format!("{cmd}\n").as_bytes())?;
            let mut lines = Vec::new();
            for line in std::io::BufReader::new(s).lines() {
                let line = line?;
                if line.trim() == "end" {
                    break;
                }
                lines.push(line);
            }
            Ok(lines.join("\n"))
        });
        lines.unwrap_or_else(|e| format!("no answer: {e}"))
    }

    fn registers(&mut self) -> String {
        self.monitor("info registers -a")
    }

    /// A QEMU monitor command's answer, through the QMP the test holds.
    fn monitor(&mut self, line: &str) -> String {
        let cmd = fjson::Value::obj([("command-line", fjson::Value::Str(line.into()))]);
        match self.q.execute("human-monitor-command", Some(cmd)) {
            Ok(v) => v.as_str().unwrap_or("(not text)").trim_end().to_string(),
            Err(e) => format!("(no answer to {line}: {e})"),
        }
    }

    /// QEMU quits (the load mid-write: a crash too) and starts again on the
    /// same disk.
    fn cold(&mut self) -> Result<(), String> {
        let _ = self.q.execute("quit", None);
        let _ = self.child.wait();
        match &self.settled {
            Some(s) => {
                std::fs::copy(s, self.vars()).map_err(|e| format!("the firmware's variables: {e}"))?;
            }
            None => {
                let _ = std::fs::remove_file(self.vars());
            }
        }
        self.o.keep_disk = true;
        self.child = run::start_built(&self.o, self.built)?;
        self.q = Qmp::connect(&self.o.qmp_socket(), Duration::from_secs(20))?;
        // A new QEMU starts a new log.
        self.at = 0;
        Ok(())
    }

    /// The next boot, through the misses before our code has the disk.
    fn boot(&mut self) -> Result<Boot, String> {
        let log = self.o.com1_log();
        for _ in 0..=MISSES_IN_A_ROW {
            let (b, end) = match next_boot(&log, self.at) {
                Ok(x) => x,
                Err(e) => return Err(format!("{e}\n{}", self.stalled())),
            };
            self.at = end;
            match b {
                Boot::Stalled => {
                    // A warm reset leaves QEMU's NVMe as it was: QEMU starts again.
                    self.misses.0 += 1;
                    println!("crash-test: the firmware didn't find the disk; QEMU again");
                    self.cold()?;
                }
                Boot::NoController => {
                    self.misses.1 += 1;
                    println!("crash-test: drv-nvme couldn't start the controller; QEMU again");
                    self.cold()?;
                }
                b => {
                    if self.settled.is_none() {
                        let s = self.o.settled_vars();
                        std::fs::copy(self.vars(), &s).map_err(|e| format!("the firmware's variables: {e}"))?;
                        self.settled = Some(s);
                    }
                    return Ok(b);
                }
            }
        }
        Err(format!("the disk was missed {} times in a row", MISSES_IN_A_ROW + 1))
    }
}

pub fn run(resets: usize, seed: u64, planted: bool) -> Result<String, String> {
    let built = build::build_crash_load(planted)?;
    let mut o = VmOptions::new(Flavour::E2e, if planted { "crash-test-planted" } else { "crash-test" });
    o.headless = true;
    o.tcg = !std::path::Path::new("/dev/kvm").exists();
    o.gpt = true;
    o.port_offset = PORT_OFFSET;
    println!("crash-test: {resets} resets, seed {seed}{}", if planted { ", the planted ordering bug" } else { "" });
    // Fresh firmware variables and disk for each run.
    let _ = std::fs::remove_file(o.vars());
    let mut m = Machine::start(o, &built)?;
    let result = (|| {
        let mut rng = Rng(seed | 1);
        let first = m.boot()?;
        let Boot::Running { version: mut last, .. } = first else {
            return Err(format!("the first boot: {first:?}"));
        };
        let (mut advanced, mut checked) = (0usize, 0usize);
        let started = Instant::now();
        for i in 1..=resets {
            // A random moment of the load: 20 to 1,200 ms into it.
            std::thread::sleep(Duration::from_millis(20 + rng.below(1180)));
            if i % COLD_EVERY == 0 {
                m.cold().map_err(|e| format!("reset {i}: {e}"))?;
            } else {
                m.q.execute("system_reset", None).map_err(|e| format!("reset {i}: {e}"))?;
            }
            match m.boot().map_err(|e| format!("reset {i}: {e}"))? {
                Boot::Running { version, ffsck_ran } => {
                    if version < last {
                        return Err(format!("reset {i}: the load went back from version {last} to {version}"));
                    }
                    advanced += usize::from(version > last);
                    checked += usize::from(ffsck_ran);
                    last = version;
                }
                Boot::Broken(why) => return Err(format!("reset {i} (seed {seed}): {why}")),
                Boot::Stalled | Boot::NoController | Boot::Waiting => unreachable!(),
            }
            if i % 50 == 0 {
                println!(
                    "crash-test: {i} of {resets} resets whole, at version {last} ({:.0} s)",
                    started.elapsed().as_secs_f64()
                );
            }
        }
        let (firmware, controller) = m.misses;
        if (firmware + controller) * 10 > resets {
            return Err(format!(
                "the disk was missed {} times in {resets} resets (over 10 %): the firmware {firmware}, drv-nvme {controller}",
                firmware + controller
            ));
        }
        Ok(format!(
            "{resets} resets, every mount whole: ffsck clean after {checked} unclean shutdowns, the load at version {last}, \
             {advanced} reset(s) landed after a commit; the disk missed before FreallyFS {} time(s) (the firmware \
             {firmware}, drv-nvme {controller}) ({:.1} min, seed {seed})",
            firmware + controller,
            started.elapsed().as_secs_f64() / 60.0
        ))
    })();
    let _ = m.child.kill();
    let _ = m.child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_boot_is_judged_from_its_log() {
        assert_eq!(judge("[ffs] ffs: mounted generation 3\n"), Boot::Waiting);
        let ok = "[ffs] ffs: generation 9 wasn't unmounted: ffsck ran\n[ffs] ffs: ffsck: clean (40 nodes, 12 data blocks)\n\
                  [ffs] ffs: crash-load: version 17 whole\n[ffs] ffs: crash-load: running\n";
        assert_eq!(judge(ok), Boot::Running { version: 17, ffsck_ran: true });
        let fresh = "[ffs] ffs: formatted FREALLY_DATA\n[ffs] ffs: crash-load: fresh: nothing written yet\n[ffs] ffs: crash-load: running\n";
        assert_eq!(judge(fresh), Boot::Running { version: 0, ffsck_ran: false });
        assert!(
            matches!(judge("[ffs] ffs: ffsck: block 40 is used twice\n"), Boot::Broken(w) if w.contains("used twice"))
        );
        assert!(matches!(judge("[ffs] ffs: crash-load: BROKEN: file-1 isn't what version 3 wrote\n"), Boot::Broken(_)));
        assert!(matches!(judge("[conductor] conductor: ffs crashed 6 times in 60 s\n"), Boot::Broken(_)));
    }
}
