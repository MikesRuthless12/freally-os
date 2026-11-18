// SPDX-License-Identifier: Apache-2.0
//! The windows' rules, each worked out by hand on a 1000 × 800 screen with a
//! 32 px title strip.

use super::*;
use alloc::vec;

const ME: (u32, &str) = (1, "test");

fn screen() -> Windows {
    Windows::new(1000, 800, 32)
}

fn normal(ws: &mut Windows, title: &str, w: u32, h: u32) -> WindowId {
    ws.create(title, Kind::Normal, (w, h), (0, 0), (0, 0), ME)
}

#[test]
fn a_normal_window_is_centred_with_its_strip_above_its_surface() {
    let mut ws = screen();
    let a = normal(&mut ws, "a", 400, 300);
    let w = ws.get(a).unwrap();
    // Frame 400 × 332, centred: (300, 234).
    assert_eq!(w.frame, Rect::new(300, 234, 400, 332));
    assert_eq!(ws.surface_rect(w), Rect::new(300, 266, 400, 300));
    assert_eq!(ws.strip_rect(w), Some(Rect::new(300, 234, 400, 32)));
    assert_eq!(ws.close_rect(w), Some(Rect::new(668, 234, 32, 32)));
    assert_eq!(ws.focused(), Some(a), "a new window takes focus");
}

#[test]
fn the_next_ones_cascade() {
    let mut ws = screen();
    normal(&mut ws, "a", 400, 300);
    let b = normal(&mut ws, "b", 400, 300);
    assert_eq!(ws.get(b).unwrap().frame.x, 332);
    assert_eq!(ws.get(b).unwrap().frame.y, 266);
}

#[test]
fn sizes_keep_their_minimum_and_the_screen() {
    let mut ws = screen();
    let a = ws.create("a", Kind::Normal, (10, 10), (200, 150), (0, 0), ME);
    assert_eq!((ws.get(a).unwrap().w, ws.get(a).unwrap().h), (200, 150));
    let b = normal(&mut ws, "b", 5000, 5000);
    assert_eq!((ws.get(b).unwrap().w, ws.get(b).unwrap().h), (1000, 800));
    let f = ws.create("f", Kind::Fullscreen, (1, 1), (0, 0), (0, 0), ME);
    assert_eq!(ws.get(f).unwrap().frame, Rect::new(0, 0, 1000, 800));
}

#[test]
fn layers_hold_panels_below_and_popups_above() {
    let mut ws = screen();
    let wall = ws.create("wall", Kind::Panel, (1000, 800), (0, 0), (0, 0), ME);
    let a = normal(&mut ws, "a", 300, 200);
    let pop = ws.create("pop", Kind::Popup, (100, 50), (0, 0), (10, 20), ME);
    let b = normal(&mut ws, "b", 300, 200);
    let ids: Vec<WindowId> = ws.stack().iter().map(|w| w.id).collect();
    assert_eq!(ids, vec![wall, a, b, pop], "b goes under the popup");
    ws.focus(wall);
    assert_eq!(ws.stack()[0].id, wall, "a panel raised stays at the bottom");
    assert_eq!(ws.get(pop).unwrap().frame, Rect::new(10, 20, 100, 50), "a popup is where its client put it");
    let rack = ws.create("rack", Kind::Panel, (1000, 56), (0, 0), (0, 744), ME);
    assert_eq!(ws.stack()[1].id, rack);
    assert_eq!(ws.focused(), Some(wall), "a new panel doesn't take focus");
}

#[test]
fn focus_raises_and_closing_gives_focus_back() {
    let mut ws = screen();
    let a = normal(&mut ws, "a", 300, 200);
    let b = normal(&mut ws, "b", 300, 200);
    let c = normal(&mut ws, "c", 300, 200);
    let changed = ws.focus(a);
    assert_eq!(ws.stack().last().unwrap().id, a);
    assert_eq!(changed, ws.get(a).unwrap().frame.union(&ws.get(c).unwrap().frame), "both looks change");
    // Most recent: a, c, b. Closing a gives focus to c.
    let gone = ws.close(a).unwrap();
    assert_eq!(ws.focused(), Some(c));
    assert!(gone.w > 0);
    ws.close(c);
    assert_eq!(ws.focused(), Some(b));
    ws.close(b);
    assert_eq!(ws.focused(), None);
    assert_eq!(ws.close(b), None, "closing twice");
}

#[test]
fn moving_keeps_the_strip_reachable() {
    let mut ws = screen();
    let a = normal(&mut ws, "a", 400, 300);
    let area = ws.move_to(a, 10, 20);
    assert_eq!(ws.get(a).unwrap().frame, Rect::new(10, 20, 400, 332));
    assert_eq!(area, Rect::new(10, 20, 690, 546), "old and new frames");
    ws.move_to(a, -5000, -50);
    assert_eq!((ws.get(a).unwrap().frame.x, ws.get(a).unwrap().frame.y), (48 - 400, 0));
    ws.move_to(a, 5000, 5000);
    assert_eq!((ws.get(a).unwrap().frame.x, ws.get(a).unwrap().frame.y), (1000 - 48, 800 - 32));
}

#[test]
fn hits_find_the_close_button_the_strip_and_the_surface() {
    let mut ws = screen();
    let a = normal(&mut ws, "a", 400, 300);
    let b = normal(&mut ws, "b", 400, 300); // at (332, 266)
    assert_eq!(ws.hit(690, 240), Some((a, Part::Close)));
    assert_eq!(ws.hit(400, 240), Some((a, Part::Strip)));
    assert_eq!(ws.hit(400, 280), Some((b, Part::Strip)), "b is on top there");
    assert_eq!(ws.hit(340, 310), Some((b, Part::Surface { x: 8, y: 12 })));
    assert_eq!(ws.hit(5, 5), None);
}

#[test]
fn alt_tab_walks_normal_windows_most_recent_first() {
    let mut ws = screen();
    ws.create("wall", Kind::Panel, (1000, 800), (0, 0), (0, 0), ME);
    let a = normal(&mut ws, "a", 300, 200);
    let b = normal(&mut ws, "b", 300, 200);
    let c = normal(&mut ws, "c", 300, 200);
    ws.focus(a);
    assert_eq!(ws.cycle_order(), vec![a, c, b]);
}

#[test]
fn titles_change_and_windows_belong_to_their_connection() {
    let mut ws = screen();
    let a = normal(&mut ws, "a", 300, 200);
    let b = ws.create("b", Kind::Normal, (300, 200), (0, 0), (0, 0), (2, "other"));
    let strip = ws.set_title(a, "Terminal").unwrap();
    assert_eq!(ws.get(a).unwrap().title, "Terminal");
    assert_eq!(strip.h, 32);
    assert_eq!(ws.owned_by(1), vec![a]);
    assert_eq!(ws.owned_by(2), vec![b]);
}

#[test]
fn rects_intersect_and_unite() {
    let a = Rect::new(0, 0, 10, 10);
    let b = Rect::new(5, 5, 10, 10);
    assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
    assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
    assert!(a.intersect(&Rect::new(20, 20, 1, 1)).is_empty());
    assert_eq!(Rect::default().union(&b), b);
}

#[test]
fn a_tooltip_sits_on_top_never_focused_and_the_pointer_passes_through() {
    let mut ws = screen();
    let a = normal(&mut ws, "a", 400, 300); // (300, 234)
    let tip = ws.create("tip", Kind::Tooltip, (80, 24), (0, 0), (350, 300), ME);
    assert_eq!(ws.stack().last().unwrap().id, tip, "on top");
    assert_eq!(ws.focused(), Some(a), "never focused");
    assert_eq!(ws.hit(360, 310), Some((a, Part::Surface { x: 60, y: 44 })), "the pointer passes through");
    assert!(!ws.cycle_order().contains(&tip));
}
