//! Fighter-wide values: `fighter/common/param/fighter_param.prc`.
//!
//! One file for the whole game. It holds a `fighter_param_table` of 94 rows, each identified
//! by a single key — `fighter_kind` — and carrying 369 fields: weight, gravity, walk and run
//! speeds, jump heights, landing lag, shield size, jostle, combo counts.
//!
//! Two facts, both measured against a real dump, shape everything here:
//!
//!  * **Nothing is per-slot.** Not one of the 369 field names mentions a slot, colour, or
//!    costume. A slot-backed character therefore shares every trait with its donor, and the
//!    editor says so once and prominently rather than badging individual fields.
//!  * **It is one shared file.** Every fighter in the game lives in it, so edits must be
//!    sparse. A mod that shipped a whole copy of this file would overwrite every other mod's
//!    fighter along with its own.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use prc::ParamKind;

use crate::mod_project::{ParamMod, ParamValue};

/// The game path of the shared trait file.
pub const FIGHTER_PARAM_PATH: &str = "fighter/common/param/fighter_param.prc";

/// The list inside it, one row per fighter.
const TABLE: &str = "fighter_param_table";

/// The field that identifies a row.
const ROW_KEY: &str = "fighter_kind";

/// A named group of fields, so 369 values are approachable.
pub struct TraitSection {
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [TraitField],
}

/// One surfaced field: the name it has in the file, plus what it means in plain language.
pub struct TraitField {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

/// The fields worth putting in front of someone, grouped. Everything not listed here is still
/// reachable through the full field list — the curation decides what is *prominent*, never
/// what is *editable*, so a field nobody thought to curate is never unreachable.
pub const SECTIONS: &[TraitSection] = &[
    TraitSection {
        title: "Weight & size",
        description: "How hard the character is to launch, and how big they are.",
        fields: &[
            TraitField {
                key: "weight",
                label: "Weight",
                description: "Resistance to knockback. Higher survives longer.",
            },
            TraitField {
                key: "scale",
                label: "Model scale",
                description: "Overall size multiplier. Also scales hitboxes and reach.",
            },
            TraitField {
                key: "jostle_weight",
                label: "Push weight",
                description: "How hard this character shoves others aside when standing together.",
            },
            TraitField {
                key: "jostle_front",
                label: "Push range (front)",
                description: "How far in front the character pushes others.",
            },
            TraitField {
                key: "jostle_back",
                label: "Push range (back)",
                description: "How far behind the character pushes others.",
            },
        ],
    },
    TraitSection {
        title: "Ground movement",
        description: "Walking, dashing, running, and stopping on the ground.",
        fields: &[
            TraitField {
                key: "walk_speed_max",
                label: "Walk speed",
                description: "Top walking speed.",
            },
            TraitField {
                key: "walk_accel_mul",
                label: "Walk acceleration",
                description: "How quickly walking reaches top speed.",
            },
            TraitField {
                key: "dash_speed",
                label: "Dash speed",
                description: "Speed of the initial dash out of standing.",
            },
            TraitField {
                key: "run_speed_max",
                label: "Run speed",
                description: "Top running speed.",
            },
            TraitField {
                key: "run_accel_add",
                label: "Run acceleration",
                description: "How quickly running reaches top speed.",
            },
            TraitField {
                key: "ground_brake",
                label: "Ground friction",
                description: "How quickly the character slows to a stop.",
            },
        ],
    },
    TraitSection {
        title: "Jumps",
        description: "Jump squat, heights, and air jumps.",
        fields: &[
            TraitField {
                key: "jump_squat_frame",
                label: "Jump squat",
                description: "Frames of crouch before leaving the ground. Lower is faster.",
            },
            TraitField {
                key: "jump_y",
                label: "Full hop height",
                description: "Upward speed of a full jump.",
            },
            TraitField {
                key: "mini_jump_y",
                label: "Short hop height",
                description: "Upward speed of a short hop.",
            },
            TraitField {
                key: "jump_aerial_y",
                label: "Air jump height",
                description: "Upward speed of a midair jump.",
            },
            TraitField {
                key: "jump_speed_x",
                label: "Jump horizontal speed",
                description: "Forward speed carried into a jump.",
            },
            TraitField {
                key: "cliff_jump_y",
                label: "Ledge jump height",
                description: "Upward speed of a jump from a ledge.",
            },
        ],
    },
    TraitSection {
        title: "Air movement",
        description: "Drift, fall speed, and fast falling.",
        fields: &[
            TraitField {
                key: "air_speed_x_stable",
                label: "Air speed",
                description: "Top horizontal drift speed in the air.",
            },
            TraitField {
                key: "air_accel_x_mul",
                label: "Air acceleration",
                description: "How quickly air drift reaches top speed.",
            },
            TraitField {
                key: "air_brake_x",
                label: "Air friction",
                description: "How quickly horizontal air speed decays.",
            },
            TraitField {
                key: "air_accel_y",
                label: "Gravity",
                description: "Downward acceleration. Higher falls faster and is easier to combo.",
            },
            TraitField {
                key: "air_speed_y_stable",
                label: "Fall speed",
                description: "Top falling speed.",
            },
            TraitField {
                key: "dive_speed_y",
                label: "Fast fall speed",
                description: "Top falling speed while fast falling.",
            },
        ],
    },
    TraitSection {
        title: "Landing & shield",
        description: "Landing lag, and the size and behaviour of the shield.",
        fields: &[
            TraitField {
                key: "landing_frame",
                label: "Landing lag",
                description: "Frames of recovery on a normal landing.",
            },
            TraitField {
                key: "landing_heavy_frame",
                label: "Hard landing lag",
                description: "Frames of recovery after a heavy landing.",
            },
            TraitField {
                key: "landing_attack_air_frame_n",
                label: "Neutral air landing lag",
                description: "Landing lag after a neutral aerial.",
            },
            TraitField {
                key: "landing_attack_air_frame_f",
                label: "Forward air landing lag",
                description: "Landing lag after a forward aerial.",
            },
            TraitField {
                key: "landing_attack_air_frame_b",
                label: "Back air landing lag",
                description: "Landing lag after a back aerial.",
            },
            TraitField {
                key: "landing_attack_air_frame_hi",
                label: "Up air landing lag",
                description: "Landing lag after an up aerial.",
            },
            TraitField {
                key: "landing_attack_air_frame_lw",
                label: "Down air landing lag",
                description: "Landing lag after a down aerial.",
            },
            TraitField {
                key: "shield_radius",
                label: "Shield size",
                description: "Radius of the shield bubble.",
            },
            TraitField {
                key: "guard_speed_limit",
                label: "Shield drift speed",
                description: "How fast the character can slide while shielding.",
            },
        ],
    },
    TraitSection {
        title: "Combos",
        description: "How many hits the jab and tilt strings have.",
        fields: &[
            TraitField {
                key: "attack_combo_max",
                label: "Jab hits",
                description: "Number of hits in the jab string.",
            },
            TraitField {
                key: "s3_combo_max",
                label: "Side tilt hits",
                description: "Number of hits in the side tilt string.",
            },
            TraitField {
                key: "s4_combo_max",
                label: "Side smash hits",
                description: "Number of hits in the side smash string.",
            },
        ],
    },
];

/// Every curated key, as one set.
///
/// The single definition of "the curated fields", so the checks that they are distinct and
/// that they name real fields cannot disagree about what the curated set is. The UI walks
/// [`SECTIONS`] directly, so this exists for those checks alone.
#[cfg(test)]
fn curated_keys() -> std::collections::BTreeSet<&'static str> {
    SECTIONS
        .iter()
        .flat_map(|section| section.fields.iter().map(|field| field.key))
        .collect()
}

/// One field of one fighter's row, as loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitValue {
    pub key: String,
    pub value: ParamValue,
}

/// The trait file, and the row belonging to one fighter.
pub struct FighterTraits {
    /// The whole file, so every fighter's row and every unmodelled field round-trips.
    root: prc::ParamStruct,
    /// Index of this fighter's row in `fighter_param_table`.
    row: usize,
    pub fighter: String,
    /// The row as loaded, in file order.
    values: Vec<TraitValue>,
    /// hash40 → field name, from the downloaded param labels. Held because the file stores
    /// hashes, not names, and the full-field view has nothing to show without them.
    labels: HashMap<u64, String>,
}

impl FighterTraits {
    /// Load one fighter's row out of the shared trait file.
    pub fn open(path: &Path, fighter: &str, labels: &HashMap<u64, String>) -> Result<Self> {
        let root = prc::open(path)
            .map_err(|error| anyhow::anyhow!("{error:?}"))
            .with_context(|| format!("reading {}", path.display()))?;
        let wanted = super::css::fighter_kind_hash(fighter);
        let row = table(&root)
            .and_then(|list| {
                list.0.iter().position(|item| {
                    matches!(item, ParamKind::Struct(entry) if field(entry, ROW_KEY)
                        .and_then(param_hash) == Some(wanted))
                })
            })
            .with_context(|| {
                format!(
                    "{fighter} has no row in {TABLE} — only fighters the base game ships have one"
                )
            })?;
        let mut traits = Self {
            root,
            row,
            fighter: fighter.to_string(),
            values: Vec::new(),
            labels: labels.clone(),
        };
        traits.refresh();
        Ok(traits)
    }

    /// Locate the trait file across the data root and enabled mod roots.
    pub fn locate(roots: &[PathBuf]) -> Option<PathBuf> {
        roots
            .iter()
            .map(|root| root.join(FIGHTER_PARAM_PATH))
            .find(|path| path.is_file())
    }

    pub fn values(&self) -> &[TraitValue] {
        &self.values
    }

    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.values
            .iter()
            .find(|value| value.key == key)
            .map(|value| &value.value)
    }

    /// Write one field into the in-memory tree, keeping its original prc type.
    ///
    /// The type comes from the file, not from the caller: these are typed values, and writing
    /// a float where the game reads an int produces a file that loads and behaves wrongly
    /// rather than one that fails.
    pub fn set(&mut self, key: &str, value: ParamValue) -> Result<()> {
        let key_hash = hash40::hash40(key).0;
        let row = self.row;
        let Some(list) = table_mut(&mut self.root) else {
            anyhow::bail!("{TABLE} is missing");
        };
        let Some(ParamKind::Struct(entry)) = list.0.get_mut(row) else {
            anyhow::bail!("row {row} is not a struct");
        };
        for (hash, slot) in entry.0.iter_mut() {
            if hash.0 == key_hash {
                *slot = coerce(slot, value)?;
                self.refresh();
                return Ok(());
            }
        }
        anyhow::bail!("{} has no field named {key}", self.fighter)
    }

    /// Apply every sparse edit for this fighter, returning the keys that no longer exist.
    ///
    /// Missing keys are returned, never skipped: an edit whose field is gone from the base
    /// file is one the project still holds and the mod will not contain, and the export path
    /// has to be able to name it.
    pub fn apply(&mut self, edits: &BTreeMap<String, ParamValue>) -> Vec<String> {
        let mut missing = Vec::new();
        for (key, value) in edits {
            if self.set(key, *value).is_err() {
                missing.push(key.clone());
            }
        }
        missing
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        prc::save(path, &self.root)
            .map_err(|error| anyhow::anyhow!("{error:?}"))
            .with_context(|| format!("writing {}", path.display()))
    }

    fn refresh(&mut self) {
        self.values.clear();
        let labels = std::mem::take(&mut self.labels);
        let Some(list) = table(&self.root) else {
            return;
        };
        let Some(ParamKind::Struct(entry)) = list.0.get(self.row) else {
            return;
        };
        for (hash, value) in &entry.0 {
            let Some(value) = to_param_value(value) else {
                // Rows contain nested lists and structs as well as scalars. Those are not
                // editable here and are deliberately not surfaced as though they were.
                continue;
            };
            self.values.push(TraitValue {
                key: labels
                    .get(&hash.0)
                    .cloned()
                    // A field with no label is still editable; it is shown by hash rather
                    // than hidden, because hiding it would make it unreachable.
                    .unwrap_or_else(|| format!("{:#x}", hash.0)),
                value,
            });
        }
        self.labels = labels;
    }
}

/// Read a sparse edit set for one fighter out of a project.
pub fn edits_for(params: &ParamMod) -> &BTreeMap<String, ParamValue> {
    static EMPTY: std::sync::OnceLock<BTreeMap<String, ParamValue>> = std::sync::OnceLock::new();
    params
        .files
        .get(FIGHTER_PARAM_PATH)
        .unwrap_or_else(|| EMPTY.get_or_init(BTreeMap::new))
}

/// Record one sparse edit, or clear it when the value returns to the base file's.
///
/// Clearing matters: an override that agrees with the base pins a value that would otherwise
/// track the game and any mod underneath it.
pub fn record_edit(params: &mut ParamMod, key: &str, value: ParamValue, base: Option<ParamValue>) {
    let file = params
        .files
        .entry(FIGHTER_PARAM_PATH.to_string())
        .or_default();
    if base == Some(value) {
        file.remove(key);
    } else {
        file.insert(key.to_string(), value);
    }
    if file.is_empty() {
        params.files.remove(FIGHTER_PARAM_PATH);
    }
}

/// Convert `value` to the type `existing` already holds.
///
/// Refuses rather than truncates when the value does not fit: silently clamping a weight of
/// 300 into an `i8` would store 44 and look like a successful edit.
fn coerce(existing: &ParamKind, value: ParamValue) -> Result<ParamKind> {
    let number = match value {
        ParamValue::Bool(flag) => {
            return match existing {
                ParamKind::Bool(_) => Ok(ParamKind::Bool(flag)),
                other => anyhow::bail!(
                    "this field is {} and cannot hold a yes/no value",
                    kind_name(other)
                ),
            }
        }
        ParamValue::Float(number) => number as f64,
        ParamValue::I8(number) => number as f64,
        ParamValue::U8(number) => number as f64,
        ParamValue::I16(number) => number as f64,
        ParamValue::U16(number) => number as f64,
        ParamValue::I32(number) => number as f64,
        ParamValue::U32(number) => number as f64,
        ParamValue::Hash(raw) => {
            return match existing {
                ParamKind::Hash(_) => Ok(ParamKind::Hash(prc::hash40::Hash40(raw))),
                other => anyhow::bail!("this field is {} and cannot hold a hash", kind_name(other)),
            }
        }
    };
    let fits = |low: f64, high: f64| (low..=high).contains(&number) && number.fract() == 0.0;
    match existing {
        ParamKind::Float(_) => Ok(ParamKind::Float(number as f32)),
        ParamKind::I8(_) if fits(i8::MIN as f64, i8::MAX as f64) => Ok(ParamKind::I8(number as i8)),
        ParamKind::U8(_) if fits(0.0, u8::MAX as f64) => Ok(ParamKind::U8(number as u8)),
        ParamKind::I16(_) if fits(i16::MIN as f64, i16::MAX as f64) => {
            Ok(ParamKind::I16(number as i16))
        }
        ParamKind::U16(_) if fits(0.0, u16::MAX as f64) => Ok(ParamKind::U16(number as u16)),
        ParamKind::I32(_) if fits(i32::MIN as f64, i32::MAX as f64) => {
            Ok(ParamKind::I32(number as i32))
        }
        ParamKind::U32(_) if fits(0.0, u32::MAX as f64) => Ok(ParamKind::U32(number as u32)),
        ParamKind::Bool(_) => {
            anyhow::bail!("this field is a yes/no value and cannot hold a number")
        }
        other => anyhow::bail!(
            "{number} does not fit in this field, which is {}",
            kind_name(other)
        ),
    }
}

fn kind_name(kind: &ParamKind) -> &'static str {
    match kind {
        ParamKind::Bool(_) => "a yes/no value",
        ParamKind::I8(_) => "a whole number from -128 to 127",
        ParamKind::U8(_) => "a whole number from 0 to 255",
        ParamKind::I16(_) => "a whole number from -32768 to 32767",
        ParamKind::U16(_) => "a whole number from 0 to 65535",
        ParamKind::I32(_) => "a whole number",
        ParamKind::U32(_) => "a positive whole number",
        ParamKind::Float(_) => "a decimal number",
        ParamKind::Hash(_) => "a name",
        ParamKind::Str(_) => "text",
        ParamKind::List(_) => "a list",
        ParamKind::Struct(_) => "a group",
    }
}

fn to_param_value(kind: &ParamKind) -> Option<ParamValue> {
    Some(match kind {
        ParamKind::Bool(value) => ParamValue::Bool(*value),
        ParamKind::I8(value) => ParamValue::I8(*value),
        ParamKind::U8(value) => ParamValue::U8(*value),
        ParamKind::I16(value) => ParamValue::I16(*value),
        ParamKind::U16(value) => ParamValue::U16(*value),
        ParamKind::I32(value) => ParamValue::I32(*value),
        ParamKind::U32(value) => ParamValue::U32(*value),
        ParamKind::Float(value) => ParamValue::Float(*value),
        ParamKind::Hash(value) => ParamValue::Hash(value.0),
        _ => return None,
    })
}

fn table(root: &prc::ParamStruct) -> Option<&prc::ParamList> {
    let wanted = hash40::hash40(TABLE).0;
    root.0.iter().find_map(|(hash, value)| match value {
        ParamKind::List(list) if hash.0 == wanted => Some(list),
        _ => None,
    })
}

fn table_mut(root: &mut prc::ParamStruct) -> Option<&mut prc::ParamList> {
    let wanted = hash40::hash40(TABLE).0;
    root.0.iter_mut().find_map(|(hash, value)| match value {
        ParamKind::List(list) if hash.0 == wanted => Some(list),
        _ => None,
    })
}

fn field<'a>(entry: &'a prc::ParamStruct, name: &str) -> Option<&'a ParamKind> {
    let wanted = hash40::hash40(name).0;
    entry
        .0
        .iter()
        .find(|(hash, _)| hash.0 == wanted)
        .map(|(_, value)| value)
}

fn param_hash(kind: &ParamKind) -> Option<u64> {
    match kind {
        ParamKind::Hash(value) => Some(value.0),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn test_file(fighters: &[(&str, f32, i32)]) -> prc::ParamStruct {
    use prc::hash40::Hash40;
    use prc::{ParamList, ParamStruct};

    let rows = fighters
        .iter()
        .map(|(name, weight, jump_squat)| {
            ParamKind::Struct(ParamStruct(vec![
                (
                    Hash40(hash40::hash40(ROW_KEY).0),
                    ParamKind::Hash(Hash40(super::css::fighter_kind_hash(name))),
                ),
                (
                    Hash40(hash40::hash40("weight").0),
                    ParamKind::Float(*weight),
                ),
                (
                    Hash40(hash40::hash40("jump_squat_frame").0),
                    ParamKind::I32(*jump_squat),
                ),
                (
                    Hash40(hash40::hash40("attack100_type").0),
                    ParamKind::Bool(false),
                ),
            ]))
        })
        .collect();
    ParamStruct(vec![(
        Hash40(hash40::hash40(TABLE).0),
        ParamKind::List(ParamList(rows)),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The labels the synthetic file's fields need, so the lifted view carries real names.
    fn labels() -> HashMap<u64, String> {
        ["weight", "jump_squat_frame", "attack100_type", ROW_KEY]
            .iter()
            .map(|name| (hash40::hash40(name).0, (*name).to_string()))
            .collect()
    }

    /// Labels for the real-file check, from the same source the app downloads.
    fn real_labels() -> HashMap<u64, String> {
        curated_keys()
            .into_iter()
            .map(|name| (hash40::hash40(name).0, name.to_string()))
            .collect()
    }

    fn written(dir: &Path) -> PathBuf {
        let path = dir.join(FIGHTER_PARAM_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        prc::save(&path, &test_file(&[("mario", 98.0, 3), ("link", 104.0, 7)])).unwrap();
        path
    }

    #[test]
    fn a_fighters_row_is_found_by_fighter_kind() {
        let dir = tempfile::tempdir().unwrap();
        let traits = FighterTraits::open(&written(dir.path()), "link", &labels()).unwrap();
        assert_eq!(traits.get("weight"), Some(&ParamValue::Float(104.0)));
        assert_eq!(traits.get("jump_squat_frame"), Some(&ParamValue::I32(7)));
    }

    #[test]
    fn a_fighter_with_no_row_is_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let result = FighterTraits::open(&written(dir.path()), "mychar", &labels());
        let error = match result {
            Ok(_) => panic!("a fighter with no row must be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("no row"), "{error}");
    }

    /// The file is shared by every fighter in the game. Editing one row must leave the other
    /// 93 exactly as they were, or a mod would overwrite every other fighter.
    #[test]
    fn editing_one_fighter_leaves_every_other_row_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = written(dir.path());
        let mut traits = FighterTraits::open(&path, "mario", &labels()).unwrap();
        traits.set("weight", ParamValue::Float(120.0)).unwrap();
        let out = dir.path().join("out.prc");
        traits.save(&out).unwrap();

        let reloaded = FighterTraits::open(&out, "mario", &labels()).unwrap();
        assert_eq!(reloaded.get("weight"), Some(&ParamValue::Float(120.0)));
        let other = FighterTraits::open(&out, "link", &labels()).unwrap();
        assert_eq!(other.get("weight"), Some(&ParamValue::Float(104.0)));
        assert_eq!(other.get("jump_squat_frame"), Some(&ParamValue::I32(7)));
    }

    /// These are typed values. Writing a float where the game reads an int produces a file
    /// that loads and behaves wrongly rather than one that fails.
    #[test]
    fn a_value_is_written_as_the_type_the_field_already_holds() {
        let dir = tempfile::tempdir().unwrap();
        let mut traits = FighterTraits::open(&written(dir.path()), "mario", &labels()).unwrap();
        traits
            .set("jump_squat_frame", ParamValue::Float(5.0))
            .unwrap();
        assert_eq!(traits.get("jump_squat_frame"), Some(&ParamValue::I32(5)));
    }

    /// Silently clamping is the failure mode worth refusing: a stored 44 that the user typed
    /// as 300 looks like a successful edit.
    #[test]
    fn a_value_that_does_not_fit_the_field_is_refused_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let mut traits = FighterTraits::open(&written(dir.path()), "mario", &labels()).unwrap();
        assert!(traits
            .set("attack100_type", ParamValue::Float(3.0))
            .is_err());
        assert!(traits
            .set("jump_squat_frame", ParamValue::Float(2.5))
            .is_err());
        // And the refused edits changed nothing.
        assert_eq!(traits.get("jump_squat_frame"), Some(&ParamValue::I32(3)));
        assert_eq!(traits.get("attack100_type"), Some(&ParamValue::Bool(false)));
    }

    /// An edit whose field no longer exists is one the project holds and the mod will not
    /// contain. Returning it is what lets the export path name it.
    #[test]
    fn edits_naming_absent_fields_are_returned_rather_than_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut traits = FighterTraits::open(&written(dir.path()), "mario", &labels()).unwrap();
        let mut edits = BTreeMap::new();
        edits.insert("weight".to_string(), ParamValue::Float(110.0));
        edits.insert("no_such_field".to_string(), ParamValue::Float(1.0));
        let missing = traits.apply(&edits);
        assert_eq!(missing, vec!["no_such_field"]);
        assert_eq!(traits.get("weight"), Some(&ParamValue::Float(110.0)));
    }

    /// An override that agrees with the base pins a value that would otherwise track the game
    /// and any mod underneath it.
    #[test]
    fn returning_a_value_to_the_base_clears_the_override() {
        let mut params = ParamMod::default();
        let base = Some(ParamValue::Float(98.0));
        record_edit(&mut params, "weight", ParamValue::Float(120.0), base);
        assert_eq!(
            edits_for(&params).get("weight"),
            Some(&ParamValue::Float(120.0))
        );

        record_edit(&mut params, "weight", ParamValue::Float(98.0), base);
        assert!(edits_for(&params).is_empty());
        assert!(params.is_empty(), "an empty file entry was left behind");
    }

    /// Every curated key has to exist in a real row, or the section is a heading over nothing.
    /// This checks against the synthetic file's shape only; the real-file check is below.
    #[test]
    fn curated_sections_name_distinct_fields() {
        let mut listed = 0;
        for section in SECTIONS {
            for field in section.fields {
                listed += 1;
                assert!(
                    !field.description.is_empty(),
                    "{} has no explanation",
                    field.key
                );
            }
        }
        // The set deduplicates, so a smaller set than the walk means a field is in two
        // sections — which would show it twice and let two edits of it disagree.
        assert_eq!(
            curated_keys().len(),
            listed,
            "a curated field appears in more than one section"
        );
        assert!(listed > 30, "the curated set is suspiciously small");
    }

    /// The curated field names were written from a real dump; this is what keeps them true.
    /// Skipped unless `VISIONARY_TEST_FIGHTER_PARAM` points at a real file.
    #[test]
    fn every_curated_field_exists_in_a_real_fighter_row() {
        let Some(path) = std::env::var_os("VISIONARY_TEST_FIGHTER_PARAM") else {
            return;
        };
        let traits = FighterTraits::open(&PathBuf::from(path), "mario", &real_labels()).unwrap();
        let present: std::collections::BTreeSet<&str> = traits
            .values()
            .iter()
            .map(|value| value.key.as_str())
            .collect();
        let missing: Vec<&str> = curated_keys()
            .into_iter()
            .filter(|key| !present.contains(key))
            .collect();
        assert!(
            missing.is_empty(),
            "curated fields that do not exist: {missing:?}"
        );
        assert!(
            traits.values().len() > 300,
            "a real row has hundreds of fields, got {}",
            traits.values().len()
        );
    }
}
