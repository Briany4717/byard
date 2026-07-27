# Erratum to RFC-0020: a `Canvas` body admits `for` and `when`

- **Status:** Active erratum (amends, does not replace, RFC-0020)
- **Author(s):** Briany4717
- **Created:** 2026-07-27
- **Applies to:** RFC-0020 §1 ("the `Canvas` intrinsic"), specifically the rule
  that a canvas body is *"shape commands only"*.
- **Authority:** RFC-0030 §V3, which requires the dev HUD to be written in
  `byld` **with no privileged syntax** — *"if a construct the HUD needs does not
  exist, that is a gap in the language and should be filled there, not worked
  around here."*

---

## Why this erratum exists

RFC-0020 §1 restricts a `Canvas` body to shape commands, and rejects everything
else — including `for` and `when` — with `UnknownShapeCommand`. The restriction
is right about declarations and style blocks: there is nothing for a `var` or a
`style { … }` inside a canvas to mean, and silently ignoring one is how a
developer spends an afternoon on a shape that was never going to appear.

It is wrong about control flow, and the way it is wrong is easy to miss because
nothing fails. It makes the **shape count** a compile-time constant. A canvas
can animate every coordinate, every colour and every sweep from live data, and
cannot draw *n* bars for *n* data points. That excludes charts, sparklines,
histograms, waveforms and legends — which is to say, most of what a drawing
surface exists for.

The workaround is available and is exactly the tell: write twenty-four
`rect(…)` lines against twenty-four separately named fields. That is not a use
of the language, it is a substitute for a feature it does not have.

RFC-0030's in-window HUD is what surfaced it. The HUD's sparkline is
`CanvasShape` geometry rather than text on purpose — a paint-class change never
touches layout, which is what keeps the HUD from re-shaping text every frame and
defeating the invalidation model it exists to report on (INV-24). Written
against RFC-0020 as specified, that sparkline was inexpressible.

---

## Correction

A `Canvas` body admits `for` and `when`, nested to any depth, alongside shape
commands. Their bodies are validated exactly as a top-level body is, so a bad
shape inside a loop is still a bad shape and a loop is not an escape hatch.
Everything RFC-0020 §1 rejects today other than these two stays rejected.

```byld
Canvas #[width: 240, height: 40] {
    for b in bars {
        rect(x: b.x, y: b.y, w: 3, h: b.h, fill: b.over ? 0xF2B8B5 : 0xD0BCFF)
    }
    when showBaseline {
        line(x1: 0, y1: 39, x2: 240, y2: 39, stroke: 0x49454F)
    }
}
```

## They expand at emit time, not at lowering

This is the part that is a design decision rather than a mechanical extension.

Everywhere else in the language, `for` and `when` lower into reactive pools
(`ForPool` / `WhenPool`). That is correct there and it is not free: those bodies
are *elements*, with layout, identity and mountable state, and re-deriving them
per frame would throw all three away — a text field would lose its cursor every
time its sibling list changed.

A canvas body has none of those. Shape commands carry no layout, no identity and
no state, and the render walk **already** re-evaluates every one of their
parameter expressions every tick — that is precisely what makes `sweep: percent
* 3.6` reactive with no plumbing at all. Expanding the loop in the same walk is
therefore the *consistent* choice, not the cheap one: it makes the shape count as
reactive as the coordinates already were, and it introduces no render node, no
pool, and no per-frame allocation beyond the binding it pushes and pops.

The practical consequence is that a canvas's data can change shape frame to
frame with no reconciliation and no cost, which is exactly what a sparkline over
a rolling window needs.

## What this does not change

- Shape commands are still the only *leaves* a canvas body may contain.
- `var`, `let`, `fn`, `inject`, `style` and `route`/`tab` are still rejected
  inside a canvas, with the same diagnostic and the same reasoning.
- Individual shapes are still not hit-testable (RFC-0020's resolved question);
  events belong to the canvas rect.
- Tier-2 tessellated paths and clip masks remain deferred.
