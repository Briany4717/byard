## What does this PR do?

Implements the ripple slice of RFC-0023 (paint effects): four paint-time style properties — `ripple: Color`, `ripple_active: Bool`, `ripple_radius: Float`, `ripple_duration: Int` — backed by a new `Ripple` render pipeline (the seventh). A press spawns a `frame::RippleInstance` at the exact tap point (`EventRouter::press_gesture`); the evaluator samples its ease-out expansion and linear fade through the shared `Motion` closed forms each tick (the RFC-0010 model as landed) and emits it between the element's background push and its child walk, so the emission-order draw depth composites it background → ripple → children with no new render pass. The fragment shader rasterises the circle clipped to the element's rounded rect (analytic SDF, same shape function as `DecoratedBox`) and composites with premultiplied-alpha "over" blending, so light ink brightens dark surfaces, dark ink darkens light ones, and rapid taps pool their ink where the circles overlap. Live ripples join the RFC-0010 active-animation set and the incremental dirty-scissor union.

## Linked issue

Closes #136

## New surface area

- `frame::RippleInstance` (Pod, GPU-ready) + `RenderFrame::{push_ripple, ripples, ripple_clips}` + `LayerMark::ripple` — the ripple pool crosses the Evaluator → Encoder boundary in `frame.rs` like every primitive (RFC-0001 §9); depth is stamped at push (the `VectorInstance` model).
- `encoder::ripple` (`build_pipeline`, `draw`, `RippleInstance::layout`) + `ripple.wgsl` — transparent no-depth-write additive pipeline sharing the viewport bind group; zero cost when no ripple is live (draw skipped on an empty pool).
- `EventRouter::press_gesture(elem) -> Option<((f32, f32), u64)>` — exposes the in-flight press's position (the RFC-0023 ripple origin) and timestamp (the press identity that makes spawning edge-triggered: a hold spawns once, each rapid tap spawns its own).
- Compiler: `EFFECTS` prop group (`ripple`/`ripple_active`/`ripple_radius`/`ripple_duration`) on every box-path intrinsic, validated and Levenshtein-hinted like all catalog props; byld-lsp hover docs extended.

## Tasks

- [x] `RippleInstance` + pool/clip/layer plumbing in `frame.rs`
- [x] `Ripple` wgpu pipeline (additive blend, rounded-rect clip SDF, RFC-0011 transform support)
- [x] Encoder integration: draw in the layered UI pass, dirty-scissor union, frame bookkeeping
- [x] Catalog props + validation + LSP docs
- [x] Press-gesture spawning (edge-triggered), per-element emission, time-based retirement, active-animation participation
- [x] Unit tests (compiler + core) and GPU readback tests (platform)
- [x] Runnable example `crates/byard-cli/examples/ripple` + `byard check` guard
- [x] CHANGELOG entry

## Acceptance criteria

- [x] A press spawns exactly one ripple at the tap point; a hold never respawns; rapid taps spawn one each — `a_press_spawns_a_ripple_at_the_tap_point`, `a_hold_spawns_once_while_rapid_taps_spawn_one_ripple_each`.
- [x] The ink expands monotonically (ease-out) to the farthest-corner radius by default and retires after `ripple_duration` (default 300 ms), releasing the frame demand — `ripple_expands_monotonically_and_retires_after_its_duration`, `the_auto_max_radius_reaches_the_farthest_corner`.
- [x] `ripple_radius` / `ripple_duration` overrides are honoured — `ripple_radius_and_duration_props_override_the_defaults`.
- [x] `ripple:` without a trigger never inks — `a_ripple_without_an_active_trigger_never_spawns`.
- [x] Compositing sits background → ripple → children — `ripple_depth_sits_between_the_background_and_the_children` (depths) and GPU readback `ripple_depth_keeps_children_crisp_above_the_ink` (pixels).
- [x] The ink composites over both dark and light surfaces (light ink brightens, dark ink darkens) and never bleeds past a rounded corner — GPU readback `ripple_ink_composites_over_light_and_dark_and_clips_to_the_rounded_corner`.
- [x] The four props validate on the box render path with typo hints — `ripple_props_are_accepted_on_the_box_render_path`, `a_misspelled_ripple_prop_suggests_the_real_one`.
- [x] Pool bookkeeping (depth/clip/layer stamping, clear) — `push_ripple_stamps_depth_clip_and_layer_cursor`.
- [x] The committed example checks clean through the real binary — `ripple_example_checks_clean`.
- [x] Manual verification: `cd crates/byard-cli/examples/ripple && cargo run -p byard-cli -- dev` — tap point origin, corner clipping, crisp label, additive rapid taps, per-style overrides (steps documented in the example header).

## Checklist

- [x] `cargo fmt --all` passes
- [x] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [x] `cargo test --workspace` passes
- [x] New public items have doc comments
- [x] `CHANGELOG.md` updated under `[Unreleased]` (Added: Material ripple ink, RFC-0023)
- [x] Consistent with RFC-0001: the ripple pool crosses subsystems only through `frame.rs` (§9 dependency graph); the pipeline wraps its whole create sequence in a validation error scope (§8); paint-time only, never touches layout (INV-8).

## Notes for reviewers

- `31a6b51` (feat(render)): the engine half — `RippleInstance`, the seventh pipeline, encoder plumbing, GPU readback tests.
- `64888b3` (feat(compiler)): the language half — `EFFECTS` catalog group, `press_gesture`, spawn/emission/retirement in the evaluator, LSP docs.
- `a846fd1` (feat(cli)): the proof — runnable example, `byard check` guard, changelog.
- One deliberate deviation from the RFC's *reference sketch* (not its contract): the reference describes a shader-side clock (`start_time` + time uniform). As landed, the evaluator samples the same closed forms on the logic thread each tick — identical observable behaviour, because frames are already re-produced every tick while any animation is active (RFC-0010 as landed for every paint prop), and it avoids a per-frame time uniform plus encoder plumbing. The RFC-visible contract (tap-point origin, ~300 ms fade, auto corner-covering radius, additive multi-ripple, mandatory border-radius clip, GPU rasterisation, zero cost when idle) is implemented exactly.
- Spawning is latched by press identity (`(elem, down.time_ms)`), a single global slot — sound because there is at most one in-flight press (RFC-0003 E4). This is what makes a long hold spawn once even after its ink retires, while rapid taps each spawn.
- Blending is `PREMULTIPLIED_ALPHA_BLENDING` ("over"), not literal `One + One` addition: pure addition can only add light, which made dark ink invisible on light surfaces (caught during manual verification of the example's third card). RFC-0023 §1's "ripples blend additively" is honoured as *simultaneous ripples accumulate with each other* — n overlapping inks compose to `1 − (1−a)ⁿ` coverage — which is also what Material's real ink does. The readback test pins the dark-ink-on-light case.
