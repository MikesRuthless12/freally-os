// SPDX-License-Identifier: Apache-2.0
//! Layout (TASK-055): two passes. Measure, bottom up: each node's natural
//! size (its content's, or its children's arranged, plus padding; a fixed
//! size wins). Arrange, top down: each node gets a rectangle; a row or column
//! gives fixed and fitted children their size and shares what's left among
//! the filling ones by weight; a grid makes equal columns; a stack lays every
//! child over the same area; a scroll is a column moved by its offset. All
//! lengths are 100 % pixels, scaled by the theme to whole device pixels.

use crate::strings::Catalog;
use crate::text::Fonts;
use crate::theme::Theme;
use crate::tree::{Align, Content, Layout, NodeId, Size, Tree};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    /// `p` pixels in from every side.
    pub fn inset(self, p: u32) -> Rect {
        Rect::new(self.x + p as i32, self.y + p as i32, self.w.saturating_sub(2 * p), self.h.saturating_sub(2 * p))
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w as i32 && y < self.y + self.h as i32
    }

    /// The part of `self` inside `other`.
    pub fn intersect(self, other: Rect) -> Rect {
        let (x0, y0) = (self.x.max(other.x), self.y.max(other.y));
        let x1 = (self.x + self.w as i32).min(other.x + other.w as i32);
        let y1 = (self.y + self.h as i32).min(other.y + other.h as i32);
        Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
    }
}

/// What layout needs: the theme (scale), the fonts (to measure words) and
/// the strings (the words themselves).
pub struct Ctx<'a, 'f> {
    pub theme: Theme,
    pub fonts: &'a mut Fonts<'f>,
    pub strings: &'a Catalog,
}

impl core::fmt::Debug for Ctx<'_, '_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ctx").field("theme", &self.theme).finish()
    }
}

/// Lays the tree out in `rect`. Debug builds also check every node is named.
pub fn layout(tree: &mut Tree, rect: Rect, cx: &mut Ctx) {
    measure(tree, Tree::ROOT, cx);
    arrange(tree, Tree::ROOT, rect, cx.theme);
    debug_assert!(tree.check().is_empty(), "the accessibility tree has holes: {:?}", tree.check());
}

fn fixed(s: Size, theme: Theme, natural: u32) -> u32 {
    match s {
        Size::Fixed(v) => theme.px(v),
        _ => natural,
    }
}

fn measure(tree: &mut Tree, id: NodeId, cx: &mut Ctx) -> (u32, u32) {
    let children = tree.nodes[id].children.clone();
    let sizes: Vec<(u32, u32)> = children.iter().map(|&c| measure(tree, c, cx)).collect();
    let t = cx.theme;
    let node = &mut tree.nodes[id];
    let (pad, gap) = (t.px(node.style.padding), t.px(node.style.gap));
    let gaps = |n: usize| gap * n.saturating_sub(1) as u32;
    let (mut w, mut h) = match node.layout {
        Layout::Row => {
            (sizes.iter().map(|s| s.0).sum::<u32>() + gaps(sizes.len()), sizes.iter().map(|s| s.1).max().unwrap_or(0))
        }
        Layout::Column | Layout::Scroll => {
            (sizes.iter().map(|s| s.0).max().unwrap_or(0), sizes.iter().map(|s| s.1).sum::<u32>() + gaps(sizes.len()))
        }
        Layout::Grid { columns } => {
            let c = columns.max(1) as usize;
            let cell_w = sizes.iter().map(|s| s.0).max().unwrap_or(0);
            let rows: u32 = sizes.chunks(c).map(|r| r.iter().map(|s| s.1).max().unwrap_or(0)).sum();
            (cell_w * c.min(sizes.len().max(1)) as u32 + gaps(c.min(sizes.len())), rows + gaps(sizes.len().div_ceil(c)))
        }
        Layout::Stack => (sizes.iter().map(|s| s.0).max().unwrap_or(0), sizes.iter().map(|s| s.1).max().unwrap_or(0)),
    };
    if let Content::Text { key, style, .. } = &node.content {
        let words = cx.strings.get(key);
        let (tw, line) = cx.fonts.measure(t, *style, &words);
        w = w.max(tw);
        h = h.max(line);
        node.a11y.name = Some(words);
    }
    let natural = (fixed(node.style.width, t, w + 2 * pad), fixed(node.style.height, t, h + 2 * pad));
    node.natural = natural;
    natural
}

/// A child's size along the main axis before sharing: fixed or natural;
/// filling children start at 0 and take a share.
fn main_size(s: Size, natural: u32) -> u32 {
    if matches!(s, Size::Fill(_)) { 0 } else { natural }
}

/// Where something `size` long sits in `space` of `avail`, aligned.
fn place(align: Align, avail: u32, size: u32) -> i32 {
    match align {
        Align::Start => 0,
        Align::Center => (avail.saturating_sub(size) / 2) as i32,
        Align::End => avail.saturating_sub(size) as i32,
    }
}

fn arrange(tree: &mut Tree, id: NodeId, rect: Rect, t: Theme) {
    let node = &mut tree.nodes[id];
    node.bounds = rect;
    node.a11y.bounds = rect;
    let inner = rect.inset(t.px(node.style.padding));
    let gap = t.px(node.style.gap);
    let (layout, align, scroll_y) = (node.layout, node.style.align, node.scroll_y);
    let children = node.children.clone();
    let kids: Vec<(Size, Size, (u32, u32))> = children
        .iter()
        .map(|&c| {
            let n = &tree.nodes[c];
            (n.style.width, n.style.height, n.natural)
        })
        .collect();
    let gaps = gap * children.len().saturating_sub(1) as u32;
    let mut rects = Vec::with_capacity(children.len());
    match layout {
        Layout::Row | Layout::Column => {
            let row = layout == Layout::Row;
            let (main_avail, cross_avail) = if row { (inner.w, inner.h) } else { (inner.h, inner.w) };
            let main = |k: &(Size, Size, (u32, u32))| if row { (k.0, k.2.0) } else { (k.1, k.2.1) };
            let cross = |k: &(Size, Size, (u32, u32))| if row { (k.1, k.2.1) } else { (k.0, k.2.0) };
            let used: u32 = kids.iter().map(|k| main_size(main(k).0, main(k).1)).sum::<u32>() + gaps;
            let left = main_avail.saturating_sub(used);
            let weights: u32 = kids.iter().map(|k| if let Size::Fill(w) = main(k).0 { w as u32 } else { 0 }).sum();
            let (mut pos, mut given) = (0u32, 0u32);
            let fills = kids.iter().filter(|k| matches!(main(k).0, Size::Fill(_))).count();
            let mut seen = 0;
            for k in &kids {
                let (ms, mn) = main(k);
                let len = match ms {
                    Size::Fill(w) => {
                        seen += 1;
                        // The last filler takes the remainder, so the shares add up exactly.
                        let share = if seen == fills { left - given } else { left * w as u32 / weights.max(1) };
                        given += share;
                        share
                    }
                    _ => mn,
                };
                let (cs, cn) = cross(k);
                let cl = if matches!(cs, Size::Fill(_)) { cross_avail } else { cn.min(cross_avail) };
                let co = place(align, cross_avail, cl);
                rects.push(if row {
                    Rect::new(inner.x + pos as i32, inner.y + co, len, cl)
                } else {
                    Rect::new(inner.x + co, inner.y + pos as i32, cl, len)
                });
                pos += len + gap;
            }
        }
        Layout::Grid { columns } => {
            let c = columns.max(1) as usize;
            let cell_w = inner.w.saturating_sub(gap * (c as u32 - 1)) / c as u32;
            let mut y = inner.y;
            for (r, row) in kids.chunks(c).enumerate() {
                let row_h = row.iter().map(|k| k.2.1).max().unwrap_or(0);
                for (i, k) in row.iter().enumerate() {
                    let h = if matches!(k.1, Size::Fill(_)) { row_h } else { k.2.1 };
                    rects.push(Rect::new(inner.x + (i as u32 * (cell_w + gap)) as i32, y, cell_w, h));
                }
                y += row_h as i32 + if r + 1 < kids.len().div_ceil(c) { gap as i32 } else { 0 };
            }
        }
        Layout::Stack => {
            for k in &kids {
                let w = if matches!(k.0, Size::Fill(_)) { inner.w } else { k.2.0.min(inner.w) };
                let h = if matches!(k.1, Size::Fill(_)) { inner.h } else { k.2.1.min(inner.h) };
                rects.push(Rect::new(inner.x + place(align, inner.w, w), inner.y + place(align, inner.h, h), w, h));
            }
        }
        Layout::Scroll => {
            let content: u32 = kids.iter().map(|k| k.2.1).sum::<u32>() + gaps;
            let max = content.saturating_sub(inner.h) as i32;
            let off = scroll_y.clamp(0, max);
            tree.nodes[id].scroll_y = off;
            let mut y = inner.y - off;
            for k in &kids {
                rects.push(Rect::new(inner.x, y, inner.w, k.2.1));
                y += (k.2.1 + gap) as i32;
            }
        }
    }
    for (&c, r) in children.iter().zip(rects) {
        arrange(tree, c, r, t);
    }
}
