// SPDX-License-Identifier: Apache-2.0
//! ftls against OpenSSL's server (`openssl s_server -tls1_3`) over a socket
//! on this machine: an independent implementation at the other end, with a
//! root and a leaf OpenSSL makes for each run. Every cipher suite, ECDSA
//! P-256 and P-384 and RSA-PSS leaves, data both ways in one record and many,
//! a server asking for a client certificate; a wrong name and an unknown
//! root refused. OpenSSL's command line tool is needed: without it the test
//! fails and says so (in `ci-linux` the image installs it).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ftls::schedule::Suite;
use ftls::{Client, Error};
use fx509::verify::Refused;

const HOST: &str = "test.freally.local";

fn openssl(dir: &Path, args: &[&str]) {
    let out = Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("this test needs OpenSSL's command line tool (apt install openssl): {e}"));
    assert!(out.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A root and a leaf for test.freally.local, of `kind` (p256, p384, rsa),
/// made by OpenSSL in a directory of their own: (the directory, the root's DER).
fn pki(kind: &str) -> (PathBuf, Vec<u8>) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ftls-openssl-{kind}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (alg, opt, sha) = match kind {
        "rsa" => ("RSA", "rsa_keygen_bits:2048", "-sha256"),
        "p384" => ("EC", "ec_paramgen_curve:P-384", "-sha384"),
        _ => ("EC", "ec_paramgen_curve:P-256", "-sha256"),
    };
    for key in ["root.key", "leaf.key"] {
        openssl(&dir, &["genpkey", "-algorithm", alg, "-pkeyopt", opt, "-out", key]);
    }
    openssl(
        &dir,
        &[
            "req",
            "-x509",
            "-new",
            "-key",
            "root.key",
            "-subj",
            "/CN=Freally Test Root",
            "-days",
            "30",
            sha,
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
            "-out",
            "root.pem",
        ],
    );
    openssl(&dir, &["req", "-new", "-key", "leaf.key", "-subj", "/CN=test.freally.local", "-out", "leaf.csr"]);
    std::fs::write(
        dir.join("leaf.ext"),
        "subjectAltName=DNS:test.freally.local\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature\n",
    )
    .unwrap();
    openssl(
        &dir,
        &[
            "x509",
            "-req",
            "-in",
            "leaf.csr",
            "-CA",
            "root.pem",
            "-CAkey",
            "root.key",
            "-set_serial",
            "2",
            "-days",
            "30",
            sha,
            "-extfile",
            "leaf.ext",
            "-out",
            "leaf.pem",
        ],
    );
    openssl(&dir, &["x509", "-in", "root.pem", "-outform", "DER", "-out", "root.der"]);
    let root = std::fs::read(dir.join("root.der")).unwrap();
    (dir, root)
}

/// One `s_server` for one connection, reversing each line it's sent (`-rev`).
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve(dir: &Path, extra: &[&str]) -> (Server, TcpStream) {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let accept = format!("127.0.0.1:{port}");
    let child = Command::new("openssl")
        .current_dir(dir)
        .args(["s_server", "-accept", &accept, "-tls1_3", "-cert", "leaf.pem", "-key", "leaf.key", "-rev"])
        .args(["-naccept", "1", "-quiet"])
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("openssl s_server");
    let server = Server(child);
    // The port opens once s_server listens; the first connection is the test's.
    let start = Instant::now();
    loop {
        if let Ok(s) = TcpStream::connect(&accept) {
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            return (server, s);
        }
        assert!(start.elapsed() < Duration::from_secs(10), "s_server never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn entropy(seed: &str) -> [u8; 64] {
    fcrypto::sha2::sha512(format!("{seed} {}", now()).as_bytes())
}

fn handshake(c: &mut Client, s: &mut TcpStream) -> Result<(), Error> {
    s.write_all(&c.take_output()).unwrap();
    let mut buf = [0u8; 8192];
    while !c.is_connected() {
        let n = s.read(&mut buf).expect("the server answers");
        assert!(n > 0, "the server closed the connection mid-handshake: {:?}", c.error());
        let r = c.feed(&buf[..n]);
        let _ = s.write_all(&c.take_output());
        r?;
    }
    Ok(())
}

/// Sends `lines` and reads back what the server answers, until as many newlines.
fn round_trip(c: &mut Client, s: &mut TcpStream, lines: &[u8]) -> Vec<u8> {
    let want = lines.iter().filter(|&&b| b == b'\n').count();
    c.write(lines).unwrap();
    s.write_all(&c.take_output()).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 8192];
    while got.iter().filter(|&&b| b == b'\n').count() < want {
        let n = s.read(&mut buf).expect("the server answers");
        assert!(n > 0, "the server closed the connection");
        c.feed(&buf[..n]).unwrap();
        s.write_all(&c.take_output()).unwrap();
        got.extend(c.take_data());
    }
    got
}

#[test]
fn every_suite_and_key_against_openssl() {
    for (kind, name, suite) in [
        ("p256", "TLS_CHACHA20_POLY1305_SHA256", Suite::ChaCha20Poly1305Sha256),
        ("p256", "TLS_AES_128_GCM_SHA256", Suite::Aes128GcmSha256),
        ("p384", "TLS_AES_256_GCM_SHA384", Suite::Aes256GcmSha384),
        ("rsa", "TLS_AES_128_GCM_SHA256", Suite::Aes128GcmSha256),
    ] {
        let (dir, root) = pki(kind);
        let roots = [root.as_slice()];
        let (_server, mut s) = serve(&dir, &["-ciphersuites", name]);
        let mut c = Client::new(HOST, &roots, now(), &entropy(name)).unwrap();
        handshake(&mut c, &mut s).unwrap_or_else(|e| panic!("{kind} {name}: {e:?}"));
        assert_eq!(c.suite(), Some(suite), "{kind} {name}");
        assert_eq!(round_trip(&mut c, &mut s, b"hello freally\n"), b"yllaerf olleh\n", "{kind} {name}");
        // More than one record each way: twelve lines of 3,000 bytes (s_server
        // reverses at most 16 KiB at a time, so no line is longer).
        let line: Vec<u8> = (0..3000).map(|i| b'a' + (i % 26) as u8).collect();
        let mut back = line.clone();
        back.reverse();
        let (mut out, mut want) = (Vec::new(), Vec::new());
        for _ in 0..12 {
            out.extend_from_slice(&line);
            out.push(b'\n');
            want.extend_from_slice(&back);
            want.push(b'\n');
        }
        assert_eq!(round_trip(&mut c, &mut s, &out), want, "{kind} {name}: 36 KB");
        c.close();
        s.write_all(&c.take_output()).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_certificate_request_is_answered_anonymously() {
    let (dir, root) = pki("p256");
    let roots = [root.as_slice()];
    // -verify asks for a certificate but goes on without one.
    let (_server, mut s) = serve(&dir, &["-verify", "1"]);
    let mut c = Client::new(HOST, &roots, now(), &entropy("request")).unwrap();
    handshake(&mut c, &mut s).unwrap();
    assert_eq!(round_trip(&mut c, &mut s, b"anonymous\n"), b"suomynona\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_wrong_name_and_an_unknown_root_are_refused() {
    let (dir, root) = pki("p256");
    let (other_dir, other_root) = pki("p384");
    for (host, roots, want) in [
        ("other.freally.local", [root.as_slice()], Refused::Name),
        (HOST, [other_root.as_slice()], Refused::UnknownRoot),
    ] {
        let (_server, mut s) = serve(&dir, &[]);
        let mut c = Client::new(host, &roots, now(), &entropy(host)).unwrap();
        assert_eq!(handshake(&mut c, &mut s), Err(Error::Certificate(want)), "{host}");
        assert!(!c.is_connected());
    }
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(other_dir);
}
