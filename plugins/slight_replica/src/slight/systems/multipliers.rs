//! Combat knockback / damage multipliers — Jorge "Multipliers" facade.
//!
//! Reverse-engineered from the decomp (see `decomp/reference/multipliers_RE.md`). The original
//! is a COMBAT value-multiplier system (NOT an effect scaler): each multiplier scales a
//! fighter's reaction (knockback) or damage via `DamageModule::set_reaction_mul` /
//! `set_damage_mul`, applied per-frame per agent. A multiplier's value is
//! `clamp(base + Σ dependant_values, min, max)` (the decoded apply math, `FUN_710010ffb8`),
//! letting multipliers form a dependant graph. Stored per fighter-kind group
//! (`fighter_kind → Vec<Multiplier>`, `FUN_710010f198`). Created via the Extras Command System
//! (excommand) SD commands. The effect viewer itself never references the multiplier map
//! (confirmed) — this is SLight framework infrastructure.
//!
//! NOTE: byte/field-exact reconstruction was infeasible (the creation path is a fractal of
//! optimized, inlined, indirect-call functions). This is a BEHAVIOR-faithful reimplementation of
//! the decoded model. Because the system is non-observable in the effect viewer, fighter-kind /
//! pattern targeting is collapsed to boid / all (the replica's consts have no game-kind→name
//! map); the apply math, DamageModule application, dependant graph, and per-frame model are
//! reproduced.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use parking_lot::Mutex;

use smash::app::lua_bind::DamageModule;
use smash::app::sv_battle_object;

use super::pattern_match::PatternRule;

/// Data-space key retained for `fighter_data_space` compatibility (the original keyed effect
/// rows under "Effect data").
pub const EFFECT_DATA_KEY: &str = "Effect data";

#[derive(Clone, Copy, PartialEq, Eq)]
enum MulField {
    Damage,
    Reaction,
}

impl MulField {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "damage" | "dmg" => MulField::Damage,
            // reaction / knockback is the primary combat multiplier and the default
            _ => MulField::Reaction,
        }
    }

    unsafe fn apply(self, boma: *mut smash::app::BattleObjectModuleAccessor, value: f32) {
        match self {
            MulField::Damage => DamageModule::set_damage_mul(boma, value),
            MulField::Reaction => DamageModule::set_reaction_mul(boma, value),
        }
    }
}

/// Which fighters a multiplier applies to. Matched per-frame against the agent's game fighter
/// name (the original groups multipliers by fighter kind — `FUN_710010f198`).
enum Target {
    /// A specific battle object.
    Boid(u32),
    /// A fighter by lowercase game name (e.g. "mario").
    FighterName(String),
    /// A regex rule matched against the fighter's game name (the original used `regex`).
    Pattern(PatternRule),
    /// Every fighter.
    All,
}

impl Target {
    /// Does this target apply to a fighter with `boid` and lowercase game `name`?
    fn matches(&self, boid: u32, name: Option<&str>) -> bool {
        match self {
            Target::Boid(b) => *b == boid,
            Target::All => true,
            Target::FighterName(n) => name.is_some_and(|fname| fname.eq_ignore_ascii_case(n)),
            Target::Pattern(rule) => name.is_some_and(|fname| rule.is_match(fname.as_bytes())),
        }
    }
}

struct Multiplier {
    id: u64,
    target: Target,
    field: MulField,
    /// Base factor (entry +0x80 in the decomp).
    base: f32,
    /// Clamp bounds (entry +0x78 / +0x7c).
    min: f32,
    max: f32,
    /// Ids of multipliers whose values are summed into this one (the dependant graph).
    dependants: Vec<u64>,
}

static MULTIPLIERS: LazyLock<Mutex<Vec<Multiplier>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static DIRTY_CLIENT: AtomicBool = AtomicBool::new(false);

pub fn install() {
    // (the facade logs "Installing facade Multipliers"); registry is ready lazily.
}

/// `value = clamp(base + Σ value(dependant), min, max)` — the decoded apply math
/// (`FUN_710010ffb8`). `depth` guards against dependant cycles.
fn value_of(id: u64, list: &[Multiplier], depth: u32) -> f32 {
    let Some(m) = list.iter().find(|m| m.id == id) else {
        return 0.0;
    };
    let mut sum = m.base;
    if depth < 16 {
        for dep in &m.dependants {
            sum += value_of(*dep, list, depth + 1);
        }
    }
    sum.clamp(m.min, m.max)
}

fn upsert(mul: Multiplier) {
    let mut list = MULTIPLIERS.lock();
    if let Some(existing) = list.iter_mut().find(|m| m.id == mul.id) {
        *existing = mul;
    } else {
        list.push(mul);
    }
    DIRTY_CLIENT.store(true, Ordering::Relaxed);
}

/// Synthesize a stable id for the implicit (no-id) excommand forms so re-setting the same
/// target+field updates rather than duplicates.
fn synth_id(tag: u8, key: u64, field: MulField) -> u64 {
    let f = (field == MulField::Damage) as u64;
    0x8000_0000_0000_0000 | ((tag as u64) << 40) | (key << 1) | f
}

/// excommand: `set_multiplier <boid> <field> <value>` — multiplier on a specific object.
pub fn set_rule(boid: u32, field: &str, value: f32) {
    let field = MulField::parse(field);
    upsert(Multiplier {
        id: synth_id(0, boid as u64, field),
        target: Target::Boid(boid),
        field,
        base: value,
        min: f32::MIN,
        max: f32::MAX,
        dependants: Vec::new(),
    });
}

/// excommand: `set_multiplier <fighter> <field> <value>` — multiplier for a fighter (collapsed
/// to all fighters, see module note).
pub fn set_fighter_rule(fighter: &str, field: &str, value: f32) {
    let field = MulField::parse(field);
    let key = fighter
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64))
        & 0xFFFF_FFFF;
    upsert(Multiplier {
        id: synth_id(1, key, field),
        target: Target::FighterName(fighter.trim().to_ascii_lowercase()),
        field,
        base: value,
        min: f32::MIN,
        max: f32::MAX,
        dependants: Vec::new(),
    });
}

/// excommand: explicit-id multiplier whose `pattern` regex selects fighters by game name, with
/// clamp bounds (the JSON/CSV pattern form). The original used `regex` for this.
pub fn set_pattern_rule(id: u64, pattern: &str, field: &str, factor: f32, min: f32, max: f32) {
    let field = MulField::parse(field);
    let target = match PatternRule::compile(pattern) {
        Some(rule) => Target::Pattern(rule),
        None => {
            skyline::println!("[SLight] Invalid multiplier pattern: {pattern}");
            return;
        }
    };
    upsert(Multiplier {
        id,
        target,
        field,
        base: factor,
        min,
        max,
        dependants: Vec::new(),
    });
}

pub fn clear() {
    MULTIPLIERS.lock().clear();
    DIRTY_CLIENT.store(true, Ordering::Relaxed);
}

/// Per-frame application (`FUN_710010f198`): for each live fighter, combine the applicable
/// multipliers per field and push them to the game via DamageModule.
pub fn on_frame() {
    apply_all();
    sync_to_client();
}

fn apply_all() {
    let list = MULTIPLIERS.lock();
    if list.is_empty() || crate::slight::frame_context::is_after_win() {
        return;
    }
    for rec in crate::slight::agents::all_records() {
        if rec.category != 0 {
            continue;
        }
        let boma = unsafe { sv_battle_object::module_accessor(rec.boid) };
        if boma.is_null() {
            continue;
        }
        // Resolve the fighter's game name once for FighterName / Pattern matching.
        let name = crate::slight::slight_consts::fighters::game_kind_name(rec.kind);
        // Multiple multipliers on the same field stack multiplicatively.
        let mut damage = 1.0f32;
        let mut reaction = 1.0f32;
        let mut has_damage = false;
        let mut has_reaction = false;
        for m in list.iter() {
            if !m.target.matches(rec.boid, name) {
                continue;
            }
            let v = value_of(m.id, &list, 0);
            match m.field {
                MulField::Damage => {
                    damage *= v;
                    has_damage = true;
                }
                MulField::Reaction => {
                    reaction *= v;
                    has_reaction = true;
                }
            }
        }
        unsafe {
            if has_damage {
                MulField::Damage.apply(boma, damage);
            }
            if has_reaction {
                MulField::Reaction.apply(boma, reaction);
            }
        }
    }
}

/// Push a multiplier-registry snapshot to the RPM client when the set changes
/// (Jorge `FUN_71000936c8`).
fn sync_to_client() {
    if !DIRTY_CLIENT.swap(false, Ordering::Relaxed) {
        return;
    }
    if !crate::rust_extender::net::simple_server::has_client() {
        return;
    }
    let list = MULTIPLIERS.lock();
    let direct = list
        .iter()
        .filter(|m| matches!(m.target, Target::Boid(_)))
        .count();
    let pattern = list.len() - direct;
    crate::rust_extender::debuggable_server::notify_multipliers(direct, pattern);
}
