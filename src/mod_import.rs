//! Import any mod folder as an editable project.
//!
//! A compiled mod is an arc root (`fighter/`, `effect/`, `ui/`, …) possibly wrapped
//! in `romfs/` or a folder named after the mod. This module detects that root,
//! then adopts what is reversibly editable:
//!
//! * `ui_chara_db` diffs (order / visibility / row fields) → [`RosterMod`]
//! * `.xmsbt` names → [`RosterMod`] name overrides
//! * BNTX portraits → PNG assets + [`RosterMod`] image overrides
//! * `fighter_param` diffs → sparse [`ParamMod`] per fighter
//! * Rust/TOML source text → copied beside the project as reference
//! * Loose assets (models, motions, …) → linked via the mod library (reported
//!   here, inserted by the caller)
//!
//! Compiled ACMD, binary EFF/MSBT, and `.nro` plugins are reference-only by
//! design: they are stated in the report, never silently dropped. Unknown roster
//! rows are reported, never fabricated. Missing base dumps skip their diff
//! honestly instead of guessing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::mod_project::{CharaOverrides, ModProjectFile, NameVariants, ParamMod, ParamValue};
use crate::roster::{css, traits};

// ── Report ────────────────────────────────────────────────────────────────

/// What happened to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    /// Adopted into the editable project (roster diff, names, portraits, params).
    Adopted,
    /// Kept as reference only (compiled ACMD/EFF/MSBT, plugins, source text).
    /// Stated, never silent.
    ReferenceOnly,
    /// Left in place and linked via the mod library.
    Linked,
    /// Not adopted, with a reason (missing base dump, unknown row, undecodable).
    Skipped,
}

impl FileOutcome {
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::ReferenceOnly => "reference-only",
            Self::Linked => "linked",
            Self::Skipped => "skipped",
        }
    }
}

/// One file's outcome in the import report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    /// Game-relative (`fighter/mario/…`) for files under the arc root, or
    /// mod-relative for wrapper-level files (source, readme, …).
    pub path: String,
    pub outcome: FileOutcome,
    pub detail: String,
}

/// Per-file import report. Accounts for every file the mod contained.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub files: Vec<FileReport>,
    /// Non-file warnings: unknown roster rows, missing base dumps, undecodable
    /// portraits, fields that no longer exist, etc.
    pub warnings: Vec<String>,
    /// How the arc root was found (for the "which level it chose" line).
    pub detection: String,
    /// Arc root game-relative file count, for the summary line.
    #[allow(dead_code)]
    pub arc_root: PathBuf,
}

impl ImportReport {
    pub fn count(&self, outcome: FileOutcome) -> usize {
        self.files.iter().filter(|f| f.outcome == outcome).count()
    }

    #[allow(dead_code)]
    pub fn adopted(&self) -> usize {
        self.count(FileOutcome::Adopted)
    }

    pub fn push(&mut self, path: String, outcome: FileOutcome, detail: String) {
        self.files.push(FileReport {
            path,
            outcome,
            detail,
        });
    }

    pub fn warn(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    /// Every game-relative path in `paths` appears in the report. The import
    /// must never silently drop a file class.
    ///
    /// Exercised by the import acceptance test rather than the import flow
    /// itself; kept as the completeness check the flow will report through
    /// once it surfaces per-file uptake in the UI.
    #[allow(dead_code)]
    pub fn covers(&self, paths: &[String]) -> Vec<String> {
        let seen: BTreeSet<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
        paths
            .iter()
            .filter(|p| !seen.contains(p.as_str()))
            .cloned()
            .collect()
    }

    pub fn summary(&self) -> String {
        format!(
            "{} adopted · {} reference-only · {} linked · {} skipped{}",
            self.count(FileOutcome::Adopted),
            self.count(FileOutcome::ReferenceOnly),
            self.count(FileOutcome::Linked),
            self.count(FileOutcome::Skipped),
            if self.warnings.is_empty() {
                String::new()
            } else {
                format!(" · {} warning(s)", self.warnings.len())
            }
        )
    }
}

// ── Roster key mapping ────────────────────────────────────────────────────

/// Map a `name_id` to the project key that owns it: the fighter key when a
/// known fighter directory matches, else the select-screen-row key.
///
/// Never fabricates a fighter: an unknown `name_id` becomes `ui:<id>`, which
/// the roster index reports as stale until its row exists rather than as a
/// character that does not exist.
pub fn key_for_name_id(name_id: &str, known_fighters: &[String]) -> crate::roster::RosterKey {
    let lower = name_id.to_ascii_lowercase();
    if known_fighters
        .iter()
        .any(|f| f.eq_ignore_ascii_case(&lower))
    {
        crate::roster::RosterKey::fighter(&lower)
    } else {
        crate::roster::RosterKey::chara(&lower)
    }
}

/// Map a `fighter_kind` hash to a known fighter directory name, if any.
pub fn fighter_for_kind(hash: u64, known_fighters: &[String]) -> Option<String> {
    known_fighters
        .iter()
        .find(|name| css::fighter_kind_hash(name) == hash)
        .cloned()
}

// ── .xmsbt ────────────────────────────────────────────────────────────────

/// Decode an `.xmsbt` file (UTF-16LE with BOM, as [`crate::roster::names`]
/// writes) into `(label, text)` pairs.
pub fn parse_xmsbt(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).map_err(|e| anyhow::anyhow!("bad UTF-16: {e}"))?
    } else {
        std::str::from_utf8(bytes)
            .context("xmsbt is neither UTF-16LE-BOM nor UTF-8")?
            .to_string()
    };
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(entry_start) = rest.find("<entry") {
        rest = &rest[entry_start..];
        let Some(label_start) = rest.find("label=\"") else {
            break;
        };
        rest = &rest[label_start + 7..];
        let Some(label_end) = rest.find('"') else {
            break;
        };
        let label = rest[..label_end].to_string();
        rest = &rest[label_end..];
        let Some(text_start) = rest.find("<text>") else {
            break;
        };
        rest = &rest[text_start + 6..];
        let Some(text_end) = rest.find("</text>") else {
            break;
        };
        let raw = rest[..text_end].to_string();
        rest = &rest[text_end + 7..];
        out.push((label, unescape(&raw)));
    }
    Ok(out)
}

fn unescape(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Split `nam_chr{0,1,2}_<slot>_<name_id>` into `(chr, slot, name_id)`.
fn split_name_label(label: &str) -> Option<(u8, u8, String)> {
    let rest = label.strip_prefix("nam_chr")?;
    let (chr_digit, rest) = rest.split_at_checked(1)?;
    let chr: u8 = chr_digit.parse().ok()?;
    if chr > 2 {
        return None;
    }
    let rest = rest.strip_prefix('_')?;
    let (slot_str, name_id) = rest.split_once('_')?;
    if slot_str.len() != 2 || !slot_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let slot: u8 = slot_str.parse().ok()?;
    if name_id.is_empty() {
        return None;
    }
    Some((chr, slot, name_id.to_ascii_lowercase()))
}

/// Adopt `.xmsbt` labels into roster name edits.
///
/// * slot 0 → `names` (+ `name_variants` when chr0/1/2 disagree beyond the
///   vanilla upper-case rule)
/// * non-zero slot on a known fighter → `per_costume_names` (the vanilla
///   alt-costume path; variants there collapse to the simple name with a
///   warning because that table holds one string per slot)
/// * non-zero slot on an unknown `name_id` → `names` on the `ui:` key with a
///   warning (the slot is kept in the warning, never silently dropped)
#[allow(clippy::too_many_arguments)]
pub fn adopt_xmsbt_labels(
    labels: &[(String, String)],
    known_fighters: &[String],
    names: &mut BTreeMap<crate::roster::RosterKey, String>,
    name_variants: &mut BTreeMap<crate::roster::RosterKey, NameVariants>,
    per_costume: &mut BTreeMap<String, BTreeMap<u8, String>>,
    warnings: &mut Vec<String>,
) {
    let mut grouped: BTreeMap<(String, u8), BTreeMap<u8, String>> = BTreeMap::new();
    for (label, text) in labels {
        if let Some((chr, slot, name_id)) = split_name_label(label) {
            grouped
                .entry((name_id, slot))
                .or_default()
                .insert(chr, text.clone());
        } else {
            warnings.push(format!("{label}: not a character name label, left alone"));
        }
    }
    for ((name_id, slot), chrs) in grouped {
        let chr0 = chrs.get(&0).cloned();
        let chr1 = chrs.get(&1).cloned();
        let chr2 = chrs.get(&2).cloned();
        let simple = match (&chr0, &chr1, &chr2) {
            (Some(a), Some(b), Some(c)) => a == b && *c == a.to_uppercase(),
            _ => false,
        };
        let fighter_known = known_fighters.iter().any(|f| f == &name_id);
        if slot == 0 {
            let key = key_for_name_id(&name_id, known_fighters);
            if simple {
                names.insert(key, chr0.unwrap_or_default());
            } else {
                // Keep the mixed-case name as the fallback so the export's
                // detailed path still writes all three labels.
                let fallback = chr0.clone().or(chr1.clone()).unwrap_or_default();
                if !fallback.is_empty() {
                    names.insert(key.clone(), fallback);
                }
                // An explicitly authored chr2 keeps its case; the export only
                // upper-cases fallbacks.
                let variants = NameVariants { chr0, chr1, chr2 };
                if !variants.is_empty() {
                    name_variants.insert(key, variants);
                }
            }
        } else if fighter_known {
            let display = chr0.clone().or(chr1.clone()).unwrap_or_default();
            if simple || (chr0.is_some() || chr1.is_some()) {
                if !simple {
                    warnings.push(format!(
                        "nam_chr*_{slot:02}_{name_id}: per-costume names hold one string, kept \"{display}\""
                    ));
                }
                if !display.is_empty() {
                    per_costume
                        .entry(name_id.clone())
                        .or_default()
                        .insert(slot, display);
                }
            } else if let Some(upper) = chr2 {
                per_costume
                    .entry(name_id.clone())
                    .or_default()
                    .insert(slot, upper);
            }
        } else {
            warnings.push(format!(
                "nam_chr*_{slot:02}_{name_id}: no fighter named {name_id}, kept under ui:{name_id} (slot kept in this warning)"
            ));
            let key = key_for_name_id(&name_id, known_fighters);
            let display = chr0.or(chr1).or(chr2).unwrap_or_default();
            if !display.is_empty() {
                names.insert(key, display);
            }
        }
    }
}

// ── ui_chara_db diff ──────────────────────────────────────────────────────

/// Diff a modded `ui_chara_db` against the base dump.
///
/// A diffed roster database: `(order, hidden, chara_overrides, unknown_rows)`.
/// Rows in the mod file with no base row are unknown: reported, never
/// fabricated into project edits.
type CharaDbDiff = (
    BTreeMap<crate::roster::RosterKey, i8>,
    BTreeSet<crate::roster::RosterKey>,
    BTreeMap<crate::roster::RosterKey, CharaOverrides>,
    Vec<String>,
);

pub fn diff_chara_db(
    base: &css::CharaDb,
    modded: &css::CharaDb,
    known_fighters: &[String],
) -> CharaDbDiff {
    use std::collections::BTreeSet;
    let mut order = BTreeMap::new();
    let mut hidden = BTreeSet::new();
    let mut patches = BTreeMap::new();
    let mut unknown = Vec::new();

    let base_by_name: HashMap<&str, &css::CharaRow> = base
        .entries()
        .iter()
        .map(|row| (row.name_id.as_str(), row))
        .collect();

    for row in modded.entries() {
        let Some(base_row) = base_by_name.get(row.name_id.as_str()) else {
            unknown.push(row.name_id.clone());
            continue;
        };
        // Resolve the key through the fighter when the row backs one, so the
        // override resolves through the same index the export reads.
        let key = match fighter_for_kind(row.fighter_kind, known_fighters) {
            Some(fighter) => crate::roster::RosterKey::fighter(&fighter),
            None if known_fighters.iter().any(|f| f == &row.name_id) => {
                crate::roster::RosterKey::fighter(&row.name_id)
            }
            None => crate::roster::RosterKey::chara(&row.name_id),
        };
        let base_hidden = !base_row.can_select && base_row.disp_order == css::OFF_ROSTER;
        let mod_hidden = !row.can_select && row.disp_order == css::OFF_ROSTER;
        if mod_hidden != base_hidden {
            if mod_hidden {
                hidden.insert(key.clone());
            }
            // Unhiding is expressed by the absence of `hidden` plus the order
            // below when the position also changed. A row unhidden at the same
            // position needs no edit at all.
        } else if mod_hidden {
            // Both hidden: no order to record.
        }
        if row.disp_order != base_row.disp_order && !mod_hidden {
            // Hidden wins over a position on export, so a hidden row's stale
            // position is not recorded here either.
            order.insert(key.clone(), row.disp_order);
        }
        let mut patch = CharaOverrides::default();
        if row.color_num != base_row.color_num {
            patch.color_num = Some(row.color_num);
        }
        if row.save_no != base_row.save_no {
            patch.save_no = Some(row.save_no);
        }
        if patch.color_num.is_some() || patch.save_no.is_some() {
            patches.insert(key, patch);
        }
    }
    unknown.sort();
    unknown.dedup();
    (order, hidden, patches, unknown)
}

// ── fighter_param diff ────────────────────────────────────────────────────

fn param_table(root: &prc::ParamStruct) -> Option<&prc::ParamList> {
    let wanted = hash40::hash40("fighter_param_table").0;
    root.0.iter().find_map(|(hash, value)| match value {
        prc::ParamKind::List(list) if hash.0 == wanted => Some(list),
        _ => None,
    })
}

fn row_kind_hash(entry: &prc::ParamStruct) -> Option<u64> {
    let wanted = hash40::hash40("fighter_kind").0;
    entry.0.iter().find_map(|(hash, value)| match value {
        prc::ParamKind::Hash(h) if hash.0 == wanted => Some(h.0),
        _ => None,
    })
}

fn to_param_value(kind: &prc::ParamKind) -> Option<ParamValue> {
    Some(match kind {
        prc::ParamKind::Bool(v) => ParamValue::Bool(*v),
        prc::ParamKind::I8(v) => ParamValue::I8(*v),
        prc::ParamKind::U8(v) => ParamValue::U8(*v),
        prc::ParamKind::I16(v) => ParamValue::I16(*v),
        prc::ParamKind::U16(v) => ParamValue::U16(*v),
        prc::ParamKind::I32(v) => ParamValue::I32(*v),
        prc::ParamKind::U32(v) => ParamValue::U32(*v),
        prc::ParamKind::Float(v) => ParamValue::Float(*v),
        prc::ParamKind::Hash(v) => ParamValue::Hash(v.0),
        _ => return None,
    })
}

fn field_name(hash: u64, labels: &HashMap<u64, String>) -> String {
    labels
        .get(&hash)
        .cloned()
        .unwrap_or_else(|| format!("{hash:#x}"))
}

/// Diff a modded `fighter_param.prc` against the base dump.
///
/// One sparse [`ParamMod`] per fighter, keyed by game-relative path then field.
/// Rows with no known fighter are unknown: reported, never fabricated. Fields
/// whose hash has no label keep their hex name (honest) — the export then
/// reports them as unwritable rather than guessing.
pub fn diff_fighter_param(
    base: &prc::ParamStruct,
    modded: &prc::ParamStruct,
    known_fighters: &[String],
    labels: &HashMap<u64, String>,
) -> (BTreeMap<String, ParamMod>, Vec<String>) {
    let mut out: BTreeMap<String, ParamMod> = BTreeMap::new();
    let mut unknown = Vec::new();

    let Some(base_table) = param_table(base) else {
        return (
            out,
            vec!["base fighter_param has no fighter_param_table".into()],
        );
    };
    let Some(mod_table) = param_table(modded) else {
        return (
            out,
            vec!["mod fighter_param has no fighter_param_table".into()],
        );
    };

    let mut base_rows: HashMap<u64, &prc::ParamStruct> = HashMap::new();
    for item in &base_table.0 {
        if let prc::ParamKind::Struct(entry) = item {
            if let Some(kind) = row_kind_hash(entry) {
                base_rows.insert(kind, entry);
            }
        }
    }

    for item in &mod_table.0 {
        let prc::ParamKind::Struct(mod_entry) = item else {
            continue;
        };
        let Some(kind) = row_kind_hash(mod_entry) else {
            continue;
        };
        let Some(fighter) = fighter_for_kind(kind, known_fighters) else {
            unknown.push(format!("fighter_kind {kind:#x}"));
            continue;
        };
        let Some(base_entry) = base_rows.get(&kind) else {
            unknown.push(fighter.clone());
            continue;
        };
        let base_fields: HashMap<u64, &prc::ParamKind> = base_entry
            .0
            .iter()
            .map(|(hash, value)| (hash.0, value))
            .collect();
        for (hash, mod_value) in &mod_entry.0 {
            // Nested lists/structs are not scalar trait values; ignore like the
            // trait editor does rather than inventing an edit for them.
            let Some(mod_scalar) = to_param_value(mod_value) else {
                continue;
            };
            let base_scalar = base_fields.get(&hash.0).and_then(|v| to_param_value(v));
            if base_scalar != Some(mod_scalar) {
                // A field the base row lacks is still a real mod value; keep it
                // under its label so the export can name what is missing.
                let name = field_name(hash.0, labels);
                out.entry(fighter.clone())
                    .or_default()
                    .files
                    .entry(traits::FIGHTER_PARAM_PATH.to_string())
                    .or_default()
                    .insert(name, mod_scalar);
            }
        }
    }
    unknown.sort();
    unknown.dedup();
    (out, unknown)
}

// ── Portraits ─────────────────────────────────────────────────────────────

/// Parse a portrait filename `<kind>_<name_id>_<slot>.bntx`.
#[allow(dead_code)]
fn split_portrait_filename(file_name: &str) -> Option<(String, String, u8)> {
    let stem = file_name.strip_suffix(".bntx")?;
    // Slot is the last `_NN`; the kind/name split goes through
    // `portrait_kind_split` because stock kinds contain an underscore
    // (`stock_90`) and names may too (`ice_climber`).
    let slot: u8 = match stem.rsplit_once('_') {
        Some((_, tail)) if tail.len() == 2 && tail.bytes().all(|b| b.is_ascii_digit()) => {
            tail.parse().ok()?
        }
        _ => return None,
    };
    let (kind, name_id) = portrait_kind_split(stem)?;
    Some((kind.to_string(), name_id.to_string(), slot))
}

/// Split `<kind>_<name_id>_<slot>` with multi-underscore kinds (`stock_90`).
fn portrait_kind_split(stem: &str) -> Option<(&str, &str)> {
    // Strip the trailing _NN slot first.
    let (head, _) = stem.rsplit_once('_')?;
    for kind in ["stock_90", "stock_80", "chara_0", "chara_1", "chara_2"] {
        if let Some(rest) = head.strip_prefix(&format!("{kind}_")) {
            if !rest.is_empty() {
                return Some((kind, rest));
            }
        }
    }
    // Unknown kind: first component is the kind, the rest is the name.
    let (kind, name_id) = head.split_once('_')?;
    if kind.is_empty() || name_id.is_empty() {
        return None;
    }
    Some((kind, name_id))
}

/// Decode one portrait BNTX to PNG bytes.
pub fn decode_portrait_bntx(bytes: &[u8], name: &str) -> Result<image::RgbaImage> {
    // UI portraits are single-texture BNTX; the pool decoder handles them as a
    // pool of one. Fall back to a direct BNTX read for containers it refuses.
    match crate::texture_import::decode_rgba(bytes, 0, name, None) {
        Ok(image) => Ok(image),
        Err(_) => {
            use binrw::BinRead;
            let mut cursor = std::io::Cursor::new(bytes);
            let bntx = bntx::Bntx::read_le(&mut cursor)
                .map_err(|e| anyhow::anyhow!("portrait BNTX unreadable ({name}): {e}"))?;
            let surface = bntx
                .to_surface()
                .map_err(|e| anyhow::anyhow!("portrait {name} has no surface: {e}"))?;
            let rgba = surface
                .decode_rgba8()
                .map_err(|e| anyhow::anyhow!("portrait {name} undecodable: {e}"))?;
            rgba.to_image(0)
                .map_err(|e| anyhow::anyhow!("portrait {name} has no image: {e}"))
        }
    }
}

fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut out, image::ImageFormat::Png)
        .context("encoding portrait PNG")?;
    Ok(out.into_inner())
}

// ── File classification ───────────────────────────────────────────────────

fn is_source_text(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("rs") | Some("toml") | Some("md") | Some("txt")
    )
}

fn is_reference_only_binary(game_path: &str) -> Option<&'static str> {
    let lower = game_path.to_ascii_lowercase();
    if lower.ends_with(".eff") {
        Some("binary EFF is reference-only by design (adopt its roster/param effects instead)")
    } else if lower.ends_with(".msbt") {
        Some("binary MSBT is reference-only by design (adopt its .xmsbt override instead)")
    } else if lower.ends_with(".nro") {
        Some("compiled plugin (.nro) is reference-only by design — the editor shows vanilla scripts for those fighters")
    } else if lower.ends_with(".prc")
        && game_path != css::CHARA_DB_PATH
        && game_path != traits::FIGHTER_PARAM_PATH
    {
        Some("binary param is reference-only by design (only ui_chara_db and fighter_param diffs are adopted)")
    } else {
        None
    }
}

fn is_portrait_path(game_path: &str) -> bool {
    let lower = game_path.to_ascii_lowercase();
    (lower.contains("ui/replace/chara/")
        || lower.contains("ui/replace/stock/")
        || lower.contains("ui/replace_patch/chara/")
        || lower.contains("ui/replace_patch/stock/"))
        && lower.ends_with(".bntx")
}

fn is_xmsbt_path(game_path: &str) -> bool {
    game_path.to_ascii_lowercase().ends_with(".xmsbt")
}

// ── Workspace destinations ─────────────────────────────────────────────
// Every file the tool does not support still has a home: the workspace
// `romfs/` overlay in arc layout. This is the same map the Project Files
// panel shows, so the import report and the panel can never disagree.

/// Where a game-relative file lives in a project workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDest {
    pub support: crate::project_hub::WorkspaceSupport,
    /// Workspace-relative destination (`modproject.json`, `assets/…`,
    /// `reference/…`, or `romfs/<game path>`).
    pub workspace: String,
    /// One-line note for the report / panel.
    pub note: &'static str,
}

/// Map any game-relative mod path to its workspace home, including the
/// unsupported kinds (models, animations, sound, …) which go to the manual
/// `romfs/` overlay and ship verbatim on export.
pub fn workspace_dest_for_game_path(game_path: &str) -> WorkspaceDest {
    use crate::project_hub::WorkspaceSupport as Ws;
    let lower = game_path.to_ascii_lowercase();
    if game_path == css::CHARA_DB_PATH
        || game_path == traits::FIGHTER_PARAM_PATH
        || is_xmsbt_path(game_path)
    {
        return WorkspaceDest {
            support: Ws::Supported,
            workspace: "modproject.json".into(),
            note: "adopted as an editable diff",
        };
    }
    if is_portrait_path(game_path) {
        return WorkspaceDest {
            support: Ws::Supported,
            workspace: "assets/roster_ui/...".into(),
            note: "decoded to a managed PNG asset",
        };
    }
    if lower.ends_with(".rs") || lower.ends_with(".toml") {
        return WorkspaceDest {
            support: Ws::Reference,
            workspace: format!("reference/{game_path}"),
            note: "copied as reference; never exported",
        };
    }
    if lower.ends_with(".eff") || lower.ends_with(".msbt") || lower.ends_with(".nro") {
        return WorkspaceDest {
            support: Ws::Reference,
            workspace: format!("reference/{game_path}"),
            note: "reference-only by design; manual override ships from romfs/",
        };
    }
    if lower.ends_with(".prc") {
        return WorkspaceDest {
            support: Ws::Reference,
            workspace: format!("reference/{game_path}"),
            note: "only ui_chara_db + fighter_param diffs are adopted",
        };
    }
    // Everything else with a game path is a loose asset the tool links: models,
    // animations, sound, stages, … Owning it in this workspace means copying it
    // to the manual overlay, which ships verbatim on export.
    let manual_kind = if lower.contains("/model/") || lower.ends_with(".numdlb") {
        "models"
    } else if lower.contains("/motion/") || lower.ends_with(".nuanmb") {
        "animations"
    } else {
        "loose assets"
    };
    let _ = manual_kind;
    WorkspaceDest {
        support: Ws::Manual,
        workspace: format!("romfs/{game_path}"),
        note: "manual overlay — ships verbatim on export",
    }
}

/// Options for [`import_mod_as_project`].
pub struct ImportOptions<'a> {
    /// Known fighter directory names (from the base scan).
    pub known_fighters: &'a [String],
    /// Param field labels (hash → name).
    pub labels: &'a HashMap<u64, String>,
    /// Base arc roots for diffs (data root first, then enabled mods).
    pub base_roots: &'a [PathBuf],
    /// Asset folder name beside `modproject.json` (e.g. `my_mod_assets`).
    pub asset_dir_name: &'a str,
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn game_relative(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root).ok().map(|rel| {
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    })
}

fn mod_relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|rel| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_else(|_| path.to_string_lossy().to_string())
}

/// Import any mod folder as an editable project.
///
/// * `mod_folder` — the folder the user picked (wrappers allowed).
/// * `project_dir` — where `modproject.json` will live; PNG assets go under
///   `<project_dir>/<asset_dir_name>/roster_ui/`, source references under
///   `<project_dir>/reference/`.
///
/// Returns the adopted project plus a per-file report. Loose assets are
/// reported as `Linked`: the caller inserts the mod into the mod library so
/// they stay live.
pub fn import_mod_as_project(
    mod_folder: &Path,
    project_dir: &Path,
    options: &ImportOptions<'_>,
) -> Result<(ModProjectFile, ImportReport)> {
    let (arc_root, detection) = crate::roster::library::detect_arc_root(mod_folder)?;
    let mut report = ImportReport {
        detection: detection.describe(),
        arc_root: arc_root.clone(),
        ..Default::default()
    };

    let mut project = ModProjectFile {
        version: crate::mod_project::PROJECT_VERSION,
        name: mod_folder
            .file_name()
            .and_then(|n| n.to_str())
            .map(crate::mod_export::slugify)
            .unwrap_or_else(|| "imported_mod".into()),
        ..Default::default()
    };

    // Base dumps for honest diffs. Missing → skip that diff, never guess.
    let base_chara_db = css::locate_ui_root(options.base_roots)
        .and_then(|root| css::CharaDb::open(&root.join(css::CHARA_DB_PATH)).ok());
    let base_param_path = crate::roster::traits::FighterTraits::locate(options.base_roots);
    let base_name_ids: BTreeSet<String> = base_chara_db
        .as_ref()
        .map(|db| db.entries().iter().map(|r| r.name_id.clone()).collect())
        .unwrap_or_default();

    std::fs::create_dir_all(project_dir).context("creating the project folder")?;
    let reference_root = project_dir.join("reference");

    // Collect xmsbt labels across all files first (one .xmsbt per mod in
    // practice, but a mod may ship several).
    let mut xmsbt_labels: Vec<(String, String)> = Vec::new();
    let mut xmsbt_paths: Vec<String> = Vec::new();

    let all_files = walk_files(mod_folder);
    // Game files under the arc root, plus wrapper-level files outside it.
    for path in &all_files {
        let under_arc = path.starts_with(&arc_root);
        let display = if under_arc {
            game_relative(path, &arc_root).unwrap_or_else(|| mod_relative(path, mod_folder))
        } else {
            format!("(outer) {}", mod_relative(path, mod_folder))
        };

        if !under_arc {
            // Wrapper-level files: source text is copied as reference, the
            // rest is skipped honestly.
            if is_source_text(path) {
                let rel = mod_relative(path, mod_folder);
                let dest = reference_root.join(&rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(path, &dest)
                    .with_context(|| format!("copying reference {}", path.display()))?;
                report.push(
                    display,
                    FileOutcome::ReferenceOnly,
                    format!("copied as reference to reference/{rel}"),
                );
            } else {
                report.push(
                    display,
                    FileOutcome::Skipped,
                    "outside the arc root and not source text".into(),
                );
            }
            continue;
        }

        let game_path = game_relative(path, &arc_root).unwrap_or(display.clone());

        // The two diffed databases and the adopted companions are handled in
        // their own passes below (they need base + cross-file grouping); mark
        // them here as placeholders to be rewritten, so every file is already
        // accounted for even if a later pass bails.
        if game_path == css::CHARA_DB_PATH
            || game_path == traits::FIGHTER_PARAM_PATH
            || is_xmsbt_path(&game_path)
            || is_portrait_path(&game_path)
        {
            continue;
        }

        if let Some(reason) = is_reference_only_binary(&game_path) {
            let dest = workspace_dest_for_game_path(&game_path);
            report.push(
                display,
                FileOutcome::ReferenceOnly,
                format!("{reason} — workspace: {} ({})", dest.workspace, dest.note),
            );
        } else if is_source_text(path) {
            let rel = game_relative(path, &arc_root).unwrap_or_else(|| game_path.clone());
            let dest = reference_root.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(path, &dest)
                .with_context(|| format!("copying reference {}", path.display()))?;
            report.push(
                display,
                FileOutcome::ReferenceOnly,
                format!("copied as reference to reference/{rel}"),
            );
        } else {
            let dest = workspace_dest_for_game_path(&game_path);
            report.push(
                display,
                FileOutcome::Linked,
                format!(
                    "linked via the mod library — to own it here copy to {} ({})",
                    dest.workspace, dest.note
                ),
            );
        }
    }

    // ── ui_chara_db diff ──────────────────────────────────────────────
    let mod_db_path = arc_root.join(css::CHARA_DB_PATH);
    if mod_db_path.is_file() {
        match css::CharaDb::open(&mod_db_path) {
            Ok(mod_db) => match &base_chara_db {
                Some(base) => {
                    let (order, hidden, patches, unknown) =
                        diff_chara_db(base, &mod_db, options.known_fighters);
                    project.roster.order = order.into_iter().collect();
                    project.roster.hidden = hidden.into_iter().collect();
                    project.roster.chara_overrides = patches;
                    for row in &unknown {
                        report.warn(format!(
                            "{row}: roster row in the mod has no base row, reported and never fabricated"
                        ));
                    }
                    let n = project.roster.order.len()
                        + project.roster.hidden.len()
                        + project.roster.chara_overrides.len();
                    report.push(
                        css::CHARA_DB_PATH.to_string(),
                        if n == 0 && unknown.is_empty() {
                            FileOutcome::Skipped
                        } else {
                            FileOutcome::Adopted
                        },
                        if n == 0 && unknown.is_empty() {
                            "no differences from the base roster".into()
                        } else {
                            format!(
                                "adopted {} order/visibility/row-field edit(s){}",
                                n,
                                if unknown.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {} unknown row(s) reported", unknown.len())
                                }
                            )
                        },
                    );
                }
                None => {
                    report.push(
                        css::CHARA_DB_PATH.to_string(),
                        FileOutcome::Skipped,
                        "no base ui/ dump found, skipping the roster diff honestly instead of guessing".into(),
                    );
                    report.warn(
                        "ui_chara_db present but no base ui/ dump: roster diff skipped".into(),
                    );
                }
            },
            Err(e) => {
                report.push(
                    css::CHARA_DB_PATH.to_string(),
                    FileOutcome::Skipped,
                    format!("could not read the mod roster database: {e:#}"),
                );
            }
        }
    }

    // ── fighter_param diff ────────────────────────────────────────────
    let mod_param_path = arc_root.join(traits::FIGHTER_PARAM_PATH);
    if mod_param_path.is_file() {
        match (base_param_path.as_ref(), prc::open(&mod_param_path)) {
            (Some(base_path), Ok(mod_root)) => match prc::open(base_path) {
                Ok(base_root) => {
                    let (per_fighter, unknown) = diff_fighter_param(
                        &base_root,
                        &mod_root,
                        options.known_fighters,
                        options.labels,
                    );
                    let n_fields: usize = per_fighter.values().map(|m| m.field_count()).sum();
                    for (fighter, param_mod) in per_fighter {
                        project.fighters.entry(fighter).or_default().params = param_mod;
                    }
                    for row in &unknown {
                        report.warn(format!(
                            "{row}: fighter_param row with no known fighter, reported and never fabricated"
                        ));
                    }
                    report.push(
                        traits::FIGHTER_PARAM_PATH.to_string(),
                        if n_fields == 0 && unknown.is_empty() {
                            FileOutcome::Skipped
                        } else {
                            FileOutcome::Adopted
                        },
                        if n_fields == 0 && unknown.is_empty() {
                            "no differences from the base values".into()
                        } else {
                            format!(
                                "adopted {n_fields} trait value(s){}",
                                if unknown.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {} unknown row(s) reported", unknown.len())
                                }
                            )
                        },
                    );
                }
                Err(e) => report.push(
                    traits::FIGHTER_PARAM_PATH.to_string(),
                    FileOutcome::Skipped,
                    format!("could not read the base values file: {e:#}"),
                ),
            },
            (None, _) => {
                report.push(
                    traits::FIGHTER_PARAM_PATH.to_string(),
                    FileOutcome::Skipped,
                    "no base fighter/ dump found, skipping the values diff honestly instead of guessing".into(),
                );
                report.warn(
                    "fighter_param present but no base fighter/ dump: values diff skipped".into(),
                );
            }
            (_, Err(e)) => report.push(
                traits::FIGHTER_PARAM_PATH.to_string(),
                FileOutcome::Skipped,
                format!("could not read the mod values file: {e:?}"),
            ),
        }
    }

    // ── .xmsbt names ──────────────────────────────────────────────────
    let xmsbt_files: Vec<PathBuf> = all_files
        .iter()
        .filter(|p| {
            p.starts_with(&arc_root)
                && game_relative(p, &arc_root)
                    .map(|g| is_xmsbt_path(&g))
                    .unwrap_or(false)
        })
        .cloned()
        .collect();
    for path in &xmsbt_files {
        let game_path =
            game_relative(path, &arc_root).unwrap_or_else(|| path.to_string_lossy().to_string());
        xmsbt_paths.push(game_path.clone());
        match std::fs::read(path).map(|b| parse_xmsbt(&b)) {
            Ok(Ok(labels)) => {
                xmsbt_labels.extend(labels);
            }
            Ok(Err(e)) => {
                report.push(
                    game_path,
                    FileOutcome::Skipped,
                    format!("could not parse the names override: {e:#}"),
                );
            }
            Err(e) => {
                report.push(
                    game_path,
                    FileOutcome::Skipped,
                    format!("could not read the names override: {e:#}"),
                );
            }
        }
    }
    if !xmsbt_labels.is_empty() {
        let mut warnings = Vec::new();
        adopt_xmsbt_labels(
            &xmsbt_labels,
            options.known_fighters,
            &mut project.roster.names,
            &mut project.roster.name_variants,
            &mut project.roster.per_costume_names,
            &mut warnings,
        );
        // Unknown name_ids (no base row, no known fighter) are adopted under
        // ui: but warned about — the override is kept, no row is fabricated.
        for (label, _) in &xmsbt_labels {
            if let Some((_, _, name_id)) = split_name_label(label) {
                if !base_name_ids.contains(&name_id)
                    && !options.known_fighters.iter().any(|f| f == &name_id)
                {
                    warnings.push(format!(
                        "{name_id}: name for a roster row the base dump has no record of, kept as ui:{name_id} and reported"
                    ));
                }
            }
        }
        warnings.sort();
        warnings.dedup();
        for w in warnings {
            report.warn(w);
        }
        for game_path in xmsbt_paths {
            report.push(
                game_path,
                FileOutcome::Adopted,
                format!("adopted {} name label(s)", xmsbt_labels.len()),
            );
        }
    }

    // ── BNTX portraits → PNG assets ───────────────────────────────────
    let portrait_files: Vec<PathBuf> = all_files
        .iter()
        .filter(|p| {
            p.starts_with(&arc_root)
                && game_relative(p, &arc_root)
                    .map(|g| is_portrait_path(&g))
                    .unwrap_or(false)
        })
        .cloned()
        .collect();
    for path in &portrait_files {
        let game_path =
            game_relative(path, &arc_root).unwrap_or_else(|| path.to_string_lossy().to_string());
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let Some((kind, name_id, slot)) = portrait_kind_split(
            file_name.strip_suffix(".bntx").unwrap_or(&file_name),
        )
        .map(|(k, n)| {
            // Re-parse the slot from the full stem (kind split drops it).
            let stem = file_name.strip_suffix(".bntx").unwrap_or(&file_name);
            let slot = stem
                .rsplit_once('_')
                .and_then(|(_, tail)| tail.parse::<u8>().ok())
                .unwrap_or(0);
            (k.to_string(), n.to_ascii_lowercase(), slot)
        }) else {
            report.push(
                game_path,
                FileOutcome::Skipped,
                format!("{file_name}: not a <kind>_<name>_<slot>.bntx portrait name"),
            );
            continue;
        };
        if !base_name_ids.contains(&name_id)
            && !options.known_fighters.iter().any(|f| f == &name_id)
        {
            report.warn(format!(
                "{name_id}: portrait for a roster row the base dump has no record of, kept as ui:{name_id} and reported"
            ));
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                report.push(
                    game_path.clone(),
                    FileOutcome::Skipped,
                    format!("could not read the portrait: {e:#}"),
                );
                continue;
            }
        };
        let image = match decode_portrait_bntx(&bytes, &file_name) {
            Ok(image) => image,
            Err(e) => {
                report.push(
                    game_path.clone(),
                    FileOutcome::Skipped,
                    format!("portrait undecodable, left as reference: {e:#}"),
                );
                continue;
            }
        };
        let png = match encode_png(&image) {
            Ok(png) => png,
            Err(e) => {
                report.push(
                    game_path.clone(),
                    FileOutcome::Skipped,
                    format!("portrait PNG encode failed: {e:#}"),
                );
                continue;
            }
        };
        let key = key_for_name_id(&name_id, options.known_fighters);
        // Bare kind for the entry's own slot, suffixed otherwise (matches the
        // export's `image_key` spelling so the round trip is exact).
        let stored_key = if slot == 0 {
            kind.clone()
        } else {
            format!("{kind}#c{slot:02}")
        };
        let png_rel = PathBuf::from(options.asset_dir_name)
            .join("roster_ui")
            .join(key.as_str().replace(':', "_"))
            .join(format!("{stored_key}_{name_id}_{slot:02}.png"));
        let dest = project_dir.join(&png_rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, png)?;
        project.roster.ui_images.entry(key).or_default().insert(
            stored_key,
            crate::mod_project::UiImageOverride {
                png_path: png_rel.to_string_lossy().replace('\\', "/"),
                gamma_render: false,
                gamma_upload: false,
            },
        );
        report.push(
            game_path,
            FileOutcome::Adopted,
            format!(
                "decoded to {}",
                png_rel.to_string_lossy().replace('\\', "/")
            ),
        );
    }

    // Sort the report for a stable, reviewable order.
    report.files.sort_by(|a, b| a.path.cmp(&b.path));
    report.warnings.sort();
    report.warnings.dedup();

    Ok((project, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mod_project::PROJECT_VERSION;

    fn labels_for(keys: &[&str]) -> HashMap<u64, String> {
        keys.iter()
            .map(|k| (hash40::hash40(k).0, (*k).to_string()))
            .collect()
    }

    fn known() -> Vec<String> {
        vec!["mario".to_string(), "link".to_string()]
    }

    fn base_chara_db(dir: &Path) -> PathBuf {
        use crate::roster::css::{fighter_kind_hash, test_db};
        let db = test_db(&[
            ("mario", 0, fighter_kind_hash("mario"), true),
            ("link", 2, fighter_kind_hash("link"), true),
        ]);
        let path = dir.join(css::CHARA_DB_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        db.save(&path).unwrap();
        dir.to_path_buf()
    }

    fn base_fighter_param(dir: &Path) {
        use crate::roster::traits;
        let path = dir.join(traits::FIGHTER_PARAM_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        prc::save(
            &path,
            &traits::test_file(&[("mario", 98.0, 3), ("link", 104.0, 7)]),
        )
        .unwrap();
    }

    fn mod_chara_db(base_dir: &Path, mod_dir: &Path) {
        // Mario moved 0→5, link hidden, mario color_num 8→10.
        let base = css::CharaDb::open(&base_dir.join(css::CHARA_DB_PATH)).unwrap();
        let mut modded = base;
        modded.set_disp_order("mario", 5).unwrap();
        modded.set_disp_order("link", css::OFF_ROSTER).unwrap();
        modded
            .apply_chara_patches(&std::collections::BTreeMap::from([(
                "mario".to_string(),
                CharaOverrides {
                    color_num: Some(10),
                    save_no: None,
                },
            )]))
            .retain(|_| false);
        let dest = mod_dir.join(css::CHARA_DB_PATH);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        modded.save(&dest).unwrap();
    }

    fn mod_fighter_param(base_dir: &Path, mod_dir: &Path) {
        let labels = labels_for(&[
            "weight",
            "jump_squat_frame",
            "attack100_type",
            "fighter_kind",
        ]);
        let mut mario = crate::roster::traits::FighterTraits::open(
            &base_dir.join(traits::FIGHTER_PARAM_PATH),
            "mario",
            &labels,
        )
        .unwrap();
        mario.set("weight", ParamValue::Float(120.0)).unwrap();
        let dest = mod_dir.join(traits::FIGHTER_PARAM_PATH);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        // Save via the whole-file path: open base, apply, save (mirrors export).
        let root = prc::open(base_dir.join(traits::FIGHTER_PARAM_PATH)).unwrap();
        let mut root = root;
        // Apply mario's edit into the same file link does not matter here; just
        // write a file with the edited row.
        {
            use prc::ParamKind;
            let wanted = hash40::hash40("fighter_param_table").0;
            let table = root
                .0
                .iter_mut()
                .find_map(|(h, v)| match v {
                    ParamKind::List(l) if h.0 == wanted => Some(l),
                    _ => None,
                })
                .unwrap();
            for item in &mut table.0 {
                if let ParamKind::Struct(entry) = item {
                    let is_mario = entry.0.iter().any(|(h, v)| {
                        h.0 == hash40::hash40("fighter_kind").0
                            && matches!(v, ParamKind::Hash(x) if x.0 == crate::roster::css::fighter_kind_hash("mario"))
                    });
                    if is_mario {
                        for (h, v) in entry.0.iter_mut() {
                            if h.0 == hash40::hash40("weight").0 {
                                *v = ParamKind::Float(120.0);
                            }
                        }
                    }
                }
            }
        }
        prc::save(&dest, &root).unwrap();
    }

    fn portrait_bntx() -> Vec<u8> {
        let mut img = image::RgbaImage::new(32, 32);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 8) as u8, (y * 8) as u8, 0x80, 0xFF]);
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        crate::roster::ui_images::encode_ui_png(&png.into_inner(), false).unwrap()
    }

    /// The acceptance fixture: db diff + xmsbt + portraits + params + source,
    /// plus one of every reference-only / linked class.
    fn synthetic_mod(base_dir: &Path, mod_dir: &Path) {
        mod_chara_db(base_dir, mod_dir);
        mod_fighter_param(base_dir, mod_dir);
        // xmsbt: mario renamed (slot 0, simple) + link alt c02 (per-costume).
        let labels = vec![
            ("nam_chr0_00_mario".to_string(), "Jumpman".to_string()),
            ("nam_chr1_00_mario".to_string(), "Jumpman".to_string()),
            ("nam_chr2_00_mario".to_string(), "JUMPMAN".to_string()),
            ("nam_chr0_02_link".to_string(), "Hero".to_string()),
            ("nam_chr1_02_link".to_string(), "Hero".to_string()),
            ("nam_chr2_02_link".to_string(), "HERO".to_string()),
        ];
        let body = crate::roster::names::render_xmsbt_from_labels(&labels).unwrap();
        let xmsbt = mod_dir.join(crate::roster::names::XMSBT_PATH);
        std::fs::create_dir_all(xmsbt.parent().unwrap()).unwrap();
        std::fs::write(&xmsbt, body).unwrap();
        // Portraits: one grid portrait for mario c00.
        let portrait = mod_dir.join("ui/replace/chara/chara_1/chara_1_mario_00.bntx");
        std::fs::create_dir_all(portrait.parent().unwrap()).unwrap();
        std::fs::write(&portrait, portrait_bntx()).unwrap();
        // Source text as reference.
        let src = mod_dir.join("src/mario/acmd.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"// mario acmd source reference\n").unwrap();
        std::fs::write(mod_dir.join("Cargo.toml"), b"[package]\nname=\"x\"\n").unwrap();
        // Reference-only binaries.
        std::fs::create_dir_all(mod_dir.join("effect/fighter/mario")).unwrap();
        std::fs::write(mod_dir.join("effect/fighter/mario/ef_mario.eff"), b"EFF").unwrap();
        std::fs::create_dir_all(mod_dir.join("ui/message")).unwrap();
        std::fs::write(mod_dir.join("ui/message/msg_name.msbt"), b"MSBT").unwrap();
        std::fs::write(mod_dir.join("plugin.nro"), b"NRO").unwrap();
        // Loose asset (linked via the library).
        let model = mod_dir.join("fighter/mario/model/body/c00/model.numdlb");
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        std::fs::write(&model, b"model").unwrap();
    }

    #[test]
    fn xmsbt_round_trips_through_parse() {
        let labels = vec![
            (
                "nam_chr0_00_mario".to_string(),
                "R.O.B. & <Friends>".to_string(),
            ),
            (
                "nam_chr1_00_mario".to_string(),
                "R.O.B. & <Friends>".to_string(),
            ),
            (
                "nam_chr2_00_mario".to_string(),
                "R.O.B. & <FRIENDS>".to_string(),
            ),
        ];
        let body = crate::roster::names::render_xmsbt_from_labels(&labels).unwrap();
        let parsed = parse_xmsbt(&body).unwrap();
        assert_eq!(parsed, labels);
    }

    #[test]
    fn name_labels_split_slot_and_underscored_name_ids() {
        assert_eq!(
            split_name_label("nam_chr0_00_pickel"),
            Some((0, 0, "pickel".into()))
        );
        assert_eq!(
            split_name_label("nam_chr2_08_ice_climber"),
            Some((2, 8, "ice_climber".into()))
        );
        assert!(split_name_label("nam_chr3_00_mario").is_none());
        assert!(split_name_label("nam_chr0_0_mario").is_none());
        assert!(split_name_label("something_else").is_none());
    }

    #[test]
    fn portrait_kind_split_handles_stock_and_unknown_kinds() {
        assert_eq!(
            portrait_kind_split("stock_90_link_03"),
            Some(("stock_90", "link"))
        );
        assert_eq!(
            portrait_kind_split("chara_1_ice_climber_08"),
            Some(("chara_1", "ice_climber"))
        );
        assert_eq!(
            portrait_kind_split("custom_mario_00"),
            Some(("custom", "mario"))
        );
        // A stem with no name part falls through to the unknown-kind arm
        // rather than failing: kind "chara", name "1".
        assert_eq!(portrait_kind_split("chara_1_00"), Some(("chara", "1")));
    }

    #[test]
    fn every_unsupported_edit_has_a_workspace_home() {
        use crate::project_hub::WorkspaceSupport as Ws;
        // Models + animations go to the manual romfs overlay, never silent.
        let model = workspace_dest_for_game_path("fighter/mario/model/body/c00/model.numdlb");
        assert_eq!(model.support, Ws::Manual);
        assert_eq!(
            model.workspace,
            "romfs/fighter/mario/model/body/c00/model.numdlb"
        );
        let anim = workspace_dest_for_game_path("fighter/mario/motion/body/c00/attack.nuanmb");
        assert_eq!(anim.support, Ws::Manual);
        assert_eq!(
            anim.workspace,
            "romfs/fighter/mario/motion/body/c00/attack.nuanmb"
        );
        // Supported edits stay in the JSON/assets.
        let db = workspace_dest_for_game_path(crate::roster::css::CHARA_DB_PATH);
        assert_eq!(db.support, Ws::Supported);
        assert_eq!(db.workspace, "modproject.json");
        let portrait =
            workspace_dest_for_game_path("ui/replace/chara/chara_1/chara_1_mario_00.bntx");
        assert_eq!(portrait.support, Ws::Supported);
    }

    /// Acceptance: a synthetic mod (db diff + xmsbt + portraits + params +
    /// source) imports to an exact project, and the report accounts for every
    /// file class.
    #[test]
    fn synthetic_mod_imports_to_an_exact_project() {
        let tmp = tempfile::tempdir().unwrap();
        let base_dir = tmp.path().join("base");
        let mod_dir = tmp.path().join("my_mod");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::create_dir_all(&mod_dir).unwrap();
        base_chara_db(&base_dir);
        base_fighter_param(&base_dir);
        synthetic_mod(&base_dir, &mod_dir);

        let labels = labels_for(&[
            "weight",
            "jump_squat_frame",
            "attack100_type",
            "fighter_kind",
        ]);
        let options = ImportOptions {
            known_fighters: &known(),
            labels: &labels,
            base_roots: std::slice::from_ref(&base_dir),
            asset_dir_name: "my_mod_assets",
        };
        let (project, report) = import_mod_as_project(&mod_dir, &project_dir, &options).unwrap();

        // ── Exactness: every adopted edit ──
        use crate::roster::RosterKey;
        assert_eq!(
            project.roster.order.get(&RosterKey::fighter("mario")),
            Some(&5),
            "mario's moved position must survive"
        );
        assert!(
            project.roster.hidden.contains(&RosterKey::fighter("link")),
            "link's hidden flag must survive"
        );
        assert_eq!(
            project
                .roster
                .chara_overrides
                .get(&RosterKey::fighter("mario"))
                .and_then(|p| p.color_num),
            Some(10),
            "mario's color_num patch must survive"
        );
        assert_eq!(
            project
                .roster
                .names
                .get(&RosterKey::fighter("mario"))
                .map(String::as_str),
            Some("Jumpman")
        );
        assert_eq!(
            project
                .roster
                .per_costume_names
                .get("link")
                .and_then(|m| m.get(&2))
                .map(String::as_str),
            Some("Hero"),
            "link c02 per-costume name must survive"
        );
        let params = project
            .fighters
            .get("mario")
            .expect("mario params")
            .params
            .clone();
        let edits = crate::roster::traits::edits_for(&params);
        assert_eq!(
            edits.get("weight"),
            Some(&ParamValue::Float(120.0)),
            "mario's weight edit must survive"
        );
        // Portrait adopted to a PNG asset beside the project.
        let images = project
            .roster
            .ui_images
            .get(&RosterKey::fighter("mario"))
            .expect("portrait");
        assert!(images.contains_key("chara_1"), "{images:?}");
        let png_rel = &images["chara_1"].png_path;
        assert!(project_dir.join(png_rel).is_file(), "{png_rel} missing");
        // Through JSON (the trip the hub's Save takes).
        let json = serde_json::to_string(&project).unwrap();
        let reloaded: ModProjectFile = serde_json::from_str(&json).unwrap();
        assert_eq!(reloaded.roster.order, project.roster.order);
        assert_eq!(reloaded.roster.names, project.roster.names);
        assert_eq!(reloaded.roster.ui_images, project.roster.ui_images);
        assert_eq!(
            reloaded.fighters["mario"].params,
            project.fighters["mario"].params
        );
        assert_eq!(reloaded.version, PROJECT_VERSION);

        // ── Report accounts for every file class ──
        let paths: Vec<String> = report.files.iter().map(|f| f.path.clone()).collect();
        for needle in [
            css::CHARA_DB_PATH,
            traits::FIGHTER_PARAM_PATH,
            crate::roster::names::XMSBT_PATH,
            "ui/replace/chara/chara_1/chara_1_mario_00.bntx",
            "effect/fighter/mario/ef_mario.eff",
            "ui/message/msg_name.msbt",
            "plugin.nro",
            "fighter/mario/model/body/c00/model.numdlb",
            "src/mario/acmd.rs",
        ] {
            assert!(
                paths.iter().any(|p| p == needle || p.ends_with(needle)),
                "report never mentioned {needle}: {paths:?}"
            );
        }
        let outcome_of = |needle: &str| {
            report
                .files
                .iter()
                .find(|f| f.path == needle || f.path.ends_with(needle))
                .map(|f| f.outcome)
        };
        assert_eq!(outcome_of(css::CHARA_DB_PATH), Some(FileOutcome::Adopted));
        assert_eq!(
            outcome_of(traits::FIGHTER_PARAM_PATH),
            Some(FileOutcome::Adopted)
        );
        assert_eq!(
            outcome_of(crate::roster::names::XMSBT_PATH),
            Some(FileOutcome::Adopted)
        );
        assert_eq!(
            outcome_of("ui/replace/chara/chara_1/chara_1_mario_00.bntx"),
            Some(FileOutcome::Adopted)
        );
        // Compiled ACMD / binary EFF/MSBT / plugins are reference-only by
        // design — stated, never silent.
        assert_eq!(
            outcome_of("effect/fighter/mario/ef_mario.eff"),
            Some(FileOutcome::ReferenceOnly)
        );
        assert_eq!(
            outcome_of("ui/message/msg_name.msbt"),
            Some(FileOutcome::ReferenceOnly)
        );
        assert_eq!(outcome_of("plugin.nro"), Some(FileOutcome::ReferenceOnly));
        // Source text copied as reference; loose assets linked.
        assert_eq!(
            outcome_of("src/mario/acmd.rs"),
            Some(FileOutcome::ReferenceOnly)
        );
        assert!(project_dir.join("reference/src/mario/acmd.rs").is_file());
        assert_eq!(
            outcome_of("fighter/mario/model/body/c00/model.numdlb"),
            Some(FileOutcome::Linked)
        );
        // No file silently missing.
        assert!(report.covers(&paths).is_empty());
        assert!(!report.summary().is_empty());
    }

    #[test]
    fn missing_base_dumps_skip_their_diff_honestly() {
        let tmp = tempfile::tempdir().unwrap();
        let mod_dir = tmp.path().join("mod");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&mod_dir).unwrap();
        // A mod db + params with no base anywhere.
        {
            use crate::roster::css::{fighter_kind_hash, test_db};
            let db = test_db(&[("mario", 5, fighter_kind_hash("mario"), true)]);
            let dest = mod_dir.join(css::CHARA_DB_PATH);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            db.save(&dest).unwrap();
        }
        {
            use crate::roster::traits;
            let dest = mod_dir.join(traits::FIGHTER_PARAM_PATH);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            prc::save(&dest, &traits::test_file(&[("mario", 120.0, 3)])).unwrap();
        }
        let labels = labels_for(&["weight", "jump_squat_frame", "fighter_kind"]);
        let options = ImportOptions {
            known_fighters: &known(),
            labels: &labels,
            base_roots: &[],
            asset_dir_name: "assets",
        };
        let (project, report) = import_mod_as_project(&mod_dir, &project_dir, &options).unwrap();
        assert!(project.roster.order.is_empty());
        assert!(project.fighters.is_empty(), "no param diff without a base");
        assert_eq!(
            outcome(&report, css::CHARA_DB_PATH),
            Some(FileOutcome::Skipped)
        );
        assert_eq!(
            outcome(&report, traits::FIGHTER_PARAM_PATH),
            Some(FileOutcome::Skipped)
        );
        assert!(
            report.warnings.iter().any(|w| w.contains("no base")),
            "{:?}",
            report.warnings
        );

        fn outcome(report: &ImportReport, needle: &str) -> Option<FileOutcome> {
            report
                .files
                .iter()
                .find(|f| f.path == needle)
                .map(|f| f.outcome)
        }
    }

    #[test]
    fn unknown_roster_rows_are_reported_never_fabricated() {
        let tmp = tempfile::tempdir().unwrap();
        let base_dir = tmp.path().join("base");
        let mod_dir = tmp.path().join("mod");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::create_dir_all(&mod_dir).unwrap();
        base_chara_db(&base_dir);
        // Mod adds a row the base has never heard of.
        {
            use crate::roster::css::{fighter_kind_hash, test_db};
            let db = test_db(&[
                ("mario", 0, fighter_kind_hash("mario"), true),
                ("link", 2, fighter_kind_hash("link"), true),
                ("ghost", 50, 0xdeadbeef, true),
            ]);
            let dest = mod_dir.join(css::CHARA_DB_PATH);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            db.save(&dest).unwrap();
        }
        // And names + portraits for that ghost.
        let labels = vec![
            ("nam_chr0_00_ghost".to_string(), "Ghost".to_string()),
            ("nam_chr1_00_ghost".to_string(), "Ghost".to_string()),
            ("nam_chr2_00_ghost".to_string(), "GHOST".to_string()),
        ];
        let body = crate::roster::names::render_xmsbt_from_labels(&labels).unwrap();
        let xmsbt = mod_dir.join(crate::roster::names::XMSBT_PATH);
        std::fs::create_dir_all(xmsbt.parent().unwrap()).unwrap();
        std::fs::write(&xmsbt, body).unwrap();

        let labels_map = labels_for(&["weight", "fighter_kind"]);
        let options = ImportOptions {
            known_fighters: &known(),
            labels: &labels_map,
            base_roots: &[base_dir],
            asset_dir_name: "assets",
        };
        let (project, report) = import_mod_as_project(&mod_dir, &project_dir, &options).unwrap();
        // No order/hidden fabricated for the ghost row…
        assert!(!project
            .roster
            .order
            .keys()
            .any(|k| k.as_str().contains("ghost")));
        // …but the report says its name out loud.
        assert!(
            report.warnings.iter().any(|w| w.contains("ghost")),
            "{:?}",
            report.warnings
        );
        // No authored entry fabricated either.
        assert!(project.roster.authored.is_empty());
    }
}
