// SPDX-License-Identifier: Apache-2.0
//! AES and GHASH with the x86 instructions made for them, used when the CPU
//! has them (TASK-178): AES-NI's `aesenc` and `aesenclast`, PCLMULQDQ's
//! carry-less multiply. Only built with the `aesni` feature (user space and
//! the host tools): the kernel's and the bootloader's soft-float builds
//! refuse a function that turns SSE on, and never do AES-GCM.
//!
//! The round keys are [`crate::aes::Aes`]'s key schedule, byte for byte what
//! `aesenc` takes. GCTR runs four blocks at once (an `aesenc` takes four
//! cycles to finish, and starts every cycle). GHASH's product is Gueron and
//! Kounavis's (*Intel Carry-Less Multiplication Instruction and its Usage
//! for Computing the GCM Mode*, rev. 2.02, its Figure 5): the 256-bit
//! carry-less product of the two halves, shifted left a bit (GCM's bits are
//! reflected), reduced modulo x^128 + x^7 + x^2 + x + 1. Its operands are
//! [`crate::gcm`]'s: a block read big-endian, which is the byte-reflected
//! form that figure starts from. Neither instruction's time depends on its
//! data, so both stay constant time, as the software path is.

use core::arch::x86_64::{
    __cpuid, __m128i, _mm_aesenc_si128, _mm_aesenclast_si128, _mm_clmulepi64_si128, _mm_loadu_si128, _mm_or_si128,
    _mm_slli_epi32, _mm_slli_si128, _mm_srli_epi32, _mm_srli_si128, _mm_storeu_si128, _mm_xor_si128,
};
use core::sync::atomic::{AtomicU8, Ordering};

/// 0 not asked yet, 1 no, 2 yes.
static HAVE: AtomicU8 = AtomicU8::new(0);

/// Whether this CPU has AES-NI and PCLMULQDQ, asked of CPUID once.
pub fn available() -> bool {
    match HAVE.load(Ordering::Relaxed) {
        0 => {
            let ecx = __cpuid(1).ecx;
            let yes = ecx & (1 << 25) != 0 && ecx & (1 << 1) != 0;
            HAVE.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
            yes
        }
        v => v == 2,
    }
}

fn load(b: &[u8; 16]) -> __m128i {
    // SAFETY: 16 readable bytes; an unaligned load takes any address.
    unsafe { _mm_loadu_si128(b.as_ptr().cast()) }
}

fn store(x: __m128i) -> [u8; 16] {
    let mut b = [0u8; 16];
    // SAFETY: 16 writable bytes; an unaligned store takes any address.
    unsafe { _mm_storeu_si128(b.as_mut_ptr().cast(), x) };
    b
}

/// GCTR (SP 800-38D § 6.5): `data` XORed in place with E(K, nonce ‖ n) for
/// n = `first`, `first` + 1, … (each counter is the block's last 32 bits,
/// big-endian, wrapping as GCM's inc32 does). `keys` are the round keys,
/// `rounds` + 1 of them.
#[target_feature(enable = "aes")]
pub fn ctr(keys: &[[u8; 16]; 15], rounds: usize, nonce: &[u8; 12], first: u32, data: &mut [u8]) {
    let k: [__m128i; 15] = core::array::from_fn(|i| load(&keys[i]));
    let mut counter = [0u8; 16];
    counter[..12].copy_from_slice(nonce);
    let mut n = first;
    for chunk in data.chunks_mut(64) {
        let mut x = [0u32, 1, 2, 3].map(|i| {
            counter[12..].copy_from_slice(&n.wrapping_add(i).to_be_bytes());
            _mm_xor_si128(load(&counter), k[0])
        });
        n = n.wrapping_add(4);
        for key in &k[1..rounds] {
            x = x.map(|b| _mm_aesenc_si128(b, *key));
        }
        x = x.map(|b| _mm_aesenclast_si128(b, k[rounds]));
        for (part, pad) in chunk.chunks_mut(16).zip(x) {
            for (d, p) in part.iter_mut().zip(store(pad)) {
                *d ^= p;
            }
        }
    }
}

/// GHASH (SP 800-38D § 6.4) over `data`, each 16 bytes (the last zero-padded)
/// taken in as Y = (Y ⊕ X) · H, from `y`; blocks as [`crate::gcm`] holds
/// them (read big-endian).
#[target_feature(enable = "pclmulqdq")]
pub fn ghash(y: u128, h: u128, data: &[u8]) -> u128 {
    let h = load(&h.to_le_bytes());
    let mut y = load(&y.to_le_bytes());
    for chunk in data.chunks(16) {
        let mut b = [0u8; 16];
        b[..chunk.len()].copy_from_slice(chunk);
        y = gf_mul(_mm_xor_si128(y, load(&u128::from_be_bytes(b).to_le_bytes())), h);
    }
    u128::from_le_bytes(store(y))
}

/// a · b in GCM's GF(2^128), both byte-reflected (Gueron and Kounavis, Figure 5).
#[target_feature(enable = "pclmulqdq")]
pub fn gf_mul(a: __m128i, b: __m128i) -> __m128i {
    // The 256-bit carry-less product: lo ‖ hi.
    let lo = _mm_clmulepi64_si128::<0x00>(a, b);
    let hi = _mm_clmulepi64_si128::<0x11>(a, b);
    let mid = _mm_xor_si128(_mm_clmulepi64_si128::<0x10>(a, b), _mm_clmulepi64_si128::<0x01>(a, b));
    let lo = _mm_xor_si128(lo, _mm_slli_si128::<8>(mid));
    let hi = _mm_xor_si128(hi, _mm_srli_si128::<8>(mid));
    // Shifted left one bit across all 256: the operands were reflected.
    let (lo_out, hi_out) = (_mm_srli_epi32::<31>(lo), _mm_srli_epi32::<31>(hi));
    let lo = _mm_or_si128(_mm_slli_epi32::<1>(lo), _mm_slli_si128::<4>(lo_out));
    let hi =
        _mm_or_si128(_mm_or_si128(_mm_slli_epi32::<1>(hi), _mm_slli_si128::<4>(hi_out)), _mm_srli_si128::<12>(lo_out));
    // Reduced modulo x^128 + x^7 + x^2 + x + 1: the first phase…
    let t = _mm_xor_si128(_mm_xor_si128(_mm_slli_epi32::<31>(lo), _mm_slli_epi32::<30>(lo)), _mm_slli_epi32::<25>(lo));
    let carry = _mm_srli_si128::<4>(t);
    let lo = _mm_xor_si128(lo, _mm_slli_si128::<12>(t));
    // …and the second.
    let t = _mm_xor_si128(_mm_xor_si128(_mm_srli_epi32::<1>(lo), _mm_srli_epi32::<2>(lo)), _mm_srli_epi32::<7>(lo));
    _mm_xor_si128(hi, _mm_xor_si128(lo, _mm_xor_si128(t, carry)))
}
