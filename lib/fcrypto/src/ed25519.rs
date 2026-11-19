// SPDX-License-Identifier: Apache-2.0
//! Ed25519 (RFC 8032 § 5.1): signing and verification.
//!
//! - The field GF(2^255 − 19) is held in five 51-bit limbs (radix 2^51), with
//!   128-bit products. No table is indexed by secret data.
//! - Scalar multiplication is a double-and-add-always ladder that picks with
//!   masks, so its time and memory accesses don't depend on the scalar.
//! - Scalars are reduced mod L by constant-time shift-and-subtract.
//! - Verification is cofactorless ([S]B = R + [k]A, compared by encoding), and
//!   rejects a non-canonical S (S ≥ L) and a public key that doesn't decode.

use crate::ct;
use crate::sha2::Sha512;

const MASK: u64 = (1 << 51) - 1;

/// A 128-bit product of two limbs.
const fn m(x: u64, y: u64) -> u128 {
    (x as u128) * (y as u128)
}

/// Eight little-endian bytes of `s` from `i`.
const fn load64(s: &[u8; 32], i: usize) -> u64 {
    let mut v = 0u64;
    let mut k = 0;
    while k < 8 {
        v |= (s[i + k] as u64) << (8 * k);
        k += 1;
    }
    v
}

/// An element of GF(2^255 − 19): Σ limb[i] · 2^(51·i).
#[derive(Clone, Copy)]
pub(crate) struct Fe([u64; 5]);

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    pub(crate) const fn small(v: u64) -> Fe {
        Fe([v, 0, 0, 0, 0])
    }

    /// Carries every limb down to 51 bits, folding the top carry back as ×19.
    const fn weak_reduce(mut l: [u64; 5]) -> Fe {
        let c = l[0] >> 51;
        l[0] &= MASK;
        l[1] += c;
        let c = l[1] >> 51;
        l[1] &= MASK;
        l[2] += c;
        let c = l[2] >> 51;
        l[2] &= MASK;
        l[3] += c;
        let c = l[3] >> 51;
        l[3] &= MASK;
        l[4] += c;
        let c = l[4] >> 51;
        l[4] &= MASK;
        l[0] += c * 19;
        Fe(l)
    }

    pub(crate) const fn add(&self, b: &Fe) -> Fe {
        let a = &self.0;
        let b = &b.0;
        Fe::weak_reduce([a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3], a[4] + b[4]])
    }

    /// `self − b`, computed as `self + 4p − b` so no limb goes negative.
    pub(crate) const fn sub(&self, b: &Fe) -> Fe {
        let a = &self.0;
        let b = &b.0;
        // 4p in radix 2^51: 4·(2^51 − 19), then 4·(2^51 − 1) four times.
        Fe::weak_reduce([
            (a[0] + 0x1F_FFFF_FFFF_FFB4) - b[0],
            (a[1] + 0x1F_FFFF_FFFF_FFFC) - b[1],
            (a[2] + 0x1F_FFFF_FFFF_FFFC) - b[2],
            (a[3] + 0x1F_FFFF_FFFF_FFFC) - b[3],
            (a[4] + 0x1F_FFFF_FFFF_FFFC) - b[4],
        ])
    }

    const fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    pub(crate) const fn mul(&self, b: &Fe) -> Fe {
        let a = &self.0;
        let b = &b.0;
        let b1 = b[1] * 19;
        let b2 = b[2] * 19;
        let b3 = b[3] * 19;
        let b4 = b[4] * 19;
        let r0 = m(a[0], b[0]) + m(a[1], b4) + m(a[2], b3) + m(a[3], b2) + m(a[4], b1);
        let r1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4) + m(a[3], b3) + m(a[4], b2);
        let r2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4) + m(a[4], b3);
        let r3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4);
        let r4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);
        let m51 = MASK as u128;
        let r1 = r1 + (r0 >> 51);
        let r2 = r2 + (r1 >> 51);
        let r3 = r3 + (r2 >> 51);
        let r4 = r4 + (r3 >> 51);
        let l0 = (r0 & m51) + (r4 >> 51) * 19;
        let l1 = (r1 & m51) + (l0 >> 51);
        Fe([(l0 & m51) as u64, l1 as u64, (r2 & m51) as u64, (r3 & m51) as u64, (r4 & m51) as u64])
    }

    pub(crate) const fn square(&self) -> Fe {
        self.mul(self)
    }

    /// `self^e`, for a public exponent `e` (little-endian 64-bit words).
    const fn pow(&self, e: &[u64; 4]) -> Fe {
        let mut r = Fe::ONE;
        let mut i: usize = 256;
        while i > 0 {
            i -= 1;
            r = r.square();
            if (e[i / 64] >> (i % 64)) & 1 == 1 {
                r = r.mul(self);
            }
        }
        r
    }

    /// 1/self, as self^(p − 2). Zero maps to zero.
    pub(crate) const fn invert(&self) -> Fe {
        self.pow(&[0xFFFF_FFFF_FFFF_FFEB, u64::MAX, u64::MAX, 0x7FFF_FFFF_FFFF_FFFF])
    }

    /// self^((p − 5) / 8) = self^(2^252 − 3), for square roots.
    const fn pow_p58(&self) -> Fe {
        self.pow(&[0xFFFF_FFFF_FFFF_FFFD, u64::MAX, u64::MAX, 0x0FFF_FFFF_FFFF_FFFF])
    }

    /// Loads 32 little-endian bytes, ignoring the top bit.
    pub(crate) const fn from_bytes(s: &[u8; 32]) -> Fe {
        Fe([
            load64(s, 0) & MASK,
            (load64(s, 6) >> 3) & MASK,
            (load64(s, 12) >> 6) & MASK,
            (load64(s, 19) >> 1) & MASK,
            (load64(s, 24) >> 12) & MASK,
        ])
    }

    /// The canonical encoding (fully reduced mod p), as in curve25519-donna's
    /// contraction: carry twice, add 19, carry, add 2^255 − 19, carry, drop 2^255.
    pub(crate) const fn to_bytes(self) -> [u8; 32] {
        let mut t = self.0;
        let mut pass = 0;
        while pass < 2 {
            t[1] += t[0] >> 51;
            t[0] &= MASK;
            t[2] += t[1] >> 51;
            t[1] &= MASK;
            t[3] += t[2] >> 51;
            t[2] &= MASK;
            t[4] += t[3] >> 51;
            t[3] &= MASK;
            t[0] += 19 * (t[4] >> 51);
            t[4] &= MASK;
            pass += 1;
        }
        // Now 0 ≤ t < 2^255. Add 19 and carry: offsets the value by 19.
        t[0] += 19;
        t[1] += t[0] >> 51;
        t[0] &= MASK;
        t[2] += t[1] >> 51;
        t[1] &= MASK;
        t[3] += t[2] >> 51;
        t[2] &= MASK;
        t[4] += t[3] >> 51;
        t[3] &= MASK;
        t[0] += 19 * (t[4] >> 51);
        t[4] &= MASK;
        // Add 2^255 − 19 (all limbs 2^51 − 1, the first 2^51 − 19), carry, and drop 2^255.
        t[0] += (1 << 51) - 19;
        t[1] += (1 << 51) - 1;
        t[2] += (1 << 51) - 1;
        t[3] += (1 << 51) - 1;
        t[4] += (1 << 51) - 1;
        t[1] += t[0] >> 51;
        t[0] &= MASK;
        t[2] += t[1] >> 51;
        t[1] &= MASK;
        t[3] += t[2] >> 51;
        t[2] &= MASK;
        t[4] += t[3] >> 51;
        t[3] &= MASK;
        t[4] &= MASK;
        let w = [
            t[0] | (t[1] << 51),
            (t[1] >> 13) | (t[2] << 38),
            (t[2] >> 26) | (t[3] << 25),
            (t[3] >> 39) | (t[4] << 12),
        ];
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            out[i] = (w[i / 8] >> (8 * (i % 8))) as u8;
            i += 1;
        }
        out
    }

    fn eq(&self, b: &Fe) -> bool {
        ct::eq(&self.to_bytes(), &b.to_bytes())
    }

    fn is_negative(&self) -> u8 {
        self.to_bytes()[0] & 1
    }

    fn is_zero(&self) -> bool {
        self.eq(&Fe::ZERO)
    }

    /// `a` if `bit` is 0, `b` if `bit` is 1, without a branch.
    pub(crate) fn select(bit: u64, a: &Fe, b: &Fe) -> Fe {
        let mut r = [0u64; 5];
        for (i, l) in r.iter_mut().enumerate() {
            *l = ct::select64(bit, a.0[i], b.0[i]);
        }
        Fe(r)
    }
}

/// d = −121665 / 121666.
const D: Fe = Fe::small(121665).neg().mul(&Fe::small(121666).invert());
/// 2·d.
const D2: Fe = D.add(&D);
/// √−1 = 2^((p − 1) / 4).
const SQRT_M1: Fe = Fe::small(2).pow(&[0xFFFF_FFFF_FFFF_FFFB, u64::MAX, u64::MAX, 0x1FFF_FFFF_FFFF_FFFF]);

/// A point in extended coordinates (X : Y : Z : T), with x = X/Z, y = Y/Z, xy = T/Z.
#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

impl Point {
    const IDENTITY: Point = Point { x: Fe::ZERO, y: Fe::ONE, z: Fe::ONE, t: Fe::ZERO };

    /// The unified addition of RFC 8032 § 5.1.4 (complete for Ed25519).
    fn add(&self, q: &Point) -> Point {
        let a = self.y.sub(&self.x).mul(&q.y.sub(&q.x));
        let b = self.y.add(&self.x).mul(&q.y.add(&q.x));
        let c = self.t.mul(&D2).mul(&q.t);
        let zz = self.z.mul(&q.z);
        let d = zz.add(&zz);
        let e = b.sub(&a);
        let f = d.sub(&c);
        let g = d.add(&c);
        let h = b.add(&a);
        Point { x: e.mul(&f), y: g.mul(&h), z: f.mul(&g), t: e.mul(&h) }
    }

    fn neg(&self) -> Point {
        Point { x: self.x.neg(), y: self.y, z: self.z, t: self.t.neg() }
    }

    fn select(bit: u64, a: &Point, b: &Point) -> Point {
        Point {
            x: Fe::select(bit, &a.x, &b.x),
            y: Fe::select(bit, &a.y, &b.y),
            z: Fe::select(bit, &a.z, &b.z),
            t: Fe::select(bit, &a.t, &b.t),
        }
    }

    /// [s]P for a 256-bit little-endian scalar, in constant time.
    fn mul(&self, s: &[u8; 32]) -> Point {
        let mut r = Point::IDENTITY;
        for i in (0..256).rev() {
            r = r.add(&r);
            let sum = r.add(self);
            let bit = ((s[i / 8] >> (i % 8)) & 1) as u64;
            r = Point::select(bit, &r, &sum);
        }
        r
    }

    fn encode(&self) -> [u8; 32] {
        let zi = self.z.invert();
        let x = self.x.mul(&zi);
        let y = self.y.mul(&zi);
        let mut s = y.to_bytes();
        s[31] |= x.is_negative() << 7;
        s
    }

    /// RFC 8032 § 5.1.3. `None` if `s` isn't a point's canonical encoding.
    fn decode(s: &[u8; 32]) -> Option<Point> {
        let sign = s[31] >> 7;
        let mut ys = *s;
        ys[31] &= 0x7F;
        let y = Fe::from_bytes(&ys);
        if y.to_bytes() != ys {
            return None; // y ≥ p
        }
        let yy = y.square();
        let u = yy.sub(&Fe::ONE);
        let v = D.mul(&yy).add(&Fe::ONE);
        let v3 = v.square().mul(&v);
        let v7 = v3.square().mul(&v);
        let mut x = u.mul(&v3).mul(&u.mul(&v7).pow_p58());
        let vxx = v.mul(&x.square());
        if !vxx.eq(&u) {
            if vxx.eq(&u.neg()) {
                x = x.mul(&SQRT_M1);
            } else {
                return None;
            }
        }
        if x.is_zero() && sign == 1 {
            return None;
        }
        if x.is_negative() != sign {
            x = x.neg();
        }
        Some(Point { x, y, z: Fe::ONE, t: x.mul(&y) })
    }

    fn base() -> Point {
        // B: y = 4/5, x even (RFC 8032 § 5.1).
        let mut s = [0x66u8; 32];
        s[0] = 0x58;
        match Point::decode(&s) {
            Some(p) => p,
            None => unreachable!("the base point always decodes"),
        }
    }
}

/// L = 2^252 + 27742317777372353535851937790883648493, the group order.
const L: [u64; 4] = [0x5812_631A_5CF5_D3ED, 0x14DE_F9DE_A2F7_9CD6, 0, 0x1000_0000_0000_0000];

/// A 512-bit little-endian number mod L, by constant-time shift-and-subtract.
fn reduce_wide(x: &[u64; 8]) -> [u64; 4] {
    let mut r = [0u64; 4];
    for i in (0..512).rev() {
        let bit = (x[i / 64] >> (i % 64)) & 1;
        // r = 2r + bit (r < L < 2^253, so 2r + 1 fits in 256 bits)
        r[3] = (r[3] << 1) | (r[2] >> 63);
        r[2] = (r[2] << 1) | (r[1] >> 63);
        r[1] = (r[1] << 1) | (r[0] >> 63);
        r[0] = (r[0] << 1) | bit;
        // t = r − L; keep t when there was no borrow.
        let mut t = [0u64; 4];
        let mut borrow = 0u64;
        for k in 0..4 {
            let (d1, b1) = r[k].overflowing_sub(L[k]);
            let (d2, b2) = d1.overflowing_sub(borrow);
            t[k] = d2;
            borrow = (b1 | b2) as u64;
        }
        for k in 0..4 {
            r[k] = ct::select64(borrow, t[k], r[k]);
        }
    }
    r
}

fn words_from_bytes<const N: usize>(b: &[u8]) -> [u64; N] {
    let mut w = [0u64; N];
    for (i, chunk) in b.as_chunks::<8>().0.iter().enumerate().take(N) {
        w[i] = u64::from_le_bytes(*chunk);
    }
    w
}

fn bytes_from_words(w: &[u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, word) in w.iter().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// A 64-byte hash, read as a little-endian integer, mod L.
fn scalar_from_hash(h: &[u8; 64]) -> [u8; 32] {
    bytes_from_words(&reduce_wide(&words_from_bytes::<8>(h)))
}

/// (a·b + c) mod L, in constant time.
fn mul_add(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let a = words_from_bytes::<4>(a);
    let b = words_from_bytes::<4>(b);
    let c = words_from_bytes::<4>(c);
    let mut p = [0u64; 8];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let v = (a[i] as u128) * (b[j] as u128) + (p[i + j] as u128) + carry;
            p[i + j] = v as u64;
            carry = v >> 64;
        }
        p[i + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for (k, word) in p.iter_mut().enumerate() {
        let add = if k < 4 { c[k] } else { 0 };
        let v = (*word as u128) + (add as u128) + carry;
        *word = v as u64;
        carry = v >> 64;
    }
    bytes_from_words(&reduce_wide(&p))
}

/// `true` if the 32-byte little-endian `s` is below L (S must be canonical).
fn is_canonical_scalar(s: &[u8; 32]) -> bool {
    let w = words_from_bytes::<4>(s);
    for k in (0..4).rev() {
        if w[k] != L[k] {
            return w[k] < L[k];
        }
    }
    false
}

/// The secret scalar and the nonce prefix from a 32-byte seed (RFC 8032 § 5.1.5).
fn expand(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut h = Sha512::new();
    h.update(seed);
    let h = h.finish();
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&h[32..]);
    (a, prefix)
}

/// The public key for a 32-byte secret seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let (a, _) = expand(seed);
    Point::base().mul(&a).encode()
}

/// Signs `msg` with the secret `seed` (RFC 8032 § 5.1.6). Deterministic.
pub fn sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let (a, prefix) = expand(seed);
    let base = Point::base();
    let public = base.mul(&a).encode();

    let mut h = Sha512::new();
    h.update(&prefix);
    h.update(msg);
    let r = scalar_from_hash(&h.finish());
    let big_r = base.mul(&r).encode();

    let mut h = Sha512::new();
    h.update(&big_r);
    h.update(&public);
    h.update(msg);
    let k = scalar_from_hash(&h.finish());
    let s = mul_add(&k, &a, &r);

    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&big_r);
    sig[32..].copy_from_slice(&s);
    sig
}

/// Verifies `sig` over `msg` against `public` (RFC 8032 § 5.1.7, cofactorless).
pub fn verify(public: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let mut big_r = [0u8; 32];
    big_r.copy_from_slice(&sig[..32]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&sig[32..]);
    if !is_canonical_scalar(&s) {
        return false;
    }
    let Some(a) = Point::decode(public) else {
        return false;
    };
    let mut h = Sha512::new();
    h.update(&big_r);
    h.update(public);
    h.update(msg);
    let k = scalar_from_hash(&h.finish());
    let check = Point::base().mul(&s).add(&a.neg().mul(&k));
    ct::eq(&check.encode(), &big_r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_inverse_is_an_inverse() {
        let x = Fe::small(1234567);
        assert!(x.mul(&x.invert()).eq(&Fe::ONE));
    }

    #[test]
    fn sqrt_m1_squares_to_minus_one() {
        assert!(SQRT_M1.square().eq(&Fe::ONE.neg()));
    }

    #[test]
    fn base_point_round_trips() {
        let b = Point::base();
        let mut s = [0x66u8; 32];
        s[0] = 0x58;
        assert_eq!(b.encode(), s);
    }

    #[test]
    fn order_times_base_is_identity() {
        let l = bytes_from_words(&L);
        assert_eq!(Point::base().mul(&l).encode(), Point::IDENTITY.encode());
    }

    #[test]
    fn non_canonical_s_is_rejected() {
        let seed = [7u8; 32];
        let public = public_key(&seed);
        let mut sig = sign(&seed, b"freally");
        assert!(verify(&public, b"freally", &sig));
        // S + L is the same scalar mod L, but non-canonical: must be refused.
        let s = words_from_bytes::<4>(&sig[32..]);
        let mut sum = [0u64; 4];
        let mut carry = 0u128;
        for k in 0..4 {
            let v = s[k] as u128 + L[k] as u128 + carry;
            sum[k] = v as u64;
            carry = v >> 64;
        }
        sig[32..].copy_from_slice(&bytes_from_words(&sum));
        assert!(!verify(&public, b"freally", &sig));
    }
}
