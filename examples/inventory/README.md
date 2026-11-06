# Stockroom

A small stock-keeping app, and the biggest test Byard has been given: lists that
grow, forms with numbers, validation, navigation three levels deep, dialogs,
saving, a Rust controller, and 2,000 products to see whether it stays fast.

It was written to find what Byard cannot do yet. The gaps it found are in the
gap log kept beside the pull request. Most of them have been fixed and the
workarounds removed; what is still open, and how the example works around it,
is under "Still open" below.

## What it shows

- **Dashboard**: five numbers, the stock value by category as bars, what is low
  or out, and the last movements. Tapping a product there opens it.
- **Products**, a navigation stack: a list with search, category chips, a
  low-stock switch and sortable columns (a windowed `ScrollView`); a product's
  page; Receive, Ship and Adjust in a dialog; Delete after asking; and the
  form, for a new product and for editing one, where every field says what is
  wrong and Save waits.
- **Movements**: the whole log, filtered by kind and by product.
- **Settings**: export to CSV (a Rust controller), put the sample data back,
  and the minimum a new product starts with.

Everything is saved to the app's `Store` after each change; the first run starts
with sixty sample products.

## Run it

From the repository root:

```sh
cargo run -p stockroom                 # the window
cargo run -p stockroom -- --dev        # the window, following its own files
```

It cannot run under `byard dev` or draw with `byard shot`, because it has a Rust
controller (`src/controller.rs`, which writes the CSV file). `byard check`
works, with one warning that says so.

## Test it

```sh
cargo test -p stockroom
```

The tests drive the app through the interpreter with no window, taps and typing
included, on a store in a temporary folder and a clock fixed with
`Interpreter::set_wall_clock`. `byard check` lowers every tab and route, so
`cargo run -p byard-cli -- check examples/inventory` checks the whole app once.

The 2,000-product test prints what it measured next to its ceilings in
`ci/budgets.toml` (`[inventory]`):

```sh
cargo test -p stockroom budgets -- --nocapture
```

## Make the pictures

Pictures are drawn without a window by the app's own binary. The word after the
file is the screen; `--tap x,y`, `--type text` and `--key Tab` act first, in the
order given.

```sh
cargo run -p stockroom -- --shot dashboard.png dashboard
cargo run -p stockroom -- --shot products.png products
cargo run -p stockroom -- --shot movements.png movements
cargo run -p stockroom -- --shot settings.png settings
# a product's page, then the Ship dialog
cargo run -p stockroom -- --shot detail.png products --tap 220,250
cargo run -p stockroom -- --shot ship.png products --tap 220,250 --tap 170,566
# the form, with a duplicate SKU and a bad price
cargo run -p stockroom -- --shot form.png products --tap 1014,104 \
    --tap 126,167 --type HW-001 --tap 481,376 --type abc --tap 126,450
```

Pictures use a store of their own (a folder under the temporary directory), so
they never read or change your catalogue. `--light` draws the light scheme, and the
clock is fixed at 2026-11-05 09:00 so a record made in a picture is dated the same
every time.

## Still open

What Byard cannot do yet, and what the example does about it:

- **No autofocus.** `focused:` only reports focus, so the form opens with no
  field focused and the user taps the first one.
- **A Toggle's label is not part of the Toggle.** Pressing the words "Low stock
  only" does nothing; the switch beside them is the control.
- **No `nil` literal.** "Nothing" is tested as `(product.id ?? 0) == 0` (the
  deleted product's name in the log).
- **A `for` builds every row.** A windowed list only culls layout and paint, so
  2,000 products take about a second to open in the debug test build and a
  keystroke in the search box takes up to 100 ms. `tests/it/budgets.rs` keeps it
  from getting worse; its ceilings are in `ci/budgets.toml` under `[inventory]`.
- **Testing needs the engine's internals.** `tests/it/common/mod.rs` copies about
  150 lines of `Interpreter`, `Dispatcher` and `Relay` setup from `aura_weather`,
  and a text's position leaves out an overlay's paint transform.

## Layout of the source

| File | What is in it |
|------|---------------|
| `main.byd` | The shell, the data, and every change made to it |
| `sample.byd` | The first-run catalogue |
| `stock.byd` | Shared functions: stock words, money, movement labels |
| `parts.byd` | Cards, tiles, chips, buttons, the dialog, `FormField` |
| `dashboard.byd`, `products.byd`, `detail.byd`, `form.byd`, `movements.byd`, `settings.byd` | The screens |
| `controller.rs`, `lib.rs`, `main.rs` | The Rust half and the entry points |
