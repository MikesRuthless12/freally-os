// SPDX-License-Identifier: Apache-2.0
//! Unsigned integers of up to 4,096 bits and arithmetic modulo an odd number
//! in Montgomery form (Montgomery 1985; the CIOS method of Koç, Acar and
//! Kaliski 1996), for verifying RSA and ECDSA signatures (TASK-178).
//!
//! **Public values only:** signature verification works on certificates,
//! signatures and public keys, none of them secret, so this is written to be
//! simple and correct, not constant time. Never use it with a private key.

/// The most 64-bit limbs a number holds (4,096 bits).
pub const LIMBS: usize = 64;

/// A number, little-endian 64-bit limbs; only the first `n` of a modulus's size are used.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Uint {
    pub l: [u64; LIMBS],
}

impl core::fmt::Debug for Uint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let top = self.l.iter().rposition(|&x| x != 0).unwrap_or(0);
        write!(f, "0x")?;
        for i in (0..=top).rev() {
            write!(f, "{:016x}", self.l[i])?;
        }
        Ok(())
    }
}

impl Uint {
    pub const ZERO: Uint = Uint { l: [0; LIMBS] };

    pub fn from_u64(v: u64) -> Uint {
        let mut u = Uint::ZERO;
        u.l[0] = v;
        u
    }

    /// From big-endian bytes; `None` past 4,096 bits.
    pub fn from_be(b: &[u8]) -> Option<Uint> {
        let b = &b[b.iter().position(|&x| x != 0).unwrap_or(b.len())..];
        if b.len() > LIMBS * 8 {
            return None;
        }
        let mut u = Uint::ZERO;
        for (i, &byte) in b.iter().rev().enumerate() {
            u.l[i / 8] |= (byte as u64) << (8 * (i % 8));
        }
        Some(u)
    }

    /// As `out.len()` big-endian bytes (the low ones; `out` long enough for the number).
    pub fn to_be(&self, out: &mut [u8]) {
        let n = out.len();
        for (i, o) in out.iter_mut().enumerate() {
            let k = n - 1 - i;
            *o = if k / 8 < LIMBS { (self.l[k / 8] >> (8 * (k % 8))) as u8 } else { 0 };
        }
    }

    pub fn bits(&self) -> usize {
        match self.l.iter().rposition(|&x| x != 0) {
            Some(i) => 64 * i + 64 - self.l[i].leading_zeros() as usize,
            None => 0,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.l.iter().all(|&x| x == 0)
    }

    pub fn bit(&self, i: usize) -> bool {
        i < 64 * LIMBS && (self.l[i / 64] >> (i % 64)) & 1 == 1
    }

    /// `self` < `o`.
    pub fn lt(&self, o: &Uint) -> bool {
        for i in (0..LIMBS).rev() {
            if self.l[i] != o.l[i] {
                return self.l[i] < o.l[i];
            }
        }
        false
    }

    /// `self` − `o` (the borrow out, if `o` was larger).
    fn sub_assign(&mut self, o: &Uint) -> bool {
        let mut borrow = 0u64;
        for i in 0..LIMBS {
            let (d, b1) = self.l[i].overflowing_sub(o.l[i]);
            let (d, b2) = d.overflowing_sub(borrow);
            self.l[i] = d;
            borrow = (b1 | b2) as u64;
        }
        borrow != 0
    }

    /// `self` + `o` (the carry out).
    fn add_assign(&mut self, o: &Uint) -> bool {
        let mut carry = 0u64;
        for i in 0..LIMBS {
            let (s, c1) = self.l[i].overflowing_add(o.l[i]);
            let (s, c2) = s.overflowing_add(carry);
            self.l[i] = s;
            carry = (c1 | c2) as u64;
        }
        carry != 0
    }
}

/// Arithmetic modulo an odd `m`, in Montgomery form (R = 2^(64·n)).
#[derive(Clone, Copy)]
pub struct Mont {
    pub m: Uint,
    n: usize,
    /// −m⁻¹ mod 2^64.
    minv: u64,
    /// R² mod m.
    r2: Uint,
}

impl core::fmt::Debug for Mont {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Mont({} bits)", self.m.bits())
    }
}

impl Mont {
    /// For an odd modulus above 1; `None` otherwise.
    pub fn new(m: Uint) -> Option<Mont> {
        if m.l[0] & 1 == 0 || m.bits() < 2 {
            return None;
        }
        let n = m.bits().div_ceil(64);
        // Newton's iteration: each step doubles the correct low bits of m⁻¹.
        let mut inv = 1u64;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m.l[0].wrapping_mul(inv)));
        }
        // R² mod m, by doubling 1 a total of 2·64·n times, each step reduced.
        let mut r = Uint::from_u64(1);
        for _ in 0..2 * 64 * n {
            let carry = r.add_assign(&r.clone());
            if carry || !r.lt(&m) {
                r.sub_assign(&m);
            }
        }
        Some(Mont { m, n, minv: inv.wrapping_neg(), r2: r })
    }

    /// a · b · R⁻¹ mod m (a, b < m), by CIOS.
    pub fn mul(&self, a: &Uint, b: &Uint) -> Uint {
        let n = self.n;
        let mut t = [0u64; LIMBS + 2];
        for &bi in &b.l[..n] {
            let mut c = 0u128;
            for j in 0..n {
                let s = t[j] as u128 + a.l[j] as u128 * bi as u128 + c;
                t[j] = s as u64;
                c = s >> 64;
            }
            let s = t[n] as u128 + c;
            t[n] = s as u64;
            t[n + 1] = (s >> 64) as u64;
            let q = t[0].wrapping_mul(self.minv);
            let mut c = (t[0] as u128 + q as u128 * self.m.l[0] as u128) >> 64;
            for j in 1..n {
                let s = t[j] as u128 + q as u128 * self.m.l[j] as u128 + c;
                t[j - 1] = s as u64;
                c = s >> 64;
            }
            let s = t[n] as u128 + c;
            t[n - 1] = s as u64;
            t[n] = t[n + 1] + (s >> 64) as u64;
        }
        let mut r = Uint::ZERO;
        r.l[..n].copy_from_slice(&t[..n]);
        // The result is below 2m: once more m off, within n limbs (the last
        // borrow cancels t[n] when the result went past 2^(64n)).
        if t[n] != 0 || !r.lt(&self.m) {
            let mut borrow = 0u64;
            for j in 0..n {
                let (d, b1) = r.l[j].overflowing_sub(self.m.l[j]);
                let (d, b2) = d.overflowing_sub(borrow);
                r.l[j] = d;
                borrow = (b1 | b2) as u64;
            }
        }
        r
    }

    pub fn to_mont(&self, a: &Uint) -> Uint {
        self.mul(a, &self.r2)
    }

    pub fn from_mont(&self, a: &Uint) -> Uint {
        self.mul(a, &Uint::from_u64(1))
    }

    /// a + b mod m (a, b < m).
    pub fn add(&self, a: &Uint, b: &Uint) -> Uint {
        let mut r = *a;
        let carry = r.add_assign(b);
        if carry || !r.lt(&self.m) {
            r.sub_assign(&self.m);
        }
        r
    }

    /// a − b mod m (a, b < m).
    pub fn sub(&self, a: &Uint, b: &Uint) -> Uint {
        let mut r = *a;
        if r.sub_assign(b) {
            r.add_assign(&self.m);
        }
        r
    }

    /// base^exp mod m, `base` < m and in normal form, the result in normal form.
    pub fn pow(&self, base: &Uint, exp: &Uint) -> Uint {
        let b = self.to_mont(base);
        let mut r = self.to_mont(&Uint::from_u64(1));
        for i in (0..exp.bits()).rev() {
            r = self.mul(&r, &r);
            if exp.bit(i) {
                r = self.mul(&r, &b);
            }
        }
        self.from_mont(&r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_numbers_agree_with_u128() {
        let m = Mont::new(Uint::from_u64(1_000_000_007)).unwrap();
        let p = |b: u64, e: u64| m.pow(&Uint::from_u64(b), &Uint::from_u64(e)).l[0];
        let slow = |b: u64, e: u64| (0..e).fold(1u128, |acc, _| acc * b as u128 % 1_000_000_007) as u64;
        for (b, e) in [(2, 10), (3, 1000), (123_456_789, 65537), (999_999_999, 2), (5, 0), (0, 5)] {
            assert_eq!(p(b, e), slow(b, e), "{b}^{e}");
        }
        assert!(Mont::new(Uint::from_u64(10)).is_none(), "even");
        assert!(Mont::new(Uint::from_u64(1)).is_none());
    }

    #[test]
    fn bytes_both_ways() {
        let b = [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let u = Uint::from_be(&b).unwrap();
        assert_eq!(u.l[0], 0x0203_0405_0607_0809);
        assert_eq!(u.l[1], 1);
        assert_eq!(u.bits(), 65);
        let mut out = [0u8; 11];
        u.to_be(&mut out);
        assert_eq!(out, b);
        assert!(Uint::from_be(&[1; 513]).is_none(), "past 4,096 bits");
    }
}
