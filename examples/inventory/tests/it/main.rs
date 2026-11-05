//! Every integration test of Stockroom, in one binary. They drive the app
//! through the interpreter with no window: the project is read from this
//! directory, taps land where a person would tap, and the store is a file in a
//! temporary folder that nothing else uses.

mod budgets;
mod common;
mod dashboard;
mod export;
mod forms;
mod products;
mod stock;
