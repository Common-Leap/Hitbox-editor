//! Access the game's loaded ARC/search tables and patch raw heap buffers (SSBU 13.0.4).
//!
//! Ported from the original effect viewer's `resource_reload.rs`. Uses the real
//! `smash-arc` crate structs (not hand-computed offsets) to resolve a game path to its
//! search index / loaded-data buffer.

use smash_arc::{ArcLookup, Hash40, Region, SearchLookup};

// SSBU 13.0.4 — pointer-to-FilesystemInfo (smashline `resources::types::FilesystemInfo::instance`).
const FILESYSTEM_INFO_PTR_OFFSET: usize = 0x5331f20;

#[repr(C)]
struct CppVector {
    start: *mut u8,
    end: *mut u8,
    eos: *mut u8,
}

#[repr(C)]
struct LoadedFilepath {
    loaded_data_index: u32,
    is_loaded: u32,
}

#[repr(C)]
struct LoadedData {
    data: *const u8,
}

#[repr(C)]
struct PathInformation {
    arc: *mut smash_arc::LoadedArc,
    search: *mut smash_arc::LoadedSearchSection,
}

/// Mirrors `resources::types::FilesystemInfo` (13.0.4).
#[repr(C)]
struct FilesystemInfo {
    _mutex: *mut u8,
    loaded_filepaths: *mut LoadedFilepath,
    loaded_datas: *mut LoadedData,
    loaded_filepath_len: u32,
    loaded_data_len: u32,
    loaded_filepath_count: u32,
    loaded_data_count: u32,
    _loaded_filepath_list: CppVector,
    _loaded_directories: *const u8,
    _loaded_directory_len: u32,
    _unk: u32,
    _unk2: CppVector,
    _unk3: u8,
    _unk4: [u8; 7],
    _addr: *const u8,
    path_info: *mut PathInformation,
    _version: u32,
}

fn filesystem_info() -> Option<&'static FilesystemInfo> {
    let text =
        unsafe { skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as *const u8 };
    let fs_info_ptr = unsafe {
        text.add(FILESYSTEM_INFO_PTR_OFFSET)
            .cast::<*const FilesystemInfo>()
    };
    let fs_info = unsafe { *fs_info_ptr };
    if fs_info.is_null() {
        return None;
    }
    Some(unsafe { &*fs_info })
}

pub fn loaded_arc() -> Option<&'static smash_arc::LoadedArc> {
    let fs_info = filesystem_info()?;
    if fs_info.path_info.is_null() {
        return None;
    }
    let path_info = unsafe { &*fs_info.path_info };
    if path_info.arc.is_null() {
        return None;
    }
    Some(unsafe { &*path_info.arc })
}

pub fn loaded_search() -> Option<&'static smash_arc::LoadedSearchSection> {
    let fs_info = filesystem_info()?;
    if fs_info.path_info.is_null() {
        return None;
    }
    let path_info = unsafe { &*fs_info.path_info };
    if path_info.search.is_null() {
        return None;
    }
    Some(unsafe { &*path_info.search })
}

pub fn search_index_for_path(game_path: &str) -> Option<u32> {
    search_index_for_path_hash(smash::hash40(game_path))
}

/// Resolved path-list index (`path_list_indices[hash_to_index.index()]`).
pub fn search_index_for_path_hash(path_hash: u64) -> Option<u32> {
    let hash = Hash40(path_hash);
    if let Some(search) = loaded_search() {
        if let Ok(idx) = search.get_path_list_index_from_hash(hash) {
            return Some(idx);
        }
    }
    loaded_arc()?.get_path_list_index_from_hash(hash).ok()
}

/// Raw `HashToIndex.index()` (position in `path_list_indices`) — the space smashline's
/// effect-transplant passes to `load_effects`. May or may not equal the path-list index;
/// callers calibrate against a search_index the GAME itself was observed to use.
pub fn hash_to_index_for_path_hash(path_hash: u64) -> Option<u32> {
    let hash = Hash40(path_hash);
    if let Some(search) = loaded_search() {
        if let Ok(hti) = search.get_path_index_from_hash(hash) {
            return Some(hti.index());
        }
    }
    loaded_arc()?
        .get_path_index_from_hash(hash)
        .ok()
        .map(|hti| hti.index())
}

/// Point the game's resident-data slot for `file_hash` at a caller-owned, self-contained
/// EFFN buffer, so `load_effects` (which reads `LoadedData[FileToLoad[fp].data_index]`)
/// sees it WITHOUT the res loading thread ever running. This is the deterministic way to
/// make an absent (e.g. DLC) donor's effect resident: we supply the bytes, so there's no
/// dependency on the loading thread or a mounted DLC region.
///
/// `buffer` must outlive the effect (leak it) and start with `EFFN`. Returns the loaded-
/// data index touched, or None if the file has no path/slot in the arc. LoadedData entries
/// are 0x18 bytes in 13.0.4 (buffer@+0, refcount@+8, inuse@+0xc); the typed `LoadedData`
/// mirror here is only the +0 pointer, so the entry address is computed with raw 0x18 stride.
/// Result of a fill attempt, for diagnostics.
pub enum FillResult {
    Filled(u32, u32),
    /// The game hasn't allocated a data slot for this file yet (loaded_data_index invalid).
    NoSlot,
    /// The slot already holds a real buffer — we must NOT overwrite live data.
    Occupied(u32),
    NoArc,
}

pub fn inject_resident_buffer(file_hash: u64, buffer: *const u8) -> FillResult {
    let Some(fs) = filesystem_info() else {
        return FillResult::NoArc;
    };
    let Some(arc) = loaded_arc() else {
        return FillResult::NoArc;
    };
    let Ok(fpi) = arc.get_file_path_index_from_hash(Hash40(file_hash)) else {
        return FillResult::NoArc;
    };
    let fp = fpi.0 as usize;
    if fs.loaded_filepaths.is_null() || fp >= fs.loaded_filepath_len as usize {
        return FillResult::NoArc;
    }
    let _ = std::fs::write(
        "sd:/effect_viewer_inject.txt",
        format!(
            "step=inject_enter fp={fp} fplen={}\n",
            fs.loaded_filepath_len
        ),
    );
    let fpe = unsafe { &mut *fs.loaded_filepaths.add(fp) };
    let di = fpe.loaded_data_index;
    if di == 0x00ff_ffff || fs.loaded_datas.is_null() || di as usize >= fs.loaded_data_len as usize
    {
        return FillResult::NoSlot;
    }
    let entry = unsafe { (fs.loaded_datas as *mut u8).add(di as usize * 0x18) };
    let _ = std::fs::write(
        "sd:/effect_viewer_inject.txt",
        format!(
            "step=inject_read fp={fp} di={di} datalen={}\n",
            fs.loaded_data_len
        ),
    );
    // SAFETY GATE: only fill a slot whose data buffer is NULL — i.e. the game allocated it
    // (via the directory loader) but the read never completed. NEVER overwrite a non-null
    // buffer; that would be a live file and corrupt the game (the earlier freeze).
    let existing = unsafe { *(entry as *const *const u8) };
    let _ = std::fs::write(
        "sd:/effect_viewer_inject.txt",
        format!(
            "step=inject_read_ok di={di} existing_null={}\n",
            existing.is_null()
        ),
    );
    if !existing.is_null() {
        return FillResult::Occupied(di);
    }
    let _ = std::fs::write(
        "sd:/effect_viewer_inject.txt",
        format!("step=inject_write_start di={di}\n"),
    );
    unsafe {
        *(entry as *mut *const u8) = buffer; // +0  data buffer
        *(entry.add(8) as *mut i32) = 0x4000_0000; // +8  refcount — huge so it's never freed
        *entry.add(0xc) = 1; // +0xc in-use flag
        fpe.is_loaded = 1;
    }
    let _ = std::fs::write(
        "sd:/effect_viewer_inject.txt",
        format!("step=inject_write_done di={di}\n"),
    );
    FillResult::Filled(fp as u32, di)
}

/// The current resident buffer pointer for `file_hash` (LoadedData[di].data), or None.
/// Used to call the effect-set builder on the game's OWN vanilla bytes as a threading test.
pub fn resident_buffer(file_hash: u64) -> Option<*const u8> {
    let fs = filesystem_info()?;
    let arc = loaded_arc()?;
    let fp = arc.get_file_path_index_from_hash(Hash40(file_hash)).ok()?.0 as usize;
    if fs.loaded_filepaths.is_null() || fp >= fs.loaded_filepath_len as usize {
        return None;
    }
    let di = unsafe { (*fs.loaded_filepaths.add(fp)).loaded_data_index };
    if di == 0x00ff_ffff || fs.loaded_datas.is_null() || di as usize >= fs.loaded_data_len as usize
    {
        return None;
    }
    let entry = unsafe { (fs.loaded_datas as *const u8).add(di as usize * 0x18) };
    let ptr = unsafe { *(entry as *const *const u8) };
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

/// The arc table's decomp size for `file_hash` — the length of the resident buffer the
/// game allocated for it (arcropolis patches this up when a bigger mod file replaces it).
pub fn resident_len(file_hash: u64) -> Option<usize> {
    let arc = loaded_arc()?;
    let info = arc.get_file_info_from_hash(Hash40(file_hash)).ok()?;
    Some(arc.get_file_data(info, Region::UsEnglish).decomp_size as usize)
}

/// DELIBERATELY repoint the resident-data slot for `file_hash` at a caller-owned buffer,
/// whether or not the slot currently holds a live buffer. Unlike [`inject_resident_buffer`]
/// (which refuses to touch a non-null slot), this is the live-re-read primitive: the caller
/// MUST have just `unload_effects`'d the owning handle (so the effect manager no longer
/// parses/renders the old buffer) and MUST NOT free the old buffer (it may still be pointed
/// at elsewhere — leaking it is a few MB, a UAF is a freeze). Returns (fp, di, old_was_null).
///
/// `buffer` outlives the effect (leak it), starts with `EFFN`, and is >= the arc table's
/// decomp size for this file (the generic callback patches that size up to the merged size,
/// so the game's own re-read would allocate this much — matching it keeps load_effects'
/// bounds valid).
pub fn repoint_resident_buffer(file_hash: u64, buffer: *const u8) -> Option<(u32, u32, bool)> {
    let fs = filesystem_info()?;
    let arc = loaded_arc()?;
    let fpi = arc.get_file_path_index_from_hash(Hash40(file_hash)).ok()?;
    let fp = fpi.0 as usize;
    if fs.loaded_filepaths.is_null() || fp >= fs.loaded_filepath_len as usize {
        return None;
    }
    let fpe = unsafe { &mut *fs.loaded_filepaths.add(fp) };
    let di = fpe.loaded_data_index;
    if di == 0x00ff_ffff || fs.loaded_datas.is_null() || di as usize >= fs.loaded_data_len as usize
    {
        return None;
    }
    let entry = unsafe { (fs.loaded_datas as *mut u8).add(di as usize * 0x18) };
    let old_null = unsafe { (*(entry as *const *const u8)).is_null() };
    unsafe {
        *(entry as *mut *const u8) = buffer; // +0    data buffer
        *(entry.add(8) as *mut i32) = 0x4000_0000; // +8    refcount — huge so never freed
        *entry.add(0xc) = 1; // +0xc  in-use flag
        fpe.is_loaded = 1;
    }
    Some((fp as u32, di, old_null))
}

/// Arc `DirInfo` index for a directory path hash — the index space the game's directory
/// loader (`FUN_035407a0`/`FUN_03540860` @ 0x35407a0) works in (NOT the search-path index
/// that `load_effects` takes). Used to REQUEST an absent donor's effect folder resident.
pub fn dir_info_index_for_path_hash(path_hash: u64) -> Option<u32> {
    let arc = loaded_arc()?;
    let hash = Hash40(path_hash);
    let table = arc.get_dir_hash_to_info_index();
    let pos = table.binary_search_by_key(&hash, |d| d.hash40()).ok()?;
    Some(table[pos].index())
}

/// Enumerate every child FILE's path hash in a DirInfo group (by dir index). Used to arcrop-fill
/// the donor's SUB-resources (the textures/models its eff references) directly — the non-blocking
/// alternative to waiting for the async worker to make the whole folder resident (which never
/// completes mid-match, and driving the drain ourselves hangs — build ak).
pub fn dir_child_file_hashes(dir_index: u32) -> Vec<u64> {
    let mut out = Vec::new();
    let Some(arc) = loaded_arc() else { return out };
    let dir_infos = arc.get_dir_infos();
    let Some(dir) = dir_infos.get(dir_index as usize) else {
        return out;
    };
    let file_infos = arc.get_file_infos();
    let file_paths = arc.get_file_paths();
    for fi_idx in dir.file_info_range() {
        let Some(fi) = file_infos.get(fi_idx) else {
            continue;
        };
        let fp_idx = fi.file_path_index.0 as usize;
        if let Some(fp) = file_paths.get(fp_idx) {
            out.push(fp.path.hash40().0);
        }
    }
    out
}

pub fn path_hash_for_search_index(search_index: u32) -> Option<u64> {
    if let Some(search) = loaded_search() {
        let entry = search.get_path_list().get(search_index as usize)?;
        return Some(entry.path.hash40().0);
    }
    let arc = loaded_arc()?;
    let entry = arc.get_path_list().get(search_index as usize)?;
    Some(entry.path.hash40().0)
}

/// Patch a raw ARC heap buffer in place (works for textures/models, NOT parsed .eff
/// containers — those need effect_reload's unload/load reparse). Reads the redirected
/// bytes back through arcrop_load_file into the resident buffer.
pub fn replace_loaded_file(path_hash: u64) -> bool {
    let fs_info = match filesystem_info() {
        Some(f) => f,
        None => return false,
    };
    if fs_info.path_info.is_null() {
        return false;
    }
    let path_info = unsafe { &*fs_info.path_info };
    if path_info.arc.is_null() {
        return false;
    }
    let arc = unsafe { &*path_info.arc };
    let hash = Hash40(path_hash);
    let file_info = match arc.get_file_info_from_hash(hash) {
        Ok(info) => info,
        Err(_) => return false,
    };
    let filepath_index = file_info.file_path_index.0 as usize;
    let data_index = file_info.file_info_indice_index.0 as usize;
    let loaded_filepaths = unsafe {
        std::slice::from_raw_parts(
            fs_info.loaded_filepaths,
            fs_info.loaded_filepath_len as usize,
        )
    };
    let loaded_datas = unsafe {
        std::slice::from_raw_parts(fs_info.loaded_datas, fs_info.loaded_data_len as usize)
    };
    if filepath_index >= loaded_filepaths.len() || data_index >= loaded_datas.len() {
        return false;
    }
    if loaded_filepaths[filepath_index].is_loaded == 0 {
        return false;
    }
    let decomp_size = arc.get_file_data(file_info, Region::UsEnglish).decomp_size as usize;
    let data_ptr = loaded_datas[data_index].data;
    if data_ptr.is_null() {
        return false;
    }
    let buffer = unsafe { std::slice::from_raw_parts_mut(data_ptr as *mut u8, decomp_size) };
    let out_size = match crate::slight::effect_viewer::arcrop::load_file(path_hash, buffer) {
        Some(n) => n,
        None => return false,
    };
    crate::slight::diag::note(format!(
        "resource_reload: refreshed in-memory file {path_hash:#x} ({out_size} B)"
    ));
    true
}

/// Diagnostic view of the RESIDENT (in-memory) copy of a loaded file: the arc table's
/// decomp size, the first bytes, and whether `needle` occurs in the buffer. Tells us
/// whether the game's loaded data is vanilla or our merged bytes (one-slot entry names
/// end in `_os`, so `b"_os\0"` only exists in a merged eff's string table).
pub fn resident_probe(path_hash: u64, needle: &[u8]) -> Option<(usize, [u8; 8], bool)> {
    let fs_info = filesystem_info()?;
    if fs_info.path_info.is_null() {
        return None;
    }
    let path_info = unsafe { &*fs_info.path_info };
    if path_info.arc.is_null() {
        return None;
    }
    let arc = unsafe { &*path_info.arc };
    let file_info = arc.get_file_info_from_hash(Hash40(path_hash)).ok()?;
    let filepath_index = file_info.file_path_index.0 as usize;
    let data_index = file_info.file_info_indice_index.0 as usize;
    let loaded_filepaths = unsafe {
        std::slice::from_raw_parts(
            fs_info.loaded_filepaths,
            fs_info.loaded_filepath_len as usize,
        )
    };
    let loaded_datas = unsafe {
        std::slice::from_raw_parts(fs_info.loaded_datas, fs_info.loaded_data_len as usize)
    };
    if filepath_index >= loaded_filepaths.len()
        || data_index >= loaded_datas.len()
        || loaded_filepaths[filepath_index].is_loaded == 0
    {
        return None;
    }
    let decomp_size = arc.get_file_data(file_info, Region::UsEnglish).decomp_size as usize;
    let data_ptr = loaded_datas[data_index].data;
    if data_ptr.is_null() {
        return None;
    }
    let buffer = unsafe { std::slice::from_raw_parts(data_ptr, decomp_size) };
    let mut head = [0u8; 8];
    let n = head.len().min(buffer.len());
    head[..n].copy_from_slice(&buffer[..n]);
    let found = !needle.is_empty() && buffer.windows(needle.len()).any(|w| w == needle);
    Some((decomp_size, head, found))
}

pub fn debug_line() -> String {
    let arc_ok = loaded_arc().is_some();
    let search_ok = loaded_search().is_some();
    let kirby_idx = search_index_for_path("effect/fighter/kirby/ef_kirby.eff").unwrap_or(u32::MAX);
    format!(
        "arc_lookup_ok={arc_ok} search_lookup_ok={search_ok} kirby_eff_search_index={kirby_idx}"
    )
}
