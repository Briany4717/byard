### What problem does this solve?

Every Material interactive surface shows an ink ripple on tap. Today `byard-material` can only approximate it with `on pressed { bg: darkerColor }` — a flat colour swap. The difference is viscerally obvious: flat swaps feel dead, ripples feel responsive. Material Design's identity is built on ink physics; without it any Material package looks like a wireframe.

The effect cannot be faked at the `byld` level: it needs the tap coordinates (pointer-down position), a time-driven expansion/fade, and compositing *between* an element's background and its children — all render-pipeline concerns. RFC-0023 specifies this as the first of the paint-time visual effects.

### Proposed solution

Implement the ripple slice of RFC-0023 (its blur/vibrancy slice is tracked separately):

- Four paint-time style properties on the box render path (usable in `style {}` blocks and `on <state>` blocks): `ripple: Color` (enables the effect, sets the ink colour), `ripple_active: Bool` (the trigger — typically `on pressed { ripple_active: true }`), `ripple_radius: Float` (max-radius override; auto = distance from the tap point to the farthest corner), `ripple_duration: Int` (fade-out ms, default 300).
- A press spawns one ink circle at the exact tap point; it expands with an ease-out ramp, fades linearly, and is always clipped to the element's border radius (RFC-0023 resolved question — no opt-out). The ink alpha-composites over the surface (light ink brightens dark surfaces, dark ink darkens light ones); rapid taps spawn one ripple each, pooling where they overlap, while a hold spawns exactly once.
- Compositing order background → ripple → children via the emission-order draw depth (no new render pass): the label stays crisp above the ink.
- A dedicated `Ripple` render pipeline (the seventh) with analytic circle + rounded-rect SDF in the fragment shader; instances carry the current sampled radius/fade, following the RFC-0010 animation model as landed (CPU samples the closed forms while active, GPU rasterises).
- Live ripples join the active-animation set (frames keep flowing until the ink fades) and the incremental dirty-scissor union.

### Alternatives you have considered

- A `Ripple { … }` wrapper intrinsic — rejected by RFC-0023 itself (wrapper-hell; effects are properties of an element, not elements).
- A CPU-animated circle overlay node — would mutate the render tree every frame; rejected in favour of the pipeline approach (RFC-0023 §rationale).
- Reusing the `CanvasShape` circle pipeline — it has no rounded-rect clip and its instances aren't shaped like ink (per-corner clip radii, snapshot colour, sampled fade); a dedicated pipeline keeps both shaders single-purpose.

### Which area does this affect?

Render subsystem (wgpu pipelines), Logic subsystem (signals / arenas), byld (DSL syntax / compiler)
