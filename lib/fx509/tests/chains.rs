// SPDX-License-Identifier: Apache-2.0
//! Chains made by an independent implementation (gen_chains.py: Python's
//! cryptography), each checked and its outcome compared.

use fx509::cert::Unreadable;
use fx509::verify::{Refused, name_matches, verify};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn every_chain_gets_its_outcome() {
    let mut seen = 0;
    for line in include_str!("chains.txt").lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let list = |s: &str| if s == "-" { Vec::new() } else { s.split(',').map(unhex).collect::<Vec<_>>() };
        let (chain, roots) = (list(f[4]), list(f[5]));
        let chain: Vec<&[u8]> = chain.iter().map(Vec::as_slice).collect();
        let roots: Vec<&[u8]> = roots.iter().map(Vec::as_slice).collect();
        let got = verify(&chain, &roots, f[2], f[3].parse().unwrap());
        let want = match f[1] {
            "ok" => Ok(()),
            "Name" => Err(Refused::Name),
            "Expired" => Err(Refused::Expired),
            "NotYetValid" => Err(Refused::NotYetValid),
            "UnknownRoot" => Err(Refused::UnknownRoot),
            "Signature" => Err(Refused::Signature),
            "NotCa" => Err(Refused::NotCa),
            "Usage" => Err(Refused::Usage),
            "Critical" => Err(Refused::Unreadable(Unreadable::Critical)),
            other => panic!("{other}"),
        };
        assert_eq!(got, want, "{}", f[0]);
        seen += 1;
    }
    assert_eq!(seen, 17);
}

#[test]
fn names_and_wildcards() {
    assert!(name_matches(b"test.freally.local", b"test.freally.local."), "a trailing dot");
    assert!(!name_matches(b"*.local", b"freally.local"), "never a wildcard over the last two labels");
    assert!(!name_matches(b"f*.freally.local", b"foo.freally.local"), "only a whole label");
    assert!(!name_matches(b"*.*.freally.local", b"a.b.freally.local"));
    assert!(!name_matches(b"*.freally.local", b".freally.local"), "an empty label");
    assert!(!name_matches(b"test.freally.local", b"*.freally.local"), "a host with a star");
    assert!(!name_matches(b"test.freally.local", b""));
}
