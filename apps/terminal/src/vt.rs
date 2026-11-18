// SPDX-License-Identifier: Apache-2.0
//! The VT parser (TASK-060, FR-032): bytes in, actions out, after DEC's
//! state machine (the subset xterm-likes use): UTF-8 text, C0 controls,
//! ESC sequences, CSI with parameters, sub-parameters (`38:2::r:g:b`) and a
//! private marker (`?`), and OSC strings (read and kept, BEL or ST ended).
//! DCS and other strings are swallowed whole. Malformed input never stops
//! it: a bad byte is dropped and parsing goes on.

use alloc::vec::Vec;

/// One parameter of a CSI sequence and its sub-parameters (`38:2::1:2:3`
/// is one parameter, 38, with sub-parameters 2, -, 1, 2, 3; a missing one
/// is `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub value: Option<u32>,
    pub sub: Vec<Option<u32>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A character to put on the screen.
    Print(char),
    /// A C0 control (BS, HT, LF, VT, FF, CR, BEL...).
    Execute(u8),
    /// `ESC [ ... final`: its private marker (`?`, `>`, `=`, `<`), its
    /// parameters, its intermediate bytes, and its final byte.
    Csi { private: Option<u8>, params: Vec<Param>, intermediates: Vec<u8>, final_byte: u8 },
    /// `ESC (intermediates) final`.
    Esc { intermediates: Vec<u8>, final_byte: u8 },
    /// `ESC ] ... BEL` or `ESC ] ... ESC \`.
    Osc(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Csi,
    Osc,
    /// DCS, SOS, PM, APC: swallowed until ST.
    Ignore,
}

/// The most a sequence keeps: a longer one is cut (never unbounded memory).
const MAX_PARAMS: usize = 32;
const MAX_OSC: usize = 4096;

#[derive(Debug)]
pub struct Parser {
    state: State,
    /// A UTF-8 character in progress: what's come and how many more.
    utf8: u32,
    utf8_left: u8,
    private: Option<u8>,
    params: Vec<Param>,
    current: Option<u32>,
    sub: Vec<Option<u32>>,
    in_sub: bool,
    intermediates: Vec<u8>,
    osc: Vec<u8>,
    /// An ESC seen inside a string: `\` ends it.
    string_esc: bool,
}

impl Default for Parser {
    fn default() -> Parser {
        Parser {
            state: State::Ground,
            utf8: 0,
            utf8_left: 0,
            private: None,
            params: Vec::new(),
            current: None,
            sub: Vec::new(),
            in_sub: false,
            intermediates: Vec::new(),
            osc: Vec::new(),
            string_esc: false,
        }
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Feeds bytes; each action as it completes goes to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut impl FnMut(Action)) {
        for &b in bytes {
            self.byte(b, out);
        }
    }

    fn clear(&mut self) {
        self.private = None;
        self.params.clear();
        self.current = None;
        self.sub.clear();
        self.in_sub = false;
        self.intermediates.clear();
    }

    /// The parameter in progress is done (at `;` or the final byte).
    fn end_param(&mut self) {
        let p = if self.in_sub {
            self.sub.push(self.current.take());
            let mut sub = core::mem::take(&mut self.sub);
            let value = sub.remove(0);
            Param { value, sub }
        } else {
            Param { value: self.current.take(), sub: Vec::new() }
        };
        if self.params.len() < MAX_PARAMS {
            self.params.push(p);
        }
        self.sub.clear();
        self.in_sub = false;
    }

    fn byte(&mut self, b: u8, out: &mut impl FnMut(Action)) {
        // CAN and SUB cancel any sequence; ESC starts a new one (except in a
        // string, where it may be the start of ST).
        match (self.state, b) {
            (State::Osc | State::Ignore, 0x1B) => {
                self.string_esc = true;
                return;
            }
            (State::Osc | State::Ignore, b'\\') if self.string_esc => {
                self.string_esc = false;
                if self.state == State::Osc {
                    out(Action::Osc(core::mem::take(&mut self.osc)));
                }
                self.state = State::Ground;
                return;
            }
            (State::Osc | State::Ignore, _) if self.string_esc => {
                // ESC then something else: the string ends, a new sequence begins.
                self.string_esc = false;
                if self.state == State::Osc {
                    out(Action::Osc(core::mem::take(&mut self.osc)));
                }
                self.state = State::Escape;
                self.clear();
                self.byte(b, out);
                return;
            }
            (_, 0x18 | 0x1A) => {
                self.state = State::Ground;
                self.utf8_left = 0;
                return;
            }
            (State::Osc | State::Ignore, _) => {}
            (_, 0x1B) => {
                self.state = State::Escape;
                self.utf8_left = 0;
                self.clear();
                return;
            }
            _ => {}
        }
        match self.state {
            State::Ground => self.ground(b, out),
            State::Escape => match b {
                0x00..=0x1F => out(Action::Execute(b)),
                0x20..=0x2F => {
                    if self.intermediates.len() < 4 {
                        self.intermediates.push(b);
                    }
                }
                b'[' if self.intermediates.is_empty() => self.state = State::Csi,
                b']' if self.intermediates.is_empty() => {
                    self.state = State::Osc;
                    self.osc.clear();
                }
                b'P' | b'X' | b'^' | b'_' if self.intermediates.is_empty() => self.state = State::Ignore,
                0x30..=0x7E => {
                    out(Action::Esc { intermediates: core::mem::take(&mut self.intermediates), final_byte: b });
                    self.state = State::Ground;
                }
                _ => self.state = State::Ground,
            },
            State::Csi => match b {
                0x00..=0x1F => out(Action::Execute(b)),
                b'0'..=b'9' => {
                    let d = (b - b'0') as u32;
                    self.current = Some(self.current.unwrap_or(0).saturating_mul(10).saturating_add(d).min(65_535));
                }
                b':' => {
                    // The value (or the sub-parameter) so far, then the next.
                    self.sub.push(self.current.take());
                    self.in_sub = true;
                }
                b';' => self.end_param(),
                b'<'..=b'?' => {
                    if self.params.is_empty() && self.current.is_none() && self.private.is_none() {
                        self.private = Some(b);
                    }
                }
                0x20..=0x2F => {
                    if self.intermediates.len() < 4 {
                        self.intermediates.push(b);
                    }
                }
                0x40..=0x7E => {
                    if self.current.is_some() || !self.params.is_empty() || self.in_sub {
                        self.end_param();
                    }
                    out(Action::Csi {
                        private: self.private,
                        params: core::mem::take(&mut self.params),
                        intermediates: core::mem::take(&mut self.intermediates),
                        final_byte: b,
                    });
                    self.clear();
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::Osc => match b {
                0x07 => {
                    out(Action::Osc(core::mem::take(&mut self.osc)));
                    self.state = State::Ground;
                }
                _ => {
                    if self.osc.len() < MAX_OSC {
                        self.osc.push(b);
                    }
                }
            },
            State::Ignore => {}
        }
    }

    fn ground(&mut self, b: u8, out: &mut impl FnMut(Action)) {
        if self.utf8_left > 0 {
            if b & 0xC0 == 0x80 {
                self.utf8 = self.utf8 << 6 | (b & 0x3F) as u32;
                self.utf8_left -= 1;
                if self.utf8_left == 0 {
                    out(Action::Print(char::from_u32(self.utf8).unwrap_or('\u{FFFD}')));
                }
                return;
            }
            // A broken sequence: the replacement character, then this byte afresh.
            self.utf8_left = 0;
            out(Action::Print('\u{FFFD}'));
        }
        match b {
            0x00..=0x1F => out(Action::Execute(b)),
            0x20..=0x7E => out(Action::Print(b as char)),
            0x7F => {}
            0xC2..=0xDF => {
                self.utf8 = (b & 0x1F) as u32;
                self.utf8_left = 1;
            }
            0xE0..=0xEF => {
                self.utf8 = (b & 0x0F) as u32;
                self.utf8_left = 2;
            }
            0xF0..=0xF4 => {
                self.utf8 = (b & 0x07) as u32;
                self.utf8_left = 3;
            }
            _ => out(Action::Print('\u{FFFD}')),
        }
    }
}

/// A parameter's value, or `default` when it's missing or zero (CSI's rule
/// for counts: `CSI A` and `CSI 0 A` both move one).
pub fn count(params: &[Param], i: usize, default: u32) -> u32 {
    params.get(i).and_then(|p| p.value).filter(|&v| v != 0).unwrap_or(default)
}

#[cfg(test)]
mod tests;
