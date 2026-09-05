//! Project Hub: one current project, its path, and the cold-start choices.
//!
//! Visionary edits used to live in memory until manually exported: launches started
//! blank, and there was no notion of "the project I have open". This module gives
//! that notion a shape that can be tested without a GUI:
//!
//! * [`CurrentProject`] tracks the open project's path (persisted) and whether it
//!   holds unsaved edits. Save writes silently when a path is known; Save As
//!   relocates; Export/Load adopt the path they touched.
//! * [`HubState`] models the cold-start hub — Resume last / New / Open
//!   (`modproject.json`) / Import mod / Recent / Browse without project — and the
//!   mid-session reopen with its unsaved-edits guard.
//! * Persistence helpers take an explicit config directory so tests run against a
//!   tempdir instead of the user's real config.

use std::path::{Path, PathBuf};

/// How many recent projects are remembered.
pub const MAX_RECENT_PROJECTS: usize = 10;

/// Config file holding the last open project path (one absolute path, or empty).
pub const LAST_PROJECT_KEY: &str = "project_path";
/// Config file holding recent project paths (one absolute path per line).
pub const RECENT_PROJECTS_KEY: &str = "recent_projects";

/// One current project holds every edit.
#[derive(Debug, Clone, Default)]
pub struct CurrentProject {
    /// Where Save writes silently. `None` means "never saved yet".
    pub path: Option<PathBuf>,
    /// True once any edit lands after the last save/load.
    pub dirty: bool,
    /// Display name (`modproject.json`'s `name`), for the hub and title bar.
    pub name: String,
}

/// Dirty-tracking model for the hub window. The app still tracks saves with
/// its own snapshot comparison; this model and its tests are the staged
/// replacement — wire the hub UI to it (or remove it) rather than letting the
/// two trackers drift.
#[allow(dead_code)]
impl CurrentProject {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_path(path: PathBuf, name: String) -> Self {
        Self {
            path: Some(path),
            dirty: false,
            name,
        }
    }

    #[allow(dead_code)]
    pub fn has_path(&self) -> bool {
        self.path.is_some()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Any edit lands here. Called by every mutation path (or, failing that,
    /// derived by comparing against the last saved snapshot).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// A successful save/load/export-adopt clears the flag and records the path.
    pub fn mark_saved(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.dirty = false;
    }

    /// Loading a project (or New) replaces the current one clean.
    #[allow(dead_code)]
    pub fn mark_loaded(&mut self, path: Option<PathBuf>, name: String) {
        self.path = path;
        self.name = name;
        self.dirty = false;
    }

    #[allow(dead_code)]
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// Save needs a file dialog when there is no path yet.
    pub fn needs_save_as(&self) -> bool {
        self.path.is_none()
    }

    /// Switching projects with unsaved edits must warn first.
    pub fn needs_warning(&self) -> bool {
        self.dirty
    }
}

/// What the hub can do. Pure so the cold-start matrix is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubAction {
    /// Reopen the last project path from config.
    ResumeLast(PathBuf),
    /// Start empty in a picked workspace folder. Carries the folder so every
    /// edit has a home from the first save — nothing lives only in memory.
    New(PathBuf),
    /// Open a `modproject.json` picked from disk.
    Open(PathBuf),
    /// Import a compiled mod folder as an editable project.
    ImportMod(PathBuf),
    /// Reopen one of the recent projects.
    OpenRecent(PathBuf),
    /// Dismiss the hub and keep browsing with no project.
    BrowseWithoutProject,
}

impl HubAction {
    /// Switching to this discards in-memory edits and must be guarded when dirty.
    /// Browsing without a project keeps the current in-memory edits, so it needs
    /// no guard; every other switch replaces them.
    pub fn discards_edits(&self) -> bool {
        !matches!(self, HubAction::BrowseWithoutProject)
    }

    #[allow(dead_code)]
    pub fn label(&self) -> String {
        match self {
            HubAction::ResumeLast(path) => format!("Resume {}", path.display()),
            HubAction::New(path) => format!("New project at {}", path.display()),
            HubAction::Open(path) => format!("Open {}", path.display()),
            HubAction::ImportMod(path) => format!("Import {}", path.display()),
            HubAction::OpenRecent(path) => format!("Open {}", path.display()),
            HubAction::BrowseWithoutProject => "Browse without project".to_string(),
        }
    }
}

/// True when choosing `action` with `dirty` unsaved edits must warn first.
pub fn needs_hub_warning(dirty: bool, action: &HubAction) -> bool {
    dirty && action.discards_edits()
}

/// The hub's cold-start state, derived from persisted config.
#[derive(Debug, Clone, Default)]
pub struct HubState {
    /// Whether the hub window is currently shown.
    pub show: bool,
    /// Last project path that still exists on disk, if any.
    pub last: Option<PathBuf>,
    /// Recent project paths that still exist, most-recent first.
    pub recent: Vec<PathBuf>,
}

impl HubState {
    /// Build from disk. Missing files are dropped so an unplugged drive does not
    /// leave a permanently failing Resume entry.
    pub fn cold_start(config_dir: &Path) -> Self {
        let last = load_last_project(config_dir).filter(|p| p.is_file());
        let mut recent = load_recent_projects(config_dir);
        recent.retain(|p| p.is_file());
        // The last project leads the recent list when it is also recent.
        Self {
            show: true,
            last,
            recent,
        }
    }

    /// The actions the hub offers right now, in display order.
    /// Same staged-model note as [`CurrentProject`]: tested, not yet wired.
    #[allow(dead_code)]
    pub fn choices(&self) -> Vec<HubChoice> {
        let mut out = Vec::new();
        if let Some(last) = &self.last {
            out.push(HubChoice::ResumeLast(last.clone()));
        }
        out.push(HubChoice::New);
        out.push(HubChoice::Open);
        out.push(HubChoice::ImportMod);
        for path in &self.recent {
            // Resume already covers the most recent path; listing it twice
            // invites opening it twice.
            if Some(path) != self.last.as_ref() {
                out.push(HubChoice::Recent(path.clone()));
            }
        }
        out.push(HubChoice::BrowseWithoutProject);
        out
    }
}

/// One row in the hub window. `Open`/`ImportMod` without a path mean "pick via
/// dialog"; the `HubAction` variants carry the picked path.
/// Same staged-model note as [`CurrentProject`]: tested, not yet wired.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubChoice {
    ResumeLast(PathBuf),
    New,
    Open,
    ImportMod,
    Recent(PathBuf),
    BrowseWithoutProject,
}

#[allow(dead_code)]
impl HubChoice {
    pub fn title(&self) -> &'static str {
        match self {
            HubChoice::ResumeLast(_) => "Resume last",
            HubChoice::New => "New",
            HubChoice::Open => "Open",
            HubChoice::ImportMod => "Import mod",
            HubChoice::Recent(_) => "Recent",
            HubChoice::BrowseWithoutProject => "Browse without project",
        }
    }
}

// ── Persistence (explicit config dir for testability) ───────────────────────

fn last_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LAST_PROJECT_KEY)
}

fn recent_path(config_dir: &Path) -> PathBuf {
    config_dir.join(RECENT_PROJECTS_KEY)
}

pub fn load_last_project(config_dir: &Path) -> Option<PathBuf> {
    let body = std::fs::read_to_string(last_path(config_dir)).ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

pub fn save_last_project(config_dir: &Path, path: &Path) {
    let _ = std::fs::create_dir_all(config_dir);
    let _ = std::fs::write(last_path(config_dir), path.to_string_lossy().as_bytes());
}

#[allow(dead_code)]
pub fn clear_last_project(config_dir: &Path) {
    let _ = std::fs::remove_file(last_path(config_dir));
}

pub fn load_recent_projects(config_dir: &Path) -> Vec<PathBuf> {
    let Ok(body) = std::fs::read_to_string(recent_path(config_dir)) else {
        return Vec::new();
    };
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

pub fn save_recent_projects(config_dir: &Path, recent: &[PathBuf]) {
    let _ = std::fs::create_dir_all(config_dir);
    let body = recent
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(recent_path(config_dir), body);
}

/// Record a project open/save: it becomes last and leads recents (deduped,
/// capped). Returns the new recent list.
pub fn push_recent(config_dir: &Path, path: &Path) -> Vec<PathBuf> {
    let mut recent = load_recent_projects(config_dir);
    recent.retain(|p| p != path);
    recent.insert(0, path.to_path_buf());
    recent.truncate(MAX_RECENT_PROJECTS);
    save_recent_projects(config_dir, &recent);
    save_last_project(config_dir, path);
    recent
}

/// Starting fresh (New / Browse without project) clears the last-project resume
/// but keeps recents: the user may still want to go back.
#[allow(dead_code)]
pub fn clear_current(config_dir: &Path) {
    clear_last_project(config_dir);
}

// ── Workspace: one folder that holds every edit ──────────────────────────
//
// A project is a folder, not just a file:
//   <workspace>/modproject.json   (every edit the tool supports)
//   <workspace>/assets/...        (portable textures/portraits, managed)
//   <workspace>/reference/...     (import reference copies, never exported)
//   <workspace>/romfs/...         (manual overlay, arc layout — models,
//                                  animations, and anything the tool does not
//                                  model; merged on export)
//
// New picks this folder upfront so nothing lives only in memory.

/// `modproject.json` inside a workspace folder.
pub fn workspace_file(workspace: &Path) -> PathBuf {
    workspace.join(crate::mod_project::PROJECT_FILE_NAME)
}

/// Manual overlay inside a workspace, in arc layout (`fighter/…`, `effect/…`).
pub fn workspace_romfs(workspace: &Path) -> PathBuf {
    workspace.join("romfs")
}

const WORKSPACE_README: &str = "\
# Visionary project workspace\n\
\n\
This folder holds every edit for the project.\n\
\n\
- `modproject.json` — every edit the tool supports (moves, params, roster,\n\
  names, portraits, effect values/textures). Edited in Visionary, saved with\n\
  Ctrl+S.\n\
- `assets/` — portable images managed by Visionary. Keep beside the JSON.\n\
- `reference/` — import reference copies (source text, notes). Never exported.\n\
- `romfs/` — MANUAL overlay in arc layout. Drop files the tool does not model\n\
  here and they ship verbatim on export:\n\
  - models: `romfs/fighter/<fighter>/model/...` (*.numdlb, *.numshb, *.nusktb…)\n\
  - animations: `romfs/fighter/<fighter>/motion/...` (*.nuanmb, *.nushdb…)\n\
  - sound: `romfs/sound/...`\n\
  - binary effect/message overrides: `romfs/effect/...`, `romfs/ui/message/...`\n\
\n\
See Windows → Project Files in Visionary for the full map.\n";

/// Create a workspace folder: `modproject.json` (if missing), `assets/`,
/// `reference/`, `romfs/` plus a README explaining the manual drop zones.
/// Returns the `modproject.json` path. Idempotent — rerunning keeps edits.
pub fn scaffold_workspace(workspace: &Path, name: &str) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(workspace)?;
    std::fs::create_dir_all(workspace.join("assets"))?;
    std::fs::create_dir_all(workspace.join("reference"))?;
    std::fs::create_dir_all(workspace.join("romfs"))?;
    let readme = workspace.join("README.txt");
    if !readme.is_file() {
        std::fs::write(&readme, WORKSPACE_README)?;
    }
    let file = workspace_file(workspace);
    if !file.is_file() {
        let project = crate::mod_project::ModProjectFile {
            version: crate::mod_project::PROJECT_VERSION,
            name: crate::mod_export::slugify(name),
            ..Default::default()
        };
        let json = serde_json::to_string_pretty(&project)?;
        std::fs::write(&file, json)?;
    }
    Ok(file)
}

/// Merge a workspace `romfs/` overlay into an exported mod folder.
///
/// Copies every file under `overlay` into `dest` preserving arc-relative
/// paths. Files the generated export already wrote are skipped (generated
/// wins) and returned as conflicts so the export reports them instead of
/// silently overwriting in either direction. Returns `(copied, skipped)`.
pub fn merge_romfs_overlay(overlay: &Path, dest: &Path) -> anyhow::Result<(usize, Vec<String>)> {
    let mut copied = 0usize;
    let mut skipped = Vec::new();
    if !overlay.is_dir() {
        return Ok((copied, skipped));
    }
    let mut stack = vec![overlay.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(overlay) else {
                continue;
            };
            let out = dest.join(rel);
            if out.exists() {
                skipped.push(
                    rel.components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
                continue;
            }
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, &out)?;
            copied += 1;
        }
    }
    skipped.sort();
    skipped.dedup();
    Ok((copied, skipped))
}

/// How an edit kind is handled by the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSupport {
    /// Edited in Visionary, stored in `modproject.json` / `assets/`.
    Supported,
    /// Copied on import for reading, never exported.
    Reference,
    /// Not modelled: drop into `romfs/` overlay, ships verbatim on export.
    Manual,
}

/// One row of the workspace map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub kind: &'static str,
    pub support: WorkspaceSupport,
    /// Where it lives in the workspace (relative).
    pub workspace_path: &'static str,
    /// Example game path it ships as.
    pub game_path: &'static str,
    pub notes: &'static str,
}

/// Every edit class and where it goes — including the ones the tool does not
/// support (models, animations, …). Single source for the Project Files panel.
pub fn workspace_map() -> Vec<WorkspaceEntry> {
    vec![
        WorkspaceEntry {
            kind: "Fighter moves (ACMD)",
            support: WorkspaceSupport::Supported,
            workspace_path: "modproject.json",
            game_path: "plugin.nro (built on export)",
            notes: "Hitboxes, effects, sounds per move.",
        },
        WorkspaceEntry {
            kind: "Fighter params",
            support: WorkspaceSupport::Supported,
            workspace_path: "modproject.json",
            game_path: "fighter/common/param/fighter_param.prc",
            notes: "Weight, speeds, jumps — sparse diffs.",
        },
        WorkspaceEntry {
            kind: "Roster order / visibility / row fields",
            support: WorkspaceSupport::Supported,
            workspace_path: "modproject.json",
            game_path: "ui/param/database/ui_chara_db.prc",
            notes: "Rebuilt from base + overrides on export.",
        },
        WorkspaceEntry {
            kind: "Display names",
            support: WorkspaceSupport::Supported,
            workspace_path: "modproject.json",
            game_path: "ui/message/msg_name.xmsbt",
            notes: "Adopted from .xmsbt on import.",
        },
        WorkspaceEntry {
            kind: "Portraits / stock icons",
            support: WorkspaceSupport::Supported,
            workspace_path: "assets/roster_ui/...",
            game_path: "ui/replace/chara/.../*.bntx",
            notes: "PNGs managed by Visionary.",
        },
        WorkspaceEntry {
            kind: "Effect textures",
            support: WorkspaceSupport::Supported,
            workspace_path: "assets/textures/...",
            game_path: "effect pool BNTX",
            notes: "Replacements + additions.",
        },
        WorkspaceEntry {
            kind: "Source reference",
            support: WorkspaceSupport::Reference,
            workspace_path: "reference/...",
            game_path: "—",
            notes: "Copied on import for reading; never exported.",
        },
        WorkspaceEntry {
            kind: "Models",
            support: WorkspaceSupport::Manual,
            workspace_path: "romfs/fighter/<fighter>/model/...",
            game_path: "fighter/<fighter>/model/...",
            notes: "Not modelled — drop .numdlb/.numshb/.nusktb here; ships verbatim.",
        },
        WorkspaceEntry {
            kind: "Animations",
            support: WorkspaceSupport::Manual,
            workspace_path: "romfs/fighter/<fighter>/motion/...",
            game_path: "fighter/<fighter>/motion/...",
            notes: "Not modelled — drop .nuanmb/.nusmab here; ships verbatim.",
        },
        WorkspaceEntry {
            kind: "Sound / streams",
            support: WorkspaceSupport::Manual,
            workspace_path: "romfs/sound/...",
            game_path: "sound/...",
            notes: "Not modelled — ships verbatim.",
        },
        WorkspaceEntry {
            kind: "Binary effects / messages",
            support: WorkspaceSupport::Manual,
            workspace_path: "romfs/effect/... · romfs/ui/message/...",
            game_path: "effect/... · ui/message/*.msbt",
            notes: "Compiled .eff/.msbt are reference-only; manual overrides ship verbatim.",
        },
        WorkspaceEntry {
            kind: "Compiled plugins",
            support: WorkspaceSupport::Reference,
            workspace_path: "reference/...",
            game_path: "*.nro",
            notes: "Reference-only by design; editor shows vanilla scripts.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"{}").unwrap();
    }

    #[test]
    fn cold_start_with_no_config_offers_new_open_import_and_browse() {
        let dir = config();
        let hub = HubState::cold_start(dir.path());
        assert!(hub.show, "the hub must appear on launch");
        assert!(hub.last.is_none());
        assert!(hub.recent.is_empty());
        let titles: Vec<&str> = hub.choices().iter().map(|c| c.title()).collect();
        assert_eq!(
            titles,
            vec!["New", "Open", "Import mod", "Browse without project"]
        );
    }

    #[test]
    fn resume_leads_and_recent_skips_the_duplicate() {
        let dir = config();
        let a = dir.path().join("a/modproject.json");
        let b = dir.path().join("b/modproject.json");
        touch(&a);
        touch(&b);
        save_last_project(dir.path(), &a);
        save_recent_projects(dir.path(), &[a.clone(), b.clone()]);

        let hub = HubState::cold_start(dir.path());
        assert_eq!(hub.last.as_deref(), Some(a.as_path()));
        // Recent still holds both, but the choices list Resume once.
        let choices = hub.choices();
        assert!(matches!(&choices[0], HubChoice::ResumeLast(p) if p == &a));
        let recents: Vec<&PathBuf> = choices
            .iter()
            .filter_map(|c| match c {
                HubChoice::Recent(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(recents, vec![&b], "Resume must not duplicate its path");
    }

    #[test]
    fn missing_files_are_dropped_from_resume_and_recents() {
        let dir = config();
        let gone = dir.path().join("gone/modproject.json");
        save_last_project(dir.path(), &gone);
        save_recent_projects(dir.path(), &[gone]);
        let hub = HubState::cold_start(dir.path());
        assert!(
            hub.last.is_none(),
            "a missing Resume target must not linger"
        );
        assert!(hub.recent.is_empty());
    }

    #[test]
    fn save_clears_dirty_and_save_as_is_needed_only_without_a_path() {
        let mut current = CurrentProject::new();
        assert!(current.needs_save_as());
        assert!(!current.is_dirty());
        current.mark_dirty();
        assert!(current.needs_warning());
        let dir = config();
        let path = dir.path().join("modproject.json");
        current.mark_saved(path.clone());
        assert!(!current.is_dirty());
        assert!(!current.needs_save_as());
        assert_eq!(current.path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn hub_switch_warns_only_when_edits_would_be_discarded() {
        let dirty = true;
        assert!(needs_hub_warning(
            dirty,
            &HubAction::Open("/tmp/x.json".into())
        ));
        assert!(needs_hub_warning(dirty, &HubAction::New("/tmp/new".into())));
        assert!(needs_hub_warning(
            dirty,
            &HubAction::ImportMod("/tmp/mod".into())
        ));
        assert!(needs_hub_warning(
            dirty,
            &HubAction::ResumeLast("/tmp/x.json".into())
        ));
        assert!(!needs_hub_warning(dirty, &HubAction::BrowseWithoutProject));
        assert!(!needs_hub_warning(
            false,
            &HubAction::New("/tmp/new".into())
        ));
    }

    #[test]
    fn push_recent_dedupes_caps_and_adopts_last() {
        let dir = config();
        let paths: Vec<PathBuf> = (0..12)
            .map(|i| dir.path().join(format!("p{i}/modproject.json")))
            .collect();
        for p in &paths {
            push_recent(dir.path(), p);
        }
        let recent = load_recent_projects(dir.path());
        assert_eq!(recent.len(), MAX_RECENT_PROJECTS);
        assert_eq!(recent[0], paths[11]);
        // Reopening an older entry moves it to the front without duplicating.
        push_recent(dir.path(), &paths[0]);
        let recent = load_recent_projects(dir.path());
        assert_eq!(recent[0], paths[0]);
        assert_eq!(
            recent.iter().filter(|p| *p == &paths[0]).count(),
            1,
            "reopening must not duplicate the entry"
        );
        assert_eq!(
            load_last_project(dir.path()).as_deref(),
            Some(paths[0].as_path())
        );
    }

    #[test]
    fn save_round_trip_persists_path_and_recent() {
        let dir = config();
        let path = dir.path().join("my_mod/modproject.json");
        touch(&path);
        let mut current = CurrentProject::with_path(path.clone(), "my_mod".into());
        current.mark_dirty();
        // Save adopts the path and clears dirty; hub records it.
        current.mark_saved(path.clone());
        push_recent(dir.path(), &path);
        assert!(!current.is_dirty());

        let hub = HubState::cold_start(dir.path());
        assert_eq!(hub.last.as_deref(), Some(path.as_path()));
        assert!(hub.recent.contains(&path));
    }

    #[test]
    fn new_scaffolds_a_workspace_where_every_edit_has_a_home() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("my_mod");
        let file = scaffold_workspace(&workspace, "My Mod").unwrap();
        assert_eq!(file, workspace.join("modproject.json"));
        assert!(file.is_file(), "New must create modproject.json upfront");
        assert!(workspace.join("assets").is_dir());
        assert!(workspace.join("reference").is_dir());
        assert!(workspace.join("romfs").is_dir());
        assert!(workspace.join("README.txt").is_file());
        // Rerunning keeps edits (idempotent).
        std::fs::write(&file, b"{\"kept\":true}").unwrap();
        let again = scaffold_workspace(&workspace, "My Mod").unwrap();
        assert_eq!(again, file);
        assert_eq!(std::fs::read(&file).unwrap(), b"{\"kept\":true}");
    }

    #[test]
    fn workspace_map_names_manual_drop_zones_for_unsupported_edits() {
        let map = workspace_map();
        let kinds: Vec<&str> = map.iter().map(|e| e.kind).collect();
        for needed in [
            "Models",
            "Animations",
            "Sound / streams",
            "Binary effects / messages",
        ] {
            assert!(
                kinds.contains(&needed),
                "workspace map must show where {needed} go: {kinds:?}"
            );
        }
        let models = map.iter().find(|e| e.kind == "Models").unwrap();
        assert_eq!(models.support, WorkspaceSupport::Manual);
        assert!(models.workspace_path.contains("romfs/fighter"));
        assert!(models.notes.contains("ships verbatim"));
        let anims = map.iter().find(|e| e.kind == "Animations").unwrap();
        assert_eq!(anims.support, WorkspaceSupport::Manual);
        assert!(anims.workspace_path.contains("motion"));
        // Supported edits keep living in the JSON/assets.
        assert!(map
            .iter()
            .any(|e| e.support == WorkspaceSupport::Supported
                && e.workspace_path == "modproject.json"));
    }

    #[test]
    fn romfs_overlay_merges_manual_files_and_reports_generated_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join("ws/romfs");
        let dest = dir.path().join("mod");
        std::fs::create_dir_all(overlay.join("fighter/mario/model/body/c00")).unwrap();
        std::fs::create_dir_all(overlay.join("fighter/mario/motion/body/c00")).unwrap();
        std::fs::write(
            overlay.join("fighter/mario/model/body/c00/model.numdlb"),
            b"model",
        )
        .unwrap();
        std::fs::write(
            overlay.join("fighter/mario/motion/body/c00/attack.nuanmb"),
            b"anim",
        )
        .unwrap();
        // Generated export already wrote this one — manual must not silently win.
        std::fs::create_dir_all(dest.join("fighter/common/param")).unwrap();
        std::fs::write(
            dest.join("fighter/common/param/fighter_param.prc"),
            b"generated",
        )
        .unwrap();
        std::fs::create_dir_all(overlay.join("fighter/common/param")).unwrap();
        std::fs::write(
            overlay.join("fighter/common/param/fighter_param.prc"),
            b"manual",
        )
        .unwrap();

        let (copied, skipped) = merge_romfs_overlay(&overlay, &dest).unwrap();
        assert_eq!(copied, 2, "model + animation ship verbatim");
        assert_eq!(skipped, vec!["fighter/common/param/fighter_param.prc"]);
        assert!(dest
            .join("fighter/mario/model/body/c00/model.numdlb")
            .is_file());
        assert!(dest
            .join("fighter/mario/motion/body/c00/attack.nuanmb")
            .is_file());
        assert_eq!(
            std::fs::read(dest.join("fighter/common/param/fighter_param.prc")).unwrap(),
            b"generated",
            "generated wins; the conflict is reported, not overwritten"
        );
    }
}
