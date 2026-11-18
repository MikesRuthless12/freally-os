// SPDX-License-Identifier: Apache-2.0
//! Formatting into a fixed buffer, for messages that must not allocate (the
//! panic screen, the exception path). Text past the end is dropped.

use core::fmt;

pub struct Buf<const N: usize> {
    b: [u8; N],
    n: usize,
}

impl<const N: usize> Default for Buf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Buf<N> {
    pub const fn new() -> Self {
        Buf { b: [0; N], n: 0 }
    }

    pub fn as_str(&self) -> &str {
        // Only whole UTF-8 sequences are ever copied in (see write_str).
        core::str::from_utf8(&self.b[..self.n]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            let mut tmp = [0u8; 4];
            let e = c.encode_utf8(&mut tmp).as_bytes();
            if self.n + e.len() > N {
                break;
            }
            self.b[self.n..self.n + e.len()].copy_from_slice(e);
            self.n += e.len();
        }
        Ok(())
    }
}
