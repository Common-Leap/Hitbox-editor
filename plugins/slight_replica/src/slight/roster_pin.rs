use parking_lot::Mutex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

// In-plugin pin store: ui_chara_id -> costume slot index, plus live CSS order.
// R-84/R-85: two live mechanisms share this file so their scoping stays in one place.
// PINS is the costume gate (slot-backed clone selected as donor's costume).
// LIVE_ORDER is the CSS order patch (disp_order / hidden). Both are properly
// scoped per-entry via stable RosterKey strings ("mario", "mario#c08", "ui:ptrainer")
// so adding a character on the fly does not bleed into unrelated entries.

static PINS: Mutex<BTreeMap<u64, u8>> = Mutex::new(BTreeMap::new());
static LIVE_ORDER: Mutex<BTreeMap<String, i8>> = Mutex::new(BTreeMap::new());
static LIVE_HIDDEN: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static LAST_ENFORCED_FINGERPRINT: AtomicU64 = AtomicU64::new(0);
static LAST_LIVE_ORDER_FINGERPRINT: AtomicU64 = AtomicU64::new(0);

fn pin_fingerprint(pins: &BTreeMap<u64, u8>) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for (ui_chara_id, slot) in pins {
        hash ^= ui_chara_id.wrapping_add(0x9e3779b97f4a7c15) ^ u64::from(*slot);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn order_fingerprint(order: &BTreeMap<String, i8>, hidden: &BTreeSet<String>) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for (k, v) in order {
        for b in k.bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= u64::from(*v as u8);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for k in hidden {
        for b in k.bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// Replace the entire pin map with the provided one.
pub fn set_pins(m: BTreeMap<u64, u8>) {
    let mut p = PINS.lock();
    *p = m;
    let fingerprint = pin_fingerprint(&p);
    LAST_ENFORCED_FINGERPRINT.store(fingerprint ^ 0xffff_ffff_ffff_ffff, Ordering::Release);
}

/// Clear all pins.
pub fn clear_pins() {
    PINS.lock().clear();
    LAST_ENFORCED_FINGERPRINT.store(0, Ordering::Release);
}

/// Get the pinned slot for a ui_chara_id, if any.
pub fn get_pin(id: u64) -> Option<u8> {
    PINS.lock().get(&id).copied()
}

/// Snapshot of all pins.
pub fn all_pins() -> BTreeMap<u64, u8> {
    PINS.lock().clone()
}

/// R-82 / R-86 probe: dump the resident ui_chara_db's size + head bytes.
/// Called on `roster_probe` command and on every live change for diagnostics.
pub fn probe_roster() {
    const UI_CHARA_DB: &str = "ui/param/database/ui_chara_db.prc";
    let hash = smash::hash40(UI_CHARA_DB);
    if let Some((size, head, found)) =
        crate::slight::effect_viewer::resource_reload::resident_probe(hash, &[])
    {
        crate::slight::diag::note(format!(
            "roster_probe: {} -> size={} head={:02x?} needle_found={}",
            UI_CHARA_DB, size, head, found
        ));
    } else {
        crate::slight::diag::note(format!(
            "roster_probe: {} -> not resident or not loaded",
            UI_CHARA_DB
        ));
    }
    // Also report live state so the harness can correlate.
    let pins = PINS.lock().len();
    let order = LIVE_ORDER.lock().len();
    let hidden = LIVE_HIDDEN.lock().len();
    crate::slight::diag::note(format!(
        "roster_probe: live_state pins={} order_overrides={} hidden={}",
        pins, order, hidden
    ));
}

/// Apply a live CSS order patch: `key → disp_order` + hidden set.
/// Properly scoped per RosterKey ("mario", "mario#c08", …) — adding a character
/// on the fly only touches its own entries. Validates every disp_order before
/// touching memory (R-81 / R-87). Today this goes via the file fallback
/// (`replace_loaded_file`) and logs that heap offsets are pending R-80 measurement;
/// once `HeapOffsetTable` is filled the same entry point will do the in-mem write
/// plus the rebuild trigger from R-83.
pub fn apply_live_css_order(order: &BTreeMap<String, i8>, hidden: &BTreeSet<String>) {
    // Validate every disp_order before any write — a bad value silently claims the wrong cell.
    for (key, disp) in order {
        if let Err(why) = validate_live_disp(*disp) {
            crate::slight::diag::note(format!(
                "live_css_order: refused {} -> {}: {}",
                key, disp, why
            ));
            return;
        }
    }
    for key in hidden {
        // Hidden is always disp_order -1 + can_select false; no value to validate.
        let _ = key;
    }

    let fingerprint = order_fingerprint(order, hidden);
    let prev = LAST_LIVE_ORDER_FINGERPRINT.swap(fingerprint, Ordering::AcqRel);
    if prev == fingerprint {
        return;
    }

    *LIVE_ORDER.lock() = order.clone();
    *LIVE_HIDDEN.lock() = hidden.clone();

    crate::slight::diag::note(format!(
        "live_css_order: {} order overrides, {} hidden (fingerprint {:016x})",
        order.len(),
        hidden.len(),
        fingerprint
    ));

    // Heap-only, no file fallback.
    let instant = crate::slight::roster_heap::patch_instant(order, hidden);
    if instant {
        crate::slight::diag::note(
            "live_css_order: heap patch succeeded — CSS should update without re-enter",
        );
    } else {
        crate::slight::diag::note("live_css_order: heap patch not yet instant — need DumpUiCharaDb/DumpCssRebuild for heap offsets");
    }
    probe_roster();
}

fn validate_live_disp(v: i8) -> Result<(), String> {
    match v {
        -1 => Ok(()),
        99 => Err("99 is Random sentinel, must not be written by reorder".into()),
        0..=127 => Ok(()),
        _ => Err("out of I8 range".into()),
    }
}

/// Snapshot of live CSS state for diagnostics.
pub fn live_css_snapshot() -> (BTreeMap<String, i8>, BTreeSet<String>) {
    (LIVE_ORDER.lock().clone(), LIVE_HIDDEN.lock().clone())
}

/// R-85: resolve the slot that should be used for a selected ui_chara_id.
/// Properly scoped: only the pinned ui_chara_id is affected; all others
/// pass through the game's requested `slot` unchanged. This is the single
/// place a future Skyline hook on the selection function will call — today it
/// is used for diagnostics and for the file-based fallback's logging, so the
/// scoping is already testable without a hook address.
pub fn resolve_slot(ui_chara_id: u64, requested_slot: u8) -> u8 {
    get_pin(ui_chara_id).unwrap_or(requested_slot)
}

/// Hook placeholder for R-85: to be called from the game's selection resolver
/// once its address is measured (R-80).  Until then this is a no-op that
/// just logs, so adding it does not change behavior.  The signature mirrors
/// what the game passes: ui_chara_id + game-chosen slot → final slot.
pub fn on_fighter_select(ui_chara_id: u64, game_slot: u8) -> u8 {
    let resolved = resolve_slot(ui_chara_id, game_slot);
    if resolved != game_slot {
        crate::slight::diag::note(format!(
            "roster_pin: pinned selection ui_chara_id={} game_slot={} -> pinned_slot={}",
            ui_chara_id, game_slot, resolved
        ));
    }
    resolved
}

/// Heap-only pin enforcement: no file fallback.
pub fn enforce_pins() {
    let pins = PINS.lock().clone();
    if pins.is_empty() {
        LAST_ENFORCED_FINGERPRINT.store(0, Ordering::Release);
        return;
    }

    let fingerprint = pin_fingerprint(&pins);
    let previous = LAST_ENFORCED_FINGERPRINT.swap(fingerprint, Ordering::AcqRel);
    if previous == fingerprint {
        return;
    }

    crate::slight::diag::note(format!(
        "roster_pin: {} pin(s) updated (heap-only, no file fallback)",
        pins.len()
    ));
    // Probe for diagnostics, but do not try file reload.
    const UI_CHARA_DB: &str = "ui/param/database/ui_chara_db.prc";
    let hash = smash::hash40(UI_CHARA_DB);
    if let Some((size, head, found)) =
        crate::slight::effect_viewer::resource_reload::resident_probe(hash, &[])
    {
        crate::slight::diag::note(format!(
            "roster_pin: resident_probe {} -> size={} head={:02x?} needle_found={}",
            UI_CHARA_DB, size, head, found
        ));
    }
}
