// SPDX-License-Identifier: Apache-2.0
//! What Welcome and Login draw with: a full-screen window of stage's, the
//! theme and fonts, the ground (the theme's, or the wallpaper dimmed 40 %),
//! the centred module, fields, keys and the meter, each in the theme's
//! tokens at the screen's scale, and each published to the accessibility
//! tree with its test id.

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, perm, well_known};
use fimage::blit::Surface;
use fimage::bundle::Bundle;
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fui::draw::{self, fade, fill};
use fui::layout::Rect;
use fui::text::{FontData, Fonts, words};
use fui::theme::{Mode, TextStyle, Theme, Tok};
use fwire::proto::ax;
use fwire::proto::stage as st;

pub fn deadline() -> u64 {
    Instant::after(Duration::from_secs(5))
}

/// A key's look under the pointer and the focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub focused: bool,
    pub hovered: bool,
    pub pressed: bool,
}

pub struct Screen {
    pub stage: Client,
    pub id: u32,
    pub events: Channel,
    px: *mut u32,
    pub w: u32,
    pub h: u32,
    _vmo: Handle,
    pub theme: Theme,
    pub fonts: Fonts<'static>,
    /// The ax tree drawn last (published once the frame is shown).
    pub nodes: Vec<ax::Node>,
    ax: fstd::ax::Publisher,
    /// The wallpaper at the screen's size, for the theme it was made for.
    wall: Option<(Mode, Vec<u32>)>,
    /// The endpoint registered with conductor (so the desktop can see the
    /// screen is up); nothing is served on it.
    listen: Channel,
}

fn fonts() -> Result<Fonts<'static>, String> {
    let f = |file| fstd::service::font(file).map_err(|e| alloc::format!("{file}: {}", e.describe()));
    Fonts::new(FontData {
        regular: f("NotoSans-Regular.ttf")?,
        medium: f("NotoSans-Medium.ttf")?,
        semibold: f("NotoSans-SemiBold.ttf")?,
        mono: f("NotoSansMono-Regular.ttf")?,
        symbols: f("NotoSansSymbols2-Regular.ttf")?,
    })
    .map_err(|e| alloc::format!("the fonts: {e:?}"))
}

impl Screen {
    /// A full-screen window called `title`.
    pub fn open(title: &str) -> Result<Screen, String> {
        let ch = fstd::service::connect("stage").map_err(|e| alloc::format!("stage: {e:?}"))?;
        let mut stage = Client::new(ch);
        let (scr, _) = stage
            .call::<_, st::ScreenInfo>(st::SCREEN, &(), Vec::new(), deadline())
            .map_err(|e| alloc::format!("the screen: {e:?}"))?;
        let theme = Theme::new(
            if scr.theme == st::Theme::Light { Mode::Light } else { Mode::Dark },
            scr.scale_pct as f32 / 100.0,
        );
        let req = st::CreateWindow {
            title: String::from(title),
            w: scr.w,
            h: scr.h,
            min_w: 0,
            min_h: 0,
            kind: st::Kind::Lock,
            x: 0,
            y: 0,
        };
        let (made, mut hs) = stage
            .call::<_, st::Created>(st::CREATE_WINDOW, &req, Vec::new(), deadline())
            .map_err(|e| alloc::format!("create_window: {e:?}"))?;
        if hs.len() != 2 {
            return Err(String::from("stage sent no surface"));
        }
        let events = Channel(hs.remove(1));
        let vmo = hs.remove(0);
        let len = (scr.w as u64 * scr.h as u64 * 4).next_multiple_of(4096);
        let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE)
            .map_err(|e| String::from(e.describe()))?;
        let (listen, give) = Channel::pair().map_err(|e| String::from(e.describe()))?;
        fstd::service::register(give).map_err(|e| alloc::format!("register: {e:?}"))?;
        Ok(Screen {
            stage,
            id: made.window_id,
            events,
            px: at as *mut u32,
            w: scr.w,
            h: scr.h,
            _vmo: vmo,
            theme,
            fonts: fonts()?,
            nodes: Vec::new(),
            ax: Default::default(),
            wall: None,
            listen,
        })
    }

    pub fn surface(&mut self) -> Surface<'_> {
        // SAFETY: the surface VMO maps w × h u32s, ours to write, for the window's life.
        let px = unsafe { core::slice::from_raw_parts_mut(self.px, (self.w * self.h) as usize) };
        Surface { px, width: self.w, height: self.h, stride: self.w }
    }

    /// Shows the frame (opaque: every pixel its full alpha) and publishes
    /// its tree.
    pub fn present(&mut self) {
        for p in self.surface().px.iter_mut() {
            *p |= 0xFF00_0000;
        }
        let shown = st::Present { window_id: self.id, damage: Vec::new() };
        let _ = self.stage.call::<_, ()>(st::PRESENT, &shown, Vec::new(), deadline());
        let tree = ax::Tree { nodes: core::mem::take(&mut self.nodes) };
        self.ax.publish(&mut self.stage, self.id, &tree);
    }

    /// Every event waiting, as (ordinal, payload); a theme or scale change
    /// is taken here (and reported, so the screen is drawn again).
    pub fn events(&mut self) -> Result<Vec<(u32, Vec<u8>)>, FErr> {
        // Whoever connected (the desktop, seeing the screen is up) is let go.
        while self.listen.try_recv().is_ok() {}
        let mut out = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(m) => {
                    let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
                    match h.ordinal {
                        st::EV_THEME => {
                            if let Ok(t) = fwire::from_payload::<st::SetTheme>(p) {
                                self.theme.mode = if t.theme == st::Theme::Light { Mode::Light } else { Mode::Dark };
                            }
                        }
                        st::EV_SCALE => {
                            if let Ok(sc) = fwire::from_payload::<st::SetScale>(p) {
                                self.theme.scale = sc.pct as f32 / 100.0;
                            }
                        }
                        _ => {}
                    }
                    out.push((h.ordinal, p.to_vec()));
                }
                Err(FErr::ShouldWait) => return Ok(out),
                Err(e) => return Err(e),
            }
        }
    }

    /// The ground: the theme's, or (`wallpaper`) the wallpaper dimmed 40 %.
    pub fn ground(&mut self, wallpaper: bool) {
        let (w, h, mode) = (self.w, self.h, self.theme.mode);
        let bg = self.theme.colour(Tok::Bg).xrgb();
        if wallpaper && self.wall.as_ref().is_none_or(|(m, _)| *m != mode) {
            let name = if mode == Mode::Light { "light/wallpaper" } else { "dark/wallpaper" };
            let wall = fstd::service::data("os:wallpaper")
                .ok()
                .and_then(|b| Bundle::parse(b).ok())
                .and_then(|b| b.get(name).and_then(|f| fimage::fimg::parse(f).ok()))
                .map(|f| fimage::blit::cover(&f, w, h));
            self.wall = wall.map(|px| (mode, px));
        }
        let wall = self.wall.as_ref().filter(|_| wallpaper).map(|(_, px)| px.clone());
        let mut s = self.surface();
        match wall {
            Some(px) => {
                s.px.copy_from_slice(&px);
                fade(&mut s, Rect::new(0, 0, w, h), 0, 0.4);
            }
            None => fill(&mut s, Rect::new(0, 0, w, h), bg),
        }
    }

    /// The centred module, `mw` × `mh` (scaled), drawn; its rectangle.
    pub fn module(&mut self, mw: u32, mh: u32) -> Rect {
        let t = self.theme;
        let (mw, mh) = (t.px(mw).min(self.w), t.px(mh).min(self.h));
        let r = Rect::new(((self.w - mw) / 2) as i32, ((self.h - mh) / 2) as i32, mw, mh);
        let rad = 12.0 * t.scale;
        let mut s = self.surface();
        draw::round_rect(&mut s, r, rad, t.colour(Tok::Border));
        draw::round_rect(&mut s, Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2), rad - 1.0, t.colour(Tok::Surface));
        r
    }

    /// Words at (x, y), their top.
    pub fn say(&mut self, (x, y): (i32, i32), style: TextStyle, text: &str, tok: Tok) {
        let t = self.theme;
        let px = self.px;
        let (w, h) = (self.w, self.h);
        // SAFETY: as `surface`; the fonts are borrowed beside it.
        let mut s = Surface {
            px: unsafe { core::slice::from_raw_parts_mut(px, (w * h) as usize) },
            width: w,
            height: h,
            stride: w,
        };
        words(&mut s, &mut self.fonts, t, style, (x, y), text, tok);
    }

    pub fn measure(&mut self, style: TextStyle, text: &str) -> (u32, u32) {
        self.fonts.measure(self.theme, style, text)
    }

    /// A field: its label above, the well, what's typed (dots for a
    /// password) with the caret when focused, and its error under it. The
    /// well's rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn field(
        &mut self,
        (x, y, w): (i32, i32, u32),
        label: &str,
        value: &str,
        secret: bool,
        focused: bool,
        error: Option<&str>,
        testid: &str,
    ) -> Rect {
        let t = self.theme;
        let (_, small, _) = t.text(TextStyle::Small);
        self.say((x, y), TextStyle::Small, label, Tok::Text2);
        let well = Rect::new(x, y + small as i32 + t.px(4) as i32, w, t.px(40));
        let rad = 6.0 * t.scale;
        let edge = if error.is_some() {
            Tok::Error
        } else if focused {
            Tok::Focus
        } else {
            Tok::Border
        };
        let ring = if focused || error.is_some() { t.px(2).max(1) } else { 1 };
        {
            let mut s = self.surface();
            draw::round_rect(&mut s, well, rad, t.colour(edge));
            draw::round_rect(
                &mut s,
                Rect::new(well.x + ring as i32, well.y + ring as i32, well.w - 2 * ring, well.h - 2 * ring),
                rad - ring as f32,
                t.colour(Tok::Bg),
            );
        }
        let shown: String = if secret { value.chars().map(|_| '•').collect() } else { String::from(value) };
        let (_, line, _) = t.text(TextStyle::Body);
        let ty = well.y + (well.h as i32 - line as i32) / 2;
        let pad = t.px(12) as i32;
        self.say((well.x + pad, ty), TextStyle::Body, &shown, Tok::Text);
        if focused {
            let cx =
                well.x + pad + if shown.is_empty() { 0 } else { self.measure(TextStyle::Body, &shown).0 as i32 + 1 };
            let c = t.colour(Tok::PrimaryInk).xrgb();
            fill(&mut self.surface(), Rect::new(cx, ty + 2, t.px(2).max(1), line.saturating_sub(4)), c);
        }
        if let Some(e) = error {
            self.say((x, well.y + well.h as i32 + t.px(4) as i32), TextStyle::Small, e, Tok::Error);
        }
        let mut n = fstd::ax::node(
            ax::ROOT,
            ax::TEXTBOX,
            label,
            if focused { ax::FOCUSED } else { 0 },
            (well.x, well.y, well.w, well.h),
        );
        n.testid = String::from(testid);
        n.value = shown;
        self.nodes.push(n);
        if let Some(e) = error {
            let (ew, eh) = self.measure(TextStyle::Small, e);
            let mut n = fstd::ax::node(ax::ROOT, ax::TEXT, e, 0, (x, well.y + well.h as i32 + t.px(4) as i32, ew, eh));
            n.testid = alloc::format!("{testid}-error");
            self.nodes.push(n);
        }
        well
    }

    /// The vertical room a field takes, with the gap after it.
    pub fn field_height(&self) -> u32 {
        let t = self.theme;
        let (_, small, _) = t.text(TextStyle::Small);
        small + t.px(4) + t.px(40) + t.px(4) + small + t.px(8)
    }

    /// A key: Freally green for the primary one, else the raised surface;
    /// lighter under the pointer, darker pressed, ringed when focused.
    pub fn key(&mut self, r: Rect, label: &str, primary: bool, look: Look, testid: &str) {
        let t = self.theme;
        let rad = 6.0 * t.scale;
        let (ground, ink) = if primary { (Tok::Primary, Tok::OnPrimary) } else { (Tok::Surface2, Tok::Text) };
        {
            let mut s = self.surface();
            if look.focused {
                let g = t.px(2).max(1) as i32;
                draw::round_rect(
                    &mut s,
                    Rect::new(r.x - g, r.y - g, r.w + 2 * g as u32, r.h + 2 * g as u32),
                    rad + g as f32,
                    t.colour(Tok::Focus),
                );
            }
            draw::round_rect(&mut s, r, rad, t.colour(ground));
            if look.pressed {
                fade(&mut s, r, 0, 0.15);
            } else if look.hovered {
                fade(&mut s, r, 0xFF_FFFF, 0.12);
            }
        }
        let (lw, lh) = self.measure(TextStyle::Body, label);
        self.say((r.x + (r.w as i32 - lw as i32) / 2, r.y + (r.h as i32 - lh as i32) / 2), TextStyle::Body, label, ink);
        let state = if look.focused { ax::FOCUSED } else { 0 } | if look.pressed { ax::PRESSED } else { 0 };
        let mut n = fstd::ax::node(ax::ROOT, ax::BUTTON, label, state, (r.x, r.y, r.w, r.h));
        n.testid = String::from(testid);
        self.nodes.push(n);
    }

    /// The meter shown while the password is checked (Argon2id, about half
    /// a second): the track, filled, and what's happening.
    pub fn meter(&mut self, r: Rect, label: &str, testid: &str) {
        let t = self.theme;
        let h = t.px(6);
        let track = Rect::new(r.x, r.y, r.w, h);
        {
            let mut s = self.surface();
            draw::round_rect(&mut s, track, h as f32 / 2.0, t.colour(Tok::MeterOff));
            draw::round_rect(&mut s, track, h as f32 / 2.0, t.colour(Tok::PrimaryInk));
        }
        self.say((r.x, r.y + h as i32 + t.px(6) as i32), TextStyle::Small, label, Tok::Text2);
        let mut n = fstd::ax::node(ax::ROOT, ax::METER, label, 0, (r.x, r.y, r.w, h));
        n.testid = String::from(testid);
        self.nodes.push(n);
    }
}

/// Whether (x, y) is inside `r`.
pub fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.w as i32 && y < r.y + r.h as i32
}
