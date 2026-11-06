# Erratum to RFC-0028: how a name is spelled in Byld, and where an app's data lives

- **Status:** Active erratum (amends, does not replace, RFC-0028)
- **Author(s):** Briany4717
- **Created:** 2026-11-06
- **Applies to:** RFC-0028 §2 (what `#[byard_controller]` and `#[derive(HostValue)]` expose
  to a view), RFC-0025 §4 and RFC-0031 (the `anim.*` curve names), and RFC-0039 (the names
  of a native view's props and events).
- **Authority:** the implementation, and the tests named against each correction below.

---

RFC-0028 says a controller's methods are called "by name" and a derived struct crosses as a
record "field for field". It never says which spelling. The implementation took the Rust
identifier as written, so `fn export_csv` was `room.export_csv(...)` in a language whose other
methods are `toLower`, `sortBy`, `formatDate` and `urlEncoded`. Looking at that exposed that
Byld already follows three conventions, each for a reason, and this erratum states them so a
new name is put in the right one. 0.1.0 is the last moment a rename is cheap.

## 1. Three rules (§2)

| What | Spelling | Examples |
|------|----------|----------|
| A **method** | `camelCase` | `toLower`, `sortBy`, `formatDate`, `urlEncoded`, `room.exportCsv(...)`, `anim.easeInOut(...)` |
| An **attribute** or an **event** | `snake_case` | `swipe_back`, `deep_link`, `max_depth`, `border_width`, `row_height`, `collapse_header`, `key_down`, `route_change` |
| A **record field** | the name it was given | `temperature_2m`, `timeout_ms`, `unit_price` |

**Methods are camelCase** because a method is called, and the calls a Byld programmer writes
all day (values, capabilities) are camelCase. A controller method and an animation curve are
methods, so they follow.

**Attributes and events are snake_case** because they sit in a `#[ ... ]` block or after
`on`, a different place in the grammar from a call, and they were snake_case before this
erratum (`row_height`, `key_down`). A native view's props and events are attributes and
events, so they stay exactly as the Rust field declares them and `[native_views.Name]` in
`byard.toml` writes the same names.

**Record fields keep the name they were given** because a record is data, and data comes from
somewhere else: JSON from a server has `temperature_2m`, the Http request record is
`timeout_ms`. A Rust struct that is also serialized into the `Store`, or sent over HTTP with
serde, must not be one name in Byld and another in its JSON, so `#[derive(HostValue)]` and the
`BYARD_FIELDS` list use the Rust field name unchanged. `#[host(rename = "...")]` gives a field
another name, used exactly as written, for anyone who wants one.

## 2. What the macro and the checker do (§2)

**A controller method** `export_csv` is `exportCsv` in Byld; the Rust name does not change.
Calling the Rust spelling from a view gives the reply error ``error[UnknownMethod]: unknown
controller method `export_csv` `` with the help to call `exportCsv`. The checker cannot see
Rust, so this is the run-time check, where a misspelt controller method was already
reported. `#[byard(rename = "...")]` on a method gives it exactly the name written, for the
rare method whose automatic name is wrong, and the automatic spelling then stops working. Two
methods that would share a Byld name are a compile error naming both.

**The animation curves** `anim.ease_in`, `anim.ease_out` and `anim.ease_in_out` are
`anim.easeIn`, `anim.easeOut` and `anim.easeInOut`. The old names are an
`UnknownAnimation` error whose suggestion is the new name. No other built-in method (value,
list, text, `Http`, `Json`, `Store`, `Timer`) had an underscore.

**The keyframe-step easing words** follow the curves, so a curve has one name everywhere:
`60%: (160, 0) easeInOut`, `easeIn`, `easeOut`. The old `ease_in`, `ease_out` and
`ease_in_out` on a step are an error ("unknown keyframe easing ... did you mean `easeInOut`?").

RFC-0025 writes `ease_in`, `ease_out` and `ease_in_out` in its keyframe examples and grammar
(§4, `curve_fn`), and RFC-0031 shows `anim.ease_in_out`; read them as `easeIn`, `easeOut` and
`easeInOut`, both as `anim.*` calls and as the easing word on a step.

**What an app must change:** a Byld call to a controller method whose Rust name has an
underscore, any `anim.ease_in`, `anim.ease_out` or `anim.ease_in_out`, and the same three
words after a keyframe step.

Covered by `a_method_is_called_by_its_camel_case_name`,
`the_rust_name_is_unknown_and_the_error_names_the_camel_case_one`,
`an_explicit_name_replaces_the_automatic_one` (methods);
`a_curve_is_a_method_and_is_camel_case`,
`the_snake_case_curve_names_are_unknown_and_suggest_the_camel_case_one` (curves); `the_snake_case_keyframe_easings_are_errors_that_suggest_the_camel_case_one`
(keyframe steps);
`props_and_events_keep_their_snake_case_names` (attributes and events);
`a_field_is_a_record_key_exactly_as_declared` and `a_controller_field_keeps_its_name`
(fields).

## 3. An app's data directory has one source (§3, RFC-0029 §6)

`Store` filed its file under `ProjectDirs::from("dev", "byard", app_id)`, and a controller
that wanted to write beside it had to repeat that call.

**Correction.** `byard::data_dir(app_id)` (in `byard-core` as `cap::data_dir`) returns that
directory, and `Store::for_app` calls it. It is a free function because it needs only the
identity, so a controller built before `App::run` can call it; a method on `App` would not
exist yet, and a context handed to a controller would make every controller take one. The
identity is the one RFC-0029 §6 already fixes: the project's name under `byard dev`, and in a
shipped app the executable's name or the `app_id` set in Rust.

Covered by `a_store_for_an_app_lives_in_the_apps_data_dir`.
