//! The map: the one part of Aura Weather written in Rust.
//!
//! Everything else in the app is `byld`. A slippy map is not something `byld`
//! composes out of boxes: it needs images that arrive one at a time over the
//! network, drawn many per frame, with a look no stock element has. So it is
//! the app's own element, with the app's own shader, and to `byld` it is
//! still just `Map #[lat: …, lon: …, zoom: …]`.
//!
//! Three pieces:
//!
//! - [`MapTiles`], a shader. It draws a tile from a layer of an array
//!   texture, rounds the card's corners itself, and repaints the daytime map
//!   tiles in the app's night colours.
//! - [`Map`], the view. It works out which tiles its rectangle shows, asks
//!   for the ones it does not have, and emits every one it does in a single
//!   batch. Dragging pans it.
//! - [`Tiles`], a controller. It fetches a tile and decodes it, on the pool,
//!   and hands the pixels back. The view never waits: a tile that has not
//!   arrived is simply not drawn yet.
//!
//! The tiles are OpenStreetMap's, used under its tile usage policy: a
//! `User-Agent` that names this app, a handful of tiles per view, and the
//! attribution on screen (it is in `map.byd`).

use std::collections::HashMap;

use byard::bridge::HostValue;
use byard::render::{
    ClipShape, Event, EventKind, Handled, Lane, Layers, Layout, Measure, NativeView, RenderCtx,
    RequestKey, Shader,
};

/// A tile's edge, in the pixels the tile server sends and in the logical
/// pixels it is drawn at.
const TILE: u32 = 256;
#[allow(clippy::cast_precision_loss)]
const TILE_F: f64 = TILE as f64;
/// How many tiles the view keeps on the GPU. A phone-sized card shows nine at
/// most; the rest is what makes a pan back the way it came instant.
const LAYER_COUNT: u32 = 48;
/// The zoom levels the tile server has.
const MIN_ZOOM: f32 = 2.0;
const MAX_ZOOM: f32 = 18.0;

/// One tile on screen.
#[repr(C)]
#[derive(Clone, Copy, byard::render::bytemuck::Pod, byard::render::bytemuck::Zeroable)]
#[bytemuck(crate = "byard::render::bytemuck")]
pub struct TileInstance {
    /// Where the tile is drawn, `[x, y, width, height]` in logical pixels.
    rect: [f32; 4],
    /// The card the map lives in, which the shader clips to.
    card: [f32; 4],
    /// Which layer holds this tile's pixels.
    layer: u32,
    /// The card's corner radius.
    radius: f32,
}

/// The map's shader: a tile from its layer, clipped to the card's rounded
/// rectangle, in night colours.
pub struct MapTiles;

impl Shader for MapTiles {
    const NAME: &'static str = "aura_map_tiles";
    const LAYERS: Option<Layers> = Some(Layers {
        width: TILE,
        height: TILE,
        count: LAYER_COUNT,
    });
    type Instance = TileInstance;

    fn lanes() -> &'static [Lane] {
        &[Lane::Vec4, Lane::Vec4, Lane::U32, Lane::F32]
    }

    fn wgsl() -> &'static str {
        "
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec2<f32>,
    @location(2) @interpolate(flat) card: vec4<f32>,
    @location(3) @interpolate(flat) layer: u32,
    @location(4) @interpolate(flat) radius: f32,
};

@vertex
fn vs_main(
    @location(0) quad: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(2) card: vec4<f32>,
    @location(3) layer: u32,
    @location(4) radius: f32,
    @location(15) depth: f32,
) -> Out {
    var out: Out;
    out.world = rect.xy + quad * rect.zw;
    out.position = to_clip(out.world, depth);
    out.uv = quad;
    out.card = card;
    out.layer = layer;
    out.radius = radius;
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    // How far this pixel is inside the card's rounded rectangle.
    let half = in.card.zw * 0.5;
    let q = abs(in.world - (in.card.xy + half)) - half + vec2<f32>(in.radius);
    let outside = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - in.radius;
    let cover = clamp(0.5 - outside, 0.0, 1.0);

    // The tile is a daytime map: light land, dark lettering. Turn it over, so
    // the land is dark and the lettering light, keep a little of its colour,
    // and lean the result towards the app's blue.
    // The turn is made on the encoded values, the ones the eye reads as even
    // steps: turning the linear ones over leaves the land far too bright.
    let day = textureSample(layers, layer_sampler, in.uv, in.layer).rgb;
    let turned = pow(vec3<f32>(1.0) - pow(day, vec3<f32>(1.0 / 2.2)), vec3<f32>(2.2));
    let grey = dot(turned, vec3<f32>(0.299, 0.587, 0.114));
    let night = mix(vec3<f32>(grey), turned, 0.35) * vec3<f32>(0.75, 0.95, 1.5)
        + vec3<f32>(0.006, 0.010, 0.026);
    return vec4<f32>(night, cover);
}
"
    }
}

/// A tile's address: zoom, column, row.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileId {
    z: u32,
    x: u32,
    y: u32,
}

impl TileId {
    /// The request key a tile is asked for under, and found again by.
    fn key(self) -> RequestKey {
        RequestKey((u64::from(self.z) << 48) | (u64::from(self.x) << 24) | u64::from(self.y))
    }
}

/// Where a place is on the map of the whole world at zoom `z`, in pixels
/// from its top-left corner (Web Mercator).
#[must_use]
pub fn project(lat: f64, lon: f64, z: u32) -> (f64, f64) {
    let size = TILE_F * f64::from(1_u32 << z);
    // The projection has no poles; the tile server's world stops here.
    let lat = lat.clamp(-85.0511, 85.0511).to_radians();
    let x = (lon + 180.0) / 360.0 * size;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * size;
    (x, y)
}

/// The tiles a `width` by `height` window centred on world pixel `centre`
/// shows at zoom `z`, each with where its top-left corner falls relative to
/// the window's.
///
/// Columns wrap around the world; rows do not, so a window past a pole shows
/// nothing there.
#[must_use]
pub fn visible_tiles(
    centre: (f64, f64),
    z: u32,
    width: f32,
    height: f32,
) -> Vec<(TileId, [f32; 2])> {
    let count = i64::from(1_u32 << z);
    let left = centre.0 - f64::from(width) / 2.0;
    let top = centre.1 - f64::from(height) / 2.0;
    #[allow(clippy::cast_possible_truncation)]
    let span = |from: f64, extent: f32| {
        let first = (from / TILE_F).floor() as i64;
        let last = ((from + f64::from(extent)) / TILE_F).ceil() as i64;
        first..last
    };
    let mut tiles = Vec::new();
    for row in span(top, height) {
        if row < 0 || row >= count {
            continue;
        }
        for column in span(left, width) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let at = [
                (column as f64 * TILE_F - left) as f32,
                (row as f64 * TILE_F - top) as f32,
            ];
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let id = TileId {
                z,
                x: column.rem_euclid(count) as u32,
                y: row as u32,
            };
            tiles.push((id, at));
        }
    }
    tiles
}

/// What the view knows about one tile it has a layer for.
struct Slot {
    layer: u32,
    /// Whether its pixels are in the layer yet.
    ready: bool,
    /// The frame it was last on screen, for choosing which to give up.
    seen: u64,
}

/// A pannable map of the place the app is showing.
#[byard::native_view(name = "Map")]
#[derive(Default)]
pub struct Map {
    /// The place at the centre, until the map is dragged.
    #[prop]
    lat: f32,
    #[prop]
    lon: f32,
    /// The zoom level, a whole number between 2 and 18.
    #[prop]
    zoom: f32,
    /// The card's corner radius.
    #[prop]
    radius: f32,
    /// The colour of the pin at the place.
    #[prop]
    pin: u32,

    /// How far the map has been dragged from the place, in logical pixels at
    /// the current zoom.
    pan: (f32, f32),
    /// The place and zoom `pan` is relative to. A new place puts the map back
    /// on it.
    panned_from: (f32, f32, f32),
    /// Where the pointer was when it last moved with the button down.
    drag: Option<(f32, f32)>,
    /// The tiles that have a layer.
    slots: HashMap<TileId, Slot>,
    /// Pixels that arrived and are waiting for the next frame to be written.
    arrived: Vec<(TileId, Vec<u8>)>,
    frame: u64,
    repaint: bool,
}

impl Map {
    fn level(&self) -> u32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            self.zoom.clamp(MIN_ZOOM, MAX_ZOOM).round() as u32
        }
    }

    /// The world pixel at the centre of the view.
    fn centre(&self) -> (f64, f64) {
        let (x, y) = project(f64::from(self.lat), f64::from(self.lon), self.level());
        (x - f64::from(self.pan.0), y - f64::from(self.pan.1))
    }

    /// A layer for `id`: a free one, or the one whose tile has gone longest
    /// without being on screen. `None` when every layer is on screen now.
    fn layer_for(&mut self, id: TileId) -> Option<u32> {
        #[allow(clippy::cast_possible_truncation)]
        let used = self.slots.len() as u32;
        let layer = if used < LAYER_COUNT {
            // Layers are handed out in order and only ever reused, so the
            // count of slots is the first one never used.
            used
        } else {
            let (&oldest, slot) = self
                .slots
                .iter()
                .filter(|(_, slot)| slot.seen < self.frame)
                .min_by_key(|(_, slot)| slot.seen)?;
            let layer = slot.layer;
            self.slots.remove(&oldest);
            layer
        };
        self.slots.insert(
            id,
            Slot {
                layer,
                ready: false,
                seen: self.frame,
            },
        );
        Some(layer)
    }

    /// How many tiles the view holds, and how many of them have arrived.
    #[must_use]
    pub fn tiles(&self) -> (usize, usize) {
        (
            self.slots.len(),
            self.slots.values().filter(|slot| slot.ready).count(),
        )
    }
}

impl NativeView for Map {
    /// Fills the card it is given.
    fn measure(&self, known: Measure) -> Measure {
        known.keep()
    }

    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
        self.frame += 1;
        if std::mem::take(&mut self.repaint) {
            cx.request_repaint();
        }
        // A new place, or a new zoom, and the drag no longer means anything.
        let from = (self.lat, self.lon, self.zoom);
        if self.panned_from != from {
            self.panned_from = from;
            self.pan = (0.0, 0.0);
        }

        // Tiles that arrived since the last frame go into their layers.
        for (id, pixels) in std::mem::take(&mut self.arrived) {
            if let Some(slot) = self.slots.get_mut(&id) {
                cx.write_layer::<MapTiles>(slot.layer, &pixels);
                slot.ready = true;
            }
        }

        let [x, y, width, height] = layout.rect;
        let mut drawn = Vec::new();
        for (id, at) in visible_tiles(self.centre(), self.level(), width, height) {
            let Some(slot) = self.slots.get_mut(&id) else {
                // Asked for once. Until it answers, nothing is drawn there
                // and the frame goes on without it.
                if self.layer_for(id).is_some() {
                    cx.call(
                        id.key(),
                        "Tiles",
                        "tile",
                        vec![
                            HostValue::Int(i64::from(id.z)),
                            HostValue::Int(i64::from(id.x)),
                            HostValue::Int(i64::from(id.y)),
                        ],
                    );
                }
                continue;
            };
            slot.seen = self.frame;
            if slot.ready {
                #[allow(clippy::cast_precision_loss)]
                drawn.push(TileInstance {
                    rect: [x + at[0], y + at[1], TILE as f32, TILE as f32],
                    card: layout.rect,
                    layer: slot.layer,
                    radius: self.radius,
                });
            }
        }

        // The tiles reach past the card on every side. The scissor keeps
        // them to its rectangle; the shader rounds the corners.
        cx.clip(ClipShape::Rect(layout.rect), |cx| {
            // One batch, however many tiles.
            cx.emit(cx.shader::<MapTiles>(), &drawn);

            // The pin, where the place is: it moves with the map.
            let at = (x + width / 2.0 + self.pan.0, y + height / 2.0 + self.pan.1);
            let dot = |size: f32, colour: [f32; 4]| byard::render::BoxInstance {
                rect: [at.0 - size / 2.0, at.1 - size / 2.0, size, size],
                color: colour,
                radii: [size / 2.0; 4],
                transform: byard::render::Transform::IDENTITY,
                smooth: 0.0,
            };
            let pin = byard::render::rgba(self.pin);
            cx.emit(
                cx.pipeline::<byard::render::SolidBoxPipeline>(),
                &[
                    dot(26.0, [pin[0], pin[1], pin[2], 0.28]),
                    dot(14.0, [1.0, 1.0, 1.0, 1.0]),
                    dot(9.0, pin),
                ],
            );
        });
    }

    /// Dragging pans the map.
    fn on_event(&mut self, event: &Event, _layout: Layout) -> Handled {
        match event.kind {
            EventKind::PointerDown => {
                self.drag = Some(event.local);
                // Claimed: a press on the map is the start of a drag. The zoom
                // buttons drawn over it get their own presses first.
                Handled::Yes
            }
            EventKind::PointerMove => {
                let Some(last) = self.drag else {
                    return Handled::No;
                };
                self.pan.0 += event.local.0 - last.0;
                self.pan.1 += event.local.1 - last.1;
                self.drag = Some(event.local);
                self.repaint = true;
                Handled::Yes
            }
            EventKind::PointerUp | EventKind::PointerExit => {
                self.drag = None;
                Handled::No
            }
            _ => Handled::No,
        }
    }

    /// A tile's pixels, or the news that it is not coming.
    fn on_result(&mut self, key: RequestKey, value: &HostValue) {
        let Some(id) = self.slots.keys().copied().find(|id| id.key() == key) else {
            // It was given up while its answer was on the way.
            return;
        };
        if let HostValue::Bytes(pixels) = value {
            self.arrived.push((id, pixels.clone()));
            self.repaint = true;
        }
        // Anything else is a failure. The slot stays, not ready, so the tile
        // is not asked for again on every frame; it is asked for again when
        // it next comes into view after being given up.
    }
}

/// Fetches map tiles (the only I/O the map does, and none of it in the view).
#[byard::byard_controller]
#[derive(Clone)]
pub struct Tiles {
    /// Where tiles come from: `{base}/{z}/{x}/{y}.png`.
    base: String,
    client: reqwest::Client,
}

impl Tiles {
    /// A tile source at `base`. The app's is OpenStreetMap; the tests' is a
    /// server on this machine.
    ///
    /// # Panics
    ///
    /// If the HTTP client cannot be built, which is a broken TLS setup and
    /// not something the app can run without.
    #[must_use]
    pub fn at(base: &str) -> Self {
        let client = reqwest::Client::builder()
            // The tile usage policy asks for a client that says who it is.
            .user_agent(concat!(
                "AuraWeather/",
                env!("CARGO_PKG_VERSION"),
                " (a Byard example app; https://github.com/Briany4717/byard)"
            ))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("an HTTP client");
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        }
    }
}

fn failed(message: String) -> HostValue {
    HostValue::Record(vec![("message".into(), HostValue::Str(message))])
}

#[byard::byard_controller]
impl Tiles {
    /// One tile, as `256 * 256 * 4` bytes of RGBA.
    async fn tile(&self, z: i64, x: i64, y: i64) -> Result<HostValue, HostValue> {
        let url = format!("{}/{z}/{x}/{y}.png", self.base);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| failed(format!("{url}: {e}")))?;
        let body = response
            .bytes()
            .await
            .map_err(|e| failed(format!("{url}: {e}")))?;
        let image = image::load_from_memory(&body)
            .map_err(|e| failed(format!("{url}: {e}")))?
            .to_rgba8();
        if image.dimensions() != (TILE, TILE) {
            return Err(failed(format!(
                "{url}: a {}x{} tile, expected {TILE}x{TILE}",
                image.width(),
                image.height()
            )));
        }
        Ok(HostValue::Bytes(image.into_raw()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_world_is_one_tile_at_zoom_zero() {
        let (x, y) = project(0.0, 0.0, 0);
        assert!(
            (x - 128.0).abs() < 1e-6 && (y - 128.0).abs() < 1e-6,
            "{x} {y}"
        );
        // Greenwich's column and row at zoom 10, as every slippy map has it.
        let (x, y) = project(51.4779, -0.0015, 10);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let tile = ((x / TILE_F) as u32, (y / TILE_F) as u32);
        assert_eq!(tile, (511, 340));
    }

    #[test]
    fn a_window_shows_the_tiles_it_overlaps_and_no_others() {
        // Centred on the corner four tiles share, a window smaller than a
        // tile shows exactly those four.
        let centre = (TILE_F * 3.0, TILE_F * 2.0);
        let tiles = visible_tiles(centre, 3, 200.0, 100.0);
        let ids: Vec<(u32, u32)> = tiles.iter().map(|(id, _)| (id.x, id.y)).collect();
        assert_eq!(ids, [(2, 1), (3, 1), (2, 2), (3, 2)]);
        // The first tile's corner is a tile up and left of the centre.
        let [left, top] = tiles[0].1;
        assert!(
            (left + 156.0).abs() < 1e-3 && (top + 206.0).abs() < 1e-3,
            "{left} {top}"
        );
    }

    #[test]
    fn columns_wrap_around_the_world_and_rows_do_not() {
        // At zoom 1 the world is two tiles across. A window over its left
        // edge shows the last column to the left of the first.
        let tiles = visible_tiles((0.0, 256.0), 1, 100.0, 100.0);
        let columns: Vec<u32> = tiles.iter().map(|(id, _)| id.x).collect();
        assert!(columns.contains(&1) && columns.contains(&0), "{columns:?}");
        // Above the top row there is nothing.
        let tiles = visible_tiles((256.0, 0.0), 1, 100.0, 100.0);
        assert!(
            tiles.iter().all(|(id, at)| id.y == 0 && at[1] >= -50.0),
            "{tiles:?}"
        );
    }

    #[test]
    fn a_tiles_key_finds_it_again() {
        let a = TileId {
            z: 12,
            x: 2005,
            y: 1543,
        };
        let b = TileId {
            z: 12,
            x: 1543,
            y: 2005,
        };
        assert_ne!(a.key(), b.key());
        assert_eq!(
            a.key(),
            TileId {
                z: 12,
                x: 2005,
                y: 1543
            }
            .key()
        );
    }

    #[test]
    fn a_full_map_gives_up_the_tile_longest_off_screen() {
        let mut map = Map {
            frame: 1,
            ..Map::default()
        };
        for x in 0..LAYER_COUNT {
            let id = TileId { z: 5, x, y: 0 };
            assert_eq!(map.layer_for(id), Some(x));
            // Tile 7 was last on screen longest ago.
            map.slots.get_mut(&id).unwrap().seen = u64::from(x != 7);
        }
        map.frame = 2;
        let newcomer = TileId { z: 5, x: 99, y: 0 };
        assert_eq!(map.layer_for(newcomer), Some(7), "it takes tile 7's layer");
        assert!(!map.slots.contains_key(&TileId { z: 5, x: 7, y: 0 }));

        // With every tile on screen this frame, there is nothing to give up.
        for slot in map.slots.values_mut() {
            slot.seen = 2;
        }
        assert_eq!(map.layer_for(TileId { z: 5, x: 100, y: 0 }), None);
    }
}
