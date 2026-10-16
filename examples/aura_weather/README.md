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

## What it still could not be built with

Worked around in the app, and not yet fixed in Byard:

- **A native view hears a press before the elements drawn over it.** The
  map would starve its own zoom buttons if it claimed presses, so it claims
  only drags.
- **A missing JSON field is silently nothing.** The geocoder leaves `results`
  out when it finds nothing; the app has to test `res.json.results.len > 0`,
  and a typo in a field name would read as nothing just the same.
- **A JSON list of records has no type.** `var found: List<Place>` names a
  type nothing defines and nothing checks.
- **Some misspellings still check clean**: a name that is a property's whole
  value (`bg: prmary`), an intrinsic's named argument, a function's name.
- **A native view's property type is not checked** at check time:
  `zoom: "near"` passes for a float.
- **A lambda's parameters take no type**: `(asked) => …`, never
  `(asked: Str) => …`.
- **The Rust half has no hot reload.** `byard dev examples/aura_weather`
  reloads every screen but draws a plain panel where the map is; the map
  itself changes only when the app is rebuilt.
- **Not measured**: the frame time while panning the map.
