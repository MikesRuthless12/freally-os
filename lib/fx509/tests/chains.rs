// SPDX-License-Identifier: Apache-2.0
//! Chains made by an independent implementation (gen_chains.py: Python's
//! cryptography), each checked and its outcome compared.

use fx509::cert::{Certificate, Unreadable};
use fx509::der::{Bad, Reader, tag};
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

/// The certificates of `case`'s chain in chains.txt.
fn chain_of(case: &str) -> Vec<Vec<u8>> {
    let line = include_str!("chains.txt").lines().find(|l| l.split(' ').next() == Some(case)).unwrap();
    line.split(' ').nth(4).unwrap().split(',').map(unhex).collect()
}

/// A DER element: `tag`, its length, `body`.
fn tlv(t: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![t];
    match body.len() {
        n if n < 0x80 => v.push(n as u8),
        n if n < 0x100 => v.extend([0x81, n as u8]),
        n => v.extend([0x82, (n >> 8) as u8, n as u8]),
    }
    v.extend_from_slice(body);
    v
}

/// `der` with the extension `oid` given twice (the signature no longer
/// covers it: the reader is what's tested).
fn with_twice(der: &[u8], oid: &[u8]) -> Vec<u8> {
    let mut cert = Reader::new(der).sequence().unwrap();
    let (_, tbs_body, _) = cert.any().unwrap();
    let (_, _, alg) = cert.any().unwrap();
    let (_, _, sig) = cert.any().unwrap();
    let (mut t, mut tbs) = (Reader::new(tbs_body), Vec::new());
    while !t.is_empty() {
        let (tg, body, whole) = t.any().unwrap();
        if tg != tag::explicit(3) {
            tbs.extend_from_slice(whole);
            continue;
        }
        let (mut list, mut exts) = (Reader::new(body).sequence().unwrap(), Vec::new());
        while !list.is_empty() {
            let (_, ext, whole) = list.any().unwrap();
            exts.extend_from_slice(whole);
            if Reader::new(ext).read(tag::OID).unwrap() == oid {
                exts.extend_from_slice(whole);
            }
        }
        tbs.extend(tlv(tg, &tlv(tag::SEQUENCE, &exts)));
    }
    let mut body = tlv(tag::SEQUENCE, &tbs);
    body.extend_from_slice(alg);
    body.extend_from_slice(sig);
    tlv(tag::SEQUENCE, &body)
}

#[test]
fn an_extension_it_takes_given_twice_is_refused() {
    let leaf = &chain_of("good")[0];
    assert_eq!(&with_twice(leaf, b"none"), leaf, "rebuilt as it was");
    for oid in [&[0x55, 0x1D, 0x11][..], &[0x55, 0x1D, 0x25]] {
        let twice = with_twice(leaf, oid);
        assert_ne!(&twice, leaf);
        assert_eq!(Certificate::parse(&twice).map(|_| ()), Err(Unreadable::Duplicate), "{oid:02x?}");
    }
}

#[test]
fn a_boolean_is_ff_or_00_and_nothing_else() {
    // The leaf with an unknown critical extension, each TRUE written as BER's 0x01.
    let leaf = &chain_of("an_unknown_critical_extension")[0];
    let ber: Vec<u8> = {
        let mut v = leaf.clone();
        let mut i = 0;
        while i + 3 <= v.len() {
            if v[i..i + 3] == [tag::BOOLEAN, 1, 0xFF] {
                v[i + 2] = 0x01;
            }
            i += 1;
        }
        v
    };
    assert_ne!(&ber, leaf);
    assert_eq!(Certificate::parse(&ber).map(|_| ()), Err(Unreadable::Der(Bad::Value)));
    assert_eq!(Reader::new(&[tag::BOOLEAN, 1, 0xFF]).optional_bool(), Ok(Some(true)));
    assert_eq!(Reader::new(&[tag::BOOLEAN, 1, 0x00]).optional_bool(), Ok(Some(false)));
    assert_eq!(Reader::new(&[tag::NULL, 0]).optional_bool(), Ok(None));
}
