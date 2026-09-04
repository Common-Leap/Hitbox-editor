//! Character select portraits.
//!
//! Portraits are `bntx` textures under `ui/replace/chara/`, one file per character per costume
//! slot. Decoding goes through [`crate::texture_import`] rather than adding a second image
//! path — that module already reads `bntx` and already knows which surface formats it can and
//! cannot honestly convert.
//!
//! Loading is budgeted rather than eager. A full roster is over a hundred BC7 textures, and
//! decoding them all on the frame the window opens is a visible stall; a handful per frame
//! fills the grid over a few frames instead, and a cached entry never decodes twice.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Portrait sets, in the order the select screen preview prefers them.
///
/// `chara_1` is the select screen's own grid portrait, which is what this preview is showing;
/// `chara_2` (the large preview art) and `chara_0` (the stock icon) are fallbacks so an entry
/// that ships only one of them still renders as itself rather than as a placeholder.
const PORTRAIT_SETS: &[&str] = &["chara_1", "chara_2", "chara_0"];

/// Directories portraits live under. `replace_patch` carries the DLC characters, and a mod
/// that adds a character may use either.
const PORTRAIT_DIRS: &[&str] = &["ui/replace/chara", "ui/replace_patch/chara"];

/// Directories stock icons live under, mirroring the chara trees.
const STOCK_DIRS: &[&str] = &["ui/replace/stock", "ui/replace_patch/stock"];

/// How many portraits to decode per frame. Keeps opening the window responsive without
/// leaving the grid empty for long.
const DECODE_BUDGET_PER_FRAME: usize = 4;

/// Longest edge of a cached portrait. The grid draws them small; decoding a 512² sheet and
/// holding it at full size would cost a hundred times the memory for no visible difference.
const PORTRAIT_MAX_EDGE: u32 = 160;

/// Every game path one image kind might live at, most preferred first.
///
/// A chara set prefers its own files, then falls back through the other
/// sets — a character shipping only `chara_0` still renders on the
/// `chara_1` tab rather than as a placeholder. Stock icons only ever look
/// for their own kind: a grid portrait is not a stock icon, and showing one
/// as if it were sends the author after the wrong file.
pub fn image_candidates(kind: &str, name_id: &str, slot: u8) -> Vec<String> {
    if kind.starts_with("stock") {
        return STOCK_DIRS
            .iter()
            .map(|directory| {
                format!("{directory}/{kind}/{kind}_{name_id}_{slot:02}.bntx")
            })
            .collect();
    }
    let mut sets = vec![kind];
    sets.extend(PORTRAIT_SETS.iter().filter(|set| **set != kind).copied());
    let mut out = Vec::new();
    for directory in PORTRAIT_DIRS {
        for set in &sets {
            out.push(format!("{directory}/{set}/{set}_{name_id}_{slot:02}.bntx"));
        }
    }
    out
}

/// Locate one image kind across the data root and enabled mod roots.
///
/// `roots` is searched in order, so a mod that ships its own file wins over
/// the base game exactly as it does for every other game file.
pub fn find_image(
    roots: &[PathBuf],
    kind: &str,
    name_id: &str,
    slot: u8,
) -> Option<PathBuf> {
    for candidate in image_candidates(kind, name_id, slot) {
        for root in roots {
            let path = root.join(&candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Locate a character's portrait across the data root and enabled mod roots.
///
/// The grid portrait (`chara_1` first) — what the select screen preview and
/// the dossier dot mean by "a portrait".
pub fn find_portrait(roots: &[PathBuf], name_id: &str, slot: u8) -> Option<PathBuf> {
    find_image(roots, "chara_1", name_id, slot)
}

/// Decode a portrait file to RGBA pixels at preview size.
pub fn load_portrait(path: &Path) -> Result<image::RgbaImage> {
    let pool = std::fs::read(path)?;
    crate::texture_import::decode_rgba(&pool, 0, "portrait", Some(PORTRAIT_MAX_EDGE))
}

/// Decode a portrait file with optional gamma fix for preview.
pub fn load_portrait_with_gamma(path: &Path, gamma_render: bool) -> Result<image::RgbaImage> {
    let image = load_portrait(path)?;
    Ok(if gamma_render {
        crate::roster::gamma::gamma_corrected(
            &image,
            crate::roster::gamma::DEFAULT_GAMMA,
            false,
        )
    } else {
        image
    })
}

/// Decode a PNG override for preview, with optional gamma fix.
pub fn load_png_override(path: &Path, gamma_render: bool, max_edge: Option<u32>) -> Result<image::RgbaImage> {
    let bytes = std::fs::read(path)?;
    let image = crate::texture_import::decode_png_rgba(&bytes, max_edge)?;
    Ok(if gamma_render {
        crate::roster::gamma::gamma_corrected(
            &image,
            crate::roster::gamma::DEFAULT_GAMMA,
            false,
        )
    } else {
        image
    })
}

/// What the cache knows about one character's portrait.
enum Slot {
    Ready(egui::TextureHandle),
    /// Looked for and not found, or found and undecodable. Kept so the lookup is not retried
    /// every frame, and so the reason can be shown rather than silently rendering a
    /// placeholder that looks the same as "not loaded yet".
    Missing(String),
}

/// Portraits decoded so far, keyed by the character's `name_id` and costume slot.
#[derive(Default)]
pub struct PortraitCache {
    entries: HashMap<(String, u8, bool), Slot>,
    /// Decodes performed this frame, against [`DECODE_BUDGET_PER_FRAME`].
    spent: usize,
}

impl PortraitCache {
    /// Call once per frame before drawing, to re-arm the per-frame decode budget.
    pub fn begin_frame(&mut self) {
        self.spent = 0;
    }

    /// Drop everything. Used when the mod library changes, since a newly enabled mod may now
    /// win a portrait an earlier root was providing.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The portrait for one character, decoding it if there is budget left this frame.
    ///
    /// Returns `None` while a portrait is still queued behind the budget — the caller draws a
    /// placeholder — and `Some(Err)` once it is known to be unavailable.
    /// `gamma_render` previews corrected; the grid always passes `true`, the
    /// Images editor passes the row's own flag. The game file is the grid
    /// portrait chain.
    pub fn get_with_gamma(
        &mut self,
        ctx: &egui::Context,
        roots: &[PathBuf],
        name_id: &str,
        slot: u8,
        gamma_render: bool,
        png_override: Option<&Path>,
    ) -> Option<Result<&egui::TextureHandle, &str>> {
        self.get_image_with_gamma(ctx, roots, "chara_1", name_id, slot, gamma_render, png_override)
    }

    /// Same, but the game file is looked up as `kind` (a stock tab previews
    /// stock files, not grid portraits). A picked PNG always wins over the
    /// game file regardless of kind.
    pub fn get_image_with_gamma(
        &mut self,
        ctx: &egui::Context,
        roots: &[PathBuf],
        kind: &str,
        name_id: &str,
        slot: u8,
        gamma_render: bool,
        png_override: Option<&Path>,
    ) -> Option<Result<&egui::TextureHandle, &str>> {
        let key = (name_id.to_string(), slot, gamma_render);
        // The kind disambiguates game lookups (a stock tab and a grid tab
        // for one slot decode different files); the override path
        // disambiguates picked PNGs. Both ride in the same string prefix
        // that `clear_for` matches on.
        let key = if let Some(p) = png_override {
            // Use path string as part of key; not perfect but cache will be cleared on change
            // via explicit `clear` from the view. Include hash of path + gamma flag.
            let mut k = key;
            k.0 = format!("{}|{kind}|{}", k.0, p.display());
            k
        } else {
            let mut k = key;
            k.0 = format!("{}|{kind}", k.0);
            k
        };
        if !self.entries.contains_key(&key) {
            if self.spent >= DECODE_BUDGET_PER_FRAME {
                // Not cached and no budget: ask for another frame rather than leaving the
                // grid permanently half-filled while the window sits idle.
                ctx.request_repaint();
                return None;
            }
            self.spent += 1;
            let loaded = if let Some(png) = png_override {
                match load_png_override(png, gamma_render, Some(PORTRAIT_MAX_EDGE)) {
                    Ok(image) => {
                        let size = [image.width() as usize, image.height() as usize];
                        let texture = ctx.load_texture(
                            format!("roster_portrait_{name_id}_{slot}_ov"),
                            egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                            egui::TextureOptions::LINEAR,
                        );
                        Slot::Ready(texture)
                    }
                    Err(error) => Slot::Missing(format!("{error:#}")),
                }
            } else {
                match find_image(roots, kind, name_id, slot) {
                    Some(path) => match load_portrait_with_gamma(&path, gamma_render) {
                        Ok(image) => {
                            let size = [image.width() as usize, image.height() as usize];
                            let texture = ctx.load_texture(
                                format!("roster_portrait_{name_id}_{slot}_{gamma_render}"),
                                egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                                egui::TextureOptions::LINEAR,
                            );
                            Slot::Ready(texture)
                        }
                        Err(error) => Slot::Missing(format!("{error:#}")),
                    },
                    None => Slot::Missing("no portrait in any root".to_string()),
                }
            };
            self.entries.insert(key.clone(), loaded);
        }
        Some(match &self.entries[&key] {
            Slot::Ready(texture) => Ok(texture),
            Slot::Missing(reason) => Err(reason.as_str()),
        })
    }

    /// Clear only entries for one name_id/slot (used when its override changes).
    pub fn clear_for(&mut self, name_id: &str, slot: u8) {
        self.entries.retain(|(n, s, _), _| {
            // Keys for PNG overrides are `name_id|path`, so match prefix.
            let base = n.split('|').next().unwrap_or(n);
            !(base == name_id && *s == slot)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_cover_both_portrait_trees_with_the_grid_portrait_first() {
        let candidates = image_candidates("chara_1", "mario", 0);
        assert_eq!(
            candidates[0],
            "ui/replace/chara/chara_1/chara_1_mario_00.bntx"
        );
        assert!(candidates
            .iter()
            .any(|path| path.starts_with("ui/replace_patch/chara")));
    }

    /// Slots are two-digit in these filenames; a bare `{slot}` would look for `_8` and find
    /// nothing, which is indistinguishable from a character having no portrait.
    #[test]
    fn slot_numbers_are_two_digit() {
        assert!(image_candidates("chara_1", "mario", 8)[0].ends_with("_mario_08.bntx"));
    }

    /// Root order is mod load order, so a mod portrait must beat the base game's.
    #[test]
    fn the_first_root_that_has_the_portrait_wins() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let modded = dir.path().join("mod");
        for root in [&base, &modded] {
            let path = root.join("ui/replace/chara/chara_1/chara_1_mario_00.bntx");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        let roots = vec![modded.clone(), base.clone()];
        assert!(find_portrait(&roots, "mario", 0)
            .unwrap()
            .starts_with(&modded));
    }

    #[test]
    fn a_character_with_no_portrait_anywhere_reports_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_portrait(&[dir.path().to_path_buf()], "mario", 0).is_none());
    }

    /// Each chara tab previews its own set first — a `chara_2` tab showing
    /// the `chara_1` file while the `chara_2` file sits next to it sends the
    /// author after the wrong file.
    #[test]
    fn each_chara_set_prefers_its_own_files() {
        let kinds = image_candidates("chara_2", "mario", 0);
        assert_eq!(
            kinds[0],
            "ui/replace/chara/chara_2/chara_2_mario_00.bntx"
        );
        assert!(kinds
            .iter()
            .any(|path| path.contains("chara_1")));
    }

    /// Stock tabs look for stock files, never grid portraits: showing a
    /// portrait as a stock icon preview is worse than showing nothing.
    #[test]
    fn stock_tabs_search_only_stock_files() {
        let kinds = image_candidates("stock_90", "mario", 3);
        assert!(!kinds.is_empty());
        assert!(kinds.iter().all(|path| path.contains("stock_90")));
        assert_eq!(
            kinds[0],
            "ui/replace/stock/stock_90/stock_90_mario_03.bntx"
        );
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        assert!(find_image(&roots, "stock_90", "mario", 3).is_none());
    }
}
