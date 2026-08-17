//! Jorge debuggable_server — Notify/Remove + SD transactions + TCP inbound.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::slight::effect_viewer::apply::ParsedEdit;
use crate::slight::effect_viewer::effect_data::{Color, EffectData, Point3D, RpmEffectData};
use crate::slight::smash_utils;

static SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize)]
struct OutRecord {
    seq: u64,
    header: String,
    body: String,
}

pub fn init() {
    smash_utils::ensure_slight_dirs();
    crate::slight::effect_viewer::kinds::load_saved_pins();
    let port = smash_utils::rpm_listen_port();
    crate::rust_extender::net::simple_server::start(port);
    skyline::println!(
        "[SLight] Debuggable server — TCP :{port}, SD {}",
        smash_utils::DEBUGGABLES_DIR
    );
}

/// Jorge sends RemoveAll + GiveClientId, then re-notifies every tracked effect.
pub fn on_rpm_client_connected(client_id: u64) {
    smash_utils::ensure_slight_dirs();
    let _ = std::fs::write(smash_utils::CLIENT_ID_FILE, client_id.to_string());
    // Force the next carrier-status pump to re-send: everything emitted before this client
    // connected went to the SD fallback, so without this the editor starts blind.
    crate::slight::effect_viewer::effect_reload::reset_carrier_status_latch();

    // Do NOT touch shared state (kinds/tracker) from this SERVER thread: a contended
    // parking_lot lock parks the waiter, and parked threads never wake in this environment
    // (same class as the std::thread::sleep bug) — the game thread froze on training entry
    // waiting for KINDS held here. Just flag a resync; the game thread does the notify pass.
    RESYNC.store(true, std::sync::atomic::Ordering::Release);
    skyline::println!("[SLight] RPM client {client_id} connected — resync queued");
}

/// Set on RPM connect (server thread); consumed by the game thread each frame.
pub static RESYNC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Game-thread half of the connect handshake: re-notify every kind tab.
pub fn resync_if_requested() {
    if !RESYNC.swap(false, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let tabs = crate::slight::effect_viewer::kinds::all();
    let count = tabs.len();
    for (id, name, data) in tabs {
        notify_effect(id, &name, &data);
    }
    // The new client also gets the whole ACMD capture log (drained by the facade).
    crate::slight::hitbox_viewer::requeue_all();
    skyline::println!("[SLight] RPM resync — flushed {count} kind tab(s)");
}

/// Is there anywhere for an emitted message to go? Callers that build an expensive payload
/// should check this FIRST — `emit` drops the message otherwise, and the serialization to
/// produce it would be pure waste (see `notify_effect`).
fn emit_wanted() -> bool {
    crate::rust_extender::net::simple_server::has_client()
        || crate::slight::smash_utils::debug_logging_enabled()
}

fn emit(header: &str, body: &serde_json::Value) {
    if crate::rust_extender::net::simple_server::has_client() {
        let body_str = serde_json::to_string(body).unwrap_or_else(|_| "{}".into());
        let body_esc = serde_json::to_string(&body_str).unwrap_or_else(|_| "\"{}\"".into());
        crate::rust_extender::net::simple_server::queue(format!(
            r#"{{"header":"{header}","body":{body_esc}}}"#
        ));
    } else if crate::slight::smash_utils::debug_logging_enabled() {
        // With no editor connected this used to write an SD file per message, unconditionally.
        // `notify_effect` runs once per effect SPAWN (the spawn hook queues a notify and drains
        // the queue inline, up to 64 jobs a tick), so a busy match created dozens of brand-new
        // files per frame — on the game thread, for a directory nothing ever reads and nothing
        // ever cleans up. File creation is the most expensive filesystem operation Windows has:
        // an MFT record, a directory-index insert, and a fresh file for Defender to scan. It
        // also got progressively slower as the directory filled.
        //
        // Dropping instead is safe: everything is re-sent when a client arrives. Kind tabs go
        // through `resync_if_requested`, the carrier report through `reset_carrier_status_latch`,
        // and the ACMD log through `hitbox_viewer::requeue_all` — all three fire on connect.
        // The fallback survives behind the debug switch for when someone actually wants it.
        sd_fallback(header, body);
    }
}

fn sd_fallback(header: &str, body: &serde_json::Value) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let rec = OutRecord {
        seq,
        header: header.into(),
        body: serde_json::to_string(body).unwrap_or_else(|_| "{}".into()),
    };
    let path = format!("{}rpm_out_{seq:08}.json", smash_utils::DEBUG_LOGGERS);
    if let Ok(json) = serde_json::to_string(&rec) {
        let _ = std::fs::write(path, json);
    }
}

pub fn notify_effect(id: u64, name: &str, data: &EffectData) {
    // The hottest emitter in the plugin: once per effect spawn, dozens of times a frame in a
    // busy match. Everything below allocates and serializes twice, so bail before paying for a
    // payload that `emit` would only drop. Kind tabs are re-sent in full on connect
    // (`resync_if_requested`), so nothing is lost by skipping this while disconnected.
    if !emit_wanted() {
        return;
    }
    let rpm = RpmEffectData::from_effect_data(data);
    let value_in_json = serde_json::to_string(&rpm).unwrap_or_else(|_| "{}".into());
    // The pins-only sparse form rides along so a fresh editor can tell user overrides
    // apart from pristine observed values (value_in_json is the MERGED form).
    let pinned_in_json = crate::slight::effect_viewer::kinds::pinned_of(id)
        .and_then(|p| serde_json::to_string(&p).ok());
    emit(
        "Notify",
        &serde_json::json!({
            "Notify": {
                "id": id,
                "name": name,
                "value_in_json": value_in_json,
                "pinned_in_json": pinned_in_json,
            }
        }),
    );
}

/// Stream one captured ACMD line to the editor (live-ACMD source).
pub fn notify_acmd_capture(line: &crate::slight::hitbox_viewer::CaptureLine) {
    // Capture history remains in the plugin for a later connection, but there is no reason to
    // allocate a JSON value for every ordinary move when neither a client nor debug fallback can
    // receive it. Busy no-editor matches are the common case.
    if !emit_wanted() {
        return;
    }
    emit("AcmdCapture", &serde_json::json!({ "AcmdCapture": line }));
}

/// Tell the editor a captured motion has finished playing — every line it produces has now
/// been streamed. Emitted strictly after those lines (see `take_pending_ends`).
pub fn notify_acmd_capture_end(end: &crate::slight::hitbox_viewer::CaptureEnd) {
    if !emit_wanted() {
        return;
    }
    emit(
        "AcmdCaptureEnd",
        &serde_json::json!({ "AcmdCaptureEnd": end }),
    );
}

/// Replay one validated record from the disk-backed capture archive. Keeping the archive format
/// private to the plugin avoids deserializing a lifetime-bound `func` field just to send the same
/// JSON back over the wire.
pub fn notify_acmd_capture_archive_record(record: &str) {
    if !emit_wanted() {
        return;
    }
    let Ok(record) = serde_json::from_str::<serde_json::Value>(record) else {
        return;
    };
    let Some(data) = record.get("data") else {
        return;
    };
    match record.get("type").and_then(serde_json::Value::as_str) {
        Some("line") => emit("AcmdCapture", &serde_json::json!({ "AcmdCapture": data })),
        Some("end") => emit(
            "AcmdCaptureEnd",
            &serde_json::json!({ "AcmdCaptureEnd": data }),
        ),
        _ => {}
    }
}

pub fn remove_effect(id: u64) {
    emit("Remove", &serde_json::json!({ "Remove": { "id": id } }));
}

pub fn remove_all() {
    emit("RemoveAll", &serde_json::json!({}));
}

/// Jorge FUN_71000936c8 — multiplier registry changed.
pub fn notify_multipliers(direct_rules: usize, pattern_rules: usize) {
    emit(
        "Multipliers",
        &serde_json::json!({
            "Multipliers": {
                "direct": direct_rules,
                "pattern": pattern_rules,
                "key": "Effect data"
            }
        }),
    );
}

/// Jorge FUN_71001078f8 — serialized agent row on init when after-win.
pub fn notify_agent_info(info: &crate::slight::systems::agent_info::AgentInfo) {
    emit("AgentInfo", &serde_json::json!({ "AgentInfo": info }));
}

pub fn notify_acmd_capture_cleared() {
    emit(
        "AcmdCaptureCleared",
        &serde_json::json!({ "AcmdCaptureCleared": true }),
    );
}

#[derive(Deserialize)]
struct PathUpdate {
    path: String,
    value: serde_json::Value,
}

#[derive(Deserialize)]
struct IncomingEdit {
    object_id: u64,
    client_id: u64,
    #[allow(dead_code)]
    transaction_id: u64,
    updates: Vec<PathUpdate>,
}

/// RPM edits sent over TCP (plain JSON or framed envelope body).
pub fn poll_tcp_edits() -> Vec<ParsedEdit> {
    let mut out = Vec::new();
    for raw in crate::rust_extender::net::simple_server::take_inbound() {
        if let Some(edit) = parse_tcp_payload(&raw) {
            out.push(edit);
        }
    }
    out
}

/// Apply the two timing-rule lists as soon as the socket thread receives them. These setters
/// only replace lock-protected data and reset injection latches; they do not touch any game
/// module or Lua state, so they can safely apply before the first ACMD boundary. A contended
/// setter stages the complete list for the next boundary instead of dropping it; false is
/// reserved for malformed or unrelated payloads that the normal parser should handle.
pub fn apply_timing_rules_from_network(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    if let Some(rules) = value.get("spawn_rules") {
        let Ok(rules) = serde_json::from_value::<
            Vec<crate::slight::effect_viewer::spawn_rules::SpawnRule>,
        >(rules.clone()) else {
            return false;
        };
        return crate::slight::effect_viewer::spawn_rules::set_rules(rules);
    }
    if let Some(rules) = value.get("effect_control_rules") {
        let Ok(rules) = serde_json::from_value::<
            Vec<crate::slight::effect_viewer::control_rules::ControlRule>,
        >(rules.clone()) else {
            return false;
        };
        return crate::slight::effect_viewer::control_rules::set_rules(rules);
    }
    false
}

fn parse_tcp_payload(raw: &str) -> Option<ParsedEdit> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;

    // Editor commands (no effect-edit payload).
    if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
        match cmd {
            "reset_pins" => {
                crate::slight::effect_viewer::kinds::reset_all_pins();
                RESYNC.store(true, std::sync::atomic::Ordering::Release);
            }
            // Stage capture cleanup for the game thread. Taking the capture locks from this
            // server thread can park against the game thread and freeze Skyline.
            "clear_acmd_captures" => {
                crate::slight::hitbox_viewer::request_clear_captures();
            }
            // Editor deployed/updated merged eff files on the SD — refresh the served map
            // (content is re-read per load; only NEW paths need registration here).
            "live_eff_reload" => crate::slight::effect_viewer::live_eff::reload(),
            // Diagnostic: verify what Arcropolis serves + what the game holds resident.
            "live_eff_probe" => {
                crate::slight::effect_viewer::live_eff::probe();
            }
            // Live memory inspection — result hexdump lands in sd:/effect_viewer_peek.txt
            // (the Eden SD is host-visible, so the PC reads it directly). `addr` is absolute
            // hex, or text-relative when "rel":"text". Bounded to 4 KiB per request.
            "peek" => {
                let addr = v
                    .get("addr")
                    .and_then(|a| a.as_str())
                    .and_then(|s| usize::from_str_radix(s.trim_start_matches("0x"), 16).ok());
                let len = v
                    .get("len")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(64)
                    .min(4096) as usize;
                if let Some(mut addr) = addr {
                    if v.get("rel").and_then(|r| r.as_str()) == Some("text") {
                        addr = addr.wrapping_add(unsafe {
                            skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize
                        });
                    }
                    let bytes =
                        unsafe { std::slice::from_raw_parts(addr as *const u8, len) }.to_vec();
                    let mut out = format!("addr={addr:#x} len={len}\n");
                    for (i, chunk) in bytes.chunks(16).enumerate() {
                        out.push_str(&format!("{:#x}: ", addr + i * 16));
                        for b in chunk {
                            out.push_str(&format!("{b:02x} "));
                        }
                        out.push('\n');
                    }
                    let _ = std::fs::write("sd:/effect_viewer_peek.txt", out);
                }
            }
            other => crate::slight::diag::note(format!("unknown command: {other}")),
        }
        return None;
    }

    // Hitbox rules: full-list replace (modify/suppress/inject ATTACKs).
    if let Some(rules_v) = v.get("hitbox_rules") {
        match serde_json::from_value::<Vec<crate::slight::hitbox_viewer::HitboxRule>>(
            rules_v.clone(),
        ) {
            Ok(rules) => crate::slight::hitbox_viewer::set_rules(rules),
            Err(e) => crate::slight::diag::note(format!("hitbox_rules parse error: {e}")),
        }
        return None;
    }

    // Cross-fighter donor eff co-loading (smashline transplant mechanism).
    if let Some(specs_v) = v.get("donor_effs") {
        match serde_json::from_value::<Vec<crate::slight::effect_viewer::effect_reload::DonorSpec>>(
            specs_v.clone(),
        ) {
            Ok(specs) => crate::slight::effect_viewer::effect_reload::set_donor_specs(specs),
            Err(e) => crate::slight::diag::note(format!("donor_effs parse error: {e}")),
        }
        return None;
    }

    // Stripped donor eff bytes to inject as resident data (live cross-character transplant).
    if let Some(bytes_v) = v.get("donor_bytes") {
        #[derive(serde::Deserialize)]
        struct DonorBytes {
            path: String,
            /// base64 payload — the transport of last resort.
            #[serde(default)]
            b64: String,
            /// sd:-relative file the editor already wrote the payload to. Preferred: this runs
            /// under an emulator whose sdmc is a directory on the same machine as the editor,
            /// so the editor can drop the bytes straight there. base64 inside a JSON frame cost
            /// ~1.33x in size on a socket read in 8 KB chunks, for payloads of several MB.
            #[serde(default)]
            file: String,
        }
        match serde_json::from_value::<Vec<DonorBytes>>(bytes_v.clone()) {
            Ok(list) => {
                let mut decoded: Vec<(String, Vec<u8>)> = Vec::with_capacity(list.len());
                let mut failed: Option<String> = None;
                for d in list {
                    let bytes = if !d.file.is_empty() {
                        let at = format!("sd:/{}", d.file);
                        match std::fs::read(&at) {
                            Ok(b) => Some(b),
                            Err(e) => {
                                failed = Some(format!("cannot read {at}: {e}"));
                                None
                            }
                        }
                    } else {
                        let b = crate::slight::effect_viewer::effect_reload::b64_decode(&d.b64);
                        if b.is_none() {
                            failed = Some(format!("undecodable base64 payload for {}", d.path));
                        }
                        b
                    };
                    match bytes {
                        Some(bytes) => decoded.push((d.path, bytes)),
                        None => break,
                    }
                }
                if let Some(why) = failed {
                    // All-or-nothing, and this is load-bearing. An EMPTY donor list is a
                    // command — it is how the editor tears the live carrier down (see the
                    // editor's `clear_all_game_edits`). Dropping unreadable entries and
                    // carrying on would turn a failed transfer into that command: it would
                    // wipe DONOR_BYTES while the aliases the editor sends microseconds later
                    // still point at the carrier, and the next spawn of an aliased effect
                    // would resolve to a resource with no bytes behind it. This plugin is
                    // built `panic = "abort"`, so that took the whole game down.
                    //
                    // Leave the previously staged buffers untouched and say so instead.
                    crate::slight::diag::note(format!(
                        "donor_bytes REJECTED, previous staging kept: {why}"
                    ));
                    notify_carrier_error(&why);
                } else {
                    crate::slight::effect_viewer::effect_reload::set_donor_bytes(decoded);
                }
            }
            Err(e) => crate::slight::diag::note(format!("donor_bytes parse error: {e}")),
        }
        return None;
    }

    // Custom effect names (transplant copies) for hash→name display resolution.
    if let Some(names_v) = v.get("effect_names") {
        if let Ok(names) = serde_json::from_value::<Vec<String>>(names_v.clone()) {
            crate::slight::effect_viewer::effect_names::register(&names);
        }
        return None;
    }

    // Live transplant aliases: full-list replace (copy/replaced kind → donor kind).
    if let Some(aliases_v) = v.get("effect_aliases") {
        match serde_json::from_value::<Vec<crate::slight::effect_viewer::spawn_rules::EffectAlias>>(
            aliases_v.clone(),
        ) {
            Ok(aliases) => crate::slight::effect_viewer::spawn_rules::set_aliases(aliases),
            Err(e) => crate::slight::diag::note(format!("effect_aliases parse error: {e}")),
        }
        return None;
    }

    // Eff-editor spawn rules: full-list replace, handled here (not an effect edit).
    if let Some(rules_v) = v.get("spawn_rules") {
        match serde_json::from_value::<Vec<crate::slight::effect_viewer::spawn_rules::SpawnRule>>(
            rules_v.clone(),
        ) {
            Ok(rules) => {
                let _ = crate::slight::effect_viewer::spawn_rules::set_rules(rules);
            }
            Err(e) => crate::slight::diag::note(format!("spawn_rules parse error: {e}")),
        }
        return None;
    }

    // Effect point controls: full-list replace (detach and area toggles).
    if let Some(rules_v) = v.get("effect_control_rules") {
        match serde_json::from_value::<Vec<crate::slight::effect_viewer::control_rules::ControlRule>>(
            rules_v.clone(),
        ) {
            Ok(rules) => {
                let _ = crate::slight::effect_viewer::control_rules::set_rules(rules);
            }
            Err(e) => crate::slight::diag::note(format!("effect_control_rules parse error: {e}")),
        }
        return None;
    }

    if let (Some(id), Some(nv)) = (v.get("id"), v.get("newValue")) {
        let val = if let Some(s) = nv.as_str() {
            serde_json::from_str(s).unwrap_or(nv.clone())
        } else {
            nv.clone()
        };
        return Some(parse_new_value(id.as_u64().unwrap_or(0), &val));
    }

    let header = v.get("header")?.as_str()?;
    let body = v.get("body")?;
    let body_obj: serde_json::Value = if let Some(s) = body.as_str() {
        serde_json::from_str(s).ok()?
    } else {
        body.clone()
    };

    match header {
        "Update" | "Apply" => {
            if let (Some(id), Some(nv)) = (body_obj.get("id"), body_obj.get("newValue")) {
                let val = if let Some(s) = nv.as_str() {
                    serde_json::from_str(s).unwrap_or(nv.clone())
                } else {
                    nv.clone()
                };
                return Some(parse_new_value(id.as_u64().unwrap_or(0), &val));
            }
        }
        _ => {}
    }
    None
}

fn parse_transaction_name(name: &str) -> Option<(u64, u64, u64)> {
    if name.ends_with(".done") {
        return None;
    }
    let rest = name.strip_prefix("object-")?;
    let (object_s, rest) = rest.split_once("-client-")?;
    let (client_s, tid_s) = rest.split_once("-transaction-")?;
    let object_id = object_s.parse().ok()?;
    let client_id = client_s.parse().ok()?;
    let transaction_id = tid_s.parse().ok()?;
    Some((object_id, client_id, transaction_id))
}

/// How often [`poll_transactions`] may run, in game frames. The caller gates on this.
///
/// Declared here, next to the budget it divides, because the two are one decision: the retry
/// window is `MAX_TRANSACTION_ATTEMPTS * TRANSACTION_POLL_EVERY` frames, and moving the poll off
/// the frame path without moving the budget would have quietly stretched a 10-second window to
/// 100 seconds.
pub const TRANSACTION_POLL_EVERY: u64 = 10;

/// Attempts before a transaction that keeps failing to apply is parked as `.failed`.
///
/// One attempt per *poll*, not per frame — see [`TRANSACTION_POLL_EVERY`]. 60 polls at one every
/// 10 frames is ~10s at 60fps, which is what this was before the poll was throttled and is still
/// what it is for: long enough for a briefly-missing effect to come back.
const MAX_TRANSACTION_ATTEMPTS: u32 = 60;

static TRANSACTION_ATTEMPTS: parking_lot::Mutex<Option<std::collections::HashMap<String, u32>>> =
    parking_lot::Mutex::new(None);

/// Mark a polled transaction consumed (`.done`) on successful apply; failed applies stay on
/// disk to be retried next frame, bounded by MAX_TRANSACTION_ATTEMPTS (then parked `.failed`).
/// Previously the file was renamed `.done` at parse time, silently dropping edits whose
/// target effect was momentarily missing.
pub fn finish_transaction(path: &std::path::Path, applied: bool) {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let mut guard = TRANSACTION_ATTEMPTS.lock();
    let attempts = guard.get_or_insert_with(Default::default);
    if applied {
        attempts.remove(&name);
        let _ = std::fs::rename(path, path.with_extension("done"));
        return;
    }
    let n = attempts.entry(name.clone()).or_insert(0);
    *n += 1;
    if *n >= MAX_TRANSACTION_ATTEMPTS {
        attempts.remove(&name);
        let _ = std::fs::rename(path, path.with_extension("failed"));
        skyline::println!("[SLight] Transaction {name} failed to apply — parked as .failed");
    }
}

pub fn poll_transactions() -> Vec<(ParsedEdit, std::path::PathBuf)> {
    smash_utils::ensure_slight_dirs();
    let Ok(entries) = std::fs::read_dir(smash_utils::DEBUGGABLES_DIR) else {
        return Vec::new();
    };

    let our_client = crate::rust_extender::net::simple_server::client_id();
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name.ends_with(".done") || name.ends_with(".failed") {
            continue;
        }
        let Some((object_id, file_client, _tid)) = parse_transaction_name(name) else {
            continue;
        };
        if our_client != 0 && file_client != our_client {
            continue;
        }

        // Log only the first sighting — failed applies re-poll this file for up to
        // MAX_TRANSACTION_ATTEMPTS frames.
        let first_sighting = TRANSACTION_ATTEMPTS
            .lock()
            .as_ref()
            .map_or(true, |m| !m.contains_key(name));
        if first_sighting {
            skyline::println!("[SLight] Found a transaction on {name}");
        }

        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        let parsed = if let Ok(edit) = serde_json::from_str::<IncomingEdit>(&text) {
            if our_client != 0 && edit.client_id != our_client {
                continue;
            }
            parse_updates(edit.object_id, &edit.updates)
        } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let (Some(id), Some(nv)) = (v.get("id"), v.get("newValue")) {
                let val = if let Some(s) = nv.as_str() {
                    serde_json::from_str(s).unwrap_or(nv.clone())
                } else {
                    nv.clone()
                };
                parse_new_value(id.as_u64().unwrap_or(object_id), &val)
            } else {
                continue;
            }
        } else {
            continue;
        };

        // Consumption is decided by the caller: finish_transaction(path, applied) renames to
        // .done on success, retries (bounded) on failure.
        out.push((parsed, path));
    }
    out
}

fn json_f32(v: &serde_json::Value) -> Option<f32> {
    v.as_f64().map(|n| n as f32)
}

fn parse_updates(id: u64, updates: &[PathUpdate]) -> ParsedEdit {
    let mut edit = ParsedEdit {
        id,
        ..Default::default()
    };
    let mut pos = Point3D::default();
    let mut rot = Point3D::default();
    let mut color = Color::default();
    let mut pos_set = false;
    let mut rot_set = false;
    let mut color_set = false;

    for u in updates {
        match u.path.as_str() {
            "scale" => edit.scale = json_f32(&u.value),
            "speed" | "rate" => edit.rate = json_f32(&u.value),
            "visible" => edit.visible = u.value.as_bool(),
            "frame" => edit.frame = json_f32(&u.value),
            "is_follow" => edit.is_follow = u.value.as_bool(),
            "pos.x" => {
                pos.x = json_f32(&u.value).unwrap_or(0.0);
                pos_set = true;
            }
            "pos.y" => {
                pos.y = json_f32(&u.value).unwrap_or(0.0);
                pos_set = true;
            }
            "pos.z" => {
                pos.z = json_f32(&u.value).unwrap_or(0.0);
                pos_set = true;
            }
            "rot.x" => {
                rot.x = json_f32(&u.value).unwrap_or(0.0);
                rot_set = true;
            }
            "rot.y" => {
                rot.y = json_f32(&u.value).unwrap_or(0.0);
                rot_set = true;
            }
            "rot.z" => {
                rot.z = json_f32(&u.value).unwrap_or(0.0);
                rot_set = true;
            }
            "rainbow.color" | "color" => {
                if let Ok(c) = serde_json::from_value::<Color>(u.value.clone()) {
                    color = c;
                    color_set = true;
                }
            }
            "rainbow.color.red" | "color.red" => {
                color.red = json_f32(&u.value).unwrap_or(color.red);
                color_set = true;
            }
            "rainbow.color.green" | "color.green" => {
                color.green = json_f32(&u.value).unwrap_or(color.green);
                color_set = true;
            }
            "rainbow.color.blue" | "color.blue" => {
                color.blue = json_f32(&u.value).unwrap_or(color.blue);
                color_set = true;
            }
            "rainbow.color.alpha" | "color.alpha" => {
                color.alpha = json_f32(&u.value).unwrap_or(color.alpha);
                color_set = true;
            }
            "rainbow.movement_state" => edit.movement_state = json_f32(&u.value),
            _ => {}
        }
    }

    if pos_set {
        edit.pos = Some(pos);
    }
    if rot_set {
        edit.rot = Some(rot);
    }
    if color_set {
        edit.color = Some(color);
    }
    edit
}

fn parse_new_value(id: u64, val: &serde_json::Value) -> ParsedEdit {
    let mut edit = ParsedEdit {
        id,
        ..Default::default()
    };
    if let Some(v) = val.get("scale").and_then(json_f32) {
        edit.scale = Some(v);
    }
    if let Some(v) = val.get("speed").and_then(json_f32) {
        edit.rate = Some(v);
    }
    if let Some(v) = val.get("rate").and_then(json_f32) {
        edit.rate = Some(v);
    }
    if let Some(v) = val.get("pos") {
        edit.pos = serde_json::from_value(v.clone()).ok();
    }
    if let Some(v) = val.get("rot") {
        edit.rot = serde_json::from_value(v.clone()).ok();
    }
    if let Some(v) = val.get("visible").and_then(|v| v.as_bool()) {
        edit.visible = Some(v);
    }
    if let Some(v) = val.get("is_follow").and_then(|v| v.as_bool()) {
        edit.is_follow = Some(v);
    }
    if let Some(v) = val.get("frame").and_then(json_f32) {
        edit.frame = Some(v);
    }
    if let Some(r) = val.get("rainbow") {
        if let Some(c) = r
            .get("color")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
        {
            edit.color = Some(c);
        }
        if let Some(v) = r.get("movement_state").and_then(json_f32) {
            edit.movement_state = Some(v);
        }
    }
    edit
}

/// Tell the editor how the live carrier is doing.
///
/// The editor pushes a carrier and then has no idea when the game has actually taken it —
/// the bytes decompress, the resource service settles and the carrier object is created
/// asynchronously, which took visibly long enough that edits looked like they had failed.
/// Emitting the state lets the editor show "waiting for game…" instead of guessing.
/// `gen` is the donor-bytes generation the CURRENTLY LIVE carrier was built from. The editor
/// needs it to tell "the carrier from my previous send is still up" from "my new bytes are
/// live" — without it, a second send saw state=2/object=up immediately and reported success
/// before the game had taken anything.
/// Tell the editor a carrier push was rejected, and why.
///
/// Without this the editor sits on "waiting for game…" forever when a payload never arrives,
/// because the carrier status it polls simply never advances. The reason is the useful part:
/// a `cannot read sd:/effect_viewer/payload/...` here means the editor wrote the payload
/// somewhere that is not the emulator's SD root.
pub fn notify_carrier_error(reason: &str) {
    emit(
        "CarrierError",
        &serde_json::json!({ "CarrierError": { "reason": reason } }),
    );
}

pub fn notify_carrier_status(state: u8, kinds: usize, spawned: bool, generation: u64) {
    emit(
        "CarrierStatus",
        &serde_json::json!({
            "CarrierStatus": {
                "state": state, "kinds": kinds, "spawned": spawned, "gen": generation
            }
        }),
    );
}
