// SPDX-License-Identifier: Apache-2.0
//! `ffsck <image>` (TASK-096): checks a FreallyFS volume in a file, a raw
//! volume or a GPT disk image (its FREALLY_DATA partition), and prints
//! every finding; exits 1 if there are any. It never writes: the store
//! refuses every write.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use svc_ffs::fsck::Report;
use svc_ffs::store::Store;
use svc_ffs::volume::Volume;
use svc_ffs::{BLOCK, FsError};

/// A volume in a file from byte `base`, read-only.
struct ReadOnlyFile {
    file: File,
    base: u64,
    blocks: u64,
}

impl Store for ReadOnlyFile {
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<(), FsError> {
        self.file.seek(SeekFrom::Start(self.base + block * BLOCK as u64)).map_err(|_| FsError::Io)?;
        self.file.read_exact(buf).map_err(|_| FsError::Io)
    }
    fn write(&mut self, _: u64, _: &[u8; BLOCK]) -> Result<(), FsError> {
        Err(FsError::ReadOnly)
    }
    fn flush(&mut self) -> Result<(), FsError> {
        Ok(())
    }
}

/// The volume in `path`: FREALLY_DATA when it's a GPT disk image, else the whole file.
fn open(path: &str) -> Result<ReadOnlyFile, String> {
    let mut file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut head = vec![0u8; 34 * 512];
    if len >= head.len() as u64 {
        file.read_exact(&mut head).map_err(|e| e.to_string())?;
        if let Ok(h) = fgpt::Header::parse(&head[512..1024], 1, len / 512 - 1)
            && let Ok(t) = fgpt::Table::new(h, &head[1024..])
        {
            let data = t
                .partitions()
                .find(|p| p.type_guid == fgpt::PART_TYPE_FREALLY_DATA)
                .ok_or(format!("{path}: a GPT disk with no FREALLY_DATA partition"))?;
            return Ok(ReadOnlyFile { file, base: data.first_lba * 512, blocks: data.blocks() * 512 / BLOCK as u64 });
        }
    }
    Ok(ReadOnlyFile { file, base: 0, blocks: len / BLOCK as u64 })
}

fn check(path: &str) -> Result<Report, String> {
    let mut v = Volume::mount(open(path)?).map_err(|e| format!("{path}: can't mount: {e:?}"))?;
    v.fsck().map_err(|e| format!("{path}: {e:?}"))
}

fn main() -> std::process::ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("ffsck <image>: a FreallyFS volume, or a GPT disk image with FREALLY_DATA");
        return std::process::ExitCode::from(2);
    };
    match check(&path) {
        Ok(r) if r.is_clean() => {
            println!("ffsck: {path}: clean ({} nodes, {} data blocks, {} items)", r.nodes, r.data, r.items);
            std::process::ExitCode::SUCCESS
        }
        Ok(r) => {
            for f in &r.findings {
                match f.block {
                    Some(b) => println!("block {b}: {}", f.what),
                    None => println!("{}", f.what),
                }
            }
            println!("ffsck: {path}: {} finding(s)", r.findings.len());
            std::process::ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("ffsck: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use svc_ffs::inode::{FileKind, ROOT_INO};
    use svc_ffs::store::MemStore;

    #[test]
    fn a_volume_file_checks_clean_and_damage_is_found() {
        let mut v = Volume::format(MemStore::new(1024), [4; 16]).unwrap();
        let f = v.create(svc_ffs::subvol::SYSTEM, ROOT_INO, "notes", FileKind::File).unwrap();
        v.write(svc_ffs::subvol::SYSTEM, f, 0, &[42; 10_000]).unwrap();
        let store = v.unmount().unwrap();
        let path = std::env::temp_dir().join(format!("ffsck-test-{}.ffs", std::process::id()));
        let mut out = File::create(&path).unwrap();
        for b in 0..1024 {
            out.write_all(store.peek(b)).unwrap();
        }
        drop(out);
        let p = path.to_str().unwrap();
        let r = check(p).unwrap();
        assert!(r.is_clean() && r.data == 3, "{:?}", r);
        // A data block damaged in the file.
        let mut bytes = std::fs::read(&path).unwrap();
        let data_block = (17..1024).rev().find(|&b| bytes[b * 4096..(b + 1) * 4096].contains(&42)).unwrap();
        bytes[data_block * 4096 + 9] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(check(p).unwrap().says("fails its check"));
        let _ = std::fs::remove_file(&path);
    }
}
