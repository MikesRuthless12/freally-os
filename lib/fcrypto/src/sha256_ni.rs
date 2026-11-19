// SPDX-License-Identifier: Apache-2.0
//! SHA-256's compression with the x86 SHA extensions (SHA-NI: `sha256rnds2`,
//! `sha256msg1`, `sha256msg2`), used when the CPU has them (TASK-076: the
//! bootloader hashes the whole image twice before anything runs; this is
//! about ten times the software path's speed). Only built with the
//! `shani` feature, which the kernel's soft-float build never turns on.
//!
//! The same FIPS 180-4 rounds as the software path, four at a time: the
//! state is held as (A, B, E, F) and (C, D, G, H), as `sha256rnds2` wants it,
//! and the message schedule's W[16..64] is made four words at a time,
//! W[t..t+4] = msg2(msg1(W[t-16..], W[t-12..]) + W[t-7..t-3], W[t-4..]).

use core::arch::x86_64::*;
use core::sync::atomic::{AtomicU8, Ordering};

/// 0 not asked yet, 1 no, 2 yes.
static HAVE: AtomicU8 = AtomicU8::new(0);

/// Whether this CPU has the SHA extensions (and the SSSE3 and SSE4.1 they
/// lean on), asked of CPUID once.
pub fn available() -> bool {
    match HAVE.load(Ordering::Relaxed) {
        0 => {
            let leaf1 = __cpuid(1);
            let leaf7 = if __cpuid(0).eax >= 7 { __cpuid_count(7, 0).ebx } else { 0 };
            let yes = leaf7 & (1 << 29) != 0 && leaf1.ecx & (1 << 9) != 0 && leaf1.ecx & (1 << 19) != 0;
            HAVE.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
            yes
        }
        v => v == 2,
    }
}

/// Compresses `blocks` into `h`.
///
/// # Safety
/// The CPU must have the SHA extensions, SSSE3 and SSE4.1 ([`available`]).
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
pub unsafe fn compress(h: &mut [u32; 8], blocks: &[[u8; 64]], k: &[u32; 64]) {
    // Big-endian words: each 32-bit lane's bytes reversed.
    let swap = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0b, 0x0405_0607_0001_0203);
    // SAFETY: `h` is 8 u32s: two unaligned 16-byte loads.
    let (dcba, hgfe) = unsafe {
        (_mm_loadu_si128(h.as_ptr() as *const __m128i), _mm_loadu_si128(h.as_ptr().add(4) as *const __m128i))
    };
    // (A, B, E, F) and (C, D, G, H), highest lane first, as sha256rnds2 takes them.
    let cdab = _mm_shuffle_epi32(dcba, 0xB1);
    let efgh = _mm_shuffle_epi32(hgfe, 0x1B);
    let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
    let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xF0);
    for block in blocks {
        let (abef_was, cdgh_was) = (abef, cdgh);
        // SAFETY: a block is 64 bytes: four unaligned 16-byte loads.
        let mut w: [__m128i; 4] = core::array::from_fn(|i| {
            _mm_shuffle_epi8(unsafe { _mm_loadu_si128(block.as_ptr().add(16 * i) as *const __m128i) }, swap)
        });
        for i in 0..16 {
            // SAFETY: `k` is 64 u32s; 4 × i + 4 ≤ 64.
            let ki = unsafe { _mm_loadu_si128(k.as_ptr().add(4 * i) as *const __m128i) };
            let wk = _mm_add_epi32(w[i % 4], ki);
            cdgh = _mm_sha256rnds2_epu32(cdgh, abef, wk);
            abef = _mm_sha256rnds2_epu32(abef, cdgh, _mm_shuffle_epi32(wk, 0x0E));
            // The schedule's next four words, in the slot just used.
            if i < 12 {
                let (x0, x1, x2, x3) = (w[i % 4], w[(i + 1) % 4], w[(i + 2) % 4], w[(i + 3) % 4]);
                let t = _mm_add_epi32(_mm_sha256msg1_epu32(x0, x1), _mm_alignr_epi8(x3, x2, 4));
                w[i % 4] = _mm_sha256msg2_epu32(t, x3);
            }
        }
        abef = _mm_add_epi32(abef, abef_was);
        cdgh = _mm_add_epi32(cdgh, cdgh_was);
    }
    // Back to (A, B, C, D) and (E, F, G, H).
    let feba = _mm_shuffle_epi32(abef, 0x1B);
    let dchg = _mm_shuffle_epi32(cdgh, 0xB1);
    let dcba = _mm_blend_epi16(feba, dchg, 0xF0);
    let hgfe = _mm_alignr_epi8(dchg, feba, 8);
    // SAFETY: as the loads above.
    unsafe {
        _mm_storeu_si128(h.as_mut_ptr() as *mut __m128i, dcba);
        _mm_storeu_si128(h.as_mut_ptr().add(4) as *mut __m128i, hgfe);
    }
}
