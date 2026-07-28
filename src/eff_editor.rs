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
// So authored edits do NOT go on the modifier wire at all. They are applied by rebuilding
// the fighter's .eff with `eff_export::rebuild_eff_bytes` — which is per-emitter exact, the
// same bytes the exporter ships — and hot-reloading it in the running game (app-side
// `deploy_live_eff`). The editor only raises a request flag; the app owns the rebuild.
//
// The kind-level multipliers are still exposed, but only as their own honest feature: the
// "Kind look — color × / speed ×" panel, which the user drives directly and which is
// documented as applying to every spawn of the effect.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::Ui;

use crate::effects::{load_effect, ColorKey, EmitterDef, PtclFile};
use crate::game_link::{GameLink, LinkStatus, LiveOverrides};
use crate::mod_project::{AuthoredEdit, EmitterFieldEdits, TransplantOp};

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
fn field_edits(em: &EmitterDef, pr: &EmitterSnapshot) -> EmitterFieldEdits {
    let mut f = EmitterFieldEdits::default();
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
        self.eff_dirty_at = None;

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

    /// Diff the working PTCL against the pristine snapshots → absolute-value edit records.
    pub fn collect_authored_edits(&self) -> Vec<AuthoredEdit> {
        let Some(ptcl) = self.ptcl.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (set_idx, (set, pset)) in ptcl.emitter_sets.iter().zip(&self.pristine).enumerate() {
            for (em_idx, (em, pr)) in set.emitters.iter().zip(pset.iter()).enumerate() {
                let f = field_edits(em, pr);
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
        }
    }

    /// Ask the app to push the current authored edits into the running game.
    ///
    /// DISABLED: this used to rebuild the FIGHTER's own eff and hot-reload it. That path is
    /// a dead end and was ruled out — `reparse_game_path` rebuilds the parsed emitter
    /// structs from the RESIDENT buffer and never re-requests the file, so the merged bytes
    /// were never read mid-match (`cb_game=0`) and edits only appeared after a full reboot.
    ///
    /// The replacement is the CARRIER path: clone the effects that need editing into the
    /// carrier's eff space and reload that instead of the fighter's. Until that lands (or
    /// per-emitter runtime modifiers prove workable, which is the cheaper answer), authored
    /// colour edits are export-only and nothing is pushed live.
    /// Note that the project now differs from what the running game was last given.
    ///
    /// Lights the Send button's unsent indicator. Used by changes that deliberately do NOT
    /// deploy — recording a transplant, for one — so the divergence is visible rather than
    /// silent, without taking the decision to send away from the user.
    pub fn mark_unsent(&mut self) {
        self.eff_dirty_at = Some(Instant::now());
    }

    pub fn request_live_apply(&mut self) {
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
    fn edited_emitters(&self, set_idx: usize) -> Vec<usize> {
        let (Some(ptcl), Some(pristine)) = (self.ptcl.as_ref(), self.pristine.get(set_idx)) else {
            return Vec::new();
        };
        let Some(set) = ptcl.emitter_sets.get(set_idx) else {
            return Vec::new();
        };
        set.emitters
            .iter()
            .zip(pristine.iter())
            .enumerate()
            .filter(|(_, (e, p))| !field_edits(e, p).is_empty())
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

    pub fn show(&mut self, ctx: &egui::Context, link: &GameLink, overrides: &mut LiveOverrides) {
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
                    self.ui_contents(ui, link, overrides);
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

    fn ui_contents(&mut self, ui: &mut Ui, link: &GameLink, overrides: &mut LiveOverrides) {
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
                self.draw_game_panel(ui, link, overrides);
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
            // Primary send, in the header so it is reachable from ANY panel — the one down in
            // the game panel is easy to miss and only visible once you have scrolled there.
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
                    "Rebuild the live carrier with every authored edit baked in and hand it to \
                     the running game. Re-trigger the move to see it on a fresh spawn.",
                )
                .on_disabled_hover_text(if self.sending {
                    "Sending…"
                } else {
                    "Connect to the running game first."
                })
                .clicked()
            {
                self.eff_dirty_at = None;
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
                    // The long half: the game is decompressing the carrier and bringing its
                    // object up. This is what the minute-long wait actually was. Name the
                    // PHASE rather than dumping raw numbers — "object=down" told the user
                    // nothing about which of three very different stalls they were in.
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
                        (0, _) => "waiting for the game to take the bytes",
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

            ui.label(egui::RichText::new(self.export_root.display().to_string()).small());
            if ui.small_button("Change…").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Select ArcExplorer export root")
                    .pick_folder()
                {
                    self.export_root = dir;
                    self.rescan();
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
        ui.horizontal(|ui| {
            ui.label(format!("Entries ({})", self.entries.len()));
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
                        ui.colored_label(dot, "●");
                        let selected = self.selected_entry == Some(i);
                        let response = ui
                            .selectable_label(
                                selected,
                                egui::RichText::new(&entry.name).monospace(),
                            )
                            .on_hover_text(format!("hash40 0x{:010x}", entry.hash));
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
        let Some(entry_idx) = self.selected_entry else {
            ui.colored_label(egui::Color32::GRAY, "Select an entry to edit its emitters.");
            return;
        };
        let set_idx = self.entries[entry_idx].set_idx;
        let entry_name = self.entries[entry_idx].name.clone();
        let Some(ptcl) = self.ptcl.as_mut() else {
            return;
        };
        // Texture pool of the loaded eff (for the per-emitter "swap texture" picker). Captured
        // as owned labels up front so the `set`/`em` mutable borrows below don't conflict.
        let tex_labels: Vec<String> = ptcl
            .bntx_textures
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
            .collect();
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

        // Emitter tabs
        ui.horizontal_wrapped(|ui| {
            for (i, em) in set.emitters.iter().enumerate() {
                let name = if em.name.is_empty() {
                    format!("emitter {i}")
                } else {
                    em.name.clone()
                };
                if ui
                    .selectable_label(self.selected_emitter == i, name)
                    .clicked()
                {
                    self.selected_emitter = i;
                }
            }
        });
        ui.separator();

        let ei = self
            .selected_emitter
            .min(set.emitters.len().saturating_sub(1));
        let (Some(em), Some(pr)) = (set.emitters.get_mut(ei), pristine_set.get(ei)) else {
            ui.colored_label(egui::Color32::GRAY, "No emitters in this set.");
            return;
        };

        let mut changed = false;
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

                // ── Textures ──────────────────────────────────────────────────────
                // View the texture this emitter samples and swap it to any other texture
                // present in the eff (applies live in the preview). Importing external images
                // needs a BNTX encoder (see status) — swap-to-existing works today.
                if !tex_labels.is_empty() {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("texture").strong());
                    let cur = em.texture_index as usize;
                    let cur_label = tex_labels
                        .get(cur)
                        .cloned()
                        .unwrap_or_else(|| "(none / index out of range)".to_string());
                    egui::ComboBox::from_id_salt("emitter_texture_swap")
                        .selected_text(cur_label)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for (i, label) in tex_labels.iter().enumerate() {
                                if ui
                                    .selectable_label(em.texture_index as usize == i, label)
                                    .clicked()
                                {
                                    em.texture_index = i as u32;
                                    changed = true;
                                }
                            }
                        });
                    ui.label(
                        egui::RichText::new(
                            "Swap re-skins this emitter in the preview. Importing your own PNG \
                         needs a BNTX encoder (coming — see notes).",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                }

                ui.add_space(6.0);
                if ui.small_button("Reset emitter").clicked() {
                    pr.restore(em);
                    changed = true;
                }
            });

        if changed {
            self.eff_dirty_at = Some(Instant::now());
        }
    }

    fn draw_game_panel(&mut self, ui: &mut Ui, link: &GameLink, overrides: &mut LiveOverrides) {
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

        // Authored edits → live game. These are applied by REBUILDING this fighter's eff and
        // hot-reloading it, so each emitter gets exactly its own edited values. They are
        // deliberately NOT translated into the kind-level colour multiplier below: that
        // multiplier is whole-effect by construction and would tint emitters you never
        // touched (and could not carry a per-key or color1 edit at all).
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
                    "Applied by rebuilding the eff — only these emitters change. The game \
                     re-reads the file, so re-trigger the move to see it on a fresh spawn.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
        }
        ui.horizontal(|ui| {
            // EXPLICIT send only. Auto-apply used to rebuild and re-upload the carrier a
            // moment after every edit, which meant a colour drag shipped a full carrier
            // rebuild + in-game reload repeatedly. Editing is now free; you pay only when
            // you ask to.
            let connected = link.status() == LinkStatus::Connected;
            let unsent = self.eff_dirty_at.is_some();
            let label = if unsent {
                "Send to game •"
            } else {
                "Send to game"
            };
            let btn = egui::Button::new(label);
            let resp = ui
                .add_enabled(connected && !self.sending, btn)
                .on_hover_text(
                    "Rebuild the live carrier with every authored edit baked in and hand it to \
                 the running game. Re-trigger the move to see it on a fresh spawn.",
                );
            if resp.clicked() {
                self.eff_dirty_at = None;
                self.request_live_apply();
            }
            // The wait continues after the bytes leave the app, so this spinner has to track
            // `awaiting_game` too — otherwise it clears while the carrier is still coming up,
            // which is exactly the "loading finishes before the item spawns" complaint.
            if self.sending || self.awaiting_game.is_some() {
                ui.add(egui::Spinner::new().size(14.0));
                let (text, tint) = if self.sending {
                    ("sending…", egui::Color32::from_rgb(0x90, 0xC0, 0xF0))
                } else {
                    (
                        "waiting for game…",
                        egui::Color32::from_rgb(0xE0, 0xC0, 0x60),
                    )
                };
                ui.label(egui::RichText::new(text).small().color(tint));
            } else if !connected {
                ui.label(
                    egui::RichText::new("game not connected")
                        .small()
                        .color(egui::Color32::GRAY),
                );
            } else if unsent {
                ui.label(
                    egui::RichText::new("unsent changes")
                        .small()
                        .color(egui::Color32::from_rgb(0xE0, 0xA0, 0x30)),
                );
            }
        });
        // Feedback for BOTH senders (the rebuild above and the kind color×/speed "Send now"
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
            self.draw_transplant_section(ui, &entry_name, set_idx);
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

        // Direct runtime overrides — the SHARED store the Effects panel also edits; changes
        // here back-sync into the move's EffectCalls (markers, edit records) app-side.
        ui.separator();
        ui.label(egui::RichText::new("Kind look — color × / speed × (all spawns)").strong());
        ui.label(
            egui::RichText::new(
                "Per-spawn position/rotation/size live in the Effects panel (each spawn \
                 independent). These multipliers apply to every spawn of the effect.",
            )
            .small()
            .color(egui::Color32::GRAY),
        );
        let mut tweak_changed = false; // color×/speed rows (export as LAST_EFFECT_SET_*)
        {
            let form = overrides.form_mut(hash, || kind.data.clone());
            egui::Grid::new("live_overrides")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    ui.label("speed ×");
                    tweak_changed |= ui
                        .add(
                            egui::DragValue::new(&mut form.speed)
                                .speed(0.02)
                                .range(0.0..=8.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("color ×");
                    ui.horizontal(|ui| {
                        let c = &mut form.rainbow.color;
                        tweak_changed |= ui
                            .add(
                                egui::DragValue::new(&mut c.red)
                                    .speed(0.02)
                                    .range(0.0..=8.0),
                            )
                            .changed();
                        tweak_changed |= ui
                            .add(
                                egui::DragValue::new(&mut c.green)
                                    .speed(0.02)
                                    .range(0.0..=8.0),
                            )
                            .changed();
                        tweak_changed |= ui
                            .add(
                                egui::DragValue::new(&mut c.blue)
                                    .speed(0.02)
                                    .range(0.0..=8.0),
                            )
                            .changed();
                        tweak_changed |= ui
                            .add(
                                egui::DragValue::new(&mut c.alpha)
                                    .speed(0.02)
                                    .range(0.0..=8.0),
                            )
                            .changed();
                    });
                    ui.end_row();
                });
        }
        if tweak_changed {
            overrides.mark_tweak(hash);
        }
        ui.horizontal(|ui| {
            if ui.small_button("Re-seed from game").clicked() {
                let seed = kind.data.clone();
                let form = overrides.form_mut(hash, || seed.clone());
                form.speed = seed.speed;
                form.rainbow = seed.rainbow;
                overrides.mark_tweak(hash);
            }
            if ui.small_button("Send now").clicked() {
                overrides.flush_one(hash, link);
                self.last_sent_note = Some("sent kind color/speed".into());
            }
        });

        ui.separator();
        self.draw_transplant_section(ui, &entry_name, set_idx);
    }

    fn draw_transplant_section(&mut self, ui: &mut Ui, entry_name: &str, _set_idx: usize) {
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
            texture_index: 0,
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
