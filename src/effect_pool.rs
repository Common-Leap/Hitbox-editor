//! Persistent pool of effect entries across ALL eff files under the export root.
//!
//! Backs the Transplant studio's donor picker: every ef_*.eff's entry names, scanned
//! incrementally (a few files per frame) and cached by (mtime, size) in
//! `{app_storage_root}/eff-entry-cache.json` so subsequent launches are instant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PoolFile {
    mtime: u64,
    size: u64,
    entries: Vec<String>,
}

pub struct EffectPool {
    root: PathBuf,
    /// rel path (forward slashes) → cached entry list.
    cache: HashMap<String, PoolFile>,
    /// Files still awaiting a scan this session.
    queue: Vec<PathBuf>,
    queued: bool,
    total: usize,
    dirty: bool,
}

/// The entries belonging to one source directory in the transplant picker.
///
/// The source directory is kept as the stable identity rather than only exposing its display
/// label: two different effect kinds can legitimately end up with the same human-readable name,
/// and the transplant operation still needs the original relative file paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EffectSourceGroup {
    pub(crate) source_dir: String,
    pub(crate) entries: Vec<(String, String)>,
}

fn cache_path() -> PathBuf {
    // v3: entry names are canonicalized to lowercase (v2 caches kept the file's
    // original case, so old scans still showed UPPERCASE names in the pickers).
    crate::scratch_dirs::app_storage_root().join("eff-entry-cache-v3.json")
}

fn file_stamp(path: &Path) -> (u64, u64) {
    let meta = std::fs::metadata(path).ok();
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let size = meta.map(|m| m.len()).unwrap_or(0);
    (mtime, size)
}

impl EffectPool {
    pub fn new(root: PathBuf) -> Self {
        let cache = std::fs::read_to_string(cache_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            root,
            cache,
            queue: Vec::new(),
            queued: false,
            total: 0,
            dirty: false,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn rel_of(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Queue every .eff under the root exactly once per session; cached-and-unchanged
    /// files are skipped immediately.
    fn ensure_queue(&mut self) {
        if self.queued {
            return;
        }
        self.queued = true;
        let mut files: Vec<PathBuf> = Vec::new();
        walk_effs(&self.root.join("effect"), &mut files);
        if files.is_empty() {
            walk_effs(&self.root, &mut files);
        }
        self.total = files.len();
        for f in files {
            let rel = self.rel_of(&f);
            let (mtime, size) = file_stamp(&f);
            let fresh = self
                .cache
                .get(&rel)
                .map(|c| c.mtime == mtime && c.size == size)
                .unwrap_or(false);
            if !fresh {
                self.queue.push(f);
            }
        }
    }

    /// Scan up to `budget` files (call once per UI frame while the picker is open).
    /// Returns true while scanning is still in progress.
    pub fn tick(&mut self, budget: usize) -> bool {
        self.ensure_queue();
        for _ in 0..budget {
            let Some(path) = self.queue.pop() else { break };
            let rel = self.rel_of(&path);
            let (mtime, size) = file_stamp(&path);
            let entries = crate::effects::EffIndex::from_file(&path)
                .map(|idx| entry_names_deduped(&idx))
                .unwrap_or_default();
            self.cache.insert(
                rel,
                PoolFile {
                    mtime,
                    size,
                    entries,
                },
            );
            self.dirty = true;
        }
        if self.queue.is_empty() && self.dirty {
            self.dirty = false;
            if let Ok(json) = serde_json::to_string(&self.cache) {
                let _ = std::fs::write(cache_path(), json);
            }
        }
        !self.queue.is_empty()
    }

    /// (files scanned, files total) for the progress label.
    pub fn progress(&self) -> (usize, usize) {
        (
            self.total.saturating_sub(self.queue.len()),
            self.total.max(self.cache.len()),
        )
    }

    /// Case-insensitive entry search across every scanned file: (file rel, entry name).
    /// Exact (case-insensitive) entry-name lookup → the eff file (rel path) holding it.
    pub fn file_of_entry(&self, name: &str) -> Option<String> {
        let want = name.to_lowercase();
        for (rel, file) in &self.cache {
            if file.entries.iter().any(|e| e.to_lowercase() == want) {
                return Some(rel.clone());
            }
        }
        None
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(String, String)> {
        let q = query.to_lowercase();
        let mut out: Vec<(String, String)> = Vec::new();
        let mut rels: Vec<&String> = self.cache.keys().collect();
        rels.sort();
        'outer: for rel in rels {
            for name in &self.cache[rel].entries {
                if q.is_empty() || name.to_lowercase().contains(&q) {
                    out.push((rel.clone(), name.clone()));
                    if out.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
        out
    }

    /// Case-insensitive search grouped by the parent directory of each EFF file.
    ///
    /// Unlike [`Self::search`], this intentionally has no global result cap: the collapsible
    /// source sections make the full pool navigable without forcing every entry into the layout
    /// at once. The source directories and entries are sorted so the picker does not jump around
    /// as the cache is populated.
    pub(crate) fn search_grouped(&self, query: &str) -> Vec<EffectSourceGroup> {
        let q = query.to_lowercase();
        let mut grouped: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut rels: Vec<&String> = self.cache.keys().collect();
        rels.sort();
        for rel in rels {
            let source_dir = source_dir_of_rel(rel);
            for name in &self.cache[rel].entries {
                if q.is_empty() || name.to_lowercase().contains(&q) {
                    grouped
                        .entry(source_dir.clone())
                        .or_default()
                        .push((rel.clone(), name.clone()));
                }
            }
        }

        let mut source_dirs: Vec<String> = grouped.keys().cloned().collect();
        source_dirs.sort();
        source_dirs
            .into_iter()
            .map(|source_dir| {
                let mut entries = grouped.remove(&source_dir).unwrap_or_default();
                entries.sort();
                EffectSourceGroup {
                    source_dir,
                    entries,
                }
            })
            .collect()
    }

    /// All entry names of one file (donor listing for the currently selected source).
    pub fn entries_of(&self, rel: &str) -> Vec<String> {
        self.cache
            .get(rel)
            .map(|c| c.entries.clone())
            .unwrap_or_default()
    }

    /// Index an eff file explicitly (e.g. one imported from outside the export root) so
    /// its entries show up in donor search and the effect-name picker. Returns the key
    /// (rel path if under the root, otherwise the absolute path) that `entries_of`/transplant
    /// resolution use. Persists the cache immediately.
    pub fn add_file(&mut self, path: &Path) -> String {
        let rel = self.rel_of(path);
        let (mtime, size) = file_stamp(path);
        let fresh = self
            .cache
            .get(&rel)
            .map(|c| c.mtime == mtime && c.size == size)
            .unwrap_or(false);
        if !fresh {
            let entries = crate::effects::EffIndex::from_file(path)
                .map(|idx| entry_names_deduped(&idx))
                .unwrap_or_default();
            self.cache.insert(
                rel.clone(),
                PoolFile {
                    mtime,
                    size,
                    entries,
                },
            );
            if let Ok(json) = serde_json::to_string(&self.cache) {
                let _ = std::fs::write(cache_path(), json);
            }
        }
        rel
    }
}

/// One name per entry: `EffIndex.handles` deliberately carries an original-case AND a
/// lowercase key for every entry — listing both showed each effect twice in the studio.
/// Canonicalize to LOWERCASE: it matches the live-kind names the game link reports and
/// the case ACMD hashes against, so the same effect reads identically everywhere.
fn entry_names_deduped(idx: &crate::effects::EffIndex) -> Vec<String> {
    let mut names: Vec<String> = idx
        .handles
        .keys()
        .map(|name| name.to_lowercase())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    names.sort();
    names
}

/// Return the normalized parent directory used as a source-group identity.
pub(crate) fn source_dir_of_rel(rel: &str) -> String {
    let normalized = rel.replace('\\', "/");
    normalized
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or(normalized)
}

fn walk_effs(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_effs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("eff") {
            // Skip the transient transplant preview eff the editor writes next to a fighter eff.
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map(crate::scratch_dirs::is_transplant_preview_name)
                .unwrap_or(false)
            {
                continue;
            }
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_with_files(files: &[(&str, &[&str])]) -> EffectPool {
        let cache = files
            .iter()
            .map(|(rel, entries)| {
                (
                    (*rel).to_string(),
                    PoolFile {
                        mtime: 0,
                        size: 0,
                        entries: entries.iter().map(|entry| (*entry).to_string()).collect(),
                    },
                )
            })
            .collect();
        EffectPool {
            root: PathBuf::from("root"),
            cache,
            queue: Vec::new(),
            queued: true,
            total: files.len(),
            dirty: false,
        }
    }

    #[test]
    fn grouped_search_collects_costume_files_under_one_source() {
        let pool = pool_with_files(&[
            (
                "effect/fighter/kirby/ef_kirby_c01.eff",
                &["kirby_c01", "kirby_shared"],
            ),
            ("effect/system/item/ef_item.eff", &["item_b", "item_a"]),
            ("effect/fighter/kirby/ef_kirby.eff", &["kirby_base"]),
        ]);

        let groups = pool.search_grouped("");
        assert_eq!(
            groups
                .iter()
                .map(|group| group.source_dir.as_str())
                .collect::<Vec<_>>(),
            vec!["effect/fighter/kirby", "effect/system/item"]
        );
        assert_eq!(
            groups[0].entries,
            vec![
                (
                    "effect/fighter/kirby/ef_kirby.eff".to_string(),
                    "kirby_base".to_string()
                ),
                (
                    "effect/fighter/kirby/ef_kirby_c01.eff".to_string(),
                    "kirby_c01".to_string()
                ),
                (
                    "effect/fighter/kirby/ef_kirby_c01.eff".to_string(),
                    "kirby_shared".to_string()
                ),
            ]
        );
    }

    #[test]
    fn grouped_search_filters_without_dropping_later_sources() {
        let pool = pool_with_files(&[
            ("effect/fighter/pickel/ef_pickel.eff", &["steve_tnt"]),
            ("effect/system/common/ef_common.eff", &["sys_bomb"]),
        ]);

        let groups = pool.search_grouped("sys_");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].source_dir, "effect/system/common");
        assert_eq!(groups[0].entries[0].1, "sys_bomb");
    }

    #[test]
    fn source_directory_normalizes_separators() {
        assert_eq!(
            source_dir_of_rel("effect\\fighter\\pickel\\ef_pickel.eff"),
            "effect/fighter/pickel"
        );
        assert_eq!(source_dir_of_rel("ef_common.eff"), "ef_common.eff");
    }
}
