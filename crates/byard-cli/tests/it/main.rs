//! Every integration test of `byard-cli`, in one binary.
//!
//! One file per subject, as before, but compiled and linked once: each
//! `tests/*.rs` used to be its own executable linking the whole engine, and
//! relinking all of them was most of the time an edit spent before a test
//! could run.

mod callbacks_example;
mod checkbox_example;
mod collapse_header_example;
mod corner_smoothing_example;
mod data_ops_example;
mod diagnostics_shape;
mod error_overlay_example;
mod example_roots;
mod font_families_example;
mod frosted_glass_example;
mod gradient_kinds_example;
mod grid_example;
mod incremental_example;
mod list_animations_example;
mod looping_animations_example;
mod navigation_example;
mod organic_fusion_example;
mod package_distribution;
mod profiling_example;
mod radio_button_example;
mod registry_http;
mod responsive_example;
mod ripple_example;
mod scroll_snap_example;
mod seed_image_example;
mod seed_scheme_example;
mod self_measurement_example;
mod shape_morph_example;
mod shot;
mod style_states_example;
mod text_input_example;
mod text_wrap_example;
mod theme_transition_example;
mod todo_example;
mod transform_stack_example;
mod workspace_example;
mod zstack_example;
