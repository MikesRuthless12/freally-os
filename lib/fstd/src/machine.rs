// SPDX-License-Identifier: Apache-2.0
//! What the kernel says about the machine (`system_info`, PRD § 4): every
//! CPU and its core type (TASK-070), the display's EDID and the GOP's modes
//! (TASK-069, TASK-071). The readers are pure and host-tested; the calls are
//! thin.

use alloc::vec::Vec;
use fcore_abi::topic;

/// One CPU as the kernel knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub apic_id: u32,
    /// `fcore_abi::core_type`: unknown, efficiency or performance.
    pub core_type: u8,
    pub online: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cpus {
    pub x2apic: bool,
    pub tsc_deadline: bool,
    pub hybrid: bool,
    /// Where the kernel found the TSC's frequency (`fcore_abi::tsc_from`).
    pub tsc_from: u8,
    pub cpus: Vec<Cpu>,
}

impl Cpus {
    /// "8 performance + 16 efficiency cores", or "4 CPUs" when they're all alike.
    pub fn layout(&self) -> alloc::string::String {
        use fcore_abi::core_type::{EFFICIENCY, PERFORMANCE};
        let p = self.cpus.iter().filter(|c| c.core_type == PERFORMANCE).count();
        let e = self.cpus.iter().filter(|c| c.core_type == EFFICIENCY).count();
        let n = self.cpus.len();
        if p + e == n && p > 0 && e > 0 {
            alloc::format!("{p} performance + {e} efficiency cores")
        } else {
            alloc::format!("{n} CPU{}", if n == 1 { "" } else { "s" })
        }
    }
}

/// The clock in words (Monitor → Boot, TASK-073): "TSC 2,416 MHz, from
/// CPUID 0x15 (the HPET agrees)".
pub fn clock_line(tsc_hz: u64, from: u8) -> alloc::string::String {
    use fcore_abi::tsc_from::{CPUID, CPUID_HPET_AGREES, HPET, HYPERVISOR};
    let how = match from {
        CPUID_HPET_AGREES => "from CPUID 0x15 (the HPET agrees)",
        CPUID => "from CPUID 0x15 (no HPET to check it)",
        HYPERVISOR => "from the hypervisor (CPUID 0x40000010)",
        HPET => "measured against the HPET",
        _ => "measured against the PIT",
    };
    let mhz = tsc_hz / 1_000_000;
    alloc::format!("TSC {},{:03} MHz, {how}", mhz / 1000, mhz % 1000).replacen("TSC 0,", "TSC ", 1)
}

/// A core type in words.
pub fn core_type_name(t: u8) -> &'static str {
    match t {
        fcore_abi::core_type::PERFORMANCE => "performance",
        fcore_abi::core_type::EFFICIENCY => "efficiency",
        _ => "—",
    }
}

/// What a PCI function is, from its class, subclass and programming
/// interface (the PCI Code and ID Assignment specification). Short-range
/// wireless controllers are "Wireless controller": the product name of that
/// protocol never appears where a person reads (owner instruction 19).
pub fn pci_class(class: u8, subclass: u8, prog_if: u8) -> &'static str {
    match (class, subclass, prog_if) {
        (0x01, 0x06, _) => "SATA storage",
        (0x01, 0x08, _) => "NVMe storage",
        (0x01, _, _) => "Storage controller",
        (0x02, 0x00, _) => "Ethernet",
        (0x02, 0x80, _) => "Network controller",
        (0x02, _, _) => "Network",
        (0x03, 0x00, _) => "Display (VGA)",
        (0x03, 0x02, _) => "3D controller",
        (0x03, _, _) => "Display controller",
        (0x04, 0x03, _) => "Audio (HD Audio)",
        (0x04, _, _) => "Multimedia",
        (0x05, _, _) => "Memory controller",
        (0x06, 0x00, _) => "Host bridge",
        (0x06, 0x01, _) => "ISA bridge",
        (0x06, 0x04, _) => "PCI bridge",
        (0x06, _, _) => "Bridge",
        (0x07, _, _) => "Communication controller",
        (0x08, 0x05, _) => "SD card reader",
        (0x08, _, _) => "System peripheral",
        (0x09, _, _) => "Input controller",
        (0x0C, 0x03, 0x30) => "USB (xHCI)",
        (0x0C, 0x03, 0x40) => "USB4",
        (0x0C, 0x03, _) => "USB (older)",
        (0x0C, 0x05, _) => "SMBus",
        (0x0C, 0x80, _) => "Serial bus",
        (0x0C, _, _) => "Serial bus controller",
        (0x0D, _, _) => "Wireless controller",
        (0x10, _, _) => "Encryption controller",
        (0x11, _, _) => "Signal processing",
        (0x12, _, _) => "Processing accelerator",
        _ => "Other device",
    }
}

/// Reads `system_info(CPUS)`'s bytes.
pub fn parse_cpus(b: &[u8]) -> Option<Cpus> {
    if b.len() < 8 {
        return None;
    }
    let n = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    let recs = b.get(8..8 + n.checked_mul(8)?)?;
    Some(Cpus {
        x2apic: b[0] != 0,
        tsc_deadline: b[1] != 0,
        hybrid: b[2] != 0,
        tsc_from: b[3],
        cpus: recs
            .as_chunks::<8>()
            .0
            .iter()
            .map(|r| Cpu { apic_id: u32::from_le_bytes([r[0], r[1], r[2], r[3]]), core_type: r[4], online: r[5] != 0 })
            .collect(),
    })
}

/// A mode's width and height.
pub type Mode = (u32, u32);

/// Reads `system_info(GOP_MODES)`'s bytes: every mode, and the one in use.
pub fn parse_gop_modes(b: &[u8]) -> (Vec<Mode>, Option<Mode>) {
    let words: Vec<u32> = b.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
    let split = |v: u32| (v >> 16, v & 0xFFFF);
    let end = words.iter().position(|&w| w == 0).unwrap_or(words.len());
    let modes = words[..end].iter().map(|&w| split(w)).collect();
    let current = words.get(end + 1).map(|&w| split(w));
    (modes, current)
}

/// Reads a topic whole, however long it is.
fn read(t: u64) -> Option<Vec<u8>> {
    let mut buf = alloc::vec![0u8; 512];
    loop {
        let n = crate::sys::system_info(t, &mut buf).ok()?;
        if n <= buf.len() {
            buf.truncate(n);
            return Some(buf);
        }
        buf.resize(n, 0);
    }
}

pub fn cpus() -> Option<Cpus> {
    parse_cpus(&read(topic::CPUS)?)
}

/// The display's EDID as the firmware gave it (empty when there's none).
pub fn edid() -> Vec<u8> {
    read(topic::EDID).unwrap_or_default()
}

pub fn gop_modes() -> (Vec<Mode>, Option<Mode>) {
    read(topic::GOP_MODES).map_or((Vec::new(), None), |b| parse_gop_modes(&b))
}

/// The running image (`system_info(IMAGE)`, TASK-081).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// 0 A, 1 B.
    pub slot: u8,
    /// 0 release, 1 dev, 2 e2e.
    pub flavour: u8,
    pub version: alloc::string::String,
    /// The git commit it was built from.
    pub build_id: [u8; 20],
}

impl Image {
    /// "A" or "B".
    pub fn slot_name(&self) -> &'static str {
        if self.slot == 0 { "A" } else { "B" }
    }

    /// The build id in hex, as `git log` shows a commit.
    pub fn build_hex(&self) -> alloc::string::String {
        self.build_id.iter().map(|b| alloc::format!("{b:02x}")).collect()
    }
}

/// Reads the IMAGE topic's bytes.
pub fn parse_image(b: &[u8]) -> Option<Image> {
    if b.len() < 56 {
        return None;
    }
    let v = &b[4..36];
    let len = v.iter().position(|&c| c == 0).unwrap_or(v.len());
    Some(Image {
        slot: b[0],
        flavour: b[1],
        version: alloc::string::String::from_utf8_lossy(&v[..len]).into_owned(),
        build_id: b[36..56].try_into().ok()?,
    })
}

/// The running image.
pub fn image() -> Option<Image> {
    parse_image(&read(topic::IMAGE)?)
}

/// `CLOCK_ZONE`'s answer: the offset in minutes east of UTC, and whether
/// the RTC keeps local time.
pub fn parse_zone(b: &[u8]) -> Option<(i16, bool)> {
    (b.len() == 4 && b[2] <= 1 && b[3] == 0).then(|| (i16::from_le_bytes([b[0], b[1]]), b[2] == 1))
}

/// The zone in use (TASK-180A).
pub fn zone() -> Option<(i16, bool)> {
    parse_zone(&read(topic::CLOCK_ZONE)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_zone_topic_reads_back() {
        assert_eq!(parse_zone(&[0xA8, 0xFE, 1, 0]), Some((-344, true)));
        assert_eq!(parse_zone(&[0x4A, 0x01, 0, 0]), Some((330, false)));
        assert_eq!(parse_zone(&[0, 0, 2, 0]), None);
        assert_eq!(parse_zone(&[0, 0, 0]), None);
    }

    #[test]
    fn the_image_topic_reads_back() {
        let mut b = alloc::vec![1u8, 1, 0, 0];
        let mut v = [0u8; 32];
        v[..5].copy_from_slice(b"0.6.0");
        b.extend_from_slice(&v);
        b.extend_from_slice(&[0xAB; 20]);
        let i = parse_image(&b).unwrap();
        assert_eq!((i.slot_name(), i.flavour, i.version.as_str()), ("B", 1, "0.6.0"));
        assert_eq!(&i.build_hex()[..6], "ababab");
        assert_eq!(parse_image(&b[..40]), None);
    }
    use fcore_abi::core_type::{EFFICIENCY as E, PERFORMANCE as P};

    fn bytes(x2apic: u8, hybrid: u8, cpus: &[(u32, u8, u8)]) -> Vec<u8> {
        let mut b = alloc::vec![x2apic, 1, hybrid, 0];
        b.extend_from_slice(&(cpus.len() as u32).to_le_bytes());
        for &(id, t, on) in cpus {
            b.extend_from_slice(&id.to_le_bytes());
            b.extend_from_slice(&[t, on, 0, 0]);
        }
        b
    }

    #[test]
    fn a_hybrid_cpu_reads_its_layout() {
        let c = parse_cpus(&bytes(1, 1, &[(0, P, 1), (8, P, 1), (32, E, 1), (33, E, 0)])).unwrap();
        assert!(c.x2apic && c.hybrid && c.tsc_deadline);
        assert_eq!(c.cpus[2], Cpu { apic_id: 32, core_type: E, online: true });
        assert!(!c.cpus[3].online);
        assert_eq!(c.layout(), "2 performance + 2 efficiency cores");
        assert_eq!(core_type_name(P), "performance");
    }

    #[test]
    fn the_clock_says_where_its_frequency_came_from() {
        use fcore_abi::tsc_from::{CPUID_HPET_AGREES, PIT};
        assert_eq!(clock_line(2_416_000_000, CPUID_HPET_AGREES), "TSC 2,416 MHz, from CPUID 0x15 (the HPET agrees)");
        assert_eq!(clock_line(800_000_000, PIT), "TSC 800 MHz, measured against the PIT");
        assert_eq!(
            clock_line(2_419_201_000, fcore_abi::tsc_from::HYPERVISOR),
            "TSC 2,419 MHz, from the hypervisor (CPUID 0x40000010)"
        );
        let mut b = bytes(1, 0, &[(0, 0, 1)]);
        b[3] = CPUID_HPET_AGREES;
        assert_eq!(parse_cpus(&b).map(|c| c.tsc_from), Some(CPUID_HPET_AGREES));
    }

    #[test]
    fn cpus_all_alike_are_just_counted() {
        let c = parse_cpus(&bytes(1, 0, &[(0, 0, 1), (1, 0, 1)])).unwrap();
        assert_eq!(c.layout(), "2 CPUs");
        assert_eq!(core_type_name(0), "—");
    }

    #[test]
    fn pci_classes_read_in_plain_words() {
        assert_eq!(pci_class(0x0C, 0x03, 0x30), "USB (xHCI)");
        assert_eq!(pci_class(0x01, 0x08, 0x02), "NVMe storage");
        assert_eq!(pci_class(0x03, 0x00, 0x00), "Display (VGA)");
        // Never the wireless protocol's trademark.
        assert_eq!(pci_class(0x0D, 0x11, 0x00), "Wireless controller");
        assert_eq!(pci_class(0xFF, 0, 0), "Other device");
    }

    #[test]
    fn a_short_cpus_answer_is_refused() {
        let mut b = bytes(1, 0, &[(0, 0, 1), (1, 0, 1)]);
        b.truncate(b.len() - 1);
        assert_eq!(parse_cpus(&b), None);
        assert_eq!(parse_cpus(&[1, 0]), None);
    }

    #[test]
    fn gop_modes_read_with_the_one_in_use() {
        let mut b = Vec::new();
        for v in [(1280u32 << 16) | 800, (2560 << 16) | 1600, 0, (2560 << 16) | 1600] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(parse_gop_modes(&b), (alloc::vec![(1280, 800), (2560, 1600)], Some((2560, 1600))));
        // BootInfo v2: no list, only the mode in use.
        let b: Vec<u8> = [0u32, (1024 << 16) | 768].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(parse_gop_modes(&b), (Vec::new(), Some((1024, 768))));
    }
}
