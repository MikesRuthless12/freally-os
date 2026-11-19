// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask probe` (TASK-069): builds the probe image, boots it headless
//! in QEMU, checks on COM1 what it gathered (every CPU, the PCI functions, the
//! ACPI tables, the keyboard's verdict), and keeps a picture of each page in
//! `target/probe/page-N.png`: what the owner will photograph on a new
//! machine. The image itself, for the stick, is `target/freally/probe/`.

use crate::build;
use crate::run::{self, VmOptions};
use crate::util;
use cdp_proto::qmp::Qmp;
use fcore_abi::Flavour;
use fjson::Value;
use std::path::Path;
use std::time::{Duration, Instant};

const PORT_OFFSET: u16 = 800;
const CPUS: u32 = 2;

/// Waits for a COM1 line holding `needle`; returns that line.
fn wait_line(log: &Path, needle: &str, within: Duration) -> Result<String, String> {
    let until = Instant::now() + within;
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if let Some(l) = text.lines().find(|l| l.contains(needle)) {
            return Ok(l.to_string());
        }
        if Instant::now() > until {
            let tail: Vec<&str> = text.lines().rev().take(15).collect();
            return Err(format!(
                "no \"{needle}\" on COM1 within {} s; its last lines:\n{}",
                within.as_secs(),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A number after `key ` in a line (`… 6 PCI functions …` → 6 for "PCI").
fn number_before(line: &str, word: &str) -> Option<usize> {
    let words: Vec<&str> = line.split_whitespace().collect();
    words.windows(2).find(|w| w[1].starts_with(word)).and_then(|w| w[0].parse().ok())
}

pub fn probe_cmd(_args: &[String]) -> Result<(), String> {
    let built = build::build_probe()?;
    let mut o = VmOptions::new(Flavour::Dev, "probe");
    o.headless = true;
    o.cpus = CPUS;
    o.tcg = !Path::new("/dev/kvm").exists();
    o.port_offset = PORT_OFFSET;
    let mut vm = run::start_built(&o, &built)?;
    let result = (|| {
        let first = wait_line(&o.com1_log(), "probe: page 1 of", Duration::from_secs(90))?;
        let pages = first.rsplit("of ").next().and_then(|s| s.split_whitespace().next()?.parse::<usize>().ok());
        let gathered = wait_line(&o.com1_log(), "probe: gathered", Duration::from_secs(5))?;
        println!("probe: {}", gathered.split("probe: ").nth(1).unwrap_or(&gathered));
        let cpus = number_before(&gathered, "CPU").unwrap_or(0);
        let pci = number_before(&gathered, "PCI").unwrap_or(0);
        let tables = number_before(&gathered, "ACPI").unwrap_or(0);
        if cpus != CPUS as usize {
            return Err(format!("the probe saw {cpus} CPUs; the VM has {CPUS}"));
        }
        if pci == 0 || tables == 0 {
            return Err(String::from("the probe found no PCI functions or no ACPI tables"));
        }
        if !gathered.contains("the keyboard is PS/2") {
            return Err(String::from("the probe's keyboard verdict isn't PS/2, on a q35 that has one"));
        }
        let out = util::target_dir().join("probe");
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        let mut q = Qmp::connect(&o.qmp_socket(), Duration::from_secs(20))?;
        let pages = pages.unwrap_or(1).min(8);
        for n in 1..=pages {
            if n > 1 {
                wait_line(&o.com1_log(), &format!("probe: page {n} of"), Duration::from_secs(30))?;
            }
            // A moment for the page to finish drawing.
            std::thread::sleep(Duration::from_millis(500));
            let ppm = out.join(format!("page-{n}.ppm"));
            q.execute("screendump", Some(Value::obj([("filename", ppm.display().to_string().into())])))?;
            let data = std::fs::read(&ppm).map_err(|e| format!("screendump: {e}"))?;
            let _ = std::fs::remove_file(&ppm);
            let (w, h, rgb) = fimage::ppm::parse(&data)?;
            let mut colours: Vec<[u8; 3]> = rgb.as_chunks::<3>().0.iter().step_by(97).copied().collect();
            colours.sort_unstable();
            colours.dedup();
            if colours.len() < 3 {
                return Err(format!("page {n} is blank ({} colours)", colours.len()));
            }
            let png = out.join(format!("page-{n}.png"));
            std::fs::write(&png, fimage::png::encode(w, h, 3, rgb)).map_err(|e| e.to_string())?;
            println!("probe: page {n} of {pages}: {}", png.display());
        }
        Ok(())
    })();
    let _ = vm.kill();
    let _ = vm.wait();
    result?;
    println!("probe: green; the image for a stick is {}", built.dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::number_before;

    #[test]
    fn numbers_read_from_the_probes_line() {
        let l = "[  1.2] probe: gathered 2 CPUs (2 CPUs), 6 PCI functions, 9 ACPI tables; the keyboard is PS/2";
        assert_eq!(number_before(l, "CPU"), Some(2));
        assert_eq!(number_before(l, "PCI"), Some(6));
        assert_eq!(number_before(l, "ACPI"), Some(9));
        assert_eq!(number_before(l, "USB"), None);
    }
}
