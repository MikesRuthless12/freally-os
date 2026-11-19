// SPDX-License-Identifier: Apache-2.0
use super::expr::{Angle, eval, show};
use super::*;

fn n(s: &str) -> Decimal {
    Decimal::parse(s).unwrap()
}

fn shown(src: &str) -> String {
    show(src, Angle::Radians).unwrap_or_else(|e| alloc::format!("!{e:?}"))
}

#[test]
fn a_tenth_and_two_tenths_are_three_tenths() {
    assert_eq!(n("0.1").add(&n("0.2")), n("0.3"));
    assert_eq!(shown("0.1+0.2"), "0.3");
    assert_eq!(shown("1-0.9"), "0.1");
    assert_eq!(shown("0.1×3"), "0.3");
}

#[test]
fn numbers_read_and_shown() {
    for (text, want) in [
        ("12.50", "12.5"),
        ("-0.003", "-0.003"),
        ("1E3", "1000"),
        ("2.5E-3", "0.0025"),
        ("0", "0"),
        ("-0", "0"),
        ("1E40", "1e+40"),
        ("1.25E-8", "1.25e-8"),
        ("0.0000001", "0.0000001"),
    ] {
        assert_eq!(n(text).show().unwrap(), want, "{text}");
    }
    assert_eq!(Decimal::parse("1.2.3"), Err(Error::Syntax).map(|_: ()| Decimal::zero()));
    assert!(Decimal::parse("").is_err());
    assert!(Decimal::parse("-").is_err());
}

#[test]
fn halves_round_to_even() {
    for (v, places, want) in
        [("2.5", 0, "2"), ("3.5", 0, "4"), ("2.51", 0, "3"), ("-2.5", 0, "-2"), ("0.125", 2, "0.12")]
    {
        assert_eq!(n(v).round_places(places).show().unwrap(), want, "{v}");
    }
    // Division sees past its guard digit: 0.1250001 is over the half.
    assert_eq!(n("1.0000008").div_to(&n("8"), 2).unwrap().show().unwrap(), "0.13");
    assert_eq!(n("1").div_to(&n("8"), 2).unwrap().show().unwrap(), "0.12");
}

#[test]
fn thirds_are_shown_to_32_digits() {
    assert_eq!(shown("1÷3"), "0.33333333333333333333333333333333");
    assert_eq!(shown("2÷3"), "0.66666666666666666666666666666667");
    assert_eq!(shown("1÷3×3"), "1");
    // A small quotient keeps its digits too.
    assert_eq!(shown("1E-30÷3"), "3.3333333333333333333333333333333e-31");
}

#[test]
fn the_usual_order() {
    for (src, want) in [
        ("2+3×4", "14"),
        ("(2+3)×4", "20"),
        ("-2^2", "-4"),
        ("(-2)^2", "4"),
        ("2^3^2", "512"),
        ("2^-1", "0.5"),
        ("2π÷π", "2"),
        ("3(4)", "12"),
        ("(1+2", "3"),
        ("√4+5", "7"),
        ("√(4+5)", "3"),
        ("5--3", "8"),
        ("2×-3", "-6"),
        ("3!²", "!Syntax"),
        ("10−4", "6"),
        ("8 ÷ 2 ÷ 2", "2"),
    ] {
        assert_eq!(shown(src), want, "{src}");
    }
}

#[test]
fn degrees_give_exact_halves_and_wholes() {
    for (src, want) in [
        ("sin(30)", "0.5"),
        ("sin(90)", "1"),
        ("sin(180)", "0"),
        ("sin(-90)", "-1"),
        ("cos(60)", "0.5"),
        ("cos(90)", "0"),
        ("cos(360)", "1"),
        ("tan(45)", "1"),
        ("sin(390)", "0.5"),
        ("asin(0.5)", "30"),
        ("atan(1)", "45"),
        ("acos(0)", "90"),
    ] {
        assert_eq!(show(src, Angle::Degrees).unwrap(), want, "{src}");
    }
    assert_eq!(eval("tan(90)", Angle::Degrees), Err(Error::Domain));
}

#[test]
fn every_error_says_why() {
    assert_eq!(eval("1÷0", Angle::Radians), Err(Error::DivideByZero));
    assert_eq!(eval("√(-1)", Angle::Radians), Err(Error::Domain));
    assert_eq!(eval("1E9999×100", Angle::Radians), Err(Error::Overflow));
    assert_eq!(eval("", Angle::Radians), Err(Error::Syntax));
    for e in [Error::DivideByZero, Error::Domain, Error::Overflow, Error::Syntax] {
        assert!(!e.words().is_empty());
    }
}

/// The 500 host sums (TASK-112): each answer worked out independently
/// (exact fractions and Python's decimal module at 90 digits, by
/// `tests/gen_cases.py`) and checked digit for digit.
#[test]
fn five_hundred_sums() {
    let mut wrong = Vec::new();
    let mut count = 0;
    for line in include_str!("tests/cases.txt").lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let mut parts = line.split('\t');
        let (mode, src, want) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let angle = if mode == "deg" { Angle::Degrees } else { Angle::Radians };
        let got = show(src, angle).unwrap_or_else(|e| alloc::format!("!{e:?}"));
        count += 1;
        if got != want {
            wrong.push(alloc::format!("{mode} {src}: {got}, not {want}"));
        }
    }
    assert_eq!(count, 500);
    assert!(wrong.is_empty(), "{} of 500 wrong:\n{}", wrong.len(), wrong.join("\n"));
}
