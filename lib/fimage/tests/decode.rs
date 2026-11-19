// SPDX-License-Identifier: Apache-2.0
//! The host-only decoders (feature `decode`) against streams Python's zlib
//! made: every deflate block type and every PNG row filter.
#![cfg(feature = "decode")]

use fimage::inflate::{adler32, zlib};
use fimage::png;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// `zlib.compress(b"hello")`: one fixed-Huffman block.
#[test]
fn a_fixed_huffman_stream() {
    assert_eq!(zlib(&hex("789ccb48cdc9c90700062c0215")).unwrap(), b"hello");
}

/// A dynamic-Huffman stream (Python, level 9) of a long, varied text.
const DYNAMIC: &str = "78da85d74d4a2c411045e1add412f227322303e7eec341a182b4a04edcfd9bbca97ce30e6838959cc37dfeba5f3e3e7eaf767dbf3f5ebfaf9fb7fb7afdbaefc7f5f8fcb99faee7ffbf572f5c9c7970912b71b173e362d5d2450f5cc49cb8986be06264c7452f31ed4d4c87905608e9d9429a4748b309e91e42ba4248630be93c423a8a48bb98f629a64b482b85f494909e2ea43985742f215d29a47184349a90ce21a423c4b46f32155201ad31f817029abb517245c91d4a2e29b94dc92d4a2e283931ed5d4ca790d66a945c5172ac53639d06eb148b92e3233dac53639d06eb148d8f90923b941cebd459a7b928b9a0e484348a75eaacd3649da2283932e52ba5e4ecd1a0e404741fb6a9b14d836d9a45c91d4a2e2939b6a98be90c4a6ef2cb0b6916ebd459a7c13ac5a1e49292639d1aeb3458a7e03315d23aac53639db89d92db69733b2d6ea7e0760a6ea7c9ed34b89d7a9aa90e28397a94cb29b99c3697d3e6725a5c4ec1e534ff5c4eff0072696a36";

#[test]
fn a_dynamic_huffman_stream() {
    let text: String = (0..120).map(|i| format!("Freally {} sings the green note; ", i * 7919 % 1000)).collect();
    let out = zlib(&hex(DYNAMIC)).unwrap();
    assert_eq!(out.len(), 4063);
    assert_eq!(out, text.as_bytes());
}

#[test]
fn damage_is_caught() {
    let mut z = hex(DYNAMIC);
    let n = z.len();
    z[n - 1] ^= 1; // the Adler-32
    assert!(zlib(&z).is_err());
    let mut z = hex(DYNAMIC);
    z[0] = 0x79; // not deflate
    assert!(zlib(&z).is_err());
    assert!(zlib(&hex(DYNAMIC)[..100]).is_err(), "a cut-off stream");
    assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
}

/// A 5×5 RGBA PNG whose rows use filters 0 to 4 in turn (Python wrote it).
#[test]
fn every_row_filter() {
    let file = hex(
        "89504e470d0a1a0a0000000d49484452000000050000000508060000008d6f26e50000005c4944415478da63606060b73115644fc95262ef996fccbee58a0bfb1d46f95476575341590d64cc249fcac0299f2a0bc456401c0ec4259ccc76a7580cb4765b496aedf604e270c955d6cf2459c02a05812a05812a05c3394d054b3801997f1622d048eb520000000049454e44ae426082",
    );
    let want = hex(
        "0000073c351107646a22078c9f3307b4d44407dc1f6507455476246d89874195be985ebdf3a97be53eca074e73db4176a8ec7b9eddfdb5c6120eefee5d2f075792405e7fc751b5a7fc620ccf317363f77c940760b1a57b88e6b6efb01bc763d850d8d700",
    );
    let d = png::decode(&file).unwrap();
    assert_eq!((d.w, d.h, d.channels), (5, 5, 4));
    assert_eq!(d.pixels, want);
}

#[test]
fn our_own_encoder_decodes_too() {
    let px: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 37 % 256) as u8).collect();
    let file = png::encode(4, 3, 4, &px);
    assert_eq!(png::decode(&file).unwrap().pixels, px);
}

/// The Brain's deflated screenshots read back exactly, and are small.
#[test]
fn a_deflated_png_reads_back() {
    let (w, h) = (320u32, 200u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x * 255 / w) as u8, (y & 0xF0) as u8, if x % 50 < 25 { 0x30 } else { 0x90 }]);
        }
    }
    let png_bytes = png::encode_deflated(w, h, 3, &px);
    let d = png::decode(&png_bytes).unwrap();
    assert_eq!((d.w, d.h, d.channels), (w, h, 3));
    assert_eq!(d.pixels, px);
    assert!(png_bytes.len() * 4 < px.len(), "{} bytes", png_bytes.len());
}
