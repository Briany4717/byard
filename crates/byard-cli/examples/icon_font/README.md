# icon_font

RFC-0054: `variations:` on text, and an icon font that `byard bundle` cuts down to the icons
the program uses.

```sh
cargo run -p byard-cli -- shot crates/byard-cli/examples/icon_font -o icons.png
cargo run -p byard-cli -- shot crates/byard-cli/examples/icon_font --tap 48,470 -o liked.png
```

`fonts/ByardIcons.ttf` is a 3 KB variable font with eight shapes, made by
`../assets/fonts/make_icon_font.py`. It has the four axes of Material Symbols (`FILL`, `GRAD`,
`opsz`, `wght`), so the program below runs unchanged on the real one.

## With Material Symbols

Material Symbols Rounded is Apache-2.0 and 15 MB, so it is not in this repository. Download it
from the [material-design-icons](https://github.com/google/material-design-icons) repository (the
variable font, `MaterialSymbolsRounded[FILL,GRAD,opsz,wght].ttf`), and point the manifest at it:

```toml
[assets.fonts]
symbols = { path = "fonts/MaterialSymbolsRounded.ttf", icons = true }
```

Then the same `shot`, and a bundle:

```sh
cargo build --release
cargo run -p byard-cli -- bundle crates/byard-cli/examples/icon_font --bin target/release/byard-cli
```

The bundle prints the font before and after. With this program (five names) 15 235 612 bytes
become 16 596, and `shot` of the bundled project draws the same picture byte for byte.
