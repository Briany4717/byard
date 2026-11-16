# Byld for VS Code

Byld is the language of [Byard](https://byard.dev), a Rust UI framework: you describe an interface in `.byd` files and Byard draws it. This extension gives those files an editor.

## What it does

- Syntax highlighting, brackets, comments and indentation for `.byd` files.
- A colour picker on `0xRRGGBB` and `0xAARRGGBB` literals.
- Through the `byld-lsp` language server: errors and warnings as you type, hover, completion, go to definition, document outline, rename, quick fixes, formatting (the same as `byard fmt`) and semantic highlighting.

Highlighting and the colour picker work without the server. Everything else needs it.

## Getting the language server

```sh
cargo install byld-lsp
```

The extension looks for `byld-lsp` in this order: the `byld.lsp.serverPath` setting, your `PATH`, `~/.cargo/bin`, then a `target/release` or `target/debug` folder of the workspace. If it finds none, it asks once whether to install it for you. Run `Byld: Restart Language Server` after installing it.

## Settings

| Setting | Default | What it does |
| --- | --- | --- |
| `byld.lsp.serverPath` | empty | Path to the `byld-lsp` executable. |
| `byld.lsp.trace.server` | `off` | Traces the messages between VS Code and the server (`messages` or `verbose`). |

## Commands

- `Byld: Restart Language Server`

## Links

- [Byard](https://byard.dev) and the [guide](https://byard.dev/guide/dev-loop.html)
- [Source and issues](https://github.com/Briany4717/byard)

Licensed under MIT or Apache-2.0, at your option.
