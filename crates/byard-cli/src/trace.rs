//! Chrome Trace Event export for `byard dev --trace <path>` (RFC-0030 §V5).
//!
//! # Why this is thirty lines and not a viewer
//!
//! The profiler already produces everything a flame chart needs: a scope name,
//! a start, an end, and — since RFC-0030 §I2 — a nesting `depth`. The Chrome
//! Trace Event format is a JSON array of exactly those fields. So the entire
//! feature is a serialiser, and Byard never has to build a flame-graph viewer:
//! Perfetto, `chrome://tracing` and `speedscope` all read this natively.
//!
//! It is the highest ratio of capability to code in RFC-0030, and it is worth
//! saying why that is not luck: `depth` was added in Phase 8 for a completely
//! different reason (a frame total that double-counted nested scopes), and it
//! is the one field that makes nesting reconstruct here. Getting the data model
//! right paid for a feature nobody was thinking about at the time.
//!
//! # Nesting, without emitting any structure
//!
//! `ph: "X"` is a *complete* event: a start and a duration. The viewer nests
//! events purely by containment on one `tid` — B contains C when B's
//! `[ts, ts+dur)` contains C's. Our samples already satisfy that, because a
//! child scope's guard is created after its parent's and dropped before it. So
//! no tree is serialised and none has to be reconstructed; `depth` is carried
//! as an argument for a reader's benefit, not for the viewer's.
//!
//! # Threads
//!
//! The logic and render threads have separate sample rings and are emitted as
//! separate `tid`s, which is what puts `interp.render` and `encode.frame` on
//! two lanes in the viewer rather than falsely nesting one inside the other.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use byard_core::telemetry::{SampleBlock, ScopeKind, scope_kind, scope_name};

/// Trace thread id for the logic thread's samples.
const TID_LOGIC: u32 = 1;
/// Trace thread id for the render thread's samples (its own `encode.*` scopes
/// and the asynchronously-resolved `gpu.*` passes).
const TID_RENDER: u32 = 2;

/// A streaming Chrome Trace Event writer.
///
/// Opens the file, writes the opening bracket, appends one object per sample
/// as blocks arrive, and closes the array on drop. Nothing is buffered beyond
/// the `BufWriter`, so a long session does not grow memory and a session that
/// is `Ctrl-C`'d still leaves a file every viewer will open.
pub struct TraceWriter {
    out: BufWriter<File>,
    /// Whether an object has already been written, so the comma separator can
    /// be emitted *before* each subsequent object rather than after each one —
    /// which is what keeps the array valid if the process dies mid-session.
    wrote_any: bool,
    /// Reused between samples so the per-sample path performs no allocation
    /// after the first few (§P1's spirit: a diagnostic that allocates per
    /// sample is measuring itself).
    scratch: String,
}

impl TraceWriter {
    /// Creates the trace file at `path`.
    ///
    /// # Errors
    ///
    /// Returns the OS error message if the file cannot be created.
    pub fn create(path: &Path) -> Result<Self, String> {
        let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut out = BufWriter::new(file);
        out.write_all(b"[\n")
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            out,
            wrote_any: false,
            scratch: String::with_capacity(256),
        })
    }

    /// Appends one tick's worth of logic-thread samples.
    pub fn write_logic(&mut self, block: &SampleBlock) {
        self.write_block(block, TID_LOGIC);
    }

    /// Appends one redraw's worth of render-thread samples.
    pub fn write_render(&mut self, block: &SampleBlock) {
        self.write_block(block, TID_RENDER);
    }

    // The profiler's timestamps are nanoseconds since its own epoch and a
    // session would have to run for 52 days before a `u64`→`f64` conversion
    // lost a microsecond of resolution — which is the unit the trace format
    // uses anyway.
    #[allow(clippy::cast_precision_loss)]
    fn write_block(&mut self, block: &SampleBlock, tid: u32) {
        for sample in &block.samples {
            let name = scope_name(sample.scope).unwrap_or("<unknown scope>");
            let kind = scope_kind(sample.scope).unwrap_or(ScopeKind::Native);
            // A `Gpu` sample carries a duration with no meaningful start (it
            // resolves asynchronously two frames later — RFC-0013), so placing
            // it on the CPU timeline would draw it in the wrong place. It is
            // emitted on its own lane at its own origin, categorised so a
            // reader can tell the two timelines apart rather than being
            // silently misled by one.
            let category = match kind {
                ScopeKind::Interpreter => "interp",
                ScopeKind::Native => "native",
                ScopeKind::Gpu => "gpu",
            };
            // Trace timestamps are microseconds; the profiler's are
            // nanoseconds since its own epoch.
            let ts_us = sample.start as f64 / 1000.0;
            let dur_us = sample.duration_ns() as f64 / 1000.0;
            let tid = if kind == ScopeKind::Gpu {
                tid + 100
            } else {
                tid
            };

            self.scratch.clear();
            let _ = writeln!(
                self.scratch,
                "{}{{\"name\":\"{name}\",\"cat\":\"{category}\",\"ph\":\"X\",\
                 \"ts\":{ts_us:.3},\"dur\":{dur_us:.3},\"pid\":1,\"tid\":{tid},\
                 \"args\":{{\"depth\":{}}}}}",
                if self.wrote_any { "," } else { "" },
                sample.depth()
            );
            // A trace file is a diagnostic; a failed write must not take the
            // dev runner down with it. The `BufWriter`'s own errors surface on
            // flush at drop, where they are reported once.
            let _ = self.out.write_all(self.scratch.as_bytes());
            self.wrote_any = true;
        }
    }
}

impl Drop for TraceWriter {
    fn drop(&mut self) {
        // Closing the array on drop is what makes a `Ctrl-C`'d session produce
        // a file that still parses. The alternative — closing it only on a
        // clean shutdown — means the trace you most want (the one from the
        // session that went wrong) is the one that is truncated.
        let _ = self.out.write_all(b"]\n");
        if let Err(e) = self.out.flush() {
            crate::style::warn(&format!("trace file could not be flushed: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byard_core::telemetry::{Sample, ScopeId, scope_id_tagged};

    fn read_back(write: impl FnOnce(&mut TraceWriter)) -> serde_json::Value {
        let dir = std::env::temp_dir().join(format!(
            "byard-trace-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.json");
        {
            let mut w = TraceWriter::create(&path).unwrap();
            write(&mut w);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("trace is not valid JSON: {e}\n{text}"))
    }

    fn scope(name: &'static str, kind: ScopeKind) -> ScopeId {
        scope_id_tagged(name, kind)
    }

    #[test]
    fn an_empty_trace_is_still_valid_json() {
        // The `Ctrl-C`-on-startup case, and the one a naive "write the closing
        // bracket at the end of a clean shutdown" design gets wrong.
        let v = read_back(|_| {});
        assert_eq!(v.as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn samples_round_trip_with_their_names_and_durations() {
        let outer = scope("trace.test.outer", ScopeKind::Interpreter);
        let inner = scope("trace.test.inner", ScopeKind::Native);
        let block = SampleBlock {
            // Drop order: a child is pushed before its parent.
            samples: vec![
                Sample::cpu(inner, 1, 2_000, 5_000),
                Sample::cpu(outer, 0, 1_000, 9_000),
            ],
            dropped: 0,
        };
        let v = read_back(|w| w.write_logic(&block));
        let events = v.as_array().unwrap();
        assert_eq!(events.len(), 2);

        let by_name = |n: &str| {
            events
                .iter()
                .find(|e| e["name"] == n)
                .unwrap_or_else(|| panic!("missing {n}"))
        };
        let o = by_name("trace.test.outer");
        let i = by_name("trace.test.inner");
        assert_eq!(o["ph"], "X");
        assert_eq!(o["cat"], "interp");
        assert_eq!(i["cat"], "native");
        assert!((o["ts"].as_f64().unwrap() - 1.0).abs() < 1e-6);
        assert!((o["dur"].as_f64().unwrap() - 8.0).abs() < 1e-6);
        assert_eq!(o["args"]["depth"], 0);
        assert_eq!(i["args"]["depth"], 1);
    }

    #[test]
    fn nesting_round_trips_as_containment_on_one_thread() {
        // This is the whole correctness claim of the format choice: the viewer
        // nests by containment on a `tid`, so the parent's window must contain
        // the child's and both must share a lane.
        let outer = scope("trace.test.nest.outer", ScopeKind::Native);
        let inner = scope("trace.test.nest.inner", ScopeKind::Native);
        let block = SampleBlock {
            samples: vec![
                Sample::cpu(inner, 1, 2_000, 5_000),
                Sample::cpu(outer, 0, 1_000, 9_000),
            ],
            dropped: 0,
        };
        let v = read_back(|w| w.write_logic(&block));
        let events = v.as_array().unwrap();
        let o = events
            .iter()
            .find(|e| e["name"] == "trace.test.nest.outer")
            .unwrap();
        let i = events
            .iter()
            .find(|e| e["name"] == "trace.test.nest.inner")
            .unwrap();

        assert_eq!(o["tid"], i["tid"], "nesting only happens within one lane");
        let (ots, odur) = (o["ts"].as_f64().unwrap(), o["dur"].as_f64().unwrap());
        let (its, idur) = (i["ts"].as_f64().unwrap(), i["dur"].as_f64().unwrap());
        assert!(
            ots <= its && its + idur <= ots + odur,
            "the parent's window must contain the child's: {ots}+{odur} vs {its}+{idur}"
        );
    }

    #[test]
    fn the_two_threads_and_the_gpu_get_separate_lanes() {
        // `interp.render` and `encode.frame` are unrelated scopes on different
        // threads; sharing a lane would make the viewer draw one inside the
        // other purely because their timestamps happen to overlap.
        let logic = scope("trace.test.lane.logic", ScopeKind::Interpreter);
        let render = scope("trace.test.lane.render", ScopeKind::Native);
        let gpu = scope("trace.test.lane.gpu", ScopeKind::Gpu);
        let v = read_back(|w| {
            w.write_logic(&SampleBlock {
                samples: vec![Sample::cpu(logic, 0, 0, 1_000)],
                dropped: 0,
            });
            w.write_render(&SampleBlock {
                samples: vec![
                    Sample::cpu(render, 0, 0, 1_000),
                    Sample::gpu_duration(gpu, 500),
                ],
                dropped: 0,
            });
        });
        let events = v.as_array().unwrap();
        let tid = |n: &str| events.iter().find(|e| e["name"] == n).unwrap()["tid"].clone();
        let (l, r, g) = (
            tid("trace.test.lane.logic"),
            tid("trace.test.lane.render"),
            tid("trace.test.lane.gpu"),
        );
        assert_ne!(l, r);
        assert_ne!(r, g, "a GPU pass resolves on its own timeline (RFC-0013)");
    }

    #[test]
    fn the_per_sample_path_does_not_allocate_after_warm_up() {
        // A diagnostic that allocates per sample is measuring itself. The
        // scratch `String` is the only per-sample buffer and it must stop
        // growing once it has seen a full-length line.
        let s = scope("trace.test.alloc", ScopeKind::Native);
        let block = SampleBlock {
            samples: vec![Sample::cpu(s, 0, 1_000, 2_000); 8],
            dropped: 0,
        };
        let dir = std::env::temp_dir().join(format!("byard-trace-alloc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.json");
        let mut w = TraceWriter::create(&path).unwrap();
        w.write_logic(&block);
        let capacity = w.scratch.capacity();
        for _ in 0..50 {
            w.write_logic(&block);
        }
        assert_eq!(
            w.scratch.capacity(),
            capacity,
            "the scratch buffer reallocated on the per-sample path"
        );
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
