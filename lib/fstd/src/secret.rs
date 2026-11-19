// SPDX-License-Identifier: Apache-2.0
//! Keeping secrets short-lived (FR-046): a key or password is overwritten
//! when its use is over, never left for the allocator or a later stack
//! frame to keep.

/// Overwrites `b` with zeros, in a way the compiler can't drop.
pub fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        // SAFETY: a valid byte of `b`.
        unsafe { core::ptr::write_volatile(x, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Overwrites a string's bytes (a password) with zeros.
pub fn wipe_str(s: &mut str) {
    // SAFETY: zeros are valid UTF-8.
    wipe(unsafe { s.as_bytes_mut() });
}

/// The stack under the caller's frame (64 KiB of the 256), zeroed: the
/// copies a key leaves in the frames of the calls that used it (a move, a
/// cipher's state) are gone once that work returns.
#[inline(never)]
pub fn scrub_stack() {
    let mut below = [0u8; 64 * 1024];
    wipe(&mut below);
    core::hint::black_box(&below);
}
