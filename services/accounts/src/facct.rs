// SPDX-License-Identifier: Apache-2.0
//! The account bundle (`.facct`, TASK-103, PRD § 3): a 128-byte header,
//! then the FPK archive in 64 KiB chunks, each sealed with
//! XChaCha20-Poly1305 under `HKDF-SHA256(Argon2id(password, salt),
//! "freally/facct/v1")`. The header's tag catches a wrong password; each
//! chunk's tag, the index in its nonce, the last flag and the payload's
//! length catch a change, a move or a file that ends early.
//!
//! The FPK archive inside: magic, the account's record (its wrap fields
//! zeroed), when it was exported, then the home's tree, each folder before
//! what's in it, and an end.

use crate::{AccountRecord, Refused, wipe};
use alloc::string::String;
use alloc::vec::Vec;
use fcrypto::aead::{xopen, xseal};
use fcrypto::argon2::Params;

pub const MAGIC: [u8; 8] = *b"FRLYACCT";
pub const VERSION: u16 = 1;
pub const HEADER: usize = 128;
pub const CHUNK: usize = 64 * 1024;
pub const TAG: usize = 16;
const INFO: &[u8] = b"freally/facct/v1";
pub const FPK_MAGIC: [u8; 8] = *b"FRLYFPK1";

/// What's wrong with a bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// "That password doesn't open this file."
    WrongPassword,
    /// "This account file is damaged or incomplete, so nothing was imported."
    Damaged,
    /// Not a `.facct` at all, or a version this build doesn't read.
    NotAnAccount,
    /// The key derivation couldn't run.
    Kdf,
}

impl Problem {
    pub fn words(self) -> &'static str {
        match self {
            Problem::WrongPassword => "That password doesn't open this file.",
            Problem::Damaged => "This account file is damaged or incomplete, so nothing was imported.",
            Problem::NotAnAccount => "This isn't a Freally OS account file.",
            Problem::Kdf => "There isn't enough memory to check the password.",
        }
    }
}

fn key(password: &str, salt: &[u8; 16], kdf: Params) -> Result<[u8; 32], Problem> {
    crate::derive(password, salt, kdf, INFO)
        .map_err(|e| if e == Refused::Kdf { Problem::Kdf } else { Problem::WrongPassword })
}

fn nonce(prefix: &[u8; 16], index: u32, last: bool) -> [u8; 24] {
    let mut n = [0u8; 24];
    n[..16].copy_from_slice(prefix);
    n[16..20].copy_from_slice(&index.to_le_bytes());
    n[20] = last as u8;
    n
}

/// The header's own nonce: index FFFFFFFF, last FF (no chunk has it).
fn header_nonce(prefix: &[u8; 16]) -> [u8; 24] {
    let mut n = nonce(prefix, u32::MAX, false);
    n[20] = 0xFF;
    n
}

/// How many chunks a payload of `len` bytes takes (an empty one, one).
pub fn chunks(len: u64) -> u64 {
    len.div_ceil(CHUNK as u64).max(1)
}

/// The bundle's size for a payload of `len` bytes.
pub fn size(len: u64) -> u64 {
    HEADER as u64 + len + chunks(len) * TAG as u64
}

/// Writes a bundle: the header now, then each chunk as the payload comes.
pub struct Writer {
    key: [u8; 32],
    prefix: [u8; 16],
    len: u64,
    index: u32,
    written: u64,
    buf: Vec<u8>,
}

impl Drop for Writer {
    fn drop(&mut self) {
        wipe(&mut self.key);
        wipe(&mut self.buf);
    }
}

impl Writer {
    /// The header, and the writer for a payload of exactly `len` bytes.
    pub fn start(
        password: &str,
        kdf: Params,
        salt: [u8; 16],
        prefix: [u8; 16],
        len: u64,
    ) -> Result<([u8; HEADER], Writer), Problem> {
        let k = key(password, &salt, kdf)?;
        let mut h = [0u8; HEADER];
        h[..8].copy_from_slice(&MAGIC);
        h[8..10].copy_from_slice(&VERSION.to_le_bytes());
        h[12..16].copy_from_slice(&kdf.m_kib.to_le_bytes());
        h[16..20].copy_from_slice(&kdf.t.to_le_bytes());
        h[20..24].copy_from_slice(&kdf.p.to_le_bytes());
        h[24..40].copy_from_slice(&salt);
        h[40..56].copy_from_slice(&prefix);
        h[56..64].copy_from_slice(&len.to_le_bytes());
        let tag = xseal(&k, &header_nonce(&prefix), &h[..112], &mut []);
        h[112..].copy_from_slice(&tag);
        Ok((h, Writer { key: k, prefix, len, index: 0, written: 0, buf: Vec::new() }))
    }

    /// More of the payload: every whole chunk it completes, sealed, onto `out`.
    pub fn push(&mut self, mut data: &[u8], out: &mut Vec<u8>) -> Result<(), Problem> {
        if self.written + self.buf.len() as u64 + data.len() as u64 > self.len {
            return Err(Problem::Damaged);
        }
        while !data.is_empty() {
            let take = (CHUNK - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            // A full chunk that isn't the last is sealed now.
            if self.buf.len() == CHUNK && self.written + (CHUNK as u64) < self.len {
                self.seal(false, out);
            }
        }
        Ok(())
    }

    fn seal(&mut self, last: bool, out: &mut Vec<u8>) {
        let tag = xseal(&self.key, &nonce(&self.prefix, self.index, last), &[], &mut self.buf);
        out.extend_from_slice(&self.buf);
        out.extend_from_slice(&tag);
        self.written += self.buf.len() as u64;
        wipe(&mut self.buf);
        self.buf.clear();
        self.index += 1;
    }

    /// The last chunk, sealed onto `out`; the payload must be whole.
    pub fn finish(mut self, out: &mut Vec<u8>) -> Result<(), Problem> {
        if self.written + self.buf.len() as u64 != self.len {
            return Err(Problem::Damaged);
        }
        self.seal(true, out);
        Ok(())
    }
}

/// Reads a bundle: the header checked against the password, then each
/// chunk in turn.
pub struct Reader {
    key: [u8; 32],
    prefix: [u8; 16],
    pub len: u64,
    index: u64,
}

impl Drop for Reader {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

impl Reader {
    pub fn open(header: &[u8], password: &str) -> Result<Reader, Problem> {
        if header.len() < HEADER || header[..8] != MAGIC {
            return Err(Problem::NotAnAccount);
        }
        if u16::from_le_bytes([header[8], header[9]]) != VERSION {
            return Err(Problem::NotAnAccount);
        }
        let u32_at = |o: usize| u32::from_le_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);
        let kdf = Params { m_kib: u32_at(12), t: u32_at(16), p: u32_at(20) };
        // Never a derivation an attacker's header could make enormous.
        if kdf.m_kib > 4 * 1024 * 1024 || kdf.t > 64 || kdf.p != 1 {
            return Err(Problem::Damaged);
        }
        let mut salt = [0u8; 16];
        salt.copy_from_slice(&header[24..40]);
        let mut prefix = [0u8; 16];
        prefix.copy_from_slice(&header[40..56]);
        let len = u64::from_le_bytes(header[56..64].try_into().unwrap_or_default());
        let mut tag = [0u8; 16];
        tag.copy_from_slice(&header[112..HEADER]);
        let k = key(password, &salt, kdf)?;
        if xopen(&k, &header_nonce(&prefix), &header[..112], &mut [], &tag).is_err() {
            let mut k = k;
            wipe(&mut k);
            return Err(Problem::WrongPassword);
        }
        Ok(Reader { key: k, prefix, len, index: 0 })
    }

    /// How many sealed bytes the next chunk is (0 once all are read).
    pub fn next_size(&self) -> usize {
        if self.index >= chunks(self.len) {
            return 0;
        }
        let left = self.len - self.index * CHUNK as u64;
        left.min(CHUNK as u64) as usize + TAG
    }

    /// The next chunk, opened: its plaintext.
    pub fn chunk(&mut self, sealed: &[u8]) -> Result<Vec<u8>, Problem> {
        let want = self.next_size();
        if want == 0 || sealed.len() != want {
            return Err(Problem::Damaged);
        }
        let last = self.index + 1 == chunks(self.len);
        let (body, t) = sealed.split_at(want - TAG);
        let mut plain = body.to_vec();
        let mut tag = [0u8; 16];
        tag.copy_from_slice(t);
        xopen(&self.key, &nonce(&self.prefix, self.index as u32, last), &[], &mut plain, &tag)
            .map_err(|_| Problem::Damaged)?;
        self.index += 1;
        Ok(plain)
    }

    /// Whether every chunk has been read.
    pub fn done(&self) -> bool {
        self.index >= chunks(self.len)
    }
}

/// What an archive holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File = 1,
    Folder = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    /// Relative to the home, `/` between names.
    pub path: String,
    pub modified_ns: u64,
    pub len: u64,
}

/// A path an archive may hold: names separated by `/`, none empty, `.`,
/// `..`, or holding a NUL.
pub fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && p.split('/').all(|n| !n.is_empty() && n != "." && n != ".." && !n.contains('\0') && n.len() <= 255)
}

/// The archive's start: magic, the record with its wrap fields zeroed, the
/// export time.
pub fn fpk_start(record: &AccountRecord, exported_unix: u64) -> Vec<u8> {
    let r = AccountRecord {
        salt: [0; 16],
        wrapped_master_key: [0; 48],
        wrap_nonce: [0; 24],
        home_subvolume: 0,
        ..record.clone()
    };
    let mut out = Vec::with_capacity(8 + crate::RECORD_LEN + 8);
    out.extend_from_slice(&FPK_MAGIC);
    out.extend_from_slice(&r.encode());
    out.extend_from_slice(&exported_unix.to_le_bytes());
    out
}

/// An entry's head (a file's bytes follow it).
pub fn fpk_entry(e: &Entry) -> Vec<u8> {
    let mut out = Vec::with_capacity(23 + e.path.len());
    out.push(e.kind as u8);
    out.extend_from_slice(&(e.path.len() as u16).to_le_bytes());
    out.extend_from_slice(e.path.as_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&e.modified_ns.to_le_bytes());
    out.extend_from_slice(&if e.kind == Kind::File { e.len } else { 0 }.to_le_bytes());
    out
}

pub const FPK_END: u8 = 0;

/// The archive's length for these entries (each with its length).
pub fn fpk_len(entries: &[Entry]) -> u64 {
    let mut n = (8 + crate::RECORD_LEN + 8) as u64;
    for e in entries {
        n += 23 + e.path.len() as u64 + if e.kind == Kind::File { e.len } else { 0 };
    }
    n + 1
}

/// What the parser found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Account {
        record: AccountRecord,
        exported_unix: u64,
    },
    Start(Entry),
    /// Bytes of the file last started.
    Data(Vec<u8>),
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Head,
    Entry,
    Data(u64),
    Done,
}

/// Reads an archive as it comes, in pieces of any size.
#[derive(Debug)]
pub struct Parser {
    buf: Vec<u8>,
    stage: Stage,
}

impl Default for Parser {
    fn default() -> Parser {
        Parser { buf: Vec::new(), stage: Stage::Head }
    }
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, Problem> {
        if self.stage == Stage::Done && !bytes.is_empty() {
            return Err(Problem::Damaged);
        }
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            match self.stage {
                Stage::Head => {
                    let n = 8 + crate::RECORD_LEN + 8;
                    if self.buf.len() < n {
                        break;
                    }
                    if self.buf[..8] != FPK_MAGIC {
                        return Err(Problem::Damaged);
                    }
                    let record =
                        AccountRecord::decode(&self.buf[8..8 + crate::RECORD_LEN]).map_err(|_| Problem::Damaged)?;
                    let at = 8 + crate::RECORD_LEN;
                    let exported_unix = u64::from_le_bytes(self.buf[at..at + 8].try_into().unwrap_or_default());
                    self.buf.drain(..n);
                    out.push(Event::Account { record, exported_unix });
                    self.stage = Stage::Entry;
                }
                Stage::Entry => {
                    let Some(&kind) = self.buf.first() else { break };
                    if kind == FPK_END {
                        self.buf.remove(0);
                        if !self.buf.is_empty() {
                            return Err(Problem::Damaged);
                        }
                        out.push(Event::End);
                        self.stage = Stage::Done;
                        break;
                    }
                    if self.buf.len() < 3 {
                        break;
                    }
                    let plen = u16::from_le_bytes([self.buf[1], self.buf[2]]) as usize;
                    let n = 3 + plen + 20;
                    if self.buf.len() < n {
                        break;
                    }
                    let kind = match kind {
                        1 => Kind::File,
                        2 => Kind::Folder,
                        _ => return Err(Problem::Damaged),
                    };
                    let path = core::str::from_utf8(&self.buf[3..3 + plen]).map_err(|_| Problem::Damaged)?;
                    if !valid_path(path) {
                        return Err(Problem::Damaged);
                    }
                    let path = String::from(path);
                    let at = 3 + plen + 4;
                    let modified_ns = u64::from_le_bytes(self.buf[at..at + 8].try_into().unwrap_or_default());
                    let len = u64::from_le_bytes(self.buf[at + 8..at + 16].try_into().unwrap_or_default());
                    if kind == Kind::Folder && len != 0 {
                        return Err(Problem::Damaged);
                    }
                    self.buf.drain(..n);
                    out.push(Event::Start(Entry { kind, path, modified_ns, len }));
                    self.stage = if len > 0 { Stage::Data(len) } else { Stage::Entry };
                }
                Stage::Data(left) => {
                    if self.buf.is_empty() {
                        break;
                    }
                    let take = (left.min(self.buf.len() as u64)) as usize;
                    out.push(Event::Data(self.buf.drain(..take).collect()));
                    self.stage = if left == take as u64 { Stage::Entry } else { Stage::Data(left - take as u64) };
                }
                Stage::Done => break,
            }
        }
        Ok(out)
    }

    /// Whether the end was reached.
    pub fn done(&self) -> bool {
        self.stage == Stage::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const TINY: Params = Params { m_kib: 64, t: 1, p: 1 };

    fn bundle(payload: &[u8], password: &str) -> Vec<u8> {
        let (h, mut w) = Writer::start(password, TINY, [1; 16], [2; 16], payload.len() as u64).unwrap();
        let mut out = h.to_vec();
        // In odd pieces, as an export streams it.
        for piece in payload.chunks(10_007) {
            w.push(piece, &mut out).unwrap();
        }
        w.finish(&mut out).unwrap();
        out
    }

    fn read(file: &[u8], password: &str) -> Result<Vec<u8>, Problem> {
        let mut r = Reader::open(file.get(..HEADER).ok_or(Problem::Damaged)?, password)?;
        let mut at = HEADER;
        let mut out = Vec::new();
        while !r.done() {
            let n = r.next_size();
            out.extend(r.chunk(file.get(at..at + n).ok_or(Problem::Damaged)?)?);
            at += n;
        }
        if at != file.len() {
            return Err(Problem::Damaged);
        }
        Ok(out)
    }

    #[test]
    fn a_bundle_round_trips_at_every_chunk_boundary() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 77] {
            let payload: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let file = bundle(&payload, "correct horse");
            assert_eq!(file.len() as u64, size(len as u64), "{len}");
            assert_eq!(read(&file, "correct horse"), Ok(payload), "{len}");
        }
    }

    #[test]
    fn a_wrong_password_tampering_and_truncation_are_each_caught() {
        let payload: Vec<u8> = (0..(2 * CHUNK + 500)).map(|i| i as u8).collect();
        let file = bundle(&payload, "correct horse");
        assert_eq!(read(&file, "correct horsf"), Err(Problem::WrongPassword));
        // The header: its length, the salt, the tag.
        for at in [57, 30, 120] {
            let mut bad = file.clone();
            bad[at] ^= 1;
            assert_eq!(read(&bad, "correct horse"), Err(Problem::WrongPassword), "header byte {at}");
        }
        // A bit of every chunk, and a tag.
        for at in [HEADER + 5, HEADER + CHUNK + 40, file.len() - 3] {
            let mut bad = file.clone();
            bad[at] ^= 0x80;
            assert_eq!(read(&bad, "correct horse"), Err(Problem::Damaged), "byte {at}");
        }
        // Ending early: a chunk short, a whole chunk gone.
        assert_eq!(read(&file[..file.len() - 1], "correct horse"), Err(Problem::Damaged));
        assert_eq!(read(&file[..HEADER + CHUNK + TAG], "correct horse"), Err(Problem::Damaged));
        // Two whole chunks swapped.
        let mut swapped = file[..HEADER].to_vec();
        swapped.extend_from_slice(&file[HEADER + CHUNK + TAG..HEADER + 2 * (CHUNK + TAG)]);
        swapped.extend_from_slice(&file[HEADER..HEADER + CHUNK + TAG]);
        swapped.extend_from_slice(&file[HEADER + 2 * (CHUNK + TAG)..]);
        assert_eq!(read(&swapped, "correct horse"), Err(Problem::Damaged));
        // Something after the end.
        let mut longer = file.clone();
        longer.push(0);
        assert_eq!(read(&longer, "correct horse"), Err(Problem::Damaged));
        assert_eq!(
            read(b"not an account file at all, not even close to 128 bytes".repeat(4).as_slice(), "x"),
            Err(Problem::NotAnAccount)
        );
    }

    #[test]
    fn a_writer_refuses_a_payload_of_another_length() {
        let (_, mut w) = Writer::start("pw", TINY, [1; 16], [2; 16], 10).unwrap();
        let mut out = Vec::new();
        assert_eq!(w.push(&[0; 11], &mut out), Err(Problem::Damaged));
        w.push(&[0; 9], &mut out).unwrap();
        assert_eq!(w.finish(&mut out), Err(Problem::Damaged), "a byte short");
    }

    fn record() -> AccountRecord {
        AccountRecord {
            id: [7; 16],
            username: String::from("mike"),
            display_name: String::from("Mike"),
            role: crate::Role::Owner,
            created_unix: 1_763_500_000,
            kdf: crate::DEFAULT_KDF,
            salt: [1; 16],
            wrapped_master_key: [2; 48],
            wrap_nonce: [3; 24],
            home_subvolume: 257,
            dark: true,
            ui_scale_pct: 150,
            locale: *b"en-US\0\0\0",
            look: crate::Look::default(),
        }
    }

    #[test]
    fn an_archive_is_read_back_in_pieces_of_any_size() {
        let entries = vec![
            Entry { kind: Kind::Folder, path: "Documents".into(), modified_ns: 5, len: 0 },
            Entry { kind: Kind::File, path: "Documents/notes.txt".into(), modified_ns: 6, len: 5 },
            Entry { kind: Kind::File, path: "empty".into(), modified_ns: 7, len: 0 },
        ];
        let mut a = fpk_start(&record(), 1_763_600_000);
        a.extend(fpk_entry(&entries[0]));
        a.extend(fpk_entry(&entries[1]));
        a.extend_from_slice(b"hello");
        a.extend(fpk_entry(&entries[2]));
        a.push(FPK_END);
        assert_eq!(a.len() as u64, fpk_len(&entries));
        for piece in [1, 3, 64, a.len()] {
            let mut p = Parser::default();
            let mut events = Vec::new();
            for c in a.chunks(piece) {
                events.extend(p.feed(c).unwrap());
            }
            assert!(p.done());
            let Event::Account { record: r, exported_unix } = &events[0] else { panic!("no account first") };
            assert_eq!((r.username.as_str(), r.wrapped_master_key, r.home_subvolume), ("mike", [0; 48], 0));
            assert_eq!(*exported_unix, 1_763_600_000);
            let starts: Vec<&Entry> =
                events.iter().filter_map(|e| if let Event::Start(e) = e { Some(e) } else { None }).collect();
            assert_eq!(starts, entries.iter().collect::<Vec<_>>(), "piece {piece}");
            let data: Vec<u8> =
                events.iter().flat_map(|e| if let Event::Data(d) = e { d.clone() } else { Vec::new() }).collect();
            assert_eq!(data, b"hello");
            assert_eq!(events.last(), Some(&Event::End));
        }
    }

    #[test]
    fn an_archive_with_a_bad_path_or_a_tail_is_damaged() {
        for bad in ["", "/abs", "a//b", "a/../b", "..", "a/./b"] {
            assert!(!valid_path(bad), "{bad}");
        }
        assert!(valid_path("Documents/2026/notes.txt"));
        let mut a = fpk_start(&record(), 0);
        a.extend(fpk_entry(&Entry { kind: Kind::File, path: "../escape".into(), modified_ns: 0, len: 0 }));
        assert_eq!(Parser::default().feed(&a), Err(Problem::Damaged));
        let mut a = fpk_start(&record(), 0);
        a.extend_from_slice(&[FPK_END, 9]);
        assert_eq!(Parser::default().feed(&a), Err(Problem::Damaged), "bytes after the end");
    }
}
