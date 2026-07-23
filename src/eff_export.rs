// Mod export: rebuild an ef_*.eff with the project's authored edits applied, using the
// effect_library crate (byte-exact writer). The in-tree parser (src/effects.rs) is the
// EDITOR's view; this module maps its edited fields onto effect_library's structs:
//
//   emission_rate → Emission.rate            lifetime      → ParticleData.life (i32)
//   scale         → ParticleScale.scale_x/y/z (ratio vs scale_x, preserves anisotropy)
//   color_scale   → EmitterStatic.color_scale emitter_scale → EmitterInfo.scale_x/y/z
//   color0/color1 → EmitterStatic key tables (key.x/y/z = r/g/b; times untouched)
//   alpha0        → EmitterStatic.alpha0 (value in key.x)
//
// Edited emitters get `cached_binary` cleared — the serializer prefers the cached EMTR
// blob and would otherwise silently drop the edits.

use anyhow::{anyhow, Context, Result};

use crate::mod_project::{AuthoredEdit, EffMod, EmitterFieldEdits, OneSlotOp};

/// Rebuild the source .eff bytes with all authored edits applied. `donor_root` resolves
/// cross-file one-slot donors (the ArcExplorer export root the `src_file_rel`s are
/// relative to); pass None to restrict to same-file duplication.
/// Applies EVERY one-slot op regardless of costume scoping (the preview view).
pub fn rebuild_eff_bytes(
    src_bytes: &[u8],
    eff: &EffMod,
    donor_root: Option<&std::path::Path>,
) -> Result<Vec<u8>> {
    rebuild_eff_bytes_filtered(src_bytes, eff, donor_root, |_| true)
}

/// Slot-scoped rebuild for export: `slot` None = the base file (only costume-unscoped
/// ops); Some(s) = the ef_*_c0s.eff file (unscoped ops + ops scoped to that slot —
/// the slotted file replaces the base for that costume, so it must carry both).
pub fn rebuild_eff_bytes_for_slot(
    src_bytes: &[u8],
    eff: &EffMod,
    donor_root: Option<&std::path::Path>,
    slot: Option<u8>,
) -> Result<Vec<u8>> {
    rebuild_eff_bytes_filtered(src_bytes, eff, donor_root, |op| match slot {
        None => op.slots.is_empty(),
        Some(s) => op.slots.is_empty() || op.slots.contains(&s),
    })
}

fn rebuild_eff_bytes_filtered(
    src_bytes: &[u8],
    eff: &EffMod,
    donor_root: Option<&std::path::Path>,
    keep: impl Fn(&OneSlotOp) -> bool,
) -> Result<Vec<u8>> {
    let mut namco = effect_library::NamcoEffectFile::load(src_bytes)
        .context("effect_library failed to parse the source .eff")?;
    // One-slot ops FIRST (they only append sets, so pre-existing set indices are
    // stable), then authored edits — which may target a freshly cloned set by name
    // (editing the copy in the eff editor). Cross-fighter donors bake in here too: the
    // merged file replaces the player's own (resident) eff at boot, so it renders.
    for op in eff.one_slot.iter().filter(|op| keep(op)) {
        let same_file = op.src_file_rel.is_empty() || op.src_file_rel == eff.source_rel;
        if same_file {
            apply_one_slot_same_file(&mut namco, op)?;
        } else {
            let root = donor_root.ok_or_else(|| {
                anyhow!(
                    "one-slot '{}': cross-file donor '{}' needs the export root",
                    op.new_entry_name,
                    op.src_file_rel
                )
            })?;
            let donor_bytes = std::fs::read(root.join(&op.src_file_rel)).with_context(|| {
                format!("cross-file one-slot donor '{}' unreadable", op.src_file_rel)
            })?;
            let donor = effect_library::NamcoEffectFile::load(&donor_bytes)
                .context("effect_library failed to parse the donor .eff")?;
            apply_one_slot_cross_file(&mut namco, &donor, op)?;
        }
    }
    {
        let ptcl = namco
            .ptcl_file
            .as_mut()
            .ok_or_else(|| anyhow!("source .eff has no embedded PTCL"))?;
        for edit in &eff.authored {
            apply_authored(ptcl, edit)?;
        }
    }
    namco
        .save()
        .context("effect_library failed to re-encode the .eff")
}

/// Duplicate a donor entry's emitter set(s) within the same eff under a new entry name —
/// the ACMD side then retargets the fighter's call to `new_entry_name`, leaving the
/// original entry vanilla for everyone else.
fn apply_one_slot_same_file(
    namco: &mut effect_library::NamcoEffectFile,
    op: &OneSlotOp,
) -> Result<()> {
    if op.replace_entry.is_none()
        && namco
            .entry_names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&op.new_entry_name))
    {
        anyhow::bail!(
            "one-slot target name '{}' already exists",
            op.new_entry_name
        );
    }
    // Case-insensitive: the studio's donor list mixes original-case file entries with
    // lowercase live kinds (hash40 is over the lowercase name).
    let donor_idx = namco
        .entry_names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(&op.src_set_name))
        .ok_or_else(|| {
            anyhow!(
                "one-slot donor entry '{}' not found in this eff",
                op.src_set_name
            )
        })?;
    let donor = namco.entries[donor_idx].clone();

    let ptcl = namco
        .ptcl_file
        .as_mut()
        .ok_or_else(|| anyhow!("eff has no embedded PTCL"))?;
    let sets = &mut ptcl.emitter_list.emitter_sets;

    // NOTE: the on-disk entry/variant ids are 1-BASED handles (0 = none) — see
    // eff_lib's EffectHandle docs and `entry_set_id_is_one_based`. Reads subtract 1;
    // writes store `len` (= appended 0-based index + 1).
    let clone_set =
        |raw_id: u32, sets: &mut Vec<effect_library::structs::EmitterSet>| -> Result<u32> {
            let idx = (raw_id as usize)
                .checked_sub(1)
                .ok_or_else(|| anyhow!("donor entry has no emitter set (id 0)"))?;
            let src = sets
                .get(idx)
                .ok_or_else(|| anyhow!("donor emitter set id {raw_id} out of range"))?
                .clone();
            let mut new_set = src;
            new_set.name = op.new_entry_name.clone();
            sets.push(new_set);
            Ok(sets.len() as u32) // raw 1-based handle of the appended set
        };

    let mut new_entry = donor.clone();
    if donor.variant_count == 0 {
        new_entry.emitter_set_id = clone_set(donor.emitter_set_id, sets)?;
    } else {
        // Multi-part effect: clone every variant's set and append a new variant block.
        let start = (donor.variant_start_idx as usize)
            .checked_sub(1)
            .ok_or_else(|| anyhow!("donor variant start id 0"))?;
        let count = donor.variant_count as usize;
        let donor_variants: Vec<effect_library::namco_file::EffectVariant> = namco
            .effect_variants
            .get(start..start + count)
            .ok_or_else(|| anyhow!("donor variant range out of bounds"))?
            .to_vec();
        let new_start = namco.effect_variants.len() as u16 + 1; // raw 1-based
        for v in donor_variants {
            let new_set_id = clone_set(v.emitter_set_id as u32, sets)?;
            namco
                .effect_variants
                .push(effect_library::namco_file::EffectVariant {
                    start_frame: v.start_frame,
                    emitter_set_id: new_set_id as u16,
                });
        }
        new_entry.variant_start_idx = new_start;
    }
    finish_one_slot_entry(namco, op, new_entry)
}

/// Append `new_entry` under the op's new name, or — replace mode — repoint an existing
/// entry at the cloned set(s) (its name and slot in the table stay; the old set is
/// orphaned but harmless). Replace is the costume-scoped semantic: every ACMD use of
/// the entry switches on that costume with no redirect needed.
fn finish_one_slot_entry(
    namco: &mut effect_library::NamcoEffectFile,
    op: &OneSlotOp,
    new_entry: effect_library::namco_file::EffectHeader,
) -> Result<()> {
    match &op.replace_entry {
        None => {
            namco.entries.push(new_entry);
            namco.entry_names.push(op.new_entry_name.clone());
        }
        Some(target) => {
            let ti = namco
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(target))
                .ok_or_else(|| {
                    anyhow!("one-slot replace target entry '{target}' not found in this eff")
                })?;
            // Take the donor's header (kind/flags describe how its set plays) but keep
            // the entry's name so every existing ACMD call keeps resolving.
            namco.entries[ti] = new_entry;
        }
    }
    Ok(())
}

/// Copy a donor entry from ANOTHER eff into this one: clone the emitter set(s), transfer
/// the textures they reference (by GUID, merged into this file's BNTX), append the donor's
/// shader variations (BNSH merge) and remap the cloned emitters' shader indices. Primitives
/// (rare) are not transferable yet — referencing one bails with a clear error.
fn apply_one_slot_cross_file(
    namco: &mut effect_library::NamcoEffectFile,
    donor: &effect_library::NamcoEffectFile,
    op: &OneSlotOp,
) -> Result<()> {
    if op.replace_entry.is_none()
        && namco
            .entry_names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&op.new_entry_name))
    {
        anyhow::bail!(
            "one-slot target name '{}' already exists",
            op.new_entry_name
        );
    }
    // Case-insensitive: live-kind donors arrive lowercase while file entries are
    // original-case (the hash40 name is the lowercase form).
    let donor_idx = donor
        .entry_names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(&op.src_set_name))
        .ok_or_else(|| {
            anyhow!(
                "one-slot donor entry '{}' not found in '{}'",
                op.src_set_name,
                op.src_file_rel
            )
        })?;
    let donor_entry = donor.entries[donor_idx].clone();
    let donor_ptcl = donor
        .ptcl_file
        .as_ref()
        .ok_or_else(|| anyhow!("donor eff has no embedded PTCL"))?;

    // Gather the donor emitter set(s) this entry uses (variants included).
    // Entry/variant ids are 1-BASED handles (0 = none) — see `entry_set_id_is_one_based`.
    let mut donor_sets: Vec<effect_library::structs::EmitterSet> = Vec::new();
    let mut variant_frames: Vec<u16> = Vec::new();
    if donor_entry.variant_count == 0 {
        let idx = (donor_entry.emitter_set_id as usize)
            .checked_sub(1)
            .ok_or_else(|| anyhow!("donor entry has no emitter set (id 0)"))?;
        donor_sets.push(
            donor_ptcl
                .emitter_list
                .emitter_sets
                .get(idx)
                .ok_or_else(|| anyhow!("donor emitter set out of range"))?
                .clone(),
        );
    } else {
        let start = (donor_entry.variant_start_idx as usize)
            .checked_sub(1)
            .ok_or_else(|| anyhow!("donor variant start id 0"))?;
        let count = donor_entry.variant_count as usize;
        for v in donor
            .effect_variants
            .get(start..start + count)
            .ok_or_else(|| anyhow!("donor variant range out of bounds"))?
        {
            let idx = (v.emitter_set_id as usize)
                .checked_sub(1)
                .ok_or_else(|| anyhow!("donor variant has no emitter set (id 0)"))?;
            donor_sets.push(
                donor_ptcl
                    .emitter_list
                    .emitter_sets
                    .get(idx)
                    .ok_or_else(|| anyhow!("donor variant set out of range"))?
                    .clone(),
            );
            variant_frames.push(v.start_frame);
        }
    }

    // Collect the resources the donor emitters reference.
    let mut tex_ids: Vec<u64> = Vec::new();
    let mut uses_shader = false;
    let mut uses_compute = false;
    let mut prim_ids: Vec<u64> = Vec::new();
    for set in &donor_sets {
        visit_emitters_ref(&set.emitters, &mut |em| {
            let d = &em.data;
            for s in [&d.sampler0, &d.sampler1, &d.sampler2]
                .into_iter()
                .flatten()
            {
                // 0 and u64::MAX are "no texture" sentinels.
                if s.texture_id != 0 && s.texture_id != u64::MAX && !tex_ids.contains(&s.texture_id)
                {
                    tex_ids.push(s.texture_id);
                }
            }
            if d.shader_references.shader_index >= 0
                || d.shader_references.user_shader_index1 >= 0
                || d.shader_references.user_shader_index2 >= 0
                || d.shader_references.custom_shader_index >= 0
            {
                uses_shader = true;
            }
            if d.shader_references.compute_shader_index >= 0 {
                uses_compute = true;
            }
            for pid in [
                d.particle_data.primitive_id,
                d.particle_data.primitive_ex_id,
                d.shape_info.primitive_index,
            ] {
                // 0 and u64::MAX are "no primitive" sentinels (see descriptor_index_for_id).
                if pid != 0 && pid != u64::MAX && !prim_ids.contains(&pid) {
                    prim_ids.push(pid);
                }
            }
        });
    }

    // Primitives: the PRMA section = descriptor table (GUID + per-model attribute
    // indices) + a multi-model BFRES container, with descriptor index i ↔ model index i
    // (verified against the crate's dumper/creator). Emitters reference primitives by
    // GUID, so appending donor models + their descriptors in lockstep needs no emitter
    // rewrites. Cases:
    //  (a) target already has every referenced GUID → nothing to do;
    //  (b) target has NO primitive table → transplant the donor's whole PRMA section;
    //  (c) BOTH have tables → extract each missing donor primitive as a single-model
    //      BFRES and append (this was the "sys donor into fighter" ⚠ failure).
    if !prim_ids.is_empty() {
        let dest_has = |id: u64| {
            namco
                .ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.as_ref())
                .map(|pi| pi.descriptors.iter().any(|d| d.id == id))
                .unwrap_or(false)
        };
        let dest_has_table = namco
            .ptcl_file
            .as_ref()
            .and_then(|p| p.primitive_info.as_ref())
            .map(|pi| !pi.descriptors.is_empty())
            .unwrap_or(false);
        let missing: Vec<u64> = prim_ids
            .iter()
            .copied()
            .filter(|id| !dest_has(*id))
            .collect();
        if !missing.is_empty() {
            let donor_prim = donor_ptcl.primitive_info.as_ref().ok_or_else(|| {
                anyhow!("donor references primitives but has no PRMA section to transfer")
            })?;
            let dest_ptcl = namco
                .ptcl_file
                .as_mut()
                .ok_or_else(|| anyhow!("target eff has no embedded PTCL"))?;
            if !dest_has_table {
                // Safe transplant: take the donor's whole PRMA section.
                dest_ptcl.primitive_info = Some(donor_prim.clone());
            } else {
                let donor_bin = donor_prim
                    .binary_data
                    .as_ref()
                    .ok_or_else(|| anyhow!("donor PRMA has descriptors but no BFRES binary"))?;
                let dest_prim = dest_ptcl.primitive_info.as_mut().unwrap();
                let dest_count = dest_prim.descriptors.len();
                let mut files: Vec<Vec<u8>> =
                    vec![dest_prim.binary_data.clone().unwrap_or_default()];
                for id in &missing {
                    let idx = effect_library::bfres::descriptor_index_for_id(
                        &donor_prim.descriptors,
                        *id,
                    )
                    .ok_or_else(|| anyhow!("donor primitive {id:#x} has no descriptor"))?;
                    let blob = effect_library::bfres::export_single_model(donor_bin, idx)
                        .with_context(|| format!("extracting donor primitive {id:#x}"))?;
                    files.push(blob);
                    dest_prim
                        .descriptors
                        .push(donor_prim.descriptors[idx].clone());
                }
                let merged = effect_library::bfres::ResFile::merge_model_files(&files)
                    .context("merging donor primitives into the target PRMA")?;
                // Sanity: descriptor index i must still map to model index i — a model
                // NAME collision would make the container replace-instead-of-append and
                // silently desync every primitive after it.
                let last = dest_count + missing.len() - 1;
                if effect_library::bfres::export_single_model(&merged, last).is_err()
                    || effect_library::bfres::export_single_model(&merged, last + 1).is_ok()
                {
                    anyhow::bail!(
                        "one-slot '{}': donor primitive model name collides with the target's \
                         (PRMA merge would desync) — not merged",
                        op.new_entry_name
                    );
                }
                dest_prim.binary_data = Some(merged);
            }
        }
    }

    // Textures: copy missing GUIDs (descriptor + single-texture BNTX merged into ours).
    {
        let dest_ptcl = namco
            .ptcl_file
            .as_mut()
            .ok_or_else(|| anyhow!("target eff has no embedded PTCL"))?;
        let donor_tex = donor_ptcl.texture_info.as_ref();
        let dest_tex = dest_ptcl
            .texture_info
            .as_mut()
            .ok_or_else(|| anyhow!("target eff has no texture section"))?;
        let mut new_blobs: Vec<Vec<u8>> = Vec::new();
        for id in &tex_ids {
            if dest_tex.descriptors.iter().any(|d| d.id == *id) {
                continue;
            }
            let donor_tex = donor_tex
                .ok_or_else(|| anyhow!("donor eff has no texture section but emitters sample"))?;
            let idx = donor_tex
                .descriptors
                .iter()
                .position(|d| d.id == *id)
                .ok_or_else(|| anyhow!("donor texture {id:#x} has no descriptor"))?;
            let name = donor_tex.descriptors[idx].name.clone();
            let donor_bin = donor_tex
                .binary_data
                .as_ref()
                .ok_or_else(|| anyhow!("donor texture section has no binary"))?;
            // bntx extraction is file-path based — round-trip through the scratch dir.
            let tmp = crate::scratch_dirs::app_scratch_dir("oneslot-tex")?;
            let path = tmp.path().join(format!("{name}.bntx"));
            effect_library::bntx::export_single_texture(donor_bin, idx, &name, &path)
                .with_context(|| format!("extracting donor texture '{name}'"))?;
            new_blobs.push(std::fs::read(&path)?);
            dest_tex
                .descriptors
                .push(effect_library::ptcl_file::TextureDescriptor { id: *id, name });
        }
        if !new_blobs.is_empty() {
            let mut files: Vec<Vec<u8>> = Vec::with_capacity(new_blobs.len() + 1);
            files.push(dest_tex.binary_data.clone().unwrap_or_default());
            files.extend(new_blobs);
            dest_tex.binary_data = Some(
                effect_library::bntx::merge_texture_files(&files)
                    .context("merging donor textures into the target BNTX")?,
            );
        }
    }

    // Shaders: append the donor's whole variation container after ours and shift the
    // cloned emitters' indices by our original count. Correct (indices stay valid);
    // costs some file size, which a one-slot can afford.
    let mut shader_index_base: i32 = 0;
    let mut compute_index_base: i32 = 0;
    if uses_shader || uses_compute {
        let dest_ptcl = namco.ptcl_file.as_mut().unwrap();
        let dest_shader = dest_ptcl
            .shader_info
            .as_mut()
            .ok_or_else(|| anyhow!("target eff has no shader section"))?;
        let donor_shader = donor_ptcl
            .shader_info
            .as_ref()
            .ok_or_else(|| anyhow!("donor eff has no shader section"))?;
        if uses_shader {
            let dest_bin = dest_shader.binary_data.clone().unwrap_or_default();
            let donor_bin = donor_shader
                .binary_data
                .as_ref()
                .ok_or_else(|| anyhow!("donor shader section has no binary"))?;
            shader_index_base = effect_library::bnsh::BnshFile::read(&dest_bin)
                .map(|f| f.variations.len() as i32)
                .unwrap_or(0);
            dest_shader.binary_data = Some(
                effect_library::bnsh::merge_variation_files(&[dest_bin, donor_bin.clone()])
                    .context("merging donor shader variations")?,
            );
        }
        if uses_compute {
            let dest_bin = dest_shader.compute_binary.clone().unwrap_or_default();
            let donor_bin = donor_shader
                .compute_binary
                .as_ref()
                .ok_or_else(|| anyhow!("donor compute-shader section has no binary"))?;
            compute_index_base = if dest_bin.is_empty() {
                0
            } else {
                effect_library::bnsh::BnshFile::read(&dest_bin)
                    .map(|f| f.variations.len() as i32)
                    .unwrap_or(0)
            };
            dest_shader.compute_binary = Some(
                effect_library::bnsh::merge_variation_files(&[dest_bin, donor_bin.clone()])
                    .context("merging donor compute shaders")?,
            );
        }
    }

    // Shader-remap the donor emitters (shaders are index-referenced; textures/primitives are
    // GUID-referenced against the merged pools, so they need no remap).
    let remap = |set: &mut effect_library::structs::EmitterSet| {
        visit_emitters(&mut set.emitters, &mut 0, &mut |_, em| {
            let s = &mut em.data.shader_references;
            for idx in [
                &mut s.shader_index,
                &mut s.user_shader_index1,
                &mut s.user_shader_index2,
                &mut s.custom_shader_index,
            ] {
                if *idx >= 0 {
                    *idx += shader_index_base;
                }
            }
            if s.compute_shader_index >= 0 {
                s.compute_shader_index += compute_index_base;
            }
            em.cached_binary = None; // indices moved → cached EMTR blob is stale
        });
    };

    // LIVE REPLACE-IN-PLACE: replacing a single-set target with a single-set donor overwrites
    // the target entry's OWN emitter set in place — the entry name + set index are unchanged,
    // so `req(name)` (already registered at fighter-load) keeps resolving and a mid-match
    // reparse rebuilds that set with the donor's content → renders LIVE with NO re-entry and
    // no new-kind registration (the wall that makes appended `_os` entries NOT-FOUND live).
    if let Some(target) = &op.replace_entry {
        if donor_entry.variant_count == 0 {
            let ti = namco
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(target))
                .ok_or_else(|| anyhow!("replace target entry '{target}' not found in this eff"))?;
            let tgt = &namco.entries[ti];
            if tgt.variant_count == 0 {
                let tset = (tgt.emitter_set_id as usize)
                    .checked_sub(1)
                    .ok_or_else(|| anyhow!("replace target '{target}' has no emitter set"))?;
                let mut donor_set = donor_sets.into_iter().next().unwrap();
                remap(&mut donor_set);
                let dest_ptcl = namco.ptcl_file.as_mut().unwrap();
                let sets = &mut dest_ptcl.emitter_list.emitter_sets;
                let keep_name = sets
                    .get(tset)
                    .map(|s| s.name.clone())
                    .ok_or_else(|| anyhow!("replace target set {tset} out of range"))?;
                donor_set.name = keep_name; // keep the target set's name; only its content changes
                sets[tset] = donor_set;
                return Ok(());
            }
        }
        // Fall through to append for multi-variant targets/donors (registration caveat applies).
    }

    // Append path (new named entry). NOTE: an APPENDED entry does NOT register as a spawnable
    // kind mid-match (`req` NOT-FOUND) — it only becomes visible on a fresh fighter load. For
    // live cross-fighter, prefer replace-in-place above.
    let dest_ptcl = namco.ptcl_file.as_mut().unwrap();
    let sets = &mut dest_ptcl.emitter_list.emitter_sets;
    let mut new_set_ids: Vec<u32> = Vec::new();
    for mut set in donor_sets {
        set.name = op.new_entry_name.clone();
        remap(&mut set);
        sets.push(set);
        new_set_ids.push(sets.len() as u32); // raw 1-based handle
    }

    let mut new_entry = donor_entry;
    // The donor's external-model handle indexes the DONOR file's model table — it
    // dangles here. Cross-file model transfer is unsupported; drop the reference.
    new_entry.external_model_idx = 0;
    // Non-fighter donors (assists/items — e.g. ef_alucard) mark entries type 0 in the kind
    // u16's high byte; fighter tables use 0x01xx for particle entries, and the fighter
    // spawn path REJECTS type-0 entries: the kind resolves (name registered, entry found)
    // but never instantiates — verified live: kirby_dash entry=0x0100 spawns, the appended
    // alucard entry=0x0000 returns handle 0. Normalize to the particle type.
    if new_entry.kind & 0xff00 == 0 {
        new_entry.kind |= 0x0100;
    }
    if new_entry.variant_count == 0 {
        new_entry.emitter_set_id = new_set_ids[0];
    } else {
        let new_start = namco.effect_variants.len() as u16 + 1; // raw 1-based
        for (frame, set_id) in variant_frames.iter().zip(&new_set_ids) {
            namco
                .effect_variants
                .push(effect_library::namco_file::EffectVariant {
                    start_frame: *frame,
                    emitter_set_id: *set_id as u16,
                });
        }
        new_entry.variant_start_idx = new_start;
    }
    finish_one_slot_entry(namco, op, new_entry)
}

/// Read-only recursive emitter visit (children after parent).
fn visit_emitters_ref<F: FnMut(&effect_library::structs::Emitter)>(
    emitters: &[effect_library::structs::Emitter],
    f: &mut F,
) {
    for em in emitters {
        f(em);
        visit_emitters_ref(&em.children, f);
    }
}

fn apply_authored(ptcl: &mut effect_library::PtclFile, edit: &AuthoredEdit) -> Result<()> {
    let sets = &mut ptcl.emitter_list.emitter_sets;
    let set_idx = sets
        .iter()
        .position(|s| !edit.set_name.is_empty() && s.name == edit.set_name)
        .unwrap_or(edit.set_idx);
    let set = sets.get_mut(set_idx).ok_or_else(|| {
        anyhow!(
            "emitter set '{}' (idx {}) not found in source eff",
            edit.set_name,
            edit.set_idx
        )
    })?;
    if !edit.set_name.is_empty() && set.name != edit.set_name {
        eprintln!(
            "[EFF-EXPORT] warning: set name mismatch ('{}' vs '{}') — applying by index {}",
            set.name, edit.set_name, set_idx
        );
    }

    // Flat, parent-first traversal (matches the editor's per-set emitter enumeration).
    let mut flat_idx = 0usize;
    let mut applied = false;
    visit_emitters(&mut set.emitters, &mut flat_idx, &mut |idx, em| {
        if applied {
            return;
        }
        let name = em.data.display_name();
        let by_name = !edit.emitter_name.is_empty() && name == edit.emitter_name;
        let by_idx = edit.emitter_name.is_empty() && idx == edit.emitter_idx;
        if by_name || by_idx {
            apply_fields(em, &edit.fields);
            applied = true;
        }
    });
    if !applied {
        // Name lookup failed (renamed dump?) — retry strictly by stored index.
        let mut flat_idx = 0usize;
        visit_emitters(&mut set.emitters, &mut flat_idx, &mut |idx, em| {
            if !applied && idx == edit.emitter_idx {
                eprintln!(
                    "[EFF-EXPORT] warning: emitter '{}' not found by name in set '{}' — using index {}",
                    edit.emitter_name, set.name, edit.emitter_idx
                );
                apply_fields(em, &edit.fields);
                applied = true;
            }
        });
    }
    if !applied {
        anyhow::bail!(
            "emitter '{}' (idx {}) not found in set '{}'",
            edit.emitter_name,
            edit.emitter_idx,
            set.name
        );
    }
    Ok(())
}

/// Depth-first, parent-before-children traversal with a running flat index.
fn visit_emitters<F: FnMut(usize, &mut effect_library::structs::Emitter)>(
    emitters: &mut [effect_library::structs::Emitter],
    idx: &mut usize,
    f: &mut F,
) {
    for em in emitters.iter_mut() {
        f(*idx, em);
        *idx += 1;
        visit_emitters(&mut em.children, idx, f);
    }
}

fn apply_fields(em: &mut effect_library::structs::Emitter, f: &EmitterFieldEdits) {
    let d = &mut em.data;
    if let Some(v) = f.emission_rate {
        d.emission.rate = v;
    }
    if let Some(v) = f.lifetime {
        d.particle_data.life = v.round() as i32;
    }
    if let Some(v) = f.scale {
        // The editor's scalar tracks ParticleScale.scale_x; apply the ratio to all axes so
        // authored anisotropy (scale_y/z ≠ scale_x) is preserved.
        let base = d.particle_scale.scale_x;
        if base.abs() > 1e-6 {
            let r = v / base;
            d.particle_scale.scale_x = v;
            d.particle_scale.scale_y *= r;
            d.particle_scale.scale_z *= r;
        } else {
            d.particle_scale.scale_x = v;
            d.particle_scale.scale_y = v;
            d.particle_scale.scale_z = v;
        }
    }
    if let Some(v) = f.color_scale {
        d.emitter_static.color_scale = v;
    }
    if let Some(v) = f.emitter_scale {
        d.emitter_info.scale_x = v[0];
        d.emitter_info.scale_y = v[1];
        d.emitter_info.scale_z = v[2];
    }
    // Color precedence mirrors the editor's read path (`effects.rs`): a
    // ParticleColor "Constant" type overrides the key table; an empty key table falls
    // back to the EmitterInfo static color. Write to whichever source the editor showed.
    if let Some(rows) = &f.color0 {
        if matches!(
            d.particle_color.color0_type,
            effect_library::ColorType::Constant
        ) {
            if let Some(row) = rows.first() {
                d.particle_color.color0_r = row[0];
                d.particle_color.color0_g = row[1];
                d.particle_color.color0_b = row[2];
            }
        } else if d.emitter_static.num_color0_keys > 0 {
            for (k, row) in d.emitter_static.color0.keys.iter_mut().zip(rows) {
                k.x = row[0];
                k.y = row[1];
                k.z = row[2];
            }
        } else if let Some(row) = rows.first() {
            d.emitter_info.color0_r = row[0];
            d.emitter_info.color0_g = row[1];
            d.emitter_info.color0_b = row[2];
        }
    }
    if let Some(rows) = &f.color1 {
        if matches!(
            d.particle_color.color1_type,
            effect_library::ColorType::Constant
        ) {
            if let Some(row) = rows.first() {
                d.particle_color.color1_r = row[0];
                d.particle_color.color1_g = row[1];
                d.particle_color.color1_b = row[2];
            }
        } else if d.emitter_static.num_color1_keys > 0 {
            for (k, row) in d.emitter_static.color1.keys.iter_mut().zip(rows) {
                k.x = row[0];
                k.y = row[1];
                k.z = row[2];
            }
        } else if let Some(row) = rows.first() {
            d.emitter_info.color1_r = row[0];
            d.emitter_info.color1_g = row[1];
            d.emitter_info.color1_b = row[2];
        }
    }
    if let Some(rows) = &f.alpha0 {
        for (k, row) in d.emitter_static.alpha0.keys.iter_mut().zip(rows) {
            k.x = row[0];
        }
    }
    // Serializer prefers the cached EMTR blob; clearing it forces a re-encode of `data`.
    em.cached_binary = None;
}
