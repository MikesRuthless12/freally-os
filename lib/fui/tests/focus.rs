// SPDX-License-Identifier: Apache-2.0
//! Focus and keyboard navigation (TASK-056): Tab walks the controls in tree
//! order and wraps, Shift+Tab walks back, disabled controls and plain
//! structure are skipped; the focused control wears the focus ring. And
//! every control's keys: the Well, lists, tabs, menus, dialogs, meters.
#![cfg(feature = "ui")]

mod common;

use fimage::blit::Surface;
use fui::a11y::{Role, state};
use fui::controls::paint::{meter_cell_centre, meter_size, paint_meter};
use fui::controls::{
    Dialog, Done, Focus, Key, List, Menu, Press, Tabs, Well, Zone, key, meter_lit, meter_zone, tooltip_shown,
};
use fui::draw::focus_ring;
use fui::layout::Rect;
use fui::theme::{Mode, Theme, Tok};
use fui::tree::{Layout, Node, Tree};

fn control(role: Role, name: &str) -> Node {
    Node::new(Layout::Stack, role).name(name.into()).testid(name)
}

fn tab() -> Press {
    Press::key(key::TAB)
}

fn shift_tab() -> Press {
    Press { shift: true, ..Press::key(key::TAB) }
}

#[test]
fn tab_walks_the_controls_in_order_and_wraps() {
    let mut t = Tree::new(Node::new(Layout::Column, Role::Window).name("w".into()));
    let a = t.add(0, control(Role::Button, "a"));
    let row = t.add(0, Node::new(Layout::Row, Role::Group));
    let b = t.add(row, control(Role::Switch, "b"));
    t.add(row, control(Role::Text, "label"));
    let off = t.add(row, control(Role::Slider, "off"));
    t.node_mut(off).a11y.state |= state::DISABLED;
    let c = t.add(0, control(Role::TextField, "c"));
    let mut f = Focus::of(&t);
    assert_eq!(f.order, vec![a, b, c], "text, structure and the disabled slider take no focus");
    assert_eq!(f.current(), None);
    let walk: Vec<_> = (0..4).map(|_| (f.press(tab()), f.current().unwrap()).1).collect();
    assert_eq!(walk, vec![a, b, c, a], "wrapping");
    f.press(shift_tab());
    assert_eq!(f.current(), Some(c), "Shift+Tab walks back, wrapping");
    let mut g = Focus::of(&t);
    g.press(shift_tab());
    assert_eq!(g.current(), Some(c), "Shift+Tab from nothing starts at the end");
    assert_eq!(g.press(Press::key(key::DOWN)), Done::No, "only Tab is focus's key");
}

#[test]
fn the_focus_ring_is_two_pixels_outside_the_control() {
    let t = Theme::new(Mode::Dark, 1.0);
    let bg = t.colour(Tok::Surface).xrgb();
    let mut px = vec![bg; 60 * 40];
    let mut s = Surface { px: &mut px, width: 60, height: 40, stride: 60 };
    let r = Rect::new(15, 10, 30, 20);
    focus_ring(&mut s, r, 6.0, t.colour(Tok::Focus));
    let ring = t.colour(Tok::Focus).xrgb();
    // The band sits 2..4 px outside the control; the 2 px against it stay clear.
    assert_eq!(px[20 * 60 + 11], ring, "left of the control, in the band");
    assert_eq!(px[20 * 60 + 13], bg, "the gap between ring and control");
    assert_eq!(px[20 * 60 + 20], bg, "never over the control");
    assert_eq!(px[7 * 60 + 30], ring, "above it");
    assert_eq!(px[33 * 60 + 30], ring, "below it");
}

#[test]
fn the_well_edits_with_a_caret_and_a_selection() {
    let mut w = Well::default();
    for c in "héllo".chars() {
        w.press(Press { ch: Some(c), ..Press::default() });
    }
    assert_eq!((w.text.as_str(), w.caret), ("héllo", 5));
    w.press(Press::key(key::BACKSPACE));
    assert_eq!(w.text, "héll");
    w.press(Press::key(key::HOME));
    w.press(Press { shift: true, ..Press::key(key::RIGHT) });
    w.press(Press { shift: true, ..Press::key(key::RIGHT) });
    assert_eq!(w.selection(), Some((0, 2)));
    w.press(Press { ch: Some('J'), ..Press::default() });
    assert_eq!((w.text.as_str(), w.caret, w.selection()), ("Jll", 1, None), "typing replaces the selection");
    w.press(Press { ctrl: true, ..Press::key(key::A) });
    assert_eq!(w.selection(), Some((0, 3)));
    w.press(Press::key(key::DELETE));
    assert_eq!((w.text.as_str(), w.caret), ("", 0));
    w.press(Press::key(key::BACKSPACE));
    assert_eq!(w.text, "", "nothing to take");
    assert_eq!(w.press(Press { ch: Some('\u{7}'), ..Press::default() }), Done::No, "no control characters");
}

#[test]
fn a_left_or_right_ends_a_selection_at_its_side() {
    let mut w = Well { text: "abcdef".into(), caret: 4, anchor: Some(1) };
    w.press(Press::key(key::LEFT));
    assert_eq!((w.caret, w.selection()), (1, None));
    let mut w = Well { text: "abcdef".into(), caret: 1, anchor: Some(4) };
    w.press(Press::key(key::RIGHT));
    assert_eq!((w.caret, w.selection()), (4, None));
}

#[test]
fn lists_and_tables_move_their_selection() {
    let mut l = List { len: 30, selected: None, page: 10 };
    assert_eq!(l.press(Press::key(key::DOWN)), Done::Changed);
    assert_eq!(l.selected, Some(0));
    l.press(Press::key(key::PAGE_DOWN));
    assert_eq!(l.selected, Some(10));
    l.press(Press::key(key::END));
    assert_eq!(l.selected, Some(29));
    assert_eq!(l.press(Press::key(key::DOWN)), Done::Moved, "held at the end");
    l.press(Press::key(key::PAGE_UP));
    l.press(Press::key(key::PAGE_UP));
    l.press(Press::key(key::PAGE_UP));
    assert_eq!(l.selected, Some(0));
    assert_eq!(List::default().press(Press::key(key::DOWN)), Done::No, "an empty list");
}

#[test]
fn tabs_wrap_across_and_down() {
    let mut across = Tabs { count: 3, selected: 0, vertical: false };
    across.press(Press::key(key::LEFT));
    assert_eq!(across.selected, 2);
    across.press(Press::key(key::RIGHT));
    assert_eq!(across.selected, 0);
    assert_eq!(across.press(Press::key(key::DOWN)), Done::No, "across, Down is not a tab key");
    let mut down = Tabs { count: 4, selected: 3, vertical: true };
    down.press(Press::key(key::DOWN));
    assert_eq!(down.selected, 0, "the two-pane list wraps too");
    down.press(Press::key(key::END));
    assert_eq!(down.selected, 3);
}

#[test]
fn menus_open_move_choose_and_close() {
    let mut m = Menu { items: 3, ..Menu::default() };
    assert_eq!(m.press(Press::key(key::UP)), Done::No, "closed, Up does nothing");
    m.press(Press::key(key::ENTER));
    assert!(m.open);
    m.press(Press::key(key::UP));
    assert_eq!(m.highlighted, 2, "wrapping up from the first");
    assert_eq!(m.press(Press::key(key::ENTER)), Done::Activated);
    assert!(!m.open);
    m.press(Press::key(key::DOWN));
    assert_eq!(m.press(Press::key(key::ESCAPE)), Done::Closed);
    let mut d = Dialog { open: true };
    assert_eq!(d.press(Press::key(key::ENTER)), Done::No);
    assert_eq!(d.press(Press::key(key::ESCAPE)), Done::Closed, "Escape is Cancel");
    assert!(!d.open);
    assert_eq!(Key::default().press(Press::key(key::SPACE)), Done::Activated);
}

#[test]
fn meters_light_by_level_in_their_zones() {
    assert_eq!(meter_zone(-20.0), Zone::Green);
    assert_eq!(meter_zone(-12.0), Zone::Yellow);
    assert_eq!(meter_zone(-3.0), Zone::Red);
    assert_eq!((meter_lit(-60.0, 20), meter_lit(-6.0, 20), meter_lit(0.0, 20), meter_lit(9.0, 20)), (0, 18, 20, 20));
    let hw = common::dark();
    let (w, h) = meter_size(hw, 20);
    let mut px = vec![0u32; (w * h) as usize];
    let mut s = Surface { px: &mut px, width: w, height: h, stride: w };
    paint_meter(&mut s, hw, 0, 0, 20, -6.0);
    let cell = |i: u32| {
        let (x, y) = meter_cell_centre(hw, 0, 0, 20, i);
        px[(y as u32 * w + x as u32) as usize]
    };
    let rgb = |p: u32| (p >> 16 & 0xFF, p >> 8 & 0xFF, p & 0xFF);
    let (r0, g0, _) = rgb(cell(0));
    assert!(g0 > r0 + 40, "the bottom cell lit green: {:06x}", cell(0));
    let (r16, g16, b16) = rgb(cell(16));
    assert!(r16 > b16 + 60 && g16 > b16 + 60, "cell 16 (−10.5 dBFS) lit yellow: {:06x}", cell(16));
    assert!(common::luma(cell(18)) + 40 < common::luma(cell(16)), "above −6 dBFS: unlit, dark");
    // −0 dBFS lights the red cell at the top.
    let mut px2 = vec![0u32; (w * h) as usize];
    let mut s = Surface { px: &mut px2, width: w, height: h, stride: w };
    paint_meter(&mut s, hw, 0, 0, 20, 0.0);
    let (x, y) = meter_cell_centre(hw, 0, 0, 20, 19);
    let (r, g, _) = rgb(px2[(y as u32 * w + x as u32) as usize]);
    assert!(r > g + 60, "the top cell lit red: {r} {g}");
    assert!(common::luma(px2[(y as u32 * w + x as u32) as usize]) > common::luma(cell(19)) + 40, "lit, not dark");
}

#[test]
fn tooltips_wait_for_the_pointer_to_rest() {
    assert!(!tooltip_shown(499));
    assert!(tooltip_shown(500));
}
