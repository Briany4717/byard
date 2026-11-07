//! Integration tests of `byld-lsp`, in one binary: the server is started as
//! the editor starts it, spoken to over its standard input and output, and
//! asked what it says about real files.

mod aura_weather;
mod bindable_params;
mod client;
mod examples;
mod package_language;
mod package_names;
mod project;
