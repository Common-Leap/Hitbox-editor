//! Grouping for the move list.
//!
//! The list used to be six substrings wide (`attack`, `special`, `throw`, `catch`, `cliff`,
//! `final`) and everything else was invisible. That was a *performance* guard — it avoided
//! parsing a `.nuanmb` per move for its frame count — and it was correct while the editor only
//! edited hitboxes, because non-attack moves mostly have none. D1 changed what "relevant" means:
//! 65% of the corpus's sound scripts and 57% of its sound calls are in moves the filter hid,
//! including every `PLAY_FLY_VOICE` in the corpus.
//!
//! Widening it makes the list ~460 entries for a fighter, which is not something anyone can
//! scroll. So the list is grouped, and the groups are derived from what the corpus actually
//! contains rather than from what a moveset "should" have — see
//! `every_corpus_move_name_lands_in_a_group`.

/// A section in the move list, in the order the panel draws them.
///
/// Ordered by how often a modder opens one, not alphabetically: the attack families first,
/// because that is what the editor was built for and what most sessions touch, and the long tail
/// of situational states last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MoveGroup {
    Jab,
    Tilt,
    Smash,
    DashAttack,
    Aerial,
    Special,
    /// A special copied from another fighter — Kirby's inhale results, spelled `<donor>_special_*`.
    ///
    /// A group of its own rather than part of [`MoveGroup::Special`] because they are a different
    /// fighter's move living in this one's list, and a modder looking for "Kirby's neutral B"
    /// should not have to scroll past thirty of them. Note these are only the copies that sit in
    /// the ordinary motion list; the per-ability motion *directories* are a separate problem
    /// entirely (R6).
    CopySpecial,
    Grab,
    Ledge,
    FinalSmash,
    Movement,
    JumpLand,
    Idle,
    Defense,
    Damage,
    /// Carrying and using an item, including the weapon movesets.
    ///
    /// Large: `scope_*` alone is about fifty names. They are here rather than in
    /// the fighter's own attack groups because they belong to the *item*, not to the fighter — every
    /// fighter shares them, and they are edited when modding the item.
    Item,
    Presentation,
    Situational,
    /// Nothing matched. Kept as a real group rather than hidden, because a move the editor cannot
    /// classify is still a move the user may need to open — hiding it is what this whole entry
    /// exists to undo.
    Other,
}

impl MoveGroup {
    pub const ORDER: [MoveGroup; 18] = [
        MoveGroup::Jab,
        MoveGroup::Tilt,
        MoveGroup::Smash,
        MoveGroup::DashAttack,
        MoveGroup::Aerial,
        MoveGroup::Special,
        MoveGroup::CopySpecial,
        MoveGroup::Grab,
        MoveGroup::Ledge,
        MoveGroup::FinalSmash,
        MoveGroup::Movement,
        MoveGroup::JumpLand,
        MoveGroup::Idle,
        MoveGroup::Defense,
        MoveGroup::Damage,
        MoveGroup::Item,
        MoveGroup::Presentation,
        MoveGroup::Situational,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MoveGroup::Jab => "Jabs",
            MoveGroup::Tilt => "Tilts",
            MoveGroup::Smash => "Smashes",
            MoveGroup::DashAttack => "Dash attack",
            MoveGroup::Aerial => "Aerials",
            MoveGroup::Special => "Specials",
            MoveGroup::CopySpecial => "Copied specials",
            MoveGroup::Grab => "Grabs & throws",
            MoveGroup::Ledge => "Ledge",
            MoveGroup::FinalSmash => "Final Smash",
            MoveGroup::Movement => "Movement",
            MoveGroup::JumpLand => "Jumps & landing",
            MoveGroup::Idle => "Idle & crouch",
            MoveGroup::Defense => "Defense",
            MoveGroup::Damage => "Damage & knockdown",
            MoveGroup::Item => "Items & weapons",
            MoveGroup::Presentation => "Taunts & results",
            MoveGroup::Situational => "Situational",
            MoveGroup::Other => "Other",
        }
    }

    /// Plain-language help for the matching move-list section.
    pub fn description(self) -> &'static str {
        match self {
            MoveGroup::Jab => "Standing neutral attacks, including rapid-jab phases.",
            MoveGroup::Tilt => "Grounded directional attacks that are not smash attacks.",
            MoveGroup::Smash => "Side, up, and down smash attacks, including charge phases.",
            MoveGroup::DashAttack => "Attacks performed from a dash.",
            MoveGroup::Aerial => "Neutral, forward, back, up, and down aerial attacks.",
            MoveGroup::Special => "This fighter's neutral, side, up, and down special moves.",
            MoveGroup::CopySpecial => {
                "Special moves copied from another fighter, primarily Kirby copy abilities."
            }
            MoveGroup::Grab => "Grab, pummel, catch, and throw animations.",
            MoveGroup::Ledge => "Ledge catches, climbs, attacks, rolls, and related states.",
            MoveGroup::FinalSmash => "Final Smash animations and their supporting phases.",
            MoveGroup::Movement => "Walking, running, dashing, turning, and braking animations.",
            MoveGroup::JumpLand => "Jump, fall, and landing animations.",
            MoveGroup::Idle => "Standing idle, crouching, and related waiting animations.",
            MoveGroup::Defense => "Shield, dodge, and other defensive animations.",
            MoveGroup::Damage => "Hit reactions, knockdown, capture, sleep, and recovery states.",
            MoveGroup::Item => "Shared item-use and weapon-specific animations.",
            MoveGroup::Presentation => "Taunts, entrances, victories, losses, and result poses.",
            MoveGroup::Situational => {
                "Uncommon environment or mechanic states such as swimming, ladders, and wall contact."
            }
            MoveGroup::Other => "Moves whose internal names do not fit a known category.",
        }
    }
}

/// Weapon movesets that do not share the `_swing` shape, matched as prefixes.
///
/// These are item moves rather than fighter moves — every fighter shares them — so they group
/// with `item_*` rather than with the fighter's own attacks.
///
/// **Kept as short as possible on purpose.** A hand-written list of weapon names rots: the first
/// draft of this one guessed `lipstick`, and the real motion is `lip_stick_swing1`, so all five
/// Lip's Stick moves fell through to `Other` while the entry read as covered. Anything with a
/// `_swing` in it is caught by shape below and does not belong here — this list is only for the
/// weapons whose moves are named for what they *do* (`scope_air_rapid`, `magic_pot_start`) rather
/// than for the swing.
const WEAPON_PREFIXES: &[&str] = &[
    "scope_",
    "l_gun_",
    "steel_diver_",
    "drill_shoot",
    "f_flower_",
    "genesis_",
    "magic_pot_",
    "shoot_legs_",
    "hammer_",
    "dragoon_",
    "warp_star",
    "assist_item",
];

/// True for the melee-item movesets, which are all spelled `<weapon>_swing…`.
///
/// A shape rather than a name list, because the names are per weapon and there is one for every
/// melee item in the game — `bat_swing1`, `club_swing4_charge`, `death_scythe_swing_dash`,
/// `fire_bar_swing3`, `kill_sword_swing1_common`, `lip_stick_swing4`, `sword_swing_dash`. The
/// fighter's own attacks are claimed by an earlier arm, so this cannot swallow one.
fn is_weapon_swing(name: &str) -> bool {
    name.contains("_swing")
}

/// Which section of the move list a motion name belongs in.
///
/// Order matters here and several arms exist only because an earlier one would have swallowed
/// them — each is commented with what it is defending against. The name is the lowercase
/// snake_case motion name (`attack_air_n`), which is what `ParamLabels.csv` resolves a motion
/// hash to.
pub fn group_of(name: &str) -> MoveGroup {
    let n = name.trim().to_lowercase();

    // Before the `special` arms: a copied special is `<donor>_special_*`, and testing for
    // `contains("special")` first would file every one of them under Specials.
    if let Some(rest) = n.split_once("_special") {
        if !n.starts_with("special") && !rest.0.is_empty() && !n.starts_with("item_") {
            return MoveGroup::CopySpecial;
        }
    }
    // Before the attack arms: `final_` is spelled `special_s_final` on some fighters and
    // `final_*` on others, and a Final Smash is not an ordinary special.
    if n.starts_with("final") || n.contains("_final") {
        return MoveGroup::FinalSmash;
    }
    if n.starts_with("special") {
        return MoveGroup::Special;
    }
    // Before the ground arm, which would otherwise take every `attack_air_*` as well.
    if n.starts_with("attack_air") {
        return MoveGroup::Aerial;
    }
    // The four ground families, split the way a modder thinks about them rather than by prefix
    // length. Order within this block matters once: `attack_1*` covers the jab string
    // (`attack_11`/`_12`/`_13`) and `attack_100` the rapid jab, so it has to be tested as a
    // prefix of `attack_1` and not as an equality.
    if n.starts_with("attack_dash") {
        return MoveGroup::DashAttack;
    }
    if n.starts_with("attack_s3") || n.starts_with("attack_hi3") || n.starts_with("attack_lw3") {
        return MoveGroup::Tilt;
    }
    if n.starts_with("attack_s4") || n.starts_with("attack_hi4") || n.starts_with("attack_lw4") {
        return MoveGroup::Smash;
    }
    // **The underscore before the number is optional, and both spellings are real.**
    // `ParamLabels.csv` carries `attack_11` *and* `attack11`, `attack_100` *and* `attack100` —
    // the motion and the ACMD script are labelled differently and either can reach here
    // depending on which hash was resolved. Matching only `attack_1` put every jab in the game
    // into `Other`, and the corpus guard below is what caught it.
    if let Some(rest) = n.strip_prefix("attack") {
        let rest = rest.strip_prefix('_').unwrap_or(rest);
        if rest.starts_with('1') || rest.starts_with('9') {
            return MoveGroup::Jab;
        }
    }
    // Anything else beginning `attack` is a fighter move of some kind, and belongs with the
    // ground families rather than falling through to `Other`.
    if n.starts_with("attack") {
        return MoveGroup::Tilt;
    }
    if n.starts_with("catch") || n.contains("throw") {
        return MoveGroup::Grab;
    }
    if n.starts_with("cliff") {
        return MoveGroup::Ledge;
    }
    // Before `item_`: the weapon movesets do not share its prefix.
    if is_weapon_swing(&n)
        || WEAPON_PREFIXES.iter().any(|p| n.starts_with(p))
        || n.starts_with("item")
    {
        return MoveGroup::Item;
    }
    // Before the movement arm: `shoot_legs_dash_f` is already claimed above, but `turn_run_brake`
    // and `run_brake_l` both have to reach Movement rather than one of them falling through.
    if n.starts_with("walk")
        || n.starts_with("run")
        || n.starts_with("dash")
        || n.starts_with("turn")
        || n.starts_with("step_pose")
    {
        return MoveGroup::Movement;
    }
    if n.starts_with("jump") || n.starts_with("landing") || n.starts_with("fall") {
        return MoveGroup::JumpLand;
    }
    // Before Defense: `squat_wait_item` is an idle, and `passive` is a knockdown recovery.
    if n.starts_with("wait") || n.starts_with("squat") || n.starts_with("bend") {
        return MoveGroup::Idle;
    }
    if n.starts_with("guard")
        || n.starts_with("escape")
        || n.starts_with("just_shield")
        || n.starts_with("shield")
    {
        return MoveGroup::Defense;
    }
    if n.starts_with("lie") {
        return MoveGroup::Damage;
    }
    if n.starts_with("damage")
        || n.starts_with("down")
        || n.starts_with("passive")
        || n.starts_with("fura_fura")
        || n.starts_with("slip")
        || n.starts_with("bind")
        || n.starts_with("capture")
        || n.starts_with("ceil_damage")
        || n.starts_with("bury")
        || n.starts_with("sleep")
    {
        return MoveGroup::Damage;
    }
    if n.starts_with("appeal")
        || n.starts_with("win")
        || n.starts_with("lose")
        || n.starts_with("entry")
        || n.starts_with("result")
        || n.starts_with("smash_exit")
    {
        return MoveGroup::Presentation;
    }
    if n.starts_with("swim")
        || n.starts_with("ladder")
        || n.starts_with("ottotto")
        || n.starts_with("attach_wall")
        || n.starts_with("stop_wall")
        || n.starts_with("stop_ceil")
        || n.starts_with("glide")
        || n.starts_with("adventure")
        || n.starts_with("screw")
        || n.starts_with("tornado")
        || n.starts_with("set_ink")
    {
        return MoveGroup::Situational;
    }
    MoveGroup::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Named examples land where a person would look for them.
    ///
    /// Each of these is an arm that an earlier arm would have swallowed, so this is the ordering
    /// of `group_of` under test rather than its contents. Reordering the function passes every
    /// other check in this file.
    #[test]
    fn the_arms_are_ordered_so_the_specific_case_wins() {
        // `attack_air_*` before `attack*`.
        assert_eq!(group_of("attack_air_n"), MoveGroup::Aerial);
        assert_eq!(group_of("attack_s4_s"), MoveGroup::Smash);
        assert_eq!(group_of("attack_s3_s"), MoveGroup::Tilt);
        assert_eq!(group_of("attack_11"), MoveGroup::Jab);
        assert_eq!(group_of("attack_100"), MoveGroup::Jab);
        // Both spellings are real ParamLabels entries. Matching only the underscored one put
        // every jab in the game into `Other`.
        assert_eq!(group_of("attack11"), MoveGroup::Jab);
        assert_eq!(group_of("attack100end"), MoveGroup::Jab);
        assert_eq!(group_of("attack_dash"), MoveGroup::DashAttack);
        // A copied special before Specials, and Kirby's own before that.
        assert_eq!(group_of("cloud_special_n"), MoveGroup::CopySpecial);
        assert_eq!(group_of("cloud_special_air_n"), MoveGroup::CopySpecial);
        assert_eq!(group_of("special_n_start"), MoveGroup::Special);
        // An item's flag-hoisting "special" is not a copy ability.
        assert_eq!(group_of("item_special_flag_hoist"), MoveGroup::Item);
        // Weapon movesets before `item_`, and before Movement for the `_dash` ones.
        assert_eq!(group_of("bat_swing_dash"), MoveGroup::Item);
        assert_eq!(group_of("scope_air_rapid_empty_fly2"), MoveGroup::Item);
        assert_eq!(group_of("shoot_legs_dash_f"), MoveGroup::Item);
        assert_eq!(group_of("item_light_get"), MoveGroup::Item);
        // Matched by shape, not by name. These five are the ones a hand-written weapon list got
        // wrong — `lip_stick`, not `lipstick` — so they are pinned individually.
        assert_eq!(group_of("lip_stick_swing4"), MoveGroup::Item);
        assert_eq!(group_of("fire_bar_swing3"), MoveGroup::Item);
        assert_eq!(group_of("kill_sword_swing1_common"), MoveGroup::Item);
        assert_eq!(group_of("sword_swing_dash"), MoveGroup::Item);
        assert_eq!(group_of("death_scythe_swing4_charge"), MoveGroup::Item);
        // Movement, including the brake forms that end in a side.
        assert_eq!(group_of("walk_middle"), MoveGroup::Movement);
        assert_eq!(group_of("run_brake_r"), MoveGroup::Movement);
        assert_eq!(group_of("turn_run_brake"), MoveGroup::Movement);
        // The families D1 actually needs.
        assert_eq!(group_of("landing_air_f"), MoveGroup::JumpLand);
        assert_eq!(group_of("down_bound_d"), MoveGroup::Damage);
        assert_eq!(group_of("passive_wall"), MoveGroup::Damage);
        assert_eq!(group_of("appeal_s_l"), MoveGroup::Presentation);
        assert_eq!(group_of("entry_l"), MoveGroup::Presentation);
        assert_eq!(group_of("swim_drown"), MoveGroup::Situational);
        assert_eq!(group_of("squat_wait_item"), MoveGroup::Idle);
    }

    /// Case and whitespace do not decide a group.
    ///
    /// Move names reach this from two places — `ParamLabels.csv` and a live capture's motion hash
    /// resolution — and they have disagreed on case before.
    #[test]
    fn grouping_is_case_insensitive() {
        assert_eq!(group_of("AttackAirN"), group_of("attackairn"));
        assert_eq!(group_of("  walk_middle  "), MoveGroup::Movement);
    }

    /// An unresolved hash is not silently swallowed.
    ///
    /// A motion whose label the dump does not carry shows as `0x…`, and it must still appear in
    /// the list. Hiding it is exactly the failure this module was written to undo.
    #[test]
    fn an_unnamed_motion_still_gets_a_group() {
        assert_eq!(group_of("0x00000b1a8c28e7"), MoveGroup::Other);
    }

    #[test]
    fn every_move_list_section_has_explanatory_hover_text() {
        for group in MoveGroup::ORDER
            .into_iter()
            .chain(std::iter::once(MoveGroup::Other))
        {
            assert!(!group.label().trim().is_empty(), "{group:?} has no label");
            assert!(
                group.description().split_whitespace().count() >= 4,
                "{} does not explain its contents",
                group.label()
            );
        }
    }

    /// Every distinct move name in the corpus lands in a group, and few land in `Other`.
    ///
    /// **The load-bearing check, and the one whose absence would be invisible.** A categoriser
    /// that dumps everything into `Other` still produces a complete, correctly-ordered,
    /// non-crashing list — it is simply useless, and every example test above would still pass.
    /// So this asserts a *proportion*, over real names rather than chosen ones.
    #[test]
    fn every_corpus_move_name_lands_in_a_group() {
        let cache = match dirs::cache_dir() {
            Some(d) => d.join("visionary/script-cache"),
            None => return,
        };
        if !cache.is_dir() {
            return;
        }
        let mut names: std::collections::BTreeSet<String> = Default::default();
        for entry in walk(&cache) {
            let Some(stem) = entry.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            names.insert(snake(stem));
        }
        // Guard against the whole test passing on an empty corpus, which is how a check like
        // this rots: the directory moves, the set is empty, and 0 of 0 is 100%.
        assert!(
            names.len() > 300,
            "corpus too thin to mean anything: {} names",
            names.len()
        );

        let other: Vec<&String> = names
            .iter()
            .filter(|n| group_of(n) == MoveGroup::Other)
            .collect();
        let pct = 100 * other.len() / names.len();
        assert!(
            pct <= 3,
            "{}% of {} corpus move names are uncategorised ({} of them), e.g. {:?}",
            pct,
            names.len(),
            other.len(),
            other.iter().take(20).collect::<Vec<_>>()
        );

        // And the groups that D1 exists for are genuinely populated — a rule that matched
        // nothing would leave the percentage fine and the section empty.
        for group in [
            MoveGroup::Movement,
            MoveGroup::JumpLand,
            MoveGroup::Damage,
            MoveGroup::Presentation,
            MoveGroup::Item,
            MoveGroup::Smash,
            MoveGroup::Jab,
        ] {
            let n = names.iter().filter(|m| group_of(m) == group).count();
            assert!(n >= 5, "{:?} only matched {n} corpus names", group);
        }
    }

    fn snake(stem: &str) -> String {
        let mut out = String::new();
        for (i, ch) in stem.chars().enumerate() {
            if ch.is_uppercase() && i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        }
        out
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else if path.extension().is_some_and(|e| e == "txt") {
                out.push(path);
            }
        }
        out
    }
}
