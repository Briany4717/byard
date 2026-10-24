# Theming

```byld
View Main() {
    inject Theme as t

    Column #[bg: t.surface, p: 32, gap: 20, width: 460] {
        Text("Theme runtime") #[typo: t.headlineLarge, color: t.onSurface]
        Text("Colour, type and shape come from the theme, so there are no hex literals here.")
            #[typo: t.bodyLarge, color: t.onSurfaceVariant]

        Column #[bg: t.primaryContainer, radius: t.cornerXl, p: 20, gap: 8] {
            Text("Primary container") #[typo: t.titleLarge, color: t.onPrimaryContainer]
            Box #[bg: t.primary, radius: t.cornerMd, p: (10, 16)] {
                Text("Primary") #[typo: t.labelLarge, color: t.onPrimary]
            }
            Box #[bg: t.primary.alpha(0.16), radius: t.cornerMd, p: (6, 12)] {
                Text("Primary at 16%") #[typo: t.labelLarge, color: t.onPrimaryContainer]
            }
        }

        Row #[gap: 12, align: center] {
            Toggle #[bind: t.dark, bg: t.primary, width: 52, height: 30]
            Text("Scheme: {t.mode}") #[typo: t.bodyLarge, color: t.onSurfaceVariant]
        }
    }
}
```

A theme is the app's colours, type scale, corner shapes and fonts, written once in
`byard.toml` and read from views by name. Change the theme and every screen follows.
Change the scheme, light to dark, and the colours cross-fade.

## Reading the theme

`inject Theme as t` gives the view a handle on it. Then `t.name` is a token:

| Kind | Examples | Used as |
|------|----------|---------|
| Colour | `t.primary`, `t.onSurface`, `t.surfaceContainer` | `bg:`, `color:`, `border:` |
| Type | `t.headlineLarge`, `t.bodyMedium` | `typo:` |
| Shape | `t.cornerMd`, `t.cornerXl` | `radius:` |
| Scheme | `t.dark`, `t.mode` | `t.dark` is a `Bool` you can write, and `t.mode` is its name as text |

Tokens are spelt in `snake_case` in `byard.toml` and in `camelCase` in byld:
`primary_container` is `t.primaryContainer`. A token that the theme does not have is
an error, and it names the three places it looked:

```byld,fails
View Main() {
    inject Theme as t
    Text("x") #[color: t.primry]
}
```

```diagnostic
error[UnknownThemeToken]: theme `byard-base` declares no token `primry`
```

`color.alpha(0.16)` is a colour at an opacity, so a translucent tint follows the
theme and the scheme instead of being a hex of its own. The opacity is held between
0 and 1.

Writing `t.dark` flips the scheme: `Toggle #[bind: t.dark]` is a dark mode switch, and
so is `Button("flip") => t.dark = !t.dark`.

## The built-in theme

With no `[theme]` at all you get `byard-base`, which is close to Material 3. Colours:
`primary`, `onPrimary`, `primaryContainer`, `onPrimaryContainer`, `secondary`,
`onSecondary`, `secondaryContainer`, `onSecondaryContainer`, `surface`,
`surfaceContainerLow`, `surfaceContainer`, `surfaceContainerHigh`, `onSurface`,
`onSurfaceVariant`, `outline`, `outlineVariant`, `background`, `onBackground`, `error`
and `onError`, each in a light and a dark scheme.

The type scale is `displayLarge`, `displayMedium`, `displaySmall`, `headlineLarge`,
`headlineMedium`, `headlineSmall`, `titleLarge`, `titleMedium`, `titleSmall`,
`bodyLarge`, `bodyMedium`, `bodySmall`, `labelLarge`, `labelMedium` and `labelSmall`,
plus the short names `display`, `headline`, `title`, `body`, `caption` and `label`.

The shapes are `cornerNone` (0), `cornerXs` (4), `cornerSm` (8), `cornerMd` (12),
`cornerLg` (16), `cornerXl` (28) and `cornerFull`.

## Declaring your own: `byard.toml`

Everything is under `[theme]`. Every table is optional and every mistake is an error
that names the token, because a theme token that was silently dropped is a visual
regression nobody can find.

```toml
[theme]
name = "demo"
extends = "byard-base"
scheme = "system"
transition = 300

[theme.color.light]
primary = "#6750A4"
on_primary = "#FFFFFF"
surface = "#FEF7FF"
on_surface = "#1D1B20"

[theme.color.dark]
primary = "#D0BCFF"
on_primary = "#381E72"
surface = "#141218"
on_surface = "#E6E0E9"

[theme.typography]
headline_large = { size = 32, weight = "regular" }
body_large = { size = 16, weight = "regular", tracking = 0.5 }

[theme.shape]
corner_md = 12
corner_xl = 28

[theme.breakpoints]
md = 640
lg = 1024
```

- **`name`** is the theme's name, shown in messages.
- **`extends`** is `"byard-base"`, or the name of a dependency whose theme this one
  builds on. See [packages](packages.md).
- **`scheme`** is the scheme the app starts in: `"light"`, `"dark"`, or `"system"`.
  `"system"` follows the operating system's appearance, now and when it changes.
- **`transition`** is how many milliseconds a scheme flip takes to cross-fade every
  colour token, in a colour space where the middle of the fade does not turn grey.
  Without it the flip is a cut.
- **`[theme.color.light]` and `[theme.color.dark]`** hold colours as `#RRGGBB` or
  `#RGB`. A token you leave out keeps the built-in value, and a dark scheme falls back
  to the light one.
- **`[theme.typography]`** declares text styles: `{ size, family, weight, tracking,
  line_height }`. Only `size` is required. `weight` is `thin`, `light`, `regular`,
  `medium`, `semibold`, `bold` or `black`, and `family` names one of your fonts.
- **`[theme.shape]`** declares corner radii.
- **`[theme.breakpoints]`** names widths for [responsive styles](styling.md#responsive-styles).

A token you declare is used by name. With `hero = { size = 44, weight = "bold" }` and
`label = { size = 12 }` added under `[theme.typography]`, this works:

```byld,ignore
// Uses tokens declared in byard.toml, which a sample cannot see.
View Main() {
    Text("Aura") #[typo: hero]
    Text("a quiet line") #[typo: label]
}
```

A `typo` is written bare, as above, or through the theme as `t.hero`. A screen says
which *kind* of text it shows, and the theme says how that looks.

## One colour is enough: seeds

You do not have to write a palette. Name one colour and Byard derives every role of
both schemes from it, with the tones chosen so text stays readable on whatever it
sits on:

```toml
[theme]
name = "seeded"
seed = "#1E8E3E"
transition = 300
```

That gives `primary`, `secondary`, `tertiary`, each with its `on` colour and container,
the `surface` family, `outline`, `error` and so on, light and dark, from one hex. Change
the seed, save, and the whole app moves to the new hue under `byard dev`.

A colour you declare yourself wins over the derived one, so you can take the seed's
palette and pin a single role:

```toml
[theme]
seed = "#1E8E3E"

[theme.color.light]
primary = "#0B57D0"
```

The seed can also be a picture:

```toml
[theme]
seed = { image = "brand.png" }
```

The palette comes from the picture's dominant colour: the most common colour, weighted by
how colourful it is, so a small logo on a large grey background still gives the brand.
The result is the same on every machine. A missing or unreadable image is an error in
`byard.toml` that names it, and the file is watched, so replacing it updates the
running app.

The derived palette is related to Material's, but not equal to it. A seed run through
Google's tool and through Byard gives similar colours, not identical hexes.

## Fonts

Declare each typeface with a name and a file, and use it by name:

```toml
[assets.fonts]
display = "assets/fonts/SpaceGrotesk-Variable.ttf"
body = "assets/fonts/Manrope-Variable.ttf"

[theme.typography]
hero = { size = 44, family = "display", weight = "bold" }
label = { size = 12, family = "body", weight = "medium" }
```

```byld,ignore
// Uses the families declared in byard.toml above.
View Main() {
    Column #[gap: 8] {
        Text("Handgloves") #[font: display, size: 34, weight: 300]
        Text("A paragraph in the body face") #[typo: label]
    }
}
```

The name on the left, `display`, is what `font:` and a token's `family` use. The face's
own family name is read from the file, so neither has to be repeated. Fonts are loaded
once and registered for layout and drawing alike, so the text you measure is the text
you draw. A weight is a number from 100 to 900, and a variable font follows it exactly.

Naming a family you did not declare is an error, and so is a font file that cannot be
read:

```byld,fails
View Main() {
    Text("x") #[font: display]
}
```

```diagnostic
error[UnknownFontFamily]: no font family `display` is declared
```

A token can name a family, which is the point of a type scale: a screen says `typo:
hero` and gets the face, the size and the weight together.

## The window's icon

`icon = "assets/icon.png"` in `[project]` sets the window's icon in the title bar and
taskbar on Windows and Linux. macOS takes its icon from the app bundle. See
[shipping](shipping.md). A path that is not there is a manifest error.

## Themes in packages

A design system can ship as a package: its palette, type scale, shapes and fonts in
one dependency. An app then says `extends = "brand"` and gets all of it. See
[packages](packages.md).
