#!/usr/bin/env python3
"""Write the seed corpus of the fuzz targets: one file a program.

    fuzz-seeds.py DIR

The seeds are the programs of the conformance suite
(crates/byard-cli/tests/conformance/*.byd), each as the harness runs it (a
`//# fragment` case inside `View Main() { ... }`), and every `byld` sample of
the guide and of the error index. They cover the whole language, which is what
the fuzzer then changes a byte at a time. Nothing is kept in the repository:
the nightly job writes them fresh and adds what its cached corpus already holds.
"""

import glob
import hashlib
import os
import re
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")


def cases():
    for path in sorted(glob.glob(os.path.join(ROOT, "crates/byard-cli/tests/conformance/*.byd"))):
        text = open(path, encoding="utf-8").read()
        for chunk in re.split(r"\n(?=//# case:)", text):
            if not chunk.startswith("//# case:"):
                continue
            lines = chunk.split("\n")
            headers = [l for l in lines if l.startswith("//#")]
            body = "\n".join(l for l in lines if not l.startswith("//#")).strip() + "\n"
            if any(h.strip() == "//# fragment" for h in headers):
                body = "View Main() {\n" + body + "}\n"
            yield body


def samples():
    paths = glob.glob(os.path.join(ROOT, "docs/guide/src/*.md"))
    index = os.path.join(ROOT, "crates/byard-compiler/errors.md")
    if os.path.exists(index):
        paths.append(index)
    for path in sorted(paths):
        text = open(path, encoding="utf-8").read()
        for m in re.finditer(r"```byld([^\n]*)\n(.*?)```", text, re.S):
            tags, body = m.group(1), m.group(2)
            if "fragment" in tags:
                body = "View Main() {\n" + body + "}\n"
            yield body


def main(argv):
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    out = argv[1]
    os.makedirs(out, exist_ok=True)
    written = 0
    for program in list(cases()) + list(samples()):
        name = hashlib.sha1(program.encode("utf-8")).hexdigest()
        with open(os.path.join(out, name), "w", encoding="utf-8") as f:
            f.write(program)
        written += 1
    print(f"{written} seeds in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
