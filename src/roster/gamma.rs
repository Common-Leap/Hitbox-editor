//! Gamma correction for UI textures.
//!
//! UI portraits are stored as BNTX (often BC7Srgb). When decoded to RGBA8 via
//! `bntx`/`image_dds`, the bytes may be linear or sRGB depending on the format,
//! but `egui` expects sRGB. A texture that looks too dark or too bright is
//! typically a gamma mismatch. These helpers apply a simple power-law correction
//! so the preview and the encoded upload can be toggled independently.

use image::RgbaImage;

/// Default gamma used for the UI toggle. 2.2 is the sRGB approximation; the
/// exact sRGB transfer function is piecewise but the power law is what most
/// editors expose as "gamma".
pub const DEFAULT_GAMMA: f32 = 2.2;

/// Apply a power-law gamma correction in place.
///
/// `gamma > 1.0` brightens mid-tones (linear → sRGB: `pow(1/gamma)`),
/// `gamma < 1.0` darkens. Only RGB channels are touched; alpha is preserved.
///
/// `out = 255 * (in/255) ^ (1/gamma)` when `inverse=false` is the usual
/// "decode" (brighten), while `inverse=true` does `pow(gamma)` (darken/linearise).
pub fn apply_gamma(image: &mut RgbaImage, gamma: f32, inverse: bool) {
    if (gamma - 1.0).abs() < f32::EPSILON {
        return;
    }
    let p = if inverse { gamma } else { 1.0 / gamma };
    for px in image.pixels_mut() {
        for c in 0..3 {
            let v = px[c] as f32 / 255.0;
            // Clamp to avoid NaN for 0^.
            let corrected = v.powf(p);
            px[c] = (corrected * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Return a gamma-corrected clone, leaving the original untouched.
pub fn gamma_corrected(image: &RgbaImage, gamma: f32, inverse: bool) -> RgbaImage {
    let mut out = image.clone();
    apply_gamma(&mut out, gamma, inverse);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn gamma_is_invertible() {
        let mut img = RgbaImage::from_pixel(2, 2, Rgba([64, 128, 192, 255]));
        let original = img.clone();
        apply_gamma(&mut img, DEFAULT_GAMMA, false);
        apply_gamma(&mut img, DEFAULT_GAMMA, true);
        for (a, b) in img.pixels().zip(original.pixels()) {
            for c in 0..3 {
                assert!((a[c] as i16 - b[c] as i16).abs() <= 1, "{a:?} vs {b:?}");
            }
            assert_eq!(a[3], b[3]);
        }
    }

    #[test]
    fn gamma_brightens_dark_midtones() {
        let img = RgbaImage::from_pixel(1, 1, Rgba([64, 64, 64, 255]));
        let bright = gamma_corrected(&img, DEFAULT_GAMMA, false);
        assert!(bright.get_pixel(0, 0)[0] > 64);
        let dark = gamma_corrected(&img, DEFAULT_GAMMA, true);
        assert!(dark.get_pixel(0, 0)[0] < 64);
    }

    #[test]
    fn alpha_is_preserved() {
        let mut img = RgbaImage::from_pixel(1, 1, Rgba([100, 100, 100, 42]));
        apply_gamma(&mut img, 2.2, false);
        assert_eq!(img.get_pixel(0, 0)[3], 42);
    }
}
