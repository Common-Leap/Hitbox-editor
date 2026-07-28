// Mod export: rebuild an ef_*.eff with the project's authored edits applied, using the
// effect_library crate (byte-exact writer). The in-tree parser (src/effects.rs) is the
// EDITOR's view; this module maps its edited fields onto effect_library's structs:
//
//   emission_rate → Emission.rate            lifetime      → ParticleData.life (i32)
//   scale         → ParticleScale.scale_x/y/z (ratio vs scale_x, preserves anisotropy)
//   color_scale   → EmitterStatic.color_scale emitter_scale → EmitterInfo.scale_x/y/z
//   color0/color1 → whichever source `effects.rs` READ for this emitter, at the same
//                   precedence: a Constant ParticleColor, else the EmitterStatic key table
//                   (key.x/y/z = r/g/b; times untouched), else the EmitterInfo static color
//   alpha0        → EmitterStatic.alpha0 keys (value in key.x), else a Constant
//                   ParticleColor.alpha0, else EmitterInfo.color0_a — same rule
//
// Writes are strictly scoped: one AuthoredEdit names ONE emitter set and ONE emitter, and
// touches only that emitter's LIVE key slots. Every other emitter, channel and inert key
// slot stays byte-identical to the source — see the
// `authored_color_edit_touches_only_the_targeted_emitter` regression test.
//
// Edited emitters get `cached_binary` cleared — the serializer prefers the cached EMTR
// blob and would otherwise silently drop the edits.

use anyhow::{anyhow, Context, Result};

use crate::mod_project::{AuthoredEdit, EffMod, EmitterFieldEdits, TransplantOp};

/// Rebuild the source .eff bytes with all authored edits applied. `donor_root` resolves
/// cross-file transplant donors (the ArcExplorer export root the `src_file_rel`s are
/// relative to); pass None to restrict to same-file duplication.
/// Applies EVERY transplant op regardless of costume scoping (the preview view).
pub fn rebuild_eff_bytes(
    src_bytes: &[u8],
    eff: &EffMod,
    donor_root: Option<&std::path::Path>,
) -> Result<Vec<u8>> {
    rebuild_eff_bytes_filtered(src_bytes, eff, donor_root, |_| true)
}

/// ONE-SLOT-scoped rebuild for export: `slot` None = the base file (only transplants with
/// no one-slot scoping); Some(s) = the `ef_*_cNN.eff` file (unscoped transplants + those
/// scoped to that slot — the slotted file replaces the base for that costume, so it must
/// carry both).
///
/// `slot` is a real costume index, NOT limited to the vanilla 0–7: the caller writes
/// `c{slot:02}`, so c08 and up (and three-digit slots) name their files correctly.
pub fn rebuild_eff_bytes_for_slot(
    src_bytes: &[u8],
    eff: &EffMod,
    donor_root: Option<&std::path::Path>,
    slot: Option<u8>,
) -> Result<Vec<u8>> {
    rebuild_eff_bytes_filtered(src_bytes, eff, donor_root, |op| match slot {
        None => op.one_slot_slots.is_empty(),
        Some(s) => op.one_slot_slots.is_empty() || op.one_slot_slots.contains(&s),
    })
}

/// Build a runtime carrier from its game-owned EFF plus only the requested donor entries.
///
/// What comes back holds the transplants and nothing else. The carrier's own entry names survive
/// so the item's effect requests still resolve, but their emitters are cleared and every texture,
/// primitive and shader variation left unreferenced is dropped — for two transplants that is
/// 463 KB where transferring the pools whole gave 6.3 MB.
///
/// Each donor's shader containers are merged in once, the copied emitters are relocated onto
/// them, and the result is then compacted down to the variations something actually references.
/// The compaction matters: a donor ships its whole fighter's shader library, so without it the
/// carrier for one Pickel effect would haul all 370 of Pickel's variations into memory.
pub fn rebuild_runtime_carrier_eff_bytes(
    carrier_bytes: &[u8],
    carrier_rel: &str,
    ops: &[TransplantOp],
    donor_root: &std::path::Path,
) -> Result<Vec<u8>> {
    rebuild_runtime_carrier_eff_bytes_with_edits(carrier_bytes, carrier_rel, ops, donor_root, &[])
}

/// Authored edits to bake into the carrier's cloned entries.
///
/// The carrier path exists because the FIGHTER's own eff cannot be reloaded mid-match:
/// `reparse_game_path` rebuilds the parsed emitter structs from the RESIDENT buffer and never
/// re-requests the file, so the merged bytes were never read (`cb_game=0` in
/// `effect_viewer_cb.txt`) and edits only appeared after a full reboot. The carrier's eff IS
/// reloadable — that is the path transplants already use — so an edited effect is cloned into
/// the carrier with its edits baked in and the original kind is aliased onto the clone.
///
/// `set_name` names the entry AS IT EXISTS IN THE CARRIER (the transplant's `new_entry_name`),
/// not the fighter's original entry name.
pub struct CarrierAuthored {
    pub set_name: String,
    pub edits: Vec<AuthoredEdit>,
}

/// As [`rebuild_runtime_carrier_eff_bytes`], but bakes authored edits into the cloned entries.
pub fn rebuild_runtime_carrier_eff_bytes_with_edits(
    carrier_bytes: &[u8],
    carrier_rel: &str,
    ops: &[TransplantOp],
    donor_root: &std::path::Path,
    authored: &[CarrierAuthored],
) -> Result<Vec<u8>> {
    // A carrier-native selection needs no transplant. The runtime remap already turns an `_os`
    // request into the carrier's existing real entry, so parsing, pruning, and repacking these
    // resource pools only introduces risk. Preserve the game's known-good payload byte-for-byte.
    // ...but only when there is nothing to bake in. With authored edits the bytes MUST be
    // rebuilt, otherwise the edits silently do not ship.
    if authored.is_empty()
        && !ops.is_empty()
        && ops
            .iter()
            .all(|op| op.src_file_rel.eq_ignore_ascii_case(carrier_rel))
    {
        return Ok(carrier_bytes.to_vec());
    }

    let mut carrier = effect_library::NamcoEffectFile::load(carrier_bytes)
        .context("effect_library failed to parse the carrier .eff")?;

    // The compute container is engine-global rather than per-file: ef_bomberman, ef_pickel,
    // ef_alucard and ef_kirby all ship the byte-identical single-variation GRSC. Daisy is the
    // exception, and blindly adopting its two-variation container is what froze the carrier load.
    let carrier_compute = carrier
        .ptcl_file
        .as_ref()
        .and_then(|ptcl| ptcl.shader_info.as_ref())
        .and_then(|shader| shader.compute_binary.clone());

    // Load every donor once, then decide which shader strategy the whole carrier uses.
    let mut donors: Vec<(String, effect_library::NamcoEffectFile)> = Vec::new();
    let mut plan: Vec<(&TransplantOp, usize)> = Vec::new();
    for op in ops {
        // A selected native carrier effect is already present in the required scaffold.
        if op.src_file_rel.eq_ignore_ascii_case(carrier_rel) {
            continue;
        }
        let key = op.src_file_rel.to_lowercase();
        let index = match donors.iter().position(|(existing, _)| *existing == key) {
            Some(index) => index,
            None => {
                let bytes =
                    std::fs::read(donor_root.join(&op.src_file_rel)).with_context(|| {
                        format!("transplant donor '{}' is unreadable", op.src_file_rel)
                    })?;
                let donor = effect_library::NamcoEffectFile::load(&bytes)
                    .context("effect_library failed to parse a transplant donor .eff")?;
                donors.push((key, donor));
                donors.len() - 1
            }
        };
        plan.push((op, index));
    }

    // Merge each donor's containers ONCE up front and hand every op the base its variations
    // landed on. Letting each op merge would append the same container once per transplanted
    // effect — three effects from two files produced 892 variations where 522 suffice.
    let mut bases: Vec<(i32, i32)> = vec![(0, 0); donors.len()];
    {
        let mut standard = carrier
            .ptcl_file
            .as_ref()
            .and_then(|ptcl| ptcl.shader_info.as_ref())
            .and_then(|shader| shader.binary_data.clone())
            .unwrap_or_default();
        let mut compute = carrier_compute.clone().unwrap_or_default();
        for (index, (_, donor)) in donors.iter().enumerate() {
            let donor_shader = donor
                .ptcl_file
                .as_ref()
                .and_then(|ptcl| ptcl.shader_info.as_ref())
                .ok_or_else(|| anyhow!("transplant donor has no shader section"))?;
            bases[index] = (
                variation_count(&standard)? as i32,
                variation_count(&compute)? as i32,
            );
            if let Some(donor_bin) = donor_shader.binary_data.as_ref() {
                standard = if standard.is_empty() {
                    donor_bin.clone()
                } else {
                    effect_library::bnsh::merge_variation_files(&[standard, donor_bin.clone()])
                        .context("merging a donor's shader container into the carrier")?
                };
            }
            // Only take a donor's compute container when one of its transplanted effects
            // actually addresses it. The engine-global single-variation container is the shape
            // every loading carrier has had, so it is not worth disturbing for donors that make
            // no use of compute shaders.
            let donor_uses_compute = plan
                .iter()
                .any(|(op, other)| *other == index && donor_compute_demand(donor, op).is_some());
            if donor_uses_compute {
                if let Some(donor_bin) = donor_shader.compute_binary.as_ref() {
                    compute = if compute.is_empty() {
                        donor_bin.clone()
                    } else {
                        effect_library::bnsh::merge_variation_files(&[compute, donor_bin.clone()])
                            .context("merging a donor's compute-shader container into the carrier")?
                    };
                }
            }
        }
        if let Some(shader) = carrier
            .ptcl_file
            .as_mut()
            .and_then(|ptcl| ptcl.shader_info.as_mut())
        {
            shader.binary_data = (!standard.is_empty()).then_some(standard);
            shader.compute_binary = (!compute.is_empty()).then_some(compute);
        }
    }

    for (op, donor_index) in &plan {
        let donor = &donors[*donor_index].1;
        let (standard_base, compute_base) = bases[*donor_index];
        apply_transplant_cross_file(
            &mut carrier,
            donor,
            op,
            false,
            ShaderStrategy::AlreadyMerged {
                standard_base,
                compute_base,
            },
        )?;
    }

    // The Rust BFRES writer round-trips model topology and vertex/index data, but its rebuilt
    // primitive container has not proven GPU-valid in game: effects made from those primitives
    // construct and return handles, then draw nothing. When one donor owns the snapshot's
    // primitive-using effects, retain that donor's original PRMA/BFRES pool exactly. Descriptor
    // IDs remain the emitter-facing lookup key, so unused donor models are harmless.
    //
    // This used to require the effect to be primitive-ONLY, which missed the common case: a
    // MIXED effect (mostly particles plus one mesh emitter). Those fell through to the pruning
    // path, which prunes against the CARRIER's pool — the donor's primitive IDs are not in it,
    // so the mesh emitter's geometry was silently dropped and that part of the effect vanished
    // while every particle emitter around it rendered normally (kirby_dash lost the cone at the
    // front of the dash; its pool went 39 descriptors → 2). Replacing the carrier's pool is safe
    // here because the carrier's own effects are silenced anyway.
    let preserve_raw_primitives = donors.len() == 1
        && ops
            .iter()
            .all(|op| !op.src_file_rel.eq_ignore_ascii_case(carrier_rel))
        && plan
            .iter()
            .any(|(op, donor_index)| donor_effect_uses_primitives(&donors[*donor_index].1, op));
    if preserve_raw_primitives {
        let mut donor_primitives = donors[0]
            .1
            .ptcl_file
            .as_ref()
            .and_then(|ptcl| ptcl.primitive_info.clone())
            .ok_or_else(|| anyhow!("primitive-only donor has no primitive pool"))?;
        donor_primitives.preserve_binary_data = true;
        carrier
            .ptcl_file
            .as_mut()
            .ok_or_else(|| anyhow!("carrier has no PTCL"))?
            .primitive_info = Some(donor_primitives);
    }

    let transplanted: Vec<&str> = plan
        .iter()
        .map(|(op, _)| op.new_entry_name.as_str())
        .chain(
            ops.iter()
                .filter(|op| op.src_file_rel.eq_ignore_ascii_case(carrier_rel))
                // Native carrier effects are spawned through an alias to their existing real
                // name, so retain that set rather than looking for the editor-facing `_os`
                // name. Omitting it creates a zero-emitter EFF; the game's set builder panics
                // while loading that degenerate resource container.
                .map(|op| op.src_set_name.as_str()),
        )
        .collect();
    silence_carrier_native_effects(&mut carrier, &transplanted);
    prune_unreferenced_resources(&mut carrier, preserve_raw_primitives)?;
    compact_shader_containers(&mut carrier)?;

    // No emitter may address a shader variation the final containers lack.
    let (standard_variations, compute_variations) = {
        let shader = carrier
            .ptcl_file
            .as_ref()
            .and_then(|ptcl| ptcl.shader_info.as_ref());
        let count = |binary: Option<&Vec<u8>>| match binary {
            Some(bnsh) => effect_library::bnsh::BnshFile::read(bnsh)
                .map(|file| file.variations.len())
                .unwrap_or(0),
            None => 0,
        };
        match shader {
            Some(shader) => (
                count(shader.binary_data.as_ref()),
                count(shader.compute_binary.as_ref()),
            ),
            None => (0, 0),
        }
    };
    if let Some(ptcl) = carrier.ptcl_file.as_ref() {
        for set in &ptcl.emitter_list.emitter_sets {
            let mut bad: Option<(&'static str, i32, usize)> = None;
            visit_emitters_ref(&set.emitters, &mut |em| {
                let refs = &em.data.shader_references;
                for index in [
                    refs.shader_index,
                    refs.user_shader_index1,
                    refs.user_shader_index2,
                ] {
                    if !shader_index_fits(index, standard_variations) {
                        bad = Some(("shader", index, standard_variations));
                    }
                }
                if !shader_index_fits(refs.compute_shader_index, compute_variations) {
                    bad = Some((
                        "compute-shader",
                        refs.compute_shader_index,
                        compute_variations,
                    ));
                }
            });
            if let Some((kind, index, available)) = bad {
                anyhow::bail!(
                    "live carrier safety: '{}' addresses {kind} variation {index}, but the \
                     carrier's container only holds {available}. Shipping that would hang the \
                     game's loader, so the carrier was not built.",
                    set.name
                );
            }
        }
    }

    // Bake authored edits into the CLONED entries, last — after the transplant has created
    // them and after the shader-safety check, so an edit can never mask a bad clone.
    if !authored.is_empty() {
        let ptcl = carrier
            .ptcl_file
            .as_mut()
            .context("carrier has no ptcl section — cannot apply authored edits")?;
        for entry in authored {
            let idx = ptcl
                .emitter_list
                .emitter_sets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(&entry.set_name));
            let Some(idx) = idx else {
                anyhow::bail!(
                    "authored edits target '{}', which is not in the built carrier — the \
                     transplant that should have created it did not run",
                    entry.set_name
                );
            };
            for edit in &entry.edits {
                // Retarget the edit at the carrier's copy: the editor authored it against the
                // FIGHTER's eff, where the set sits at a different index under its original
                // name. `apply_authored` resolves strictly by (name, index), so both must be
                // rewritten or the edit lands on the wrong set — or silently on none.
                let mut edit = edit.clone();
                edit.set_name = entry.set_name.clone();
                edit.set_idx = idx;
                apply_authored(ptcl, &edit)?;
            }
        }
    }

    carrier
        .save()
        .context("effect_library failed to encode the runtime carrier .eff")
}

/// Empty every emitter set the transplants did not create, leaving the carrier's own effects as
/// resolvable-but-silent entries.
///
/// The carrier is a game-owned assist EFF borrowed for its blessed load path — its native effects
/// are never played, but they are what holds the file open: Bomberman's four sets reference all
/// nineteen of its textures, 2.9 MB of a 3.4 MB base. Clearing their emitters makes those
/// textures, primitives and shader variations collectable by the passes that follow.
///
/// The entries and the sets themselves stay. The item still requests its own effects by name at
/// spawn, and a name that resolves to an empty set yields a valid handle that emits nothing —
/// whereas deleting the entry would leave those requests resolving against nothing at all.
fn silence_carrier_native_effects(
    carrier: &mut effect_library::NamcoEffectFile,
    transplanted: &[&str],
) {
    let keep: std::collections::HashSet<usize> = carrier
        .entry_names
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            transplanted
                .iter()
                .any(|new| new.eq_ignore_ascii_case(name))
        })
        .filter_map(|(index, _)| {
            carrier
                .entries
                .get(index)
                .and_then(|entry| (entry.emitter_set_id as usize).checked_sub(1))
        })
        .collect();
    let Some(ptcl) = carrier.ptcl_file.as_mut() else {
        return;
    };
    for (index, set) in ptcl.emitter_list.emitter_sets.iter_mut().enumerate() {
        if !keep.contains(&index) {
            set.emitters.clear();
        }
    }
}

/// Drop every texture and primitive no surviving emitter samples.
///
/// Both pools are GUID-keyed, so dropping an entry moves nothing: the emitters that remain keep
/// referring to exactly the resources they already did. Shader variations are index-keyed and so
/// need renumbering — [`compact_shader_containers`] handles those separately.
fn prune_unreferenced_resources(
    carrier: &mut effect_library::NamcoEffectFile,
    preserve_raw_primitives: bool,
) -> Result<()> {
    let Some(ptcl) = carrier.ptcl_file.as_mut() else {
        return Ok(());
    };
    let mut used_textures: Vec<u64> = Vec::new();
    let mut used_primitives: Vec<u64> = Vec::new();
    for set in &ptcl.emitter_list.emitter_sets {
        visit_emitters_ref(&set.emitters, &mut |em| {
            let d = &em.data;
            append_unique_resource_ids(
                &mut used_textures,
                [
                    d.sampler0.as_ref().map(|s| s.texture_id),
                    d.sampler1.as_ref().map(|s| s.texture_id),
                    d.sampler2.as_ref().map(|s| s.texture_id),
                    d.sampler3.as_ref().map(|s| s.texture_id),
                    d.sampler4.as_ref().map(|s| s.texture_id),
                    d.sampler5.as_ref().map(|s| s.texture_id),
                ],
            );
            append_unique_resource_ids(
                &mut used_primitives,
                [
                    Some(d.particle_data.primitive_id),
                    Some(d.particle_data.primitive_ex_id),
                    Some(d.shape_info.primitive_index),
                ],
            );
        });
    }

    if let Some(textures) = ptcl.texture_info.as_mut() {
        // Descriptor order matches BNTX texture order: the serializer sorts the descriptor table
        // and reorders the archive to match (`reorder_and_save`), so index i addresses both.
        let keep: Vec<usize> = (0..textures.descriptors.len())
            .filter(|index| used_textures.contains(&textures.descriptors[*index].id))
            .collect();
        if keep.len() < textures.descriptors.len() {
            let binary = textures
                .binary_data
                .as_ref()
                .ok_or_else(|| anyhow!("carrier has texture descriptors but no BNTX"))?;
            let scratch = crate::scratch_dirs::app_scratch_dir("carrier-tex")?;
            let mut blobs = Vec::with_capacity(keep.len());
            for index in &keep {
                let name = textures.descriptors[*index].name.clone();
                let path = scratch.path().join(format!("{name}.bntx"));
                effect_library::bntx::export_single_texture(binary, *index, &name, &path)
                    .with_context(|| format!("extracting carrier texture '{name}'"))?;
                blobs.push(std::fs::read(&path)?);
            }
            textures.binary_data = match blobs.len() {
                0 => None,
                1 => blobs.pop(),
                _ => Some(
                    effect_library::bntx::merge_texture_files(&blobs)
                        .context("repacking the carrier's texture pool")?,
                ),
            };
            let kept: std::collections::HashSet<usize> = keep.into_iter().collect();
            let mut index = 0;
            textures.descriptors.retain(|_| {
                index += 1;
                kept.contains(&(index - 1))
            });
        }
    }

    if !preserve_raw_primitives {
        let Some(primitives) = ptcl.primitive_info.as_mut() else {
            return Ok(());
        };
        let keep: Vec<usize> = (0..primitives.descriptors.len())
            .filter(|index| used_primitives.contains(&primitives.descriptors[*index].id))
            .collect();
        if keep.len() < primitives.descriptors.len() {
            let binary = primitives
                .binary_data
                .as_ref()
                .ok_or_else(|| anyhow!("carrier has primitive descriptors but no BFRES"))?;
            let mut blobs = Vec::with_capacity(keep.len());
            for index in &keep {
                blobs.push(
                    effect_library::bfres::export_single_model(binary, *index).with_context(
                        || {
                            format!(
                                "extracting carrier primitive {:#x}",
                                primitives.descriptors[*index].id
                            )
                        },
                    )?,
                );
            }
            primitives.binary_data = match blobs.len() {
                0 => None,
                1 => blobs.pop(),
                _ => Some(
                    effect_library::bfres::ResFile::merge_model_files(&blobs)
                        .context("repacking the carrier's primitive pool")?,
                ),
            };
            let kept: std::collections::HashSet<usize> = keep.into_iter().collect();
            let mut index = 0;
            primitives.descriptors.retain(|_| {
                index += 1;
                kept.contains(&(index - 1))
            });
        }
    }
    Ok(())
}

/// Drop every shader variation no emitter addresses, then renumber the emitters onto the
/// compacted containers.
///
/// A donor's BNSH holds its whole fighter's shader library — Pickel ships 370 variations and a
/// single transplanted effect uses twelve of them — and once merged that library is the largest
/// thing in the carrier. Keeping only what is referenced is what lets the carrier hold just the
/// effects the user asked to transplant.
///
/// Repacking variations was tried once before and produced handles with no pixels, which is why
/// the containers used to be transferred whole. That has since been diagnosed: `BnshFile::write`
/// emitted a relocation table listing slots that hold no offset, so the runtime turned each into
/// a pointer to the image base and every container the writer touched was corrupt. With the
/// writer fixed it reproduces the game's own pointer set exactly, so repacking is sound again.
fn compact_shader_containers(carrier: &mut effect_library::NamcoEffectFile) -> Result<()> {
    let Some(ptcl) = carrier.ptcl_file.as_mut() else {
        return Ok(());
    };
    let mut used_standard: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
    let mut used_compute: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
    for set in &ptcl.emitter_list.emitter_sets {
        visit_emitters_ref(&set.emitters, &mut |em| {
            let refs = &em.data.shader_references;
            for index in [
                refs.shader_index,
                refs.user_shader_index1,
                refs.user_shader_index2,
            ] {
                if index >= 0 {
                    used_standard.insert(index);
                }
            }
            if refs.compute_shader_index >= 0 {
                used_compute.insert(refs.compute_shader_index);
            }
        });
    }

    let (standard_map, compute_map) = {
        let Some(shader) = ptcl.shader_info.as_mut() else {
            return Ok(());
        };
        (
            subset_container(shader.binary_data.as_mut(), &used_standard, "shader")?,
            subset_container(
                shader.compute_binary.as_mut(),
                &used_compute,
                "compute-shader",
            )?,
        )
    };

    for set in &mut ptcl.emitter_list.emitter_sets {
        let name = set.name.clone();
        let mut failed = None;
        visit_emitters(&mut set.emitters, &mut 0, &mut |_, em| {
            let refs = &mut em.data.shader_references;
            for (index, map, kind) in [
                (&mut refs.shader_index, &standard_map, "shader"),
                (&mut refs.user_shader_index1, &standard_map, "shader"),
                (&mut refs.user_shader_index2, &standard_map, "shader"),
                (
                    &mut refs.compute_shader_index,
                    &compute_map,
                    "compute-shader",
                ),
            ] {
                if *index < 0 || map.is_empty() {
                    continue;
                }
                match map.get(index) {
                    Some(new) => *index = *new,
                    None => failed = Some((kind, *index)),
                }
            }
            // `custom_shader_index` is deliberately untouched: it is a small custom-shader mode,
            // not an index into the variation array.
            em.cached_binary = None; // indices moved → the cached EMTR blob is stale
        });
        if let Some((kind, index)) = failed {
            anyhow::bail!(
                "compacting shader containers: '{name}' addresses {kind} variation {index}, \
                 which the merged container does not hold"
            );
        }
    }
    Ok(())
}

/// Rewrite `binary` keeping only the listed variations, returning the old-index → new-index map.
/// An empty map means the container was left exactly as it was and its indices still apply.
fn subset_container(
    binary: Option<&mut Vec<u8>>,
    used: &std::collections::BTreeSet<i32>,
    kind: &str,
) -> Result<std::collections::HashMap<i32, i32>> {
    let Some(binary) = binary else {
        return Ok(std::collections::HashMap::new());
    };
    // An unused container is left alone rather than removed: the engine-global single-variation
    // compute container is the shape every carrier that has ever loaded was built with.
    if used.is_empty() || binary.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let mut file = effect_library::bnsh::BnshFile::read(binary)
        .with_context(|| format!("reading the merged {kind} container"))?;
    // Already exactly 0..n with nothing to drop — leave the bytes untouched rather than putting
    // a game-authored container through the writer for no gain.
    if used.len() == file.variations.len()
        && used.iter().enumerate().all(|(i, old)| *old as usize == i)
    {
        return Ok(std::collections::HashMap::new());
    }
    let mut kept = Vec::with_capacity(used.len());
    let mut map = std::collections::HashMap::with_capacity(used.len());
    for old in used {
        let variation = file
            .variations
            .get(*old as usize)
            .ok_or_else(|| anyhow!("an emitter addresses {kind} variation {old}, which the merged container does not hold"))?
            .clone();
        map.insert(*old, kept.len() as i32);
        kept.push(variation);
    }
    file.variations = kept;
    *binary = file.write();
    Ok(map)
}

/// Variation count of a BNSH container; an absent container holds none.
fn variation_count(bnsh: &[u8]) -> Result<usize> {
    if bnsh.is_empty() {
        return Ok(0);
    }
    Ok(effect_library::bnsh::BnshFile::read(bnsh)
        .context("reading a shader container's variation count")?
        .variations
        .len())
}

/// How many compute-shader variations a donor entry needs (`highest index + 1`), or None when it
/// uses no compute shader at all.
fn donor_compute_demand(
    donor: &effect_library::NamcoEffectFile,
    op: &TransplantOp,
) -> Option<usize> {
    let ptcl = donor.ptcl_file.as_ref()?;
    let entry_index = donor
        .entry_names
        .iter()
        .position(|name| name.eq_ignore_ascii_case(&op.src_set_name))?;
    let set_index = (donor.entries[entry_index].emitter_set_id as usize).checked_sub(1)?;
    let set = ptcl.emitter_list.emitter_sets.get(set_index)?;
    let mut highest = -1i32;
    visit_emitters_ref(&set.emitters, &mut |em| {
        highest = highest.max(em.data.shader_references.compute_shader_index);
    });
    // `then_some` would evaluate the argument even for the -1 "unused" sentinel, wrapping to
    // usize::MAX before overflowing.
    (highest >= 0).then(|| highest as usize + 1)
}

/// True when every emitter in the selected donor set renders through a donor-owned primitive.
/// These are the effects for which a GPU-invalid rewritten BFRES means complete invisibility,
/// rather than one missing layer among otherwise visible quad particles.
fn donor_effect_uses_primitives(
    donor: &effect_library::NamcoEffectFile,
    op: &TransplantOp,
) -> bool {
    let Some(ptcl) = donor.ptcl_file.as_ref() else {
        return false;
    };
    let Some(primitives) = ptcl.primitive_info.as_ref() else {
        return false;
    };
    let Some(entry_index) = donor
        .entry_names
        .iter()
        .position(|name| name.eq_ignore_ascii_case(&op.src_set_name))
    else {
        return false;
    };
    let Some(set_index) = (donor.entries[entry_index].emitter_set_id as usize).checked_sub(1)
    else {
        return false;
    };
    let Some(set) = ptcl.emitter_list.emitter_sets.get(set_index) else {
        return false;
    };
    let mut count = 0usize;
    let mut any_primitive = false;
    visit_emitters_ref(&set.emitters, &mut |em| {
        count += 1;
        let ids = [
            em.data.particle_data.primitive_id,
            em.data.particle_data.primitive_ex_id,
            em.data.shape_info.primitive_index,
        ];
        let has_local_primitive = ids.iter().any(|id| {
            *id != 0
                && *id != u64::MAX
                && primitives
                    .descriptors
                    .iter()
                    .any(|descriptor| descriptor.id == *id)
        });
        any_primitive |= has_local_primitive;
    });
    count != 0 && any_primitive
}

/// How a transplant's emitters relate to the destination's shader containers.
#[derive(Clone, Copy)]
enum ShaderStrategy {
    /// Append the donor's container to the destination's, relocating by the destination's count.
    Merge,
    /// The destination already holds this donor's variations at these bases, so the containers
    /// must not be touched again — only the copied emitters' indices are relocated. Merging a
    /// donor once per transplanted effect instead of once per file duplicated whole containers.
    AlreadyMerged {
        standard_base: i32,
        compute_base: i32,
    },
}

/// A negative index is the "no shader" sentinel; anything else must address a variation the
/// container actually holds.
fn shader_index_fits(index: i32, variations: usize) -> bool {
    index < 0 || (index as usize) < variations
}

fn rebuild_eff_bytes_filtered(
    src_bytes: &[u8],
    eff: &EffMod,
    donor_root: Option<&std::path::Path>,
    keep: impl Fn(&TransplantOp) -> bool,
) -> Result<Vec<u8>> {
    let mut namco = effect_library::NamcoEffectFile::load(src_bytes)
        .context("effect_library failed to parse the source .eff")?;
    let destination_is_fighter = eff
        .source_rel
        .to_ascii_lowercase()
        .starts_with("effect/fighter/");
    // Transplant ops FIRST (they only append sets, so pre-existing set indices are
    // stable), then authored edits — which may target a freshly cloned set by name
    // (editing the copy in the eff editor). Cross-fighter donors bake in here too: the
    // merged file replaces the player's own (resident) eff at boot, so it renders.
    for op in eff.transplants.iter().filter(|op| keep(op)) {
        let same_file = op.src_file_rel.is_empty() || op.src_file_rel == eff.source_rel;
        if same_file {
            apply_transplant_same_file(&mut namco, op)?;
        } else {
            let root = donor_root.ok_or_else(|| {
                anyhow!(
                    "transplant '{}': cross-file donor '{}' needs the export root",
                    op.new_entry_name,
                    op.src_file_rel
                )
            })?;
            let donor_bytes = std::fs::read(root.join(&op.src_file_rel)).with_context(|| {
                format!(
                    "cross-file transplant donor '{}' unreadable",
                    op.src_file_rel
                )
            })?;
            let donor = effect_library::NamcoEffectFile::load(&donor_bytes)
                .context("effect_library failed to parse the donor .eff")?;
            apply_transplant_cross_file(
                &mut namco,
                &donor,
                op,
                destination_is_fighter,
                ShaderStrategy::Merge,
            )?;
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
fn apply_transplant_same_file(
    namco: &mut effect_library::NamcoEffectFile,
    op: &TransplantOp,
) -> Result<()> {
    if op.replace_entry.is_none()
        && namco
            .entry_names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&op.new_entry_name))
    {
        anyhow::bail!(
            "transplant target name '{}' already exists",
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
                "transplant donor entry '{}' not found in this eff",
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
    finish_transplant_entry(namco, op, new_entry)
}

/// Append `new_entry` under the op's new name, or — replace mode — repoint an existing
/// entry at the cloned set(s) (its name and slot in the table stay; the old set is
/// orphaned but harmless). Replace is the costume-scoped semantic: every ACMD use of
/// the entry switches on that costume with no redirect needed.
fn finish_transplant_entry(
    namco: &mut effect_library::NamcoEffectFile,
    op: &TransplantOp,
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
                    anyhow!("transplant replace target entry '{target}' not found in this eff")
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
fn apply_transplant_cross_file(
    namco: &mut effect_library::NamcoEffectFile,
    donor: &effect_library::NamcoEffectFile,
    op: &TransplantOp,
    destination_is_fighter: bool,
    shader_strategy: ShaderStrategy,
) -> Result<()> {
    if op.replace_entry.is_none()
        && namco
            .entry_names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&op.new_entry_name))
    {
        anyhow::bail!(
            "transplant target name '{}' already exists",
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
                "transplant donor entry '{}' not found in '{}'",
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
            append_unique_resource_ids(
                &mut tex_ids,
                [
                    d.sampler0.as_ref().map(|s| s.texture_id),
                    d.sampler1.as_ref().map(|s| s.texture_id),
                    d.sampler2.as_ref().map(|s| s.texture_id),
                    d.sampler3.as_ref().map(|s| s.texture_id),
                    d.sampler4.as_ref().map(|s| s.texture_id),
                    d.sampler5.as_ref().map(|s| s.texture_id),
                ],
            );
            uses_shader |= d.shader_references.shader_index >= 0
                || d.shader_references.user_shader_index1 >= 0
                || d.shader_references.user_shader_index2 >= 0;
            uses_compute |= d.shader_references.compute_shader_index >= 0;
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
    // Each missing donor primitive is extracted as a single-model BFRES and appended.
    // This deliberately avoids copying the donor's whole PRMA when the destination pool
    // is empty: the runtime carrier should retain only resources the selected effect uses.
    if !prim_ids.is_empty() {
        let dest_has = |id: u64| {
            namco
                .ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.as_ref())
                .map(|pi| pi.descriptors.iter().any(|d| d.id == id))
                .unwrap_or(false)
        };
        // Some vanilla emitters retain nonzero values in inactive primitive fields (and some
        // refer to globally-owned primitives). If the donor itself has no descriptor for an
        // ID, there is no local model to transplant; vanilla already resolves or ignores it.
        // Only donor-owned descriptors belong in the merged PRMA.
        let donor_prim = donor_ptcl.primitive_info.as_ref();
        let missing: Vec<u64> = prim_ids
            .iter()
            .copied()
            .filter(|id| {
                !dest_has(*id)
                    && donor_prim
                        .map(|p| p.descriptors.iter().any(|d| d.id == *id))
                        .unwrap_or(false)
            })
            .collect();
        if !missing.is_empty() {
            let donor_prim = donor_prim.expect("missing IDs were filtered through donor PRMA");
            let dest_ptcl = namco
                .ptcl_file
                .as_mut()
                .ok_or_else(|| anyhow!("target eff has no embedded PTCL"))?;
            if dest_ptcl.primitive_info.is_none() {
                let mut empty = donor_prim.clone();
                empty.descriptors.clear();
                empty.binary_data = None;
                dest_ptcl.primitive_info = Some(empty);
            }
            let donor_bin = donor_prim
                .binary_data
                .as_ref()
                .ok_or_else(|| anyhow!("donor PRMA has descriptors but no BFRES binary"))?;
            let dest_prim = dest_ptcl.primitive_info.as_mut().unwrap();
            let dest_count = dest_prim.descriptors.len();
            let mut files: Vec<Vec<u8>> = dest_prim
                .binary_data
                .as_ref()
                .filter(|b| !b.is_empty())
                .cloned()
                .into_iter()
                .collect();
            for id in &missing {
                let idx =
                    effect_library::bfres::descriptor_index_for_id(&donor_prim.descriptors, *id)
                        .ok_or_else(|| anyhow!("donor primitive {id:#x} has no descriptor"))?;
                let blob = effect_library::bfres::export_single_model(donor_bin, idx)
                    .with_context(|| format!("extracting donor primitive {id:#x}"))?;
                files.push(blob);
                dest_prim
                    .descriptors
                    .push(donor_prim.descriptors[idx].clone());
            }
            let merged = if files.len() == 1 {
                files.pop().unwrap()
            } else {
                effect_library::bfres::ResFile::merge_model_files(&files)
                    .context("merging donor primitives into the target PRMA")?
            };
            // Sanity: descriptor index i must still map to model index i — a model
            // NAME collision would make the container replace-instead-of-append and
            // silently desync every primitive after it.
            let last = dest_count + missing.len() - 1;
            if effect_library::bfres::export_single_model(&merged, last).is_err()
                || effect_library::bfres::export_single_model(&merged, last + 1).is_ok()
            {
                anyhow::bail!(
                    "transplant '{}': donor primitive model name collides with the target's \
                     (PRMA merge would desync) — not merged",
                    op.new_entry_name
                );
            }
            dest_prim.binary_data = Some(merged);
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
            let tmp = crate::scratch_dirs::app_scratch_dir("transplant-tex")?;
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
            if let Some(existing) = dest_tex.binary_data.as_ref().filter(|b| !b.is_empty()) {
                files.push(existing.clone());
            }
            files.extend(new_blobs);
            dest_tex.binary_data = Some(if files.len() == 1 {
                files.pop().unwrap()
            } else {
                effect_library::bntx::merge_texture_files(&files)
                    .context("merging donor textures into the target BNTX")?
            });
        }
    }

    // Donor shader containers are appended whole here and the copied emitters relocated onto
    // them. Narrowing to the variations actually referenced is a separate, later pass
    // (`compact_shader_containers`) so it can see every transplant at once; doing it per-donor
    // would have to guess which of the destination's own variations stay reachable.
    let mut shader_index_base = 0i32;
    let mut compute_index_base = 0i32;
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
            match shader_strategy {
                ShaderStrategy::AlreadyMerged { standard_base, .. } => {
                    shader_index_base = standard_base;
                }
                ShaderStrategy::Merge => {
                    if dest_bin.is_empty() {
                        dest_shader.binary_data = Some(donor_bin.clone());
                    } else {
                        shader_index_base = effect_library::bnsh::BnshFile::read(&dest_bin)
                            .context("reading destination shader variations")?
                            .variations
                            .len() as i32;
                        dest_shader.binary_data = Some(
                            effect_library::bnsh::merge_variation_files(&[
                                dest_bin,
                                donor_bin.clone(),
                            ])
                            .context("merging the donor shader container")?,
                        );
                    }
                }
            }
        }
        if uses_compute {
            let dest_bin = dest_shader.compute_binary.clone().unwrap_or_default();
            let donor_bin = donor_shader
                .compute_binary
                .as_ref()
                .ok_or_else(|| anyhow!("donor compute-shader section has no binary"))?;
            match shader_strategy {
                ShaderStrategy::AlreadyMerged { compute_base, .. } => {
                    compute_index_base = compute_base;
                }
                ShaderStrategy::Merge => {
                    if dest_bin.is_empty() {
                        dest_shader.compute_binary = Some(donor_bin.clone());
                    } else {
                        compute_index_base = effect_library::bnsh::BnshFile::read(&dest_bin)
                            .context("reading destination compute-shader variations")?
                            .variations
                            .len() as i32;
                        dest_shader.compute_binary = Some(
                            effect_library::bnsh::merge_variation_files(&[
                                dest_bin,
                                donor_bin.clone(),
                            ])
                            .context("merging the donor compute-shader container")?,
                        );
                    }
                }
            }
        }
    }

    // Shader-remap the donor emitters (shaders are index-referenced; textures/primitives are
    // GUID-referenced against the merged pools, so they need no remap).
    let remap = |set: &mut effect_library::structs::EmitterSet| {
        visit_emitters(&mut set.emitters, &mut 0, &mut |_, em| {
            let s = &mut em.data.shader_references;
            remap_shader_indices(
                &mut s.shader_index,
                &mut s.user_shader_index1,
                &mut s.user_shader_index2,
                &mut s.compute_shader_index,
                shader_index_base,
                compute_index_base,
            );
            // `custom_shader_index` is NOT an index into the BNSH variation array. Its values
            // are small custom-shader modes (typically 0, 4, or 8) shared by emitters whose
            // real `shader_index` spans the whole file. Relocating it by the destination's
            // variation count makes otherwise valid effects invisible (Daisy's petals exposed
            // this by changing mode 0 into the invalid value 38 in the Bomberman carrier).
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
    // Non-fighter donors (assists/items — e.g. ef_alucard) commonly use type 0 in the
    // kind u16's high byte. A FIGHTER destination needs 0x01xx: the fighter spawn path
    // rejects type-0 entries. An assist-owned carrier needs the inverse normalization:
    // fighter-type 0x01xx entries are not registered by the assist loader.
    new_entry.kind = destination_entry_kind(new_entry.kind, destination_is_fighter);
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
    finish_transplant_entry(namco, op, new_entry)
}

fn destination_entry_kind(kind: u16, destination_is_fighter: bool) -> u16 {
    if destination_is_fighter {
        if kind & 0xff00 == 0 {
            kind | 0x0100
        } else {
            kind
        }
    } else {
        // Assist/item owners register type-0 entries. Leaving a fighter donor as 0x01xx
        // makes load_effects succeed while omitting the entry from the live kind table.
        kind & 0x00ff
    }
}

fn remap_shader_indices(
    shader: &mut i32,
    user1: &mut i32,
    user2: &mut i32,
    compute: &mut i32,
    shader_base: i32,
    compute_base: i32,
) {
    for index in [shader, user1, user2] {
        if *index >= 0 {
            *index += shader_base;
        }
    }
    if *compute >= 0 {
        *compute += compute_base;
    }
}

fn append_unique_resource_ids<const N: usize>(out: &mut Vec<u64>, ids: [Option<u64>; N]) {
    for id in ids.into_iter().flatten() {
        // 0 and u64::MAX are the EFF "no resource" sentinels.
        if id != 0 && id != u64::MAX && !out.contains(&id) {
            out.push(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_unique_resource_ids, destination_entry_kind, remap_shader_indices, shader_index_fits,
    };
    use crate::mod_project::{AuthoredEdit, EmitterFieldEdits};

    /// Every emitter in the file, as (set name, flat emitter index, serialized data).
    /// Serializing `EmitterData` catches ANY field drift, not just the ones we edit.
    fn emitter_snapshot(file: &effect_library::NamcoEffectFile) -> Vec<(String, usize, String)> {
        let mut out = Vec::new();
        let Some(ptcl) = file.ptcl_file.as_ref() else {
            return out;
        };
        for set in &ptcl.emitter_list.emitter_sets {
            let mut idx = 0usize;
            super::visit_emitters_ref(&set.emitters, &mut |em| {
                out.push((
                    set.name.clone(),
                    idx,
                    serde_json::to_string(&em.data).unwrap_or_default(),
                ));
                idx += 1;
            });
        }
        out
    }

    /// A color edit on ONE emitter must leave every other emitter byte-identical.
    /// Point `VISIONARY_EFF_ROOT` at an extracted `effect/` tree to run this.
    #[test]
    fn authored_color_edit_touches_only_the_targeted_emitter() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let source = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let before = emitter_snapshot(&source);

        // Target the first set that has at least two emitters, and edit its SECOND one.
        let ptcl = source.ptcl_file.as_ref().expect("source PTCL");
        let (set_idx, set_name) = ptcl
            .emitter_list
            .emitter_sets
            .iter()
            .enumerate()
            .find_map(|(i, s)| {
                let mut n = 0usize;
                super::visit_emitters_ref(&s.emitters, &mut |_| n += 1);
                (n >= 2).then(|| (i, s.name.clone()))
            })
            .expect("a set with >= 2 emitters");
        let target_emitter = 1usize;
        let target_name = before
            .iter()
            .filter(|(name, ..)| *name == set_name)
            .nth(target_emitter)
            .map(|_| {
                let mut nth = None;
                let mut idx = 0usize;
                super::visit_emitters_ref(
                    &ptcl.emitter_list.emitter_sets[set_idx].emitters,
                    &mut |em| {
                        if idx == target_emitter {
                            nth = Some(em.data.display_name());
                        }
                        idx += 1;
                    },
                );
                nth.unwrap_or_default()
            })
            .expect("target emitter");

        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            authored: vec![AuthoredEdit {
                set_name: set_name.clone(),
                // Not exercised by this test: it drives `rebuild_eff_bytes`, which resolves
                // by emitter-set name. Only the carrier path needs the kind name.
                entry_name: String::new(),
                set_idx,
                emitter_name: target_name,
                emitter_idx: target_emitter,
                fields: EmitterFieldEdits {
                    color0: Some(vec![[1.0, 0.0, 0.0, 0.0]]),
                    ..Default::default()
                },
            }],
            transplants: Vec::new(),
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, Some(&root)).expect("rebuild");
        let after = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");
        let after = emitter_snapshot(&after);

        assert_eq!(before.len(), after.len(), "emitter count changed");
        let changed: Vec<(String, usize)> = before
            .iter()
            .zip(&after)
            .filter(|((_, _, a), (_, _, b))| a != b)
            .map(|((s, i, _), _)| (s.clone(), *i))
            .collect();
        assert_eq!(
            changed,
            vec![(set_name, target_emitter)],
            "a single-emitter color edit leaked into other emitters"
        );
    }

    #[test]
    fn carrier_rejects_shader_variations_its_container_lacks() {
        // The engine-global compute container ships a single variation. -1 means "unused".
        assert!(shader_index_fits(-1, 1));
        assert!(shader_index_fits(0, 1));
        // Daisy's DAISY_KINOPIO_BULLET wants variation 1 from Daisy's two-variation container;
        // shipping that container in a Bomberman carrier froze the game.
        assert!(!shader_index_fits(1, 1));
        assert!(!shader_index_fits(0, 0));
    }

    #[test]
    fn assist_carrier_preserves_donor_entry_type() {
        assert_eq!(destination_entry_kind(0x0005, false), 0x0005);
        assert_eq!(destination_entry_kind(0x0100, false), 0x0000);
        assert_eq!(destination_entry_kind(0x0105, false), 0x0005);
    }

    #[test]
    fn fighter_destination_promotes_non_fighter_entry_type() {
        assert_eq!(destination_entry_kind(0x0005, true), 0x0105);
        assert_eq!(destination_entry_kind(0x0105, true), 0x0105);
    }

    #[test]
    fn shader_relocation_moves_only_bnsh_variation_indices() {
        let (mut shader, mut user1, mut user2, mut compute) = (88, -1, 3, 1);
        let custom_shader_mode = 0;
        remap_shader_indices(&mut shader, &mut user1, &mut user2, &mut compute, 38, 1);
        assert_eq!((shader, user1, user2, compute), (126, -1, 41, 2));
        assert_eq!(custom_shader_mode, 0);
    }

    #[test]
    fn texture_collection_covers_all_six_sampler_slots() {
        let mut ids = vec![10];
        append_unique_resource_ids(
            &mut ids,
            [Some(10), Some(11), None, Some(13), Some(14), Some(15)],
        );
        assert_eq!(ids, vec![10, 11, 13, 14, 15]);
    }

    /// End-to-end carrier builds against the real game files, which is the only way to exercise
    /// resource stripping: BNSH variations and BNTX textures are opaque blobs that cannot be
    /// synthesized. Point `VISIONARY_EFF_ROOT` at a directory holding the extracted `effect/`
    /// tree to run this; it skips otherwise so a checkout without game assets still passes.
    #[test]
    fn stripped_carrier_holds_only_what_was_transplanted() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let op = |file: &str, set: &str| crate::mod_project::TransplantOp {
            new_entry_name: set.to_string(),
            src_file_rel: file.to_string(),
            src_set_name: set.to_string(),
            src_set_idx: 0,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        const PICKEL: &str = "effect/fighter/pickel/ef_pickel.eff";
        const DAISY: &str = "effect/fighter/daisy/ef_daisy.eff";
        let cases: [(&str, Vec<crate::mod_project::TransplantOp>); 3] = [
            ("one donor", vec![op(PICKEL, "pickel_tnt")]),
            (
                // DAISY_KINOPIO_BULLET is the one known effect that samples a compute shader.
                "compute shader",
                vec![op(DAISY, "daisy_kinopio_bullet")],
            ),
            (
                "two donor files",
                vec![
                    op(PICKEL, "pickel_tnt"),
                    op(DAISY, "daisy_flower_petals"),
                    op(DAISY, "daisy_kinopio_bullet"),
                ],
            ),
        ];

        for (label, ops) in cases {
            let built =
                super::rebuild_runtime_carrier_eff_bytes(&carrier_base, CARRIER, &ops, &root)
                    .unwrap_or_else(|err| panic!("{label}: carrier build failed: {err:#}"));
            let carrier = effect_library::NamcoEffectFile::load(&built)
                .unwrap_or_else(|err| panic!("{label}: carrier does not re-read: {err:#}"));
            let ptcl = carrier.ptcl_file.as_ref().expect("carrier has a PTCL");
            let shader = ptcl
                .shader_info
                .as_ref()
                .expect("carrier has a shader section");
            let count = |binary: Option<&Vec<u8>>| {
                binary
                    .map(|b| {
                        effect_library::bnsh::BnshFile::read(b)
                            .expect("container re-reads")
                            .variations
                            .len()
                    })
                    .unwrap_or(0)
            };
            let standard = count(shader.binary_data.as_ref());
            let compute = count(shader.compute_binary.as_ref());

            // Each transplant must have arrived as a named entry backed by a populated set.
            for want in &ops {
                let index = carrier
                    .entry_names
                    .iter()
                    .position(|name| name.eq_ignore_ascii_case(&want.new_entry_name))
                    .unwrap_or_else(|| panic!("{label}: '{}' is missing", want.new_entry_name));
                let set_id = carrier.entries[index].emitter_set_id as usize;
                let set = ptcl.emitter_list.emitter_sets[set_id - 1].clone();
                assert!(
                    !set.emitters.is_empty(),
                    "{label}: '{}' was silenced along with the carrier's own effects",
                    want.new_entry_name
                );
            }
            // The carrier's own entries stay resolvable, and stay silent.
            let native = carrier
                .entry_names
                .iter()
                .position(|name| name.eq_ignore_ascii_case("bomberman_bomb"))
                .expect("carrier keeps its own entry names");
            let native_set = carrier.entries[native].emitter_set_id as usize;
            assert!(
                ptcl.emitter_list.emitter_sets[native_set - 1]
                    .emitters
                    .is_empty(),
                "{label}: the carrier's own effect still has emitters"
            );

            // Every surviving reference must resolve, and every retained resource must be
            // referenced — the second half is what makes this a stripping test rather than a
            // repeat of the pre-encode safety check.
            let mut seen_shaders = std::collections::BTreeSet::new();
            let mut seen_textures: Vec<u64> = Vec::new();
            for set in &ptcl.emitter_list.emitter_sets {
                super::visit_emitters_ref(&set.emitters, &mut |em| {
                    let refs = &em.data.shader_references;
                    for index in [
                        refs.shader_index,
                        refs.user_shader_index1,
                        refs.user_shader_index2,
                    ] {
                        assert!(
                            shader_index_fits(index, standard),
                            "{label}: {}: shader variation {index} of {standard}",
                            set.name
                        );
                        if index >= 0 {
                            seen_shaders.insert(index);
                        }
                    }
                    assert!(
                        shader_index_fits(refs.compute_shader_index, compute),
                        "{label}: {}: compute variation {} of {compute}",
                        set.name,
                        refs.compute_shader_index
                    );
                    let d = &em.data;
                    append_unique_resource_ids(
                        &mut seen_textures,
                        [
                            d.sampler0.as_ref().map(|s| s.texture_id),
                            d.sampler1.as_ref().map(|s| s.texture_id),
                            d.sampler2.as_ref().map(|s| s.texture_id),
                            d.sampler3.as_ref().map(|s| s.texture_id),
                            d.sampler4.as_ref().map(|s| s.texture_id),
                            d.sampler5.as_ref().map(|s| s.texture_id),
                        ],
                    );
                });
            }
            assert_eq!(
                seen_shaders.len(),
                standard,
                "{label}: {} shader variations nothing references survived",
                standard - seen_shaders.len()
            );
            let textures = ptcl.texture_info.as_ref().expect("carrier has textures");
            for descriptor in &textures.descriptors {
                assert!(
                    seen_textures.contains(&descriptor.id),
                    "{label}: texture '{}' survived unreferenced",
                    descriptor.name
                );
            }
            assert!(
                seen_textures
                    .iter()
                    .all(|id| textures.descriptors.iter().any(|d| d.id == *id)),
                "{label}: an emitter samples a texture that was stripped"
            );

            eprintln!(
                "{label}: {} B, {standard} standard / {compute} compute variations, {} textures",
                built.len(),
                textures.descriptors.len()
            );
            // Bomberman's base is 3.4 MB before a donor is merged in and Pickel alone ships 370
            // shader variations. Guard the order of magnitude, not the exact figure.
            assert!(
                built.len() < 1_500_000,
                "{label}: carrier is {} B — stripping regressed",
                built.len()
            );
        }
    }

    #[test]
    fn native_carrier_transplant_keeps_its_emitter_resources() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let op = crate::mod_project::TransplantOp {
            new_entry_name: "bomberman_bomb_os".into(),
            src_file_rel: CARRIER.into(),
            src_set_name: "bomberman_bomb".into(),
            src_set_idx: 0,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        let built = super::rebuild_runtime_carrier_eff_bytes(&carrier_base, CARRIER, &[op], &root)
            .expect("native carrier build");
        assert_eq!(
            built, carrier_base,
            "a carrier-native selection must preserve the game's EFF exactly"
        );
        let carrier =
            effect_library::NamcoEffectFile::load(&built).expect("native carrier re-read");
        let index = carrier
            .entry_names
            .iter()
            .position(|name| name.eq_ignore_ascii_case("bomberman_bomb"))
            .expect("native entry");
        let set_id = carrier.entries[index].emitter_set_id as usize;
        assert!(
            !carrier
                .ptcl_file
                .as_ref()
                .expect("carrier PTCL")
                .emitter_list
                .emitter_sets[set_id - 1]
                .emitters
                .is_empty(),
            "the selected native carrier set must not be silenced"
        );
    }

    #[test]
    fn primitive_only_carrier_preserves_the_game_bfres() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const DAISY: &str = "effect/fighter/daisy/ef_daisy.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(DAISY)).expect("Daisy eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Daisy parse");
        let op = crate::mod_project::TransplantOp {
            new_entry_name: "daisy_flower_petals".into(),
            src_file_rel: DAISY.into(),
            src_set_name: "daisy_flower_petals".into(),
            src_set_idx: 0,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        let built = super::rebuild_runtime_carrier_eff_bytes(&carrier_base, CARRIER, &[op], &root)
            .expect("Daisy carrier build");
        let output = effect_library::NamcoEffectFile::load(&built).expect("Daisy carrier re-read");
        let source_primitives = donor
            .ptcl_file
            .as_ref()
            .and_then(|ptcl| ptcl.primitive_info.as_ref())
            .expect("Daisy primitive pool");
        let output_primitives = output
            .ptcl_file
            .as_ref()
            .and_then(|ptcl| ptcl.primitive_info.as_ref())
            .expect("carrier primitive pool");
        assert_eq!(
            output_primitives.binary_data, source_primitives.binary_data,
            "primitive-only effects must retain the donor's game-authored BFRES"
        );
        assert_eq!(
            output_primitives.descriptors.len(),
            source_primitives.descriptors.len(),
            "the raw BFRES must retain its matching descriptor table"
        );
    }
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
    // Scope resolution is deliberately strict: an edit names ONE set and ONE emitter, so
    // when the stored index already holds the stored name that pair wins outright. Falling
    // straight to `position()` retargeted the edit at the FIRST set of that name, which a
    // transplant clone of a multi-variant entry can easily duplicate.
    let set_named = |s: &effect_library::structs::EmitterSet| {
        !edit.set_name.is_empty() && s.name == edit.set_name
    };
    let set_idx = if sets.get(edit.set_idx).map(set_named).unwrap_or(false) {
        edit.set_idx
    } else {
        sets.iter().position(set_named).unwrap_or(edit.set_idx)
    };
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

    // Resolve the target emitter FIRST over a read-only, flat parent-first traversal (the
    // same order the editor enumerates), then write to exactly that one index. Same rule as
    // the set above: stored-name-at-stored-index wins, so identically-named sibling emitters
    // can never pull an edit off the emitter the user actually selected.
    let mut names: Vec<String> = Vec::new();
    visit_emitters_ref(&set.emitters, &mut |em| names.push(em.data.display_name()));
    let named = |i: usize| {
        !edit.emitter_name.is_empty() && names.get(i).map(|n| *n == edit.emitter_name) == Some(true)
    };
    let target = if named(edit.emitter_idx) {
        Some(edit.emitter_idx)
    } else if let Some(i) = (0..names.len()).find(|i| named(*i)) {
        eprintln!(
            "[EFF-EXPORT] warning: emitter '{}' of set '{}' moved from index {} to {}",
            edit.emitter_name, set.name, edit.emitter_idx, i
        );
        Some(i)
    } else if edit.emitter_idx < names.len() {
        if !edit.emitter_name.is_empty() {
            eprintln!(
                "[EFF-EXPORT] warning: emitter '{}' not found by name in set '{}' — using index {}",
                edit.emitter_name, set.name, edit.emitter_idx
            );
        }
        Some(edit.emitter_idx)
    } else {
        None
    };
    let target = target.ok_or_else(|| {
        anyhow!(
            "emitter '{}' (idx {}) not found in set '{}'",
            edit.emitter_name,
            edit.emitter_idx,
            set.name
        )
    })?;

    let mut flat_idx = 0usize;
    visit_emitters(&mut set.emitters, &mut flat_idx, &mut |idx, em| {
        if idx == target {
            apply_fields(em, &edit.fields);
        }
    });
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
            // Only the first `num_color0_keys` slots are live; the rest of the fixed-size
            // table is inert padding the editor never showed. A stale project file carrying
            // more rows than this emitter has keys must not spill into those slots.
            let live = d.emitter_static.num_color0_keys as usize;
            for (k, row) in d.emitter_static.color0.keys.iter_mut().take(live).zip(rows) {
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
            let live = d.emitter_static.num_color1_keys as usize;
            for (k, row) in d.emitter_static.color1.keys.iter_mut().take(live).zip(rows) {
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
    // Alpha follows the SAME source-precedence rule as the colors above — `alpha_keys` in
    // `effects.rs` reads the key table first, then a Constant ParticleColor alpha, then the
    // EmitterInfo static alpha. Writing unconditionally into the key table dropped every
    // alpha edit on an emitter with no alpha keys (the game ignores the inert table) while
    // scribbling over slots the editor never showed.
    if let Some(rows) = &f.alpha0 {
        let live = d.emitter_static.num_alpha0_keys as usize;
        if live > 0 {
            for (k, row) in d.emitter_static.alpha0.keys.iter_mut().take(live).zip(rows) {
                k.x = row[0];
            }
        } else if matches!(
            d.particle_color.alpha0_type,
            effect_library::ColorType::Constant
        ) {
            if let Some(row) = rows.first() {
                d.particle_color.alpha0 = row[0];
            }
        } else if let Some(row) = rows.first() {
            d.emitter_info.color0_a = row[0];
        }
    }
    // Serializer prefers the cached EMTR blob; clearing it forces a re-encode of `data`.
    em.cached_binary = None;
}
