#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
//! What the language costs, phase by phase: lexing, parsing, checking,
//! formatting, the first frame of a program, and a frame after a write.
//!
//! Run with `cargo bench -p byard-compiler --bench language`.
//!
//! The program measured is written here, not read from an example, so a
//! change to an example does not move these numbers: `views` views, each with
//! the things a real one has (state, derived values, a `match` over an enum,
//! records, a list pipeline, a map lookup, text with interpolations, a `when`
//! and a `for`). The same program at two sizes says whether a phase grows
//! with the program as it should, linearly, or worse.
//!
//! Each line is `<name>  <time> ns/op`, the format `ci/bench-compare.py`
//! reads, so CI compares these against the base of a pull request as it does
//! the engine's.

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use byard_compiler::edition::Edition;
use byard_compiler::interp::env::Value;
use byard_compiler::interp::eval::Interpreter;
use byard_compiler::resolve::{PackageProvider, ResolvedProgram, SourceFile, resolve_program_in};
use byard_compiler::symbol::Symbol;
use byard_core::frame::RenderFrame;

/// A program with `views` views, all drawn by `Main`.
fn program(views: usize) -> String {
    let mut s = String::from(
        r#"enum Status { open, done, late }

type Item = { id: Int, name: Str, qty: Int, status: Status }

fn label(s: Status) -> Str => match s {
    .open => "open"
    .done => "done"
    .late => "late"
}

fn weight(item: Item) -> Int => item.status == Status.late ? item.qty * 2 : item.qty
"#,
    );
    for v in 0..views {
        let _ = write!(
            s,
            r#"
View Panel{v}(title: Str, start: Int = {v}) {{
    var count = start
    var open = true
    let items: List<Item> = [
        {{ id: 1, name: "a{v}", qty: 3, status: Status.open }},
        {{ id: 2, name: "b{v}", qty: 5, status: Status.late }},
        {{ id: 3, name: "c{v}", qty: 8, status: Status.done }},
    ]
    let prices: Map<Str, Int> = ["a{v}": 10, "b{v}": 20, "c{v}": 30]
    let total = items.map(i => weight(i)).reduce(0, (a, n) => a + n)
    let late = items.filter(i => i.status == Status.late).len
    Column #[gap: 4, p: 8] {{
        Text("{{title}}: {{count}} of {{total}}, {{late}} late") #[size: 14]
        when open {{
            for item in items {{
                Row #[gap: 2] {{
                    Text("{{item.name}} {{label(item.status)}} {{prices[item.name] ?? 0}}")
                    Text("{{item.qty * count}}")
                }}
            }}
        }} else {{
            Text("closed")
        }}
        Button("more") => {{ count++ }}
    }}
}}
"#
        );
    }
    s.push_str("\nView Main() {\n    Column {\n");
    for v in 0..views {
        let _ = writeln!(s, "        Panel{v}(\"panel {v}\")");
    }
    s.push_str("    }\n}\n");
    s
}

/// A program whose every frame follows a write: `n` is set, and a list of
/// `items` items is mapped, filtered and summed again from the count, with a
/// map looked up once an item, and the result shown, as a text and as the
/// width of a box (which is what lets the run see that it did recompute).
fn busy(items: usize) -> String {
    format!(
        r#"View Main() {{
    var n = 0
    let xs: List<Int> = (0..{items}).map(i => i)
    let names: Map<Int, Str> = [0: "zero", 1: "one", 2: "two", 3: "three"]
    let total = xs.map(x => x + n).filter(x => x % 3 == 0).reduce(0, (a, x) => a + x)
    let named = xs.filter(x => x < 4).map(x => names[x] ?? "?").len
    Column {{
        Text("{{n}} {{total}} {{named}}")
        Box #[width: total % 97 + named, height: 10, bg: 0x6750A4]
    }}
}}
"#
    )
}

struct NoPackages;

impl PackageProvider for NoPackages {
    fn package_files(&mut self, _: &str, _: &str) -> Result<Vec<SourceFile>, String> {
        Err("a benchmark has no packages".to_string())
    }
}

fn check(src: &str) -> ResolvedProgram {
    resolve_program_in(
        vec![SourceFile {
            name: "main.byd".to_string(),
            source: src.to_string(),
        }],
        &mut NoPackages,
        Edition::BASELINE,
    )
}

/// Runs `f` until about `budget` has gone by (at least 5 times, after 2 to
/// warm up) and prints the mean time a run took.
fn bench(name: &str, budget: Duration, mut f: impl FnMut()) {
    for _ in 0..2 {
        f();
    }
    let start = Instant::now();
    let mut runs = 0_u64;
    while runs < 5 || start.elapsed() < budget {
        f();
        runs += 1;
    }
    let ns = start.elapsed().as_nanos() as f64 / runs as f64;
    println!("{name:50} {ns:>14.2} ns/op   ({runs} iters)");
}

/// The interpreter with `program`'s views loaded, its `Main` lowered and its
/// first frame drawn, ready for another.
struct Running {
    interp: Interpreter,
    tree: Vec<byard_compiler::interp::eval::RenderNode>,
    frame: RenderFrame,
    now: u32,
}

impl Running {
    fn start(program: &ResolvedProgram) -> Self {
        let known: Vec<&str> = program.views.iter().map(|v| v.name.as_str()).collect();
        let main = program
            .views
            .iter()
            .find(|v| v.name.as_str() == "Main")
            .expect("a Main");
        let mut interp = Interpreter::new();
        interp.load_views(&program.views);
        let tree = interp.lower_view(main, &known);
        let mut running = Self {
            interp,
            tree,
            frame: RenderFrame::new(),
            now: 0,
        };
        running.frame();
        running
    }

    fn frame(&mut self) {
        self.now += 16;
        self.interp.set_now_ms(self.now);
        self.interp.tick();
        self.frame.clear();
        self.interp
            .render(&self.tree, &mut self.frame, 1280.0, 800.0);
        black_box(self.frame.instances().len());
    }
}

fn main() {
    let budget = Duration::from_millis(500);
    for views in [20, 200] {
        let src = program(views);
        let checked = check(&src);
        assert!(
            checked.errors.is_empty(),
            "the benchmark's program does not check: {:?}",
            checked.errors
        );
        bench(&format!("lex, {views} views"), budget, || {
            black_box(byard_compiler::lexer::lex(black_box(&src)));
        });
        bench(&format!("parse, {views} views"), budget, || {
            black_box(byard_compiler::parser::parse(black_box(&src)));
        });
        bench(&format!("check, {views} views"), budget, || {
            black_box(check(black_box(&src)));
        });
        bench(&format!("format, {views} views"), budget, || {
            black_box(byard_compiler::fmt::format(black_box(&src)).expect("formats"));
        });
        bench(&format!("first frame, {views} views"), budget, || {
            black_box(Running::start(&checked).frame.instances().len());
        });
        let mut running = Running::start(&checked);
        bench(
            &format!("a frame with nothing changed, {views} views"),
            budget,
            || {
                running.frame();
            },
        );
    }
    for items in [100, 1000] {
        let checked = check(&busy(items));
        assert!(
            checked.errors.is_empty(),
            "the benchmark's program does not check: {:?}",
            checked.errors
        );
        let mut running = Running::start(&checked);
        let n = running
            .interp
            .var_signal(&Symbol::intern("n"))
            .expect("the var `n`");
        let before = format!("{:?}", running.frame.texts());
        let mut k = 0_i64;
        bench(
            &format!("a frame after a write, {items} items"),
            budget,
            || {
                k += 1;
                running.interp.write_var(n, Value::Int(k));
                running.frame();
            },
        );
        // The number means nothing unless every frame recomputed.
        assert_ne!(before, format!("{:?}", running.frame.texts()));
    }
}
