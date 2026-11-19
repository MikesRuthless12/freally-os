// SPDX-License-Identifier: Apache-2.0
//! A FAT32 volume, written from Microsoft's FAT specification (TASK-091):
//! the GPT layout's ESP, holding the bootloader and (dev and e2e) its
//! stand-in EDID and boot options. The firmware reads it; Freally OS never
//! does. Short (8.3) names only, every date 1980-01-01, the volume ID from
//! the caller: the same files make the same bytes.

/// The bytes a sector holds.
const SECTOR: usize = 512;
/// Reserved sectors before the first FAT (the boot sector, FSInfo, the backup at 6).
const RESERVED: u32 = 32;
/// The smallest cluster count a FAT32 volume may have.
const MIN_CLUSTERS: u32 = 65_525;
/// End of a cluster chain.
const EOC: u32 = 0x0FFF_FFFF;
/// 1980-01-01, FAT's first day.
const DATE: u16 = (1 << 5) | 1;

/// Makes one directory entry: its name, attributes, first cluster and size.
type EntryFn<'f> = dyn Fn(&[u8; 11], u8, u32, u32) -> [u8; 32] + 'f;

enum Node<'a> {
    Dir(Vec<([u8; 11], Node<'a>)>),
    File(&'a [u8]),
}

/// "BOOTX64.EFI" as its 11 bytes: the name and the extension, space-padded.
fn short_name(name: &str) -> Result<[u8; 11], String> {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let ok = |s: &str, max| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    if !ok(base, 8) || !(ext.is_empty() || ok(ext, 3)) {
        return Err(format!("{name} isn't an 8.3 name in capitals"));
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Ok(out)
}

fn insert<'a>(dir: &mut Vec<([u8; 11], Node<'a>)>, path: &[&str], data: &'a [u8]) -> Result<(), String> {
    let name = short_name(path[0])?;
    if path.len() == 1 {
        if dir.iter().any(|(n, _)| *n == name) {
            return Err(format!("{} is there twice", path[0]));
        }
        dir.push((name, Node::File(data)));
        return Ok(());
    }
    if !dir.iter().any(|(n, _)| *n == name) {
        dir.push((name, Node::Dir(Vec::new())));
    }
    match dir.iter_mut().find(|(n, _)| *n == name) {
        Some((_, Node::Dir(sub))) => insert(sub, &path[1..], data),
        _ => Err(format!("{} is a file, not a folder", path[0])),
    }
}

/// A FAT32 volume `bytes` long (a whole number of sectors) holding `files`
/// (paths like `EFI/BOOT/BOOTX64.EFI`), labelled `label` (11 characters),
/// for a partition starting at sector `hidden` of its disk.
pub fn fat32(
    bytes: u64,
    hidden: u32,
    label: &[u8; 11],
    volume_id: u32,
    files: &[(&str, &[u8])],
) -> Result<Vec<u8>, String> {
    let total = (bytes / SECTOR as u64) as u32;
    let spc: u32 = if bytes <= 260 << 20 { 1 } else { 8 };
    let cluster = spc as usize * SECTOR;
    // The FATs' size: the first big enough for the clusters left beside
    // them (from one sector up, so it settles; never back and forth).
    let mut fat_sectors = 1u32;
    let clusters = loop {
        let left = total.checked_sub(RESERVED + 2 * fat_sectors).ok_or("the volume is too small for FAT32")?;
        let clusters = left / spc;
        let need = ((clusters + 2) * 4).div_ceil(SECTOR as u32);
        if need <= fat_sectors {
            break clusters;
        }
        fat_sectors = need;
    };
    if clusters < MIN_CLUSTERS {
        return Err(format!("{bytes} bytes is too small for FAT32 ({clusters} clusters)"));
    }
    let mut root = Vec::new();
    for (path, data) in files {
        insert(&mut root, &path.split('/').collect::<Vec<_>>(), data)?;
    }

    let mut img = vec![0u8; bytes as usize];
    let mut fat = vec![0u32; clusters as usize + 2];
    fat[0] = 0x0FFF_FFF8;
    fat[1] = EOC;
    let mut next = 2u32;
    let mut take = |n: usize| -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        let first = next;
        if (first as usize + n) > fat.len() {
            return Err("the files don't fit the volume".into());
        }
        for i in 0..n as u32 {
            fat[(first + i) as usize] = if i + 1 == n as u32 { EOC } else { first + i + 1 };
        }
        next += n as u32;
        Ok(first)
    };
    let data_start = (RESERVED + 2 * fat_sectors) as usize * SECTOR;
    let at = |c: u32| data_start + (c as usize - 2) * cluster;
    let entry = |name: &[u8; 11], attr: u8, first: u32, size: u32| {
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(name);
        e[11] = attr;
        for o in [16, 18, 24] {
            e[o..o + 2].copy_from_slice(&DATE.to_le_bytes());
        }
        e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        e
    };

    // Each folder: its entries (`.` and `..` below the root), each child's
    // clusters taken as it's written.
    #[allow(clippy::too_many_arguments)]
    fn write<'a>(
        dir: &[([u8; 11], Node<'a>)],
        me: u32,
        parent: u32,
        label: Option<&[u8; 11]>,
        img: &mut [u8],
        take: &mut dyn FnMut(usize) -> Result<u32, String>,
        at: &dyn Fn(u32) -> usize,
        entry: &EntryFn,
        cluster: usize,
    ) -> Result<(), String> {
        let mut entries: Vec<[u8; 32]> = Vec::new();
        if let Some(l) = label {
            entries.push(entry(l, 0x08, 0, 0));
        } else {
            entries.push(entry(b".          ", 0x10, me, 0));
            entries.push(entry(b"..         ", 0x10, parent, 0));
        }
        let mut subs = Vec::new();
        for (name, node) in dir {
            match node {
                Node::File(data) => {
                    let first = take(data.len().div_ceil(cluster))?;
                    img[at(first.max(2))..][..data.len()].copy_from_slice(data);
                    let size = u32::try_from(data.len()).map_err(|_| "a file over 4 GiB")?;
                    entries.push(entry(name, 0x20, first, size));
                }
                Node::Dir(sub) => {
                    let len = (sub.len() + 2) * 32;
                    let first = take(len.div_ceil(cluster))?;
                    entries.push(entry(name, 0x10, first, 0));
                    subs.push((sub, first));
                }
            }
        }
        let bytes: Vec<u8> = entries.concat();
        img[at(me)..][..bytes.len()].copy_from_slice(&bytes);
        for (sub, first) in subs {
            // `..` of a folder in the root names cluster 0.
            let up = if label.is_some() { 0 } else { me };
            write(sub, first, up, None, img, take, at, entry, cluster)?;
        }
        Ok(())
    }
    let root_first = take(((root.len() + 1) * 32).div_ceil(cluster))?;
    write(&root, root_first, 0, Some(label), &mut img, &mut take, &at, &entry, cluster)?;
    let used = next - 2;

    // The boot sector (and its backup at 6), FSInfo at 1.
    let mut bs = [0u8; SECTOR];
    bs[..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    bs[3..11].copy_from_slice(b"FREALLY ");
    bs[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    bs[13] = spc as u8;
    bs[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    bs[16] = 2;
    bs[21] = 0xF8;
    bs[24..26].copy_from_slice(&32u16.to_le_bytes());
    bs[26..28].copy_from_slice(&64u16.to_le_bytes());
    bs[28..32].copy_from_slice(&hidden.to_le_bytes());
    bs[32..36].copy_from_slice(&total.to_le_bytes());
    bs[36..40].copy_from_slice(&fat_sectors.to_le_bytes());
    bs[44..48].copy_from_slice(&root_first.to_le_bytes());
    bs[48..50].copy_from_slice(&1u16.to_le_bytes());
    bs[50..52].copy_from_slice(&6u16.to_le_bytes());
    bs[64] = 0x80;
    bs[66] = 0x29;
    bs[67..71].copy_from_slice(&volume_id.to_le_bytes());
    bs[71..82].copy_from_slice(label);
    bs[82..90].copy_from_slice(b"FAT32   ");
    bs[510] = 0x55;
    bs[511] = 0xAA;
    let mut info = [0u8; SECTOR];
    info[..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    info[488..492].copy_from_slice(&(clusters - used).to_le_bytes());
    info[492..496].copy_from_slice(&next.to_le_bytes());
    info[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
    for s in [0usize, 6] {
        img[s * SECTOR..(s + 1) * SECTOR].copy_from_slice(&bs);
        img[(s + 1) * SECTOR..(s + 2) * SECTOR].copy_from_slice(&info);
    }
    let fat_bytes: Vec<u8> = fat.iter().flat_map(|v| v.to_le_bytes()).collect();
    for f in 0..2usize {
        let off = (RESERVED as usize + f * fat_sectors as usize) * SECTOR;
        img[off..off + fat_bytes.len()].copy_from_slice(&fat_bytes);
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads `path` back the way firmware would: the BPB, the root's cluster,
    /// each folder's entries, the cluster chain.
    fn read(img: &[u8], path: &str) -> Option<Vec<u8>> {
        let u16_at = |o: usize| u16::from_le_bytes([img[o], img[o + 1]]) as u32;
        let u32_at = |o: usize| u32::from_le_bytes([img[o], img[o + 1], img[o + 2], img[o + 3]]);
        let (bps, spc, reserved, fats, fat_sz, root) =
            (u16_at(11), img[13] as u32, u16_at(14), img[16] as u32, u32_at(36), u32_at(44));
        let data = (reserved + fats * fat_sz) * bps;
        let csize = (spc * bps) as usize;
        let chain = |mut c: u32| {
            let mut out = Vec::new();
            while (2..0x0FFF_FFF8).contains(&c) {
                let at = (data as usize) + (c as usize - 2) * csize;
                out.extend_from_slice(&img[at..at + csize]);
                c = u32_at((reserved * bps + c * 4) as usize) & 0x0FFF_FFFF;
            }
            out
        };
        let mut dir = chain(root);
        let parts: Vec<&str> = path.split('/').collect();
        for (i, p) in parts.iter().enumerate() {
            let name = short_name(p).ok()?;
            let e = dir.chunks(32).take_while(|e| e[0] != 0).find(|e| e[..11] == name)?.to_vec();
            let first = u16::from_le_bytes([e[20], e[21]]) as u32 * 65536 + u16::from_le_bytes([e[26], e[27]]) as u32;
            let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]) as usize;
            let body = chain(first);
            if i + 1 == parts.len() {
                return Some(body[..size].to_vec());
            }
            dir = body;
        }
        None
    }

    #[test]
    fn files_read_back_as_firmware_reads_them() {
        let efi: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let files: [(&str, &[u8]); 4] = [
            ("EFI/BOOT/BOOTX64.EFI", &efi),
            ("FREALLY/EDID.BIN", &[0x00, 0xFF, 0xFF, 0xFF]),
            ("FREALLY/CMDLINE.TXT", b"log=screen"),
            ("FREALLY/EMPTY.TXT", b""),
        ];
        let img = fat32(64 << 20, 2048, b"FREALLY ESP", 0x1234_5678, &files).unwrap();
        assert_eq!(img.len(), 64 << 20);
        for (path, data) in files {
            assert_eq!(read(&img, path).as_deref(), Some(data), "{path}");
        }
        assert_eq!(read(&img, "EFI/BOOT/MISSING.EFI"), None);
        // The fields a firmware checks.
        assert_eq!((&img[82..90], &img[510..512]), (&b"FAT32   "[..], &[0x55, 0xAA][..]));
        assert_eq!(u32::from_le_bytes(img[28..32].try_into().unwrap()), 2048, "hidden sectors: the partition's start");
        assert_eq!(&img[6 * 512..7 * 512], &img[..512], "the backup boot sector");
        let clusters = (131_072 - 32 - 2 * u32::from_le_bytes(img[36..40].try_into().unwrap())) as u64;
        assert!(clusters >= MIN_CLUSTERS as u64, "{clusters} clusters: FAT32, not FAT16");
        // The same files, the same bytes.
        assert_eq!(img, fat32(64 << 20, 2048, b"FREALLY ESP", 0x1234_5678, &files).unwrap());
    }

    #[test]
    fn names_are_8_3_capitals() {
        assert_eq!(&short_name("BOOTX64.EFI").unwrap(), b"BOOTX64 EFI");
        assert_eq!(&short_name("EFI").unwrap(), b"EFI        ");
        for bad in ["bootx64.efi", "TOOLONGNAME.EFI", "A.EFIX", "", ".EFI", "A B"] {
            assert!(short_name(bad).is_err(), "{bad}");
        }
        assert!(fat32(1 << 20, 0, b"FREALLY ESP", 0, &[]).is_err(), "1 MiB is too small for FAT32");
        let twice: [(&str, &[u8]); 2] = [("A.TXT", b"1"), ("A.TXT", b"2")];
        assert!(fat32(64 << 20, 0, b"FREALLY ESP", 0, &twice).is_err());
    }
}
