// SPDX-License-Identifier: Apache-2.0
//! A DER reader (ITU-T X.690 § 8, § 10): every length checked before a byte
//! is read; definite, minimally encoded lengths only (DER allows nothing
//! else); tags of one byte (all X.509 needs). It borrows from the input and
//! allocates nothing.

/// Why the bytes were refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    /// Ran out of bytes.
    Short,
    /// Not the tag expected here.
    Tag,
    /// A length DER doesn't allow (indefinite, not minimal, over 4 bytes).
    Length,
    /// Bytes left over where there should be none.
    Trailing,
    /// A value that isn't well formed (an integer, a time, a bit string).
    Value,
}

pub mod tag {
    pub const BOOLEAN: u8 = 0x01;
    pub const INTEGER: u8 = 0x02;
    pub const BIT_STRING: u8 = 0x03;
    pub const OCTET_STRING: u8 = 0x04;
    pub const NULL: u8 = 0x05;
    pub const OID: u8 = 0x06;
    pub const UTF8_STRING: u8 = 0x0C;
    pub const PRINTABLE_STRING: u8 = 0x13;
    pub const IA5_STRING: u8 = 0x16;
    pub const UTC_TIME: u8 = 0x17;
    pub const GENERALIZED_TIME: u8 = 0x18;
    pub const SEQUENCE: u8 = 0x30;
    pub const SET: u8 = 0x31;
    /// [n] constructed, context-specific.
    pub const fn explicit(n: u8) -> u8 {
        0xA0 | n
    }
    /// [n] primitive, context-specific.
    pub const fn implicit(n: u8) -> u8 {
        0x80 | n
    }
}

/// Reads one element after another from a slice.
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b }
    }

    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }

    /// The next element's tag, without reading it.
    pub fn peek(&self) -> Option<u8> {
        self.b.first().copied()
    }

    /// The next element, whatever its tag: (tag, contents, the whole element).
    pub fn any(&mut self) -> Result<(u8, &'a [u8], &'a [u8]), Bad> {
        let (&t, rest) = self.b.split_first().ok_or(Bad::Short)?;
        if t & 0x1F == 0x1F {
            return Err(Bad::Tag); // multi-byte tags: never in X.509
        }
        let (&first, mut rest) = rest.split_first().ok_or(Bad::Short)?;
        let len = if first < 0x80 {
            first as usize
        } else {
            let n = (first & 0x7F) as usize;
            if n == 0 || n > 4 || rest.len() < n {
                return Err(if n == 0 || n > 4 { Bad::Length } else { Bad::Short });
            }
            let mut len = 0usize;
            for &b in &rest[..n] {
                len = (len << 8) | b as usize;
            }
            // Minimal: no leading zero byte, and the long form only past 127.
            if rest[0] == 0 || len < 0x80 {
                return Err(Bad::Length);
            }
            rest = &rest[n..];
            len
        };
        if rest.len() < len {
            return Err(Bad::Short);
        }
        let header = self.b.len() - rest.len();
        let whole = &self.b[..header + len];
        let contents = &rest[..len];
        self.b = &rest[len..];
        Ok((t, contents, whole))
    }

    /// The next element, which must have tag `t`: its contents.
    pub fn read(&mut self, t: u8) -> Result<&'a [u8], Bad> {
        if self.peek() != Some(t) {
            return Err(if self.is_empty() { Bad::Short } else { Bad::Tag });
        }
        self.any().map(|(_, c, _)| c)
    }

    /// The next element if it has tag `t`.
    pub fn optional(&mut self, t: u8) -> Result<Option<&'a [u8]>, Bad> {
        if self.peek() == Some(t) { self.read(t).map(Some) } else { Ok(None) }
    }

    /// A SEQUENCE's contents, as a reader of its own.
    pub fn sequence(&mut self) -> Result<Reader<'a>, Bad> {
        self.read(tag::SEQUENCE).map(Reader::new)
    }

    /// Nothing may be left.
    pub fn end(&self) -> Result<(), Bad> {
        if self.b.is_empty() { Ok(()) } else { Err(Bad::Trailing) }
    }

    /// A non-negative INTEGER's magnitude, big-endian, no leading zero
    /// (DER's minimal encoding checked; a negative one refused).
    pub fn unsigned(&mut self) -> Result<&'a [u8], Bad> {
        let v = self.read(tag::INTEGER)?;
        match v {
            [] => Err(Bad::Value),
            [b, ..] if b & 0x80 != 0 => Err(Bad::Value),
            [0] => Ok(v),
            [0, b, ..] if b & 0x80 == 0 => Err(Bad::Value),
            [0, rest @ ..] => Ok(rest),
            _ => Ok(v),
        }
    }

    /// A small non-negative INTEGER (a version, a path length).
    pub fn small(&mut self) -> Result<u64, Bad> {
        let v = self.unsigned()?;
        if v.len() > 8 {
            return Err(Bad::Value);
        }
        Ok(v.iter().fold(0u64, |a, &b| (a << 8) | b as u64))
    }

    /// A BIT STRING with no unused bits (keys and signatures): its bytes.
    pub fn bits(&mut self) -> Result<&'a [u8], Bad> {
        match self.read(tag::BIT_STRING)? {
            [0, rest @ ..] => Ok(rest),
            _ => Err(Bad::Value),
        }
    }

    /// A UTCTime (YYMMDDHHMMSSZ; years 50–99 are 19xx) or GeneralizedTime
    /// (YYYYMMDDHHMMSSZ), as RFC 5280 § 4.1.2.5 allows them: Unix seconds.
    pub fn time(&mut self) -> Result<u64, Bad> {
        let (t, v, _) = self.any()?;
        let digits = |s: &[u8]| -> Result<u64, Bad> {
            s.iter().try_fold(
                0u64,
                |a, &c| if c.is_ascii_digit() { Ok(a * 10 + (c - b'0') as u64) } else { Err(Bad::Value) },
            )
        };
        let (year, rest) = match (t, v.len()) {
            (tag::UTC_TIME, 13) => {
                let yy = digits(&v[..2])?;
                (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &v[2..])
            }
            (tag::GENERALIZED_TIME, 15) => (digits(&v[..4])?, &v[4..]),
            (tag::UTC_TIME | tag::GENERALIZED_TIME, _) => return Err(Bad::Value),
            _ => return Err(Bad::Tag),
        };
        if rest[10] != b'Z' {
            return Err(Bad::Value);
        }
        let (mo, d, h, mi, s) = (
            digits(&rest[0..2])?,
            digits(&rest[2..4])?,
            digits(&rest[4..6])?,
            digits(&rest[6..8])?,
            digits(&rest[8..10])?,
        );
        if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 || year < 1970 {
            return Err(Bad::Value);
        }
        Ok(days_from_civil(year, mo, d) * 86_400 + h * 3600 + mi * 60 + s)
    }
}

/// Days since 1970-01-01 of a date on or after it (Howard Hinnant's algorithm).
pub fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
