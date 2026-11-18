// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask specimen` (TASK-053): Noto Sans at 400, 500 and 600, drawn by
//! ffont's run-time path (the one user space draws with: glyphs rendered on
//! first use, blended in linear light), on both of the UI kit's grounds, for
//! the owner's sign-off. Writes `target/specimen/noto-sans.{png,fimg}` (and
//! `noto-mono.png`: Noto Sans Mono on the terminal's cell grid) and
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
