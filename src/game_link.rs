// Game link: TCP client to the slight_replica plugin (127.0.0.1:7878).
// Speaks the plugin's `<TCP_MESSAGE>{json}</TCP_MESSAGE>` framing (formerly RPM's role):
//  - inbound  `{"header":"Notify","body":"{\"Notify\":{id,name,value_in_json}}"}` = live
//    effect-kind tabs (id = hash40 of the effect name, value = RpmEffectData JSON)
//  - outbound `{"id":<hash>,"newValue":"<sparse JSON>"}` = an edit; only controls the
//    user actually changed are sent and pinned.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const PLUGIN_ADDR: &str = "127.0.0.1:7878";

// ── Wire structs (must match slight_replica effect_data.rs exactly) ──────────

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Point3D {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Color {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rainbow {
    pub color: Color,
    pub movement_state: f32,
}

/// One live effect kind as the plugin reports it. `rainbow.color` and `speed` are runtime
/// MULTIPLIERS (the game has no getters for authored color); `pos`/`rot` are in ACMD script
/// offset space for script-spawned effects; `scale` is the spawn size argument.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RpmEffectData {
    pub index: u32,
    pub effect_name: String,
    pub bone_name: String,
    pub is_follow: bool,
    pub visible: bool,
    pub scale: f32,
    pub frame: f32,
    pub pos: Point3D,
    pub rot: Point3D,
    pub speed: f32,
    pub rainbow: Rainbow,
}

impl Default for RpmEffectData {
    fn default() -> Self {
        Self {
            index: 0,
            effect_name: "0x0".into(),
            bone_name: "0x0".into(),
            is_follow: false,
            visible: true,
            scale: 1.0,
            frame: 0.0,
            pos: Point3D::default(),
            rot: Point3D::default(),
            speed: 1.0,
            rainbow: Rainbow::default(),
        }
    }
}

/// Wire form of a plugin spawn rule (matches slight_replica spawn_rules::SpawnRule).
/// `motion` + frame window scope the rule to ONE spawn so editing one spawn of an effect
/// doesn't move every spawn; `pos`/`rot`/`scale` are the per-spawn transform override.
#[derive(Clone, Debug, Serialize)]
pub struct SpawnRuleWire {
    pub eff_hash: u64,
    pub suppress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<u64>,
    pub frame_start: Option<f32>,
    pub frame_end: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rot: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Live retime: re-fire a captured spawn at a new frame (paired with a suppress rule at
    /// the pristine frame). Omitted for plain transform/suppress rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject: Option<SpawnInjectWire>,
}

/// Wire form of plugin `spawn_rules::SpawnInject` — a captured EFFECT spawn to replay.
#[derive(Clone, Debug, Serialize)]
pub struct SpawnInjectWire {
    pub frame: f32,
    pub func: String,
    pub args: Vec<LuaArgWire>,
}

/// Wire form of plugin `spawn_rules::EffectAlias` — live transplant kind substitution:
/// a copy/replaced entry that doesn't exist in the running game spawns as its donor.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EffectAliasWire {
    /// Requested kind (hash40 of the copy / replaced entry name, lowercase).
    pub from: u64,
    /// Kind that exists in the loaded eff resources (the donor).
    pub to: u64,
    /// Costume slots (c00…) the alias applies to; empty = all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<u8>,
}

/// Wire form of the plugin's `effect_reload::DonorSpec` (field names must match).
#[derive(Clone, PartialEq, serde::Serialize)]
pub struct DonorEffWire {
    /// Target fighter's eff arc path (lowercase), e.g. "effect/fighter/kirby/ef_kirby.eff".
    pub target: String,
    /// Donor eff arc paths to co-load whenever the target's effects load.
    pub donors: Vec<String>,
}

/// A stripped donor eff (only the referenced effects + their resources), base64-encoded,
/// that the plugin injects as resident data for a live cross-character transplant.
#[derive(Clone, PartialEq, serde::Serialize)]
pub struct DonorBytesWire {
    /// Donor eff arc path (lowercase), e.g. "effect/assist/alucard/ef_alucard.eff".
    pub path: String,
    /// base64(stripped ef bytes).
    pub b64: String,
}

// ── Live ACMD capture + hitbox rules (wire forms match slight_replica hitbox_viewer) ──

/// One typed lua argument (plugin `LuaArg`): losslessly round-trips capture → edit → inject.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum LuaArgWire {
    #[serde(rename = "h")]
    Hash(u64),
    #[serde(rename = "n")]
    Num(f32),
    #[serde(rename = "i")]
    Int(i64),
    #[serde(rename = "b")]
    Bool(bool),
    #[serde(rename = "x")]
    Nil,
}

impl LuaArgWire {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            LuaArgWire::Num(n) => Some(*n),
            LuaArgWire::Int(i) => Some(*i as f32),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            LuaArgWire::Int(i) => Some(*i),
            LuaArgWire::Num(n) => Some(*n as i64),
            LuaArgWire::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_hash(&self) -> Option<u64> {
        match self {
            LuaArgWire::Hash(h) => Some(*h),
            LuaArgWire::Int(i) => Some(*i as u64),
            _ => None,
        }
    }
}

/// One captured ACMD call, as streamed by the plugin (`AcmdCapture`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CaptureLine {
    /// Fighter kind of the performing agent.
    pub kind: i32,
    /// hash40 of the motion name (e.g. "attack_air_n").
    pub motion: u64,
    /// Motion frame at call time.
    pub frame: f32,
    /// sv_animcmd function name ("ATTACK", "EFFECT_FOLLOW", …).
    pub func: String,
    pub args: Vec<LuaArgWire>,
}

/// Sparse ATTACK-arg overrides (plugin `HbOverrides`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct HbOverridesWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub damage: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub angle: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kbg: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fkb: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bkb: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x2: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y2: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z2: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hitlag: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdi: Option<f32>,
    // ── Attribute slots 17..35 ───────────────────────────────────────────────
    //
    // Sent as the NUMBERS the lua stack wants (the editor holds them as symbolic names);
    // `collision_attr` is a hash40. `None` means "leave the game's own value alone", so a
    // hitbox whose attributes were never resolved to a known constant is not clobbered.
    // Plugins predating these fields ignore them, so an old plugin build still applies the
    // geometry/damage overrides exactly as before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setoff: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lr_check: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clang: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_attack: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hitbox_attr: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_or_air: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtk: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shield_disable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflectable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absorbable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing_attack: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub situation_mask: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_mask: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_mask: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_finish_camera: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision_attr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_attr: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attack_region: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InjectRuleWire {
    pub frame: f32,
    pub args: Vec<LuaArgWire>,
}

/// One live hitbox rule (plugin `HitboxRule`); the full list replaces on every send.
fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

/// `frame_start`/`frame_end` scope suppress/override to ONE hit so multi-hit moves (which
/// reuse the same id across frames) stay independent.
#[derive(Clone, Debug, Serialize)]
pub struct HitboxRuleWire {
    pub motion: u64,
    /// Collision family: 0 attack, 1 grab, 2 wind. Omitted when 0 so old plugins default it.
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub category: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hitbox_id: Option<u64>,
    pub suppress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_start: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_end: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<HbOverridesWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject: Option<InjectRuleWire>,
}

// ── Shared live-override store ───────────────────────────────────────────────
//
// ONE per-kind runtime form that both the Effects panel and the Eff Editor game panel read
// and write, flushed to the plugin by a single debounced sender. (They used to keep separate
// forms with separate debounces and stomped each other's sends for the same kind id.)

const OVERRIDE_DEBOUNCE_MS: u128 = 200;

// This store carries ONLY user-set kind-level tweaks (color× / speed×, which apply to every
// spawn of the effect — that is what the user is asking for when they drag those fields).
// It used to carry a second class of entry, "modifiers derived from authored .eff edits",
// which also pushed `scale`. That derivation was removed: authored values are per emitter
// and this wire message is per kind, so it recoloured whole effects when one emitter was
// edited. Authored edits now go through the eff rebuild + hot-reload path (app::
// apply_authored_eff_live), which is per-emitter exact.

#[derive(Clone, Debug)]
pub struct LiveOverride {
    pub form: RpmEffectData,
    dirty_at: Option<Instant>,
    /// The USER set this entry's color×/speed — only these export as LAST_EFFECT_SET_*
    /// tweaks and persist in the project.
    user_tweaked: bool,
}

impl LiveOverride {
    fn new(form: RpmEffectData) -> Self {
        Self {
            form,
            dirty_at: None,
            user_tweaked: false,
        }
    }
}

#[derive(Default)]
pub struct LiveOverrides {
    entries: BTreeMap<u64, LiveOverride>,
}

impl LiveOverrides {
    /// The editable form for a kind, created from `init()` on first access.
    pub fn form_mut(
        &mut self,
        hash: u64,
        init: impl FnOnce() -> RpmEffectData,
    ) -> &mut RpmEffectData {
        &mut self
            .entries
            .entry(hash)
            .or_insert_with(|| LiveOverride::new(init()))
            .form
    }

    /// Adopt a form already reported by the game. Importing the game's current pins does not
    /// need to echo the complete form back; doing so could race a new spawn observation.
    pub fn set_form(&mut self, hash: u64, form: RpmEffectData) {
        let e = self
            .entries
            .entry(hash)
            .or_insert_with(|| LiveOverride::new(form.clone()));
        e.form = form;
        e.dirty_at = None;
    }

    /// Send every entry whose debounce has elapsed. Returns how many were sent.
    /// `include_scale` is false: size is a per-spawn value pushed through the spawn rules,
    /// never inferred from this kind-level form. Transform fields are omitted for the same
    /// reason — ACMD position/rotation are per spawn.
    pub fn flush_due(&mut self, link: &GameLink) -> usize {
        let mut sent = 0;
        for (hash, e) in self.entries.iter_mut() {
            if let Some(t) = e.dirty_at {
                if t.elapsed().as_millis() > OVERRIDE_DEBOUNCE_MS {
                    e.dirty_at = None;
                    link.send_modifier_edit(*hash, &e.form, false);
                    sent += 1;
                }
            }
        }
        sent
    }

    /// Send one entry immediately (the "Send now" button).
    pub fn flush_one(&mut self, hash: u64, link: &GameLink) {
        if let Some(e) = self.entries.get_mut(&hash) {
            e.dirty_at = None;
            link.send_modifier_edit(hash, &e.form, false);
        }
    }

    /// True while a debounced send is pending (keep repainting so it fires).
    pub fn any_dirty(&self) -> bool {
        self.entries.values().any(|e| e.dirty_at.is_some())
    }

    /// Flag the entry's color×/speed as USER-set (exports + persists as a live tweak).
    pub fn mark_tweak(&mut self, hash: u64) {
        if let Some(e) = self.entries.get_mut(&hash) {
            e.dirty_at = Some(Instant::now());
            e.user_tweaked = true;
        }
    }

    /// All user-set tweak entries: (hash, form) — export/persist as LiveTweaks.
    pub fn tweaked(&self) -> Vec<(u64, RpmEffectData)> {
        self.entries
            .iter()
            .filter(|(_, e)| e.user_tweaked)
            .map(|(h, e)| (*h, e.form.clone()))
            .collect()
    }

    /// Revert a user tweak: color/speed back to identity, unflag, and re-send.
    pub fn clear_tweak(&mut self, hash: u64) {
        if let Some(e) = self.entries.get_mut(&hash) {
            e.form.rainbow = Rainbow::default();
            e.form.speed = 1.0;
            e.user_tweaked = false;
            e.dirty_at = Some(Instant::now());
        }
    }

    /// Restore a tweak from a loaded project: sets color/speed, flags user_tweaked,
    /// and schedules a send.
    pub fn restore_tweak(&mut self, hash: u64, init: RpmEffectData) {
        let e = self
            .entries
            .entry(hash)
            .or_insert_with(|| LiveOverride::new(init.clone()));
        e.form.rainbow = init.rainbow;
        e.form.speed = init.speed;
        if e.form.effect_name == "0x0" {
            e.form.effect_name = init.effect_name;
        }
        e.user_tweaked = true;
        e.dirty_at = Some(Instant::now());
    }
}

// ── Link state ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    Disconnected,
    Connecting,
    Connected,
}

/// Sparse pins-only form the plugin reports alongside merged values (newer plugins) —
/// mirrors slight_replica kinds::Pinned. `Some` fields are active user overrides in-game.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PinsWire {
    pub scale: Option<f32>,
    pub rate: Option<f32>,
    pub pos: Option<Point3D>,
    pub rot: Option<Point3D>,
    pub visible: Option<bool>,
    pub frame: Option<f32>,
    pub color: Option<Color>,
    pub movement_state: Option<f32>,
}

impl PinsWire {
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
}

#[derive(Clone, Debug)]
pub struct LiveKind {
    pub name: String,
    /// Latest values from the plugin (merged observed + pins).
    pub data: RpmEffectData,
    /// First values seen for this kind this connection — the pristine spawn baseline
    /// edits are computed against so repeated edits don't compound.
    pub first: RpmEffectData,
    /// Active in-game pins (None on older plugins or when nothing is pinned).
    pub pins: Option<PinsWire>,
    pub updates: u64,
    pub last_update: Instant,
}

struct Shared {
    status: LinkStatus,
    client_id: Option<u64>,
    kinds: BTreeMap<u64, LiveKind>,
    /// Live ACMD capture log, keyed by motion hash (deduped; survives reconnects).
    captures: BTreeMap<u64, Vec<CaptureLine>>,
    /// Bumped on every new capture line — lets the app cheaply notice new data.
    captures_seq: u64,
    /// Live-carrier readiness reported by the plugin: 0 = none, 1 = staged/building, 2 = live.
    carrier_state: u8,
    /// The carrier battle object exists and is active — the real "can spawn now" signal.
    carrier_spawned: bool,
    /// Kinds the current carrier can serve — distinguishes "not up yet" from "up but does
    /// not know this kind".
    carrier_kinds: usize,
    /// Bumped on every CarrierStatus so callers can tell "no report yet" from "reported 0".
    carrier_seq: u64,
    /// (fighter kind, motion hash) → number of completed playbacks the plugin reported.
    /// A bump means "every line that motion produces has now been streamed".
    capture_ends: BTreeMap<(i32, u64), u64>,
    outbox: Vec<String>,
    last_error: Option<String>,
    frames_rx: u64,
    edits_tx: u64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            status: LinkStatus::Disconnected,
            client_id: None,
            kinds: BTreeMap::new(),
            captures: BTreeMap::new(),
            captures_seq: 0,
            carrier_state: 0,
            carrier_spawned: false,
            carrier_kinds: 0,
            carrier_seq: 0,
            capture_ends: BTreeMap::new(),
            outbox: Vec::new(),
            last_error: None,
            frames_rx: 0,
            edits_tx: 0,
        }
    }
}

pub struct GameLink {
    shared: Arc<Mutex<Shared>>,
    started: AtomicBool,
}

impl Default for GameLink {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            started: AtomicBool::new(false),
        }
    }
}

impl GameLink {
    /// Spawn the connection thread (idempotent). Called lazily when the eff editor opens.
    pub fn ensure_started(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        std::thread::Builder::new()
            .name("game-link".into())
            .spawn(move || link_thread(shared))
            .expect("spawn game-link thread");
    }

    pub fn status(&self) -> LinkStatus {
        self.shared
            .lock()
            .map(|s| s.status)
            .unwrap_or(LinkStatus::Disconnected)
    }

    pub fn last_error(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.last_error.clone())
    }

    pub fn stats(&self) -> (u64, u64) {
        self.shared
            .lock()
            .map(|s| (s.frames_rx, s.edits_tx))
            .unwrap_or((0, 0))
    }

    /// Snapshot of all live kinds (id = hash40 of effect name).
    pub fn kinds(&self) -> Vec<(u64, LiveKind)> {
        self.shared
            .lock()
            .map(|s| s.kinds.iter().map(|(k, v)| (*k, v.clone())).collect())
            .unwrap_or_default()
    }

    pub fn kind(&self, id: u64) -> Option<LiveKind> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.kinds.get(&id).cloned())
    }

    pub fn is_live(&self, id: u64) -> bool {
        self.shared
            .lock()
            .map(|s| s.kinds.contains_key(&id))
            .unwrap_or(false)
    }

    /// Replace the plugin's live spawn-rule list (suppress/retime ACMD effect spawns).
    /// Send the FULL current rule set every time — an empty slice clears all rules.
    pub fn send_spawn_rules(&self, rules: &[SpawnRuleWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "spawn_rules": rules }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Replace the plugin's live transplant alias list (copy/replaced kind → donor kind,
    /// optionally costume-gated). Full-list replace; empty clears all aliases.
    pub fn send_effect_aliases(&self, aliases: &[EffectAliasWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "effect_aliases": aliases }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Cross-fighter donor eff files the plugin co-loads with each target fighter's
    /// effects (smashline-transplant mechanism), so donor content is spawnable live.
    pub fn send_donor_effs(&self, specs: &[DonorEffWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "donor_effs": specs })) else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Stripped donor eff bytes for the plugin to inject as resident data (live
    /// cross-character transplant). Sent whenever the referenced donor set changes.
    pub fn send_donor_bytes(&self, donors: &[DonorBytesWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "donor_bytes": donors }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Custom names (transplant copies) so the plugin resolves their hashes for display
    /// instead of falling back to hex.
    pub fn send_effect_names(&self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "effect_names": names }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Client id assigned by the plugin for this connection (changes on reconnect).
    pub fn client_id(&self) -> Option<u64> {
        self.shared.lock().ok().and_then(|s| s.client_id)
    }

    /// Kinds the plugin reports active user pins for (fresh-session desync detection).
    pub fn pinned_kinds(&self) -> Vec<(u64, LiveKind)> {
        self.shared
            .lock()
            .map(|s| {
                s.kinds
                    .iter()
                    .filter(|(_, v)| v.pins.as_ref().map(|p| p.any()).unwrap_or(false))
                    .map(|(k, v)| (*k, v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ask the plugin to clear ALL its pinned edits (incl. the SD save) and re-notify
    /// pristine values.
    pub fn send_reset_pins(&self) {
        let frame = "<TCP_MESSAGE>{\"command\":\"reset_pins\"}</TCP_MESSAGE>".to_string();
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Tell the plugin the live-eff manifest / merged files on the Eden SD changed —
    /// it refreshes its Arcropolis file-provider registrations.
    pub fn send_live_eff_reload(&self) {
        let frame = "<TCP_MESSAGE>{\"command\":\"live_eff_reload\"}</TCP_MESSAGE>".to_string();
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Ask the plugin to synchronously LIVE RE-READ a fighter's resident eff: swap it for
    /// the deployed merged bytes + reparse, so a cross-fighter transplant (or authored eff
    /// edit) renders mid-match without a re-entry. `arc_path` = "effect/fighter/<f>/ef_<f>.eff".
    pub fn send_force_reread(&self, arc_path: &str) {
        let frame = format!(
            "<TCP_MESSAGE>{{\"command\":\"force_reread\",\"path\":\"{}\"}}</TCP_MESSAGE>",
            arc_path.to_lowercase()
        );
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Ask the plugin to write `sd:/effect_viewer_probe.txt` (serving-chain diagnosis).
    pub fn send_live_eff_probe(&self) {
        let frame = "<TCP_MESSAGE>{\"command\":\"live_eff_probe\"}</TCP_MESSAGE>".to_string();
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Captured ACMD lines for one motion (hash40 of the move/motion name), sorted by frame.
    pub fn captures_for(&self, motion: u64) -> Vec<CaptureLine> {
        let mut v = self
            .shared
            .lock()
            .ok()
            .and_then(|s| s.captures.get(&motion).cloned())
            .unwrap_or_default();
        v.sort_by(|a, b| {
            a.frame
                .partial_cmp(&b.frame)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Every captured line across ALL motions, tagged with its motion hash. Used to discover
    /// every place an effect is used (each move performed live contributes its motion's lines).
    pub fn all_captures(&self) -> Vec<(u64, CaptureLine)> {
        self.shared
            .lock()
            .ok()
            .map(|s| {
                s.captures
                    .iter()
                    .flat_map(|(m, lines)| lines.iter().map(move |l| (*m, l.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Monotonic counter of received capture lines (cheap "anything new?" check).
    /// Live-carrier readiness: `(state, kinds, reports_seen)`.
    ///
    /// `state` is 0 none / 1 staged / 2 live. `reports_seen` is 0 with plugin builds that
    /// predate the `CarrierStatus` notify, so callers can degrade gracefully rather than
    /// waiting forever for a signal that will never arrive.
    pub fn carrier_status(&self) -> (u8, usize, u64, bool) {
        self.shared
            .lock()
            .map(|s| {
                (
                    s.carrier_state,
                    s.carrier_kinds,
                    s.carrier_seq,
                    s.carrier_spawned,
                )
            })
            .unwrap_or((0, 0, 0, false))
    }

    pub fn captures_seq(&self) -> u64 {
        self.shared.lock().map(|s| s.captures_seq).unwrap_or(0)
    }

    /// How many capture lines are held for one motion (optionally restricted to a fighter
    /// kind). Cheap "did more arrive?" check that avoids cloning the whole bucket.
    pub fn captures_count(&self, motion: u64, kind: Option<i32>) -> usize {
        self.shared
            .lock()
            .ok()
            .and_then(|s| {
                s.captures.get(&motion).map(|lines| match kind {
                    Some(k) => lines.iter().filter(|l| l.kind == k).count(),
                    None => lines.len(),
                })
            })
            .unwrap_or(0)
    }

    /// How many times the plugin has reported this motion finishing (i.e. "its script has
    /// been streamed in full"). `kind = None` sums every fighter that played the motion.
    /// Stays 0 with plugin builds predating the `AcmdCaptureEnd` notify.
    pub fn capture_end_count(&self, motion: u64, kind: Option<i32>) -> u64 {
        self.shared
            .lock()
            .map(|s| match kind {
                Some(k) => s.capture_ends.get(&(k, motion)).copied().unwrap_or(0),
                None => s
                    .capture_ends
                    .iter()
                    .filter(|((_, m), _)| *m == motion)
                    .map(|(_, n)| *n)
                    .sum(),
            })
            .unwrap_or(0)
    }

    /// Replace the plugin's live hitbox-rule list (modify/suppress/inject ATTACKs).
    /// Always the FULL set — an empty slice clears all rules.
    pub fn send_hitbox_rules(&self, rules: &[HitboxRuleWire]) {
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "hitbox_rules": rules }))
        else {
            return;
        };
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }

    /// Queue a sparse kind-modifier edit. Color/speed controls must not upload the live
    /// form's position/rotation: those values are ACMD script offsets, while a carrier-owned
    /// transplant is physically world-space, so accidentally pinning them moves it to the
    /// stage origin. Authored EFF modifiers may additionally include kind-global scale.
    pub fn send_modifier_edit(&self, id: u64, data: &RpmEffectData, include_scale: bool) {
        let mut value = serde_json::json!({
            "speed": data.speed,
            "rainbow": {
                "color": data.rainbow.color,
            },
        });
        if include_scale {
            value["scale"] = serde_json::json!(data.scale);
        }
        let payload = serde_json::json!({ "id": id, "newValue": value.to_string() });
        let frame = format!("<TCP_MESSAGE>{payload}</TCP_MESSAGE>");
        if let Ok(mut s) = self.shared.lock() {
            s.outbox.push(frame);
            s.edits_tx += 1;
        }
    }
}

// ── Connection thread ─────────────────────────────────────────────────────────

fn link_thread(shared: Arc<Mutex<Shared>>) {
    loop {
        {
            let mut s = shared.lock().unwrap();
            s.status = LinkStatus::Connecting;
        }
        let addr: std::net::SocketAddr = PLUGIN_ADDR.parse().unwrap();
        match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            Ok(stream) => {
                {
                    let mut s = shared.lock().unwrap();
                    s.status = LinkStatus::Connected;
                    s.last_error = None;
                }
                let reason = serve_connection(&shared, stream);
                let mut s = shared.lock().unwrap();
                s.status = LinkStatus::Disconnected;
                s.last_error = reason;
            }
            Err(e) => {
                let mut s = shared.lock().unwrap();
                s.status = LinkStatus::Disconnected;
                s.last_error = Some(format!("connect: {e}"));
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn serve_connection(shared: &Arc<Mutex<Shared>>, mut stream: TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = stream.set_nodelay(true);
    let mut buf = String::new();
    let mut chunk = [0u8; 8192];

    loop {
        // Outbound edits first — they're latency-sensitive.
        let pending: Vec<String> = {
            let mut s = shared.lock().unwrap();
            std::mem::take(&mut s.outbox)
        };
        for msg in pending {
            if let Err(e) = stream.write_all(msg.as_bytes()) {
                return Some(format!("send: {e}"));
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) => return Some("closed by plugin".into()),
            Ok(n) => {
                buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
                for payload in extract_frames(&mut buf) {
                    handle_frame(shared, &payload);
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Some(format!("recv: {e}")),
        }
    }
}

fn extract_frames(buf: &mut String) -> Vec<String> {
    const OPEN: &str = "<TCP_MESSAGE>";
    const CLOSE: &str = "</TCP_MESSAGE>";
    let mut out = Vec::new();
    loop {
        let (Some(s), Some(e)) = (buf.find(OPEN), buf.find(CLOSE)) else {
            break;
        };
        if e < s {
            // Torn close tag before an open — drop the garbage prefix.
            *buf = buf[e + CLOSE.len()..].to_string();
            continue;
        }
        let payload = buf[s + OPEN.len()..e].trim().to_string();
        *buf = buf[e + CLOSE.len()..].to_string();
        if !payload.is_empty() {
            out.push(payload);
        }
    }
    if buf.len() > 1 << 20 {
        buf.clear();
    }
    out
}

fn handle_frame(shared: &Arc<Mutex<Shared>>, payload: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let Some(header) = v.get("header").and_then(|h| h.as_str()) else {
        return;
    };
    // The plugin serializes `body` as a JSON *string*; tolerate an object too.
    let body: serde_json::Value = match v.get("body") {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
        Some(other) => other.clone(),
        None => serde_json::Value::Null,
    };

    let mut s = shared.lock().unwrap();
    s.frames_rx += 1;
    match header {
        "Notify" => {
            let Some(n) = body.get("Notify") else { return };
            let Some(id) = n.get("id").and_then(|i| i.as_u64()) else {
                return;
            };
            let name = n
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string();
            let data: RpmEffectData = match n.get("value_in_json") {
                Some(serde_json::Value::String(raw)) => match serde_json::from_str(raw) {
                    Ok(d) => d,
                    Err(_) => return,
                },
                Some(obj) => match serde_json::from_value(obj.clone()) {
                    Ok(d) => d,
                    Err(_) => return,
                },
                None => return,
            };
            // Sparse pins-only form (newer plugins) — user overrides already active in-game.
            let pins: Option<PinsWire> = match n.get("pinned_in_json") {
                Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
                Some(serde_json::Value::Null) | None => None,
                Some(obj) => serde_json::from_value(obj.clone()).ok(),
            };
            match s.kinds.get_mut(&id) {
                Some(k) => {
                    k.name = name;
                    k.data = data;
                    k.pins = pins;
                    k.updates += 1;
                    k.last_update = Instant::now();
                }
                None => {
                    s.kinds.insert(
                        id,
                        LiveKind {
                            name,
                            first: data.clone(),
                            data,
                            pins,
                            updates: 1,
                            last_update: Instant::now(),
                        },
                    );
                }
            }
        }
        "AcmdCapture" => {
            let Some(c) = body.get("AcmdCapture") else {
                return;
            };
            let Ok(line) = serde_json::from_value::<CaptureLine>(c.clone()) else {
                return;
            };
            let bucket = s.captures.entry(line.motion).or_default();
            // Plugin dedupes per session, but a reconnect re-sends the whole log.
            if !bucket.contains(&line) {
                bucket.push(line);
                s.captures_seq += 1;
            }
        }
        "AcmdCaptureEnd" => {
            // "That motion just finished playing" — every line it produces has already been
            // delivered (the plugin holds these back until the line backlog drains).
            let Some(e) = body.get("AcmdCaptureEnd") else {
                return;
            };
            let (Some(kind), Some(motion)) = (
                e.get("kind").and_then(|k| k.as_i64()),
                e.get("motion").and_then(|m| m.as_u64()),
            ) else {
                return;
            };
            *s.capture_ends.entry((kind as i32, motion)).or_insert(0) += 1;
        }
        "CarrierStatus" => {
            // How far along the game is in taking the carrier we pushed. 0 = none staged,
            // 1 = staged/building, 2 = live and serving. The editor uses this to keep its
            // "sending" state up until the game has ACTUALLY taken the edit, instead of
            // clearing the moment the bytes left the socket.
            let Some(c) = body.get("CarrierStatus") else {
                return;
            };
            s.carrier_state = c.get("state").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            // The battle object actually existing is what `spawn_via_carrier` requires;
            // `state` alone can be 2 while nothing can spawn yet.
            s.carrier_spawned = c.get("spawned").and_then(|v| v.as_bool()).unwrap_or(false);
            s.carrier_kinds = c.get("kinds").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            s.carrier_seq += 1;
        }
        "Remove" => {
            if let Some(id) = body
                .get("Remove")
                .and_then(|r| r.get("id"))
                .and_then(|i| i.as_u64())
            {
                s.kinds.remove(&id);
            }
        }
        "RemoveAll" => s.kinds.clear(),
        "GiveClientId" => {
            s.client_id = body
                .get("GiveClientId")
                .and_then(|g| g.get("client_id"))
                .and_then(|c| c.as_u64());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte-exact replica of the plugin's `emit()`: body is a JSON-escaped *string*.
    fn plugin_frame(header: &str, body: &serde_json::Value) -> String {
        let body_str = serde_json::to_string(body).unwrap();
        let body_esc = serde_json::to_string(&body_str).unwrap();
        format!("<TCP_MESSAGE>{{\"header\":\"{header}\",\"body\":{body_esc}}}</TCP_MESSAGE>")
    }

    #[test]
    fn parses_notify_remove_and_torn_frames() {
        let shared = Arc::new(Mutex::new(Shared::default()));

        let data = RpmEffectData {
            effect_name: "sys_flyroll_smoke".into(),
            scale: 1.2,
            ..Default::default()
        };
        let value_in_json = serde_json::to_string(&data).unwrap();
        let notify = plugin_frame(
            "Notify",
            &serde_json::json!({
                "Notify": { "id": 0x1154cb72bfu64, "name": "sys_flyroll_smoke", "value_in_json": value_in_json }
            }),
        );
        let handshake = "<TCP_MESSAGE>{\"header\":\"RemoveAll\",\"body\":\"{}\"}</TCP_MESSAGE>";

        // Feed the stream in torn chunks like a real socket would.
        let stream = format!("{handshake}{notify}");
        let (a, b) = stream.split_at(stream.len() / 2);
        let mut buf = String::new();
        buf.push_str(a);
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }
        buf.push_str(b);
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }

        let s = shared.lock().unwrap();
        let kind = s.kinds.get(&0x1154cb72bf).expect("kind parsed");
        assert_eq!(kind.name, "sys_flyroll_smoke");
        assert!((kind.data.scale - 1.2).abs() < 1e-6);
        assert_eq!(kind.first.scale, kind.data.scale);
        drop(s);

        let remove = plugin_frame(
            "Remove",
            &serde_json::json!({ "Remove": { "id": 0x1154cb72bfu64 } }),
        );
        let mut buf = remove;
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }
        assert!(shared.lock().unwrap().kinds.is_empty());
    }

    #[test]
    fn acmd_capture_parses_from_plugin_emit_form() {
        let shared: Arc<Mutex<Shared>> = Arc::default();
        // Exactly what the plugin serializes: CaptureLine with tagged LuaArgs.
        let capture = plugin_frame(
            "AcmdCapture",
            &serde_json::json!({
                "AcmdCapture": {
                    "kind": 0,
                    "motion": 0x1234u64,
                    "frame": 3.0,
                    "func": "ATTACK",
                    "args": [
                        {"t":"i","v":0}, {"t":"i","v":0}, {"t":"h","v":0x031ed91fcau64},
                        {"t":"n","v":8.0}, {"t":"i","v":361}, {"t":"i","v":100},
                        {"t":"i","v":0}, {"t":"i","v":40}, {"t":"n","v":4.0},
                        {"t":"n","v":0.0}, {"t":"n","v":8.0}, {"t":"n","v":6.0},
                        {"t":"x","v":null}
                    ]
                }
            }),
        );
        let mut buf = capture;
        for payload in extract_frames(&mut buf) {
            handle_frame(&shared, &payload);
        }
        let s = shared.lock().unwrap();
        let lines = s.captures.get(&0x1234).expect("capture stored");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].func, "ATTACK");
        assert_eq!(lines[0].args[2].as_hash(), Some(0x031ed91fca));
        assert_eq!(lines[0].args[3].as_f32(), Some(8.0));
        assert_eq!(lines[0].args[12], LuaArgWire::Nil);
        assert_eq!(s.captures_seq, 1);
        drop(s);

        // Re-delivery (reconnect resend) must not duplicate.
        let s2 = shared.clone();
        let mut buf2 = plugin_frame(
            "AcmdCapture",
            &serde_json::json!({
                "AcmdCapture": {
                    "kind": 0, "motion": 0x1234u64, "frame": 3.0, "func": "ATTACK",
                    "args": [
                        {"t":"i","v":0}, {"t":"i","v":0}, {"t":"h","v":0x031ed91fcau64},
                        {"t":"n","v":8.0}, {"t":"i","v":361}, {"t":"i","v":100},
                        {"t":"i","v":0}, {"t":"i","v":40}, {"t":"n","v":4.0},
                        {"t":"n","v":0.0}, {"t":"n","v":8.0}, {"t":"n","v":6.0},
                        {"t":"x","v":null}
                    ]
                }
            }),
        );
        for payload in extract_frames(&mut buf2) {
            handle_frame(&s2, &payload);
        }
        assert_eq!(s2.lock().unwrap().captures.get(&0x1234).unwrap().len(), 1);
    }

    /// A capture line for a fighter kind, in the plugin's exact emit form.
    fn capture_frame(kind: i32, motion: u64, frame: f32) -> String {
        plugin_frame(
            "AcmdCapture",
            &serde_json::json!({
                "AcmdCapture": {
                    "kind": kind, "motion": motion, "frame": frame, "func": "ATTACK",
                    "args": [
                        {"t":"i","v":0}, {"t":"i","v":0}, {"t":"h","v":0x031ed91fcau64},
                        {"t":"n","v":8.0}, {"t":"i","v":361}, {"t":"i","v":100},
                        {"t":"i","v":0}, {"t":"i","v":40}, {"t":"n","v":4.0},
                        {"t":"n","v":0.0}, {"t":"n","v":8.0}, {"t":"n","v":6.0},
                        {"t":"x","v":null}
                    ]
                }
            }),
        )
    }

    fn feed(link: &GameLink, payloads: &[String]) {
        for p in payloads {
            let mut buf = p.clone();
            for payload in extract_frames(&mut buf) {
                handle_frame(&link.shared, &payload);
            }
        }
    }

    /// `AcmdCaptureEnd` is the "this move finished, its script is fully streamed" signal the
    /// deferred auto-fetch keys off. It must be counted per (fighter kind, motion) and must
    /// NOT land in the capture buckets.
    #[test]
    fn acmd_capture_end_counts_per_kind_and_motion() {
        let link = GameLink::default();
        let end = |kind: i32, motion: u64| {
            plugin_frame(
                "AcmdCaptureEnd",
                &serde_json::json!({ "AcmdCaptureEnd": { "kind": kind, "motion": motion } }),
            )
        };

        assert_eq!(link.capture_end_count(0x1234, Some(8)), 0);
        feed(&link, &[capture_frame(8, 0x1234, 3.0), end(8, 0x1234)]);
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 1);
        // The marker is not a capture line.
        assert_eq!(link.captures_for(0x1234).len(), 1);

        // A different fighter playing the same motion is tracked separately…
        feed(&link, &[end(9, 0x1234)]);
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 1);
        assert_eq!(link.capture_end_count(0x1234, Some(9)), 1);
        // …but `None` sums every kind, for when the editor has no fighter selected.
        assert_eq!(link.capture_end_count(0x1234, None), 2);

        // Re-performing the move bumps the count again — that is what re-triggers adoption.
        feed(&link, &[end(8, 0x1234)]);
        assert_eq!(link.capture_end_count(0x1234, Some(8)), 2);
        // Unrelated motions stay at zero.
        assert_eq!(link.capture_end_count(0x5678, Some(8)), 0);
    }

    /// The settle window uses `captures_count` to tell "more lines arrived" from "some other
    /// move produced a line", so it has to be kind-filtered and per-motion.
    #[test]
    fn captures_count_filters_by_kind_and_motion() {
        let link = GameLink::default();
        feed(
            &link,
            &[
                capture_frame(8, 0x1234, 3.0),
                capture_frame(8, 0x1234, 5.0),
                capture_frame(9, 0x1234, 3.0),
                capture_frame(8, 0x5678, 3.0),
            ],
        );
        assert_eq!(link.captures_count(0x1234, Some(8)), 2);
        assert_eq!(link.captures_count(0x1234, Some(9)), 1);
        assert_eq!(link.captures_count(0x1234, None), 3);
        assert_eq!(link.captures_count(0x5678, Some(8)), 1);
        assert_eq!(link.captures_count(0xdead, Some(8)), 0);
    }

    #[test]
    fn outbound_hitbox_rules_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[
            HitboxRuleWire {
                motion: 0x99,
                category: 0,
                hitbox_id: Some(1),
                suppress: false,
                frame_start: Some(6.5),
                frame_end: Some(8.5),
                overrides: Some(HbOverridesWire {
                    damage: Some(12.0),
                    ..Default::default()
                }),
                inject: None,
            },
            HitboxRuleWire {
                motion: 0x99,
                category: 1,
                hitbox_id: None,
                suppress: false,
                frame_start: None,
                frame_end: None,
                overrides: None,
                inject: Some(InjectRuleWire {
                    frame: 5.0,
                    args: vec![LuaArgWire::Int(2), LuaArgWire::Hash(0xabc), LuaArgWire::Nil],
                }),
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let rules = v.get("hitbox_rules").and_then(|r| r.as_array()).unwrap();
        // Field names the plugin's serde(Deserialize) expects.
        assert_eq!(rules[0]["motion"].as_u64(), Some(0x99));
        assert_eq!(rules[0]["hitbox_id"].as_u64(), Some(1));
        assert_eq!(rules[0]["overrides"]["damage"].as_f64(), Some(12.0));
        // Frame window scopes the override to one hit (multi-hit independence).
        assert_eq!(rules[0]["frame_start"].as_f64(), Some(6.5));
        assert_eq!(rules[0]["frame_end"].as_f64(), Some(8.5));
        assert!(rules[0].get("inject").is_none());
        // category 0 (attack) is omitted so old plugins default it; grab (1) is sent.
        assert!(rules[0].get("category").is_none());
        assert_eq!(rules[1]["category"].as_u64(), Some(1));
        // Inject rule carries no frame window (it fires at its own frame).
        assert!(rules[1].get("frame_start").is_none());
        assert_eq!(rules[1]["inject"]["frame"].as_f64(), Some(5.0));
        assert_eq!(rules[1]["inject"]["args"][0]["t"].as_str(), Some("i"));
        assert_eq!(rules[1]["inject"]["args"][1]["t"].as_str(), Some("h"));
        assert_eq!(rules[1]["inject"]["args"][2]["t"].as_str(), Some("x"));
    }

    /// Attribute edits (Hit Properties / Collision Masks / Effect-Sound) used to be dropped
    /// on the floor: the override payload had no slot for them, so picking a value in the UI
    /// changed nothing in game. Guard both that they are SENT and that the JSON keys are the
    /// ones the plugin's `HbOverrides` deserializes.
    #[test]
    fn outbound_attribute_overrides_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_hitbox_rules(&[HitboxRuleWire {
            motion: 0x99,
            category: 0,
            hitbox_id: Some(0),
            suppress: false,
            frame_start: None,
            frame_end: None,
            overrides: Some(HbOverridesWire {
                setoff: Some(1),
                lr_check: Some(3),
                clang: Some(true),
                add_attack: Some(0),
                hitbox_attr: Some(0.0),
                ground_or_air: Some(2),
                mtk: Some(false),
                shield_disable: Some(true),
                reflectable: Some(false),
                absorbable: Some(true),
                landing_attack: Some(false),
                situation_mask: Some(3),
                category_mask: Some(0x3F),
                part_mask: Some(0x1F),
                no_finish_camera: Some(true),
                collision_attr: Some(0x15a2c502b3),
                sound_level: Some(1),
                sound_attr: Some(1),
                attack_region: Some(4),
                ..Default::default()
            }),
            inject: None,
        }]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        let ov = &v["hitbox_rules"][0]["overrides"];
        assert_eq!(ov["setoff"].as_i64(), Some(1));
        assert_eq!(ov["lr_check"].as_i64(), Some(3));
        assert_eq!(ov["clang"].as_bool(), Some(true));
        assert_eq!(ov["add_attack"].as_i64(), Some(0));
        assert_eq!(ov["hitbox_attr"].as_f64(), Some(0.0));
        assert_eq!(ov["ground_or_air"].as_i64(), Some(2));
        assert_eq!(ov["mtk"].as_bool(), Some(false));
        assert_eq!(ov["shield_disable"].as_bool(), Some(true));
        assert_eq!(ov["reflectable"].as_bool(), Some(false));
        assert_eq!(ov["absorbable"].as_bool(), Some(true));
        assert_eq!(ov["landing_attack"].as_bool(), Some(false));
        assert_eq!(ov["situation_mask"].as_i64(), Some(3));
        assert_eq!(ov["category_mask"].as_i64(), Some(0x3F));
        assert_eq!(ov["part_mask"].as_i64(), Some(0x1F));
        assert_eq!(ov["no_finish_camera"].as_bool(), Some(true));
        assert_eq!(ov["collision_attr"].as_u64(), Some(0x15a2c502b3));
        assert_eq!(ov["sound_level"].as_i64(), Some(1));
        assert_eq!(ov["sound_attr"].as_i64(), Some(1));
        assert_eq!(ov["attack_region"].as_i64(), Some(4));
        // Unresolved slots stay absent so the plugin keeps the script's own value.
        let bare = serde_json::to_value(HbOverridesWire::default()).unwrap();
        assert!(bare.as_object().unwrap().is_empty());
    }

    #[test]
    fn outbound_effect_aliases_match_plugin_field_names() {
        let link = GameLink::default();
        link.send_effect_aliases(&[
            EffectAliasWire {
                from: 0x111,
                to: 0x222,
                slots: vec![],
            },
            EffectAliasWire {
                from: 0x333,
                to: 0x444,
                slots: vec![1, 3],
            },
        ]);
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        // Field names the plugin's spawn_rules::EffectAlias serde(Deserialize) expects.
        let aliases = v.get("effect_aliases").and_then(|a| a.as_array()).unwrap();
        assert_eq!(aliases[0]["from"].as_u64(), Some(0x111));
        assert_eq!(aliases[0]["to"].as_u64(), Some(0x222));
        // Empty slots (all costumes) omitted so the plugin's serde(default) fills it.
        assert!(aliases[0].get("slots").is_none());
        assert_eq!(aliases[1]["slots"][0].as_u64(), Some(1));
        assert_eq!(aliases[1]["slots"][1].as_u64(), Some(3));
    }

    #[test]
    fn outbound_live_tweak_omits_spawn_transform_fields() {
        let link = GameLink::default();
        let mut data = RpmEffectData::default();
        data.pos = Point3D {
            x: 6.0,
            y: -2.0,
            z: 1.5,
        };
        data.rot = Point3D {
            x: 0.0,
            y: 110.0,
            z: 0.0,
        };
        data.scale = 0.7;
        data.speed = 1.25;
        data.rainbow.color.red = 0.5;
        link.send_modifier_edit(0x1311e844a4, &data, false);

        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let outer: serde_json::Value = serde_json::from_str(inner).unwrap();
        let edit: serde_json::Value =
            serde_json::from_str(outer["newValue"].as_str().unwrap()).unwrap();

        assert_eq!(edit["speed"].as_f64(), Some(1.25));
        assert_eq!(edit["rainbow"]["color"]["red"].as_f64(), Some(0.5));
        assert!(edit.get("pos").is_none());
        assert!(edit.get("rot").is_none());
        assert!(edit.get("scale").is_none());
    }

    #[test]
    fn outbound_authored_modifiers_only_add_scale() {
        let link = GameLink::default();
        let mut data = RpmEffectData::default();
        data.scale = 1.75;
        data.pos.x = 99.0;
        link.send_modifier_edit(0x109297479a, &data, true);

        let frame = link.shared.lock().unwrap().outbox[0].clone();
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        let outer: serde_json::Value = serde_json::from_str(inner).unwrap();
        let edit: serde_json::Value =
            serde_json::from_str(outer["newValue"].as_str().unwrap()).unwrap();

        assert_eq!(edit["scale"].as_f64(), Some(1.75));
        assert!(edit.get("pos").is_none());
        assert!(edit.get("rot").is_none());
    }
}
