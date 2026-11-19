// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask image --gpt` (TASK-091): a whole disk in Freally OS's GPT
//! layout (PRD § 3): the ESP (FAT32, the bootloader and, for dev and e2e,
//! its stand-in EDID and boot options), FREALLY_SYSTEM_A and _B (each slot's
//! raw `.fsi`, or nothing), FREALLY_DATA (empty: the welcome screen formats
//! it). The partitions' GUIDs come from the slot A image's hash, so a build
//! makes the same disk every time. The file is sparse: what isn't written
//! takes no room.

use crate::{build, fat, pack, util};
use fgpt::build::{Part, guid_from, tables};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The disks' block size.
const BLOCK: u32 = 512;
/// The data partition's size when none is asked for: 1 GiB.
pub const DATA_MIB: u64 = 1024;

/// What goes on the disk.
pub struct Disk<'a> {
    pub efi: &'a [u8],
    pub slot_a: &'a [u8],
    pub slot_b: Option<&'a [u8]>,
    /// More files for the ESP (`FREALLY/EDID.BIN`, `FREALLY/CMDLINE.TXT`).
    pub esp_files: Vec<(&'a str, &'a [u8])>,
    pub data_mib: u64,
}

/// Writes `d` to `path` as a GPT disk: 1 MiB for the table, the ESP, both
/// slots, the data partition, and 1 MiB for the backup table.
pub fn write_disk(path: &Path, d: &Disk) -> Result<(), String> {
    let mib = (1u64 << 20) / BLOCK as u64;
    let blocks = (2 + 64 + 2 * 512 + d.data_mib) * mib;
    let layout = fgpt::freally_layout(blocks, BLOCK).ok_or("the disk is too small for Freally OS's layout")?;
    let room = |i: usize| (layout[i].3 - layout[i].2 + 1) * BLOCK as u64;
    for (i, slot) in [(1, Some(d.slot_a)), (2, d.slot_b)] {
        if slot.is_some_and(|s| s.len() as u64 > room(i)) {
            return Err(format!("{}'s image is larger than its {} MiB partition", layout[i].1, room(i) >> 20));
        }
    }
    let seed = fcrypto::sha2::sha256(d.slot_a);
    let guid = |what: &str| {
        let mut b = [0u8; 16];
        b.copy_from_slice(&fcrypto::sha2::sha256(&[&seed[..], what.as_bytes()].concat())[..16]);
        guid_from(b)
    };
    let parts: Vec<Part> = layout
        .iter()
        .map(|&(type_guid, name, first_lba, last_lba)| Part {
            type_guid,
            unique: guid(name),
            first_lba,
            last_lba,
            name,
        })
        .collect();
    let (start, end) = tables(blocks, BLOCK, guid("disk"), &parts);
    let at = |lba: u64| lba * BLOCK as u64;
    let mut files = vec![("EFI/BOOT/BOOTX64.EFI", d.efi)];
    files.extend(d.esp_files.iter().copied());
    let volume_id = u32::from_le_bytes([seed[0], seed[1], seed[2], seed[3]]);
    let esp = fat::fat32(room(0), layout[0].2 as u32, b"FREALLY ESP", volume_id, &files)?;

    let mut f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.set_len(at(blocks)).map_err(|e| e.to_string())?;
    let mut put = |offset: u64, bytes: &[u8]| -> Result<(), String> {
        f.seek(SeekFrom::Start(offset)).and_then(|_| f.write_all(bytes)).map_err(|e| format!("{}: {e}", path.display()))
    };
    put(0, &start)?;
    put(at(layout[0].2), &esp)?;
    put(at(layout[1].2), d.slot_a)?;
    if let Some(b) = d.slot_b {
        put(at(layout[2].2), b)?;
    }
    put(at(blocks) - end.len() as u64, &end)?;
    Ok(())
}

/// The disk for a built flavour, with what a VM's ESP folder holds.
pub fn disk_from_esp(esp: &Path, out: &Path, data_mib: u64) -> Result<(), String> {
    let read = |rel: &str| std::fs::read(esp.join(rel)).ok();
    let efi = read("EFI/BOOT/BOOTX64.EFI").ok_or("the ESP folder has no bootloader")?;
    let a = read("FREALLY/SYSTEM_A.FSI").ok_or("the ESP folder has no slot A image")?;
    let b = read("FREALLY/SYSTEM_B.FSI");
    let (edid, cmdline) = (read("FREALLY/EDID.BIN"), read("FREALLY/CMDLINE.TXT"));
    let mut esp_files = Vec::new();
    if let Some(e) = &edid {
        esp_files.push(("FREALLY/EDID.BIN", e.as_slice()));
    }
    if let Some(c) = &cmdline {
        esp_files.push(("FREALLY/CMDLINE.TXT", c.as_slice()));
    }
    write_disk(out, &Disk { efi: &efi, slot_a: &a, slot_b: b.as_deref(), esp_files, data_mib })
}

/// `cargo xtask image --gpt [--flavour F] [--out P] [--data-mib N]`.
pub fn image_cmd(args: &[String]) -> Result<(), String> {
    if !args.iter().any(|a| a == "--gpt") {
        return Err(
            "cargo xtask image --gpt [--flavour F] [--out P] [--data-mib N]: only the GPT layout is made this way"
                .into(),
        );
    }
    let flavour = pack::flavour_arg(args)?;
    let arg = |n: &str| util::flag(args, n).map(String::from);
    let data_mib = match arg("--data-mib") {
        Some(v) => v.parse().map_err(|_| "--data-mib takes a number")?,
        None => DATA_MIB,
    };
    let out = arg("--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| util::target_dir().join(format!("freally-{}.disk", flavour.name())));
    let built = build::build(flavour)?;
    let efi = std::fs::read(&built.efi).map_err(|e| e.to_string())?;
    let a = std::fs::read(&built.image).map_err(|e| e.to_string())?;
    write_disk(&out, &Disk { efi: &efi, slot_a: &a, slot_b: None, esp_files: Vec::new(), data_mib })?;
    println!(
        "image: {} ({} GiB, GPT: ESP, FREALLY_SYSTEM_A and _B, FREALLY_DATA)",
        out.display(),
        (2 + 64 + 1024 + data_mib) >> 10
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fgpt::{Header, Table};
    use std::io::Read;

    /// Two files compared a MiB at a time (never a GiB in memory).
    fn same_file(a: &Path, b: &Path) -> bool {
        let (mut fa, mut fb) = (std::fs::File::open(a).unwrap(), std::fs::File::open(b).unwrap());
        let (mut x, mut y) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
        loop {
            let n = fa.read(&mut x).unwrap();
            if n == 0 {
                return fb.read(&mut y).unwrap() == 0;
            }
            if fb.read_exact(&mut y[..n]).is_err() || x[..n] != y[..n] {
                return false;
            }
        }
    }

    #[test]
    fn a_disk_holds_the_layout_the_slots_and_the_bootloader() {
        // On disk, not /tmp (a RAM disk under WSL): the file is over a GiB, sparse.
        let dir = util::target_dir().join(format!("disk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.disk");
        let (efi, a) = (vec![0x4Du8; 5000], vec![0xA5u8; 70_000]);
        write_disk(
            &path,
            &Disk { efi: &efi, slot_a: &a, slot_b: None, esp_files: vec![("FREALLY/CMDLINE.TXT", b"x")], data_mib: 64 },
        )
        .unwrap();
        let mut f = std::fs::File::open(&path).unwrap();
        let len = f.metadata().unwrap().len();
        assert_eq!(len, (2 + 64 + 1024 + 64) << 20);
        let mut head = vec![0u8; 34 * 512];
        f.read_exact(&mut head).unwrap();
        let blocks = len / 512;
        let h = Header::parse(&head[512..1024], 1, blocks - 1).unwrap();
        let t = Table::new(h, &head[1024..]).unwrap();
        let [sa, sb] = t.system_slots();
        let (sa, sb) = (sa.unwrap(), sb.unwrap());
        let mut got = vec![0u8; a.len()];
        f.seek(SeekFrom::Start(sa.first_lba * 512)).unwrap();
        f.read_exact(&mut got).unwrap();
        assert_eq!(got, a, "slot A's image at its partition's start");
        let mut b = [1u8; 4096];
        f.seek(SeekFrom::Start(sb.first_lba * 512)).unwrap();
        f.read_exact(&mut b).unwrap();
        assert!(b.iter().all(|&x| x == 0), "slot B empty");
        // The same input, the same disk.
        let again = dir.join("t2.disk");
        write_disk(
            &again,
            &Disk { efi: &efi, slot_a: &a, slot_b: None, esp_files: vec![("FREALLY/CMDLINE.TXT", b"x")], data_mib: 64 },
        )
        .unwrap();
        assert!(same_file(&path, &again), "the same disk, byte for byte");
        // A slot image larger than its partition is refused (before it's
        // read: the zero pages are never touched).
        let huge = vec![0u8; (512 << 20) + 1];
        assert!(
            write_disk(&path, &Disk { efi: &efi, slot_a: &huge, slot_b: None, esp_files: vec![], data_mib: 64 })
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
