# Native views

A chart, a map, a gauge: some widgets need their own drawing. A *native view* is a Rust
type that lays out, draws and handles events like an element of the language, and is used
from byld by name. It is compiled into your binary and reaches the GPU the way the
engine's own boxes do, so it costs what they cost.

The example to read is
[`examples/sparkline_view`](https://github.com/Briany4717/byard/tree/main/examples/sparkline_view).
Run it with `cargo run -p sparkline-view`. Aura Weather's map is another.

## The view

A native view is a struct. Mark it with `#[byard::native_view(name = "…")]`, and mark each
field that byld may set with `#[prop]`:

```rust,ignore,excerpt
#[byard::native_view(name = "Sparkline")]
#[derive(Default)]
struct Sparkline {
    /// The series, in whatever units the caller likes; the view scales to fit.
    #[prop]
    data: Vec<f32>,
    /// Bar colour.
    #[prop]
    bar: u32,
    /// Colour of the bar under the pointer.
    #[prop]
    highlight: u32,
    /// Gap between bars, in logical pixels. Marked `layout` because it is the
    /// one prop that could change how much room the widget wants.
    #[prop(layout)]
    gap: f32,
    /// Which bar the pointer is over, if any. Not a prop: it is the view's own
    /// state, which is exactly the kind of thing a native view exists to keep.
    hovered: Option<usize>,
    /// Whether the hover moved since the last frame, so `render` can ask for
    /// the frame that shows it.
    repaint: bool,
}
```

The name is how byld writes it, and it defaults to the struct's own name. A field without `#[prop]` is the view's own state, such as
`hovered` above, which is what a native view exists to keep. The struct must be `Default`,
because one is made for every element that uses it.

A prop's type is one a byld value can fill:

| Rust type | byld | Declared as |
|-----------|------|-------------|
| `i8`, `i16`, `i32`, `i64`, `isize`, `u8`, `u16`, `u64`, `usize` | a number | `int` |
| `f32`, `f64` | a number | `float` |
| `bool` | `true` or `false` | `bool` |
| `String` | text | `str` |
| `u32` (the one `u32` is a colour, not a number) | a colour | `color` |
| `(f32, f32)` | a pair | `vec2` |
| `Vec` of a number type, such as `Vec<f32>` | a list | `floats` |

`#[prop(layout)]` says that changing the prop can change the size the view wants, as
`gap` does. The engine uses it to decide whether a change needs a layout or only a redraw,
so leave it off for a prop that only changes pixels. A field marked `#[event]` instead
declares an event name the view raises.

## Drawing it

The macro writes the glue. What the view *is*, you write, by implementing `NativeView`.
Only `render` is required:

```rust,ignore,excerpt
impl NativeView for Sparkline {
    /// Fills whatever the layout gives it, which is what a chart wants: the
    /// card decides how much room the data gets, not the data.
    fn measure(&self, known: Measure) -> Measure {
        known.keep()
    }

    fn render(&mut self, layout: Layout, cx: &mut RenderCtx<'_>) {
        if std::mem::take(&mut self.repaint) {
            cx.request_repaint();
        }
        if self.data.is_empty() {
            return;
        }
        let peak = self.peak();
        let pipeline = cx.pipeline::<byard::render::SolidBoxPipeline>();
        // One batch for the whole series. A hundred bars is a hundred
        // instances and one draw, not a hundred elements.
        let bars: Vec<byard::render::BoxInstance> = (0..self.data.len())
            .map(|i| byard::render::BoxInstance {
                rect: self.bar_rect(layout, i, peak),
                color: byard::render::rgba(if self.hovered == Some(i) {
                    self.highlight
                } else {
                    self.bar
                }),
                radii: [2.0; 4],
                transform: byard::render::Transform::IDENTITY,
                smooth: 0.0,
```

- **`measure`** gives the view's size, given what layout has decided. The default fills what
  it is offered, which suits a chart: the card decides how much room the data gets.
- **`render`** draws, from the box layout resolved for the view. It does not draw pixels
  itself. It asks the context for a pipeline and emits instances into it. Here one batch holds
  every bar, so a hundred bars are a hundred instances and one draw, not a hundred elements.
- **`on_event`** handles a routed input event, in the view's own coordinates, and says
  whether it took it. Declining passes the event on to whatever is behind.
- **`on_mount`** and **`on_unmount`** mark the element appearing and going.
- **`on_result`** receives the answer to a controller call the view made.

The context hands out no device, no queue and no texture to keep. A view that needs a frame
because something changed inside it, such as a hover that byld cannot see, calls
`cx.request_repaint()`.

A view that needs pixels no engine pipeline draws describes a shader of its own: the
instance record, its fields and the WGSL, registered with `App::shader`. The engine builds
the pipeline. The `Glow` view in the example does this.

## Asking for data

A native view never waits or touches a thread. When it needs something, it calls a
[controller](rust-controllers.md) with `cx.call`, keyed by whatever it likes, and is
answered in `on_result` on the logic thread. The dashed mean line in the example does
exactly this: it notices its data changed, asks the `Stats` controller for the mean, and
draws the last answer it was given until a better one arrives. Nothing blocks the frame.

A controller hands a view a picture as `HostValue::Bytes`.

## Registering it

```rust,ignore,excerpt
fn app() -> byard::App {
    byard::App::new(env!("CARGO_MANIFEST_DIR"))
        .title("Sparkline, a native view")
        .size(760, 560)
        .shader::<GlowShader>()
        .native_view::<Glow>()
        .native_view::<Sparkline>()
        .native_view::<MeanLine>()
        .provide(Stats)
}
```

Registering two views under one name is an error, and `run` says so. Which widget appears
must not depend on the order two calls happened in.

## Telling the tools about it

`byard check`, `byard dev` and the editor are programs that never link your Rust. To them,
`Sparkline` would be an unknown element. So declare it in `byard.toml`, with the properties
it takes:

```toml
[native_views.Sparkline]
data = "floats"
bar = "color"
highlight = "color"
gap = { type = "float", layout = true }

[native_views.MeanLine]
of = "floats"
stroke = "color"
```

The types are `int`, `float`, `bool`, `str`, `color`, `vec2` and `floats`, and `layout =
true` is the manifest's way to say `#[prop(layout)]`. An element's name starts with a capital
letter.

With that, the tools check every use of the element, its properties included, and draw a
faint panel in its place. The app itself refuses to start if a declaration stops matching the
view registered under that name, so the two cannot drift apart.

## Using it

It reads like any element, and a mistake in it is an ordinary error with a span:

```byld,ignore
// Sparkline is declared in byard.toml and written in Rust, so this file needs both.
View Main() {
    var series = [12.0, 30.0, 18.0, 44.0, 26.0, 51.0, 33.0]

    Column #[bg: 0x14141C, p: 32, gap: 24] {
        Box #[bg: 0x1C1C26, radius: 16, p: 20] {
            Sparkline #[data: series, bar: 0x5B8DEF, highlight: 0xF9A8A8, gap: 6.0,
                        width: 680, height: 220]
        }
        Button("add") => series = series + [series.max() * 1.2]
    }
}
```

It takes `width`, `height`, `bg`, `opacity` and the rest of the layout and decoration
attributes, plus its own. `data: series` is re-evaluated every tick, so pressing the button
redraws the chart with no wiring: the view owns no signal.

A typo in a property is an error, and so is text given to a number property, for a native
view as for any element.

## A package can ship one

A native view in a package is how a design system or a library ships a widget. It is the same
Rust type, registered by the app that uses the package, and declared in that app's manifest.
