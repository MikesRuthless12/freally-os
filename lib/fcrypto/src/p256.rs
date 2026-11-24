// SPDX-License-Identifier: Apache-2.0
//! P-256 ECDH (SEC 1 § 3.3.1; the curve of FIPS 186-4 § D.1.2.3), for TLS
//! 1.3's secp256r1 key share (TASK-180).
//!
//! **Constant time in the secret scalar.** The field is Montgomery
//! arithmetic modulo p with every correction masked, never branched on; points
//! are added with the complete formula of Renes, Costello and Batina (2016,
//! algorithm 4, a = −3), which has no special case (doubling and the identity
//! included), so the same operations run whatever the points are; the scalar
//! is walked by a Montgomery ladder whose swaps are masks. Only public values
//! choose a branch: the inversion's fixed exponent, and the peer's point,
//! which is checked on the curve before it's used.

type Fe = [u64; 4];

const P: Fe = [0xffff_ffff_ffff_ffff, 0x0000_0000_ffff_ffff, 0x0000_0000_0000_0000, 0xffff_ffff_0000_0001];
const N: Fe = [0xf3b9_cac2_fc63_2551, 0xbce6_faad_a717_9e84, 0xffff_ffff_ffff_ffff, 0xffff_ffff_0000_0000];
/// R² mod p, R = 2^256: into Montgomery form by one multiplication.
const R2: Fe = [0x0000_0000_0000_0003, 0xffff_fffb_ffff_ffff, 0xffff_ffff_ffff_fffe, 0x0000_0004_ffff_fffd];
/// 1 in Montgomery form (R mod p).
const ONE: Fe = [0x0000_0000_0000_0001, 0xffff_ffff_0000_0000, 0xffff_ffff_ffff_ffff, 0x0000_0000_ffff_fffe];
/// The curve's b, in Montgomery form.
const B: Fe = [0xd89c_df62_29c4_bddf, 0xacf0_05cd_7884_3090, 0xe5a2_20ab_f721_2ed6, 0xdc30_061d_0487_4834];
const GX: Fe = [0x79e7_30d4_18a9_143c, 0x75ba_95fc_5fed_b601, 0x79fb_732b_7762_2510, 0x1890_5f76_a537_55c6];
const GY: Fe = [0xddf2_5357_ce95_560a, 0x8b4a_b8e4_ba19_e45c, 0xd2e8_8688_dd21_f325, 0x8571_ff18_2588_5d85];

fn adc(a: u64, b: u64, c: u64) -> (u64, u64) {
    let t = a as u128 + b as u128 + c as u128;
    (t as u64, (t >> 64) as u64)
}

fn sbb(a: u64, b: u64, borrow: u64) -> (u64, u64) {
    let t = (a as u128).wrapping_sub(b as u128 + borrow as u128);
    (t as u64, (t >> 127) as u64)
}

fn mac(a: u64, b: u64, c: u64, carry: u64) -> (u64, u64) {
    let t = a as u128 + (b as u128) * (c as u128) + carry as u128;
    (t as u64, (t >> 64) as u64)
}

/// `a` (with `hi` the bit above it) less `m` if that isn't negative, else
/// `a`: chosen by a mask.
fn reduce_once(a: Fe, hi: u64, m: &Fe) -> Fe {
    let mut r = [0u64; 4];
    let mut borrow = 0;
    for i in 0..4 {
        (r[i], borrow) = sbb(a[i], m[i], borrow);
    }
    let (_, borrow) = sbb(hi, 0, borrow);
    let keep = 0u64.wrapping_sub(borrow);
    core::array::from_fn(|i| (a[i] & keep) | (r[i] & !keep))
}

fn add(a: &Fe, b: &Fe) -> Fe {
    let mut r = [0u64; 4];
    let mut c = 0;
    for i in 0..4 {
        (r[i], c) = adc(a[i], b[i], c);
    }
    reduce_once(r, c, &P)
}

fn sub(a: &Fe, b: &Fe) -> Fe {
    let mut r = [0u64; 4];
    let mut borrow = 0;
    for i in 0..4 {
        (r[i], borrow) = sbb(a[i], b[i], borrow);
    }
    let mask = 0u64.wrapping_sub(borrow);
    let mut c = 0;
    for i in 0..4 {
        (r[i], c) = adc(r[i], P[i] & mask, c);
    }
    r
}

/// Montgomery multiplication, a·b/R mod p (CIOS; −p⁻¹ mod 2^64 is 1).
fn mul(a: &Fe, b: &Fe) -> Fe {
    let mut t = [0u64; 6];
    for &bi in b {
        let mut c = 0;
        for j in 0..4 {
            (t[j], c) = mac(t[j], a[j], bi, c);
        }
        (t[4], t[5]) = adc(t[4], c, 0);
        let m = t[0];
        let (_, mut c) = mac(t[0], m, P[0], 0);
        for j in 1..4 {
            (t[j - 1], c) = mac(t[j], m, P[j], c);
        }
        let (s, c2) = adc(t[4], c, 0);
        t[3] = s;
        t[4] = t[5] + c2;
    }
    reduce_once([t[0], t[1], t[2], t[3]], t[4], &P)
}

/// a^(p−2): the inverse (Fermat), over the fixed exponent's bits.
fn inv(a: &Fe) -> Fe {
    const E: Fe = [0xffff_ffff_ffff_fffd, 0x0000_0000_ffff_ffff, 0, 0xffff_ffff_0000_0001];
    let mut r = ONE;
    for i in (0..256).rev() {
        r = mul(&r, &r);
        if (E[i / 64] >> (i % 64)) & 1 == 1 {
            r = mul(&r, a);
        }
    }
    r
}

fn from_bytes(b: &[u8]) -> Fe {
    core::array::from_fn(|i| u64::from_be_bytes(b[24 - 8 * i..32 - 8 * i].try_into().expect("8 bytes")))
}

fn to_bytes(a: &Fe, out: &mut [u8]) {
    for i in 0..4 {
        out[24 - 8 * i..32 - 8 * i].copy_from_slice(&a[i].to_be_bytes());
    }
}

/// Whether `a` < `m`, for public values.
fn below(a: &Fe, m: &Fe) -> bool {
    let mut borrow = 0;
    for i in 0..4 {
        (_, borrow) = sbb(a[i], m[i], borrow);
    }
    borrow == 1
}

/// A point in projective coordinates (Montgomery form); the identity is (0 : 1 : 0).
#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
}

const IDENTITY: Point = Point { x: [0; 4], y: ONE, z: [0; 4] };

/// P + Q for any P and Q (Renes–Costello–Batina 2016, algorithm 4, a = −3).
fn add_points(p: &Point, q: &Point) -> Point {
    let (x1, y1, z1, x2, y2, z2) = (&p.x, &p.y, &p.z, &q.x, &q.y, &q.z);
    let mut t0 = mul(x1, x2);
    let mut t1 = mul(y1, y2);
    let mut t2 = mul(z1, z2);
    let mut t3 = add(x1, y1);
    let mut t4 = add(x2, y2);
    t3 = mul(&t3, &t4);
    t4 = add(&t0, &t1);
    t3 = sub(&t3, &t4);
    t4 = add(y1, z1);
    let mut x3 = add(y2, z2);
    t4 = mul(&t4, &x3);
    x3 = add(&t1, &t2);
    t4 = sub(&t4, &x3);
    x3 = add(x1, z1);
    let mut y3 = add(x2, z2);
    x3 = mul(&x3, &y3);
    y3 = add(&t0, &t2);
    y3 = sub(&x3, &y3);
    let mut z3 = mul(&B, &t2);
    x3 = sub(&y3, &z3);
    z3 = add(&x3, &x3);
    x3 = add(&x3, &z3);
    z3 = sub(&t1, &x3);
    x3 = add(&t1, &x3);
    y3 = mul(&B, &y3);
    t1 = add(&t2, &t2);
    t2 = add(&t1, &t2);
    y3 = sub(&y3, &t2);
    y3 = sub(&y3, &t0);
    t1 = add(&y3, &y3);
    y3 = add(&t1, &y3);
    t1 = add(&t0, &t0);
    t0 = add(&t1, &t0);
    t0 = sub(&t0, &t2);
    t1 = mul(&t4, &y3);
    t2 = mul(&t0, &y3);
    y3 = mul(&x3, &z3);
    y3 = add(&y3, &t2);
    x3 = mul(&t3, &x3);
    x3 = sub(&x3, &t1);
    z3 = mul(&t4, &z3);
    t1 = mul(&t3, &t0);
    z3 = add(&z3, &t1);
    Point { x: x3, y: y3, z: z3 }
}

/// Swaps `a` and `b` when `bit` is 1, by a mask.
fn cswap(a: &mut Point, b: &mut Point, bit: u64) {
    let mask = 0u64.wrapping_sub(bit);
    for (u, v) in [(&mut a.x, &mut b.x), (&mut a.y, &mut b.y), (&mut a.z, &mut b.z)] {
        for i in 0..4 {
            let t = (u[i] ^ v[i]) & mask;
            u[i] ^= t;
            v[i] ^= t;
        }
    }
}

/// k·P by a Montgomery ladder over all 256 bits of `k` (big-endian).
fn ladder(k: &Fe, p: &Point) -> Point {
    let (mut r0, mut r1) = (IDENTITY, *p);
    for i in (0..256).rev() {
        let bit = (k[i / 64] >> (i % 64)) & 1;
        cswap(&mut r0, &mut r1, bit);
        r1 = add_points(&r0, &r1);
        r0 = add_points(&r0, &r0);
        cswap(&mut r0, &mut r1, bit);
    }
    r0
}

/// The affine x and y (out of Montgomery form), or None for the identity.
fn affine(p: &Point) -> Option<(Fe, Fe)> {
    if p.z == [0; 4] {
        return None;
    }
    let zi = inv(&p.z);
    let one = [1, 0, 0, 0];
    Some((mul(&mul(&p.x, &zi), &one), mul(&mul(&p.y, &zi), &one)))
}

/// The scalar from 32 random bytes: reduced mod n (2^256 < 2n, so once).
fn scalar(k: &[u8; 32]) -> Option<Fe> {
    let s = reduce_once(from_bytes(k), 0, &N);
    (s != [0; 4]).then_some(s)
}

/// The public key of the secret `k` (32 random bytes), uncompressed:
/// 0x04, x, y. None only for a scalar of 0 (or n).
pub fn public_key(k: &[u8; 32]) -> Option<[u8; 65]> {
    let s = scalar(k)?;
    let (x, y) = affine(&ladder(&s, &Point { x: GX, y: GY, z: ONE }))?;
    let mut out = [0u8; 65];
    out[0] = 4;
    to_bytes(&x, &mut out[1..33]);
    to_bytes(&y, &mut out[33..]);
    Some(out)
}

/// The shared secret with a peer's public key (uncompressed, 65 bytes):
/// the x of k·Q. None if the peer's point isn't on the curve.
pub fn ecdh(k: &[u8; 32], peer: &[u8]) -> Option<[u8; 32]> {
    let s = scalar(k)?;
    let [4, coords @ ..] = peer else { return None };
    if coords.len() != 64 {
        return None;
    }
    let (x, y) = (from_bytes(&coords[..32]), from_bytes(&coords[32..]));
    if !below(&x, &P) || !below(&y, &P) {
        return None;
    }
    let (xm, ym) = (mul(&x, &R2), mul(&y, &R2));
    // y² = x³ − 3x + b
    let rhs = add(&sub(&mul(&mul(&xm, &xm), &xm), &add(&add(&xm, &xm), &xm)), &B);
    if mul(&ym, &ym) != rhs {
        return None;
    }
    let (sx, _) = affine(&ladder(&s, &Point { x: xm, y: ym, z: ONE }))?;
    let mut out = [0u8; 32];
    to_bytes(&sx, &mut out);
    Some(out)
}
