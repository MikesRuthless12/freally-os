// SPDX-License-Identifier: Apache-2.0
//! The Calculator's working (TASK-112, FR-054): the sum being typed, its
//! answer, the history, the angle and the mode. `main.rs` draws it and
//! feeds it keys; everything it decides is tested here.
//!
//! A sum is text in `fdecimal::expr`'s grammar, built by the keys or typed;
//! `=` works it out exactly (`0.1 + 0.2 = 0.3`).
#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use fdecimal::expr::{self, Angle};

/// The keys the program reads (HID usages) and Ctrl, in stage's modifier bits.
pub mod key {
    pub const C: u16 = 0x06;
    pub const V: u16 = 0x19;
    pub const ENTER: u16 = 0x28;
    pub const ESCAPE: u16 = 0x29;
    pub const BACKSPACE: u16 = 0x2A;
    pub const DELETE: u16 = 0x4C;
    pub const KP_ENTER: u16 = 0x58;
    pub const CTRL: u8 = 1 << 1;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Standard,
    Scientific,
}

/// The most sums the history keeps.
pub const HISTORY: usize = 50;
/// The longest sum (in characters).
pub const LONGEST: usize = 256;

/// A key of the pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Digit(u8),
    Point,
    Plus,
    Minus,
    Times,
    Divide,
    Open,
    Close,
    Percent,
    Equals,
    Clear,
    Back,
    Negate,
    Square,
    Power,
    Root,
    Recip,
    Fact,
    Exp10,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Ln,
    Log,
    ExpE,
    Abs,
    Pi,
    E,
}

/// The standard pad, row by row.
pub const STANDARD: [[Key; 4]; 5] = [
    [Key::Clear, Key::Back, Key::Percent, Key::Divide],
    [Key::Digit(7), Key::Digit(8), Key::Digit(9), Key::Times],
    [Key::Digit(4), Key::Digit(5), Key::Digit(6), Key::Minus],
    [Key::Digit(1), Key::Digit(2), Key::Digit(3), Key::Plus],
    [Key::Negate, Key::Digit(0), Key::Point, Key::Equals],
];

/// The scientific keys, to the left of the standard pad.
pub const SCIENTIFIC: [[Key; 4]; 5] = [
    [Key::Open, Key::Close, Key::Square, Key::Power],
    [Key::Sin, Key::Cos, Key::Tan, Key::Root],
    [Key::Asin, Key::Acos, Key::Atan, Key::Fact],
    [Key::Ln, Key::Log, Key::ExpE, Key::Pi],
    [Key::E, Key::Exp10, Key::Abs, Key::Recip],
];

impl Key {
    /// What's printed on it.
    pub fn label(self) -> &'static str {
        const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
        match self {
            Key::Digit(d) => DIGITS[d.min(9) as usize],
            Key::Point => ".",
            Key::Plus => "+",
            Key::Minus => "−",
            Key::Times => "×",
            Key::Divide => "÷",
            Key::Open => "(",
            Key::Close => ")",
            Key::Percent => "%",
            Key::Equals => "=",
            Key::Clear => "C",
            Key::Back => "←",
            Key::Negate => "±",
            Key::Square => "x²",
            Key::Power => "xʸ",
            Key::Root => "√",
            Key::Recip => "1/x",
            Key::Fact => "n!",
            Key::Exp10 => "EXP",
            Key::Sin => "sin",
            Key::Cos => "cos",
            Key::Tan => "tan",
            Key::Asin => "asin",
            Key::Acos => "acos",
            Key::Atan => "atan",
            Key::Ln => "ln",
            Key::Log => "log",
            Key::ExpE => "eˣ",
            Key::Abs => "|x|",
            Key::Pi => "π",
            Key::E => "e",
        }
    }

    /// Its name in a test id (`calc-key-<name>`) and to a screen reader.
    pub fn name(self) -> &'static str {
        const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
        match self {
            Key::Digit(d) => DIGITS[d.min(9) as usize],
            Key::Point => "point",
            Key::Plus => "plus",
            Key::Minus => "minus",
            Key::Times => "times",
            Key::Divide => "divide",
            Key::Open => "open",
            Key::Close => "close",
            Key::Percent => "percent",
            Key::Equals => "equals",
            Key::Clear => "clear",
            Key::Back => "back",
            Key::Negate => "negate",
            Key::Square => "square",
            Key::Power => "power",
            Key::Root => "root",
            Key::Recip => "recip",
            Key::Fact => "fact",
            Key::Exp10 => "exp10",
            Key::Sin => "sin",
            Key::Cos => "cos",
            Key::Tan => "tan",
            Key::Asin => "asin",
            Key::Acos => "acos",
            Key::Atan => "atan",
            Key::Ln => "ln",
            Key::Log => "log",
            Key::ExpE => "exp",
            Key::Abs => "abs",
            Key::Pi => "pi",
            Key::E => "e",
        }
    }

    /// A function key's opening (`sin(`), as the sum shows it.
    fn function(self) -> Option<&'static str> {
        Some(match self {
            Key::Sin => "sin(",
            Key::Cos => "cos(",
            Key::Tan => "tan(",
            Key::Asin => "asin(",
            Key::Acos => "acos(",
            Key::Atan => "atan(",
            Key::Ln => "ln(",
            Key::Log => "log(",
            Key::ExpE => "exp(",
            Key::Abs => "abs(",
            Key::Root => "√(",
            _ => return None,
        })
    }
}

/// The binary operators, as the sum shows them.
const BINARY: [char; 5] = ['+', '−', '×', '÷', '^'];

pub struct Calc {
    pub mode: Mode,
    pub angle: Angle,
    /// The sum, as shown.
    pub entry: String,
    /// The last `=`'s answer, or why there's none.
    pub answer: Option<Result<String, &'static str>>,
    /// Earlier sums and their answers, newest first.
    pub history: Vec<(String, String)>,
    /// `=` was the last key: a number starts a new sum, an operator goes on
    /// from the answer.
    fresh: bool,
}

impl Default for Calc {
    fn default() -> Calc {
        Calc::new()
    }
}

impl Calc {
    pub fn new() -> Calc {
        Calc {
            mode: Mode::Standard,
            angle: Angle::Degrees,
            entry: String::new(),
            answer: None,
            history: Vec::new(),
            fresh: false,
        }
    }

    /// Whether the answer is showing (`=` was the last key).
    pub fn answered(&self) -> bool {
        self.fresh
    }

    /// The answer, if the last `=` had one.
    fn ok_answer(&self) -> Option<String> {
        match (&self.answer, self.fresh) {
            (Some(Ok(v)), true) => Some(v.clone()),
            _ => None,
        }
    }

    /// A new sum (after `=`): empty.
    fn start(&mut self) {
        if self.fresh {
            self.entry.clear();
            self.answer = None;
            self.fresh = false;
        }
    }

    /// Going on from the answer (after `=`): the sum is the answer.
    fn go_on(&mut self) {
        if self.fresh {
            if let Some(v) = self.ok_answer() {
                self.entry = v;
            }
            self.answer = None;
            self.fresh = false;
        }
    }

    fn push(&mut self, s: &str) {
        if self.entry.chars().count() + s.chars().count() <= LONGEST {
            self.entry.push_str(s);
        }
    }

    /// The number being typed at the end of the sum: where it starts.
    fn last_number(&self) -> Option<usize> {
        let s = &self.entry;
        let mut i = s.len();
        while let Some(c) = s[..i].chars().last() {
            let w = c.len_utf8();
            // An exponent's sign (2E−3) belongs to the number.
            let exp_sign = matches!(c, '−' | '-' | '+') && s[..i - w].ends_with('E');
            if c.is_ascii_digit() || c == '.' || c == 'E' || exp_sign {
                i -= w;
            } else {
                break;
            }
        }
        (i < s.len()).then_some(i)
    }

    fn binary(&mut self, op: char) {
        self.go_on();
        let last = self.entry.chars().last();
        match last {
            // A sum can't start with ×, ÷ or ^ (a minus is a sign).
            None if op != '−' => {}
            // An operator after an operator replaces it; a minus after
            // another one is a sign (2×−3).
            Some(l) if BINARY.contains(&l) => {
                if op == '−' && l != '−' {
                    self.push("−");
                } else {
                    while self.entry.ends_with(BINARY) {
                        self.entry.pop();
                    }
                    if !self.entry.is_empty() || op == '−' {
                        self.entry.push(op);
                    }
                }
            }
            Some('(') if op != '−' => {}
            _ => {
                let mut b = [0u8; 4];
                self.push(op.encode_utf8(&mut b));
            }
        }
    }

    pub fn press(&mut self, k: Key) {
        match k {
            Key::Digit(d) => {
                self.start();
                self.push(Key::Digit(d).label());
            }
            Key::Point => {
                self.start();
                match self.last_number() {
                    Some(i) if self.entry[i..].contains(['.', 'E']) => {}
                    Some(_) => self.push("."),
                    None => self.push("0."),
                }
            }
            Key::Plus => self.binary('+'),
            Key::Minus => self.binary('−'),
            Key::Times => self.binary('×'),
            Key::Divide => self.binary('÷'),
            Key::Power => self.binary('^'),
            Key::Percent | Key::Fact | Key::Square => {
                self.go_on();
                // After a number, a bracket or a constant.
                if self
                    .entry
                    .ends_with(|c: char| c.is_ascii_digit() || c == ')' || c == 'π' || c == 'e' || c == '!' || c == '%')
                {
                    self.push(match k {
                        Key::Percent => "%",
                        Key::Fact => "!",
                        _ => "^2",
                    });
                }
            }
            Key::Open => {
                self.start();
                self.push("(");
            }
            Key::Close => {
                let open = self.entry.matches('(').count();
                if open > self.entry.matches(')').count() && !self.entry.ends_with('(') {
                    self.push(")");
                }
            }
            Key::Pi | Key::E => {
                self.start();
                self.push(if k == Key::Pi { "π" } else { "e" });
            }
            Key::Exp10 => {
                if !self.fresh
                    && let Some(i) = self.last_number()
                    && !self.entry[i..].contains('E')
                    && self.entry.ends_with(|c: char| c.is_ascii_digit())
                {
                    self.push("E");
                }
            }
            Key::Recip => {
                let inner = self.ok_answer().unwrap_or_else(|| self.entry.clone());
                self.answer = None;
                self.fresh = false;
                self.entry = if inner.is_empty() { String::from("1÷") } else { alloc::format!("1÷({inner})") };
            }
            Key::Negate => self.negate(),
            Key::Equals => self.equals(),
            Key::Clear => {
                self.entry.clear();
                self.answer = None;
                self.fresh = false;
            }
            Key::Back => self.back(),
            k => {
                // A function: around the answer, or opened in the sum.
                let f = k.function().unwrap_or("(");
                match self.ok_answer() {
                    Some(v) => {
                        self.entry = alloc::format!("{f}{v})");
                        self.answer = None;
                        self.fresh = false;
                    }
                    None => {
                        self.start();
                        self.push(f);
                    }
                }
            }
        }
    }

    /// ±: the last number's sign turned over (or the whole sum's).
    fn negate(&mut self) {
        self.go_on();
        let Some(i) = self.last_number() else {
            self.entry =
                if self.entry.is_empty() { String::from("−") } else { alloc::format!("−({})", self.entry) };
            return;
        };
        let before = self.entry[..i].chars().last();
        let sign_at = i - before.map_or(0, char::len_utf8);
        let is_sign =
            before == Some('−') && self.entry[..sign_at].chars().last().is_none_or(|c| BINARY.contains(&c) || c == '(');
        if is_sign {
            self.entry.replace_range(sign_at..i, "");
        } else {
            self.entry.insert(i, '−');
        }
    }

    fn equals(&mut self) {
        if self.fresh || self.entry.is_empty() {
            return;
        }
        match expr::show(&self.entry, self.angle) {
            Ok(v) => {
                self.history.insert(0, (self.entry.clone(), v.clone()));
                self.history.truncate(HISTORY);
                self.answer = Some(Ok(v));
            }
            Err(e) => self.answer = Some(Err(e.words())),
        }
        self.fresh = true;
    }

    fn back(&mut self) {
        if self.fresh {
            self.answer = None;
            self.fresh = false;
            return;
        }
        for f in ["asin(", "acos(", "atan(", "sin(", "cos(", "tan(", "ln(", "log(", "exp(", "abs(", "√(", "^2"] {
            if self.entry.ends_with(f) {
                self.entry.truncate(self.entry.len() - f.len());
                return;
            }
        }
        self.entry.pop();
    }

    /// A character typed on the keyboard.
    pub fn typed(&mut self, c: char) {
        match c {
            '0'..='9' => self.press(Key::Digit(c as u8 - b'0')),
            '.' | ',' => self.press(Key::Point),
            '+' => self.press(Key::Plus),
            '-' | '−' => self.press(Key::Minus),
            '*' | '×' | 'x' | 'X' => self.press(Key::Times),
            '/' | '÷' => self.press(Key::Divide),
            '^' => self.press(Key::Power),
            '(' => self.press(Key::Open),
            ')' => self.press(Key::Close),
            '%' => self.press(Key::Percent),
            '!' => self.press(Key::Fact),
            '=' | '\n' | '\r' => self.press(Key::Equals),
            'π' => self.press(Key::Pi),
            '√' => self.press(Key::Root),
            'E' => self.press(Key::Exp10),
            // Function names, typed whole (sin, ln …) and e.
            'a'..='z' => {
                self.start();
                let mut b = [0u8; 4];
                self.push(c.encode_utf8(&mut b));
            }
            _ => {}
        }
    }

    /// Text pasted: each character as if typed.
    pub fn paste(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_whitespace()) {
            self.typed(c);
        }
    }

    /// The sum's value while it's typed (when it has one, and it isn't just
    /// the number typed).
    pub fn preview(&self) -> Option<String> {
        if self.fresh || self.entry.is_empty() {
            return None;
        }
        expr::show(&self.entry, self.angle).ok().filter(|v| *v != self.entry)
    }

    /// The big line: the answer (or why there's none), else the sum.
    pub fn shown(&self) -> String {
        match (&self.answer, self.fresh) {
            (Some(Ok(v)), true) => v.clone(),
            (Some(Err(w)), true) => w.to_string(),
            _ if self.entry.is_empty() => String::from("0"),
            _ => self.entry.clone(),
        }
    }

    /// What Copy takes: the answer, else the sum's value, else the sum.
    pub fn copy_text(&self) -> String {
        self.ok_answer().or_else(|| self.preview()).unwrap_or_else(|| self.entry.clone())
    }

    /// A sum from the history, shown with its answer.
    pub fn recall(&mut self, i: usize) {
        if let Some((sum, v)) = self.history.get(i).cloned() {
            self.entry = sum;
            self.answer = Some(Ok(v));
            self.fresh = true;
        }
    }

    pub fn toggle_angle(&mut self) {
        self.angle = if self.angle == Angle::Degrees { Angle::Radians } else { Angle::Degrees };
        // An answer worked out in the other unit no longer stands.
        if self.fresh {
            self.fresh = false;
            self.answer = None;
        }
    }
}

#[cfg(test)]
mod tests;
