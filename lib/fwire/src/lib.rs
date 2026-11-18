// SPDX-License-Identifier: Apache-2.0
//! fwire: how service messages are written on channels (PRD § 4).
//!
//! Every message is a 16-byte header, then the payload:
//! `txid u32, ordinal u32, flags u16 (bit0 reply, bit1 error, bit2 event),
//! payload_len u16, reserved u32` (little-endian; `reserved` is zero).
//!
//! Payloads are little-endian fixed-width integers; `bool` is one byte (0/1);
//! strings and lists carry a u16 length, then their bytes or items; `Option`
//! is a tag byte (0 none, 1 some) then the value; a handle travels in the
//! channel's handle list and the payload carries its index there (u8). Decoding
//! is strict: a wrong length, a stray byte, invalid UTF-8 or an unknown tag is
//! an error, never a panic.
//!
//! An error reply has `flags.error` and the payload `code i32, message String`.
#![no_std]

extern crate alloc;

pub mod proto;

use alloc::string::String;
use alloc::vec::Vec;

pub const HEADER_LEN: usize = 16;
/// The most payload one message can carry (the channel limit less the header).
pub const MAX_PAYLOAD: usize = 65_536 - HEADER_LEN;

pub mod flags {
    pub const REPLY: u16 = 1 << 0;
    pub const ERROR: u16 = 1 << 1;
    pub const EVENT: u16 = 1 << 2;
    pub const KNOWN: u16 = REPLY | ERROR | EVENT;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WireError {
    /// Shorter than its header or than what it says it holds.
    Truncated,
    /// Bytes left over after the payload's last field.
    TrailingBytes,
    /// `payload_len` disagrees with the message's length.
    BadLength,
    /// A string that isn't UTF-8.
    BadUtf8,
    /// An enum or option tag this side doesn't know.
    BadTag,
    /// A reserved field or flag bit that isn't zero.
    Reserved,
    /// More than `MAX_PAYLOAD` bytes, or a list longer than 65,535.
    TooLong,
}

impl WireError {
    pub fn describe(self) -> &'static str {
        match self {
            WireError::Truncated => "a message cut short",
            WireError::TrailingBytes => "bytes after the end of a message",
            WireError::BadLength => "a message whose length doesn't match its header",
            WireError::BadUtf8 => "a string that isn't UTF-8",
            WireError::BadTag => "an unknown value",
            WireError::Reserved => "a reserved field that isn't zero",
            WireError::TooLong => "a message too long to send",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    pub txid: u32,
    pub ordinal: u32,
    pub flags: u16,
    pub payload_len: u16,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.txid.to_le_bytes());
        b[4..8].copy_from_slice(&self.ordinal.to_le_bytes());
        b[8..10].copy_from_slice(&self.flags.to_le_bytes());
        b[10..12].copy_from_slice(&self.payload_len.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Result<Header, WireError> {
        let b: &[u8; HEADER_LEN] =
            b.get(..HEADER_LEN).ok_or(WireError::Truncated)?.try_into().map_err(|_| WireError::Truncated)?;
        let h = Header {
            txid: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            ordinal: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            flags: u16::from_le_bytes([b[8], b[9]]),
            payload_len: u16::from_le_bytes([b[10], b[11]]),
        };
        if b[12..16] != [0; 4] || h.flags & !flags::KNOWN != 0 {
            return Err(WireError::Reserved);
        }
        Ok(h)
    }

    pub fn is_reply(&self) -> bool {
        self.flags & flags::REPLY != 0
    }
    pub fn is_error(&self) -> bool {
        self.flags & flags::ERROR != 0
    }
    pub fn is_event(&self) -> bool {
        self.flags & flags::EVENT != 0
    }
}

/// Builds a payload.
#[derive(Default)]
pub struct Writer {
    pub bytes: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { bytes: Vec::new() }
    }
    pub fn raw(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
    }
}

/// Reads a payload, front to back.
pub struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, at: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(n).ok_or(WireError::Truncated)?;
        let s = self.b.get(self.at..end).ok_or(WireError::Truncated)?;
        self.at = end;
        Ok(s)
    }
    pub fn finish(&self) -> Result<(), WireError> {
        if self.at == self.b.len() { Ok(()) } else { Err(WireError::TrailingBytes) }
    }
}

/// A type with a wire form.
pub trait Wire: Sized {
    fn put(&self, w: &mut Writer);
    fn get(r: &mut Reader) -> Result<Self, WireError>;
}

macro_rules! ints {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            fn put(&self, w: &mut Writer) {
                w.raw(&self.to_le_bytes());
            }
            fn get(r: &mut Reader) -> Result<Self, WireError> {
                let b = r.take(size_of::<$t>())?;
                Ok(<$t>::from_le_bytes(b.try_into().map_err(|_| WireError::Truncated)?))
            }
        }
    )*};
}
ints!(u8, u16, u32, u64, i32, i64);

impl Wire for bool {
    fn put(&self, w: &mut Writer) {
        w.raw(&[*self as u8]);
    }
    fn get(r: &mut Reader) -> Result<Self, WireError> {
        match u8::get(r)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::BadTag),
        }
    }
}

impl Wire for () {
    fn put(&self, _: &mut Writer) {}
    fn get(_: &mut Reader) -> Result<Self, WireError> {
        Ok(())
    }
}

impl Wire for String {
    fn put(&self, w: &mut Writer) {
        // Longer strings are cut at a character boundary (MAX is u16::MAX bytes).
        let mut n = self.len().min(u16::MAX as usize);
        while !self.is_char_boundary(n) {
            n -= 1;
        }
        (n as u16).put(w);
        w.raw(&self.as_bytes()[..n]);
    }
    fn get(r: &mut Reader) -> Result<Self, WireError> {
        let n = u16::get(r)? as usize;
        let b = r.take(n)?;
        core::str::from_utf8(b).map(String::from).map_err(|_| WireError::BadUtf8)
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, w: &mut Writer) {
        let n = self.len().min(u16::MAX as usize);
        (n as u16).put(w);
        for x in &self[..n] {
            x.put(w);
        }
    }
    fn get(r: &mut Reader) -> Result<Self, WireError> {
        let n = u16::get(r)? as usize;
        // Never trust a count to size an allocation: each item needs at least a byte,
        // except zero-sized ones, which are capped by the count itself.
        let mut v = Vec::with_capacity(n.min(r.b.len() - r.at));
        for _ in 0..n {
            v.push(T::get(r)?);
        }
        Ok(v)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, w: &mut Writer) {
        match self {
            None => 0u8.put(w),
            Some(x) => {
                1u8.put(w);
                x.put(w);
            }
        }
    }
    fn get(r: &mut Reader) -> Result<Self, WireError> {
        match u8::get(r)? {
            0 => Ok(None),
            1 => Ok(Some(T::get(r)?)),
            _ => Err(WireError::BadTag),
        }
    }
}

impl<const N: usize> Wire for [u8; N] {
    fn put(&self, w: &mut Writer) {
        w.raw(self);
    }
    fn get(r: &mut Reader) -> Result<Self, WireError> {
        r.take(N)?.try_into().map_err(|_| WireError::Truncated)
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
        self.1.put(w);
    }
    fn get(r: &mut Reader) -> Result<Self, WireError> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

/// A handle in the message's handle list, by index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HandleRef(pub u8);

impl Wire for HandleRef {
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
    }
    fn get(r: &mut Reader) -> Result<Self, WireError> {
        u8::get(r).map(HandleRef)
    }
}

/// A struct whose wire form is its fields in order.
#[macro_export]
macro_rules! wire_struct {
    ($(#[$m:meta])* pub struct $name:ident { $($(#[$fm:meta])* pub $f:ident: $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, PartialEq, Eq, Debug)]
        pub struct $name { $($(#[$fm])* pub $f: $t),* }
        impl $crate::Wire for $name {
            fn put(&self, w: &mut $crate::Writer) {
                $(self.$f.put(w);)*
            }
            fn get(r: &mut $crate::Reader) -> Result<Self, $crate::WireError> {
                Ok($name { $($f: <$t as $crate::Wire>::get(r)?),* })
            }
        }
    };
}

/// A fieldless enum carried as one byte.
#[macro_export]
macro_rules! wire_enum {
    ($(#[$m:meta])* pub enum $name:ident { $($(#[$vm:meta])* $v:ident = $n:expr),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u8)]
        pub enum $name { $($(#[$vm])* $v = $n),* }
        impl $crate::Wire for $name {
            fn put(&self, w: &mut $crate::Writer) {
                (*self as u8).put(w);
            }
            fn get(r: &mut $crate::Reader) -> Result<Self, $crate::WireError> {
                match <u8 as $crate::Wire>::get(r)? {
                    $(x if x == $n => Ok($name::$v),)*
                    _ => Err($crate::WireError::BadTag),
                }
            }
        }
    };
}

/// Encodes a value alone (the payload).
pub fn to_payload<T: Wire>(v: &T) -> Vec<u8> {
    let mut w = Writer::new();
    v.put(&mut w);
    w.bytes
}

/// Decodes a whole payload: every byte must belong to `T`.
pub fn from_payload<T: Wire>(b: &[u8]) -> Result<T, WireError> {
    let mut r = Reader::new(b);
    let v = T::get(&mut r)?;
    r.finish()?;
    Ok(v)
}

/// A whole message: the header for `payload`, then `payload`.
pub fn message_bytes(txid: u32, ordinal: u32, flags: u16, payload: Vec<u8>) -> Result<Vec<u8>, WireError> {
    message(txid, ordinal, flags, payload)
}

fn message(txid: u32, ordinal: u32, flags: u16, payload: Vec<u8>) -> Result<Vec<u8>, WireError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(WireError::TooLong);
    }
    let h = Header { txid, ordinal, flags, payload_len: payload.len() as u16 };
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&h.encode());
    out.extend_from_slice(&payload);
    Ok(out)
}

pub fn request<T: Wire>(txid: u32, ordinal: u32, body: &T) -> Result<Vec<u8>, WireError> {
    message(txid, ordinal, 0, to_payload(body))
}

pub fn reply<T: Wire>(txid: u32, ordinal: u32, body: &T) -> Result<Vec<u8>, WireError> {
    message(txid, ordinal, flags::REPLY, to_payload(body))
}

/// An error reply: the `FErr` code and a plain message.
pub fn error_reply(txid: u32, ordinal: u32, code: i32, msg: &str) -> Result<Vec<u8>, WireError> {
    message(txid, ordinal, flags::REPLY | flags::ERROR, to_payload(&(code, String::from(msg))))
}

pub fn event<T: Wire>(ordinal: u32, body: &T) -> Result<Vec<u8>, WireError> {
    message(0, ordinal, flags::EVENT, to_payload(body))
}

/// Splits a message into its header and payload, checking the length.
pub fn parse(msg: &[u8]) -> Result<(Header, &[u8]), WireError> {
    let h = Header::decode(msg)?;
    let payload = &msg[HEADER_LEN..];
    if payload.len() != h.payload_len as usize {
        return Err(WireError::BadLength);
    }
    Ok((h, payload))
}

/// An error reply's code and message.
pub fn error_of(payload: &[u8]) -> Result<(i32, String), WireError> {
    from_payload(payload)
}

#[cfg(test)]
mod tests;
