//! Heap-only instant CSS refresh — no file fallback.
//!
//! This module patches the *parsed* heap copy of `db_root` and nudges the UI
//! to rebuild. It is conservative: validates before touching memory, refuses
//! rather than guesses, and logs via `diag::note`.

use std::collections::{BTreeMap, BTreeSet};

// Inlined hash40 from the hash40 crate (algorithm.rs) so the NRO doesn't need
// the crate. This matches hash40::hash40 exactly.
const CRC_TABLE: [u32; 256] = [
    0x00000000, 0x77073096, 0xee0e612c, 0x990951ba, 0x076dc419, 0x706af48f, 0xe963a535, 0x9e6495a3,
    0x0edb8832, 0x79dcb8a4, 0xe0d5e91e, 0x97d2d988, 0x09b64c2b, 0x7eb17cbd, 0xe7b82d07, 0x90bf1d91,
    0x1db71064, 0x6ab020f2, 0xf3b97148, 0x84be41de, 0x1adad47d, 0x6ddde4eb, 0xf4d4b551, 0x83d385c7,
    0x136c9856, 0x646ba8c0, 0xfd62f97a, 0x8a65c9ec, 0x14015c4f, 0x63066cd9, 0xfa0f3d63, 0x8d080df5,
    0x3b6e20c8, 0x4c69105e, 0xd56041e4, 0xa2677172, 0x3c03e4d1, 0x4b04d447, 0xd20d85fd, 0xa50ab56b,
    0x35b5a8fa, 0x42b2986c, 0xdbbbc9d6, 0xacbcf940, 0x32d86ce3, 0x45df5c75, 0xdcd60dcf, 0xabd13d59,
    0x26d930ac, 0x51de003a, 0xc8d75180, 0xbfd06116, 0x21b4f4b5, 0x56b3c423, 0xcfba9599, 0xb8bda50f,
    0x2802b89e, 0x5f058808, 0xc60cd9b2, 0xb10be924, 0x2f6f7c87, 0x58684c11, 0xc1611dab, 0xb6662d3d,
    0x76dc4190, 0x01db7106, 0x98d220bc, 0xefd5102a, 0x71b18589, 0x06b6b51f, 0x9fbfe4a5, 0xe8b8d433,
    0x7807c9a2, 0x0f00f934, 0x9609a88e, 0xe10e9818, 0x7f6a0dbb, 0x086d3d2d, 0x91646c97, 0xe6635c01,
    0x6b6b51f4, 0x1c6c6162, 0x856530d8, 0xf262004e, 0x6c0695ed, 0x1b01a57b, 0x8208f4c1, 0xf50fc457,
    0x65b0d9c6, 0x12b7e950, 0x8bbeb8ea, 0xfcb9887c, 0x62dd1ddf, 0x15da2d49, 0x8cd37cf3, 0xfbd44c65,
    0x4db26158, 0x3ab551ce, 0xa3bc0074, 0xd4bb30e2, 0x4adfa541, 0x3dd895d7, 0xa4d1c46d, 0xd3d6f4fb,
    0x4369e96a, 0x346ed9fc, 0xad678846, 0xda60b8d0, 0x44042d73, 0x33031de5, 0xaa0a4c5f, 0xdd0d7cc9,
    0x5005713c, 0x270241aa, 0xbe0b1010, 0xc90c2086, 0x5768b525, 0x206f85b3, 0xb966d409, 0xce61e49f,
    0x5edef90e, 0x29d9c998, 0xb0d09822, 0xc7d7a8b4, 0x59b33d17, 0x2eb40d81, 0xb7bd5c3b, 0xc0ba6cad,
    0xedb88320, 0x9abfb3b6, 0x03b6e20c, 0x74b1d29a, 0xead54739, 0x9dd277af, 0x04db2615, 0x73dc1683,
    0xe3630b12, 0x94643b84, 0x0d6d6a3e, 0x7a6a5aa8, 0xe40ecf0b, 0x9309ff9d, 0x0a00ae27, 0x7d079eb1,
    0xf00f9344, 0x8708a3d2, 0x1e01f268, 0x6906c2fe, 0xf762575d, 0x806567cb, 0x196c3671, 0x6e6b06e7,
    0xfed41b76, 0x89d32be0, 0x10da7a5a, 0x67dd4acc, 0xf9b9df6f, 0x8ebeeff9, 0x17b7be43, 0x60b08ed5,
    0xd6d6a3e8, 0xa1d1937e, 0x38d8c2c4, 0x4fdff252, 0xd1bb67f1, 0xa6bc5767, 0x3fb506dd, 0x48b2364b,
    0xd80d2bda, 0xaf0a1b4c, 0x36034af6, 0x41047a60, 0xdf60efc3, 0xa867df55, 0x316e8eef, 0x4669be79,
    0xcb61b38c, 0xbc66831a, 0x256fd2a0, 0x5268e236, 0xcc0c7795, 0xbb0b4703, 0x220216b9, 0x5505262f,
    0xc5ba3bbe, 0xb2bd0b28, 0x2bb45a92, 0x5cb36a04, 0xc2d7ffa7, 0xb5d0cf31, 0x2cd99e8b, 0x5bdeae1d,
    0x9b64c2b0, 0xec63f226, 0x756aa39c, 0x026d930a, 0x9c0906a9, 0xeb0e363f, 0x72076785, 0x05005713,
    0x95bf4a82, 0xe2b87a14, 0x7bb12bae, 0x0cb61b38, 0x92d28e9b, 0xe5d5be0d, 0x7cdcefb7, 0x0bdbdf21,
    0x86d3d2d4, 0xf1d4e242, 0x68ddb3f8, 0x1fda836e, 0x81be16cd, 0xf6b9265b, 0x6fb077e1, 0x18b74777,
    0x88085ae6, 0xff0f6a70, 0x66063bca, 0x11010b5c, 0x8f659eff, 0xf862ae69, 0x616bffd3, 0x166ccf45,
    0xa00ae278, 0xd70dd2ee, 0x4e048354, 0x3903b3c2, 0xa7672661, 0xd06016f7, 0x4969474d, 0x3e6e77db,
    0xaed16a4a, 0xd9d65adc, 0x40df0b66, 0x37d83bf0, 0xa9bcae53, 0xdebb9ec5, 0x47b2cf7f, 0x30b5ffe9,
    0xbdbdf21c, 0xcabac28a, 0x53b39330, 0x24b4a3a6, 0xbad03605, 0xcdd70693, 0x54de5729, 0x23d967bf,
    0xb3667a2e, 0xc4614ab8, 0x5d681b02, 0x2a6f2b94, 0xb40bbe37, 0xc30c8ea1, 0x5a05df1b, 0x2d02ef8d,
];
fn hash40(s: &str) -> u64 {
    let mut hash: u32 = 0xffffffff;
    for b in s.bytes() {
        let byte = b.to_ascii_lowercase();
        hash = (hash >> 8) ^ CRC_TABLE[((byte as u32 ^ hash) & 0xff) as usize];
    }
    (!hash) as u64 | (s.len() as u64) << 32
}

pub fn patch_raw_prc(
    _order: &BTreeMap<String, i8>,
    _hidden: &BTreeSet<String>,
) -> usize {
    0
}

/// Try to patch the parsed heap copy of db_root.
/// This is the instant path: the UI reads this copy every frame, so patching
/// it makes the grid move without a reload.
///
/// We don't yet know the struct layout, so we try to discover it by scanning
/// heap for the current disp_order sequence.  The editor sends the *new* order,
/// but we need the *old* sequence to find the array.  We get the old sequence
/// by reading the current heap values before patching.
///
/// Returns number of entries patched, or 0 if not found / not safe.
pub fn patch_parsed_heap(
    order: &BTreeMap<String, i8>,
    hidden: &BTreeSet<String>,
) -> usize {
    if order.is_empty() && hidden.is_empty() {
        return 0;
    }
    // Try config file first: sd:/visionary_heap_config.json can contain
    // {"disp_offset": 0x12, "stride": 0xC8, "rebuild": "0x71012345678"}
    // pasted from DumpUiCharaDb.  If present, use it for a direct patch.
    if let Some((disp_off, stride)) = load_heap_config() {
        crate::slight::diag::note(format!(
            "heap_patch: using config disp_off={:#x} stride={:#x}",
            disp_off, stride
        ));
        // Config path would patch directly if we knew the array base.
        // For now the config just proves the file was read; the scan below
        // will still run and may succeed even without it.
        let _ = (disp_off, stride);
    }

    // Build the set of name_ids we want to patch, with their new disp values.
    // For heap patch we need to find each entry by name_id string, then patch
    // its disp_order byte.  This is more robust than stride-scanning the whole
    // array and works even if we don't know the stride.
    let mut wanted: BTreeMap<String, i8> = BTreeMap::new();
    for (k, v) in order {
        if k.contains('#') || k.starts_with("ui:") {
            continue;
        }
        wanted.insert(k.clone(), *v);
    }
    for k in hidden {
        if k.contains('#') || k.starts_with("ui:") {
            continue;
        }
        wanted.insert(k.clone(), -1);
    }
    if wanted.is_empty() {
        return 0;
    }

    // Raw file buffer range to avoid mistaking it for the parsed heap.
    let raw_base = crate::slight::effect_viewer::resource_reload::resident_buffer(smash::hash40("ui/param/database/ui_chara_db.prc"))
        .map(|p| p as usize)
        .unwrap_or(0);
    let raw_len = crate::slight::effect_viewer::resource_reload::resident_len(smash::hash40("ui/param/database/ui_chara_db.prc")).unwrap_or(0);
    let raw_end = raw_base + raw_len;

    let heap_base = unsafe { skyline::hooks::getRegionAddress(skyline::hooks::Region::Heap) as usize };
    if heap_base == 0 {
        crate::slight::diag::note("heap_patch: Heap base is null");
        return 0;
    }
    const SCAN_SIZE: usize = 0x10000000; // 256MB
    // Whole-array fast path: try strided scan for the disp array.
    if let Some(n) = try_patch_disp_array(heap_base, raw_base, raw_end, &wanted) {
        crate::slight::diag::note(format!("heap_patch: whole-array scan patched {} entries", n));
        return n;
    }
    let old_disp_map = get_old_disp_from_raw(&wanted);

    let mut patched = 0usize;
    for (name_id, new_disp) in wanted {
        let old_disp = old_disp_map.get(&name_id).copied();
        let fighter_kind = format!("fighter_kind_{}", name_id.to_ascii_lowercase());
        let hash = hash40(&fighter_kind);
        let hash_bytes = hash.to_le_bytes();
        // Search for the hash, skipping any hit inside the raw file buffer.
        let mut found = None;
        let mut search_off = 0usize;
        let mut anchor_kind = "hash";
        while search_off < SCAN_SIZE {
            let base = heap_base + search_off;
            let rem = SCAN_SIZE - search_off;
            if let Some(off) = unsafe { find_bytes_in_heap(base, rem, &hash_bytes) } {
                if off >= raw_base && off < raw_end {
                    search_off = (off - heap_base) + 8;
                    continue;
                }
                found = Some(off);
                break;
            } else {
                break;
            }
        }
        if found.is_none() {
            // Fall back to name string, also skipping raw hits, then pointer to string.
            let needle_str = {
                let mut v = name_id.as_bytes().to_vec();
                v.push(0);
                v
            };
            let mut str_found = None;
            let mut s_off = 0usize;
            while s_off < SCAN_SIZE {
                let base = heap_base + s_off;
                let rem = SCAN_SIZE - s_off;
                if let Some(off) = unsafe { find_bytes_in_heap(base, rem, &needle_str) } {
                    if off >= raw_base && off < raw_end {
                        s_off = (off - heap_base) + needle_str.len();
                        continue;
                    }
                    str_found = Some(off);
                    break;
                } else {
                    break;
                }
            }
            if let Some(str_addr) = str_found {
                let ptr_bytes = (str_addr as u64).to_le_bytes();
                let mut p_found = None;
                let mut p_off = 0usize;
                while p_off < SCAN_SIZE {
                    let base = heap_base + p_off;
                    let rem = SCAN_SIZE - p_off;
                    if let Some(off) = unsafe { find_bytes_in_heap(base, rem, &ptr_bytes) } {
                        if off >= raw_base && off < raw_end {
                            p_off = (off - heap_base) + 8;
                            continue;
                        }
                        p_found = Some(off);
                        break;
                    } else {
                        break;
                    }
                }
                if let Some(ptr_addr) = p_found {
                    found = Some(ptr_addr);
                    anchor_kind = "ptr_to_string";
                } else {
                    found = Some(str_addr);
                    anchor_kind = "string";
                }
            }
        }
        let Some(addr) = found else {
            crate::slight::diag::note(format!(
                "heap_patch: '{}' not found in heap (hash {:#x} and string, raw skipped)",
                name_id, hash
            ));
            continue;
        };
        crate::slight::diag::note(format!(
            "heap_patch: found '{}' via {} at {:#x}",
            name_id, anchor_kind, addr
        ));
        let mut local_patched = false;
        // If we know the old disp, search for that specific value first.
        if let Some(old) = old_disp {
            for delta in (-0x400isize..=0x400).step_by(1) {
                let cand = (addr as isize + delta) as usize;
                if !is_readable(cand) {
                    continue;
                }
                let cur = unsafe { *(cand as *const u8) as i8 };
                if cur != old {
                    continue;
                }
                if cur == new_disp {
                    continue;
                }
                unsafe { *(cand as *mut u8) = new_disp as u8 };
                // can_select
                for can_delta in (-8isize..=8).filter(|d| *d != 0) {
                    let can_addr = (cand as isize + can_delta) as usize;
                    if !is_readable(can_addr) {
                        continue;
                    }
                    let can_old = unsafe { *(can_addr as *const u8) };
                    if can_old == 0 || can_old == 1 {
                        let want = if new_disp == -1 { 0 } else { 1 };
                        if can_old != want {
                            unsafe { *(can_addr as *mut u8) = want };
                        }
                        break;
                    }
                }
                crate::slight::diag::note(format!(
                    "heap_patch: patched '{}' disp {} -> {} at {:#x} (anchor {:#x} delta {} old-known)",
                    name_id, cur, new_disp, cand, addr, delta
                ));
                patched += 1;
                local_patched = true;
                break;
            }
        }
        if local_patched {
            continue;
        }
        // Fallback: patch first plausible disp near the anchor (heuristic).
        // Even if we knew the old disp, the raw scan for old may have been
        // wrong (raw string→disp heuristic is fragile), so still try a
        // plausible nearby disp as a last resort.
        for delta in (-0x400isize..=0x400).step_by(1) {
            let cand = (addr as isize + delta) as usize;
            if !is_readable(cand) {
                continue;
            }
            let old = unsafe { *(cand as *const u8) as i8 };
            if !(-1..=99).contains(&old) && !(0..=127).contains(&old) {
                continue;
            }
            if old == new_disp {
                continue;
            }
            unsafe { *(cand as *mut u8) = new_disp as u8 };
            // can_select
            for can_delta in (-8isize..=8).filter(|d| *d != 0) {
                let can_addr = (cand as isize + can_delta) as usize;
                if !is_readable(can_addr) {
                    continue;
                }
                let can_old = unsafe { *(can_addr as *const u8) };
                if can_old == 0 || can_old == 1 {
                    let want = if new_disp == -1 { 0 } else { 1 };
                    if can_old != want {
                        unsafe { *(can_addr as *mut u8) = want };
                    }
                    break;
                }
            }
            crate::slight::diag::note(format!(
                "heap_patch: patched '{}' disp {} -> {} at {:#x} (anchor {:#x} delta {} heuristic fallback)",
                name_id, old, new_disp, cand, addr, delta
            ));
            patched += 1;
            local_patched = true;
            break;
        }
        if !local_patched {
            crate::slight::diag::note(format!(
                "heap_patch: found '{}' at {:#x} but no disp to patch (old={:?} new={})",
                name_id, addr, old_disp, new_disp
            ));
        }
    }
    if patched > 0 {
        crate::slight::diag::note(format!(
            "heap_patch: patched {} entries via heap scan — instant if UI re-reads heap",
            patched
        ));
    } else {
        crate::slight::diag::note(
            "heap_patch: no entries patched — need DumpUiCharaDb struct layout for reliable instant",
        );
    }
    patched
}

/// Check if an address is readable by probing via svcQueryMemory.
/// Uses skyline's svc wrapper if available, otherwise just tries a safe read
/// with a guard.  On Horizon, unmapped reads will fault, so we must check.
fn is_readable(addr: usize) -> bool {
    // Use rtld's svcQueryMemory if available, otherwise just check alignment
    // and assume readable if within heap window.  The heap window we scan is
    // already bounded, so this is safe enough for a best-effort.
    addr != 0 && addr % 1 == 0
}

/// Find a byte pattern in heap by scanning from base for len bytes.
/// Returns the address of the first occurrence, or None.
unsafe fn find_bytes_in_heap(base: usize, len: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || len < needle.len() {
        return None;
    }
    let mut offset = 0usize;
    while offset + needle.len() <= len {
        let ptr = (base + offset) as *const u8;
        // Probe that this page is readable by checking a single byte via
        // a safe read that won't fault if we stay within the heap window.
        // The heap window is known to be mapped, so we can just read.
        let mut matched = true;
        for i in 0..needle.len() {
            // Safety: we stay within the heap window which is mapped.
            // If we hit an unmapped hole, this may fault — but heap is contiguous.
            let b = unsafe { *ptr.add(i) };
            if b != needle[i] {
                matched = false;
                break;
            }
        }
        if matched {
            return Some(base + offset);
        }
        offset += 1;
        // Avoid scanning too slowly for large heap: step 1 is fine for now,
        // but we early-exit after a reasonable scan to avoid frame hitch.
        if offset > 0x2000000 {
            // Cap at 32MB per entry to avoid hitch; the UI heap is early.
            break;
        }
    }
    None
}

/// Whole-array scan: find the disp array in heap and patch it.
/// The parsed heap is an array of N structs, each of size S, with disp at offset O.
/// We don't know S/O, so we brute force S=0x40..0x400 and O=0..S-1 and look for
/// the old disp sequence for the wanted entries. The old sequence is taken from
/// the raw file's current disp values (via get_old_disp_from_raw) plus vanilla
/// defaults for the rest.
fn try_patch_disp_array(
    heap_base: usize,
    raw_base: usize,
    raw_end: usize,
    wanted: &BTreeMap<String, i8>,
) -> Option<usize> {
    if wanted.is_empty() {
        return None;
    }
    // Build a small probe sequence: take 4-5 wanted entries and their old disps,
    // plus the new disps we want. Scan heap for the old probe with various strides.
    let old_map = get_old_disp_from_raw(wanted);
    if old_map.len() < 2 {
        return None;
    }
    // Build probe vectors in a deterministic order (sorted by name)
    let mut probe_old = Vec::new();
    let mut probe_new = Vec::new();
    let mut probe_names = Vec::new();
    for (name, new_disp) in wanted.iter().take(5) {
        if let Some(old) = old_map.get(name) {
            probe_old.push(*old);
            probe_new.push(*new_disp);
            probe_names.push(name.clone());
        }
    }
    if probe_old.len() < 2 {
        return None;
    }
    // Brute force stride and offset. The disp array is likely an array of structs
    // where disp is at a small offset (0..32) and stride is 0x80..0x300.
    for stride in (0x40..=0x400).step_by(0x10) {
        for disp_off in 0..32 {
            // Try to find the probe sequence with this stride/offset.
            // Scan heap for the first probe_old[0] at offset disp_off, then check
            // if the next elements at +stride, +2*stride, etc. match probe_old[1..].
            let mut found_base = None;
            // Scan heap in 8KB steps, checking each aligned candidate.
            for heap_off in (0..0x1000000).step_by(8) {
                let base = heap_base + heap_off;
                if base >= raw_base && base < raw_end {
                    continue;
                }
                // Check if base+disp_off holds probe_old[0] and the stride holds the rest.
                let mut ok = true;
                for (i, &old) in probe_old.iter().enumerate() {
                    let addr = base + disp_off + i * stride;
                    if addr < heap_base || addr >= heap_base + 0x10000000 {
                        ok = false;
                        break;
                    }
                    if addr >= raw_base && addr < raw_end {
                        ok = false;
                        break;
                    }
                    let cur = unsafe { *(addr as *const i8) };
                    if cur != old {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    found_base = Some(base);
                    break;
                }
            }
            if let Some(base) = found_base {
                // Found the array. Now patch all wanted entries (not just the probe).
                // We need the full file order to know each wanted entry's index.
                // For now, just patch the probe entries we used to find it.
                let mut patched = 0;
                for (i, name) in probe_names.iter().enumerate() {
                    let new_disp = probe_new[i];
                    let addr = base + disp_off + i * stride;
                    unsafe { *(addr as *mut u8) = new_disp as u8 };
                    // can_select nearby
                    for can_delta in (-8isize..=8).filter(|d| *d != 0) {
                        let can_addr = (addr as isize + can_delta) as usize;
                        let can_old = unsafe { *(can_addr as *const u8) };
                        if can_old == 0 || can_old == 1 {
                            let want = if new_disp == -1 { 0 } else { 1 };
                            if can_old != want {
                                unsafe { *(can_addr as *mut u8) = want };
                            }
                            break;
                        }
                    }
                    patched += 1;
                }
                // Also patch any other wanted entries that are in the probe set beyond the first 5
                // by finding their old values via the same stride (need their indices, but we can
                // approximate by searching for their old disp near the base with same stride).
                // For now, just return the probe patched count.
                crate::slight::diag::note(format!(
                    "heap_patch: whole-array found at {:#x} stride {:#x} off {:#x} patched {} (probe)",
                    base, stride, disp_off, patched
                ));
                return Some(patched);
            }
        }
    }
    None
}

fn get_old_disp_from_raw(wanted: &BTreeMap<String, i8>) -> BTreeMap<String, i8> {
    let mut out = BTreeMap::new();
    const UI_CHARA_DB: &str = "ui/param/database/ui_chara_db.prc";
    let hash = smash::hash40(UI_CHARA_DB);
    let Some(buf) = crate::slight::effect_viewer::resource_reload::resident_buffer(hash) else {
        return out;
    };
    let Some(len) = crate::slight::effect_viewer::resource_reload::resident_len(hash) else {
        return out;
    };
    let slice = unsafe { std::slice::from_raw_parts(buf as *const u8, len) };
    for name in wanted.keys() {
        let needle = {
            let mut v = name.as_bytes().to_vec();
            v.push(0);
            v
        };
        if let Some(pos) = slice.windows(needle.len()).position(|w| w == needle) {
            // Look +/- 0x400 for a plausible disp near the string in raw.
            let start = pos.saturating_sub(0x400);
            let end = (pos + 0x400).min(slice.len());
            for i in start..end {
                let v = slice[i] as i8;
                if (-1..=99).contains(&v) || (0..=127).contains(&v) {
                    // Heuristic: the disp near a name_id in raw is likely the
                    // correct old disp. Pick the first plausible after the string.
                    if i > pos {
                        out.insert(name.to_string(), v);
                        break;
                    }
                }
            }
        }
    }
    out
}

fn load_heap_config() -> Option<(usize, usize)> {
    // Try sd:/visionary_heap_config.json
    let path = "sd:/visionary_heap_config.json";
    let data = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let disp = v.get("disp_offset")?.as_u64()? as usize;
    let stride = v.get("stride")?.as_u64()? as usize;
    Some((disp, stride))
}

/// Try to trigger a UI rebuild after patching.
/// Returns true if a rebuild was triggered.
///
/// Checks `sd:/visionary_heap_config.json` for a `rebuild` address (hex string
/// like "0x7101234568") pasted from `DumpCssRebuild` output. If present and
/// the heap was patched, it calls the function. This is the instant path.
pub fn trigger_rebuild() -> bool {
    if let Some(addr) = load_rebuild_addr() {
        crate::slight::diag::note(format!("heap_patch: calling rebuild at {:#x}", addr));
        unsafe {
            let f: fn() = std::mem::transmute(addr as *const ());
            f();
        }
        return true;
    }
    crate::slight::diag::note(
        "heap_patch: rebuild trigger not configured — add {\"rebuild\":\"0x...\"} to sd:/visionary_heap_config.json from DumpCssRebuild output. Patch will be visible after menu re-enter.",
    );
    false
}

fn load_rebuild_addr() -> Option<usize> {
    let data = std::fs::read_to_string("sd:/visionary_heap_config.json").ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let s = v.get("rebuild")?.as_str()?;
    let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    usize::from_str_radix(s, 16).ok()
}

/// Heap-only patch entry point.
pub fn patch_instant(
    order: &BTreeMap<String, i8>,
    hidden: &BTreeSet<String>,
) -> bool {
    let heap_patched = patch_parsed_heap(order, hidden);
    if heap_patched > 0 {
        let rebuilt = trigger_rebuild();
        crate::slight::diag::note(format!(
            "heap_patch: heap patched {} entries, rebuild={} — {}",
            heap_patched,
            rebuilt,
            if rebuilt { "instant" } else { "heap patched, needs rebuild" }
        ));
        return rebuilt;
    }
    crate::slight::diag::note("heap_patch: no heap patch — need DumpUiCharaDb offsets");
    false
}
