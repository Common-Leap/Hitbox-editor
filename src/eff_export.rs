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
pub fn rebuild_eff_bytes(
    src_bytes: &[u8],
    eff: &EffMod,
    donor_root: Option<&std::path::Path>,
) -> Result<Vec<u8>> {
    let mut namco = effect_library::NamcoEffectFile::load(src_bytes)
        .context("effect_library failed to parse the source .eff")?;
    {
        let ptcl = namco
            .ptcl_file
            .as_mut()
            .ok_or_else(|| anyhow!("source .eff has no embedded PTCL"))?;
        for edit in &eff.authored {
            apply_authored(ptcl, edit)?;
        }
    }
    for op in &eff.one_slot {
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
    if namco.entry_names.iter().any(|n| n == &op.new_entry_name) {
        anyhow::bail!("one-slot target name '{}' already exists", op.new_entry_name);
    }
    let donor_idx = namco
        .entry_names
        .iter()
        .position(|n| n == &op.src_set_name)
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

    let mut clone_set = |set_id: usize, sets: &mut Vec<effect_library::structs::EmitterSet>| -> Result<u32> {
        let src = sets
            .get(set_id)
            .ok_or_else(|| anyhow!("donor emitter set id {set_id} out of range"))?
            .clone();
        let mut new_set = src;
        new_set.name = op.new_entry_name.clone();
        sets.push(new_set);
        Ok((sets.len() - 1) as u32)
    };

    let mut new_entry = donor.clone();
    if donor.variant_count == 0 {
        new_entry.emitter_set_id = clone_set(donor.emitter_set_id as usize, sets)?;
    } else {
        // Multi-part effect: clone every variant's set and append a new variant block.
        let start = donor.variant_start_idx as usize;
        let count = donor.variant_count as usize;
        let donor_variants: Vec<effect_library::namco_file::EffectVariant> = namco
            .effect_variants
            .get(start..start + count)
            .ok_or_else(|| anyhow!("donor variant range out of bounds"))?
            .to_vec();
        let new_start = namco.effect_variants.len() as u16;
        for v in donor_variants {
            let new_set_id = clone_set(v.emitter_set_id as usize, sets)?;
            namco.effect_variants.push(effect_library::namco_file::EffectVariant {
                start_frame: v.start_frame,
                emitter_set_id: new_set_id as u16,
            });
        }
        new_entry.variant_start_idx = new_start;
    }
    namco.entries.push(new_entry);
    namco.entry_names.push(op.new_entry_name.clone());
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
    if namco.entry_names.iter().any(|n| n == &op.new_entry_name) {
        anyhow::bail!("one-slot target name '{}' already exists", op.new_entry_name);
    }
    let donor_idx = donor
        .entry_names
        .iter()
        .position(|n| n == &op.src_set_name)
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
    let mut donor_sets: Vec<effect_library::structs::EmitterSet> = Vec::new();
    let mut variant_frames: Vec<u16> = Vec::new();
    if donor_entry.variant_count == 0 {
        donor_sets.push(
            donor_ptcl
                .emitter_list
                .emitter_sets
                .get(donor_entry.emitter_set_id as usize)
                .ok_or_else(|| anyhow!("donor emitter set out of range"))?
                .clone(),
        );
    } else {
        let start = donor_entry.variant_start_idx as usize;
        let count = donor_entry.variant_count as usize;
        for v in donor
            .effect_variants
            .get(start..start + count)
            .ok_or_else(|| anyhow!("donor variant range out of bounds"))?
        {
            donor_sets.push(
                donor_ptcl
                    .emitter_list
                    .emitter_sets
                    .get(v.emitter_set_id as usize)
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
            for s in [&d.sampler0, &d.sampler1, &d.sampler2].into_iter().flatten() {
                if s.texture_id != 0 && !tex_ids.contains(&s.texture_id) {
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
                if pid != 0 && !prim_ids.contains(&pid) {
                    prim_ids.push(pid);
                }
            }
        });
    }

    // Primitives (rare): the donor's PRMA `binary_data` is an opaque buffer table whose
    // descriptors index into it, so two populated tables can't be merged without remapping.
    // Two cases we DO handle: (a) the target already has the GUIDs → nothing to do;
    // (b) the target has NO primitive table → transplant the donor's whole PRMA section
    // wholesale (GUIDs + descriptors + buffers stay self-consistent). Only a genuine
    // collision (target already has a different primitive table) is refused.
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
        let missing: Vec<u64> = prim_ids.iter().copied().filter(|id| !dest_has(*id)).collect();
        if !missing.is_empty() {
            if dest_has_table {
                anyhow::bail!(
                    "one-slot '{}': donor uses {} primitive model(s) and the target already has \
                     a different primitive table — merging distinct primitive tables isn't \
                     supported yet",
                    op.new_entry_name,
                    missing.len()
                );
            }
            // Safe transplant: target has no primitives → take the donor's whole PRMA section.
            let donor_prim = donor_ptcl.primitive_info.clone().ok_or_else(|| {
                anyhow!("donor references primitives but has no PRMA section to transfer")
            })?;
            let dest_ptcl = namco
                .ptcl_file
                .as_mut()
                .ok_or_else(|| anyhow!("target eff has no embedded PTCL"))?;
            dest_ptcl.primitive_info = Some(donor_prim);
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

    // Clone the sets in (renamed), remapping shader indices; then append the entry.
    let dest_ptcl = namco.ptcl_file.as_mut().unwrap();
    let sets = &mut dest_ptcl.emitter_list.emitter_sets;
    let mut new_set_ids: Vec<u32> = Vec::new();
    for mut set in donor_sets {
        set.name = op.new_entry_name.clone();
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
            // Indices moved → the cached EMTR blob is stale.
            em.cached_binary = None;
        });
        sets.push(set);
        new_set_ids.push((sets.len() - 1) as u32);
    }

    let mut new_entry = donor_entry;
    if new_entry.variant_count == 0 {
        new_entry.emitter_set_id = new_set_ids[0];
    } else {
        let new_start = namco.effect_variants.len() as u16;
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
    namco.entries.push(new_entry);
    namco.entry_names.push(op.new_entry_name.clone());
    Ok(())
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
    // Color precedence mirrors the editor's READ path (effect_converter.rs): a
    // ParticleColor "Constant" type overrides the key table; an empty key table falls
    // back to the EmitterInfo static color. Write to whichever source the editor showed.
    if let Some(rows) = &f.color0 {
        if matches!(d.particle_color.color0_type, effect_library::ColorType::Constant) {
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
        if matches!(d.particle_color.color1_type, effect_library::ColorType::Constant) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE: &str = "/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export/effect/fighter/mario/ef_mario.eff";

    /// The writer is not byte-exact against Bandai originals (padding/alignment differ;
    /// the crate's byte-exact guarantee is for its folder-rebuild path). What we require:
    /// (a) a zero-edit rebuild equals a plain load→save (the exporter adds nothing), and
    /// (b) the rebuilt bytes re-parse with identical emitter values AND survive a second
    /// save byte-identically (the writer is a fixpoint — safe to ship).
    #[test]
    fn zero_edit_rebuild_is_stable_and_lossless() {
        if !Path::new(SAMPLE).exists() {
            eprintln!("sample eff missing — skipping");
            return;
        }
        let src = std::fs::read(SAMPLE).unwrap();
        let plain = effect_library::NamcoEffectFile::load(&src).unwrap().save().unwrap();
        let rebuilt = rebuild_eff_bytes(&src, &EffMod::default(), None).unwrap();
        assert!(rebuilt == plain, "zero-edit rebuild differs from plain load→save");

        // Fixpoint: re-load + re-save of the rebuilt file must be byte-identical.
        let reloaded = effect_library::NamcoEffectFile::load(&rebuilt).unwrap();
        assert!(
            reloaded.save().unwrap() == rebuilt,
            "writer is not a fixpoint — second save differs"
        );

        // Semantic: emitter values survive the rebuild.
        let a = effect_library::NamcoEffectFile::load(&src).unwrap();
        let (pa, pb) = (a.ptcl_file.as_ref().unwrap(), reloaded.ptcl_file.as_ref().unwrap());
        assert_eq!(
            pa.emitter_list.emitter_sets.len(),
            pb.emitter_list.emitter_sets.len()
        );
        for (sa, sb) in pa.emitter_list.emitter_sets.iter().zip(&pb.emitter_list.emitter_sets) {
            assert_eq!(sa.name, sb.name);
            assert_eq!(sa.emitters.len(), sb.emitters.len());
            for (ea, eb) in sa.emitters.iter().zip(&sb.emitters) {
                assert_eq!(ea.data.display_name(), eb.data.display_name());
                assert_eq!(ea.data.emission.rate, eb.data.emission.rate);
                assert_eq!(ea.data.particle_data.life, eb.data.particle_data.life);
                assert_eq!(ea.data.particle_scale.scale_x, eb.data.particle_scale.scale_x);
                assert_eq!(
                    ea.data.emitter_static.color_scale,
                    eb.data.emitter_static.color_scale
                );
            }
        }
    }

    /// Same-file one-slot: duplicating an entry must produce a loadable eff with the new
    /// entry name pointing at a cloned emitter set.
    #[test]
    fn same_file_one_slot_duplicates_entry() {
        if !Path::new(SAMPLE).exists() {
            eprintln!("sample eff missing — skipping");
            return;
        }
        let src = std::fs::read(SAMPLE).unwrap();
        let orig = effect_library::NamcoEffectFile::load(&src).unwrap();
        let donor_name = orig.entry_names[0].clone();
        let n_entries = orig.entries.len();
        let n_sets = orig.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets.len();

        let eff = EffMod {
            source_rel: "test".into(),
            authored: vec![],
            one_slot: vec![OneSlotOp {
                new_entry_name: format!("{donor_name}_os"),
                src_file_rel: String::new(),
                src_set_name: donor_name.clone(),
                src_set_idx: 0,
            }],
        };
        let rebuilt = rebuild_eff_bytes(&src, &eff, None).unwrap();
        let reloaded = effect_library::NamcoEffectFile::load(&rebuilt).unwrap();
        assert_eq!(reloaded.entries.len(), n_entries + 1);
        assert!(reloaded
            .entry_names
            .iter()
            .any(|n| n == &format!("{donor_name}_os")));
        let new_sets = reloaded.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets.len();
        assert!(new_sets > n_sets, "no emitter set was cloned");
        // The new entry's set carries the new name.
        let new_entry = reloaded.entries.last().unwrap();
        if new_entry.variant_count == 0 {
            let set = &reloaded.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets
                [new_entry.emitter_set_id as usize];
            assert_eq!(set.name, format!("{donor_name}_os"));
        }
    }

    /// Cross-file one-slot: a donor entry from another eff lands in the target with its
    /// textures merged (descriptors grow) and the result is loadable + a writer fixpoint.
    #[test]
    fn cross_file_one_slot_transfers_entry_and_textures() {
        let root = Path::new("/home/leap/Workshop/Smash Mod Tools/ArcExplorer_linux_x64/export");
        let donor_rel = "effect/fighter/kirby/ef_kirby.eff";
        if !Path::new(SAMPLE).exists() || !root.join(donor_rel).exists() {
            eprintln!("sample effs missing — skipping");
            return;
        }
        let donor_bytes = std::fs::read(root.join(donor_rel)).unwrap();
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).unwrap();
        // Pick a donor entry with a plain (variant-free) set to keep the assert simple.
        let donor_name = donor
            .entries
            .iter()
            .zip(&donor.entry_names)
            .find(|(e, _)| e.variant_count == 0)
            .map(|(_, n)| n.clone())
            .expect("donor has a variant-free entry");

        let src = std::fs::read(SAMPLE).unwrap();
        let before = effect_library::NamcoEffectFile::load(&src).unwrap();
        let n_entries = before.entries.len();
        let n_desc = before
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .map(|t| t.descriptors.len())
            .unwrap_or(0);

        let eff = EffMod {
            source_rel: "effect/fighter/mario/ef_mario.eff".into(),
            authored: vec![],
            one_slot: vec![OneSlotOp {
                new_entry_name: format!("{donor_name}_kirby_os"),
                src_file_rel: donor_rel.into(),
                src_set_name: donor_name.clone(),
                src_set_idx: 0,
            }],
        };
        let rebuilt = match rebuild_eff_bytes(&src, &eff, Some(root)) {
            Ok(b) => b,
            Err(e) => {
                // Primitive-referencing donors are expected to refuse; anything else fails.
                assert!(
                    e.to_string().contains("primitive"),
                    "cross-file one-slot failed unexpectedly: {e:#}"
                );
                eprintln!("donor entry uses primitives — refusal path exercised: {e}");
                return;
            }
        };
        let reloaded = effect_library::NamcoEffectFile::load(&rebuilt).unwrap();
        assert_eq!(reloaded.entries.len(), n_entries + 1);
        assert!(reloaded
            .entry_names
            .iter()
            .any(|n| n == &format!("{donor_name}_kirby_os")));
        let n_desc_after = reloaded
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .map(|t| t.descriptors.len())
            .unwrap_or(0);
        assert!(
            n_desc_after >= n_desc,
            "texture descriptors shrank ({n_desc} → {n_desc_after})"
        );
        // Writer fixpoint on the merged file.
        assert!(
            reloaded.save().unwrap() == rebuilt,
            "cross-file merge output is not a writer fixpoint"
        );
    }

    /// An actual edit must land in the rebuilt bytes.
    #[test]
    fn edited_scale_lands_in_rebuilt_file() {
        if !Path::new(SAMPLE).exists() {
            eprintln!("sample eff missing — skipping");
            return;
        }
        let src = std::fs::read(SAMPLE).unwrap();
        let namco = effect_library::NamcoEffectFile::load(&src).unwrap();
        let set0 = &namco.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets[0];
        let set_name = set0.name.clone();
        let em_name = set0.emitters[0].data.display_name();
        let old_scale = set0.emitters[0].data.particle_scale.scale_x;

        let eff = EffMod {
            source_rel: "test".into(),
            authored: vec![AuthoredEdit {
                set_name: set_name.clone(),
                set_idx: 0,
                emitter_name: em_name.clone(),
                emitter_idx: 0,
                fields: EmitterFieldEdits {
                    scale: Some(old_scale * 2.0),
                    ..Default::default()
                },
            }],
            one_slot: vec![],
        };
        let rebuilt = rebuild_eff_bytes(&src, &eff, None).unwrap();
        let reloaded = effect_library::NamcoEffectFile::load(&rebuilt).unwrap();
        let new_scale = reloaded.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets[0]
            .emitters[0]
            .data
            .particle_scale
            .scale_x;
        assert!(
            (new_scale - old_scale * 2.0).abs() < 1e-4,
            "edited scale did not survive rebuild ({new_scale} vs {})",
            old_scale * 2.0
        );
    }

    /// The in-tree editor parser and effect_library must agree on the fields we map,
    /// otherwise exported values would not match what the editor showed.
    #[test]
    fn editor_and_writer_field_mapping_agree() {
        if !Path::new(SAMPLE).exists() {
            eprintln!("sample eff missing — skipping");
            return;
        }
        let src = std::fs::read(SAMPLE).unwrap();
        let namco = effect_library::NamcoEffectFile::load(&src).unwrap();
        let lib_ptcl = namco.ptcl_file.as_ref().unwrap();

        let index = crate::effects::EffIndex::from_file(Path::new(SAMPLE)).unwrap();
        let tree_ptcl = crate::effects::PtclFile::parse(&index.ptcl_data).unwrap();

        assert_eq!(
            tree_ptcl.emitter_sets.len(),
            lib_ptcl.emitter_list.emitter_sets.len(),
            "emitter set count differs between parsers"
        );

        let mut checked = 0;
        for (set_i, tree_set) in tree_ptcl.emitter_sets.iter().enumerate() {
            let lib_set = &lib_ptcl.emitter_list.emitter_sets[set_i];
            // Flatten lib emitters parent-first (the editor's enumeration order).
            let mut lib_flat: Vec<&effect_library::structs::Emitter> = Vec::new();
            fn flatten<'a>(
                ems: &'a [effect_library::structs::Emitter],
                out: &mut Vec<&'a effect_library::structs::Emitter>,
            ) {
                for e in ems {
                    out.push(e);
                    flatten(&e.children, out);
                }
            }
            flatten(&lib_set.emitters, &mut lib_flat);
            assert_eq!(
                tree_set.emitters.len(),
                lib_flat.len(),
                "emitter count differs in set {set_i} ({})",
                tree_set.name
            );
            for (tree_em, lib_em) in tree_set.emitters.iter().zip(&lib_flat) {
                let d = &lib_em.data;
                assert_eq!(tree_em.name, d.display_name(), "emitter name mismatch");
                assert!(
                    (tree_em.emission_rate - d.emission.rate).abs() < 1e-4,
                    "emission rate mismatch on {}",
                    tree_em.name
                );
                assert!(
                    (tree_em.lifetime - d.particle_data.life as f32).abs() < 1.5,
                    "lifetime mismatch on {} ({} vs {})",
                    tree_em.name,
                    tree_em.lifetime,
                    d.particle_data.life
                );
                assert!(
                    (tree_em.scale - d.particle_scale.scale_x).abs() < 1e-4,
                    "scale mismatch on {} ({} vs {})",
                    tree_em.name,
                    tree_em.scale,
                    d.particle_scale.scale_x
                );
                assert!(
                    (tree_em.color_scale - d.emitter_static.color_scale).abs() < 1e-4,
                    "color_scale mismatch on {}",
                    tree_em.name
                );
                // Colors follow the editor's precedence: ParticleColor constant →
                // key table → EmitterInfo static. Compare against the active source.
                if let Some(key0) = tree_em.color0.first() {
                    let (sr, sg, sb) = if matches!(
                        d.particle_color.color0_type,
                        effect_library::ColorType::Constant
                    ) {
                        (
                            d.particle_color.color0_r,
                            d.particle_color.color0_g,
                            d.particle_color.color0_b,
                        )
                    } else if d.emitter_static.num_color0_keys > 0 {
                        let k = &d.emitter_static.color0.keys[0];
                        (k.x, k.y, k.z)
                    } else {
                        (
                            d.emitter_info.color0_r,
                            d.emitter_info.color0_g,
                            d.emitter_info.color0_b,
                        )
                    };
                    assert!(
                        (key0.r - sr).abs() < 1e-3
                            && (key0.g - sg).abs() < 1e-3
                            && (key0.b - sb).abs() < 1e-3,
                        "color0 source mismatch on {} (editor [{:.3} {:.3} {:.3}] vs lib [{sr:.3} {sg:.3} {sb:.3}])",
                        tree_em.name, key0.r, key0.g, key0.b
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 10, "too few emitters checked ({checked})");
    }
}
