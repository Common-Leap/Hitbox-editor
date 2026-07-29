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

/// Offset of `comp_sel` within a BRTI block — the channel swizzle.
///
/// One byte per output channel (R, G, B, A), each naming the SOURCE it reads from: 0 zero,
/// 1 one, 2 red, 3 green, 4 blue, 5 alpha.
///
/// This is how a single-channel texture reaches a shader. BC4 stores exactly one channel, and
/// the game's own textures broadcast it to all four — `0x02020202`, "every channel reads red" —
/// so a mask sampled as `texture.a` returns the shape. The identity swizzle `0x05040302` says
/// "alpha comes from alpha", and for BC4 there is no alpha plane, so the hardware returns 1.0
/// for every pixel: the effect draws as a fully opaque square no matter what the image contains.
///
/// So the swizzle is part of what a texture MEANS, not incidental header noise, and an import
/// has to carry the original's over. `bntx` writes the identity swizzle for everything it
/// builds, which is correct only for the four-channel formats.
const BRTI_COMP_SEL: usize = 0x58;

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
        convertible: format
            .and_then(|f| ImageFormat::try_from(f).ok())
            .is_some_and(|f| !is_signed(f)),
    })
}

/// Does this format store SIGNED values?
///
/// Signed formats span −1..1, and every path here goes through 8-bit unsigned RGBA, which cannot
/// represent the negative half. Decoding one and re-encoding it does not round-trip: measured
/// against the corpus, `ef_mario_localcoin00_nor` (BC5Snorm) came back with a mean error of 51
/// levels per channel — a normal map turned into noise.
///
/// So they are reported as unconvertible, which is what they are, rather than importing as
/// something the game will render wrong. This is the same rule the module already applies to
/// formats `bntx` cannot model: refuse rather than silently substitute.
fn is_signed(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::R8Snorm
            | ImageFormat::Rg8Snorm
            | ImageFormat::Rgba8Snorm
            | ImageFormat::BC4RSnorm
            | ImageFormat::BC5RgSnorm
            | ImageFormat::R16Snorm
            | ImageFormat::Rg16Snorm
            | ImageFormat::Rgba16Snorm
    )
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
    Ok(fit(image, max_edge))
}

/// Decode a PNG the user picked, to the same straight RGBA8 the pool decode produces.
///
/// The editor previews an imported texture from ITS OWN file rather than from the pool: the
/// pool still holds the game's original until the carrier is rebuilt, so decoding the pool
/// after an import shows the texture the user just replaced.
pub fn decode_png_rgba(png: &[u8], max_edge: Option<u32>) -> Result<image::RgbaImage> {
    let image = image::load_from_memory(png)
        .context("cannot read that file as an image")?
        .to_rgba8();
    Ok(fit(image, max_edge))
}

/// Cap the longest side, preserving aspect. `None` leaves the image alone.
fn fit(image: image::RgbaImage, max_edge: Option<u32>) -> image::RgbaImage {
    match max_edge {
        Some(edge) if image.width() > edge || image.height() > edge => {
            let scale = edge as f32 / image.width().max(image.height()) as f32;
            let (w, h) = (
                ((image.width() as f32 * scale).round() as u32).max(1),
                ((image.height() as f32 * scale).round() as u32).max(1),
            );
            image::imageops::thumbnail(&image, w, h)
        }
        _ => image,
    }
}

/// RGB channels within this much of each other still count as "one flat colour". BCn decodes a
/// pure-white block back to exactly 255, so this only has to absorb the odd rounding artefact.
const MATTE_RGB_TOLERANCE: u8 = 4;

/// How far brightness and alpha may drift apart and still be considered the same channel.
const MASK_TOLERANCE: u8 = 6;

/// Which form of a texture is being handed around.
///
/// Effect textures are not pictures — they are masks, and the game stores them in whichever
/// packing is cheapest, then relies on the channel swizzle to present them to the shader. Edited
/// in that stored packing they are close to unintelligible: a BC5 mask exports as a red-and-green
/// image whose GREEN channel is secretly the transparency, and a BC3 mask exports as a blank
/// white square whose only content is invisible.
///
/// So everything the user sees or paints goes through [`Form::Editable`], and the raw packing
/// stays available for anyone who needs byte-level control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Form {
    /// Swizzle applied, and anything carrying a single channel of information flattened to an
    /// opaque black-and-white image where black means "nothing is drawn".
    #[default]
    Editable,
    /// The stored channels exactly as the file holds them.
    Raw,
}

/// What a texture actually carries, once the swizzle has been applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    /// One channel of information: brightness and alpha are the same thing. Every BC4 mask.
    Mask,
    /// A flat colour with the shape in alpha. Most BC3 masks — a white field plus a silhouette.
    Matte { color: [u8; 3] },
    /// Fully opaque; everything is in the colour channels. Normal maps and colour sheets.
    Opaque,
    /// Independent colour and alpha, both meaningful. Kept as straight RGBA.
    ColorAlpha,
}

/// Perceived brightness — the shape channel on the way back in.
fn luminance(px: &image::Rgba<u8>) -> u8 {
    ((px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000).min(255) as u8
}

/// The four `comp_sel` bytes for texture `index`: the source each output channel reads from.
fn comp_sel(pool: &[u8], index: usize) -> Result<[u8; 4]> {
    Ok(le_u32(pool, brti_offset(pool, index)? + BRTI_COMP_SEL)?.to_le_bytes())
}

/// Stored channels → what the shader samples.
fn apply_swizzle(image: &image::RgbaImage, sel: [u8; 4]) -> image::RgbaImage {
    let mut out = image.clone();
    for px in out.pixels_mut() {
        let src = *px;
        for (channel, source) in sel.iter().enumerate() {
            px[channel] = match source {
                0 => 0,
                1 => 255,
                2..=5 => src[(source - 2) as usize],
                // Undocumented source: leave the channel as it was rather than invent a value.
                _ => src[channel],
            };
        }
    }
    out
}

/// What the shader samples → stored channels, the inverse of [`apply_swizzle`].
///
/// A stored channel no output reads back is unrecoverable, and gets a neutral value. That is
/// never a loss in practice: those are exactly the channels the format does not encode — BC4
/// keeps only red, BC5 only red and green.
fn unapply_swizzle(image: &image::RgbaImage, sel: [u8; 4]) -> image::RgbaImage {
    let read_by: [Option<usize>; 4] =
        [0, 1, 2, 3].map(|stored| (0..4).find(|&out| sel[out] as usize == stored + 2));
    let mut out = image.clone();
    for px in out.pixels_mut() {
        let sampled = *px;
        for stored in 0..4 {
            px[stored] = match read_by[stored] {
                Some(from) => sampled[from],
                None if stored == 3 => 255,
                None => 0,
            };
        }
    }
    out
}

/// Classify a SAMPLED image — the swizzle must already have been applied.
fn classify(image: &image::RgbaImage) -> Layout {
    let mut lo = [255u8; 3];
    let mut hi = [0u8; 3];
    let mut opaque = true;
    let mut mask_like = true;
    for px in image.pixels() {
        for c in 0..3 {
            lo[c] = lo[c].min(px[c]);
            hi[c] = hi[c].max(px[c]);
        }
        opaque &= px[3] == 255;
        mask_like &= luminance(px).abs_diff(px[3]) <= MASK_TOLERANCE;
    }
    if opaque {
        return Layout::Opaque;
    }
    if mask_like {
        return Layout::Mask;
    }
    if (0..3).all(|c| hi[c].saturating_sub(lo[c]) <= MATTE_RGB_TOLERANCE) {
        return Layout::Matte {
            color: [0, 1, 2].map(|c| ((lo[c] as u16 + hi[c] as u16) / 2) as u8),
        };
    }
    Layout::ColorAlpha
}

/// Sampled pixels → the image the user paints on.
///
/// `Mask` and `Matte` carry one channel of information, and it is written into BOTH the grey
/// level and the alpha. Each does a different job:
///
/// - the grey level is what makes the file editable at all. Alpha alone is invisible in most
///   editors, and a texture that opens as a blank white square is the reason this exists;
/// - the alpha is what makes it preview correctly. Dropped, the file reads as a solid black
///   rectangle everywhere the shape is absent.
///
/// Writing both means the image is right whichever way a tool chooses to read it, and
/// [`from_editable`] accepts an edit to either. `ColorAlpha` already has two independent
/// channels and is passed through untouched.
fn to_editable(image: &image::RgbaImage, layout: Layout) -> image::RgbaImage {
    let mut out = image.clone();
    match layout {
        Layout::Mask | Layout::Matte { .. } => {
            for px in out.pixels_mut() {
                *px = image::Rgba([px[3], px[3], px[3], px[3]]);
            }
        }
        Layout::Opaque => {
            for px in out.pixels_mut() {
                px[3] = 255;
            }
        }
        Layout::ColorAlpha => {}
    }
    out
}

/// The shape a painted mask is asking for: whichever of brightness or alpha says "less".
///
/// [`to_editable`] writes the same value into both, so an untouched export gives them back
/// identical and this is exact. Once edited they can disagree, and taking the minimum is what
/// lets EITHER edit mean what it looks like — painting black erases, and so does erasing. It
/// also absorbs the two ways a paint program mangles this kind of file on save: flattening
/// alpha to opaque (brightness still carries the shape) and compositing colour onto white
/// (alpha still carries it).
fn shape_of(px: &image::Rgba<u8>) -> u8 {
    luminance(px).min(px[3])
}

/// The image the user painted → sampled pixels, the inverse of [`to_editable`].
fn from_editable(image: &image::RgbaImage, layout: Layout) -> image::RgbaImage {
    let mut out = image.clone();
    match layout {
        Layout::Mask => {
            for px in out.pixels_mut() {
                let v = shape_of(px);
                *px = image::Rgba([v, v, v, v]);
            }
        }
        Layout::Matte { color } => {
            for px in out.pixels_mut() {
                *px = image::Rgba([color[0], color[1], color[2], shape_of(px)]);
            }
        }
        Layout::Opaque => {
            for px in out.pixels_mut() {
                px[3] = 255;
            }
        }
        Layout::ColorAlpha => {}
    }
    out
}

/// The layout of texture `index`, and the swizzle needed to get to and from it.
fn layout_of(pool: &[u8], index: usize, name: &str) -> Result<([u8; 4], Layout)> {
    let sel = comp_sel(pool, index)?;
    let stored = decode_rgba(pool, index, name, None)?;
    Ok((sel, classify(&apply_swizzle(&stored, sel))))
}

/// The layout of texture `index`, for the UI to describe what the user is looking at.
pub fn layout_of_public(pool: &[u8], index: usize, name: &str) -> Result<Layout> {
    Ok(layout_of(pool, index, name)?.1)
}

/// The image the PANEL draws for `form`.
///
/// `Editable` is what the shader samples: the swizzle applied, alpha intact. A mask therefore
/// previews as a shape over the checkerboard — you can see the transparency, which is the whole
/// point of looking at it. Flattening it to the paintable matte here would hide exactly the
/// property the preview exists to show.
fn preview_for_form(stored: &image::RgbaImage, sel: [u8; 4], form: Form) -> image::RgbaImage {
    match form {
        Form::Raw => stored.clone(),
        Form::Editable => apply_swizzle(stored, sel),
    }
}

/// The image the EXPORT writes for `form`.
///
/// Same source as [`preview_for_form`] and the same swizzle, but single-channel layouts are
/// flattened to an opaque black-and-white matte. That difference is deliberate: transparency is
/// the right way to LOOK at a mask and the wrong way to PAINT one, because most editors will not
/// let you paint into an alpha channel that has no colour behind it.
fn export_for_form(stored: &image::RgbaImage, sel: [u8; 4], form: Form) -> image::RgbaImage {
    match form {
        Form::Raw => stored.clone(),
        Form::Editable => {
            let sampled = apply_swizzle(stored, sel);
            to_editable(&sampled, classify(&sampled))
        }
    }
}

/// Decode texture `index` as the panel shows it.
pub fn decode_preview(
    pool: &[u8],
    index: usize,
    name: &str,
    form: Form,
    max_edge: Option<u32>,
) -> Result<image::RgbaImage> {
    let stored = decode_rgba(pool, index, name, None)?;
    let sel = comp_sel(pool, index).unwrap_or([2, 3, 4, 5]);
    Ok(fit(preview_for_form(&stored, sel, form), max_edge))
}

/// Decode texture `index` as [`export_png`] writes it.
pub fn decode_form(
    pool: &[u8],
    index: usize,
    name: &str,
    form: Form,
    max_edge: Option<u32>,
) -> Result<image::RgbaImage> {
    let stored = decode_rgba(pool, index, name, None)?;
    let sel = comp_sel(pool, index).unwrap_or([2, 3, 4, 5]);
    Ok(fit(export_for_form(&stored, sel, form), max_edge))
}

/// Decode texture `index` as the GAME samples it: the swizzle applied, nothing flattened.
///
/// The ground truth about what an effect draws, and what the round-trip and erasure tests
/// measure. Identical to the `Editable` preview — named separately because the tests are
/// asserting about the game, not about the panel, and those two must be free to diverge.
#[cfg(test)]
fn decode_sampled(
    pool: &[u8],
    index: usize,
    name: &str,
    max_edge: Option<u32>,
) -> Result<image::RgbaImage> {
    decode_preview(pool, index, name, Form::Editable, max_edge)
}

/// Encode texture `index` to PNG bytes — the starting point for editing it.
///
/// `Form::Editable` writes the image described by [`to_editable`]: black means empty, and
/// painting black erases. [`replace_with_png`] reverses it exactly, so the round trip is
/// lossless. `Form::Raw` writes the stored channels untouched, for anyone who wants them.
pub fn export_png(pool: &[u8], index: usize, name: &str, form: Form) -> Result<Vec<u8>> {
    let image = decode_form(pool, index, name, form, None)?;
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
    /// How the incoming image was read.
    pub form: Form,
    /// What the texture it replaced turned out to carry — `None` when read raw.
    pub layout: Option<Layout>,
}

/// Turn an imported PNG into the STORED channels to encode.
fn stored_from_png(
    pool: &[u8],
    index: usize,
    name: &str,
    image: &image::RgbaImage,
    form: Form,
) -> (image::RgbaImage, Option<Layout>) {
    if form == Form::Raw {
        return (image.clone(), None);
    }
    // A texture whose layout cannot be read is passed through rather than mangled by a guess.
    let Ok((sel, layout)) = layout_of(pool, index, name) else {
        return (image.clone(), None);
    };
    (
        unapply_swizzle(&from_editable(image, layout), sel),
        Some(layout),
    )
}

/// Decode an imported PNG for previewing, in the same form the panel is showing.
///
/// The file is not shown literally — it is put through the import and back out again, so the
/// preview reflects what will actually ship rather than what happens to be on disk. An image
/// whose alpha the import is about to discard shows here without it.
pub fn decode_import_preview(
    pool: &[u8],
    index: usize,
    name: &str,
    png: &[u8],
    form: Form,
    max_edge: Option<u32>,
) -> Result<image::RgbaImage> {
    let image = image::load_from_memory(png)
        .context("cannot read that file as an image")?
        .to_rgba8();
    let (stored, _) = stored_from_png(pool, index, name, &image, form);
    let sel = comp_sel(pool, index).unwrap_or([2, 3, 4, 5]);
    Ok(fit(preview_for_form(&stored, sel, form), max_edge))
}

/// Encode a PNG into a single-texture BNTX matching what texture `template` already is.
///
/// The format, the channel swizzle and the DIMENSIONS all come from the template. Dimensions are
/// a hard requirement rather than a courtesy: emitters carry UV rects and scroll rates sized for
/// the texture they were authored against, so a different-sized image does not "just scale" — it
/// resamples the effect. The template is the texture the new pixels are standing in for, whether
/// that is the one being replaced or the one a new entry is taking over from.
fn encode_like(
    pool: &[u8],
    names: &[String],
    template: usize,
    output_name: &str,
    png: &[u8],
    form: Form,
) -> Result<(Vec<u8>, ImportReport)> {
    let template_name = names.get(template).ok_or_else(|| {
        anyhow!(
            "texture index {template} is outside a pool of {}",
            names.len()
        )
    })?;

    // Read the target's shape before touching the PNG, so an unreadable one fails before the
    // (slow) BCn encode.
    let desc = describe(pool, template, template_name)?;
    let original_format = SurfaceFormat::read_le(&mut Cursor::new(
        le_u32(pool, brti_offset(pool, template)? + 0x1C)?.to_le_bytes(),
    ))
    .ok();
    let (target, substituted_from) = match original_format.map(ImageFormat::try_from) {
        // BC6h encodes from float data; an 8-bit PNG has nothing to give it.
        Some(Ok(f)) if !matches!(f, ImageFormat::BC6hRgbUfloat | ImageFormat::BC6hRgbSfloat) => {
            (f, None)
        }
        _ => (ImageFormat::BC7RgbaUnormSrgb, Some(desc.format.clone())),
    };
    // Only meaningful while the format is unchanged: a substituted BC7 really does have four
    // channels, so the identity swizzle `bntx` writes is the right one for it.
    let keep_comp_sel = substituted_from
        .is_none()
        .then(|| le_u32(pool, brti_offset(pool, template)? + BRTI_COMP_SEL))
        .transpose()?;

    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .context("reading the PNG")?
        .to_rgba8();
    if image.width() == 0 || image.height() == 0 {
        bail!("the PNG is empty");
    }
    if (image.width(), image.height()) != (desc.width, desc.height) {
        bail!(
            "that image is {}×{}, but '{template_name}' is {}×{} — effect textures have to keep \
             their size, because emitters carry UV rects authored against it. Resize the image \
             and try again.",
            image.width(),
            image.height(),
            desc.width,
            desc.height
        );
    }
    // Back to the packing the file uses. Skipping this is what made an edited mask encode as
    // alpha=255 everywhere, so the game drew a solid square instead of the shape.
    let (image, layout) = stored_from_png(pool, template, template_name, &image, form);

    let encoded = SurfaceRgba8::from_image(&image)
        .encode(target, Quality::Normal, Mipmaps::GeneratedAutomatic)
        .map_err(|e| anyhow!("encoding the PNG as {target:?}: {e}"))?;
    let report = ImportReport {
        width: encoded.width,
        height: encoded.height,
        format: format!("{target:?}"),
        format_substituted_from: substituted_from,
        mipmaps: encoded.mipmaps,
        form,
        layout,
    };

    let mut built = Bntx::from_surface(encoded, output_name)
        .map_err(|e| anyhow!("building a BNTX for '{output_name}': {e}"))?;
    if let (Some(comp_sel), Some(brti)) = (keep_comp_sel, built.nx_header.brtis.first_mut()) {
        brti.brti.comp_sel = comp_sel;
    }
    let mut bytes = Cursor::new(Vec::new());
    built
        .write(&mut bytes)
        .with_context(|| format!("writing the BNTX for '{output_name}'"))?;
    Ok((bytes.into_inner(), report))
}

/// Rebuild the pool from one single-texture BNTX per surviving texture.
///
/// `slots` is the pool order to produce: `Some(i)` passes existing texture `i` through untouched,
/// `None` takes the next of `added`. Dropping an index deletes it. The eff serializer re-sorts
/// descriptors by name and reorders the archive to match on save, so this order only has to be
/// self-consistent, not sorted.
fn rebuild(
    pool: &[u8],
    names: &[String],
    slots: &[Option<usize>],
    added: Vec<Vec<u8>>,
) -> Result<Vec<u8>> {
    let mut added = added.into_iter();
    let mut exports = Vec::with_capacity(slots.len());
    for slot in slots {
        match slot {
            Some(i) => exports.push(slice_one(pool, *i, &names[*i])?),
            None => exports.push(
                added
                    .next()
                    .ok_or_else(|| anyhow!("internal: fewer new textures than pool slots"))?,
            ),
        }
    }
    effect_library::bntx::rebuild_from_base_and_exports(pool, &exports)
        .context("rebuilding the texture pool")
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
///
/// The original's channel swizzle is carried over too — see [`BRTI_COMP_SEL`]. Without that a
/// re-encoded single-channel texture is structurally perfect and renders as a solid square.
pub fn replace_with_png(
    pool: &[u8],
    names: &[String],
    index: usize,
    png: &[u8],
    form: Form,
) -> Result<(Vec<u8>, ImportReport)> {
    let name = names
        .get(index)
        .ok_or_else(|| anyhow!("texture index {index} is outside a pool of {}", names.len()))?
        .clone();
    let (replacement, report) = encode_like(pool, names, index, &name, png, form)?;
    // Same slots, same names — only the one texture's payload differs.
    let slots: Vec<Option<usize>> = (0..names.len())
        .map(|i| (i != index).then_some(i))
        .collect();
    Ok((rebuild(pool, names, &slots, vec![replacement])?, report))
}

/// Append a copy of texture `src` under `new_name`, returning the rebuilt pool.
///
/// The point is ISOLATION. A pool texture is shared by every emitter that samples it — and
/// `ef_cmn_*` names are shared by dozens of effects within one eff — so editing one to change a
/// single effect changes all of them. A private copy is the only way to alter one effect's
/// texture and leave the rest alone.
///
/// The copy is byte-identical: it is sliced straight out of the pool and only renamed, so no
/// re-encode and no generation loss.
pub fn duplicate_texture(
    pool: &[u8],
    names: &[String],
    src: usize,
    new_name: &str,
) -> Result<Vec<u8>> {
    if src >= names.len() {
        bail!("texture index {src} is outside a pool of {}", names.len());
    }
    if names.iter().any(|n| n == new_name) {
        bail!("this eff already holds a texture called '{new_name}'");
    }
    let copy = slice_one(pool, src, new_name)?;
    let mut slots: Vec<Option<usize>> = (0..names.len()).map(Some).collect();
    slots.push(None);
    rebuild(pool, names, &slots, vec![copy])
}

/// Append a new texture built from `png`, shaped like texture `template`.
pub fn add_texture_from_png(
    pool: &[u8],
    names: &[String],
    template: usize,
    new_name: &str,
    png: &[u8],
    form: Form,
) -> Result<(Vec<u8>, ImportReport)> {
    if names.iter().any(|n| n == new_name) {
        bail!("this eff already holds a texture called '{new_name}'");
    }
    let (built, report) = encode_like(pool, names, template, new_name, png, form)?;
    let mut slots: Vec<Option<usize>> = (0..names.len()).map(Some).collect();
    slots.push(None);
    Ok((rebuild(pool, names, &slots, vec![built])?, report))
}

/// Drop texture `index` from the pool, returning the rebuilt pool.
///
/// Says nothing about whether anything still samples it — the caller owns that check, because
/// only the caller can see the emitters.
pub fn remove_texture(pool: &[u8], names: &[String], index: usize) -> Result<Vec<u8>> {
    if index >= names.len() {
        bail!("texture index {index} is outside a pool of {}", names.len());
    }
    if names.len() == 1 {
        bail!(
            "'{}' is the only texture in this eff — removing it would leave the pool empty, \
               which is not a shape this writer has been calibrated against",
            names[index]
        );
    }
    let slots: Vec<Option<usize>> = (0..names.len()).filter(|i| *i != index).map(Some).collect();
    rebuild(pool, names, &slots, Vec::new())
}

/// A pool-unique name for a new texture derived from `base`.
///
/// Kept short: these end up in the BNTX string table and the descriptor sort, and the game's own
/// names run to about 24 characters.
pub fn unique_texture_name(existing: &[String], base: &str) -> String {
    // Don't stack suffixes on a name that already has one.
    let stem = match base.rsplit_once("_v") {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => base,
    };
    (2..)
        .map(|n| format!("{stem}_v{n}"))
        .find(|candidate| !existing.iter().any(|n| n == candidate))
        .unwrap_or_else(|| base.to_string())
}

/// A descriptor id no other texture in this eff uses.
///
/// The game's own ids are a 1:1 function of the texture name that nothing in this toolchain can
/// compute — no CRC variant matches and it is not affine over GF(2), so it cannot be reproduced.
/// What the format needs from an id is that emitters and descriptors agree WITHIN the file: the
/// pruner, the transplant merge and `apply_authored` all resolve textures by matching a sampler's
/// `texture_id` to a descriptor `id`, and the eff is self-contained.
///
/// So a new texture gets a derived-but-arbitrary id, probed until it is unique here. If it turns
/// out the runtime recomputes ids from names, a duplicated texture will fail to bind and render
/// as a solid square — the one thing to watch for on the first in-game test.
pub fn unused_descriptor_id(taken: &[u64], name: &str) -> u64 {
    // FNV-1a, so the same name yields the same id run to run and diffs stay readable.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in name.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let mut candidate = hash & 0xFFFF_FFFF;
    while candidate == 0 || taken.contains(&candidate) {
        candidate = (candidate + 1) & 0xFFFF_FFFF;
    }
    candidate
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
        let u64_at = |o: usize| u64::from_le_bytes(pool[o..o + 8].try_into().unwrap()) as usize;
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
            let names: Vec<String> = textures
                .descriptors
                .iter()
                .map(|d| d.name.clone())
                .collect();
            assert!(names.len() > 1, "{src}: expected a multi-texture pool");

            for (index, name) in names.iter().enumerate() {
                let desc = describe(&pool, index, name).expect("describe");
                if !desc.convertible {
                    skipped.push(format!("{name} ({})", desc.format));
                    // An unconvertible texture must FAIL the import, not import as something
                    // else — a silently substituted format would corrupt what the game samples.
                    assert!(
                        export_png(&pool, index, name, Form::Editable).is_err(),
                        "{name}: reported unconvertible but exported anyway"
                    );
                    continue;
                }
                let png = export_png(&pool, index, name, Form::Editable).expect("export png");
                let (rebuilt, report) =
                    replace_with_png(&pool, &names, index, &png, Form::Editable)
                        .expect("import png");

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

    /// An imported texture must keep the original's channel swizzle.
    ///
    /// This is the "it renders as a solid colored square" defect, and it is invisible in the
    /// pixels: the image data round-trips perfectly while the texture still draws wrong. BC4
    /// stores one channel and BC5 two, and the game's textures broadcast those to the channels
    /// the shader actually reads — `0x02020202` and `0x03020202`. `bntx` builds every texture
    /// with the identity swizzle `0x05040302`, which tells the hardware to read alpha from an
    /// alpha plane these formats do not have, so it returns 1.0 for every pixel and the effect
    /// covers its whole quad.
    ///
    /// 116 of the 162 convertible textures in these three effs carry a non-identity swizzle, so
    /// this was the common case, not an edge case.
    #[test]
    fn an_imported_texture_keeps_the_originals_channel_swizzle() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const IDENTITY: u32 = 0x05040302;
        let mut checked = 0usize;
        let mut non_identity = 0usize;
        for rel in [
            "effect/fighter/kirby/ef_kirby.eff",
            "effect/fighter/mario/ef_mario.eff",
            "effect/assist/bomberman/ef_bomberman.eff",
        ] {
            let Ok(bytes) = std::fs::read(root.join(rel)) else {
                continue;
            };
            let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
            let info = file
                .ptcl_file
                .as_ref()
                .and_then(|p| p.texture_info.as_ref())
                .expect("textures");
            let pool = info.binary_data.clone().expect("pool");
            let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();
            for (index, name) in names.iter().enumerate() {
                let desc = describe(&pool, index, name).expect("describe");
                if !desc.convertible {
                    continue;
                }
                let before = le_u32(&pool, brti_offset(&pool, index).unwrap() + BRTI_COMP_SEL)
                    .expect("original comp_sel");
                let png = export_png(&pool, index, name, Form::Editable).expect("export");
                let (rebuilt, _) =
                    replace_with_png(&pool, &names, index, &png, Form::Editable).expect("import");
                let after = le_u32(
                    &rebuilt,
                    brti_offset(&rebuilt, index).unwrap() + BRTI_COMP_SEL,
                )
                .expect("rebuilt comp_sel");
                assert_eq!(
                    after, before,
                    "{name} ({}) came back with swizzle {after:#010x}, was {before:#010x} — \
                     the game would draw this as a solid square",
                    desc.format
                );
                checked += 1;
                non_identity += usize::from(before != IDENTITY);
            }
        }
        assert!(checked > 100, "only {checked} textures round-tripped");
        assert!(
            non_identity > checked / 2,
            "expected most textures to carry a non-identity swizzle, got {non_identity} of \
             {checked} — if this drops, the test has stopped covering the case that broke"
        );
    }

    /// What the panel previews after an import must be what the export of the shipped texture
    /// gives back — the preview is a promise about the file, not a separate rendering.
    ///
    /// These two run through different code: the preview converts the PNG forward into stored
    /// channels and back out, while the shipped texture has additionally been through a BCn
    /// encode and a re-decode. They are only allowed to differ by that compression.
    #[test]
    fn the_preview_after_an_import_matches_what_actually_ships() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let bytes = std::fs::read(root.join("effect/fighter/kirby/ef_kirby.eff")).expect("eff");
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
        let info = file
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .expect("textures");
        let pool = info.binary_data.clone().expect("pool");
        let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();

        let mut checked = 0usize;
        for (index, name) in names.iter().enumerate() {
            for form in [Form::Editable, Form::Raw] {
                let Ok(png) = export_png(&pool, index, name, form) else {
                    continue;
                };
                // A real edit, so this cannot pass by both sides being the untouched original.
                let mut edited = image::load_from_memory(&png).expect("png").to_rgba8();
                let half = edited.width() / 2;
                for (x, _, px) in edited.enumerate_pixels_mut() {
                    if x < half {
                        *px = image::Rgba([0, 0, 0, px[3]]);
                    }
                }
                let mut edited_png = Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(edited)
                    .write_to(&mut edited_png, image::ImageFormat::Png)
                    .unwrap();
                let edited_png = edited_png.into_inner();

                let previewed = decode_import_preview(&pool, index, name, &edited_png, form, None)
                    .expect("preview");
                let (rebuilt, _) =
                    replace_with_png(&pool, &names, index, &edited_png, form).expect("import");
                let shipped = decode_preview(&rebuilt, index, name, form, None).expect("shipped");

                let pixels = previewed.pixels().len() as f64;
                let drift: f64 = previewed
                    .pixels()
                    .zip(shipped.pixels())
                    .map(|(p, s)| (0..4).map(|c| p[c].abs_diff(s[c]) as f64).sum::<f64>())
                    .sum::<f64>()
                    / (pixels * 4.0);
                assert!(
                    drift < 2.0,
                    "{name} ({form:?}): the preview differs from what ships by {drift:.1} \
                     levels per channel"
                );
                checked += 1;
            }
        }
        assert!(checked > 40, "only {checked} preview/ship pairs compared");
    }

    /// Growing and shrinking the pool has to leave every OTHER texture untouched, and the new
    /// one has to be readable at the right size and format.
    ///
    /// This is the risky class of edit: the pool is one packed archive with its own string table
    /// and alignment, and the eff serializer re-sorts descriptors by name and reorders the
    /// archive to match. A duplicate that shifted a neighbour would corrupt textures the user
    /// never touched, and nothing about it would be visible until something rendered wrong.
    #[test]
    fn duplicating_and_removing_a_texture_leaves_the_others_byte_identical() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let bytes = std::fs::read(root.join("effect/fighter/kirby/ef_kirby.eff")).expect("eff");
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
        let info = file
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .expect("textures");
        let pool = info.binary_data.clone().expect("pool");
        let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();
        let src = names
            .iter()
            .position(|n| n == "ef_cmn_star00")
            .expect("star00");

        // Every texture's own bytes, to compare against after the pool changes shape.
        let before: Vec<Vec<u8>> = names
            .iter()
            .enumerate()
            .map(|(i, n)| slice_one(&pool, i, n).expect("slice"))
            .collect();

        let copy_name = unique_texture_name(&names, "ef_cmn_star00");
        assert_eq!(copy_name, "ef_cmn_star00_v2");
        let grown = duplicate_texture(&pool, &names, src, &copy_name).expect("duplicate");
        let mut grown_names = names.clone();
        grown_names.push(copy_name.clone());

        // Pool order after a rebuild is the writer's business, so match textures by NAME.
        let find = |p: &[u8], all: &[String], want: &str| -> Option<usize> {
            (0..all.len()).find(|i| {
                effect_library::bntx::first_texture_name(&slice_one(p, *i, &all[*i]).unwrap())
                    .map(|n| n == want)
                    .unwrap_or(false)
            })
        };
        for (i, n) in names.iter().enumerate() {
            let at = find(&grown, &grown_names, n).unwrap_or_else(|| panic!("{n} vanished"));
            assert_eq!(
                slice_one(&grown, at, n).expect("slice"),
                before[i],
                "{n} changed when an unrelated texture was duplicated"
            );
        }
        // The copy is present, identical to its source, and describes the same way.
        let at = find(&grown, &grown_names, &copy_name).expect("the copy is in the pool");
        let copy_desc = describe(&grown, at, &copy_name).expect("describe copy");
        let src_desc = describe(&pool, src, &names[src]).expect("describe source");
        assert_eq!(
            (copy_desc.width, copy_desc.height, copy_desc.format.clone()),
            (src_desc.width, src_desc.height, src_desc.format.clone()),
            "the copy is not shaped like its source"
        );
        assert_eq!(
            decode_sampled(&grown, at, &copy_name, None)
                .expect("decode copy")
                .as_raw(),
            decode_sampled(&pool, src, &names[src], None)
                .expect("decode source")
                .as_raw(),
            "a duplicate must be pixel-identical — it is a rename, not a re-encode"
        );

        // And removing it again puts the pool back to exactly what it was.
        let shrunk = remove_texture(&grown, &grown_names, at).expect("remove");
        for (i, n) in names.iter().enumerate() {
            let at = find(&shrunk, &names, n).unwrap_or_else(|| panic!("{n} vanished on remove"));
            assert_eq!(
                slice_one(&shrunk, at, n).expect("slice"),
                before[i],
                "{n} changed when the copy was removed"
            );
        }
        assert!(
            find(&shrunk, &names, &copy_name).is_none(),
            "the removed texture is still in the pool"
        );
    }

    /// An import that is not the texture's own size must be refused, not resampled.
    #[test]
    fn a_differently_sized_image_is_refused() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let bytes = std::fs::read(root.join("effect/fighter/kirby/ef_kirby.eff")).expect("eff");
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
        let info = file
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .expect("textures");
        let pool = info.binary_data.clone().expect("pool");
        let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();
        let index = names
            .iter()
            .position(|n| n == "ef_cmn_star00")
            .expect("star00");
        let shape = describe(&pool, index, &names[index]).expect("describe");

        let mut wrong = image::RgbaImage::new(shape.width / 2, shape.height);
        for px in wrong.pixels_mut() {
            *px = image::Rgba([255, 255, 255, 255]);
        }
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(wrong)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();

        let err = replace_with_png(&pool, &names, index, &png, Form::Editable)
            .expect_err("a mismatched size must be refused");
        let message = format!("{err:#}");
        assert!(
            message.contains("keep their size"),
            "the refusal must explain itself, got: {message}"
        );
        // The same rule guards a brand-new entry, which is standing in for the same texture.
        assert!(
            add_texture_from_png(&pool, &names, index, "ef_test_new", &png, Form::Editable)
                .is_err(),
            "a new entry must be held to the template's size too"
        );
    }

    /// Painting black and erasing must do the same thing.
    ///
    /// The editable form writes the shape into the grey level AND the alpha, so a paint program
    /// can present it either way and a user can reasonably edit either one. Honouring only one
    /// of them would silently ignore half of the edits people actually make — and "my change did
    /// nothing" is indistinguishable from the bug this whole path exists to fix.
    #[test]
    fn painting_black_and_erasing_are_the_same_edit() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let bytes = std::fs::read(root.join("effect/fighter/kirby/ef_kirby.eff")).expect("eff");
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
        let info = file
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .expect("textures");
        let pool = info.binary_data.clone().expect("pool");
        let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();

        let mut checked = 0usize;
        for (index, name) in names.iter().enumerate() {
            let Ok(sampled) = decode_sampled(&pool, index, name, None) else {
                continue;
            };
            if !matches!(classify(&sampled), Layout::Mask | Layout::Matte { .. }) {
                continue;
            }
            let png = export_png(&pool, index, name, Form::Editable).expect("export");
            let base = image::load_from_memory(&png).expect("png").to_rgba8();
            let half = base.width() / 2;

            // Two ways a user might cut the left half away.
            let mut painted = base.clone();
            for (x, _, px) in painted.enumerate_pixels_mut() {
                if x < half {
                    *px = image::Rgba([0, 0, 0, px[3]]); // black brush, alpha untouched
                }
            }
            let mut erased = base.clone();
            for (x, _, px) in erased.enumerate_pixels_mut() {
                if x < half {
                    px[3] = 0; // eraser, colour untouched
                }
            }

            let ship = |img: &image::RgbaImage| {
                let mut buf = Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(img.clone())
                    .write_to(&mut buf, image::ImageFormat::Png)
                    .unwrap();
                let (rebuilt, _) =
                    replace_with_png(&pool, &names, index, &buf.into_inner(), Form::Editable)
                        .expect("import");
                decode_sampled(&rebuilt, index, name, None).expect("re-decode")
            };
            let (from_paint, from_erase) = (ship(&painted), ship(&erased));

            for (x, y, px) in from_paint.enumerate_pixels() {
                if x < half {
                    assert!(px[3] <= 8, "{name}: painting black left alpha {}", px[3]);
                }
                let other = from_erase.get_pixel(x, y);
                assert!(
                    px[3].abs_diff(other[3]) <= 8,
                    "{name}: painting black and erasing disagree at ({x},{y}): {} vs {}",
                    px[3],
                    other[3]
                );
            }
            checked += 1;
        }
        assert!(
            checked > 0,
            "no mask-like texture found — the test proved nothing"
        );
    }

    /// Export in editable form, import it straight back, and the texture must be unchanged —
    /// for every layout, including the ones that get restructured on the way out.
    ///
    /// The editable form moves information between channels (a mask's alpha becomes grey; a
    /// matte's flat colour is dropped and reinstated). Every one of those moves is a chance to
    /// lose the shape, and losing it looks exactly like the solid-square bug. So the round trip
    /// is checked on what the SHADER samples, not on the stored channels: BC4 keeps only red, so
    /// the bytes legitimately differ while the sampled result must not.
    #[test]
    fn an_untouched_editable_round_trip_changes_nothing() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let mut covered: std::collections::BTreeMap<&str, usize> = Default::default();
        for rel in [
            "effect/fighter/kirby/ef_kirby.eff",
            "effect/fighter/mario/ef_mario.eff",
            "effect/assist/bomberman/ef_bomberman.eff",
        ] {
            let Ok(bytes) = std::fs::read(root.join(rel)) else {
                continue;
            };
            let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
            let info = file
                .ptcl_file
                .as_ref()
                .and_then(|p| p.texture_info.as_ref())
                .expect("textures");
            let pool = info.binary_data.clone().expect("pool");
            let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();

            for (index, name) in names.iter().enumerate() {
                let Ok(before) = decode_sampled(&pool, index, name, None) else {
                    continue;
                };
                let png = export_png(&pool, index, name, Form::Editable).expect("export");
                let (rebuilt, _) =
                    replace_with_png(&pool, &names, index, &png, Form::Editable).expect("import");
                let after = decode_sampled(&rebuilt, index, name, None).expect("re-decode");

                let pixels = before.pixels().len() as f64;
                let drift: f64 = before
                    .pixels()
                    .zip(after.pixels())
                    .map(|(b, a)| (0..4).map(|c| b[c].abs_diff(a[c]) as f64).sum::<f64>())
                    .sum::<f64>()
                    / (pixels * 4.0);
                // BCn is lossy, so re-encoding always drifts a little; anything beyond a couple
                // of levels per channel means information was moved to the wrong place.
                assert!(
                    drift < 2.0,
                    "{name} ({rel}): mean channel drift {drift:.1} over an untouched round trip"
                );
                *covered
                    .entry(match classify(&before) {
                        Layout::Mask => "Mask",
                        Layout::Matte { .. } => "Matte",
                        Layout::Opaque => "Opaque",
                        Layout::ColorAlpha => "ColorAlpha",
                    })
                    .or_default() += 1;
            }
        }
        println!("layouts round-tripped: {covered:?}");
        assert_eq!(
            covered.keys().copied().collect::<Vec<_>>(),
            ["ColorAlpha", "Mask", "Matte", "Opaque"],
            "every layout must be exercised, saw {covered:?}"
        );
    }

    /// Painting an exported texture black must ERASE that part, for every mask-like texture.
    ///
    /// This is the reported failure, generalised. Effect textures hold their shape in whichever
    /// channel the packing made cheapest and rely on the swizzle to present it; edited in that
    /// stored form they are unintelligible, and an image saved without the right channel comes
    /// back fully opaque, which the game draws as a solid square. The editable form exists so
    /// that "paint black to cut a hole" means what it looks like, on every one of them.
    #[test]
    fn painting_an_exported_texture_black_erases_exactly_that_part() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let mut covered = std::collections::BTreeSet::new();
        for rel in [
            "effect/fighter/kirby/ef_kirby.eff",
            "effect/assist/bomberman/ef_bomberman.eff",
        ] {
            let Ok(bytes) = std::fs::read(root.join(rel)) else {
                continue;
            };
            let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
            let info = file
                .ptcl_file
                .as_ref()
                .and_then(|p| p.texture_info.as_ref())
                .expect("textures");
            let pool = info.binary_data.clone().expect("pool");
            let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();

            for (index, name) in names.iter().enumerate() {
                let Ok(sampled) = decode_sampled(&pool, index, name, None) else {
                    continue;
                };
                let layout = classify(&sampled);
                if !matches!(layout, Layout::Mask | Layout::Matte { .. }) {
                    continue;
                }

                // Exported greyscale AND with the same value in alpha, so the file is legible
                // whether a tool reads colour or transparency — and either can be edited.
                let png = export_png(&pool, index, name, Form::Editable).expect("export");
                let editable = image::load_from_memory(&png).expect("png").to_rgba8();
                assert!(
                    editable
                        .pixels()
                        .all(|px| px[0] == px[1] && px[1] == px[2] && px[3] == px[0]),
                    "{name}: an editable mask must be greyscale with matching alpha"
                );
                for (s, e) in sampled.pixels().zip(editable.pixels()) {
                    assert_eq!(s[3], e[0], "{name}: grey level must be the sampled alpha");
                    assert_eq!(s[3], e[3], "{name}: alpha must survive the export");
                }

                // Paint the left half black, intending to cut that half of the shape away.
                let half = editable.width() / 2;
                let mut edited = editable.clone();
                for (x, _, px) in edited.enumerate_pixels_mut() {
                    if x < half {
                        *px = image::Rgba([0, 0, 0, 255]);
                    }
                }
                let mut edited_png = Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(edited)
                    .write_to(&mut edited_png, image::ImageFormat::Png)
                    .unwrap();
                let (rebuilt, report) = replace_with_png(
                    &pool,
                    &names,
                    index,
                    &edited_png.into_inner(),
                    Form::Editable,
                )
                .expect("import");
                assert_eq!(
                    report.layout,
                    Some(layout),
                    "{name}: layout changed on import"
                );

                let after = decode_sampled(&rebuilt, index, name, None).expect("re-decode");
                for (x, _, px) in after.enumerate_pixels() {
                    if x < half {
                        assert!(
                            px[3] <= 8,
                            "{name}: painting black left alpha {} at x={x}",
                            px[3]
                        );
                    }
                }
                // ...and the half that was NOT painted keeps its shape. Without this the test
                // would pass on a texture that came back uniformly empty — the opposite failure.
                let solid = |img: &image::RgbaImage| {
                    img.enumerate_pixels()
                        .filter(|(x, _, px)| *x >= half && px[3] > 128)
                        .count()
                };
                let (before, after_count) = (solid(&sampled), solid(&after));
                if before > 32 {
                    let drift = (after_count as f64 - before as f64).abs() / before as f64;
                    assert!(
                        drift < 0.05,
                        "{name}: the untouched half changed, {before} solid pixels became \
                         {after_count}"
                    );
                }
                covered.insert(match layout {
                    Layout::Mask => "Mask",
                    Layout::Matte { .. } => "Matte",
                    _ => unreachable!(),
                });
            }
        }
        assert_eq!(
            covered,
            ["Mask", "Matte"].into_iter().collect(),
            "both single-channel layouts must be exercised, covered {covered:?}"
        );
    }
}
