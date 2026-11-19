// SPDX-License-Identifier: Apache-2.0
use super::*;

fn keys(c: &mut Calc, ks: &[Key]) {
    for &k in ks {
        c.press(k);
    }
}

fn typed(text: &str) -> Calc {
    let mut c = Calc::new();
    c.paste(text);
    c
}

#[test]
fn a_tenth_and_two_tenths_by_the_keys() {
    let mut c = Calc::new();
    keys(&mut c, &[Key::Point, Key::Digit(1), Key::Plus, Key::Point, Key::Digit(2)]);
    assert_eq!(c.entry, "0.1+0.2");
    assert_eq!(c.preview().as_deref(), Some("0.3"));
    c.press(Key::Equals);
    assert_eq!(c.shown(), "0.3");
    assert_eq!(c.history, [(String::from("0.1+0.2"), String::from("0.3"))]);
}

#[test]
fn after_equals_a_number_starts_again_and_an_operator_goes_on() {
    let mut c = typed("6×7=");
    assert_eq!(c.shown(), "42");
    c.press(Key::Digit(5));
    assert_eq!(c.entry, "5");
    let mut c = typed("6×7=");
    c.press(Key::Plus);
    c.press(Key::Digit(1));
    assert_eq!(c.entry, "42+1");
    c.press(Key::Equals);
    assert_eq!(c.shown(), "43");
}

#[test]
fn operators_replace_each_other_and_a_minus_after_one_is_a_sign() {
    let mut c = Calc::new();
    keys(&mut c, &[Key::Digit(2), Key::Plus, Key::Times]);
    assert_eq!(c.entry, "2×");
    c.press(Key::Minus);
    assert_eq!(c.entry, "2×−");
    c.press(Key::Plus);
    assert_eq!(c.entry, "2+", "both replaced");
    let mut c = Calc::new();
    c.press(Key::Times);
    assert_eq!(c.entry, "", "no sum starts with ×");
    c.press(Key::Minus);
    assert_eq!(c.entry, "−");
}

#[test]
fn one_point_a_number() {
    let mut c = typed("1.5.");
    assert_eq!(c.entry, "1.5");
    c.press(Key::Plus);
    c.press(Key::Point);
    assert_eq!(c.entry, "1.5+0.");
}

#[test]
fn negate_turns_the_last_numbers_sign() {
    let mut c = typed("2−3");
    c.press(Key::Negate);
    assert_eq!(c.entry, "2−−3");
    assert_eq!(c.preview().as_deref(), Some("5"));
    let mut c = typed("2×3");
    c.press(Key::Negate);
    assert_eq!(c.entry, "2×−3");
    c.press(Key::Negate);
    assert_eq!(c.entry, "2×3");
    let mut c = typed("5");
    c.press(Key::Negate);
    assert_eq!(c.entry, "−5");
    c.press(Key::Negate);
    assert_eq!(c.entry, "5");
    let mut c = typed("2E");
    c.press(Key::Minus);
    c.press(Key::Digit(3));
    assert_eq!(c.entry, "2E−3");
    c.press(Key::Negate);
    assert_eq!(c.entry, "−2E−3", "the exponent's sign is the number's");
}

#[test]
fn functions_open_or_wrap_the_answer() {
    let mut c = Calc::new();
    c.mode = Mode::Scientific;
    keys(&mut c, &[Key::Sin, Key::Digit(3), Key::Digit(0), Key::Close, Key::Equals]);
    assert_eq!(c.shown(), "0.5", "degrees by default");
    c.press(Key::Root);
    assert_eq!(c.entry, "√(0.5)");
    let mut c = typed("16=");
    c.press(Key::Root);
    c.press(Key::Equals);
    assert_eq!(c.shown(), "4");
    c.press(Key::Recip);
    c.press(Key::Equals);
    assert_eq!(c.shown(), "0.25");
}

#[test]
fn radians_and_degrees() {
    let mut c = typed("sin(π÷2)");
    c.toggle_angle();
    assert_eq!(c.angle, Angle::Radians);
    c.press(Key::Equals);
    assert_eq!(c.shown(), "1");
    c.toggle_angle();
    assert!(!c.answered(), "an answer in the other unit no longer stands");
}

#[test]
fn back_takes_a_function_whole() {
    let mut c = typed("2+");
    c.press(Key::Sin);
    assert_eq!(c.entry, "2+sin(");
    c.press(Key::Back);
    assert_eq!(c.entry, "2+");
    c.press(Key::Back);
    assert_eq!(c.entry, "2");
    let mut c = typed("3=");
    c.press(Key::Back);
    assert!(!c.answered());
    assert_eq!(c.entry, "3", "the sum back to edit");
}

#[test]
fn close_only_what_was_opened() {
    let mut c = typed("2×(3+4");
    c.press(Key::Close);
    c.press(Key::Close);
    assert_eq!(c.entry, "2×(3+4)");
    let mut c = typed("(");
    c.press(Key::Close);
    assert_eq!(c.entry, "(", "not straight after the opening");
}

#[test]
fn errors_say_why_and_keep_the_sum() {
    let mut c = typed("1÷0=");
    assert_eq!(c.shown(), "Can't divide by zero");
    assert!(c.history.is_empty());
    c.press(Key::Plus);
    assert_eq!(c.entry, "1÷0+", "an operator goes on from the sum");
}

#[test]
fn square_percent_and_factorial_follow_a_value() {
    let mut c = typed("3");
    keys(&mut c, &[Key::Square, Key::Equals]);
    assert_eq!(c.shown(), "9");
    let mut c = typed("200×15");
    c.press(Key::Percent);
    c.press(Key::Equals);
    assert_eq!(c.shown(), "30");
    let mut c = typed("+");
    c.press(Key::Fact);
    assert_eq!(c.entry, "", "nothing to follow");
    let c = typed("5!=");
    assert_eq!(c.shown(), "120");
}

#[test]
fn exp_belongs_to_a_number() {
    let mut c = typed("2");
    c.press(Key::Exp10);
    c.press(Key::Digit(3));
    c.press(Key::Equals);
    assert_eq!(c.shown(), "2000");
    let mut c = typed("2+");
    c.press(Key::Exp10);
    assert_eq!(c.entry, "2+");
}

#[test]
fn the_history_keeps_fifty_and_recalls() {
    let mut c = Calc::new();
    for i in 0..60 {
        c.paste(&alloc::format!("{i}+1="));
    }
    assert_eq!(c.history.len(), HISTORY);
    assert_eq!(c.history[0], (String::from("59+1"), String::from("60")));
    c.recall(1);
    assert_eq!((c.entry.as_str(), c.shown().as_str()), ("58+1", "59"));
    assert_eq!(c.copy_text(), "59");
}

#[test]
fn copy_takes_the_answer_or_the_value() {
    assert_eq!(typed("2+2").copy_text(), "4");
    assert_eq!(typed("7").copy_text(), "7");
    assert_eq!(typed("2+2=").copy_text(), "4");
}

#[test]
fn paste_types_each_character() {
    let c = typed(" 12 × 3 ");
    assert_eq!(c.entry, "12×3");
    let c = typed("abc");
    assert_eq!(c.entry, "abc");
    assert_eq!(c.preview(), None, "not a sum");
}

#[test]
fn a_sum_stops_at_its_longest() {
    let mut c = Calc::new();
    for _ in 0..400 {
        c.press(Key::Digit(9));
    }
    assert_eq!(c.entry.chars().count(), LONGEST);
}

#[test]
fn every_key_has_a_label_and_a_name() {
    let mut names = Vec::new();
    for k in STANDARD.iter().chain(SCIENTIFIC.iter()).flatten() {
        assert!(!k.label().is_empty());
        names.push(k.name());
    }
    let n = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), n, "names are unique");
    assert_eq!(n, 40);
}

#[test]
fn a_third_typed_shows_thirty_two_digits() {
    let mut c = Calc::new();
    for ch in "1/3".chars() {
        c.typed(ch);
        let _ = c.preview();
        let _ = c.shown();
    }
    c.press(Key::Equals);
    assert_eq!(c.shown(), "0.33333333333333333333333333333333");
}

#[test]
fn a_sum_typed_after_a_result_starts_afresh() {
    let mut c = Calc::new();
    for ch in "0.1+0.2".chars() {
        c.typed(ch);
    }
    c.press(Key::Equals);
    assert_eq!(c.shown(), "0.3");
    for ch in "1/3".chars() {
        c.typed(ch);
        let _ = c.preview();
    }
    assert_eq!(c.shown(), "1÷3", "typed after a result");
    c.press(Key::Equals);
    assert_eq!(c.shown(), "0.33333333333333333333333333333333");
}
