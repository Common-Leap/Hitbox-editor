// Re-export all modules as a library so integration tests can import them
// This is a thin wrapper around the main binary module structure

pub mod effects;
pub mod particle_renderer;
pub mod particle_renderer_bnsh;
pub mod bnsh_shader_integration;
pub mod bnsh_reflection;
pub mod batch_loader;
pub mod shader_cache;
pub mod bnsh_ffi;
pub mod spirv_to_wgsl;
