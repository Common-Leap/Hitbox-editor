//! Kind-centric RPM view — ONE tab per effect kind (eff_hash), live parameter updates, and
//! user-pinned overrides enforced on every live instance until edited again.
//!
//! The instance tracker (tracker.rs) keeps tracking every live spawn for game-side apply and
//! liveness; RPM display is aggregated here so repeated spawns of the same effect update one
//! tab instead of creating duplicates. The RPM object id IS the effect hash (stable per kind).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

use super::apply::ParsedEdit;
use super::effect_data::{Color, EffectData, Point3D};

/// User-pinned parameter overrides. A `Some` field is enforced on every live instance of the
/// kind, every frame, and merged into the displayed data — until the user edits it again.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Pinned {
    pub scale: Option<f32>,
    pub rate: Option<f32>,
    pub pos: Option<Point3D>,
    pub rot: Option<Point3D>,
    pub visible: Option<bool>,
    pub frame: Option<f32>,
    pub color: Option<Color>,
    pub movement_state: Option<f32>,
}

impl Pinned {
    pub fn any(&self) -> bool {
        self.scale.is_some()
            || self.rate.is_some()
            || self.pos.is_some()
            || self.rot.is_some()
            || self.visible.is_some()
            || self.frame.is_some()
            || self.color.is_some()
            || self.movement_state.is_some()
    }

    /// Merge pinned values over `data` (pins win over observed spawn params).
    pub fn apply_to(&self, data: &mut EffectData) {
        if let Some(v) = self.scale {
            data.scale = v;
        }
        if let Some(v) = self.rate {
            data.rate = v;
        }
        if let Some(v) = &self.pos {
            data.pos = v.clone();
        }
        if let Some(v) = &self.rot {
            data.rot = v.clone();
        }
        if let Some(v) = self.visible {
            data.visible = v;
        }
        if let Some(v) = self.frame {
            data.frame = v;
        }
        if let Some(c) = &self.color {
            data.rainbow.color = c.clone();
        }
        if let Some(v) = self.movement_state {
            data.rainbow.movement_state = v;
        }
    }

    /// Absorb an incoming edit — but pin ONLY fields that DIFFER from the current tab state.
    /// RPM uploads the ENTIRE form on every edit, so pinning every present field would pin
    /// the displayed color/frame/etc. as a side effect of editing one value (this clobbered
    /// effect colors and froze animation frames).
    fn absorb_changed(&mut self, edit: &ParsedEdit, current: &EffectData) {
        fn ne(a: f32, b: f32) -> bool {
            (a - b).abs() > 1e-4
        }
        if let Some(v) = edit.scale {
            if ne(v, current.scale) {
                self.scale = Some(v);
            }
        }
        if let Some(v) = edit.rate {
            if ne(v, current.rate) {
                self.rate = Some(v);
            }
        }
        if let Some(v) = &edit.pos {
            if ne(v.x, current.pos.x) || ne(v.y, current.pos.y) || ne(v.z, current.pos.z) {
                self.pos = Some(v.clone());
            }
        }
        if let Some(v) = &edit.rot {
            if ne(v.x, current.rot.x) || ne(v.y, current.rot.y) || ne(v.z, current.rot.z) {
                self.rot = Some(v.clone());
            }
        }
        if let Some(v) = edit.visible {
            if v != current.visible {
                self.visible = Some(v);
            }
        }
        if let Some(v) = edit.frame {
            if ne(v, current.frame) {
                self.frame = Some(v);
            }
        }
        if let Some(c) = &edit.color {
            let cc = &current.rainbow.color;
            if ne(c.red, cc.red)
                || ne(c.green, cc.green)
                || ne(c.blue, cc.blue)
                || ne(c.alpha, cc.alpha)
            {
                self.color = Some(c.clone());
            }
        }
        if let Some(v) = edit.movement_state {
            if ne(v, current.rainbow.movement_state) {
                self.movement_state = Some(v);
            }
        }
    }
}

pub struct KindState {
    pub eff_hash: u64,
    pub name: String,
    /// Latest OBSERVED spawn params — the effect's original values, untouched by pins.
    /// The tab's `original` block shows these so the user can compare/copy them.
    pub observed: EffectData,
    /// What was last actually SENT to RPM — the baseline the user's form shows. Edits are
    /// diffed against this (not live data) so only fields the user really changed get pinned.
    pub last_sent: EffectData,
    pub pinned: Pinned,
    /// Display needs a (re-)notify to RPM.
    pub dirty: bool,
    /// Spawns via the ACMD EFFECT hooks — pos/rot/scale are script-arg OFFSETS enforced by
    /// spawn-arg rewriting; per-frame world-space set_pos/set_rot must not be applied.
    pub acmd: bool,
}

impl KindState {
    /// The displayed data: observed spawn params with pins merged on top.
    fn merged(&self) -> EffectData {
        let mut d = self.observed.clone();
        self.pinned.apply_to(&mut d);
        d
    }
}

static KINDS: LazyLock<Mutex<HashMap<u64, KindState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Pins loaded from the SD save file for kinds not observed yet this session — claimed the
/// first time the kind spawns.
static PENDING_PINS: LazyLock<Mutex<HashMap<u64, Pinned>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record a spawn of this kind: update the observed (original) values and mark for notify
/// when anything the user sees changed. Returns true when this is a brand-new kind.
pub fn observe_spawn(eff_hash: u64, name: &str, spawn_data: &EffectData) -> bool {
    let mut kinds = KINDS.lock();
    match kinds.get_mut(&eff_hash) {
        Some(k) => {
            if *spawn_data != k.observed {
                k.observed = spawn_data.clone();
                k.dirty = true;
            }
            false
        }
        None => {
            // Saved pins from a previous session re-attach on first sighting.
            let pinned = PENDING_PINS.lock().remove(&eff_hash).unwrap_or_default();
            kinds.insert(
                eff_hash,
                KindState {
                    eff_hash,
                    name: name.to_string(),
                    observed: spawn_data.clone(),
                    last_sent: spawn_data.clone(),
                    pinned,
                    dirty: true,
                    acmd: false,
                },
            );
            true
        }
    }
}

/// Apply an RPM edit (id == eff_hash): pin the fields that changed and refresh the display.
/// Returns the kind's pins (for immediate game-side application). Edits for kinds not yet
/// observed this session are parked in PENDING_PINS (claimed on first spawn) instead of
/// being dropped — the editor pushes spawn-offset pins before the effect ever fires.
pub fn apply_edit(edit: &ParsedEdit) -> Option<Pinned> {
    let mut kinds = KINDS.lock();
    let Some(k) = kinds.get_mut(&edit.id) else {
        drop(kinds);
        return stash_pending(edit);
    };
    let baseline = k.last_sent.clone();
    k.pinned.absorb_changed(edit, &baseline);
    k.dirty = true;
    let pins = k.pinned.clone();
    drop(kinds);
    save_pins(); // persist edits so they survive game restarts and can be copied
    Some(pins)
}

/// Edit for a kind that hasn't spawned yet: pin the fields that differ from spawn DEFAULTS
/// (the editor fills defaults for fields it isn't setting, and its defaults mirror
/// `EffectData::default()` — so only real deviations get pinned; frame/color stay unpinned
/// and can't freeze animation). Returns the pins so the caller can report them applied.
fn stash_pending(edit: &ParsedEdit) -> Option<Pinned> {
    let baseline = EffectData::default();
    let mut pending = PENDING_PINS.lock();
    let entry = pending.entry(edit.id).or_default();
    entry.absorb_changed(edit, &baseline);
    let any = entry.any();
    let pins = entry.clone();
    if !any {
        pending.remove(&edit.id);
    }
    drop(pending);
    if any {
        save_pins();
        Some(pins)
    } else {
        None
    }
}

/// Kinds with at least one pinned field → (eff_hash, pins) for per-frame enforcement.
/// For ACMD kinds, pos/rot are stripped: those are script-space offsets enforced by
/// spawn-arg rewriting, not world coordinates for set_pos/set_rot.
pub fn pinned_kinds() -> Vec<(u64, Pinned)> {
    KINDS
        .lock()
        .values()
        .filter(|k| k.pinned.any())
        .map(|k| {
            let mut pins = k.pinned.clone();
            if k.acmd {
                pins.pos = None;
                pins.rot = None;
            }
            (k.eff_hash, pins)
        })
        .collect()
}

/// Flag a kind as ACMD-spawned (see KindState::acmd).
pub fn mark_acmd(eff_hash: u64) {
    if let Some(k) = KINDS.lock().get_mut(&eff_hash) {
        k.acmd = true;
    }
}

/// Drain kinds needing a notify (bounded per call).
pub fn take_dirty(max: usize) -> Vec<(u64, String, EffectData)> {
    let mut kinds = KINDS.lock();
    let mut out = Vec::new();
    for k in kinds.values_mut() {
        if out.len() >= max {
            break;
        }
        if k.dirty {
            k.dirty = false;
            out.push((k.eff_hash, k.name.clone(), k.merged()));
        }
    }
    out
}

/// Everything (for full re-notify on RPM connect). Records what was sent as the edit baseline.
pub fn all() -> Vec<(u64, String, EffectData)> {
    let mut kinds = KINDS.lock();
    kinds
        .values_mut()
        .map(|k| {
            let merged = k.merged();
            k.last_sent = merged.clone();
            (k.eff_hash, k.name.clone(), merged)
        })
        .collect()
}

/// Read one kind for an RPM notify — records what was sent as the edit baseline.
pub fn for_notify(eff_hash: u64) -> Option<(String, EffectData)> {
    let mut kinds = KINDS.lock();
    let k = kinds.get_mut(&eff_hash)?;
    let merged = k.merged();
    k.last_sent = merged.clone();
    Some((k.name.clone(), merged))
}

/// Pins for one kind (spawn-time pre-apply), if any field is pinned. Falls back to
/// not-yet-claimed pending pins so the FIRST spawn after an early edit is already rewritten
/// (observe_spawn claims them into the kind right after).
pub fn pinned_of(eff_hash: u64) -> Option<Pinned> {
    if let Some(p) = KINDS
        .lock()
        .get(&eff_hash)
        .filter(|k| k.pinned.any())
        .map(|k| k.pinned.clone())
    {
        return Some(p);
    }
    PENDING_PINS
        .lock()
        .get(&eff_hash)
        .filter(|p| p.any())
        .cloned()
}

pub fn get(eff_hash: u64) -> Option<(String, EffectData)> {
    KINDS
        .lock()
        .get(&eff_hash)
        .map(|k| (k.name.clone(), k.merged()))
}

pub fn count() -> usize {
    KINDS.lock().len()
}

/// Clear every pin (kind-attached and pending) plus the SD save, and mark all kinds dirty
/// so the editor gets pristine re-notifies. Sent by the editor's "Reset game pins".
pub fn reset_all_pins() {
    {
        let mut kinds = KINDS.lock();
        for k in kinds.values_mut() {
            if k.pinned.any() {
                k.pinned = Pinned::default();
                k.dirty = true;
            }
        }
    }
    PENDING_PINS.lock().clear();
    let _ = std::fs::remove_file(PINS_FILE);
    skyline::println!("[SLight] All pinned edits reset by editor");
}

pub fn clear() {
    KINDS.lock().clear();
}

// ---- Pin persistence: sd:/slight/user/pinned_edits.json ------------------------------------
//
// Human-readable save of every pinned edit, written on each edit and loaded at boot. Lets the
// user keep/copy their modified values, and re-applies them automatically next session (pins
// wait in PENDING_PINS until the kind first spawns).

const PINS_FILE: &str = "sd:/slight/user/pinned_edits.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedPin {
    effect: String,
    hash: String, // hex eff_hash — the stable kind id
    pins: Pinned,
}

fn save_pins() {
    let entries: Vec<SavedPin> = {
        let kinds = KINDS.lock();
        let mut v: Vec<SavedPin> = kinds
            .values()
            .filter(|k| k.pinned.any())
            .map(|k| SavedPin {
                effect: k.name.clone(),
                hash: format!("{:#x}", k.eff_hash),
                pins: k.pinned.clone(),
            })
            .collect();
        // Keep not-yet-claimed pins from the save file (kind not spawned this session).
        let pending = PENDING_PINS.lock();
        v.extend(pending.iter().map(|(hash, pins)| SavedPin {
            effect: crate::slight::effect_viewer::effect_names::label(*hash),
            hash: format!("{hash:#x}"),
            pins: pins.clone(),
        }));
        v
    };
    if let Ok(json) = serde_json::to_string_pretty(&entries) {
        let _ = std::fs::write(PINS_FILE, json);
    }
}

/// Load saved pins into PENDING_PINS (claimed as kinds spawn). Call once at install.
pub fn load_saved_pins() {
    let Ok(text) = std::fs::read_to_string(PINS_FILE) else {
        return;
    };
    let Ok(entries) = serde_json::from_str::<Vec<SavedPin>>(&text) else {
        skyline::println!("[SLight] pinned_edits.json unreadable — ignoring");
        return;
    };
    let mut pending = PENDING_PINS.lock();
    let mut n = 0;
    for e in entries {
        let hash = e.hash.trim_start_matches("0x");
        if let Ok(h) = u64::from_str_radix(hash, 16) {
            if e.pins.any() {
                pending.insert(h, e.pins);
                n += 1;
            }
        }
    }
    skyline::println!("[SLight] Loaded {n} saved pinned edit(s)");
}
