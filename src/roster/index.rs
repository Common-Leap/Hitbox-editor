//! Building the roster: one derived view over the data root, the mod library, and the project.
//!
//! [`RosterIndex::build`] is a pure function of its three inputs. Nothing here does I/O beyond
//! reading manifests that were already scanned, and nothing mutates an index — a roster edit
//! writes to the project and the index is rebuilt. That rule is what keeps the character
//! select preview from drifting away from what an export would actually produce.

use std::collections::BTreeMap;

use crate::data::{FighterEntry, FighterSource};
use crate::mod_project::RosterMod;

use super::css::{CharaDb, CharaRow};
use super::library::ModLibrary;
use super::{EntryOrigin, RosterBacking, RosterEntry, RosterEntryId, RosterKey};

/// The roster as it currently resolves.
#[derive(Debug, Clone, Default)]
pub struct RosterIndex {
    /// Every entry, in character select order — see [`RosterIndex::sorted`].
    pub entries: Vec<RosterEntry>,
    /// Overrides in the project that name an entry nothing currently provides.
    ///
    /// Reported rather than dropped. A project that reopens after a mod was disabled must say
    /// which of its edits no longer have a target; silently discarding them means the user
    /// re-enables the mod later and finds their work gone.
    pub stale_overrides: Vec<StaleOverride>,
}

/// A project override with no entry behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleOverride {
    pub key: RosterKey,
    pub kind: StaleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleKind {
    Order,
    Name,
    NameVariant,
    PerCostumeName,
    Hidden,
    Authored,
    UiImage,
    CharaPatch,
}

impl StaleKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Order => "a character select position",
            Self::Name => "a display name",
            Self::NameVariant => "a detailed name variant",
            Self::PerCostumeName => "a per-costume name",
            Self::Hidden => "a hidden entry",
            Self::Authored => "an authored character",
            Self::UiImage => "a UI portrait override",
            Self::CharaPatch => "a ui_chara_db patch",
        }
    }
}

impl RosterIndex {
    /// Resolve the roster from the indexed fighters, the enabled mod library, the project, and
    /// the roster database when one is available.
    ///
    /// `fighters` is the existing fighter scan — the data root first, then enabled mod roots in
    /// load order — so a fighter provided only by a mod is already present there.
    ///
    /// The select screen and the fighter list are not the same set, and both mismatches are
    /// real. A row with no fighter behind it (Pokémon Trainer, the Random slot, every boss)
    /// still occupies a cell. A fighter with no row (any slot-add mod) is editable but not on
    /// the select screen. Both become entries; dropping either would hide something the user
    /// can see in the game.
    pub fn build(
        fighters: &[FighterEntry],
        library: &ModLibrary,
        project: &RosterMod,
        db: Option<&CharaDb>,
    ) -> Self {
        let mut entries: Vec<RosterEntry> = Vec::new();
        let mut stale_overrides = Vec::new();
        let mut next_id = 0u32;

        // `fighter_kind` is `hash40("fighter_kind_<name>")`, verified against real data. It is
        // the reliable link between a database row and a fighter directory, because `name_id`
        // is not always the directory name — the Ice Climbers' row is `ice_climber` and their
        // directories are `popo` and `nana`.
        let by_kind: BTreeMap<u64, &FighterEntry> = fighters
            .iter()
            .map(|fighter| (super::css::fighter_kind_hash(&fighter.name), fighter))
            .collect();

        let mut claimed: std::collections::BTreeSet<&str> = Default::default();
        let authored_by_name: BTreeMap<&str, &crate::mod_project::AuthoredEntry> = project
            .authored
            .iter()
            .map(|authored| (authored.name_id.as_str(), authored))
            .collect();

        // Select screen rows first, so the roster reads in the game's own order.
        for row in db.map(CharaDb::entries).unwrap_or_default() {
            let fighter = by_kind.get(&row.fighter_kind).copied();
            if let Some(fighter) = fighter {
                claimed.insert(fighter.name.as_str());
            }
            let authored = authored_by_name.get(row.name_id.as_str()).copied();
            let key = match (authored, fighter) {
                (Some(authored), _) => authored.key.clone(),
                (None, Some(fighter)) => RosterKey::fighter(&fighter.name),
                (None, None) => RosterKey::chara(&row.name_id),
            };
            let backing = match authored {
                Some(authored) => RosterBacking::SlotClone {
                    donor: authored.donor.clone(),
                    slot: authored.slot,
                },
                None => RosterBacking::Fighter,
            };
            let origin = match (authored, fighter) {
                (Some(_), _) => EntryOrigin::Authored,
                (None, Some(fighter)) => origin_of(fighter),
                // A row with no fighter directory is part of the base database.
                (None, None) => EntryOrigin::Vanilla,
            };
            entries.push(RosterEntry {
                id: allocate(&mut next_id),
                display_name: project
                    .names
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| display_name_for(row, fighter, authored)),
                css_order: Some(project.order.get(&key).copied().unwrap_or(row.disp_order)),
                providers: fighter
                    .map(|fighter| library.providers_for_fighter(&fighter.name))
                    .unwrap_or_default(),
                hidden: project.hidden.contains(&key),
                on_roster: row.on_roster(),
                name_id: Some(row.name_id.clone()),
                fighter: fighter.map(|fighter| fighter.name.clone()),
                backing,
                origin,
                key,
            });
        }

        // Then fighters no row claimed. With no database loaded that is every fighter, which
        // is the right answer: nothing is known about select screen positions yet.
        for fighter in fighters {
            if claimed.contains(fighter.name.as_str()) {
                continue;
            }
            let key = RosterKey::fighter(&fighter.name);
            entries.push(RosterEntry {
                id: allocate(&mut next_id),
                display_name: project
                    .names
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| fighter.display_name.clone()),
                css_order: project.order.get(&key).copied(),
                providers: library.providers_for_fighter(&fighter.name),
                origin: origin_of(fighter),
                hidden: project.hidden.contains(&key),
                on_roster: false,
                name_id: None,
                fighter: Some(fighter.name.clone()),
                backing: RosterBacking::Fighter,
                key,
            });
        }

        // Authored entries whose row is not in the database yet — the state between creating
        // an entry and exporting it — provided their donor is present. An entry backed by a
        // fighter this data root does not have can be neither edited nor exported, and showing
        // it would invite both.
        let known: std::collections::BTreeSet<&str> = fighters
            .iter()
            .map(|fighter| fighter.name.as_str())
            .collect();
        let present_rows: std::collections::BTreeSet<String> = entries
            .iter()
            .filter_map(|entry| entry.name_id.clone())
            .collect();
        for authored in &project.authored {
            if present_rows.contains(&authored.name_id) {
                continue;
            }
            if !known.contains(authored.donor.as_str()) {
                stale_overrides.push(StaleOverride {
                    key: authored.key.clone(),
                    kind: StaleKind::Authored,
                });
                continue;
            }
            let key = authored.key.clone();
            entries.push(RosterEntry {
                id: allocate(&mut next_id),
                display_name: project
                    .names
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| authored.display_name.clone()),
                css_order: project.order.get(&key).copied(),
                providers: library.providers_for_fighter(&authored.donor),
                origin: EntryOrigin::Authored,
                hidden: project.hidden.contains(&key),
                on_roster: false,
                name_id: Some(authored.name_id.clone()),
                fighter: Some(authored.donor.clone()),
                backing: RosterBacking::SlotClone {
                    donor: authored.donor.clone(),
                    slot: authored.slot,
                },
                key,
            });
        }

        let present: std::collections::BTreeSet<&RosterKey> =
            entries.iter().map(|entry| &entry.key).collect();
        for (key, kind) in project
            .order
            .keys()
            .map(|key| (key, StaleKind::Order))
            .chain(project.names.keys().map(|key| (key, StaleKind::Name)))
            .chain(
                project
                    .name_variants
                    .keys()
                    .map(|key| (key, StaleKind::NameVariant)),
            )
            .chain(project.hidden.iter().map(|key| (key, StaleKind::Hidden)))
            .chain(
                project
                    .ui_images
                    .keys()
                    .map(|key| (key, StaleKind::UiImage)),
            )
            .chain(
                project
                    .chara_overrides
                    .keys()
                    .map(|key| (key, StaleKind::CharaPatch)),
            )
        {
            if !present.contains(key) {
                stale_overrides.push(StaleOverride {
                    key: key.clone(),
                    kind,
                });
            }
        }
        // Per-costume names are keyed by fighter string, not RosterKey — they are stale when no entry for that fighter exists.
        for fighter in project.per_costume_names.keys() {
            let has_fighter = entries.iter().any(|e| {
                e.fighter
                    .as_deref()
                    .map(|f| f.eq_ignore_ascii_case(fighter))
                    .unwrap_or(false)
            });
            if !has_fighter {
                // Use a synthetic key for reporting; keep it distinct.
                stale_overrides.push(StaleOverride {
                    key: crate::roster::RosterKey::fighter(fighter),
                    kind: StaleKind::PerCostumeName,
                });
            }
        }
        stale_overrides.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then(a.kind.describe().cmp(b.kind.describe()))
        });
        stale_overrides.dedup();

        Self {
            entries,
            stale_overrides,
        }
    }

    pub fn by_key(&self, key: &RosterKey) -> Option<&RosterEntry> {
        self.entries.iter().find(|entry| &entry.key == key)
    }

    /// Entries in select screen order.
    ///
    /// Entries with a position come first in that order, ties keeping their database order
    /// behind the shared value — two entries sharing a position share a cell, as Pyra and
    /// Mythra do. Entries with no position follow: a fighter whose position is unknown must
    /// not silently claim the first cell.
    pub fn sorted(&self) -> Vec<&RosterEntry> {
        let mut ordered: Vec<&RosterEntry> = self.entries.iter().collect();
        ordered.sort_by_key(|entry| {
            (
                entry.css_order.is_none(),
                entry.css_order.unwrap_or(0),
                entry.id.0,
            )
        });
        ordered
    }

    /// Entries that occupy a select screen cell, in the order the game shows them.
    pub fn visible(&self) -> Vec<&RosterEntry> {
        self.sorted()
            .into_iter()
            .filter(|entry| entry.on_roster && !entry.hidden)
            .collect()
    }

    /// Entries that are editable but not on the select screen — the normal state of a
    /// slot-add mod, and of an authored entry that has not been exported yet.
    pub fn off_roster(&self) -> Vec<&RosterEntry> {
        self.sorted()
            .into_iter()
            .filter(|entry| !entry.on_roster || entry.hidden)
            .collect()
    }
}

fn allocate(next: &mut u32) -> RosterEntryId {
    let id = RosterEntryId(*next);
    *next += 1;
    id
}

/// The best available name for a row.
///
/// The fighter's own display name is preferred over the raw `name_id` when there is a fighter,
/// because `name_id` is an internal spelling — `purin`, `gekkouga`, `szerosuit`. Display names
/// proper come from `ui/message` and land in R-29; until then this is honest about being a
/// fallback rather than inventing one.
fn display_name_for(
    row: &CharaRow,
    fighter: Option<&FighterEntry>,
    authored: Option<&crate::mod_project::AuthoredEntry>,
) -> String {
    if let Some(authored) = authored {
        return authored.display_name.clone();
    }
    match fighter {
        Some(fighter) => fighter.display_name.clone(),
        None => row.name_id.clone(),
    }
}

fn origin_of(fighter: &FighterEntry) -> EntryOrigin {
    match fighter.source {
        FighterSource::ModRoot => EntryOrigin::Imported,
        // A fighter found in the data root is vanilla unless its name is not one — a user who
        // installed an added character directly into their dump has an imported fighter with
        // no mod root behind it.
        FighterSource::DataRoot if fighter.is_modded() => EntryOrigin::Imported,
        FighterSource::DataRoot => EntryOrigin::Vanilla,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mod_project::AuthoredEntry;
    use crate::roster::css::{fighter_kind_hash, test_db};
    use std::path::PathBuf;

    fn fighter(name: &str, source: FighterSource) -> FighterEntry {
        FighterEntry {
            name: name.to_string(),
            display_name: name.to_uppercase(),
            param_path: PathBuf::new(),
            motion_dir: PathBuf::new(),
            model_dir: PathBuf::new(),
            effect_dir: None,
            slots: vec![0],
            fighter_dir: PathBuf::new(),
            source,
        }
    }

    fn build(fighters: &[FighterEntry], project: &RosterMod, db: Option<&CharaDb>) -> RosterIndex {
        RosterIndex::build(fighters, &ModLibrary::default(), project, db)
    }

    #[test]
    fn with_no_database_every_fighter_is_an_entry_with_no_position() {
        let fighters = vec![
            fighter("mario", FighterSource::DataRoot),
            fighter("mychar", FighterSource::ModRoot),
        ];
        let index = build(&fighters, &RosterMod::default(), None);
        assert_eq!(index.entries.len(), 2);
        assert!(index.entries.iter().all(|entry| entry.css_order.is_none()));
        assert!(index.entries.iter().all(|entry| !entry.on_roster));
        assert!(index.visible().is_empty());
        assert_eq!(
            index.by_key(&RosterKey::fighter("mario")).unwrap().origin,
            EntryOrigin::Vanilla
        );
        assert_eq!(
            index.by_key(&RosterKey::fighter("mychar")).unwrap().origin,
            EntryOrigin::Imported
        );
    }

    /// An added character copied straight into the dump has no mod root behind it, but it is
    /// still not vanilla and must not be presented as untouched.
    #[test]
    fn a_non_vanilla_name_in_the_data_root_reads_as_imported() {
        let fighters = vec![fighter("mychar", FighterSource::DataRoot)];
        let index = build(&fighters, &RosterMod::default(), None);
        assert_eq!(index.entries[0].origin, EntryOrigin::Imported);
    }

    /// `name_id` is not always the fighter directory name — the Ice Climbers' row is
    /// `ice_climber` and their directories are `popo` and `nana`. Linking on `name_id` would
    /// leave them permanently unmatched.
    #[test]
    fn rows_link_to_fighters_by_fighter_kind_not_by_name() {
        let fighters = vec![fighter("popo", FighterSource::DataRoot)];
        let db = test_db(&[("ice_climber", 16, fighter_kind_hash("popo"), true)]);
        let index = build(&fighters, &RosterMod::default(), Some(&db));
        assert_eq!(index.entries.len(), 1);
        let entry = &index.entries[0];
        assert_eq!(entry.fighter.as_deref(), Some("popo"));
        assert_eq!(entry.name_id.as_deref(), Some("ice_climber"));
        assert_eq!(entry.key, RosterKey::fighter("popo"));
        assert_eq!(entry.css_order, Some(16));
        assert!(entry.on_roster);
    }

    /// Pokémon Trainer is on the select screen with `fighter_kind = 0`. An index that only
    /// represented fighters would drop a cell the user can see in the game.
    #[test]
    fn a_row_with_no_fighter_behind_it_is_still_an_entry() {
        let db = test_db(&[("ptrainer", 37, 0, true)]);
        let index = build(&[], &RosterMod::default(), Some(&db));
        let entry = &index.entries[0];
        assert_eq!(entry.key, RosterKey::chara("ptrainer"));
        assert_eq!(entry.fighter, None);
        assert!(entry.on_roster);
        assert_eq!(index.visible().len(), 1);
    }

    /// The other half of the mismatch: a slot-add mod ships a fighter directory and no roster
    /// row. It is editable and must appear, but it is not on the select screen.
    #[test]
    fn a_fighter_with_no_row_is_an_entry_that_is_off_roster() {
        let fighters = vec![fighter("mychar", FighterSource::ModRoot)];
        let db = test_db(&[("mario", 0, fighter_kind_hash("mario"), true)]);
        let index = build(&fighters, &RosterMod::default(), Some(&db));
        assert_eq!(index.entries.len(), 2);
        let entry = index.by_key(&RosterKey::fighter("mychar")).unwrap();
        assert!(!entry.on_roster);
        assert_eq!(entry.name_id, None);
        assert_eq!(index.visible().len(), 1);
        assert_eq!(index.off_roster().len(), 1);
    }

    /// Off-roster database rows — bosses, the Random slot, the unselectable Pyra/Mythra
    /// variants — must not be drawn as select screen cells.
    #[test]
    fn off_roster_rows_are_not_visible() {
        let db = test_db(&[
            ("mario", 0, fighter_kind_hash("mario"), true),
            ("random", crate::roster::css::RANDOM_SLOT_ORDER, 0, false),
            ("masterhand", crate::roster::css::OFF_ROSTER, 0, false),
        ]);
        let index = build(&[], &RosterMod::default(), Some(&db));
        assert_eq!(index.entries.len(), 3);
        let visible: Vec<&str> = index
            .visible()
            .iter()
            .filter_map(|entry| entry.name_id.as_deref())
            .collect();
        assert_eq!(visible, vec!["mario"]);
    }

    #[test]
    fn project_overrides_replace_the_database_position_and_name() {
        let fighters = vec![fighter("mario", FighterSource::DataRoot)];
        let db = test_db(&[("mario", 0, fighter_kind_hash("mario"), true)]);
        let key = RosterKey::fighter("mario");
        let mut project = RosterMod::default();
        project.names.insert(key.clone(), "Jumpman".into());
        project.order.insert(key.clone(), 4);
        let index = build(&fighters, &project, Some(&db));
        let entry = index.by_key(&key).unwrap();
        assert_eq!(entry.display_name, "Jumpman");
        assert_eq!(entry.css_order, Some(4));
        assert!(index.stale_overrides.is_empty());
    }

    #[test]
    fn a_hidden_entry_leaves_the_visible_roster() {
        let fighters = vec![fighter("mario", FighterSource::DataRoot)];
        let db = test_db(&[("mario", 0, fighter_kind_hash("mario"), true)]);
        let key = RosterKey::fighter("mario");
        let mut project = RosterMod::default();
        project.hidden.insert(key.clone());
        let index = build(&fighters, &project, Some(&db));
        assert!(index.by_key(&key).unwrap().hidden);
        assert!(index.visible().is_empty());
        assert_eq!(index.off_roster().len(), 1);
    }

    /// The failure this guards is silent: a project reopened after a mod was disabled would
    /// otherwise look clean while its edits had no target left.
    #[test]
    fn overrides_with_no_entry_behind_them_are_reported_not_dropped() {
        let fighters = vec![fighter("mario", FighterSource::DataRoot)];
        let mut project = RosterMod::default();
        project.order.insert(RosterKey::fighter("gone"), 3);
        project.authored.push(AuthoredEntry {
            key: RosterKey::slot("missingdonor", 8),
            donor: "missingdonor".into(),
            slot: 8,
            display_name: "Ghost".into(),
            name_id: "ghost".into(),
            moveset_scaffolded: false,
            files_root: None,
        });
        let index = build(&fighters, &project, None);
        assert_eq!(index.entries.len(), 1);
        let kinds: Vec<StaleKind> = index
            .stale_overrides
            .iter()
            .map(|stale| stale.kind)
            .collect();
        assert!(kinds.contains(&StaleKind::Order));
        assert!(kinds.contains(&StaleKind::Authored));
    }

    /// An authored entry exists before its database row does — that is the state between
    /// creating it and exporting it, and it has to be selectable in that state.
    #[test]
    fn an_authored_entry_appears_before_its_row_exists_and_merges_with_it_after() {
        let fighters = vec![fighter("mario", FighterSource::DataRoot)];
        let key = RosterKey::slot("mario", 8);
        let mut project = RosterMod::default();
        project.authored.push(AuthoredEntry {
            key: key.clone(),
            donor: "mario".into(),
            slot: 8,
            display_name: "Vision".into(),
            name_id: "vision".into(),
            moveset_scaffolded: false,
            files_root: None,
        });

        let before = build(&fighters, &project, None);
        let entry = before.by_key(&key).unwrap();
        assert_eq!(entry.origin, EntryOrigin::Authored);
        assert_eq!(entry.backing.slot(), Some(8));
        assert!(!entry.on_roster);
        assert!(before.stale_overrides.is_empty());

        // Once exported, the row exists and must merge with the authored entry rather than
        // producing a second cell for the same character.
        let db = test_db(&[
            ("mario", 0, fighter_kind_hash("mario"), true),
            ("vision", 87, fighter_kind_hash("mario"), true),
        ]);
        let after = build(&fighters, &project, Some(&db));
        assert_eq!(after.entries.len(), 2, "the authored entry was duplicated");
        let entry = after.by_key(&key).unwrap();
        assert_eq!(entry.origin, EntryOrigin::Authored);
        assert_eq!(entry.css_order, Some(87));
        assert!(entry.on_roster);
    }

    /// An entry whose position is unknown must not sort as though it were first.
    #[test]
    fn unpositioned_entries_sort_after_positioned_ones() {
        let fighters = vec![
            fighter("mario", FighterSource::DataRoot),
            fighter("link", FighterSource::DataRoot),
        ];
        let db = test_db(&[("link", 2, fighter_kind_hash("link"), true)]);
        let index = build(&fighters, &RosterMod::default(), Some(&db));
        let order: Vec<&str> = index
            .sorted()
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(order, vec!["link", "mario"]);
    }
}

/// Reopening a project: an authored character has to come back intact, and its files have to
/// be found again after a rescan that may have moved, disabled, or replaced the mod holding
/// them.
#[cfg(test)]
mod reopen_tests {
    use super::*;
    use crate::data::{FighterEntry, FighterSource};
    use crate::mod_project::{ModProjectFile, PROJECT_VERSION};
    use crate::roster::css::{fighter_kind_hash, test_db};
    use crate::roster::new_character::authored_entry;
    use std::path::PathBuf;

    fn fighter(name: &str) -> FighterEntry {
        FighterEntry {
            name: name.to_string(),
            display_name: name.to_uppercase(),
            param_path: PathBuf::new(),
            motion_dir: PathBuf::new(),
            model_dir: PathBuf::new(),
            effect_dir: None,
            slots: vec![0, 8],
            fighter_dir: PathBuf::new(),
            source: FighterSource::DataRoot,
        }
    }

    fn saved_project() -> ModProjectFile {
        let entry = authored_entry("mario", 8, "Vision", "vision");
        let mut project = ModProjectFile {
            version: PROJECT_VERSION,
            name: "reopen".into(),
            ..Default::default()
        };
        project
            .roster
            .names
            .insert(entry.key.clone(), "Vision".into());
        project.roster.order.insert(entry.key.clone(), 87);
        project.roster.authored.push(entry);
        project
    }

    #[test]
    fn an_authored_character_survives_a_save_and_reload() {
        let saved = saved_project();
        let json = serde_json::to_string(&saved).unwrap();
        let mut reloaded: ModProjectFile = serde_json::from_str(&json).unwrap();
        reloaded.migrate().unwrap();
        assert_eq!(reloaded.roster, saved.roster);

        let index = RosterIndex::build(
            &[fighter("mario")],
            &ModLibrary::default(),
            &reloaded.roster,
            None,
        );
        let entry = index
            .by_key(&crate::roster::RosterKey::slot("mario", 8))
            .expect("the authored character came back");
        assert_eq!(entry.display_name, "Vision");
        assert_eq!(entry.origin, EntryOrigin::Authored);
        assert_eq!(entry.backing.slot(), Some(8));
        assert!(index.stale_overrides.is_empty());
    }

    /// Once exported, the character has a roster row. Reopening must merge the two rather than
    /// showing the character twice — once from the project and once from the database.
    #[test]
    fn reopening_after_an_export_merges_the_entry_with_its_row() {
        let project = saved_project();
        let db = test_db(&[
            ("mario", 0, fighter_kind_hash("mario"), true),
            ("vision", 87, fighter_kind_hash("mario"), true),
        ]);
        let index = RosterIndex::build(
            &[fighter("mario")],
            &ModLibrary::default(),
            &project.roster,
            Some(&db),
        );
        assert_eq!(index.entries.len(), 2, "the character appears twice");
        let entry = index
            .by_key(&crate::roster::RosterKey::slot("mario", 8))
            .unwrap();
        assert_eq!(entry.name_id.as_deref(), Some("vision"));
        assert_eq!(entry.css_order, Some(87));
    }

    /// The donor gone means the mod providing it was disabled or removed. The character has to
    /// be reported, not dropped: dropping it would let the next save write the project without
    /// it, losing work that is merely temporarily unresolvable.
    #[test]
    fn an_authored_character_whose_donor_is_gone_is_reported_and_kept_in_the_project() {
        let project = saved_project();
        let index = RosterIndex::build(&[], &ModLibrary::default(), &project.roster, None);
        assert!(index.entries.is_empty());
        assert!(index
            .stale_overrides
            .iter()
            .any(|stale| stale.kind == StaleKind::Authored));
        // And nothing here has mutated the project.
        assert_eq!(project.roster.authored.len(), 1);
    }
}
