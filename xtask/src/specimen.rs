// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask specimen` (TASK-053): Noto Sans at 400, 500 and 600, drawn by
//! ffont's run-time path (the one user space draws with: glyphs rendered on
//! first use, blended in linear light), on both of the UI kit's grounds, for
//! the owner's sign-off. Writes `target/specimen/noto-sans.{png,fimg}` (and
//! `noto-mono.png`: Noto Sans Mono on the terminal's cell grid, and
//! `controls.png`: TASK-056's controls from the hardware set, and
//! `stage.png`: TASK-057's windows, composed as stage composes them) and
//! says which characters of the coverage set (ASCII, Latin-1, the PRD's
//! specials) the font doesn't draw.

use crate::util;
use ffont::atlas::Target;
use ffont::text::{Face, covers, draw};
use fui::tokens_gen::{dark, light};

const WEIGHTS: [(&str, &str); 3] = [
    ("400 Regular", "NotoSans-Regular.ttf"),
    ("500 Medium", "NotoSans-Medium.ttf"),
    ("600 SemiBold", "NotoSans-SemiBold.ttf"),
];
/// FR-026's specials beyond Latin-1.
const SPECIALS: &str = "• – — … ← → ↑ ↓ ✓ ×";

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u32>,
}

impl Target for Canvas {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn get(&self, x: usize, y: usize) -> u32 {
        self.px[y * self.w + x]
    }
    fn set(&mut self, x: usize, y: usize, rgb: u32) {
        self.px[y * self.w + x] = rgb;
    }
}

/// Every character the coverage set asks for.
pub fn coverage() -> Vec<char> {
    (' '..='~').chain('\u{A0}'..='\u{FF}').chain(SPECIALS.chars().filter(|c| *c != ' ')).collect()
}

pub fn specimen_cmd(_args: &[String]) -> Result<(), String> {
    let dir = util::repo_root().join("assets/fonts/noto");
    let files: Vec<Vec<u8>> = WEIGHTS
        .iter()
        .map(|(_, f)| std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}")))
        .collect::<Result<_, _>>()?;
    let fallback: Vec<Vec<u8>> = ["NotoSansSymbols2-Regular.ttf", "NotoSansMono-Regular.ttf"]
        .iter()
        .map(|f| std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}")))
        .collect::<Result<_, _>>()?;
    fn face(d: &[u8]) -> Result<Face<'_>, String> {
        Face::new(d).map_err(|e| format!("{e:?}"))
    }
    // Each weight's chain: the weight, then the faces with the arrows and ✓.
    let mut chains: Vec<Vec<Face>> = files
        .iter()
        .map(|d| Ok(vec![face(d)?, face(&fallback[0])?, face(&fallback[1])?]))
        .collect::<Result<_, String>>()?;

    let ascii: String = (' '..='~').collect();
    let latin1: String = ('\u{A1}'..='\u{FF}').collect();
    let lines: [(f32, String); 6] = [
        (18.0, ascii[..48].to_string()),
        (18.0, ascii[48..].to_string()),
        (18.0, latin1[..48].to_string()),
        (18.0, latin1[48..].to_string()),
        (18.0, SPECIALS.to_string()),
        (28.0, "Freally OS boots to a desktop in under 3 s.".to_string()),
    ];
    let (w, panel) = (1180usize, 360usize);
    let mut c = Canvas { w, h: panel * 6, px: vec![0; w * panel * 6] };
    let grounds = [
        ("dark", dark::COLOR_BG.xrgb(), dark::COLOR_TEXT.xrgb(), dark::COLOR_TEXT_2.xrgb()),
        ("light", light::COLOR_BG.xrgb(), light::COLOR_TEXT.xrgb(), light::COLOR_TEXT_2.xrgb()),
    ];
    let mut y0 = 0;
    for (_, bg, text, muted) in grounds {
        for (i, (label, _)) in WEIGHTS.iter().enumerate() {
            c.px[y0 * w..(y0 + panel) * w].fill(bg);
            let face = &mut chains[i][..];
            draw(&mut c, face, 14.0, 24, y0 as i32 + 30, &format!("Noto Sans {label}"), muted);
            let mut y = y0 as i32 + 70;
            for (px, s) in &lines {
                draw(&mut c, face, *px, 24, y, s, text);
                y += if *px > 20.0 { 44 } else { 34 };
            }
            draw(&mut c, face, 14.0, 24, y + 20, "0123456789  0123456789  14 px", muted);
            y0 += panel;
        }
    }

    let out = util::target_dir().join("specimen");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let rgb: Vec<u8> = c.px.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
    std::fs::write(out.join("noto-sans.png"), fimage::png::encode(w as u32, c.h as u32, 3, &rgb))
        .map_err(|e| e.to_string())?;
    let bgra: Vec<u8> = c.px.iter().flat_map(|&p| [p as u8, (p >> 8) as u8, (p >> 16) as u8, 255]).collect();
    std::fs::write(out.join("noto-sans.fimg"), fimage::fimg::encode(w as u32, c.h as u32, 1, [0; 4], &bgra))
        .map_err(|e| e.to_string())?;
    println!("specimen: {}", out.join("noto-sans.png").display());
    mono(&dir, &out)?;
    controls(&out)?;
    stage(&out, &dir)?;

    for ((label, _), chain) in WEIGHTS.iter().zip(&chains) {
        let missing: String = coverage().into_iter().filter(|c| !covers(chain, *c)).collect();
        if missing.is_empty() {
            println!("specimen: {label}: every character of the set");
        } else {
            println!("specimen: {label}: draws no {missing} (its missing-glyph box)");
        }
    }
    Ok(())
}

/// Noto Sans Mono on the terminal's cell grid (TASK-054), both grounds, at
/// 14 and 16 px: every printable ASCII character in its cell.
fn mono(dir: &std::path::Path, out: &std::path::Path) -> Result<(), String> {
    use ffont::text::{cell, draw_cell};
    let data = std::fs::read(dir.join("NotoSansMono-Regular.ttf")).map_err(|e| e.to_string())?;
    let mut chain = [Face::new(&data).map_err(|e| format!("{e:?}"))?];
    let ascii: Vec<char> = ('!'..='~').collect();
    let (w, h) = (900usize, 520usize);
    let mut c = Canvas { w, h, px: vec![0; w * h] };
    let grounds = [
        (dark::COLOR_BG.xrgb(), dark::COLOR_TEXT.xrgb(), dark::COLOR_PRIMARY_INK.xrgb()),
        (light::COLOR_BG.xrgb(), light::COLOR_TEXT.xrgb(), light::COLOR_PRIMARY_INK.xrgb()),
    ];
    for (i, (bg, text, green)) in grounds.into_iter().enumerate() {
        let y0 = i * h / 2;
        c.px[y0 * w..(y0 + h / 2) * w].fill(bg);
        let mut y = y0 as i32 + 16;
        for px in [14.0f32, 16.0] {
            let k = cell(&mut chain[0], px);
            for (n, ch) in ascii.iter().enumerate() {
                let (col, row) = (n as i32 % 47, n as i32 / 47);
                draw_cell(&mut c, &mut chain, px, k, 24, y, col, row, *ch, text);
            }
            let prompt: Vec<char> = "freally:~$ cargo xtask ci".chars().collect();
            for (col, ch) in prompt.iter().enumerate() {
                draw_cell(&mut c, &mut chain, px, k, 24, y, col as i32, 2, *ch, if col < 10 { green } else { text });
            }
            y += 3 * k.h + 24;
        }
    }
    let rgb: Vec<u8> = c.px.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
    std::fs::write(out.join("noto-mono.png"), fimage::png::encode(w as u32, h as u32, 3, &rgb))
        .map_err(|e| e.to_string())?;
    println!("specimen: {}", out.join("noto-mono.png").display());
    Ok(())
}

/// TASK-056's controls as the OS draws them, from the image's own FIMGs at
/// 2x, on both grounds: the switch off and on, the knob at 0, ½ and 1, the
/// fader low and high, the key at rest, under the pointer and pressed, a
/// toggle key, a meter at −18 and −2 dBFS, the lamps, and a focus ring.
fn controls(out: &std::path::Path) -> Result<(), String> {
    use fimage::blit::Surface;
    use fui::controls::paint as cp;
    use fui::hardware::{Hardware, KeyLook, Lamp};
    use fui::layout::Rect;
    use fui::theme::{Mode, Theme, Tok};
    crate::sync_hardware::assets()?;
    let base = util::target_dir().join("assets/hardware");
    let (w, half) = (1260u32, 460u32);
    let mut px = vec![0u32; (w * half * 2) as usize];
    for (i, (theme, mode)) in [("dark", Mode::Dark), ("light", Mode::Light)].into_iter().enumerate() {
        let files: Vec<(String, Vec<u8>)> = fui::hardware::PIECES
            .iter()
            .map(|p| {
                let f = base.join(theme).join(format!("{p}@2x.fimg"));
                std::fs::read(&f).map(|b| (p.to_string(), b)).map_err(|e| format!("{}: {e}", f.display()))
            })
            .collect::<Result<_, _>>()?;
        let hw = Hardware::load(2, |p| files.iter().find(|(n, _)| n == p).map(|(_, b)| b.as_slice()))?;
        let t = Theme::new(mode, 2.0);
        let y0 = i as u32 * half;
        px[(y0 * w) as usize..((y0 + half) * w) as usize].fill(t.colour(Tok::Bg).xrgb());
        let mut s =
            Surface { px: &mut px[(y0 * w) as usize..((y0 + half) * w) as usize], width: w, height: half, stride: w };
        let (sw, _) = cp::switch_size(&hw);
        cp::paint_switch(&mut s, &hw, 40, 40, false);
        cp::paint_switch(&mut s, &hw, 40 + sw as i32 + 24, 40, true);
        let k = cp::knob_size(&hw) as i32;
        for (n, v) in [0.0, 0.5, 1.0].into_iter().enumerate() {
            cp::paint_knob(&mut s, &hw, &t, 40 + n as i32 * (k + 16), 140, v);
        }
        let (fw, _) = cp::fader_size(&hw);
        cp::paint_fader(&mut s, &hw, 760, 17, 0.2);
        cp::paint_fader(&mut s, &hw, 760 + fw as i32 + 8, 17, 0.85);
        for (n, look) in [KeyLook::Rest, KeyLook::Hover, KeyLook::Press(5)].into_iter().enumerate() {
            cp::paint_key(&mut s, &hw, Rect::new(40 + n as i32 * 200, 330, 160, 50), look);
        }
        cp::paint_toggle_key(&mut s, &hw, 640, 330, true, false);
        let (mw, _) = cp::meter_size(&hw, 12);
        cp::paint_meter(&mut s, &hw, 1040, 30, 12, -18.0);
        cp::paint_meter(&mut s, &hw, 1040 + mw as i32 + 16, 30, 12, -2.0);
        for (n, lamp) in [Lamp::Off, Lamp::On, Lamp::Warn, Lamp::Stop].into_iter().enumerate() {
            cp::paint_lamp(&mut s, &hw, 1060 + n as i32 * 30, 400, lamp);
        }
        cp::paint_focus(&mut s, &t, Rect::new(40, 330, 160, 50), 16.0);
    }
    let rgb: Vec<u8> = px.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
    std::fs::write(out.join("controls.png"), fimage::png::encode(w, half * 2, 3, &rgb)).map_err(|e| e.to_string())?;
    println!("specimen: {}", out.join("controls.png").display());
    Ok(())
}

/// TASK-057's windows, composed by stage's own code at 960 × 600 in both
/// themes: an unfocused window behind a focused one with a long title cut
/// short, each surface a client's gradient.
fn stage(out: &std::path::Path, fonts_dir: &std::path::Path) -> Result<(), String> {
    use fui::text::{FontData, Fonts};
    use fui::theme::{Mode, Theme};
    use fwire::proto::stage::Kind;
    use svc_stage::compose::{Back, Look, Pixels, compose};
    use svc_stage::windows::Windows;
    let read = |f: &str| std::fs::read(fonts_dir.join(f)).map_err(|e| format!("{f}: {e}"));
    let (r, m, sb, mono, sym) = (
        read("NotoSans-Regular.ttf")?,
        read("NotoSans-Medium.ttf")?,
        read("NotoSans-SemiBold.ttf")?,
        read("NotoSansMono-Regular.ttf")?,
        read("NotoSansSymbols2-Regular.ttf")?,
    );
    let mut fonts = Fonts::new(FontData { regular: &r, medium: &m, semibold: &sb, mono: &mono, symbols: &sym })
        .map_err(|e| format!("{e:?}"))?;
    let (w, h) = (960u32, 600u32);
    let gradient = |sw: u32, sh: u32, a: u32, b: u32| -> Vec<u32> {
        let ch = |c: u32, s: u32| (c >> s) & 0xFF;
        (0..sh)
            .flat_map(|y| {
                (0..sw).map(move |x| {
                    let t = (x + y) as f32 / (sw + sh) as f32;
                    let mix = |s| ((ch(a, s) as f32 * (1.0 - t) + ch(b, s) as f32 * t) as u32) << s;
                    0xFF00_0000 | mix(16) | mix(8) | mix(0)
                })
            })
            .collect()
    };
    let back_px = gradient(420, 260, 0x1E3A5F, 0x2B6CB0);
    let front_px = gradient(380, 240, 0x23402A, 0x79D920);
    let mut all = vec![0u32; (w * h * 2) as usize];
    for (i, mode) in [Mode::Dark, Mode::Light].into_iter().enumerate() {
        let mut ws = Windows::new(w, h, 32);
        let a = ws.create("Monitor", Kind::Normal, (420, 260), (0, 0), (0, 0), (1, "specimen"));
        ws.move_to(a, 120, 90);
        let b = ws.create(
            "Terminal — freally:~ — a title far too long for its strip, cut short",
            Kind::Normal,
            (380, 240),
            (0, 0),
            (0, 0),
            (1, "specimen"),
        );
        ws.move_to(b, 420, 230);
        let (sa, sb) = ((420u32, 260u32, &back_px[..]), (380u32, 240u32, &front_px[..]));
        let mut back = Back::new(w, h);
        let mut look = Look { theme: Theme::new(mode, 1.0), fonts: &mut fonts, art: None };
        let area = back.rect();
        let find = |id| match id {
            _ if id == a => Some(&sa as &dyn Pixels),
            _ if id == b => Some(&sb as &dyn Pixels),
            _ => None,
        };
        compose(&mut back, area, &ws, &find, &mut look);
        all[(i as u32 * w * h) as usize..((i as u32 + 1) * w * h) as usize].copy_from_slice(&back.px);
    }
    let rgb: Vec<u8> = all.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
    std::fs::write(out.join("stage.png"), fimage::png::encode(w, h * 2, 3, &rgb)).map_err(|e| e.to_string())?;
    println!("specimen: {}", out.join("stage.png").display());
    Ok(())
}
