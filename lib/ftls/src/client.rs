// SPDX-License-Identifier: Apache-2.0
//! The TLS 1.3 client, sans I/O: bytes from the server go in through
//! [`Client::feed`], bytes for it come out of [`Client::take_output`], so
//! the same code runs over netstack's TCP and, in host tests, over a socket.
//! One full handshake (no PSK, no 0-RTT, no resumption, no TLS 1.2), the
//! server authenticated against the roots given, the client anonymous. The
//! first ClientHello carries an X25519 share; a server that wants P-256 says
//! so with a HelloRetryRequest, and the second ClientHello carries that.

use alloc::string::String;
use alloc::vec::Vec;

use fx509::cert::{Certificate, Key, SigAlg};
use fx509::verify::Refused;

use crate::Alert;
use crate::msg::{self, group, hs, scheme};
use crate::record::{self, MAX_BODY, MAX_PLAIN, Protection, content};
use crate::schedule::{self, Schedule, Secret, Suite, Transcript, wipe};

/// The longest handshake message taken (a certificate chain's, in practice).
pub const MAX_MESSAGE: usize = 1 << 17;

/// Why a connection ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The client found the server at fault and sent it this alert.
    Sent(Alert),
    /// The server's certificate chain was refused (and the alert for it sent).
    Certificate(Refused),
    /// The server sent this alert.
    Received(Alert),
    /// The host name can't be a server's name (empty, over 253 bytes, not ASCII).
    Host,
    /// Data written to a connection not open.
    NotOpen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    ServerHello,
    EncryptedExtensions,
    CertificateOrRequest,
    Certificate,
    CertificateVerify,
    Finished,
    Connected,
    Closed,
    Failed(Error),
}

/// The handshake's secrets, from the ServerHello on.
struct Keys {
    suite: Suite,
    transcript: Transcript,
    schedule: Schedule,
    client_hs: Secret,
    server_hs: Secret,
    read: Protection,
    write: Protection,
    read_secret: Secret,
    write_secret: Secret,
}

pub struct Client<'r> {
    host: String,
    roots: &'r [&'r [u8]],
    now: u64,
    state: State,
    /// The ClientHello, until the ServerHello names the transcript's hash.
    hello: Vec<u8>,
    /// The extensions it offered (the only ones a server may answer).
    offered: Vec<u16>,
    x25519: [u8; 32],
    p256: [u8; 32],
    /// After a HelloRetryRequest: its suite, the group it asked for, and the
    /// transcript begun again (§ 4.4.1).
    retry: Option<(Suite, u16, Transcript)>,
    retried: bool,
    keys: Option<Keys>,
    /// The server's chain (DER), from Certificate to CertificateVerify.
    chain: Vec<Vec<u8>>,
    /// A CertificateRequest's context, answered with an empty Certificate.
    requested: Option<Vec<u8>>,
    rx: Vec<u8>,
    hs: Vec<u8>,
    out: Vec<u8>,
    data: Vec<u8>,
    peer_closed: bool,
}

impl Drop for Client<'_> {
    fn drop(&mut self) {
        wipe(&mut self.x25519);
        wipe(&mut self.p256);
    }
}

impl<'r> Client<'r> {
    /// Starts a handshake with `host` (its name checked against the leaf's
    /// subjectAltName), trusting only `roots` (DER) at `now` (Unix seconds).
    /// `entropy` is 96 bytes from the system's random source: the client
    /// random, the X25519 secret and the P-256 one. The ClientHello is ready in
    /// [`Client::take_output`].
    pub fn new(host: &str, roots: &'r [&'r [u8]], now: u64, entropy: &[u8; 96]) -> Result<Client<'r>, Error> {
        if host.is_empty() || host.len() > 253 || !host.is_ascii() {
            return Err(Error::Host);
        }
        let random: [u8; 32] = entropy[..32].try_into().expect("32");
        let x25519: [u8; 32] = entropy[32..64].try_into().expect("32");
        let share = fcrypto::x25519::public_key(&x25519);
        let hello = msg::client_hello(&random, host, &[(group::X25519, &share)], &group::OFFERED, None);
        let mut c = Client::start(host, roots, now, hello, x25519);
        c.p256 = entropy[64..].try_into().expect("32");
        Ok(c)
    }

    pub(crate) fn start(host: &str, roots: &'r [&'r [u8]], now: u64, hello: Vec<u8>, x25519: [u8; 32]) -> Client<'r> {
        let mut out = Vec::new();
        // The first ClientHello's record says TLS 1.0 for old middleboxes (§ 5.1).
        record::plain(content::HANDSHAKE, 0x0301, &hello, &mut out);
        Client {
            host: String::from(host),
            roots,
            now,
            state: State::ServerHello,
            offered: msg::offered(&hello).expect("the client's own ClientHello"),
            hello,
            x25519,
            p256: [0; 32],
            retry: None,
            retried: false,
            keys: None,
            chain: Vec::new(),
            requested: None,
            rx: Vec::new(),
            hs: Vec::new(),
            out,
            data: Vec::new(),
            peer_closed: false,
        }
    }

    /// Bytes to send to the server (taken: each call returns what's new).
    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.out)
    }

    /// Application data received (taken).
    pub fn take_data(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.data)
    }

    pub fn is_connected(&self) -> bool {
        self.state == State::Connected
    }

    /// The server sent close_notify: nothing more will come.
    pub fn peer_closed(&self) -> bool {
        self.peer_closed
    }

    /// The server asked again for another key share (a HelloRetryRequest).
    pub fn retried(&self) -> bool {
        self.retried
    }

    pub fn suite(&self) -> Option<Suite> {
        self.keys.as_ref().map(|k| k.suite)
    }

    pub fn error(&self) -> Option<Error> {
        match self.state {
            State::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// Takes bytes from the server, as many or as few as arrived. On an error
    /// the alert for it is in [`Client::take_output`], and every later call
    /// returns the same error.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if let State::Failed(e) = self.state {
            return Err(e);
        }
        self.rx.extend_from_slice(bytes);
        while self.rx.len() >= record::HEADER {
            let len = u16::from_be_bytes([self.rx[3], self.rx[4]]) as usize;
            if len > MAX_BODY {
                return Err(self.fail(Error::Sent(Alert::RECORD_OVERFLOW)));
            }
            if self.rx.len() < record::HEADER + len {
                break;
            }
            let mut rec: Vec<u8> = self.rx.drain(..record::HEADER + len).collect();
            if let Err(e) = self.record(&mut rec) {
                return Err(self.fail(e));
            }
        }
        Ok(())
    }

    /// Sends application data, in records of at most 2^14 bytes.
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        match (self.state, &mut self.keys) {
            (State::Connected, Some(k)) => {
                for chunk in data.chunks(MAX_PLAIN) {
                    k.write.seal(content::APPLICATION_DATA, chunk, &mut self.out);
                }
                Ok(())
            }
            (State::Failed(e), _) => Err(e),
            _ => Err(Error::NotOpen),
        }
    }

    /// Sends close_notify; nothing more is written.
    pub fn close(&mut self) {
        if self.state == State::Connected {
            self.alert_out(Alert::CLOSE_NOTIFY);
            self.state = State::Closed;
        }
    }

    fn fail(&mut self, e: Error) -> Error {
        let alert = match e {
            Error::Sent(a) => Some(a),
            Error::Certificate(r) => Some(certificate_alert(r)),
            _ => None,
        };
        if let Some(a) = alert {
            self.alert_out(a);
        }
        self.state = State::Failed(e);
        wipe(&mut self.x25519);
        wipe(&mut self.p256);
        e
    }

    fn alert_out(&mut self, a: Alert) {
        let level = if a == Alert::CLOSE_NOTIFY { 1 } else { 2 };
        match &mut self.keys {
            Some(k) => k.write.seal(content::ALERT, &[level, a.0], &mut self.out),
            None => record::plain(content::ALERT, 0x0303, &[level, a.0], &mut self.out),
        }
    }

    fn record(&mut self, rec: &mut [u8]) -> Result<(), Error> {
        let (header, body) = rec.split_at_mut(record::HEADER);
        let header: &[u8; record::HEADER] = (&*header).try_into().expect("5");
        let handshaking = !matches!(self.state, State::Connected | State::Closed);
        match (header[0], &mut self.keys) {
            // A middlebox-compatibility ChangeCipherSpec, which a server may send
            // during the handshake: dropped (§ 5).
            (content::CHANGE_CIPHER_SPEC, _) if handshaking && body == [1] => Ok(()),
            (content::HANDSHAKE, None) if !body.is_empty() => self.handshake(body),
            (content::ALERT, None) => self.alert_in(body),
            (content::APPLICATION_DATA, Some(k)) => {
                let (inner, n) = k.read.open(header, body).map_err(Error::Sent)?;
                let data = &body[..n];
                match inner {
                    content::HANDSHAKE if n > 0 => self.handshake(data),
                    content::ALERT if self.hs.is_empty() => self.alert_in(data),
                    content::APPLICATION_DATA if self.state == State::Connected && self.hs.is_empty() => {
                        self.data.extend_from_slice(data);
                        Ok(())
                    }
                    _ => Err(Error::Sent(Alert::UNEXPECTED_MESSAGE)),
                }
            }
            _ => Err(Error::Sent(Alert::UNEXPECTED_MESSAGE)),
        }
    }

    fn alert_in(&mut self, body: &[u8]) -> Result<(), Error> {
        let [_, desc] = body else { return Err(Error::Sent(Alert::DECODE_ERROR)) };
        match Alert(*desc) {
            Alert::CLOSE_NOTIFY => {
                self.peer_closed = true;
                Ok(())
            }
            Alert::USER_CANCELED => Ok(()), // a warning; close_notify follows
            a => Err(Error::Received(a)),
        }
    }

    fn handshake(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.hs.extend_from_slice(bytes);
        while self.hs.len() >= 4 {
            let len = (self.hs[1] as usize) << 16 | (self.hs[2] as usize) << 8 | self.hs[3] as usize;
            if len > MAX_MESSAGE {
                return Err(Error::Sent(Alert::ILLEGAL_PARAMETER));
            }
            if self.hs.len() < 4 + len {
                break;
            }
            let m: Vec<u8> = self.hs.drain(..4 + len).collect();
            self.message(&m)?;
        }
        Ok(())
    }

    /// Keys change only between records: nothing of the next message may be waiting.
    fn key_change(&self) -> Result<(), Error> {
        if self.hs.is_empty() { Ok(()) } else { Err(Error::Sent(Alert::UNEXPECTED_MESSAGE)) }
    }

    fn message(&mut self, m: &[u8]) -> Result<(), Error> {
        let body = &m[4..];
        let sent = Error::Sent;
        match (self.state, m[0]) {
            (State::ServerHello, hs::SERVER_HELLO) => self.server_hello(m),
            (State::EncryptedExtensions, hs::ENCRYPTED_EXTENSIONS) => {
                msg::encrypted_extensions(body, &self.offered).map_err(sent)?;
                self.keys_mut().transcript.update(m);
                self.state = State::CertificateOrRequest;
                Ok(())
            }
            (State::CertificateOrRequest, hs::CERTIFICATE_REQUEST) => {
                self.requested = Some(msg::certificate_request(body).map_err(sent)?.to_vec());
                self.keys_mut().transcript.update(m);
                self.state = State::Certificate;
                Ok(())
            }
            (State::CertificateOrRequest | State::Certificate, hs::CERTIFICATE) => {
                let certs = msg::certificate(body).map_err(sent)?;
                let roots = self.roots;
                fx509::verify::verify(&certs, roots, &self.host, self.now).map_err(Error::Certificate)?;
                self.chain = certs.iter().map(|c| c.to_vec()).collect();
                self.keys_mut().transcript.update(m);
                self.state = State::CertificateVerify;
                Ok(())
            }
            (State::CertificateVerify, hs::CERTIFICATE_VERIFY) => {
                let (s, sig) = msg::certificate_verify(body).map_err(sent)?;
                let content = msg::signed_content(self.keys_mut().transcript.hash().as_slice());
                let leaf =
                    Certificate::parse(&self.chain[0]).map_err(|u| Error::Certificate(Refused::Unreadable(u)))?;
                if !scheme::OFFERED[..5].contains(&s) {
                    return Err(sent(Alert::ILLEGAL_PARAMETER));
                }
                if !handshake_signed(&leaf.key, s, &content, sig) {
                    return Err(sent(Alert::DECRYPT_ERROR));
                }
                self.keys_mut().transcript.update(m);
                self.state = State::Finished;
                Ok(())
            }
            (State::Finished, hs::FINISHED) => self.finished(m),
            (State::Connected | State::Closed, hs::NEW_SESSION_TICKET) => Ok(()), // no resumption
            (State::Connected | State::Closed, hs::KEY_UPDATE) => self.key_update(body),
            _ => Err(sent(Alert::UNEXPECTED_MESSAGE)),
        }
    }

    fn keys_mut(&mut self) -> &mut Keys {
        self.keys.as_mut().expect("keys exist after the ServerHello")
    }

    fn server_hello(&mut self, m: &[u8]) -> Result<(), Error> {
        let sent = Error::Sent;
        let sh = msg::server_hello(&m[4..]).map_err(sent)?;
        if sh.version != Some(msg::TLS13) {
            return Err(sent(Alert::PROTOCOL_VERSION)); // TLS 1.3 only
        }
        if !sh.session_id.is_empty() {
            return Err(sent(Alert::ILLEGAL_PARAMETER));
        }
        if sh.retry {
            return self.hello_retry(&sh, m);
        }
        let suite = Suite::from_id(sh.suite).ok_or(sent(Alert::ILLEGAL_PARAMETER))?;
        // After a HelloRetryRequest the ServerHello keeps its suite and its group (§ 4.1.4).
        let (mut transcript, want) = match self.retry.take() {
            Some((s, g, t)) if s == suite => (t, g),
            Some(_) => return Err(sent(Alert::ILLEGAL_PARAMETER)),
            None => {
                let mut t = Transcript::new(suite);
                t.update(&self.hello);
                (t, group::X25519)
            }
        };
        transcript.update(m);
        let mut shared = match sh.share {
            Some((g, k)) if g == want && g == group::X25519 => {
                let peer: &[u8; 32] = k.try_into().map_err(|_| sent(Alert::ILLEGAL_PARAMETER))?;
                let s = fcrypto::x25519::x25519(&self.x25519, peer);
                if fcrypto::ct::eq(&s, &[0; 32]) {
                    return Err(sent(Alert::ILLEGAL_PARAMETER)); // a small-order point (§ 7.4.2)
                }
                s
            }
            Some((g, k)) if g == want => fcrypto::p256::ecdh(&self.p256, k).ok_or(sent(Alert::ILLEGAL_PARAMETER))?,
            Some(_) => return Err(sent(Alert::ILLEGAL_PARAMETER)),
            None => return Err(sent(Alert::MISSING_EXTENSION)),
        };
        wipe(&mut self.x25519);
        wipe(&mut self.p256);
        let schedule = Schedule::handshake(suite, &shared);
        wipe(&mut shared);
        let th = transcript.hash();
        let client_hs = schedule.traffic(b"c hs traffic", &th);
        let server_hs = schedule.traffic(b"s hs traffic", &th);
        self.keys = Some(Keys {
            suite,
            read: Protection::new(suite, &server_hs),
            write: Protection::new(suite, &client_hs),
            read_secret: server_hs.clone(),
            write_secret: client_hs.clone(),
            transcript,
            schedule,
            client_hs,
            server_hs,
        });
        self.hello = Vec::new();
        self.state = State::EncryptedExtensions;
        self.key_change()
    }

    /// A HelloRetryRequest (§ 4.1.4): it must ask for P-256 (X25519's share
    /// was sent, and nothing else is offered), once. The second ClientHello
    /// keeps the random, carries a P-256 share and echoes any cookie; the
    /// transcript begins again with the first ClientHello's hash (§ 4.4.1).
    fn hello_retry(&mut self, sh: &msg::ServerHello, m: &[u8]) -> Result<(), Error> {
        let sent = Error::Sent;
        if self.retried {
            return Err(sent(Alert::UNEXPECTED_MESSAGE)); // a second HelloRetryRequest
        }
        let suite = Suite::from_id(sh.suite).ok_or(sent(Alert::ILLEGAL_PARAMETER))?;
        match sh.share {
            Some((group::SECP256R1, _)) => {}
            _ => return Err(sent(Alert::ILLEGAL_PARAMETER)),
        }
        let share = fcrypto::p256::public_key(&self.p256).ok_or(sent(Alert::INTERNAL_ERROR))?;
        wipe(&mut self.x25519);
        let random: [u8; 32] = self.hello[6..38].try_into().expect("a ClientHello's random");
        let hello = msg::client_hello(&random, &self.host, &[(group::SECP256R1, &share)], &group::OFFERED, sh.cookie);
        let first = {
            let mut t = Transcript::new(suite);
            t.update(&self.hello);
            t.hash()
        };
        let mut t = Transcript::new(suite);
        t.update(&[254, 0, 0, suite.hash_len() as u8]); // message_hash
        t.update(first.as_slice());
        t.update(m);
        t.update(&hello);
        record::plain(content::HANDSHAKE, 0x0303, &hello, &mut self.out);
        self.offered = msg::offered(&hello).expect("the client's own ClientHello");
        self.hello = hello;
        self.retry = Some((suite, group::SECP256R1, t));
        self.retried = true;
        Ok(())
    }

    fn finished(&mut self, m: &[u8]) -> Result<(), Error> {
        let requested = self.requested.take();
        let k = self.keys_mut();
        let want = schedule::finished(k.suite, &k.server_hs, &k.transcript.hash());
        if !fcrypto::ct::eq(&m[4..], want.as_slice()) {
            return Err(Error::Sent(Alert::DECRYPT_ERROR));
        }
        k.transcript.update(m);
        let th = k.transcript.hash();
        let master = k.schedule.master();
        let client_ap = master.traffic(b"c ap traffic", &th);
        let server_ap = master.traffic(b"s ap traffic", &th);
        let mut out = Vec::new();
        if let Some(context) = requested {
            let c = msg::empty_certificate(&context);
            k.transcript.update(&c);
            k.write.seal(content::HANDSHAKE, &c, &mut out);
        }
        let verify = schedule::finished(k.suite, &k.client_hs, &k.transcript.hash());
        let fin = msg::message(hs::FINISHED, |b| b.extend_from_slice(verify.as_slice()));
        k.transcript.update(&fin);
        k.write.seal(content::HANDSHAKE, &fin, &mut out);
        k.write = Protection::new(k.suite, &client_ap);
        k.read = Protection::new(k.suite, &server_ap);
        k.write_secret = client_ap;
        k.read_secret = server_ap;
        self.out.extend_from_slice(&out);
        self.chain = Vec::new();
        self.state = State::Connected;
        self.key_change()
    }

    fn key_update(&mut self, body: &[u8]) -> Result<(), Error> {
        let asked = match body {
            [0] => false,
            [1] => true,
            _ => return Err(Error::Sent(Alert::DECODE_ERROR)),
        };
        self.key_change()?;
        let k = self.keys_mut();
        k.read_secret = schedule::next_traffic(k.suite, &k.read_secret);
        k.read = Protection::new(k.suite, &k.read_secret);
        if asked && self.state == State::Connected {
            let k = self.keys.as_mut().expect("keys");
            let m = msg::message(hs::KEY_UPDATE, |b| b.push(0));
            k.write.seal(content::HANDSHAKE, &m, &mut self.out);
            k.write_secret = schedule::next_traffic(k.suite, &k.write_secret);
            k.write = Protection::new(k.suite, &k.write_secret);
        }
        Ok(())
    }
}

/// Whether a CertificateVerify's `sig` under scheme `s` verifies with the leaf's key.
pub(crate) fn handshake_signed(key: &Key, s: u16, content: &[u8], sig: &[u8]) -> bool {
    use fcrypto::ecdsa::Curve;
    use fcrypto::rsa::Hash;
    let pss = |h: Hash| match key {
        Key::Rsa { n, e } => fcrypto::rsa::PublicKey::new(n, e).is_some_and(|k| {
            let d = match h {
                Hash::Sha256 => fcrypto::sha2::sha256(content).to_vec(),
                Hash::Sha384 => fcrypto::sha2::sha384(content).to_vec(),
                Hash::Sha512 => fcrypto::sha2::sha512(content).to_vec(),
            };
            k.verify_pss(h, &d, sig)
        }),
        _ => false,
    };
    match (s, key) {
        (scheme::ECDSA_P256_SHA256, Key::Ec { curve: Curve::P256, .. }) => {
            fx509::verify::signed(key, SigAlg::Ecdsa(Hash::Sha256), content, sig)
        }
        (scheme::ECDSA_P384_SHA384, Key::Ec { curve: Curve::P384, .. }) => {
            fx509::verify::signed(key, SigAlg::Ecdsa(Hash::Sha384), content, sig)
        }
        (scheme::RSA_PSS_SHA256, _) => pss(Hash::Sha256),
        (scheme::RSA_PSS_SHA384, _) => pss(Hash::Sha384),
        (scheme::RSA_PSS_SHA512, _) => pss(Hash::Sha512),
        _ => false,
    }
}

/// The alert a refused chain is answered with.
pub fn certificate_alert(r: Refused) -> Alert {
    match r {
        Refused::UnknownRoot => Alert::UNKNOWN_CA,
        Refused::Expired | Refused::NotYetValid => Alert::CERTIFICATE_EXPIRED,
        Refused::Name => Alert::CERTIFICATE_UNKNOWN,
        Refused::Usage | Refused::Unreadable(fx509::cert::Unreadable::Algorithm) => Alert::UNSUPPORTED_CERTIFICATE,
        Refused::Empty => Alert::DECODE_ERROR,
        _ => Alert::BAD_CERTIFICATE,
    }
}
