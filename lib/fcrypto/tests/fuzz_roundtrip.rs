// SPDX-License-Identifier: Apache-2.0
//! The fuzz target: Ed25519 sign/verify round trips over random keys and messages,
//! with every single-bit change to the signature or the message refused.
//!
//! No outside fuzzing crate (charter rule 1): a seeded xorshift drives it, so a
//! failure names the iteration that reproduces it. `cargo xtask ci -- --slow` runs
//! it with `FREALLY_FUZZ_ITERS=2000`; the default run does 40.

use fcrypto::ed25519;

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

    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = self.next() as u8;
        }
    }
}

#[test]
fn ed25519_sign_verify_round_trips() {
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for i in 0..iters {
        let mut seed = [0u8; 32];
        rng.fill(&mut seed);
        let mut msg = vec![0u8; (rng.next() % 300) as usize];
        rng.fill(&mut msg);
        let public = ed25519::public_key(&seed);
        let sig = ed25519::sign(&seed, &msg);
        assert!(ed25519::verify(&public, &msg, &sig), "iteration {i}: a fresh signature was refused");

        let bit = (rng.next() % 512) as usize;
        let mut bad = sig;
        bad[bit / 8] ^= 1 << (bit % 8);
        assert!(!ed25519::verify(&public, &msg, &bad), "iteration {i}: signature bit {bit} flipped, still accepted");

        if !msg.is_empty() {
            let bit = (rng.next() % (msg.len() as u64 * 8)) as usize;
            let mut other = msg.clone();
            other[bit / 8] ^= 1 << (bit % 8);
            assert!(
                !ed25519::verify(&public, &other, &sig),
                "iteration {i}: message bit {bit} flipped, still accepted"
            );
        }

        let mut other_seed = seed;
        other_seed[0] ^= 1;
        let other_public = ed25519::public_key(&other_seed);
        assert!(!ed25519::verify(&other_public, &msg, &sig), "iteration {i}: another key accepted the signature");
    }
}
