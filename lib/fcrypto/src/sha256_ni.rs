// SPDX-License-Identifier: Apache-2.0
//! SHA-256's compression with the x86 SHA extensions (SHA-NI: `sha256rnds2`,
//! `sha256msg1`, `sha256msg2`), used when the CPU has them (TASK-076: the
//! bootloader hashes the whole image twice before anything runs; this is
//! about ten times the software path's speed). Only built with the
//! `shani` feature (the bootloader and the host tools).
//!
//! **Assembly, not intrinsics:** the bootloader's UEFI target is soft-float,
//! and refuses any Rust function that turns SSE on; the same rounds as one
//! assembly function (generated, its sixteen groups uniform) assemble on any
//! x86-64 target. It keeps the XMM registers the firmware's ABI expects kept
//! (xmm6 to xmm10, the ones it uses), so a bootloader that hands control back
//! to the firmware leaves them as they were.
//!
//! The same FIPS 180-4 rounds as the software path, four at a time: the
//! state is held as (A, B, E, F) and (C, D, G, H), as `sha256rnds2` wants it,
//! and the message schedule's W[16..64] is made four words at a time,
//! W[t..t+4] = msg2(msg1(W[t-16..], W[t-12..]) + W[t-7..t-3], W[t-4..]).

use core::arch::x86_64::{__cpuid, __cpuid_count};
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

/// The byte swap that makes each 32-bit lane big-endian (`pshufb`'s mask).
static SWAP: [u8; 16] = [3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12];

unsafe extern "sysv64" {
    /// `blocks` 64-byte blocks at `data` compressed into `state` (System V:
    /// all XMM registers the caller's to lose; xmm6-xmm10 kept regardless).
    fn fcrypto_sha256_ni_blocks(state: *mut u32, data: *const u8, blocks: usize, k: *const u32, swap: *const u8);
}

/// Compresses `blocks` into `h`.
///
/// # Safety
/// The CPU must have the SHA extensions, SSSE3 and SSE4.1 ([`available`]).
pub unsafe fn compress(h: &mut [u32; 8], blocks: &[[u8; 64]], k: &[u32; 64]) {
    // SAFETY: `h` is 8 u32s, `blocks` holds `len` whole blocks, `k` 64 u32s
    // and `SWAP` 16 bytes; the caller checked the CPU.
    unsafe { fcrypto_sha256_ni_blocks(h.as_mut_ptr(), blocks.as_ptr().cast(), blocks.len(), k.as_ptr(), SWAP.as_ptr()) }
}

// The rounds: FIPS 180-4's, four at a time. xmm1 holds (A, B, E, F) and
// xmm2 (C, D, G, H), as sha256rnds2 takes them; xmm8 the byte swap; xmm9 and
// xmm10 the state the block started from. The message words live in
// M(i) = xmm(3 + i % 4). Group i (rounds 4i..4i+3) is always:
//   xmm0 = M(i) (groups 0-3 load and byte-swap it from the block first)
//   xmm0 += K[4i..4i+4]; sha256rnds2 xmm2, xmm1
//   groups 3-14: M(i+1) = msg2(M(i+1) + alignr(M(i), M(i-1), 4), M(i))
//   xmm0 = its high half; sha256rnds2 xmm1, xmm2
//   groups 1-12: M(i-1) = msg1(M(i-1), M(i))
core::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".global fcrypto_sha256_ni_blocks",
    "fcrypto_sha256_ni_blocks:",
    "    test rdx, rdx",
    "    jz 4f",
    "    sub rsp, 80",
    "    movdqu [rsp + 0], xmm6",
    "    movdqu [rsp + 16], xmm7",
    "    movdqu [rsp + 32], xmm8",
    "    movdqu [rsp + 48], xmm9",
    "    movdqu [rsp + 64], xmm10",
    "    movdqu xmm1, [rdi]",
    "    movdqu xmm2, [rdi + 16]",
    "    movdqa xmm7, xmm1",
    "    punpcklqdq xmm1, xmm2",
    "    punpckhqdq xmm2, xmm7",
    "    pshufd xmm1, xmm1, 0x1B",
    "    pshufd xmm2, xmm2, 0xB1",
    "    movdqu xmm8, [r8]",
    "2:",
    "    movdqa xmm9, xmm1",
    "    movdqa xmm10, xmm2",
    "    movdqu xmm0, [rsi + 0]",
    "    pshufb xmm0, xmm8",
    "    movdqa xmm3, xmm0",
    "    movdqu xmm7, [rcx + 0]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    movdqu xmm0, [rsi + 16]",
    "    pshufb xmm0, xmm8",
    "    movdqa xmm4, xmm0",
    "    movdqu xmm7, [rcx + 16]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm3, xmm4",
    "    movdqu xmm0, [rsi + 32]",
    "    pshufb xmm0, xmm8",
    "    movdqa xmm5, xmm0",
    "    movdqu xmm7, [rcx + 32]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm4, xmm5",
    "    movdqu xmm0, [rsi + 48]",
    "    pshufb xmm0, xmm8",
    "    movdqa xmm6, xmm0",
    "    movdqu xmm7, [rcx + 48]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm6",
    "    palignr xmm7, xmm5, 4",
    "    paddd xmm3, xmm7",
    "    sha256msg2 xmm3, xmm6",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm5, xmm6",
    "    movdqa xmm0, xmm3",
    "    movdqu xmm7, [rcx + 64]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm3",
    "    palignr xmm7, xmm6, 4",
    "    paddd xmm4, xmm7",
    "    sha256msg2 xmm4, xmm3",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm6, xmm3",
    "    movdqa xmm0, xmm4",
    "    movdqu xmm7, [rcx + 80]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm4",
    "    palignr xmm7, xmm3, 4",
    "    paddd xmm5, xmm7",
    "    sha256msg2 xmm5, xmm4",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm3, xmm4",
    "    movdqa xmm0, xmm5",
    "    movdqu xmm7, [rcx + 96]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm5",
    "    palignr xmm7, xmm4, 4",
    "    paddd xmm6, xmm7",
    "    sha256msg2 xmm6, xmm5",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm4, xmm5",
    "    movdqa xmm0, xmm6",
    "    movdqu xmm7, [rcx + 112]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm6",
    "    palignr xmm7, xmm5, 4",
    "    paddd xmm3, xmm7",
    "    sha256msg2 xmm3, xmm6",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm5, xmm6",
    "    movdqa xmm0, xmm3",
    "    movdqu xmm7, [rcx + 128]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm3",
    "    palignr xmm7, xmm6, 4",
    "    paddd xmm4, xmm7",
    "    sha256msg2 xmm4, xmm3",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm6, xmm3",
    "    movdqa xmm0, xmm4",
    "    movdqu xmm7, [rcx + 144]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm4",
    "    palignr xmm7, xmm3, 4",
    "    paddd xmm5, xmm7",
    "    sha256msg2 xmm5, xmm4",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm3, xmm4",
    "    movdqa xmm0, xmm5",
    "    movdqu xmm7, [rcx + 160]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm5",
    "    palignr xmm7, xmm4, 4",
    "    paddd xmm6, xmm7",
    "    sha256msg2 xmm6, xmm5",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm4, xmm5",
    "    movdqa xmm0, xmm6",
    "    movdqu xmm7, [rcx + 176]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm6",
    "    palignr xmm7, xmm5, 4",
    "    paddd xmm3, xmm7",
    "    sha256msg2 xmm3, xmm6",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm5, xmm6",
    "    movdqa xmm0, xmm3",
    "    movdqu xmm7, [rcx + 192]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm3",
    "    palignr xmm7, xmm6, 4",
    "    paddd xmm4, xmm7",
    "    sha256msg2 xmm4, xmm3",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    sha256msg1 xmm6, xmm3",
    "    movdqa xmm0, xmm4",
    "    movdqu xmm7, [rcx + 208]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm4",
    "    palignr xmm7, xmm3, 4",
    "    paddd xmm5, xmm7",
    "    sha256msg2 xmm5, xmm4",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    movdqa xmm0, xmm5",
    "    movdqu xmm7, [rcx + 224]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    movdqa xmm7, xmm5",
    "    palignr xmm7, xmm4, 4",
    "    paddd xmm6, xmm7",
    "    sha256msg2 xmm6, xmm5",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    movdqa xmm0, xmm6",
    "    movdqu xmm7, [rcx + 240]",
    "    paddd xmm0, xmm7",
    "    sha256rnds2 xmm2, xmm1",
    "    pshufd xmm0, xmm0, 0x0E",
    "    sha256rnds2 xmm1, xmm2",
    "    paddd xmm1, xmm9",
    "    paddd xmm2, xmm10",
    "    add rsi, 64",
    "    dec rdx",
    "    jnz 2b",
    "    pshufd xmm1, xmm1, 0x1B",
    "    pshufd xmm2, xmm2, 0xB1",
    "    movdqa xmm7, xmm1",
    "    pblendw xmm1, xmm2, 0xF0",
    "    palignr xmm2, xmm7, 8",
    "    movdqu [rdi], xmm1",
    "    movdqu [rdi + 16], xmm2",
    "    movdqu xmm6, [rsp + 0]",
    "    movdqu xmm7, [rsp + 16]",
    "    movdqu xmm8, [rsp + 32]",
    "    movdqu xmm9, [rsp + 48]",
    "    movdqu xmm10, [rsp + 64]",
    "    add rsp, 80",
    "4:",
    "    ret",
);
