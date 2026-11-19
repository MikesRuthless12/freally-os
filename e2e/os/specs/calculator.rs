// SPDX-License-Identifier: Apache-2.0
//! `@apps.calculator` (TASK-112, FR-054): the Calculator opened from the
//! launcher, every key of both pads pressed in sums whose answers are
//! checked on the display (exact: `0.1 + 0.2` is `0.3`), degrees and
//! radians, the keyboard (typing, Enter, Backspace, Escape), Copy and Paste
//! through stage's clipboard (read back from stage), and the history
//! (recalled, cleared).

use crate::Ctx;
use crate::session::{expect_value, press, sel};
use crate::stage::key;
use crate::sweep::{click, q, until_attr, until_gone, until_q};

const SEARCH: &str = "[data-testid=\"launcher-search\"]";

fn tap(ctx: &mut Ctx, names: &[&str]) -> Result<(), String> {
    for n in names {
        let id = until_q(ctx, &sel(&format!("calc-key-{n}")))?;
        click(ctx, id)?;
    }
    Ok(())
}

fn shows(ctx: &mut Ctx, want: &str) -> Result<(), String> {
    expect_value(ctx, "calc-display", want)
}

/// `names` pressed after C, then `=`: the display shows `want`.
fn sum(ctx: &mut Ctx, names: &[&str], want: &str) -> Result<(), String> {
    tap(ctx, &["clear"])?;
    tap(ctx, names)?;
    tap(ctx, &["equals"])?;
    shows(ctx, want)
}

fn id(ctx: &mut Ctx, testid: &str) -> Result<(), String> {
    let n = until_q(ctx, &sel(testid))?;
    click(ctx, n)
}

fn held(ctx: &mut Ctx, modifier: &str, code: &str) -> Result<(), String> {
    key(ctx, modifier, true)?;
    press(ctx, code)?;
    key(ctx, modifier, false)
}

/// A sum typed on the keyboard (digits, + - * / . ^ ( ) and Enter for =).
fn type_sum(ctx: &mut Ctx, text: &str) -> Result<(), String> {
    for c in text.chars() {
        match c {
            '0'..='9' => press(ctx, &format!("Digit{c}"))?,
            '.' => press(ctx, "Period")?,
            '-' => press(ctx, "Minus")?,
            '/' => press(ctx, "Slash")?,
            '+' => held(ctx, "ShiftLeft", "Equal")?,
            '*' => held(ctx, "ShiftLeft", "Digit8")?,
            '^' => held(ctx, "ShiftLeft", "Digit6")?,
            '(' => held(ctx, "ShiftLeft", "Digit9")?,
            ')' => held(ctx, "ShiftLeft", "Digit0")?,
            '=' => press(ctx, "Enter")?,
            _ => return Err(format!("type_sum can't type {c:?}")),
        }
    }
    Ok(())
}

/// The Calculator, from the launcher's row.
fn open(ctx: &mut Ctx) -> Result<(), String> {
    if q(ctx, &sel("calc-display"))? != 0 {
        return Ok(());
    }
    let opened = ctx.log_count("calculator: open");
    key(ctx, "MetaLeft", true)?;
    key(ctx, "MetaLeft", false)?;
    until_attr(ctx, SEARCH, "data-focused", "true")?;
    id(ctx, "launcher-calculator")?;
    ctx.expect_log_count("calculator: open", opened + 1)?;
    until_q(ctx, &sel("calc-display"))?;
    Ok(())
}

pub fn calculator(ctx: &mut Ctx) -> Result<(), String> {
    open(ctx)?;
    shows(ctx, "0")?;
    // The standard pad: every digit, Back and C.
    tap(ctx, &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"])?;
    shows(ctx, "1234567890")?;
    tap(ctx, &["back"])?;
    shows(ctx, "123456789")?;
    tap(ctx, &["clear"])?;
    shows(ctx, "0")?;
    // Exact decimals, the operators, percent and the sign.
    sum(ctx, &["point", "1", "plus", "point", "2"], "0.3")?;
    expect_value(ctx, "calc-sum", "0.1+0.2 =")?;
    sum(ctx, &["9", "times", "8", "minus", "6", "divide", "3"], "70")?;
    sum(ctx, &["2", "0", "0", "times", "1", "5", "percent"], "30")?;
    sum(ctx, &["7", "negate"], "-7")?;
    sum(ctx, &["1", "divide", "3"], "0.33333333333333333333333333333333")?;
    sum(ctx, &["1", "divide", "0"], "Can't divide by zero")?;
    // The scientific pad (the window made again, wider), in degrees.
    let wider = ctx.log_count("calculator: scientific");
    id(ctx, "calc-mode-scientific")?;
    ctx.expect_log_count("calculator: scientific", wider + 1)?;
    until_q(ctx, &sel("calc-key-sin"))?;
    expect_value(ctx, "calc-angle", "degrees")?;
    sum(ctx, &["sin", "3", "0", "close"], "0.5")?;
    sum(ctx, &["tan", "4", "5", "close"], "1")?;
    sum(ctx, &["asin", "1", "close"], "90")?;
    sum(ctx, &["acos", "0", "close"], "90")?;
    sum(ctx, &["atan", "1", "close"], "45")?;
    // Radians.
    id(ctx, "calc-angle")?;
    expect_value(ctx, "calc-angle", "radians")?;
    sum(ctx, &["cos", "pi", "close"], "-1")?;
    sum(ctx, &["asin", "1", "close"], "1.5707963267948966192313216916398")?;
    id(ctx, "calc-angle")?;
    expect_value(ctx, "calc-angle", "degrees")?;
    sum(ctx, &["open", "2", "plus", "3", "close", "square"], "25")?;
    sum(ctx, &["2", "power", "1", "0"], "1024")?;
    sum(ctx, &["root", "1", "6", "close"], "4")?;
    sum(ctx, &["ln", "e", "close"], "1")?;
    sum(ctx, &["log", "1", "0", "0", "0", "close"], "3")?;
    sum(ctx, &["exp", "0", "close"], "1")?;
    sum(ctx, &["abs", "minus", "5", "close"], "5")?;
    sum(ctx, &["5", "fact"], "120")?;
    sum(ctx, &["2", "exp10", "3"], "2000")?;
    sum(ctx, &["4", "recip"], "0.25")?;
    sum(ctx, &["pi"], "3.1415926535897932384626433832795")?;
    sum(ctx, &["e"], "2.7182818284590452353602874713527")?;
    // The keyboard: typed, Enter, Backspace, Escape.
    press(ctx, "Escape")?;
    shows(ctx, "0")?;
    type_sum(ctx, "12*3=")?;
    shows(ctx, "36")?;
    type_sum(ctx, "2^100")?;
    press(ctx, "Backspace")?;
    type_sum(ctx, "=")?;
    shows(ctx, "1024")?;
    // Copy: the answer on stage's clipboard; Paste types it back.
    held(ctx, "ControlLeft", "KeyC")?;
    ctx.expect_log("calculator: copied")?;
    let c = crate::settings::clipboard(ctx)?;
    if c != "1024" {
        return Err(format!("the clipboard holds {c:?}, not \"1024\""));
    }
    press(ctx, "Escape")?;
    shows(ctx, "0")?;
    held(ctx, "ControlLeft", "KeyV")?;
    shows(ctx, "1024")?;
    type_sum(ctx, "/4=")?;
    shows(ctx, "256")?;
    // The history: newest first, recalled, cleared.
    id(ctx, "calc-history")?;
    expect_value(ctx, "calc-history-0", "256")?;
    expect_value(ctx, "calc-history-1", "1024")?;
    id(ctx, "calc-history-1")?;
    until_gone(ctx, &sel("calc-history-list"))?;
    shows(ctx, "1024")?;
    expect_value(ctx, "calc-sum", "2^10 =")?;
    id(ctx, "calc-history")?;
    id(ctx, "calc-history-clear")?;
    until_gone(ctx, &sel("calc-history-0"))?;
    id(ctx, "calc-history")?;
    until_q(ctx, &sel("calc-key-equals"))?;
    // Back to the standard pad, and closed.
    let narrower = ctx.log_count("calculator: standard");
    id(ctx, "calc-mode-standard")?;
    ctx.expect_log_count("calculator: standard", narrower + 1)?;
    until_gone(ctx, &sel("calc-key-sin"))?;
    held(ctx, "AltLeft", "F4")?;
    until_gone(ctx, &sel("calc-display"))?;
    ctx.expect_log("calculator: closed")
}
