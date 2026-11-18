// SPDX-License-Identifier: Apache-2.0
//! Painting a laid-out tree (TASK-055): each node's background and border in
//! the theme's colours, then its words, centred in their line; a scroll
//! area clips its children to its inside. The controls' rendered hardware
//! (TASK-056) draws through the same surface with `fimage::blit`.

use crate::layout::Rect;
use crate::text::Fonts;
use crate::theme::Theme;
use crate::tree::{Content, Layout, NodeId, Tree};
use ffont::atlas::Target;
use fimage::blit::Surface;

/// A surface seen through a clip: writes outside it are dropped.
struct Clipped<'s, 'p> {
    s: &'s mut Surface<'p>,
    clip: Rect,
}

impl Target for Clipped<'_, '_> {
    fn size(&self) -> (usize, usize) {
        (self.s.width as usize, self.s.height as usize)
    }
    fn get(&self, x: usize, y: usize) -> u32 {
        self.s.px[y * self.s.stride as usize + x]
    }
    fn set(&mut self, x: usize, y: usize, rgb: u32) {
        if self.clip.contains(x as i32, y as i32) {
            self.s.px[y * self.s.stride as usize + x] = rgb;
        }
    }
}

fn fill(s: &mut Surface, r: Rect, rgb: u32) {
    let r = r.intersect(Rect::new(0, 0, s.width, s.height));
    for y in r.y..r.y + r.h as i32 {
        let row = y as usize * s.stride as usize;
        s.px[row + r.x as usize..row + r.x as usize + r.w as usize].fill(rgb);
    }
}

/// Paints the tree (laid out already) onto `s`.
pub fn paint(tree: &Tree, theme: Theme, fonts: &mut Fonts, s: &mut Surface) {
    let all = Rect::new(0, 0, s.width, s.height);
    node(tree, Tree::ROOT, theme, fonts, s, all);
}

fn node(tree: &Tree, id: NodeId, theme: Theme, fonts: &mut Fonts, s: &mut Surface, clip: Rect) {
    let n = tree.node(id);
    let b = n.bounds.intersect(clip);
    if b.w == 0 || b.h == 0 {
        return;
    }
    if let Some(bg) = n.style.background {
        fill(s, b, theme.colour(bg).xrgb());
    }
    if let Some(edge) = n.style.border {
        let c = theme.colour(edge).xrgb();
        let r = n.bounds;
        for side in [
            Rect::new(r.x, r.y, r.w, 1),
            Rect::new(r.x, r.y + r.h as i32 - 1, r.w, 1),
            Rect::new(r.x, r.y, 1, r.h),
            Rect::new(r.x + r.w as i32 - 1, r.y, 1, r.h),
        ] {
            fill(s, side.intersect(clip), c);
        }
    }
    if let (Content::Text { style, colour, .. }, Some(words)) = (&n.content, &n.a11y.name) {
        let (px, line, _) = theme.text(*style);
        let chain = fonts.chain(*style);
        let (up, down, _) = chain[0].metrics(px);
        let inner = n.bounds.inset(theme.px(n.style.padding));
        let baseline = inner.y + (line as i32 - (up + down)) / 2 + up;
        let mut t = Clipped { s, clip: b };
        ffont::text::draw(&mut t, chain, px, inner.x, baseline, words, theme.colour(*colour).xrgb());
    }
    let inside = if n.layout == Layout::Scroll { b.intersect(n.bounds.inset(theme.px(n.style.padding))) } else { clip };
    for &c in &n.children {
        node(tree, c, theme, fonts, s, inside);
    }
}
