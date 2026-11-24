// SPDX-License-Identifier: Apache-2.0
//! Wycheproof's ECDSA tests (P-256 with SHA-256, P-384 with SHA-384; made by
//! fcrypto's gen_wycheproof.py): the signature as a certificate or a
//! CertificateVerify carries it, DER (read strictly) then the curve. A valid
//! case verifies; an invalid one (BER, a changed byte, r or s out of range,
//! the point at infinity …) never does.

use fcrypto::ecdsa::Curve;
use fcrypto::rsa::Hash;
use fx509::cert::{Key, SigAlg};
use fx509::verify::signed;

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn wycheproof_ecdsa() {
    let (mut curve, mut x, mut y) = (Curve::P256, Vec::new(), Vec::new());
    let (mut valid, mut invalid) = (0, 0);
    for line in include_str!("wycheproof_ecdsa.txt").lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        if f[0] == "key" {
            curve = if f[1] == "p256" { Curve::P256 } else { Curve::P384 };
            (x, y) = (unhex(f[2]), unhex(f[3]));
            continue;
        }
        let hash = if f[0] == "p256" { Hash::Sha256 } else { Hash::Sha384 };
        let key = Key::Ec { curve, x: &x, y: &y };
        let ok = signed(&key, SigAlg::Ecdsa(hash), &unhex(f[3]), &unhex(f[4]));
        match f[2] {
            "valid" => {
                assert!(ok, "{} {}: refused", f[0], f[1]);
                valid += 1;
            }
            "invalid" => {
                assert!(!ok, "{} {}: verified", f[0], f[1]);
                invalid += 1;
            }
            _ => {}
        }
    }
    println!("wycheproof ecdsa: {valid} valid, {invalid} invalid");
    assert!(valid > 100 && invalid > 500);
}
