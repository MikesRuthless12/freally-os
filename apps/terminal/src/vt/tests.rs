// SPDX-License-Identifier: Apache-2.0
//! The parser, sequence by sequence.

use super::*;
use alloc::vec;

fn parse(bytes: &[u8]) -> Vec<Action> {
    let mut p = Parser::new();
    let mut out = Vec::new();
    p.feed(bytes, &mut |a| out.push(a));
    out
}

fn csi(private: Option<u8>, params: &[Option<u32>], final_byte: u8) -> Action {
    Action::Csi {
        private,
        params: params.iter().map(|&v| Param { value: v, sub: vec![] }).collect(),
        intermediates: vec![],
        final_byte,
    }
}

#[test]
fn text_and_utf8() {
    assert_eq!(parse(b"hi"), vec![Action::Print('h'), Action::Print('i')]);
    assert_eq!(parse("é→✓".as_bytes()), vec![Action::Print('é'), Action::Print('→'), Action::Print('✓')]);
    assert_eq!(parse(b"\xC3a"), vec![Action::Print('\u{FFFD}'), Action::Print('a')], "a broken sequence");
    assert_eq!(parse(b"\xFF"), vec![Action::Print('\u{FFFD}')]);
    assert_eq!(parse(b"\r\n\x07\x7F"), vec![Action::Execute(b'\r'), Action::Execute(b'\n'), Action::Execute(7)]);
}

#[test]
fn csi_with_parameters_defaults_and_a_private_marker() {
    assert_eq!(parse(b"\x1b[H"), vec![csi(None, &[], b'H')]);
    assert_eq!(parse(b"\x1b[12;40H"), vec![csi(None, &[Some(12), Some(40)], b'H')]);
    assert_eq!(parse(b"\x1b[;5H"), vec![csi(None, &[None, Some(5)], b'H')], "the first missing");
    assert_eq!(parse(b"\x1b[3;H"), vec![csi(None, &[Some(3), None], b'H')], "the second missing");
    assert_eq!(parse(b"\x1b[?1049h"), vec![csi(Some(b'?'), &[Some(1049)], b'h')]);
    assert_eq!(parse(b"\x1b[99999999A"), vec![csi(None, &[Some(65_535)], b'A')], "held to 65,535");
}

#[test]
fn sgr_sub_parameters() {
    let a = parse(b"\x1b[38:2::10:20:30;1m");
    let Action::Csi { params, final_byte: b'm', .. } = &a[0] else { panic!("{a:?}") };
    assert_eq!(params[0], Param { value: Some(38), sub: vec![Some(2), None, Some(10), Some(20), Some(30)] });
    assert_eq!(params[1], Param { value: Some(1), sub: vec![] });
}

#[test]
fn esc_sequences_and_controls_inside_csi() {
    assert_eq!(
        parse(b"\x1b7\x1b8"),
        vec![
            Action::Esc { intermediates: vec![], final_byte: b'7' },
            Action::Esc { intermediates: vec![], final_byte: b'8' }
        ]
    );
    assert_eq!(parse(b"\x1b(B"), vec![Action::Esc { intermediates: vec![b'('], final_byte: b'B' }]);
    // A control inside a CSI runs at once; the CSI goes on.
    assert_eq!(parse(b"\x1b[1\r0A"), vec![Action::Execute(b'\r'), csi(None, &[Some(10)], b'A')]);
}

#[test]
fn osc_ended_by_bel_or_st_and_dcs_swallowed() {
    assert_eq!(parse(b"\x1b]0;title\x07x"), vec![Action::Osc(b"0;title".to_vec()), Action::Print('x')]);
    assert_eq!(parse(b"\x1b]2;t\x1b\\y"), vec![Action::Osc(b"2;t".to_vec()), Action::Print('y')]);
    assert_eq!(parse(b"\x1bPq#0;1\x1b\\z"), vec![Action::Print('z')], "DCS swallowed");
}

#[test]
fn cancel_and_a_new_escape_restart() {
    assert_eq!(parse(b"\x1b[12\x18a"), vec![Action::Print('a')], "CAN abandons the sequence");
    assert_eq!(parse(b"\x1b[12\x1b[3B"), vec![csi(None, &[Some(3)], b'B')], "ESC starts again");
}

#[test]
fn counts_default_on_missing_or_zero() {
    let p = [Param { value: Some(0), sub: vec![] }, Param { value: Some(4), sub: vec![] }];
    assert_eq!(count(&p, 0, 1), 1);
    assert_eq!(count(&p, 1, 1), 4);
    assert_eq!(count(&p, 2, 1), 1);
}
