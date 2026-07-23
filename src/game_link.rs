// Game link: TCP client to the slight_replica plugin (127.0.0.1:7878).
// Speaks the plugin's `<TCP_MESSAGE>{json}</TCP_MESSAGE>` framing (formerly RPM's role):
//  - inbound  `{"header":"Notify","body":"{\"Notify\":{id,name,value_in_json}}"}` = live
//    effect-kind tabs (id = hash40 of the effect name, value = RpmEffectData JSON)
//  - outbound `{"id":<hash>,"newValue":"<RpmEffectData JSON>"}` = an edit; the plugin
//    diffs the form against what it last notified and pins only the changed fields.

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

/// Wire form of plugin `spawn_rules::EffectAlias` — live one-slot kind substitution:
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
/// that the plugin injects as resident data for a live cross-character one-slot.
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

#[derive(Clone, Debug)]
pub struct LiveOverride {
    pub form: RpmEffectData,
    dirty_at: Option<Instant>,
    /// The USER set this entry's color×/speed (vs. values derived from authored edits) —
    /// only these export as LAST_EFFECT_SET_* tweaks and persist in the project.
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

    /// Overwrite the form (Effects-panel path) and schedule a send.
    pub fn set_form(&mut self, hash: u64, form: RpmEffectData) {
        let e = self
            .entries
            .entry(hash)
            .or_insert_with(|| LiveOverride::new(form.clone()));
        e.form = form;
        e.dirty_at = Some(Instant::now());
    }

    /// Schedule a send after an in-place `form_mut` edit.
    pub fn mark_dirty(&mut self, hash: u64) {
        if let Some(e) = self.entries.get_mut(&hash) {
            e.dirty_at = Some(Instant::now());
        }
    }

    /// Send every entry whose debounce has elapsed. Returns how many were sent.
    pub fn flush_due(&mut self, link: &GameLink) -> usize {
        let mut sent = 0;
        for (hash, e) in self.entries.iter_mut() {
            if let Some(t) = e.dirty_at {
                if t.elapsed().as_millis() > OVERRIDE_DEBOUNCE_MS {
                    e.dirty_at = None;
                    link.send_edit(*hash, &e.form);
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
            link.send_edit(hash, &e.form);
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

    /// Replace the plugin's live one-slot alias list (copy/replaced kind → donor kind,
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
    /// cross-character one-slot). Sent whenever the referenced donor set changes.
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

    /// Custom names (one-slot copies) so the plugin resolves their hashes for display
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
    /// the deployed merged bytes + reparse, so a cross-fighter one-slot (or authored eff
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
    pub fn captures_seq(&self) -> u64 {
        self.shared.lock().map(|s| s.captures_seq).unwrap_or(0)
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

    /// Queue a full-form edit for a kind. The plugin pins whichever fields differ from
    /// the form it last sent us, so unchanged fields are a no-op.
    pub fn send_edit(&self, id: u64, data: &RpmEffectData) {
        let value = match serde_json::to_string(data) {
            Ok(v) => v,
            Err(_) => return,
        };
        let payload = serde_json::json!({ "id": id, "newValue": value });
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
    fn outbound_edit_matches_plugin_inbound_format() {
        let link = GameLink::default();
        link.send_edit(0x1154cb72bf, &RpmEffectData::default());
        let frame = link.shared.lock().unwrap().outbox[0].clone();
        assert!(frame.starts_with("<TCP_MESSAGE>") && frame.ends_with("</TCP_MESSAGE>"));
        let inner = &frame["<TCP_MESSAGE>".len()..frame.len() - "</TCP_MESSAGE>".len()];
        // Must parse the way the plugin's parse_tcp_payload does: {id, newValue:"<json>"}
        let v: serde_json::Value = serde_json::from_str(inner).unwrap();
        assert_eq!(v.get("id").and_then(|i| i.as_u64()), Some(0x1154cb72bf));
        let nv = v.get("newValue").and_then(|n| n.as_str()).unwrap();
        let parsed: RpmEffectData = serde_json::from_str(nv).unwrap();
        assert_eq!(parsed, RpmEffectData::default());
    }
}
