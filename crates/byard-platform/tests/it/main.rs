//! Every integration test of `byard-platform`, in one binary.
//!
//! One file per subject, as before, but compiled and linked once: each
//! `tests/*.rs` used to be its own executable linking the whole engine, and
//! relinking all of them was most of the time an edit spent before a test
//! could run.

mod backdrop_readback;
mod canvas_fill_readback;
mod canvas_shape_readback;
mod clip_mask_readback;
mod clip_path_readback;
mod colour_truth_readback;
mod corner_smoothing_readback;
mod font_families_example_readback;
mod font_family_readback;
mod frame_budget;
mod gradient_kinds_readback;
mod group_opacity_example_readback;
mod group_opacity_readback;
mod late_mark_keeps_dirty_bits;
mod looping_animation_readback;
mod nav_transition_readback;
mod paint_details_readback;
mod pipeline_registry;
mod render_readback;
mod retained_layout_parity;
mod ripple_readback;
mod rotated_clip_readback;
mod text_colour_readback;
mod text_transform_readback;
mod text_weight_readback;
mod z_order_readback;
