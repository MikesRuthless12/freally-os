// SPDX-License-Identifier: Apache-2.0
//! `send_input`'s keys (TASK-084): text and chords ("Super+M") as the key
//! events drv-i8042 would send, US layout, each modifier pressed before the
//! key and let go after it, as a person's hands do.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use finput::{Modifiers, us_char};

/// One key going down or up, as `stage::Inject` carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    pub usage: u16,
    pub down: bool,
    pub ch: u32,
    /// Bit 0 shift, 1 ctrl, 2 alt, 3 meta.
    pub modifiers: u8,
}

const SHIFT: u16 = 0xE1;
const CTRL: u16 = 0xE0;
const ALT: u16 = 0xE2;
const META: u16 = 0xE3;

/// The named keys a chord may end in, beside letters, digits and the
/// printable characters.
const NAMED: &[(&str, u16)] = &[
    ("enter", 0x28),
    ("return", 0x28),
    ("escape", 0x29),
    ("esc", 0x29),
    ("backspace", 0x2A),
    ("tab", 0x2B),
    ("space", 0x2C),
    ("home", 0x4A),
    ("pageup", 0x4B),
    ("delete", 0x4C),
    ("end", 0x4D),
    ("pagedown", 0x4E),
    ("right", 0x4F),
    ("left", 0x50),
    ("down", 0x51),
    ("up", 0x52),
];

fn mods(bits: u8) -> Modifiers {
    Modifiers { shift: bits & 1 != 0, ctrl: bits & 2 != 0, alt: bits & 4 != 0, meta: bits & 8 != 0, caps: false }
}

/// The key and whether Shift is needed for `c`.
fn key_for(c: char) -> Option<(u16, bool)> {
    if c == '\n' {
        return Some((0x28, false));
    }
    if c == '\t' {
        return Some((0x2B, false));
    }
    (0x04..=0x38u16).find_map(|u| {
        if us_char(u, mods(0)) == Some(c) {
            Some((u, false))
        } else if us_char(u, mods(1)) == Some(c) {
            Some((u, true))
        } else {
            None
        }
    })
}

/// The presses for `usage` with `held` modifiers (bits): each modifier
/// down in turn, the key down and up, the modifiers up in reverse.
fn tap(out: &mut Vec<Press>, usage: u16, held: u8) {
    press_mods(out, held, true);
    let ch = us_char(usage, mods(held)).filter(|_| held & !1 == 0).map_or(0, |c| c as u32);
    out.push(Press { usage, down: true, ch, modifiers: held });
    out.push(Press { usage, down: false, ch: 0, modifiers: held });
    press_mods(out, held, false);
}

const MODIFIERS: [(u8, u16); 4] = [(1, SHIFT), (2, CTRL), (4, ALT), (8, META)];

/// The `held` modifiers going down in turn (each event carrying the
/// modifiers held after it, as drv-i8042's do), or up in reverse.
fn press_mods(out: &mut Vec<Press>, held: u8, down: bool) {
    if down {
        let mut bits = 0u8;
        for (b, u) in MODIFIERS {
            if held & b != 0 {
                bits |= b;
                out.push(Press { usage: u, down: true, ch: 0, modifiers: bits });
            }
        }
    } else {
        let mut bits = held;
        for (b, u) in MODIFIERS.iter().rev() {
            if held & b != 0 {
                bits &= !b;
                out.push(Press { usage: *u, down: false, ch: 0, modifiers: bits });
            }
        }
    }
}

fn modifier(name: &str) -> Option<u8> {
    match name {
        "shift" => Some(1),
        "ctrl" | "control" => Some(2),
        "alt" => Some(4),
        "super" | "meta" | "win" | "cmd" => Some(8),
        _ => None,
    }
}

/// `text` typed out; the first character the US layout can't type stops it.
pub fn text(text: &str) -> Result<Vec<Press>, String> {
    let mut out = Vec::new();
    for c in text.chars() {
        let (u, shift) = key_for(c).ok_or_else(|| format!("{c:?} can't be typed on the US layout"))?;
        tap(&mut out, u, shift as u8);
    }
    Ok(out)
}

/// A chord: modifiers and one key joined by `+` ("Super+M", "Ctrl+Shift+Tab",
/// "F5", "Enter"), any case.
pub fn chord(chord: &str) -> Result<Vec<Press>, String> {
    let parts: Vec<String> = chord.split('+').map(|p| p.trim().to_ascii_lowercase()).collect();
    let (last, rest) = parts.split_last().ok_or("an empty chord")?;
    let mut held = 0u8;
    for m in rest {
        held |= modifier(m).ok_or_else(|| format!("{m:?} isn't a modifier (Shift, Ctrl, Alt, Super)"))?;
    }
    // A chord ending in a modifier taps the modifiers alone ("Super" opens
    // the launcher).
    if let Some(bit) = modifier(last) {
        let mut out = Vec::new();
        press_mods(&mut out, held | bit, true);
        press_mods(&mut out, held | bit, false);
        return Ok(out);
    }
    let usage = if let Some(&(_, u)) = NAMED.iter().find(|(n, _)| n == last) {
        u
    } else if let Some(n) = last.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()).filter(|n| (1..=12).contains(n))
    {
        0x3A + n - 1
    } else {
        let mut cs = last.chars();
        match (cs.next(), cs.next()) {
            (Some(c), None) => {
                let (u, shift) = key_for(c).ok_or_else(|| format!("{c:?} isn't a key"))?;
                held |= shift as u8;
                u
            }
            _ => return Err(format!("{last:?} isn't a key name")),
        }
    };
    let mut out = Vec::new();
    tap(&mut out, usage, held);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn downs(p: &[Press]) -> Vec<(u16, u32, u8)> {
        p.iter().filter(|p| p.down).map(|p| (p.usage, p.ch, p.modifiers)).collect()
    }

    #[test]
    fn text_comes_as_keys() {
        assert_eq!(downs(&text("a1").unwrap()), [(0x04, 'a' as u32, 0), (0x1E, '1' as u32, 0)]);
        // A capital: Shift down, the key with Shift held, Shift up.
        let t = text("A!").unwrap();
        assert_eq!(downs(&t), [(SHIFT, 0, 1), (0x04, 'A' as u32, 1), (SHIFT, 0, 1), (0x1E, '!' as u32, 1)]);
        assert_eq!(t.iter().filter(|p| !p.down && p.usage == SHIFT).count(), 2);
        assert_eq!(downs(&text("\n").unwrap()), [(0x28, 0, 0)]);
        assert!(text("é").unwrap_err().contains("US layout"));
    }

    #[test]
    fn chords_press_modifiers_around_the_key() {
        let c = chord("Super+M").unwrap();
        assert_eq!(
            c,
            [
                Press { usage: META, down: true, ch: 0, modifiers: 8 },
                Press { usage: 0x10, down: true, ch: 0, modifiers: 8 },
                Press { usage: 0x10, down: false, ch: 0, modifiers: 8 },
                Press { usage: META, down: false, ch: 0, modifiers: 0 },
            ]
        );
        assert_eq!(downs(&chord("ctrl+shift+tab").unwrap()).last(), Some(&(0x2B, 0, 3)));
        assert_eq!(downs(&chord("F5").unwrap()), [(0x3E, 0, 0)]);
        assert_eq!(downs(&chord("Enter").unwrap()), [(0x28, 0, 0)]);
        // Super alone: the tap that opens the launcher, Meta held on its own down.
        assert_eq!(
            chord("Super").unwrap(),
            [
                Press { usage: META, down: true, ch: 0, modifiers: 8 },
                Press { usage: META, down: false, ch: 0, modifiers: 0 },
            ]
        );
        assert!(chord("Hyper+A").unwrap_err().contains("isn't a modifier"));
        assert!(chord("Ctrl+Banana").unwrap_err().contains("isn't a key name"));
    }
}
