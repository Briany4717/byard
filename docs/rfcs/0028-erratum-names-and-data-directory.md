# Erratum to RFC-0028: how a Rust name is spelled in Byld, and where an app's data lives

- **Status:** Active erratum (amends, does not replace, RFC-0028)
- **Author(s):** Briany4717
- **Created:** 2026-11-06
- **Applies to:** RFC-0028 §2 (what `#[byard_controller]` and `#[derive(HostValue)]` expose
  to a view), and RFC-0039 (the names of a native view's props and events).
- **Authority:** the implementation, and the tests named against each correction below.

---

RFC-0028 says a controller's methods are called "by name" and a derived struct crosses as a
record "field for field". It never says which spelling of the name. The implementation took
the Rust identifier as written, so `fn export_csv` was `room.export_csv(...)` in a language
whose every other name is camelCase (`toLower`, `sortBy`, `formatDate`, `urlEncoded`).
Building a real app found it, and 0.1.0 is the last moment it is cheap to change.

## 1. A name crosses in camelCase (§2)

**Decision.** Byld spells every name camelCase, so a name that crosses the boundary is
translated from Rust's snake_case by the macros:

- a controller method `export_csv` is `exportCsv` in Byld;
- a field of a `#[derive(HostValue)]` struct, `unit_price`, is the record key `unitPrice`;
- the fields `#[byard_controller]` lists in `BYARD_FIELDS` are spelled the same way;
- a native view's `#[prop]` and `#[event]` fields are attribute and event names in the same
  way (`max_value` is `maxValue`), and `[native_views.Name]` in `byard.toml` writes the
  camelCase name. This is the rule `[theme]` already follows: tokens are snake_case in
  `byard.toml` and camelCase in Byld.

RFC-0028 does not decide field naming, so there was nothing to contradict: the rule applies
to fields because a record a controller returns is read in the same expressions as the
methods, and a view that wrote `reply.unit_price` beside `list.sortBy(...)` would be
spelled in two styles for no reason a user could see.

The Rust name does not change, and nothing is lost: a name that is one word, which is most
of them, is the same in both.

**Calling the Rust name.** A view that writes `room.export_csv(...)` gets a reply error,
`error[UnknownMethod]: unknown controller method `export_csv`` with the help
``a controller method is called by its camelCase name in Byld: `exportCsv` ``, rather than a
bare "unknown method". The checker cannot see Rust, so this is the run-time check; it is the
same place a misspelt controller method was already reported.

**Overriding it.** The automatic name is wrong for the odd method (an acronym, a name Byld
already uses). `#[byard(rename = "…")]` on the method gives it exactly the name written, and
the automatic spelling then stops working. For a record field the existing
`#[host(rename = "…")]` does the same, used as written. Two methods that would share a
Byld name are a compile error naming both.

**What an app must change.** A Byld call to a controller method whose Rust name has an
underscore, a Byld read of a derived struct's multi-word field, and a `[native_views]` entry
for a prop whose Rust name has an underscore. The last is caught when the app starts, by the
check that the declaration matches the registered view.

Covered by `a_method_is_called_by_its_camel_case_name`,
`the_rust_name_is_unknown_and_the_error_names_the_camel_case_one`,
`an_explicit_name_replaces_the_automatic_one`, `a_snake_case_field_is_a_camel_case_key`,
`a_controller_field_is_described_by_its_camel_case_name` and
`a_multi_word_prop_is_camel_case_in_byld`.

## 2. An app's data directory has one source (§3, RFC-0029 §6)

`Store` filed its file under `ProjectDirs::from("dev", "byard", app_id)`, and a controller
that wanted to write beside it had to repeat that call.

**Correction.** `byard::data_dir(app_id)` (in `byard-core` as `cap::data_dir`) returns that
directory, and `Store::for_app` calls it. It is a free function because it needs only the
identity, so a controller built before `App::run` can call it; a method on `App` would not
exist yet, and a context handed to a controller would make every controller take one. The
identity is the one RFC-0029 §6 already fixes: the project's name under `byard dev`, and in a
shipped app the executable's name or the `app_id` set in Rust.

Covered by `a_store_for_an_app_lives_in_the_apps_data_dir`.
