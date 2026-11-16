# byld: what to add

The features byld should gain, how each would be written, and why. Two lists: what 1.0
needs, and what people would simply like. A third says what to leave out.

[The reference](../guide/src/language-reference.md) is what the language is today.
[The roadmap](roadmap.md) is the order to build this in, with the fixes that come first.
Nothing here is implemented; a sample is a proposal, and the ones marked *(case)* are
already in `crates/byard-cli/tests/conformance/15_wanted_for_1_0.byd`, waiting to pass.

Every proposal is held to four tests, and one that fails any of them is in the last
section:

1. **It can be checked.** A mistake in using it is an error where it is written.
2. **It ends.** No form lets a view or an action run for ever.
3. **Nothing changes in place.** A value a reader holds is the value it read.
4. **It costs nothing unused.** A program that does not write it pays nothing for it.

## Needed for 1.0

### `match` *(case)*

The language has enums and no way to say "one thing for each case". A chain of `when`s
does it, and nothing notices when the chain forgets one.

```text
match status {
    .idle    => Text("Nothing yet")
    .loading => Spinner()
    .failed  => Text("Could not load")
}

let label = match size { .small => "S", .medium => "M", .large => "L" }
```

- An expression, a view member and an action statement, as `when` and `? :` and `if` are
  today for two cases.
- The cases may be written short, `.idle`, because the value says which enum it is.
- **Every case is covered**, or there is an `else =>`. A missing case is `NonExhaustive`
  and names it. This is the reason to have `match` at all.
- It also takes a `Bool`, an `Int` or a `Str` with literal arms, and then `else` is
  required.
- As a view member it is `when` chains underneath: only the arm that holds exists.

### Enum cases that carry values *(case)*

```text
enum Load {
    idle
    loading
    failed(message: Str)
    ready(items: List<Item>)
}

var state = Load.idle

match state {
    .idle            => Button("Load") => load()
    .loading         => Spinner()
    .failed(message) => Text("Could not load: {message}")
    .ready(items)    => for item in items { Row(item) }
}
```

Every screen that loads something carries `loading`, `error` and `data` as three `var`s
that can disagree: loading and failed at once, data with no loading ever started. One
`var` of this type cannot be in a state that makes no sense, and `match` makes the view
handle each one.

- Built with `Load.failed(message: "offline")`. Fields are named, as a record's are.
- Read only through `match`, which binds them. There is no `state.message`.
- To a controller and in JSON a case with values is `{ "case": "failed", "message": … }`,
  and a plain case stays its name, as today.

### Nothing, for any value *(case)*

`T?` is a parameter's today. It should be any value's.

```text
var selected: Product? = none
type User = { name: Str, email: Str? }

let city = user?.address?.city ?? "unknown"

Button("Open") => {
    if let product = selected {
        navigate(path, "/product/{product.id}")
    }
}

let first = xs.get(0) ?? fallback
```

- `none` is a value of every `T?` and of nothing else.
- `a?.b` is `none` when `a` is, and `a.b` otherwise.
- `if let name = expr { … }` in an action, and `when let name = expr { … }` in a view,
  bind the value when there is one.
- `xs.get(i)` is a `T?`. `xs[i]` stays a `T`, for when the index is known to be there,
  and is a fault when it is not (roadmap, step 10).
- Stockroom's `(product.id ?? 0) == 0` becomes `product == none`.

### Loops in an action *(case)*

```text
Button("Select all") => {
    for item in items {
        if item.hidden { continue }
        selected = selected.push(item.id)
    }
}

for i in 0..count { … }          // also in a view
```

- `for` as a statement, with `continue` and `break`. `return` leaves the action.
- `a..b` is the `Int`s from `a` up to, not including, `b`.
- No `while` and no loop without a list: an action always ends.

### Functions with a body *(case)*

```text
fn total(items: List<Item>, tax: Float = 0.0) -> Float {
    let net = items.map(i => i.price * i.qty).sum()
    net * (1 + tax)
}

total(cart, tax: 0.16)

fn first<T>(xs: List<T>) -> T? => xs.get(0)
```

- A block body: `let`s, then one last expression that is the result. Still pure.
- Defaults and arguments by name, exactly as a view takes them.
- Type parameters on `fn` and on `type`. No bounds, no traits: a `T` can be stored,
  passed and compared with `==`, and that is all.
- Recursion with a depth limit that is an error with the call's line, so a tree can be
  walked. If that proves hard to make cheap, leave recursion out and keep the error
  message that says to use `reduce`.

### `Map` and `Set` *(case)*

```text
var stock: Map<Str, Int> = ["A-100": 3, "B-200": 0]

stock["A-100"] ?? 0
stock = stock.set("C-300", 12)
stock = stock.remove("B-200")
for sku, qty in stock { … }

var tags: Set<Str> = Set(["new", "sale"])
tags = tags.add("clearance")
tags.contains("sale")
```

A list of records searched with `find` is what programs do today, and it is linear for
every lookup. `m[k]` is a `V?`. Both keep the order things were added in, so a `for` over
one is the same every time. Methods return a new value, as a list's do.

### The list methods people write by hand *(case)*

`first()`, `last()`, `get(i)`, `any(f)`, `all(f)`, `count(f)`, `sum()`, `take(n)`,
`skip(n)`, `insertAt(i, x)`, `updateAt(i, x)`, `flatMap(f)`, `groupBy(f)` (a
`Map<K, List<T>>`), `unique()`, `zip(other)`, `chunk(n)`, and `enumerate()` for a list of
`{ index, item }`.

### `Color`, `Duration`, `Angle` *(case)*

```text
let brand: Color = 0x3B82F6
brand.alpha(0.5)
brand.mix(0x000000, 0.2)
brand.lighten(0.1)
Color.rgb(59, 130, 246)

let wait: Duration = 300ms
after wait * 2 => …
```

Today all three are numbers: `{brand}` draws `3900150`, `200ms` is `200`, and
`brand + wait` checks. As types of their own a literal still reads the same, the wrong
arithmetic is an error, and the methods have somewhere to live. `Duration * Int` and
`Duration + Duration` are durations; a `Duration` in a text shows its unit.

### A reply with a type *(case)*

```text
type Forecast = { current: { temperature_2m: Float }, daily: List<Day> }

http.get(url)
    ok res => {
        let forecast = res.json as Forecast
        temp = forecast.current.temperature_2m
    }
    err e => { message = e.message }      // kind: "shape", path: "daily[3].max"
```

`as Type` compares what arrived with the type, there, and a reply of the wrong shape goes
to the `err` arm with the path of the first difference. After it, a misspelt field is a
check error. Reading a field of an untyped reply is a warning that says to give it a type.

### State above the views *(case)*

```text
store Session {
    user: User? = none
    theme: Scheme = .system
}

View Header() {
    inject Session as session
    Text(session.user?.name ?? "Guest")
    Button("Sign out") => session.user = none
}
```

One instance for the program, read and written through the `inject` a context already
uses, and reactive field by field. `store Name persist "session" { … }` keeps it between
runs over the `Store` capability. This replaces passing a `bind` through six views, and
it is opt-in: a program with no `store` is as it is today.

### Writing a field

```text
user.name = "Ada"            // user = { ..user, name: "Ada" }
todos[i].done = true         // todos = todos.updateAt(i, { ..todos[i], done: true })
```

Sugar over what the language does now. The value is still replaced, not changed, so
everything that read it follows.

### Text

```text
let help = """
    Usage:
      byard dev
    """

/// A card with a title and a body.
/// `tone` picks the colours.
View Card(title: Str, tone: Tone = .neutral, content) { … }

"{count:3}"   "{price:08.2}"   "{name:<12}"   "{ratio:.1%}"
```

- A multi-line string drops the indentation of its closing quotes.
- `///` before a declaration is its documentation: hover in the editor, and the page
  `byard doc` writes for a package.
- A width, a fill, an alignment and a percentage in a format.
- `/* … */`.

## Wanted

Not needed to call it 1.0. Each would make someone's day.

### A pipe

```text
let names = users |> filter(u => u.active) |> sortBy(u => u.name) |> map(u => u.name)
```

`x |> f(a)` is `f(x, a)`. For shared functions, which cannot be called with a dot.

### Destructuring

```text
let { name, email } = user
for { id, title } in todos key id { … }
let (width, height) = size
```

### Text that knows its language

```text
Text(t"{count} items")
```

A `t"…"` string is looked up in the catalogue for the current language, with its
interpolations as named places, and with plural forms chosen by number. The catalogue is
a file per language in the project. Design the syntax before 1.0 even if the feature comes
after it, because it is how every visible text in every app will be written.

### Dates

`Date`, `Time` and `Instant` instead of ISO text, with arithmetic (`today + 3.days`),
comparison, and `format` that follows the language.

### Units on numbers

`16px`, `1.5rem`, `50%`, as the types of lengths, so a length cannot be given where an
opacity is wanted.

### Names for arguments at the call

`Card(title:, tone:)` for `Card(title: title, tone: tone)`, when the names agree.

### Computed bindings

```text
Slider #[bind: celsius.map(to: c => c * 9 / 5 + 32, from: f => (f - 32) * 5 / 9)]
```

A binding seen through a pair of functions, for a control that shows one unit and stores
another.

### `where` on a `for`

```text
for todo in todos where !todo.done key todo.id { … }
```

### Pattern arms in `match`

```text
match (loading, error) {
    (true, _)     => Spinner()
    (false, none) => List(items)
    (false, e)    => Text(e)
}
```

### Modules by folder

`ui/card.byd` is reachable as `ui.Card` when the project sets `modules = true`. Everything
being visible everywhere is right for a small project and crowded for a large one.

### Assertions for developers

```text
assert index < items.len, "the selection is past the end"
```

Checked under `byard dev` and `byard test`, reported like a fault, and absent from a
shipped app.

### Previews

```text
preview "empty" { TodoList(todos: []) }
preview "three" { TodoList(todos: sample) }
```

A named use of a view with sample values. `byard shot --preview`, `byard test` and an
editor's side panel draw each one. It is the cheapest way to see every state of a view,
and to test it.

## Left out, and why

| Asked for | Why not |
|-----------|---------|
| `while`, `loop` | An action must end. A list bounds every loop. |
| `null`, and nothing as a member of every type | It is `T?` or it is a value. |
| Exceptions, `try` | A call that can fail has an `err` arm. A fault does not stop an app. |
| Mutation in place, `xs.push(x)` changing `xs` | A write to a `var` is the one thing the engine watches. |
| Classes, inheritance | A view composes; it has no parent. |
| Traits, interfaces, bounds on type parameters | They buy library authors power and cost every reader a concept. Rust is one file away. |
| Macros | The language is small enough to read without knowing what a word expands to. |
| User-defined operators | `a + b` means one thing. |
| `async` and `await` | A view never waits. The arms are the model. |
| A second lambda syntax, a second string quote | One way to write each thing. `\|x\| e` should go, in an edition. |
| Global `var` | A `store` names its state and says who may reach it. |
| Reflection, `eval` | Everything the checker cannot see is something it cannot check. |
| Arbitrary Rust inline in a `.byd` | The boundary is a controller, with types on both sides. |
