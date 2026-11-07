#!/usr/bin/env python3
"""Builds ByardIcons.ttf, a tiny variable icon font for Byard's tests and the
`icon_font` sample. It is not a design: it exists so the engine's icon-font
paths (ligatures, variation axes, subsetting) can be tested without shipping a
15 MB font. Run it with fontTools installed:

    python3 -m venv v && v/bin/pip install fonttools
    v/bin/python make_icon_font.py ByardIcons.ttf

Eight icons, each a ring that fills in:
  * `FILL` 0..1 (default 0): the ring's hole closes as FILL goes to 1;
  * `wght` 100..700 (default 400): the outline grows with the weight;
  * `GRAD` and `opsz` exist as axes with no effect on the outline, as in the
    fonts this stands in for, so a program naming all four works.
Each icon is named by a ligature: the letters of `favorite` shape the heart.
Licence: the font and this script are MIT OR Apache-2.0, like Byard.
"""
import math
import sys

from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables._g_v_a_r import TupleVariation

UPM = 1000
CENTER = (500, 400)


def ngon(n, r, rot=-90):
    return [(CENTER[0] + r * math.cos(math.radians(rot + 360 * i / n)),
             CENTER[1] + r * math.sin(math.radians(rot + 360 * i / n))) for i in range(n)]


def star():
    pts = []
    for i in range(10):
        r = 340 if i % 2 == 0 else 150
        a = math.radians(-90 + 36 * i)
        pts.append((CENTER[0] + r * math.cos(a), CENTER[1] - r * math.sin(a)))
    return pts


def heart():
    pts = []
    for i in range(16):
        t = 2 * math.pi * i / 16
        x = 16 * math.sin(t) ** 3
        y = 13 * math.cos(t) - 5 * math.cos(2 * t) - 2 * math.cos(3 * t) - math.cos(4 * t)
        pts.append((CENTER[0] + x * 20, CENTER[1] + y * 20))
    return pts


def rect(w, h):
    return [(CENTER[0] - w, CENTER[1] - h), (CENTER[0] + w, CENTER[1] - h),
            (CENTER[0] + w, CENTER[1] + h), (CENTER[0] - w, CENTER[1] + h)]


ICONS = {
    "favorite": heart(),
    "star": star(),
    "home": [(500, 780), (140, 440), (240, 440), (240, 40), (760, 40), (760, 440), (860, 440)],
    "search": ngon(12, 330),
    "settings": ngon(8, 340, 22.5),
    "mail": rect(360, 260),
    "menu": rect(300, 300),
    "close": ngon(4, 360, 0),
}
LETTERS = sorted({c for name in ICONS for c in name})


def scaled(pts, s):
    return [(CENTER[0] + s * (x - CENTER[0]), CENTER[1] + s * (y - CENTER[1])) for x, y in pts]


def contours(pts):
    """Outer contour (clockwise in font space) and the hole (counter-clockwise)."""
    outer = scaled(pts, 0.9)
    inner = list(reversed(scaled(pts, 0.6)))
    return outer, inner


def draw(pts_list):
    pen = TTGlyphPen(None)
    for pts in pts_list:
        pen.moveTo((round(pts[0][0]), round(pts[0][1])))
        for x, y in pts[1:]:
            pen.lineTo((round(x), round(y)))
        pen.closePath()
    return pen.glyph()


def main(out):
    names = [".notdef", "space"] + [f"letter_{c}" if c != "_" else "underscore" for c in LETTERS] + list(ICONS)
    glyphs, metrics, variations = {}, {}, {}
    glyphs[".notdef"] = draw([rect(300, 300)])
    metrics[".notdef"] = (UPM, 0)
    glyphs["space"] = TTGlyphPen(None).glyph()
    metrics["space"] = (400, 0)
    letter = draw([rect(120, 120)])
    cmap = {32: "space"}
    for c in LETTERS:
        g = f"letter_{c}" if c != "_" else "underscore"
        glyphs[g] = letter
        metrics[g] = (500, 0)
        cmap[ord(c)] = g
    for name, pts in ICONS.items():
        outer, inner = contours(pts)
        glyphs[name] = draw([outer, inner])
        metrics[name] = (UPM, 0)
        # Deltas in contour order: outer points then the hole's points.
        # Closing the hole: from scale 0.6 to 0 moves a point by -(p - c).
        fill = [(0, 0)] * len(outer) + [(-round(x - CENTER[0]), -round(y - CENTER[1])) for x, y in inner]
        up = [(round((x - CENTER[0]) * 0.1 / 0.9), round((y - CENTER[1]) * 0.1 / 0.9)) for x, y in outer]
        down = [(-dx, -dy) for dx, dy in up]
        variations[name] = [
            TupleVariation({"FILL": (0.0, 1.0, 1.0)}, fill + [(0, 0)] * 4),
            TupleVariation({"wght": (0.0, 1.0, 1.0)}, up + [(0, 0)] * (len(inner) + 4)),
            TupleVariation({"wght": (-1.0, -1.0, 0.0)}, down + [(0, 0)] * (len(inner) + 4)),
        ]
    fb = FontBuilder(UPM, isTTF=True)
    fb.setupGlyphOrder(names)
    fb.setupCharacterMap(cmap)
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics(metrics)
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "Byard Icons", "styleName": "Regular", "psName": "ByardIcons-Regular"})
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200)
    fb.setupPost()
    fb.setupFvar([("FILL", 0, 0, 1, "Fill"), ("GRAD", -25, 0, 200, "Grade"),
                  ("opsz", 20, 24, 48, "Optical size"), ("wght", 100, 400, 700, "Weight")], [])
    fb.setupGvar(variations)
    rules = "\n".join(
        f"    sub {' '.join(('underscore' if c == '_' else 'letter_' + c) for c in name)} by {name};"
        for name in sorted(ICONS, key=len, reverse=True))
    addOpenTypeFeaturesFromString(fb.font, f"languagesystem DFLT dflt;\nfeature liga {{\n{rules}\n}} liga;\n")
    fb.save(out)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "ByardIcons.ttf")
