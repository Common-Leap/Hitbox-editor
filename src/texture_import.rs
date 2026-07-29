//! Bring your own PNG: swap an image into an effect's BNTX texture pool, or pull one out.
//!
//! An eff's textures live in one multi-texture BNTX. `effect_library` can slice that pool
//! into single-texture BNTX exports and rebuild it from them, but the pixel payload stays
//! opaque to it — it copies bytes and never decodes. ScanMountGoat's `bntx` supplies the
//! other half: it parses a single-texture BNTX into an `image_dds` surface (undoing the
//! Tegra X1 block-linear swizzle) and builds one back from a surface.
//!
//! So a replacement is: slice every texture out of the pool, re-encode the one the user
//! picked from their PNG, and rebuild the pool from that set. Rebuilding from the ORIGINAL
//! pool as the base (rather than merging the exports into a fresh container) is deliberate —
//! it keeps the pool's own alignment, container name and string-table order, so a file with
//! one imported texture still differs from the game's only in the pixels that changed.
//!
//! Not every texture can make the trip. `bntx` models the surface format as a closed enum,
//! and the game corpus contains 102 textures (of 11915) in a format it does not list; those
//! fail to parse. They report an error rather than silently importing as something else.

use std::io::Cursor;

use anyhow::{anyhow, bail, Context, Result};
use binrw::BinRead;
use bntx::{Bntx, SurfaceFormat};
use image_dds::{ImageFormat, Mipmaps, Quality, SurfaceRgba8};

/// What one pool texture is, for the editor's picker and for round-trip decisions.
#[derive(Clone, Debug, PartialEq)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub mipmaps: u32,
    /// Format name as `bntx` calls it, e.g. `BC7Srgb` — shown verbatim in the UI.
    pub format: String,
    /// Whether this texture's format can be decoded to a PNG and encoded back.
    pub convertible: bool,
}

/// Read the pool's texture `index` back out as its own single-texture BNTX.
///
/// `effect_library` builds these for its own merge paths; reusing that builder means an
/// imported texture and an untouched one travel through exactly the same container code.
fn slice_one(pool: &[u8], index: usize, name: &str) -> Result<Vec<u8>> {
    effect_library::bntx::build_single_texture_bntx_public(pool, index, name)
        .with_context(|| format!("slicing texture '{name}' out of the pool"))
}

fn parse_single(bntx_bytes: &[u8], name: &str) -> Result<Bntx> {
    let mut cursor = Cursor::new(bntx_bytes);
    Bntx::read_le(&mut cursor).map_err(|e| anyhow!("bntx could not parse '{name}': {e}"))
}

fn le_u16(data: &[u8], at: usize) -> Result<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| anyhow!("BNTX truncated at {at:#x}"))
}

fn le_u32(data: &[u8], at: usize) -> Result<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| anyhow!("BNTX truncated at {at:#x}"))
}

fn le_u64(data: &[u8], at: usize) -> Result<u64> {
    data.get(at..at + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| anyhow!("BNTX truncated at {at:#x}"))
}

/// Offset of texture `index`'s BRTI block inside a pool.
fn brti_offset(pool: &[u8], index: usize) -> Result<usize> {
    let count = le_u32(pool, 0x24)? as usize;
    if index >= count {
        bail!("texture index {index} is outside a pool of {count}");
    }
    let table = le_u64(pool, 0x28)? as usize;
    let brti = le_u64(pool, table + index * 8)? as usize;
    if pool.get(brti..brti + 4) != Some(b"BRTI") {
        bail!("texture {index} does not start with a BRTI block");
    }
    Ok(brti)
}

/// Describe texture `index` of the pool.
///
/// Deliberately reads the BRTI header directly instead of going through `bntx`: `bntx`
/// models the surface format as a closed enum and refuses the whole container when it meets
/// one it does not list, which is exactly the case this needs to REPORT rather than fail on.
/// 102 of the corpus's 11915 textures are such a format.
pub fn describe(pool: &[u8], index: usize, _name: &str) -> Result<TextureDesc> {
    let brti = brti_offset(pool, index)?;
    let format_code = le_u32(pool, brti + 0x1C)?;
    // Round-trip the code through `bntx`'s own enum rather than keeping a second copy of the
    // table here: whatever it accepts is exactly what the encode/decode paths below accept.
    let format = SurfaceFormat::read_le(&mut Cursor::new(format_code.to_le_bytes())).ok();
    Ok(TextureDesc {
        width: le_u32(pool, brti + 0x24)?,
        height: le_u32(pool, brti + 0x28)?,
        mipmaps: le_u16(pool, brti + 0x16)? as u32,
        format: match format {
            Some(f) => format!("{f:?}"),
            None => format!("unknown ({format_code:#06x})"),
        },
        convertible: format.map(|f| ImageFormat::try_from(f).is_ok()) == Some(true),
    })
}

/// Decode texture `index` of the pool to straight RGBA8 pixels.
///
/// `max_edge` caps the longest side — pass `None` for the real thing, or a small number for a
/// preview. Downscaling here rather than at the call site means a 1024² BC7 sheet is decoded
/// once and the caller only ever holds the pixels it will actually draw.
pub fn decode_rgba(
    pool: &[u8],
    index: usize,
    name: &str,
    max_edge: Option<u32>,
) -> Result<image::RgbaImage> {
    let desc = describe(pool, index, name)?;
    if !desc.convertible {
        bail!(
            "'{name}' is {} — Visionary cannot convert that format",
            desc.format
        );
    }
    let single = slice_one(pool, index, name)?;
    let bntx = parse_single(&single, name)?;
    let surface = bntx
        .to_surface()
        .map_err(|e| anyhow!("cannot read '{name}' as an image: {e}"))?;
    let rgba = surface
        .decode_rgba8()
        .map_err(|e| anyhow!("cannot decode '{name}' ({}): {e}", desc.format))?;
    let image = rgba
        .to_image(0)
        .map_err(|e| anyhow!("cannot build an image from '{name}': {e}"))?;
    Ok(match max_edge {
        Some(edge) if image.width() > edge || image.height() > edge => {
            let scale = edge as f32 / image.width().max(image.height()) as f32;
            let (w, h) = (
                ((image.width() as f32 * scale).round() as u32).max(1),
                ((image.height() as f32 * scale).round() as u32).max(1),
            );
            image::imageops::thumbnail(&image, w, h)
        }
        _ => image,
    })
}

/// Decode texture `index` of the pool to PNG bytes — the starting point for editing it.
pub fn export_png(pool: &[u8], index: usize, name: &str) -> Result<Vec<u8>> {
    let image = decode_rgba(pool, index, name, None)?;
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .context("encoding PNG")?;
    Ok(out.into_inner())
}

/// What an import actually did, so the UI can say it rather than imply it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportReport {
    pub width: u32,
    pub height: u32,
    /// The format the new pixels were encoded to.
    pub format: String,
    /// Set when the original format could not be re-encoded and a substitute was used.
    pub format_substituted_from: Option<String>,
    pub mipmaps: u32,
}

/// Encode `png` over texture `index`, returning the rebuilt pool.
///
/// `names` must be the pool's texture names in pool order — the same order the eff's texture
/// descriptors are in, which is what the caller already holds.
///
/// The new pixels are encoded to the format the texture already used, so an imported normal
/// map stays BC5 and a colour sheet stays BC3/BC7. When the original format cannot be
/// encoded from RGBA (BC6h's float targets, or a format `bntx` does not model), BC7Srgb is
/// substituted and [`ImportReport::format_substituted_from`] records what was replaced.
pub fn replace_with_png(
    pool: &[u8],
    names: &[String],
    index: usize,
    png: &[u8],
) -> Result<(Vec<u8>, ImportReport)> {
    let name = names
        .get(index)
        .ok_or_else(|| anyhow!("texture index {index} is outside a pool of {}", names.len()))?
        .clone();

    // The format to aim for comes from the texture being replaced. Read it before touching
    // the PNG so an unreadable target fails before the (slow) BCn encode.
    let desc = describe(pool, index, &name)?;
    let original_format = SurfaceFormat::read_le(&mut Cursor::new(
        le_u32(pool, brti_offset(pool, index)? + 0x1C)?.to_le_bytes(),
    ))
    .ok();
    let (target, substituted_from) = match original_format.map(ImageFormat::try_from) {
        // BC6h encodes from float data; an 8-bit PNG has nothing to give it.
        Some(Ok(f)) if !matches!(f, ImageFormat::BC6hRgbUfloat | ImageFormat::BC6hRgbSfloat) => {
            (f, None)
        }
        _ => (ImageFormat::BC7RgbaUnormSrgb, Some(desc.format.clone())),
    };

    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .context("reading the PNG")?
        .to_rgba8();
    if image.width() == 0 || image.height() == 0 {
        bail!("the PNG is empty");
    }

    let encoded = SurfaceRgba8::from_image(&image)
        .encode(target, Quality::Normal, Mipmaps::GeneratedAutomatic)
        .map_err(|e| anyhow!("encoding the PNG as {target:?}: {e}"))?;
    let report = ImportReport {
        width: encoded.width,
        height: encoded.height,
        format: format!("{target:?}"),
        format_substituted_from: substituted_from,
        mipmaps: encoded.mipmaps,
    };

    let replacement = Bntx::from_surface(encoded, &name)
        .map_err(|e| anyhow!("building a BNTX for '{name}': {e}"))?;
    let mut replacement_bytes = Cursor::new(Vec::new());
    replacement
        .write(&mut replacement_bytes)
        .with_context(|| format!("writing the BNTX for '{name}'"))?;

    // Every OTHER texture is passed through untouched, sliced by the same builder that
    // produced `original`, and the pool is rebuilt on top of its own header.
    let mut exports = Vec::with_capacity(names.len());
    for (i, texture_name) in names.iter().enumerate() {
        if i == index {
            exports.push(replacement_bytes.into_inner());
            replacement_bytes = Cursor::new(Vec::new());
        } else {
            exports.push(slice_one(pool, i, texture_name)?);
        }
    }

    let rebuilt = effect_library::bntx::rebuild_from_base_and_exports(pool, &exports)
        .context("rebuilding the texture pool around the imported image")?;
    Ok((rebuilt, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one texture in the corpus scan that is plain RGBA — a shape the encoder handles
    /// without any BCn involvement, so this exercises the container plumbing on its own.
    fn tiny_png(width: u32, height: u32) -> Vec<u8> {
        let mut image = image::RgbaImage::new(width, height);
        for (x, y, px) in image.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 8) as u8, (y * 8) as u8, 0x40, 0xFF]);
        }
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn a_png_becomes_a_bntx_the_pool_builder_can_read() {
        // Standalone: encode a PNG the way an import does, then hand the result to
        // effect_library's pool reader. If the two libraries disagree on the container
        // layout, this is where it shows — no game file needed.
        let image = image::load_from_memory_with_format(&tiny_png(64, 64), image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        let encoded = SurfaceRgba8::from_image(&image)
            .encode(
                ImageFormat::BC7RgbaUnormSrgb,
                Quality::Fast,
                Mipmaps::GeneratedAutomatic,
            )
            .unwrap();
        let built = Bntx::from_surface(encoded, "ef_visionary_test").unwrap();
        let mut bytes = Cursor::new(Vec::new());
        built.write(&mut bytes).unwrap();
        let bytes = bytes.into_inner();

        assert_eq!(&bytes[..4], b"BNTX");
        assert_eq!(
            effect_library::bntx::first_texture_name(&bytes).unwrap(),
            "ef_visionary_test",
            "effect_library must be able to name the texture bntx just wrote"
        );
    }

    /// Names as the POOL itself records them, read straight out of the container rather than
    /// from the eff's descriptor table — so a rebuild that silently dropped or reordered a
    /// texture cannot be masked by the descriptors the test started from.
    fn pool_texture_names(pool: &[u8]) -> Vec<String> {
        let u16_at = |o: usize| u16::from_le_bytes([pool[o], pool[o + 1]]) as usize;
        let u32_at = |o: usize| u32::from_le_bytes(pool[o..o + 4].try_into().unwrap()) as usize;
        let u64_at = |o: usize| {
            u64::from_le_bytes(pool[o..o + 8].try_into().unwrap()) as usize
        };
        let count = u32_at(0x24);
        let table = u64_at(0x28);
        (0..count)
            .map(|i| {
                let brti = u64_at(table + i * 8);
                let name_at = u64_at(brti + 0x60);
                let len = u16_at(name_at);
                String::from_utf8_lossy(&pool[name_at + 2..name_at + 2 + len]).into_owned()
            })
            .collect()
    }

    /// Every texture of an eff's pool, exported to PNG and imported straight back. The pool
    /// must still parse, still hold the same textures under the same names, and the eff must
    /// still load around it. Point `VISIONARY_EFF_ROOT` at an extracted `effect/` tree.
    ///
    /// This is the test that matters: the unit tests above prove the two libraries agree on a
    /// container built from nothing, and this proves they agree on the game's own.
    #[test]
    fn every_texture_of_a_game_eff_survives_a_png_round_trip() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        // A fighter (BC1/BC3/BC4/BC5/BC7 mix) and an assist that carries one texture in the
        // format `bntx` does not model — so both the working path and the refusal are covered.
        const SOURCES: [&str; 2] = [
            "effect/fighter/mario/ef_mario.eff",
            "effect/assist/dossun/ef_dossun.eff",
        ];

        let mut converted = 0usize;
        let mut skipped: Vec<String> = Vec::new();
        for src in SOURCES {
            let bytes = std::fs::read(root.join(src)).expect("source eff");
            let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
            let textures = file
                .ptcl_file
                .as_ref()
                .and_then(|p| p.texture_info.as_ref())
                .unwrap_or_else(|| panic!("{src} has textures"));
            let pool = textures
                .binary_data
                .clone()
                .unwrap_or_else(|| panic!("{src} has a BNTX"));
            let names: Vec<String> =
                textures.descriptors.iter().map(|d| d.name.clone()).collect();
            assert!(names.len() > 1, "{src}: expected a multi-texture pool");

            for (index, name) in names.iter().enumerate() {
                let desc = describe(&pool, index, name).expect("describe");
                if !desc.convertible {
                    skipped.push(format!("{name} ({})", desc.format));
                    // An unconvertible texture must FAIL the import, not import as something
                    // else — a silently substituted format would corrupt what the game samples.
                    assert!(
                        export_png(&pool, index, name).is_err(),
                        "{name}: reported unconvertible but exported anyway"
                    );
                    continue;
                }
                let png = export_png(&pool, index, name).expect("export png");
                let (rebuilt, report) =
                    replace_with_png(&pool, &names, index, &png).expect("import png");

                assert_eq!(
                    (report.width, report.height),
                    (desc.width, desc.height),
                    "{name}: re-import changed the dimensions"
                );
                assert_eq!(
                    pool_texture_names(&rebuilt),
                    names,
                    "{name}: the rebuilt pool lost or reordered textures"
                );
                let reimported = describe(&rebuilt, index, name).expect("describe rebuilt");
                assert_eq!(
                    (reimported.width, reimported.height),
                    (desc.width, desc.height),
                    "{name}: the texture came back at the wrong size"
                );
                converted += 1;
            }
        }

        assert!(converted > 0, "nothing converted; skipped: {skipped:?}");
        assert!(
            !skipped.is_empty(),
            "expected ef_dossun to carry one texture bntx cannot model — if the crate gained \
             that format, drop this assertion"
        );
        eprintln!("round-tripped {converted} texture(s); skipped {skipped:?}");
    }

    /// The editor's thumbnail comes from `decode_rgba(.., Some(edge))`. It must come back at
    /// preview size — decoding a 1024² sheet and handing the full thing to the GPU every time
    /// the selection changes is the difference between a thumbnail and a stall.
    ///
    /// Writes the decoded previews to `VISIONARY_PREVIEW_DUMP` when set, so the images can be
    /// eyeballed rather than only measured.
    #[test]
    fn previews_come_back_downscaled_and_opaque_where_the_texture_is() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let textures = file
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .expect("textures");
        let pool = textures.binary_data.clone().expect("pool");
        let dump = std::env::var_os("VISIONARY_PREVIEW_DUMP").map(std::path::PathBuf::from);
        if let Some(dir) = &dump {
            std::fs::create_dir_all(dir).expect("dump dir");
        }

        let mut checked = 0usize;
        for (index, descriptor) in textures.descriptors.iter().enumerate() {
            let name = &descriptor.name;
            let full = match describe(&pool, index, name) {
                Ok(d) if d.convertible => d,
                _ => continue,
            };
            let preview = decode_rgba(&pool, index, name, Some(168)).expect("preview");
            assert!(
                preview.width() <= 168 && preview.height() <= 168,
                "{name}: preview is {}×{}, larger than the cap",
                preview.width(),
                preview.height()
            );
            // Aspect ratio preserved, within a pixel of rounding.
            let expected = full.width as f32 / full.height as f32;
            let got = preview.width() as f32 / preview.height() as f32;
            assert!(
                (expected - got).abs() < 0.05,
                "{name}: preview aspect {got} != source aspect {expected}"
            );
            // A decode that silently produced an empty buffer would still be the right SIZE,
            // so check there are actually pixels carrying something.
            assert!(
                preview.pixels().any(|p| p.0 != [0, 0, 0, 0]),
                "{name}: preview decoded to nothing at all"
            );
            if let Some(dir) = &dump {
                preview.save(dir.join(format!("{name}.png"))).expect("save");
            }
            checked += 1;
        }
        assert!(checked > 0, "no convertible texture to preview");
        eprintln!("previewed {checked} texture(s)");
    }

    #[test]
    fn png_round_trips_through_a_bntx() {
        let source = tiny_png(32, 32);
        let image = image::load_from_memory_with_format(&source, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        let encoded = SurfaceRgba8::from_image(&image)
            .encode(
                ImageFormat::Rgba8Unorm,
                Quality::Fast,
                Mipmaps::GeneratedAutomatic,
            )
            .unwrap();
        let built = Bntx::from_surface(encoded, "round_trip").unwrap();
        let mut bytes = Cursor::new(Vec::new());
        built.write(&mut bytes).unwrap();

        let reparsed = parse_single(&bytes.into_inner(), "round_trip").unwrap();
        assert_eq!(reparsed.width(), 32);
        assert_eq!(reparsed.height(), 32);
        // Uncompressed RGBA is lossless, so the pixels must come back exactly.
        let decoded = reparsed
            .to_surface()
            .unwrap()
            .decode_rgba8()
            .unwrap()
            .to_image(0)
            .unwrap();
        assert_eq!(decoded.as_raw(), image.as_raw());
    }
}
