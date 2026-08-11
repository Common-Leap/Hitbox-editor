//! Runtime param-label download from ultimate-research/param-labels.
//!
//! Replaces the old requirement of a `ParamLabels.csv` / `Labels.txt` inside the game
//! data (export) folder: at startup a background thread loads the cached copy from app
//! storage instantly, then checks GitHub for updates via ETag and re-downloads only when
//! the repo actually changed. Results arrive over an mpsc channel and are merged into
//! `state.labels` (hash40 → label).

use std::collections::HashMap;
use std::path::PathBuf;

const CSV_URL: &str =
    "https://raw.githubusercontent.com/ultimate-research/param-labels/master/ParamLabels.csv";

pub enum Msg {
    /// A full label map is ready (from cache or a fresh download).
    Loaded { labels: HashMap<u64, String> },
    /// Progress/result note for the status bar.
    Status(String),
}

fn cache_csv() -> PathBuf {
    crate::scratch_dirs::app_storage_root().join("ParamLabels.csv")
}

fn cache_etag() -> PathBuf {
    crate::scratch_dirs::app_storage_root().join("ParamLabels.etag")
}

/// Parse `0xHASH,label` lines (the repo format; hash may also be bare hex).
pub fn parse_csv(content: &str) -> HashMap<u64, String> {
    let mut labels = HashMap::new();
    for line in content.lines() {
        let mut parts = line.splitn(2, ',');
        if let (Some(hex), Some(label)) = (parts.next(), parts.next()) {
            let hex = hex.trim();
            let hex = hex.strip_prefix("0x").unwrap_or(hex);
            if let Ok(val) = u64::from_str_radix(hex, 16) {
                let label = label.trim();
                if !label.is_empty() {
                    labels.insert(val, label.to_string());
                }
            }
        }
    }
    labels
}

/// Start the background load + update check; poll the receiver each UI frame.
pub fn spawn_fetch() -> std::sync::mpsc::Receiver<Msg> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // 1. Serve the cached copy immediately (startup never waits on the network).
        let cached = std::fs::read_to_string(cache_csv()).ok();
        if let Some(content) = &cached {
            let labels = parse_csv(content);
            if !labels.is_empty() {
                let n = labels.len();
                let _ = tx.send(Msg::Loaded { labels });
                let _ = tx.send(Msg::Status(format!("Param labels: {n} cached")));
            }
        }

        // 2. Update check: conditional GET — GitHub answers 304 when nothing changed.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build();
        let Ok(client) = client else { return };
        let mut req = client.get(CSV_URL);
        if cached.is_some() {
            if let Ok(etag) = std::fs::read_to_string(cache_etag()) {
                let etag = etag.trim();
                if !etag.is_empty() {
                    req = req.header(reqwest::header::IF_NONE_MATCH, etag);
                }
            }
        }
        match req.send() {
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
                let _ = tx.send(Msg::Status("Param labels are up to date".into()));
            }
            Ok(resp) if resp.status().is_success() => {
                let etag = resp
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                match resp.text() {
                    Ok(content) => {
                        let labels = parse_csv(&content);
                        if labels.is_empty() {
                            let _ = tx.send(Msg::Status(
                                "Param labels download parsed to 0 labels — kept cache".into(),
                            ));
                            return;
                        }
                        let _ = std::fs::write(cache_csv(), &content);
                        match etag {
                            Some(e) => {
                                let _ = std::fs::write(cache_etag(), e);
                            }
                            None => {
                                let _ = std::fs::remove_file(cache_etag());
                            }
                        }
                        let n = labels.len();
                        let _ = tx.send(Msg::Loaded { labels });
                        let _ = tx.send(Msg::Status(format!(
                            "Param labels updated from GitHub ({n} labels)"
                        )));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Status(format!("Param labels read failed: {e}")));
                    }
                }
            }
            Ok(resp) => {
                let _ = tx.send(Msg::Status(format!(
                    "Param labels download failed: HTTP {}",
                    resp.status()
                )));
            }
            Err(e) => {
                // Offline is fine — the cache (if any) is already serving.
                let _ = tx.send(Msg::Status(format!("Param labels: offline ({e})")));
            }
        }
    });
    rx
}

// ── Attack-attribute lua-const tables ────────────────────────────────────────
//
// The ATTACK macro's attribute slots are plain lua ints in the running game, but the
// editor models them as the SYMBOLIC names the decompiled scripts use. Live capture
// therefore arrives as numbers and has to be decoded to a name before the dropdowns can
// show it as selected; the reverse encode is what the live-edit wire actually sends.
// Values are transcribed from `skyline-smash`'s `lua_const.rs` (the same table the game
// scripts compile against). Where several names share one value, the canonical/most
// commonly written one is kept.

/// `(symbolic name, lua value)` pairs for one attribute slot, ordered by value.
pub type ConstTable = &'static [(&'static str, i64)];

/// Work slots used by the measured `EFFECT_DETACH_KIND_WORK` corpus.
///
/// This is intentionally a small, evidence-bounded table rather than a general WorkModule
/// constant dump. The names and values are the five distinct tokens found in the retained public
/// standard/HDR script corpus, transcribed from the pinned `skyline-smash` `lua_const.rs` table.
/// Keeping the table narrow prevents an unverified game-version mapping from being presented as
/// portable. Unknown Work IDs remain authored source text and are not sent as live replacements.
pub const EFFECT_DETACH_KIND_WORK_SLOTS: ConstTable = &[
    (
        "FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_AFTERBURNER_LINE",
        0x100000D3,
    ),
    (
        "FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_ATTACK_ARC1",
        0x100000CC,
    ),
    (
        "FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_ATTACK_ARC2",
        0x100000CD,
    ),
    (
        "FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_ATTACK_LINE1",
        0x100000CA,
    ),
    (
        "FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_WITCHTWIST_WIND",
        0x100000D4,
    ),
];

/// `ATTACK_SETOFF_KIND_*` — clank/setoff behaviour.
pub const SETOFF_KIND: ConstTable = &[
    ("ATTACK_SETOFF_KIND_OFF", 0x0),
    ("ATTACK_SETOFF_KIND_ON", 0x1),
    ("ATTACK_SETOFF_KIND_THRU", 0x2),
    ("ATTACK_SETOFF_KIND_NO_STOP", 0x3),
];

/// `ATTACK_LR_CHECK_*` — how the knockback angle picks a facing.
pub const LR_CHECK: ConstTable = &[
    ("ATTACK_LR_CHECK_POS", 0x0),
    ("ATTACK_LR_CHECK_SPEED", 0x1),
    ("ATTACK_LR_CHECK_LR", 0x2),
    ("ATTACK_LR_CHECK_F", 0x3),
    ("ATTACK_LR_CHECK_B", 0x4),
    ("ATTACK_LR_CHECK_PART", 0x5),
    ("ATTACK_LR_CHECK_BACK_SLASH", 0x6),
    ("ATTACK_LR_CHECK_LEFT", 0x7),
    ("ATTACK_LR_CHECK_RIGHT", 0x8),
];

/// `COLLISION_SITUATION_MASK_*` — ground/air situations the hit connects in.
pub const SITUATION_MASK: ConstTable = &[
    ("COLLISION_SITUATION_MASK_G", 0x1),
    ("COLLISION_SITUATION_MASK_A", 0x2),
    ("COLLISION_SITUATION_MASK_GA", 0x3),
    ("COLLISION_SITUATION_MASK_ODD", 0x4),
    ("COLLISION_SITUATION_MASK_ALL", 0x7),
    ("COLLISION_SITUATION_MASK_IGNORE_DOWN", 0x80000000),
    ("COLLISION_SITUATION_MASK_G_d", 0x80000001),
    ("COLLISION_SITUATION_MASK_GA_d", 0x80000003),
];

/// `COLLISION_CATEGORY_MASK_*` — object categories the hit collides with.
pub const CATEGORY_MASK: ConstTable = &[
    ("COLLISION_CATEGORY_MASK_NONE", 0x0),
    ("COLLISION_CATEGORY_MASK_FIGHTER", 0x1),
    ("COLLISION_CATEGORY_MASK_ENEMY", 0x2),
    ("COLLISION_CATEGORY_MASK_FEB", 0x3),
    ("COLLISION_CATEGORY_MASK_ITEM", 0x4),
    ("COLLISION_CATEGORY_MASK_FI", 0x5),
    ("COLLISION_CATEGORY_MASK_FIEB", 0x7),
    ("COLLISION_CATEGORY_MASK_GIMMICK", 0x8),
    ("COLLISION_CATEGORY_MASK_FEBG", 0xb),
    ("COLLISION_CATEGORY_MASK_IG", 0xc),
    ("COLLISION_CATEGORY_MASK_FIEBG", 0xf),
    ("COLLISION_CATEGORY_MASK_ITEM_E", 0x10),
    ("COLLISION_CATEGORY_MASK_EM_ALL", 0x13),
    ("COLLISION_CATEGORY_MASK_II", 0x14),
    ("COLLISION_CATEGORY_MASK_IIE", 0x16),
    ("COLLISION_CATEGORY_MASK_NO_STAGE_GIMMICK", 0x17),
    ("COLLISION_CATEGORY_MASK_NO_FIGHTER", 0x1c),
    ("COLLISION_CATEGORY_MASK_NO_ENEMY", 0x1d),
    ("COLLISION_CATEGORY_MASK_NO_FLOOR", 0x1f),
    ("COLLISION_CATEGORY_MASK_FLOOR", 0x20),
    ("COLLISION_CATEGORY_MASK_NO_IFG", 0x22),
    ("COLLISION_CATEGORY_MASK_STAGE_GIMMICK", 0x28),
    ("COLLISION_CATEGORY_MASK_NO_ITEM_FIGHTER", 0x2a),
    ("COLLISION_CATEGORY_MASK_NO_ITEM", 0x2b),
    ("COLLISION_CATEGORY_MASK_NO_GIMMICK", 0x37),
    ("COLLISION_CATEGORY_MASK_NO_E", 0x3d),
    ("COLLISION_CATEGORY_MASK_ALL", 0x3f),
];

/// `COLLISION_PART_MASK_*` — body parts the hit collides with.
pub const PART_MASK: ConstTable = &[
    ("COLLISION_PART_MASK_ETC", 0x2),
    ("COLLISION_PART_MASK_BODY", 0x9),
    ("COLLISION_PART_MASK_LEGS", 0xc),
    ("COLLISION_PART_MASK_BODY_LEGS", 0xd),
    ("COLLISION_PART_MASK_HEAD", 0x10),
    ("COLLISION_PART_MASK_BODY_HEAD", 0x19),
    ("COLLISION_PART_MASK_BODY_LEGS_HEAD", 0x1d),
    ("COLLISION_PART_MASK_ALL", 0x1f),
];

/// `COLLISION_KIND_MASK_*` — which kinds of collision a `SEARCH` volume looks for.
///
/// Ascending by value, like every table here, so [`const_name`] resolves the composites
/// unambiguously. The multi-letter names are unions of the single ones — `AH` is attack|hit,
/// `AHS` adds shield — and they are listed because a script may write one, not because the
/// editor has any way to compose them.
pub const COLLISION_KIND_MASK: ConstTable = &[
    ("COLLISION_KIND_MASK_ATTACK", 0x1),
    ("COLLISION_KIND_MASK_HIT", 0x2),
    ("COLLISION_KIND_MASK_AH", 0x3),
    ("COLLISION_KIND_MASK_SHIELD", 0x4),
    ("COLLISION_KIND_MASK_HS", 0x6),
    ("COLLISION_KIND_MASK_AHS", 0x7),
    ("COLLISION_KIND_MASK_REFLECTOR", 0x8),
    ("COLLISION_KIND_MASK_HR", 0xa),
    ("COLLISION_KIND_MASK_HSR", 0xe),
    ("COLLISION_KIND_MASK_AHSR", 0xf),
    ("COLLISION_KIND_MASK_ABSORBER", 0x10),
    ("COLLISION_KIND_MASK_HSRAB", 0x1e),
    ("COLLISION_KIND_MASK_AHSRAB", 0x1f),
    ("COLLISION_KIND_MASK_SEARCH", 0x20),
    ("COLLISION_KIND_MASK_GRAB", 0x40),
    ("COLLISION_KIND_MASK_ALL", 0x7f),
];

/// `HIT_STATUS_MASK_*` — which hurtbox states a `SEARCH` volume counts as found.
///
/// The table [`HIT_STATUS`] warned about. These share a namespace with the four hurtbox states
/// and **overlap them numerically** — `HIT_STATUS_MASK_NORMAL` is `0x1`, the same value as
/// `HIT_STATUS_INVINCIBLE` — so keeping them apart is what stops a live capture of an
/// invincible bone coming back labelled as a mask. They are a different argument to a different
/// function; do not merge the two tables.
pub const HIT_STATUS_MASK: ConstTable = &[
    ("HIT_STATUS_MASK_NORMAL", 0x1),
    ("HIT_STATUS_MASK_INVINCIBLE", 0x2),
    ("HIT_STATUS_MASK_NI", 0x3),
    ("HIT_STATUS_MASK_XLU", 0x4),
    ("HIT_STATUS_MASK_NX", 0x5),
    ("HIT_STATUS_MASK_ALL", 0x7),
    ("HIT_STATUS_MASK_OFF", 0x8),
];

/// `HIT_STATUS_*` — how a bone or hurtbox group receives hits.
///
/// Exactly the four real states, and deliberately not every constant whose name starts with
/// `HIT_STATUS_`. `lua_const.rs` also defines a `HIT_STATUS_MASK_*` family in the same
/// namespace, and those overlap this one numerically — `HIT_STATUS_MASK_NORMAL` is `0x1`, the
/// same value as `HIT_STATUS_INVINCIBLE`. Since [`const_name`] resolves a value to the *first*
/// matching entry, including the masks would make the reverse direction ambiguous and a live
/// capture of an invincible bone could come back labelled as a mask. The masks are a different
/// argument to different functions, so they belong in their own table if they are ever needed.
///
/// `HIT_STATUS_TERM` (`0x4`) is the enum's count, not a state, and is left out for the same
/// reason: nothing can legitimately be set to it.
pub const HIT_STATUS: ConstTable = &[
    ("HIT_STATUS_NORMAL", 0x0),
    ("HIT_STATUS_INVINCIBLE", 0x1),
    ("HIT_STATUS_XLU", 0x2),
    ("HIT_STATUS_OFF", 0x3),
];

/// `DAMAGE_NO_REACTION_MODE_*` values used by `damage!(…, MA_MSC_DAMAGE_DAMAGE_NO_REACTION, …)`.
///
/// This small table is local and versioned with the editor, so armor live-preview does not rely
/// on the optional network label download. The value slot remains a float because the game uses
/// it for reaction-value and damage-power thresholds.
pub const DAMAGE_NO_REACTION_MODE: ConstTable = &[
    ("DAMAGE_NO_REACTION_MODE_NORMAL", 0x0),
    ("DAMAGE_NO_REACTION_MODE_ALWAYS", 0x1),
    ("DAMAGE_NO_REACTION_MODE_REACTION_VALUE", 0x2),
    ("DAMAGE_NO_REACTION_MODE_DAMAGE_POWER", 0x3),
    ("DAMAGE_NO_REACTION_MODE_DAMAGE_POWER_COUNT", 0x4),
    ("DAMAGE_NO_REACTION_MODE_NONE", -1),
];

/// `ATTACK_SOUND_LEVEL_*` — hit SFX loudness tier.
pub const SOUND_LEVEL: ConstTable = &[
    ("ATTACK_SOUND_LEVEL_S", 0x0),
    ("ATTACK_SOUND_LEVEL_M", 0x1),
    ("ATTACK_SOUND_LEVEL_L", 0x2),
    ("ATTACK_SOUND_LEVEL_LL", 0x3),
    ("ATTACK_SOUND_LEVEL_TERM", 0x4),
];

/// `COLLISION_SOUND_ATTR_*` — hit SFX sample set.
pub const SOUND_ATTR: ConstTable = &[
    ("COLLISION_SOUND_ATTR_NONE", 0x0),
    ("COLLISION_SOUND_ATTR_PUNCH", 0x1),
    ("COLLISION_SOUND_ATTR_KICK", 0x2),
    ("COLLISION_SOUND_ATTR_CUTUP", 0x3),
    ("COLLISION_SOUND_ATTR_COIN", 0x4),
    ("COLLISION_SOUND_ATTR_BAT", 0x5),
    ("COLLISION_SOUND_ATTR_HARISEN", 0x6),
    ("COLLISION_SOUND_ATTR_ELEC", 0x7),
    ("COLLISION_SOUND_ATTR_FIRE", 0x8),
    ("COLLISION_SOUND_ATTR_WATER", 0x9),
    ("COLLISION_SOUND_ATTR_GRASS", 0xa),
    ("COLLISION_SOUND_ATTR_BOMB", 0xb),
    ("COLLISION_SOUND_ATTR_PEACHWEAPON", 0xc),
    ("COLLISION_SOUND_ATTR_SNAKE", 0xd),
    ("COLLISION_SOUND_ATTR_IKE", 0xe),
    ("COLLISION_SOUND_ATTR_DEDEDE", 0xf),
    ("COLLISION_SOUND_ATTR_MAGIC", 0x10),
    ("COLLISION_SOUND_ATTR_KAMEHIT", 0x11),
    ("COLLISION_SOUND_ATTR_PEACH_BINTA", 0x12),
    ("COLLISION_SOUND_ATTR_PEACH_FRYINGPAN", 0x13),
    ("COLLISION_SOUND_ATTR_PEACH_GOLF", 0x14),
    ("COLLISION_SOUND_ATTR_PEACH_TENNIS", 0x15),
    ("COLLISION_SOUND_ATTR_PEACH_PARASOL", 0x16),
    ("COLLISION_SOUND_ATTR_DAISY_BINTA", 0x17),
    ("COLLISION_SOUND_ATTR_DAISY_FRYINGPAN", 0x18),
    ("COLLISION_SOUND_ATTR_DAISY_GOLF", 0x19),
    ("COLLISION_SOUND_ATTR_DAISY_TENNIS", 0x1a),
    ("COLLISION_SOUND_ATTR_DAISY_PARASOL", 0x1b),
    ("COLLISION_SOUND_ATTR_LUCARIO", 0x1c),
    ("COLLISION_SOUND_ATTR_MARTH_SWORD", 0x1d),
    ("COLLISION_SOUND_ATTR_MARTH_FINAL", 0x1e),
    ("COLLISION_SOUND_ATTR_MARIO_COIN", 0x1f),
    ("COLLISION_SOUND_ATTR_MARIO_FINAL", 0x20),
    ("COLLISION_SOUND_ATTR_LUIGI_COIN", 0x21),
    ("COLLISION_SOUND_ATTR_NESS_BAT", 0x22),
    ("COLLISION_SOUND_ATTR_FREEZE", 0x23),
    ("COLLISION_SOUND_ATTR_MARIO_FIREBALL", 0x24),
    ("COLLISION_SOUND_ATTR_MARIO_COIN_LAST", 0x25),
    ("COLLISION_SOUND_ATTR_MARIO_MANT", 0x26),
    ("COLLISION_SOUND_ATTR_FOX_BLASTER", 0x27),
    ("COLLISION_SOUND_ATTR_LUIGI_ATTACKDASH", 0x28),
    ("COLLISION_SOUND_ATTR_LUIGI_SMASH", 0x29),
    ("COLLISION_SOUND_ATTR_MARIO_WATER_PUMP", 0x2a),
    ("COLLISION_SOUND_ATTR_PACMAN_BELL", 0x2b),
    ("COLLISION_SOUND_ATTR_GURUGURU_HIT", 0x2c),
    ("COLLISION_SOUND_ATTR_LIZARDON_FIRE", 0x2d),
    ("COLLISION_SOUND_ATTR_TRAIN_HIT", 0x2e),
    ("COLLISION_SOUND_ATTR_MARIOD_COIN_LAST", 0x2f),
    ("COLLISION_SOUND_ATTR_MARIOD_MANT", 0x30),
    ("COLLISION_SOUND_ATTR_MARIOD_CAPSULE", 0x31),
    ("COLLISION_SOUND_ATTR_PACMAN_WATER", 0x32),
    ("COLLISION_SOUND_ATTR_MIIGUNNER_BLASTER", 0x33),
    ("COLLISION_SOUND_ATTR_REFLET_FINAL_SWORD", 0x34),
    ("COLLISION_SOUND_ATTR_REFLET_FINAL_FIRE", 0x35),
    ("COLLISION_SOUND_ATTR_REFLET_FINAL_ELEC", 0x36),
    ("COLLISION_SOUND_ATTR_DUCKHUNT_FINAL", 0x37),
    ("COLLISION_SOUND_ATTR_SHULK_FINAL_DANBAN", 0x38),
    ("COLLISION_SOUND_ATTR_SHULK_FINAL_RIKI", 0x39),
    ("COLLISION_SOUND_ATTR_FALCO_BLASTER", 0x3a),
    ("COLLISION_SOUND_ATTR_RYU_PUNCH", 0x3b),
    ("COLLISION_SOUND_ATTR_RYU_KICK", 0x3c),
    ("COLLISION_SOUND_ATTR_LUCAS_BAT", 0x3d),
    ("COLLISION_SOUND_ATTR_RYU_FINAL01", 0x3e),
    ("COLLISION_SOUND_ATTR_RYU_FINAL02", 0x3f),
    ("COLLISION_SOUND_ATTR_RYU_FINAL03", 0x40),
    ("COLLISION_SOUND_ATTR_CLOUD_HIT", 0x41),
    ("COLLISION_SOUND_ATTR_CLOUD_SMASH_01", 0x42),
    ("COLLISION_SOUND_ATTR_CLOUD_SMASH_02", 0x43),
    ("COLLISION_SOUND_ATTR_CLOUD_SMASH_03", 0x44),
    ("COLLISION_SOUND_ATTR_CLOUD_FINAL_01", 0x45),
    ("COLLISION_SOUND_ATTR_CLOUD_FINAL_02", 0x46),
    ("COLLISION_SOUND_ATTR_CLOUD_FINAL_03", 0x47),
    ("COLLISION_SOUND_ATTR_BAYONETTA_HIT_01", 0x48),
    ("COLLISION_SOUND_ATTR_BAYONETTA_HIT_02", 0x49),
    ("COLLISION_SOUND_ATTR_YOSHI_BITE_HIT", 0x4a),
    ("COLLISION_SOUND_ATTR_YOSHI_EGG_HIT", 0x4b),
    ("COLLISION_SOUND_ATTR_ROY_HIT", 0x4c),
    ("COLLISION_SOUND_ATTR_CHROM_HIT", 0x4d),
    ("COLLISION_SOUND_ATTR_FOX_TAIL", 0x4e),
    ("COLLISION_SOUND_ATTR_HEAVY", 0x4f),
    ("COLLISION_SOUND_ATTR_SLAP", 0x50),
    ("COLLISION_SOUND_ATTR_ITEM_HAMMER", 0x51),
    ("COLLISION_SOUND_ATTR_INKLING_HIT", 0x52),
    ("COLLISION_SOUND_ATTR_MARIO_LOCAL_COIN", 0x53),
    ("COLLISION_SOUND_ATTR_MARIO_LOCAL_COIN_LAST", 0x54),
    ("COLLISION_SOUND_ATTR_FAMICOM_HIT", 0x55),
    ("COLLISION_SOUND_ATTR_ZENIGAME_SHELLHIT", 0x56),
    ("COLLISION_SOUND_ATTR_SAMUS_SCREW", 0x57),
    ("COLLISION_SOUND_ATTR_SAMUSD_SCREW", 0x58),
    ("COLLISION_SOUND_ATTR_SAMUS_SCREW_FINISH", 0x59),
    ("COLLISION_SOUND_ATTR_SAMUSD_SCREW_FINISH", 0x5a),
    ("COLLISION_SOUND_ATTR_KEN_PUNCH", 0x5b),
    ("COLLISION_SOUND_ATTR_KEN_KICK", 0x5c),
    ("COLLISION_SOUND_ATTR_KEN_FINAL01", 0x5d),
    ("COLLISION_SOUND_ATTR_KEN_FINAL02", 0x5e),
    ("COLLISION_SOUND_ATTR_KEN_FINAL03", 0x5f),
    ("COLLISION_SOUND_ATTR_SHIZUE_HAMMER", 0x60),
    ("COLLISION_SOUND_ATTR_SIMON_WHIP", 0x61),
    ("COLLISION_SOUND_ATTR_SIMON_CROSS", 0x62),
    ("COLLISION_SOUND_ATTR_RICHTER_WHIP", 0x63),
    ("COLLISION_SOUND_ATTR_RICHTER_CROSS", 0x64),
    ("COLLISION_SOUND_ATTR_SHEIK_FINAL_PUNCH", 0x65),
    ("COLLISION_SOUND_ATTR_SHEIK_FINAL_KICK", 0x66),
    ("COLLISION_SOUND_ATTR_METAKNIGHT_FINAL_HIT", 0x67),
    ("COLLISION_SOUND_ATTR_ROBOT_FINAL_HIT", 0x68),
    ("COLLISION_SOUND_ATTR_KEN_SHORYU", 0x69),
    ("COLLISION_SOUND_ATTR_DIDDY_SCRATCH", 0x6a),
    ("COLLISION_SOUND_ATTR_MIIENEMYG_BLASTER", 0x6b),
    ("COLLISION_SOUND_ATTR_TOONLINK_HIT", 0x6c),
    ("COLLISION_SOUND_ATTR_JACK_SHOT", 0x6d),
    ("COLLISION_SOUND_ATTR_BRAVE_CRITICALHIT", 0x6e),
    ("COLLISION_SOUND_ATTR_BUDDY_WING", 0x6f),
    ("COLLISION_SOUND_ATTR_DOLLY_PUNCH", 0x70),
    ("COLLISION_SOUND_ATTR_DOLLY_KICK", 0x71),
    ("COLLISION_SOUND_ATTR_DOLLY_CRITICAL", 0x72),
    ("COLLISION_SOUND_ATTR_DOLLY_SUPERSPECIAL01", 0x73),
    ("COLLISION_SOUND_ATTR_MASTER_AXE", 0x74),
    ("COLLISION_SOUND_ATTR_MASTER_ARROW_MAX", 0x75),
    ("COLLISION_SOUND_ATTR_MASTER_ATTACK100END", 0x76),
    ("COLLISION_SOUND_ATTR_TANTAN_PUNCH01", 0x77),
    ("COLLISION_SOUND_ATTR_TANTAN_PUNCH02", 0x78),
    ("COLLISION_SOUND_ATTR_TANTAN_PUNCH03", 0x79),
    ("COLLISION_SOUND_ATTR_TANTAN_FINAL", 0x7a),
    ("COLLISION_SOUND_ATTR_CLOUD_FINAL_APPEND_HIT01", 0x7b),
    ("COLLISION_SOUND_ATTR_CLOUD_FINAL_APPEND_HIT02", 0x7c),
    ("COLLISION_SOUND_ATTR_FLAME_FINAL", 0x7d),
    ("COLLISION_SOUND_ATTR_DEMON_PUNCH01", 0x7e),
    ("COLLISION_SOUND_ATTR_DEMON_PUNCH02", 0x7f),
    ("COLLISION_SOUND_ATTR_DEMON_KICK", 0x80),
    ("COLLISION_SOUND_ATTR_DEMON_CATCHATTACK", 0x81),
    ("COLLISION_SOUND_ATTR_DEMON_FINAL", 0x82),
    ("COLLISION_SOUND_ATTR_DEMON_THROWCOMMAND", 0x83),
    ("COLLISION_SOUND_ATTR_DEMON_ATTACKSQUAT4", 0x84),
    ("COLLISION_SOUND_ATTR_DEMON_ATTACKLW3", 0x85),
    ("COLLISION_SOUND_ATTR_DEMON_APPEAL", 0x86),
    ("COLLISION_SOUND_ATTR_TRAIL_SLASH", 0x87),
    ("COLLISION_SOUND_ATTR_TRAIL_STAB", 0x88),
    ("COLLISION_SOUND_ATTR_TRAIL_CLEAVE", 0x89),
    ("COLLISION_SOUND_ATTR_TRAIL_CLEAVE_SINGLE", 0x8a),
    ("COLLISION_SOUND_ATTR_TRAIL_KICK", 0x8b),
    ("COLLISION_SOUND_ATTR_TRAIL_FINAL", 0x8c),
    ("COLLISION_SOUND_ATTR_TERM", 0x8d),
];

/// `ATTACK_REGION_*` — attack region (drives counters, absorb, region-specific SFX).
pub const ATTACK_REGION: ConstTable = &[
    ("ATTACK_REGION_NONE", 0x0),
    ("ATTACK_REGION_HEAD", 0x1),
    ("ATTACK_REGION_BODY", 0x2),
    ("ATTACK_REGION_HIP", 0x3),
    ("ATTACK_REGION_PUNCH", 0x4),
    ("ATTACK_REGION_ELBOW", 0x5),
    ("ATTACK_REGION_KICK", 0x6),
    ("ATTACK_REGION_KNEE", 0x7),
    ("ATTACK_REGION_THROW", 0x8),
    ("ATTACK_REGION_OBJECT", 0x9),
    ("ATTACK_REGION_SWORD", 0xa),
    ("ATTACK_REGION_HAMMER", 0xb),
    ("ATTACK_REGION_BOMB", 0xc),
    ("ATTACK_REGION_SPIN", 0xd),
    ("ATTACK_REGION_BITE", 0xe),
    ("ATTACK_REGION_MAGIC", 0xf),
    ("ATTACK_REGION_PSI", 0x10),
    ("ATTACK_REGION_PALUTENA", 0x11),
    ("ATTACK_REGION_AURA", 0x12),
    ("ATTACK_REGION_BAT", 0x13),
    ("ATTACK_REGION_PARASOL", 0x14),
    ("ATTACK_REGION_PIKMIN", 0x15),
    ("ATTACK_REGION_WATER", 0x16),
    ("ATTACK_REGION_WHIP", 0x17),
    ("ATTACK_REGION_TAIL", 0x18),
    ("ATTACK_REGION_ENERGY", 0x19),
    ("ATTACK_REGION_TERM", 0x1a),
];

/// `collision_attr_*` hash40 labels (from ultimate-research/param-labels).
pub const COLLISION_ATTRS: &[&str] = &[
    "collision_attr_normal",
    "collision_attr_none",
    "collision_attr_fire",
    "collision_attr_elec",
    "collision_attr_ice",
    "collision_attr_water",
    "collision_attr_grass",
    "collision_attr_magic",
    "collision_attr_aura",
    "collision_attr_punch",
    "collision_attr_kick",
    "collision_attr_cutup",
    "collision_attr_stab",
    "collision_attr_whip",
    "collision_attr_coin",
    "collision_attr_bury",
    "collision_attr_sleep",
    "collision_attr_paralyze",
    "collision_attr_flower",
    "collision_attr_slip",
    "collision_attr_sting",
    "collision_attr_turn",
    "collision_attr_pierce",
    "collision_attr_rush",
    "collision_attr_saving",
    "collision_attr_auto_shift",
    "collision_attr_bind",
    "collision_attr_bind_extra",
    "collision_attr_blaster_throw_down",
    "collision_attr_blaster_throw_up",
    "collision_attr_bury_f",
    "collision_attr_bury_r",
    "collision_attr_curse_poison",
    "collision_attr_cutup_metal",
    "collision_attr_cutup_poison",
    "collision_attr_death",
    "collision_attr_deathball",
    "collision_attr_deathscythe",
    "collision_attr_dedede_hammer",
    "collision_attr_deep_sleep",
    "collision_attr_elec_whip",
    "collision_attr_fist_down",
    "collision_attr_fist_down2",
    "collision_attr_fist_down3",
    "collision_attr_grudge",
    "collision_attr_head_mushroom",
    "collision_attr_ink_hit",
    "collision_attr_item_hammer",
    "collision_attr_jack_bullet",
    "collision_attr_jack_final",
    "collision_attr_lay",
    "collision_attr_leviathan_wave",
    "collision_attr_leviathan_wave_owner",
    "collision_attr_mario_local_coin",
    "collision_attr_marth_shield_breaker",
    "collision_attr_noamal",
    "collision_attr_normal_bullet",
    "collision_attr_normal_poison",
    "collision_attr_odin_slash",
    "collision_attr_palutena_bullet",
    "collision_attr_paralyze_ghost",
    "collision_attr_pit_fall",
    "collision_attr_purple",
    "collision_attr_saving_ken",
    "collision_attr_search",
    "collision_attr_sleep_ex",
    "collision_attr_sting_bowarrow",
    "collision_attr_sting_flash",
    "collision_attr_stop",
    "collision_attr_taiyo_hit",
];

// ── Const encode / decode ────────────────────────────────────────────────────

/// Canonical symbolic name for a numeric lua value.
///
/// Comparison is masked to 32 bits: masks like `COLLISION_SITUATION_MASK_GA_d`
/// (`0x8000_0003`) can arrive sign-extended from the lua stack.
pub fn const_name(table: ConstTable, value: i64) -> Option<&'static str> {
    let v = (value as u64) & 0xFFFF_FFFF;
    table
        .iter()
        .find(|(_, tv)| ((*tv as u64) & 0xFFFF_FFFF) == v)
        .map(|(n, _)| *n)
}

/// Lua value for a symbolic name (a leading `*` deref is tolerated).
pub fn const_value(table: ConstTable, name: &str) -> Option<i64> {
    let name = name.trim().trim_start_matches('*');
    table.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

/// A raw slot string that may be decimal, `0x`-hex, or a float ("3.0").
pub fn parse_raw_value(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|v| v as i64);
    }
    t.parse::<i64>()
        .ok()
        .or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
}

/// Fetched slot → the symbolic name the dropdown offers.
///
/// Numbers decode to their name; names pass through (minus any `*`); numbers with no known
/// name are kept verbatim so an unrecognised value is never silently rewritten.
pub fn decode_const(table: ConstTable, raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    match parse_raw_value(t) {
        Some(v) => const_name(table, v)
            .map(str::to_string)
            .unwrap_or_else(|| t.to_string()),
        None => t.trim_start_matches('*').to_string(),
    }
}

/// Editor value → the lua number to write into the live ATTACK arg. `None` means "leave the
/// game's own value alone" (empty slot, or a name this build does not know).
pub fn encode_const(table: ConstTable, value: &str) -> Option<i64> {
    let t = value.trim();
    if t.is_empty() {
        return None;
    }
    const_value(table, t).or_else(|| parse_raw_value(t))
}

// ── collision_attr (a hash40, not an int) ────────────────────────────────────

static COLLISION_ATTR_BY_HASH: std::sync::LazyLock<HashMap<u64, &'static str>> =
    std::sync::LazyLock::new(|| {
        COLLISION_ATTRS
            .iter()
            .map(|name| (hash40::hash40(name).0, *name))
            .collect()
    });

/// Live-captured collision-attr hash → its `collision_attr_*` label. Falls back to the
/// downloaded ParamLabels map, then to raw hex so the value still round-trips.
pub fn decode_collision_attr(hash: u64, labels: &HashMap<u64, String>) -> String {
    if hash == 0 {
        return String::new();
    }
    if let Some(name) = COLLISION_ATTR_BY_HASH.get(&hash) {
        return (*name).to_string();
    }
    match labels.get(&hash) {
        Some(l) if l.starts_with("collision_attr_") => l.clone(),
        _ => format!("{hash:#x}"),
    }
}

/// Editor collision-attr value → the hash40 to write into the live ATTACK arg.
pub fn encode_collision_attr(value: &str) -> Option<u64> {
    let t = value.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    Some(hash40::hash40(&t.to_lowercase()).0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_repo_csv_lines() {
        let csv = "0x0000000000,\n0x00aa8bcd06,attack_air_n\n0x112233,with,comma\nnothex,line\n";
        let labels = super::parse_csv(csv);
        assert_eq!(
            labels.get(&0x00aa8bcd06).map(String::as_str),
            Some("attack_air_n")
        );
        // Label keeps everything after the first comma.
        assert_eq!(
            labels.get(&0x112233).map(String::as_str),
            Some("with,comma")
        );
        // Empty labels and unparsable hashes are skipped.
        assert_eq!(labels.len(), 2);
    }

    /// The bug this guards: fetched hitboxes showed a NUMBER while the dropdown offered
    /// NAMES, so nothing ever looked selected. Decode must land on a real dropdown entry.
    #[test]
    fn captured_numbers_decode_to_dropdown_entries() {
        for (table, raw, want) in [
            (super::SETOFF_KIND, "1", "ATTACK_SETOFF_KIND_ON"),
            (super::LR_CHECK, "0", "ATTACK_LR_CHECK_POS"),
            (super::SITUATION_MASK, "3", "COLLISION_SITUATION_MASK_GA"),
            (super::CATEGORY_MASK, "63", "COLLISION_CATEGORY_MASK_ALL"),
            (super::PART_MASK, "31", "COLLISION_PART_MASK_ALL"),
            (super::SOUND_LEVEL, "1", "ATTACK_SOUND_LEVEL_M"),
            (super::SOUND_ATTR, "1", "COLLISION_SOUND_ATTR_PUNCH"),
            (super::ATTACK_REGION, "4", "ATTACK_REGION_PUNCH"),
        ] {
            let decoded = super::decode_const(table, raw);
            assert_eq!(decoded, want, "decode {raw}");
            assert!(
                table.iter().any(|(n, _)| *n == decoded),
                "{decoded} is not an offered option"
            );
            // …and the dropdown's choice encodes back to the number the game wants.
            assert_eq!(
                super::encode_const(table, &decoded),
                super::parse_raw_value(raw),
                "round-trip {raw}"
            );
        }
    }

    /// Situation masks carry bit 31; a sign-extended capture must still resolve.
    #[test]
    fn sign_extended_mask_decodes() {
        let raw = ((0x8000_0003u32 as i32) as i64).to_string();
        assert_eq!(
            super::decode_const(super::SITUATION_MASK, &raw),
            "COLLISION_SITUATION_MASK_GA_d"
        );
    }

    /// An unknown number is preserved verbatim rather than snapped to a wrong name,
    /// and still encodes back to itself.
    #[test]
    fn unknown_value_round_trips() {
        assert_eq!(super::decode_const(super::ATTACK_REGION, "999"), "999");
        assert_eq!(super::encode_const(super::ATTACK_REGION, "999"), Some(999));
    }

    /// collision_attr is a hash40; hashes must match ultimate-research/param-labels.
    #[test]
    fn collision_attr_hashes_match_param_labels() {
        let labels = std::collections::HashMap::new();
        assert_eq!(
            super::decode_collision_attr(0x15a2c502b3, &labels),
            "collision_attr_normal"
        );
        assert_eq!(
            super::encode_collision_attr("collision_attr_normal"),
            Some(0x15a2c502b3)
        );
        assert_eq!(
            super::encode_collision_attr("collision_attr_elec"),
            Some(0x13462fcfe4)
        );
        // Unknown hash falls back to hex, which still encodes back to the same hash.
        let unknown = super::decode_collision_attr(0x1234, &labels);
        assert_eq!(unknown, "0x1234");
        assert_eq!(super::encode_collision_attr(&unknown), Some(0x1234));
    }

    /// Empty slots must NOT be encoded — that is what keeps the plugin from clobbering a
    /// value the editor never resolved.
    #[test]
    fn empty_encodes_to_none() {
        assert_eq!(super::encode_const(super::SOUND_LEVEL, ""), None);
        assert_eq!(super::encode_collision_attr("  "), None);
    }

    #[test]
    fn measured_work_detach_tokens_encode_with_the_pinned_slots() {
        let expected = [
            (
                "*FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_AFTERBURNER_LINE",
                0x100000D3,
            ),
            (
                "FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_ATTACK_ARC1",
                0x100000CC,
            ),
            (
                "*FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_ATTACK_ARC2",
                0x100000CD,
            ),
            (
                "FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_ATTACK_LINE1",
                0x100000CA,
            ),
            (
                "*FIGHTER_BAYONETTA_INSTANCE_WORK_ID_INT_EFFECT_KIND_BAYONETTA_WITCHTWIST_WIND",
                0x100000D4,
            ),
        ];
        for (name, value) in expected {
            assert_eq!(
                super::const_value(super::EFFECT_DETACH_KIND_WORK_SLOTS, name),
                Some(value),
                "symbolic Work slot {name}"
            );
        }
        assert_eq!(
            super::const_value(super::EFFECT_DETACH_KIND_WORK_SLOTS, "OTHER_WORK"),
            None
        );
    }
}
