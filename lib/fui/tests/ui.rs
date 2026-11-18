// SPDX-License-Identifier: Apache-2.0
//! Freally UI part 1 (TASK-055): layout worked out by hand, the
//! accessibility check, the catalog, the theme, and painting.
#![cfg(feature = "ui")]

use fimage::blit::Surface;
use fui::a11y::Role;
use fui::layout::{Ctx, Rect, layout};
use fui::paint::paint;
use fui::strings::Catalog;
use fui::text::{FontData, Fonts};
use fui::theme::{Mode, TextStyle, Theme, Tok};
use fui::tree::{Align, Layout, Node, Size, Tree};

const STRINGS: &str = include_str!("../../../assets/strings/en.txt");

fn fonts() -> Fonts<'static> {
    Fonts::new(FontData {
        regular: include_bytes!("../../../assets/fonts/noto/NotoSans-Regular.ttf"),
        medium: include_bytes!("../../../assets/fonts/noto/NotoSans-Medium.ttf"),
        semibold: include_bytes!("../../../assets/fonts/noto/NotoSans-SemiBold.ttf"),
        mono: include_bytes!("../../../assets/fonts/noto/NotoSansMono-Regular.ttf"),
        symbols: include_bytes!("../../../assets/fonts/noto/NotoSansSymbols2-Regular.ttf"),
    })
    .unwrap()
}

fn group(l: Layout) -> Node {
    Node::new(l, Role::Group)
}

fn boxed(w: Size, h: Size) -> Node {
    group(Layout::Stack).width(w).height(h)
}

fn run(tree: &mut Tree, rect: Rect, scale: f32) {
    let strings = Catalog::parse(STRINGS).unwrap();
    let mut f = fonts();
    let mut cx = Ctx { theme: Theme::new(Mode::Dark, scale), fonts: &mut f, strings: &strings };
    layout(tree, rect, &mut cx);
}

#[test]
fn a_row_shares_what_is_left_by_weight() {
    let mut t = Tree::new(group(Layout::Row).gap(4).height(Size::Fixed(20)));
    let a = t.add(0, boxed(Size::Fixed(10), Size::Fill(1)));
    let b = t.add(0, boxed(Size::Fill(1), Size::Fill(1)));
    let c = t.add(0, boxed(Size::Fill(2), Size::Fixed(8)));
    run(&mut t, Rect::new(0, 0, 100, 20), 1.0);
    // 100 − 10 − two gaps of 4 leaves 82: a third (27) and the rest (55).
    assert_eq!(t.node(a).bounds, Rect::new(0, 0, 10, 20));
    assert_eq!(t.node(b).bounds, Rect::new(14, 0, 27, 20));
    assert_eq!(t.node(c).bounds, Rect::new(45, 0, 55, 8));
}

#[test]
fn a_column_pads_gaps_and_centres() {
    let mut t = Tree::new(group(Layout::Column).padding(8).gap(6).align(Align::Center));
    let a = t.add(0, boxed(Size::Fixed(20), Size::Fixed(10)));
    let b = t.add(0, boxed(Size::Fill(1), Size::Fixed(10)));
    run(&mut t, Rect::new(0, 0, 100, 60), 1.0);
    assert_eq!(t.node(a).bounds, Rect::new(8 + 32, 8, 20, 10), "centred in the 84 inside");
    assert_eq!(t.node(b).bounds, Rect::new(8, 24, 84, 10), "filling across");
}

#[test]
fn a_grid_makes_equal_columns() {
    let mut t = Tree::new(group(Layout::Grid { columns: 2 }).gap(2));
    let ids: Vec<usize> =
        [10u32, 14, 8, 6, 12].iter().map(|&h| t.add(0, boxed(Size::Fixed(5), Size::Fixed(h)))).collect();
    run(&mut t, Rect::new(0, 0, 50, 100), 1.0);
    // Cells (50 − 2) / 2 = 24 wide; rows as tall as their tallest: 14, 8, 12.
    assert_eq!(t.node(ids[0]).bounds, Rect::new(0, 0, 24, 10));
    assert_eq!(t.node(ids[1]).bounds, Rect::new(26, 0, 24, 14));
    assert_eq!(t.node(ids[2]).bounds, Rect::new(0, 16, 24, 8));
    assert_eq!(t.node(ids[4]).bounds, Rect::new(0, 26, 24, 12));
}

#[test]
fn a_stack_fills_or_centres() {
    let mut t = Tree::new(group(Layout::Stack).align(Align::Center));
    let back = t.add(0, boxed(Size::Fill(1), Size::Fill(1)));
    let front = t.add(0, boxed(Size::Fixed(10), Size::Fixed(4)));
    run(&mut t, Rect::new(5, 5, 30, 20), 1.0);
    assert_eq!(t.node(back).bounds, Rect::new(5, 5, 30, 20));
    assert_eq!(t.node(front).bounds, Rect::new(15, 13, 10, 4));
}

#[test]
fn a_scroll_moves_and_stops_at_its_ends() {
    let mut t = Tree::new(group(Layout::Scroll).width(Size::Fixed(40)).height(Size::Fixed(30)));
    let rows: Vec<usize> = (0..5).map(|_| t.add(0, boxed(Size::Fill(1), Size::Fixed(20)))).collect();
    t.node_mut(0).scroll_y = 30;
    run(&mut t, Rect::new(0, 0, 40, 30), 1.0);
    assert_eq!(t.node(rows[0]).bounds.y, -30);
    assert_eq!(t.node(rows[2]).bounds, Rect::new(0, 10, 40, 20));
    // Past the end: held at content − view (100 − 30).
    t.node_mut(0).scroll_y = 500;
    run(&mut t, Rect::new(0, 0, 40, 30), 1.0);
    assert_eq!(t.node(0).scroll_y, 70);
    t.node_mut(0).scroll_y = -5;
    run(&mut t, Rect::new(0, 0, 40, 30), 1.0);
    assert_eq!(t.node(0).scroll_y, 0);
}

#[test]
fn the_scale_grows_every_length() {
    let mut t = Tree::new(group(Layout::Row).padding(4).gap(2));
    let a = t.add(0, boxed(Size::Fixed(10), Size::Fixed(10)));
    let b = t.add(0, boxed(Size::Fixed(10), Size::Fixed(10)));
    run(&mut t, Rect::new(0, 0, 200, 100), 1.5);
    assert_eq!(t.node(a).bounds, Rect::new(6, 6, 15, 15));
    assert_eq!(t.node(b).bounds, Rect::new(24, 6, 15, 15));
    assert_eq!(t.node(0).natural, (6 + 15 + 3 + 15 + 6, 6 + 15 + 6));
}

#[test]
fn words_come_from_the_catalog_and_name_their_node() {
    let mut t = Tree::new(group(Layout::Column));
    let w = t.add(0, Node::text("app.terminal", TextStyle::Title, Tok::Text));
    run(&mut t, Rect::new(0, 0, 300, 100), 1.0);
    assert_eq!(t.node(w).a11y.name.as_deref(), Some("Terminal"));
    let (width, height) = t.node(w).natural;
    assert!(width > 40 && height == 26, "{width}×{height}: the title style's line is 26 px");
}

#[test]
fn every_control_needs_a_name_and_test_ids_are_unique() {
    let mut t = Tree::new(group(Layout::Row));
    t.add(0, Node::new(Layout::Stack, Role::Button).testid("go"));
    t.add(0, Node::new(Layout::Stack, Role::Switch).name("Wi-Fi".into()).testid("go"));
    let problems = t.check();
    assert!(problems.iter().any(|p| p.contains("Button") && p.contains("no name")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("test id go twice")), "{problems:?}");
    assert_eq!(problems.len(), 2);
}

#[test]
fn the_catalog_reads_its_file_and_refuses_damage() {
    let c = Catalog::parse(STRINGS).unwrap();
    assert!(c.len() >= 10);
    assert_eq!(c.get("common.ok"), "OK");
    assert!(Catalog::parse("Bad Key = x").is_err());
    assert!(Catalog::parse("a = 1\na = 2").is_err());
    assert!(Catalog::parse("no equals").is_err());
}

#[test]
fn the_theme_switches_every_token() {
    let (d, l) = (Theme::new(Mode::Dark, 1.0), Theme::new(Mode::Light, 1.0));
    assert_eq!(d.colour(Tok::Bg).xrgb(), 0x090C09);
    assert_ne!(d.colour(Tok::Bg), l.colour(Tok::Bg));
    assert_eq!(d.colour(Tok::Primary), l.colour(Tok::Primary), "Freally green in both");
}

#[test]
fn painting_draws_backgrounds_borders_and_words() {
    let mut t = Tree::new(group(Layout::Column).padding(10).background(Tok::Surface).border(Tok::Border));
    t.add(0, Node::text("monitor.processes", TextStyle::Body, Tok::Text));
    run(&mut t, Rect::new(0, 0, 120, 40), 1.0);
    let mut px = vec![0u32; 120 * 40];
    let mut s = Surface { px: &mut px, width: 120, height: 40, stride: 120 };
    let mut f = fonts();
    paint(&t, Theme::new(Mode::Dark, 1.0), &mut f, &mut s);
    let th = Theme::new(Mode::Dark, 1.0);
    assert_eq!(px[0], th.colour(Tok::Border).xrgb(), "the border's corner");
    assert_eq!(px[5 * 120 + 5], th.colour(Tok::Surface).xrgb(), "the background");
    let lit = px.iter().filter(|&&p| p != th.colour(Tok::Surface).xrgb() && p != th.colour(Tok::Border).xrgb()).count();
    assert!(lit > 100, "the words drew {lit} pixels");
}
