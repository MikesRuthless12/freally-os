// SPDX-License-Identifier: Apache-2.0
//! FreallyFS part 4 (TASK-098): encrypted subvolumes. File names and
//! contents never reach the raw disk (a plain subvolume's do: the control);
//! a locked subvolume can't be read, a wrong key is told apart, the right
//! one reads everything back; `ffsck` checks with the key and, without it,
//! judges nothing it can't read; a flipped bit anywhere is `Integrity` or
//! harmless, never different data; a snapshot reads what its source sealed.

use svc_ffs::FsError;
use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::{MemStore, Store};
use svc_ffs::subvol::SYSTEM;
use svc_ffs::volume::Volume;

const KEY: [u8; 32] = [0x4B; 32];
const NAME: &str = "secret-plan-for-the-launch.txt";
const DIR: &str = "Taxes and receipts 2026";

fn content() -> Vec<u8> {
    b"the eagle lands at dawn; bring the blue folder. ".repeat(300)
}

/// A volume with an encrypted home holding a directory and a file (and a
/// second file over several blocks); committed. Its home's id.
fn build() -> (Volume<MemStore>, u64) {
    let mut v = Volume::format(MemStore::new(2048), [1; 16]).unwrap();
    let home = v.create_encrypted_subvol("home", [7; 16], &KEY).unwrap();
    let d = v.create(home, ROOT_INO, DIR, FileKind::Dir).unwrap();
    let f = v.create(home, d, NAME, FileKind::File).unwrap();
    v.write(home, f, 0, &content()).unwrap();
    let g = v.create(home, ROOT_INO, "notes", FileKind::File).unwrap();
    v.write(home, g, 5000, b"a few words").unwrap();
    v.commit().unwrap();
    (v, home)
}

fn read_all(v: &mut Volume<MemStore>, home: u64) -> Result<(Vec<u8>, Vec<u8>), FsError> {
    let d = v.lookup(home, ROOT_INO, DIR)?.ok_or(FsError::NotFound)?.child;
    let f = v.lookup(home, d, NAME)?.ok_or(FsError::NotFound)?.child;
    let g = v.lookup(home, ROOT_INO, "notes")?.ok_or(FsError::NotFound)?.child;
    let (fs, gs) = (v.stat(home, f)?.size, v.stat(home, g)?.size);
    Ok((v.read(home, f, 0, fs)?, v.read(home, g, 0, gs)?))
}

fn on_disk(s: &MemStore, needle: &[u8]) -> bool {
    (0..s.blocks()).any(|b| s.peek(b).windows(needle.len()).any(|w| w == needle))
}

#[test]
fn names_and_contents_never_reach_the_disk() {
    let (mut v, _) = build();
    // The control: the same in the plain system subvolume is found.
    let f = v.create(SYSTEM, ROOT_INO, "plain-control-name.txt", FileKind::File).unwrap();
    v.write(SYSTEM, f, 0, b"plain control contents").unwrap();
    v.commit().unwrap();
    let s = v.into_store();
    assert!(on_disk(&s, b"plain-control-name.txt") && on_disk(&s, b"plain control contents"), "the scan works");
    for needle in [NAME.as_bytes(), DIR.as_bytes(), b"the eagle lands", b"a few words"] {
        assert!(!on_disk(&s, needle), "{:?} is on the raw disk", String::from_utf8_lossy(needle));
    }
}

#[test]
fn locked_wrong_key_right_key() {
    let (v, home) = build();
    let mut v = Volume::mount(v.into_store()).unwrap();
    assert!(v.is_locked(home).unwrap());
    assert_eq!(read_all(&mut v, home).err(), Some(FsError::Locked));
    let r = v.fsck().unwrap();
    assert!(r.is_clean() && r.locked == vec![home], "without the key: {r:?}");
    assert_eq!(v.unlock(home, &[0x4C; 32]), Err(FsError::WrongKey));
    v.unlock(home, &KEY).unwrap();
    assert_eq!(read_all(&mut v, home).unwrap(), (content(), [vec![0; 5000], b"a few words".to_vec()].concat()));
    let r = v.fsck().unwrap();
    assert!(r.is_clean() && r.locked.is_empty(), "with the key: {:?}", r.findings);
    // Changes go on sealed; lock commits them and forgets the key.
    let g = v.lookup(home, ROOT_INO, "notes").unwrap().unwrap().child;
    v.write(home, g, 0, b"changed").unwrap();
    v.lock(home).unwrap();
    assert_eq!(v.read(home, g, 0, 7).err(), Some(FsError::Locked));
    v.unlock(home, &KEY).unwrap();
    assert_eq!(v.read(home, g, 0, 7).unwrap(), b"changed");
    // An encrypted snapshot reads what its source sealed.
    let snap = v.snapshot(home, "before").unwrap();
    v.write(home, g, 0, b"after!!").unwrap();
    v.commit().unwrap();
    assert_eq!(v.read(snap, g, 0, 7).unwrap(), b"changed");
    let mut v = Volume::mount(v.into_store()).unwrap();
    assert_eq!(v.read(snap, g, 0, 7).err(), Some(FsError::Locked));
    v.unlock(snap, &KEY).unwrap();
    assert_eq!(v.read(snap, g, 0, 7).unwrap(), b"changed");
}

#[test]
fn a_flipped_bit_is_never_different_data() {
    let (v, home) = build();
    let s = v.into_store();
    let used: Vec<u64> = (16..s.blocks()).filter(|&b| s.peek(b).iter().any(|&x| x != 0)).collect();
    let mut caught = 0;
    for &b in &used {
        let (v, _) = build();
        let mut s = v.into_store();
        s.flip(b, 1234);
        let Ok(mut m) = Volume::mount(s) else {
            caught += 1;
            continue;
        };
        match m.unlock(home, &KEY) {
            Ok(()) => {}
            // The root tree's node holding the record: damage, never a wrong key.
            Err(FsError::Integrity(_)) => {
                caught += 1;
                continue;
            }
            Err(e) => panic!("block {b} flipped: unlock said {e:?}"),
        }
        match read_all(&mut m, home) {
            Ok((f, g)) => {
                assert_eq!(f, content(), "block {b} flipped: different data, silently");
                assert_eq!(&g[5000..], b"a few words");
            }
            Err(FsError::Integrity(_)) | Err(FsError::Corrupt(_)) => caught += 1,
            Err(e) => panic!("block {b} flipped: {e:?}"),
        }
    }
    // Every block of the home (its nodes and its three data blocks... and
    // more) is caught; the rest are other trees'.
    assert!(caught >= 5, "{caught} of {} flips caught", used.len());
}
