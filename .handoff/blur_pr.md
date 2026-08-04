## What does this PR do?

Implements the backdrop-blur/vibrancy slice of RFC-0023 (§2) — the iOS frosted-glass effect — completing the RFC on top of the ripple slice (#136). Four paint-time style properties (`blur`, `backdrop_tint`, `blur_saturation`, `blur_quality`) lower to a `frame::BackdropInstance` emitted right after the element's background (the §4 compositing slot). A backdrop is a **barrier**: `push_backdrop` records a pool-cursor snapshot of everything emitted behind it, and a new pure `compute_segments` partitions the draw stream (RFC-0017 z-layers × backdrop barriers) into one render pass per segment — colour `Load`ed and depth carried (`Store`/`Load`) across splits, so occlusion spans the whole frame and a frame with no effects renders through the exact previous single-pass stream. Between segments, `encoder::backdrop::prepare` copies the pane's region (+ blur halo) out of the persistent target and blurs it into per-slot cached scratch textures — a two-pass 21-tap separable Gaussian (`blur` is σ, the CSS `backdrop-filter: blur(N)` convention; the first pass doubles as the downsample, and the downsample deepens adaptively so tap spacing never gaps — the anti-ghosting guarantee) — and the resumed pass composites it via `backdrop.wgsl`: UV derived from the fragment's own framebuffer position (exact under transforms), rounded-rect clip, saturation boost, tint blend. `blur`/`backdrop_tint` animate through the existing RFC-0010 `with` chokepoints; a tint without blur lowers to a plain translucent fill (zero GPU cost); ≥ 3 overlapping panes raise `PerfWarning::OverlappingBlurs`, printed once by `byard dev`.

## Linked issue

Closes #138

## New surface area

- `frame::BackdropInstance` + `RenderFrame::{push_backdrop, backdrops, backdrop_marks, backdrop_clips}` + `LayerMark::backdrop` + `BLUR_QUALITY_{AUTO,LOW,HIGH}` / `BLUR_MAX_RADIUS` — the pool and its barrier snapshots cross subsystems only through `frame.rs` (RFC-0001 §9); the snapshot reuses `LayerMark` (it is exactly "a cursor into every pool").
- `encoder::backdrop` (`build_pipelines`, `prepare`, `draw_composite`, `BackdropPipelines`, `BlurScratch`/`ScratchCache`, `PreparedBackdrop`) + `blur.wgsl` + `backdrop.wgsl` — the off-screen blur passes and the in-pass composite; scratch textures cached per slot, recreated only on region-size change.
- `EncoderSubsystem::set_blur_auto_capable` — the engine's startup GPU probe for the `auto` tier (0.5× base resolution on real GPUs, 0.25× on `Cpu`/`VirtualGpu`; the kernel is always the Gaussian — tiers differ in resolution only); bare encoders default to the deterministic 0.25× tier.
- Compiler: `EFFECTS` group grows `blur`/`backdrop_tint`/`blur_saturation`/`blur_quality` (closed token set, validated + hinted); `Interpreter::{emit_backdrop, perf_warnings}` and `PerfWarning` (`OverlappingBlurs`); byld-lsp hover docs extended.
- Internal (not pub): `compute_segments`/`SegmentRanges` — the pass-segmentation CPU mirror; text glyph batches now partition per segment (glyphon ranges generalise unchanged).
- `lexer::COLOR_HAS_ALPHA_TAG` + `intrinsics::{color_has_alpha, color_rgba_auto}` — >6-digit hex literals are tagged at lex time (RFC-0005 §1 reserves that width for `0xAARRGGBB`), which is what makes an explicitly-written zero alpha (`0x00FFFFFF`) distinguishable from opaque `0xFFFFFF`; one shared auto-detect for every alpha-aware colour consumer.
- Two engine fixes surfaced by manual verification of the example (details in Notes): hit rects now follow scroll (`scroll_shift` through the render walk + `scrolled_hit_rect`), and colour `with` animations carry a fourth alpha `Motion`.

## Tasks

- [x] `BackdropInstance` + barrier snapshots + pool/clip/layer plumbing in `frame.rs`
- [x] Pass segmentation (`compute_segments`) + multi-pass `draw_ui_pass` with depth carried across splits
- [x] Region copy (transformed on-screen AABB + 2.5σ halo) + separable Gaussian passes + composite pipeline (framebuffer-position UV, rounded clip, saturation, tint)
- [x] Quality tiers by base resolution (high 0.75× / low 0.25× / GPU-probed auto via `set_blur_auto_capable`) + adaptive σ-capped downsample
- [x] Catalog props + validation + LSP docs; tint-only fast path; `blur`/`backdrop_tint` through the RFC-0010 animation chokepoints
- [x] `PerfWarning::OverlappingBlurs` (conservative overlap clustering) surfaced by `byard dev`
- [x] Unit tests (compiler + core + segmentation) and GPU readback tests (platform)
- [x] Runnable example `crates/byard-cli/examples/frosted_glass` + `byard check` guard
- [x] CHANGELOG entry

## Acceptance criteria

- [x] The four props resolve into the emitted pane (rect, radii, blur, tint alpha, saturation, quality) — `blur_props_emit_a_backdrop_with_the_resolved_fields`.
- [x] `blur` clamps to 40 and `blur_saturation` defaults to 1.8 — `blur_clamps_to_the_max_radius_and_defaults_saturation`.
- [x] A tint without blur emits no barrier and lowers to a translucent fill — `a_tint_without_blur_lowers_to_a_plain_translucent_fill`.
- [x] The barrier snapshot captures exactly the content behind the pane, and the pane's depth sits between that content and its children — `the_backdrop_barrier_snapshots_the_content_behind_it`, `push_backdrop_stamps_depth_clip_and_records_the_behind_cursor`.
- [x] `blur` animates through the RFC-0010 ramp — `blur_animates_as_a_paint_prop`.
- [x] An animated colour now ramps its **alpha byte** alongside the OKLab channels, so a translucent tint fades instead of popping — `an_animated_color_ramps_its_alpha_channel_too`.
- [x] Hit targets follow the scroll offset (tappable at the on-screen position, inert at the stale laid-out one, clipped to the viewport) — `hit_targets_follow_the_scroll_offset`.
- [x] A state override retargets the base's `with` animation instead of popping (`blur: 4 with anim` + `on hover { blur: 16 }` ramps) — `a_state_override_retargets_the_base_with_animation`.
- [x] Segmentation: layers alone reproduce the old partition byte-for-byte; barriers split within their layer; malformed cursors clamp — `no_marks_is_one_full_range`, `marks_split_the_pool_into_contiguous_layers`, `malformed_marks_clamp_instead_of_panicking`, `a_backdrop_barrier_splits_the_single_layer_into_two_segments`, `two_backdrops_in_one_layer_stack_in_painter_order`, `a_backdrop_inside_an_overlay_layer_splits_only_that_layer`, `a_malformed_backdrop_cursor_clamps_into_its_layer`.
- [x] On a real GPU the pane softens a hard edge behind it while the same edge stays crisp outside the pane — readback `the_pane_blurs_the_edge_behind_it_and_only_there`.
- [x] Tint lightens the blurred sample, the rounded corner clips the glass, children stay crisp above it — readback `tint_corner_clip_and_children_compose_over_the_glass`.
- [x] ≥ 3 overlapping panes raise the diagnostic, 2 do not — `three_stacked_glass_panes_raise_the_overlap_warning`, `deepest_rect_overlap_counts_the_tallest_stack`.
- [x] Props validate with the closed `blur_quality` token set — `blur_props_are_accepted_and_quality_is_a_closed_token_set`.
- [x] The committed example checks clean through the real binary — `frosted_glass_example_checks_clean`.
- [x] Manual verification: `cd crates/byard-cli/examples/frosted_glass && cargo run -p byard-cli -- dev` — live re-blur under the nav bar while scrolling, corner clipping, spring-animated hover glass, low-vs-high tier comparison, tint-only wash, crisp labels (steps documented in the example header).

## Checklist

- [x] `cargo fmt --all` passes
- [x] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [x] `cargo test --workspace` passes
- [x] New public items have doc comments
- [x] `CHANGELOG.md` updated under `[Unreleased]` (Added: backdrop blur & vibrancy, RFC-0023 §2)
- [x] Consistent with RFC-0001: pools and barrier snapshots cross subsystems only through `frame.rs` (§9); every pipeline create sequence sits in a validation error scope (§8); paint-time only, layout untouched (INV-8); the no-effects frame renders through the unchanged single-pass stream (no regression to §3.3 incremental drawing).

## Notes for reviewers

- `4f046f7` (feat(render)): the engine half — barrier marks, pass segmentation, blur/composite pipelines, the `auto`-tier probe, GPU readbacks.
- `cbb9142` (feat(compiler)): the language half — the four props, `emit_backdrop` (with the tint-only lowering), the overlap diagnostic.
- `e0b334d` (feat(cli)): the proof — the frosted-glass example, its check guard, `byard dev` printing perf warnings, changelog.
- `eb0133e` (docs(changelog)): aligns the changelog's blur entry with the landed σ/tier model (the committed text predated the kernel redesign).
- The composite UV comes from `@builtin(position)` mapped into the copied region, not from rect-derived UVs: "what is behind this pixel" is by definition the same physical pixel of the colour target, so sampling stays exact under paint transforms, and the bilinear upscale from the downsampled scratch is the smoothing the RFC's resolution answer relies on.
- Depth across pass splits: the draw-order depth buffer is `Store`d at every split and `Load`ed by the next segment (`Discard` only after the last), so cross-segment occlusion behaves exactly as the single pass did. The GPU-timed scope brackets the first segment; per-segment timing is the existing per-pipeline-timing follow-up.
- The RFC sketches the low tier as "single-pass, 3-tap"; as landed it is a single-pass 3×3 box (9 taps) — one pass as specified, and a visibly boxy-but-stable result instead of a directional artefact. The Gaussian tier is the RFC's two-pass separable pair.
- Windows CI (WARP, the DX12 software rasteriser) zeroes exactly one discarded corner of the split-pass composite — an alpha-0 write its own blend state cannot produce, while the three symmetric corners (identical shader path) are correct, and lavapipe/Metal/real hardware all render it right. The corner-clip *sub-assertion* is skipped on WARP only (diagnostic dump retained); the shader hardenings that came out of chasing it stay (spec-ordered `smoothstep` edges — descending edges are UB — and NaN-safe discards). Follow-up: verify on real DX12 hardware.
- `wgpu` cannot distinguish "high-end mobile" from other integrated adapters, so the `auto` probe reads conservatively: Gaussian on `DiscreteGpu` only; `blur_quality: high` forces it anywhere.
- Three engine fixes landed here because manual verification of the frosted-glass example surfaced them:
  1. **Hit rects now follow scroll** (pre-existing RFC-0005 bug): the scroll translate rides the paint transform, and RFC-0011/INV-8 deliberately keeps paint transforms out of hit-testing, so every hit rect inside a scrolled subtree stayed at its laid-out position. A separate `scroll_shift` accumulator threads through the render walk; `scrolled_hit_rect` shifts every registered target to its on-screen position and clips it to the scroll viewport (invisible ⇒ untappable). Ripple tap points map back through the same shift; the backdrop copy region now uses the pane's *transformed* AABB for the same reason.
  2. **Colour animations dropped the alpha byte** (`eval_animated_color` interpolated only OKLab L/a/b), and no magnitude heuristic can rescue it: `0x00FFFFFF` and `0xFFFFFF` are the same `i64`. Root fix: the lexer tags >6-digit hex literals (`COLOR_HAS_ALPHA_TAG`, bit 32 — channel extraction truncates to `u32`, so it never reaches a colour), a fourth alpha `Motion` rides the OKLab trio, and the animated result is tagged too so a mid-ramp zero alpha survives. `bg`/`color`/`border` behaviour is unchanged (the existing OKLab test still passes).
  3. **The blur kernel was redesigned against ghosting and timidity**: sparse taps at high resolution read as overlaid copies ("double vision"), Apple Silicon reports `IntegratedGpu` (so the auto tier was falling to the 3×3 box — three ghosts by construction), and the initial σ = r/3 mapping was visually weak. Final model: `blur` **is** the Gaussian σ (the CSS `backdrop-filter: blur(N)` convention the RFC cites), one 21-tap separable kernel spanning ±2.5σ at σ/4 spacing, an adaptive downsample capping σ at 8 destination texels (resolution is traded, never tap coverage — the composite's bilinear upscale hides it, the RFC's own rationale), and quality tiers that differ **only in base resolution** (high 0.75×, auto 0.5× on real GPUs / 0.25× on `Cpu`/`VirtualGpu`, low 0.25×; `set_blur_auto_capable`). A single-pass Kawase kernel was evaluated for the low tier and rejected: one pass cannot cover a real radius — that technique earns its quality from iteration, so dual-Kawase *chains* remain the noted follow-up for extreme radii.
  4. **State overrides now retarget `with` animations instead of popping** (engine-wide, RFC-0010 × RFC-0012/0016): `resolve_state_attrs` replaced whole attrs, so `on hover { blur: 16 }` silently discarded the base's `blur: 0 with anim.spring()` shell — no state-driven style ever animated. `override_state_attr` keeps the base's curve and Motion key (the state changes the target; the base owns the curve), so entering/leaving a state drives one interruptible animation. Its regression test then exposed a second, older fight: the build-phase layout resolver (`eval_container_style`, which runs against the *base* attrs) evaluated **every** attribute through `eval_pure` before matching, dragging the same Motion back toward the base target each frame — a retarget ping-pong converging short of the goal. It now evaluates only the layout props it consumes (which are never animatable), so paint props are evaluated exactly once per frame, by the paint pass, against the state-resolved attrs.
