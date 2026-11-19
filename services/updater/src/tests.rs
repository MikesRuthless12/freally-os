// SPDX-License-Identifier: Apache-2.0
use super::*;
use alloc::vec;
use fsi::{ImageBuilder, Kind, sign};

const SEED: [u8; 32] = [42; 32];
const NOTES: &str = "# 0.4.1\n\n- Copies check themselves.\n";

/// A slot in RAM.
struct Mem {
    bytes: Vec<u8>,
    writes: u32,
    /// Writes after this many fail.
    fail_after: Option<u32>,
    /// Every read flips a byte (a disk that lies).
    lies: bool,
}

impl Mem {
    fn new(blocks: usize) -> Mem {
        Mem { bytes: vec![0xEE; blocks * BLOCK], writes: 0, fail_after: None, lies: false }
    }
}

impl Slot for Mem {
    fn blocks(&self) -> u64 {
        (self.bytes.len() / BLOCK) as u64
    }
    fn write(&mut self, block: u64, bytes: &[u8]) -> Result<(), String> {
        assert!(bytes.len().is_multiple_of(BLOCK) && bytes.len() <= RUN * BLOCK, "whole blocks, a run at most");
        if self.fail_after.is_some_and(|n| self.writes >= n) {
            return Err(String::from("the disk said no"));
        }
        self.writes += 1;
        let at = block as usize * BLOCK;
        self.bytes[at..at + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
    fn read(&mut self, block: u64, count: u32) -> Result<Vec<u8>, String> {
        let at = block as usize * BLOCK;
        let mut b = self.bytes[at..at + count as usize * BLOCK].to_vec();
        if self.lies {
            b[0] ^= 1;
        }
        Ok(b)
    }
    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn image(version: &str) -> Vec<u8> {
    let public = fcrypto::ed25519::public_key(&SEED);
    let mut b = ImageBuilder::new(2, version, [7; 20], 1_763_424_000);
    b.add("/system/version", Kind::Data, 0, alloc::format!("Freally OS {version}\n").into_bytes());
    b.add("/system/NOTICE", Kind::Data, 0, (0..200_000u32).map(|i| (i % 251) as u8).collect());
    let mut image = b.build(&public).unwrap();
    sign(&mut image, &SEED).unwrap();
    image
}

fn trusted() -> Vec<[u8; 32]> {
    vec![fcrypto::ed25519::public_key(&SEED)]
}

fn package() -> (Vec<u8>, Vec<u8>) {
    let img = image("0.4.1");
    (fsi::fup::make("", NOTES, &img, &SEED).unwrap(), img)
}

/// The package sent as Settings sends it: the head, then pieces of `size`.
fn send(f: &[u8], slot: &mut Mem, size: usize) -> Result<String, Refusal> {
    let head_len = HEADER + NOTES.len();
    let mut i = Install::begin(&f[..head_len], &trusted(), 2, "0.4.0", slot)?;
    let image = &f[head_len..];
    let mut at = 0;
    while at < image.len() {
        let n = size.min(image.len() - at);
        i.piece(at as u64, &image[at..at + n], slot)?;
        at += n;
    }
    i.finish(slot)
}

#[test]
fn an_update_lands_whole_in_any_size_of_piece() {
    for size in [PIECE, 4096, 1000, 77_777.min(PIECE)] {
        let (f, img) = package();
        let mut slot = Mem::new(128);
        assert_eq!(send(&f, &mut slot, size).as_deref(), Ok("0.4.1"), "pieces of {size}");
        assert_eq!(&slot.bytes[..img.len()], &img[..], "pieces of {size}");
        fsi::verify(&slot.bytes[..img.len()], &trusted()).unwrap();
    }
}

#[test]
fn a_changed_image_leaves_no_header_in_the_slot() {
    let (mut f, _) = package();
    let n = f.len();
    f[n - 9] ^= 1;
    let mut slot = Mem::new(128);
    assert_eq!(send(&f, &mut slot, PIECE), Err(Refusal::Fup(FupError::Changed)));
    assert!(slot.bytes[..BLOCK].iter().all(|&b| b == 0), "the first block cleared, never written");
}

#[test]
fn a_disk_that_lies_or_fails_is_caught() {
    let (f, _) = package();
    let mut lying = Mem::new(128);
    lying.lies = true;
    assert!(matches!(send(&f, &mut lying, PIECE), Err(Refusal::Disk(_))));
    assert!(lying.bytes[..BLOCK].iter().all(|&b| b == 0), "no header after a bad read-back");
    let mut failing = Mem::new(128);
    failing.fail_after = Some(3);
    assert!(matches!(send(&f, &mut failing, PIECE), Err(Refusal::Disk(_))));
}

#[test]
fn out_of_order_too_big_or_short_is_refused() {
    let (f, _) = package();
    let head_len = HEADER + NOTES.len();
    let mut slot = Mem::new(128);
    let mut i = Install::begin(&f[..head_len], &trusted(), 2, "0.4.0", &mut slot).unwrap();
    assert_eq!(i.piece(5, &f[head_len..head_len + 10], &mut slot), Err(Refusal::OutOfOrder));
    i.piece(0, &f[head_len..head_len + 10], &mut slot).unwrap();
    assert_eq!(i.finish(&mut slot), Err(Refusal::NotAllThere));
    let mut small = Mem::new(4);
    assert!(matches!(Install::begin(&f[..head_len], &trusted(), 2, "0.4.0", &mut small), Err(Refusal::TooBig)));
    assert!(matches!(
        Install::begin(&f[..head_len], &trusted(), 2, "0.4.1", &mut slot),
        Err(Refusal::Fup(FupError::SameVersion))
    ));
    assert!(matches!(
        Install::begin(&f[..head_len], &trusted(), 0, "0.4.0", &mut slot),
        Err(Refusal::Fup(FupError::WrongFlavour))
    ));
}

/// An install that didn't finish leaves the other slot without an image,
/// whatever the A/B record still says: there's nothing to switch back to.
#[test]
fn a_cleared_slot_is_empty_whatever_the_record_says() {
    let mut slot = Mem::new(128);
    let img = image("0.4.0");
    slot.write(0, &img[..BLOCK]).unwrap();
    assert_eq!(other_state(GOOD, &mut slot), GOOD, "an image there: as recorded");
    let (mut f, _) = package();
    let n = f.len();
    f[n - 9] ^= 1;
    assert!(send(&f, &mut slot, PIECE).is_err());
    assert_eq!(other_state(GOOD, &mut slot), EMPTY, "cleared by an install that failed");
}
