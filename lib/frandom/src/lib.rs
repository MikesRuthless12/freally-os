// SPDX-License-Identifier: Apache-2.0
//! frandom (TASK-097, PRD § 10): random bytes for account salts and keys.
//!
//! - **The source:** the CPU's own generator, RDSEED (straight from its
//!   entropy source) or, without it, RDRAND, each draw retried a few times
//!   as Intel's guidance says.
//! - **The self-test,** before the first bytes and never skipped: eight
//!   draws, all successful, no two alike, none all zeros or all ones (some
//!   CPUs' RDRAND has answered 0xFFFF…FFFF after a firmware fault). A source
//!   that fails it is refused: no salt or key is made from it (PRD § 10:
//!   "refuse if RDRAND fails its self-test").
//! - **The mix:** four hardware words and sixteen TSC jitter samples (how
//!   long a short loop took, which wobbles with the machine) through
//!   HKDF-SHA-256, with a counter so no two calls answer alike.

#![no_std]

use fcrypto::hkdf;

/// Where randomness comes from: the hardware generator and the clock.
pub trait Entropy {
    /// One 64-bit draw, or `None` when the generator gave nothing.
    fn draw(&mut self) -> Option<u64>;
    /// The time-stamp counter.
    fn tsc(&mut self) -> u64;
}

/// Why no bytes were made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The CPU has neither RDSEED nor RDRAND.
    NoGenerator,
    /// The generator failed its self-test.
    SelfTest,
}

/// The CPU's generator: RDSEED when it has it, else RDRAND.
#[derive(Clone, Copy, Debug)]
pub struct Cpu {
    seed: bool,
}

impl Cpu {
    /// The CPU's generator, if it has one.
    pub fn open() -> Result<Cpu, Refused> {
        use core::arch::x86_64::{__cpuid, __cpuid_count};
        let rdrand = __cpuid(1).ecx & (1 << 30) != 0;
        let rdseed = __cpuid(0).eax >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0;
        if rdseed || rdrand { Ok(Cpu { seed: rdseed }) } else { Err(Refused::NoGenerator) }
    }
}

#[target_feature(enable = "rdseed")]
unsafe fn rdseed() -> Option<u64> {
    let mut v = 0u64;
    (core::arch::x86_64::_rdseed64_step(&mut v) == 1).then_some(v)
}

#[target_feature(enable = "rdrand")]
unsafe fn rdrand() -> Option<u64> {
    let mut v = 0u64;
    (core::arch::x86_64::_rdrand64_step(&mut v) == 1).then_some(v)
}

impl Entropy for Cpu {
    fn draw(&mut self) -> Option<u64> {
        // RDSEED may run dry for a moment: up to 100 tries; RDRAND 10.
        for _ in 0..if self.seed { 100 } else { 10 } {
            // SAFETY: `open` found the instruction in CPUID.
            let v = unsafe { if self.seed { rdseed() } else { rdrand() } };
            if v.is_some() {
                return v;
            }
            core::hint::spin_loop();
        }
        None
    }

    fn tsc(&mut self) -> u64 {
        // SAFETY: RDTSC has no side effects and user mode may run it.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
}

/// The self-test: eight draws, all given, no two alike, none all zeros or
/// all ones.
pub fn self_test(e: &mut impl Entropy) -> Result<(), Refused> {
    let mut seen = [0u64; 8];
    for i in 0..8 {
        let v = e.draw().ok_or(Refused::SelfTest)?;
        if v == 0 || v == u64::MAX || seen[..i].contains(&v) {
            return Err(Refused::SelfTest);
        }
        seen[i] = v;
    }
    Ok(())
}

/// Random bytes from a source, after its self-test passed.
pub struct Random<E: Entropy> {
    e: E,
    calls: u64,
}

impl<E: Entropy> Random<E> {
    /// The source, if it passes its self-test.
    pub fn new(mut e: E) -> Result<Random<E>, Refused> {
        self_test(&mut e)?;
        Ok(Random { e, calls: 0 })
    }

    /// Fills `out`: four hardware words and sixteen jitter samples through
    /// HKDF, every 8 KiB with a fresh mix.
    pub fn fill(&mut self, out: &mut [u8]) -> Result<(), Refused> {
        for chunk in out.chunks_mut(255 * 32) {
            let mut ikm = [0u8; 32 + 16 * 8];
            for i in 0..4 {
                let v = self.e.draw().ok_or(Refused::SelfTest)?;
                ikm[8 * i..8 * i + 8].copy_from_slice(&v.to_le_bytes());
            }
            for i in 0..16 {
                let t0 = self.e.tsc();
                let mut x = t0;
                for k in 0..64u64 {
                    x = core::hint::black_box(x.rotate_left(7) ^ k);
                }
                let dt = self.e.tsc().wrapping_sub(t0) ^ x;
                ikm[32 + 8 * i..40 + 8 * i].copy_from_slice(&dt.to_le_bytes());
            }
            self.calls += 1;
            let prk = hkdf::extract(b"freally/frandom/v1", &ikm);
            hkdf::expand(&prk, &self.calls.to_le_bytes(), chunk).map_err(|_| Refused::SelfTest)?;
            ikm.fill(0);
        }
        Ok(())
    }
}

/// Fills `out` from this CPU's generator (self-tested first).
pub fn fill(out: &mut [u8]) -> Result<(), Refused> {
    Random::new(Cpu::open()?)?.fill(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A planted source: these draws, then the same one for ever.
    struct Planted {
        draws: [Option<u64>; 8],
        at: usize,
        t: u64,
    }

    impl Entropy for Planted {
        fn draw(&mut self) -> Option<u64> {
            let v = self.draws[self.at.min(7)];
            self.at += 1;
            v
        }
        fn tsc(&mut self) -> u64 {
            self.t += 1000;
            self.t
        }
    }

    fn planted(draws: [Option<u64>; 8]) -> Planted {
        Planted { draws, at: 0, t: 0 }
    }

    #[test]
    fn a_bad_generator_is_refused() {
        // RDRAND's known failure: all ones, every time.
        assert_eq!(self_test(&mut planted([Some(u64::MAX); 8])), Err(Refused::SelfTest));
        // A draw that never comes.
        let mut some: [Option<u64>; 8] = core::array::from_fn(|i| Some(i as u64 + 1));
        some[5] = None;
        assert_eq!(self_test(&mut planted(some)), Err(Refused::SelfTest));
        // Two alike.
        let mut alike: [Option<u64>; 8] = core::array::from_fn(|i| Some(i as u64 + 1));
        alike[6] = Some(3);
        assert_eq!(self_test(&mut planted(alike)), Err(Refused::SelfTest));
        // Zero.
        let zero: [Option<u64>; 8] = core::array::from_fn(|i| Some(i as u64));
        assert!(Random::new(planted(zero)).is_err());
    }

    #[test]
    fn this_cpu_gives_bytes_that_differ_each_time() {
        let Ok(cpu) = Cpu::open() else {
            // A host with no generator: nothing to test here.
            return;
        };
        let mut r = Random::new(cpu).unwrap();
        let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
        r.fill(&mut a).unwrap();
        r.fill(&mut b).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 64]);
        // More than one HKDF's worth.
        let mut big = [0u8; 9000];
        fill(&mut big).unwrap();
        assert!(big[8160..].iter().any(|&x| x != 0));
    }
}
