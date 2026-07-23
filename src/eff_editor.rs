// Eff editor: browse ef_*.eff game dumps, edit authored emitter values against a pristine
// snapshot, and preview the edit live in game through the slight_replica plugin.
//
// The game exposes no absolute setters for authored PTCL data — runtime color/speed are
// multipliers and pos/rot/size are ACMD spawn arguments. So the editor keeps the ORIGINAL
// authored values (from the .eff dump), lets you edit copies, and translates the delta into
// the runtime modifier the plugin can pin: per-channel `edited ÷ original` color multiplier
// and a scale multiplier, applied to the live kind's pristine spawn baseline.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::Ui;

use crate::effects::{load_effect, ColorKey, EmitterDef, PtclFile};
use crate::game_link::{Color, GameLink, LinkStatus, LiveOverrides};
use crate::mod_project::{AuthoredEdit, EmitterFieldEdits, OneSlotOp};

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
/// selector is organized around. `merged` is the one-slots-applied build to parse when
/// present; `alt_base` covers the data-root path when it differs from the export root.
#[derive(Clone, PartialEq)]
pub struct EditSource {
    pub fighter: String,
    pub base: PathBuf,
    pub alt_base: Option<PathBuf>,
    pub merged: Option<PathBuf>,
    pub one_slots: usize,
    pub authored: usize,
}

/// Runtime modifiers derived from authored edits: what to send the game so the live effect
/// approximates the edited .eff.
#[derive(Clone, Copy, Debug)]
pub struct EntryMods {
    pub color: [f32; 3],
    pub alpha: f32,
    pub scale: f32,
    pub changed: bool,
}

impl Default for EntryMods {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            alpha: 1.0,
            scale: 1.0,
            changed: false,
        }
    }
}

pub struct EffEditor {
    pub open: bool,
    /// Eff path queued by the main editor (fighter selection) — loaded when the window is
    /// (or becomes) open, so closed-window fighter browsing stays cheap.
    pending_load: Option<PathBuf>,
    /// Entry name (lowercase) to select after the next load — one-slot hand-off.
    pending_select: Option<String>,
    /// Authored edits (from a loaded project) applied once the queued eff loads.
    pending_edits: Option<Vec<AuthoredEdit>>,
    export_root: PathBuf,
    eff_files: Vec<PathBuf>,
    file_filter: String,
    scan_error: Option<String>,

    loaded_path: Option<PathBuf>,
    /// Base eff path → merged (one-slots applied) file to ACTUALLY parse when that base
    /// is opened. Makes the merged view character-centric: picking ef_kirby.eff shows
    /// kirby WITH its one-slots, wherever it's opened from.
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

    // One-slot state
    /// The main editor's selected fighter — target for one-slot ops (set by the app).
    target_fighter: Option<String>,
    /// Recorded one-slot ops, drained by the app into the project store.
    pending_one_slots: Vec<OneSlotOp>,

    // Live-preview state
    pub auto_apply: bool,
    eff_dirty_at: Option<Instant>,
    last_sent_note: Option<String>,
}

impl Default for EffEditor {
    fn default() -> Self {
        Self {
            open: false,
            pending_load: None,
            pending_select: None,
            pending_edits: None,
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
            pending_one_slots: Vec::new(),
            auto_apply: true,
            eff_dirty_at: None,
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
            // Transient one-slot preview files aren't real sources — hide them.
            let is_preview = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains("_oneslot_preview"))
                .unwrap_or(false);
            if !is_preview {
                out.push(path);
            }
        }
    }
}

fn ratio(edited: f32, pristine: f32) -> f32 {
    if pristine.abs() < 1e-4 {
        if edited.abs() < 1e-4 {
            1.0
        } else {
            (edited / 1e-4).clamp(0.0, 8.0)
        }
    } else {
        (edited / pristine).clamp(0.0, 8.0)
    }
}

fn mean_rgb(keys: &[ColorKey]) -> Option<[f32; 3]> {
    if keys.is_empty() {
        return None;
    }
    let mut acc = [0.0f32; 3];
    for k in keys {
        acc[0] += k.r;
        acc[1] += k.g;
        acc[2] += k.b;
    }
    let n = keys.len() as f32;
    Some([acc[0] / n, acc[1] / n, acc[2] / n])
}

/// Mean alpha of an alpha key table (value lives in the `.r` channel — see `sample_alpha`).
fn mean_alpha(keys: &[ColorKey]) -> Option<f32> {
    if keys.is_empty() {
        return None;
    }
    Some(keys.iter().map(|k| k.r).sum::<f32>() / keys.len() as f32)
}

fn diff(a: f32, b: f32) -> bool {
    (a - b).abs() > 1e-3
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
        // MERGED (one-slots applied) file when one exists — the new/replaced entries
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
    }

    pub fn set_target_fighter(&mut self, fighter: Option<String>) {
        self.target_fighter = fighter;
    }

    /// One-slot ops recorded since the last drain (the app owns the project store).
    pub fn take_one_slots(&mut self) -> Vec<OneSlotOp> {
        std::mem::take(&mut self.pending_one_slots)
    }

    /// Diff the working PTCL against the pristine snapshots → absolute-value edit records.
    pub fn collect_authored_edits(&self) -> Vec<AuthoredEdit> {
        let Some(ptcl) = self.ptcl.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (set_idx, (set, pset)) in ptcl.emitter_sets.iter().zip(&self.pristine).enumerate() {
            for (em_idx, (em, pr)) in set.emitters.iter().zip(pset.iter()).enumerate() {
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
                    f.emitter_scale =
                        Some([em.emitter_scale.x, em.emitter_scale.y, em.emitter_scale.z]);
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
                if !f.is_empty() {
                    out.push(AuthoredEdit {
                        set_name: set.name.clone(),
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

    /// Send derived modifiers for every entry whose emitters were edited and whose kind is
    /// live in game — used after loading a project so the game reflects it immediately.
    pub fn send_all_derived(&mut self, link: &GameLink, overrides: &mut LiveOverrides) {
        if link.status() != LinkStatus::Connected {
            return;
        }
        let candidates: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| link.is_live(e.hash))
            .map(|(i, _)| i)
            .collect();
        for i in candidates {
            if self.entry_mods(self.entries[i].set_idx).changed {
                self.send_derived(link, overrides, i);
            }
        }
    }

    // ── Modifier derivation ───────────────────────────────────────────────────

    /// Aggregate authored-edit → runtime-modifier translation for one entry (emitter set).
    fn entry_mods(&self, set_idx: usize) -> EntryMods {
        let (Some(ptcl), Some(pristine)) = (self.ptcl.as_ref(), self.pristine.get(set_idx)) else {
            return EntryMods::default();
        };
        let Some(set) = ptcl.emitter_sets.get(set_idx) else {
            return EntryMods::default();
        };

        let mut color_acc = [0.0f32; 3];
        let mut color_n = 0u32;
        let mut alpha_acc = 0.0f32;
        let mut alpha_n = 0u32;
        let mut scale_acc = 0.0f32;
        let mut scale_n = 0u32;

        for (e, p) in set.emitters.iter().zip(pristine.iter()) {
            // Color: mean over color0 keys (fall back to color1), times color_scale.
            let (ce, cp) = match (mean_rgb(&e.color0), mean_rgb(&p.color0)) {
                (Some(a), Some(b)) => (Some(a), Some(b)),
                _ => (mean_rgb(&e.color1), mean_rgb(&p.color1)),
            };
            if let (Some(ce), Some(cp)) = (ce, cp) {
                let cs = ratio(e.color_scale, p.color_scale);
                let r = [
                    ratio(ce[0], cp[0]) * cs,
                    ratio(ce[1], cp[1]) * cs,
                    ratio(ce[2], cp[2]) * cs,
                ];
                if r.iter().any(|v| diff(*v, 1.0)) {
                    for i in 0..3 {
                        color_acc[i] += r[i];
                    }
                    color_n += 1;
                }
            }

            if let (Some(ae), Some(ap)) = (mean_alpha(&e.alpha0_keys), mean_alpha(&p.alpha0_keys)) {
                let r = ratio(ae, ap);
                if diff(r, 1.0) {
                    alpha_acc += r;
                    alpha_n += 1;
                }
            }

            let es = (e.emitter_scale.x + e.emitter_scale.y + e.emitter_scale.z) / 3.0;
            let ps = (p.emitter_scale.x + p.emitter_scale.y + p.emitter_scale.z) / 3.0;
            let r = ratio(e.scale, p.scale) * ratio(es, ps);
            if diff(r, 1.0) {
                scale_acc += r;
                scale_n += 1;
            }
        }

        let color = if color_n > 0 {
            [
                color_acc[0] / color_n as f32,
                color_acc[1] / color_n as f32,
                color_acc[2] / color_n as f32,
            ]
        } else {
            [1.0; 3]
        };
        let alpha = if alpha_n > 0 {
            alpha_acc / alpha_n as f32
        } else {
            1.0
        };
        let scale = if scale_n > 0 {
            scale_acc / scale_n as f32
        } else {
            1.0
        };
        EntryMods {
            color,
            alpha,
            scale,
            changed: color_n + alpha_n + scale_n > 0,
        }
    }

    /// Derive runtime modifiers from the authored edits and write them into the SHARED
    /// override form (debounced send happens app-side), so both panels show one truth.
    fn send_derived(&mut self, link: &GameLink, overrides: &mut LiveOverrides, entry_idx: usize) {
        let Some(entry) = self.entries.get(entry_idx) else {
            return;
        };
        let Some(kind) = link.kind(entry.hash) else {
            return;
        };
        let mods = self.entry_mods(entry.set_idx);

        let form = overrides.form_mut(entry.hash, || kind.data.clone());
        form.scale = kind.first.scale * mods.scale;
        form.rainbow.color = Color {
            red: mods.color[0],
            green: mods.color[1],
            blue: mods.color[2],
            alpha: mods.alpha,
        };
        overrides.mark_dirty(entry.hash);
        self.last_sent_note = Some(format!("queued eff-derived modifiers for {}", entry.name));
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
    /// one-slotted entries show no matter which path a load request used.
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

    /// Point `base` (a fighter's real eff path) at `merged` (its one-slots-applied build).
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
            self.pending_load = Some(base.to_path_buf());
        }
    }

    /// Select this entry (by name, case-insensitive) once the pending/current eff is
    /// loaded — used to land on a freshly one-slotted or replaced entry.
    pub fn queue_select(&mut self, name: &str) {
        self.pending_select = Some(name.to_lowercase());
        self.open = true; // surface the result
        if self.pending_load.is_none() {
            self.apply_pending_select();
        }
    }

    fn apply_pending_select(&mut self) {
        let Some(want) = self.pending_select.take() else {
            return;
        };
        if let Some(pos) = self.entries.iter().position(|e| e.name == want) {
            self.selected_entry = Some(pos);
            self.selected_emitter = 0;
        }
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
                    self.apply_authored_edits(&edits);
                    self.send_all_derived(link, overrides);
                }
                self.apply_pending_select();
            }
        }

        // Debounced auto-apply of authored edits. (Direct-form sends are debounced by the
        // shared override store, flushed app-side.)
        if let (Some(t), Some(sel)) = (self.eff_dirty_at, self.selected_entry) {
            if self.auto_apply && t.elapsed().as_millis() > 300 {
                self.eff_dirty_at = None;
                self.send_derived(link, overrides, sel);
            }
        }

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
                        "nothing yet — one-slot an effect or tweak an emitter to start",
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
                if src.one_slots > 0 {
                    badges.push(format!(
                        "{} one-slot{}",
                        src.one_slots,
                        if src.one_slots == 1 { "" } else { "s" }
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
                let broken = src.one_slots > 0 && src.merged.is_none();
                let text = if broken {
                    egui::RichText::new(format!("⚠ {label}")).color(egui::Color32::from_rgb(230, 190, 80))
                } else {
                    egui::RichText::new(label)
                };
                let resp = ui.selectable_label(selected, text).on_hover_text(if broken {
                    "one-slot merge output missing — check the status bar for the merge error, then re-slot"
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
                        format!("{n} (+ one-slots)")
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
                        if ui
                            .selectable_label(
                                selected,
                                egui::RichText::new(&entry.name).monospace(),
                            )
                            .on_hover_text(format!("hash40 0x{:010x}", entry.hash))
                            .clicked()
                        {
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

        let kind = link.kind(hash);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&entry_name).monospace());
            match &kind {
                Some(_) => ui.colored_label(egui::Color32::from_rgb(90, 220, 90), "live"),
                None => ui.colored_label(egui::Color32::GRAY, "not seen in game yet"),
            };
        });

        // Derived modifiers from authored edits
        let mods = self.entry_mods(set_idx);
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Eff-derived modifiers").strong());
        ui.label(
            egui::RichText::new(format!(
                "color ×[{:.2} {:.2} {:.2}]  alpha ×{:.2}  scale ×{:.2}",
                mods.color[0], mods.color[1], mods.color[2], mods.alpha, mods.scale
            ))
            .monospace(),
        );
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.auto_apply, "auto-apply");
            let can_send = kind.is_some() && link.status() == LinkStatus::Connected;
            if ui
                .add_enabled(can_send, egui::Button::new("Apply to game"))
                .clicked()
            {
                self.send_derived(link, overrides, entry_idx);
            }
        });
        if !mods.changed {
            ui.label(
                egui::RichText::new("no authored edits yet — modifiers are identity")
                    .small()
                    .color(egui::Color32::DARK_GRAY),
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
            self.draw_one_slot_section(ui, &entry_name, set_idx);
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
        if let Some(note) = &self.last_sent_note {
            ui.label(
                egui::RichText::new(note)
                    .small()
                    .color(egui::Color32::LIGHT_GRAY),
            );
        }

        ui.separator();
        self.draw_one_slot_section(ui, &entry_name, set_idx);
    }

    fn draw_one_slot_section(&mut self, ui: &mut Ui, entry_name: &str, _set_idx: usize) {
        // One-slotting moved to the One-Slot Studio (Windows menu in the main editor):
        // it can pick a donor from ANY eff (pool-wide search) and redirect existing uses.
        ui.label(
            egui::RichText::new(format!(
                "To one-slot '{entry_name}' (or any other effect), open Windows → \
                 One-Slot Studio in the main window."
            ))
            .small()
            .color(egui::Color32::GRAY),
        );
    }
}
