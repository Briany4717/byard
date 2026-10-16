# Aura Weather

A weather app for one place at a time, built to find out whether Byard can
build an application rather than an example. Five screens over live data from
[open-meteo](https://open-meteo.com) (no API key), a city search, and a map.

```bash
cargo run -p aura-weather
```

Or draw one screen to a PNG without opening a window, after two seconds of
running so its data has arrived (`now`, `days`, `air`, `trends` or `map`):

```bash
cargo run -p aura-weather -- --shot map.png map
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

`byard.toml` holds the theme (two typefaces, a type scale, the dark palette),
the three hosts the screens ask (forecast, air quality, geocoding), and the
declaration of the map element.

The one part in Rust is the map (`src/map.rs`). It is an element of the
app's own with a shader of the app's own: it turns OpenStreetMap's daytime
tiles into the app's night colours and draws however many are on screen in
one batch. To `byld` it is `Map #[lat: …, lon: …, zoom: …]`.

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

## What it still could not be built with

Worked around in the app, and not yet fixed in Byard:

- **A JSON reply is not checked against its type.** A record type checks
  the code that reads a value; the geocoder's reply is assigned without
  being matched against `Place`, so a field it leaves out reads as nothing.
- **One kind of misspelling still checks clean**: a name passed to an
  intrinsic's named argument, where the argument may also be a word.
- **The Rust half has no hot reload.** `byard dev examples/aura_weather`
  reloads every screen but draws a plain panel where the map is; the map
  itself changes only when the app is rebuilt.
- **Not measured**: the frame time while panning the map.
