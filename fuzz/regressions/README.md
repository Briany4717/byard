# Regressions

An input a fuzz target once panicked on, kept after the fix: one file an
input, named `<target>-<what it was>` (`run-range-of-a-billion`). The
workspace's tests run every file here through every target on each pull
request (`crates/byard-cli/tests/it/fuzz_targets.rs`), so a fix cannot come
undone. The nightly job uploads what it finds as an artifact; reduce it with
`cargo +nightly fuzz tmin <target> <file>` before adding it here.
