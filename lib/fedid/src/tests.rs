// SPDX-License-Identifier: Apache-2.0
use super::*;

/// A base block: manufacturer, version, the size in cm, then the descriptors;
/// the checksum fixed last.
fn block(mfg: &[u8; 3], version: (u8, u8), cm: (u8, u8), feature: u8, descs: &[[u8; 18]]) -> [u8; 128] {
    let mut b = [0u8; 128];
    b[..8].copy_from_slice(&HEADER);
    let code = |c: u8| (c - b'A' + 1) as u16;
    let m = (code(mfg[0]) << 10) | (code(mfg[1]) << 5) | code(mfg[2]);
    b[8..10].copy_from_slice(&m.to_be_bytes());
    b[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
    (b[18], b[19]) = version;
    (b[21], b[22]) = cm;
    b[24] = feature;
    for (i, d) in descs.iter().enumerate() {
        b[54 + 18 * i..72 + 18 * i].copy_from_slice(d);
    }
    let sum = b[..127].iter().fold(0u8, |a, &x| a.wrapping_add(x));
    b[127] = 0u8.wrapping_sub(sum);
    b
}

/// A detailed timing descriptor.
fn dtd(clock_10khz: u16, w: u32, hblank: u32, h: u32, vblank: u32, mm: (u32, u32)) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[..2].copy_from_slice(&clock_10khz.to_le_bytes());
    d[2] = w as u8;
    d[3] = hblank as u8;
    d[4] = (((w >> 8) as u8) << 4) | (hblank >> 8) as u8;
    d[5] = h as u8;
    d[6] = vblank as u8;
    d[7] = (((h >> 8) as u8) << 4) | (vblank >> 8) as u8;
    d[12] = mm.0 as u8;
    d[13] = mm.1 as u8;
    d[14] = (((mm.0 >> 8) as u8) << 4) | (mm.1 >> 8) as u8;
    d
}

fn name(text: &[u8]) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[3] = 0xFC;
    let mut t = [b' '; 13];
    t[..text.len()].copy_from_slice(text);
    if text.len() < 13 {
        t[text.len()] = 0x0A;
    }
    d[5..].copy_from_slice(&t);
    d
}

/// A 16-inch 2560×1600 laptop panel at 60 Hz (CVT reduced blanking).
fn laptop() -> [u8; 128] {
    block(b"BOE", (1, 4), (34, 22), 0x02, &[dtd(26850, 2560, 160, 1600, 46, (344, 215)), name(b"Panel 16")])
}

#[test]
fn a_laptop_panel_reads_whole() {
    let e = parse(&laptop()).unwrap();
    assert_eq!(&e.manufacturer, b"BOE");
    assert_eq!(e.product, 0x1234);
    assert_eq!(e.version, (1, 4));
    assert_eq!(e.preferred, Some(Mode { w: 2560, h: 1600, refresh_mhz: 59_971 }));
    assert_eq!(e.size_mm, Some((344, 215)));
    assert_eq!(e.name(), Some("Panel 16"));
    assert_eq!(e.ui_scale_pct(), 150);
}

#[test]
fn a_damaged_block_is_refused() {
    let mut b = laptop();
    b[60] ^= 1;
    assert_eq!(parse(&b), Err(Refused::Checksum));
    let mut b = laptop();
    b[0] = 1;
    assert_eq!(parse(&b), Err(Refused::NoHeader));
    assert_eq!(parse(&laptop()[..127]), Err(Refused::TooShort));
}

#[test]
fn edid_1_3_without_the_preferred_bit_names_no_mode() {
    let b = block(b"ABC", (1, 3), (52, 32), 0x00, &[dtd(14850, 1920, 280, 1080, 45, (0, 0))]);
    let e = parse(&b).unwrap();
    assert_eq!(e.preferred, None);
    // No millimetres in the descriptor: the base block's centimetres.
    assert_eq!(e.size_mm, Some((520, 320)));
    assert_eq!(e.ui_scale_pct(), 100);
}

#[test]
fn no_size_means_100_percent() {
    let b = block(b"QEM", (1, 4), (0, 0), 0x02, &[dtd(7100, 1280, 160, 800, 23, (0, 0))]);
    let e = parse(&b).unwrap();
    assert_eq!(e.preferred.map(|m| (m.w, m.h)), Some((1280, 800)));
    assert_eq!(e.size_mm, None);
    assert_eq!(e.ui_scale_pct(), 100);
}

#[test]
fn the_scale_suits_each_kind_of_display() {
    // Laptops (closer): 16" 2560×1600, 14" and 15.6" 1080p, 13.3" 2560×1600.
    assert_eq!(ui_scale_pct(2560, 344, 215), 150);
    assert_eq!(ui_scale_pct(1920, 309, 174), 125);
    assert_eq!(ui_scale_pct(1920, 344, 194), 125);
    assert_eq!(ui_scale_pct(2560, 286, 179), 175);
    // Desk monitors: 24" 1080p, 27" 1440p, 27" 4K.
    assert_eq!(ui_scale_pct(1920, 531, 299), 100);
    assert_eq!(ui_scale_pct(2560, 597, 336), 100);
    assert_eq!(ui_scale_pct(3840, 597, 336), 150);
    // QEMU's own EDID (100 dpi) stays at 100 %, whatever the mode.
    assert_eq!(ui_scale_pct(1280, 325, 203), 100);
    assert_eq!(ui_scale_pct(2560, 650, 406), 100);
    // Never past 300 %, never under 100 %.
    assert_eq!(ui_scale_pct(7680, 300, 190), 300);
    assert_eq!(ui_scale_pct(800, 600, 400), 100);
    assert_eq!(ui_scale_pct(1920, 0, 0), 100);
}

#[test]
fn the_mode_chosen_is_the_panels_own_when_offered() {
    let modes = [(800, 600), (1280, 800), (1920, 1200), (2560, 1600), (1024, 768)];
    assert_eq!(pick_mode(&modes, Some((2560, 1600)), 1), 3);
    // Not offered: the largest of the same shape that fits (16:10).
    let modes = [(800, 600), (1280, 800), (1920, 1200), (1920, 1080)];
    assert_eq!(pick_mode(&modes, Some((2560, 1600)), 0), 2);
    // Nothing of that shape: the mode already set stays.
    let modes = [(800, 600), (1024, 768)];
    assert_eq!(pick_mode(&modes, Some((2560, 1600)), 1), 1);
    // No EDID: the mode already set stays.
    assert_eq!(pick_mode(&modes, None, 0), 0);
    // A mode that couldn't be read is never chosen, even "of the same shape".
    assert_eq!(pick_mode(&[(0, 0), (1024, 768)], Some((2560, 1600)), 1), 1);
    assert_eq!(pick_mode(&[(0, 0)], Some((0, 0)), 0), 0);
}

#[test]
fn the_starting_scale_follows_the_screens_real_width() {
    let omen = laptop();
    assert_eq!(scale_for(&omen, 2560), 150);
    // The firmware didn't offer 2560×1600: 1280×800 on the same panel is 94 dpi.
    assert_eq!(scale_for(&omen, 1280), 100);
    // No EDID, a damaged one, one without a size: 100 %.
    assert_eq!(scale_for(&[], 2560), 100);
    let mut bad = omen;
    bad[40] ^= 0xFF;
    assert_eq!(scale_for(&bad, 2560), 100);
    let sizeless = block(b"QEM", (1, 4), (0, 0), 0x02, &[dtd(7100, 1280, 160, 800, 23, (0, 0))]);
    assert_eq!(scale_for(&sizeless, 1280), 100);
}

#[test]
fn a_synthesized_panel_reads_back() {
    let b = synthesize(b"FRY", (2560, 1600), (344, 215), "Panel 16");
    let e = parse(&b).unwrap();
    assert_eq!(&e.manufacturer, b"FRY");
    assert_eq!(e.preferred.map(|m| (m.w, m.h)), Some((2560, 1600)));
    assert!(e.preferred.is_some_and(|m| (59_000..61_000).contains(&m.refresh_mhz)));
    assert_eq!(e.size_mm, Some((344, 215)));
    assert_eq!(e.name(), Some("Panel 16"));
    assert_eq!(scale_for(&b, 2560), 150);
}
