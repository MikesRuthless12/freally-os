// SPDX-License-Identifier: Apache-2.0
//! The fuzz target (TASK-179): the test chains' certificates with random
//! bytes changed, cut short or added; the reader must never panic, and a
//! leaf changed anywhere in its signed part must never verify.
//!
//! No outside fuzzing crate (charter rule 1): a seeded xorshift drives it, so a
//! failure names the iteration that reproduces it. `cargo xtask ci -- --slow`
//! runs it with `FREALLY_FUZZ_ITERS=2000`; the default run does 40.

use fx509::cert::Certificate;
use fx509::verify::verify;

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

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn changed_certificates_never_panic_nor_verify() {
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let good = include_str!("chains.txt").lines().next().unwrap();
    let f: Vec<&str> = good.split(' ').collect();
    let chain: Vec<Vec<u8>> = f[4].split(',').map(unhex).collect();
    let roots: Vec<Vec<u8>> = f[5].split(',').map(unhex).collect();
    let now: u64 = f[3].parse().unwrap();
    let root_refs: Vec<&[u8]> = roots.iter().map(Vec::as_slice).collect();
    let mut rng = XorShift(0xD1B5_4A32_D192_ED03);
    for i in 0..iters {
        let mut leaf = chain[0].clone();
        match rng.next() % 4 {
            0 => {
                // One to four bytes changed.
                for _ in 0..1 + rng.next() % 4 {
                    let at = (rng.next() as usize) % leaf.len();
                    leaf[at] ^= 1 + (rng.next() % 255) as u8;
                }
            }
            1 => leaf.truncate((rng.next() as usize) % leaf.len()),
            2 => (0..1 + rng.next() % 8).for_each(|_| leaf.push(rng.next() as u8)),
            _ => {
                let at = (rng.next() as usize) % leaf.len();
                leaf.insert(at, rng.next() as u8);
            }
        }
        let _ = Certificate::parse(&leaf);
        let refs: Vec<&[u8]> = [leaf.as_slice(), chain[1].as_slice()].to_vec();
        assert!(
            verify(&refs, &root_refs, f[2], now).is_err() || leaf == chain[0],
            "iteration {i}: a changed leaf verified"
        );
    }
}
