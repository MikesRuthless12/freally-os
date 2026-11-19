// SPDX-License-Identifier: Apache-2.0
use super::*;
use alloc::string::ToString;
use alloc::vec;

fn section(title: &str, n: usize) -> Section {
    let mut s = Section::new(title);
    for i in 0..n {
        s.line(alloc::format!("{title} {i}"));
    }
    s
}

fn heading(s: &str) -> Row {
    Row::Heading(s.to_string())
}

#[test]
fn sections_fill_columns_then_pages() {
    let p = pages(&[section("A", 3), section("B", 2)], 4, 2);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0][0], vec![heading("A"), Row::Line("A 0".into()), Row::Line("A 1".into()), Row::Line("A 2".into())]);
    assert_eq!(p[0][1], vec![heading("B"), Row::Line("B 0".into()), Row::Line("B 1".into())]);
}

#[test]
fn a_heading_never_ends_a_column() {
    // A fills three of four rows; B's heading would be alone at the foot.
    let p = pages(&[section("A", 2), section("B", 1)], 4, 3);
    assert_eq!(p[0][0].len(), 3);
    assert_eq!(p[0][1][0], heading("B"));
}

#[test]
fn a_long_section_runs_on_with_its_heading() {
    let p = pages(&[section("PCI", 7)], 4, 1);
    // 7 lines in columns of 4 rows (1 heading + 3 lines each): three columns,
    // one to a page.
    assert_eq!(p.len(), 3);
    assert_eq!(p[1][0][0], heading("PCI (continued)"));
    assert_eq!(p[2][0], vec![heading("PCI (continued)"), Row::Line("PCI 6".into())]);
    // Nothing lost.
    let lines: usize = p.iter().flatten().flatten().filter(|r| matches!(r, Row::Line(_))).count();
    assert_eq!(lines, 7);
}

#[test]
fn nothing_to_show_is_no_pages() {
    assert!(pages(&[], 10, 3).is_empty());
}

#[test]
fn lists_wrap() {
    let ids: Vec<String> = (0..5).map(|i| i.to_string()).collect();
    assert_eq!(wrap(&ids, 2), vec!["0  1", "2  3", "4"]);
}

#[test]
fn the_keyboard_verdict_names_the_decision() {
    assert!(keyboard_verdict(false, None).contains("USB driver first"));
    assert!(keyboard_verdict(true, Some("Running")).contains("the keyboard is PS/2"));
    assert!(keyboard_verdict(true, Some("Held")).contains("drv-i8042 is Held"));
    assert!(keyboard_verdict(true, Some("Starting")).contains("still starting"));
    assert!(keyboard_verdict(true, None).contains("hasn't started"));
}

#[test]
fn times_read_in_milliseconds() {
    assert_eq!(ms(4_123_456_789), "4123.4 ms");
    assert_eq!(ms(0), "0.0 ms");
}
