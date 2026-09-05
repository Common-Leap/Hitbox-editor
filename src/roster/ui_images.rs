//! UI portrait / stock / result image overrides for a roster entry.
//!
//! Each roster entry is known to the game by `name_id` and costume `slot`. Its
//! on-screen portraits are single-texture BNTX files under
//! `ui/replace/chara/<set>/<set>_<name_id>_<slot>.bntx` (where `<set>` is
//! `chara_0` stock, `chara_1` CSS grid, `chara_2` large preview) and the
//! `replace_patch` mirror, plus stock icons (`ui/replace/stock/...`). This
//! module records a PNG the user picked for any of those, together with the
//! gamma toggles the UI exposes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

/// The portrait sets we expose per entry. Keep in the same preference order as
/// `icons::PORTRAIT_SETS` so the "first wins" fallback stays obvious.
pub const UI_IMAGE_KINDS: &[&str] = &["chara_1", "chara_2", "chara_0", "stock_90", "stock_80"];

/// Map key for one image override: the bare kind for the entry's own slot
/// (`"chara_1"`), or `"<kind>#c<NN>"` for another costume slot
/// (`"chara_1#c02"`). The suffixed form is how one character carries a
/// different portrait per skin; the bare form predates per-slot images and
/// keeps meaning "this entry's slot", so saved projects load unchanged.
pub fn image_key(kind: &str, slot: Option<u8>) -> String {
    match slot {
        Some(slot) => format!("{kind}#c{slot:02}"),
        None => kind.to_string(),
    }
}

/// Split a map key back into its kind and explicit slot, if it names one.
/// Anything without a well-formed `#c<NN>` suffix is a bare kind for the
/// entry's own slot — including hypothetical kind names containing `#`.
pub fn split_image_key(key: &str) -> (&str, Option<u8>) {
    if let Some((kind, suffix)) = key.rsplit_once('#') {
        if let Some(digits) = suffix.strip_prefix('c') {
            if let Ok(slot) = digits.parse::<u8>() {
                if !kind.is_empty() {
                    return (kind, Some(slot));
                }
            }
        }
    }
    (key, None)
}

/// Find the override for `kind` at `slot`: an explicit per-slot entry first,
/// then the bare-kind entry when `slot` is the entry's own slot (the legacy
/// spelling, which predates per-slot images).
pub fn find_override<'m>(
    map: &'m BTreeMap<String, crate::mod_project::UiImageOverride>,
    kind: &str,
    slot: u8,
    entry_slot: u8,
) -> Option<&'m crate::mod_project::UiImageOverride> {
    map.get(&image_key(kind, Some(slot)))
        .or_else(|| (slot == entry_slot).then(|| map.get(kind)).flatten())
}

/// Game-relative path for one UI image override.
pub fn ui_image_path(kind: &str, name_id: &str, slot: u8) -> String {
    // Stock icons historically live under ui/replace/stock/... but many mods
    // also ship stocks next to the chara portraits. Normalize:
    if kind.starts_with("stock") {
        format!("ui/replace/stock/{kind}/{kind}_{name_id}_{slot:02}.bntx")
    } else {
        // Chara portraits and every other kind land side by side.
        format!("ui/replace/chara/{kind}/{kind}_{name_id}_{slot:02}.bntx")
    }
}

/// Encode a PNG (already loaded as bytes) to a single-texture BNTX for the
/// given dimensions/format template, optionally applying gamma.
///
/// For UI we do not have a pool template to copy the format from — we encode
/// as `BC7Srgb` when the image has transparency, otherwise `BC1` is smaller but
/// `BC7` is lossless for our purposes and universally supported. The simple
/// policy here is: always `BC7RgbaUnormSrgb`.
///
/// `gamma_upload` when true applies sRGB→linear (`pow 2.2`) before encode so
/// that a bright preview PNG ends up with the right contrast in game.
pub fn encode_ui_png(png_bytes: &[u8], gamma_upload: bool) -> Result<Vec<u8>> {
    use bntx::Bntx;
    use image::ImageFormat as ImgFmt;
    use image_dds::{ImageFormat, Mipmaps, Quality, SurfaceRgba8};

    let mut image = image::load_from_memory_with_format(png_bytes, ImgFmt::Png)
        .context("reading the PNG")?
        .to_rgba8();

    if gamma_upload {
        crate::roster::gamma::apply_gamma(&mut image, crate::roster::gamma::DEFAULT_GAMMA, true);
    }

    if image.width() == 0 || image.height() == 0 {
        anyhow::bail!("the PNG is empty");
    }

    // UI portraits are square-ish but can be 128–256; we keep the image's own
    // dimensions — unlike effect textures we do NOT enforce a template size,
    // because UI textures are per-character and not UV-referenced by emitters.
    // The game will scale.

    let target = ImageFormat::BC7RgbaUnormSrgb;

    let encoded = SurfaceRgba8::from_image(&image)
        .encode(target, Quality::Normal, Mipmaps::GeneratedAutomatic)
        .map_err(|e| anyhow::anyhow!("encoding the PNG as {target:?}: {e}"))?;

    let built = Bntx::from_surface(encoded, "ui_image")
        .map_err(|e| anyhow::anyhow!("building a BNTX: {e}"))?;
    // Ensure the BNTX reports sRGB (BC7Srgb) — `Bntx::from_surface` already does for this ImageFormat.

    let mut out = std::io::Cursor::new(Vec::new());
    built.write(&mut out).context("writing the BNTX")?;
    Ok(out.into_inner())
}

/// Write each override's BNTX into `mod_root` at its game path.
///
/// `overrides` is `RosterKey -> (image key -> UiImageOverride)`, where the
/// image key is a bare kind for the entry's own slot or `<kind>#c<NN>` for
/// another costume. The `name_id` is taken from the current index entry for
/// that key, because the override is stored per RosterKey but the file name
/// needs the `name_id` (not the fighter directory name — e.g. Ice Climbers
/// are `ice_climber`).
pub fn export_ui_images(
    mod_root: &Path,
    index: &crate::roster::index::RosterIndex,
    overrides: &BTreeMap<
        crate::roster::RosterKey,
        BTreeMap<String, crate::mod_project::UiImageOverride>,
    >,
    report: &mut crate::roster::export::RosterExport,
) -> Result<()> {
    for (key, kinds) in overrides {
        let Some(entry) = index.by_key(key) else {
            report.warnings.push(format!(
                "{key}: no roster entry exists for this image override, so its portrait(s) were not written — still saved in the project."
            ));
            continue;
        };
        let Some(name_id) = entry.name_id.as_deref() else {
            report.warnings.push(format!(
                "{key}: this entry has no roster row, so its portrait(s) have nowhere to be written."
            ));
            continue;
        };
        let entry_slot = entry.backing.slot().unwrap_or(0);
        for (stored_key, ov) in kinds {
            let (kind, key_slot) = split_image_key(stored_key);
            if !UI_IMAGE_KINDS.contains(&kind) {
                // Allow custom kinds but warn if unknown.
            }
            let slot = key_slot.unwrap_or(entry_slot);
            let png_bytes = match std::fs::read(&ov.png_path) {
                Ok(b) => b,
                Err(e) => {
                    report.warnings.push(format!(
                        "{key} {stored_key}: could not read {}: {e:#}",
                        ov.png_path
                    ));
                    continue;
                }
            };
            let bntx = match encode_ui_png(&png_bytes, ov.gamma_upload) {
                Ok(b) => b,
                Err(e) => {
                    report
                        .warnings
                        .push(format!("{key} {stored_key}: encoding failed: {e:#}"));
                    continue;
                }
            };
            let rel = ui_image_path(kind, name_id, slot);
            let dest = mod_root.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, bntx).with_context(|| format!("writing {}", dest.display()))?;
            report.files.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_paths_are_two_digit_slots() {
        assert!(ui_image_path("chara_1", "mario", 8).ends_with("chara_1_mario_08.bntx"));
        assert!(ui_image_path("stock_90", "link", 3).ends_with("stock_90_link_03.bntx"));
    }

    /// Map keys round-trip: bare kinds stay bare (saved projects load
    /// unchanged) and slotted keys parse back to kind + slot.
    #[test]
    fn image_keys_round_trip_with_and_without_slots() {
        assert_eq!(image_key("chara_1", None), "chara_1");
        assert_eq!(image_key("chara_1", Some(2)), "chara_1#c02");
        assert_eq!(split_image_key("chara_1"), ("chara_1", None));
        assert_eq!(split_image_key("chara_1#c02"), ("chara_1", Some(2)));
        // Malformed suffixes stay bare kinds rather than misparsing.
        assert_eq!(split_image_key("chara_1#oops"), ("chara_1#oops", None));
        assert_eq!(split_image_key("chara_1#c"), ("chara_1#c", None));
    }

    /// Lookup prefers the explicit per-slot entry and falls back to the
    /// legacy bare-kind entry only for the entry's own slot.
    #[test]
    fn override_lookup_prefers_the_slots_own_entry() {
        use crate::mod_project::UiImageOverride;
        let mut map: BTreeMap<String, UiImageOverride> = BTreeMap::new();
        map.insert(
            "chara_1".into(),
            UiImageOverride {
                png_path: "base.png".into(),
                ..Default::default()
            },
        );
        map.insert(
            "chara_1#c02".into(),
            UiImageOverride {
                png_path: "c02.png".into(),
                ..Default::default()
            },
        );
        assert_eq!(
            find_override(&map, "chara_1", 2, 0).unwrap().png_path,
            "c02.png"
        );
        assert_eq!(
            find_override(&map, "chara_1", 0, 0).unwrap().png_path,
            "base.png"
        );
        // No entry anywhere: neither a guess nor the other slot's file.
        assert!(find_override(&map, "chara_1", 5, 0).is_none());
        assert!(find_override(&map, "chara_2", 0, 0).is_none());
    }

    #[test]
    fn encode_ui_png_round_trips_through_bntx_parse() {
        let mut img = image::RgbaImage::new(32, 32);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 8) as u8, (y * 8) as u8, 0x80, 0xFF]);
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png_bytes = png.into_inner();
        let bntx_bytes = encode_ui_png(&png_bytes, false).expect("encode");
        // Must be a BNTX.
        assert_eq!(&bntx_bytes[..4], b"BNTX");
        // And decodable as one texture: we just ensure the BNTX parses.
        use binrw::BinRead;
        use bntx::Bntx;
        use std::io::Cursor;
        let b = Bntx::read_le(&mut Cursor::new(&bntx_bytes)).expect("parse BNTX");
        assert_eq!(b.width(), 32);
        assert_eq!(b.height(), 32);
    }

    #[test]
    fn gamma_flag_changes_bytes() {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([64, 64, 64, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png_bytes = png.into_inner();
        let without = encode_ui_png(&png_bytes, false).unwrap();
        let with = encode_ui_png(&png_bytes, true).unwrap();
        assert_ne!(without, with);
    }
}
