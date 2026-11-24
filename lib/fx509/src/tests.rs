// SPDX-License-Identifier: Apache-2.0
//! The DER reader on the host, each case written out from X.690.

use crate::der::{Bad, Reader, days_from_civil, tag};

#[test]
fn lengths_short_and_long_and_only_minimal() {
    let mut r = Reader::new(&[0x04, 0x02, 0xAA, 0xBB, 0x05, 0x00]);
    assert_eq!(r.read(tag::OCTET_STRING), Ok(&[0xAA, 0xBB][..]));
    assert_eq!(r.read(tag::NULL), Ok(&[][..]));
    assert!(r.end().is_ok());
    let mut long = vec![0x04, 0x81, 0x80];
    long.extend(std::iter::repeat_n(7u8, 128));
    assert_eq!(Reader::new(&long).read(tag::OCTET_STRING).map(<[u8]>::len), Ok(128));
    // 0x81 0x05: the long form for a short length. 0x82 0x00 0x90: a leading zero.
    assert_eq!(Reader::new(&[0x04, 0x81, 0x05, 1, 2, 3, 4, 5]).any().map(|_| ()), Err(Bad::Length));
    assert_eq!(Reader::new(&[0x04, 0x82, 0x00, 0x90]).any().map(|_| ()), Err(Bad::Length));
    // Indefinite, and five length bytes.
    assert_eq!(Reader::new(&[0x30, 0x80, 0, 0]).any().map(|_| ()), Err(Bad::Length));
    assert_eq!(Reader::new(&[0x04, 0x85, 0, 0, 0, 0, 1, 9]).any().map(|_| ()), Err(Bad::Length));
    // Past the end; a multi-byte tag; the wrong tag; bytes left over.
    assert_eq!(Reader::new(&[0x04, 0x05, 1, 2]).any().map(|_| ()), Err(Bad::Short));
    assert_eq!(Reader::new(&[0x04, 0x82, 0x01]).any().map(|_| ()), Err(Bad::Short));
    assert_eq!(Reader::new(&[0x1F, 0x81, 0x01, 0x00]).any().map(|_| ()), Err(Bad::Tag));
    assert_eq!(Reader::new(&[0x05, 0x00]).read(tag::INTEGER), Err(Bad::Tag));
    assert_eq!(Reader::new(&[0x05, 0x00, 0x05, 0x00]).sequence().map(|_| ()), Err(Bad::Tag));
    let mut extra = Reader::new(&[0x05, 0x00, 0x05, 0x00]);
    extra.read(tag::NULL).unwrap();
    assert_eq!(extra.end(), Err(Bad::Trailing));
}

#[test]
fn integers_are_positive_and_minimal() {
    let int = |b: &[u8]| {
        let mut v = vec![tag::INTEGER, b.len() as u8];
        v.extend_from_slice(b);
        Reader::new(&v).unsigned().map(<[u8]>::to_vec)
    };
    assert_eq!(int(&[0x00]), Ok(vec![0]));
    assert_eq!(int(&[0x00, 0x80]), Ok(vec![0x80]), "a leading zero to keep it positive");
    assert_eq!(int(&[0x7F, 0x01]), Ok(vec![0x7F, 0x01]));
    assert_eq!(int(&[0x00, 0x7F]), Err(Bad::Value), "a needless leading zero");
    assert_eq!(int(&[0x80]), Err(Bad::Value), "negative");
    assert_eq!(int(&[]), Err(Bad::Value));
    assert_eq!(Reader::new(&[0x02, 0x01, 0x02]).small(), Ok(2));
    assert_eq!(Reader::new(&[0x03, 0x03, 0x00, 0xAB, 0xCD]).bits(), Ok(&[0xAB, 0xCD][..]));
    assert_eq!(Reader::new(&[0x03, 0x02, 0x04, 0xF0]).bits(), Err(Bad::Value), "unused bits");
}

#[test]
fn both_times_to_unix_seconds() {
    let time = |t: u8, s: &str| {
        let mut v = vec![t, s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        Reader::new(&v).time()
    };
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    assert_eq!(time(tag::UTC_TIME, "261124093015Z"), Ok(1_795_512_615));
    assert_eq!(time(tag::GENERALIZED_TIME, "20500101000000Z"), Ok(2_524_608_000));
    assert_eq!(time(tag::UTC_TIME, "491231235959Z"), Ok(2_524_607_999), "49 is 2049");
    assert_eq!(time(tag::UTC_TIME, "700101000000Z"), Ok(0), "70 is 1970");
    for bad in ["261324093015Z", "261124093015", "26112409301Z", "2611240930151", "261124093060Z"] {
        assert!(time(tag::UTC_TIME, bad).is_err(), "{bad}");
    }
    assert_eq!(time(tag::OCTET_STRING, "261124093015Z"), Err(Bad::Tag));
}
