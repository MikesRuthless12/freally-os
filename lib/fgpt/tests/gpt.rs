// SPDX-License-Identifier: Apache-2.0
//! The GPT as written and read back: Freally OS's layout, both tables, the
//! CRCs caught when damaged, and the interlock's question answered.
#![cfg(feature = "build")]

use fgpt::build::{Part, guid_from, tables};
use fgpt::{
    GptError, Header, PART_TYPE_ESP, PART_TYPE_FREALLY_DATA, PART_TYPE_FREALLY_SYSTEM, Table, crc32, freally_layout,
};

const BLOCK: u32 = 512;
/// 2 GiB.
const BLOCKS: u64 = 2 << 21;

fn disk() -> (Vec<u8>, Vec<u8>, [fgpt::Planned; 4]) {
    let layout = freally_layout(BLOCKS, BLOCK).unwrap();
    let parts: Vec<Part> = layout
        .iter()
        .enumerate()
        .map(|(i, &(t, name, first, last))| Part {
            type_guid: t,
            unique: guid_from([i as u8 + 1; 16]),
            first_lba: first,
            last_lba: last,
            name,
        })
        .collect();
    let (start, end) = tables(BLOCKS, BLOCK, guid_from([9; 16]), &parts);
    (start, end, layout)
}

#[test]
fn crc32_is_ieee() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn the_freally_layout_is_the_prds() {
    let l = freally_layout(BLOCKS, BLOCK).unwrap();
    let mib = 2048;
    assert_eq!((l[0].0, l[0].2, l[0].3 - l[0].2 + 1), (PART_TYPE_ESP, mib, 64 * mib), "the ESP: 64 MiB at 1 MiB");
    assert_eq!((l[1].0, l[1].1, l[1].3 - l[1].2 + 1), (PART_TYPE_FREALLY_SYSTEM, "FREALLY_SYSTEM_A", 512 * mib));
    assert_eq!((l[2].0, l[2].1, l[2].3 - l[2].2 + 1), (PART_TYPE_FREALLY_SYSTEM, "FREALLY_SYSTEM_B", 512 * mib));
    assert_eq!((l[3].0, l[3].1), (PART_TYPE_FREALLY_DATA, "FREALLY_DATA"));
    for w in l.windows(2) {
        assert_eq!(w[1].2, w[0].3 + 1, "back to back");
    }
    for p in l {
        assert_eq!(p.2 % mib, 0, "{} starts on a whole MiB", p.1);
    }
    assert!(l[3].3 < BLOCKS - 33, "the data ends before the backup table");
    // 4 KiB blocks: the same sizes in bytes.
    let k = freally_layout(BLOCKS / 8, 4096).unwrap();
    assert_eq!((k[0].2, k[1].3 - k[1].2 + 1), (256, 512 * 256));
    // Too small for the three and 64 MiB of data.
    assert!(freally_layout((1 << 30) / 512, BLOCK).is_none());
}

#[test]
fn both_tables_read_back() {
    let (start, end, layout) = disk();
    let bs = BLOCK as usize;
    assert_eq!(&start[510..512], &[0x55, 0xAA], "the protective MBR");
    assert_eq!(start[446 + 4], 0xEE);
    let h = Header::parse(&start[bs..2 * bs], 1, BLOCKS - 1).unwrap();
    assert_eq!((h.alternate_lba, h.first_usable, h.entries_lba), (BLOCKS - 1, 34, 2));
    let t = Table::new(h, &start[2 * bs..]).unwrap();
    let parts: Vec<_> = t.partitions().collect();
    assert_eq!(parts.len(), 4);
    for (p, l) in parts.iter().zip(layout) {
        assert_eq!((p.type_guid, p.first_lba, p.last_lba), (l.0, l.2, l.3));
        assert!(p.named(l.1), "{}", l.1);
    }
    let [a, b] = t.system_slots();
    assert_eq!((a.unwrap().first_lba, b.unwrap().first_lba), (layout[1].2, layout[2].2));
    // The backup: its entries, then its header in the last block.
    let bh = Header::parse(&end[end.len() - bs..], BLOCKS - 1, BLOCKS - 1).unwrap();
    assert_eq!((bh.alternate_lba, bh.entries_lba), (1, BLOCKS - 33));
    let bt = Table::new(bh, &end).unwrap();
    assert_eq!(bt.partitions().count(), 4);
    assert_eq!(guid_from([9; 16]), h.disk_guid);
    assert_eq!(h.disk_guid.data3 >> 12, 4, "a version-4 GUID");
}

#[test]
fn damage_is_caught() {
    let (start, _, _) = disk();
    let bs = BLOCK as usize;
    let mut bad = start[bs..2 * bs].to_vec();
    bad[40] ^= 1;
    assert_eq!(Header::parse(&bad, 1, BLOCKS - 1), Err(GptError::BadCrc));
    assert_eq!(Header::parse(&start[..bs], 1, BLOCKS - 1), Err(GptError::NotGpt), "the MBR isn't a header");
    assert_eq!(Header::parse(&start[bs..2 * bs], 2, BLOCKS - 1), Err(GptError::BadLayout), "read from the wrong LBA");
    assert_eq!(Header::parse(&start[bs..2 * bs], 1, 1000), Err(GptError::BadLayout), "a disk smaller than it says");
    let h = Header::parse(&start[bs..2 * bs], 1, BLOCKS - 1).unwrap();
    let mut entries = start[2 * bs..].to_vec();
    entries[200] ^= 1;
    assert_eq!(Table::new(h, &entries).err(), Some(GptError::BadCrc));
    assert_eq!(Table::new(h, &entries[..100]).err(), Some(GptError::BadLayout), "entries cut short");
}

#[test]
fn the_interlock_answers_by_partition() {
    let (start, _, l) = disk();
    let bs = BLOCK as usize;
    let h = Header::parse(&start[bs..2 * bs], 1, BLOCKS - 1).unwrap();
    let t = Table::new(h, &start[2 * bs..]).unwrap();
    let (data, sys_a, esp) = (l[3], l[1], l[0]);
    assert!(t.freally_owns(data.2, 8));
    assert!(t.freally_owns(data.3, 1), "the data's last block");
    assert!(!t.freally_owns(data.3, 2), "one past the data's end");
    assert!(t.freally_owns(sys_a.2, 1024));
    assert!(!t.freally_owns(sys_a.3, 2), "across two partitions");
    assert!(!t.freally_owns(esp.2, 1), "the ESP isn't Freally's to write");
    assert!(!t.freally_owns(0, 1), "nor the MBR");
    assert!(!t.freally_owns(1, 1), "nor the GPT");
    assert!(!t.freally_owns(BLOCKS - 1, 1), "nor the backup");
    assert!(!t.freally_owns(data.2, 0), "nothing isn't a write");
    assert!(!t.freally_owns(u64::MAX, 2), "no wrap-around");
}

/// TASK-114: a disk grown before its first boot, FREALLY_DATA taking the
/// new room; both tables read back whole at their new places; nothing grown
/// where it isn't Freally OS's to grow.
#[test]
fn a_grown_disk_gives_freally_data_the_room() {
    let (start, _, layout) = disk();
    let header = &start[512..1024];
    let mut entries = start[1024..1024 + 128 * 128].to_vec();
    let bigger = 2 * BLOCKS;
    let g = fgpt::grow(header, &mut entries, bigger, BLOCK).expect("a grown disk");
    let mib = (1 << 20) / BLOCK as u64;
    assert_eq!((g.data_last + 1) % mib, 0, "on a whole MiB");
    assert!(g.data_last > layout[3].3 + BLOCKS - 4 * mib);
    let pad = |h: [u8; 92]| {
        let mut b = vec![0u8; 512];
        b[..92].copy_from_slice(&h);
        b
    };
    let p = Header::parse(&pad(g.primary), 1, bigger - 1).unwrap();
    let t = Table::new(p, &entries).unwrap();
    let parts: Vec<_> = t.partitions().collect();
    assert_eq!(parts[3].last_lba, g.data_last);
    for i in 0..3 {
        assert_eq!((parts[i].first_lba, parts[i].last_lba), (layout[i].2, layout[i].3), "the others as they were");
    }
    let b = Header::parse(&pad(g.backup), bigger - 1, bigger - 1).unwrap();
    assert_eq!((b.alternate_lba, b.entries_lba), (1, g.backup_entries_lba));
    assert_eq!(p.alternate_lba, bigger - 1);
    // The same size: nothing to do. Grown once: nothing more.
    let mut again = start[1024..1024 + 128 * 128].to_vec();
    assert_eq!(fgpt::grow(header, &mut again, BLOCKS, BLOCK), None);
    let mut twice = entries.clone();
    assert_eq!(fgpt::grow(&pad(g.primary), &mut twice, bigger, BLOCK), None);
}

#[test]
fn a_disk_not_ours_or_damaged_isnt_grown() {
    let (start, _, _) = disk();
    // Damaged: a flipped byte in the header.
    let mut header = start[512..1024].to_vec();
    header[60] ^= 1;
    let mut entries = start[1024..1024 + 128 * 128].to_vec();
    assert_eq!(fgpt::grow(&header, &mut entries, 2 * BLOCKS, BLOCK), None);
    // Its last partition not FREALLY_DATA (another system's disk).
    let layout = freally_layout(BLOCKS, BLOCK).unwrap();
    let parts: Vec<Part> = layout
        .iter()
        .take(3)
        .enumerate()
        .map(|(i, &(t, name, first, last))| Part {
            type_guid: t,
            unique: guid_from([i as u8 + 1; 16]),
            first_lba: first,
            last_lba: last,
            name,
        })
        .collect();
    let (start, _) = tables(BLOCKS, BLOCK, guid_from([9; 16]), &parts);
    let mut entries = start[1024..1024 + 128 * 128].to_vec();
    assert_eq!(fgpt::grow(&start[512..1024], &mut entries, 2 * BLOCKS, BLOCK), None);
}
