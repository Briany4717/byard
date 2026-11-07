//! Every integration test of `byard-compiler`, in one binary.
//!
//! One file per subject, as before, but compiled and linked once: each
//! `tests/*.rs` used to be its own executable linking the whole engine, and
//! relinking all of them was most of the time an edit spent before a test
//! could run.

mod accessibility;
mod action_if;
mod all_events;
mod anchored_overlay;
mod bindable_params;
mod button_defaults;
mod callback_props;
mod canvas_filled_paths;
mod caret_blink;
mod checker_diagnostics;
mod clip_path;
mod clock;
mod color_alpha;
mod component_layout;
mod composed_controls;
mod control_grow;
mod controller_boundary;
mod disabled_gate;
mod ecosystem_vector_wrapper;
mod element_measure;
mod event_exposure;
mod focus_follows_element;
mod font_prop;
mod font_registration;
mod format_and_slice;
mod format_date;
mod fuzz;
mod gradient_kinds;
mod group_opacity;
mod hello_world;
mod hot_reload_state;
mod http_capability;
mod ime_input;
mod incremental_paths;
mod input_hit_test;
mod instrumentation;
mod keyboard_focus;
mod lambda_items;
mod let_functions;
mod let_in_actions;
mod list_reductions;
mod modal_overlay;
mod native_view_async;
mod native_view_element;
mod native_view_keyboard;
mod navigation;
mod numbers_from_text;
mod on_change;
mod or_else;
mod overlay_dismiss;
mod path_from_data;
mod path_morph;
mod path_morph_resample;
mod record_shape;
mod record_types;
mod responsive_variants;
mod retained_across_views;
mod runner_loop;
mod scope_reuse;
mod store_persistence;
mod system_scheme;
mod text_editing;
mod text_field_events;

mod text_field_style;
mod text_param;
mod text_transform;
mod text_weight;
mod theme_motion_and_roles;
mod theme_swap;
mod timer_effects;
mod token_transitions;
mod top_level_fn;
mod transform_hit_test;
mod typed_lambda_params;
mod unknown_method;
mod url_encoded;

mod unknown_name;
mod user_views;
mod variations;
mod watcher_files;
