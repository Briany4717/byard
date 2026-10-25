#!/usr/bin/env python3
"""Compare two runs of the `byard-core` benchmarks (RFC-0046 section 4).

The benchmarks print one line per measurement:

    <name>    123.45 ns/op   (1000 iters)

and `cargo bench` names the binary before its lines (`Running benches/atlas.rs`).
This reads that text, so it needs nothing the workspace does not have.

    bench-compare.py parse   < cargo-bench-output  > results.json
    bench-compare.py compare PERCENT --base A.json B.json ... --head C.json D.json ...

`compare` takes several runs of each side, from one machine in one job, and
prints the best of each for every benchmark. It writes a GitHub warning
annotation for a benchmark only when it is slower for sure: the change's best
run is more than PERCENT slower than the base's worst run. These benchmarks
are plain timed loops, and on a shared machine two runs of the very same code
differ by 30% or more, so a single comparison cries wolf and a gate that cries
wolf is ignored. It always exits 0: this is a warning until there is a runner
whose timings are stable (RFC-0046 Q2).
"""

import json
import re
import sys

LINE = re.compile(r"^(?P<name>\S.*?\S)\s+(?P<value>[0-9][0-9.]*)\s+(?P<unit>ns|µs|us|ms)/op\b")
RUNNING = re.compile(r"^\s*Running (?:benches/)?(?P<bench>[\w-]+?)(?:\.rs)?\s")
NANOSECONDS = {"ns": 1.0, "us": 1e3, "µs": 1e3, "ms": 1e6}


def parse(text):
    """The benchmark lines of `text`, as {"<bench>/<name>": nanoseconds per op}."""
    results = {}
    bench = "bench"
    for line in text.splitlines():
        running = RUNNING.match(line)
        if running:
            bench = running.group("bench")
            continue
        found = LINE.match(line)
        if not found:
            continue
        key = f"{bench}/{found.group('name')}"
        while key in results:
            key += "'"
        results[key] = float(found.group("value")) * NANOSECONDS[found.group("unit")]
    return results


def show(ns):
    if ns >= 1e6:
        return f"{ns / 1e6:.2f} ms"
    if ns >= 1e3:
        return f"{ns / 1e3:.2f} us"
    return f"{ns:.2f} ns"


def compare(base_runs, head_runs, percent):
    """Print both sides, and warn for each benchmark that is slower for sure.

    Each side is several runs. A benchmark is slower for sure when its best
    run on the head is more than `percent` slower than even the *worst* run on
    the base: every run of the change lost to every run of the base, by more
    than the budget. One slow run of a noisy machine cannot do that, and a real
    slowdown does.
    """
    base = {k: min(r[k] for r in base_runs if k in r) for r in base_runs for k in r}
    head = {k: min(r[k] for r in head_runs if k in r) for r in head_runs for k in r}
    width = max((len(k) for k in head), default=10)
    print(f"{'benchmark (best of ' + str(len(head_runs)) + ')':{width}}  {'base':>12}  {'head':>12}  {'change':>8}")
    slower = []
    for key, now in head.items():
        before = base.get(key)
        if before is None or before <= 0:
            print(f"{key:{width}}  {'-':>12}  {show(now):>12}  {'new':>8}")
            continue
        change = (now - before) / before * 100.0
        worst_base = max(r[key] for r in base_runs if key in r)
        sure = now > worst_base * (1 + percent / 100.0)
        if sure:
            mark = "  <-- slower in every run"
            slower.append((key, before, now, change))
        elif change > percent:
            mark = "  (over, but the runs overlap: noise)"
        else:
            mark = ""
        print(f"{key:{width}}  {show(before):>12}  {show(now):>12}  {change:>+7.1f}%{mark}")
    for key in base:
        if key not in head:
            print(f"{key:{width}}  {show(base[key]):>12}  {'-':>12}  {'gone':>8}")
    print(f"\nregression budget: {percent:g}% (ci/budgets.toml [bench])")
    for key, before, now, change in slower:
        print(
            f"::warning title=benchmark slower::{key} took {show(now)} an op against "
            f"{show(before)} on the base ({change:+.1f}%), over the {percent:g}% in "
            "ci/budgets.toml [bench], and slower in every run."
        )
    if not slower:
        print("no benchmark is slower than the budget in every run")


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def main(argv):
    if len(argv) == 2 and argv[1] == "parse":
        json.dump(parse(sys.stdin.read()), sys.stdout, indent=1)
        print()
        return 0
    # compare PERCENT --base A.json B.json ... --head C.json D.json ...
    if len(argv) >= 6 and argv[1] == "compare" and argv[3] == "--base" and "--head" in argv:
        split = argv.index("--head")
        compare([load(f) for f in argv[4:split]], [load(f) for f in argv[split + 1 :]], float(argv[2]))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
