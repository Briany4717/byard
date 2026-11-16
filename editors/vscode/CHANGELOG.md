# Changelog

## 0.1.0

- First release on the Marketplace and Open VSX.
- Syntax highlighting and language configuration for `.byd` files, with file icons.
- Colour picker for `0xRRGGBB` and `0xAARRGGBB` literals.
- Connects to the `byld-lsp` language server: diagnostics, hover, completion, go to definition, outline, rename, code actions, formatting and semantic highlighting.
- Finds the server from the `byld.lsp.serverPath` setting, `PATH`, `~/.cargo/bin` or a workspace `target/` folder, and offers to install it with cargo when it is missing.
- The `Byld: Restart Language Server` command.
