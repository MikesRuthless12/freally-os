// SPDX-License-Identifier: Apache-2.0
//! The update package (TASK-110, PRD § 3): made, read back whole, and every
//! way a changed or foreign one is refused.

use fsi::fup::{self, FupError, HEADER};
use fsi::{ImageBuilder, Kind, sign};

const SEED: [u8; 32] = [42; 32];
const OTHER: [u8; 32] = [43; 32];
const NOTES: &str = "# 0.9.0\n\n- Faster copies.\n";

fn image(seed: &[u8; 32], version: &str, flavour: u16) -> Vec<u8> {
    let public = fcrypto::ed25519::public_key(seed);
    let mut b = ImageBuilder::new(flavour, version, [7; 20], 1_763_424_000);
    b.add("/system/version", Kind::Data, 0, format!("Freally OS {version}\n").into_bytes());
    b.add("/system/NOTICE", Kind::Data, 0, vec![b'n'; 9000]);
    let mut image = b.build(&public).unwrap();
    sign(&mut image, seed).unwrap();
    image
}

fn trusted() -> [[u8; 32]; 1] {
    [fcrypto::ed25519::public_key(&SEED)]
}

/// The header, the notes and the image of a `.fup`, each checked.
fn read(f: &[u8], flavour: u8, running: &str) -> Result<String, FupError> {
    let h = fup::head(f, &trusted(), flavour)?;
    if f.len() as u64 != h.total() {
        return Err(FupError::Damaged);
    }
    let notes = fup::notes(&h, &f[HEADER..HEADER + h.notes_len as usize])?.to_string();
    fup::may_install(&h, running)?;
    fup::check_image(&h, &f[HEADER + h.notes_len as usize..], &trusted())?;
    Ok(notes)
}

fn made() -> Vec<u8> {
    fup::make("0.4.0", NOTES, &image(&SEED, "0.9.0", 2), &SEED).unwrap()
}

#[test]
fn made_and_read_back_whole() {
    let f = made();
    let h = fup::head(&f, &trusted(), 2).unwrap();
    assert_eq!((h.version(), h.oldest(), h.flavour), ("0.9.0", "0.4.0", 2));
    assert_eq!(read(&f, 2, "0.8.0").unwrap(), NOTES);
}

#[test]
fn a_change_anywhere_is_refused() {
    let f = made();
    let notes_at = HEADER + 3;
    let image_at = HEADER + NOTES.len() + 5000;
    for (at, want) in [
        (30, FupError::BadSignature),  // the version, in the signed header
        (130, FupError::BadSignature), // the image's hash
        (170, FupError::BadSignature), // the signature itself
        (notes_at, FupError::Changed), // the notes
        (image_at, FupError::Changed), // the image
        (240, FupError::Damaged),      // reserved
    ] {
        let mut g = f.clone();
        g[at] ^= 1;
        assert_eq!(read(&g, 2, "0.8.0"), Err(want), "byte {at}");
    }
    let mut longer = f.clone();
    longer.push(0);
    assert_eq!(read(&longer, 2, "0.8.0"), Err(FupError::Damaged), "a byte after the image");
    assert_eq!(read(&f[..f.len() - 1], 2, "0.8.0"), Err(FupError::Damaged), "cut short");
}

#[test]
fn a_foreign_key_another_flavour_or_the_wrong_version_is_refused() {
    let foreign = fup::make("", NOTES, &image(&OTHER, "0.9.0", 2), &OTHER).unwrap();
    assert_eq!(read(&foreign, 2, "0.8.0"), Err(FupError::UnknownKey));
    assert_eq!(read(&made(), 0, "0.8.0"), Err(FupError::WrongFlavour), "an e2e update on a release system");
    assert_eq!(read(&made(), 2, "0.9.0"), Err(FupError::SameVersion));
    assert_eq!(read(&made(), 2, "0.3.9"), Err(FupError::TooOld));
    assert_eq!(read(b"not an update at all, just some bytes", 2, "0.8.0"), Err(FupError::NotAnUpdate));
    // An image that names another version than the header says.
    let mut swapped = made();
    let other = image(&SEED, "0.9.1", 2);
    let notes_end = HEADER + NOTES.len();
    swapped.truncate(notes_end);
    swapped.extend_from_slice(&other);
    assert!(read(&swapped, 2, "0.8.0").is_err(), "another image under this header");
    // Made with a key that isn't the image's own.
    assert!(fup::make("", NOTES, &image(&SEED, "0.9.0", 2), &OTHER).is_err());
}

#[test]
fn the_images_first_block_is_checked_on_its_own() {
    let f = made();
    let h = fup::head(&f, &trusted(), 2).unwrap();
    let image = &f[HEADER + NOTES.len()..];
    assert_eq!(fup::check_image_head(&h, &image[..fsi::BLOCK], &trusted()), Ok(()));
    let mut changed = image[..fsi::BLOCK].to_vec();
    changed[40] ^= 1;
    assert!(fup::check_image_head(&h, &changed, &trusted()).is_err(), "a changed image header");
    let other = image_for_head("0.9.1");
    assert_eq!(
        fup::check_image_head(&h, &other[..fsi::BLOCK], &trusted()),
        Err(FupError::Changed),
        "another version's"
    );
}

fn image_for_head(version: &str) -> Vec<u8> {
    image(&SEED, version, 2)
}

/// FR-051: the release pipeline takes a package apart and makes it again
/// around its image signed with the release key. A package whose image
/// isn't signed yet is refused whole; re-made, it reads back whole.
#[test]
fn unpacked_and_made_again_around_its_signed_image() {
    let mut unsigned = image(&SEED, "0.9.0", 2);
    unsigned[fsi::SIGNED_LEN..fsi::HEADER_LEN].fill(0);
    let f = fup::make("0.4.0", NOTES, &unsigned, &SEED).unwrap();
    assert!(matches!(read(&f, 2, "0.8.0"), Err(FupError::Image(_))), "its image isn't signed");
    let (oldest, notes, inner) = fup::unpack(&f).unwrap();
    assert_eq!((oldest, notes, inner), ("0.4.0", NOTES, unsigned.as_slice()));
    let mut signed = inner.to_vec();
    sign(&mut signed, &SEED).unwrap();
    let again = fup::make(oldest, notes, &signed, &SEED).unwrap();
    assert_eq!(read(&again, 2, "0.8.0").unwrap(), NOTES);
    assert!(fup::unpack(&again[..again.len() - 1]).is_err(), "a short one");
    assert!(fup::unpack(&again[1..]).is_err(), "not a package");
}
