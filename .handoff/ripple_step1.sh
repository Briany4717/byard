#!/usr/bin/env bash
# RFC-0023 ripple — feature 1 handoff, step 1 of 2.
# Run from the repo root. Stops on the first failure.
set -euo pipefail

# ── Gates ────────────────────────────────────────────────────────────────────
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# ── Branch ───────────────────────────────────────────────────────────────────
git checkout -b feat/rfc-0023-ripple

# ── Commit 1: engine — the Ripple pipeline ──────────────────────────────────
git add \
  crates/byard-core/src/frame.rs \
  crates/byard-core/src/encoder/mod.rs \
  crates/byard-core/src/encoder/ripple.rs \
  crates/byard-core/src/encoder/ripple.wgsl \
  crates/byard-platform/tests/ripple_readback.rs
git commit -m "feat(render): add the RFC-0023 Ripple pipeline (additive ink reveal)"

# ── Commit 2: compiler — props, press gesture, spawning/emission ────────────
git add \
  crates/byard-compiler/src/interp/intrinsics.rs \
  crates/byard-compiler/src/interp/events.rs \
  crates/byard-compiler/src/interp/eval.rs \
  crates/byld-lsp/src/main.rs
git commit -m "feat(compiler): spawn and emit RFC-0023 ripples from press gestures"

# ── Commit 3: example, check guard, changelog ───────────────────────────────
git add \
  crates/byard-cli/examples/ripple \
  crates/byard-cli/tests/ripple_example.rs \
  CHANGELOG.md
git commit -m "feat(cli): add the RFC-0023 ripple example and check guard"

git push -u origin feat/rfc-0023-ripple

# ── Issue ────────────────────────────────────────────────────────────────────
gh issue create \
  --title "Ripple paint effect (RFC-0023): the Material ink reveal as style properties" \
  --body-file .handoff/ripple_issue.md \
  --label enhancement

# ── Paste the output of these two lines back to me ──────────────────────────
git log --oneline -3
gh issue list --limit 1
