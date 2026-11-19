// SPDX-License-Identifier: Apache-2.0
//! accounts (TASK-101, PRD § 10, FR-045/046): the account records, the
//! passwords, and the master key's lifecycle. This library is the record's
//! format, the key derivation and wrapping, the rules for names and
//! passwords, and the throttle, tested on the host; the service
//! (`main.rs`) keeps the records under `/system/accounts`, hands a logged-in
//! account's master key to ffs for its home, and zeroes it at logout.
//!
//! `KEK = HKDF-SHA256(Argon2id(password, salt), "freally/account/kek/v1")`;
//! the master key is sealed under it with XChaCha20-Poly1305, the record's
//! id as associated data.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod facct;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::accounts as abi;
use fcrypto::argon2::{Block, Params, argon2id};

pub const MAGIC: [u8; 8] = *b"FRLYAREC";
pub const VERSION: u16 = 1;
pub const RECORD_LEN: usize = 512;
/// PRD § 10: Argon2id at 64 MiB, 3 passes, 1 lane (32 MiB under 2 GiB of RAM).
pub const DEFAULT_KDF: Params = Params { m_kib: abi::ARGON2_M_KIB, t: abi::ARGON2_T, p: abi::ARGON2_P };
pub const SMALL_KDF: Params = Params { m_kib: abi::ARGON2_M_KIB_SMALL, t: abi::ARGON2_T, p: abi::ARGON2_P };
pub const MIN_PASSWORD: usize = abi::MIN_PASSWORD;
const KEK_INFO: &[u8] = b"freally/account/kek/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Owner,
    Standard,
}

/// What's wrong with a record, a name or a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not a record, or a damaged one.
    Damaged,
    /// `^[a-z][a-z0-9_-]{1,31}$`.
    BadUsername,
    /// Over 64 characters.
    BadDisplayName,
    /// Shorter than 8 characters.
    ShortPassword,
    /// The password doesn't open the master key.
    WrongPassword,
    /// The key derivation couldn't run (its parameters, or memory).
    Kdf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: [u8; 16],
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub created_unix: u64,
    pub kdf: Params,
    pub salt: [u8; 16],
    pub wrapped_master_key: [u8; 48],
    pub wrap_nonce: [u8; 24],
    pub home_subvolume: u64,
    pub dark: bool,
    pub ui_scale_pct: u16,
    pub locale: [u8; 8],
}

/// `^[a-z][a-z0-9_-]{1,31}$`.
pub fn valid_username(u: &str) -> bool {
    let b = u.as_bytes();
    (2..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..].iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

pub fn check_password(p: &str) -> Result<(), Refused> {
    if p.chars().count() < MIN_PASSWORD { Err(Refused::ShortPassword) } else { Ok(()) }
}

/// The record file's name: its id as 32 hex digits, `.rec`.
pub fn file_name(id: &[u8; 16]) -> String {
    let mut s = String::with_capacity(36);
    for b in id {
        s.push_str(&alloc::format!("{b:02x}"));
    }
    s.push_str(".rec");
    s
}

impl AccountRecord {
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut b = [0u8; RECORD_LEN];
        b[..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&VERSION.to_le_bytes());
        b[10] = (self.role == Role::Owner) as u8;
        b[11] = (!self.dark) as u8;
        b[12..14].copy_from_slice(&self.ui_scale_pct.to_le_bytes());
        b[16..32].copy_from_slice(&self.id);
        b[32..40].copy_from_slice(&self.created_unix.to_le_bytes());
        b[40..44].copy_from_slice(&self.kdf.m_kib.to_le_bytes());
        b[44..48].copy_from_slice(&self.kdf.t.to_le_bytes());
        b[48..52].copy_from_slice(&self.kdf.p.to_le_bytes());
        b[56..72].copy_from_slice(&self.salt);
        b[72..120].copy_from_slice(&self.wrapped_master_key);
        b[120..144].copy_from_slice(&self.wrap_nonce);
        b[144..152].copy_from_slice(&self.home_subvolume.to_le_bytes());
        b[152..160].copy_from_slice(&self.locale);
        let u = &self.username.as_bytes()[..self.username.len().min(32)];
        b[160] = u.len() as u8;
        b[161..161 + u.len()].copy_from_slice(u);
        let d = &self.display_name.as_bytes()[..self.display_name.len().min(255)];
        b[193] = d.len() as u8;
        b[194..194 + d.len()].copy_from_slice(d);
        let crc = fcrypto::crc32c::crc32c(&b[..508]);
        b[508..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Result<AccountRecord, Refused> {
        if b.len() != RECORD_LEN || b[..8] != MAGIC || u16::from_le_bytes([b[8], b[9]]) != VERSION {
            return Err(Refused::Damaged);
        }
        if fcrypto::crc32c::crc32c(&b[..508]).to_le_bytes() != b[508..512] {
            return Err(Refused::Damaged);
        }
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or_default());
        let arr = |o: usize, n: usize| b[o..o + n].to_vec();
        let ulen = b[160] as usize;
        let dlen = b[193] as usize;
        if ulen > 32 {
            return Err(Refused::Damaged);
        }
        let username = String::from(core::str::from_utf8(&b[161..161 + ulen]).map_err(|_| Refused::Damaged)?);
        let display_name = String::from(core::str::from_utf8(&b[194..194 + dlen]).map_err(|_| Refused::Damaged)?);
        if !valid_username(&username) {
            return Err(Refused::Damaged);
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&arr(16, 16));
        let mut salt = [0u8; 16];
        salt.copy_from_slice(&arr(56, 16));
        let mut wrapped_master_key = [0u8; 48];
        wrapped_master_key.copy_from_slice(&arr(72, 48));
        let mut wrap_nonce = [0u8; 24];
        wrap_nonce.copy_from_slice(&arr(120, 24));
        let mut locale = [0u8; 8];
        locale.copy_from_slice(&arr(152, 8));
        Ok(AccountRecord {
            id,
            username,
            display_name,
            role: if b[10] == 1 { Role::Owner } else { Role::Standard },
            created_unix: u64_at(32),
            kdf: Params { m_kib: u32_at(40), t: u32_at(44), p: u32_at(48) },
            salt,
            wrapped_master_key,
            wrap_nonce,
            home_subvolume: u64_at(144),
            dark: b[11] == 0,
            ui_scale_pct: u16::from_le_bytes([b[12], b[13]]),
            locale,
        })
    }
}

/// The key-encryption key for `password` with `salt` under `kdf`.
pub fn kek(password: &str, salt: &[u8; 16], kdf: Params) -> Result<[u8; 32], Refused> {
    derive(password, salt, kdf, KEK_INFO)
}

/// `HKDF-SHA256(Argon2id(password, salt), info)`: the KEK, or a `.facct`'s key.
pub fn derive(password: &str, salt: &[u8; 16], kdf: Params, info: &[u8]) -> Result<[u8; 32], Refused> {
    let mut memory: Vec<Block> = Vec::new();
    memory.try_reserve_exact(kdf.blocks()).map_err(|_| Refused::Kdf)?;
    memory.resize(kdf.blocks(), [0u64; 128]);
    let mut stretched = [0u8; 32];
    let made = argon2id(kdf, password.as_bytes(), salt, &[], &[], &mut memory, &mut stretched);
    // The derivation's memory, its last blocks a step from the KEK.
    for block in memory.iter_mut() {
        for w in block.iter_mut() {
            // SAFETY: a valid word of the derivation's memory.
            unsafe { core::ptr::write_volatile(w, 0) };
        }
    }
    made.map_err(|_| Refused::Kdf)?;
    let mut out = [0u8; 32];
    let expanded = fcrypto::hkdf::hkdf(&[], &stretched, info, &mut out);
    wipe(&mut stretched);
    expanded.map_err(|_| Refused::Kdf)?;
    Ok(out)
}

/// Seals `master` under `kek` with `nonce`, bound to the record `id`.
pub fn wrap(kek: &[u8; 32], nonce: &[u8; 24], id: &[u8; 16], master: &[u8; 32]) -> [u8; 48] {
    let mut out = [0u8; 48];
    out[..32].copy_from_slice(master);
    let tag = fcrypto::aead::xseal(kek, nonce, id, &mut out[..32]);
    out[32..].copy_from_slice(&tag);
    out
}

/// The master key, or `WrongPassword` (any change to the record's key, its
/// nonce, its id, or a wrong KEK).
pub fn unwrap(kek: &[u8; 32], nonce: &[u8; 24], id: &[u8; 16], wrapped: &[u8; 48]) -> Result<[u8; 32], Refused> {
    let mut key = [0u8; 32];
    key.copy_from_slice(&wrapped[..32]);
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&wrapped[32..]);
    fcrypto::aead::xopen(kek, nonce, id, &mut key, &tag).map_err(|_| Refused::WrongPassword)?;
    Ok(key)
}

/// Overwrites a secret so it doesn't outlive its use.
pub fn wipe(k: &mut [u8]) {
    for b in k.iter_mut() {
        // SAFETY: a valid byte of `k`.
        unsafe { core::ptr::write_volatile(b, 0) };
    }
}

/// US-015's throttle: from the third failure in a row, each wrong password
/// waits one more second before the next try is heard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Throttle {
    pub failures: u32,
}

impl Throttle {
    /// A failure: the wait (ns) before the next try.
    pub fn failed(&mut self) -> u64 {
        self.failures += 1;
        (self.failures.saturating_sub(2) as u64) * 1_000_000_000
    }

    pub fn succeeded(&mut self) {
        self.failures = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY: Params = Params { m_kib: 64, t: 1, p: 1 };

    fn record() -> AccountRecord {
        AccountRecord {
            id: [7; 16],
            username: String::from("mike"),
            display_name: String::from("Mike Weaver — owner"),
            role: Role::Owner,
            created_unix: 1_763_500_000,
            kdf: DEFAULT_KDF,
            salt: [1; 16],
            wrapped_master_key: [2; 48],
            wrap_nonce: [3; 24],
            home_subvolume: 257,
            dark: true,
            ui_scale_pct: 150,
            locale: *b"en-US\0\0\0",
        }
    }

    #[test]
    fn a_record_round_trips_and_damage_is_caught() {
        let r = record();
        let b = r.encode();
        assert_eq!(AccountRecord::decode(&b), Ok(r));
        let mut bad = b;
        bad[200] ^= 1;
        assert_eq!(AccountRecord::decode(&bad), Err(Refused::Damaged), "a flipped bit");
        assert_eq!(AccountRecord::decode(&b[..511]), Err(Refused::Damaged));
        assert_eq!(file_name(&[0xAB; 16]), "abababababababababababababababab.rec");
    }

    #[test]
    fn names_and_passwords() {
        for good in ["mike", "a1", "dana_k", "sam-2026", "abcdefghijklmnopqrstuvwxyz012345"] {
            assert!(valid_username(good), "{good}");
        }
        for bad in ["", "m", "Mike", "1mike", "mike!", "mi ke", "abcdefghijklmnopqrstuvwxyz0123456"] {
            assert!(!valid_username(bad), "{bad}");
        }
        assert_eq!(check_password("seven77"), Err(Refused::ShortPassword));
        assert_eq!(check_password("eight888"), Ok(()));
        assert_eq!(check_password("ünïcødé!"), Ok(()), "characters, not bytes");
    }

    #[test]
    fn the_master_key_opens_with_the_password_alone() {
        let id = [9u8; 16];
        let (salt, nonce, master) = ([4u8; 16], [5u8; 24], [6u8; 32]);
        let k = kek("correct horse", &salt, TINY).unwrap();
        let w = wrap(&k, &nonce, &id, &master);
        assert_eq!(unwrap(&k, &nonce, &id, &w), Ok(master));
        let wrong = kek("correct horsf", &salt, TINY).unwrap();
        assert_eq!(unwrap(&wrong, &nonce, &id, &w), Err(Refused::WrongPassword));
        assert_eq!(unwrap(&k, &nonce, &[8; 16], &w), Err(Refused::WrongPassword), "moved to another record");
        let other_salt = kek("correct horse", &[0; 16], TINY).unwrap();
        assert_ne!(other_salt, k, "the salt matters");
        let mut tampered = w;
        tampered[3] ^= 1;
        assert_eq!(unwrap(&k, &nonce, &id, &tampered), Err(Refused::WrongPassword));
    }

    #[test]
    fn the_throttle_waits_from_the_third_failure() {
        let mut t = Throttle::default();
        assert_eq!((t.failed(), t.failed(), t.failed(), t.failed()), (0, 0, 1_000_000_000, 2_000_000_000));
        t.succeeded();
        assert_eq!(t.failed(), 0);
    }
}
