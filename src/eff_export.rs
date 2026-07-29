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

use crate::mod_project::{AuthoredEdit, EffMod, EmitterFieldEdits, TextureImport, TransplantOp};

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
/// `warnings` collects problems that did not stop the build but DID change what ships: an
/// authored edit whose target was missing (dropped), or a model-data conflict between two
/// mesh-backed sources (see `preserve_raw_primitives`). The caller is expected to show these —
/// silently shipping a carrier that is missing an edit, or whose geometry is known-wrong, is
/// what made several of these faults take multiple test runs to pin down.
pub fn rebuild_runtime_carrier_eff_bytes_with_edits(
    carrier_bytes: &[u8],
    carrier_rel: &str,
    ops: &[TransplantOp],
    donor_root: &std::path::Path,
    authored: &[CarrierAuthored],
    textures: &[TextureImport],
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    // A carrier-native selection needs no transplant. The runtime remap already turns an `_os`
    // request into the carrier's existing real entry, so parsing, pruning, and repacking these
    // resource pools only introduces risk. Preserve the game's known-good payload byte-for-byte.
    // ...but only when there is nothing to bake in. With authored edits or an imported
    // texture the bytes MUST be rebuilt, otherwise those silently do not ship.
    if authored.is_empty()
        && textures.is_empty()
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

    // EVERY mesh-backed effect in the carrier keeps its geometry, and ONLY the models it
    // actually references. The merge above has already appended each donor's models to the
    // carrier's pool keyed by descriptor GUID; `prune_unreferenced_resources` then drops the
    // rest.
    //
    // A single mesh donor used to ship its pool VERBATIM, because a rebuilt pool did not draw
    // in game. That was a real defect, but in the WRITER, not in merging: it relocated the
    // wrong set of pointer slots — four words of header padding marked as pointers, five real
    // pointers left unrebased — so the game rewrote padding and followed addresses it never
    // fixed up. Perfect in software, invisible on hardware, which is why every structural test
    // passed. Measured against the 296 game containers under `effect/`, splitting one into
    // single-model exports and merging them back now reproduces its GPU region byte for byte,
    // and its relocation set on 292. See `bfres_merge_fidelity.rs` / `bfres_rlt_semantics.rs`.
    //
    // The shortcut is gone rather than merely narrowed: shipping a donor's pool whole is what
    // put all 39 of Kirby's models in a carrier that referenced 2 of them, and there is no way
    // to strip a pool without rebuilding it.

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
    // Textures an edit will SWAP TO are not sampled by anything yet — hold them back from the
    // prune, which otherwise drops them a few lines before the swap looks for them.
    let swap_targets: Vec<String> = authored
        .iter()
        .flat_map(|entry| entry.edits.iter())
        .filter_map(|edit| edit.fields.texture_name.clone())
        .collect();
    prune_unreferenced_resources(&mut carrier, &swap_targets)?;
    // Compaction trims the shader container to the variations something addresses. Verified
    // safe on game data: `BnshFile` rewriting preserves every variation's byte code, control
    // code and object data, and the `_RLT` table it emits describes its own pointers correctly
    // (effect_library's `bnsh_relocation` tests, run against real containers).
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
    //
    // An edit whose target entry is missing is SKIPPED, not fatal. Failing the whole build
    // here meant one stale edit took down every transplant in the snapshot — a working
    // bomberman_bomb transplant went invisible because an unrelated kirby_dash edit could not
    // be placed. Transplants and edits are independent features and must fail independently.
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
                warnings.push(format!("edit:{}", entry.set_name));
                continue;
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

    apply_texture_imports(&mut carrier, textures, warnings)?;

    carrier
        .save()
        .context("effect_library failed to encode the runtime carrier .eff")
}

/// Replace pool textures with the user's own images.
///
/// Runs LAST, on the final pool: pruning has already settled which textures survive and any
/// swap edits have already repointed samplers, so an import lands on the texture that will
/// actually be sampled. An import naming a texture this file does not hold is a WARNING, not
/// a failure — same rule as a missing authored edit. Transplants and imports are independent
/// features, and one stale import must not take a working carrier down with it.
fn apply_texture_imports(
    file: &mut effect_library::NamcoEffectFile,
    imports: &[TextureImport],
    warnings: &mut Vec<String>,
) -> Result<()> {
    if imports.is_empty() {
        return Ok(());
    }
    let Some(textures) = file
        .ptcl_file
        .as_mut()
        .and_then(|ptcl| ptcl.texture_info.as_mut())
    else {
        for import in imports {
            warnings.push(format!("texture:{}", import.texture_name));
        }
        return Ok(());
    };

    for import in imports {
        let Some(index) = textures
            .descriptors
            .iter()
            .position(|d| d.name == import.texture_name)
        else {
            warnings.push(format!("texture:{}", import.texture_name));
            continue;
        };
        let Some(pool) = textures.binary_data.as_ref() else {
            warnings.push(format!("texture:{}", import.texture_name));
            continue;
        };
        let png = std::fs::read(&import.png_path).with_context(|| {
            format!(
                "texture import for '{}': cannot read {}",
                import.texture_name, import.png_path
            )
        })?;
        let names: Vec<String> = textures.descriptors.iter().map(|d| d.name.clone()).collect();
        let (rebuilt, report) = crate::texture_import::replace_with_png(pool, &names, index, &png)
            .with_context(|| format!("importing {} over '{}'", import.png_path, import.texture_name))?;
        if let Some(original) = &report.format_substituted_from {
            // The user asked for THIS image on THIS texture and got it, but not in the format
            // the game shipped — worth saying, since a normal map re-encoded as colour will
            // look wrong in a way that has nothing to do with the image they picked.
            warnings.push(format!(
                "texture-format:{} was {original}, imported as {}",
                import.texture_name, report.format
            ));
        }
        textures.binary_data = Some(rebuilt);
    }
    Ok(())
}

/// Reduce a donor eff to the named entries plus only the resources those entries reference.
///
/// The plugin co-loads the donor eff whole so a foreign kind is resident in a match the donor's
/// character is not in. It used to be sent WHOLE: `ef_marx.eff` is 20 MB, and the transport
/// base64s it into a single JSON frame, so one Marx effect cost ~27 MB on the wire. Two donors
/// put ~43 MB through a socket the emulator reads in 8 KB chunks — the reported 30 s timeout.
///
/// Stripping was tried once and reverted, because a stripped mesh effect ("alucard_backdash")
/// spawned and rendered nothing. That is the exact signature of the BFRES relocation defect
/// since fixed and corpus-calibrated: the pool survived the trip structurally intact and was
/// unusable on hardware. The passes used here are the same three the carrier has always used,
/// and the carrier renders.
///
/// Entries and sets are kept and merely emptied, never deleted, so every id still resolves —
/// deleting an entry leaves the item's own spawn requests resolving against nothing.
pub fn strip_donor_eff_bytes(src: &[u8], keep_entries: &[&str]) -> Result<Vec<u8>> {
    let mut file = effect_library::NamcoEffectFile::load(src)
        .context("effect_library failed to parse the donor .eff")?;
    silence_carrier_native_effects(&mut file, keep_entries);
    prune_unreferenced_resources(&mut file, &[])?;
    compact_shader_containers(&mut file)?;
    file.save()
        .context("effect_library failed to encode the stripped donor .eff")
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
///
/// `keep_textures` names textures that must survive even though nothing samples them YET. The
/// carrier applies authored edits AFTER this pass (so an edit can never mask a bad transplant
/// clone), which means a texture-swap edit's TARGET is unreferenced at prune time and would be
/// thrown away right before the swap tried to point at it.
fn prune_unreferenced_resources(
    carrier: &mut effect_library::NamcoEffectFile,
    keep_textures: &[String],
) -> Result<()> {
    let Some(ptcl) = carrier.ptcl_file.as_mut() else {
        return Ok(());
    };
    let mut used_textures: Vec<u64> = Vec::new();
    for name in keep_textures {
        let id = ptcl
            .texture_info
            .as_ref()
            .and_then(|info| info.descriptors.iter().find(|d| d.name == *name))
            .map(|d| d.id);
        if let Some(id) = id {
            append_unique_resource_ids(&mut used_textures, [Some(id)]);
        }
    }
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

    {
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
#[cfg(test)]
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
    // This path does not prune, so an import only has to find its texture by name. Warnings
    // go to stderr rather than a channel: the export UI reports the file it wrote, and a
    // missing import here has already been surfaced by the live carrier build.
    let mut warnings = Vec::new();
    apply_texture_imports(&mut namco, &eff.textures, &mut warnings)?;
    for warning in &warnings {
        eprintln!("[EFF-EXPORT] warning: {warning}");
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

    /// A texture swap must repoint exactly one emitter's `sampler0` and touch nothing else.
    ///
    /// The swap is authored as a NAME; this is what proves the name resolves to the right
    /// GUID, and that resolving it does not disturb the sibling emitters that keep sampling
    /// the original texture.
    #[test]
    fn a_texture_swap_repoints_only_the_targeted_emitter() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let source = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let before = emitter_snapshot(&source);
        let ptcl = source.ptcl_file.as_ref().expect("source PTCL");
        let descriptors = &ptcl.texture_info.as_ref().expect("textures").descriptors;

        // Find an emitter that actually samples something, and a DIFFERENT texture to send it
        // to — swapping a texture for itself would pass without proving anything.
        let mut found = None;
        for (set_idx, set) in ptcl.emitter_list.emitter_sets.iter().enumerate() {
            let mut idx = 0usize;
            super::visit_emitters_ref(&set.emitters, &mut |em| {
                if found.is_none() {
                    if let Some(sampler) = em.data.sampler0.as_ref() {
                        if descriptors.iter().any(|d| d.id == sampler.texture_id) {
                            found = Some((
                                set_idx,
                                set.name.clone(),
                                idx,
                                em.data.display_name(),
                                sampler.texture_id,
                            ));
                        }
                    }
                }
                idx += 1;
            });
            if found.is_some() {
                break;
            }
        }
        let (set_idx, set_name, emitter_idx, emitter_name, original_id) =
            found.expect("an emitter that samples a pool texture");
        let target = descriptors
            .iter()
            .find(|d| d.id != original_id)
            .expect("a second texture to swap to")
            .clone();

        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            authored: vec![AuthoredEdit {
                set_name: set_name.clone(),
                entry_name: String::new(),
                set_idx,
                emitter_name: emitter_name.clone(),
                emitter_idx,
                fields: EmitterFieldEdits {
                    texture_name: Some(target.name.clone()),
                    ..Default::default()
                },
            }],
            transplants: Vec::new(),
            textures: Vec::new(),
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, None).expect("rebuild");
        let after = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");

        let mut swapped = None;
        let ptcl = after.ptcl_file.as_ref().expect("rebuilt PTCL");
        let mut idx = 0usize;
        super::visit_emitters_ref(
            &ptcl.emitter_list.emitter_sets[set_idx].emitters,
            &mut |em| {
                if idx == emitter_idx {
                    swapped = em.data.sampler0.as_ref().map(|s| s.texture_id);
                }
                idx += 1;
            },
        );
        assert_eq!(
            swapped,
            Some(target.id),
            "the targeted emitter should now sample '{}'",
            target.name
        );

        // Every OTHER emitter must be untouched.
        let after_all = emitter_snapshot(&after);
        assert_eq!(before.len(), after_all.len(), "emitter count changed");
        for (i, (b, a)) in before.iter().zip(&after_all).enumerate() {
            let is_target = b.0 == set_name && b.1 == emitter_idx;
            if is_target {
                assert_ne!(b.2, a.2, "the targeted emitter did not change");
            } else {
                assert_eq!(b.2, a.2, "emitter {i} ({}, {}) changed", b.0, b.1);
            }
        }
    }

    /// An imported PNG must reach the pool, and the eff must still parse around it with the
    /// same textures under the same names.
    #[test]
    fn a_texture_import_replaces_the_pool_texture_in_place() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let source = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let textures = source
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .expect("textures");
        let names: Vec<String> = textures.descriptors.iter().map(|d| d.name.clone()).collect();
        let original_pool = textures.binary_data.clone().expect("pool");

        // Replace the first texture Visionary can convert.
        let name = names
            .iter()
            .enumerate()
            .find(|(i, n)| {
                crate::texture_import::describe(&original_pool, *i, n)
                    .map(|d| d.convertible)
                    .unwrap_or(false)
            })
            .map(|(_, n)| n.clone())
            .expect("a convertible texture");

        let scratch = tempfile::tempdir().expect("scratch dir");
        let png_path = scratch.path().join("import.png");
        let mut image = image::RgbaImage::new(64, 64);
        for (x, y, px) in image.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 4) as u8, (y * 4) as u8, 0xC0, 0xFF]);
        }
        image.save(&png_path).expect("write png");

        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            authored: Vec::new(),
            transplants: Vec::new(),
            textures: vec![crate::mod_project::TextureImport {
                texture_name: name.clone(),
                png_path: png_path.to_string_lossy().to_string(),
            }],
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, None).expect("rebuild");
        let after = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");
        let after_textures = after
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .expect("textures survived");

        // The serializer sorts the descriptor table on save (and reorders the archive to
        // match), so compare the SET of names — order here is the writer's business, and the
        // only thing an import must not do is add or drop one.
        let mut after_names: Vec<String> = after_textures
            .descriptors
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let mut expected = names.clone();
        after_names.sort();
        expected.sort();
        assert_eq!(after_names, expected, "the import lost or added a texture");
        let new_pool = after_textures.binary_data.as_ref().expect("pool survived");
        assert_ne!(*new_pool, original_pool, "the pool was not changed");

        let new_index = after_textures
            .descriptors
            .iter()
            .position(|d| d.name == name)
            .expect("the imported texture is still in the pool");
        let described =
            crate::texture_import::describe(new_pool, new_index, &name).expect("describe imported");
        assert_eq!(
            (described.width, described.height),
            (64, 64),
            "the imported image's dimensions did not reach the pool"
        );
    }

    /// An import naming a texture the file does not hold must warn and leave everything else
    /// working — the same rule authored edits follow. One stale import cannot be allowed to
    /// take down a build carrying unrelated transplants.
    #[test]
    fn a_stale_texture_import_warns_instead_of_failing() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let mut file = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let mut warnings = Vec::new();
        super::apply_texture_imports(
            &mut file,
            &[crate::mod_project::TextureImport {
                texture_name: "ef_not_a_real_texture".into(),
                // Deliberately a path that does not exist: a missing NAME must be caught
                // before the file is ever read, so this must not surface as an IO error.
                png_path: "/nonexistent/nope.png".into(),
            }],
            &mut warnings,
        )
        .expect("a stale import must not fail the build");
        assert_eq!(warnings, vec!["texture:ef_not_a_real_texture".to_string()]);
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
            textures: Vec::new(),
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
            let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
                &carrier_base,
                CARRIER,
                &ops,
                &root,
                &[],
                &[],
                &mut Vec::new(),
            )
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
            // Compaction is not asserted here — what matters is the SAFETY property, that every
            // variation an emitter addresses exists. Byte-identity is the wrong bar for a
            // rewritten container: effect_library's `bnsh_relocation` tests establish, against
            // real game containers, that a rewrite preserves each variation's shader model and
            // emits a relocation table consistent with its own layout.
            assert!(
                seen_shaders.len() <= standard,
                "{label}: an emitter addresses a shader variation the container lacks"
            );
            assert!(
                seen_shaders.iter().all(|i| (*i as usize) < standard),
                "{label}: a shader index points past the end of the container"
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
            // Order of magnitude only. This used to guard 1.5 MB, which assumed the shader
            // container was compacted; a single-donor carrier now carries the donor's whole
            // library instead (Kirby's is 1.7 MB on its own) because a compacted one is not
            // GPU-faithful. Textures and primitives are still stripped, so the ceiling that
            // matters — not hauling whole donor files across — still holds.
            assert!(
                built.len() < 8_000_000,
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
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            &[],
            &[],
            &mut Vec::new(),
        )
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

    /// Every primitive GUID the file's emitters reference (excluding the "none" sentinels).
    pub(super) fn referenced_primitive_ids(file: &effect_library::NamcoEffectFile) -> Vec<u64> {
        let mut out = Vec::new();
        let Some(ptcl) = file.ptcl_file.as_ref() else {
            return out;
        };
        for set in &ptcl.emitter_list.emitter_sets {
            super::visit_emitters_ref(&set.emitters, &mut |em| {
                for id in [
                    em.data.particle_data.primitive_id,
                    em.data.particle_data.primitive_ex_id,
                    em.data.shape_info.primitive_index,
                ] {
                    if id != 0 && id != u64::MAX && !out.contains(&id) {
                        out.push(id);
                    }
                }
            });
        }
        out
    }

    /// The vertex and index bytes of the model a GUID addresses, or None if absent.
    ///
    /// Compares GEOMETRY, not container bytes: a single-model export inherits its source
    /// container's string-pool order, so two pools describing identical meshes serialise
    /// differently while drawing the same thing.
    #[allow(clippy::type_complexity)]
    fn geometry_for_id(
        file: &effect_library::NamcoEffectFile,
        id: u64,
    ) -> Option<(Vec<Vec<Vec<u8>>>, Vec<Vec<u8>>)> {
        let prim = file.ptcl_file.as_ref()?.primitive_info.as_ref()?;
        let index = effect_library::bfres::descriptor_index_for_id(&prim.descriptors, id)?;
        let blob =
            effect_library::bfres::export_single_model(prim.binary_data.as_ref()?, index).ok()?;
        let (_, model) = effect_library::bfres::ResFile::parse_model_export(blob).ok()?;
        Some((
            model
                .vertex_buffers
                .iter()
                .map(|vb| vb.buffers.clone())
                .collect(),
            model
                .shapes
                .values()
                .flat_map(|s| s.meshes.iter().map(|m| m.index_data.clone()))
                .collect(),
        ))
    }

    /// The carrier's mesh pool holds EXACTLY the models its emitters use — no dead models, none
    /// missing — and each one is the same geometry the source shipped.
    ///
    /// A source may legitimately not own an id: vanilla emitters keep nonzero values in inactive
    /// primitive fields and reference globally-owned models the file never carries
    /// (ef_bomberman names 0x35ca927c, which neither it nor Kirby owns). Those are not required.
    pub(super) fn assert_pool_is_exactly_what_is_used(
        output: &effect_library::NamcoEffectFile,
        sources: &[&effect_library::NamcoEffectFile],
    ) {
        let referenced = referenced_primitive_ids(output);
        let shipped: Vec<u64> = output
            .ptcl_file
            .as_ref()
            .and_then(|p| p.primitive_info.as_ref())
            .map(|p| p.descriptors.iter().map(|d| d.id).collect())
            .unwrap_or_default();

        let dead: Vec<String> = shipped
            .iter()
            .filter(|id| !referenced.contains(id))
            .map(|id| format!("{id:#x}"))
            .collect();
        assert!(
            dead.is_empty(),
            "the carrier ships {} model(s) nothing references: {}",
            dead.len(),
            dead.join(", ")
        );

        let mut checked = 0usize;
        for id in &referenced {
            let Some(source) = sources.iter().find(|s| geometry_for_id(s, *id).is_some()) else {
                continue; // globally-owned; no local model to ship
            };
            assert!(
                shipped.contains(id),
                "primitive {id:#x} is referenced and owned by a source, but was stripped"
            );
            assert_eq!(
                geometry_for_id(output, *id),
                geometry_for_id(source, *id),
                "primitive {id:#x} lost or changed its geometry in the carrier"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "no mesh-backed primitive was checked — the fixture is not exercising meshes"
        );
    }

    /// A stripped donor keeps the effect asked for — emitters, meshes and textures — and
    /// drops everything else.
    ///
    /// The donor eff is co-loaded by the plugin and used to be sent whole: 20 MB for one Marx
    /// effect, base64'd into a single JSON frame. Stripping was reverted once when a stripped
    /// mesh effect rendered nothing, which was the BFRES relocation defect, not the strip.
    #[test]
    fn a_stripped_donor_keeps_the_effect_it_was_asked_for() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        // Kirby is the demanding case: kirby_dash is MIXED — particles plus a mesh cone — so it
        // exercises the pool that used to arrive intact-but-undrawable.
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let full = effect_library::NamcoEffectFile::load(&bytes).expect("Kirby parse");
        let stripped_bytes =
            super::strip_donor_eff_bytes(&bytes, &["kirby_dash"]).expect("strip Kirby");
        let stripped =
            effect_library::NamcoEffectFile::load(&stripped_bytes).expect("stripped parse");

        assert!(
            stripped_bytes.len() < bytes.len() / 2,
            "stripping one effect out of Kirby should be a large saving, got {} of {} B",
            stripped_bytes.len(),
            bytes.len()
        );

        // The kept effect still resolves and still emits.
        let index = stripped
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("kirby_dash"))
            .expect("kirby_dash entry survives");
        let set_idx = stripped.entries[index].emitter_set_id as usize - 1;
        let set = &stripped
            .ptcl_file
            .as_ref()
            .expect("stripped PTCL")
            .emitter_list
            .emitter_sets[set_idx];
        assert!(
            !set.emitters.is_empty(),
            "the kept effect lost its emitters"
        );

        // Its geometry is intact and nothing dead came along.
        assert_pool_is_exactly_what_is_used(&stripped, &[&full]);

        // Every texture it references is still in the archive.
        let tex_ids: Vec<u64> = stripped
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .map(|t| t.descriptors.iter().map(|d| d.id).collect())
            .unwrap_or_default();
        let mut wanted: Vec<u64> = Vec::new();
        super::visit_emitters_ref(&set.emitters, &mut |em| {
            let d = &em.data;
            super::append_unique_resource_ids(
                &mut wanted,
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
        assert!(!wanted.is_empty(), "kirby_dash references no textures?");
        let missing: Vec<String> = wanted
            .iter()
            .filter(|id| !tex_ids.contains(id))
            .map(|id| format!("{id:#x}"))
            .collect();
        assert!(
            missing.is_empty(),
            "stripped donor lost {} texture(s) the kept effect uses: {}",
            missing.len(),
            missing.join(", ")
        );
    }

    /// A primitive-only transplant keeps the models it uses and drops the rest.
    #[test]
    fn a_primitive_only_transplant_ships_only_the_models_it_uses() {
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
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            &[],
            &[],
            &mut Vec::new(),
        )
        .expect("Daisy carrier build");
        let output = effect_library::NamcoEffectFile::load(&built).expect("Daisy carrier re-read");
        let carrier_src =
            effect_library::NamcoEffectFile::load(&carrier_base).expect("carrier parse");
        assert_pool_is_exactly_what_is_used(&output, &[&donor, &carrier_src]);
        let source_count = donor
            .ptcl_file
            .as_ref()
            .and_then(|ptcl| ptcl.primitive_info.as_ref())
            .expect("Daisy primitive pool")
            .descriptors
            .len();
        let output_count = output
            .ptcl_file
            .as_ref()
            .and_then(|ptcl| ptcl.primitive_info.as_ref())
            .expect("carrier primitive pool")
            .descriptors
            .len();
        assert!(
            output_count < source_count,
            "one effect should not need all {source_count} of the donor's models"
        );
    }

    /// An AUTHORED EDIT on a mesh-backed fighter effect must ship the mesh, not just the
    /// particles around it.
    ///
    /// This is the "big ball of fire instead of the cone" report: kirby_dash is a MIXED effect
    /// (particle emitters plus one primitive-backed cone). Edits clone the fighter's entry into
    /// the carrier, and the clone must arrive with (a) the donor's primitive pool intact and
    /// (b) every primitive id its emitters reference still present in that pool. A carrier
    /// whose emitters name primitives the pool lacks renders the emitter with no geometry,
    /// which is exactly what a missing cone looks like.
    #[test]
    fn authored_edit_on_a_mesh_effect_keeps_its_primitives() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Kirby parse");

        // Resolve kirby_dash the way the app does: entry name → emitter_set_id → set index.
        let entry_index = donor
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("kirby_dash"))
            .expect("kirby_dash entry");
        let set_idx = donor.entries[entry_index].emitter_set_id as usize - 1;
        let set_name = donor
            .ptcl_file
            .as_ref()
            .expect("Kirby PTCL")
            .emitter_list
            .emitter_sets[set_idx]
            .name
            .clone();

        // The editor stores the clone under its own reserved name and aliases the real kind
        // onto it — mirror that, including the authored edit that forces a full rebuild.
        let clone_name = format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX);
        let op = crate::mod_project::TransplantOp {
            new_entry_name: clone_name.clone(),
            src_file_rel: KIRBY.into(),
            src_set_name: "kirby_dash".into(),
            src_set_idx: set_idx,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        let authored = super::CarrierAuthored {
            set_name: clone_name,
            edits: vec![crate::mod_project::AuthoredEdit {
                set_name,
                entry_name: "kirby_dash".into(),
                set_idx,
                emitter_name: String::new(),
                emitter_idx: 0,
                fields: crate::mod_project::EmitterFieldEdits {
                    scale: Some(1.5),
                    ..Default::default()
                },
            }],
        };
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            std::slice::from_ref(&authored),
            &[],
            &mut Vec::new(),
        )
        .expect("authored kirby carrier build");
        let output = effect_library::NamcoEffectFile::load(&built).expect("carrier re-read");

        // Every primitive id the surviving emitters reference must exist in the shipped pool.
        let out_ptcl = output.ptcl_file.as_ref().expect("carrier PTCL");
        let pool: Vec<u64> = out_ptcl
            .primitive_info
            .as_ref()
            .map(|p| p.descriptors.iter().map(|d| d.id).collect())
            .unwrap_or_default();
        let mut wanted: Vec<u64> = Vec::new();
        for set in &out_ptcl.emitter_list.emitter_sets {
            super::visit_emitters_ref(&set.emitters, &mut |em| {
                for id in [
                    em.data.particle_data.primitive_id,
                    em.data.particle_data.primitive_ex_id,
                    em.data.shape_info.primitive_index,
                ] {
                    // 0 and u64::MAX are the "no primitive" sentinels.
                    if id != 0 && id != u64::MAX && !wanted.contains(&id) {
                        wanted.push(id);
                    }
                }
            });
        }
        // Only IDs a SOURCE container actually owns are required. Vanilla emitters keep nonzero
        // values in inactive primitive fields and reference globally-owned models the file does
        // not carry — ef_bomberman itself references 0x35ca927c, which neither it nor Kirby owns.
        // Demanding those would fail against the game's own data.
        let owns = |file: &effect_library::NamcoEffectFile, id: u64| {
            file.ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.as_ref())
                .map(|p| p.descriptors.iter().any(|d| d.id == id))
                .unwrap_or(false)
        };
        let carrier_src =
            effect_library::NamcoEffectFile::load(&carrier_base).expect("carrier parse");
        let missing: Vec<String> = wanted
            .iter()
            .filter(|id| owns(&donor, **id) || owns(&carrier_src, **id))
            .filter(|id| !pool.contains(id))
            .map(|id| format!("{id:#x}"))
            .collect();
        assert!(
            missing.is_empty(),
            "carrier emitters reference {} primitive(s) the shipped pool lacks: {} \
             (pool has {} descriptors)",
            missing.len(),
            missing.join(", "),
            pool.len()
        );
        assert!(
            !wanted.is_empty(),
            "kirby_dash is mesh-backed; the rebuilt carrier reference no primitives at all"
        );

        // And the shipped models must be the donor's geometry, with nothing dead alongside.
        assert_pool_is_exactly_what_is_used(&output, &[&donor, &carrier_src]);
    }

    /// The EMTR writer must round-trip a MESH-backed emitter losslessly.
    ///
    /// Every carrier path clears `cached_binary` (shader indices move, so the pre-serialized
    /// blob captured at load time is stale) and re-serializes the emitter from the parsed
    /// struct. Particle emitters demonstrably survive that — authored colour edits reach the
    /// game correctly. If the writer drops or reorders a field only mesh emitters use, the
    /// geometry reference would be lost while everything around it still rendered, which is
    /// what "a big ball of fire instead of the cone" looks like. Prove it here rather than
    /// inferring it from a screenshot.
    #[test]
    fn mesh_emitter_survives_a_cached_blob_invalidation() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("Kirby parse");
        let ptcl = file.ptcl_file.as_ref().expect("Kirby PTCL");
        let version = ptcl.vfx_version;

        let uses_mesh = |d: &effect_library::emitter::EmitterData| {
            [
                d.particle_data.primitive_id,
                d.particle_data.primitive_ex_id,
                d.shape_info.primitive_index,
            ]
            .iter()
            .any(|id| *id != 0 && *id != u64::MAX)
        };

        let mut mesh_seen = 0usize;
        let mut diffs: Vec<String> = Vec::new();
        for set in &ptcl.emitter_list.emitter_sets {
            let mut index = 0usize;
            super::visit_emitters_ref(&set.emitters, &mut |em| {
                let here = index;
                index += 1;
                if !uses_mesh(&em.data) {
                    return;
                }
                mesh_seen += 1;
                // `binary_data` is the emitter EXACTLY as the game ships it. Re-serializing
                // from the parsed struct is what the carrier build is forced to do.
                let Some(on_disk) = em.binary_data.as_ref() else {
                    return;
                };
                let rewritten = match em.data.write(version) {
                    Ok(b) => b,
                    Err(e) => {
                        diffs.push(format!("{}#{here}: write failed: {e}", set.name));
                        return;
                    }
                };
                if rewritten.as_slice() != on_disk.as_slice() {
                    let where_ = rewritten
                        .iter()
                        .zip(on_disk.iter())
                        .position(|(a, b)| a != b)
                        .map(|p| format!("byte {p:#x}"))
                        .unwrap_or_else(|| {
                            format!("length {} vs {}", rewritten.len(), on_disk.len())
                        });
                    diffs.push(format!("{}#{here}: differs at {where_}", set.name));
                }
            });
        }
        eprintln!("{mesh_seen} mesh-backed emitters checked in {KIRBY}");
        assert!(
            mesh_seen > 0,
            "Kirby's eff has no mesh-backed emitters — pick a different fixture"
        );
        assert!(
            diffs.is_empty(),
            "the EMTR writer does not round-trip {} mesh emitter(s):\n  {}",
            diffs.len(),
            diffs.join("\n  ")
        );
    }

    /// The carrier's copy of an edited set must differ from the fighter's original ONLY in
    /// the fields the edit touched and the shader indices relocation moves.
    ///
    /// Anything else that changed is a candidate for the mesh emitter rendering wrong, and
    /// this prints the full list rather than asserting a guess.
    #[test]
    fn carrier_copy_differs_from_the_original_only_where_expected() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Kirby parse");
        let donor_ptcl = donor.ptcl_file.as_ref().expect("Kirby PTCL");

        let entry_index = donor
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("kirby_dash"))
            .expect("kirby_dash entry");
        let set_idx = donor.entries[entry_index].emitter_set_id as usize - 1;
        let set_name = donor_ptcl.emitter_list.emitter_sets[set_idx].name.clone();

        let clone_name = format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX);
        let op = crate::mod_project::TransplantOp {
            new_entry_name: clone_name.clone(),
            src_file_rel: KIRBY.into(),
            src_set_name: "kirby_dash".into(),
            src_set_idx: set_idx,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        // No authored field changes: any difference that shows up is structural, not an edit.
        let authored = super::CarrierAuthored {
            set_name: clone_name.clone(),
            edits: Vec::new(),
        };
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            std::slice::from_ref(&authored),
            &[],
            &mut Vec::new(),
        )
        .expect("carrier build");
        let output = effect_library::NamcoEffectFile::load(&built).expect("carrier re-read");
        let out_ptcl = output.ptcl_file.as_ref().expect("carrier PTCL");
        let out_index = output
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&clone_name))
            .expect("cloned entry in carrier");
        let out_set = &out_ptcl.emitter_list.emitter_sets
            [output.entries[out_index].emitter_set_id as usize - 1];
        let src_set = &donor_ptcl.emitter_list.emitter_sets[set_idx];

        let mut src: Vec<effect_library::emitter::EmitterData> = Vec::new();
        super::visit_emitters_ref(&src_set.emitters, &mut |em| src.push(em.data.clone()));
        let mut out: Vec<effect_library::emitter::EmitterData> = Vec::new();
        super::visit_emitters_ref(&out_set.emitters, &mut |em| out.push(em.data.clone()));
        assert_eq!(
            src.len(),
            out.len(),
            "the carrier copy of '{set_name}' has a different emitter count"
        );

        // Normalize away the differences we KNOW are legitimate, then require byte equality
        // of the re-serialized emitters. Shader indices are relocated onto the merged
        // containers, so copy the carrier's back over the donor's before comparing.
        let version = donor_ptcl.vfx_version;
        let mut differing: Vec<String> = Vec::new();
        for (i, (a, b)) in src.iter().zip(out.iter()).enumerate() {
            let mut normalized = a.clone();
            normalized.shader_references = b.shader_references.clone();
            let want = normalized.write(version).expect("write donor emitter");
            let got = b.clone().write(version).expect("write carrier emitter");
            if want != got {
                let at = want
                    .iter()
                    .zip(got.iter())
                    .position(|(x, y)| x != y)
                    .map(|p| format!("byte {p:#x}"))
                    .unwrap_or_else(|| format!("length {} vs {}", want.len(), got.len()));
                differing.push(format!(
                    "emitter #{i} ({}) differs at {at}",
                    a.display_name()
                ));
            }
        }
        assert!(
            differing.is_empty(),
            "the carrier's copy of '{set_name}' diverges from the fighter's original beyond \
             shader relocation:\n  {}",
            differing.join("\n  ")
        );
    }

    /// Editing an untransplanted fighter effect clones it into the carrier with the edit
    /// baked in — and editing it AGAIN regenerates the carrier with the new value.
    ///
    /// This is the whole authored-edit mechanism in one test: the clone lands under the
    /// reserved internal name (so it cannot collide with the fighter's resident kind), the
    /// edited value is really in the shipped bytes, and a second build with a different value
    /// produces different bytes rather than reusing anything.
    #[test]
    fn an_authored_edit_clones_into_the_carrier_and_re_edits_regenerate() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Kirby parse");
        let donor_ptcl = donor.ptcl_file.as_ref().expect("Kirby PTCL");

        let entry_index = donor
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("kirby_dash"))
            .expect("kirby_dash entry");
        let set_idx = donor.entries[entry_index].emitter_set_id as usize - 1;
        let src_set = &donor_ptcl.emitter_list.emitter_sets[set_idx];
        let emitter_name = src_set.emitters[0].data.display_name();

        // Exactly what the app builds for an edit on an UNTRANSPLANTED entry: one clone op
        // under the reserved prefix, plus the edits keyed to that clone name.
        let clone_name = format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX);
        let op = crate::mod_project::TransplantOp {
            new_entry_name: clone_name.clone(),
            src_file_rel: KIRBY.into(),
            src_set_name: "kirby_dash".into(),
            src_set_idx: set_idx,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };

        // `scale` is applied as a RATIO against the emitter's own base, so read that first and
        // ask for a value that is genuinely different from it.
        let base_scale = src_set.emitters[0].data.particle_scale.scale_x;
        assert!(
            base_scale.abs() > 1e-6,
            "fixture emitter has no usable base scale"
        );

        let build = |scale: f32| -> Vec<u8> {
            let authored = super::CarrierAuthored {
                set_name: clone_name.clone(),
                edits: vec![crate::mod_project::AuthoredEdit {
                    set_name: clone_name.clone(),
                    entry_name: "kirby_dash".into(),
                    set_idx: 0,
                    emitter_name: emitter_name.clone(),
                    emitter_idx: 0,
                    fields: crate::mod_project::EmitterFieldEdits {
                        scale: Some(scale),
                        ..Default::default()
                    },
                }],
            };
            let mut skipped = Vec::new();
            let bytes = super::rebuild_runtime_carrier_eff_bytes_with_edits(
                &carrier_base,
                CARRIER,
                std::slice::from_ref(&op),
                &root,
                std::slice::from_ref(&authored),
                &[],
                &mut skipped,
            )
            .expect("carrier build");
            assert!(
                skipped.is_empty(),
                "the edit was dropped instead of applied: {skipped:?}"
            );
            bytes
        };

        // Read back the cloned entry's first emitter scale from a built carrier.
        let scale_in = |bytes: &[u8]| -> f32 {
            let out = effect_library::NamcoEffectFile::load(bytes).expect("carrier re-read");
            let ptcl = out.ptcl_file.as_ref().expect("carrier PTCL");
            let idx = out
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(&clone_name))
                .expect("the edit clone is missing from the carrier");
            let set = &ptcl.emitter_list.emitter_sets[out.entries[idx].emitter_set_id as usize - 1];
            set.emitters[0].data.particle_scale.scale_x
        };

        let first = build(base_scale * 2.0);
        assert!(
            (scale_in(&first) - base_scale * 2.0).abs() < 1e-4,
            "the first edit did not reach the carrier: got {}, wanted {}",
            scale_in(&first),
            base_scale * 2.0
        );

        // Edit again — a fresh build from the same pristine carrier base must carry the NEW
        // value, not the old one and not a cached result.
        let second = build(base_scale * 5.0);
        assert!(
            (scale_in(&second) - base_scale * 5.0).abs() < 1e-4,
            "the re-edit did not regenerate the carrier: got {}, wanted {}",
            scale_in(&second),
            base_scale * 5.0
        );
        assert_ne!(
            first, second,
            "two different edits produced identical carrier bytes — nothing regenerated"
        );

        // And the fighter's own kind name must NOT appear: the clone lives in the reserved
        // namespace precisely so it cannot collide with the resident kirby_dash.
        let out = effect_library::NamcoEffectFile::load(&second).expect("carrier re-read");
        assert!(
            !out.entry_names
                .iter()
                .any(|n| n.eq_ignore_ascii_case("kirby_dash")),
            "the clone was stored under the fighter's own kind name — that collides in game"
        );
    }

    /// Dump the game's own shader + model containers so the effect_library diagnostics can be
    /// run against REAL data. Its own round-trip tests only cover synthetic containers, which is
    /// exactly the gap that let an unfaithful rewrite go unnoticed.
    #[test]
    fn dump_game_containers_for_library_diagnostics() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            return;
        };
        let Some(out) = std::env::var_os("VISIONARY_DUMP_DIR").map(std::path::PathBuf::from) else {
            return;
        };
        let _ = std::fs::create_dir_all(&out);
        for rel in [
            "effect/assist/bomberman/ef_bomberman.eff",
            "effect/fighter/kirby/ef_kirby.eff",
        ] {
            let bytes = std::fs::read(root.join(rel)).expect("eff");
            let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
            let stem = rel.rsplit('/').next().unwrap().replace(".eff", "");
            let ptcl = file.ptcl_file.as_ref().expect("ptcl");
            if let Some(b) = ptcl
                .shader_info
                .as_ref()
                .and_then(|s| s.binary_data.as_ref())
            {
                std::fs::write(out.join(format!("{stem}.bnsh")), b).expect("write bnsh");
            }
            if let Some(b) = ptcl
                .primitive_info
                .as_ref()
                .and_then(|p| p.binary_data.as_ref())
            {
                std::fs::write(out.join(format!("{stem}.bfres")), b).expect("write bfres");
            }
            eprintln!("dumped {stem}");
        }
    }

    /// Does the SHADER container survive the carrier pipeline byte-for-byte?
    ///
    /// Every carrier merges the donor's BNSH into the carrier's, then subsets the result down
    /// to the variations something addresses. Both steps re-encode. A mesh emitter needs a
    /// shader variation compiled for MESH vertex input, so an unfaithful re-encode there would
    /// draw the geometry wrong while ordinary billboard particles around it still looked fine —
    /// the exact shape of the "big ball of flame instead of the cone" report. Measure it.
    #[test]
    fn report_bnsh_rewrite_fidelity() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            return;
        };
        for rel in [
            "effect/assist/bomberman/ef_bomberman.eff",
            "effect/fighter/kirby/ef_kirby.eff",
        ] {
            let bytes = std::fs::read(root.join(rel)).expect("eff");
            let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
            let Some(shader) = file.ptcl_file.as_ref().and_then(|p| p.shader_info.as_ref()) else {
                continue;
            };
            let Some(binary) = shader.binary_data.as_ref() else {
                continue;
            };
            let parsed = effect_library::bnsh::BnshFile::read(binary).expect("bnsh read");
            // `canonicalize` is what the PTCL writer ACTUALLY runs on every shader container —
            // unconditionally, with no preserve-bytes escape hatch (unlike the primitive pool).
            let canonical = effect_library::bnsh::canonicalize(binary).expect("canonicalize");
            eprintln!(
                "  canonicalize: {} B -> {} B, identical={}",
                binary.len(),
                canonical.len(),
                canonical == *binary
            );
            let rewritten = parsed.write();
            eprintln!(
                "{rel}: {} variations, original {} B, rewritten {} B, identical={}",
                parsed.variations.len(),
                binary.len(),
                rewritten.len(),
                rewritten == *binary
            );
            // Where does the difference land? Re-read the rewritten container and compare
            // variation counts — a LOST variation is a shader an emitter can no longer address.
            let reread = effect_library::bnsh::BnshFile::read(&rewritten).expect("re-read");
            eprintln!(
                "  after rewrite: {} variations (was {})",
                reread.variations.len(),
                parsed.variations.len()
            );
            // A full-keep subset must be a no-op by construction (the early-out in
            // `subset_container`); confirm that path really does leave the bytes alone.
            let all: std::collections::BTreeSet<i32> =
                (0..parsed.variations.len() as i32).collect();
            let mut probe = binary.clone();
            let map =
                super::subset_container(Some(&mut probe), &all, "shader").expect("subset all");
            eprintln!(
                "  full-keep subset: bytes_unchanged={} remap_empty={}",
                probe == *binary,
                map.is_empty()
            );
        }
    }

    /// Does the BFRES round-trip (extract every model, merge them back) reproduce the game's
    /// own container? If it does, merging two donors' models is viable and the "one raw pool
    /// per carrier" limit can be lifted. If it does not, the merge path is exactly why a
    /// carrier holding two mesh-backed sources renders both models wrong.
    #[test]
    fn report_bfres_merge_fidelity() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            return;
        };
        for rel in [
            "effect/assist/bomberman/ef_bomberman.eff",
            "effect/fighter/kirby/ef_kirby.eff",
        ] {
            let bytes = std::fs::read(root.join(rel)).expect("eff");
            let file = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
            let Some(prim) = file
                .ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.as_ref())
            else {
                continue;
            };
            let Some(binary) = prim.binary_data.as_ref() else {
                continue;
            };
            let blobs: Vec<Vec<u8>> = (0..prim.descriptors.len())
                .map(|i| {
                    effect_library::bfres::export_single_model(binary, i)
                        .unwrap_or_else(|e| panic!("{rel}: extract model {i}: {e:#}"))
                })
                .collect();
            let merged = effect_library::bfres::ResFile::merge_model_files(&blobs)
                .unwrap_or_else(|e| panic!("{rel}: merge: {e:#}"));
            eprintln!(
                "{rel}: {} models, original {} B, round-tripped {} B, identical={}",
                prim.descriptors.len(),
                binary.len(),
                merged.len(),
                merged == *binary
            );
        }
    }

    /// Two mesh donors in one carrier both keep their geometry.
    ///
    /// Reported: kirby_dash rendered its cone correctly alone, then transplanting pickel_tnt
    /// (also mesh-backed) left the edit applied but the cone gone. The workaround shipped one
    /// donor's pool verbatim and dropped the other's models. The real fault was in the BFRES
    /// writer's relocation table, and with that fixed both donors merge intact — so this asserts
    /// the geometry itself, per primitive GUID, rather than which donor won.
    #[test]
    fn every_mesh_donor_keeps_its_geometry_in_one_carrier() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        const PICKEL: &str = "effect/fighter/pickel/ef_pickel.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Kirby parse");
        let set_of = |f: &effect_library::NamcoEffectFile, name: &str| -> usize {
            let i = f
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
                .expect("entry");
            f.entries[i].emitter_set_id as usize - 1
        };
        let kirby_set = set_of(&donor, "kirby_dash");
        let pickel_bytes = std::fs::read(root.join(PICKEL)).expect("Pickel eff");
        let pickel = effect_library::NamcoEffectFile::load(&pickel_bytes).expect("Pickel parse");
        let pickel_set = set_of(&pickel, "pickel_tnt");

        let clone_name = format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX);
        let ops = vec![
            crate::mod_project::TransplantOp {
                new_entry_name: clone_name.clone(),
                src_file_rel: KIRBY.into(),
                src_set_name: "kirby_dash".into(),
                src_set_idx: kirby_set,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            },
            crate::mod_project::TransplantOp {
                new_entry_name: "pickel_tnt".into(),
                src_file_rel: PICKEL.into(),
                src_set_name: "pickel_tnt".into(),
                src_set_idx: pickel_set,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            },
        ];
        let authored = super::CarrierAuthored {
            set_name: clone_name.clone(),
            edits: vec![crate::mod_project::AuthoredEdit {
                set_name: clone_name,
                entry_name: "kirby_dash".into(),
                set_idx: 0,
                emitter_name: String::new(),
                emitter_idx: 0,
                fields: crate::mod_project::EmitterFieldEdits {
                    scale: Some(1.5),
                    ..Default::default()
                },
            }],
        };
        let mut warnings = Vec::new();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &ops,
            &root,
            std::slice::from_ref(&authored),
            &[],
            &mut warnings,
        )
        .expect("mixed carrier build");
        let out = effect_library::NamcoEffectFile::load(&built).expect("carrier re-read");
        let prim_of = |f: &effect_library::NamcoEffectFile| {
            f.ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.clone())
                .expect("primitive pool")
        };
        let out_prim = prim_of(&out);
        let out_pool = out_prim.binary_data.as_ref().expect("carrier pool");

        // Both donors' geometry must survive, byte for byte, still addressed by its own GUID.
        for (label, donor_file) in [("kirby", &donor), ("pickel", &pickel)] {
            let donor_prim = prim_of(donor_file);
            let donor_pool = donor_prim.binary_data.as_ref().expect("donor pool");
            let mut checked = 0usize;
            for descriptor in &donor_prim.descriptors {
                let Some(out_index) = effect_library::bfres::descriptor_index_for_id(
                    &out_prim.descriptors,
                    descriptor.id,
                ) else {
                    continue;
                };
                let donor_index = effect_library::bfres::descriptor_index_for_id(
                    &donor_prim.descriptors,
                    descriptor.id,
                )
                .expect("donor owns this id");
                // Compare the GEOMETRY, not the container encoding: a single-model export
                // inherits its source container's string-pool order, so the carrier's and the
                // donor's blobs differ in layout while describing identical meshes.
                let geometry_of = |pool: &[u8], index: usize| {
                    let blob = effect_library::bfres::export_single_model(pool, index)
                        .expect("extract model");
                    let (_, model) = effect_library::bfres::ResFile::parse_model_export(blob)
                        .expect("parse model export");
                    let vertices: Vec<Vec<Vec<u8>>> = model
                        .vertex_buffers
                        .iter()
                        .map(|vb| vb.buffers.clone())
                        .collect();
                    let indices: Vec<Vec<u8>> = model
                        .shapes
                        .values()
                        .flat_map(|shape| shape.meshes.iter().map(|m| m.index_data.clone()))
                        .collect();
                    (vertices, indices)
                };
                assert_eq!(
                    geometry_of(out_pool, out_index),
                    geometry_of(donor_pool, donor_index),
                    "{label}: primitive {:#x} must keep its geometry in the merged carrier",
                    descriptor.id
                );
                checked += 1;
            }
            assert!(
                checked > 0,
                "{label}: the merged carrier kept none of this donor's primitives"
            );
        }
        assert!(
            !warnings.iter().any(|w| w.starts_with("mesh-dropped:")),
            "no donor may lose geometry now that every model merges: {warnings:?}"
        );
    }

    /// With two donors the rebuilt pool keeps every model by id, by content, correctly aligned
    /// with its descriptor, and with the right shader variation.
    ///
    /// This passed even while such a carrier drew nothing in game, because the fault was in the
    /// container's relocation table rather than in which model sat where — nothing observable
    /// from the parsed structure. It is kept as the structural half of the guarantee;
    /// EffectLibraryRust's corpus tests cover the byte layout the GPU depends on.
    #[test]
    fn a_two_donor_rebuild_keeps_models_ids_and_shaders_consistent() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        const OTHER: &str = "effect/fighter/pickel/ef_pickel.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Kirby parse");
        let other_bytes = std::fs::read(root.join(OTHER)).expect("other eff");
        let other = effect_library::NamcoEffectFile::load(&other_bytes).expect("other parse");

        let set_of = |f: &effect_library::NamcoEffectFile, name: &str| -> Option<usize> {
            let i = f
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))?;
            (f.entries[i].emitter_set_id as usize).checked_sub(1)
        };
        let kirby_set = set_of(&donor, "kirby_dash").expect("kirby_dash entry");

        // A particle-only entry from the other fighter — the "no 3D elements" transplant.
        let particle_entry = "pickel_tnt".to_string();
        {
            let op = crate::mod_project::TransplantOp {
                new_entry_name: particle_entry.clone(),
                src_file_rel: OTHER.into(),
                src_set_name: particle_entry.clone(),
                src_set_idx: 0,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            };
            eprintln!(
                "second donor {particle_entry}: uses_primitives={}",
                super::donor_effect_uses_primitives(&other, &op)
            );
        }
        let particle_set = set_of(&other, &particle_entry).expect("particle entry set");

        let clone_name = format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX);
        let ops = vec![
            crate::mod_project::TransplantOp {
                new_entry_name: clone_name.clone(),
                src_file_rel: KIRBY.into(),
                src_set_name: "kirby_dash".into(),
                src_set_idx: kirby_set,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            },
            crate::mod_project::TransplantOp {
                new_entry_name: particle_entry.clone(),
                src_file_rel: OTHER.into(),
                src_set_name: particle_entry.clone(),
                src_set_idx: particle_set,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            },
        ];
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &ops,
            &root,
            &[],
            &[],
            &mut Vec::new(),
        )
        .expect("two-donor carrier build");
        let out = effect_library::NamcoEffectFile::load(&built).expect("carrier re-read");
        let out_ptcl = out.ptcl_file.as_ref().expect("carrier PTCL");
        let pool: Vec<u64> = out_ptcl
            .primitive_info
            .as_ref()
            .map(|p| p.descriptors.iter().map(|d| d.id).collect())
            .unwrap_or_default();

        // Every primitive kirby_dash's emitters reference, that Kirby actually owns, must have
        // survived into the shipped pool.
        let owns = |f: &effect_library::NamcoEffectFile, id: u64| {
            f.ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.as_ref())
                .map(|p| p.descriptors.iter().any(|d| d.id == id))
                .unwrap_or(false)
        };
        let src_set = &donor
            .ptcl_file
            .as_ref()
            .expect("Kirby PTCL")
            .emitter_list
            .emitter_sets[kirby_set];
        let mut needed: Vec<u64> = Vec::new();
        super::visit_emitters_ref(&src_set.emitters, &mut |em| {
            for id in [
                em.data.particle_data.primitive_id,
                em.data.particle_data.primitive_ex_id,
                em.data.shape_info.primitive_index,
            ] {
                if id != 0 && id != u64::MAX && owns(&donor, id) && !needed.contains(&id) {
                    needed.push(id);
                }
            }
        });
        assert!(
            !needed.is_empty(),
            "kirby_dash owns no primitives — fixture is not exercising the 3D case"
        );
        let missing: Vec<String> = needed
            .iter()
            .filter(|id| !pool.contains(id))
            .map(|id| format!("{id:#x}"))
            .collect();
        assert!(
            missing.is_empty(),
            "adding a second donor ({particle_entry}) stripped {} of kirby_dash's {} model(s): \
             {} (pool holds {})",
            missing.len(),
            needed.len(),
            missing.join(", "),
            pool.len()
        );

        // The descriptor surviving is not enough: descriptor index i must still address MODEL i,
        // and that model must be the one the donor authored. A pool that keeps the right ids but
        // points them at the wrong (or re-encoded) geometry looks exactly like a missing cone.
        let donor_prim = donor
            .ptcl_file
            .as_ref()
            .and_then(|p| p.primitive_info.as_ref())
            .expect("Kirby primitive pool");
        let out_prim = out_ptcl
            .primitive_info
            .as_ref()
            .expect("carrier primitive pool");
        let donor_bin = donor_prim.binary_data.as_ref().expect("Kirby BFRES");
        let out_bin = out_prim.binary_data.as_ref().expect("carrier BFRES");
        let mut wrong: Vec<String> = Vec::new();
        for id in &needed {
            let src_idx =
                effect_library::bfres::descriptor_index_for_id(&donor_prim.descriptors, *id)
                    .expect("donor descriptor");
            let out_idx =
                effect_library::bfres::descriptor_index_for_id(&out_prim.descriptors, *id)
                    .expect("carrier descriptor");
            let want = effect_library::bfres::export_single_model(donor_bin, src_idx)
                .expect("donor model");
            let got = match effect_library::bfres::export_single_model(out_bin, out_idx) {
                Ok(m) => m,
                Err(e) => {
                    wrong.push(format!("{id:#x}: model {out_idx} not extractable ({e})"));
                    continue;
                }
            };
            if want == got {
                continue;
            }
            let (_, want_model) = effect_library::bfres::ResFile::parse_model_export(want)
                .expect("donor model parses");
            let (_, got_model) = effect_library::bfres::ResFile::parse_model_export(got)
                .expect("carrier model parses");
            if format!("{want_model:?}") != format!("{got_model:?}") {
                wrong.push(format!(
                    "{id:#x}: descriptor {out_idx} addresses different geometry than the donor's \
                     model {src_idx}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of kirby_dash's models survived by id but not by content:\n  {}",
            wrong.len(),
            wrong.join("\n  ")
        );

        // SHADERS. A mesh emitter needs the variation compiled for mesh vertex input; if the
        // relocation lands it on some other variation the geometry does not draw, while the
        // billboard particles around it look untouched. Compare the variation each cloned
        // emitter now addresses against the one the donor authored for it.
        let bnsh_of = |f: &effect_library::NamcoEffectFile| {
            f.ptcl_file
                .as_ref()
                .and_then(|p| p.shader_info.as_ref())
                .and_then(|s| s.binary_data.as_ref())
                .map(|b| effect_library::bnsh::BnshFile::read(b).expect("bnsh parses"))
        };
        let donor_bnsh = bnsh_of(&donor).expect("Kirby shader container");
        let out_bnsh = bnsh_of(&out).expect("carrier shader container");
        let out_index = out
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&clone_name))
            .expect("the clone is missing from the carrier");
        let out_set =
            &out_ptcl.emitter_list.emitter_sets[out.entries[out_index].emitter_set_id as usize - 1];
        let mut src_indices = Vec::new();
        super::visit_emitters_ref(&src_set.emitters, &mut |em| {
            src_indices.push(em.data.shader_references.shader_index)
        });
        let mut out_indices = Vec::new();
        super::visit_emitters_ref(&out_set.emitters, &mut |em| {
            out_indices.push(em.data.shader_references.shader_index)
        });
        assert_eq!(
            src_indices.len(),
            out_indices.len(),
            "the cloned set has a different emitter count"
        );
        let program = |file: &effect_library::bnsh::BnshFile, index: i32| {
            (index >= 0)
                .then(|| file.variations.get(index as usize))
                .flatten()
                .map(|v| format!("{:?}", v.binary_program))
        };
        let mut bad: Vec<String> = Vec::new();
        for (emitter, (src, got)) in src_indices.iter().zip(out_indices.iter()).enumerate() {
            let want = program(&donor_bnsh, *src);
            let have = program(&out_bnsh, *got);
            if want != have {
                bad.push(format!(
                    "emitter {emitter}: donor variation {src} != carrier variation {got}"
                ));
            }
        }
        assert!(
            bad.is_empty(),
            "{} emitter(s) address the wrong shader variation after relocation:\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
    }

    /// Two mesh-backed sources in ONE carrier must both keep their geometry.
    ///
    /// bomberman_bomb is native to the carrier and every Bomberman entry renders through its own
    /// models; kirby_dash is mesh-backed too. Both pools therefore have to survive into a single
    /// container. This was believed impossible, on the strength of an unmeasured comment, until
    /// the merge was actually tested: it was silently DROPPING models whose names collided
    /// across containers (39 + 6 came out as 43). With that fixed upstream, the carrier can hold
    /// both — so assert the models are there rather than warning the user off.
    #[test]
    fn two_mesh_sources_in_one_carrier_both_keep_their_models() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Kirby parse");
        let entry_index = donor
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("kirby_dash"))
            .expect("kirby_dash entry");
        let set_idx = donor.entries[entry_index].emitter_set_id as usize - 1;
        let clone_name = format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX);
        let ops = vec![
            crate::mod_project::TransplantOp {
                new_entry_name: "bomberman_bomb".into(),
                src_file_rel: CARRIER.into(),
                src_set_name: "bomberman_bomb".into(),
                src_set_idx: 0,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            },
            crate::mod_project::TransplantOp {
                new_entry_name: clone_name.clone(),
                src_file_rel: KIRBY.into(),
                src_set_name: "kirby_dash".into(),
                src_set_idx: set_idx,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            },
        ];
        let mut warnings = Vec::new();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &ops,
            &root,
            &[],
            &[],
            &mut warnings,
        )
        .expect("mixed carrier build");
        assert!(
            warnings.is_empty(),
            "a two-mesh carrier is no longer a limitation: {warnings:?}"
        );
        let out = effect_library::NamcoEffectFile::load(&built).expect("carrier re-read");
        let out_ptcl = out.ptcl_file.as_ref().expect("carrier PTCL");
        let pool: Vec<u64> = out_ptcl
            .primitive_info
            .as_ref()
            .map(|p| p.descriptors.iter().map(|d| d.id).collect())
            .unwrap_or_default();
        // Every primitive BOTH effects reference must be present in the single shipped pool.
        let mut wanted: Vec<u64> = Vec::new();
        for set in &out_ptcl.emitter_list.emitter_sets {
            if set.emitters.is_empty() {
                continue;
            }
            super::visit_emitters_ref(&set.emitters, &mut |em| {
                for id in [
                    em.data.particle_data.primitive_id,
                    em.data.particle_data.primitive_ex_id,
                    em.data.shape_info.primitive_index,
                ] {
                    if id != 0 && id != u64::MAX && !wanted.contains(&id) {
                        wanted.push(id);
                    }
                }
            });
        }
        // Only IDs a SOURCE container actually owns are required. Vanilla emitters keep nonzero
        // values in inactive primitive fields and reference globally-owned models the file does
        // not carry — ef_bomberman itself references 0x35ca927c, which neither it nor Kirby
        // owns. Demanding those would fail against the game's own data.
        let owns = |file: &effect_library::NamcoEffectFile, id: u64| {
            file.ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.as_ref())
                .map(|p| p.descriptors.iter().any(|d| d.id == id))
                .unwrap_or(false)
        };
        let carrier_src =
            effect_library::NamcoEffectFile::load(&carrier_base).expect("carrier parse");
        let missing: Vec<String> = wanted
            .iter()
            .filter(|id| owns(&donor, **id) || owns(&carrier_src, **id))
            .filter(|id| !pool.contains(id))
            .map(|id| format!("{id:#x}"))
            .collect();
        assert!(
            missing.is_empty(),
            "{} primitive(s) referenced by a surviving emitter are absent from the shipped pool: \
             {} (pool holds {})",
            missing.len(),
            missing.join(", "),
            pool.len()
        );
        // Both sides' OWN models must be present — that is the property the merge fix restored.
        let required: Vec<u64> = wanted
            .iter()
            .copied()
            .filter(|id| owns(&donor, *id) || owns(&carrier_src, *id))
            .collect();
        assert!(
            required.iter().any(|id| owns(&donor, *id)),
            "the donor's mesh contributed no primitive — the fixture is not exercising meshes"
        );
        assert!(
            required.iter().any(|id| owns(&carrier_src, *id)),
            "the carrier-native mesh contributed no primitive — the mixed case is not covered"
        );
    }

    /// The single-mesh case strips too. It used to ship the donor's pool whole, which is how
    /// all 39 of Kirby's models reached a carrier that referenced two of them.
    #[test]
    fn a_single_mesh_source_keeps_only_the_models_it_uses() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");
        let donor_bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("Kirby parse");
        let entry_index = donor
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("kirby_dash"))
            .expect("kirby_dash entry");
        let set_idx = donor.entries[entry_index].emitter_set_id as usize - 1;
        let op = crate::mod_project::TransplantOp {
            new_entry_name: format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX),
            src_file_rel: KIRBY.into(),
            src_set_name: "kirby_dash".into(),
            src_set_idx: set_idx,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        let mut warnings = Vec::new();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            &[],
            &[],
            &mut warnings,
        )
        .expect("carrier build");
        assert!(
            warnings.is_empty(),
            "a single mesh source is not a conflict: {warnings:?}"
        );
        let out = effect_library::NamcoEffectFile::load(&built).expect("carrier re-read");
        let carrier_src =
            effect_library::NamcoEffectFile::load(&carrier_base).expect("carrier parse");
        assert_pool_is_exactly_what_is_used(&out, &[&donor, &carrier_src]);
        let count = |f: &effect_library::NamcoEffectFile| {
            f.ptcl_file
                .as_ref()
                .and_then(|p| p.primitive_info.as_ref())
                .map(|p| p.descriptors.len())
                .unwrap_or(0)
        };
        assert!(
            count(&out) < count(&donor),
            "kirby_dash uses a fraction of Kirby's {} models; the carrier shipped {}",
            count(&donor),
            count(&out)
        );
    }

    /// A stale authored edit must not take working transplants down with it.
    ///
    /// This is the regression that made a previously working bomberman_bomb transplant go
    /// invisible: an unrelated kirby_dash edit named an entry no transplant had created, the
    /// build failed outright, and the whole carrier — transplants included — was never sent.
    /// Transplants and edits are independent features and must fail independently.
    #[test]
    fn an_unplaceable_authored_edit_does_not_kill_the_transplants() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");

        // A real transplant, plus an authored edit pointing at a name nothing creates.
        let op = crate::mod_project::TransplantOp {
            new_entry_name: "kirby_dash_tp".into(),
            src_file_rel: KIRBY.into(),
            src_set_name: "kirby_dash".into(),
            src_set_idx: 0,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        let orphan = super::CarrierAuthored {
            set_name: "a_set_that_does_not_exist".into(),
            edits: vec![crate::mod_project::AuthoredEdit {
                set_name: "whatever".into(),
                entry_name: "whatever".into(),
                set_idx: 0,
                emitter_name: String::new(),
                emitter_idx: 0,
                fields: crate::mod_project::EmitterFieldEdits {
                    scale: Some(2.0),
                    ..Default::default()
                },
            }],
        };
        let mut skipped = Vec::new();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            std::slice::from_ref(&orphan),
            &[],
            &mut skipped,
        )
        .expect("an unplaceable edit must not fail the build");
        assert_eq!(
            skipped,
            vec!["edit:a_set_that_does_not_exist".to_string()],
            "the dropped edit must be reported so the user can be told"
        );
        // ...and the transplant is still there.
        let output = effect_library::NamcoEffectFile::load(&built).expect("carrier re-read");
        assert!(
            output
                .entry_names
                .iter()
                .any(|n| n.eq_ignore_ascii_case("kirby_dash_tp")),
            "the transplant was dropped along with the bad edit"
        );
    }

    /// A plain load → save of a game .eff must reproduce every SECTION the game reads.
    ///
    /// The carrier is written by this same serializer, so any section it cannot reproduce is a
    /// section the game receives subtly wrong. Reporting per-section rather than "the file
    /// differs" is the point: it localizes a rendering fault to the structure responsible for
    /// it instead of leaving it to be guessed at from screenshots.
    #[test]
    fn round_tripping_a_game_eff_reproduces_each_section() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        for rel in [
            "effect/fighter/kirby/ef_kirby.eff",
            "effect/assist/bomberman/ef_bomberman.eff",
        ] {
            let original = std::fs::read(root.join(rel)).expect("game eff");
            let file = effect_library::NamcoEffectFile::load(&original).expect("parse");
            let rebuilt = file.save().expect("save");
            // Locate each four-character section tag in both buffers and compare the payloads.
            let sections = |buf: &[u8]| -> Vec<(String, usize)> {
                let mut out = Vec::new();
                for tag in [
                    b"PTCL", b"EMTR", b"ESTA", b"PRMA", b"TEXR", b"SHDR", b"GRSC", b"EMTS",
                ] {
                    if let Some(pos) = buf.windows(4).position(|w| w == tag) {
                        out.push((String::from_utf8_lossy(tag).to_string(), pos));
                    }
                }
                out
            };
            let a = sections(&original);
            let b = sections(&rebuilt);
            eprintln!(
                "{rel}: {} B -> {} B; sections original={:?} rebuilt={:?}",
                original.len(),
                rebuilt.len(),
                a,
                b
            );
            assert_eq!(
                a.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
                b.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
                "{rel}: the rebuilt file is missing a section the game ships"
            );
        }
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
    // A texture swap names the pool texture; the emitter addresses textures by GUID. Resolve
    // it here, while the descriptor table is still reachable — `sets` borrows `ptcl` mutably
    // below.
    let swap_to = match edit.fields.texture_name.as_deref() {
        Some(wanted) => {
            let descriptors = ptcl
                .texture_info
                .as_ref()
                .map(|info| info.descriptors.as_slice())
                .unwrap_or_default();
            let id = descriptors
                .iter()
                .find(|d| d.name == wanted)
                .map(|d| d.id);
            if id.is_none() {
                // Dropping the swap leaves the original texture — wrong, but it still renders.
                // Writing a GUID no descriptor holds would make the emitter sample nothing.
                eprintln!(
                    "[EFF-EXPORT] warning: texture swap on '{}' wants '{wanted}', which this \
                     pool does not hold — swap skipped",
                    edit.emitter_name
                );
            }
            id
        }
        None => None,
    };

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
            if let Some(id) = swap_to {
                if let Some(sampler) = em.data.sampler0.as_mut() {
                    sampler.texture_id = id;
                }
            }
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

#[cfg(test)]
mod bench {
    //! Attribute the cost of the transplant path the UI runs synchronously.
    //!
    //! Recording a transplant blocks the UI thread on a full merge of the target's eff plus
    //! every recorded op, a preview write, and a reparse of the result. Run with:
    //!   VISIONARY_EFF_ROOT=<export root> cargo test --release bench_transplant -- --nocapture

    use crate::mod_project::{EffMod, TransplantOp};
    use std::time::Instant;

    fn ms(t: Instant) -> f64 {
        t.elapsed().as_secs_f64() * 1000.0
    }

    #[test]
    fn bench_transplant_stages() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT");
            return;
        };
        const FIGHTER: &str = "kirby";
        const DONOR_REL: &str = "effect/boss/marx/ef_marx.eff";
        const DONOR_SET: &str = "marx_icebomb";

        let src_rel = format!("effect/fighter/{FIGHTER}/ef_{FIGHTER}.eff");
        let t = Instant::now();
        let src_bytes = std::fs::read(root.join(&src_rel)).expect("target eff");
        eprintln!(
            "read target    {:>8.1} ms ({} MB)",
            ms(t),
            src_bytes.len() / 1_000_000
        );

        let t = Instant::now();
        let donor_bytes = std::fs::read(root.join(DONOR_REL)).expect("donor eff");
        eprintln!(
            "read donor     {:>8.1} ms ({} MB)",
            ms(t),
            donor_bytes.len() / 1_000_000
        );

        let t = Instant::now();
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("donor parse");
        eprintln!("parse donor    {:>8.1} ms", ms(t));
        let set_idx = donor
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(DONOR_SET))
            .map(|i| donor.entries[i].emitter_set_id as usize - 1)
            .expect("donor set present");
        drop(donor);

        let eff = EffMod {
            source_rel: src_rel,
            authored: Vec::new(),
            textures: Vec::new(),
            transplants: vec![TransplantOp {
                new_entry_name: format!("{DONOR_SET}_tp"),
                src_file_rel: DONOR_REL.into(),
                src_set_name: DONOR_SET.into(),
                src_set_idx: set_idx,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            }],
        };

        let t = Instant::now();
        let merged = super::rebuild_eff_bytes(&src_bytes, &eff, Some(&root)).expect("merge");
        eprintln!(
            "MERGE preview  {:>8.1} ms -> {} MB",
            ms(t),
            merged.len() / 1_000_000
        );

        let tmp = std::env::temp_dir().join("_bench_transplant_preview.eff");
        let t = Instant::now();
        std::fs::write(&tmp, &merged).expect("write preview");
        eprintln!("write preview  {:>8.1} ms", ms(t));

        let t = Instant::now();
        let loaded = crate::effects::load_effect(&tmp).expect("reload preview");
        eprintln!(
            "RELOAD preview {:>8.1} ms ({} sets)",
            ms(t),
            loaded.ptcl.emitter_sets.len()
        );

        const CARRIER_REL: &str = "effect/assist/bomberman/ef_bomberman.eff";
        if let Ok(carrier_bytes) = std::fs::read(root.join(CARRIER_REL)) {
            let mut warnings = Vec::new();
            let t = Instant::now();
            let carrier = super::rebuild_runtime_carrier_eff_bytes_with_edits(
                &carrier_bytes,
                CARRIER_REL,
                &eff.transplants,
                &root,
                &[],
                &[],
                &mut warnings,
            )
            .expect("carrier build");
            eprintln!(
                "CARRIER build  {:>8.1} ms -> {} MB",
                ms(t),
                carrier.len() / 1_000_000
            );
        }
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;
    use std::time::Instant;

    /// Time and size the work one transplant triggers, on the reported case: a 20 MB donor
    /// (`marx_icebomb`) plus an edited `kirby_dash`, into the Bomberman carrier.
    ///
    /// A measurement, printed with `--nocapture`, plus the one assertion worth keeping: the
    /// carrier must not ship models nothing references. It exists so cost is attributed to a
    /// phase rather than guessed at — the app-side build turned out to be ~90 ms, which ruled
    /// out the rebuild as the cause of a 30 s send timeout.
    #[test]
    fn measure_transplant_build_cost() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        const MARX: &str = "effect/boss/marx/ef_marx.eff";

        let kirby_bytes = std::fs::read(root.join(KIRBY)).expect("kirby");
        let marx_bytes = std::fs::read(root.join(MARX)).expect("marx");
        let carrier_bytes = std::fs::read(root.join(CARRIER)).expect("carrier");
        let kirby = effect_library::NamcoEffectFile::load(&kirby_bytes).expect("kirby parse");
        let marx = effect_library::NamcoEffectFile::load(&marx_bytes).expect("marx parse");

        let set_of = |f: &effect_library::NamcoEffectFile, name: &str| -> usize {
            let i = f
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
                .unwrap_or_else(|| panic!("entry {name} not found"));
            f.entries[i].emitter_set_id as usize - 1
        };
        let kirby_set = set_of(&kirby, "kirby_dash");
        let marx_set = set_of(&marx, "marx_icebomb");
        let clone_name = format!("{}kirby_dash", crate::mod_project::EDIT_CLONE_PREFIX);
        let marx_op = TransplantOp {
            new_entry_name: "marx_icebomb".into(),
            src_file_rel: MARX.into(),
            src_set_name: "marx_icebomb".into(),
            src_set_idx: marx_set,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        let ops = vec![
            TransplantOp {
                new_entry_name: clone_name.clone(),
                src_file_rel: KIRBY.into(),
                src_set_name: "kirby_dash".into(),
                src_set_idx: kirby_set,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            },
            marx_op.clone(),
        ];
        let authored = super::CarrierAuthored {
            set_name: clone_name.clone(),
            edits: vec![crate::mod_project::AuthoredEdit {
                set_name: clone_name,
                entry_name: "kirby_dash".into(),
                set_idx: 0,
                emitter_name: String::new(),
                emitter_idx: 0,
                fields: crate::mod_project::EmitterFieldEdits {
                    scale: Some(1.5),
                    ..Default::default()
                },
            }],
        };

        let t = Instant::now();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_bytes,
            CARRIER,
            &ops,
            &root,
            std::slice::from_ref(&authored),
            &[],
            &mut Vec::new(),
        )
        .expect("carrier build");
        eprintln!("CARRIER BUILD : {:?} -> {} B", t.elapsed(), built.len());

        let eff = crate::mod_project::EffMod {
            source_rel: KIRBY.into(),
            transplants: vec![marx_op],
            ..Default::default()
        };
        let t = Instant::now();
        let merged = super::rebuild_eff_bytes(&kirby_bytes, &eff, Some(&root)).expect("preview");
        eprintln!("MERGED PREVIEW: {:?} -> {} B", t.elapsed(), merged.len());

        // The preview is written to disk and re-parsed by the editor on every transplant.
        let scratch = crate::scratch_dirs::app_scratch_dir("perf").expect("scratch");
        let path = scratch.path().join("_perf_preview.eff");
        let t = Instant::now();
        std::fs::write(&path, &merged).expect("write preview");
        eprintln!("PREVIEW WRITE : {:?}", t.elapsed());
        let t = Instant::now();
        let _ = effect_library::NamcoEffectFile::load(&merged).expect("reparse");
        eprintln!("PREVIEW PARSE : {:?}", t.elapsed());

        let out = effect_library::NamcoEffectFile::load(&built).expect("built parse");
        let report = |label: &str, f: &effect_library::NamcoEffectFile| {
            let Some(p) = f.ptcl_file.as_ref() else {
                return;
            };
            let tex = p.texture_info.as_ref();
            let prim = p.primitive_info.as_ref();
            let sh = p.shader_info.as_ref();
            eprintln!(
                "  {label}: BNTX {} tex / {} B | BFRES {} models / {} B | SHDR {} B",
                tex.map(|t| t.descriptors.len()).unwrap_or(0),
                tex.and_then(|t| t.binary_data.as_ref())
                    .map(|b| b.len())
                    .unwrap_or(0),
                prim.map(|t| t.descriptors.len()).unwrap_or(0),
                prim.and_then(|t| t.binary_data.as_ref())
                    .map(|b| b.len())
                    .unwrap_or(0),
                sh.and_then(|t| t.binary_data.as_ref())
                    .map(|b| b.len())
                    .unwrap_or(0),
            );
        };
        let carrier_src =
            effect_library::NamcoEffectFile::load(&carrier_bytes).expect("carrier parse");
        report("bomberman", &carrier_src);
        report("BUILT    ", &out);
        report("kirby    ", &kirby);
        report("marx     ", &marx);

        super::tests::assert_pool_is_exactly_what_is_used(&out, &[&kirby, &marx, &carrier_src]);

        // The donor eff the plugin co-loads. This is the dominant payload: it was sent WHOLE,
        // and the transport base64s it into one JSON frame.
        for (label, bytes, keep) in [
            ("marx ", &marx_bytes, "marx_icebomb"),
            ("kirby", &kirby_bytes, "kirby_dash"),
        ] {
            let t = Instant::now();
            let stripped = super::strip_donor_eff_bytes(bytes, &[keep]).expect("strip donor");
            let b64 = |n: usize| n.div_ceil(3) * 4;
            eprintln!(
                "DONOR {label}: {} B -> {} B ({:.1}% ) in {:?} | on the wire {} B -> {} B",
                bytes.len(),
                stripped.len(),
                100.0 * stripped.len() as f64 / bytes.len() as f64,
                t.elapsed(),
                b64(bytes.len()),
                b64(stripped.len()),
            );
            let back = effect_library::NamcoEffectFile::load(&stripped).expect("reparse stripped");
            let kept = back
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(keep))
                .expect("kept entry survives");
            let set_idx = back.entries[kept].emitter_set_id as usize - 1;
            assert!(
                !back
                    .ptcl_file
                    .as_ref()
                    .expect("ptcl")
                    .emitter_list
                    .emitter_sets[set_idx]
                    .emitters
                    .is_empty(),
                "{label}: the kept effect must still have its emitters"
            );
        }
    }
}
