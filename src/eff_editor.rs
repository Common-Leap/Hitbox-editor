// Eff editor: browse ef_*.eff game dumps, edit authored emitter values against a pristine
// snapshot, and preview the edit live in game through the slight_replica plugin.
//
// HOW AUTHORED EDITS REACH THE GAME
// Authored PTCL values are PER EMITTER. The runtime modifier protocol is not: the plugin's
// `rainbow.color` / `scale` are pinned on an `EffectData` for a whole effect KIND, so there
// is no wire message that can say "tint only emitter 3". Deriving a multiplier from the
// authored delta therefore cannot be made correct — the old code averaged every emitter's
// colour ratio into one kind-level value, which recoloured the ENTIRE effect whenever you
// edited a single emitter (and silently ignored color1 edits entirely).
//
// So authored edits do NOT go on the modifier wire at all. They are baked into the LIVE
// CARRIER: the edited effect is cloned into a borrowed assist eff, the edits are written into
// that clone (per-emitter exact, the same writer the exporter uses), and the original kind is
// aliased onto it. The fighter's own eff is not a candidate — `reparse_game_path` rebuilds the
// parsed structs from the resident buffer and never re-requests the file, so edited bytes went
// unread mid-match. The editor only raises a request flag; the app owns the rebuild.
//
// Transplants, per-emitter texture swaps and whole-texture PNG imports all ride that same
// rebuild, so one Send ships everything the project says.
//
// The kind-level multipliers are still exposed, but only as their own honest feature: the
// "Kind look — color × / speed ×" panel, which the user drives directly and which is
// documented as applying to every spawn of the effect.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::Ui;

use crate::effects::{load_effect, ColorKey, EmitterDef, PtclFile, TextureInfo};
use crate::game_link::{GameLink, LinkStatus};
use crate::mod_project::{AuthoredEdit, EmitterFieldEdits, TransplantOp};

/// Everything that decides what the texture thumbnail should show: the eff, the pool index, the
/// replacement PNG's path and modification time, and whether raw form is selected.
///
/// Two parts of this are easy to miss. The mtime: exporting to `star.png`, painting it, and
/// importing `star.png` again leaves the path identical, so without it the panel would keep
/// showing the previous import. And the pool generation, because adding or removing a texture
/// changes what an INDEX means without changing the eff or the file on disk.
type TexturePreviewKey = (PathBuf, usize, String, bool, u128, u64);

/// Pristine copy of the editable authored fields of one emitter.
#[derive(Clone)]
struct EmitterSnapshot {
    emission_rate: f32,
    lifetime: f32,
    scale: f32,
    color_scale: f32,
    emitter_scale: glam::Vec3,
    color0: Vec<ColorKey>,
    color1: Vec<ColorKey>,
    alpha0_keys: Vec<ColorKey>,
    texture_index: Option<u32>,
}

impl EmitterSnapshot {
    fn of(e: &EmitterDef) -> Self {
        Self {
            emission_rate: e.emission_rate,
            lifetime: e.lifetime,
            scale: e.scale,
            color_scale: e.color_scale,
            emitter_scale: e.emitter_scale,
            color0: e.color0.clone(),
            color1: e.color1.clone(),
            alpha0_keys: e.alpha0_keys.clone(),
            texture_index: e.texture_index,
        }
    }

    fn restore(&self, e: &mut EmitterDef) {
        e.emission_rate = self.emission_rate;
        e.lifetime = self.lifetime;
        e.scale = self.scale;
        e.color_scale = self.color_scale;
        e.emitter_scale = self.emitter_scale;
        e.color0 = self.color0.clone();
        e.color1 = self.color1.clone();
        e.alpha0_keys = self.alpha0_keys.clone();
        e.texture_index = self.texture_index;
    }
}

/// One named effect entry inside the loaded .eff (name → emitter set).
struct EffEntry {
    name: String,
    /// hash40 of the name — the plugin's kind-tab id.
    hash: u64,
    set_idx: usize,
}

/// One edited fighter (from the app's project state) — the primary unit the editor's
/// selector is organized around. `merged` is the transplants-applied build to parse when
/// present; `alt_base` covers the data-root path when it differs from the export root.
#[derive(Clone, PartialEq)]
pub struct EditSource {
    pub fighter: String,
    pub base: PathBuf,
    pub alt_base: Option<PathBuf>,
    pub merged: Option<PathBuf>,
    pub transplant_count: usize,
    pub authored: usize,
    /// How many of this eff's pool textures the user has replaced with their own image.
    pub textures: usize,
    pub transplants: Vec<EffTransplant>,
}

/// Project metadata for one entry shown in the merged EFF view.
#[derive(Clone, PartialEq)]
pub struct EffTransplant {
    pub op_index: usize,
    /// The entry visible in the editor (the new name, or replacement target).
    pub entry_name: String,
    pub donor_name: String,
    pub donor_file: String,
    pub one_slot_slots: Vec<u8>,
}

#[derive(Clone)]
pub struct EffTransplantRemoval {
    pub fighter: String,
    pub op_index: usize,
    pub entry_name: String,
    pub donor_name: String,
}

pub struct EffEditor {
    pub open: bool,
    /// Eff path queued by the main editor (fighter selection) — loaded when the window is
    /// (or becomes) open, so closed-window fighter browsing stays cheap.
    pending_load: Option<PathBuf>,
    /// Entry name (lowercase) to select after the next load — transplant hand-off.
    pending_select: Option<String>,
    /// Authored edits applied once the queued eff loads.
    pending_edits: Option<Vec<AuthoredEdit>>,
    /// Whether applying [`Self::pending_edits`] should also push them to the running game.
    ///
    /// True when a project was just opened — the point is to get the game showing it. False
    /// when the reload is one we caused ourselves (a transplant rebuild): the edits are being
    /// carried across, not newly requested, and the user has not asked to send anything.
    pending_edits_push_live: bool,
    export_root: PathBuf,
    eff_files: Vec<PathBuf>,
    file_filter: String,
    scan_error: Option<String>,

    loaded_path: Option<PathBuf>,
    /// Base eff path → merged (transplants applied) file to ACTUALLY parse when that base
    /// is opened. Makes the merged view character-centric: picking ef_kirby.eff shows
    /// kirby WITH its transplants, wherever it's opened from.
    merged_overlays: std::collections::HashMap<PathBuf, PathBuf>,
    /// The currently loaded view came from a merged overlay (shown in the header).
    loaded_is_merged: bool,
    /// The project's edited fighters — the PRIMARY things this window edits. Fed by the
    /// app every frame; drives the "Editing:" selector and the overlay map. Base game
    /// files are only a reference source below it.
    edit_sources: Vec<EditSource>,
    load_error: Option<String>,
    ptcl: Option<PtclFile>,
    /// The loaded eff's BNTX pool, verbatim — what "Export PNG" decodes from. `ptcl` carries
    /// only what each texture IS (name, size, format), never its pixels.
    texture_pool: Option<Vec<u8>>,
    /// Texture replacements the user picked, drained by the app into the project store.
    pending_texture_imports: Vec<crate::mod_project::TextureImport>,
    /// Pool textures the user added, drained by the app into the project store.
    pending_texture_additions: Vec<crate::mod_project::TextureAddition>,
    /// Names of pool textures the user removed, drained by the app.
    pending_texture_removals: Vec<String>,
    /// Bumped whenever `texture_pool` is rebuilt in place. Part of the preview cache key: adding
    /// or removing a texture changes what an index means without changing the path or the file.
    texture_pool_gen: u64,
    /// The project's texture replacements for the loaded eff, fed by the app each frame so
    /// the texture panel can show what is already replaced.
    texture_imports: Vec<crate::mod_project::TextureImport>,
    /// Result of the last export/import, shown beside the buttons. Errors here are the whole
    /// point: an unconvertible format or an unreadable PNG has to say so, not do nothing.
    texture_note: Option<(String, bool)>,
    /// Pool index + info of the texture the selected emitter samples, published by the emitter
    /// column for the texture panel on the right.
    selected_texture: Option<(usize, TextureInfo)>,
    /// Decoded thumbnail for the selected texture. `None` in the second slot means "tried and
    /// could not" — kept so a failing texture is not re-decoded every frame.
    texture_preview: Option<(TexturePreviewKey, Option<egui::TextureHandle>)>,
    /// Show textures as the stored channels rather than in editable form.
    show_raw_textures: bool,
    /// Plain-English note about the selected texture's layout, refreshed with the thumbnail.
    /// Cached because working it out means decoding the texture, which must not happen per frame.
    texture_form_note: String,
    /// Pristine snapshots per emitter set, parallel to `ptcl.emitter_sets`.
    pristine: Vec<Vec<EmitterSnapshot>>,
    entries: Vec<EffEntry>,
    entry_filter: String,
    selected_entry: Option<usize>,
    selected_emitter: usize,

    // Transplant state
    /// The main editor's selected fighter — target for transplant ops (set by the app).
    target_fighter: Option<String>,
    /// Recorded transplant ops, drained by the app into the project store.
    pending_transplants: Vec<TransplantOp>,
    /// Entry-level removals requested from the EFF editor, drained by the app so it can
    /// rebuild project state, live aliases, and the runtime carrier atomically.
    pending_transplant_removals: Vec<EffTransplantRemoval>,

    // Live-preview state
    eff_dirty_at: Option<Instant>,
    /// Raised when the authored edits need to reach the running game. The app drains it
    /// (`take_live_deploy_request`) and services it by rebuilding this fighter's eff and
    /// hot-reloading it — the only per-emitter-exact path there is.
    live_deploy_request: bool,
    /// When the app last serviced a deploy request — drives `REDEPLOY_MIN_GAP_MS`.
    live_deploy_at: Option<Instant>,
    /// True from clicking Send until the app has finished building + sending the carrier.
    /// Drives the spinner beside the button.
    pub sending: bool,
    /// Carrier generation that was live in game when the current send started. The send is
    /// complete only once the game reports a NEWER one — see `awaiting_game`.
    pub gen_at_send: u64,
    /// Set once the snapshot has left the app: we are now waiting for the GAME to take it
    /// (bytes decompress, resource service settles, carrier object comes up). Cleared when
    /// the plugin reports the carrier live, or on timeout.
    pub awaiting_game: Option<Instant>,
    /// Last carrier report from the plugin, shown while waiting: (state, kinds, spawned).
    /// Surfaced rather than hidden because "the spinner cleared too early" has been
    /// diagnosed by guesswork twice now, and the raw signal settles it.
    pub carrier_report: (u8, usize, bool),
    /// Carrier generation the game last reported. Compared against `gen_at_send` to tell a
    /// freshly-taken carrier from the previous one still sitting there reporting ready.
    pub carrier_gen_now: u64,
    /// Final carrier reading from the last send, kept on screen after the spinner clears.
    pub last_carrier_result: Option<String>,
    /// Whether that final reading was a success — colours the persisted readout so a failed
    /// send is not a grey line that reads like a status message.
    pub carrier_ok: bool,
    /// The carrier rebuild is synchronous on the UI thread, so servicing the request in the
    /// same frame as the click would block before the spinner ever painted. Hold the request
    /// for one frame so the "sending" state is visible.
    deploy_defer: bool,
    last_sent_note: Option<String>,
}

impl Default for EffEditor {
    fn default() -> Self {
        Self {
            open: false,
            pending_load: None,
            pending_select: None,
            pending_edits: None,
            pending_edits_push_live: false,
            export_root: std::env::current_dir().unwrap_or_default(),
            eff_files: Vec::new(),
            file_filter: String::new(),
            scan_error: None,
            loaded_path: None,
            merged_overlays: std::collections::HashMap::new(),
            loaded_is_merged: false,
            edit_sources: Vec::new(),
            load_error: None,
            ptcl: None,
            texture_pool: None,
            pending_texture_imports: Vec::new(),
            pending_texture_additions: Vec::new(),
            pending_texture_removals: Vec::new(),
            texture_pool_gen: 0,
            texture_imports: Vec::new(),
            texture_note: None,
            selected_texture: None,
            texture_preview: None,
            show_raw_textures: false,
            texture_form_note: String::new(),
            pristine: Vec::new(),
            entries: Vec::new(),
            entry_filter: String::new(),
            selected_entry: None,
            selected_emitter: 0,
            target_fighter: None,
            pending_transplants: Vec::new(),
            pending_transplant_removals: Vec::new(),
            eff_dirty_at: None,
            live_deploy_request: false,
            live_deploy_at: None,
            sending: false,
            gen_at_send: 0,
            awaiting_game: None,
            carrier_report: (0, 0, false),
            carrier_gen_now: 0,
            last_carrier_result: None,
            carrier_ok: false,
            deploy_defer: false,
            last_sent_note: None,
        }
    }
}

fn scan_eff_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_eff_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("eff") {
            // Transient transplant preview files aren't real sources — hide them.
            let is_preview = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| crate::scratch_dirs::is_transplant_preview_name(n))
                .unwrap_or(false);
            if !is_preview {
                out.push(path);
            }
        }
    }
}

fn diff(a: f32, b: f32) -> bool {
    (a - b).abs() > 1e-3
}

/// Which authored fields of ONE emitter differ from its pristine snapshot, as the
/// absolute-value edit record the exporter/rebuilder consumes. Single source of truth for
/// "is this emitter edited?" (`is_empty`) and for what to write (`collect_authored_edits`),
/// so the UI's edited-emitter list can never disagree with what actually ships.
fn field_edits(em: &EmitterDef, pr: &EmitterSnapshot, tex_names: &[String]) -> EmitterFieldEdits {
    let mut f = EmitterFieldEdits::default();
    // Swaps are recorded by texture NAME. The picker works in pool indices, but the index is
    // only meaningful against the file loaded right now — see `EmitterFieldEdits::texture_name`.
    if em.texture_index != pr.texture_index {
        f.texture_name = em
            .texture_index
            .and_then(|i| tex_names.get(i as usize))
            .cloned();
    }
    if diff(em.emission_rate, pr.emission_rate) {
        f.emission_rate = Some(em.emission_rate);
    }
    if diff(em.lifetime, pr.lifetime) {
        f.lifetime = Some(em.lifetime);
    }
    if diff(em.scale, pr.scale) {
        f.scale = Some(em.scale);
    }
    if diff(em.color_scale, pr.color_scale) {
        f.color_scale = Some(em.color_scale);
    }
    if diff(em.emitter_scale.x, pr.emitter_scale.x)
        || diff(em.emitter_scale.y, pr.emitter_scale.y)
        || diff(em.emitter_scale.z, pr.emitter_scale.z)
    {
        f.emitter_scale = Some([em.emitter_scale.x, em.emitter_scale.y, em.emitter_scale.z]);
    }
    let keys_differ = |a: &[ColorKey], b: &[ColorKey]| {
        a.len() != b.len()
            || a.iter()
                .zip(b)
                .any(|(x, y)| diff(x.r, y.r) || diff(x.g, y.g) || diff(x.b, y.b))
    };
    if keys_differ(&em.color0, &pr.color0) {
        f.color0 = Some(em.color0.iter().map(|k| [k.r, k.g, k.b, k.frame]).collect());
    }
    if keys_differ(&em.color1, &pr.color1) {
        f.color1 = Some(em.color1.iter().map(|k| [k.r, k.g, k.b, k.frame]).collect());
    }
    if em.alpha0_keys.len() != pr.alpha0_keys.len()
        || em
            .alpha0_keys
            .iter()
            .zip(&pr.alpha0_keys)
            .any(|(x, y)| diff(x.r, y.r))
    {
        f.alpha0 = Some(em.alpha0_keys.iter().map(|k| [k.r, k.frame]).collect());
    }
    f
}

impl EffEditor {
    // ── Loading ───────────────────────────────────────────────────────────────

    pub fn rescan(&mut self) {
        self.eff_files.clear();
        self.scan_error = None;
        let effect_dir = self.export_root.join("effect");
        let root = if effect_dir.is_dir() {
            effect_dir
        } else {
            self.export_root.clone()
        };
        scan_eff_files(&root, &mut self.eff_files);
        self.eff_files.sort();
        if self.eff_files.is_empty() {
            self.scan_error = Some(format!("no .eff files under {}", root.display()));
        }
    }

    fn load_eff(&mut self, path: &Path) {
        self.load_error = None;
        self.ptcl = None;
        self.pristine.clear();
        self.entries.clear();
        self.selected_entry = None;
        self.selected_emitter = 0;
        // `eff_dirty_at` deliberately SURVIVES a load: it tracks project-vs-game divergence,
        // not the working copy. Clearing it here silently undid the `mark_unsent` a transplant
        // had just done, because recording one reloads the eff from the merged preview — so the
        // Send button went back to grey the frame after a transplant lit it. It is cleared where
        // the divergence actually ends, in `request_live_apply`.

        // Character-centric view: the fighter's base eff transparently resolves to its
        // MERGED (transplants applied) file when one exists — the new/replaced entries
        // show up and are editable no matter how the file was opened.
        let overlay = self
            .merged_overlays
            .get(path)
            .filter(|m| m.exists())
            .cloned();
        self.loaded_is_merged = overlay.is_some();
        let actual: PathBuf = overlay.unwrap_or_else(|| path.to_path_buf());

        let loaded = match load_effect(&actual) {
            Ok(effect) => effect,
            Err(e) => {
                self.load_error = Some(e.to_string());
                return;
            }
        };
        let index = loaded.index;
        let ptcl = loaded.ptcl;

        self.pristine = ptcl
            .emitter_sets
            .iter()
            .map(|set| set.emitters.iter().map(EmitterSnapshot::of).collect())
            .collect();

        // Handles carry an original-case and a lowercase duplicate — keep one per lowercase
        // name. hash40 in ACMD is over the lowercase name, which is what the plugin reports.
        let mut seen = std::collections::HashSet::new();
        let mut entries: Vec<EffEntry> = Vec::new();
        for (name, set_idx) in &index.handles {
            let lower = name.to_lowercase();
            if !seen.insert(lower.clone()) {
                continue;
            }
            let idx = *set_idx;
            if idx < 0 || idx as usize >= ptcl.emitter_sets.len() {
                continue;
            }
            entries.push(EffEntry {
                hash: hash40::hash40(&lower).0,
                name: lower,
                set_idx: idx as usize,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        self.entries = entries;
        self.ptcl = Some(ptcl);
        self.texture_pool = loaded.texture_pool;
        self.texture_note = None;
        self.loaded_path = Some(path.to_path_buf());
    }

    // ── Project integration ───────────────────────────────────────────────────

    /// Loaded eff path relative to the export root (project `source_rel`).
    pub fn loaded_rel(&self) -> Option<String> {
        let path = self.loaded_path.as_ref()?;
        let rel = path.strip_prefix(&self.export_root).unwrap_or(path);
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    pub fn export_root(&self) -> &Path {
        &self.export_root
    }

    pub fn set_export_root(&mut self, root: PathBuf) {
        if self.export_root != root {
            self.export_root = root;
            self.rescan();
        }
    }

    /// Queue authored edits to apply after the next queued eff loads (project load path).
    pub fn queue_edits(&mut self, edits: Vec<AuthoredEdit>) {
        self.pending_edits = Some(edits);
        self.pending_edits_push_live = true;
    }

    pub fn set_target_fighter(&mut self, fighter: Option<String>) {
        self.target_fighter = fighter;
    }

    /// Transplant ops recorded since the last drain (the app owns the project store).
    pub fn take_transplants(&mut self) -> Vec<TransplantOp> {
        std::mem::take(&mut self.pending_transplants)
    }

    pub fn take_transplant_removals(&mut self) -> Vec<EffTransplantRemoval> {
        std::mem::take(&mut self.pending_transplant_removals)
    }

    /// Texture replacements picked since the last drain (the app owns the project store).
    pub fn take_texture_imports(&mut self) -> Vec<crate::mod_project::TextureImport> {
        std::mem::take(&mut self.pending_texture_imports)
    }

    /// The project's texture replacements for the loaded eff, so the panel can show them.
    pub fn set_texture_imports(&mut self, imports: Vec<crate::mod_project::TextureImport>) {
        self.texture_imports = imports;
    }

    /// Pool textures added since the last drain (the app owns the project store).
    pub fn take_texture_additions(&mut self) -> Vec<crate::mod_project::TextureAddition> {
        std::mem::take(&mut self.pending_texture_additions)
    }

    /// Pool textures removed since the last drain.
    pub fn take_texture_removals(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_texture_removals)
    }

    /// How many of the loaded eff's emitters sample pool texture `index`.
    ///
    /// Counts sampler0 ONLY, because that is all the editor's model carries — `convert_emitter`
    /// resolves `sampler0` and discards the other five. So this is a lower bound: a texture used
    /// only as a distortion or mask input reads as unused here. The export-side removal pass
    /// checks all six and refuses with a `texture-in-use` warning, which is the backstop.
    fn texture_users(&self, index: usize) -> usize {
        self.ptcl
            .as_ref()
            .map(|p| {
                p.emitter_sets
                    .iter()
                    .flat_map(|set| set.emitters.iter())
                    .filter(|em| em.texture_index == Some(index as u32))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Point the selected emitter at pool texture `index`.
    fn point_selected_emitter_at(&mut self, index: usize) {
        if let (Some(entry_idx), Some(ptcl)) = (self.selected_entry, self.ptcl.as_mut()) {
            let set_idx = self.entries[entry_idx].set_idx;
            if let Some(em) = ptcl
                .emitter_sets
                .get_mut(set_idx)
                .and_then(|s| s.emitters.get_mut(self.selected_emitter))
            {
                em.texture_index = Some(index as u32);
                self.eff_dirty_at = Some(Instant::now());
            }
        }
    }

    /// Adopt a rebuilt pool that has one texture appended, and point the emitter at it.
    ///
    /// The editor's own view has to move with the pool or the panel lies: the label list, the
    /// preview and the index a swap edit records all read from `bntx_textures`.
    fn adopt_appended_texture(&mut self, pool: Vec<u8>, info: TextureInfo) {
        self.texture_pool = Some(pool);
        self.texture_pool_gen += 1;
        let Some(ptcl) = self.ptcl.as_mut() else {
            return;
        };
        ptcl.bntx_textures.push(info.clone());
        let new_index = ptcl.bntx_textures.len() - 1;
        self.selected_texture = Some((new_index, info));
        self.point_selected_emitter_at(new_index);
    }

    fn current_edit_source(&self) -> Option<&EditSource> {
        let loaded = self.loaded_path.as_ref()?;
        self.edit_sources.iter().find(|source| {
            loaded == &source.base
                || source.alt_base.as_ref() == Some(loaded)
                || source.merged.as_ref() == Some(loaded)
        })
    }

    fn current_transplant_for_entry(&self, entry_name: &str) -> Option<(String, EffTransplant)> {
        let source = self.current_edit_source()?;
        source
            .transplants
            .iter()
            .find(|item| item.entry_name.eq_ignore_ascii_case(entry_name))
            .cloned()
            .map(|item| (source.fighter.clone(), item))
    }

    /// Which form the texture panel is showing and importing in.
    fn texture_form(&self) -> crate::texture_import::Form {
        if self.show_raw_textures {
            crate::texture_import::Form::Raw
        } else {
            crate::texture_import::Form::Editable
        }
    }

    /// Names of the loaded eff's pool textures, in pool order.
    fn texture_names(&self) -> Vec<String> {
        self.ptcl
            .as_ref()
            .map(|p| p.bntx_textures.iter().map(|t| t.tex_name.clone()).collect())
            .unwrap_or_default()
    }

    /// Diff the working PTCL against the pristine snapshots → absolute-value edit records.
    pub fn collect_authored_edits(&self) -> Vec<AuthoredEdit> {
        let Some(ptcl) = self.ptcl.as_ref() else {
            return Vec::new();
        };
        let tex_names = self.texture_names();
        let mut out = Vec::new();
        for (set_idx, (set, pset)) in ptcl.emitter_sets.iter().zip(&self.pristine).enumerate() {
            for (em_idx, (em, pr)) in set.emitters.iter().zip(pset.iter()).enumerate() {
                let f = field_edits(em, pr, &tex_names);
                if !f.is_empty() {
                    // The entry (kind) name is a separate namespace from the emitter-set
                    // name and cannot be derived from it — resolve it via the entry list,
                    // which is the only place the set_idx → kind mapping exists.
                    let entry_name = self
                        .entries
                        .iter()
                        .find(|e| e.set_idx == set_idx)
                        .map(|e| e.name.clone())
                        .unwrap_or_default();
                    out.push(AuthoredEdit {
                        set_name: set.name.clone(),
                        entry_name,
                        set_idx,
                        emitter_name: em.name.clone(),
                        emitter_idx: em_idx,
                        fields: f,
                    });
                }
            }
        }
        out
    }

    /// Apply saved authored edits onto the loaded eff. Prefers name matches; falls back to
    /// stored indices with a warning (source dump may have changed between sessions).
    pub fn apply_authored_edits(&mut self, edits: &[AuthoredEdit]) {
        let tex_names = self.texture_names();
        let Some(ptcl) = self.ptcl.as_mut() else {
            return;
        };
        for edit in edits {
            let set_idx = ptcl
                .emitter_sets
                .iter()
                .position(|s| !edit.set_name.is_empty() && s.name == edit.set_name)
                .unwrap_or_else(|| {
                    eprintln!(
                        "[EFF-PROJECT] set '{}' not found by name; using stored index {}",
                        edit.set_name, edit.set_idx
                    );
                    edit.set_idx
                });
            let Some(set) = ptcl.emitter_sets.get_mut(set_idx) else {
                continue;
            };
            let em_idx = set
                .emitters
                .iter()
                .position(|e| !edit.emitter_name.is_empty() && e.name == edit.emitter_name)
                .unwrap_or(edit.emitter_idx);
            let Some(em) = set.emitters.get_mut(em_idx) else {
                continue;
            };

            let f = &edit.fields;
            if let Some(v) = f.emission_rate {
                em.emission_rate = v;
            }
            if let Some(v) = f.lifetime {
                em.lifetime = v;
            }
            if let Some(v) = f.scale {
                em.scale = v;
            }
            if let Some(v) = f.color_scale {
                em.color_scale = v;
            }
            if let Some(v) = f.emitter_scale {
                em.emitter_scale = glam::Vec3::from(v);
            }
            let apply_keys = |dst: &mut Vec<ColorKey>, rows: &Vec<[f32; 4]>| {
                for (k, row) in dst.iter_mut().zip(rows) {
                    k.r = row[0];
                    k.g = row[1];
                    k.b = row[2];
                }
            };
            if let Some(rows) = &f.color0 {
                apply_keys(&mut em.color0, rows);
            }
            if let Some(rows) = &f.color1 {
                apply_keys(&mut em.color1, rows);
            }
            if let Some(rows) = &f.alpha0 {
                for (k, row) in em.alpha0_keys.iter_mut().zip(rows) {
                    k.r = row[0];
                }
            }
            if let Some(wanted) = &f.texture_name {
                // A swap to a texture this file does not hold leaves the emitter showing what
                // it actually samples, which is the truth. The edit itself is untouched — it
                // may still resolve in the carrier, whose pool is assembled differently.
                if let Some(i) = tex_names.iter().position(|n| n == wanted) {
                    em.texture_index = Some(i as u32);
                }
            }
        }
    }

    /// Note that the project now differs from what the running game was last given.
    ///
    /// Lights the Send button's unsent indicator. Used by changes that deliberately do NOT
    /// deploy — recording a transplant, for one — so the divergence is visible rather than
    /// silent, without taking the decision to send away from the user.
    pub fn mark_unsent(&mut self) {
        self.eff_dirty_at = Some(Instant::now());
    }

    /// Ask the app to rebuild the live carrier and hand it to the running game.
    ///
    /// The fighter's OWN eff is not the target and never can be: `reparse_game_path` rebuilds
    /// the parsed emitter structs from the resident buffer and never re-requests the file, so
    /// edited bytes went unread mid-match (`cb_game=0`) and only appeared after a reboot. The
    /// carrier's eff IS reloadable, so an edited effect is cloned into it with its edits baked
    /// in and the original kind is aliased onto the clone. Transplants, texture imports and
    /// texture swaps all ride that same rebuild.
    pub fn request_live_apply(&mut self) {
        // The game is about to be given everything we hold, so the unsent marker goes out here —
        // the one place that is true of, whichever button or project load asked for the send.
        self.eff_dirty_at = None;
        self.live_deploy_request = true;
        self.sending = true;
        self.deploy_defer = true;
    }

    /// Drain the deploy request (app-side, once per frame). Returns true when the app
    /// should rebuild + hot-reload the loaded fighter's eff.
    pub fn take_live_deploy_request(&mut self) -> bool {
        if !self.live_deploy_request {
            return false;
        }
        // Let one frame paint with the spinner up before the blocking rebuild starts.
        if self.deploy_defer {
            self.deploy_defer = false;
            return false;
        }
        self.live_deploy_request = false;
        self.live_deploy_at = Some(Instant::now());
        true
    }

    /// Report the outcome of a serviced deploy back into the editor's status line, so the
    /// user still sees exactly what was sent (this replaces the old "queued eff-derived
    /// modifiers for …" note).
    pub fn set_sent_note(&mut self, note: impl Into<String>) {
        self.last_sent_note = Some(note.into());
    }

    // ── Edit scope ────────────────────────────────────────────────────────────

    /// Indices of the emitters in this set that differ from pristine. Purely informational
    /// (the panel shows WHAT is edited); the edits themselves travel as absolute values
    /// through `collect_authored_edits`, per emitter, never as an aggregate.
    /// Keep the emitter selection inside the set being shown.
    ///
    /// Clamping used to happen into a LOCAL index, leaving `selected_emitter` out of range —
    /// so moving from a ten-emitter entry to a three-emitter one drew emitter 2's fields with
    /// no tab highlighted, and the texture panel's swap (which writes through
    /// `selected_emitter`) silently landed on nothing at all.
    fn clamp_selected_emitter(&mut self, set_idx: usize) {
        let count = self
            .ptcl
            .as_ref()
            .and_then(|p| p.emitter_sets.get(set_idx))
            .map(|s| s.emitters.len())
            .unwrap_or(0);
        if self.selected_emitter >= count {
            self.selected_emitter = 0;
        }
    }

    /// Emitter-set indices holding at least one authored edit.
    ///
    /// One pass over the file rather than `edited_emitters` per entry: the entry list draws
    /// every frame, and re-deriving the texture-name table for each of sixty entries is the
    /// difference between free and noticeable.
    fn edited_sets(&self) -> std::collections::HashSet<usize> {
        let Some(ptcl) = self.ptcl.as_ref() else {
            return std::collections::HashSet::new();
        };
        let tex_names = self.texture_names();
        ptcl.emitter_sets
            .iter()
            .zip(&self.pristine)
            .enumerate()
            .filter(|(_, (set, pristine))| {
                set.emitters
                    .iter()
                    .zip(pristine.iter())
                    .any(|(e, p)| !field_edits(e, p, &tex_names).is_empty())
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn edited_emitters(&self, set_idx: usize) -> Vec<usize> {
        let (Some(ptcl), Some(pristine)) = (self.ptcl.as_ref(), self.pristine.get(set_idx)) else {
            return Vec::new();
        };
        let Some(set) = ptcl.emitter_sets.get(set_idx) else {
            return Vec::new();
        };
        let tex_names = self.texture_names();
        set.emitters
            .iter()
            .zip(pristine.iter())
            .enumerate()
            .filter(|(_, (e, p))| !field_edits(e, p, &tex_names).is_empty())
            .map(|(i, _)| i)
            .collect()
    }

    // ── UI ────────────────────────────────────────────────────────────────────

    /// Queue an eff to show in the editor (e.g. the selected fighter's ef_*.eff).
    /// Loads lazily on the next frame the window is open — cheap while it's closed.
    pub fn queue_load(&mut self, path: &Path) {
        // Already showing this file and no reload pending → nothing to do. (A pending
        // reload — e.g. a merged overlay just changed under the loaded file — survives.)
        if self.loaded_path.as_deref() == Some(path) && self.pending_load.is_none() {
            return;
        }
        self.pending_load = Some(path.to_path_buf());
    }

    /// Replace the edit-source list (the app's per-fighter eff mods). This is the single
    /// source of truth for the "Editing:" selector AND the overlay map — every base path
    /// (export-root AND data-root variants) redirects to the fighter's merged build, so
    /// transplanted entries show no matter which path a load request used.
    pub fn set_edit_sources(&mut self, sources: Vec<EditSource>) {
        let mut overlays = std::collections::HashMap::new();
        for s in &sources {
            if let Some(m) = &s.merged {
                overlays.insert(s.base.clone(), m.clone());
                if let Some(alt) = &s.alt_base {
                    overlays.insert(alt.clone(), m.clone());
                }
            }
        }
        // If the overlay for the currently loaded base just appeared/changed, reload so
        // the merged entries show up without any user action.
        if let Some(loaded) = &self.loaded_path {
            if overlays.get(loaded) != self.merged_overlays.get(loaded)
                && self.pending_load.is_none()
            {
                self.pending_load = Some(loaded.clone());
            }
        }
        self.merged_overlays = overlays;
        self.edit_sources = sources;
    }

    /// Point `base` (a fighter's real eff path) at `merged` (its transplants-applied build).
    /// While set, ANY load of `base` — the file combo, fighter auto-follow, project load —
    /// parses the merged file instead. Pass None to drop the overlay. If `base` is the
    /// currently loaded file, it reloads so the view updates immediately.
    pub fn set_merged_overlay(&mut self, base: &Path, merged: Option<&Path>) {
        match merged {
            Some(m) => {
                self.merged_overlays
                    .insert(base.to_path_buf(), m.to_path_buf());
            }
            None => {
                self.merged_overlays.remove(base);
            }
        }
        if self.loaded_path.as_deref() == Some(base) {
            // Carry in-progress emitter edits across the reload.
            //
            // The merged baseline deliberately does NOT bake authored edits in (see
            // `build_merged_preview`), so re-parsing it resets every emitter to pristine and
            // the working-vs-pristine diff goes empty — the edits vanish from the panel, and
            // the next `sync_eff_mods_from_editor` then writes that empty diff back over the
            // project's `authored` list. Capture them here, while the old working copy is
            // still loaded, and re-apply after the load.
            //
            // Not `queue_edits`: this reload is ours, not a project open, so it must not also
            // deploy to the running game.
            if self.pending_edits.is_none() {
                let carried = self.collect_authored_edits();
                if !carried.is_empty() {
                    self.pending_edits = Some(carried);
                    self.pending_edits_push_live = false;
                }
            }
            self.pending_load = Some(base.to_path_buf());
        }
    }

    /// Select this entry (by name, case-insensitive) once the pending/current eff is
    /// loaded — used to land on a freshly transplanted or replaced entry.
    pub fn queue_select(&mut self, name: &str) {
        // Never leave the previous donor highlighted while a new merged preview is pending.
        // If the rebuild/load fails, an empty selection is truthful; showing the old entry
        // made a Bomberman request appear to have transplanted Alucard.
        self.selected_entry = None;
        self.selected_emitter = 0;
        self.pending_select = Some(name.to_lowercase());
        self.open = true; // surface the result
    }

    fn apply_pending_select(&mut self) {
        let Some(want) = self.pending_select.take() else {
            return;
        };
        self.selected_entry = self.entries.iter().position(|e| e.name == want);
        self.selected_emitter = 0;
    }

    pub fn show(&mut self, ctx: &egui::Context, link: &GameLink) {
        if !self.open {
            return;
        }
        link.ensure_started();
        if self.eff_files.is_empty() && self.scan_error.is_none() {
            self.rescan();
        }
        if let Some(path) = self.pending_load.take() {
            if path.exists() {
                self.load_eff(&path);
                if let Some(edits) = self.pending_edits.take() {
                    let had_edits = !edits.is_empty();
                    let push_live = std::mem::take(&mut self.pending_edits_push_live);
                    self.apply_authored_edits(&edits);
                    // Project just loaded: get the game showing it. Rebuild-and-reload, so
                    // every emitter lands where the project says — not an averaged tint.
                    // Edits merely carried across a reload we caused push nothing.
                    if had_edits && push_live && link.status() == LinkStatus::Connected {
                        self.request_live_apply();
                    }
                }
                self.apply_pending_select();
            }
        }
        if self.pending_select.is_some() {
            self.apply_pending_select();
        }

        // NOTE: no auto-apply. `eff_dirty_at` now only marks "there are unsent changes" for
        // the Send button; nothing rebuilds or uploads until the user asks. Editing a colour
        // used to queue a full carrier rebuild + in-game reload a moment after every change.

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("eff_editor"),
            egui::ViewportBuilder::default()
                .with_title("Eff Editor — Visionary")
                .with_inner_size([1120.0, 680.0])
                .with_min_inner_size([760.0, 420.0]),
            |ui, class| {
                // Draw inside a CentralPanel so the window gets the normal panel background
                // (drawing straight into the viewport root left it near-black).
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    self.ui_contents(ui, link);
                });
                if class != egui::ViewportClass::EmbeddedWindow
                    && ui.ctx().input(|i| i.viewport().close_requested())
                {
                    self.open = false;
                }
                // Live values change while the game runs — keep the panel fresh.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(200));
            },
        );
    }

    fn ui_contents(&mut self, ui: &mut Ui, link: &GameLink) {
        self.draw_header(ui, link);
        ui.separator();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(240.0);
                self.draw_entry_list(ui, link);
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(430.0);
                self.draw_emitter_editor(ui);
            });
            ui.separator();
            ui.vertical(|ui| {
                // The right column now carries a texture preview on top of the game readout,
                // which can outgrow a short window — scroll it rather than clipping it.
                egui::ScrollArea::vertical()
                    .id_salt("eff_game_panel")
                    .show(ui, |ui| {
                        self.draw_game_panel(ui, link);
                    });
            });
        });
    }

    fn draw_header(&mut self, ui: &mut Ui, link: &GameLink) {
        ui.horizontal(|ui| {
            let (dot, label) = match link.status() {
                LinkStatus::Connected => (egui::Color32::from_rgb(90, 220, 90), "game connected"),
                LinkStatus::Connecting => (egui::Color32::YELLOW, "connecting…"),
                LinkStatus::Disconnected => (egui::Color32::from_rgb(220, 90, 90), "game offline"),
            };
            ui.colored_label(dot, "●");
            ui.label(label);
            ui.separator();
            // The ONLY send control, in the header so it is reachable from every panel. A
            // second copy used to sit in the game panel reporting none of the phases below it,
            // so whichever one you happened to be looking at decided how much you were told
            // about a stalled send.
            let connected = link.status() == LinkStatus::Connected;
            let unsent = self.eff_dirty_at.is_some();
            let btn = egui::Button::new(
                egui::RichText::new(if unsent {
                    "⬆ Send edits •"
                } else {
                    "⬆ Send edits"
                })
                .strong(),
            );
            let btn = if unsent {
                btn.fill(egui::Color32::from_rgb(0x7A, 0x5A, 0x10))
            } else {
                btn
            };
            if ui
                .add_enabled(connected && !self.sending, btn)
                .on_hover_text(
                    "Rebuild the live carrier and hand it to the running game — emitter edits, \
                     transplants, texture swaps and imported PNGs all ride this one send. \
                     Re-trigger the move to see it on a fresh spawn.",
                )
                .on_disabled_hover_text(if self.sending {
                    "Sending…"
                } else {
                    "Connect to the running game first."
                })
                .clicked()
            {
                self.request_live_apply();
            }
            if self.sending || self.awaiting_game.is_some() {
                ui.add(egui::Spinner::new().size(14.0));
                let (text, tint) = if self.sending {
                    (
                        "building…".to_string(),
                        egui::Color32::from_rgb(0x90, 0xC0, 0xF0),
                    )
                } else {
                    // The long half: the game is taking the carrier and bringing its object
                    // up. Name the PHASE rather than dumping raw numbers — "object=down" told
                    // the user nothing about which of several very different stalls they were
                    // in. The payload itself now moves by disk, so the old "waiting for the
                    // game to take the bytes" phase is a file read and passes in a frame; what
                    // remains is the swap: retiring the previous carrier and spawning ours.
                    let secs = self
                        .awaiting_game
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    let (st, kinds, spawned) = self.carrier_report;
                    // `took_ours` is the difference between "a carrier is up" and "MY carrier
                    // is up". Without it the previous send's carrier answers for this one.
                    let took_ours = self.carrier_gen_now > self.gen_at_send;
                    let phase = match (st, spawned) {
                        _ if secs < 2 => "waiting for game",
                        (2, true) if took_ours => "carrier up",
                        (2, true) => "game still serving the previous carrier",
                        (0, _) => "handing the carrier over",
                        (2, false) => "carrier staged — waiting for its object",
                        (3, _) | (4, _) => "retiring the previous carrier",
                        (5, _) => "waiting for the old resources to release",
                        _ => "loading the carrier",
                    };
                    (
                        if secs >= 2 {
                            format!("{phase}… {secs}s (state={st} kinds={kinds})")
                        } else {
                            phase.to_string()
                        },
                        egui::Color32::from_rgb(0xE0, 0xC0, 0x60),
                    )
                };
                ui.label(egui::RichText::new(text).small().color(tint));
            } else if unsent {
                ui.label(
                    egui::RichText::new("unsent")
                        .small()
                        .color(egui::Color32::from_rgb(0xE0, 0xA0, 0x30)),
                );
            } else if let Some(r) = &self.last_carrier_result {
                ui.label(egui::RichText::new(r).small().color(if self.carrier_ok {
                    egui::Color32::from_rgb(0x70, 0xB0, 0x70)
                } else {
                    egui::Color32::from_rgb(0xD0, 0x80, 0x60)
                }));
            }
            if link.status() != LinkStatus::Connected {
                if let Some(err) = link.last_error() {
                    ui.label(
                        egui::RichText::new(err)
                            .small()
                            .color(egui::Color32::DARK_GRAY),
                    );
                }
            }
            ui.separator();

            ui.label(egui::RichText::new(self.export_root.display().to_string()).small())
                .on_hover_text(
                    "Where this window reads base .eff files from. Follows the data root you \
                     opened in the main window until you pick one here.",
                );
            if ui.small_button("Change…").clicked() {
                let mut dialog = rfd::FileDialog::new().set_title("Select ArcExplorer export root");
                // Start where we already are, not in the launch folder.
                if self.export_root.is_dir() {
                    dialog = dialog.set_directory(&self.export_root);
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.set_export_root(dir.clone());
                    // Remembered across restarts, and it outranks the data root next launch —
                    // picking a folder here used to last only as long as the session.
                    crate::app::save_config_path(crate::app::EFF_ROOT_CONFIG_KEY, &dir);
                }
            }
            if ui.small_button("Rescan").clicked() {
                self.rescan();
            }
        });

        // ── Edits first: the project's edited fighters are what this window is FOR. ──
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Editing:").strong());
            if self.edit_sources.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "nothing yet — transplant an effect or tweak an emitter to start",
                    )
                    .small()
                    .color(egui::Color32::GRAY),
                );
            }
            let sources = self.edit_sources.clone();
            for src in &sources {
                let selected = self.loaded_path.as_deref() == Some(src.base.as_path())
                    || (src.alt_base.is_some()
                        && self.loaded_path.as_deref() == src.alt_base.as_deref());
                let mut badges: Vec<String> = Vec::new();
                if src.transplant_count > 0 {
                    badges.push(format!(
                        "{} transplant{}",
                        src.transplant_count,
                        if src.transplant_count == 1 { "" } else { "s" }
                    ));
                }
                if src.authored > 0 {
                    badges.push(format!("{} emitter edits", src.authored));
                }
                if src.textures > 0 {
                    badges.push(format!(
                        "{} texture{}",
                        src.textures,
                        if src.textures == 1 { "" } else { "s" }
                    ));
                }
                let label = if badges.is_empty() {
                    src.fighter.clone()
                } else {
                    format!("{} ({})", src.fighter, badges.join(", "))
                };
                // A slotted fighter whose merged build is missing signals a failed merge —
                // surface it instead of silently showing the base file.
                let broken = src.transplant_count > 0 && src.merged.is_none();
                let text = if broken {
                    egui::RichText::new(format!("⚠ {label}")).color(egui::Color32::from_rgb(230, 190, 80))
                } else {
                    egui::RichText::new(label)
                };
                let resp = ui.selectable_label(selected, text).on_hover_text(if broken {
                    "EFF transplant merge output missing — check the status bar for the merge error, then transplant again"
                        .to_string()
                } else {
                    format!("open {}'s eff with its edits applied", src.fighter)
                });
                if resp.clicked() {
                    self.pending_load = Some(src.base.clone());
                }
            }
        });

        // ── Base game files: reference source only. ──────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Base files (reference):")
                    .small()
                    .color(egui::Color32::GRAY),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.file_filter)
                    .hint_text("filter (e.g. mario)")
                    .desired_width(140.0),
            );
            let loaded_name = self
                .loaded_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| {
                    let n = n.to_string_lossy();
                    if self.loaded_is_merged {
                        format!("{n} (+ transplants)")
                    } else {
                        n.to_string()
                    }
                })
                .unwrap_or_else(|| "— none —".into());
            egui::ComboBox::from_id_salt("eff_file_combo")
                .selected_text(loaded_name)
                .width(300.0)
                .show_ui(ui, |ui| {
                    let filter = self.file_filter.to_lowercase();
                    let files: Vec<PathBuf> = self
                        .eff_files
                        .iter()
                        .filter(|p| {
                            filter.is_empty()
                                || p.to_string_lossy().to_lowercase().contains(&filter)
                        })
                        .take(400)
                        .cloned()
                        .collect();
                    for path in files {
                        let label = path
                            .strip_prefix(&self.export_root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        let selected = self.loaded_path.as_deref() == Some(path.as_path());
                        if ui.selectable_label(selected, label).clicked() {
                            self.load_eff(&path);
                        }
                    }
                });
            if let Some(err) = &self.scan_error {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 90), err);
            }
            if let Some(err) = &self.load_error {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 90), err);
            }
        });
    }

    fn draw_entry_list(&mut self, ui: &mut Ui, link: &GameLink) {
        let edited_sets = self.edited_sets();
        let edited_entries = self
            .entries
            .iter()
            .filter(|e| edited_sets.contains(&e.set_idx))
            .count();
        ui.horizontal(|ui| {
            ui.label(format!("Entries ({})", self.entries.len()));
            if edited_entries > 0 {
                ui.label(
                    egui::RichText::new(format!("{edited_entries} edited"))
                        .small()
                        .color(egui::Color32::from_rgb(0xE0, 0xC0, 0x60)),
                )
                .on_hover_text("Entries with unsent emitter edits, highlighted in the list");
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.entry_filter)
                .hint_text("filter entries")
                .desired_width(f32::INFINITY),
        );
        let filter = self.entry_filter.to_lowercase();
        let transplant_entries: std::collections::HashMap<String, EffTransplant> = self
            .current_edit_source()
            .map(|source| {
                source
                    .transplants
                    .iter()
                    .cloned()
                    .map(|item| (item.entry_name.to_lowercase(), item))
                    .collect()
            })
            .unwrap_or_default();
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt("eff_entries")
            .show(ui, |ui| {
                for (i, entry) in self.entries.iter().enumerate() {
                    if !filter.is_empty() && !entry.name.contains(&filter) {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        let live = link.is_live(entry.hash);
                        let dot = if live {
                            egui::Color32::from_rgb(90, 220, 90)
                        } else {
                            egui::Color32::DARK_GRAY
                        };
                        ui.colored_label(dot, "●")
                            .on_hover_text(if live {
                                "Seen spawning in game"
                            } else {
                                "Not seen in game yet — trigger the move in training mode"
                            });
                        let selected = self.selected_entry == Some(i);
                        // An edited entry is tinted rather than badged: the list is narrow, a
                        // word of text per row would push the names out of view, and this is
                        // the one thing you need to find again after scrolling away.
                        let text = egui::RichText::new(&entry.name).monospace();
                        let text = if edited_sets.contains(&entry.set_idx) {
                            text.color(egui::Color32::from_rgb(0xE0, 0xC0, 0x60))
                        } else {
                            text
                        };
                        let response = ui.selectable_label(selected, text).on_hover_text(
                            if edited_sets.contains(&entry.set_idx) {
                                format!(
                                    "hash40 0x{:010x}\nhas unsent emitter edits\nright-click to copy the name",
                                    entry.hash
                                )
                            } else {
                                format!(
                                    "hash40 0x{:010x}\nright-click to copy the name",
                                    entry.hash
                                )
                            },
                        );
                        // The entry name is what goes into an ACMD `EFFECT` call, and the list
                        // is the only place it appears in full — the label is truncated once
                        // the panel is narrow, so retyping it from the screen is not an option.
                        if response.secondary_clicked() {
                            ui.ctx().copy_text(entry.name.clone());
                        }
                        if transplant_entries.contains_key(&entry.name) {
                            ui.label(
                                egui::RichText::new("TRANSPLANTED")
                                    .small()
                                    .strong()
                                    .color(egui::Color32::from_rgb(190, 140, 255)),
                            )
                            .on_hover_text("This entry came from an EFF transplant");
                        }
                        if response.clicked() {
                            clicked = Some(i);
                        }
                    });
                }
            });
        if let Some(i) = clicked {
            if self.selected_entry != Some(i) {
                self.selected_entry = Some(i);
                self.selected_emitter = 0;
                // The shared override store keeps per-kind forms — nothing to re-seed.
            }
        }
    }

    fn draw_emitter_editor(&mut self, ui: &mut Ui) {
        // The texture panel on the right renders from this and runs AFTER this column, so
        // clear it on every path that does not reach a live emitter — otherwise deselecting
        // leaves the previous emitter's texture on screen, with working Replace buttons.
        self.selected_texture = None;
        let Some(entry_idx) = self.selected_entry else {
            ui.colored_label(egui::Color32::GRAY, "Select an entry to edit its emitters.");
            return;
        };
        let set_idx = self.entries[entry_idx].set_idx;
        let entry_name = self.entries[entry_idx].name.clone();

        // Which emitters of this set are edited, for the tab markers. Computed BEFORE the
        // mutable borrow below, which is what makes it usable while the fields are drawn.
        let edited: std::collections::HashSet<usize> =
            self.edited_emitters(set_idx).into_iter().collect();

        self.clamp_selected_emitter(set_idx);

        let Some(ptcl) = self.ptcl.as_mut() else {
            return;
        };
        // Texture pool of the loaded eff, cloned up front so the `set`/`em` mutable borrows
        // below don't conflict with reading it.
        let textures = ptcl.bntx_textures.clone();
        let Some(set) = ptcl.emitter_sets.get_mut(set_idx) else {
            return;
        };
        let Some(pristine_set) = self.pristine.get(set_idx) else {
            return;
        };

        ui.horizontal(|ui| {
            ui.heading(&entry_name);
            ui.label(
                egui::RichText::new(format!("{} emitter(s)", set.emitters.len()))
                    .color(egui::Color32::LIGHT_GRAY),
            );
            if ui.small_button("Reset entry").clicked() {
                for (e, p) in set.emitters.iter_mut().zip(pristine_set.iter()) {
                    p.restore(e);
                }
                self.eff_dirty_at = Some(Instant::now());
            }
        });

        // Emitter tabs. Edited ones carry a dot and are tinted: a set can have twenty
        // emitters, and without a marker the only way to find the two you changed was to
        // click through all of them.
        ui.horizontal_wrapped(|ui| {
            for (i, em) in set.emitters.iter().enumerate() {
                let name = if em.name.is_empty() {
                    format!("emitter {i}")
                } else {
                    em.name.clone()
                };
                let text = if edited.contains(&i) {
                    egui::RichText::new(format!("{name} •"))
                        .color(egui::Color32::from_rgb(0xE0, 0xC0, 0x60))
                } else {
                    egui::RichText::new(name)
                };
                if ui
                    .selectable_label(self.selected_emitter == i, text)
                    .clicked()
                {
                    self.selected_emitter = i;
                }
            }
        });
        ui.separator();

        let ei = self.selected_emitter;
        let (Some(em), Some(pr)) = (set.emitters.get_mut(ei), pristine_set.get(ei)) else {
            ui.colored_label(egui::Color32::GRAY, "No emitters in this set.");
            return;
        };

        let mut changed = false;
        // The texture the selected emitter samples, handed to the import/export row that runs
        // after the emitter borrow ends.
        let mut texture_actions: Option<(usize, TextureInfo)> = None;
        egui::ScrollArea::vertical()
            .id_salt("emitter_fields")
            .show(ui, |ui| {
                egui::Grid::new("authored_fields")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        let mut scalar =
                            |ui: &mut Ui, label: &str, v: &mut f32, orig: f32, speed: f64| {
                                ui.label(label);
                                changed |= ui
                                    .add(egui::DragValue::new(v).speed(speed).range(0.0..=f32::MAX))
                                    .changed();
                                ui.label(
                                    egui::RichText::new(format!("orig {orig:.3}"))
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                                ui.end_row();
                            };
                        scalar(ui, "particle scale", &mut em.scale, pr.scale, 0.01);
                        scalar(ui, "color scale", &mut em.color_scale, pr.color_scale, 0.01);
                        scalar(
                            ui,
                            "emission rate",
                            &mut em.emission_rate,
                            pr.emission_rate,
                            0.05,
                        );
                        scalar(ui, "lifetime", &mut em.lifetime, pr.lifetime, 0.5);

                        ui.label("emitter scale");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(egui::DragValue::new(&mut em.emitter_scale.x).speed(0.01))
                                .changed();
                            changed |= ui
                                .add(egui::DragValue::new(&mut em.emitter_scale.y).speed(0.01))
                                .changed();
                            changed |= ui
                                .add(egui::DragValue::new(&mut em.emitter_scale.z).speed(0.01))
                                .changed();
                        });
                        ui.label(
                            egui::RichText::new(format!(
                                "orig [{:.2}, {:.2}, {:.2}]",
                                pr.emitter_scale.x, pr.emitter_scale.y, pr.emitter_scale.z
                            ))
                            .small()
                            .color(egui::Color32::GRAY),
                        );
                        ui.end_row();
                    });

                let mut key_table =
                    |ui: &mut Ui, label: &str, keys: &mut Vec<ColorKey>, orig: &[ColorKey]| {
                        if keys.is_empty() {
                            return;
                        }
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(label).strong());
                        for (i, k) in keys.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                let mut rgb = [k.r, k.g, k.b];
                                if ui.color_edit_button_rgb(&mut rgb).changed() {
                                    k.r = rgb[0];
                                    k.g = rgb[1];
                                    k.b = rgb[2];
                                    changed = true;
                                }
                                ui.label(
                                    egui::RichText::new(format!("t={:.2}", k.frame))
                                        .small()
                                        .monospace(),
                                );
                                if let Some(o) = orig.get(i) {
                                    let mut orig_rgb = [o.r, o.g, o.b];
                                    ui.add_enabled_ui(false, |ui| {
                                        ui.color_edit_button_rgb(&mut orig_rgb);
                                    });
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "orig [{:.2} {:.2} {:.2}]",
                                            o.r, o.g, o.b
                                        ))
                                        .small()
                                        .color(egui::Color32::GRAY),
                                    );
                                }
                            });
                        }
                    };
                key_table(ui, "color0 keys", &mut em.color0, &pr.color0);
                key_table(ui, "color1 keys", &mut em.color1, &pr.color1);

                if !em.alpha0_keys.is_empty() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("alpha keys").strong());
                    for (i, k) in em.alpha0_keys.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(egui::DragValue::new(&mut k.r).speed(0.01).range(0.0..=4.0))
                                .changed();
                            ui.label(
                                egui::RichText::new(format!("t={:.2}", k.frame))
                                    .small()
                                    .monospace(),
                            );
                            if let Some(o) = pr.alpha0_keys.get(i) {
                                ui.label(
                                    egui::RichText::new(format!("orig {:.3}", o.r))
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            }
                        });
                    }
                }

                // The texture this emitter samples is shown, previewed and edited in the
                // Texture panel on the right — it needs the width for a preview, and the
                // import half is a whole-file operation that does not belong in a column of
                // per-emitter fields. Only the pointer is recorded here.
                texture_actions = em
                    .texture_index
                    .map(|i| i as usize)
                    .and_then(|i| textures.get(i).map(|t| (i, t.clone())));

                ui.add_space(6.0);
                if ui.small_button("Reset emitter").clicked() {
                    pr.restore(em);
                    changed = true;
                }
            });

        self.selected_texture = texture_actions;

        if changed {
            self.eff_dirty_at = Some(Instant::now());
        }
    }

    /// The texture panel: what the selected emitter samples, what it looks like, and the two
    /// things you can do to it.
    ///
    /// Reached through the emitter that uses it rather than from a flat list of the pool: 60-odd
    /// entries named `ef_cmn_line02` is guesswork, and going via the emitter means you are always
    /// looking at the one you are about to change.
    ///
    /// The two operations are deliberately labelled apart, because their blast radius differs:
    ///   SWAP   — point THIS emitter at another texture the eff already has. A per-emitter edit.
    ///   IMPORT — replace a pool texture's pixels. Every emitter sampling it changes.
    fn draw_texture_panel(&mut self, ui: &mut Ui) {
        ui.label(egui::RichText::new("Texture").strong());
        let Some((index, texture)) = self.selected_texture.clone() else {
            ui.label(
                egui::RichText::new(if self.ptcl.is_none() {
                    "no eff loaded"
                } else {
                    "this emitter samples no texture"
                })
                .small()
                .color(egui::Color32::GRAY),
            );
            return;
        };

        // Swap picker. Labels are rebuilt here rather than shared with the emitter column so
        // this panel does not depend on that column having drawn first.
        let labels: Vec<String> = self
            .ptcl
            .as_ref()
            .map(|p| {
                p.bntx_textures
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let name = if t.tex_name.is_empty() {
                            format!("texture {i}")
                        } else {
                            t.tex_name.clone()
                        };
                        if t.width > 0 && t.height > 0 {
                            format!("{name}  ({}×{})", t.width, t.height)
                        } else {
                            name
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Only same-size textures are offered. An emitter carries UV rects, scroll rates and
        // frame counts authored against its texture's dimensions, so pointing it at a different
        // size does not scale the effect — it resamples it, and the result reads as a bug rather
        // than an edit. Rather than let the picker offer a choice that is always wrong, the
        // mismatched ones are shown greyed with the reason.
        let same_size: Vec<bool> = self
            .ptcl
            .as_ref()
            .map(|p| {
                p.bntx_textures
                    .iter()
                    .map(|t| (t.width, t.height) == (texture.width, texture.height))
                    .collect()
            })
            .unwrap_or_default();
        let offered = same_size.iter().filter(|ok| **ok).count();
        let mut swap_to = None;
        egui::ComboBox::from_id_salt("emitter_texture_swap")
            .selected_text(
                labels
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| "(not in this pool)".to_string()),
            )
            .width(280.0)
            .show_ui(ui, |ui| {
                for (i, label) in labels.iter().enumerate() {
                    if same_size.get(i).copied().unwrap_or(true) {
                        if ui.selectable_label(index == i, label).clicked() {
                            swap_to = Some(i as u32);
                        }
                    } else {
                        ui.add_enabled(false, egui::Button::selectable(false, label))
                            .on_disabled_hover_text(format!(
                                "Not {}×{} — an emitter's UV rects are authored for its texture's \
                             size, so a different one resamples the effect.",
                                texture.width, texture.height
                            ));
                    }
                }
            })
            .response
            .on_hover_text(format!(
                "Point this emitter at another of the eff's textures. Only the {offered} at \
                 {}×{} are offered. Ships with the next Send, like any other emitter edit.",
                texture.width, texture.height
            ));
        // Every other control in this window shows what it started as; a swap is an edit like
        // any other and gets the same treatment, so a changed texture is visible without
        // opening the picker to compare.
        let original = self
            .selected_entry
            .map(|entry_idx| self.entries[entry_idx].set_idx)
            .and_then(|set_idx| self.pristine.get(set_idx))
            .and_then(|set| set.get(self.selected_emitter))
            .and_then(|snapshot| snapshot.texture_index);
        if original.map(|o| o as usize) != Some(index) {
            let was = original
                .and_then(|o| labels.get(o as usize).cloned())
                .unwrap_or_else(|| "nothing".to_string());
            ui.label(
                egui::RichText::new(format!("swapped — was {was}"))
                    .small()
                    .color(egui::Color32::from_rgb(0xE0, 0xC0, 0x60)),
            );
        }
        if let Some(i) = swap_to {
            if let (Some(entry_idx), Some(ptcl)) = (self.selected_entry, self.ptcl.as_mut()) {
                let set_idx = self.entries[entry_idx].set_idx;
                if let Some(em) = ptcl
                    .emitter_sets
                    .get_mut(set_idx)
                    .and_then(|s| s.emitters.get_mut(self.selected_emitter))
                {
                    em.texture_index = Some(i);
                    self.eff_dirty_at = Some(Instant::now());
                }
            }
        }

        self.draw_texture_preview(ui, index, &texture);
        self.draw_texture_import(ui, index, &texture);
    }

    /// Draw the texture itself.
    ///
    /// Decoding is BCn → RGBA over up to a megapixel, so it is done ONCE per texture and the
    /// result kept as a GPU handle. Doing it per frame at 60fps would spend more time
    /// decompressing this thumbnail than rendering the rest of the app.
    fn draw_texture_preview(&mut self, ui: &mut Ui, index: usize, texture: &TextureInfo) {
        const PREVIEW: u32 = 168;
        // A replaced texture previews from the PNG the user picked. The pool still holds the
        // game's original — it is only rebuilt when the carrier is built — so decoding the pool
        // here would show the texture that was just replaced and read as "the import did nothing".
        let replacement = self
            .texture_imports
            .iter()
            .find(|t| t.texture_name == texture.tex_name && !t.png_path.is_empty())
            .map(|t| t.png_path.clone())
            .unwrap_or_default();
        let form = self.texture_form();
        // The form is part of the key: flipping the checkbox has to re-decode, and so does
        // re-importing the SAME path after painting it again — hence the file's mtime.
        let stamp = std::fs::metadata(&replacement)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let key = (
            self.loaded_path.clone().unwrap_or_default(),
            index,
            replacement.clone(),
            form == crate::texture_import::Form::Raw,
            stamp,
            self.texture_pool_gen,
        );
        if self.texture_preview.as_ref().map(|(k, _)| k) != Some(&key) {
            let decoded = if replacement.is_empty() {
                self.texture_pool.as_ref().and_then(|pool| {
                    crate::texture_import::decode_preview(
                        pool,
                        index,
                        &texture.tex_name,
                        form,
                        Some(PREVIEW),
                    )
                    .ok()
                })
            } else {
                // Previewed through the same conversion the import runs, so an edited mask shows
                // as the shape the game will draw rather than as a black-and-white square.
                std::fs::read(&replacement)
                    .ok()
                    .and_then(|png| match self.texture_pool.as_ref() {
                        Some(pool) => crate::texture_import::decode_import_preview(
                            pool,
                            index,
                            &texture.tex_name,
                            &png,
                            form,
                            Some(PREVIEW),
                        )
                        .ok(),
                        None => crate::texture_import::decode_png_rgba(&png, Some(PREVIEW)).ok(),
                    })
            };
            let handle = decoded.map(|image| {
                let size = [image.width() as usize, image.height() as usize];
                ui.ctx().load_texture(
                    format!("eff_tex_{index}"),
                    egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                    egui::TextureOptions::LINEAR,
                )
            });
            self.texture_form_note = match self.texture_pool.as_ref() {
                Some(pool) => describe_form(pool, index, &texture.tex_name, form),
                None => String::new(),
            };
            self.texture_preview = Some((key, handle));
        }

        match self.texture_preview.as_ref().and_then(|(_, h)| h.as_ref()) {
            Some(handle) => {
                let size = handle.size_vec2();
                let scale = PREVIEW as f32 / size.x.max(size.y).max(1.0);
                // Checkerboard behind it, for the textures that still carry alpha in editable
                // form: on a flat background a soft-edged puff reads as "nothing decoded". Masks
                // come through opaque, so it simply never shows for those.
                let (rect, _) = ui.allocate_exact_size(size * scale, egui::Sense::hover());
                let painter = ui.painter();
                let cell = 8.0;
                let cols = (rect.width() / cell).ceil() as i32;
                let rows = (rect.height() / cell).ceil() as i32;
                for row in 0..rows {
                    for col in 0..cols {
                        let shade = if (row + col) % 2 == 0 { 60 } else { 78 };
                        let cell_rect = egui::Rect::from_min_size(
                            rect.min + egui::vec2(col as f32 * cell, row as f32 * cell),
                            egui::vec2(cell, cell),
                        )
                        .intersect(rect);
                        painter.rect_filled(cell_rect, 0.0, egui::Color32::from_gray(shade));
                    }
                }
                painter.image(
                    handle.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            None => {
                ui.label(
                    egui::RichText::new(if texture.convertible {
                        "preview unavailable"
                    } else {
                        "no preview — Visionary cannot read this format"
                    })
                    .small()
                    .color(egui::Color32::DARK_GRAY),
                );
            }
        }

        // One switch for the whole panel: preview, export, and import all follow it. Keeping
        // them together is deliberate — they are the same question, and letting them disagree is
        // how an image gets interpreted in the form it is not in.
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut self.show_raw_textures,
                egui::RichText::new("Raw channels").small(),
            )
            .on_hover_text(
                "Switches all three at once: what the preview shows, what 'Export PNG' writes, \
                 and how an imported PNG is read.\n\nOff: as the game samples it — the channel \
                 swizzle resolved, transparency against the checkerboard, and exports you can \
                 paint on.\n\nOn: the stored channels as the file holds them. Leave it ticked \
                 to import a raw PNG back.",
            );
            if !self.texture_form_note.is_empty() {
                ui.label(
                    egui::RichText::new(&self.texture_form_note)
                        .small()
                        .color(egui::Color32::GRAY),
                );
            }
        });
    }

    /// Pool-shape operations: give this emitter a texture of its own, or drop one.
    ///
    /// These exist because a pool texture is SHARED. Every emitter that samples it changes when
    /// it changes, and the `ef_cmn_*` names are shared by dozens of effects inside a single eff,
    /// so "edit the texture" and "edit this effect's texture" are different jobs. Duplicating and
    /// repointing is the only way to do the second one.
    fn draw_texture_pool_ops(&mut self, ui: &mut Ui, index: usize, texture: &TextureInfo) {
        let pool = self.texture_pool.clone();
        let names = self.texture_names();
        let can_convert = texture.convertible && pool.is_some();
        let has_emitter = self.selected_entry.is_some();
        let users = self.texture_users(index);

        ui.horizontal_wrapped(|ui| {
            // A duplicate is a rename, not a re-encode, so it works for ANY format Visionary can
            // slice — including the ones it cannot decode to a PNG.
            if ui
                .add_enabled(
                    pool.is_some() && has_emitter,
                    egui::Button::new("Duplicate for this emitter").small(),
                )
                .on_hover_text(
                    "Add a private copy of this texture and point THIS emitter at it. Edit the \
                     copy freely — every other effect sampling the original is left alone. The \
                     copy is byte-identical, not re-encoded.",
                )
                .on_disabled_hover_text(if pool.is_none() {
                    "This eff has no texture archive"
                } else {
                    "Select an emitter first — the copy is pointed at it"
                })
                .clicked()
            {
                let pool_bytes = pool.clone().unwrap_or_default();
                let new_name =
                    crate::texture_import::unique_texture_name(&names, &texture.tex_name);
                match crate::texture_import::duplicate_texture(
                    &pool_bytes,
                    &names,
                    index,
                    &new_name,
                ) {
                    Ok(rebuilt) => {
                        let mut info = texture.clone();
                        info.tex_name = new_name.clone();
                        self.adopt_appended_texture(rebuilt, info);
                        self.pending_texture_additions
                            .push(crate::mod_project::TextureAddition {
                                texture_name: new_name.clone(),
                                template_name: texture.tex_name.clone(),
                                png_path: String::new(),
                                raw: false,
                            });
                        self.texture_note =
                            Some((format!("added {new_name} — this emitter now uses it"), true));
                    }
                    Err(e) => self.texture_note = Some((format!("duplicate failed: {e}"), false)),
                }
            }

            if ui
                .add_enabled(
                    can_convert && has_emitter,
                    egui::Button::new("Import as new texture").small(),
                )
                .on_hover_text(
                    "Add your image as a NEW pool texture and point this emitter at it, leaving \
                     the original in place for everything else. Must be the same size as the \
                     texture it takes over from.",
                )
                .on_disabled_hover_text(if pool.is_none() {
                    "This eff has no texture archive".to_string()
                } else if !has_emitter {
                    "Select an emitter first — the new texture is pointed at it".to_string()
                } else {
                    format!("Visionary cannot convert {}", texture.format)
                })
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a PNG for the new texture")
                    .add_filter("PNG", &["png"])
                    .pick_file()
                {
                    let pool_bytes = pool.clone().unwrap_or_default();
                    let new_name =
                        crate::texture_import::unique_texture_name(&names, &texture.tex_name);
                    let form = self.texture_form();
                    // Encoded NOW so a wrong size or a bad image is rejected here, where the user
                    // is looking at it — not silently, mid carrier build, minutes later.
                    match std::fs::read(&path)
                        .map_err(anyhow::Error::from)
                        .and_then(|png| {
                            crate::texture_import::add_texture_from_png(
                                &pool_bytes,
                                &names,
                                index,
                                &new_name,
                                &png,
                                form,
                            )
                        }) {
                        Ok((rebuilt, report)) => {
                            let mut info = texture.clone();
                            info.tex_name = new_name.clone();
                            self.adopt_appended_texture(rebuilt, info);
                            self.pending_texture_additions.push(
                                crate::mod_project::TextureAddition {
                                    texture_name: new_name.clone(),
                                    template_name: texture.tex_name.clone(),
                                    png_path: path.to_string_lossy().to_string(),
                                    raw: form == crate::texture_import::Form::Raw,
                                },
                            );
                            self.texture_note = Some((
                                format!(
                                    "added {new_name}: {}×{} {} — this emitter now uses it",
                                    report.width, report.height, report.format
                                ),
                                true,
                            ));
                        }
                        Err(e) => self.texture_note = Some((format!("import failed: {e}"), false)),
                    }
                }
            }

            // Deleting is gated on nothing sampling it, because a pool without a texture some
            // emitter still addresses leaves that emitter pointing at a GUID no descriptor holds
            // — which draws nothing at all. Repoint the users first (the swap picker only offers
            // same-size textures, so the replacement always fits).
            let deletable = pool.is_some() && users == 0 && names.len() > 1;
            if ui
                .add_enabled(deletable, egui::Button::new("Delete texture").small())
                .on_hover_text(
                    "Remove this texture from the eff. Only possible once nothing samples it.",
                )
                .on_disabled_hover_text(if pool.is_none() {
                    "This eff has no texture archive".to_string()
                } else if names.len() <= 1 {
                    "This is the eff's only texture".to_string()
                } else {
                    format!(
                        "{users} emitter(s) still sample this texture. Point them at another one \
                         first — use the texture picker on each."
                    )
                })
                .clicked()
            {
                let pool_bytes = pool.clone().unwrap_or_default();
                match crate::texture_import::remove_texture(&pool_bytes, &names, index) {
                    Ok(rebuilt) => {
                        let gone = texture.tex_name.clone();
                        self.drop_texture_at(index, rebuilt);
                        self.pending_texture_removals.push(gone.clone());
                        self.texture_note = Some((format!("removed {gone}"), true));
                    }
                    Err(e) => self.texture_note = Some((format!("delete failed: {e}"), false)),
                }
            }
        });

        if users > 1 {
            ui.label(
                egui::RichText::new(format!(
                    "{users} emitters share this texture — editing it changes all of them"
                ))
                .small()
                .color(egui::Color32::from_rgb(0xE0, 0xC0, 0x60)),
            );
        }
    }

    /// Drop pool texture `index` from the editor's view, keeping emitter indices valid.
    ///
    /// Removing a texture shifts every LATER index down by one, and the pristine snapshots have
    /// to shift with the working copy — otherwise the next diff reports a swap on every emitter
    /// above the hole, and those phantom edits would ship.
    fn drop_texture_at(&mut self, index: usize, pool: Vec<u8>) {
        self.texture_pool = Some(pool);
        self.texture_pool_gen += 1;
        let shift = |slot: &mut Option<u32>| {
            if let Some(i) = slot {
                if *i as usize > index {
                    *i -= 1;
                }
            }
        };
        if let Some(ptcl) = self.ptcl.as_mut() {
            ptcl.bntx_textures.remove(index);
            for set in ptcl.emitter_sets.iter_mut() {
                for em in set.emitters.iter_mut() {
                    shift(&mut em.texture_index);
                }
            }
        }
        for set in self.pristine.iter_mut() {
            for snapshot in set.iter_mut() {
                shift(&mut snapshot.texture_index);
            }
        }
        self.selected_texture = None;
        self.eff_dirty_at = Some(Instant::now());
    }

    fn draw_texture_import(&mut self, ui: &mut Ui, index: usize, texture: &TextureInfo) {
        let name = texture.tex_name.clone();
        let replaced = self
            .texture_imports
            .iter()
            .find(|t| t.texture_name == name)
            .cloned();

        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(if texture.format.is_empty() {
                    format!("{}×{}", texture.width, texture.height)
                } else {
                    format!("{}×{} · {}", texture.width, texture.height, texture.format)
                })
                .small()
                .color(egui::Color32::GRAY),
            );
            let pool = self.texture_pool.clone();
            let can_convert = texture.convertible && pool.is_some();

            // ONE export button, whose form the "Raw channels" box decides — the same box that
            // decides what the preview shows and how an imported PNG is read. Two buttons made
            // the form settable in two places that could disagree with the preview; with one,
            // what you see is what you get and there is nothing to keep in sync.
            let form = self.texture_form();
            if ui
                .add_enabled(can_convert, egui::Button::new("Export PNG").small())
                .on_hover_text(match form {
                    crate::texture_import::Form::Editable => {
                        "Save what the preview is showing as something you can paint on: black \
                         where the effect is empty, white where it is solid, and it keeps its \
                         transparency. Either edit works, so painting black erases and so does \
                         erasing."
                    }
                    crate::texture_import::Form::Raw => {
                        "Save the texture's stored channels as the file holds them. Only useful \
                         if you know the packing — and 'Raw channels' has to stay ticked to \
                         import it back."
                    }
                })
                .on_disabled_hover_text(if pool.is_none() {
                    "This eff has no texture archive".to_string()
                } else {
                    format!("Visionary cannot convert {}", texture.format)
                })
                .clicked()
            {
                // The suffix keeps the two forms apart on disk. Importing the wrong one puts the
                // shape in the wrong channel, and the filename is the only warning you get.
                let suffix = if form == crate::texture_import::Form::Raw {
                    "_raw"
                } else {
                    ""
                };
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export texture as PNG")
                    .set_file_name(format!("{name}{suffix}.png"))
                    .add_filter("PNG", &["png"])
                    .save_file()
                {
                    let pool = pool.clone().unwrap_or_default();
                    self.texture_note = Some(
                        match crate::texture_import::export_png(&pool, index, &name, form)
                            .and_then(|png| Ok(std::fs::write(&path, png)?))
                        {
                            Ok(()) => (
                                format!(
                                    "exported {name}{suffix}.png ({})",
                                    describe_form(&pool, index, &name, form)
                                ),
                                true,
                            ),
                            Err(e) => (format!("export failed: {e}"), false),
                        },
                    );
                }
            }

            if ui
                .add_enabled(can_convert, egui::Button::new("Replace with PNG").small())
                .on_hover_text(match form {
                    crate::texture_import::Form::Editable => {
                        "Use your own image for this texture, read the same way 'Export PNG' \
                         wrote it: black is empty, white is solid.\n\nEVERY emitter that samples \
                         this texture changes — it replaces the texture, not just this emitter's \
                         use of it."
                    }
                    crate::texture_import::Form::Raw => {
                        "Use your own image, read as the texture's stored channels. Only correct \
                         for a PNG exported with 'Raw channels' ticked.\n\nEVERY emitter that \
                         samples this texture changes — it replaces the texture, not just this \
                         emitter's use of it."
                    }
                })
                .on_disabled_hover_text(if pool.is_none() {
                    "This eff has no texture archive".to_string()
                } else {
                    format!("Visionary cannot convert {}", texture.format)
                })
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a PNG for this texture")
                    .add_filter("PNG", &["png"])
                    .pick_file()
                {
                    // Encode once NOW so a bad image is rejected here, where the user is
                    // looking at it — not silently, mid carrier build, minutes later.
                    let pool = pool.clone().unwrap_or_default();
                    let names: Vec<String> = self.texture_names();
                    match std::fs::read(&path)
                        .map_err(anyhow::Error::from)
                        .and_then(|png| {
                            crate::texture_import::replace_with_png(
                                &pool, &names, index, &png, form,
                            )
                        }) {
                        Ok((_, report)) => {
                            self.pending_texture_imports
                                .push(crate::mod_project::TextureImport {
                                    texture_name: name.clone(),
                                    png_path: path.to_string_lossy().to_string(),
                                    raw: form == crate::texture_import::Form::Raw,
                                });
                            let shape = match report.layout {
                                Some(layout) => format!(" — {}", layout_note(layout)),
                                None => String::new(),
                            };
                            let note = match &report.format_substituted_from {
                                Some(original) => format!(
                                    "{name}: {}×{} — {original} cannot be written, saved as {}{shape}",
                                    report.width, report.height, report.format
                                ),
                                None => format!(
                                    "{name}: {}×{} {}{shape}",
                                    report.width, report.height, report.format
                                ),
                            };
                            self.texture_note = Some((note, true));
                            self.eff_dirty_at = Some(Instant::now());
                        }
                        Err(e) => self.texture_note = Some((format!("import failed: {e}"), false)),
                    }
                }
            }

            if let Some(existing) = &replaced {
                if ui
                    .small_button("Restore original")
                    .on_hover_text(format!("Stop using {}", existing.png_path))
                    .clicked()
                {
                    // Recorded as an import with no image: the app reads this as "drop the
                    // entry for this texture". Removing it from `texture_imports` here would
                    // only change the display — the project store is the app's.
                    self.pending_texture_imports
                        .push(crate::mod_project::TextureImport {
                            texture_name: name.clone(),
                            png_path: String::new(),
                            raw: false,
                        });
                    self.eff_dirty_at = Some(Instant::now());
                }
            }
        });

        self.draw_texture_pool_ops(ui, index, texture);

        if let Some(existing) = &replaced {
            ui.label(
                egui::RichText::new(format!("replaced by {}", existing.png_path))
                    .small()
                    .color(egui::Color32::from_rgb(140, 200, 255)),
            );
        }
        if let Some((note, ok)) = &self.texture_note {
            ui.label(egui::RichText::new(note).small().color(if *ok {
                egui::Color32::from_rgb(0x70, 0xB0, 0x70)
            } else {
                egui::Color32::from_rgb(0xD0, 0x80, 0x60)
            }));
        }
    }

    fn draw_game_panel(&mut self, ui: &mut Ui, link: &GameLink) {
        self.draw_texture_panel(ui);
        ui.separator();

        ui.heading("Game preview");
        let (frames_rx, edits_tx) = link.stats();
        ui.label(
            egui::RichText::new(format!(
                "{} live kind(s) · {frames_rx} frames in · {edits_tx} edits out",
                link.kinds().len()
            ))
            .small()
            .color(egui::Color32::LIGHT_GRAY),
        );
        ui.separator();

        let Some(entry_idx) = self.selected_entry else {
            ui.colored_label(egui::Color32::GRAY, "Select an entry.");
            return;
        };
        let (hash, set_idx, entry_name) = {
            let e = &self.entries[entry_idx];
            (e.hash, e.set_idx, e.name.clone())
        };
        let transplant = self.current_transplant_for_entry(&entry_name);

        let kind = link.kind(hash);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&entry_name).monospace());
            if transplant.is_some() {
                ui.colored_label(egui::Color32::from_rgb(190, 140, 255), "EFF TRANSPLANT");
            }
            match &kind {
                Some(_) => ui.colored_label(egui::Color32::from_rgb(90, 220, 90), "live"),
                None => ui.colored_label(egui::Color32::GRAY, "not seen in game yet"),
            };
        });
        if let Some((fighter, item)) = transplant {
            let scope = if item.one_slot_slots.len() == 1 {
                format!("one-slot c{:02}", item.one_slot_slots[0])
            } else if item.one_slot_slots.is_empty() {
                "all skins".to_string()
            } else {
                format!(
                    "skins {}",
                    item.one_slot_slots
                        .iter()
                        .map(|slot| format!("c{slot:02}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            ui.label(
                egui::RichText::new(format!(
                    "From {} ({}) · {scope}",
                    item.donor_name, item.donor_file
                ))
                .small()
                .color(egui::Color32::from_rgb(190, 140, 255)),
            );
            if ui
                .button("Remove this transplanted effect from the game")
                .on_hover_text(
                    "Remove this entry from the project, rebuild the merged EFF, and unload \
                     its runtime carrier resources immediately",
                )
                .clicked()
            {
                self.pending_transplant_removals.push(EffTransplantRemoval {
                    fighter,
                    op_index: item.op_index,
                    entry_name: item.entry_name,
                    donor_name: item.donor_name,
                });
            }
            ui.separator();
        }

        // Authored edits → live game. These are baked into the CARRIER's copy of the effect,
        // so each emitter gets exactly its own edited values — which is why the kind-level
        // colour multiplier that used to sit under this panel is gone: it was whole-effect by
        // construction, tinting emitters you never touched, and could not carry a per-key or
        // color1 edit at all.
        let edited = self.edited_emitters(set_idx);
        let total = self
            .ptcl
            .as_ref()
            .and_then(|p| p.emitter_sets.get(set_idx))
            .map(|s| s.emitters.len())
            .unwrap_or(0);
        let edited_names = self
            .ptcl
            .as_ref()
            .and_then(|p| p.emitter_sets.get(set_idx))
            .map(|s| {
                edited
                    .iter()
                    .map(|i| match s.emitters.get(*i) {
                        Some(e) if !e.name.is_empty() => format!("{i}:{}", e.name),
                        _ => format!("{i}"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Authored edits → live eff").strong());
        if edited.is_empty() {
            ui.label(
                egui::RichText::new("no authored edits yet — the game shows the original eff")
                    .small()
                    .color(egui::Color32::DARK_GRAY),
            );
        } else {
            ui.label(
                egui::RichText::new(format!(
                    "{} of {total} emitter(s) edited: {edited_names}",
                    edited.len()
                ))
                .monospace(),
            );
            ui.label(
                egui::RichText::new(
                    "Baked into the live carrier — only these emitters change. Send from the \
                     header, then re-trigger the move to see it on a fresh spawn.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
        }
        // Texture replacements are file-wide, not per-entry, so they are listed here in full
        // rather than filtered to the selected effect — a replaced texture changes every
        // emitter that samples it, including ones in entries you are not looking at.
        if !self.texture_imports.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} replaced texture(s) — applies to the whole eff",
                    self.texture_imports.len()
                ))
                .small()
                .color(egui::Color32::from_rgb(140, 200, 255)),
            );
            for import in &self.texture_imports {
                ui.label(
                    egui::RichText::new(format!("  {}", import.texture_name))
                        .small()
                        .monospace()
                        .color(egui::Color32::GRAY),
                );
            }
        }
        // There is exactly ONE send control, and it lives in the header where it is reachable
        // from every panel. A second copy used to sit here with a plain "waiting for game…"
        // spinner, which reported none of the phases the header's does — so whichever one you
        // happened to be looking at decided how much you were told about a stalled send.
        //
        // Feedback for BOTH senders (the header's rebuild and the kind color×/speed "Send now"
        // below) lives here, above the live-kind guard: a rebuild works whether or not the
        // effect has been seen in game yet, so its result must stay visible either way.
        if let Some(note) = &self.last_sent_note {
            ui.label(
                egui::RichText::new(note)
                    .small()
                    .color(egui::Color32::LIGHT_GRAY),
            );
        }

        let Some(kind) = kind else {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Trigger the effect in game (training mode) to open its kind tab.",
                )
                .small()
                .color(egui::Color32::DARK_GRAY),
            );
            ui.separator();
            self.draw_transplant_section(ui, &entry_name);
            return;
        };

        ui.separator();
        ui.label(egui::RichText::new("Live values (from game)").strong());
        let d = &kind.data;
        ui.label(
            egui::RichText::new(format!(
                "scale {:.3} · speed {:.2} · visible {} · updates {}",
                d.scale, d.speed, d.visible, kind.updates
            ))
            .small()
            .monospace(),
        );
        ui.label(
            egui::RichText::new(format!(
                "pos [{:.2} {:.2} {:.2}] · rot [{:.2} {:.2} {:.2}] · bone {}",
                d.pos.x, d.pos.y, d.pos.z, d.rot.x, d.rot.y, d.rot.z, d.bone_name
            ))
            .small()
            .monospace(),
        );
        ui.label(
            egui::RichText::new(format!(
                "spawn baseline: scale {:.3} · pos [{:.2} {:.2} {:.2}]",
                kind.first.scale, kind.first.pos.x, kind.first.pos.y, kind.first.pos.z
            ))
            .small()
            .color(egui::Color32::GRAY),
        );

        // The kind-level "color × / speed ×" multipliers used to sit here. They predate the
        // carrier: they were once the only way to change how an effect looked at runtime, and
        // they are whole-effect by construction — one multiplier tints every emitter of every
        // spawn and cannot express a per-key or color1 edit at all. Authored edits now do that
        // exactly, so offering a second, cruder way to recolour the same effect only invited
        // editing it two ways and wondering which won.
        //
        // The multipliers are not gone from the toolkit: `LiveOverrides` still restores them
        // from a project's saved tweaks and can still import or clear the ones already pinned
        // in the running game. What has gone is the form that let you type a new one HERE.

        ui.separator();
        self.draw_transplant_section(ui, &entry_name);
    }

    fn draw_transplant_section(&mut self, ui: &mut Ui, entry_name: &str) {
        // Transplanting moved to the Transplant Effects window (Windows menu in the main
        // editor): it can pick a donor from ANY eff (pool-wide search) and redirect existing uses.
        ui.label(
            egui::RichText::new(format!(
                "To transplant '{entry_name}' (or any other effect), open Windows → \
                 Transplant Effects in the main window."
            ))
            .small()
            .color(egui::Color32::GRAY),
        );
    }
}

/// Plain-English name for what a texture carries, for the panel and the import note.
fn layout_note(layout: crate::texture_import::Layout) -> &'static str {
    use crate::texture_import::Layout;
    match layout {
        Layout::Mask => "a black-and-white mask: black is empty, white is solid",
        Layout::Matte { .. } => "a shape on a flat colour: black is empty, white is solid",
        Layout::Opaque => "a fully opaque image: every pixel draws",
        Layout::ColorAlpha => "colour plus its own transparency — keep the alpha channel",
    }
}

/// What the panel is currently showing, so the form in use is never a guess.
fn describe_form(
    pool: &[u8],
    index: usize,
    name: &str,
    form: crate::texture_import::Form,
) -> String {
    if form == crate::texture_import::Form::Raw {
        return "raw stored channels".to_string();
    }
    match crate::texture_import::layout_of_public(pool, index, name) {
        Ok(layout) => layout_note(layout).to_string(),
        Err(_) => "editable form".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{EmitterSet, PtclFile};

    /// An editor holding one emitter set with one emitter, loaded from `path`.
    fn editor_with_one_emitter(path: &str) -> EffEditor {
        let emitter = EmitterDef {
            name: "em0".into(),
            emission_rate: 10.0,
            lifetime: 30.0,
            scale: 1.0,
            color_scale: 1.0,
            emitter_scale: glam::Vec3::ONE,
            color0: Vec::new(),
            color1: Vec::new(),
            alpha0_keys: Vec::new(),
            texture_index: None,
        };
        let mut editor = EffEditor::default();
        editor.pristine = vec![vec![EmitterSnapshot::of(&emitter)]];
        editor.ptcl = Some(PtclFile {
            emitter_sets: vec![EmitterSet {
                name: "P_KirbyDash".into(),
                emitters: vec![emitter],
            }],
            bntx_textures: Vec::new(),
        });
        editor.entries = vec![EffEntry {
            name: "kirby_dash".into(),
            hash: 0,
            set_idx: 0,
        }];
        editor.loaded_path = Some(PathBuf::from(path));
        editor
    }

    /// Tune the one emitter so the working copy differs from its pristine snapshot.
    fn tune(editor: &mut EffEditor, scale: f32) {
        editor.ptcl.as_mut().expect("ptcl").emitter_sets[0].emitters[0].scale = scale;
    }

    /// Give the fixture a two-texture pool and point its emitter at the first one.
    fn with_textures(editor: &mut EffEditor) {
        let ptcl = editor.ptcl.as_mut().expect("ptcl");
        ptcl.bntx_textures = ["ef_a", "ef_b"]
            .iter()
            .map(|n| TextureInfo {
                tex_name: (*n).into(),
                width: 64,
                height: 64,
                format: "BC7Srgb".into(),
                convertible: true,
            })
            .collect();
        ptcl.emitter_sets[0].emitters[0].texture_index = Some(0);
        editor.pristine = vec![vec![EmitterSnapshot::of(&ptcl.emitter_sets[0].emitters[0])]];
    }

    /// Moving to an entry with fewer emitters must not leave the selection past the end.
    ///
    /// It used to: the clamp went into a local index while `selected_emitter` stayed out of
    /// range. The visible symptom was a tab strip with nothing highlighted, but the damaging
    /// one was silent — the texture panel writes its swap through `selected_emitter`, so the
    /// swap landed on no emitter and was simply lost.
    #[test]
    fn selecting_a_smaller_entry_pulls_the_emitter_selection_back_in_range() {
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        // A second set with three emitters, then back to the one-emitter set.
        let ptcl = editor.ptcl.as_mut().unwrap();
        let template = ptcl.emitter_sets[0].emitters[0].clone();
        ptcl.emitter_sets.push(EmitterSet {
            name: "P_Big".into(),
            emitters: vec![template.clone(), template.clone(), template],
        });
        editor.pristine.push(
            editor.ptcl.as_ref().unwrap().emitter_sets[1]
                .emitters
                .iter()
                .map(EmitterSnapshot::of)
                .collect(),
        );

        editor.selected_emitter = 2;
        editor.clamp_selected_emitter(1);
        assert_eq!(editor.selected_emitter, 2, "in range — leave it alone");

        editor.clamp_selected_emitter(0);
        assert_eq!(
            editor.selected_emitter, 0,
            "out of range for a one-emitter set"
        );

        // A set index that does not exist at all must not leave a stale selection either.
        editor.selected_emitter = 5;
        editor.clamp_selected_emitter(99);
        assert_eq!(editor.selected_emitter, 0);
    }

    /// The entry list and the tab strip both highlight what is edited; they must agree with
    /// what `collect_authored_edits` will actually ship.
    #[test]
    fn edited_sets_match_what_gets_sent() {
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        assert!(editor.edited_sets().is_empty());

        tune(&mut editor, 2.5);
        assert_eq!(
            editor.edited_sets(),
            std::collections::HashSet::from([0]),
            "the tuned set must be marked"
        );
        assert_eq!(editor.collect_authored_edits().len(), 1);

        // A texture swap is an edit too — the markers must not miss it just because no
        // numeric field moved.
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        with_textures(&mut editor);
        assert!(editor.edited_sets().is_empty());
        editor.ptcl.as_mut().unwrap().emitter_sets[0].emitters[0].texture_index = Some(1);
        assert_eq!(
            editor.edited_sets(),
            std::collections::HashSet::from([0]),
            "a swap with no field change must still mark the set"
        );
    }

    /// The texture picker used to write `em.texture_index` and stop there: no snapshot field,
    /// no edit record, no export path. It changed the editor's own view and nothing shipped.
    #[test]
    fn a_texture_swap_becomes_an_edit_record_naming_the_texture() {
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        with_textures(&mut editor);
        assert!(
            editor.collect_authored_edits().is_empty(),
            "an untouched fixture must have no edits"
        );

        editor.ptcl.as_mut().unwrap().emitter_sets[0].emitters[0].texture_index = Some(1);

        let edits = editor.collect_authored_edits();
        assert_eq!(edits.len(), 1, "the swap must produce an edit record");
        assert_eq!(
            edits[0].fields.texture_name.as_deref(),
            Some("ef_b"),
            "the record must name the texture, not its pool index"
        );
    }

    /// Reset must undo a swap like any other field — it restores from the same snapshot.
    #[test]
    fn resetting_an_emitter_undoes_a_texture_swap() {
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        with_textures(&mut editor);
        editor.ptcl.as_mut().unwrap().emitter_sets[0].emitters[0].texture_index = Some(1);

        let pristine = editor.pristine[0][0].clone();
        pristine.restore(&mut editor.ptcl.as_mut().unwrap().emitter_sets[0].emitters[0]);

        assert!(
            editor.collect_authored_edits().is_empty(),
            "reset must clear the swap"
        );
    }

    /// A swap round-trips through the project: saved as a name, reapplied as an index against
    /// whatever pool the reloaded file has.
    #[test]
    fn a_saved_texture_swap_reapplies_onto_the_reloaded_eff() {
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        with_textures(&mut editor);
        editor.ptcl.as_mut().unwrap().emitter_sets[0].emitters[0].texture_index = Some(1);
        let saved = editor.collect_authored_edits();

        let mut reloaded = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        with_textures(&mut reloaded);
        reloaded.apply_authored_edits(&saved);

        assert_eq!(
            reloaded.ptcl.as_ref().unwrap().emitter_sets[0].emitters[0].texture_index,
            Some(1)
        );
    }

    /// A transplant reloads the eff from a baseline that deliberately excludes authored edits,
    /// so the edits must be carried across explicitly or they vanish from the panel — and the
    /// next `sync_eff_mods_from_editor` then writes the empty diff back over the project.
    #[test]
    fn transplanting_carries_emitter_edits_across_the_reload() {
        let base = PathBuf::from("/effect/fighter/kirby/ef_kirby.eff");
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        tune(&mut editor, 2.5);
        assert_eq!(
            editor.collect_authored_edits().len(),
            1,
            "fixture is edited"
        );

        editor.set_merged_overlay(&base, Some(Path::new("/tmp/_transplant_preview.eff")));

        let carried = editor.pending_edits.as_ref().expect("edits carried across");
        assert_eq!(carried.len(), 1);
        assert_eq!(carried[0].set_name, "P_KirbyDash");
        assert_eq!(carried[0].entry_name, "kirby_dash");
        assert_eq!(carried[0].fields.scale, Some(2.5));
        assert!(
            !editor.pending_edits_push_live,
            "a reload we caused must not deploy to the running game on its own"
        );
    }

    /// Re-applying them lands the values back on the working copy, so the diff is non-empty
    /// again and the project keeps its `authored` list.
    #[test]
    fn carried_edits_reapply_onto_the_reloaded_eff() {
        let base = PathBuf::from("/effect/fighter/kirby/ef_kirby.eff");
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        tune(&mut editor, 2.5);
        editor.set_merged_overlay(&base, Some(Path::new("/tmp/_transplant_preview.eff")));
        let carried = editor.pending_edits.take().expect("edits carried across");

        // Stand in for the reload: the merged baseline has the emitter back at pristine.
        tune(&mut editor, 1.0);
        assert!(editor.collect_authored_edits().is_empty(), "reload resets");

        editor.apply_authored_edits(&carried);
        let after = editor.collect_authored_edits();
        assert_eq!(after.len(), 1, "the edit is back in the panel's diff");
        assert_eq!(after[0].fields.scale, Some(2.5));
    }

    /// Recording a transplant marks the project unsent AND reloads the eff from the merged
    /// preview. The reload used to clear the marker, so the Send button lit for a frame and went
    /// back to grey — the user was never told the running game hadn't been given the transplant.
    #[test]
    fn a_reload_keeps_the_unsent_marker() {
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        editor.mark_unsent();
        // The load itself fails (nothing on disk here); what matters is that it no longer wipes
        // the marker on its way in, which the old reset did before this early return.
        editor.load_eff(Path::new("/effect/fighter/kirby/_transplant_preview.eff"));
        assert!(
            editor.eff_dirty_at.is_some(),
            "a reload cleared the unsent marker"
        );
    }

    /// Handing everything to the game is what clears it — from any caller, not just the button.
    #[test]
    fn requesting_a_live_apply_clears_the_unsent_marker() {
        let mut editor = editor_with_one_emitter("/effect/fighter/kirby/ef_kirby.eff");
        editor.mark_unsent();
        editor.request_live_apply();
        assert!(editor.eff_dirty_at.is_none());
    }

    /// A project load still pushes, so opening a project gets the game showing it.
    #[test]
    fn a_project_load_still_requests_a_live_push() {
        let mut editor = EffEditor::default();
        editor.queue_edits(vec![AuthoredEdit {
            set_name: "P_KirbyDash".into(),
            entry_name: "kirby_dash".into(),
            set_idx: 0,
            emitter_name: "em0".into(),
            emitter_idx: 0,
            fields: EmitterFieldEdits {
                scale: Some(2.5),
                ..Default::default()
            },
        }]);
        assert!(editor.pending_edits.is_some());
        assert!(editor.pending_edits_push_live);
    }
}
