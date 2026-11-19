// SPDX-License-Identifier: Apache-2.0
//! A sum as text (TASK-112): what the Calculator's keys build and its
//! keyboard types. `+ - × ÷ * /`, `^` (right to left), `!`, `%` (÷ 100),
//! brackets (the last ones may be left open), `π`/`pi`, `e`, `E` for
//! "× 10 to the" inside a number (`2.5E-3`), a number or bracket right
//! before another meaning × (`2π`, `3(4)`), and the functions `√`/`sqrt`,
//! `sin cos tan asin acos atan`, `ln log exp abs`.
//!
//! The usual order: `!` and `%`, then `^`, then a leading minus
//! (`-2^2` is -4), then × and ÷, then + and −.

use crate::{Decimal, Error, MAX_EXP, WORK, e, pi};
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Angle {
    Degrees,
    Radians,
}

#[derive(Clone, Debug)]
enum Tok {
    Num(Decimal),
    Op(char),
    Name(String),
}

/// The sum's value, or why it has none.
pub fn eval(src: &str, angle: Angle) -> Result<Decimal, Error> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return Err(Error::Syntax);
    }
    let mut p = Parser { toks, at: 0, angle };
    let v = p.sum()?;
    if p.at != p.toks.len() {
        return Err(Error::Syntax);
    }
    Ok(v)
}

/// The sum shown as the Calculator shows it.
pub fn show(src: &str, angle: Angle) -> Result<String, Error> {
    eval(src, angle)?.show()
}

fn lex(src: &str) -> Result<Vec<Tok>, Error> {
    let cs: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                i += 1;
            }
            // "E", a sign, digits: the power of ten.
            if i < cs.len() && cs[i] == 'E' {
                let mut j = i + 1;
                if j < cs.len() && matches!(cs[j], '+' | '-' | '−') {
                    j += 1;
                }
                if j < cs.len() && cs[j].is_ascii_digit() {
                    while j < cs.len() && cs[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                } else {
                    return Err(Error::Syntax);
                }
            }
            let text: String = cs[start..i].iter().map(|&c| if c == '−' { '-' } else { c }).collect();
            out.push(Tok::Num(Decimal::parse(&text)?));
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < cs.len() && cs[i].is_ascii_alphabetic() {
                i += 1;
            }
            out.push(Tok::Name(cs[start..i].iter().collect::<String>().to_ascii_lowercase()));
        } else {
            i += 1;
            out.push(match c {
                '+' | '-' | '*' | '/' | '^' | '!' | '%' | '(' | ')' => Tok::Op(c),
                '×' | '·' => Tok::Op('*'),
                '÷' => Tok::Op('/'),
                '−' => Tok::Op('-'),
                'π' => Tok::Name(String::from("pi")),
                '√' => Tok::Name(String::from("sqrt")),
                _ => return Err(Error::Syntax),
            });
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
    angle: Angle,
}

impl Parser {
    fn peek_op(&self) -> Option<char> {
        match self.toks.get(self.at) {
            Some(Tok::Op(c)) => Some(*c),
            _ => None,
        }
    }

    fn take_op(&mut self, c: char) -> bool {
        if self.peek_op() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// Does a value start here (for × left out: `2π`, `3(4)`)?
    fn starts_value(&self) -> bool {
        matches!(self.toks.get(self.at), Some(Tok::Num(_) | Tok::Name(_) | Tok::Op('(')))
    }

    fn sum(&mut self) -> Result<Decimal, Error> {
        let mut v = self.term()?;
        loop {
            if self.take_op('+') {
                v = fit(v.add(&self.term()?))?;
            } else if self.take_op('-') {
                v = fit(v.sub(&self.term()?))?;
            } else {
                return Ok(v);
            }
        }
    }

    fn term(&mut self) -> Result<Decimal, Error> {
        let mut v = self.unary()?;
        loop {
            if self.take_op('*') {
                v = fit(v.mul(&self.unary()?))?;
            } else if self.take_op('/') {
                v = fit(v.div(&self.unary()?)?)?;
            } else if self.starts_value() {
                v = fit(v.mul(&self.power()?))?;
            } else {
                return Ok(v);
            }
        }
    }

    fn unary(&mut self) -> Result<Decimal, Error> {
        if self.take_op('-') {
            Ok(self.unary()?.neg())
        } else if self.take_op('+') {
            self.unary()
        } else {
            self.power()
        }
    }

    fn power(&mut self) -> Result<Decimal, Error> {
        let base = self.postfix()?;
        if self.take_op('^') {
            let exp = self.unary()?;
            return fit(pow(&base, &exp)?);
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<Decimal, Error> {
        let mut v = self.primary()?;
        loop {
            if self.take_op('!') {
                v = factorial(&v)?;
            } else if self.take_op('%') {
                v = v.times_pow10(-2)?;
            } else {
                return Ok(v);
            }
        }
    }

    fn primary(&mut self) -> Result<Decimal, Error> {
        let Some(t) = self.toks.get(self.at).cloned() else {
            return Err(Error::Syntax);
        };
        self.at += 1;
        match t {
            Tok::Num(n) => Ok(n),
            Tok::Op('(') => self.bracketed(),
            Tok::Op(_) => Err(Error::Syntax),
            Tok::Name(name) => match name.as_str() {
                "pi" => Ok(pi()),
                "e" => Ok(e()),
                _ => {
                    // f(x), or f x (`√4`, `sin 30`).
                    let x = if self.take_op('(') { self.bracketed()? } else { self.postfix()? };
                    fit(self.apply(&name, &x)?)
                }
            },
        }
    }

    /// After a `(`: the sum, and its `)` (which may be left off at the end).
    fn bracketed(&mut self) -> Result<Decimal, Error> {
        let v = self.sum()?;
        if !self.take_op(')') && self.at != self.toks.len() {
            return Err(Error::Syntax);
        }
        Ok(v)
    }

    fn apply(&self, name: &str, x: &Decimal) -> Result<Decimal, Error> {
        match name {
            "sqrt" => x.sqrt(),
            "sin" => self.sin(x),
            "cos" => self.cos(x),
            "tan" => {
                let c = self.cos(x)?;
                if c.round_places(WORK - 10).is_zero() {
                    return Err(Error::Domain);
                }
                self.sin(x)?.div(&c)
            }
            "asin" => self.in_angle_units(asin(x)?),
            "acos" => self.in_angle_units(acos(x)?),
            "atan" => self.in_angle_units(atan(x)?),
            "ln" => x.ln(),
            "log" => x.log10(),
            "exp" => x.exp(),
            "abs" => Ok(x.abs()),
            _ => Err(Error::Syntax),
        }
    }

    fn sin(&self, x: &Decimal) -> Result<Decimal, Error> {
        match self.angle {
            Angle::Radians => {
                if x.exponent() > 25 {
                    return Err(Error::Domain);
                }
                x.sin()
            }
            Angle::Degrees => sin_degrees(x),
        }
    }

    fn cos(&self, x: &Decimal) -> Result<Decimal, Error> {
        match self.angle {
            Angle::Radians => {
                if x.exponent() > 25 {
                    return Err(Error::Domain);
                }
                x.cos()
            }
            // cos x = sin(90 − x), exactly.
            Angle::Degrees => sin_degrees(&Decimal::from_i64(90).sub(x)),
        }
    }

    fn in_angle_units(&self, r: Decimal) -> Result<Decimal, Error> {
        match self.angle {
            Angle::Radians => Ok(r),
            Angle::Degrees => r.mul(&Decimal::from_i64(180)).div(&pi()),
        }
    }
}

/// Too big to hold: Overflow.
fn fit(v: Decimal) -> Result<Decimal, Error> {
    if v.exponent() > MAX_EXP + 1 { Err(Error::Overflow) } else { Ok(v) }
}

/// sin of degrees: the angle brought into (−180, 180] exactly, so the
/// whole and half answers (0, ±½, ±1) come out exact.
fn sin_degrees(x: &Decimal) -> Result<Decimal, Error> {
    if x.exponent() > 40 {
        return Err(Error::Domain);
    }
    let full = Decimal::from_i64(360);
    let turns = x.div_to(&full, 0)?;
    let mut r = x.sub(&turns.mul(&full));
    if r.compare(&Decimal::from_i64(180)) == Ordering::Greater {
        r = r.sub(&full);
    }
    if r.compare(&Decimal::from_i64(-180)) != Ordering::Greater {
        r = r.add(&full);
    }
    if let Some(n) = r.to_i64() {
        let exact = match n {
            0 | 180 => Some(0),
            90 => Some(2),
            -90 => Some(-2),
            30 | 150 => Some(1),
            -30 | -150 => Some(-1),
            _ => None,
        };
        if let Some(halves) = exact {
            return Decimal::from_i64(halves).div(&Decimal::from_i64(2));
        }
    }
    r.mul(&pi()).div_to(&Decimal::from_i64(180), WORK + 10)?.sin()
}

/// atan: halved (atan x = 2 atan(x / (1 + √(1 + x²)))) until small, then the series.
fn atan(x: &Decimal) -> Result<Decimal, Error> {
    let one = Decimal::from_i64(1);
    let small = Decimal::parse("0.1")?;
    let mut y = x.clone();
    let mut doublings = 0;
    while y.abs().compare(&small) == Ordering::Greater {
        y = y.div_to(&one.add(&one.add(&y.mul(&y)).sqrt()?), WORK + 10)?;
        doublings += 1;
    }
    let y2 = y.mul(&y).round_places(WORK + 10);
    let (mut power, mut sum) = (y.clone(), y.clone());
    for n in 1..400i64 {
        power = power.mul(&y2).round_places(WORK + 10).neg();
        let term = power.div_to(&Decimal::from_i64(2 * n + 1), WORK + 10)?;
        if term.round_places(WORK + 8).is_zero() {
            break;
        }
        sum = sum.add(&term);
    }
    for _ in 0..doublings {
        sum = sum.mul(&Decimal::from_i64(2));
    }
    Ok(sum.round_places(WORK))
}

fn asin(x: &Decimal) -> Result<Decimal, Error> {
    let one = Decimal::from_i64(1);
    match x.abs().compare(&one) {
        Ordering::Greater => Err(Error::Domain),
        Ordering::Equal => {
            let half_pi = pi().div(&Decimal::from_i64(2))?;
            Ok(if x.is_negative() { half_pi.neg() } else { half_pi })
        }
        // asin x = atan(x / √(1 − x²)).
        Ordering::Less => atan(&x.div_to(&one.sub(&x.mul(x)).sqrt()?, WORK + 10)?),
    }
}

fn acos(x: &Decimal) -> Result<Decimal, Error> {
    let a = asin(x)?;
    Ok(pi().div(&Decimal::from_i64(2))?.sub(&a).round_places(WORK))
}

/// x^y: exact for whole y; e^(y ln x) otherwise.
fn pow(x: &Decimal, y: &Decimal) -> Result<Decimal, Error> {
    if let Some(n) = y.to_i64()
        && n.unsigned_abs() <= 10_000
    {
        if x.is_zero() && n < 0 {
            return Err(Error::DivideByZero);
        }
        return x.powi(n);
    }
    if x.is_zero() {
        return if y.is_negative() { Err(Error::DivideByZero) } else { Ok(Decimal::zero()) };
    }
    let whole = y.is_integer();
    if x.is_negative() && !whole {
        return Err(Error::Domain);
    }
    let v = y.mul(&x.abs().ln()?).exp()?;
    // A negative x to an odd whole power stays negative.
    let odd = whole && y.round_places(0).abs().div_to(&Decimal::from_i64(2), 0)?.mul(&Decimal::from_i64(2)) != y.abs();
    Ok(if x.is_negative() && odd { v.neg() } else { v })
}

fn factorial(x: &Decimal) -> Result<Decimal, Error> {
    let Some(n) = x.to_i64().filter(|n| *n >= 0) else {
        return Err(Error::Domain);
    };
    if n > 3_000 {
        return Err(Error::Overflow);
    }
    let mut v = Decimal::from_i64(1);
    for k in 2..=n {
        v = v.mul(&Decimal::from_i64(k));
    }
    Ok(v)
}
