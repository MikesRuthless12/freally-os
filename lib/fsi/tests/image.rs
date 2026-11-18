// SPDX-License-Identifier: Apache-2.0
//! Round trips, and the 1,000 single-bit flips the roadmap asks for (TASK-006).

use fsi::{FsiError, ImageBuilder, Kind, sign, verify};

const SEED: [u8; 32] = [42; 32];

fn sample(extra: usize) -> Vec<u8> {
    let public = fcrypto::ed25519::public_key(&SEED);
    let mut b = ImageBuilder::new(1, "0.1.0", [7; 20], 1_763_424_000);
    b.add("/system/version", Kind::Data, 0, b"Freally OS 0.1.0\n".to_vec());
    b.add("/system/NOTICE", Kind::Data, 0, vec![b'n'; 5000]);
    b.add("/system/fonts/sample", Kind::Font, 0, (0..extra).map(|i| (i * 7 % 256) as u8).collect());
    let mut image = b.build(&public).unwrap();
    sign(&mut image, &SEED).unwrap();
    image
}

fn trusted() -> [[u8; 32]; 1] {
    [fcrypto::ed25519::public_key(&SEED)]
}

#[test]
fn round_trip() {
    let image = sample(20_000);
    let v = verify(&image, &trusted()).expect("a fresh image verifies");
    assert_eq!(v.header.version_str(), "0.1.0");
    assert_eq!(v.file("/system/version"), Some(&b"Freally OS 0.1.0\n"[..]));
    let paths: Vec<String> = v.toc().map(|e| e.path().to_string()).collect();
    assert_eq!(paths, ["/system/NOTICE", "/system/fonts/sample", "/system/version"]);
}

#[test]
fn building_is_deterministic() {
    assert_eq!(sample(9_999), sample(9_999));
}

#[test]
fn an_unknown_key_is_refused() {
    let image = sample(10);
    let other = [fcrypto::ed25519::public_key(&[1; 32])];
    assert_eq!(verify(&image, &other).unwrap_err(), FsiError::UnknownKey);
}

#[test]
fn a_truncated_or_extended_image_is_refused() {
    let image = sample(10);
    assert!(verify(&image[..image.len() - 1], &trusted()).is_err());
    let mut longer = image.clone();
    longer.push(0);
    assert!(verify(&longer, &trusted()).is_err());
}

/// Flips 1,000 random bits, one at a time, across the whole image: every one is caught.
#[test]
fn every_single_bit_flip_is_detected() {
    let image = sample(30_000);
    let trusted = trusted();
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let bits = image.len() as u64 * 8;
    let mut caught = [0usize; 3]; // header, data, tree
    let tree_offset = u64::from_le_bytes(image[104..112].try_into().unwrap()) as usize;
    for _ in 0..1000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let bit = (x % bits) as usize;
        let mut bad = image.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        assert!(verify(&bad, &trusted).is_err(), "bit {bit} (byte {}) flipped, still verified", bit / 8);
        let byte = bit / 8;
        caught[if byte < 4096 {
            0
        } else if byte < tree_offset {
            1
        } else {
            2
        }] += 1;
    }
    // The flips really did land in every region.
    assert!(caught.iter().all(|&n| n > 0), "{caught:?}");
}

#[test]
fn reserved_and_signature_bytes_are_covered() {
    let image = sample(10);
    for byte in [200, 4031, 4032, 4095] {
        let mut bad = image.clone();
        bad[byte] ^= 0x80;
        assert!(verify(&bad, &trusted()).is_err(), "byte {byte}");
    }
}

/// `recheck_file` is conductor's check before it uses a file (TASK-046): a
/// changed byte in that file is caught, a path not in the TOC refused, and
/// the other files still read.
#[test]
fn recheck_file_catches_a_changed_file() {
    let mut image = sample(10);
    let (entry, bytes) = fsi::recheck_file(&image, "/system/NOTICE").expect("an intact file rechecks");
    assert_eq!(bytes, vec![b'n'; 5000].as_slice());
    image[entry.offset as usize + 1234] ^= 0x01;
    assert_eq!(fsi::recheck_file(&image, "/system/NOTICE").err(), Some(FsiError::BadFile));
    assert!(fsi::recheck_file(&image, "/system/version").is_ok());
    assert_eq!(fsi::recheck_file(&image, "/system/nope").err(), Some(FsiError::BadToc));
}
