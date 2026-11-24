// SPDX-License-Identifier: Apache-2.0
//! ECDSA signature verification on NIST P-256 and P-384 (FIPS 186-4 § 6.4,
//! SEC 1 § 4.1.4), for TLS 1.3's ecdsa_secp256r1_sha256 and
//! ecdsa_secp384r1_sha384 and certificates signed with them. Points in
//! Jacobian coordinates, both curves' a = −3 formulas (Bernstein and Lange's
//! dbl-2001-b and add-2007-bl). Verification only: public values, see
//! [`crate::bignum`].

use crate::bignum::{Mont, Uint};

/// A curve's numbers, as big-endian hex in SEC 2 / FIPS 186-4.
struct Params {
    p: &'static str,
    n: &'static str,
    b: &'static str,
    gx: &'static str,
    gy: &'static str,
}

const P256: Params = Params {
    p: "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
    n: "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
    b: "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
    gx: "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
    gy: "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
};

const P384: Params = Params {
    p: "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
    n: "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
    b: "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
    gx: "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
    gy: "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

impl Curve {
    /// The size of a coordinate, or a scalar, in bytes.
    pub fn size(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
        }
    }
}

fn hex(s: &str) -> Uint {
    let mut b = [0u8; 48];
    let n = s.len() / 2;
    for i in 0..n {
        b[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0);
    }
    Uint::from_be(&b[..n]).unwrap_or(Uint::ZERO)
}

/// A point (X, Y, Z) in Jacobian coordinates, the field elements in
/// Montgomery form; Z = 0 is the point at infinity.
#[derive(Clone, Copy)]
struct Point {
    x: Uint,
    y: Uint,
    z: Uint,
}

struct Field {
    f: Mont,
    one: Uint,
}

impl Field {
    fn mul(&self, a: &Uint, b: &Uint) -> Uint {
        self.f.mul(a, b)
    }
    fn sq(&self, a: &Uint) -> Uint {
        self.f.mul(a, a)
    }
    fn add(&self, a: &Uint, b: &Uint) -> Uint {
        self.f.add(a, b)
    }
    fn sub(&self, a: &Uint, b: &Uint) -> Uint {
        self.f.sub(a, b)
    }
    fn small(&self, k: u64, a: &Uint) -> Uint {
        let mut r = Uint::ZERO;
        for _ in 0..k {
            r = self.add(&r, a);
        }
        r
    }

    /// dbl-2001-b (a = −3).
    fn double(&self, p: &Point) -> Point {
        if p.z.is_zero() || p.y.is_zero() {
            return Point { x: self.one, y: self.one, z: Uint::ZERO };
        }
        let delta = self.sq(&p.z);
        let gamma = self.sq(&p.y);
        let beta = self.mul(&p.x, &gamma);
        let alpha = self.small(3, &self.mul(&self.sub(&p.x, &delta), &self.add(&p.x, &delta)));
        let x3 = self.sub(&self.sq(&alpha), &self.small(8, &beta));
        let z3 = self.sub(&self.sub(&self.sq(&self.add(&p.y, &p.z)), &gamma), &delta);
        let y3 = self.sub(&self.mul(&alpha, &self.sub(&self.small(4, &beta), &x3)), &self.small(8, &self.sq(&gamma)));
        Point { x: x3, y: y3, z: z3 }
    }

    /// add-2007-bl, with the cases it can't take (infinity, P = ±Q) handled.
    fn add_points(&self, p: &Point, q: &Point) -> Point {
        if p.z.is_zero() {
            return *q;
        }
        if q.z.is_zero() {
            return *p;
        }
        let z1z1 = self.sq(&p.z);
        let z2z2 = self.sq(&q.z);
        let u1 = self.mul(&p.x, &z2z2);
        let u2 = self.mul(&q.x, &z1z1);
        let s1 = self.mul(&self.mul(&p.y, &q.z), &z2z2);
        let s2 = self.mul(&self.mul(&q.y, &p.z), &z1z1);
        let h = self.sub(&u2, &u1);
        let r = self.small(2, &self.sub(&s2, &s1));
        if h.is_zero() {
            return if r.is_zero() { self.double(p) } else { Point { x: self.one, y: self.one, z: Uint::ZERO } };
        }
        let i = self.sq(&self.small(2, &h));
        let j = self.mul(&h, &i);
        let v = self.mul(&u1, &i);
        let x3 = self.sub(&self.sub(&self.sq(&r), &j), &self.small(2, &v));
        let y3 = self.sub(&self.mul(&r, &self.sub(&v, &x3)), &self.small(2, &self.mul(&s1, &j)));
        let z3 = self.mul(&self.sub(&self.sub(&self.sq(&self.add(&p.z, &q.z)), &z1z1), &z2z2), &h);
        Point { x: x3, y: y3, z: z3 }
    }
}

/// Verifies the signature (`r`, `s`) of `digest` by the public key
/// (`x`, `y`) on `curve` (each big-endian, `curve.size()` bytes; the digest
/// any length, its leftmost bits taken as FIPS 186-4 says).
pub fn verify(curve: Curve, x: &[u8], y: &[u8], digest: &[u8], r: &[u8], s: &[u8]) -> bool {
    let c = match curve {
        Curve::P256 => &P256,
        Curve::P384 => &P384,
    };
    let size = curve.size();
    if [x, y, r, s].iter().any(|v| v.len() != size) {
        return false;
    }
    let (Some(fp), Some(fn_)) = (Mont::new(hex(c.p)), Mont::new(hex(c.n))) else { return false };
    let f = Field { one: fp.to_mont(&Uint::from_u64(1)), f: fp };
    let (Some(qx), Some(qy), Some(ru), Some(su)) =
        (Uint::from_be(x), Uint::from_be(y), Uint::from_be(r), Uint::from_be(s))
    else {
        return false;
    };
    let n = fn_.m;
    // r and s in [1, n − 1]; the key's coordinates below p, and on the curve.
    if ru.is_zero() || su.is_zero() || !ru.lt(&n) || !su.lt(&n) || !qx.lt(&f.f.m) || !qy.lt(&f.f.m) {
        return false;
    }
    let (mx, my) = (f.f.to_mont(&qx), f.f.to_mont(&qy));
    let rhs = f.add(&f.sub(&f.mul(&f.sq(&mx), &mx), &f.small(3, &mx)), &f.f.to_mont(&hex(c.b)));
    if f.sq(&my) != rhs {
        return false;
    }
    // e: the digest's leftmost bits (a whole number of bytes for these curves), mod n.
    let take = digest.len().min(size);
    let Some(mut e) = Uint::from_be(&digest[..take]) else { return false };
    if !e.lt(&n) {
        e = fn_.sub(&e, &n);
    }
    // w = s⁻¹ = s^(n−2) mod n; u1 = e·w, u2 = r·w.
    let n2 = fn_.sub(&n, &Uint::from_u64(2));
    let w = fn_.pow(&su, &n2);
    let u1 = fn_.mul(&fn_.to_mont(&e), &w);
    let u2 = fn_.mul(&fn_.to_mont(&ru), &w);
    // u1·G + u2·Q, both scalars' bits walked together (Shamir's trick).
    let g = Point { x: f.f.to_mont(&hex(c.gx)), y: f.f.to_mont(&hex(c.gy)), z: f.one };
    let q = Point { x: mx, y: my, z: f.one };
    let gq = f.add_points(&g, &q);
    let mut acc = Point { x: f.one, y: f.one, z: Uint::ZERO };
    for i in (0..8 * size).rev() {
        acc = f.double(&acc);
        acc = match (u1.bit(i), u2.bit(i)) {
            (true, true) => f.add_points(&acc, &gq),
            (true, false) => f.add_points(&acc, &g),
            (false, true) => f.add_points(&acc, &q),
            (false, false) => acc,
        };
    }
    if acc.z.is_zero() {
        return false;
    }
    // x = X / Z² in normal form, then mod n, compared with r.
    let p2 = f.f.sub(&f.f.m, &Uint::from_u64(2));
    let zinv = f.f.to_mont(&f.f.pow(&f.f.from_mont(&acc.z), &p2));
    let ax = f.f.from_mont(&f.mul(&acc.x, &f.sq(&zinv)));
    let v = if ax.lt(&n) { ax } else { fn_.sub(&ax, &n) };
    v == ru
}
