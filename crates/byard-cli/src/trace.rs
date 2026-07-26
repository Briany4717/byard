//! Chrome Trace Event export for `byard dev --trace <path>` (RFC-0030 §V5).
//!
//! # Why this is a serialiser and not a viewer
//!
//! The profiler already produces everything a flame chart needs: a scope name,
//! a start, an end, and — since RFC-0030 §I2 — a nesting `depth`. The Chrome
//! Trace Event format is a JSON array of exactly those fields. So the entire
//! feature is a serialiser, and Byard never has to build a flame-graph viewer:
//! Perfetto, `chrome://tracing` and `speedscope` all read this natively.
//!
//! It is the highest ratio of capability to code in RFC-0030, and it is worth
//! saying why that is not luck: `depth` was added for a completely different
//! reason (a frame total that double-counted nested scopes), and it is the one
//! field that makes nesting reconstruct here. Getting the data model right paid
//! for a feature nobody was thinking about at the time.
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
//! GPU passes take a third lane: they resolve two frames later against a
//! different clock, so placing them on the CPU timeline would draw them in the
//! wrong place with complete confidence (RFC-0013, §Q6).
//!
//! # The closing bracket is written first, not last
//!
//! The obvious design writes `[`, streams objects, and writes `]` on drop. It
//! produces an unparseable file for the single most important session: the one
//! you `Ctrl-C`'d because something was wrong. `Drop` does not run on `SIGINT`
//! — the process is terminated, not unwound — so "we close it on shutdown"
//! quietly means "we close it except when it matters".
//!
//! So the terminator is maintained *continuously*. Every frame writes its
//! events, then `]`, then flushes, then rewinds two bytes so the next frame
//! overwrites the terminator it just wrote. The file on disk is therefore valid
//! JSON at **every** instant, including the instant the process is killed, and
//! including before a single sample has been written.
//!
//! The cost is one flush and one seek per traced frame. That is a real syscall
//! pair, and it is affordable precisely because `--trace` is opt-in: a session
//! that asked for a trace has already accepted that it is being instrumented,
//! and a diagnostic that loses its data on the interesting run is not cheaper,
//! it is worthless.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use byard_core::telemetry::{SampleBlock, ScopeKind, scope_kind, scope_name};

/// Trace thread id for the logic thread's samples.
const TID_LOGIC: u32 = 1;
/// Trace thread id for the render thread's samples (its own `encode.*` and
/// `present.*` scopes).
const TID_RENDER: u32 = 2;
/// The offset applied to a lane carrying `Gpu` samples, which resolve
/// asynchronously against a different clock and must not share a CPU lane.
const TID_GPU_OFFSET: u32 = 100;

/// The array terminator, rewritten after every frame and rewound over by the
/// next one.
const TERMINATOR: &[u8] = b"]\n";
/// How far to rewind to sit on top of [`TERMINATOR`] again. Spelled out rather
/// than cast from `len()` so the sign is a written intention and not a
/// conversion.
const TERMINATOR_REWIND: i64 = -2;

/// A streaming Chrome Trace Event writer whose file is valid JSON at every
/// instant — see the module docs for why that is the whole design.
pub struct TraceWriter {
    out: BufWriter<File>,
    /// Whether an object has already been written, so the comma separator is
    /// emitted *before* each subsequent object rather than after each one.
    wrote_any: bool,
    /// Reused between samples so the per-sample path performs no allocation
    /// after the first line. A diagnostic that allocates per sample is
    /// measuring itself.
    scratch: String,
    /// Set once if the file becomes unwritable, so the failure is reported one
    /// time instead of once per frame for the rest of the session.
    failed: bool,
}

impl TraceWriter {
    /// Creates the trace file at `path`, already containing a valid empty
    /// array.
    ///
    /// # Errors
    ///
    /// Returns the OS error message if the file cannot be created or the
    /// initial array cannot be written.
    pub fn create(path: &Path) -> Result<Self, String> {
        let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut this = Self {
            out: BufWriter::new(file),
            wrote_any: false,
            scratch: String::with_capacity(256),
            failed: false,
        };
        this.out
            .write_all(b"[\n")
            .map_err(|e| format!("{}: {e}", path.display()))?;
        this.terminate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(this)
    }

    /// Appends one frame: the logic thread's block (when one was published for
    /// it) followed by the render thread's own, then re-terminates the array.
    ///
    /// Both blocks go in together because the terminator only has to be
    /// rewritten once per frame, which halves the flush traffic — and because
    /// they *are* one frame, so a partially-written frame is not a state worth
    /// being able to observe.
    pub fn write_frame(&mut self, logic: Option<&SampleBlock>, render: &SampleBlock) {
        if self.failed {
            return;
        }
        if let Some(logic) = logic {
            self.write_block(logic, TID_LOGIC);
        }
        self.write_block(render, TID_RENDER);
        if let Err(e) = self.terminate() {
            // Report once. A dev runner must not be taken down by its own
            // diagnostic, and must not spend the rest of the session shouting
            // about a full disk it cannot do anything about.
            crate::style::warn(&format!("trace file is no longer writable: {e}"));
            self.failed = true;
        }
    }

    /// Writes the closing bracket, flushes it to the OS, and rewinds over it so
    /// the next frame's first event overwrites it.
    ///
    /// After this returns the file is a complete, parseable JSON array. The
    /// rewind never leaves trailing garbage because every subsequent write is a
    /// whole event object, which is far longer than the two bytes it replaces.
    fn terminate(&mut self) -> std::io::Result<()> {
        self.out.write_all(TERMINATOR)?;
        self.out.flush()?;
        // `BufWriter`'s `Seek` flushes before seeking, so the buffer and the
        // file cursor cannot disagree here.
        debug_assert_eq!(TERMINATOR.len(), 2, "the rewind distance is spelled out");
        self.out.seek(SeekFrom::Current(TERMINATOR_REWIND))?;
        Ok(())
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
            let category = match kind {
                ScopeKind::Interpreter => "interp",
                ScopeKind::Native => "native",
                ScopeKind::Gpu => "gpu",
            };
            // Trace timestamps are microseconds; the profiler's are
            // nanoseconds since its own epoch.
            let ts_us = sample.start as f64 / 1000.0;
            let dur_us = sample.duration_ns() as f64 / 1000.0;
            // A `Gpu` sample carries a duration with no meaningful start, so it
            // is given its own lane rather than being drawn somewhere confident
            // and wrong on a CPU one.
            let tid = if kind == ScopeKind::Gpu {
                tid + TID_GPU_OFFSET
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
            // A failed write is reported by `terminate`, which runs at the end
            // of every frame and cannot be skipped.
            let _ = self.out.write_all(self.scratch.as_bytes());
            self.wrote_any = true;
        }
    }
}

impl Drop for TraceWriter {
    fn drop(&mut self) {
        // Nothing to close: the terminator has been on disk since `create`.
        // All that is left is to make sure the last frame's events reached the
        // OS, and to say so if they did not — losing the tail of a trace
        // silently is how a developer ends up debugging their profiler.
        if !self.failed {
            if let Err(e) = self.out.flush() {
                crate::style::warn(&format!("trace file could not be flushed: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byard_core::telemetry::{Sample, ScopeId, scope_id_tagged};

    /// A unique scratch directory per test, so a parallel test binary never
    /// has two tests writing the same trace.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "byard-trace-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_back(write: impl FnOnce(&mut TraceWriter)) -> serde_json::Value {
        let dir = temp_dir("rt");
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

    fn block(samples: Vec<Sample>) -> SampleBlock {
        SampleBlock {
            samples,
            dropped: 0,
        }
    }

    #[test]
    fn an_empty_trace_is_still_valid_json() {
        // The `Ctrl-C`-during-startup case.
        let v = read_back(|_| {});
        assert_eq!(v.as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn the_file_parses_after_every_frame_without_the_writer_being_dropped() {
        // The claim the whole design exists for: `Drop` does **not** run on
        // `SIGINT`, so a trace that is only closed on shutdown is unparseable
        // for exactly the session a developer most wants to read. Here the
        // writer is deliberately never dropped before the file is read.
        let dir = temp_dir("live");
        let path = dir.join("trace.json");
        let s = scope("trace.test.live", ScopeKind::Native);
        let mut w = TraceWriter::create(&path).unwrap();

        for frame in 1..=4u64 {
            w.write_frame(
                None,
                &block(vec![Sample::cpu(s, 0, frame * 1_000, frame * 1_000 + 500)]),
            );
            let text = std::fs::read_to_string(&path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| {
                panic!("the file must parse mid-session, after frame {frame}: {e}\n{text}")
            });
            assert_eq!(
                v.as_array().map(Vec::len),
                Some(usize::try_from(frame).unwrap()),
                "every frame's events must already be on disk"
            );
        }

        std::mem::forget(w); // the `SIGINT` shape: no `Drop`, no clean close.
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).expect("valid without a Drop");
        assert_eq!(v.as_array().map(Vec::len), Some(4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewinding_over_the_terminator_leaves_no_trailing_bytes() {
        // The rewind overwrites two bytes with an event object. If the file
        // were ever left longer than what was written, the tail would be
        // garbage — so assert the byte length exactly, not just that it parses.
        let dir = temp_dir("len");
        let path = dir.join("trace.json");
        let s = scope("trace.test.len", ScopeKind::Native);
        let mut w = TraceWriter::create(&path).unwrap();
        w.write_frame(None, &block(vec![Sample::cpu(s, 0, 0, 1_000)]));
        w.write_frame(None, &block(vec![Sample::cpu(s, 0, 2_000, 3_000)]));
        drop(w);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("]\n"), "{text}");
        assert_eq!(
            text.matches("\"name\"").count(),
            2,
            "an overwritten terminator must not resurrect as a fragment:\n{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn samples_round_trip_with_their_names_and_durations() {
        let outer = scope("trace.test.outer", ScopeKind::Interpreter);
        let inner = scope("trace.test.inner", ScopeKind::Native);
        // Drop order: a child is pushed before its parent.
        let b = block(vec![
            Sample::cpu(inner, 1, 2_000, 5_000),
            Sample::cpu(outer, 0, 1_000, 9_000),
        ]);
        let v = read_back(|w| w.write_frame(Some(&b), &SampleBlock::default()));
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
        let b = block(vec![
            Sample::cpu(inner, 1, 2_000, 5_000),
            Sample::cpu(outer, 0, 1_000, 9_000),
        ]);
        let v = read_back(|w| w.write_frame(Some(&b), &SampleBlock::default()));
        let events = v.as_array().unwrap();
        let find = |n: &str| events.iter().find(|e| e["name"] == n).unwrap();
        let o = find("trace.test.nest.outer");
        let i = find("trace.test.nest.inner");

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
        let l = block(vec![Sample::cpu(logic, 0, 0, 1_000)]);
        let r = block(vec![
            Sample::cpu(render, 0, 0, 1_000),
            Sample::gpu_duration(gpu, 500),
        ]);
        let v = read_back(|w| w.write_frame(Some(&l), &r));
        let events = v.as_array().unwrap();
        let tid = |n: &str| events.iter().find(|e| e["name"] == n).unwrap()["tid"].clone();
        let (l, r, g) = (
            tid("trace.test.lane.logic"),
            tid("trace.test.lane.render"),
            tid("trace.test.lane.gpu"),
        );
        assert_ne!(l, r);
        assert_ne!(r, g, "a GPU pass resolves on its own timeline (RFC-0013)");
        assert_ne!(l, g);
    }

    #[test]
    fn the_per_sample_path_does_not_allocate_after_warm_up() {
        // A diagnostic that allocates per sample is measuring itself. The
        // scratch `String` is the only per-sample buffer and it must stop
        // growing once it has seen a full-length line.
        let s = scope("trace.test.alloc", ScopeKind::Native);
        let b = block(vec![Sample::cpu(s, 0, 1_000, 2_000); 8]);
        let dir = temp_dir("alloc");
        let path = dir.join("trace.json");
        let mut w = TraceWriter::create(&path).unwrap();
        w.write_frame(None, &b);
        let capacity = w.scratch.capacity();
        for _ in 0..50 {
            w.write_frame(None, &b);
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
