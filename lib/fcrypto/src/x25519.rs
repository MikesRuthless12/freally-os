// SPDX-License-Identifier: Apache-2.0
//! X25519 (RFC 7748 § 5): Diffie–Hellman on Curve25519 by the Montgomery
//! ladder, on Ed25519's field (GF(2^255 − 19), radix 2^51, constant time).
//! Every step is the same whatever the scalar: the swaps are masked selects,
//! never branches, and the inversion's exponent is public.

use crate::ed25519::Fe;

/// The base point's u-coordinate, 9.
pub const BASE: [u8; 32] = {
    let mut b = [0u8; 32];
    b[0] = 9;
    b
};

/// The scalar as RFC 7748 decodes it: low three bits cleared, bit 255
/// cleared, bit 254 set.
fn clamp(k: &[u8; 32]) -> [u8; 32] {
    let mut k = *k;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    k
}

/// `k` × `u`: the shared secret (or, with `BASE`, the public key). An
/// all-zero answer means `u` was a low-order point; callers that must
/// refuse one check for it.
pub fn x25519(k: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let k = clamp(k);
    // from_bytes ignores the top bit, as § 5 says to.
    let x1 = Fe::from_bytes(u);
    let (mut x2, mut z2, mut x3, mut z3) = (Fe::small(1), Fe::small(0), x1, Fe::small(1));
    let a24 = Fe::small(121_665);
    let mut swap = 0u64;
    for t in (0..255).rev() {
        let bit = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= bit;
        (x2, x3) = (Fe::select(swap, &x2, &x3), Fe::select(swap, &x3, &x2));
        (z2, z3) = (Fe::select(swap, &z2, &z3), Fe::select(swap, &z3, &z2));
        swap = bit;
        let a = x2.add(&z2);
        let aa = a.square();
        let b = x2.sub(&z2);
        let bb = b.square();
        let e = aa.sub(&bb);
        let c = x3.add(&z3);
        let d = x3.sub(&z3);
        let da = d.mul(&a);
        let cb = c.mul(&b);
        x3 = da.add(&cb).square();
        z3 = x1.mul(&da.sub(&cb).square());
        x2 = aa.mul(&bb);
        z2 = e.mul(&aa.add(&a24.mul(&e)));
    }
    // The last swap: only x2 and z2 are wanted.
    let x2 = Fe::select(swap, &x2, &x3);
    let z2 = Fe::select(swap, &z2, &z3);
    x2.mul(&z2.invert()).to_bytes()
}

/// The public key for a secret scalar.
pub fn public_key(secret: &[u8; 32]) -> [u8; 32] {
    x25519(secret, &BASE)
}
