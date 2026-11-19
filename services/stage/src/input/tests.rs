// SPDX-License-Identifier: Apache-2.0
//! Routing, worked out by hand on a 1000 × 800 screen (the pointer starts
//! at (500, 400)) with a 32 px title strip.

use super::*;
use crate::shortcuts::{mods, usage};
use alloc::string::String;
use alloc::vec;

fn desk() -> Desk {
    Desk::new(Windows::new(1000, 800, 32))
}

fn open(d: &mut Desk, w: u32, h: u32) -> WindowId {
    d.create("w", Kind::Normal, (w, h), (0, 0), (0, 0), (1, "t"), &mut Vec::new())
}

fn key(usage: u16, down: bool, modifiers: u8) -> Key {
    Key { usage, down, ch: 0, modifiers }
}

fn sends(out: &[Effect]) -> Vec<(WindowId, Event)> {
    out.iter()
        .filter_map(|e| match e {
            Effect::Send(id, ev) => Some((*id, ev.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_pointer_starts_centred_and_stays_on_screen() {
    let mut d = desk();
    assert_eq!((d.x, d.y), (500, 400));
    d.mouse(-9000, 9000, 0, 0, &mut Vec::new());
    assert_eq!((d.x, d.y), (0, 799));
}

#[test]
fn dragging_the_strip_moves_the_window() {
    let mut d = desk();
    let a = open(&mut d, 400, 300); // frame (300, 234) 400 × 332
    let mut out = Vec::new();
    d.mouse(0, -150, 0, 0, &mut out); // (500, 250): on the strip
    d.mouse(0, 0, 0, 1, &mut out);
    d.mouse(-100, 50, 0, 1, &mut out);
    assert_eq!(d.windows.get(a).unwrap().frame, Rect::new(200, 284, 400, 332));
    assert!(out.iter().any(|e| matches!(e, Effect::Damage(r) if *r == Rect::new(200, 234, 500, 382))));
    d.mouse(0, 0, 0, 0, &mut out);
    d.mouse(50, 0, 0, 0, &mut out);
    assert_eq!(d.windows.get(a).unwrap().frame.x, 200, "let go: it stays");
    assert!(sends(&out).iter().all(|(_, e)| !matches!(e, Event::Pointer(_))), "a drag isn't the window's");
}

#[test]
fn pressing_a_window_raises_and_focuses_it() {
    let mut d = desk();
    let a = open(&mut d, 400, 300); // (300, 234)
    let b = open(&mut d, 400, 300); // (332, 266)
    let mut out = Vec::new();
    d.mouse(-190, -100, 0, 0, &mut out); // (310, 300): a's surface, not b's
    d.mouse(0, 0, 0, 1, &mut out);
    assert_eq!(d.windows.focused(), Some(a));
    assert_eq!(d.windows.stack().last().unwrap().id, a);
    let s = sends(&out);
    assert!(s.contains(&(b, Event::Focus(false))) && s.contains(&(a, Event::Focus(true))));
    assert!(
        s.contains(&(a, Event::Pointer(Pointer { x: 10, y: 34, buttons: 1, kind: PointerKind::Down, dx: 0, dy: 0 })))
    );
}

#[test]
fn a_held_button_keeps_the_pointer_with_its_window() {
    let mut d = desk();
    let a = open(&mut d, 400, 300); // surface at (300, 266)
    let mut out = Vec::new();
    d.mouse(0, 0, 0, 1, &mut out); // (500, 400): inside
    out.clear();
    d.mouse(-400, 0, 0, 1, &mut out); // (100, 400): outside it
    assert_eq!(
        sends(&out),
        vec![(a, Event::Pointer(Pointer { x: -200, y: 134, buttons: 1, kind: PointerKind::Move, dx: -400, dy: 0 }))]
    );
    out.clear();
    d.mouse(0, 0, 0, 0, &mut out);
    assert!(matches!(sends(&out)[0].1, Event::Pointer(Pointer { kind: PointerKind::Up, .. })));
    out.clear();
    d.mouse(5, 0, 0, 0, &mut out);
    assert!(sends(&out).is_empty(), "let go and outside: nobody hears it");
}

#[test]
fn the_close_button_asks_the_window_to_close() {
    let mut d = desk();
    let a = open(&mut d, 400, 300); // close button (668, 234) 32 × 32
    let mut out = Vec::new();
    d.mouse(180, -150, 0, 0, &mut out); // (680, 250)
    d.mouse(0, 0, 0, 1, &mut out);
    assert!(sends(&out).contains(&(a, Event::CloseRequested)));
    assert!(d.windows.get(a).is_some(), "asked, not closed: the client decides");
}

#[test]
fn keys_go_to_the_focused_window_unless_they_re_shortcuts() {
    let mut d = desk();
    let a = open(&mut d, 300, 200);
    let mut out = Vec::new();
    d.key(key(0x04, true, 0), &mut out);
    assert_eq!(sends(&out), vec![(a, Event::Key(key(0x04, true, 0)))]);
    out.clear();
    d.key(key(usage::LEFT_META, true, mods::META), &mut out);
    d.key(key(usage::M, true, mods::META), &mut out);
    assert_eq!(out, vec![Effect::Action(Action::Monitor)]);
    out.clear();
    d.key(key(usage::M, false, mods::META), &mut out);
    d.key(key(usage::LEFT_META, false, 0), &mut out);
    assert!(out.is_empty(), "the shortcut's releases are swallowed");
    d.key(key(usage::F4, true, mods::ALT), &mut out);
    assert_eq!(sends(&out), vec![(a, Event::CloseRequested)]);
}

#[test]
fn alt_tab_steps_through_and_keeps_where_it_stopped() {
    let mut d = desk();
    let a = open(&mut d, 300, 200);
    let b = open(&mut d, 300, 200);
    let c = open(&mut d, 300, 200); // most recent: c, b, a
    let mut out = Vec::new();
    d.key(key(usage::LEFT_ALT, true, mods::ALT), &mut out);
    d.key(key(usage::TAB, true, mods::ALT), &mut out);
    assert_eq!(d.windows.focused(), Some(b));
    d.key(key(usage::TAB, false, mods::ALT), &mut out);
    d.key(key(usage::TAB, true, mods::ALT), &mut out);
    assert_eq!(d.windows.focused(), Some(a), "a second Tab goes on, not back");
    d.key(key(usage::LEFT_ALT, false, 0), &mut out);
    // A fresh Alt+Tab starts from the new order: a, b, c.
    d.key(key(usage::TAB, true, mods::ALT), &mut out);
    assert_eq!(d.windows.focused(), Some(b));
    d.key(key(usage::TAB, true, mods::ALT | mods::SHIFT), &mut out);
    assert_eq!(d.windows.focused(), Some(a), "Shift steps back");
    let _ = c;
}

#[test]
fn closing_tells_the_next_window_it_has_focus() {
    let mut d = desk();
    let a = open(&mut d, 300, 200);
    let b = open(&mut d, 300, 200);
    let mut out = Vec::new();
    d.close(b, &mut out);
    assert!(sends(&out).contains(&(a, Event::Focus(true))));
    assert!(out.iter().any(|e| matches!(e, Effect::Damage(_))));
}

#[test]
fn a_panel_hears_the_pointer_but_keeps_its_place() {
    let mut d = desk();
    let wall = d.create("wall", Kind::Panel, (1000, 800), (0, 0), (0, 0), (1, "desktop"), &mut Vec::new());
    let a = open(&mut d, 100, 100);
    let mut out = Vec::new();
    d.mouse(-400, -300, 0, 0, &mut out); // (100, 100): the wall only
    d.mouse(0, 0, 0, 1, &mut out);
    assert_eq!(d.windows.focused(), Some(a), "the wall doesn't take focus");
    assert!(
        matches!(sends(&out).last(), Some((w, Event::Pointer(Pointer { kind: PointerKind::Down, .. }))) if *w == wall)
    );
}

#[test]
fn a_window_hears_when_the_pointer_leaves_it() {
    let mut d = desk();
    let a = open(&mut d, 400, 300); // surface (300, 266) 400 × 300
    let mut out = Vec::new();
    d.mouse(0, 0, 0, 0, &mut out);
    d.mouse(1, 0, 0, 0, &mut out); // inside: a hears the move
    out.clear();
    d.mouse(-450, 0, 0, 0, &mut out); // off it
    assert_eq!(
        sends(&out),
        vec![(a, Event::Pointer(Pointer { x: -249, y: 134, buttons: 0, kind: PointerKind::Leave, dx: 0, dy: 0 }))]
    );
    out.clear();
    d.mouse(-5, 0, 0, 0, &mut out);
    assert!(sends(&out).is_empty(), "told once");
    // Onto its strip: off its surface, so it hears that too.
    d.mouse(500, -150, 0, 0, &mut out); // (546, 250): the strip
    assert!(sends(&out).is_empty(), "never was over it since");
}

#[test]
fn what_a_window_carries_is_dropped_on_the_window_under_the_pointer() {
    let mut d = desk();
    let a = open(&mut d, 400, 300); // surface at (300, 266)
    let mut out = Vec::new();
    let b = d.create("b", Kind::Popup, (100, 100), (0, 0), (10, 10), (2, "t"), &mut out);
    d.mouse(0, 0, 0, 1, &mut out); // (500, 400): down on a
    assert!(!d.carry(b, String::from("no")), "only the window the button went down on carries");
    assert!(d.carry(a, String::from("/home/mike/x")));
    out.clear();
    d.mouse(-440, -340, 0, 1, &mut out); // (60, 60): over b
    d.mouse(0, 0, 0, 0, &mut out); // let go over b
    let s = sends(&out);
    let dropped = DropEvent { x: 50, y: 50, data: String::from("/home/mike/x") };
    assert!(s.contains(&(b, Event::Drop(dropped))), "{s:?}");
    assert!(s.iter().any(|(id, e)| *id == a && matches!(e, Event::Pointer(Pointer { kind: PointerKind::Up, .. }))));
    // Carried and let go over its own window: no drop; nothing carried after.
    out.clear();
    d.mouse(440, 340, 0, 1, &mut out); // (500, 400): down on a
    assert!(d.carry(a, String::from("/home/mike/y")));
    d.mouse(0, 0, 0, 0, &mut out);
    assert!(!sends(&out).iter().any(|(_, e)| matches!(e, Event::Drop(_))));
    assert!(!d.carry(a, String::from("late")), "no button held: nothing to carry");
}
