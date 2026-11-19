// SPDX-License-Identifier: Apache-2.0
//! Poly1305 (RFC 8439 § 2.5): the one-time authenticator, in five 26-bit
//! limbs with 64-bit products (no branch or table depends on the key or the
//! message; the final reduction is a masked select).

/// A running Poly1305 under one 32-byte one-time key.
pub struct Poly1305 {
    r: [u32; 5],
    h: [u32; 5],
    pad: [u32; 4],
    buf: [u8; 16],
    buf_len: usize,
}

fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Poly1305 {
        // r clamped (§ 2.5): the top four bits of each word and the bottom
        // two bits of the last three words cleared.
        let r = [
            le32(key, 0) & 0x03ff_ffff,
            (le32(key, 3) >> 2) & 0x03ff_ff03,
            (le32(key, 6) >> 4) & 0x03ff_c0ff,
            (le32(key, 9) >> 6) & 0x03f0_3fff,
            (le32(key, 12) >> 8) & 0x000f_ffff,
        ];
        let pad = [le32(key, 16), le32(key, 20), le32(key, 24), le32(key, 28)];
        Poly1305 { r, h: [0; 5], pad, buf: [0; 16], buf_len: 0 }
    }

    /// One 16-byte block; `hibit` is 2^128 for a full block, the 0x01 byte
    /// placed by hand for the last partial one.
    fn block(&mut self, m: &[u8; 16], hibit: u32) {
        let [r0, r1, r2, r3, r4] = self.r.map(|x| x as u64);
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let h = &mut self.h;
        h[0] += le32(m, 0) & 0x03ff_ffff;
        h[1] += (le32(m, 3) >> 2) & 0x03ff_ffff;
        h[2] += (le32(m, 6) >> 4) & 0x03ff_ffff;
        h[3] += (le32(m, 9) >> 6) & 0x03ff_ffff;
        h[4] += (le32(m, 12) >> 8) | hibit;
        let [h0, h1, h2, h3, h4] = h.map(|x| x as u64);
        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;
        let mut c = d0 >> 26;
        h[0] = (d0 as u32) & 0x03ff_ffff;
        d1 += c;
        c = d1 >> 26;
        h[1] = (d1 as u32) & 0x03ff_ffff;
        d2 += c;
        c = d2 >> 26;
        h[2] = (d2 as u32) & 0x03ff_ffff;
        d3 += c;
        c = d3 >> 26;
        h[3] = (d3 as u32) & 0x03ff_ffff;
        d4 += c;
        c = d4 >> 26;
        h[4] = (d4 as u32) & 0x03ff_ffff;
        h[0] += (c * 5) as u32;
        let c = h[0] >> 26;
        h[0] &= 0x03ff_ffff;
        h[1] += c;
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.buf_len > 0 {
            let take = (16 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < 16 {
                return;
            }
            let b = self.buf;
            self.block(&b, 1 << 24);
            self.buf_len = 0;
        }
        let (blocks, rest) = data.as_chunks::<16>();
        for b in blocks {
            self.block(b, 1 << 24);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    pub fn finish(mut self) -> [u8; 16] {
        if self.buf_len > 0 {
            let mut last = [0u8; 16];
            last[..self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);
            last[self.buf_len] = 1;
            self.block(&last, 0);
        }
        let h = &mut self.h;
        // Carry fully.
        let mut c = h[1] >> 26;
        h[1] &= 0x03ff_ffff;
        for limb in &mut h[2..5] {
            *limb += c;
            c = *limb >> 26;
            *limb &= 0x03ff_ffff;
        }
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= 0x03ff_ffff;
        h[1] += c;
        // h − p, kept if it didn't borrow (a masked select, no branch).
        let mut g = [0u32; 5];
        let mut c = 5u32;
        for i in 0..4 {
            g[i] = h[i].wrapping_add(c);
            c = g[i] >> 26;
            g[i] &= 0x03ff_ffff;
        }
        g[4] = h[4].wrapping_add(c).wrapping_sub(1 << 26);
        let keep_g = (g[4] >> 31).wrapping_sub(1); // all ones when g[4] didn't go negative
        for i in 0..5 {
            h[i] = (h[i] & !keep_g) | (g[i] & keep_g);
        }
        // h mod 2^128, plus the pad.
        let w0 = h[0] | (h[1] << 26);
        let w1 = (h[1] >> 6) | (h[2] << 20);
        let w2 = (h[2] >> 12) | (h[3] << 14);
        let w3 = (h[3] >> 18) | (h[4] << 8);
        let mut out = [0u8; 16];
        let mut f: u64 = w0 as u64 + self.pad[0] as u64;
        out[0..4].copy_from_slice(&(f as u32).to_le_bytes());
        for (i, w) in [w1, w2, w3].into_iter().enumerate() {
            f = (f >> 32) + w as u64 + self.pad[i + 1] as u64;
            out[4 * (i + 1)..4 * (i + 2)].copy_from_slice(&(f as u32).to_le_bytes());
        }
        out
    }
}

/// Poly1305 of `msg` under the one-time `key`.
pub fn mac(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    let mut p = Poly1305::new(key);
    p.update(msg);
    p.finish()
}
