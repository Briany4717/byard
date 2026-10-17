# Aura Weather

A weather app for one place at a time, built to find out whether Byard can
build an application rather than an example. Five screens over live data from
[open-meteo](https://open-meteo.com) (no API key), a city search, and a map.

```bash
cargo run -p aura-weather
```

Or draw one screen to a PNG without opening a window, after six seconds of
running so its data has arrived (`now`, `days`, `air`, `trends` or `map`):

```bash
cargo run -p aura-weather -- --shot map.png map
```

Or follow the project while editing: every screen and the manifest apply to
the open window, map included.

```bash
cargo run -p aura-weather -- --dev
```

Or package it to run on a Mac that has neither its sources nor Byard: an
`Aura Weather.app` with its icon, under `examples/aura_weather/.byard/bundle/`.

```bash
cargo build --release -p aura-weather && cargo run --release -p byard-cli -- bundle examples/aura_weather --bin target/release/aura-weather
```

## What it is made of

Every screen is `byld`, in `src/`:

| File | Screen |
|---|---|
| `main.byd` | the shell: search field, tab host, tab bar |
| `forecast.byd` | Now: the temperature, how it feels, the next twelve hours |
| `outlook.byd` | 10 days: one row a day |
| `air.byd` | Air: the air-quality gauge |
| `trends.byd` | Trends: the week's highs as a filled curve, the lows beneath |
| `map.byd` | Map: where the place is |

The place chosen in the search is kept with `Store`, so the app opens on it
next time.

`byard.toml` holds the theme (two typefaces, a type scale, the dark palette;
the light one is derived, and the app follows the system between them),
the three hosts the screens ask (forecast, air quality, geocoding), and the
declaration of the map element.

The one part in Rust is the map (`src/map.rs`). It is an element of the
app's own with a shader of the app's own: it draws OpenStreetMap's daytime
tiles as they are on a light scheme and turns them into the app's night
colours on a dark one, however many are on screen in one batch. To `byld`
it is `Map #[lat: …, lon: …, zoom: …, night: t.dark]`.

The tests (`tests/it`) drive the real program through the interpreter, with
every request answered by a server on the machine, tiles included.

## What it needed that Byard did not have

Each of these was found by building a screen, and each is now in Byard.

| Found on | Missing | Now |
|---|---|---|
| Trends | A `path` skipped `for` and `when` in silence, so a curve from data drew nothing. | `for` and `when` inside a path body (#298) |
| Search | A screen could not load again when the place it was given changed. | `on change(lat, lon) => load()` (#300) |
| Search | `grow` on a text field did nothing; it stayed 200 wide. | `grow` on `TextField` and `Slider` (#301) |
| Search | In a reply's arm, a function's parameter read its argument again, so an old answer passed for the current one. | Arms see the arguments the call was placed with (#302) |
| Search | A text field ran none of the events written on it (`input`, `submit`, `focus`, …). | They run (#304) |
| Search | A text field ignored its `color:`, its padding and its `typo:` size. | It reads them (#305) |
| Search | Nothing made typed text safe in a URL; a `&` cut the query short. | `text.urlEncoded()` (#306) |
| Search | A misspelt name checked clean and drew its own spelling. | `byard check` reports undeclared names (#307) |
| Map | An app could not register a pipeline; a view could not be handed pixels; an app with a Rust half could not be drawn to a PNG. | `render::Shader` with layers, `HostValue::Bytes`, `App::shot` (#308) |
| Map | `byard check` and the editor did not know an element drawn in Rust. | `[native_views]` in `byard.toml` (#309) |
| Map | A native view heard a press before the zoom buttons drawn over it. | What is drawn on top gets the press first (#313) |
| Search | A missing JSON field read as nothing, and every read of one needed a test around it. | `a ?? b`: `res.json.results ?? []` (#312) |
| Map | `zoom: "near"` checked clean for a number, and so did `bg: prmary` and `serach(q)`. | `byard check` reads a value property's bare name as a name, checks called names, and rejects text for a number (#314) |
| Search | A named action's parameters could not say their types, so its body was checked against nothing. | `(asked: Str) => …` |
| Search | A JSON list of records had no type, so `hit.nmae` checked clean. | `type Place = { name: Str, … }` |
| Map | An app with a Rust half could not hot-reload: `byard dev` cannot link the map, so it drew a panel there. | `App::hot_reload(true)`, `--dev` |
| 10 days | Dates had no words: month names were a hand-written table and a weekday could not be had at all. | `iso.formatDate("ddd D")` |
| Search | A reply was assigned to `List<Place>` without being compared with it, so a field the server left out read as nothing. | A value written into a record-typed `var` is compared, and the first difference reported; `country: Str?` may be absent |
| Every screen | A theme was light or dark, never the system's: the app ignored the appearance the user had chosen. | `scheme = "system"` (#323) |
| Every screen | A translucent tint of a theme colour had to be its own hex, which stayed dark on a light scheme. | `t.primary.alpha(0.6)` (#324) |
| Shell | The window had the default icon. | `[project] icon` (#325) |
| Shipping | The app read its project from the path it was compiled at, so it ran on no other machine, and there was no way to package it. | `byard bundle --bin …`: a `.app` (or a directory) with the project and its packages beside the binary, which an app looks for when its build path is gone; `[bundle]` names it |
| Trends | A misspelt name inside a gradient or a shadow (`to: blak`) checked clean. | A record value's named fields are checked as values |

## What it still could not be built with

Worked around in the app, and not yet fixed in Byard:

- **A misspelt JSON field reads as nothing.** `??` gives a missing field a
  default, but `res.json.resluts` is a missing field too, and nothing says so.
- **The Rust half itself does not reload.** `cargo run -p aura-weather --
  --dev` follows every `.byd` and the manifest with the map linked in, but
  an edit to `map.rs` still needs the app rebuilt.
- **Measured only on the logic side**: panning over tiles the map has costs
  about 0.04 ms of tick and render for the whole app in a release build
  (0.4 ms in debug), and asks for and writes nothing
  (`panning_over_tiles_it_has_writes_and_asks_nothing`). The GPU side of a
  panning frame is not measured.
