//! Roster: everything about a fighter that exists above the level of a single move.
//!
//! The move editor in `app.rs` answers "what does this attack do". This module answers
//! "who is on the character select screen, where do they sit, what are they made of, and
//! what are their fighter-wide values". See `docs/roster/PLAN.md` for the design and
//! `docs/roster/TODO.md` for the task board.
//!
//! Two rules hold this module together, and both exist because the alternative drifts
//! silently:
//!
//!  * **The index is derived; the project is authored.** [`RosterIndex`] is rebuilt from
//!    the data root, the enabled mod library, and the open project. No UI mutates an
//!    index — a roster edit writes to the project and the index is rebuilt. This is the
//!    same discipline `AppState.sounds` follows against `sound_script`.
//!  * **Backing-agnostic.** A roster entry is an opaque id that resolves to a
//!    [`RosterBacking`]. Today the only way to add a character is a slot-backed clone of a
//!    donor fighter; a future real fighter ID must be addable as another variant without
//!    editing the CSS or trait editors.

pub mod archive;
pub mod css;
pub mod css_view;
pub mod export;
pub mod gamma;
pub mod icons;
pub mod index;
pub mod library;
pub mod names;
pub mod new_character;
pub mod reveal;
pub mod scaffold;
pub mod traits;
pub mod traits_view;
pub mod ui_images;
pub mod window;

/// Scroll source for areas hosting drag gestures (grid reorder, marquee
/// select). Content-drag scrolling is off so drags belong to the content;
/// wheel and scroll bars still scroll. egui defaults this to ALL, and an
/// always-too-large scroll area then steals reorder drags, marquee drags,
/// and any click with a little movement in it.
pub const GESTURE_AREA_SCROLL: egui::containers::scroll_area::ScrollSource =
    egui::containers::scroll_area::ScrollSource {
        scroll_bar: true,
        drag: false,
        mouse_wheel: true,
    };

use std::fmt;

/// Identifies an entry for the duration of one index build.
///
/// Deliberately *not* stable across rebuilds: the index is derived, so an id is a handle
/// into the current view and nothing may persist one. Anything that has to survive a
/// rescan — a project override, a saved selection — keys on [`RosterKey`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RosterEntryId(pub u32);

/// The stable identity of a roster entry, as written into a saved project.
///
/// Spelled `"mario"` for a whole fighter, `"mario#c08"` for a slot-backed clone, and
/// `"ui:ptrainer"` for a select screen row with no fighter behind it. The third form is not
/// hypothetical: Pokémon Trainer is on the roster with `fighter_kind = 0`, and the Random slot
/// and every boss are rows too, so a key space built only on fighters cannot name them.
///
/// A string rather than an enum because it is a serialized map key in `modproject.json`, and a
/// string key round-trips through JSON without the tuple-key encoding serde would otherwise
/// need. Parsing back into its parts is [`RosterKey::backing_parts`].
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct RosterKey(String);

impl RosterKey {
    /// The key for a whole fighter — vanilla characters and whole-fighter mods.
    pub fn fighter(name: &str) -> Self {
        Self(name.to_ascii_lowercase())
    }

    /// The key for one costume slot promoted to its own CSS cell.
    pub fn slot(donor: &str, slot: u8) -> Self {
        Self(format!("{}#c{:02}", donor.to_ascii_lowercase(), slot))
    }

    /// The key's serialized spelling — what a saved project stores and what [`fmt::Display`]
    /// writes.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The key for a select screen row that no fighter directory backs.
    pub fn chara(name_id: &str) -> Self {
        Self(format!("ui:{}", name_id.to_ascii_lowercase()))
    }
}

impl fmt::Display for RosterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a roster entry is physically made of.
///
/// `NewFighterId` is deliberately absent. Adding it later must not require edits to
/// `css.rs`, `traits.rs`, or their views; `R-73` on the task board is the check for that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterBacking {
    /// The whole fighter is the entry: every vanilla character, and mods that replace or
    /// add a complete `fighter/<name>/` tree.
    Fighter,
    /// One costume slot of a donor fighter, promoted to its own CSS cell. The entry plays
    /// as `donor` with `slot` forced, and carries its own model, motion, effects, and
    /// slot-gated ACMD.
    SlotClone { donor: String, slot: u8 },
}

impl RosterBacking {
    /// True when this entry shares its engine fighter with another entry.
    ///
    /// The question every caller outside this module actually has, phrased so a new backing
    /// answers it by implementing this rather than by being added to a `match` somewhere that
    /// would otherwise keep compiling with the wrong answer. A shared-fighter entry cannot be
    /// a donor for a new character, and its fighter-wide values belong to the fighter it
    /// shares rather than to it.
    pub fn shares_engine_fighter(&self) -> bool {
        match self {
            Self::Fighter => false,
            Self::SlotClone { .. } => true,
        }
    }

    /// The costume slot this entry is pinned to, if any.
    pub fn slot(&self) -> Option<u8> {
        match self {
            Self::Fighter => None,
            Self::SlotClone { slot, .. } => Some(*slot),
        }
    }
}

/// Where an entry came from. Drives how the CSS preview marks it and what may be edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryOrigin {
    /// Present in the base game data root.
    Vanilla,
    /// Contributed by an enabled mod in the library.
    Imported,
    /// Created by the open project through the new-entry wizard.
    Authored,
}

impl EntryOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vanilla => "Vanilla",
            Self::Imported => "Imported",
            Self::Authored => "Authored",
        }
    }
}

/// One character select screen cell, as the index resolved it.
///
/// The select screen and the fighter list are not the same set, and this type has to represent
/// both halves of the mismatch: a row with no fighter behind it (Pokémon Trainer, the Random
/// slot, every boss) and a fighter with no row (a mod that adds `fighter/<name>/` but ships no
/// roster database entry, which is the normal state of a slot-add mod). Making either half
/// unrepresentable would mean quietly dropping entries the user can see in the game.
#[derive(Debug, Clone)]
pub struct RosterEntry {
    pub id: RosterEntryId,
    pub key: RosterKey,
    /// The `ui_chara_db` row this entry corresponds to, when there is one.
    pub name_id: Option<String>,
    pub backing: RosterBacking,
    /// The engine fighter this entry plays as. `None` for a select screen row with no fighter
    /// directory behind it. For a slot clone this is the donor, which is the whole point: the
    /// engine never sees a new character.
    pub fighter: Option<String>,
    pub display_name: String,
    /// Select screen position, from `disp_order`. `None` when no roster database is loaded —
    /// an unknown position must read as unknown rather than defaulting to zero and silently
    /// claiming the first cell.
    pub css_order: Option<i8>,
    /// Which library mods contribute files to this entry, in load order.
    pub providers: Vec<library::ProviderId>,
    pub origin: EntryOrigin,
    /// Hidden from the select screen by the open project.
    pub hidden: bool,
    /// True when the roster database says this row occupies a select screen cell.
    pub on_roster: bool,
}

#[cfg(test)]
mod key_tests {
    use super::*;

    /// The spelling is a serialized map key in every saved project. Changing it silently
    /// orphans every override stored against an entry, which reads as the edits vanishing.
    #[test]
    fn keys_have_a_stable_spelling_per_kind() {
        assert_eq!(RosterKey::fighter("Mario").as_str(), "mario");
        assert_eq!(RosterKey::slot("Mario", 8).as_str(), "mario#c08");
        assert_eq!(RosterKey::chara("PTrainer").as_str(), "ui:ptrainer");
        // Display and the accessor are one spelling, not two that can drift.
        assert_eq!(RosterKey::slot("Mario", 8).to_string(), "mario#c08");
    }

    /// The three key spaces overlap in the names they carry. If they shared a spelling, a
    /// select-screen row and a fighter of the same name would silently be one entry.
    #[test]
    fn the_three_key_spaces_never_collide() {
        let keys = [
            RosterKey::fighter("ptrainer"),
            RosterKey::slot("ptrainer", 0),
            RosterKey::chara("ptrainer"),
        ];
        let unique: std::collections::BTreeSet<&RosterKey> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    /// Slots run well past c07; a format that wrapped or dropped a digit would collide keys
    /// between costumes that are nothing to do with each other.
    #[test]
    fn high_slots_keep_distinct_keys() {
        assert_eq!(RosterKey::slot("mario", 112).as_str(), "mario#c112");
        assert_ne!(RosterKey::slot("mario", 1), RosterKey::slot("mario", 11));
    }

    #[test]
    fn keys_round_trip_through_serialization() {
        for key in [
            RosterKey::fighter("mario"),
            RosterKey::slot("mario", 8),
            RosterKey::chara("ptrainer"),
        ] {
            let json = serde_json::to_string(&key).unwrap();
            assert_eq!(serde_json::from_str::<RosterKey>(&json).unwrap(), key);
        }
    }
}

/// Adding a backing must not mean editing the views.
///
/// The character select editor, the trait editor, and the new-character wizard all ask
/// questions about an entry's backing. If any of them answered by matching on the variants,
/// a new backing would compile everywhere and be silently wrong in each — a shared-fighter
/// entry offered as a donor, a fighter-wide value edited without the warning that it is
/// shared. These pin the predicates they ask through instead.
#[cfg(test)]
mod backing_agnostic_tests {
    use super::*;

    #[test]
    fn a_whole_fighter_owns_its_engine_fighter_and_a_costume_shares_one() {
        assert!(!RosterBacking::Fighter.shares_engine_fighter());
        assert!(RosterBacking::SlotClone {
            donor: "mario".into(),
            slot: 8
        }
        .shares_engine_fighter());
    }

    /// The views ask for a slot rather than matching on the backing, so a backing with no slot
    /// has to answer `None` rather than being unrepresentable.
    #[test]
    fn every_backing_answers_the_slot_question() {
        assert_eq!(RosterBacking::Fighter.slot(), None);
        assert_eq!(
            RosterBacking::SlotClone {
                donor: "mario".into(),
                slot: 8
            }
            .slot(),
            Some(8)
        );
    }
}
