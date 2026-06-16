use std::sync::OnceLock;

pub fn fx_debug_enabled() -> bool {
    static FX_DEBUG: OnceLock<bool> = OnceLock::new();
    *FX_DEBUG.get_or_init(|| std::env::var("FX_DEBUG").is_ok())
}

/// Native NVN fragment colour chain is the default.
/// Opt out with `FX_PATCHED_FS=1` or `FX_NATIVE_FS=0`.
pub fn fx_native_fs_enabled() -> bool {
    #[cfg(test)]
    {
        return match std::env::var("FX_NATIVE_FS").as_deref() {
            Ok("0") | Ok("false") | Ok("no") => false,
            Ok(_) => true,
            Err(_) => !matches!(
                std::env::var("FX_PATCHED_FS").as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            ),
        };
    }
    #[cfg(not(test))]
    {
        static FX_NATIVE_FS: OnceLock<bool> = OnceLock::new();
        *FX_NATIVE_FS.get_or_init(|| match std::env::var("FX_NATIVE_FS").as_deref() {
            Ok("0") | Ok("false") | Ok("no") => false,
            Ok(_) => true,
            Err(_) => !matches!(
                std::env::var("FX_PATCHED_FS").as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            ),
        })
    }
}

/// Keep the decoded NVN vertex position chain (no billboard override).
pub fn fx_native_vs_pos_enabled() -> bool {
    #[cfg(test)]
    {
        return std::env::var("FX_NATIVE_VS_POS").is_ok();
    }
    #[cfg(not(test))]
    {
        static FX_NATIVE_VS_POS: OnceLock<bool> = OnceLock::new();
        *FX_NATIVE_VS_POS.get_or_init(|| std::env::var("FX_NATIVE_VS_POS").is_ok())
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
