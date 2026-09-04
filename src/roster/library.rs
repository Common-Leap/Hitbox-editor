//! The mod library: many compiled mods imported at once, scanned, ordered, and resolved.
//!
//! Before this module, a mod was a bare path appended to `extra_roots` and fighters found
//! under it were tagged `FighterSource::ModRoot`. There was no notion of a *mod* as a unit,
//! so two mods editing the same fighter were indistinguishable from one, and nothing could
//! be turned off without removing it.
//!
//! A [`ModLibrary`] holds [`ImportedMod`]s in **load order**. Later entries win. That single
//! rule decides every conflict, and it is the only thing the roster index needs to know in
//! order to resolve a file to a provider.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Identifies a mod within one library. Stable for the library's lifetime and persisted, so
/// a provider recorded against an entry survives a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProviderId(pub u32);

/// Where a mod's files physically came from, before root detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModSource {
    /// The user pointed at a folder; the files are used in place and never copied.
    Folder(PathBuf),
    /// The user pointed at an archive, which was extracted into the library cache.
    Archive {
        archive: PathBuf,
        extracted: PathBuf,
    },
}

impl ModSource {
    /// The directory the scan starts from.
    pub fn scan_base(&self) -> &Path {
        match self {
            Self::Folder(path) => path,
            Self::Archive { extracted, .. } => extracted,
        }
    }

    /// What to show the user as the origin of this mod.
    pub fn origin_path(&self) -> &Path {
        match self {
            Self::Folder(path) => path,
            Self::Archive { archive, .. } => archive,
        }
    }
}

/// How [`detect_arc_root`] arrived at its answer.
///
/// Reported rather than kept internal: a mod whose root was guessed one level too high
/// contributes nothing and looks identical to a mod that simply contains no fighters. The
/// user has to be able to see which happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootDetection {
    /// The directory the user chose already contains `fighter/`, `effect/`, or `ui/`.
    AsGiven,
    /// A recognized wrapper directory was stepped through, e.g. `romfs/` or a single
    /// containing folder named after the mod.
    Descended { through: Vec<String> },
}

impl RootDetection {
    pub fn describe(&self) -> String {
        match self {
            Self::AsGiven => "used the chosen folder directly".to_string(),
            Self::Descended { through } => {
                format!("descended through {}", through.join("/"))
            }
        }
    }
}

/// Directory names that wrap an arc root in real distributed mods and carry no game meaning.
const WRAPPER_DIRS: &[&str] = &["romfs", "arc", "atmosphere", "contents", "01006a800016e000"];

/// Top-level directories that mark a directory as an arc root.
const ARC_ROOT_MARKERS: &[&str] = &[
    "fighter",
    "effect",
    "ui",
    "sound",
    "stage",
    "camera",
    "param",
    "prebuilt;",
];

/// Depth limit for wrapper descent. Real mods nest a handful of levels; an unbounded walk
/// over a deep archive would happily descend into `fighter/mario/model/body` and call that
/// the root because it contains no marker either way.
const MAX_DESCENT: usize = 6;

/// Find the arc root inside an imported folder.
///
/// A distributed mod is as likely to be `MyMod/fighter/...` or
/// `atmosphere/contents/<titleid>/romfs/fighter/...` as it is to be a bare arc root, and
/// picking the wrong level yields a mod that provides nothing.
///
/// Returns the root and a description of how it was reached, which the caller must show.
pub fn detect_arc_root(base: &Path) -> Result<(PathBuf, RootDetection)> {
    if !base.is_dir() {
        bail!("{} is not a directory", base.display());
    }
    if has_arc_marker(base) {
        return Ok((base.to_path_buf(), RootDetection::AsGiven));
    }

    let mut current = base.to_path_buf();
    let mut through = Vec::new();
    for _ in 0..MAX_DESCENT {
        let Some(next) = descend_candidate(&current) else {
            break;
        };
        let name = next
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        current = next;
        through.push(name);
        if has_arc_marker(&current) {
            return Ok((current, RootDetection::Descended { through }));
        }
    }

    bail!(
        "no fighter/, effect/, or ui/ folder found under {} — is this an Arcropolis-style mod?",
        base.display()
    )
}

fn has_arc_marker(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.path().is_dir()
            && entry
                .file_name()
                .to_str()
                .map(|name| ARC_ROOT_MARKERS.contains(&name.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
    })
}

/// The one directory worth stepping into from `dir`, if any.
///
/// Either a known wrapper name, or — when `dir` holds exactly one subdirectory and no
/// interesting files — that subdirectory, which is the "extracted archive made a folder
/// named after itself" case.
fn descend_candidate(dir: &Path) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    let subdirs: Vec<PathBuf> = entries
        .iter()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();

    if let Some(wrapper) = subdirs.iter().find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| WRAPPER_DIRS.contains(&name.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
    }) {
        return Some(wrapper.clone());
    }

    if subdirs.len() == 1 {
        return Some(subdirs[0].clone());
    }
    None
}

/// What one mod provides, as scanned from its arc root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModManifest {
    /// Every file the mod provides, as a forward-slash game-relative path.
    #[serde(default)]
    pub paths: BTreeSet<String>,
    /// Fighter name → what the mod provides for that fighter.
    #[serde(default)]
    pub fighters: BTreeMap<String, FighterProvision>,
    /// True when the mod ships anything under `ui/` — a portrait, a name, a roster row.
    #[serde(default)]
    pub provides_ui: bool,
    /// Compiled Skyline plugins shipped by this mod, relative to the mod source.
    ///
    /// Visionary cannot read compiled plugin behavior, so a fighter touched by one of these
    /// has a moveset the editor's view of is incomplete. Recorded so that can be said out
    /// loud rather than discovered by the user when an edit does not match the game.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Files that were skipped because they sit above any recognized game folder.
    #[serde(default)]
    pub unrecognized: usize,
}

/// What a mod contributes to one fighter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FighterProvision {
    /// Costume slots the mod provides files for, ascending.
    #[serde(default)]
    pub slots: BTreeSet<u8>,
    #[serde(default)]
    pub has_model: bool,
    #[serde(default)]
    pub has_motion: bool,
    #[serde(default)]
    pub has_param: bool,
    #[serde(default)]
    pub has_effect: bool,
    /// Number of files provided for this fighter, for the library summary.
    #[serde(default)]
    pub file_count: usize,
}

/// Walk an arc root and record everything it provides.
///
/// Symlinks are not followed. A mod folder that links back into the game data root would
/// otherwise be scanned as though it provided the entire base game, and every vanilla file
/// would report as a conflict.
pub fn scan_manifest(root: &Path) -> Result<ModManifest> {
    let mut manifest = ModManifest::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .flatten();
        for entry in entries {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let game_path = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            record_path(&mut manifest, &game_path);
        }
    }
    Ok(manifest)
}

/// Classify one game-relative path into the manifest.
///
/// Split out from the walk so it can be exercised without a filesystem: the fighter and slot
/// attribution is the part that decides which mods conflict, and getting it wrong is silent.
fn record_path(manifest: &mut ModManifest, game_path: &str) {
    let lower = game_path.to_ascii_lowercase();

    if lower.ends_with(".nro") {
        manifest.plugins.push(game_path.to_string());
        return;
    }

    let parts: Vec<&str> = lower.split('/').collect();
    let top = parts.first().copied().unwrap_or_default();

    if !ARC_ROOT_MARKERS.contains(&top) {
        manifest.unrecognized += 1;
        return;
    }
    manifest.paths.insert(game_path.to_string());

    if top == "ui" {
        manifest.provides_ui = true;
        return;
    }

    // `fighter/<name>/...` and `effect/fighter/<name>/...` both attribute to a fighter, and
    // both are how a mod adds a character or a slot.
    let (fighter, rest): (&str, &[&str]) = match (top, parts.as_slice()) {
        ("fighter", [_, name, rest @ ..]) => (*name, rest),
        ("effect", [_, "fighter", name, rest @ ..]) => (*name, rest),
        _ => return,
    };

    let provision = manifest.fighters.entry(fighter.to_string()).or_default();
    provision.file_count += 1;
    match rest.first().copied() {
        Some("model") => provision.has_model = true,
        Some("motion") => provision.has_motion = true,
        Some("param") => provision.has_param = true,
        _ => {}
    }
    if top == "effect" {
        provision.has_effect = true;
    }
    for part in rest {
        if let Some(slot) = parse_slot(part) {
            provision.slots.insert(slot);
        }
    }
    // One-slot effect files carry the slot in the filename rather than a directory:
    // `effect/fighter/<f>/ef_<f>_c08.eff`.
    if top == "effect" {
        if let Some(file) = rest.last() {
            if let Some(stem) = file.strip_suffix(".eff") {
                if let Some(slot) = stem.rsplit('_').next().and_then(parse_slot) {
                    provision.slots.insert(slot);
                    provision.has_effect = true;
                }
            }
        }
    }
}

/// `c08` → `Some(8)`. Slots run well past c07, so this is not limited to one digit.
fn parse_slot(part: &str) -> Option<u8> {
    let digits = part.strip_prefix('c')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// One imported mod in the library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedMod {
    pub id: ProviderId,
    /// Shown in the library list. Defaults to the folder or archive name; user-editable.
    pub name: String,
    pub source: ModSource,
    /// The detected arc root — the directory `fighter/` etc. live directly under.
    pub root: PathBuf,
    pub detection: RootDetection,
    pub enabled: bool,
    #[serde(default)]
    pub manifest: ModManifest,
}

impl ImportedMod {
    /// True when the mod ships a compiled Skyline plugin, whose behavior the editor cannot
    /// read.
    pub fn ships_plugin(&self) -> bool {
        !self.manifest.plugins.is_empty()
    }

    pub fn summary(&self) -> String {
        let fighters = self.manifest.fighters.len();
        let files = self.manifest.paths.len();
        let mut parts = vec![format!(
            "{files} file{} across {fighters} fighter{}",
            if files == 1 { "" } else { "s" },
            if fighters == 1 { "" } else { "s" }
        )];
        if self.manifest.provides_ui {
            parts.push("ships UI files".to_string());
        }
        if self.ships_plugin() {
            parts.push(format!("{} plugin(s)", self.manifest.plugins.len()));
        }
        parts.join(" · ")
    }
}

/// One game path provided by more than one enabled mod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub game_path: String,
    /// Every enabled provider of this path, in load order. The last one wins.
    pub providers: Vec<ProviderId>,
}

impl Conflict {
    pub fn winner(&self) -> Option<ProviderId> {
        self.providers.last().copied()
    }
}

/// A mod that has been located and scanned but not yet given an id.
///
/// The unit of work a background import thread produces: everything about a mod that can be
/// determined from the filesystem alone.
#[derive(Debug, Clone)]
pub struct PreparedMod {
    pub name: String,
    pub source: ModSource,
    pub root: PathBuf,
    pub detection: RootDetection,
    pub manifest: ModManifest,
}

/// Locate the arc root inside `source` and scan what it provides.
///
/// The caller supplies [`ModSource`] so that folder imports keep pointing at the user's own
/// directory — a mod folder the user maintains is not copied into the cache, because then
/// their edits to it would stop being visible.
pub fn prepare(source: ModSource, name: Option<String>) -> Result<PreparedMod> {
    let (root, detection) = detect_arc_root(source.scan_base())?;
    let manifest = scan_manifest(&root)?;
    if manifest.paths.is_empty() && manifest.plugins.is_empty() {
        bail!(
            "{} contains no game files ({})",
            source.origin_path().display(),
            detection.describe()
        );
    }
    let name = name.unwrap_or_else(|| {
        source
            .origin_path()
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "mod".to_string())
    });
    Ok(PreparedMod {
        name,
        source,
        root,
        detection,
        manifest,
    })
}

/// The imported mods, in load order. Later entries win.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModLibrary {
    #[serde(default)]
    pub mods: Vec<ImportedMod>,
    #[serde(default)]
    next_id: u32,
}

impl ModLibrary {
    pub fn is_empty(&self) -> bool {
        self.mods.is_empty()
    }

    /// Enabled mods in load order — the resolution order for every lookup.
    ///
    /// Reversible because resolution reads it from the back: later wins.
    pub fn enabled(&self) -> impl DoubleEndedIterator<Item = &ImportedMod> {
        self.mods.iter().filter(|entry| entry.enabled)
    }

    pub fn get(&self, id: ProviderId) -> Option<&ImportedMod> {
        self.mods.iter().find(|entry| entry.id == id)
    }

    pub fn name_of(&self, id: ProviderId) -> String {
        self.get(id)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| format!("mod {}", id.0))
    }

    fn allocate_id(&mut self) -> ProviderId {
        // Derived from the maximum in use rather than the length, so removing a mod cannot
        // hand a later import an id a saved project still references.
        let next = self
            .mods
            .iter()
            .map(|entry| entry.id.0 + 1)
            .chain(std::iter::once(self.next_id))
            .max()
            .unwrap_or(0);
        self.next_id = next + 1;
        ProviderId(next)
    }

    /// Add an already-scanned mod and give it an id.
    ///
    /// Separate from [`prepare`] so that detection and the manifest walk — the slow, purely
    /// filesystem half — can run on a background thread while only this cheap step needs the
    /// library itself.
    pub fn insert(&mut self, prepared: PreparedMod) -> ProviderId {
        let id = self.allocate_id();
        self.mods.push(ImportedMod {
            id,
            name: prepared.name,
            source: prepared.source,
            root: prepared.root,
            detection: prepared.detection,
            enabled: true,
            manifest: prepared.manifest,
        });
        id
    }

    /// Import an already-extracted or already-on-disk directory, scanning inline.
    pub fn import_directory(
        &mut self,
        source: ModSource,
        name: Option<String>,
    ) -> Result<ProviderId> {
        Ok(self.insert(prepare(source, name)?))
    }

    pub fn remove(&mut self, id: ProviderId) {
        self.mods.retain(|entry| entry.id != id);
    }

    /// Move a mod one position later in load order — later wins, so this promotes it.
    pub fn move_later(&mut self, id: ProviderId) {
        if let Some(index) = self.mods.iter().position(|entry| entry.id == id) {
            if index + 1 < self.mods.len() {
                self.mods.swap(index, index + 1);
            }
        }
    }

    pub fn move_earlier(&mut self, id: ProviderId) {
        if let Some(index) = self.mods.iter().position(|entry| entry.id == id) {
            if index > 0 {
                self.mods.swap(index - 1, index);
            }
        }
    }

    /// Every path provided by more than one enabled mod, with the winner last.
    ///
    /// This is the whole reason the manifest exists. Two mods that both replace Mario are
    /// indistinguishable from one until their file lists are compared.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut by_path: BTreeMap<&str, Vec<ProviderId>> = BTreeMap::new();
        for entry in self.enabled() {
            for path in &entry.manifest.paths {
                by_path.entry(path.as_str()).or_default().push(entry.id);
            }
        }
        by_path
            .into_iter()
            .filter(|(_, providers)| providers.len() > 1)
            .map(|(game_path, providers)| Conflict {
                game_path: game_path.to_string(),
                providers,
            })
            .collect()
    }

    /// Conflicts rolled up per fighter, which is the granularity the roster panel shows.
    pub fn conflicts_by_fighter(&self) -> BTreeMap<String, Vec<Conflict>> {
        let mut out: BTreeMap<String, Vec<Conflict>> = BTreeMap::new();
        for conflict in self.conflicts() {
            if let Some(fighter) = fighter_of_path(&conflict.game_path) {
                out.entry(fighter).or_default().push(conflict);
            }
        }
        out
    }

    /// Enabled providers that contribute anything to one fighter, in load order.
    pub fn providers_for_fighter(&self, fighter: &str) -> Vec<ProviderId> {
        let fighter = fighter.to_ascii_lowercase();
        self.enabled()
            .filter(|entry| entry.manifest.fighters.contains_key(&fighter))
            .map(|entry| entry.id)
            .collect()
    }

    /// Enabled roots, earliest first — what the existing fighter scan consumes as extra roots.
    pub fn enabled_roots(&self) -> Vec<PathBuf> {
        self.enabled().map(|entry| entry.root.clone()).collect()
    }
}

/// The fighter a game path belongs to, for conflict roll-up.
pub fn fighter_of_path(game_path: &str) -> Option<String> {
    let lower = game_path.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    match parts.as_slice() {
        ["fighter", name, ..] => Some((*name).to_string()),
        ["effect", "fighter", name, ..] => Some((*name).to_string()),
        _ => None,
    }
}

// ── Persistence ─────────────────────────────────────────────────────────────

fn library_path() -> PathBuf {
    crate::scratch_dirs::app_storage_root().join("mod_library.json")
}

/// Extraction cache. One stable directory per mod slug, reused and overwritten across runs
/// rather than accumulating one orphaned directory per import.
pub fn extraction_root(slug: &str) -> PathBuf {
    crate::scratch_dirs::app_storage_root()
        .join("mods")
        .join(slug)
}

pub fn save(library: &ModLibrary) {
    let path = library_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string_pretty(library) {
        let _ = std::fs::write(&path, body);
    }
}

/// Load the saved library, dropping mods whose files are gone.
///
/// A missing root is dropped rather than kept broken for the same reason `load_mod_roots`
/// drops missing paths: an unplugged drive should not leave a permanently failing entry.
pub fn load() -> ModLibrary {
    let Ok(body) = std::fs::read_to_string(library_path()) else {
        return ModLibrary::default();
    };
    let mut library: ModLibrary = serde_json::from_str(&body).unwrap_or_default();
    library.mods.retain(|entry| entry.root.is_dir());
    library
}

/// Adopt paths from the pre-library `mod_roots` config so an upgrading user keeps their
/// setup. Paths already present as a mod root are skipped.
pub fn adopt_legacy_roots(library: &mut ModLibrary, roots: &[PathBuf]) -> Vec<String> {
    let mut notes = Vec::new();
    for root in roots {
        if library
            .mods
            .iter()
            .any(|entry| entry.source.scan_base() == root.as_path())
        {
            continue;
        }
        match library.import_directory(ModSource::Folder(root.clone()), None) {
            Ok(id) => notes.push(format!("Imported {}", library.name_of(id))),
            Err(error) => notes.push(format!("{}: {error}", root.display())),
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn a_bare_arc_root_is_used_as_given() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("fighter/mario")).unwrap();
        let (root, detection) = detect_arc_root(dir.path()).unwrap();
        assert_eq!(root, dir.path());
        assert_eq!(detection, RootDetection::AsGiven);
    }

    // The failure this guards is silent: guessing one level too high yields a mod that
    // provides nothing and looks exactly like a mod that contains no fighters.
    #[test]
    fn wrapper_directories_are_descended_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("MyMod/romfs/fighter/mario")).unwrap();
        let (root, detection) = detect_arc_root(dir.path()).unwrap();
        assert_eq!(root, dir.path().join("MyMod/romfs"));
        match detection {
            RootDetection::Descended { through } => assert_eq!(through, vec!["MyMod", "romfs"]),
            other => panic!("expected descent, got {other:?}"),
        }
    }

    #[test]
    fn a_folder_with_no_game_files_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("readme/images")).unwrap();
        assert!(detect_arc_root(dir.path()).is_err());
    }

    #[test]
    fn slots_are_attributed_from_directories_and_from_eff_filenames() {
        let mut manifest = ModManifest::default();
        record_path(&mut manifest, "fighter/mario/model/body/c08/model.numdlb");
        record_path(&mut manifest, "fighter/mario/motion/body/c08/a.nuanmb");
        record_path(&mut manifest, "effect/fighter/mario/ef_mario_c08.eff");
        let mario = &manifest.fighters["mario"];
        assert_eq!(mario.slots, BTreeSet::from([8]));
        assert!(mario.has_model && mario.has_motion && mario.has_effect);
        assert_eq!(mario.file_count, 3);
    }

    #[test]
    fn three_digit_slots_are_not_truncated() {
        let mut manifest = ModManifest::default();
        record_path(&mut manifest, "fighter/mario/model/body/c112/model.numdlb");
        assert_eq!(manifest.fighters["mario"].slots, BTreeSet::from([112]));
    }

    #[test]
    fn plugins_and_stray_files_are_separated_from_game_paths() {
        let mut manifest = ModManifest::default();
        record_path(&mut manifest, "plugin.nro");
        record_path(&mut manifest, "README.txt");
        record_path(&mut manifest, "ui/message/msg_name.msbt");
        assert_eq!(manifest.plugins, vec!["plugin.nro"]);
        assert_eq!(manifest.unrecognized, 1);
        assert!(manifest.provides_ui);
        assert!(manifest.fighters.is_empty());
    }

    // Load order is the only conflict rule, so a wrong winner is wrong everywhere at once
    // and never announces itself.
    #[test]
    fn the_last_enabled_provider_wins_a_shared_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ModLibrary::default();
        let mut make = |name: &str| {
            let root = dir.path().join(name);
            touch(&root.join("fighter/mario/model/body/c00/model.numdlb"));
            library
                .import_directory(ModSource::Folder(root), Some(name.to_string()))
                .unwrap()
        };
        let first = make("first");
        let second = make("second");

        let conflicts = library.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].providers, vec![first, second]);
        assert_eq!(conflicts[0].winner(), Some(second));
        library.move_earlier(second);
        assert_eq!(library.conflicts()[0].winner(), Some(first));
    }

    #[test]
    fn a_disabled_mod_neither_wins_nor_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ModLibrary::default();
        for name in ["a", "b"] {
            let root = dir.path().join(name);
            touch(&root.join("fighter/mario/model/body/c00/model.numdlb"));
            library
                .import_directory(ModSource::Folder(root), Some(name.to_string()))
                .unwrap();
        }
        let second = library.mods[1].id;
        library.mods[1].enabled = false;
        assert!(library.conflicts().is_empty());
        // And the disabled mod contributes no root for the fighter scan to read.
        assert_eq!(library.enabled_roots().len(), 1);
        assert!(!library.providers_for_fighter("mario").contains(&second));
    }

    // Removing a mod must not let a later import reuse its id: a saved project records
    // providers by id, and a reused id would silently reattribute them.
    #[test]
    fn removing_a_mod_does_not_recycle_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ModLibrary::default();
        fn make(library: &mut ModLibrary, dir: &Path, name: &str) -> ProviderId {
            let root = dir.join(name);
            touch(&root.join(format!("fighter/{name}/model/body/c00/model.numdlb")));
            library
                .import_directory(ModSource::Folder(root), Some(name.to_string()))
                .unwrap()
        }
        let first = make(&mut library, dir.path(), "mario");
        let second = make(&mut library, dir.path(), "link");
        library.remove(first);
        let third = make(&mut library, dir.path(), "fox");
        assert_ne!(third, first);
        assert_ne!(third, second);
    }

    #[test]
    fn symlinks_are_not_followed_into_the_game_root() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("game");
        touch(&game.join("fighter/mario/model/body/c00/model.numdlb"));
        let modroot = dir.path().join("mod");
        touch(&modroot.join("fighter/link/model/body/c00/model.numdlb"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&game, modroot.join("linked")).unwrap();
        let manifest = scan_manifest(&modroot).unwrap();
        assert_eq!(manifest.fighters.keys().collect::<Vec<_>>(), vec!["link"]);
    }
}
