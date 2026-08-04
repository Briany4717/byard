## What does this PR do?

<!-- One paragraph. What changed, and why. -->

## Linked issue

Closes #

## Cheaper paths (INV-18)

**Does this add or modify a path that exists to be cheaper than an alternative?
If yes, which assertion fails when production stops taking it?**

<!--
Answer even when the answer is "no". This is here because the project shipped
three incremental paths that production never took: each was unit-tested, each
was benchmarked, and none of them had an assertion that failed when nobody
walked past. They stayed inert for several phases and nothing complained.

A benchmark proves a path is fast. Only an integration assertion proves anyone
takes it. If this PR adds one and cannot name such an assertion, that is the
thing to fix before merging, not after.

If a ceiling in `crates/byard-platform/tests/frame_budget.rs` is being
**raised**, say so here: the old value, the new value, and why the regression
is acceptable. Lowering one needs no explanation.
-->

## Checklist

- [ ] `cargo fmt --all` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] New public items have doc comments
- [ ] If this changes behavior visible to users, `CHANGELOG.md` has an entry under `[Unreleased]`
- [ ] If this changes the architecture, an RFC is linked or opened alongside this PR
- [ ] The cheaper-paths question above is answered

## Notes for reviewers

<!-- Anything that needs special attention, known trade-offs, or follow-up work. -->
