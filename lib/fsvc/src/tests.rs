// SPDX-License-Identifier: Apache-2.0
//! The manifest rules (TASK-037's verify): each bad manifest is refused, with its line.

extern crate std;

use super::*;
use alloc::string::String;

const GOOD: &str = "\
# The display.
name        = stage                       # [a-z0-9-]{1,32}, unique
binary      = /system/services/stage
description = Draws the desktop and every window, and sends your keyboard and mouse to the right one.
start       = boot
after       = logger, devmgr
restart     = always
class       = interactive
grants      = framebuffer, input
flavours    = release, dev, e2e
handoff     = true
";

fn with(key: &str, value: &str) -> String {
    GOOD.lines()
        .map(|l| if l.starts_with(key) { alloc::format!("{key} = {value}") } else { l.into() })
        .collect::<alloc::vec::Vec<_>>()
        .join("\n")
}

fn without(key: &str) -> String {
    GOOD.lines().filter(|l| !l.starts_with(key)).collect::<alloc::vec::Vec<_>>().join("\n")
}

#[test]
fn the_prd_example_parses() {
    let m = parse(GOOD).unwrap();
    assert_eq!(m.name, "stage");
    assert_eq!(m.after, ["logger", "devmgr"]);
    assert_eq!(m.class, Class::Interactive);
    assert_eq!(m.grants, ["framebuffer", "input"]);
    assert_eq!(m.flavours, [Flavour::Release, Flavour::Dev, Flavour::E2e]);
    assert!(m.handoff);
}

#[test]
fn a_missing_description_is_refused() {
    assert_eq!(parse(&without("description")).unwrap_err().what, "no description");
}

#[test]
fn a_121_character_description_is_refused() {
    let d = alloc::format!("{}.", "a".repeat(120));
    assert_eq!(d.chars().count(), 121);
    let e = parse(&with("description", &d)).unwrap_err();
    assert!(e.what.contains("121 characters"), "{e}");
    // 120 is fine.
    assert!(parse(&with("description", &alloc::format!("{}.", "a".repeat(119)))).is_ok());
}

#[test]
fn a_description_without_a_full_stop_is_refused() {
    let e = parse(&with("description", "Draws the desktop")).unwrap_err();
    assert_eq!(e.what, "the description doesn't end with a full stop");
    assert_eq!(e.line, 4);
}

#[test]
fn an_unknown_key_is_refused() {
    let e = parse(&alloc::format!("{GOOD}colour = green\n")).unwrap_err();
    assert_eq!(e.what, "unknown key \"colour\"");
    assert_eq!(e.line, 12);
}

#[test]
fn other_mistakes_are_refused() {
    assert!(parse(&alloc::format!("{GOOD}name = again\n")).unwrap_err().what.contains("twice"));
    assert!(parse(&with("grants", "framebuffer, everything")).unwrap_err().what.contains("unknown grant"));
    assert!(parse(&with("name", "Stage")).is_err());
    assert!(parse(&with("binary", "/home/stage")).is_err());
    assert!(parse(&with("restart", "sometimes")).is_err());
    assert!(parse(&with("flavours", "release, beta")).is_err());
    assert!(parse("this line has no equals sign\n").is_err());
    // brain and cdpd never ship in release.
    let brain = with("name", "brain");
    assert!(parse(&brain).unwrap_err().what.contains("never be in a release image"));
    assert!(parse(&brain.replace("release, dev, e2e", "dev")).is_ok());
}

fn named(name: &str, after: &[&str]) -> Manifest {
    let mut m = parse(GOOD).unwrap();
    m.name = name.into();
    m.binary = alloc::format!("/system/services/{name}");
    m.after = after.iter().map(|s| String::from(*s)).collect();
    m
}

#[test]
fn a_set_starts_in_dependency_order() {
    let set = [named("stage", &["logger", "devmgr"]), named("devmgr", &["logger"]), named("logger", &[])];
    let order = check_set(&set).unwrap();
    let at = |n: &str| order.iter().position(|x| x == n).unwrap();
    assert!(at("logger") < at("devmgr") && at("devmgr") < at("stage"));
}

#[test]
fn cycles_and_unknown_dependencies_are_refused() {
    let e = check_set(&[named("a", &["b"]), named("b", &["c"]), named("c", &["a"])]).unwrap_err();
    assert!(e.starts_with("a dependency cycle: a → b → c → a"), "{e}");
    assert!(check_set(&[named("a", &["a"])]).unwrap_err().contains("cycle"));
    assert!(check_set(&[named("a", &["ghost"])]).unwrap_err().contains("ghost"));
    assert!(check_set(&[named("a", &[]), named("a", &[])]).unwrap_err().contains("two manifests"));
}
