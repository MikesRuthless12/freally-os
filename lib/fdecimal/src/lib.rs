// SPDX-License-Identifier: Apache-2.0
//! Exact decimal arithmetic (TASK-112, FR-054), for the Calculator: a number
//! is a sign, its decimal digits and how many of them come after the point,
//! so `0.1 + 0.2` is `0.3` exactly. Addition, subtraction and multiplication
//! are exact; division, roots and the scientific functions are worked to
//! `WORK` digits after the point and rounded (half to even) to the
//! `SHOWN` significant digits a result is shown with.
//!
//! Digits are kept in base 10 (one per byte): a calculator's numbers are a
//! few dozen digits, and base-10 long division is short and plainly right.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

pub mod expr;

/// Digits after the point that division and the functions work to.
pub const WORK: u32 = 50;
/// Significant digits a result is shown with.
pub const SHOWN: usize = 32;
/// The largest exponent a result may have (beyond: Overflow).
pub const MAX_EXP: i64 = 9_999;
/// Past this many digits, a power's steps keep `WORK + 20` significant ones
/// (no shown result needs more; exact ones are far shorter).
const LONG: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    DivideByZero,
    /// Outside the function's domain (√ of a negative, ln of 0 …).
    Domain,
    Overflow,
    Syntax,
}

impl Error {
    pub fn words(self) -> &'static str {
        match self {
            Error::DivideByZero => "Can't divide by zero",
            Error::Domain => "Invalid input",
            Error::Overflow => "Overflow",
            Error::Syntax => "That isn't a sum",
        }
    }
}

/// A decimal number: `digits` (least significant first, no extra zeros) ×
/// 10^-`scale`, negative if `neg` (never a negative zero).
#[derive(Clone, Debug, Default)]
pub struct Decimal {
    neg: bool,
    digits: Vec<u8>,
    scale: u32,
}

// ── Magnitudes: digit vectors, least significant first ─────────────────────

fn trim(d: &mut Vec<u8>) {
    while d.len() > 1 && *d.last().unwrap_or(&1) == 0 {
        d.pop();
    }
    if d.is_empty() {
        d.push(0);
    }
}

fn is_zero(d: &[u8]) -> bool {
    d.iter().all(|&x| x == 0)
}

fn cmp_mag(a: &[u8], b: &[u8]) -> Ordering {
    let (la, lb) = (a.iter().rposition(|&x| x != 0), b.iter().rposition(|&x| x != 0));
    match (la, lb) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(x), Some(y)) if x != y => x.cmp(&y),
        (Some(x), _) => {
            for i in (0..=x).rev() {
                match a[i].cmp(&b[i]) {
                    Ordering::Equal => continue,
                    o => return o,
                }
            }
            Ordering::Equal
        }
    }
}

fn add_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0;
    for i in 0..a.len().max(b.len()) {
        let s = a.get(i).copied().unwrap_or(0) + b.get(i).copied().unwrap_or(0) + carry;
        out.push(s % 10);
        carry = s / 10;
    }
    if carry > 0 {
        out.push(carry);
    }
    trim(&mut out);
    out
}

/// `a - b`, with `a >= b`.
fn sub_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i8;
    for (i, &x) in a.iter().enumerate() {
        let mut d = x as i8 - b.get(i).copied().unwrap_or(0) as i8 - borrow;
        borrow = if d < 0 {
            d += 10;
            1
        } else {
            0
        };
        out.push(d as u8);
    }
    trim(&mut out);
    out
}

fn mul_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut acc = alloc::vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        if x == 0 {
            continue;
        }
        for (j, &y) in b.iter().enumerate() {
            acc[i + j] += x as u32 * y as u32;
        }
    }
    let mut out = Vec::with_capacity(acc.len() + 1);
    let mut carry = 0u32;
    for v in acc {
        let s = v + carry;
        out.push((s % 10) as u8);
        carry = s / 10;
    }
    while carry > 0 {
        out.push((carry % 10) as u8);
        carry /= 10;
    }
    trim(&mut out);
    out
}

/// `a / b` (b ≠ 0): the quotient and the remainder, by long division.
fn divmod_mag(a: &[u8], b: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut q = alloc::vec![0u8; a.len()];
    let top = b.iter().rposition(|&x| x != 0).unwrap_or(0);
    if top < 9 {
        // A divisor under 10^9: one digit at a time, the remainder in a u64.
        let d = b[..=top].iter().rev().fold(0u64, |v, &x| v * 10 + x as u64);
        let mut r = 0u64;
        for i in (0..a.len()).rev() {
            r = r * 10 + a[i] as u64;
            q[i] = (r / d) as u8;
            r %= d;
        }
        trim(&mut q);
        let mut rem = Vec::new();
        loop {
            rem.push((r % 10) as u8);
            r /= 10;
            if r == 0 {
                break;
            }
        }
        return (q, rem);
    }
    let mut r: Vec<u8> = alloc::vec![0];
    for i in (0..a.len()).rev() {
        // r = r * 10 + a[i]
        r.insert(0, a[i]);
        trim(&mut r);
        let mut digit = 0;
        while cmp_mag(&r, b) != Ordering::Less {
            r = sub_mag(&r, b);
            digit += 1;
        }
        q[i] = digit;
    }
    trim(&mut q);
    (q, r)
}

/// `d` × 10^`n`.
fn shift_up(d: &[u8], n: u32) -> Vec<u8> {
    if is_zero(d) {
        return alloc::vec![0];
    }
    let mut out = alloc::vec![0u8; n as usize];
    out.extend_from_slice(d);
    out
}

// ── Decimals ───────────────────────────────────────────────────────────────

impl Decimal {
    pub fn zero() -> Decimal {
        Decimal { neg: false, digits: alloc::vec![0], scale: 0 }
    }

    pub fn from_i64(v: i64) -> Decimal {
        let mut digits = Vec::new();
        let mut m = v.unsigned_abs();
        loop {
            digits.push((m % 10) as u8);
            m /= 10;
            if m == 0 {
                break;
            }
        }
        Decimal { neg: v < 0, digits, scale: 0 }.normal()
    }

    /// "12.5", "-0.003", "1e-5", "2.5E+3".
    pub fn parse(s: &str) -> Result<Decimal, Error> {
        let s = s.trim();
        let (mant, exp) = match s.find(['e', 'E']) {
            Some(i) => (&s[..i], s[i + 1..].parse::<i64>().map_err(|_| Error::Syntax)?),
            None => (s, 0),
        };
        let (neg, mant) = match mant.strip_prefix('-') {
            Some(m) => (true, m),
            None => (false, mant.strip_prefix('+').unwrap_or(mant)),
        };
        let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
        if (int.is_empty() && frac.is_empty()) || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
            return Err(Error::Syntax);
        }
        let digits: Vec<u8> = int.bytes().chain(frac.bytes()).rev().map(|b| b - b'0').collect();
        let d = Decimal { neg, digits, scale: frac.len() as u32 }.normal();
        d.times_pow10(exp)
    }

    /// No extra zeros: none leading, none trailing after the point.
    fn normal(mut self) -> Decimal {
        trim(&mut self.digits);
        while self.scale > 0 && self.digits.len() > 1 && self.digits[0] == 0 {
            self.digits.remove(0);
            self.scale -= 1;
        }
        if self.scale > 0 && is_zero(&self.digits) {
            self.scale = 0;
        }
        if is_zero(&self.digits) {
            self.neg = false;
        }
        self
    }

    pub fn is_zero(&self) -> bool {
        is_zero(&self.digits)
    }

    pub fn is_negative(&self) -> bool {
        self.neg && !self.is_zero()
    }

    pub fn is_integer(&self) -> bool {
        self.clone().normal().scale == 0
    }

    pub fn neg(&self) -> Decimal {
        Decimal { neg: !self.neg, ..self.clone() }.normal()
    }

    pub fn abs(&self) -> Decimal {
        Decimal { neg: false, ..self.clone() }
    }

    /// Both at the larger scale: their digits lined up.
    fn aligned(&self, o: &Decimal) -> (Vec<u8>, Vec<u8>, u32) {
        let s = self.scale.max(o.scale);
        (shift_up(&self.digits, s - self.scale), shift_up(&o.digits, s - o.scale), s)
    }

    pub fn add(&self, o: &Decimal) -> Decimal {
        let (a, b, scale) = self.aligned(o);
        if self.neg == o.neg {
            return Decimal { neg: self.neg, digits: add_mag(&a, &b), scale }.normal();
        }
        match cmp_mag(&a, &b) {
            Ordering::Equal => Decimal::zero(),
            Ordering::Greater => Decimal { neg: self.neg, digits: sub_mag(&a, &b), scale }.normal(),
            Ordering::Less => Decimal { neg: o.neg, digits: sub_mag(&b, &a), scale }.normal(),
        }
    }

    pub fn sub(&self, o: &Decimal) -> Decimal {
        self.add(&o.neg())
    }

    pub fn mul(&self, o: &Decimal) -> Decimal {
        Decimal { neg: self.neg != o.neg, digits: mul_mag(&self.digits, &o.digits), scale: self.scale + o.scale }
            .normal()
    }

    /// `self / o` to `digits` places after the point, rounded half to even.
    pub fn div_to(&self, o: &Decimal, places: u32) -> Result<Decimal, Error> {
        if o.is_zero() {
            return Err(Error::DivideByZero);
        }
        // (a × 10^(places + 1 + o.scale)) / (b × 10^self.scale), then rounded once.
        let a = shift_up(&self.digits, places + 1 + o.scale);
        let b = shift_up(&o.digits, self.scale);
        let (q, r) = divmod_mag(&a, &b);
        // A remainder left over: a 1 below the guard digit, so half to even
        // sees the quotient is past the half.
        let (digits, scale) = if is_zero(&r) { (q, places + 1) } else { (add_mag(&shift_up(&q, 1), &[1]), places + 2) };
        let raw = Decimal { neg: self.neg != o.neg, digits, scale };
        Ok(raw.round_places(places))
    }

    /// `self / o` to `WORK` places, or more for a small quotient: always
    /// `SHOWN + 20` significant digits at least.
    pub fn div(&self, o: &Decimal) -> Result<Decimal, Error> {
        if o.is_zero() {
            return Err(Error::DivideByZero);
        }
        let qe = self.exponent() - o.exponent();
        let places = (WORK as i64).max(SHOWN as i64 + 20 - qe).min(2 * MAX_EXP + 100);
        self.div_to(o, places as u32)
    }

    pub fn compare(&self, o: &Decimal) -> Ordering {
        let d = self.sub(o);
        if d.is_zero() {
            Ordering::Equal
        } else if d.neg {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }

    /// `self` × 10^`n` (exact).
    pub fn times_pow10(&self, n: i64) -> Result<Decimal, Error> {
        if n.unsigned_abs() > 100_000 {
            return Err(Error::Overflow);
        }
        Ok(if n >= 0 {
            let up = (n as u32).min(self.scale);
            let mut d = Decimal { scale: self.scale - up, ..self.clone() };
            d.digits = shift_up(&d.digits, n as u32 - up);
            d.normal()
        } else {
            Decimal { scale: self.scale + (-n) as u32, ..self.clone() }.normal()
        })
    }

    /// Rounded to `places` after the point, half to even.
    pub fn round_places(&self, places: u32) -> Decimal {
        if self.scale <= places {
            return self.clone().normal();
        }
        let cut = (self.scale - places) as usize;
        let mut kept: Vec<u8> = self.digits.get(cut..).map(|s| s.to_vec()).unwrap_or_else(|| alloc::vec![0]);
        if kept.is_empty() {
            kept.push(0);
        }
        let first = self.digits.get(cut - 1).copied().unwrap_or(0);
        let rest_zero = self.digits[..(cut - 1).min(self.digits.len())].iter().all(|&x| x == 0);
        let up = first > 5 || (first == 5 && (!rest_zero || kept[0] % 2 == 1));
        if up {
            kept = add_mag(&kept, &[1]);
        }
        Decimal { neg: self.neg, digits: kept, scale: places }.normal()
    }

    /// The power of ten of its first significant digit (0.05 → -2, 123 → 2).
    pub fn exponent(&self) -> i64 {
        let n = self.clone().normal();
        let top = n.digits.iter().rposition(|&x| x != 0).unwrap_or(0) as i64;
        top - n.scale as i64
    }

    /// Rounded to `sig` significant digits.
    pub fn round_sig(&self, sig: usize) -> Decimal {
        if self.is_zero() {
            return Decimal::zero();
        }
        let e = self.exponent();
        let places = sig as i64 - 1 - e;
        if places >= 0 {
            self.round_places(places as u32)
        } else {
            // Rounded to tens, hundreds …: down, rounded, back up.
            let shifted = Decimal { scale: self.scale + (-places) as u32, ..self.clone() };
            let r = shifted.round_places(0);
            r.times_pow10(-places).unwrap_or(r)
        }
    }

    /// An integer's value (None past i64, or with a fraction).
    pub fn to_i64(&self) -> Option<i64> {
        let n = self.clone().normal();
        if n.scale != 0 || n.digits.len() > 18 {
            return None;
        }
        let v = n.digits.iter().rev().fold(0i64, |a, &d| a * 10 + d as i64);
        Some(if n.neg { -v } else { v })
    }

    /// As shown: `SHOWN` significant digits; plain when it fits, else
    /// scientific (`1.25e+40`). Overflow past `MAX_EXP`.
    pub fn show(&self) -> Result<String, Error> {
        let r = self.round_sig(SHOWN);
        let e = r.exponent();
        if e > MAX_EXP {
            return Err(Error::Overflow);
        }
        if e < -MAX_EXP || r.is_zero() {
            return Ok(String::from("0"));
        }
        let mut out = String::new();
        if r.neg {
            out.push('-');
        }
        let digits: String = r.digits.iter().rev().map(|&d| (b'0' + d) as char).collect();
        if (-7..SHOWN as i64).contains(&e) {
            // Plain: the point placed by the scale.
            let s = r.scale as usize;
            if s == 0 {
                out.push_str(&digits);
            } else if digits.len() > s {
                out.push_str(&digits[..digits.len() - s]);
                out.push('.');
                out.push_str(&digits[digits.len() - s..]);
            } else {
                out.push_str("0.");
                for _ in 0..s - digits.len() {
                    out.push('0');
                }
                out.push_str(&digits);
            }
        } else {
            let sig = digits.trim_end_matches('0');
            let sig = if sig.is_empty() { "0" } else { sig };
            out.push_str(&sig[..1]);
            if sig.len() > 1 {
                out.push('.');
                out.push_str(&sig[1..]);
            }
            out.push_str(&alloc::format!("e{}{}", if e < 0 { '-' } else { '+' }, e.abs()));
        }
        Ok(out)
    }
}

impl PartialEq for Decimal {
    fn eq(&self, o: &Decimal) -> bool {
        self.compare(o) == Ordering::Equal
    }
}

// ── Functions ──────────────────────────────────────────────────────────────

fn d(s: &str) -> Decimal {
    Decimal::parse(s).unwrap_or_default()
}

/// π to 60 places.
pub fn pi() -> Decimal {
    d("3.141592653589793238462643383279502884197169399375105820974944")
}

/// e to 60 places.
pub fn e() -> Decimal {
    d("2.718281828459045235360287471352662497757247093699959574966967")
}

impl Decimal {
    /// √ by Newton's method, to `WORK` places.
    pub fn sqrt(&self) -> Result<Decimal, Error> {
        if self.is_negative() {
            return Err(Error::Domain);
        }
        if self.is_zero() {
            return Ok(Decimal::zero());
        }
        let two = Decimal::from_i64(2);
        // A start near the root: 10^(exponent / 2).
        let mut x = Decimal::from_i64(1).times_pow10(self.exponent() / 2)?;
        for _ in 0..200 {
            let next = x.add(&self.div_to(&x, WORK + 5)?).div_to(&two, WORK + 5)?;
            if next.sub(&x).abs().round_places(WORK + 2).is_zero() {
                return Ok(next.round_places(WORK));
            }
            x = next;
        }
        Ok(x.round_places(WORK))
    }

    /// `self` to an integer power (exact for positive powers).
    pub fn powi(&self, n: i64) -> Result<Decimal, Error> {
        if n.unsigned_abs() > 10_000 {
            return Err(Error::Overflow);
        }
        if n < 0 {
            return Decimal::from_i64(1).div(&self.powi(-n)?);
        }
        let (mut base, mut out, mut k) = (self.clone(), Decimal::from_i64(1), n as u64);
        while k > 0 {
            if k & 1 == 1 {
                out = out.mul(&base);
                if out.digits.len() > LONG {
                    out = out.round_sig(WORK as usize + 20);
                }
            }
            k >>= 1;
            if k > 0 {
                base = base.mul(&base);
                if base.digits.len() > LONG {
                    base = base.round_sig(WORK as usize + 20);
                }
            }
            if out.exponent() > MAX_EXP || base.exponent() > 2 * MAX_EXP {
                return Err(Error::Overflow);
            }
        }
        Ok(out)
    }

    /// e^self, by halving and the series.
    pub fn exp(&self) -> Result<Decimal, Error> {
        if self.compare(&Decimal::from_i64(23_000)) == Ordering::Greater {
            return Err(Error::Overflow);
        }
        if self.compare(&Decimal::from_i64(-23_000)) == Ordering::Less {
            return Ok(Decimal::zero());
        }
        if self.is_negative() {
            return Decimal::from_i64(1).div(&self.neg().exp()?);
        }
        // e^x = (e^(x/2^k))^(2^k), with x/2^k under 1.
        let mut k = 0;
        let mut x = self.clone();
        while x.compare(&Decimal::from_i64(1)) == Ordering::Greater {
            x = x.div_to(&Decimal::from_i64(2), WORK + 10)?;
            k += 1;
        }
        let (mut term, mut sum) = (Decimal::from_i64(1), Decimal::from_i64(1));
        for n in 1..200 {
            term = term.mul(&x).div_to(&Decimal::from_i64(n), WORK + 10)?;
            if term.round_places(WORK + 8).is_zero() {
                break;
            }
            sum = sum.add(&term);
        }
        for _ in 0..k {
            sum = sum.mul(&sum).round_sig(WORK as usize + 20);
        }
        Ok(sum.round_sig(WORK as usize))
    }

    /// The natural logarithm: ln(m × 10^k) = ln m + k ln 10, ln m by the
    /// atanh series.
    pub fn ln(&self) -> Result<Decimal, Error> {
        if self.is_negative() || self.is_zero() {
            return Err(Error::Domain);
        }
        let k = self.exponent();
        let m = self.times_pow10(-k)?; // 1 ≤ m < 10
        let ln_m = ln_near(&m)?;
        if k == 0 {
            return Ok(ln_m.round_places(WORK));
        }
        let ln10 = ln_near(&Decimal::from_i64(10))?;
        Ok(ln_m.add(&ln10.mul(&Decimal::from_i64(k))).round_places(WORK))
    }

    pub fn log10(&self) -> Result<Decimal, Error> {
        // An exact power of ten stays exact.
        let k = self.exponent();
        if !self.is_negative() && *self == Decimal::from_i64(1).times_pow10(k)? {
            return Ok(Decimal::from_i64(k));
        }
        self.ln()?.div(&Decimal::from_i64(10).ln()?)
    }

    /// sin, in radians: reduced by 2π, then the series.
    pub fn sin(&self) -> Result<Decimal, Error> {
        let x = reduce(self)?;
        let x2 = x.mul(&x).round_places(WORK + 10);
        let (mut term, mut sum) = (x.clone(), x.clone());
        for n in 1..200i64 {
            term = term.mul(&x2).div_to(&Decimal::from_i64((2 * n) * (2 * n + 1)), WORK + 10)?.neg();
            if term.round_places(WORK + 8).is_zero() {
                break;
            }
            sum = sum.add(&term);
        }
        Ok(sum.round_places(WORK))
    }

    pub fn cos(&self) -> Result<Decimal, Error> {
        let half_pi = pi().div_to(&Decimal::from_i64(2), WORK + 10)?;
        half_pi.sub(self).sin()
    }

    pub fn tan(&self) -> Result<Decimal, Error> {
        let c = self.cos()?;
        if c.round_places(WORK - 10).is_zero() {
            return Err(Error::Domain);
        }
        self.sin()?.div(&c)
    }
}

/// x reduced into (-π, π].
fn reduce(x: &Decimal) -> Result<Decimal, Error> {
    let two_pi = pi().mul(&Decimal::from_i64(2));
    let turns = x.div_to(&two_pi, 0)?;
    let mut r = x.sub(&turns.mul(&two_pi));
    if r.compare(&pi()) == Ordering::Greater {
        r = r.sub(&two_pi);
    }
    Ok(r)
}

/// ln m for m near 1–10: 2 atanh((m−1)/(m+1)).
fn ln_near(m: &Decimal) -> Result<Decimal, Error> {
    let one = Decimal::from_i64(1);
    let y = m.sub(&one).div_to(&m.add(&one), WORK + 10)?;
    let y2 = y.mul(&y).round_places(WORK + 10);
    let (mut power, mut sum) = (y.clone(), y.clone());
    for n in 1..2000i64 {
        power = power.mul(&y2).round_places(WORK + 10);
        let term = power.div_to(&Decimal::from_i64(2 * n + 1), WORK + 10)?;
        if term.round_places(WORK + 8).is_zero() {
            break;
        }
        sum = sum.add(&term);
    }
    Ok(sum.mul(&Decimal::from_i64(2)))
}

#[cfg(test)]
mod tests;
