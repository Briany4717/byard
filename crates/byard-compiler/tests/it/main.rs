//! Every integration test of `byard-compiler`, in one binary.
//!
//! One file per subject, as before, but compiled and linked once: each
//! `tests/*.rs` used to be its own executable linking the whole engine, and
//! relinking all of them was most of the time an edit spent before a test
//! could run.

mod action_if;
mod all_events;
mod anchored_overlay;
mod button_defaults;
mod callback_props;
mod canvas_filled_paths;
mod clip_path;
mod controller_boundary;
mod disabled_gate;
mod ecosystem_vector_wrapper;
mod element_measure;
mod event_exposure;
mod font_prop;
mod font_registration;
mod format_and_slice;
mod fuzz;
mod gradient_kinds;
mod group_opacity;
mod hello_world;
mod http_capability;
mod ime_input;
mod incremental_paths;
mod input_hit_test;
mod instrumentation;
mod let_functions;
mod list_reductions;
mod native_view_async;
mod native_view_element;
mod native_view_keyboard;
mod navigation;
mod overlay_dismiss;
mod path_morph;
mod path_morph_resample;
mod responsive_variants;
mod retained_across_views;
mod runner_loop;
mod store_persistence;
mod text_transform;
mod text_weight;
mod timer_effects;
mod token_transitions;
mod top_level_fn;
mod transform_hit_test;
mod unknown_method;
mod user_views;
