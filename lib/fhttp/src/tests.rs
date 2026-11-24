// SPDX-License-Identifier: Apache-2.0
//! fhttp on the host: each framing fed a byte at a time, and every bad form
//! refused with its reason (RFC 9112 written out here).

use super::*;
use alloc::vec;

fn read(bytes: &[u8], limit: usize) -> Result<Response, Bad> {
    let mut r = Reader::new(limit);
    for b in bytes {
        r.feed(&[*b])?;
    }
    match r.done() {
        Some(x) => Ok(x.clone()),
        None => r.close().cloned(),
    }
}

#[test]
fn the_request() {
    assert_eq!(
        get("updates.freally.local", "/v0.9.0.fup"),
        b"GET /v0.9.0.fup HTTP/1.1\r\nHost: updates.freally.local\r\nUser-Agent: FreallyOS\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    );
}

#[test]
fn bodies_by_length_chunks_and_the_close() {
    let r =
        read(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello and more", 100).unwrap();
    assert_eq!((r.status, r.reason.as_str(), r.body.as_slice()), (200, "OK", &b"hello"[..]));
    assert_eq!(r.header("content-type"), Some("text/plain"));
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n4;x=1\r\nWiki\r\n5\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\nExpires: never\r\n\r\n";
    assert_eq!(read(chunked, 100).unwrap().body, b"Wikipedia in\r\n\r\nchunks.");
    let close = read(b"HTTP/1.0 200 OK\r\n\r\nall of it", 100).unwrap();
    assert_eq!(close.body, b"all of it");
    assert_eq!(read(b"HTTP/1.1 204 No Content\r\n\r\n", 100).unwrap().body, b"");
    assert_eq!(read(b"HTTP/1.1 304 Not Modified\r\nETag: x\r\n\r\n", 100).unwrap().status, 304);
    assert_eq!(read(b"HTTP/1.1 200 OK\r\nContent-Length: 3, 3\r\n\r\nabc", 100).unwrap().body, b"abc");
}

#[test]
fn every_bad_form_is_refused() {
    let bad = |b: &[u8], limit: usize| read(b, limit).unwrap_err();
    assert_eq!(bad(b"HTTP/2 200 OK\r\n\r\n", 9), Bad::Status);
    assert_eq!(bad(b"HTTP/1.1 2000 OK\r\n\r\n", 9), Bad::Status);
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nNo colon\r\n\r\n", 9), Bad::Header);
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nName : x\r\n\r\n", 9), Bad::Header);
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\n", 9), Bad::Length);
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n", 9), Bad::Length);
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nabc", 9), Bad::Length, "cut short");
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n", 9), Bad::Unsupported);
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n", 9), Bad::TooBig);
    assert_eq!(
        bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nA\r\n0123456789\r\n0\r\n\r\n", 9),
        Bad::TooBig
    );
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\n\r\n0123456789", 9), Bad::TooBig, "to the close, past the limit");
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", 9), Bad::Chunk);
    assert_eq!(
        bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabXY", 9),
        Bad::Chunk,
        "data not followed by CRLF"
    );
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n", 9), Bad::Cut, "no last chunk");
    assert_eq!(bad(b"HTTP/1.1 200 OK\r\nContent-Le", 9), Bad::Cut);
    let mut long = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
    long.extend(vec![b'a'; MAX_LINE + 10]);
    assert_eq!(bad(&long, 9), Bad::TooLong);
    let mut many = b"HTTP/1.1 200 OK\r\n".to_vec();
    for i in 0..=MAX_HEADERS {
        many.extend(alloc::format!("X-{i}: y\r\n").bytes());
    }
    many.extend(b"\r\n");
    assert_eq!(bad(&many, 9), Bad::TooLong);
}

#[test]
fn redirects_go_to_https_only() {
    let to = |status: u16, loc: &str| {
        let r =
            Response { status, reason: String::new(), headers: vec![("location".into(), loc.into())], body: vec![] };
        redirect(&r)
    };
    assert_eq!(to(301, "https://cdn.freally.local/a/b.fup"), Some(("cdn.freally.local".into(), "/a/b.fup".into())));
    assert_eq!(to(308, "https://cdn.freally.local"), Some(("cdn.freally.local".into(), "/".into())));
    assert_eq!(to(302, "http://cdn.freally.local/x"), None, "never to plain HTTP");
    assert_eq!(to(302, "https://user@cdn.freally.local/x"), None);
    assert_eq!(to(302, "https://cdn.freally.local:8443/x"), None);
    assert_eq!(to(200, "https://cdn.freally.local/x"), None);
}
