// SPDX-License-Identifier: Apache-2.0
//! fedid: reads a display's EDID (VESA E-EDID 1.3 and 1.4, the 128-byte base
//! block), with no allocation, so the bootloader can use it before it has a
//! heap (TASK-071).
//!
//! - **The preferred mode:** the first detailed timing descriptor, which
//!   E-EDID 1.4 always makes the panel's native mode (1.3 when its feature
//!   byte says so).
//! - **The size:** the image size of that descriptor in millimetres, else the
//!   base block's centimetres.
//! - **The name:** the display product name descriptor (tag `0xFC`).
//! - **The UI scale that suits it** ([`ui_scale_pct`]): about 125 dots per
//!   inch to 100 % on a laptop-sized panel, 108 on a desk monitor (it's
//!   farther away), in steps of 25 % from 100 to 300 %. A 16-inch
//!   2560×1600 panel comes out at 150 %, a 24-inch 1080p monitor at 100 %.
//!
//! Every block's checksum is checked; a block that's wrong is refused, never
//! half-read.
#![no_std]

/// What a display's EDID says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edid {
    /// The three-letter PNP manufacturer ID (`b"BOE"`, `b"AUO"`, …).
    pub manufacturer: [u8; 3],
    pub product: u16,
    /// E-EDID version and revision (1, 4).
    pub version: (u8, u8),
    /// The native mode: width, height, refresh in millihertz.
    pub preferred: Option<Mode>,
    /// The visible picture's size in millimetres, when the display says.
    pub size_mm: Option<(u32, u32)>,
    /// The display product name (up to 13 characters), `name_len` of them.
    name: [u8; 13],
    name_len: u8,
}

/// A mode from a detailed timing descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub refresh_mhz: u32,
}

impl Edid {
    /// The display's product name, if it gives one.
    pub fn name(&self) -> Option<&str> {
        if self.name_len == 0 {
            return None;
        }
        core::str::from_utf8(&self.name[..self.name_len as usize]).ok()
    }

    /// The UI scale for this display at its preferred mode (100 when it
    /// gives no size).
    pub fn ui_scale_pct(&self) -> u16 {
        match (self.preferred, self.size_mm) {
            (Some(m), Some((w, h))) => ui_scale_pct(m.w, w, h),
            _ => 100,
        }
    }
}

const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// Why an EDID was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    TooShort,
    NoHeader,
    Checksum,
}

/// Reads the base block of `b`.
pub fn parse(b: &[u8]) -> Result<Edid, Refused> {
    if b.len() < 128 {
        return Err(Refused::TooShort);
    }
    let b = &b[..128];
    if b[..8] != HEADER {
        return Err(Refused::NoHeader);
    }
    if b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) != 0 {
        return Err(Refused::Checksum);
    }
    let m = u16::from_be_bytes([b[8], b[9]]);
    let letter = |shift: u16| b'A' - 1 + ((m >> shift) & 0x1F) as u8;
    let manufacturer = [letter(10), letter(5), letter(0)];
    let version = (b[18], b[19]);
    // E-EDID 1.4: the first descriptor is always the native mode; 1.3 says so
    // in bit 1 of the feature byte.
    let first_is_native = version >= (1, 4) || b[24] & 0x02 != 0;
    let mut e = Edid {
        manufacturer,
        product: u16::from_le_bytes([b[10], b[11]]),
        version,
        preferred: None,
        size_mm: None,
        name: [0; 13],
        name_len: 0,
    };
    let mut dtd_size = None;
    for (i, d) in b[54..126].chunks_exact(18).enumerate() {
        let clock = u16::from_le_bytes([d[0], d[1]]) as u32;
        if clock == 0 {
            if d[3] == 0xFC {
                let text = &d[5..18];
                let n = text.iter().position(|&c| c == 0x0A).unwrap_or(13);
                let n = text[..n].iter().rposition(|&c| c != b' ').map_or(0, |p| p + 1);
                if text[..n].iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
                    e.name[..n].copy_from_slice(&text[..n]);
                    e.name_len = n as u8;
                }
            }
            continue;
        }
        let w = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
        let hblank = d[3] as u32 | ((d[4] as u32 & 0xF) << 8);
        let h = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
        let vblank = d[6] as u32 | ((d[7] as u32 & 0xF) << 8);
        let wmm = d[12] as u32 | ((d[14] as u32 >> 4) << 8);
        let hmm = d[13] as u32 | ((d[14] as u32 & 0xF) << 8);
        if i == 0 && first_is_native && w > 0 && h > 0 {
            let total = (w + hblank) as u64 * (h + vblank) as u64;
            let refresh_mhz = if total == 0 { 0 } else { (clock as u64 * 10_000 * 1000 / total) as u32 };
            e.preferred = Some(Mode { w, h, refresh_mhz });
            if wmm > 0 && hmm > 0 {
                dtd_size = Some((wmm, hmm));
            }
        }
    }
    // The descriptor's millimetres are the finer measure; the base block's
    // centimetres are the fallback (0 in either means "not given").
    e.size_mm = dtd_size.or(if b[21] > 0 && b[22] > 0 { Some((b[21] as u32 * 10, b[22] as u32 * 10)) } else { None });
    Ok(e)
}

/// The UI scale to start at (stage, the probe): from the EDID's size, at
/// the width the screen is actually in (the panel's preferred mode, unless
/// the firmware didn't offer it). No EDID, a damaged one, or one with no
/// size: 100 %.
pub fn scale_for(edid: &[u8], screen_w: u32) -> u16 {
    match parse(edid).ok().and_then(|e| e.size_mm) {
        Some((w, h)) => ui_scale_pct(screen_w, w, h),
        None => 100,
    }
}

/// The UI scale (100–300 %, steps of 25) for a panel `w_px` wide whose
/// picture is `w_mm` × `h_mm`. A panel under 19 inches across is a laptop's,
/// seen from closer: 125 dots per inch make 100 %. Bigger ones: 108.
pub fn ui_scale_pct(w_px: u32, w_mm: u32, h_mm: u32) -> u16 {
    if w_px == 0 || w_mm == 0 {
        return 100;
    }
    // Dots per inch × 10, and the diagonal against 19 inches (482.6 mm), squared.
    let dpi_x10 = w_px as u64 * 254 / w_mm as u64;
    let laptop = (w_mm as u64).pow(2) + (h_mm as u64).pow(2) < 482 * 482;
    let base = if laptop { 125 } else { 108 };
    let pct = dpi_x10 * 10 / base;
    (((pct + 12) / 25) * 25).clamp(100, 300) as u16
}

/// The GOP mode to use: the display's preferred one if the firmware offers
/// it; else the largest offered at the same shape that isn't bigger; else the
/// one already set (`current`). `modes` are (width, height), by mode number;
/// `(0, 0)` stands for one that can't be used (never chosen).
pub fn pick_mode(modes: &[(u32, u32)], preferred: Option<(u32, u32)>, current: usize) -> usize {
    let Some((pw, ph)) = preferred else { return current };
    if pw == 0 || ph == 0 {
        return current;
    }
    if let Some(i) = modes.iter().position(|&m| m == (pw, ph)) {
        return i;
    }
    modes
        .iter()
        .enumerate()
        .filter(|(_, (w, h))| *w > 0 && *w <= pw && *h <= ph && (*w as u64) * ph as u64 == (*h as u64) * pw as u64)
        .max_by_key(|(_, (w, h))| *w as u64 * *h as u64)
        .map_or(current, |(i, _)| i)
}

/// An E-EDID 1.4 base block for a panel: its native mode at 60 Hz (reduced
/// blanking), its size, its name (≤ 13 characters). For tests and the VMs'
/// stand-in display (a VM's firmware gives no EDID): `xtask` writes it to
/// `\FREALLY\EDID.BIN`, which only dev and e2e bootloaders read.
pub fn synthesize(manufacturer: &[u8; 3], (w, h): (u32, u32), (wmm, hmm): (u32, u32), name: &str) -> [u8; 128] {
    let mut b = [0u8; 128];
    b[..8].copy_from_slice(&HEADER);
    let code = |c: u8| (c.clamp(b'A', b'Z') - b'A' + 1) as u16;
    let m = (code(manufacturer[0]) << 10) | (code(manufacturer[1]) << 5) | code(manufacturer[2]);
    b[8..10].copy_from_slice(&m.to_be_bytes());
    (b[18], b[19]) = (1, 4);
    (b[21], b[22]) = ((wmm / 10).min(255) as u8, (hmm / 10).min(255) as u8);
    b[24] = 0x02;
    let (hblank, vblank) = (160u32, (h * 3 / 100).max(23));
    let clock = ((w + hblank) as u64 * (h + vblank) as u64 * 60 / 10_000).min(u16::MAX as u64) as u16;
    let d = &mut b[54..72];
    d[..2].copy_from_slice(&clock.to_le_bytes());
    d[2] = w as u8;
    d[3] = hblank as u8;
    d[4] = (((w >> 8) as u8) << 4) | (hblank >> 8) as u8;
    d[5] = h as u8;
    d[6] = vblank as u8;
    d[7] = (((h >> 8) as u8) << 4) | (vblank >> 8) as u8;
    d[12] = wmm as u8;
    d[13] = hmm as u8;
    d[14] = (((wmm >> 8) as u8) << 4) | (hmm >> 8) as u8 & 0xF;
    let n = &mut b[72..90];
    n[3] = 0xFC;
    let text = name.as_bytes();
    let k = text.len().min(13);
    n[5..18].fill(b' ');
    n[5..5 + k].copy_from_slice(&text[..k]);
    if k < 13 {
        n[5 + k] = 0x0A;
    }
    let sum = b[..127].iter().fold(0u8, |a, &x| a.wrapping_add(x));
    b[127] = 0u8.wrapping_sub(sum);
    b
}

#[cfg(test)]
mod tests;
