# RFC-0039: Native render extensions — the zero-cost package pipeline ABI

- **Status:** Active, implemented 2026-08-05. The ABI is full as the resolved
  question promised: layout, draw, events, pipeline registration and async
  delivery all landed (`byard_core::render`, `#[byard::native_view]`,
  `encoder/pipeline.rs`), with `examples/sparkline_view` as the in-tree
  consumer and Canvas Tier-2 (RFC-0037) as the pipeline that proves the
  registration path carries a real one.

  **Deltas against this document, as written:**

  - `cx.emit` does not write "directly into the persistent instance arena".
    The arena lives on the render thread behind the frame swap, so a view
    writing into it would be exactly the cross-thread graphics access this RFC
    forbids two paragraphs later. It writes into the frame's own pool, and the
    encoder stages that into the arena in the single linear pass it stages
    every other pool in. The claim the wording defends, that a package instance
    and a core instance reach the arena *by the same code*, is what the arena
    test checks.
  - Props are attributes (`Sparkline #[data: …]`), not the parenthesised form
    the guide sketches: parentheses are content arguments in `byld`, and a
    native view has none.
  - `set_prop` lives on a `NativeProps` supertrait rather than on `NativeView`,
    because it is the half the macro writes and Rust does not let generated
    code reach into a hand-written `impl` block.
  - `texture_sampler` is the one core pipeline a view cannot emit into: its
    unit of work is one image, because sampling means binding *that* image
    before the draw, and a batch of instances names no image. A view that draws
    images registers a pipeline of its own, which is what this ABI is for.
- **Author(s):** Briany4717
- **Created:** 2026-08-04
- **Last updated:** 2026-08-04

> **Placement:** `docs/rfcs/0039-native-render-extensions.md`

---

## Summary

RFC-0028 gave packages a zero-cost boundary into Rust *logic*: a controller
compiles into the binary and is called through generated bindings with no IPC and
no serialization. This RFC is the rendering counterpart. It defines a **native
view** — a package-authored, custom-drawn widget that participates in layout,
events, the persistent GPU instance arena (RFC-0033), and its own render pipeline
at cost indistinguishable from a built-in intrinsic. It amends RFC-0005/RFC-0008's
rule that "packages ship Views, never new intrinsics": packages may now ship a
second export kind, the native view, alongside composition Views. The design
exists so that widgets too specific for core (maps, charts, rich-text editors)
live in packages **without** the performance penalty that would otherwise make
"put it in a package" the wrong call — because if a package cannot match a core
intrinsic's speed, the extension model, not the widget, is mis-designed.

## Motivation

The Aura Weather work surfaced a class of widget — the slippy map above all — that
is simultaneously (a) far too domain-specific to belong in core beside `Text` and
`Column`, and (b) impossible to build well in today's package model, which is
restricted to composing existing intrinsics from `byld`. A map needs its own tile
pipeline, its own arena-scoped tile cache, gesture handling, and async tile
delivery across the frame boundary. Composition alone cannot express that, and the
only escape hatches today are both wrong: bloat core with a narrow intrinsic, or
ship a package that draws tiles as a pile of `Image` quads and loses the batching
and cache control a real map needs.

The correct fix is neither. Byard already compiles package Rust into the app
binary (RFC-0008, RFC-0028), so a package's render code can be **monomorphized and
linked exactly like core's** — the machine code path is identical. What is missing
is the API that lets that code register a pipeline, allocate into the instance
arena, upload textures, take part in layout, and receive async results, under the
same ownership and `!Send` discipline the engine enforces on itself. That API is
this RFC. It generalizes beyond weather: any high-performance custom widget
becomes a package, and core stays small.

## Guide-level explanation

A native view is a Rust type in a package that implements `NativeView`, annotated
so the compiler exposes it to `byld` as an element with a name, props, and events —
the same surface an intrinsic presents:

```rust
use byard::render::{NativeView, Measure, Layout, Frame, RenderCtx, Event, Handled};

#[byard::native_view(name = "Sparkline")]
pub struct Sparkline {
    #[prop] data: Vec<f32>,
    #[prop] stroke: Color,
    #[event] on_hover_point: EventSlot<usize>,
}

impl NativeView for Sparkline {
    // Layout: report intrinsic size / accept the laid-out rect (via taffy).
    fn measure(&self, known: Measure) -> Measure { known.fill() }

    // Draw: emit into the instance arena and/or this view's own pipeline.
    fn render(&mut self, layout: Layout, cx: &mut RenderCtx) {
        let mesh = cx.pipeline::<FillPipeline>();       // registered once, cached
        let verts = tessellate(&self.data, layout.rect); // package's own code
        cx.emit(mesh, verts);                            // batched like an intrinsic
    }

    // Events: same model intrinsics use.
    fn on_event(&mut self, ev: Event, layout: Layout) -> Handled {
        /* hit-test against layout.rect, fire on_hover_point */ Handled::Yes
    }
}
```

In `byld` it is used exactly like any element — the package boundary is invisible
at the call site:

```
use charts as c

c.Sparkline(data: temps, stroke: accent) => hoveredPoint = it
```

Async work (fetching a tile, decoding an image) is dispatched to the pool through
the **existing RFC-0028 controller mechanism** and its result is delivered back to
the native view on the logic thread through `frame.rs`, so a native view never
blocks and never touches a graphics type off-thread.

## Reference-level explanation

**The `NativeView` trait** has four responsibilities, each mapping onto machinery
the engine already runs for intrinsics:

- `measure(&self, Measure) -> Measure` — participates in the taffy layout pass. The
  engine calls it where it calls an intrinsic's measurement; the view reports
  intrinsic size or fills its constraints.
- `render(&mut self, Layout, &mut RenderCtx)` — the draw phase, given its resolved
  rect. This is where per-instance work happens and where the "zero-cost" claim
  lives (below).
- `on_event(&mut self, Event, Layout) -> Handled` — receives routed pointer/key
  events under the same hit-testing and z-layer rules as intrinsics (RFC-0003,
  RFC-0017).
- `on_mount` / `on_unmount` (default no-op) — arena setup/teardown hooks; the
  view's state lives in its view-scoped arena and is released in the single linear
  pass at unmount, no exception to the memory model.

**Pipeline registration.** A native view draws either by emitting into an existing
core pipeline (e.g. the decorated-box or texture-sampler batch) or by registering
its **own** pipeline:

```rust
impl RenderPipeline for FillPipeline {
    const SHADER: &str = include_str!("fill.wgsl");
    type Instance = FillInstance;              // bytemuck Pod, one arena lane set
    fn vertex_layout() -> VertexLayout { … }
    fn bind_groups(cx: &PipelineCtx) -> BindGroups { … }   // textures, uniforms
}
```

Pipelines register once at startup into an ordered `Vec<Box<dyn ErasedPipeline>>`
held by the encoder. The render pass iterates registered pipelines in a
deterministic order (core pipelines first, then package pipelines by dependency
order, ties broken by registration order — declared, not incidental). This is the
*only* dynamic dispatch introduced, and it is **per-pipeline-per-frame** (a handful
of vtable calls), never per-instance.

**Why it is zero-cost.** The hot path — encoding N instances and the WGSL draw — is
the native view's own monomorphized Rust plus its own compiled shader, byte-for-byte
the same kind of code core's intrinsics compile to, because the package is linked
into the same binary (RFC-0008/RFC-0028: no dlopen, no ABI-stable C boundary, no
serialization). `cx.emit(pipeline, instances)` writes directly into the persistent
instance arena (RFC-0033) through a monomorphized generic, so a package instance and
a core instance land in the arena by the same code. The dynamic step is choosing
*which* pipelines run, which core already does implicitly and which costs O(pipelines),
not O(instances). A native view is therefore as fast as an intrinsic by construction;
if a measured native view is slower than an equivalent intrinsic, that is a defect in
this ABI to be fixed here, not an acceptable package tax.

**`RenderCtx`, the per-frame handle.** A native view's `render` receives a
`RenderCtx` exposing only safe, arena-and-thread-correct operations: `pipeline::<P>()`
(handle to a registered pipeline), `emit(handle, instances)` (append to the arena
batch), `upload_texture(bytes|handle)` (returns a `TextureHandle`, the same type tile
and image uploads already use), `clip(rect|rrect)` (RFC-0037 clip stack), and
`request_repaint()` (mark dirty for the next frame, RFC-0032). It exposes no raw
`wgpu::Device`, no queue, and no way to retain a GPU resource past the view's arena
scope — soundness is preserved by construction, not by convention.

**Async across the boundary.** Native views do no I/O themselves. They call
controllers (RFC-0028) for network/disk work; results (`HostValue`, or a
`TextureHandle` for decoded imagery) arrive on the logic thread via `frame.rs` and are
handed to the view through an `on_result` callback keyed to the request. This keeps
INV-12/INV-2 intact: only `Send` handles cross threads, never `!Send` graphics state.

**Compiler & dev-loop.** `#[native_view]` generates the same intrinsic-catalog entry
an in-tree intrinsic has (name, prop types, events), so type-checking, prop
validation, and event wiring in `byld` are identical and a native view is
indistinguishable from an intrinsic at the call site. In `byard dev`, the package's
Rust (view + pipeline) is a compiled dependency; only the `byld` that *uses* it hot-
reloads, exactly as controller-backed views already behave today.

**Capability & trust.** A native view is ordinary linked Rust: it can allocate in its
arena, register pipelines, and call controllers, but the `RenderCtx` surface bounds
what it can do to the GPU, and it inherits the app's capability set (RFC-0028) for I/O
— it cannot open a socket except through a capability the app granted. Distribution
and trust are the package ecosystem's problem (RFC-0008), unchanged here beyond
declaring that native-view packages ship compiled Rust, so a consumer builds them from
source like any dependency.

## Drawbacks

- It widens the package contract from "byld Views only" to "byld Views **or**
  compiled native views," which is more surface for the ecosystem to reason about and
  a sharper trust boundary (native views run linked Rust). Justified because the
  alternative — narrow intrinsics in core, or slow composition packages — is worse on
  both bloat and performance.
- A poorly written native view can still misuse the arena or spend too long in
  `render`; the ABI bounds *soundness*, not *taste*. The zero-alloc profiler and HUD
  (RFC-0013/RFC-0030) attribute frame cost to the offending view, so a slow extension
  is visible, but the ABI cannot make every extension fast, only make fast extensions
  possible.
- Registered pipelines add pipeline-state changes at their boundaries. Bounded (a few
  extra pipelines), deterministic in order, and paid per-frame not per-instance.

## Rationale and alternatives

- **Why compiled/linked extensions rather than a dynamic plugin ABI?** A dynamic
  (`dlopen`, C-ABI, or WASM) boundary would reintroduce exactly the serialization/IPC/
  indirection cost Byard was built to avoid, and would make a package *structurally*
  slower than core — the outcome this RFC exists to prevent. Byard already compiles
  package Rust into the binary, so the fast path is free; taking it is the whole point.
- **Why per-pipeline dynamic dispatch is acceptable.** The cost is O(pipelines) vtable
  calls per frame, which core already pays implicitly across its own pipelines; the
  per-instance hot path stays fully monomorphized. Enum-dispatch over a closed pipeline
  set is available as an optimization if even that proves measurable, but it will not.
- **Why bound the GPU surface in `RenderCtx` instead of exposing `wgpu` directly.**
  Handing out `Device`/`Queue` would let a native view retain resources past its arena
  scope and violate the no-VRAM-spike / single-pass-release guarantees that are
  first-class correctness criteria. A narrow, arena-correct `RenderCtx` keeps soundness
  a property of the type system, not of extension authors' discipline.
- **Rejected: keep the map/chart in core.** A map intrinsic beside `Column` is the
  anti-pattern Brian identified; it bloats core with a domain widget most apps never
  use, and every future domain widget would demand the same, with no principled stop.
- **Rejected: leave packages composition-only and accept the slowdown.** Contradicts
  the stated principle that a package must run "almost as if written directly in core";
  a slow map package would just push people back toward wanting it in core.

## Prior art

This is the rendering analog of Byard's own RFC-0028 controller boundary. Bevy's
`RenderPlugin`/`Node` render-graph extension and its ECS-driven pipeline registration
(compiled-in, zero dynamic boundary) are the closest external model. egui's custom
`Callback`/paint hooks and Flutter's `LeafRenderObjectWidget` + `CustomPainter` show
the "package supplies a leaf that draws itself" shape, but both sit inside a single
binary the same way this does. The compiled-not-dynamic stance mirrors how game engines
let gameplay code register render passes without a plugin ABI tax.

## Resolved questions

**Compiled/linked extensions or a dynamic plugin ABI?** Resolved: compiled and linked
into the app binary, like RFC-0028 controllers. Reasoning: a dynamic boundary reimposes
serialization/indirection and makes packages structurally slower than core, which is the
exact failure this RFC prevents; Byard already compiles package Rust in, so the zero-cost
path is available and taking it is the design's reason to exist.

**Is per-pipeline dynamic dispatch a real cost?** Resolved: no — it is O(pipelines)
vtable calls per frame, identical in kind to what core already does across its own
pipelines, with the per-instance hot path fully monomorphized. Reasoning: the cost the
weather work cares about is per-instance (thousands of tiles/vertices), and that path
carries zero dynamic overhead; a handful of per-frame indirect calls is unmeasurable and
enum-dispatch is held in reserve if ever proven otherwise.

**Expose `wgpu` directly or a bounded `RenderCtx`?** Resolved: a bounded `RenderCtx`
that offers arena emission, texture upload, clip, and repaint, and nothing that can
retain a resource past the view's arena scope. Reasoning: direct device access would let
an extension break the single-pass-release and bounded-VRAM guarantees that are
first-class correctness criteria; making soundness a property of the exposed types, not
of author discipline, is consistent with how the whole framework treats memory.

**How wide is the ABI in v1 — full or minimal?** Resolved: full (layout + events +
pipeline registration + async delivery), because the driving use case (the map) needs
all four, and a minimal draw-only ABI would leave the map unbuildable and force a second
redesign. Reasoning: the four responsibilities each map onto machinery the engine already
runs for intrinsics, so exposing them is wiring, not new subsystems; shipping a partial
ABI would validate nothing, since the proof that packages can match core is precisely a
package as demanding as the map succeeding on it.

**Does the memory model bend for extensions?** Resolved: no. A native view's state lives
in its view-scoped arena and is released in the single linear pass at unmount; GPU
resources cross threads only as `Send` handles; `RenderCtx` cannot leak a resource past
scope. Reasoning: the arena/`!Send` discipline is the core's correctness backbone, and an
extension that could opt out of it would make the whole guarantee conditional on trusting
every package — unacceptable, and unnecessary given the bounded `RenderCtx`.
