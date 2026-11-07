# RFC-0055: Constants, enums, private names and `else when`

- **Status:** Accepted
- **Author(s):** Briany4717
- **Created:** 2026-11-07
- **Last updated:** 2026-11-07
- **Depends on:** RFC-0008 (packages; §Pillar B is the namespace this extends),
  RFC-0027 (data and collection operations, whose `type` records are the
  nearest precedent), RFC-0007 (user views, whose parameter defaults the enum
  shorthand reads), RFC-0044 (the formatter).

> **Placement:** `docs/rfcs/0055-constants-enums-and-privacy.md`

---

## Summary

`byld` gets four small things a component library cannot do without:

1. **Constants**, `let gap = 8` at the top of a file: pure, typed, shared by
   every file of the project or package, and reached from a consumer as
   `m.gap`.
2. **Enums**, `enum Variant { filled, tonal, outlined }`: a closed set of named
   cases, written `Variant.tonal` (`m.Variant.tonal` from a consumer), checked
   by `byard check` so that a misspelt case is an error with a suggestion
   instead of a string that silently draws the default. Where a parameter's
   type is known, a call may write the case alone: `variant: .tonal`.
3. **Private names**, `private` before a `View`, `fn`, `type`, `let` or `enum`:
   visible in every file of its package and to nobody else. Using one from a
   consumer is `PrivateName`. In an app `private` changes nothing and is
   accepted.
4. **`else when`**: `when a { } else when b { } else { }`, sugar for the nested
   form, with the same meaning.

None of them adds anything to a program that does not use it. A constant is
evaluated once, when the program loads, and read as a value. An enum case is its
own name as text at run time. Privacy and the case checks are `byard check`
work.

## Motivation

Byard Material (RFC-0050) is a package of some forty components, and writing its
spike found the language short in exactly these places.

A package that wants one spacing scale writes `8` in thirty files, because
there is no top-level value; the only shared thing is a `fn` that returns it,
`fn gap() -> Int => 8`, called as `gap()` and `m.gap()`, which says "function"
about a number.

A component with five looks takes `variant: Str = "filled"` and branches on
`variant == "tonal"`. `variant == "tonl"` is not an error, and neither is
`Button(variant: "tonl")`: the second draws as the default, the first is never
true. The framework's own principle is that what cannot work is a check error,
never a silent no-op, and strings are the one place the language still breaks
it.

A package's helpers are public because every top-level name is. A `Button` is
written with a private `ButtonLabel` view and a `padding` function, and the day
the library renames them is the day an app that reached in breaks. Without a
way to say "mine", nothing in a package can be renamed.

Five exclusive `when`s for five icon sizes are written as five conditions that
must each exclude the others, or as four levels of `else { when … }` whose
braces are all noise.

## Guide-level explanation

```byld
// kit/src/tokens.byd
let gap = 8
let pad = gap * 2                      // constants may use constants

enum Variant { filled, tonal, outlined }

private fn tint(v: Variant) -> Int => v == Variant.filled ? 0x6750A4 : 0xE8DEF8

// kit/src/button.byd
View Button(label: Str, variant: Variant = Variant.filled) {
    Box #[bg: tint(variant), p: pad, radius: gap] { Text(label) }
}

private View Hint() { Text("only the kit draws me") }
```

```byld
// app/src/main.byd
use kit as m

View Main() {
    Column #[gap: m.gap] {
        m.Button("Save")
        m.Button("Cancel", variant: .tonal)       // short form
        m.Button("More", variant: m.Variant.outlined)
        m.Hint()                                   // error: `Hint` is private to package `kit`
    }
}
```

```byld
View Size(size: Int) {
    when size < 16 {
        Text("small")
    } else when size < 32 {
        Text("medium")
    } else {
        Text("large")
    }
}
```

### What each one does

**A constant** is `let name = expr` or `let name: Type = expr` at file level.
It may read other constants and call top-level functions, and nothing else: no
`var`, no parameter, no controller, no clock. Constants can be declared in any
order across the files of a package, and two that read each other are an error
naming the cycle (`gap → pad → gap`). A view's own `let`, `var` or parameter of
the same name hides the constant inside that view, as it hides a function.

**An enum** declares cases, separated by commas or line breaks. A case is
written `Variant.tonal`. It can be compared with `==` and `!=` (to a case of
the same enum; comparing it with a string is a type mismatch), chosen with
`?:`, stored in a `var`, passed as an argument and given as a parameter default.
In text it is its name: `"{variant}"` is `tonal`. There is no `match`: a
view chooses with a chain of `when … else when … else`, or with `?:`, and the
checker tells a misspelt case (`Variant.tonl`: "`Variant` has no case `tonl`;
did you mean `tonal`?") wherever it is written.

**A case written alone**, `.tonal`, means `Variant.tonal` in two places: an
argument for a parameter declared `: Variant`, and that parameter's own
default. Anywhere else `.tonal` is still a style class (`style: .title`), and
comparing an enum with one (`variant == .tonal`) is an error that says to
write the enum.

**`private`** goes before `View`, `fn`, `type`, `let` or `enum`. Everything
declared in a package, private or not, is visible to every file of that package.
A consumer cannot use a private name: `m.Hint()`, `use kit.{Hint}`, `m.helper(1)`,
`m.padding`, `m.Variant.tonal` and a `type` named in an annotation are each
`PrivateName`, which says the name is private and which package declares it. In
an app, or in a package checked on its own, `private` is accepted and changes
nothing.

**`else when`** continues a `when` chain. Each arm is its own branch, as
separate `when`s are: only the arm being shown is built, and the views in it are
instances of their own, which start over when the chain moves to another arm and
back (state belongs to a view instance, so a `Counter` inside an arm counts again
from zero).

## Reference-level explanation

### 1. Grammar

```
file       := (use_decl)* item*
item       := "private"? (view | fn | record | enum | const)
const      := "let" IDENT (":" type)? "=" expr
enum       := "enum" IDENT "{" (IDENT ","?)* "}"
type       := IDENT ("." IDENT)? ("<" type ("," type)* ">")? | …    // a qualified name is new
when       := "when" expr "{" member* "}" ("else" ("when" … | "{" member* "}"))?
```

`private` and `enum` are contextual, like `type`: they are keywords only at the
start of an item, followed by what an item starts with, and ordinary names
everywhere else. A program that has a variable called `enum` or `private`
reads as before. A `private` in front of anything else is the existing
"expected 'View', 'use', 'fn', 'type', 'enum' or 'let'".

### 2. Names, and canonical names

Constants and enums are namespaced like functions (RFC-0008 Pillar B): a
package's are canonically `<package>.<name>`, an app's keep their bare names.
The resolver rewrites a reference to the canonical name, so the checker and the
interpreter see one spelling and never a package:

| Written | Where | Becomes |
|---|---|---|
| `gap` | inside package `kit`, no binding of that name in the view | `kit.gap` |
| `m.gap` | any file with `use kit as m` | `kit.gap` |
| `Variant.tonal`, `m.Variant.tonal` | likewise | `kit.Variant.tonal`, as the case `tonal` of the enum `kit.Variant` |
| `variant: Variant`, `variant: m.Variant` | an annotation | the type `kit.Variant` |

A qualified type name `m.Variant` is new syntax in an annotation and nowhere
else. Records (`type`) keep their program-wide bare names (RFC-0027): they are
not namespaced, and this RFC does not change that, only whether a package lets a
consumer name one.

A name the view binds anywhere (a parameter, a `var`, `let` or `fn`, a loop
variable, a lambda or event parameter) is not rewritten in that view. This is
the rule functions already follow, and it is deliberately coarse: it is
decidable without scopes, and the failure is a name `byard check` cannot find,
never a different value.

Declaring the same constant or enum twice in a package is
`DuplicateConstant`/`DuplicateEnum`, a case twice in one enum is
`DuplicateEnumCase`, as `DuplicateFunction` is for functions.

### 3. Constants

A constant's initialiser is checked as a function body is (RFC-0002): it may
name other constants, the functions of the program and its own lambda
parameters, and may not assign, call a controller, or read a name that is none
of those (`ImpureConstant`). `now()` and `nowUtc()` are refused as they are in a
function (RFC-0049 §3).

The constants form a graph by which one reads another, directly or through a
function it calls. A cycle is `ConstantCycle` with the path
(`gap → pad → gap`), checked once for the program. Without cycles an order
exists, and the interpreter evaluates the constants in it, once, in `load_views`,
so the value is computed when the program loads and again only when it is
reloaded. Reading one, in a `lower_ident`, is cloning a captured value: there is
no signal, no memo and no scope.

The type is the annotation when there is one, else the type of the initialiser as
a `let` is inferred. An annotation the initialiser cannot satisfy is
`ConstantTypeMismatch`.

### 4. Enums

An enum is a name and an ordered list of unique case names. At run time a case
is the `Str` of its name; at check time a value of the enum has the type
`Named(<enum>)`, which only the cases of that enum, other values of that type
and `Unknown` are assignable to. Consequences:

- `variant == Variant.tonal` compares two texts; it is true for the same case.
  Two different enums sharing a case name compare equal at run time and are a
  `TypeMismatch` at check time, because their types differ.
- A string cannot be passed for an enum parameter (`ArgumentTypeMismatch`), and
  an enum cannot be compared to a string (`TypeMismatch`).
- A case of an enum that does not have it, `Variant.tonl`, is
  `UnknownEnumCase`, with the nearest case as the hint. The enum's name used as
  a value (`variant: Variant`) is told as `UnknownName`.
- **Interpolation** writes the case name. **Controllers and JSON** see text:
  an enum parameter reaches a controller as the string `"tonal"`, and a string
  arriving from one is an enum value if it equals a case and matches nothing if
  it does not. The boundary is not checked at run time, because a case set is not
  something the host can promise to hold; an app that wants the check compares
  with `==`.
- Enum cases are not exhaustive-checked in a `when` chain. See Unresolved.

**The short form.** After resolving, for each call to a view whose declaration
is known, an argument (positional or named) or a default that is exactly `.case`
(or `cond ? .a : .b` of them) is rewritten to the enum case when the parameter's
declared type names an enum. The case is then validated like any other
(`Variant.tonl` is `UnknownEnumCase`). The rewrite runs in the resolver because
it is the one pass that holds both the call and the callee's parameters; nothing
about it exists at run time.

### 5. Private names

`private` is a flag on the declaration. The resolver records, per package, which
views, functions, constants, enums and records are private, and reports
`PrivateName { name, package }` wherever a name resolves into another package: a
qualified view (`m.Hint()`), a selective import (`use kit.{Hint}`), a function
call, a qualified constant or enum, and a record named in an annotation.
References to a private name inside its own package, by bare name or through the
package's own name (`kit.Hint`), are not errors, and neither is a public view
that uses a private one: privacy is about who can write the name.

A private name is not "missing": the error is `PrivateName`, not
`UnknownImportSymbol`, because "unknown" would send the author looking for a
typo. The suggestion lists no private names.

### 6. `else when`

`else` followed by `when` parses the `when` as the single member of the else
branch, so the tree is the one the nested form has and every later stage
(lowering, `byard check`'s every-branch walk, hot reload, the language server)
needs no change. The formatter works on tokens and prints the form the author
wrote.

### 7. Tooling

- **Formatter (RFC-0044).** Token based, so `enum`, `private`, constants and
  `else when` need only spacing rules; the meaning check compares the new
  declarations too.
- **Language server.** Completion offers constants and enums, and a case after
  `Enum.`; hover shows a constant's type and an enum's cases; document symbols
  list them; a private name is not offered to a consumer.
- **`byard check`** reports every error above, and lowers every arm of an
  `else when` chain, because each arm is a nested `when` and `check` already lowers every branch of those.

## Drawbacks

- **Three new top-level forms and a modifier**, in a language that had `View`,
  `fn`, `type` and `use`. Each is one line of grammar, none has a run-time
  cost, and the alternatives below are all larger.
- **Enums are text at run time.** A host sending `"tonl"` into an enum
  parameter is not stopped. The alternative, a distinct value kind, touches every
  place a `Value` is matched, to protect the boundary the language does not
  control.
- **The binding rule is coarse.** A view that binds `gap` anywhere cannot use
  the package's `gap` by its bare name. It is the function rule, it fails as an
  unknown name, and `kit.gap` always works.
- **No `match`.** A chain of `else when … ==` is longer than a `match`, and
  exhaustiveness is not checked.
- **Records stay global**, so a private `type` can be named only by a consumer
  that did not declare one of that name, and two packages cannot both have a
  `Place`. Namespacing records is a separate change.

## Rationale and alternatives

- **Strings with a checked set** (`variant: "filled" | "tonal"`) is a union of
  literals: a second kind of type, and a check at each use. An enum has a name
  to hang the suggestion, the completion and the docs on.
- **A `match`.** The chain does everything a `match` would in a language with no
  patterns, and a `match` of enum cases is the first of several constructs
  (patterns, bindings, guards) the language would then be asked for.
- **`variant.name`.** The value is already its name; a method for it would be
  the identity.
- **Constants as zero-argument functions.** It works today and reads as a call.
  It also cannot be told apart from a function that does work, so nothing can
  promise it is evaluated once.
- **`pub` instead of `private`**, with private the default. It would break
  every package written so far, and `private` is the exception in a component
  library (a few helpers), not the rule.
- **`export` lists or a `mod.rs`-style manifest.** One more file to keep in step
  with the code.
- **`else if`.** Taken by the action form (`if cond { } else if …`), and
  `when` is the structural one; the same word in both places would hide which of
  the two a block is.

## Prior art

- **Swift** has `.tonal` where the type is known, `private`/`internal` and enums
  without payloads; the short form here is the same rule, restricted to the one
  place the checker has a type for free.
- **Rust** has `pub`, whose default is the opposite of this one's for a reason
  that does not apply: its crates grew up private.
- **Dart** has leading-underscore privacy per library and enums as a closed set.
- **Kotlin**'s `private` is per file; here it is per package, because the unit a
  consumer sees is the package.
- **Zig/Rust/Go** evaluate constants once at compile time; the interpreter does
  the equivalent at load.

## Resolved questions

### Q1, does each `else when` arm keep its own state?

**Options.** (a) One scope for the whole chain, with the arms switching inside
it; (b) each arm a branch of its own, as separate `when`s are.

**Resolution: (b).** The sugar is defined as the nested form, and the nested form
is (b): the arm after `else` is built when the previous conditions are false and
torn down when one becomes true, and the view instances in it go with it. (a)
would make `else when` mean something the nested form cannot say.

### Q2, `.tonal` where the type is known, or only strings?

**Resolution.** In arguments and defaults, by the resolver. Typing it anywhere
else wants an expected type to flow downwards through the checker, which today
infers upwards only; that is a larger change than this RFC and it is not needed
for a component library, whose cases are mostly arguments.

### Q3, does a controller or JSON boundary see text or an error?

**Resolution.** Text, both ways, unchecked. See §4.

### Q4, can a view switch on an enum?

**Resolution.** With `when … else when …` and `?:`. `variant.name` is not
added. See §4.

## Future possibilities

- A `match` over enum cases with an exhaustiveness check, once the language has a
  second reason to want patterns.
- An error for a public declaration whose signature names a private type or
  enum, which no consumer could then call.
- Enums with a payload.
- Namespacing records, so `private type` can share a name with a consumer's.
- Checking, at the host boundary, that text given to an enum parameter is one of
  its cases.
- `use kit.{gap, Variant}`: selective import of constants and enums.
