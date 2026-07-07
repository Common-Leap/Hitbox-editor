use std::sync::OnceLock;

pub fn fx_debug_enabled() -> bool {
    static FX_DEBUG: OnceLock<bool> = OnceLock::new();
    *FX_DEBUG.get_or_init(|| std::env::var("FX_DEBUG").is_ok())
}

/// Verbose editor viewport logging (`prepare` / `finish_prepare` / `paint` each frame).
pub fn fx_viewport_log_enabled() -> bool {
    static FX_VIEWPORT_LOG: OnceLock<bool> = OnceLock::new();
    *FX_VIEWPORT_LOG.get_or_init(|| std::env::var("FX_VIEWPORT_LOG").is_ok())
}

/// Native NVN fragment colour chain is the default.
/// Opt out with `FX_PATCHED_FS=1` or `FX_NATIVE_FS=0`.
fn read_native_fs_from_env() -> bool {
    match std::env::var("FX_NATIVE_FS").as_deref() {
        Ok("0") | Ok("false") | Ok("no") => false,
        Ok(_) => true,
        Err(_) => !matches!(
            std::env::var("FX_PATCHED_FS").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        ),
    }
}

pub fn fx_native_fs_enabled() -> bool {
    // Lib unit tests and debug integration tests must observe per-test env toggles.
    #[cfg(any(test, debug_assertions))]
    {
        return read_native_fs_from_env();
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        static FX_NATIVE_FS: OnceLock<bool> = OnceLock::new();
        *FX_NATIVE_FS.get_or_init(read_native_fs_from_env)
    }
}

/// Force WGSL rewrite that disables the NVN FS life gate (`gpr <= cbuf_9[94].z`).
/// Default off: [`crate::nvn_chain::force_hybrid_billboard_cbuf_defaults`] fills slot 94
/// with a large negative `.z` so the gate stays open without patching the shader.
/// Set `FX_NEUTRALIZE_FS_LIFE_DISCARD=1` only when debugging shaders that still discard.
fn read_neutralize_fs_life_discard_from_env() -> bool {
    matches!(
        std::env::var("FX_NEUTRALIZE_FS_LIFE_DISCARD").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

pub fn fx_neutralize_fs_life_discard_enabled() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        return read_neutralize_fs_life_discard_from_env();
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        static FX_NEUTRALIZE_FS_LIFE_DISCARD: OnceLock<bool> = OnceLock::new();
        *FX_NEUTRALIZE_FS_LIFE_DISCARD.get_or_init(read_neutralize_fs_life_discard_from_env)
    }
}

/// Feed the native VS/FS life chain a frame-based clock (birth = CLOCK − age,
/// lifetime = p.lifetime, cbuf_10[2].x = CLOCK) instead of the legacy normalized life_t.
/// Without this the game's `cull when age >= trunc(lifetime)` mis-culls nearly every
/// fragment (see task #22). Default ON; opt out with `FX_FRAME_CLOCK=0`.
fn read_frame_clock_from_env() -> bool {
    !matches!(
        std::env::var("FX_FRAME_CLOCK").as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    )
}

pub fn fx_frame_clock_enabled() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        return read_frame_clock_from_env();
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        static FX_FRAME_CLOCK: OnceLock<bool> = OnceLock::new();
        *FX_FRAME_CLOCK.get_or_init(read_frame_clock_from_env)
    }
}

/// Decode BC5 colour textures using the BNTX channel swizzle (RGB←R, A←G for swizzle
/// 0x03020202) instead of the fixed G→brightness / R→alpha guess, which put luminance into
/// alpha and rendered alpha-blended smoke as an opaque white band. Default ON;
/// `FX_BC5_SWIZZLE_FIX=0` restores the legacy mapping.
fn read_bc5_swizzle_fix_from_env() -> bool {
    !matches!(
        std::env::var("FX_BC5_SWIZZLE_FIX").as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    )
}

pub fn fx_bc5_swizzle_fix_enabled() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        return read_bc5_swizzle_fix_from_env();
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        static FX_BC5_SWIZZLE_FIX: OnceLock<bool> = OnceLock::new();
        *FX_BC5_SWIZZLE_FIX.get_or_init(read_bc5_swizzle_fix_from_env)
    }
}

/// Soft-particle scene-depth fade: opt-in with `FX_SOFT_PARTICLE=1` until the fade
/// math is capture-validated (it currently suppresses most of the effect body when a
/// real scene depth is bound in the live viewport).
pub fn fx_soft_particle_enabled() -> bool {
    matches!(
        std::env::var("FX_SOFT_PARTICLE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn read_hdr_composite_from_env() -> bool {
    match std::env::var("FX_HDR_COMPOSITE").as_deref() {
        Ok("0") | Ok("false") | Ok("no") => false,
        _ => true,
    }
}

/// Accumulate live-viewport particles in an RGBA16F offscreen target and tonemap on
/// composite, so additive fire doesn't clamp to white in the 8-bit surface (the game
/// renders effects into an HDR scene buffer and tonemaps afterwards).
/// Opt out with `FX_HDR_COMPOSITE=0` to draw particles directly into the surface pass.
pub fn fx_hdr_composite_enabled() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        return read_hdr_composite_from_env();
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        static FX_HDR_COMPOSITE: OnceLock<bool> = OnceLock::new();
        *FX_HDR_COMPOSITE.get_or_init(read_hdr_composite_from_env)
    }
}

/// Use the decoded NVN vertex position chain when the shader family is fully wired.
/// Opt out with `FX_NATIVE_VS_POS=0` to force the CPU billboard VP override fallback.
fn read_native_vs_pos_from_env(default_when_unset: bool) -> bool {
    match std::env::var("FX_NATIVE_VS_POS").as_deref() {
        Ok("0") | Ok("false") | Ok("no") => false,
        Ok("1") | Ok("true") | Ok("yes") => true,
        Ok(_) => true,
        Err(_) => default_when_unset,
    }
}

pub fn fx_native_vs_pos_enabled() -> bool {
    // Lib unit tests require explicit FX_NATIVE_VS_POS=1 (no accidental native clip).
    #[cfg(test)]
    {
        return read_native_vs_pos_from_env(false);
    }
    // Integration tests and debug builds: match production default (native on unless opted out).
    #[cfg(all(not(test), debug_assertions))]
    {
        return read_native_vs_pos_from_env(true);
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        static FX_NATIVE_VS_POS: OnceLock<bool> = OnceLock::new();
        *FX_NATIVE_VS_POS.get_or_init(|| read_native_vs_pos_from_env(true))
    }
}

/// Primitive billboard mode: one quad per mesh triangle (default).
/// Set `FX_PRIM_SILHOUETTE=1` for the legacy multi-quad silhouette approximation.
pub fn fx_prim_per_triangle_enabled() -> bool {
    static FX_PRIM_PER_TRI: OnceLock<bool> = OnceLock::new();
    *FX_PRIM_PER_TRI.get_or_init(|| {
        if matches!(
            std::env::var("FX_PRIM_SILHOUETTE").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        ) {
            return false;
        }
        !matches!(
            std::env::var("FX_PRIM_PER_TRI").as_deref(),
            Ok("0") | Ok("false") | Ok("no")
        )
    })
}
