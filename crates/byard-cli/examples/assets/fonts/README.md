# Example font assets

Two variable typefaces, shared by every example that declares `[assets.fonts]`
(and by the font-family tests, which resolve these paths from the workspace
root). Both carry a `wght` axis, so they exercise the RFC-0034 weight axis and
the family selector through the same file.

| File | Family | Axis | License |
|---|---|---|---|
| `SpaceGrotesk-Variable.ttf` | Space Grotesk | `wght 300..700` | SIL OFL 1.1, `OFL-SpaceGrotesk.txt` |
| `Manrope-Variable.ttf` | Manrope | `wght 200..800` | SIL OFL 1.1, `OFL-Manrope.txt` |

They are deliberately unalike: Space Grotesk is a geometric display face and
Manrope a humanist UI face, so "the two families render differently" is a claim
about the engine rather than about two cuts of the same design.
