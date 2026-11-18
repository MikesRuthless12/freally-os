// SPDX-License-Identifier: Apache-2.0
//! Synthetic firmware tables: each parser reads what it should, and refuses a
//! bad checksum, a bad length or a missing signature.

extern crate std;

use super::*;
use std::collections::BTreeMap;

/// Physical memory as a map of placed byte strings.
#[derive(Default)]
struct Fake(BTreeMap<u64, Vec<u8>>);

impl Mem for Fake {
    fn read(&mut self, addr: u64, buf: &mut [u8]) -> bool {
        for (&base, bytes) in &self.0 {
            if addr >= base && addr + buf.len() as u64 <= base + bytes.len() as u64 {
                let o = (addr - base) as usize;
                buf.copy_from_slice(&bytes[o..o + buf.len()]);
                return true;
            }
        }
        false
    }
}

/// A table: header (signature, length, revision, checksum fixed up) + body.
fn sdt(sig: &[u8; 4], rev: u8, body: &[u8]) -> Vec<u8> {
    let mut t = alloc::vec![0u8; 36];
    t[..4].copy_from_slice(sig);
    t[8] = rev;
    t.extend_from_slice(body);
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    let sum = t.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    t[9] = 0u8.wrapping_sub(sum);
    t
}

fn rsdp(xsdt: u64) -> Vec<u8> {
    let mut r = alloc::vec![0u8; 36];
    r[..8].copy_from_slice(b"RSD PTR ");
    r[15] = 2;
    r[20..24].copy_from_slice(&36u32.to_le_bytes());
    r[24..32].copy_from_slice(&xsdt.to_le_bytes());
    let s1 = r[..20].iter().fold(0u8, |a, &x| a.wrapping_add(x));
    r[8] = 0u8.wrapping_sub(s1); // the OEM id's first byte carries the fix-up
    let s2 = r.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    r[32] = 0u8.wrapping_sub(s2);
    r
}

fn madt_body() -> Vec<u8> {
    let mut b = alloc::vec![0u8; 8];
    b[..4].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
    b[4..8].copy_from_slice(&1u32.to_le_bytes()); // PC-AT compatible: 8259s present
    // Two enabled local APICs, one disabled, an I/O APIC, an override.
    for (id, flags) in [(0u8, 1u32), (1, 1), (2, 0)] {
        b.extend_from_slice(&[0, 8, id, id]);
        b.extend_from_slice(&flags.to_le_bytes());
    }
    b.extend_from_slice(&[1, 12, 0, 0, 0, 0, 0xC0, 0xFE, 0, 0, 0, 0]);
    b.extend_from_slice(&[2, 10, 0, 0, 2, 0, 0, 0, 0, 0]);
    b
}

fn world() -> (Fake, u64) {
    let mut f = Fake::default();
    let mut fbpt = alloc::vec![0u8; 8 + 48];
    fbpt[..4].copy_from_slice(b"FBPT");
    fbpt[4..8].copy_from_slice(&56u32.to_le_bytes());
    fbpt[8..10].copy_from_slice(&2u16.to_le_bytes());
    fbpt[10] = 48;
    fbpt[16..24].copy_from_slice(&1_000_000u64.to_le_bytes()); // reset end
    fbpt[32..40].copy_from_slice(&9_000_000u64.to_le_bytes()); // StartImage
    f.0.insert(0x9000, fbpt);
    let mut fpdt_body = alloc::vec![0u8; 16];
    fpdt_body[2] = 16;
    fpdt_body[8..16].copy_from_slice(&0x9000u64.to_le_bytes());
    let mut mcfg_body = alloc::vec![0u8; 8];
    mcfg_body.extend_from_slice(&0xB000_0000u64.to_le_bytes());
    mcfg_body.extend_from_slice(&[0, 0, 0, 0xFF, 0, 0, 0, 0]);
    let mut fadt_body = alloc::vec![0u8; 244 - 36];
    fadt_body[109 - 36..111 - 36].copy_from_slice(&2u16.to_le_bytes()); // 8042 present
    fadt_body[140 - 36..148 - 36].copy_from_slice(&0x7000u64.to_le_bytes());
    let mut hpet_body = alloc::vec![0u8; 20];
    hpet_body[8..16].copy_from_slice(&0xFED0_0000u64.to_le_bytes());
    let tables = [
        (0x2000u64, sdt(b"APIC", 3, &madt_body())),
        (0x3000, sdt(b"FACP", 6, &fadt_body)),
        (0x4000, sdt(b"HPET", 1, &hpet_body)),
        (0x5000, sdt(b"MCFG", 1, &mcfg_body)),
        (0x6000, sdt(b"FPDT", 1, &fpdt_body)),
    ];
    let mut xsdt_body = Vec::new();
    for (addr, t) in tables {
        xsdt_body.extend_from_slice(&addr.to_le_bytes());
        f.0.insert(addr, t);
    }
    f.0.insert(0x1000, sdt(b"XSDT", 1, &xsdt_body));
    f.0.insert(0x100, rsdp(0x1000));
    (f, 0x100)
}

#[test]
fn every_table_is_found_and_read() {
    let (mut f, rsdp) = world();
    let list = tables(&mut f, rsdp).unwrap();
    let sigs: Vec<&[u8; 4]> = list.iter().map(|t| &t.signature).collect();
    assert_eq!(sigs, [b"APIC", b"FACP", b"HPET", b"MCFG", b"FPDT"]);
    let get = |f: &mut Fake, s: &[u8; 4]| table(f, list.iter().find(|t| &t.signature == s).unwrap().addr).unwrap();
    let m = madt(&get(&mut f, b"APIC")).unwrap();
    assert_eq!(m, Madt { lapic_addr: 0xFEE0_0000, cpus: 2, ioapics: 1, overrides: 1, has_8259: true });
    // The override maps ISA IRQ 0 to GSI 2; IRQ 1 is itself.
    assert_eq!(isa_gsi(&get(&mut f, b"APIC"), 0), (2, 0));
    assert_eq!(isa_gsi(&get(&mut f, b"APIC"), 1), (1, 0));
    let fa = fadt(&get(&mut f, b"FACP")).unwrap();
    assert!(fa.has_8042);
    assert_eq!(fa.dsdt, 0x7000);
    assert_eq!(hpet(&get(&mut f, b"HPET")), Some(0xFED0_0000));
    assert_eq!(mcfg(&get(&mut f, b"MCFG")), [Ecam { base: 0xB000_0000, segment: 0, start_bus: 0, end_bus: 0xFF }]);
    let fp = get(&mut f, b"FPDT");
    assert_eq!(fpdt(&mut f, &fp), Some(FirmwareTimes { reset_end: 1_000_000, os_loader_start: 9_000_000 }));
}

#[test]
fn a_bad_checksum_or_length_is_refused() {
    let (mut f, rsdp) = world();
    // Corrupt one byte of the MADT: its checksum fails, so it isn't read.
    f.0.get_mut(&0x2000).unwrap()[50] ^= 0xFF;
    assert!(table(&mut f, 0x2000).is_none());
    // A length past the end of what's there.
    f.0.get_mut(&0x4000).unwrap()[4..8].copy_from_slice(&5000u32.to_le_bytes());
    assert!(table(&mut f, 0x4000).is_none());
    // A wrong RSDP signature or checksum: nothing at all.
    let (mut f2, _) = world();
    f2.0.get_mut(&0x100).unwrap()[0] = b'X';
    assert!(tables(&mut f2, rsdp).is_none());
    // A table of the wrong kind isn't parsed as another.
    let (mut f3, _) = world();
    let h = table(&mut f3, 0x4000).unwrap();
    assert!(madt(&h).is_none() && fadt(&h).is_none() && mcfg(&h).is_empty());
}
