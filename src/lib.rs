// Eff-editor branch: intentionally minimal library surface. The particle-rendering test
// harness lives on the `game-accurate-sim` branch; this branch is the effect-file editor
// with in-game live preview (no effect rendering).

pub mod effects;
pub mod batch_loader;
pub mod effect_converter;
pub mod scratch_dirs;
pub mod shader_registry;
pub mod combiner;
pub mod fx_env;
pub use fx_env::{fx_debug_enabled, fx_native_fs_enabled, fx_native_vs_pos_enabled, fx_prim_per_triangle_enabled};
pub mod sphere_volume_tables;
