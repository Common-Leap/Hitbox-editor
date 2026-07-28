//! hash40 → name reverse-resolution for effect and bone names.
//!
//! Visionary does not bundle the game-derived effect-name dictionary. Common bone names are
//! built in, names created by the editor are registered at runtime, and users can provide an
//! external name dictionary by placing one name per line in
//! `sd:/slight/user/effect_names.txt`. Unknown hashes fall back to hexadecimal.

use std::collections::HashMap;
use std::sync::LazyLock;

use smash::hash40;

/// Common skeleton/joint names — `bone_name` is also a hash40 and shares this resolver.
const BONE_NAMES: &[&str] = &[
    "top",
    "trans",
    "rot",
    "throw",
    "hip",
    "bust",
    "bustn",
    "neck",
    "head",
    "havel",
    "haver",
    "armr",
    "arml",
    "shoulderr",
    "shoulderl",
    "handr",
    "handl",
    "hipr",
    "hipl",
    "kneer",
    "kneel",
    "footr",
    "footl",
    "legr",
    "legl",
    "fingerr",
    "fingerl",
    "thumbr",
    "thumbl",
    "swordr",
    "swordl",
    "weaponr",
    "weaponl",
    "facen",
    "eye",
    "eyer",
    "eyel",
    "mouth",
    "toer",
    "toel",
    "tail",
    "wing",
    "wingr",
    "wingl",
    "shieldn",
    "hand",
    "foot",
    "knee",
    "elbow",
    "elbowr",
    "elbowl",
    "wristr",
    "wristl",
];

/// Optional user supplement (name-per-line). Lets DLC/custom effects resolve without a rebuild.
const SUPPLEMENT_FILE: &str = "sd:/slight/user/effect_names.txt";

/// hash40 → name. Built once from common bone names and the external supplement.
static REVERSE: LazyLock<HashMap<u64, String>> = LazyLock::new(build_reverse_map);

fn build_reverse_map() -> HashMap<u64, String> {
    let supp = std::fs::read_to_string(SUPPLEMENT_FILE).unwrap_or_default();
    let mut map = HashMap::with_capacity(512);
    let insert = |map: &mut HashMap<u64, String>, name: &str| {
        let name = name.trim();
        if !name.is_empty() {
            map.entry(hash40(name)).or_insert_with(|| name.to_string());
        }
    };
    for name in BONE_NAMES {
        insert(&mut map, name);
    }
    for name in supp.lines() {
        insert(&mut map, name);
    }
    skyline::println!("[SLight] effect-name dictionary: {} entries", map.len());
    map
}

/// Names registered at runtime (editor transplant copies etc.). Checked after the static
/// dictionary.
static EXTRA: LazyLock<parking_lot::RwLock<HashMap<u64, String>>> =
    LazyLock::new(|| parking_lot::RwLock::new(HashMap::new()));

/// Register custom names (lowercased — kind hashes are computed on lowercase).
pub fn register(names: &[String]) {
    let mut extra = EXTRA.write();
    for n in names {
        let n = n.trim().to_lowercase();
        if !n.is_empty() {
            extra.entry(hash40(&n)).or_insert(n);
        }
    }
}

/// Suffixes the editor appends to a transplanted entry's name.
///
/// `_tp` is what Visionary writes today; `_os` is the historical spelling from when
/// transplanting was still called "one-slotting". BOTH must keep resolving — projects
/// authored before the rename have `_os` entries baked into their exported effs and
/// ACMD scripts, and those files are not rewritten on load.
pub const TRANSPLANT_SUFFIXES: [&str; 2] = ["_tp", "_os"];

/// True if `label` looks like an editor-created transplant entry.
pub fn is_transplant_label(label: &str) -> bool {
    TRANSPLANT_SUFFIXES.iter().any(|s| label.ends_with(s))
}

/// Resolve a hash40 to its name, or `0x<hash>` if unknown.
pub fn label(hash: u64) -> String {
    if hash == 0 {
        return "0x0".to_string();
    }
    if let Some(name) = REVERSE.get(&hash) {
        return name.clone();
    }
    if let Some(name) = EXTRA.read().get(&hash) {
        return name.clone();
    }
    format!("0x{hash:x}")
}
