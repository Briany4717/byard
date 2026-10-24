# Data and I/O

```byld
View Main() {
    inject Http as http

    var state: Str = "idle"
    var temp: Float = 0.0
    var message: Str = ""

    let load = () => {
        state = "loading"
        http.get("https://api.open-meteo.com/v1/forecast?latitude=35.68&longitude=139.69&current=temperature_2m")
            ok res => {
                temp = res.json.current.temperature_2m
                state = "ok"
            }
            err e => {
                message = e.message
                state = "error"
            }
    }

    on mount => load()
    every 5min => load()

    Column #[bg: 0x14141C, p: 28, gap: 12, grow: 1] {
        when state == "loading" {
            Text("Asking about Tokyo") #[color: 0x9AA0AC]
        }
        when state == "ok" {
            Text("{temp}°C in Tokyo") #[color: 0xFFFFFF, size: 32]
        }
        when state == "error" {
            Text("Could not reach the forecast: {message}") #[color: 0xC08080]
        }
        Button("Refresh") => load()
    }
}
```

A real weather screen, and no Rust. Byld reaches the network, JSON, the clock and
storage through *capabilities*: Rust services the engine provides, that a view
calls the same way it would call a controller of its own. This chapter covers the
four that come with Byard.

## Asking, and being answered

A view never waits. A call to a capability, or to your own Rust, starts the work on a
thread pool and returns at once, and the window keeps drawing. When the answer comes,
it is delivered back on the logic thread and runs the arm you wrote:

```text
call ok name => action err name => action
```

The `ok` arm runs on success and binds the result to `name`. The `err` arm runs on
failure and binds the error. Both are optional, both are ordinary actions, and the
usual way to finish is to write a `var`. The screen then repaints by itself, as it does
for any `var`.

The call sits in an action: a button, `on mount`, a timer, or a named `let`. Before any
of those can use a capability, the view says it wants it:

```byld,fragment
inject Http as http
```

`inject Name as handle` makes the capability's methods callable as `handle.method(…)`.

### The error record

Every capability fails the same way. The `err` arm gets a record with a `kind`, for a
program to branch on, and a `message`, for a person to read. A failed HTTP request
adds `status` and `body` when there was an answer. Turn the wi-fi off and press the
button, and the `err` arm runs: the failure has a name, and the screen is not left
with a spinner that never stops.

### Late answers

Answers can arrive in any order. If a view asks for each keystroke of a search, a slow
answer to an old word may land after the answer to the new one. The arms see the
arguments of the call they belong to, as they were when it was made, so a guard is
enough to drop the stale one:

```byld
type Place = { name: Str, country: Str?, latitude: Float, longitude: Float }
type GeoReply = { results: List<Place>? }

View Main() {
    inject Http as http

    var query: Str = ""
    var found: List<Place> = []

    let search = (asked: Str) => {
        if asked.len < 2 {
            found = []
        } else {
            http.get("https://geocoding-api.open-meteo.com/v1/search?name={asked.urlEncoded()}&count=5")
                ok res => {
                    if asked == query {
                        let reply: GeoReply = res.json
                        found = reply.results ?? []
                    }
                }
                err e => {
                    if asked == query {
                        found = []
                    }
                }
        }
    }

    on change(query) => search(query)

    Column #[bg: 0x14141C, p: 24, gap: 8, grow: 1] {
        TextField #[bind: query, placeholder: "Search for a city", height: 40, bg: 0x1E1E2A,
                    color: 0xFFFFFF, radius: 8, p: (horizontal: 12)]
        for hit in found {
            Text("{hit.name}, {hit.country ?? "at sea"}") #[color: 0xFFFFFF]
        }
    }
}
```

Three things here are worth copying. `on change(query)` asks again whenever the field
changes. `"{asked.urlEncoded()}"` keeps a `&` or `#` typed into the field from cutting
the request short. And `let reply: GeoReply = res.json` gives the reply a type, so
`reply.resluts` is a check error and not an empty list, and what the server sent is
compared with `GeoReply` when it arrives.

## Http

```byld,fragment
inject Http as http
var status = 0
var sent = ""
let go = () => {
    http.get("https://example.com/items")
        ok res => { status = res.status }
    http.post("https://example.com/items", { name: "Ada" })
        ok res => { sent = res.body }
    http.request({ url: "https://example.com/items/1", method: "PUT",
                   headers: { authorization: "Bearer abc" }, body: { name: "Grace" },
                   timeout_ms: 5000 })
        err e => { sent = "{e.kind}: {e.message}" }
}
```

- `http.get(url)` fetches.
- `http.post(url, body)` sends a body. A record or a list is sent as JSON, and text is
  sent as it is.
- `http.request(record)` takes a `url` and optionally `method`, `headers`, `body` and
  `timeout_ms`.

An answer is a record: `status`, `ok`, `body` (the text) and `json`. `json` is the body
parsed, a record or list, so `res.json.current.temperature_2m` reads into the reply the
way it reads into a record you wrote. If the server says the body is JSON and it is not,
`json` is empty and `body` is intact. Answers with a status outside 200 to 299 go to
`err`, with `status` and `body` still on it, so `e.status` is there if you want to
branch on a 404.

An error's `kind` is `timeout`, `connect`, `decode`, `bad_request`, `http_status` or
`transport`. A request takes at most 30 seconds unless `timeout_ms` says otherwise, and
follows up to 10 redirects.

### Naming hosts once

A request that names only a path goes to the host named in `byard.toml`, so views do not
repeat it. A second host gets a name:

```toml
[http]
base_url = "https://api.open-meteo.com"

[http.hosts]
air = "https://air-quality-api.open-meteo.com"
geo = "https://geocoding-api.open-meteo.com"
```

Now `http.get("/v1/forecast?…")` goes to the first, and `http.get("air:/v1/air-quality?…")`
to the second. Pointing the whole app at a local mock server is one edit to the
manifest. Host names are lowercase letters, digits, `_` and `-`, starting with a letter.
A name that is not declared is an `unknown_host` error.

## Json

`Http` parses what it receives. `Json` is for JSON that came some other way:

```byld,fragment
inject Json as json
var parsed = 0
var text = ""
let go = () => {
    json.parse("{\"a\": 1}")
        ok v => { parsed = v.a }
        err e => { text = e.message }
    json.stringify({ a: 1 })
        ok s => { text = s }
}
```

`json.parse(text)` gives a record or list, and fails with `invalid_json` on text that
is not JSON. `json.stringify(value)` gives the text.

## Timers

A timer is not a call. It is a line in a view, and it lives as long as the view does:

```byld
View Main() {
    var seconds = 0
    var revealed = false
    var beats = 0
    var live = true

    every 1s => { seconds = seconds + 1 }
    after 3s => { revealed = true }

    Column #[bg: 0x14141C, p: 28, gap: 16, width: 420] {
        Text("open for {seconds}s") #[color: 0xFFFFFF, size: 22]

        when revealed {
            Text("`after 3s` fired, and will not fire again") #[color: 0x8FD19E]
        }

        Button(live ? "Stop the scoped timer" : "Start it again") => live = !live

        when live {
            Column #[bg: 0x1E1E2A, radius: 12, p: 16, gap: 4] {
                every 500ms => { beats = beats + 1 }
                Text("scoped timer running: {beats}") #[color: 0xFFFFFF]
            }
        } else {
            Text("cancelled, `beats` is frozen at {beats}") #[color: 0x9AA0AC]
        }
    }
}
```

`every 1s => …` repeats, and `after 3s => …` runs once. Durations are `ms`, `s` and
`min`: `500ms`, `30s`, `5min`. The first `every` fires after its interval, and an
interval of zero is refused.

Where you write a timer decides when it runs. At the top of a view, it runs while the
view is up. Inside a `when`, it runs while that `when` is true, stops the moment it turns
false, and starts again from nothing when it returns. There is no handle to hold and no
`stop` to forget.

A reply and a tick arrive the same way, so a timer is how a screen refreshes itself: the
forecast above asks again every five minutes, and closing the window ends it.

## Store

A *store* keeps values between runs. It is one JSON file, in the data directory your
platform gives the app, and you can open it and read it:

```byld
View Main() {
    inject Store as store

    var todos: List = []
    var draft: Str = ""
    var nextId = 0

    on mount => {
        store.get("todos", [])
            ok saved => {
                todos = saved
                nextId = saved.len
            }
    }

    Column #[bg: 0x14141C, p: 28, gap: 12, width: 480] {
        Row #[gap: 8] {
            TextField #[bind: draft, placeholder: "New task", grow: 1, height: 40, bg: 0x1E1E2A,
                        color: 0xFFFFFF, radius: 8, p: (horizontal: 12)]
            Button("Add") => {
                todos = todos.push({ id: nextId, text: draft, done: false })
                nextId = nextId + 1
                draft = ""
                store.set("todos", todos)
            }
        }
        for t in todos {
            Row #[gap: 12, align: center] {
                Text(t.text) #[color: 0xFFFFFF, grow: 1]
                Button("Remove") => {
                    todos = todos.filter(x => x.id != t.id)
                    store.set("todos", todos)
                }
            }
        }
    }
}
```

- `store.get(key, fallback)` reads a value. The second argument is what a missing key
  gives, which is what every first run of every app is. Without it you would assign
  nothing to a list.
- `store.set(key, value)` writes any value.
- `store.remove(key)` deletes one.
- `store.keys()` lists them.

Writes are atomic: a crash halfway through leaves the old file or the new one, not half
of each. A file that has been damaged loads empty, the failure is reported, and the next
write replaces it.

The file belongs to the app by its *identity*. Under `byard dev` that is the project's
name. In a shipped app it is the executable's name, or the `app_id` you set in Rust, never
the name of the entry file. See [the Rust half](rust-app.md).

## Your own capabilities

`Http`, `Json`, `Store` and `Timer` are reserved names. An app cannot provide a Rust
controller under one of them, so `inject Http as http` means the same in every project.
For anything else, such as a database, auth, a device, write a controller. The next
chapters show how.
