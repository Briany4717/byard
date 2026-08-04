#!/usr/bin/env bash
# RFC-0023 §2 blur/vibrancy — feature 2 handoff, step 1 of 2.
# Run from the repo root, on `feat/rfc-0023-ripple`. Stops on first failure.
set -euo pipefail

# ── Gates ────────────────────────────────────────────────────────────────────
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# ── Stacked branch (base: feat/rfc-0023-ripple) ─────────────────────────────
git checkout -b feat/rfc-0023-blur

# ── Commit 1: engine — barrier segmentation + blur/composite pipelines ──────
git add \
  crates/byard-core/src/frame.rs \
  crates/byard-core/src/encoder/mod.rs \
  crates/byard-core/src/encoder/backdrop.rs \
  crates/byard-core/src/encoder/backdrop.wgsl \
  crates/byard-core/src/encoder/blur.wgsl \
  crates/byard-core/src/engine.rs \
  crates/byard-platform/tests/backdrop_readback.rs
git commit -m "feat(render): add the RFC-0023 backdrop blur pipeline (barrier-segmented passes)"

# ── Commit 2: compiler — props, emission, overlap diagnostic ────────────────
git add \
  crates/byard-compiler/src/interp/intrinsics.rs \
  crates/byard-compiler/src/interp/eval.rs \
  crates/byld-lsp/src/main.rs
git commit -m "feat(compiler): lower RFC-0023 blur/vibrancy props and the overlap diagnostic"

# ── Commit 3: example, check guard, dev-runner surfacing, changelog ─────────
git add \
  crates/byard-cli/examples/frosted_glass \
  crates/byard-cli/tests/frosted_glass_example.rs \
  crates/byard-cli/src/commands/dev.rs \
  CHANGELOG.md
git commit -m "feat(cli): add the RFC-0023 frosted-glass example and perf-warning surfacing"

git push -u origin feat/rfc-0023-blur

# ── Issue ────────────────────────────────────────────────────────────────────
gh issue create \
  --title "Backdrop blur & vibrancy (RFC-0023 §2): the iOS frosted-glass effect" \
  --body-file .handoff/blur_issue.md \
  --label enhancement

# ── Paste the output of these two lines back to me ──────────────────────────
git log --oneline -3
gh issue list --limit 1
