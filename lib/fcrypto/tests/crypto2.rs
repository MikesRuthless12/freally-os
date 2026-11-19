// SPDX-License-Identifier: Apache-2.0
//! TASK-097: every primitive against its official vectors (`rfc_vectors`,
//! lifted from the RFC texts by a script), and what must be refused.

mod rfc_vectors;

use fcrypto::{aead, argon2, blake2b, chacha, hkdf, poly1305, x25519};
use rfc_vectors::*;

fn arr<const N: usize>(s: &str) -> [u8; N] {
    hex(s).try_into().unwrap()
}

#[test]
fn hmac_sha256_rfc4231() {
    for (i, (key, data, want)) in HMAC_SHA256.iter().enumerate() {
        let got = hkdf::hmac(&hex(key), &hex(data));
        let want = hex(want);
        // Case 5 gives only the first 128 bits.
        assert_eq!(&got[..want.len()], &want[..], "RFC 4231 test case {}", i + 1);
    }
}

#[test]
fn hkdf_rfc5869() {
    for (i, (ikm, salt, info, l, prk, okm)) in HKDF_SHA256.iter().enumerate() {
        assert_eq!(hkdf::extract(&hex(salt), &hex(ikm)).to_vec(), hex(prk), "A.{} PRK", i + 1);
        let mut out = vec![0u8; *l];
        hkdf::expand(&arr(prk), &hex(info), &mut out).unwrap();
        assert_eq!(out, hex(okm), "A.{} OKM", i + 1);
    }
    let mut too_long = vec![0u8; 255 * 32 + 1];
    assert_eq!(hkdf::expand(&[0; 32], b"", &mut too_long), Err(hkdf::TooLong));
}

#[test]
fn chacha20_rfc8439() {
    for (key, nonce, counter, stream) in CHACHA_BLOCKS {
        assert_eq!(chacha::block(&arr(key), counter, &arr(nonce)).to_vec(), hex(stream));
    }
    for (key, nonce, counter, plain, cipher) in CHACHA_ENCRYPT {
        let mut data = hex(plain);
        chacha::xor(&arr(key), counter, &arr(nonce), &mut data);
        assert_eq!(data, hex(cipher));
    }
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    let mut data = hex(CHACHA_PLAIN);
    chacha::xor(&key, 1, &arr("000000000000004a00000000"), &mut data);
    assert_eq!(data, hex(CHACHA_CIPHER), "§ 2.4.2");
}

#[test]
fn poly1305_rfc8439() {
    for (i, (key, msg, tag)) in POLY1305.iter().enumerate() {
        assert_eq!(poly1305::mac(&arr(key), &hex(msg)).to_vec(), hex(tag), "Poly1305 vector {i}");
        // Fed in pieces, the same tag.
        let mut p = poly1305::Poly1305::new(&arr(key));
        for piece in hex(msg).chunks(7) {
            p.update(piece);
        }
        assert_eq!(p.finish().to_vec(), hex(tag));
    }
    for (key, nonce, otk) in POLY_KEYGEN {
        assert_eq!(chacha::block(&arr(key), 0, &arr(nonce))[..32].to_vec(), hex(otk));
    }
}

#[test]
fn chacha20_poly1305_rfc8439_and_refusals() {
    for (key, nonce, aad, plain, cipher, tag) in AEAD {
        let (key, nonce) = (arr(key), arr(nonce));
        let mut data = hex(plain);
        let t = aead::seal(&key, &nonce, &hex(aad), &mut data);
        assert_eq!(data, hex(cipher));
        assert_eq!(t.to_vec(), hex(tag));
        aead::open(&key, &nonce, &hex(aad), &mut data, &t).unwrap();
        assert_eq!(data, hex(plain));
    }
    // Any change is refused, and the buffer is left as it was.
    let (key, nonce, aad, _, cipher, tag) = AEAD[0];
    let (key, nonce, t) = (arr(key), arr::<12>(nonce), arr::<16>(tag));
    let mut flipped = hex(cipher);
    flipped[40] ^= 1;
    let before = flipped.clone();
    assert_eq!(aead::open(&key, &nonce, &hex(aad), &mut flipped, &t), Err(aead::Forged));
    assert_eq!(flipped, before);
    let mut data = hex(cipher);
    assert_eq!(aead::open(&key, &nonce, b"other", &mut data, &t), Err(aead::Forged));
    let mut bad = t;
    bad[0] ^= 0x80;
    assert_eq!(aead::open(&key, &nonce, &hex(aad), &mut data, &bad), Err(aead::Forged));
}

#[test]
fn xchacha20_poly1305_draft03() {
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    assert_eq!(
        chacha::hchacha20(&key, &arr("000000090000004a0000000031415927")).to_vec(),
        hex("82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc"),
        "§ 2.2.1"
    );
    let key = arr("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let nonce = arr("404142434445464748494a4b4c4d4e4f5051525354555657");
    let aad = hex("50515253c0c1c2c3c4c5c6c7");
    let mut data = hex(CHACHA_PLAIN);
    let t = aead::xseal(&key, &nonce, &aad, &mut data);
    assert_eq!(
        data,
        hex(concat!(
            "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb",
            "731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452",
            "2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9",
            "21f9664c97637da9768812f615c68b13b52e"
        )),
        "A.3.1 ciphertext"
    );
    assert_eq!(t.to_vec(), hex("c0875924c1c7987947deafd8780acf49"), "A.3.1 tag");
    aead::xopen(&key, &nonce, &aad, &mut data, &t).unwrap();
    assert_eq!(data, hex(CHACHA_PLAIN));
}

#[test]
fn x25519_rfc7748() {
    let cases = [
        (
            "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
            "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552",
        ),
        (
            "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
            "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957",
        ),
    ];
    for (k, u, want) in cases {
        assert_eq!(x25519::x25519(&arr(k), &arr(u)).to_vec(), hex(want), "§ 5.2");
    }
    // § 6.1: both sides reach the same secret.
    let a = arr("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let b = arr("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let (pa, pb) = (x25519::public_key(&a), x25519::public_key(&b));
    assert_eq!(pa.to_vec(), hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"));
    assert_eq!(pb.to_vec(), hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"));
    let k = hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    assert_eq!(x25519::x25519(&a, &pb).to_vec(), k);
    assert_eq!(x25519::x25519(&b, &pa).to_vec(), k);
    // § 5.2's iterations: once and 1,000 times.
    let (mut k, mut u) = (x25519::BASE, x25519::BASE);
    for i in 1..=1000 {
        let next = x25519::x25519(&k, &u);
        u = k;
        k = next;
        if i == 1 {
            assert_eq!(k.to_vec(), hex("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079"));
        }
    }
    assert_eq!(k.to_vec(), hex("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51"));
}

#[test]
fn blake2b_rfc7693() {
    let mut out = [0u8; 64];
    blake2b::blake2b(b"abc", &mut out);
    assert_eq!(
        out.to_vec(),
        hex(concat!(
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1",
            "7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        ))
    );
    // Across a block boundary, fed in pieces: the one-shot answer.
    let long: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
    let mut one = [0u8; 32];
    blake2b::blake2b(&long, &mut one);
    let mut b = blake2b::Blake2b::new(32);
    for piece in long.chunks(41) {
        b.update(piece);
    }
    let mut pieces = [0u8; 32];
    b.finish_into(&mut pieces);
    assert_eq!(one, pieces);
}

#[test]
fn argon2id_rfc9106() {
    let params = argon2::Params { m_kib: 32, t: 3, p: 4 };
    let mut memory = vec![[0u64; 128]; params.blocks()];
    let mut tag = [0u8; 32];
    argon2::argon2id(params, &[1; 32], &[2; 16], &[3; 8], &[4; 12], &mut memory, &mut tag).unwrap();
    assert_eq!(tag.to_vec(), hex("0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"), "§ 5.3");
    // Too little memory, or no lanes: refused.
    let mut small = vec![[0u64; 128]; 8];
    assert_eq!(
        argon2::argon2id(params, b"p", b"saltsalt", b"", b"", &mut small, &mut tag),
        Err(argon2::Refused::TooLittleMemory)
    );
    let none = argon2::Params { m_kib: 32, t: 3, p: 0 };
    assert_eq!(argon2::argon2id(none, b"p", b"s", b"", b"", &mut memory, &mut tag), Err(argon2::Refused::BadParams));
}
