//! The character select screen database: `ui/param/database/ui_chara_db.prc`.
//!
//! What decides a character's position on the select screen is the `disp_order` field of that
//! character's `db_root` entry, sorted ascending — **not** the list order, and not `save_no`.
//! The two `I8` fields agree for 110 of vanilla's 121 entries, which is exactly why it had to
//! be measured; they diverge on Inkling/Ridley/Simon/Richter/K. Rool, and the real select
//! screen follows `disp_order`. See `docs/roster/PLAN.md` for the evidence.
//!
//! Three properties of the field shape everything here:
//!
//!  * `-1` means "not on the select screen", and every vanilla entry carrying it also has
//!    `can_select = false`. The two are written together; setting one alone produces an entry
//!    the game disagrees with itself about.
//!  * `99` is the Random slot. It is a sentinel, not a position, and is never renumbered.
//!  * The value is not unique — Pyra and Mythra share `80`. Entries sharing a value share a
//!    cell, so a reorder moves the whole group.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use prc::ParamKind;

/// Where the roster database lives inside a dumped arc root.
pub const CHARA_DB_PATH: &str = "ui/param/database/ui_chara_db.prc";

/// `disp_order` for an entry that is not on the select screen.
pub const OFF_ROSTER: i8 = -1;

/// `disp_order` of the Random slot. Reserved: it is a sentinel, not a position.
pub const RANDOM_SLOT_ORDER: i8 = 99;

/// Find the arc root that provides the UI database.
///
/// Searched in the same precedence the rest of the tool uses for game files: the data root
/// first, then enabled mod roots in load order, so a mod that ships its own roster database
/// is picked up without a second configuration step. Returns the root, not the file, because
/// portraits and names are looked up relative to the same place.
pub fn locate_ui_root(roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .find(|root| root.join(CHARA_DB_PATH).is_file())
        .cloned()
}

/// The `fighter_kind` value a fighter directory name corresponds to.
///
/// Verified against real data: Mario's row carries `hash40("fighter_kind_mario")`, and the Ice
/// Climbers' carries `hash40("fighter_kind_ice_climber")`. This is the reliable link between a
/// database row and a fighter, because `name_id` is not always the directory name.
pub fn fighter_kind_hash(fighter: &str) -> u64 {
    hash40::hash40(&format!("fighter_kind_{}", fighter.to_ascii_lowercase())).0
}

/// One character select entry, read from a `db_root` element.
///
/// Only the fields the roster editor reasons about are lifted out. The full struct stays in
/// [`CharaDb::root`] and is what gets written back, so a field this type does not know about
/// survives a load-and-save untouched.
#[derive(Debug, Clone, PartialEq)]
pub struct CharaRow {
    /// Index into `db_root`. Stable for one loaded file; not the select screen position.
    pub index: usize,
    pub name_id: String,
    pub ui_chara_id: u64,
    pub fighter_kind: u64,
    pub disp_order: i8,
    pub save_no: i8,
    pub can_select: bool,
    /// Number of costume slots the select screen offers for this character.
    pub color_num: u8,
}

impl CharaRow {
    /// True when this entry occupies a cell on the select screen.
    pub fn on_roster(&self) -> bool {
        self.disp_order != OFF_ROSTER && self.disp_order != RANDOM_SLOT_ORDER && self.can_select
    }
}

/// A loaded `ui_chara_db.prc`.
pub struct CharaDb {
    /// The whole parsed file. Edits mutate this, and it is what [`CharaDb::save`] writes, so
    /// every field the editor does not model round-trips untouched.
    root: prc::ParamStruct,
    /// The lifted view, rebuilt after every edit rather than mutated alongside the tree —
    /// the tree is the model, this is a view of it.
    entries: Vec<CharaRow>,
}

impl CharaDb {
    pub fn open(path: &Path) -> Result<Self> {
        let root = prc::open(path)
            .map_err(|error| anyhow::anyhow!("{error:?}"))
            .with_context(|| format!("reading {}", path.display()))?;
        let mut db = Self {
            root,
            entries: Vec::new(),
        };
        db.refresh();
        if db.entries.is_empty() {
            anyhow::bail!(
                "{} has no db_root entries — is this a ui_chara_db?",
                path.display()
            );
        }
        Ok(db)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        prc::save(path, &self.root)
            .map_err(|error| anyhow::anyhow!("{error:?}"))
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn entries(&self) -> &[CharaRow] {
        &self.entries
    }

    pub fn row(&self, name_id: &str) -> Option<&CharaRow> {
        self.entries.iter().find(|row| row.name_id == name_id)
    }

    /// Set a character's select screen position.
    ///
    /// Writing [`OFF_ROSTER`] also clears `can_select`, and writing a real position sets it:
    /// vanilla never disagrees between the two, and an entry that does is one the game will
    /// render but refuse to pick, or list as pickable at a position that does not exist.
    pub fn set_disp_order(&mut self, name_id: &str, order: i8) -> Result<()> {
        let can_select = order != OFF_ROSTER;
        self.set_field(name_id, "disp_order", ParamKind::I8(order))?;
        self.set_field(name_id, "can_select", ParamKind::Bool(can_select))?;
        self.refresh();
        Ok(())
    }

    /// Apply a complete desired ordering, given as `name_id → position`.
    ///
    /// Applied as one pass so that a swap cannot transiently collide, and returns the names it
    /// could not find rather than skipping them silently — a project override naming a
    /// character this database does not contain is exactly the stale-override case the roster
    /// index reports, and the export path has to be able to report it too.
    pub fn apply_order(&mut self, order: &BTreeMap<String, i8>) -> Vec<String> {
        let mut missing = Vec::new();
        for (name_id, position) in order {
            if self.set_disp_order(name_id, *position).is_err() {
                missing.push(name_id.clone());
            }
        }
        missing
    }

    /// Apply per-entry `ui_chara_db` patches (color_num, save_no, ...).
    pub fn apply_chara_patches(
        &mut self,
        patches: &BTreeMap<String, crate::mod_project::CharaOverrides>,
    ) -> Vec<String> {
        let mut missing = Vec::new();
        for (name_id, patch) in patches {
            if let Some(n) = patch.color_num {
                if self.set_field(name_id, "color_num", ParamKind::U8(n)).is_err() {
                    missing.push(name_id.clone());
                    continue;
                }
            }
            if let Some(n) = patch.save_no {
                if self.set_field(name_id, "save_no", ParamKind::I8(n)).is_err() {
                    if !missing.contains(name_id) {
                        missing.push(name_id.clone());
                    }
                    continue;
                }
            }
        }
        if !missing.is_empty() {
            self.refresh();
        } else if !patches.is_empty() {
            self.refresh();
        }
        missing
    }

    fn set_field(&mut self, name_id: &str, field: &str, value: ParamKind) -> Result<()> {
        let index = self
            .entries
            .iter()
            .find(|row| row.name_id == name_id)
            .map(|row| row.index)
            .with_context(|| format!("no character select entry named {name_id}"))?;
        let field_hash = hash40::hash40(field).0;
        let Some(ParamKind::List(list)) = db_root_mut(&mut self.root) else {
            anyhow::bail!("db_root is missing");
        };
        let Some(ParamKind::Struct(entry)) = list.0.get_mut(index) else {
            anyhow::bail!("db_root entry {index} is not a struct");
        };
        for (hash, slot) in entry.0.iter_mut() {
            if hash.0 == field_hash {
                *slot = value;
                return Ok(());
            }
        }
        anyhow::bail!("entry {name_id} has no {field} field")
    }

    /// Rebuild the lifted view from the tree.
    fn refresh(&mut self) {
        self.entries.clear();
        let Some(ParamKind::List(list)) = db_root(&self.root) else {
            return;
        };
        for (index, item) in list.0.iter().enumerate() {
            let ParamKind::Struct(entry) = item else {
                continue;
            };
            let Some(name_id) = string_field(entry, "name_id") else {
                continue;
            };
            self.entries.push(CharaRow {
                index,
                name_id,
                ui_chara_id: hash_field(entry, "ui_chara_id").unwrap_or(0),
                fighter_kind: hash_field(entry, "fighter_kind").unwrap_or(0),
                disp_order: i8_field(entry, "disp_order").unwrap_or(OFF_ROSTER),
                save_no: i8_field(entry, "save_no").unwrap_or(OFF_ROSTER),
                can_select: bool_field(entry, "can_select").unwrap_or(false),
                color_num: u8_field(entry, "color_num").unwrap_or(0),
            });
        }
    }
}

fn db_root(root: &prc::ParamStruct) -> Option<&ParamKind> {
    let wanted = hash40::hash40("db_root").0;
    root.0
        .iter()
        .find(|(hash, _)| hash.0 == wanted)
        .map(|(_, value)| value)
}

fn db_root_mut(root: &mut prc::ParamStruct) -> Option<&mut ParamKind> {
    let wanted = hash40::hash40("db_root").0;
    root.0
        .iter_mut()
        .find(|(hash, _)| hash.0 == wanted)
        .map(|(_, value)| value)
}

fn field<'a>(entry: &'a prc::ParamStruct, name: &str) -> Option<&'a ParamKind> {
    let wanted = hash40::hash40(name).0;
    entry
        .0
        .iter()
        .find(|(hash, _)| hash.0 == wanted)
        .map(|(_, value)| value)
}

fn string_field(entry: &prc::ParamStruct, name: &str) -> Option<String> {
    match field(entry, name)? {
        ParamKind::Str(value) => Some(value.clone()),
        _ => None,
    }
}

fn hash_field(entry: &prc::ParamStruct, name: &str) -> Option<u64> {
    match field(entry, name)? {
        ParamKind::Hash(value) => Some(value.0),
        _ => None,
    }
}

fn i8_field(entry: &prc::ParamStruct, name: &str) -> Option<i8> {
    match field(entry, name)? {
        ParamKind::I8(value) => Some(*value),
        _ => None,
    }
}

fn u8_field(entry: &prc::ParamStruct, name: &str) -> Option<u8> {
    match field(entry, name)? {
        ParamKind::U8(value) => Some(*value),
        _ => None,
    }
}

fn bool_field(entry: &prc::ParamStruct, name: &str) -> Option<bool> {
    match field(entry, name)? {
        ParamKind::Bool(value) => Some(*value),
        _ => None,
    }
}

/// Build a database in memory from `(name_id, disp_order, fighter_kind, can_select)`.
///
/// Test-only, and shared with `index.rs` so the roster index is exercised against the same
/// row model the real file produces rather than a second hand-written stand-in.
#[cfg(test)]
pub(crate) fn test_db(rows: &[(&str, i8, u64, bool)]) -> CharaDb {
    use prc::hash40::Hash40;
    use prc::{ParamList, ParamStruct};

    let entries = rows
        .iter()
        .map(|(name_id, disp_order, fighter_kind, can_select)| {
            ParamKind::Struct(ParamStruct(vec![
                (
                    Hash40(hash40::hash40("name_id").0),
                    ParamKind::Str((*name_id).into()),
                ),
                (
                    Hash40(hash40::hash40("ui_chara_id").0),
                    ParamKind::Hash(Hash40(hash40::hash40(&format!("ui_chara_{name_id}")).0)),
                ),
                (
                    Hash40(hash40::hash40("fighter_kind").0),
                    ParamKind::Hash(Hash40(*fighter_kind)),
                ),
                (
                    Hash40(hash40::hash40("disp_order").0),
                    ParamKind::I8(*disp_order),
                ),
                (Hash40(hash40::hash40("save_no").0), ParamKind::I8(0)),
                (
                    Hash40(hash40::hash40("can_select").0),
                    ParamKind::Bool(*can_select),
                ),
                (Hash40(hash40::hash40("color_num").0), ParamKind::U8(8)),
            ]))
        })
        .collect();
    let mut db = CharaDb {
        root: ParamStruct(vec![(
            Hash40(hash40::hash40("db_root").0),
            ParamKind::List(ParamList(entries)),
        )]),
        entries: Vec::new(),
    };
    db.refresh();
    db
}

#[cfg(test)]
mod tests {
    use super::*;
    use prc::hash40::Hash40;
    use prc::{ParamList, ParamStruct};

    fn entry(name: &str, disp_order: i8, save_no: i8, can_select: bool) -> ParamKind {
        ParamKind::Struct(ParamStruct(vec![
            (
                Hash40(hash40::hash40("name_id").0),
                ParamKind::Str(name.into()),
            ),
            (
                Hash40(hash40::hash40("ui_chara_id").0),
                ParamKind::Hash(Hash40(hash40::hash40(name).0)),
            ),
            (
                Hash40(hash40::hash40("fighter_kind").0),
                ParamKind::Hash(Hash40(hash40::hash40(name).0)),
            ),
            (
                Hash40(hash40::hash40("disp_order").0),
                ParamKind::I8(disp_order),
            ),
            (Hash40(hash40::hash40("save_no").0), ParamKind::I8(save_no)),
            (
                Hash40(hash40::hash40("can_select").0),
                ParamKind::Bool(can_select),
            ),
            (Hash40(hash40::hash40("color_num").0), ParamKind::U8(8)),
        ]))
    }

    /// Select screen order, the way the roster index computes it: the rows that occupy a cell,
    /// by position, ties keeping file order because they share a cell.
    fn roster_order(db: &CharaDb) -> Vec<&CharaRow> {
        let mut rows: Vec<&CharaRow> = db.entries().iter().filter(|row| row.on_roster()).collect();
        rows.sort_by_key(|row| (row.disp_order, row.index));
        rows
    }

    /// A stand-in for the real file with the properties that matter: a Random sentinel, an
    /// off-roster entry, a shared position, and a `save_no` that disagrees with `disp_order`.
    fn db() -> CharaDb {
        let root = ParamStruct(vec![(
            Hash40(hash40::hash40("db_root").0),
            ParamKind::List(ParamList(vec![
                entry("random", RANDOM_SLOT_ORDER, -1, false),
                entry("mario", 0, 0, true),
                entry("richter", 67, 64, true),
                entry("inkling", 64, 65, true),
                entry("simon", 66, 68, true),
                entry("eflame_first", 80, 83, true),
                entry("elight_first", 80, 83, true),
                entry("masterhand", OFF_ROSTER, -1, false),
            ])),
        )]);
        let mut db = CharaDb {
            root,
            entries: Vec::new(),
        };
        db.refresh();
        db
    }

    /// The whole reason this module exists. Following `save_no` would put Richter before
    /// Simon, which is not what the game shows.
    #[test]
    fn roster_order_follows_disp_order_not_save_no_or_list_order() {
        let db = db();
        let names: Vec<&str> = roster_order(&db)
            .iter()
            .map(|row| row.name_id.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "mario",
                "inkling",
                "simon",
                "richter",
                "eflame_first",
                "elight_first"
            ]
        );
    }

    #[test]
    fn the_random_slot_and_off_roster_entries_are_not_positions() {
        let db = db();
        // The Random slot is a sentinel, not a position, and must never be drawn as a cell.
        assert_eq!(db.row("random").unwrap().disp_order, RANDOM_SLOT_ORDER);
        assert!(!db.row("random").unwrap().on_roster());
        assert!(!db.row("masterhand").unwrap().on_roster());
    }

    /// Two entries sharing a position share a cell, so neither may be dropped or reordered
    /// past the other by the tie-break.
    #[test]
    fn entries_sharing_a_position_stay_adjacent_in_file_order() {
        let db = db();
        let shared: Vec<&str> = roster_order(&db)
            .iter()
            .filter(|row| row.disp_order == 80)
            .map(|row| row.name_id.as_str())
            .collect();
        assert_eq!(shared, vec!["eflame_first", "elight_first"]);
    }

    /// Vanilla never disagrees between these two fields, and an entry that does is one the
    /// game will render but refuse to pick.
    #[test]
    fn hiding_and_restoring_keeps_can_select_in_step_with_disp_order() {
        let mut db = db();
        db.set_disp_order("mario", OFF_ROSTER).unwrap();
        let row = db.row("mario").unwrap();
        assert_eq!(row.disp_order, OFF_ROSTER);
        assert!(!row.can_select);
        assert!(!row.on_roster());

        db.set_disp_order("mario", 3).unwrap();
        let row = db.row("mario").unwrap();
        assert_eq!(row.disp_order, 3);
        assert!(row.can_select);
        assert!(row.on_roster());
    }

    #[test]
    fn an_order_naming_an_absent_character_is_reported_rather_than_skipped() {
        let mut db = db();
        let mut order = BTreeMap::new();
        order.insert("mario".to_string(), 5);
        order.insert("nosuchfighter".to_string(), 6);
        let missing = db.apply_order(&order);
        assert_eq!(missing, vec!["nosuchfighter"]);
        assert_eq!(db.row("mario").unwrap().disp_order, 5);
    }

    /// Fields this editor does not model must survive a load-and-save. The real file carries
    /// 75 fields per entry and models seven of them.
    #[test]
    fn unmodelled_fields_survive_a_round_trip() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut root = ParamStruct(vec![(
            Hash40(hash40::hash40("db_root").0),
            ParamKind::List(ParamList(vec![entry("mario", 0, 0, true)])),
        )]);
        // A field the lifted view knows nothing about.
        if let Some(ParamKind::List(list)) = db_root_mut(&mut root) {
            if let Some(ParamKind::Struct(st)) = list.0.get_mut(0) {
                st.0.push((
                    Hash40(hash40::hash40("exhibit_year").0),
                    ParamKind::I16(1981),
                ));
            }
        }
        prc::save(file.path(), &root).unwrap();

        let mut db = CharaDb::open(file.path()).unwrap();
        db.set_disp_order("mario", 7).unwrap();
        db.save(file.path()).unwrap();

        let reloaded = prc::open(file.path()).unwrap();
        let ParamKind::List(list) = db_root(&reloaded).unwrap() else {
            panic!("db_root lost");
        };
        let ParamKind::Struct(st) = &list.0[0] else {
            panic!("entry lost");
        };
        assert_eq!(i8_field(st, "disp_order"), Some(7));
        assert_eq!(
            field(st, "exhibit_year"),
            Some(&ParamKind::I16(1981)),
            "a field the editor does not model was dropped by a save"
        );
    }

    /// Reading and writing without editing must produce the same file. Until this holds,
    /// nothing about an edited file can be trusted.
    #[test]
    fn an_untouched_load_and_save_is_byte_identical() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let source = db();
        prc::save(file.path(), &source.root).unwrap();
        let original = std::fs::read(file.path()).unwrap();

        let db = CharaDb::open(file.path()).unwrap();
        let out = tempfile::NamedTempFile::new().unwrap();
        db.save(out.path()).unwrap();
        assert_eq!(std::fs::read(out.path()).unwrap(), original);
    }

    /// The synthetic fixture above pins the logic; only a real file pins the *format*. A
    /// hand-built `ParamStruct` round-trips through `prc` trivially — the question that
    /// matters is whether a 121-entry, 75-field vanilla database does, and whether the
    /// ordering rule reproduces the select screen everyone has seen.
    ///
    /// Skipped unless `VISIONARY_TEST_CHARA_DB` points at one, because the file cannot be
    /// committed. Run it whenever this module changes:
    ///
    /// ```text
    /// VISIONARY_TEST_CHARA_DB=/path/to/ui_chara_db.prc cargo test --bin visionary against_a_real
    /// ```
    #[test]
    fn against_a_real_chara_db() {
        let Some(path) = std::env::var_os("VISIONARY_TEST_CHARA_DB") else {
            return;
        };
        let path = PathBuf::from(path);
        let db = CharaDb::open(&path).expect("a real ui_chara_db must load");

        let out = tempfile::NamedTempFile::new().unwrap();
        db.save(out.path()).unwrap();
        assert_eq!(
            std::fs::read(out.path()).unwrap(),
            std::fs::read(&path).unwrap(),
            "an untouched load-and-save of a real ui_chara_db changed its bytes"
        );

        let order: Vec<&str> = roster_order(&db)
            .iter()
            .map(|row| row.name_id.as_str())
            .collect();
        assert_eq!(order.first(), Some(&"mario"));

        // The five entries whose `disp_order` disagrees with `save_no`. This sequence is what
        // the game shows and what following `save_no` would get wrong.
        let tail: Vec<&&str> = order
            .iter()
            .skip_while(|name| **name != "inkling")
            .take(5)
            .collect();
        assert_eq!(
            tail,
            vec![&"inkling", &"ridley", &"simon", &"richter", &"krool"],
            "the select screen sequence around the Ultimate newcomers is wrong"
        );

        // The properties the rest of this module is built on, asserted against real data
        // rather than against the fixture that was written to have them.
        assert_eq!(
            db.row("random").map(|row| row.disp_order),
            Some(RANDOM_SLOT_ORDER)
        );
        assert!(db
            .entries()
            .iter()
            .any(|row| row.disp_order == OFF_ROSTER && !row.can_select));
        let mut positions: Vec<i8> = roster_order(&db).iter().map(|row| row.disp_order).collect();
        let total = positions.len();
        positions.dedup();
        assert!(
            positions.len() < total,
            "a real database has at least one shared position (Pyra/Mythra)"
        );
    }
}
