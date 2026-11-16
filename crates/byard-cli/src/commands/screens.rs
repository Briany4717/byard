//! Headless screens: the compositor `byard shot` and `byard test` stand in for
//! (RFC-0068 §9).
//!
//! A program of windows, panels and popups needs somebody to say where each
//! goes. On the desktop that is the compositor; here it is this module, with the
//! same arithmetic, and nothing else: panels are placed on their screen by layer
//! shell's rules ([`placement::arrange`]), windows are centred in the area the
//! panels left, popups are placed by the `xdg_positioner` solver against the
//! screen they are on ([`PopupSpec::positioner`]). Which surface is on top of
//! which, and which one a point of the screen belongs to, is answered here too,
//! and so is the picture of all of it: the screens side by side over a
//! checkerboard, every surface composited in layer order.

#![allow(
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use byard_compiler::interp::eval::Interpreter;
use byard_core::surface::placement::{self, Layer};
use byard_core::surface::{InputRegion, Rect, Size};
use byard_core::{ScreenInfo, SurfaceChange, SurfaceId, SurfaceKind, SurfaceSpec};

/// `--screens "NAME:WxH[@SCALE][+X+Y], …"`, as the screens a program sees.
///
/// `WxH` is the screen's mode in device pixels, as the compositor lists it, so
/// the logical size a program lays out in is that divided by the scale
/// (`DP-1:2560x1440@1.5` is 1707 by 960 logical). `+X+Y` is where the screen sits
/// in the logical layout; without it a screen sits right of the ones before it,
/// at `y` 0. The list is in the order `screens` is read in (`x`, then `y`).
pub fn parse_screens(text: &str) -> Result<Vec<ScreenInfo>, String> {
    let example = "NAME:WxH[@SCALE][+X+Y], like \"DP-1:2560x1440@1.5, HDMI-A-1:1920x1080+1707+0\"";
    let mut out: Vec<ScreenInfo> = Vec::new();
    for item in text.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let bad = |why: &str| format!("screen `{item}`: {why}; expected {example}");
        let (name, rest) = item.split_once(':').ok_or_else(|| bad("no `:`"))?;
        let name = name.trim();
        if name.is_empty() {
            return Err(bad("no name"));
        }
        if out.iter().any(|s| s.name == name) {
            return Err(format!("two screens are called `{name}`"));
        }
        // `WxH`, then `@SCALE`, then `+X+Y`, each optional after the first.
        let (size, rest) = rest
            .find(['@', '+', '-'])
            .map_or((rest, ""), |i| rest.split_at(i));
        let (w, h) = size.split_once('x').ok_or_else(|| bad("no `WxH`"))?;
        let w: f32 = w
            .trim()
            .parse()
            .map_err(|_| bad("the width is not a number"))?;
        let h: f32 = h
            .trim()
            .parse()
            .map_err(|_| bad("the height is not a number"))?;
        if w < 1.0 || h < 1.0 {
            return Err(bad("a screen has a size"));
        }
        let (scale, rest) = match rest.strip_prefix('@') {
            Some(after) => {
                let end = after.find(['+', '-']).unwrap_or(after.len());
                let scale: f32 = after[..end]
                    .trim()
                    .parse()
                    .map_err(|_| bad("the scale is not a number"))?;
                if scale <= 0.0 {
                    return Err(bad("the scale is above zero"));
                }
                (scale, &after[end..])
            }
            None => (1.0, rest),
        };
        let at = if rest.is_empty() {
            None
        } else {
            Some(position(rest).ok_or_else(|| bad("the position is `+X+Y`"))?)
        };
        let (width, height) = ((w / scale).round().max(1.0), (h / scale).round().max(1.0));
        let (x, y) = at.unwrap_or_else(|| {
            let right = out.iter().map(|s| s.x + s.width).fold(0.0, f32::max);
            (right, 0.0)
        });
        out.push(ScreenInfo {
            name: name.to_string(),
            description: String::new(),
            x,
            y,
            width,
            height,
            scale,
        });
    }
    if out.is_empty() {
        return Err(format!("no screens; expected {example}"));
    }
    ScreenInfo::sort(&mut out);
    Ok(out)
}

/// `+X+Y` (either may be negative, `-1920+0`) as a point.
fn position(text: &str) -> Option<(f32, f32)> {
    let second = text[1..].find(['+', '-'])? + 1;
    let (x, y) = text.split_at(second);
    let num = |s: &str| s.strip_prefix('+').unwrap_or(s).parse::<f32>().ok();
    Some((num(x)?, num(y)?))
}

/// Where a surface is, on which screen, in that screen's logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Index into the screens.
    pub screen: usize,
    /// Its rectangle, with the origin at the screen's top-left corner.
    pub rect: Rect,
}

/// A surface the simulated compositor knows.
struct Known {
    id: SurfaceId,
    spec: SurfaceSpec,
    region: InputRegion,
    placed: Option<Placed>,
}

/// The simulated compositor: the screens, the surfaces the program asked for,
/// and where each ended up.
pub struct Compositor {
    screens: Vec<ScreenInfo>,
    /// In creation order, which is what the placement of equal layers follows.
    known: Vec<Known>,
}

impl Compositor {
    /// A compositor with these screens and no surface.
    pub fn new(screens: Vec<ScreenInfo>) -> Self {
        Self {
            screens,
            known: Vec::new(),
        }
    }

    /// The screens, in the order a program reads them.
    pub fn screens(&self) -> &[ScreenInfo] {
        &self.screens
    }

    /// Takes what the program asked of its surfaces: one is made, changes or is
    /// gone, or says what part of it takes the pointer.
    pub fn apply(&mut self, change: SurfaceChange) {
        match change {
            SurfaceChange::Create { id, spec } => self.known.push(Known {
                id,
                spec,
                region: InputRegion::All,
                placed: None,
            }),
            SurfaceChange::Configure { id, spec } => {
                if let Some(k) = self.known.iter_mut().find(|k| k.id == id) {
                    k.spec = spec;
                }
            }
            SurfaceChange::Destroy { id } => self.known.retain(|k| k.id != id),
            SurfaceChange::InputRegion { id, region } => {
                if let Some(k) = self.known.iter_mut().find(|k| k.id == id) {
                    k.region = region;
                }
            }
        }
    }

    /// The screen a panel is on: the one it names, else the first (the
    /// compositor's choice).
    fn screen_of(&self, spec: &SurfaceSpec) -> usize {
        spec.layer
            .as_ref()
            .and_then(|l| l.screen.as_deref())
            .and_then(|name| self.screens.iter().position(|s| s.name == name))
            .unwrap_or(0)
    }

    /// Places every surface and tells the program the size each panel was given.
    /// Returns whether a size moved, so the program is drawn again.
    pub fn arrange(&mut self, interp: &mut Interpreter) -> bool {
        let mut moved = false;
        let mut usable: Vec<Rect> = self
            .screens
            .iter()
            .map(|s| Rect::new(0, 0, whole(s.width), whole(s.height)))
            .collect();
        // Panels, screen by screen: the layer shell's arrange pass.
        for (si, screen) in self.screens.iter().enumerate() {
            let on: Vec<usize> = (0..self.known.len())
                .filter(|&i| {
                    let k = &self.known[i];
                    k.spec.kind == SurfaceKind::Panel && self.screen_of(&k.spec) == si
                })
                .collect();
            let layers: Vec<_> = on
                .iter()
                .filter_map(|&i| {
                    let spec = &self.known[i].spec;
                    let size = Size::new(whole(spec.size.0), whole(spec.size.1));
                    Some(spec.layer.as_ref()?.layer_surface(size))
                })
                .collect();
            let screen_size = Size::new(whole(screen.width), whole(screen.height));
            // A panel the protocol would refuse (a size of 0 with nothing to
            // stretch) is left unplaced: the compositor would have closed it.
            let Ok(arranged) = placement::arrange(screen_size, &layers) else {
                continue;
            };
            usable[si] = arranged.usable;
            for (&i, p) in on.iter().zip(&arranged.surfaces) {
                self.known[i].placed = Some(Placed {
                    screen: si,
                    rect: p.rect,
                });
                #[allow(clippy::cast_precision_loss)]
                let size = (p.configure.width as f32, p.configure.height as f32);
                let id = self.known[i].id;
                if interp.surface_size(id) != Some(size) {
                    interp.set_surface_size(id, size);
                    moved = true;
                }
            }
        }
        // Windows, centred in what is left of the first screen, at the size
        // they were opened at (or the user made them: the program's).
        for i in 0..self.known.len() {
            if self.known[i].spec.kind != SurfaceKind::Window {
                continue;
            }
            let id = self.known[i].id;
            let size = interp.surface_size(id).unwrap_or(self.known[i].spec.size);
            let (w, h) = (whole(size.0), whole(size.1));
            let u = usable[0];
            self.known[i].placed = Some(Placed {
                screen: 0,
                rect: Rect::new(u.x + (u.width - w) / 2, u.y + (u.height - h) / 2, w, h),
            });
        }
        // Popups, parents first (they were made in that order): placed by the
        // positioner against the screen they are on, as seen from the parent.
        for i in 0..self.known.len() {
            let Some(popup) = self.known[i].spec.popup else {
                continue;
            };
            let Some(parent) = self
                .known
                .iter()
                .find(|k| k.id == popup.parent)
                .and_then(|k| k.placed)
            else {
                self.known[i].placed = None;
                continue;
            };
            let screen = &self.screens[parent.screen];
            let (sw, sh) = (whole(screen.width), whole(screen.height));
            let size = Size::new(
                whole(self.known[i].spec.size.0).max(1),
                whole(self.known[i].spec.size.1).max(1),
            );
            // The screen in the parent's coordinates: its origin is where the
            // parent is, so the screen starts at a negative offset from it.
            let constraint = Rect::new(-parent.rect.x, -parent.rect.y, sw, sh);
            let at = popup
                .positioner(size, constraint)
                .solve()
                // A positioner the protocol would refuse (a zero-width anchor):
                // the compositor would have closed the client; here the popup
                // sits where the anchor starts.
                .unwrap_or_else(|_| {
                    Rect::new(
                        popup.anchor_rect.x,
                        popup.anchor_rect.y,
                        size.width,
                        size.height,
                    )
                });
            self.known[i].placed = Some(Placed {
                screen: parent.screen,
                rect: Rect::new(
                    parent.rect.x + at.x,
                    parent.rect.y + at.y,
                    at.width,
                    at.height,
                ),
            });
        }
        moved
    }

    /// Where `id` is, if it has been placed.
    pub fn placed(&self, id: SurfaceId) -> Option<Placed> {
        self.known.iter().find(|k| k.id == id)?.placed
    }

    /// Where `id` is in the layout of all the screens (logical pixels).
    pub fn global(&self, id: SurfaceId) -> Option<(f32, f32)> {
        let p = self.placed(id)?;
        let s = &self.screens[p.screen];
        #[allow(clippy::cast_precision_loss)]
        Some((s.x + p.rect.x as f32, s.y + p.rect.y as f32))
    }

    /// The name of the screen `id` is on.
    pub fn screen_name(&self, id: SurfaceId) -> Option<&str> {
        Some(self.screens[self.placed(id)?.screen].name.as_str())
    }

    /// The surfaces from the back to the front: `background`, `bottom`, windows,
    /// `top`, `overlay`, and popups over all of them, each group in the order it
    /// was made.
    pub fn back_to_front(&self) -> Vec<SurfaceId> {
        let rank = |spec: &SurfaceSpec| match (spec.kind, spec.layer.as_ref()) {
            (SurfaceKind::Popup, _) => 5,
            (SurfaceKind::Panel, Some(l)) => match l.layer {
                Layer::Background => 0,
                Layer::Bottom => 1,
                Layer::Top => 3,
                Layer::Overlay => 4,
            },
            _ => 2,
        };
        let mut order: Vec<(u8, usize, SurfaceId)> = self
            .known
            .iter()
            .enumerate()
            .filter(|(_, k)| k.placed.is_some())
            .map(|(i, k)| (rank(&k.spec), i, k.id))
            .collect();
        order.sort_by_key(|&(r, i, _)| (r, i));
        order.into_iter().map(|(_, _, id)| id).collect()
    }

    /// The topmost surface whose input region contains the point `(x, y)` of the
    /// layout, and the point in that surface's own coordinates. A tap on the
    /// transparent part of a panel reaches what is below it.
    pub fn hit(&self, x: f32, y: f32) -> Option<(SurfaceId, (f32, f32))> {
        for id in self.back_to_front().into_iter().rev() {
            let k = self.known.iter().find(|k| k.id == id)?;
            let (gx, gy) = self.global(id)?;
            let rect = k.placed?.rect;
            #[allow(clippy::cast_precision_loss)]
            let (w, h) = (rect.width as f32, rect.height as f32);
            let (lx, ly) = (x - gx, y - gy);
            if lx < 0.0 || ly < 0.0 || lx >= w || ly >= h {
                continue;
            }
            let takes = match (&k.region, k.spec.kind) {
                (InputRegion::None, _) => false,
                (InputRegion::Rects(rects), SurfaceKind::Panel) => rects.iter().any(|r| {
                    #[allow(clippy::cast_precision_loss)]
                    let (rx, ry, rw, rh) =
                        (r.x as f32, r.y as f32, r.width as f32, r.height as f32);
                    lx >= rx && ly >= ry && lx < rx + rw && ly < ry + rh
                }),
                _ => true,
            };
            if takes {
                return Some((id, (lx, ly)));
            }
        }
        None
    }

    /// The popups that have the grab, with their ancestors that are popups, so a
    /// press outside the chain is the compositor's `popup_done` and a press in it
    /// is the popup's own.
    pub fn popup_chain(&self, grab: SurfaceId) -> Vec<SurfaceId> {
        let mut chain = vec![grab];
        let mut at = grab;
        while let Some(parent) = self
            .known
            .iter()
            .find(|k| k.id == at)
            .and_then(|k| k.spec.popup)
            .map(|p| p.parent)
        {
            if !self
                .known
                .iter()
                .any(|k| k.id == parent && k.spec.kind == SurfaceKind::Popup)
            {
                break;
            }
            chain.push(parent);
            at = parent;
        }
        chain
    }

    /// The area of the layout the screens cover: its top-left corner and size, in
    /// logical pixels.
    pub fn bounds(&self, only: Option<usize>) -> (f32, f32, f32, f32) {
        let mut it = self
            .screens
            .iter()
            .enumerate()
            .filter(|(i, _)| only.is_none_or(|o| o == *i))
            .map(|(_, s)| s);
        let first = it.next().expect("a screen");
        let (mut x0, mut y0, mut x1, mut y1) = (
            first.x,
            first.y,
            first.x + first.width,
            first.y + first.height,
        );
        for s in it {
            x0 = x0.min(s.x);
            y0 = y0.min(s.y);
            x1 = x1.max(s.x + s.width);
            y1 = y1.max(s.y + s.height);
        }
        (x0, y0, x1 - x0, y1 - y0)
    }
}

/// A logical size as whole pixels.
#[allow(clippy::cast_possible_truncation)]
fn whole(v: f32) -> i32 {
    v.round() as i32
}

/// The picture of the screens: the pixels of what the screens cover, with a
/// checkerboard where nothing is, surfaces composited over it.
pub struct Canvas {
    /// Device pixels wide.
    pub width: u32,
    /// Device pixels high.
    pub height: u32,
    /// RGBA8, opaque.
    pub pixels: Vec<u8>,
    origin: (f32, f32),
    scale: f32,
}

/// The two greys of the checkerboard, and what is outside every screen.
const CHECK_A: [u8; 3] = [0x3A, 0x3A, 0x40];
const CHECK_B: [u8; 3] = [0x2E, 0x2E, 0x33];
const OUTSIDE: [u8; 3] = [0x0E, 0x0E, 0x10];
/// A square of the checkerboard, in logical pixels.
const CHECK_SIZE: f32 = 8.0;

impl Canvas {
    /// A canvas for the area `bounds` (`x, y, w, h`, logical) at `scale` device
    /// pixels per logical pixel, with the checkerboard on each of `screens`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn new(bounds: (f32, f32, f32, f32), screens: &[&ScreenInfo], scale: f32) -> Self {
        let width = (bounds.2 * scale).round().max(1.0) as u32;
        let height = (bounds.3 * scale).round().max(1.0) as u32;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for px in pixels.chunks_exact_mut(4) {
            px[..3].copy_from_slice(&OUTSIDE);
            px[3] = 255;
        }
        let mut canvas = Self {
            width,
            height,
            pixels,
            origin: (bounds.0, bounds.1),
            scale,
        };
        let cell = (CHECK_SIZE * scale).round().max(1.0) as u32;
        for s in screens {
            let (x0, y0) = canvas.to_device(s.x, s.y);
            let (x1, y1) = canvas.to_device(s.x + s.width, s.y + s.height);
            for y in y0.max(0)..y1.min(i64::from(height)) {
                for x in x0.max(0)..x1.min(i64::from(width)) {
                    let (cx, cy) = ((x - x0) as u32 / cell, (y - y0) as u32 / cell);
                    let colour = if (cx + cy) % 2 == 0 { CHECK_A } else { CHECK_B };
                    let at = (y as usize * width as usize + x as usize) * 4;
                    canvas.pixels[at..at + 3].copy_from_slice(&colour);
                }
            }
        }
        canvas
    }

    /// A point of the layout as a device pixel of the canvas.
    #[allow(clippy::cast_possible_truncation)]
    fn to_device(&self, x: f32, y: f32) -> (i64, i64) {
        (
            ((x - self.origin.0) * self.scale).round() as i64,
            ((y - self.origin.1) * self.scale).round() as i64,
        )
    }

    /// Composites a surface over what is there: `rgba` is `w` by `h` device
    /// pixels, straight (not premultiplied) alpha, with its top-left corner at
    /// the point `(x, y)` of the layout. What falls outside the canvas is
    /// dropped.
    pub fn over(&mut self, x: f32, y: f32, w: u32, h: u32, rgba: &[u8]) {
        let (ox, oy) = self.to_device(x, y);
        for sy in 0..i64::from(h) {
            let dy = oy + sy;
            if dy < 0 || dy >= i64::from(self.height) {
                continue;
            }
            for sx in 0..i64::from(w) {
                let dx = ox + sx;
                if dx < 0 || dx >= i64::from(self.width) {
                    continue;
                }
                #[allow(clippy::cast_sign_loss)]
                let src = (sy as usize * w as usize + sx as usize) * 4;
                #[allow(clippy::cast_sign_loss)]
                let dst = (dy as usize * self.width as usize + dx as usize) * 4;
                let a = u32::from(rgba[src + 3]);
                if a == 0 {
                    continue;
                }
                for c in 0..3 {
                    let s = u32::from(rgba[src + c]);
                    let d = u32::from(self.pixels[dst + c]);
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        self.pixels[dst + c] = ((s * a + d * (255 - a) + 127) / 255) as u8;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byard_core::surface::placement::Anchors;
    use byard_core::surface::popup::{Align, Side};
    use byard_core::surface::{Exclusive, LayerSpec, PopupSpec};

    #[test]
    fn screens_are_named_sized_scaled_and_placed() {
        let s = parse_screens("DP-1:2560x1440@1.5, HDMI-A-1:1920x1080").unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "DP-1");
        // The mode in device pixels is the logical size times the scale.
        assert_eq!((s[0].width, s[0].height, s[0].scale), (1707.0, 960.0, 1.5));
        assert_eq!((s[0].x, s[0].y), (0.0, 0.0));
        // The next one sits to its right, at scale 1.
        assert_eq!(
            (s[1].x, s[1].y, s[1].width, s[1].scale),
            (1707.0, 0.0, 1920.0, 1.0)
        );
    }

    #[test]
    fn a_position_says_where_a_screen_is_and_the_list_is_in_reading_order() {
        let s = parse_screens("B:800x600+0+600, A:1000x600+0+0, C:640x480+1000+0").unwrap();
        let names: Vec<_> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"], "ordered by x, then y");
        assert_eq!((s[1].x, s[1].y), (0.0, 600.0));
        let left = parse_screens("L:100x100-100+0, R:100x100+0+0").unwrap();
        assert_eq!(left[0].name, "L");
        assert_eq!(left[0].x, -100.0);
    }

    #[test]
    fn a_scale_and_a_position_together() {
        let s = parse_screens("X:3000x2000@2+10+20").unwrap();
        assert_eq!((s[0].width, s[0].height, s[0].scale), (1500.0, 1000.0, 2.0));
        assert_eq!((s[0].x, s[0].y), (10.0, 20.0));
    }

    #[test]
    fn a_mistake_in_the_screens_says_which_and_how_to_write_it() {
        for bad in [
            "",
            "DP-1",
            "DP-1:1920",
            ":100x100",
            "A:100x100,A:200x200",
            "A:0x100",
            "A:100x100@0",
            "A:100x100@x",
            "A:100x100+3",
            "A:axb",
        ] {
            let e = parse_screens(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad:?}");
            if !bad.is_empty() && !bad.contains("A:100x100,A:200x200") {
                assert!(e.contains("NAME:WxH"), "{bad:?}: {e}");
            }
        }
        assert!(
            parse_screens("A:100x100,A:200x200")
                .unwrap_err()
                .contains("two screens")
        );
    }

    fn bar() -> SurfaceSpec {
        let mut spec = SurfaceSpec::panel("bar");
        let mut layer = LayerSpec::new("bar");
        layer.anchors = Anchors::TOP | Anchors::LEFT | Anchors::RIGHT;
        layer.exclusive = Exclusive::Thickness;
        spec.layer = Some(layer);
        spec.size = (0.0, 40.0);
        spec
    }

    fn compositor() -> Compositor {
        Compositor::new(parse_screens("S1:800x600, S2:400x300").unwrap())
    }

    #[test]
    fn a_panel_is_placed_on_its_screen_and_a_window_in_the_area_left() {
        let mut c = compositor();
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        let (bar_id, win_id) = (SurfaceId::new(1), SurfaceId::new(2));
        c.apply(SurfaceChange::Create {
            id: bar_id,
            spec: bar(),
        });
        let mut win = SurfaceSpec::window("w");
        win.size = (200.0, 100.0);
        c.apply(SurfaceChange::Create {
            id: win_id,
            spec: win,
        });
        c.arrange(&mut interp);
        let bar = c.placed(bar_id).unwrap();
        assert_eq!((bar.screen, bar.rect), (0, Rect::new(0, 0, 800, 40)));
        // The window is centred in what the bar left: 800x560 under it.
        let win = c.placed(win_id).unwrap();
        assert_eq!(win.rect, Rect::new(300, 40 + (560 - 100) / 2, 200, 100));
    }

    #[test]
    fn a_panel_on_the_second_screen_is_placed_there_and_found_by_a_point_there() {
        let mut c = compositor();
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        let mut spec = bar();
        spec.layer.as_mut().unwrap().screen = Some("S2".into());
        c.apply(SurfaceChange::Create {
            id: SurfaceId::new(1),
            spec,
        });
        c.arrange(&mut interp);
        let p = c.placed(SurfaceId::new(1)).unwrap();
        assert_eq!((p.screen, p.rect), (1, Rect::new(0, 0, 400, 40)));
        assert_eq!(c.global(SurfaceId::new(1)), Some((800.0, 0.0)));
        assert_eq!(c.screen_name(SurfaceId::new(1)), Some("S2"));
        let (id, local) = c.hit(850.0, 10.0).unwrap();
        assert_eq!((id, local), (SurfaceId::new(1), (50.0, 10.0)));
        assert!(
            c.hit(850.0, 100.0).is_none(),
            "below the bar there is nothing"
        );
    }

    #[test]
    fn layers_stack_background_bottom_windows_top_overlay_popups() {
        let mut c = compositor();
        let make = |layer: Layer| {
            let mut spec = bar();
            spec.layer.as_mut().unwrap().layer = layer;
            spec
        };
        let ids: Vec<SurfaceId> = (1..=6).map(SurfaceId::new).collect();
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        // Made in the opposite order of how they stack.
        let popup = {
            let mut spec = SurfaceSpec::popup(
                "p",
                PopupSpec::anchored(
                    ids[3],
                    Rect::new(0, 0, 10, 10),
                    Side::Below,
                    Align::Start,
                    0,
                    true,
                    false,
                ),
            );
            spec.size = (50.0, 50.0);
            spec
        };
        c.apply(SurfaceChange::Create {
            id: ids[0],
            spec: make(Layer::Overlay),
        });
        c.apply(SurfaceChange::Create {
            id: ids[1],
            spec: make(Layer::Top),
        });
        c.apply(SurfaceChange::Create {
            id: ids[2],
            spec: SurfaceSpec::window("w"),
        });
        c.apply(SurfaceChange::Create {
            id: ids[3],
            spec: make(Layer::Bottom),
        });
        c.apply(SurfaceChange::Create {
            id: ids[4],
            spec: make(Layer::Background),
        });
        c.apply(SurfaceChange::Create {
            id: ids[5],
            spec: popup,
        });
        c.arrange(&mut interp);
        assert_eq!(
            c.back_to_front(),
            [ids[4], ids[3], ids[2], ids[1], ids[0], ids[5]]
        );
    }

    #[test]
    fn a_popup_is_placed_by_the_positioner_against_its_screen() {
        let mut c = Compositor::new(parse_screens("S:800x600").unwrap());
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        let (bar_id, pop_id) = (SurfaceId::new(1), SurfaceId::new(2));
        let mut spec = bar();
        // A bar at the bottom of the screen.
        let l = spec.layer.as_mut().unwrap();
        l.anchors = Anchors::BOTTOM | Anchors::LEFT | Anchors::RIGHT;
        c.apply(SurfaceChange::Create { id: bar_id, spec });
        let mut popup = SurfaceSpec::popup(
            "menu",
            PopupSpec::anchored(
                bar_id,
                Rect::new(10, 5, 80, 30),
                Side::Below,
                Align::Start,
                4,
                true,
                true,
            ),
        );
        popup.size = (120.0, 200.0);
        c.apply(SurfaceChange::Create {
            id: pop_id,
            spec: popup,
        });
        c.arrange(&mut interp);
        // Below the button there is no room (the bar ends the screen), so the
        // popup is flipped above it, and the screen coordinates are the bar's
        // (y 560) plus the popup's in the bar.
        let p = c.placed(pop_id).unwrap();
        assert_eq!(p.screen, 0);
        assert_eq!(p.rect, Rect::new(10, 560 - 191, 120, 200));
    }

    #[test]
    fn a_press_on_the_transparent_part_of_a_panel_reaches_what_is_below() {
        let mut c = Compositor::new(parse_screens("S:800x600").unwrap());
        let mut interp = Interpreter::new();
        interp.enable_surfaces();
        let (bar_id, win_id) = (SurfaceId::new(1), SurfaceId::new(2));
        let mut win = SurfaceSpec::window("w");
        win.size = (800.0, 600.0);
        c.apply(SurfaceChange::Create {
            id: win_id,
            spec: win,
        });
        c.apply(SurfaceChange::Create {
            id: bar_id,
            spec: bar(),
        });
        c.arrange(&mut interp);
        // The bar takes the pointer only where its button is.
        c.apply(SurfaceChange::InputRegion {
            id: bar_id,
            region: InputRegion::Rects(vec![Rect::new(10, 5, 80, 30)]),
        });
        assert_eq!(c.hit(20.0, 20.0).unwrap().0, bar_id);
        let (id, local) = c.hit(400.0, 20.0).unwrap();
        // The window is 600 tall in the 560 the bar left: centred, it starts at y 20.
        assert_eq!((id, local), (win_id, (400.0, 0.0)));
        c.apply(SurfaceChange::InputRegion {
            id: bar_id,
            region: InputRegion::None,
        });
        assert_eq!(c.hit(20.0, 20.0).unwrap().0, win_id);
    }

    #[test]
    fn a_surface_is_composited_over_a_checkerboard_by_its_alpha() {
        let screens = parse_screens("S:16x16").unwrap();
        let refs: Vec<&ScreenInfo> = screens.iter().collect();
        let mut canvas = Canvas::new((0.0, 0.0, 16.0, 16.0), &refs, 1.0);
        assert_eq!((canvas.width, canvas.height), (16, 16));
        assert_eq!(canvas.pixels[..3], CHECK_A);
        // The next square along is the other grey.
        assert_eq!(canvas.pixels[8 * 4..8 * 4 + 3], CHECK_B);
        // A 2x1 surface at (1, 0): one opaque red pixel, one half transparent white.
        canvas.over(1.0, 0.0, 2, 1, &[255, 0, 0, 255, 255, 255, 255, 128]);
        assert_eq!(canvas.pixels[4..8], [255, 0, 0, 255]);
        let px = &canvas.pixels[8..11];
        let want = |bg: u8| ((255 * 128 + u32::from(bg) * 127 + 127) / 255) as u8;
        assert_eq!(px, [want(CHECK_A[0]), want(CHECK_A[1]), want(CHECK_A[2])]);
        // What is outside the canvas is dropped, not wrapped.
        canvas.over(15.0, 15.0, 4, 4, &[9; 64]);
        canvas.over(-3.0, -3.0, 4, 4, &[9; 64]);
    }

    #[test]
    fn the_popup_chain_is_the_grab_and_its_popup_ancestors() {
        let mut c = compositor();
        let spec = |parent| {
            SurfaceSpec::popup(
                "p",
                PopupSpec::anchored(
                    parent,
                    Rect::new(0, 0, 10, 10),
                    Side::After,
                    Align::Start,
                    0,
                    true,
                    true,
                ),
            )
        };
        c.apply(SurfaceChange::Create {
            id: SurfaceId::new(1),
            spec: bar(),
        });
        c.apply(SurfaceChange::Create {
            id: SurfaceId::new(2),
            spec: spec(SurfaceId::new(1)),
        });
        c.apply(SurfaceChange::Create {
            id: SurfaceId::new(3),
            spec: spec(SurfaceId::new(2)),
        });
        // The panel is not in the chain: a press on it is outside the grab.
        assert_eq!(
            c.popup_chain(SurfaceId::new(3)),
            [SurfaceId::new(3), SurfaceId::new(2)]
        );
        assert_eq!(c.popup_chain(SurfaceId::new(2)), [SurfaceId::new(2)]);
    }
}
