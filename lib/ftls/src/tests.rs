// SPDX-License-Identifier: Apache-2.0
//! ftls against RFC 8448 § 3 (the simple 1-RTT handshake, TLS_AES_128_GCM_SHA256
//! over X25519): its secrets, its records and both Finished messages
//! reproduced byte for byte; its server's flight through the client up to the
//! certificate (which names no host: the RFC's is CN "rsa", 1,024 bits).
//! Then two fuzz targets: that flight changed, re-sealed, fed in; and three
//! sessions OpenSSL's server had with this client (tests/seeds.txt), a
//! record's content changed and sealed again under the keys it had, so the
//! change gets past the certificate. Nothing panics; no change connects.

use alloc::vec::Vec;

use fx509::verify::Refused;

use crate::client::{Client, Error};
use crate::msg::{self, hs};
use crate::record::{Protection, content};
use crate::schedule::{Schedule, Secret, Suite, Transcript, expand_label, finished};

const S: Suite = Suite::Aes128GcmSha256;

fn rfc(name: &str) -> Vec<u8> {
    let line = include_str!("../tests/rfc8448.txt")
        .lines()
        .find(|l| l.split(' ').next() == Some(name))
        .unwrap_or_else(|| panic!("no {name} in rfc8448.txt"));
    let h = line.split(' ').nth(1).unwrap();
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
}

fn key32(name: &str) -> [u8; 32] {
    rfc(name).try_into().unwrap()
}

/// The handshake messages in a run of them.
fn messages(mut b: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let len = (b[1] as usize) << 16 | (b[2] as usize) << 8 | b[3] as usize;
        out.push(b[..4 + len].to_vec());
        b = &b[4 + len..];
    }
    out
}

fn open(p: &mut Protection, record: &[u8]) -> Result<(u8, Vec<u8>), crate::Alert> {
    let mut r = record.to_vec();
    let (h, b) = r.split_at_mut(5);
    let (ty, n) = p.open((&*h).try_into().unwrap(), b)?;
    Ok((ty, b[..n].to_vec()))
}

fn shared() -> [u8; 32] {
    fcrypto::x25519::x25519(&key32("client_x25519"), &key32("server_x25519_public"))
}

#[test]
fn rfc8448_key_schedule() {
    let hs = Schedule::handshake(S, &shared());
    assert_eq!(hs.secret.as_slice(), rfc("handshake_secret"));
    let mut t = Transcript::new(S);
    t.update(&rfc("client_hello"));
    t.update(&rfc("server_hello"));
    let th = t.hash();
    assert_eq!(hs.traffic(b"c hs traffic", &th).as_slice(), rfc("client_hs"));
    assert_eq!(hs.traffic(b"s hs traffic", &th).as_slice(), rfc("server_hs"));
    assert_eq!(hs.master().secret.as_slice(), rfc("master_secret"));
    let (mut key, mut iv) = ([0u8; 16], [0u8; 12]);
    expand_label(S, &Secret::new(&rfc("server_hs")), b"key", b"", &mut key);
    expand_label(S, &Secret::new(&rfc("server_hs")), b"iv", b"", &mut iv);
    assert_eq!((key.to_vec(), iv.to_vec()), (rfc("server_hs_key"), rfc("server_hs_iv")));
}

#[test]
fn rfc8448_records_and_finished_byte_for_byte() {
    // The server's encrypted flight opens to its four messages.
    let server_hs = Secret::new(&rfc("server_hs"));
    let (ty, flight) = open(&mut Protection::new(S, &server_hs), &rfc("server_flight_record")).unwrap();
    assert_eq!((ty, &flight), (content::HANDSHAKE, &rfc("server_flight")));
    let m = messages(&flight);
    assert_eq!(m.iter().map(|m| m[0]).collect::<Vec<_>>(), [8, 11, 15, 20]);
    let offered = msg::offered(&rfc("client_hello")).unwrap();
    assert_eq!(offered, [0, 0xFF01, 10, 0x23, 51, 43, 13, 45, 28], "the RFC's ClientHello's extensions");
    msg::encrypted_extensions(&m[0][4..], &offered).unwrap();
    assert_eq!(msg::encrypted_extensions(&m[0][4..], &[0, 10]), Err(crate::Alert::UNSUPPORTED_EXTENSION));
    assert_eq!(msg::certificate(&m[1][4..]).unwrap().len(), 1);
    assert_eq!(msg::certificate_verify(&m[2][4..]).unwrap().0, msg::scheme::RSA_PSS_SHA256);

    // The server's Finished, then the client's, from the transcript.
    let mut t = Transcript::new(S);
    for part in [rfc("client_hello"), rfc("server_hello"), m[0].clone(), m[1].clone(), m[2].clone()] {
        t.update(&part);
    }
    assert_eq!(finished(S, &server_hs, &t.hash()).as_slice(), rfc("server_finished"));
    assert_eq!(&m[3][4..], rfc("server_finished"));
    t.update(&m[3]);
    let th = t.hash();
    let master = Schedule::handshake(S, &shared()).master();
    let (client_ap, server_ap) = (master.traffic(b"c ap traffic", &th), master.traffic(b"s ap traffic", &th));
    assert_eq!((client_ap.as_slice(), server_ap.as_slice()), (&rfc("client_ap")[..], &rfc("server_ap")[..]));
    let client_hs = Secret::new(&rfc("client_hs"));
    assert_eq!(finished(S, &client_hs, &th).as_slice(), rfc("client_finished"));

    // The client's records, sealed: its Finished, its data, its close_notify.
    let fin = msg::message(hs::FINISHED, |b| b.extend_from_slice(&rfc("client_finished")));
    let mut out = Vec::new();
    Protection::new(S, &client_hs).seal(content::HANDSHAKE, &fin, &mut out);
    assert_eq!(out, rfc("client_finished_record"));
    let mut write = Protection::new(S, &client_ap);
    out.clear();
    write.seal(content::APPLICATION_DATA, &rfc("app_data"), &mut out);
    assert_eq!(out, rfc("client_app_record"));
    out.clear();
    write.seal(content::ALERT, &[1, 0], &mut out);
    assert_eq!(out, rfc("client_alert_record"));

    // The server's, opened in order: the ticket, the data, its close_notify.
    let mut read = Protection::new(S, &server_ap);
    assert_eq!(open(&mut read, &rfc("server_ticket_record")), Ok((content::HANDSHAKE, rfc("server_ticket"))));
    assert_eq!(open(&mut read, &rfc("server_app_record")), Ok((content::APPLICATION_DATA, rfc("app_data"))));
    assert_eq!(open(&mut read, &rfc("server_alert_record")), Ok((content::ALERT, alloc::vec![1, 0])));

    // A changed byte, or a record out of order, never opens.
    let mut changed = rfc("server_app_record");
    changed[20] ^= 1;
    let alert = Err(crate::Alert::BAD_RECORD_MAC);
    assert_eq!(open(&mut Protection::new(S, &server_ap), &changed), alert);
    assert_eq!(open(&mut Protection::new(S, &server_ap), &rfc("server_app_record")), alert, "sequence 0, sealed at 1");
}

fn rfc_client() -> Client<'static> {
    Client::start("server", &[], 1_469_841_779, rfc("client_hello"), key32("client_x25519"))
}

#[test]
fn rfc8448_flight_through_the_client() {
    let mut c = rfc_client();
    assert_eq!(c.take_output(), rfc("client_hello_record"));
    // Byte by byte: records arrive in pieces.
    for b in rfc("server_hello_record") {
        c.feed(&[b]).unwrap();
    }
    assert_eq!(c.suite(), Some(S));
    let e = c.feed(&rfc("server_flight_record")).unwrap_err();
    assert_eq!(e, Error::Certificate(Refused::Name), "the RFC's certificate names no host");
    let mut want = Vec::new();
    Protection::new(S, &Secret::new(&rfc("client_hs"))).seal(content::ALERT, &[2, 46], &mut want);
    assert_eq!(c.take_output(), want, "certificate_unknown, sealed under the client's handshake keys");
    assert_eq!(c.feed(&[0x17]), Err(e), "a failed connection stays failed");
    assert_eq!(c.write(b"x"), Err(e));
}

#[test]
fn the_server_hello_is_checked() {
    let sh = rfc("server_hello_record");
    let refused = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut r = sh.clone();
        change(&mut r);
        let mut c = rfc_client();
        c.feed(&r).unwrap_err()
    };
    let sent = |a| Error::Sent(a);
    // The record: ServerHello at 5, its body at 9; the suite at 9+2+32+1 = 44.
    assert_eq!(refused(&|r| r[44] = 0x05), sent(crate::Alert::ILLEGAL_PARAMETER), "a suite not offered");
    assert_eq!(refused(&|r| r[94] = 0x03), sent(crate::Alert::PROTOCOL_VERSION), "TLS 1.2 in supported_versions");
    assert_eq!(refused(&|r| r[11..43].copy_from_slice(&msg::RETRY_RANDOM)), sent(crate::Alert::DECODE_ERROR));
    assert_eq!(refused(&|r| r[5] = 4), sent(crate::Alert::UNEXPECTED_MESSAGE), "a ticket first");
    assert_eq!(refused(&|r| r[0] = 0x17), sent(crate::Alert::UNEXPECTED_MESSAGE), "data before keys");
    assert_eq!(refused(&|r| r[57..89].fill(0)), sent(crate::Alert::ILLEGAL_PARAMETER), "an all-zero shared secret");
    let mut c = rfc_client();
    assert_eq!(c.feed(&[0x15, 3, 3, 0, 2, 2, 40]), Err(Error::Received(crate::Alert::HANDSHAKE_FAILURE)));
    assert_eq!(c.take_output().len(), 201, "no alert answers an alert");
    let mut c = rfc_client();
    assert_eq!(c.feed(&[0x16, 3, 3, 0x48, 1]), Err(sent(crate::Alert::RECORD_OVERFLOW)));
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// The fuzz target: the RFC's server flight with bytes changed, cut or added
/// inside the encryption (re-sealed, so the parsers see it), or the
/// ServerHello changed in the clear. No input may panic, and none may connect
/// (the certificate can't pass). `ci --slow` runs 2,000 (`FREALLY_FUZZ_ITERS`).
#[test]
fn changed_flights_never_panic() {
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let mut rng = XorShift(0xA076_1D64_78BD_642F);
    let server_hs = Secret::new(&rfc("server_hs"));
    let flight = rfc("server_flight");
    for i in 0..iters {
        let (mut sh, mut inner) = (rfc("server_hello_record"), flight.clone());
        let target = if rng.next() % 4 == 0 { &mut sh } else { &mut inner };
        match rng.next() % 3 {
            0 => (0..1 + rng.next() % 4).for_each(|_| {
                let at = (rng.next() as usize) % target.len();
                target[at] ^= 1 + (rng.next() % 255) as u8;
            }),
            1 => target.truncate((rng.next() as usize) % target.len()),
            _ => {
                let at = (rng.next() as usize) % target.len();
                target.insert(at, rng.next() as u8);
            }
        }
        let mut sealed = Vec::new();
        if !inner.is_empty() {
            Protection::new(S, &server_hs).seal(content::HANDSHAKE, &inner, &mut sealed);
        }
        let mut c = rfc_client();
        let _ = c.feed(&sh);
        let _ = c.feed(&sealed);
        assert!(!c.is_connected(), "iteration {i}");
        let _ = c.take_output();
    }
}

/// CertificateVerify's signature, checked against signatures Python's
/// cryptography made (gen_cv.py): each verifies; a changed transcript, a
/// changed signature, or a scheme that isn't the key's never does.
#[test]
fn certificate_verify_signatures() {
    use fcrypto::ecdsa::Curve;
    use fx509::cert::Key;
    let content = msg::signed_content(&(0u8..32).collect::<Vec<_>>());
    let mut other = content.clone();
    other[100] ^= 1;
    let mut seen = 0;
    for line in include_str!("../tests/cv.txt").lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let unhex = |h: &str| {
            (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>()
        };
        let (s, key, sig) = (u16::from_str_radix(f[0], 16).unwrap(), unhex(f[2]), unhex(f[3]));
        let k = match f[1] {
            "ec" => {
                let curve = if key.len() == 64 { Curve::P256 } else { Curve::P384 };
                let (x, y) = key.split_at(key.len() / 2);
                Key::Ec { curve, x, y }
            }
            _ => Key::Rsa { n: &key, e: &[1, 0, 1] },
        };
        let check = |s: u16, c: &[u8], sig: &[u8]| crate::client::handshake_signed(&k, s, c, sig);
        assert!(check(s, &content, &sig), "{s:04x}");
        assert!(!check(s, &other, &sig), "{s:04x}: a changed transcript");
        let mut bad = sig.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(!check(s, &content, &bad), "{s:04x}: a changed signature");
        for wrong in [
            msg::scheme::ECDSA_P256_SHA256,
            msg::scheme::ECDSA_P384_SHA384,
            msg::scheme::RSA_PSS_SHA256,
            msg::scheme::RSA_PKCS1_SHA256,
        ] {
            if wrong != s {
                assert!(!check(wrong, &content, &sig), "{s:04x} signed, {wrong:04x} claimed");
            }
        }
        seen += 1;
    }
    assert_eq!(seen, 5);
}

/// A HelloRetryRequest's record: its suite, and the group it asks for.
fn retry_request(suite: u16, group: u16) -> Vec<u8> {
    let mut body = alloc::vec![3, 3];
    body.extend_from_slice(&msg::RETRY_RANDOM);
    body.push(0);
    body.extend_from_slice(&suite.to_be_bytes());
    body.push(0);
    let exts = [0x00, 0x2b, 0, 2, 3, 4, 0x00, 0x33, 0, 2, (group >> 8) as u8, group as u8];
    body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    body.extend_from_slice(&exts);
    let mut m = alloc::vec![2, 0, (body.len() >> 8) as u8, body.len() as u8];
    m.extend_from_slice(&body);
    let mut r = alloc::vec![22, 3, 3, (m.len() >> 8) as u8, m.len() as u8];
    r.extend_from_slice(&m);
    r
}

/// A HelloRetryRequest for P-256 gets a second ClientHello with the same
/// random and a P-256 share; a second request, one for X25519 (whose share
/// went already) or for a group never offered, and a ServerHello that
/// changes the request's suite or group, are each refused (§ 4.1.4).
#[test]
fn hello_retry_requests_are_checked() {
    use crate::Alert;
    let fresh = || Client::new("test.freally.local", &[], 0, &[9u8; 96]).unwrap();
    let share = [0x00, 0x17, 0x00, 0x41, 0x04];
    let mut c = fresh();
    let first = c.take_output();
    c.feed(&retry_request(0x1301, 0x0017)).unwrap();
    assert!(c.retried());
    let second = c.take_output();
    assert_eq!(&second[..3], &[22, 3, 3]);
    assert_eq!(second[11..43], first[11..43], "the same random");
    assert!(second.windows(5).any(|w| w == share), "a 65-byte P-256 share");
    assert!(!first.windows(5).any(|w| w == share), "none at first");
    assert_eq!(c.feed(&retry_request(0x1301, 0x0017)), Err(Error::Sent(Alert::UNEXPECTED_MESSAGE)));
    for g in [0x001d, 0x0018] {
        assert_eq!(fresh().feed(&retry_request(0x1301, g)), Err(Error::Sent(Alert::ILLEGAL_PARAMETER)), "{g:04x}");
    }
    // The RFC's ServerHello (AES-128, X25519) after a request for AES-256, or for P-256.
    let mut c = fresh();
    c.feed(&retry_request(0x1302, 0x0017)).unwrap();
    assert_eq!(c.feed(&rfc("server_hello_record")), Err(Error::Sent(Alert::ILLEGAL_PARAMETER)), "another suite");
    let mut c = fresh();
    c.feed(&retry_request(0x1301, 0x0017)).unwrap();
    assert_eq!(c.feed(&rfc("server_hello_record")), Err(Error::Sent(Alert::ILLEGAL_PARAMETER)), "another group");
}

/// A session OpenSSL's server had with this client (`tests/seeds.txt`,
/// recorded by `tests/openssl.rs`'s `record_seeds`): the client's entropy and
/// clock, the root, every byte the server sent, and the line it sent back.
struct Session {
    name: &'static str,
    now: u64,
    entropy: [u8; 96],
    root: Vec<u8>,
    server: Vec<u8>,
    data: Vec<u8>,
}

fn sessions() -> Vec<Session> {
    let unhex =
        |h: &str| (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
    let mut out: Vec<Session> = Vec::new();
    for line in include_str!("../tests/seeds.txt").lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&'static str> = line.split(' ').collect();
        if out.last().is_none_or(|s| s.name != f[0]) {
            let entropy = [0; 96];
            out.push(Session { name: f[0], now: 0, entropy, root: Vec::new(), server: Vec::new(), data: Vec::new() });
        }
        let s = out.last_mut().unwrap();
        match f[1] {
            "now" => s.now = f[2].parse().unwrap(),
            "entropy" => s.entropy = unhex(f[2]).try_into().unwrap(),
            "root" => s.root = unhex(f[2]),
            "server" => s.server = unhex(f[2]),
            "data" => s.data = unhex(f[2]),
            other => panic!("seeds.txt: what is {other}?"),
        }
    }
    out
}

/// One record of a session, opened: its content type, what it carries, the
/// suite and secret it was sealed under (none in the clear), and whether the
/// client was connected before it.
#[derive(Clone)]
struct Piece {
    ty: u8,
    body: Vec<u8>,
    key: Option<(Suite, Secret)>,
    after_handshake: bool,
}

/// The session's records as the client read them, each opened with the key
/// it read it under. The session must still connect and bring its line.
fn pieces(s: &Session) -> Vec<Piece> {
    let roots = [s.root.as_slice()];
    let mut c = Client::new("test.freally.local", &roots, s.now, &s.entropy).unwrap();
    let (mut rest, mut out) = (&s.server[..], Vec::new());
    let mut read: Option<(Secret, Protection)> = None;
    let stale = |e: &dyn core::fmt::Debug| -> ! {
        panic!("{}: {e:?}: stale? (a changed ClientHello: tests/openssl.rs's record_seeds records them again)", s.name)
    };
    while !rest.is_empty() {
        let (record, after) = rest.split_at(5 + usize::from(u16::from_be_bytes([rest[3], rest[4]])));
        rest = after;
        let after_handshake = c.is_connected();
        let piece = if record[0] == content::APPLICATION_DATA {
            let (suite, secret) = c.reading().expect("a protected record before any key");
            if read.as_ref().is_none_or(|(s, _)| s.as_slice() != secret.as_slice()) {
                read = Some((secret.clone(), Protection::new(suite, &secret)));
            }
            let (ty, body) = open(&mut read.as_mut().unwrap().1, record).unwrap_or_else(|e| stale(&e));
            Piece { ty, body, key: Some((suite, secret)), after_handshake }
        } else {
            Piece { ty: record[0], body: record[5..].to_vec(), key: None, after_handshake }
        };
        c.feed(record).unwrap_or_else(|e| stale(&e));
        out.push(piece);
    }
    assert!(c.is_connected(), "{}", s.name);
    assert_eq!(c.take_data(), s.data, "{}", s.name);
    out
}

/// The pieces as records again, each sealed as it was (each key's records
/// numbered from 0, in order).
fn sealed(pieces: &[Piece]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut write: Option<(&Secret, Protection)> = None;
    for p in pieces {
        match &p.key {
            None => crate::record::plain(p.ty, 0x0303, &p.body, &mut out),
            Some((suite, secret)) => {
                if write.as_ref().is_none_or(|(s, _)| s.as_slice() != secret.as_slice()) {
                    write = Some((secret, Protection::new(*suite, secret)));
                }
                write.as_mut().unwrap().1.seal(p.ty, &p.body, &mut out);
            }
        }
    }
    out
}

/// The recorded sessions still connect (else `record_seeds` makes them
/// again), and sealed again they're what OpenSSL sent, byte for byte.
#[test]
fn recorded_sessions_replay() {
    let all = sessions();
    assert_eq!(all.iter().map(|s| s.name).collect::<Vec<_>>(), ["p256", "rsa-request", "p384-retry"]);
    for s in &all {
        let p = pieces(s);
        assert!(p.iter().any(|p| p.after_handshake), "{}: records after the handshake", s.name);
        assert_eq!(sealed(&p), s.server, "{}", s.name);
    }
}

/// The fuzz target past the record layer (TASK-185): a recorded session
/// (P-256 and AES-128-GCM; RSA, a CertificateRequest and ChaCha20; P-384
/// after a HelloRetryRequest, AES-256-GCM) with one record's content
/// changed and the session sealed again under the keys it had, so the
/// change reaches the parser and the check behind each message: the
/// certificate chain, its signature, Finished, and what comes after the
/// handshake. Fed in random pieces. No input may panic, and a changed
/// handshake never connects. `ci --slow` runs 2,000 (`FREALLY_FUZZ_ITERS`).
#[test]
fn changed_sessions_never_panic() {
    // Each state's unexpected message: every handshake type the client knows.
    const TYPES: [u8; 11] = [1, 2, 4, 5, 8, 11, 13, 15, 20, 24, 254];
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let all = sessions();
    let recorded: Vec<Vec<Piece>> = all.iter().map(pieces).collect();
    for i in 0..iters {
        let n = i as usize % all.len();
        let (s, mut p) = (&all[n], recorded[n].clone());
        let k = rng.next() as usize % p.len();
        let piece = &mut p[k];
        match rng.next() % 5 {
            0 if !piece.body.is_empty() => (0..1 + rng.next() % 4).for_each(|_| {
                let at = rng.next() as usize % piece.body.len();
                piece.body[at] ^= 1 + (rng.next() % 255) as u8;
            }),
            1 if !piece.body.is_empty() => piece.body.truncate(rng.next() as usize % piece.body.len()),
            2 => {
                let at = rng.next() as usize % (piece.body.len() + 1);
                piece.body.insert(at, rng.next() as u8);
            }
            3 if piece.ty == content::HANDSHAKE && !piece.body.is_empty() => {
                piece.body[0] = TYPES[rng.next() as usize % TYPES.len()];
            }
            _ => piece.ty = [20, 21, 22, 23][rng.next() as usize % 4],
        }
        let changed = (piece.ty, &piece.body) != (recorded[n][k].ty, &recorded[n][k].body);
        let bytes = sealed(&p);
        // Where the changed record ends: the client is judged there, before
        // the records after it (sealed under the keys the session had) end
        // what a wrongly made connection would have shown.
        let judged = sealed(&p[..=k]).len();
        let roots = [s.root.as_slice()];
        let mut c = Client::new("test.freally.local", &roots, s.now, &s.entropy).unwrap();
        let mut at = 0;
        while at < bytes.len() {
            let end = (at + 1 + rng.next() as usize % 2048).min(bytes.len());
            let end = if at < judged { end.min(judged) } else { end };
            let _ = c.feed(&bytes[at..end]);
            let _ = c.take_output();
            let _ = c.take_data();
            at = end;
            if at == judged && changed && !p[k].after_handshake {
                assert!(!c.is_connected(), "{} iteration {i}: record {k} changed, and it connected", s.name);
            }
        }
    }
}
