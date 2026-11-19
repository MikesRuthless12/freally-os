// SPDX-License-Identifier: Apache-2.0
//! Argon2id (RFC 9106, version 0x13): what makes a guessed account password
//! slow and memory-hungry to check (PRD § 10: m = 64 MiB, t = 3, p = 1;
//! 32 MiB under 2 GiB of RAM).
//!
//! The caller lends the memory (`m` blocks of 1 KiB), so fcrypto needs no
//! allocator. Lanes are filled slice by slice, one after another: a lane
//! only ever reads other lanes' finished slices, so the answer is the
//! parallel one's.
//!
//! The first half of the first pass picks blocks by a data-independent
//! sequence (Argon2i's), the rest by the data (Argon2d's): that's the "id".

use crate::blake2b::{Blake2b, blake2b};

/// One 1 KiB block: 128 little-endian u64s.
pub type Block = [u64; 128];

const SYNC_POINTS: usize = 4;
const VERSION: u32 = 0x13;
const ARGON2ID: u32 = 2;

/// Argon2's parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// Memory, KiB (at least 8 × lanes).
    pub m_kib: u32,
    /// Passes.
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

/// The parameters or the memory lent can't work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    BadParams,
    /// The memory lent holds fewer blocks than the parameters need.
    TooLittleMemory,
}

impl Params {
    /// The blocks used: `m` rounded down to a multiple of 4 × lanes.
    pub fn blocks(&self) -> usize {
        let per = (SYNC_POINTS as u32 * self.p) as usize;
        (self.m_kib as usize / per) * per
    }
}

/// H' (§ 3.3): BLAKE2b stretched to `out.len()` bytes.
fn h_prime(input: &[&[u8]], out: &mut [u8]) {
    let len = (out.len() as u32).to_le_bytes();
    if out.len() <= 64 {
        let mut b = Blake2b::new(out.len());
        b.update(&len);
        for part in input {
            b.update(part);
        }
        b.finish_into(out);
        return;
    }
    let mut v = [0u8; 64];
    let mut b = Blake2b::new(64);
    b.update(&len);
    for part in input {
        b.update(part);
    }
    b.finish_into(&mut v);
    let r = out.len().div_ceil(32) - 2;
    out[..32].copy_from_slice(&v[..32]);
    for i in 1..r {
        let prev = v;
        blake2b(&prev, &mut v);
        out[32 * i..32 * i + 32].copy_from_slice(&v[..32]);
    }
    let rest = out.len() - 32 * r;
    let prev = v;
    blake2b(&prev, &mut out[32 * r..32 * r + rest]);
}

fn gb(v: &mut [u64; 128], a: usize, b: usize, c: usize, d: usize) {
    let mul = |x: u64, y: u64| 2u64.wrapping_mul((x as u32 as u64).wrapping_mul(y as u32 as u64));
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(mul(v[a], v[b]));
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]).wrapping_add(mul(v[c], v[d]));
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(mul(v[a], v[b]));
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]).wrapping_add(mul(v[c], v[d]));
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

/// The permutation P on sixteen of `v`'s words, named by their indices.
fn p(v: &mut [u64; 128], i: [usize; 16]) {
    gb(v, i[0], i[4], i[8], i[12]);
    gb(v, i[1], i[5], i[9], i[13]);
    gb(v, i[2], i[6], i[10], i[14]);
    gb(v, i[3], i[7], i[11], i[15]);
    gb(v, i[0], i[5], i[10], i[15]);
    gb(v, i[1], i[6], i[11], i[12]);
    gb(v, i[2], i[7], i[8], i[13]);
    gb(v, i[3], i[4], i[9], i[14]);
}

/// The compression G (§ 3.5): R = X ⊕ Y; P over its rows, then its columns;
/// the answer R ⊕ that (⊕ `next`'s old value when `xor`: passes after the first).
fn fill(x: &Block, y: &Block, next: &mut Block, xor: bool) {
    let mut r = [0u64; 128];
    for i in 0..128 {
        r[i] = x[i] ^ y[i];
    }
    let mut q = r;
    for row in 0..8 {
        p(&mut q, core::array::from_fn(|k| 16 * row + k));
    }
    for col in 0..8 {
        p(&mut q, core::array::from_fn(|k| 2 * col + (k / 2) * 16 + (k % 2)));
    }
    for i in 0..128 {
        let z = q[i] ^ r[i];
        next[i] = if xor { next[i] ^ z } else { z };
    }
}

fn block_bytes(b: &Block) -> [u8; 1024] {
    let mut out = [0u8; 1024];
    for (i, w) in b.iter().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&w.to_le_bytes());
    }
    out
}

fn block_from(bytes: &[u8; 1024]) -> Block {
    core::array::from_fn(|i| u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().unwrap_or([0; 8])))
}

/// Argon2id of `password` and `salt` (with an optional `secret` and
/// associated data `ad`), into `out` (4 bytes or more), using `memory`.
pub fn argon2id(
    params: Params,
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    ad: &[u8],
    memory: &mut [Block],
    out: &mut [u8],
) -> Result<(), Refused> {
    let lanes = params.p as usize;
    if lanes == 0 || params.t == 0 || out.len() < 4 || params.m_kib < 8 * params.p {
        return Err(Refused::BadParams);
    }
    let blocks = params.blocks();
    if memory.len() < blocks {
        return Err(Refused::TooLittleMemory);
    }
    let memory = &mut memory[..blocks];
    let lane_len = blocks / lanes;
    let seg = lane_len / SYNC_POINTS;

    // H0 (§ 3.2).
    let mut h0 = [0u8; 64];
    let mut b = Blake2b::new(64);
    for v in [params.p, out.len() as u32, params.m_kib, params.t, VERSION, ARGON2ID] {
        b.update(&v.to_le_bytes());
    }
    for part in [password, salt, secret, ad] {
        b.update(&(part.len() as u32).to_le_bytes());
        b.update(part);
    }
    b.finish_into(&mut h0);

    // Each lane's first two blocks.
    let mut buf = [0u8; 1024];
    for l in 0..lanes {
        for j in 0..2u32 {
            h_prime(&[&h0, &j.to_le_bytes(), &(l as u32).to_le_bytes()], &mut buf);
            memory[l * lane_len + j as usize] = block_from(&buf);
        }
    }

    let zero = [0u64; 128];
    for pass in 0..params.t as usize {
        for slice in 0..SYNC_POINTS {
            for lane in 0..lanes {
                // Argon2i's addressing for the first half of the first pass.
                let independent = pass == 0 && slice < SYNC_POINTS / 2;
                let mut input = [0u64; 128];
                let mut addresses = [0u64; 128];
                if independent {
                    input[..6].copy_from_slice(&[
                        pass as u64,
                        lane as u64,
                        slice as u64,
                        blocks as u64,
                        params.t as u64,
                        ARGON2ID as u64,
                    ]);
                }
                let next_addresses = |input: &mut Block, addresses: &mut Block| {
                    input[6] += 1;
                    let mut tmp = [0u64; 128];
                    fill(&zero, input, &mut tmp, false);
                    fill(&zero, &tmp, addresses, false);
                };
                let start = if pass == 0 && slice == 0 { 2 } else { 0 };
                if independent && start == 2 {
                    next_addresses(&mut input, &mut addresses);
                }
                for index in start..seg {
                    let cur = lane * lane_len + slice * seg + index;
                    let prev = if cur.is_multiple_of(lane_len) { cur + lane_len - 1 } else { cur - 1 };
                    let rand = if independent {
                        if index % 128 == 0 {
                            next_addresses(&mut input, &mut addresses);
                        }
                        addresses[index % 128]
                    } else {
                        memory[prev][0]
                    };
                    let (j1, j2) = (rand & 0xFFFF_FFFF, rand >> 32);
                    let ref_lane = if pass == 0 && slice == 0 { lane } else { (j2 % lanes as u64) as usize };
                    let same = ref_lane == lane;
                    // How many blocks may be referred to (§ 3.4.1.2).
                    let area = if pass == 0 {
                        if slice == 0 || same { slice * seg + index - 1 } else { slice * seg - usize::from(index == 0) }
                    } else if same {
                        lane_len - seg + index - 1
                    } else {
                        lane_len - seg - usize::from(index == 0)
                    } as u64;
                    let x = (j1 * j1) >> 32;
                    let y = (area * x) >> 32;
                    let z = area - 1 - y;
                    let begin = if pass == 0 || slice == SYNC_POINTS - 1 { 0 } else { (slice + 1) * seg } as u64;
                    let ref_index = ref_lane * lane_len + ((begin + z) % lane_len as u64) as usize;
                    let (a, r) = (memory[prev], memory[ref_index]);
                    fill(&a, &r, &mut memory[cur], pass > 0);
                }
            }
        }
    }

    // The last column, xor'd across the lanes, through H'.
    let mut c = memory[lane_len - 1];
    for l in 1..lanes {
        let last = &memory[l * lane_len + lane_len - 1];
        for i in 0..128 {
            c[i] ^= last[i];
        }
    }
    h_prime(&[&block_bytes(&c)], out);
    Ok(())
}
