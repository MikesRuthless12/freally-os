// SPDX-License-Identifier: Apache-2.0
//! Settings ▸ About ▸ the hardware report (TASK-111, FR-053): what this
//! machine is, as text a tester can send by email: the image, the processor
//! (its name, features, cores, clock), the memory, the firmware, the
//! display (its mode and the panel's name), every PCI function and the
//! driver that took it, and how the boot went. Copied, or saved to a file;
//! never sent anywhere.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The report's keys, in About: Copy, then Save.
pub const KEYS: [(&str, &str); 2] =
    [("Copy the report", "settings-report-copy"), ("Save it to a file", "settings-report-save")];

/// Where Save puts it, in the account's home.
pub const FOLDER: &str = "Exports";
pub const FILE: &str = "hardware-report.txt";

/// Each section's heading, in order.
pub const SECTIONS: [&str; 8] =
    ["Freally OS", "Processor", "Memory", "Firmware", "Display", "PCI devices", "Drivers", "Boot"];

/// What the report says, gathered by the program.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The version, the build, the slot (as About shows them).
    pub image: String,
    pub cpu_name: String,
    pub cpu_features: Vec<&'static str>,
    /// The cores (`Cpus::layout`, the APIC mode, the timer).
    pub cores: String,
    pub clock: String,
    /// Free now, and what there was at boot (bytes).
    pub memory: Option<(u64, u64)>,
    pub firmware: String,
    /// The mode in use.
    pub display: Option<(u32, u32)>,
    /// The panel's name from its EDID.
    pub panel: Option<String>,
    /// (address, what it is).
    pub pci: Vec<(String, String)>,
    /// (device, driver).
    pub bound: Vec<(String, String)>,
    /// (stage, milliseconds since the machine was switched on).
    pub boot: Vec<(String, f64)>,
}

fn or_unknown(s: &str) -> &str {
    if s.trim().is_empty() { "unknown" } else { s.trim() }
}

/// The report, as text.
pub fn report(f: &Facts) -> String {
    let mut s = String::from("Freally OS hardware report\nNothing in it is sent anywhere: you choose where it goes.\n");
    let section = |s: &mut String, name: &str| {
        let _ = write!(s, "\n{name}\n");
    };
    section(&mut s, SECTIONS[0]);
    let _ = writeln!(s, "  {}", or_unknown(&f.image));
    section(&mut s, SECTIONS[1]);
    let _ = writeln!(s, "  {}", or_unknown(&f.cpu_name));
    let _ = writeln!(s, "  {}", or_unknown(&f.cores));
    if !f.clock.is_empty() {
        let _ = writeln!(s, "  {}", f.clock);
    }
    let features = if f.cpu_features.is_empty() { String::from("none found") } else { f.cpu_features.join(", ") };
    let _ = writeln!(s, "  Features: {features}");
    section(&mut s, SECTIONS[2]);
    match f.memory {
        Some((free, total)) => {
            let _ = writeln!(s, "  {} MiB at boot, {} MiB free now", total >> 20, free >> 20);
        }
        None => s.push_str("  unknown\n"),
    }
    section(&mut s, SECTIONS[3]);
    let _ = writeln!(s, "  {}", or_unknown(&f.firmware));
    section(&mut s, SECTIONS[4]);
    match f.display {
        Some((w, h)) => {
            let _ = writeln!(s, "  {w} × {h}");
        }
        None => s.push_str("  unknown mode\n"),
    }
    let _ = writeln!(s, "  Panel: {}", f.panel.as_deref().map_or("no EDID", or_unknown));
    section(&mut s, SECTIONS[5]);
    if f.pci.is_empty() {
        s.push_str("  none found\n");
    }
    for (at, what) in &f.pci {
        let _ = writeln!(s, "  {at}  {what}");
    }
    section(&mut s, SECTIONS[6]);
    if f.bound.is_empty() {
        s.push_str("  none bound\n");
    }
    for (device, driver) in &f.bound {
        let _ = writeln!(s, "  {device}: {driver}");
    }
    section(&mut s, SECTIONS[7]);
    if f.boot.is_empty() {
        s.push_str("  no timeline\n");
    }
    for (stage, ms) in &f.boot {
        let _ = writeln!(s, "  {stage}: {ms:.1} ms");
    }
    s
}

/// The processor's name, from CPUID's leaves 0x80000002–4 (each leaf's
/// eax, ebx, ecx, edx: 16 bytes of the name).
pub fn cpu_name(leaves: &[[u32; 4]; 3]) -> String {
    let bytes: Vec<u8> = leaves.iter().flatten().flat_map(|r| r.to_le_bytes()).collect();
    String::from_utf8_lossy(&bytes).trim_matches(|c: char| c == '\0' || c.is_whitespace()).into()
}

/// The features worth knowing, from CPUID leaf 1 (ecx, edx) and leaf 7
/// (ebx).
pub fn cpu_features(ecx1: u32, edx1: u32, ebx7: u32) -> Vec<&'static str> {
    let bit = |r: u32, b: u32| r & (1 << b) != 0;
    let mut v = Vec::new();
    for (on, name) in [
        (bit(edx1, 26), "SSE2"),
        (bit(ecx1, 0), "SSE3"),
        (bit(ecx1, 9), "SSSE3"),
        (bit(ecx1, 19), "SSE4.1"),
        (bit(ecx1, 20), "SSE4.2"),
        (bit(ecx1, 23), "POPCNT"),
        (bit(ecx1, 25), "AES-NI"),
        (bit(ecx1, 28), "AVX"),
        (bit(ebx7, 5), "AVX2"),
        (bit(ebx7, 16), "AVX-512F"),
        (bit(ebx7, 8), "BMI2"),
        (bit(ebx7, 29), "SHA-NI"),
        (bit(ecx1, 30), "RDRAND"),
        (bit(ebx7, 18), "RDSEED"),
        (bit(ecx1, 21), "x2APIC"),
        (bit(ecx1, 24), "TSC-deadline"),
    ] {
        if on {
            v.push(name);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            image: String::from("0.8.0 (a test build), A, build abcdef012345"),
            cpu_name: String::from("13th Gen Intel(R) Core(TM) i7-13700HX"),
            cpu_features: alloc::vec!["SSE2", "AVX2"],
            cores: String::from("8 performance + 16 efficient · x2APIC"),
            clock: String::from("TSC 2.3 GHz (from CPUID)"),
            memory: Some((3 << 30, 4 << 30)),
            firmware: String::from("HP"),
            display: Some((1920, 1080)),
            panel: Some(String::from("BOE NE173QHM")),
            pci: alloc::vec![(String::from("00:04.0"), String::from("NVMe controller (1b36:0010)"))],
            bound: alloc::vec![(String::from("00:04.0"), String::from("drv-nvme"))],
            boot: alloc::vec![(String::from("B3 kernel"), 412.25)],
        }
    }

    #[test]
    fn every_section_is_there_with_what_was_found() {
        let r = report(&facts());
        let mut at = 0;
        for name in SECTIONS {
            let i = r[at..].find(&alloc::format!("\n{name}\n")).unwrap_or_else(|| panic!("{name} missing"));
            at += i + 1;
        }
        for want in [
            "i7-13700HX",
            "Features: SSE2, AVX2",
            "4096 MiB at boot, 3072 MiB free now",
            "1920 × 1080",
            "Panel: BOE NE173QHM",
            "00:04.0  NVMe controller",
            "00:04.0: drv-nvme",
            "B3 kernel: 412.2 ms",
            "Nothing in it is sent anywhere",
        ] {
            assert!(r.contains(want), "{want:?} not in\n{r}");
        }
    }

    #[test]
    fn what_wasnt_found_says_so() {
        let r = report(&Facts::default());
        for want in ["unknown", "unknown mode", "Panel: no EDID", "none found", "none bound", "no timeline"] {
            assert!(r.contains(want), "{want:?} not in\n{r}");
        }
    }

    #[test]
    fn cpuid_is_read_as_the_manual_says() {
        // "Intel(R) Core(TM)" … as CPUID returns it, padded with NULs.
        let text = b"  Freally Test CPU @ 3.00GHz\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let mut leaves = [[0u32; 4]; 3];
        for (i, w) in text.chunks(4).take(12).enumerate() {
            leaves[i / 4][i % 4] = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        }
        assert_eq!(cpu_name(&leaves), "Freally Test CPU @ 3.00GHz");
        let f = cpu_features(1 << 25 | 1 << 20, 1 << 26, 1 << 29 | 1 << 5);
        assert_eq!(f, ["SSE2", "SSE4.2", "AES-NI", "AVX2", "SHA-NI"]);
    }
}
