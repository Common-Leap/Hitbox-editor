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
///
/// Cross-file transplants are stripped on the way out, as the carrier's are: one Pickel effect
/// exported into `ef_mario.eff` measures 5.67 MB rather than 7.35 MB, because the merged shader
/// library is cut from 492 variations to the 134 the file addresses.
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
    /// The set's edited emitter list, when the user changed which emitters play. Applied before
    /// `edits`, for the same reason the export path applies it first: it decides what there is
    /// to edit. Retargeted onto the carrier's copy of the set, exactly as the edits are.
    pub roster: Option<crate::mod_project::EmitterRoster>,
    /// Spawn header edits are applied to the donor entry before it is cloned. This is what lets
    /// primary-set changes, part lists, bones and external models determine which sets/resources
    /// the carrier copies, instead of trying to repair an already-pruned clone afterward.
    pub entry_edit: Option<crate::mod_project::EntryEdit>,
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
#[allow(clippy::too_many_arguments)] // The public pipeline boundary keeps each input explicit.
pub fn rebuild_runtime_carrier_eff_bytes_with_edits(
    carrier_bytes: &[u8],
    carrier_rel: &str,
    ops: &[TransplantOp],
    donor_root: &std::path::Path,
    authored: &[CarrierAuthored],
    textures: &[TextureImport],
    added_textures: &[crate::mod_project::TextureAddition],
    removed_textures: &[String],
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    // A carrier-native selection needs no transplant. The runtime remap already turns an `_os`
    // request into the carrier's existing real entry, so parsing, pruning, and repacking these
    // resource pools only introduces risk. Preserve the game's known-good payload byte-for-byte.
    // ...but only when there is nothing to bake in. With authored edits or an imported
    // texture the bytes MUST be rebuilt, otherwise those silently do not ship.
    if authored.is_empty()
        && textures.is_empty()
        && added_textures.is_empty()
        && removed_textures.is_empty()
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
        let mut edited_donor = None;
        if let Some(header) = authored
            .iter()
            .find(|entry| entry.set_name.eq_ignore_ascii_case(&op.new_entry_name))
            .and_then(|entry| entry.entry_edit.as_ref())
        {
            let donor_bytes = donors[*donor_index]
                .1
                .save()
                .context("serializing a donor before applying its live spawn structure")?;
            let mut donor = effect_library::NamcoEffectFile::load(&donor_bytes)
                .context("reloading a donor before applying its live spawn structure")?;
            let mut header = header.clone();
            // The editor names the target kind. Inside the source donor it is still the
            // original kind; `new_entry_name` only comes into existence after cloning.
            header.entry_name = op.src_set_name.clone();
            apply_entry_edit(&mut donor, &header).with_context(|| {
                format!("applying live spawn structure for '{}'", op.src_set_name)
            })?;
            edited_donor = Some(donor);
        }
        let donor = edited_donor.as_ref().unwrap_or(&donors[*donor_index].1);
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
    // The game does not treat an ESET with zero emitters as a valid no-op.  If an entry still
    // points at one, its loader walks into the following ESET (and can crash).  Keep the entry
    // name/header so requests still resolve, but make the spawn handle explicitly `none`.
    clear_empty_emitter_set_references(&mut carrier);
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

    verify_shader_indices_resolve(&carrier, "live carrier")?;

    // Textures the user ADDED go in before the authored edits, so a swap can resolve one by
    // name, and after the prune, so the prune cannot drop one nothing samples yet.
    apply_texture_additions(&mut carrier, added_textures, warnings)?;

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
            if let Some(roster) = &entry.roster {
                let mut roster = roster.clone();
                roster.set_name = entry.set_name.clone();
                roster.set_idx = idx;
                if let Err(err) = apply_roster(ptcl, &roster) {
                    // Same rule as a missing edit target: one bad roster must not take down
                    // every other transplant in the same snapshot.
                    warnings.push(format!("emitters:{}: {err}", entry.set_name));
                }
            }
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
    // Removals last: the swaps that moved emitters off these textures have now been applied.
    apply_texture_removals(&mut carrier, removed_textures, warnings)?;

    carrier
        .save()
        .context("effect_library failed to encode the runtime carrier .eff")
}

/// Every texture GUID some surviving emitter samples.
fn sampled_texture_ids(ptcl: &effect_library::PtclFile) -> std::collections::HashSet<u64> {
    let mut used = std::collections::HashSet::new();
    for set in &ptcl.emitter_list.emitter_sets {
        visit_emitters_ref(&set.emitters, &mut |em| {
            let d = &em.data;
            for id in [
                d.sampler0.as_ref().map(|s| s.texture_id),
                d.sampler1.as_ref().map(|s| s.texture_id),
                d.sampler2.as_ref().map(|s| s.texture_id),
                d.sampler3.as_ref().map(|s| s.texture_id),
                d.sampler4.as_ref().map(|s| s.texture_id),
                d.sampler5.as_ref().map(|s| s.texture_id),
            ]
            .into_iter()
            .flatten()
            {
                used.insert(id);
            }
        });
    }
    used
}

/// Grow the pool with the textures the user added.
///
/// Runs BEFORE authored edits, because a texture swap resolves its target by NAME against the
/// descriptor table (`apply_authored`) — an addition applied afterwards would not exist yet and
/// the swap would be skipped with a warning. And AFTER pruning, so the pruner cannot drop a
/// texture nothing samples yet: the emitter that will use it is repointed by an edit that has not
/// run.
///
/// A missing template is a WARNING, not a failure — same rule as a missing authored edit. A
/// stale addition must not take a working carrier down with it.
fn apply_texture_additions(
    file: &mut effect_library::NamcoEffectFile,
    additions: &[crate::mod_project::TextureAddition],
    warnings: &mut Vec<String>,
) -> Result<()> {
    if additions.is_empty() {
        return Ok(());
    }
    let Some(textures) = file
        .ptcl_file
        .as_mut()
        .and_then(|ptcl| ptcl.texture_info.as_mut())
    else {
        for add in additions {
            warnings.push(format!("texture-add:{}", add.texture_name));
        }
        return Ok(());
    };

    for add in additions {
        let names: Vec<String> = textures
            .descriptors
            .iter()
            .map(|d| d.name.clone())
            .collect();
        if names.contains(&add.texture_name) {
            // Already present: a rebuild of a project that has been sent before.
            continue;
        }
        let Some(template) = names.iter().position(|n| *n == add.template_name) else {
            warnings.push(format!("texture-add:{}", add.texture_name));
            continue;
        };
        let Some(pool) = textures.binary_data.as_ref() else {
            warnings.push(format!("texture-add:{}", add.texture_name));
            continue;
        };
        let rebuilt = if add.png_path.is_empty() {
            crate::texture_import::duplicate_texture(pool, &names, template, &add.texture_name)
                .with_context(|| {
                    format!(
                        "duplicating '{}' as '{}'",
                        add.template_name, add.texture_name
                    )
                })?
        } else {
            let png = std::fs::read(&add.png_path).with_context(|| {
                format!(
                    "new texture '{}': cannot read {}",
                    add.texture_name, add.png_path
                )
            })?;
            let form = if add.raw {
                crate::texture_import::Form::Raw
            } else {
                crate::texture_import::Form::Editable
            };
            let (rebuilt, _) = crate::texture_import::add_texture_from_png(
                pool,
                &names,
                template,
                &add.texture_name,
                &png,
                form,
            )
            .with_context(|| format!("adding {} as '{}'", add.png_path, add.texture_name))?;
            rebuilt
        };
        let taken: Vec<u64> = textures.descriptors.iter().map(|d| d.id).collect();
        let id = crate::texture_import::unused_descriptor_id(&taken, &add.texture_name);
        textures
            .descriptors
            .push(effect_library::ptcl_file::TextureDescriptor {
                id,
                name: add.texture_name.clone(),
            });
        textures.binary_data = Some(rebuilt);
    }
    Ok(())
}

/// Drop the textures the user removed.
///
/// Runs LAST, so the swaps that moved emitters off the doomed texture have already been applied.
/// A texture something still samples is KEPT, with a warning: writing a pool without it would
/// leave that emitter addressing a GUID no descriptor holds, which renders as nothing at all.
/// The editor blocks the delete for the same reason, so reaching this is either a stale project
/// or a swap that could not be placed.
fn apply_texture_removals(
    file: &mut effect_library::NamcoEffectFile,
    removals: &[String],
    warnings: &mut Vec<String>,
) -> Result<()> {
    if removals.is_empty() {
        return Ok(());
    }
    let Some(ptcl) = file.ptcl_file.as_mut() else {
        return Ok(());
    };
    let still_used = sampled_texture_ids(ptcl);
    let Some(textures) = ptcl.texture_info.as_mut() else {
        return Ok(());
    };
    for name in removals {
        let names: Vec<String> = textures
            .descriptors
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let Some(index) = names.iter().position(|n| n == name) else {
            continue; // already gone, or pruned — nothing to do
        };
        if still_used.contains(&textures.descriptors[index].id) {
            warnings.push(format!("texture-in-use:{name}"));
            continue;
        }
        let Some(pool) = textures.binary_data.as_ref() else {
            continue;
        };
        let rebuilt = crate::texture_import::remove_texture(pool, &names, index)
            .with_context(|| format!("removing texture '{name}'"))?;
        textures.descriptors.remove(index);
        textures.binary_data = Some(rebuilt);
    }
    Ok(())
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
        let names: Vec<String> = textures
            .descriptors
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let form = if import.raw {
            crate::texture_import::Form::Raw
        } else {
            crate::texture_import::Form::Editable
        };
        let (rebuilt, report) = crate::texture_import::replace_with_png(
            pool, &names, index, &png, form,
        )
        .with_context(|| {
            format!(
                "importing {} over '{}'",
                import.png_path, import.texture_name
            )
        })?;
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
    clear_empty_emitter_set_references(&mut file);
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
    let mut keep = std::collections::HashSet::new();
    for (index, _) in carrier.entry_names.iter().enumerate().filter(|(_, name)| {
        transplanted
            .iter()
            .any(|new| new.eq_ignore_ascii_case(name))
    }) {
        // Every set the entry can spawn, not just `emitter_set_id` — a multi-part transplant
        // reaches its content only through its variants, and `apply_transplant_cross_file` leaves
        // `emitter_set_id` at the DONOR's value for those. Keying on that field alone kept a set
        // belonging to some unrelated carrier effect and cleared the transplant's real ones.
        entry_emitter_sets(carrier, index, &mut keep);
    }
    let Some(ptcl) = carrier.ptcl_file.as_mut() else {
        return;
    };
    for (index, set) in ptcl.emitter_list.emitter_sets.iter_mut().enumerate() {
        if !keep.contains(&index) {
            set.emitters.clear();
        }
    }
}

/// Collect the 0-based emitter-set indices entry `index` can spawn into `out`.
///
/// A single-part entry names one set through `emitter_set_id`. A multi-part entry names one per
/// variant AND keeps its own `emitter_set_id` live — 13 game entries point it at a populated set
/// that appears in no variant, so both are collected (see
/// `every_game_emitter_set_is_reachable_from_the_entry_table`). Ids are 1-based handles; 0 means
/// none.
fn entry_emitter_sets(
    file: &effect_library::NamcoEffectFile,
    index: usize,
    out: &mut std::collections::HashSet<usize>,
) {
    let Some(entry) = file.entries.get(index) else {
        return;
    };
    out.extend((entry.emitter_set_id as usize).checked_sub(1));
    if entry.variant_count == 0 {
        return;
    }
    let start = (entry.variant_start_idx as usize).saturating_sub(1);
    out.extend(
        file.effect_variants
            .iter()
            .skip(start)
            .take(entry.variant_count as usize)
            .filter_map(|variant| (variant.emitter_set_id as usize).checked_sub(1)),
    );
}

/// The 0-based indices of every emitter set some entry can spawn.
///
/// Emitter sets are addressable ONLY through the entry table, so a set outside this index can
/// never play. Every set in every one of the 328 game files is reachable, which is what makes
/// clearing the rest safe: the only way to produce an unreachable set is for Visionary to orphan
/// one, and replace-mode transplants do exactly that.
fn live_emitter_sets(file: &effect_library::NamcoEffectFile) -> std::collections::HashSet<usize> {
    let mut live = std::collections::HashSet::new();
    for index in 0..file.entries.len() {
        entry_emitter_sets(file, index, &mut live);
    }
    live
}

/// Empty every emitter set no entry can reach.
///
/// Replace mode repoints an entry at freshly cloned sets and leaves the set it used to name
/// behind, emitters intact (`finish_transplant_entry`). Nothing can spawn that set again, but the
/// passes that collect resources scan sets rather than entries, so an orphan goes on pinning its
/// textures and its share of the donor's shader library. Clearing orphans first is what lets
/// those passes stay simple and still see the truth.
///
/// The sets themselves are kept and merely emptied, exactly as
/// [`silence_carrier_native_effects`] keeps the carrier's own: every id in the file still
/// resolves, and an empty set is a valid handle that emits nothing.
fn clear_unreachable_emitter_sets(file: &mut effect_library::NamcoEffectFile) {
    let live = live_emitter_sets(file);
    let Some(ptcl) = file.ptcl_file.as_mut() else {
        return;
    };
    for (index, set) in ptcl.emitter_list.emitter_sets.iter_mut().enumerate() {
        if !live.contains(&index) {
            set.emitters.clear();
        }
    }
}

/// An empty ESET is not a safe game-side no-op: the runtime's set loader can consume the next
/// ESET's first emitter when the referenced set has no children.  Entries and variants use
/// 1-based set handles, with zero already defined as "none", so detach those handles while
/// retaining the ESET and entry name.  Retaining the tables preserves all other indices and
/// makes the operation idempotent across repeated project rebuilds.
fn clear_empty_emitter_set_references(file: &mut effect_library::NamcoEffectFile) {
    let Some(ptcl) = file.ptcl_file.as_ref() else {
        return;
    };
    let empty: std::collections::HashSet<u32> = ptcl
        .emitter_list
        .emitter_sets
        .iter()
        .enumerate()
        .filter(|(_, set)| set.emitters.is_empty())
        .map(|(index, _)| index as u32 + 1)
        .collect();
    if empty.is_empty() {
        return;
    }
    for entry in &mut file.entries {
        if empty.contains(&entry.emitter_set_id) {
            entry.emitter_set_id = 0;
        }
    }
    for variant in &mut file.effect_variants {
        if empty.contains(&(variant.emitter_set_id as u32)) {
            variant.emitter_set_id = 0;
        }
    }
}

/// Entry (kind) names in `file` whose emitters sample the named pool texture, with the emitter
/// set each one owns.
///
/// A texture import names a texture, not an effect, but the carrier can only hold textures its
/// own entries reference. This is how the live-carrier build turns "replace this texture" into
/// "and therefore clone these effects", so the pool the import lands on actually contains it.
/// All six sampler slots are checked, not just `sampler0` — a texture used only as a distortion
/// or mask input is still a texture the user can replace.
pub fn entries_sampling_texture(
    file: &effect_library::NamcoEffectFile,
    texture_name: &str,
) -> Vec<(String, usize)> {
    let Some(ptcl) = file.ptcl_file.as_ref() else {
        return Vec::new();
    };
    let Some(id) = ptcl
        .texture_info
        .as_ref()
        .and_then(|info| info.descriptors.iter().find(|d| d.name == texture_name))
        .map(|d| d.id)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, name) in file.entry_names.iter().enumerate() {
        let Some(set_idx) = (file.entries[index].emitter_set_id as usize).checked_sub(1) else {
            continue;
        };
        let Some(set) = ptcl.emitter_list.emitter_sets.get(set_idx) else {
            continue;
        };
        let mut samples = false;
        visit_emitters_ref(&set.emitters, &mut |em| {
            let d = &em.data;
            samples |= [
                d.sampler0.as_ref().map(|s| s.texture_id),
                d.sampler1.as_ref().map(|s| s.texture_id),
                d.sampler2.as_ref().map(|s| s.texture_id),
                d.sampler3.as_ref().map(|s| s.texture_id),
                d.sampler4.as_ref().map(|s| s.texture_id),
                d.sampler5.as_ref().map(|s| s.texture_id),
            ]
            .iter()
            .flatten()
            .any(|tid| *tid == id);
        });
        if samples {
            out.push((name.clone(), set_idx));
        }
    }
    out
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
///
/// Whole resources go or stay; the ones that stay are not slimmed, and that is where the
/// remaining headroom is. A stripped Kirby donor is 3.12 MB, of which 2.84 MB is 13 textures this
/// pass has already proved are all sampled — and all 13 carry full mip chains (112 levels between
/// them), roughly a quarter of those bytes. Dropping mips is therefore the largest lever left, and
/// it is deliberately not pulled: mip count is part of what the sampler descriptors expect, so a
/// short chain is a change to what the GPU reads rather than to what the file merely holds. That
/// is the class of change this format punishes with a pool that validates and draws nothing, so it
/// needs on-hardware confirmation and belongs behind a user's choice, not in every build.
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
///
/// Identical variations are NOT folded together, though they exist: a donor's own library is full
/// of them (225 of Pickel's 370 are byte-identical to another), and the map returned here is
/// exactly the mechanism that could collapse them. It is not worth it. Compaction runs first and
/// leaves 12–27 variations, of which 0–6 are duplicates, and folding those saves a flat ~8.8 KB —
/// 2.2% of a one-donor carrier, 0.2% of a three-donor one. That buys very little in exchange for
/// changing which compiled program an emitter binds to, in a format whose characteristic failure
/// is a pool that is structurally perfect and draws nothing on hardware.
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

/// Refuse to encode a file in which some emitter addresses a shader variation its containers do
/// not hold.
///
/// This is the last line before bytes reach the game, and it is deliberately a hard failure:
/// an out-of-range variation index hangs the game's loader rather than drawing wrong, so there is
/// nothing to be gained by shipping and seeing. `subject` names what was being built, since the
/// carrier and the exported eff fail for different reasons and the fix differs.
///
/// Runs AFTER compaction and independently of it — compaction renumbers indices through a map it
/// built itself, and this checks the result against the container that actually shipped.
fn verify_shader_indices_resolve(
    file: &effect_library::NamcoEffectFile,
    subject: &str,
) -> Result<()> {
    let shader = file
        .ptcl_file
        .as_ref()
        .and_then(|ptcl| ptcl.shader_info.as_ref());
    let count = |binary: Option<&Vec<u8>>| match binary {
        Some(bnsh) => effect_library::bnsh::BnshFile::read(bnsh)
            .map(|file| file.variations.len())
            .unwrap_or(0),
        None => 0,
    };
    let (standard_variations, compute_variations) = match shader {
        Some(shader) => (
            count(shader.binary_data.as_ref()),
            count(shader.compute_binary.as_ref()),
        ),
        None => (0, 0),
    };
    let Some(ptcl) = file.ptcl_file.as_ref() else {
        return Ok(());
    };
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
                "{subject} safety: '{}' addresses {kind} variation {index}, but the container \
                 only holds {available}. Shipping that would hang the game's loader, so nothing \
                 was written.",
                set.name
            );
        }
    }
    Ok(())
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
    // Strip the shader library down to what this file addresses, the same way the carrier does.
    // Cross-file transplants merge in each donor's ENTIRE BNSH — Pickel ships 370 variations and
    // one transplanted effect uses twelve — so without this a single transplant makes the
    // player's ef_*.eff carry a second fighter's whole shader library.
    //
    // Orphans go first: a replace-mode transplant leaves the set it repointed away from holding
    // live emitters, and compaction scans sets, so an orphan would keep its share of the donor
    // library reachable. Clearing them cannot touch anything the game shipped — every set in all
    // 328 game corpus files is reachable from the entry table.
    //
    // Unlike the carrier, native effects are NOT silenced: this file IS the fighter's own eff and
    // everything in it has to keep playing. Textures are not pruned either, and deliberately —
    // `apply_transplant_cross_file` copies only the textures the cloned emitters reference, so
    // there is no unreferenced-texture bloat here to collect, while a prune this late could drop
    // a pool texture that an authored swap or an import below is about to name.
    clear_unreachable_emitter_sets(&mut namco);
    compact_shader_containers(&mut namco)?;
    verify_shader_indices_resolve(&namco, "eff export")?;

    // Same order as the carrier build, and for the same reasons: added textures before the
    // authored edits (a swap resolves its target by name, so it has to exist first), replacements
    // after (they land on whichever texture will actually be sampled), removals last (the swaps
    // that moved emitters off them have run by then).
    //
    // Warnings go to stderr rather than a channel: the export UI reports the file it wrote, and
    // anything dropped here has already been surfaced by the live carrier build.
    let mut warnings = Vec::new();
    apply_texture_additions(&mut namco, &eff.textures_added, &mut warnings)?;
    {
        let ptcl = namco
            .ptcl_file
            .as_mut()
            .ok_or_else(|| anyhow!("source .eff has no embedded PTCL"))?;
        // Emitter lists first: they decide which emitters EXIST, and an authored edit addresses
        // one of the survivors by name and index. The other order would apply edits to emitters
        // the roster is about to drop, and miss the duplicates it is about to add.
        for roster in &eff.rosters {
            apply_roster(ptcl, roster)?;
        }
        for edit in &eff.authored {
            apply_authored(ptcl, edit)?;
        }
    }
    // Spawn structure last of the eff's own edits: it resolves emitter sets by name, so every
    // set a transplant was going to add is in place by now.
    for edit in &eff.entry_edits {
        apply_entry_edit(&mut namco, edit)?;
    }
    // Apply this after spawn edits too: an entry edit can deliberately repoint a kind or part
    // at the roster-cleared set, and that must not reintroduce the unsafe zero-emitter handle.
    clear_empty_emitter_set_references(&mut namco);
    apply_texture_imports(&mut namco, &eff.textures, &mut warnings)?;
    apply_texture_removals(&mut namco, &eff.textures_removed, &mut warnings)?;
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
    let mut primary_set_pos: Option<usize> = None;
    if let Some(idx) = (donor_entry.emitter_set_id as usize).checked_sub(1) {
        primary_set_pos = Some(donor_sets.len());
        donor_sets.push(
            donor_ptcl
                .emitter_list
                .emitter_sets
                .get(idx)
                .ok_or_else(|| anyhow!("donor primary emitter set out of range"))?
                .clone(),
        );
    }
    let variant_set_start = donor_sets.len();
    let mut variant_frames: Vec<u16> = Vec::new();
    let mut variant_bones: Vec<String> = Vec::new();
    if donor_entry.variant_count == 0 {
        if primary_set_pos.is_none() {
            anyhow::bail!("donor entry has neither a primary emitter set nor parts");
        }
    } else {
        let start = (donor_entry.variant_start_idx as usize)
            .checked_sub(1)
            .ok_or_else(|| anyhow!("donor variant start id 0"))?;
        let count = donor_entry.variant_count as usize;
        for (variant_offset, v) in donor
            .effect_variants
            .get(start..start + count)
            .ok_or_else(|| anyhow!("donor variant range out of bounds"))?
            .iter()
            .enumerate()
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
            variant_bones.push(
                donor
                    .external_bone_names
                    .get(start + variant_offset)
                    .cloned()
                    .unwrap_or_default(),
            );
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
    // External models are referenced through parallel name/flag tables. Transfer the selected
    // row and repoint the cloned entry; retaining the donor's numeric handle would index an
    // unrelated row in the carrier, while dropping it made model edits export-only.
    new_entry.external_model_idx = (new_entry.external_model_idx as usize)
        .checked_sub(1)
        .and_then(|index| {
            let name = donor.external_model_names.get(index)?.clone();
            let flag = donor.effect_models.get(index).copied().unwrap_or(0);
            let dest = namco
                .external_model_names
                .iter()
                .zip(&namco.effect_models)
                .position(|(n, f)| *n == name && *f == flag)
                .unwrap_or_else(|| {
                    namco.external_model_names.push(name);
                    namco.effect_models.push(flag);
                    namco.external_model_names.len() - 1
                });
            Some(dest as u32 + 1)
        })
        .unwrap_or(0);
    // Non-fighter donors (assists/items — e.g. ef_alucard) commonly use type 0 in the
    // kind u16's high byte. A FIGHTER destination needs 0x01xx: the fighter spawn path
    // rejects type-0 entries. An assist-owned carrier needs the inverse normalization:
    // fighter-type 0x01xx entries are not registered by the assist loader.
    new_entry.kind = destination_entry_kind(new_entry.kind, destination_is_fighter);
    if new_entry.variant_count == 0 {
        new_entry.emitter_set_id = new_set_ids[primary_set_pos.unwrap_or(0)];
    } else {
        new_entry.emitter_set_id = primary_set_pos
            .map(|position| new_set_ids[position])
            .unwrap_or(0);
        let new_start = namco.effect_variants.len() as u16 + 1; // raw 1-based
        for ((frame, set_id), bone) in variant_frames
            .iter()
            .zip(&new_set_ids[variant_set_start..])
            .zip(&variant_bones)
        {
            namco
                .effect_variants
                .push(effect_library::namco_file::EffectVariant {
                    start_frame: *frame,
                    emitter_set_id: *set_id as u16,
                });
            namco.external_bone_names.push(bone.clone());
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

    /// Compaction must be invisible to the GPU: after it, every emitter binds to a compiled
    /// program byte-identical to the one it bound to before.
    ///
    /// This is the property that could not be established by reading the code. Compaction drops
    /// variations and renumbers the survivors through a map it builds itself, so "the index is in
    /// range" — which `verify_shader_indices_resolve` already enforces — is a much weaker claim
    /// than "the index still means the same program". An off-by-one in that map satisfies the
    /// first while silently rebinding every emitter in the file to its neighbour's shader, and
    /// that failure only shows up on hardware.
    ///
    /// So it is checked end-to-end against real containers: record what each emitter resolves to,
    /// compact, resolve again through the rewritten container, compare. Half the sets are emptied
    /// first because that is what makes compaction do anything — it mirrors what silencing does to
    /// a carrier, and leaves a sparse, non-contiguous used-set, which is exactly the input a naive
    /// remap gets wrong.
    #[test]
    fn compaction_rebinds_every_emitter_to_the_same_shader_bytes() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        // Everything the GPU is handed for one variation, as a comparable value.
        fn programs(file: &effect_library::NamcoEffectFile, compute: bool) -> Option<Vec<String>> {
            let shader = file.ptcl_file.as_ref()?.shader_info.as_ref()?;
            let binary = if compute {
                shader.compute_binary.as_ref()?
            } else {
                shader.binary_data.as_ref()?
            };
            if binary.is_empty() {
                return None;
            }
            Some(
                effect_library::bnsh::BnshFile::read(binary)
                    .ok()?
                    .variations
                    .iter()
                    .map(|v| format!("{:?}", v.binary_program))
                    .collect(),
            )
        }
        // (set name, flat emitter index, which container, index into it) for every reference.
        fn bindings(
            file: &effect_library::NamcoEffectFile,
        ) -> Vec<(String, usize, &'static str, i32)> {
            let mut out = Vec::new();
            let Some(ptcl) = file.ptcl_file.as_ref() else {
                return out;
            };
            for set in &ptcl.emitter_list.emitter_sets {
                let mut flat = 0usize;
                super::visit_emitters_ref(&set.emitters, &mut |em| {
                    let refs = &em.data.shader_references;
                    for (kind, index) in [
                        ("shader", refs.shader_index),
                        ("user1", refs.user_shader_index1),
                        ("user2", refs.user_shader_index2),
                        ("compute", refs.compute_shader_index),
                    ] {
                        if index >= 0 {
                            out.push((set.name.clone(), flat, kind, index));
                        }
                    }
                    flat += 1;
                });
            }
            out
        }

        let (mut files, mut compacted_files, mut checked, mut dropped_total) =
            (0, 0, 0usize, 0usize);
        for path in walkdir(&root.join("effect")) {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(mut file) = effect_library::NamcoEffectFile::load(&bytes) else {
                continue;
            };
            if file.ptcl_file.is_none() {
                continue;
            }
            // Empty every other set, as silencing does, so the used-set is sparse.
            {
                let ptcl = file.ptcl_file.as_mut().unwrap();
                for (i, set) in ptcl.emitter_list.emitter_sets.iter_mut().enumerate() {
                    if i % 2 == 1 {
                        set.emitters.clear();
                    }
                }
            }
            let Some(before_std) = programs(&file, false) else {
                continue;
            };
            let before_cmp = programs(&file, true);
            files += 1;
            let before = bindings(&file);
            let want: Vec<Option<String>> = before
                .iter()
                .map(|(_, _, kind, index)| {
                    let table = if *kind == "compute" {
                        before_cmp.as_ref()?
                    } else {
                        &before_std
                    };
                    table.get(*index as usize).cloned()
                })
                .collect();

            if super::compact_shader_containers(&mut file).is_err() {
                continue;
            }
            let after_std = programs(&file, false);
            let after_cmp = programs(&file, true);
            let dropped = before_std.len() - after_std.as_ref().map(|c| c.len()).unwrap_or(0);
            if dropped > 0 {
                compacted_files += 1;
                dropped_total += dropped;
            }
            let after = bindings(&file);
            assert_eq!(
                before.len(),
                after.len(),
                "{}: compaction changed how many shader references exist",
                path.display()
            );
            for (i, (set, flat, kind, index)) in after.iter().enumerate() {
                let Some(want) = &want[i] else { continue };
                let table = if *kind == "compute" {
                    after_cmp.as_ref()
                } else {
                    after_std.as_ref()
                };
                assert_eq!(
                    table.and_then(|t| t.get(*index as usize)),
                    Some(want),
                    "{}: '{set}' emitter {flat} {kind} was rebound to a DIFFERENT program (now \
                     index {index}) — compaction's renumbering is wrong",
                    path.display()
                );
                checked += 1;
            }
        }
        eprintln!(
            "{checked} shader bindings across {files} files re-resolved to identical programs; \
             {compacted_files} files actually compacted, {dropped_total} variations dropped"
        );
        assert!(files > 300, "only {files} files exercised — wrong root?");
        assert!(
            compacted_files > 100 && dropped_total > 1000,
            "only {compacted_files} files compacted / {dropped_total} variations dropped — this \
             test is not exercising the renumbering it exists to check"
        );
    }

    /// Reachability is calibrated against the whole game corpus, because it decides what gets
    /// THROWN AWAY: a set the traversal fails to reach has its emitters cleared and its textures
    /// and shader variations collected.
    ///
    /// Two properties, both measured over the 327 game files that carry a PTCL (328 `.eff` files
    /// less the one with no particle section) and their 6865 emitter sets:
    ///
    /// - EVERY set in a game file is reachable. So [`clear_unreachable_emitter_sets`] cannot
    ///   touch anything the game itself shipped — it can only reach sets Visionary orphaned, and
    ///   a replace-mode transplant orphans one every time (`finish_transplant_entry`).
    /// - A multi-part entry's OWN `emitter_set_id` is live and is NOT merely a copy of its first
    ///   variant. 13 entries (Bayonetta, Zelda, Zero Suit Samus, Pikmin, the roulette stage and
    ///   `ef_item`) point it at a populated set that appears in no variant, and in none of the 77
    ///   multi-part entries does it duplicate a variant's set. Walking variants alone would strip
    ///   those 13 sets' resources out from under a live effect.
    #[test]
    fn every_game_emitter_set_is_reachable_from_the_entry_table() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let (mut files, mut sets_total, mut multi, mut set_id_distinct) = (0, 0, 0, 0);
        let mut unreachable: Vec<String> = Vec::new();
        for path in walkdir(&root.join("effect")) {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(file) = effect_library::NamcoEffectFile::load(&bytes) else {
                continue;
            };
            let Some(ptcl) = file.ptcl_file.as_ref() else {
                continue;
            };
            files += 1;
            sets_total += ptcl.emitter_list.emitter_sets.len();
            let live = super::live_emitter_sets(&file);
            for (index, set) in ptcl.emitter_list.emitter_sets.iter().enumerate() {
                if !live.contains(&index) {
                    unreachable.push(format!(
                        "{}: set {index} '{}' ({} emitters)",
                        path.display(),
                        set.name,
                        set.emitters.len()
                    ));
                }
            }
            // Independently re-derive the multi-part case, so this test fails if
            // `live_emitter_sets` ever stops unioning the entry's own set id.
            for entry in &file.entries {
                if entry.variant_count == 0 {
                    continue;
                }
                multi += 1;
                let start = (entry.variant_start_idx as usize).saturating_sub(1);
                let variants: Vec<usize> = file
                    .effect_variants
                    .iter()
                    .skip(start)
                    .take(entry.variant_count as usize)
                    .filter_map(|v| (v.emitter_set_id as usize).checked_sub(1))
                    .collect();
                let Some(own) = (entry.emitter_set_id as usize).checked_sub(1) else {
                    continue;
                };
                assert!(
                    !variants.contains(&own),
                    "a multi-part entry's emitter_set_id duplicates a variant set — the union in \
                     live_emitter_sets may be redundant after all, recheck before simplifying it"
                );
                if ptcl
                    .emitter_list
                    .emitter_sets
                    .get(own)
                    .is_some_and(|s| !s.emitters.is_empty())
                {
                    set_id_distinct += 1;
                }
            }
        }
        assert!(
            files > 300,
            "only {files} corpus files parsed — wrong root?"
        );
        eprintln!(
            "{files} files, {sets_total} sets, {multi} multi-part entries, \
             {set_id_distinct} with a distinct populated emitter_set_id"
        );
        assert!(
            unreachable.is_empty(),
            "{} game emitter sets are not reachable from the entry table, so clearing \
             unreachable sets would discard content the game ships:\n{}",
            unreachable.len(),
            unreachable.join("\n")
        );
        assert!(
            set_id_distinct > 0,
            "no multi-part entry pointed its own emitter_set_id at a populated set — this test \
             is no longer pinning the union it was written to pin"
        );
    }

    /// A texture NAME identifies a texture globally: the same name never carries two different
    /// descriptor ids anywhere in the corpus.
    ///
    /// Two things rest on this. Every name → id lookup in this module takes the FIRST matching
    /// descriptor (`entries_sampling_texture`, `prune_unreferenced_resources`,
    /// `apply_texture_imports`), which is only unambiguous because there is nothing else to
    /// match. And cross-donor texture dedup is already exact: the corpus holds 11831 descriptors
    /// under 3123 names, so donors share textures constantly, and because sharing a name means
    /// sharing an id, `apply_transplant_cross_file`'s "append only the ids the destination lacks"
    /// collapses every one of those without hashing a single byte. That is why there is no
    /// content-hash dedup pass here — it would have nothing left to find.
    #[test]
    fn a_texture_name_identifies_one_descriptor_id_corpus_wide() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        let mut ids_by_name: std::collections::HashMap<String, std::collections::HashSet<u64>> =
            Default::default();
        let mut total = 0usize;
        for path in walkdir(&root.join("effect")) {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(file) = effect_library::NamcoEffectFile::load(&bytes) else {
                continue;
            };
            let Some(info) = file
                .ptcl_file
                .as_ref()
                .and_then(|p| p.texture_info.as_ref())
            else {
                continue;
            };
            for descriptor in &info.descriptors {
                total += 1;
                ids_by_name
                    .entry(descriptor.name.clone())
                    .or_default()
                    .insert(descriptor.id);
            }
        }
        assert!(
            total > 10_000,
            "only {total} textures scanned — wrong root?"
        );
        let ambiguous: Vec<&String> = ids_by_name
            .iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(name, _)| name)
            .collect();
        eprintln!(
            "{total} texture descriptors under {} distinct names ({:.1}x sharing)",
            ids_by_name.len(),
            total as f64 / ids_by_name.len() as f64
        );
        assert!(
            ambiguous.is_empty(),
            "{} texture names carry more than one descriptor id, so resolving a texture by name \
             is ambiguous and GUID-keyed dedup is no longer exact: {ambiguous:?}",
            ambiguous.len()
        );
    }

    /// Every GAME `.eff` under `dir`.
    ///
    /// Leading-underscore names are skipped because they are not game data: Visionary writes
    /// `_transplant_preview.eff` NEXT TO the source file it was built from (`build_merged_preview`
    /// does this so sibling merges still resolve), so the export tree accumulates this tool's own
    /// output alongside Nintendo's. Letting one into a corpus test would be circular at best —
    /// and actively misleading here, since a preview built from a replace-mode transplant contains
    /// an orphaned set BY DESIGN and would fail an assertion about what the game ships.
    fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(read) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in read.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walkdir(&path));
                continue;
            }
            let is_generated = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('_'));
            if !is_generated && path.extension().is_some_and(|x| x == "eff") {
                out.push(path);
            }
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
            ..Default::default()
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
        let names: Vec<String> = textures
            .descriptors
            .iter()
            .map(|d| d.name.clone())
            .collect();
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
            .map(|(i, n)| (i, n.clone()))
            .expect("a convertible texture");
        // An import has to keep the texture's dimensions, so build the stand-in at whatever size
        // the chosen texture actually is rather than a fixed 64×64.
        let (index, name) = name;
        let shape =
            crate::texture_import::describe(&original_pool, index, &name).expect("describe");

        let scratch = tempfile::tempdir().expect("scratch dir");
        let png_path = scratch.path().join("import.png");
        let mut image = image::RgbaImage::new(shape.width, shape.height);
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
                raw: false,
            }],
            ..Default::default()
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
            (shape.width, shape.height),
            "an import must land at the texture's own size"
        );
    }

    /// The whole point of an added texture: give ONE emitter its own copy, edit that, and leave
    /// every other user of the original alone.
    ///
    /// Drives the real export path so the ordering constraints are exercised for real — the
    /// addition has to land before the authored swap can resolve it by name, and the swap has to
    /// land before the removal will let the original go.
    #[test]
    fn an_added_texture_isolates_one_emitter_and_the_original_is_untouched() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/kirby/ef_kirby.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let before = effect_library::NamcoEffectFile::load(&bytes).expect("parse");
        let ptcl = before.ptcl_file.as_ref().expect("ptcl");
        let info = ptcl.texture_info.as_ref().expect("textures");
        let names: Vec<String> = info.descriptors.iter().map(|d| d.name.clone()).collect();

        // Pick a texture more than one emitter samples — that is the case worth proving.
        let counts = {
            let mut counts: std::collections::HashMap<u64, usize> = Default::default();
            for set in &ptcl.emitter_list.emitter_sets {
                super::visit_emitters_ref(&set.emitters, &mut |em| {
                    if let Some(s) = em.data.sampler0.as_ref() {
                        *counts.entry(s.texture_id).or_default() += 1;
                    }
                });
            }
            counts
        };
        let (shared_id, users) = counts
            .iter()
            .filter(|(_, n)| **n >= 2)
            .max_by_key(|(_, n)| **n)
            .map(|(id, n)| (*id, *n))
            .expect("some texture is sampled by two or more emitters");
        let shared_name = info
            .descriptors
            .iter()
            .find(|d| d.id == shared_id)
            .map(|d| d.name.clone())
            .expect("descriptor for the shared texture");

        // Find one emitter that samples it, and record where it lives.
        let mut target: Option<(usize, String, usize, String)> = None;
        for (set_idx, set) in ptcl.emitter_list.emitter_sets.iter().enumerate() {
            let mut emitter_idx = 0usize;
            super::visit_emitters_ref(&set.emitters, &mut |em| {
                if target.is_none()
                    && em.data.sampler0.as_ref().map(|s| s.texture_id) == Some(shared_id)
                {
                    let name = em.data.name.clone().unwrap_or_default();
                    target = Some((set_idx, set.name.clone(), emitter_idx, name));
                }
                emitter_idx += 1;
            });
            if target.is_some() {
                break;
            }
        }
        let (set_idx, set_name, emitter_idx, emitter_name) = target.expect("an emitter using it");

        let copy_name = crate::texture_import::unique_texture_name(&names, &shared_name);
        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            textures_added: vec![crate::mod_project::TextureAddition {
                texture_name: copy_name.clone(),
                template_name: shared_name.clone(),
                png_path: String::new(), // a straight copy
                raw: false,
            }],
            authored: vec![AuthoredEdit {
                set_name: set_name.clone(),
                entry_name: String::new(),
                set_idx,
                emitter_name: emitter_name.clone(),
                emitter_idx,
                fields: EmitterFieldEdits {
                    texture_name: Some(copy_name.clone()),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, None).expect("rebuild");
        let after = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");
        let after_ptcl = after.ptcl_file.as_ref().expect("ptcl");
        let after_info = after_ptcl.texture_info.as_ref().expect("textures");

        // The copy exists, with an id of its own.
        let copy = after_info
            .descriptors
            .iter()
            .find(|d| d.name == copy_name)
            .expect("the added texture is in the pool");
        assert_ne!(
            copy.id, shared_id,
            "the copy must have its own descriptor id"
        );
        assert_eq!(
            after_info.descriptors.len(),
            info.descriptors.len() + 1,
            "exactly one texture should have been added"
        );

        // The targeted emitter moved to the copy; everyone else still uses the original.
        let after_counts = {
            let mut counts: std::collections::HashMap<u64, usize> = Default::default();
            for set in &after_ptcl.emitter_list.emitter_sets {
                super::visit_emitters_ref(&set.emitters, &mut |em| {
                    if let Some(s) = em.data.sampler0.as_ref() {
                        *counts.entry(s.texture_id).or_default() += 1;
                    }
                });
            }
            counts
        };
        assert_eq!(
            after_counts.get(&copy.id).copied().unwrap_or(0),
            1,
            "exactly one emitter should sample the copy"
        );
        assert_eq!(
            after_counts.get(&shared_id).copied().unwrap_or(0),
            users - 1,
            "the other users of '{shared_name}' must be left on it"
        );

        // And a removal is REFUSED while anything still samples the texture, rather than writing
        // a pool that leaves an emitter addressing a GUID no descriptor holds.
        let mut warnings = Vec::new();
        let mut still = effect_library::NamcoEffectFile::load(&rebuilt).expect("reload");
        super::apply_texture_removals(
            &mut still,
            std::slice::from_ref(&shared_name),
            &mut warnings,
        )
        .expect("a refused removal is not an error");
        assert_eq!(warnings, vec![format!("texture-in-use:{shared_name}")]);
        assert!(
            still
                .ptcl_file
                .as_ref()
                .and_then(|p| p.texture_info.as_ref())
                .is_some_and(|t| t.descriptors.iter().any(|d| d.name == shared_name)),
            "an in-use texture must survive the removal attempt"
        );
    }

    /// A cross-fighter transplant into a fighter's OWN eff must not smuggle the donor's whole
    /// shader library in with it.
    ///
    /// The static export path merges each donor's entire BNSH — that is how the copied emitters
    /// find their variations — and used to stop there, so exporting one Pickel effect into Mario
    /// shipped all of Pickel's variations inside `ef_mario.eff`. This is the carrier's compaction
    /// applied to the export, and the property to hold is both halves at once: the count drops,
    /// AND every index still resolves. A container trimmed without renumbering its emitters is
    /// exactly the shape that hangs the game's loader.
    #[test]
    fn exporting_a_cross_fighter_transplant_compacts_the_shader_library() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        const DONOR: &str = "effect/fighter/pickel/ef_pickel.eff";
        let base = std::fs::read(root.join(SRC)).expect("mario eff");
        let donor_bytes = std::fs::read(root.join(DONOR)).expect("pickel eff");

        let variations = |bytes: &[u8]| -> usize {
            let file = effect_library::NamcoEffectFile::load(bytes).expect("parse");
            let ptcl = file.ptcl_file.as_ref().expect("PTCL");
            ptcl.shader_info
                .as_ref()
                .and_then(|s| s.binary_data.as_ref())
                .map(|b| {
                    effect_library::bnsh::BnshFile::read(b)
                        .expect("container re-reads")
                        .variations
                        .len()
                })
                .unwrap_or(0)
        };
        let mario_variations = variations(&base);
        let donor_variations = variations(&donor_bytes);

        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            transplants: vec![crate::mod_project::TransplantOp {
                new_entry_name: "pickel_tnt_os".into(),
                src_file_rel: DONOR.into(),
                src_set_name: "pickel_tnt".into(),
                src_set_idx: 0,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            }],
            ..Default::default()
        };
        let built = super::rebuild_eff_bytes(&base, &eff, Some(&root)).expect("export rebuild");
        let after = variations(&built);
        eprintln!(
            "mario {mario_variations} + pickel {donor_variations} variations -> {after} after \
             compaction ({} B from {} B base)",
            built.len(),
            base.len()
        );
        assert!(
            after < mario_variations + donor_variations,
            "the merged container still holds every variation of both files ({after}) — \
             compaction did not run on the export path"
        );

        // What compaction does to an export with NO transplant, which is the risk case: this pass
        // now runs on every export, including one carrying only an authored edit. Mario addresses
        // every variation he ships, so `subset_container` leaves the container's bytes alone and
        // the only file that gets rewritten is one that actually needed trimming.
        let untouched = super::rebuild_eff_bytes(
            &base,
            &crate::mod_project::EffMod {
                source_rel: SRC.to_string(),
                ..Default::default()
            },
            Some(&root),
        )
        .expect("transplant-free rebuild");
        assert_eq!(
            variations(&untouched),
            mario_variations,
            "compaction dropped variations from an eff with no transplant — the destination \
             addresses fewer than it ships and this pass is no longer a no-op for plain exports"
        );

        // And the transplant arrived intact, addressing variations that exist.
        let out = effect_library::NamcoEffectFile::load(&built).expect("export re-reads");
        super::verify_shader_indices_resolve(&out, "test")
            .expect("every emitter must address a variation the shipped container holds");
        let index = out
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("pickel_tnt_os"))
            .expect("the transplanted entry is present");
        let ptcl = out.ptcl_file.as_ref().expect("PTCL");
        let set = out.entries[index].emitter_set_id as usize;
        assert!(
            !ptcl.emitter_list.emitter_sets[set - 1].emitters.is_empty(),
            "the transplanted set lost its emitters"
        );
        // EVERY one of Mario's own entries must still be playable — this file IS his eff, so
        // unlike the carrier nothing here may be silenced. Checked against the untouched original
        // rather than a hardcoded entry name, so the whole fighter is covered.
        let before = effect_library::NamcoEffectFile::load(&base).expect("mario re-reads");
        let before_ptcl = before.ptcl_file.as_ref().expect("PTCL");
        let emitters_of = |file: &effect_library::NamcoEffectFile,
                           ptcl: &effect_library::PtclFile,
                           index: usize| {
            let mut sets = std::collections::HashSet::new();
            super::entry_emitter_sets(file, index, &mut sets);
            sets.iter()
                .filter_map(|s| ptcl.emitter_list.emitter_sets.get(*s))
                .filter(|set| !set.emitters.is_empty())
                .count()
        };
        for (index, name) in before.entry_names.iter().enumerate() {
            let want = emitters_of(&before, before_ptcl, index);
            if want == 0 {
                continue;
            }
            let after_index = out
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(name))
                .unwrap_or_else(|| panic!("the export dropped Mario's entry '{name}'"));
            assert_eq!(
                emitters_of(&out, ptcl, after_index),
                want,
                "'{name}' lost populated emitter sets — the export path must not strip the \
                 destination's own effects"
            );
        }
    }

    /// The set a replace-mode transplant leaves behind must stop pinning resources.
    ///
    /// Replace repoints an entry at a freshly cloned set and leaves the set it used to name in the
    /// file with its emitters intact (`finish_transplant_entry`). No entry can spawn it again, but
    /// the resource passes scan sets rather than entries, so until it is cleared it goes on holding
    /// its textures and its share of the shader library. Nothing in the corpus is unreachable
    /// (`every_game_emitter_set_is_reachable_from_the_entry_table`), so a set this pass empties is
    /// always one Visionary itself orphaned.
    #[test]
    fn a_replaced_entrys_old_set_stops_holding_its_resources() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let base = std::fs::read(root.join(SRC)).expect("mario eff");
        let before = effect_library::NamcoEffectFile::load(&base).expect("mario parse");

        // Two of Mario's own entries backed by different, populated sets: one is repointed at a
        // clone of the other, orphaning the set it used to name.
        let populated = |file: &effect_library::NamcoEffectFile, index: usize| -> Option<usize> {
            let set = (file.entries[index].emitter_set_id as usize).checked_sub(1)?;
            let ptcl = file.ptcl_file.as_ref()?;
            (!ptcl.emitter_list.emitter_sets.get(set)?.emitters.is_empty()).then_some(set)
        };
        let usable: Vec<(usize, usize)> = (0..before.entries.len())
            .filter(|i| before.entries[*i].variant_count == 0)
            .filter_map(|i| populated(&before, i).map(|set| (i, set)))
            .collect();
        let (donor_index, _) = usable[0];
        let (target_index, orphan_set) = usable
            .iter()
            .copied()
            .find(|(_, set)| *set != usable[0].1)
            .expect("two entries on different populated sets");
        let donor_name = before.entry_names[donor_index].clone();
        let target_name = before.entry_names[target_index].clone();

        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            transplants: vec![crate::mod_project::TransplantOp {
                new_entry_name: target_name.clone(),
                src_file_rel: String::new(), // same-file
                src_set_name: donor_name.clone(),
                src_set_idx: 0,
                one_slot_slots: Vec::new(),
                replace_entry: Some(target_name.clone()),
            }],
            ..Default::default()
        };
        let built = super::rebuild_eff_bytes(&base, &eff, Some(&root)).expect("replace rebuild");
        let out = effect_library::NamcoEffectFile::load(&built).expect("re-read");
        let ptcl = out.ptcl_file.as_ref().expect("PTCL");

        // The replaced entry now points somewhere else...
        let after_index = out
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&target_name))
            .expect("the replaced entry keeps its name");
        let after_set = (out.entries[after_index].emitter_set_id as usize) - 1;
        assert_ne!(
            after_set, orphan_set,
            "'{target_name}' was not repointed, so this build orphaned nothing and the test \
             proves nothing"
        );
        assert!(
            !ptcl.emitter_list.emitter_sets[after_set]
                .emitters
                .is_empty(),
            "'{target_name}' points at an empty set — the clone did not arrive"
        );
        // ...and the set it abandoned is empty, so nothing it referenced is pinned by it.
        assert!(
            ptcl.emitter_list.emitter_sets[orphan_set]
                .emitters
                .is_empty(),
            "set {orphan_set} ('{}') is unreachable but still holds {} emitters, which keep its \
             textures and shader variations alive",
            ptcl.emitter_list.emitter_sets[orphan_set].name,
            ptcl.emitter_list.emitter_sets[orphan_set].emitters.len()
        );
        // The set is kept, not deleted: every id in the file still resolves.
        assert_eq!(
            ptcl.emitter_list.emitter_sets.len(),
            before
                .ptcl_file
                .as_ref()
                .map(|p| p.emitter_list.emitter_sets.len())
                .unwrap_or(0)
                + 1,
            "sets were removed rather than emptied — ids past the deletion no longer resolve"
        );
    }

    /// Building a carrier around a `SYS_*` donor is CORRECT — the reason the app refuses to send
    /// one is not in this module.
    ///
    /// Worth pinning precisely because it is counter-intuitive. `rides_the_carrier` blocks system
    /// donors from the live path, and the obvious reading of that is "the carrier build cannot
    /// handle them". It handles them fine: the entry arrives with populated emitter sets, every
    /// shader index resolves, and compaction holds the result to ~16 variations where `ef_common`
    /// would otherwise contribute 1348. The refusal upstream is about what the RUNTIME does with a
    /// carrier full of system effects — it stops the game's own `SYS_*` effects rendering — and
    /// nothing here can see or fix that.
    ///
    /// So this guards against two different mistakes: breaking the builder for multi-source
    /// donors, and "fixing" the wrong layer by looking for a byte-level fault that is not there.
    #[test]
    fn a_sys_effect_builds_a_correct_carrier_even_though_the_app_will_not_send_one() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const COMMON: &str = "effect/system/common/ef_common.eff";
        let Ok(carrier_base) = std::fs::read(root.join(CARRIER)) else {
            eprintln!("skipped: no carrier eff");
            return;
        };
        if !root.join(COMMON).exists() {
            eprintln!("skipped: no ef_common.eff");
            return;
        }
        for entry in ["SYS_HIT_NORMAL_S", "SYS_HIT_CRITICAL"] {
            let name = format!("{entry}_os");
            let mut warnings = Vec::new();
            let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
                &carrier_base,
                CARRIER,
                &[crate::mod_project::TransplantOp {
                    new_entry_name: name.clone(),
                    src_file_rel: COMMON.into(),
                    src_set_name: entry.into(),
                    src_set_idx: 0,
                    one_slot_slots: Vec::new(),
                    replace_entry: None,
                }],
                &root,
                &[],
                &[],
                &[],
                &[],
                &mut warnings,
            )
            .unwrap_or_else(|e| panic!("{entry}: sys carrier build failed: {e:#}"));

            let out = effect_library::NamcoEffectFile::load(&built)
                .unwrap_or_else(|e| panic!("{entry}: carrier does not re-read: {e:#}"));
            let ptcl = out.ptcl_file.as_ref().expect("carrier PTCL");
            let variations = ptcl
                .shader_info
                .as_ref()
                .and_then(|s| s.binary_data.as_ref())
                .map(|b| {
                    effect_library::bnsh::BnshFile::read(b)
                        .expect("container re-reads")
                        .variations
                        .len()
                })
                .unwrap_or(0);
            eprintln!(
                "sys carrier '{entry}': {} B, {variations} variations, {} textures, warnings \
                 {warnings:?}",
                built.len(),
                ptcl.texture_info
                    .as_ref()
                    .map(|t| t.descriptors.len())
                    .unwrap_or(0)
            );

            // It arrived, and it can actually emit.
            let index = out
                .entry_names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(&name))
                .unwrap_or_else(|| panic!("{entry}: '{name}' is not in the carrier"));
            let mut sets = std::collections::HashSet::new();
            super::entry_emitter_sets(&out, index, &mut sets);
            assert!(
                sets.iter().any(|s| ptcl
                    .emitter_list
                    .emitter_sets
                    .get(*s)
                    .is_some_and(|set| !set.emitters.is_empty())),
                "{entry}: '{name}' arrived with no populated emitter set — it would resolve and \
                 render nothing"
            );
            // Small enough to be worth sending: ef_common's own library is 1348 variations.
            assert!(
                variations < 100,
                "{entry}: the carrier kept {variations} shader variations — ef_common's library \
                 is being hauled into a live send"
            );
            assert!(
                built.len() < 3_000_000,
                "{entry}: the sys carrier is {} B",
                built.len()
            );
            super::verify_shader_indices_resolve(&out, "test")
                .expect("every emitter must address a variation the carrier holds");
        }
    }

    /// Transplanting one effect out of `ef_common.eff` must not drag the system library with it.
    ///
    /// `ef_common` is the worst case in the game and the one people actually hit: 33.4 MB, 205
    /// entries, 196 textures and **1348 shader variations**, and every fighter borrows hit and
    /// smoke effects from it. Merging its container whole put all 1348 into the destination — one
    /// `SYS_HIT_NORMAL_S` turned a 5.3 MB `ef_mario.eff` into 15.7 MB. Keeping only the 138 the
    /// merged file addresses brings the same build to 6.8 MB.
    ///
    /// What remains is irreducible without changing pixels, and was measured rather than assumed:
    /// of the +1.49 MB, textures are +1.25 MB (8 of them, every one reached through `sampler0`,
    /// all genuinely sampled, up to 405 KB each), shaders +167 KB, models +28 KB. There is no
    /// packing waste — merging those textures costs 36 KB LESS than exporting them standalone —
    /// and no dedup left to do, since a texture name IS its GUID and the destination holds none
    /// of them.
    #[test]
    fn a_common_eff_transplant_does_not_haul_the_system_shader_library() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const COMMON: &str = "effect/system/common/ef_common.eff";
        const DEST: &str = "effect/fighter/mario/ef_mario.eff";
        let Ok(common_bytes) = std::fs::read(root.join(COMMON)) else {
            eprintln!("skipped: no ef_common.eff in the export tree");
            return;
        };
        let base = std::fs::read(root.join(DEST)).expect("mario eff");
        let variations = |bytes: &[u8]| -> usize {
            let file = effect_library::NamcoEffectFile::load(bytes).expect("parse");
            file.ptcl_file
                .as_ref()
                .and_then(|p| p.shader_info.as_ref())
                .and_then(|s| s.binary_data.as_ref())
                .map(|b| {
                    effect_library::bnsh::BnshFile::read(b)
                        .expect("container re-reads")
                        .variations
                        .len()
                })
                .unwrap_or(0)
        };
        let common_variations = variations(&common_bytes);
        let base_variations = variations(&base);
        assert!(
            common_variations > 1000,
            "ef_common holds only {common_variations} variations — this test assumes it is the \
             big one"
        );

        let entry = "SYS_HIT_NORMAL_S";
        let eff = crate::mod_project::EffMod {
            source_rel: DEST.to_string(),
            transplants: vec![crate::mod_project::TransplantOp {
                new_entry_name: format!("{entry}_os"),
                src_file_rel: COMMON.into(),
                src_set_name: entry.into(),
                src_set_idx: 0,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            }],
            ..Default::default()
        };
        let built = super::rebuild_eff_bytes(&base, &eff, Some(&root)).expect("common transplant");
        let after = variations(&built);
        eprintln!(
            "ef_common '{entry}' into ef_mario: {} B -> {} B (+{} B); variations \
             {base_variations} + {common_variations} merged -> {after}",
            base.len(),
            built.len(),
            built.len() - base.len()
        );

        // The whole point: only a small fraction of the system library survives.
        assert!(
            after < base_variations + common_variations / 4,
            "the merged container kept {after} variations of a possible {}, so ef_common's \
             library is still being hauled across",
            base_variations + common_variations
        );
        // Growth is dominated by textures the effect genuinely samples, not by the library. The
        // uncompacted build measures +10.4 MB; anything near that means compaction stopped
        // running on this path.
        assert!(
            built.len() - base.len() < 2_500_000,
            "a single ef_common transplant added {} B — it measures +1.49 MB when only the \
             addressed variations are kept",
            built.len() - base.len()
        );
        // And it is still a loadable, self-consistent file.
        let out = effect_library::NamcoEffectFile::load(&built).expect("re-reads");
        super::verify_shader_indices_resolve(&out, "test")
            .expect("every emitter must address a variation the shipped container holds");
        let index = out
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&format!("{entry}_os")))
            .expect("the transplanted entry is present");
        let ptcl = out.ptcl_file.as_ref().expect("PTCL");
        let set = (out.entries[index].emitter_set_id as usize) - 1;
        assert!(
            !ptcl.emitter_list.emitter_sets[set].emitters.is_empty(),
            "the transplanted common effect arrived empty"
        );
        // Every texture anything samples came along — a hit effect whose pixels were left behind
        // is the failure this would otherwise ship silently.
        // `sampled_texture_ids` reports raw slot values, so the "no texture" sentinels 0 and
        // u64::MAX come back with the real GUIDs and are filtered here rather than treated as
        // missing resources.
        let sampled = super::sampled_texture_ids(ptcl);
        let pool: std::collections::HashSet<u64> = ptcl
            .texture_info
            .as_ref()
            .map(|t| t.descriptors.iter().map(|d| d.id).collect())
            .unwrap_or_default();
        let missing: Vec<u64> = sampled
            .iter()
            .filter(|id| **id != 0 && **id != u64::MAX)
            .filter(|id| !pool.contains(id))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "emitters sample {} texture GUIDs the merged pool does not hold: {missing:#x?}",
            missing.len()
        );
    }

    /// A multi-part transplant must keep the sets its VARIANTS name, not whichever set its stale
    /// `emitter_set_id` happens to point at.
    ///
    /// `apply_transplant_cross_file` appends a fresh variant block and leaves `emitter_set_id` at
    /// the donor's value, so silencing — which used to read only that field — kept some unrelated
    /// carrier set and cleared every one of the transplant's real ones. The effect arrived as a
    /// named, resolvable, completely silent entry.
    #[test]
    fn a_multi_part_transplant_keeps_every_variant_set() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        let carrier_base = std::fs::read(root.join(CARRIER)).expect("carrier eff");

        // Find any donor with a multi-part entry whose variants have emitters. Bayonetta and Zero
        // Suit Samus both qualify; scanning keeps the test from pinning one fighter's entry list.
        let candidates = [
            "effect/fighter/bayonetta/ef_bayonetta.eff",
            "effect/fighter/szerosuit/ef_szerosuit.eff",
            "effect/fighter/zelda/ef_zelda.eff",
            "effect/fighter/pikmin/ef_pikmin.eff",
        ];
        let mut chosen = None;
        for rel in candidates {
            let Ok(bytes) = std::fs::read(root.join(rel)) else {
                continue;
            };
            let Ok(file) = effect_library::NamcoEffectFile::load(&bytes) else {
                continue;
            };
            let Some(ptcl) = file.ptcl_file.as_ref() else {
                continue;
            };
            let found = file.entries.iter().enumerate().find(|(i, entry)| {
                if entry.variant_count < 2 {
                    return false;
                }
                let mut sets = std::collections::HashSet::new();
                super::entry_emitter_sets(&file, *i, &mut sets);
                sets.len() > 1
                    && sets.iter().all(|s| {
                        ptcl.emitter_list
                            .emitter_sets
                            .get(*s)
                            .is_some_and(|set| !set.emitters.is_empty())
                    })
            });
            if let Some((i, _)) = found {
                chosen = Some((rel, file.entry_names[i].clone(), i));
                break;
            }
        }
        let Some((donor_rel, entry_name, donor_index)) = chosen else {
            eprintln!("skipped: no multi-part donor with populated variant sets in the corpus");
            return;
        };
        eprintln!("multi-part donor: {donor_rel} '{entry_name}' (entry {donor_index})");

        let new_name = format!("{entry_name}_os");
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[crate::mod_project::TransplantOp {
                new_entry_name: new_name.clone(),
                src_file_rel: donor_rel.into(),
                src_set_name: entry_name.clone(),
                src_set_idx: 0,
                one_slot_slots: Vec::new(),
                replace_entry: None,
            }],
            &root,
            &[],
            &[],
            &[],
            &[],
            &mut Vec::new(),
        )
        .expect("multi-part carrier build");

        let out = effect_library::NamcoEffectFile::load(&built).expect("carrier re-reads");
        let ptcl = out.ptcl_file.as_ref().expect("carrier PTCL");
        let index = out
            .entry_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&new_name))
            .expect("the multi-part transplant is present");
        let entry = &out.entries[index];
        assert!(
            entry.variant_count >= 2,
            "'{new_name}' arrived with {} variants — it was flattened, not transplanted",
            entry.variant_count
        );
        let start = (entry.variant_start_idx as usize).saturating_sub(1);
        let mut populated = 0;
        for variant in out
            .effect_variants
            .iter()
            .skip(start)
            .take(entry.variant_count as usize)
        {
            let set_index = (variant.emitter_set_id as usize)
                .checked_sub(1)
                .expect("a variant with no emitter set");
            let set = ptcl
                .emitter_list
                .emitter_sets
                .get(set_index)
                .expect("a variant naming a set past the end of the file");
            assert!(
                !set.emitters.is_empty(),
                "'{new_name}' variant set {set_index} ('{}') was silenced — the transplant \
                 resolves by name and emits nothing",
                set.name
            );
            populated += 1;
        }
        eprintln!("'{new_name}': {populated} variant sets survived with emitters");
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
                raw: false,
            }],
            &mut warnings,
        )
        .expect("a stale import must not fail the build");
        assert_eq!(warnings, vec!["texture:ef_not_a_real_texture".to_string()]);
    }

    /// The point of [`entries_sampling_texture`] is that cloning what it returns carries the
    /// texture into the carrier's pool. Assert exactly that, end to end: strip the eff down to
    /// the entries it names and check the texture survives the prune. A helper that returned
    /// plausible-looking but wrong entries would leave the import landing on a pool that still
    /// does not hold the texture — the silent no-op this whole path exists to prevent.
    #[test]
    fn cloning_the_entries_that_sample_a_texture_carries_it_along() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const KIRBY: &str = "effect/fighter/kirby/ef_kirby.eff";
        let bytes = std::fs::read(root.join(KIRBY)).expect("Kirby eff");
        let file = effect_library::NamcoEffectFile::load(&bytes).expect("Kirby parse");
        let descriptors: Vec<(String, u64)> = file
            .ptcl_file
            .as_ref()
            .and_then(|p| p.texture_info.as_ref())
            .map(|t| {
                t.descriptors
                    .iter()
                    .map(|d| (d.name.clone(), d.id))
                    .collect()
            })
            .unwrap_or_default();
        assert!(!descriptors.is_empty(), "Kirby has pool textures");

        let mut checked = 0;
        for (name, id) in descriptors.iter().take(12) {
            let entries = super::entries_sampling_texture(&file, name);
            if entries.is_empty() {
                continue; // an unsampled texture has no effects to clone — nothing to assert
            }
            let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
            let stripped_bytes =
                super::strip_donor_eff_bytes(&bytes, &names).expect("strip to sampling entries");
            let stripped =
                effect_library::NamcoEffectFile::load(&stripped_bytes).expect("stripped parse");
            let survived = stripped
                .ptcl_file
                .as_ref()
                .and_then(|p| p.texture_info.as_ref())
                .is_some_and(|t| t.descriptors.iter().any(|d| d.id == *id));
            assert!(
                survived,
                "'{name}' was dropped by the prune even though {names:?} were said to sample it"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "no sampled texture found in Kirby — the test proved nothing"
        );
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
            ..Default::default()
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

    /// The same scoping guarantee as the colour test above, for the general attribute path.
    ///
    /// Worth checking separately because it writes through a table of a few hundred setters
    /// rather than through hand-written field assignments: a setter reaching the wrong field
    /// would move a value on an emitter the user never selected, and the effect would look wrong
    /// somewhere other than where the edit was made.
    #[test]
    fn an_attribute_edit_touches_only_the_targeted_emitter() {
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
        let mut names = Vec::new();
        super::visit_emitters_ref(
            &ptcl.emitter_list.emitter_sets[set_idx].emitters,
            &mut |em| names.push(em.data.display_name()),
        );

        // One attribute from each of three different blocks, so a setter that lands on a
        // neighbouring block shows up as a change in a place this test names.
        let mut attrs = std::collections::BTreeMap::new();
        attrs.insert(
            "emission.rate".to_string(),
            crate::eff_attrs::AttrValue::Float(7.25),
        );
        attrs.insert(
            "shape_info.volume_radius_x".to_string(),
            crate::eff_attrs::AttrValue::Float(3.5),
        );
        attrs.insert(
            "particle_data.life".to_string(),
            crate::eff_attrs::AttrValue::Int(42),
        );

        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            authored: vec![AuthoredEdit {
                set_name: set_name.clone(),
                entry_name: String::new(),
                set_idx,
                emitter_name: names[target_emitter].clone(),
                emitter_idx: target_emitter,
                fields: EmitterFieldEdits {
                    attrs: attrs.clone(),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, Some(&root)).expect("rebuild");
        let parsed = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");
        let after = emitter_snapshot(&parsed);

        let changed: Vec<(String, usize)> = before
            .iter()
            .zip(&after)
            .filter(|((_, _, a), (_, _, b))| a != b)
            .map(|((s, i, _), _)| (s.clone(), *i))
            .collect();
        assert_eq!(
            changed,
            vec![(set_name.clone(), target_emitter)],
            "an attribute edit leaked into other emitters"
        );

        // And the values are the ones asked for, read back out of the rebuilt file.
        let set = parsed
            .ptcl_file
            .as_ref()
            .expect("rebuilt PTCL")
            .emitter_list
            .emitter_sets
            .iter()
            .find(|s| s.name == set_name)
            .expect("target set");
        let mut nth = None;
        let mut idx = 0usize;
        super::visit_emitters_ref(&set.emitters, &mut |em| {
            if idx == target_emitter {
                nth = Some(em.data.clone());
            }
            idx += 1;
        });
        let data = nth.expect("target emitter in the rebuilt file");
        for (id, wanted) in &attrs {
            let got = crate::eff_attrs::index_of(id)
                .and_then(|i| (crate::eff_attrs::table()[i].get)(&data))
                .unwrap_or_else(|| panic!("'{id}' unreadable after the rebuild"));
            assert!(
                got.same(*wanted),
                "'{id}' came back as {got:?}, wanted {wanted:?}"
            );
        }
    }

    /// An edited emitter list has to survive the round trip through the writer: the right
    /// emitters, in the right order, with the copy carrying its own name.
    #[test]
    fn an_edited_emitter_list_rebuilds_into_a_loadable_eff() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let source = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let ptcl = source.ptcl_file.as_ref().expect("source PTCL");
        let (set_idx, set_name, names) = ptcl
            .emitter_list
            .emitter_sets
            .iter()
            .enumerate()
            .find_map(|(i, s)| {
                let mut names = Vec::new();
                super::visit_emitters_ref(&s.emitters, &mut |em| {
                    names.push(em.data.display_name())
                });
                (names.len() >= 3).then(|| (i, s.name.clone(), names))
            })
            .expect("a set with >= 3 emitters");

        // Drop the middle emitter, and play the first one twice.
        let slot = |idx: usize, name: &str| crate::mod_project::EmitterSlot {
            source_idx: idx,
            source_name: names[idx].clone(),
            name: name.to_string(),
            depth: 0,
        };
        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            rosters: vec![crate::mod_project::EmitterRoster {
                set_name: set_name.clone(),
                entry_name: String::new(),
                set_idx,
                slots: vec![
                    slot(0, &names[0]),
                    slot(0, "clone_of_first"),
                    slot(2, &names[2]),
                ],
            }],
            ..Default::default()
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, Some(&root)).expect("rebuild");
        let parsed = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");
        let set = parsed
            .ptcl_file
            .as_ref()
            .expect("rebuilt PTCL")
            .emitter_list
            .emitter_sets
            .iter()
            .find(|s| s.name == set_name)
            .expect("target set survives");
        let mut got = Vec::new();
        super::visit_emitters_ref(&set.emitters, &mut |em| got.push(em.data.display_name()));
        assert_eq!(
            got,
            vec![
                names[0].clone(),
                "clone_of_first".to_string(),
                names[2].clone()
            ],
            "the rebuilt set does not hold the emitters the list asked for"
        );

        // Every other set is untouched — a roster names one set and must not disturb the file.
        let other_before = ptcl.emitter_list.emitter_sets.len();
        let other_after = parsed
            .ptcl_file
            .as_ref()
            .expect("rebuilt PTCL")
            .emitter_list
            .emitter_sets
            .len();
        assert_eq!(other_before, other_after, "the set count changed");
    }

    /// An intentionally empty ESET must carry a NONE child offset.  The writer used to point
    /// this offset at the byte immediately after the ESET header; the game then interpreted the
    /// following ESET's first emitter as a child of the empty one.
    #[test]
    fn an_empty_roster_writes_a_none_child_offset() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let source = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let ptcl = source.ptcl_file.as_ref().expect("source PTCL");
        let set_name = ptcl.emitter_list.emitter_sets[0].name.clone();
        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            rosters: vec![crate::mod_project::EmitterRoster {
                set_name: set_name.clone(),
                entry_name: String::new(),
                set_idx: 0,
                slots: Vec::new(),
            }],
            ..Default::default()
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, Some(&root)).expect("rebuild");
        let out = effect_library::NamcoEffectFile::load(&rebuilt).expect("re-read");
        assert!(out.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets[0]
            .emitters
            .is_empty());

        let mut child_offset = None;
        for start in rebuilt
            .windows(4)
            .enumerate()
            .filter_map(|(i, w)| (w == b"ESET").then_some(i))
        {
            let binary_offset =
                u32::from_le_bytes(rebuilt[start + 16..start + 20].try_into().unwrap()) as usize;
            let name_start = start + binary_offset + 16;
            let name_end = name_start + set_name.len();
            if rebuilt.get(name_start..name_end) == Some(set_name.as_bytes()) {
                child_offset = Some(u32::from_le_bytes(
                    rebuilt[start + 8..start + 12].try_into().unwrap(),
                ));
                break;
            }
        }
        assert_eq!(
            child_offset,
            Some(u32::MAX),
            "empty ESET child offset must be NONE"
        );
    }

    #[test]
    fn an_emitter_animation_subsection_edit_survives_export() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let source = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let ptcl = source.ptcl_file.as_ref().expect("source PTCL");
        let mut target = None;
        for (set_idx, set) in ptcl.emitter_list.emitter_sets.iter().enumerate() {
            let mut emitter_idx = 0usize;
            super::visit_emitters_ref(&set.emitters, &mut |emitter| {
                if target.is_none() {
                    if let Some((section_idx, section)) =
                        emitter.subsections.iter().enumerate().find(|(_, section)| {
                            section.magic.starts_with("EA") && section.data.len() >= 16
                        })
                    {
                        target = Some((
                            set_idx,
                            set.name.clone(),
                            emitter_idx,
                            emitter.data.display_name(),
                            section_idx,
                            section.magic.clone(),
                        ));
                    }
                }
                emitter_idx += 1;
            });
            if target.is_some() {
                break;
            }
        }
        let (set_idx, set_name, emitter_idx, emitter_name, section_idx, magic) =
            target.expect("Mario has an EA emitter animation subsection");
        let replacement = 123.25f32.to_le_bytes();
        let subsection = crate::mod_project::SubsectionEdit {
            index: section_idx,
            magic: magic.clone(),
            bytes: replacement
                .into_iter()
                .enumerate()
                .map(|(i, byte)| (12 + i, byte))
                .collect(),
        };
        let eff = crate::mod_project::EffMod {
            source_rel: SRC.into(),
            authored: vec![crate::mod_project::AuthoredEdit {
                set_name: set_name.clone(),
                entry_name: String::new(),
                set_idx,
                emitter_name,
                emitter_idx,
                fields: crate::mod_project::EmitterFieldEdits {
                    subsections: vec![subsection],
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, Some(&root)).expect("rebuild");
        let parsed = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");
        let set = &parsed.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets[set_idx];
        let mut found = None;
        let mut index = 0usize;
        super::visit_emitters_ref(&set.emitters, &mut |emitter| {
            if index == emitter_idx {
                found = emitter.subsections.get(section_idx).cloned();
            }
            index += 1;
        });
        let section = found.expect("edited subsection survives");
        assert_eq!(section.magic, magic);
        assert_eq!(
            f32::from_le_bytes(section.data[12..16].try_into().unwrap()),
            123.25
        );
    }

    /// A spawn-structure edit has to come back out of the rebuilt file saying the same thing:
    /// the part list is stored as a shared table addressed by 1-based handles, so an off-by-one
    /// here would point the entry at another effect's parts.
    #[test]
    fn an_edited_spawn_structure_survives_the_rebuild() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const SRC: &str = "effect/fighter/mario/ef_mario.eff";
        let bytes = std::fs::read(root.join(SRC)).expect("source eff");
        let source = effect_library::NamcoEffectFile::load(&bytes).expect("parse source");
        let ptcl = source.ptcl_file.as_ref().expect("source PTCL");

        // A single-part entry, made into a two-part one.
        let entry_idx = source
            .entries
            .iter()
            .position(|e| e.variant_count == 0 && e.emitter_set_id != 0)
            .expect("a single-part entry");
        let entry_name = source.entry_names[entry_idx].clone();
        let first_set = ptcl.emitter_list.emitter_sets[0].name.clone();
        let second_set = ptcl.emitter_list.emitter_sets[1].name.clone();

        let eff = crate::mod_project::EffMod {
            source_rel: SRC.to_string(),
            entry_edits: vec![crate::mod_project::EntryEdit {
                entry_name: entry_name.clone(),
                emitter_set: Some(second_set.clone()),
                variants: Some(vec![
                    crate::mod_project::VariantEdit {
                        start_frame: 0,
                        set_name: first_set.clone(),
                        bone: String::new(),
                    },
                    crate::mod_project::VariantEdit {
                        start_frame: 12,
                        set_name: second_set.clone(),
                        bone: "handr".to_string(),
                    },
                ]),
                model: None,
            }],
            ..Default::default()
        };
        let rebuilt = super::rebuild_eff_bytes(&bytes, &eff, Some(&root)).expect("rebuild");
        let parsed = effect_library::NamcoEffectFile::load(&rebuilt).expect("parse rebuilt");

        let idx = parsed
            .entry_names
            .iter()
            .position(|n| *n == entry_name)
            .expect("entry survives");
        let entry = &parsed.entries[idx];
        assert_eq!(
            parsed.ptcl_file.as_ref().unwrap().emitter_list.emitter_sets
                [entry.emitter_set_id as usize - 1]
                .name,
            second_set,
            "the entry's primary emitter set did not change"
        );
        assert_eq!(entry.variant_count, 2, "the entry did not become two-part");
        let start = entry.variant_start_idx as usize - 1;
        let parts = &parsed.effect_variants[start..start + 2];
        let sets = &parsed
            .ptcl_file
            .as_ref()
            .expect("rebuilt PTCL")
            .emitter_list
            .emitter_sets;
        assert_eq!(parts[0].start_frame, 0);
        assert_eq!(parts[1].start_frame, 12);
        assert_eq!(sets[parts[0].emitter_set_id as usize - 1].name, first_set);
        assert_eq!(sets[parts[1].emitter_set_id as usize - 1].name, second_set);
        // The bone table runs one name per part; a short one would make every later entry read
        // the wrong bone.
        assert_eq!(
            parsed.external_bone_names.len(),
            parsed.effect_variants.len(),
            "the bone table did not grow with the part table"
        );
        assert_eq!(
            parsed.external_bone_names[start + 1],
            "handr",
            "the edited part attachment did not round-trip"
        );
    }

    /// Send uses an assist-owned carrier rather than rewriting the resident fighter EFF. Header
    /// edits must therefore be applied before the donor entry is cloned, or its new parts and
    /// model never enter the carrier's resource graph at all.
    #[test]
    fn live_carrier_includes_primary_parts_bones_and_model_edits() {
        let Some(root) = std::env::var_os("VISIONARY_EFF_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: set VISIONARY_EFF_ROOT to the extracted effect/ tree");
            return;
        };
        const CARRIER: &str = "effect/assist/bomberman/ef_bomberman.eff";
        const DONOR: &str = "effect/fighter/mario/ef_mario.eff";
        let carrier = std::fs::read(root.join(CARRIER)).expect("carrier");
        let donor_bytes = std::fs::read(root.join(DONOR)).expect("donor");
        let donor = effect_library::NamcoEffectFile::load(&donor_bytes).expect("parse donor");
        let ptcl = donor.ptcl_file.as_ref().expect("donor PTCL");
        let entry_idx = donor
            .entries
            .iter()
            .position(|entry| entry.variant_count == 0 && entry.emitter_set_id != 0)
            .expect("single-part entry");
        let source_name = donor.entry_names[entry_idx].clone();
        let first_set = ptcl.emitter_list.emitter_sets[0].name.clone();
        let second_set = ptcl.emitter_list.emitter_sets[1].name.clone();
        let clone_name = format!(
            "{}{}",
            crate::mod_project::EDIT_CLONE_PREFIX,
            source_name.to_lowercase()
        );
        let header = crate::mod_project::EntryEdit {
            entry_name: source_name.clone(),
            emitter_set: Some(second_set),
            variants: Some(vec![
                crate::mod_project::VariantEdit {
                    start_frame: 3,
                    set_name: first_set.clone(),
                    bone: "top".into(),
                },
                crate::mod_project::VariantEdit {
                    start_frame: 17,
                    set_name: first_set,
                    bone: "handr".into(),
                },
            ]),
            model: Some(crate::mod_project::ModelEdit {
                name: "visionary_live_model".into(),
                flag: 7,
            }),
        };
        let op = crate::mod_project::TransplantOp {
            new_entry_name: clone_name.clone(),
            src_file_rel: DONOR.into(),
            src_set_name: source_name,
            src_set_idx: 0,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        };
        let authored = super::CarrierAuthored {
            set_name: clone_name.clone(),
            edits: Vec::new(),
            roster: None,
            entry_edit: Some(header),
        };
        let bytes = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier,
            CARRIER,
            &[op],
            &root,
            &[authored],
            &[],
            &[],
            &[],
            &mut Vec::new(),
        )
        .expect("build live carrier");
        let built = effect_library::NamcoEffectFile::load(&bytes).expect("parse live carrier");
        let index = built
            .entry_names
            .iter()
            .position(|name| name == &clone_name)
            .expect("edited clone");
        let entry = &built.entries[index];
        assert_ne!(entry.emitter_set_id, 0, "edited primary set was dropped");
        assert_eq!(entry.variant_count, 2);
        let start = entry.variant_start_idx as usize - 1;
        assert_eq!(built.effect_variants[start].start_frame, 3);
        assert_eq!(built.effect_variants[start + 1].start_frame, 17);
        assert_eq!(
            &built.external_bone_names[start..start + 2],
            &["top", "handr"]
        );
        let model = entry.external_model_idx as usize - 1;
        assert_eq!(built.external_model_names[model], "visionary_live_model");
        assert_eq!(built.effect_models[model], 7);
    }

    #[test]
    fn changing_one_shared_model_flag_does_not_change_other_entries() {
        let mut file = effect_library::NamcoEffectFile {
            header: effect_library::namco_file::EffnHeader {
                magic: "EFFN".into(),
                version: 1,
                num_effects: 2,
                num_external_models: 1,
                multi_part_effects: 0,
                header_chunk_align: 1,
            },
            entries: vec![
                effect_library::namco_file::EffectHeader {
                    kind: 0,
                    unknown: 0,
                    emitter_set_id: 0,
                    external_model_idx: 1,
                    variant_start_idx: 0,
                    variant_count: 0,
                },
                effect_library::namco_file::EffectHeader {
                    kind: 0,
                    unknown: 0,
                    emitter_set_id: 0,
                    external_model_idx: 1,
                    variant_start_idx: 0,
                    variant_count: 0,
                },
            ],
            effect_variants: Vec::new(),
            effect_models: vec![3],
            entry_names: vec!["first".into(), "second".into()],
            external_model_names: vec!["model".into()],
            external_bone_names: Vec::new(),
            ptcl_file: None,
        };
        super::apply_entry_edit(
            &mut file,
            &crate::mod_project::EntryEdit {
                entry_name: "first".into(),
                emitter_set: None,
                variants: None,
                model: Some(crate::mod_project::ModelEdit {
                    name: "model".into(),
                    flag: 9,
                }),
            },
        )
        .unwrap();

        assert_eq!(file.entries[1].external_model_idx, 1);
        assert_eq!(file.effect_models[0], 3, "the other entry's flag changed");
        let first = file.entries[0].external_model_idx as usize - 1;
        assert_eq!(file.external_model_names[first], "model");
        assert_eq!(file.effect_models[first], 9);
    }

    #[test]
    fn legacy_keyframe_edits_preserve_their_frames() {
        let mut data = crate::eff_attrs::blank_emitter_data(crate::eff_attrs::SSBU_VFX_VERSION);
        data.particle_color.color0_type = effect_library::ColorType::Animated8Key;
        data.particle_color.color1_type = effect_library::ColorType::Animated8Key;
        data.emitter_static.num_color0_keys = 1;
        data.emitter_static.num_color1_keys = 1;
        data.emitter_static.num_alpha0_keys = 1;
        let mut emitter = effect_library::structs::Emitter {
            data,
            binary_data: None,
            cached_binary: None,
            subsections: Vec::new(),
            children: Vec::new(),
        };
        super::apply_fields(
            &mut emitter,
            &EmitterFieldEdits {
                color0: Some(vec![[0.1, 0.2, 0.3, 11.0]]),
                color1: Some(vec![[0.4, 0.5, 0.6, 12.0]]),
                alpha0: Some(vec![[0.7, 13.0]]),
                ..Default::default()
            },
        );

        assert_eq!(emitter.data.emitter_static.color0.keys[0].time, 11.0);
        assert_eq!(emitter.data.emitter_static.color1.keys[0].time, 12.0);
        assert_eq!(emitter.data.emitter_static.alpha0.keys[0].time, 13.0);
    }

    /// The exporter takes an emitter list back from flat-and-parent-first to a tree, which is
    /// how the file stores it. Nesting is the part of an emitter list that cannot be seen in the
    /// UI once it is wrong — a child emitter inherits from its parent, so a mis-parented one
    /// keeps drawing, just off the wrong thing.
    #[test]
    fn an_emitter_list_nests_back_into_a_tree() {
        let blank = crate::eff_attrs::blank_emitter_data(crate::eff_attrs::SSBU_VFX_VERSION);
        let named = |name: &str| {
            let mut data = blank.clone();
            super::set_emitter_name(&mut data, name);
            effect_library::structs::Emitter {
                data,
                binary_data: None,
                cached_binary: None,
                subsections: Vec::new(),
                children: Vec::new(),
            }
        };
        //  A
        //    A1
        //      A1a
        //    A2
        //  B
        let flat: Vec<_> = ["A", "A1", "A1a", "A2", "B"]
            .iter()
            .map(|n| named(n))
            .collect();
        let tree = super::nest_by_depth(flat, &[0, 1, 2, 1, 0]);

        assert_eq!(tree.len(), 2, "two roots");
        assert_eq!(tree[0].data.display_name(), "A");
        assert_eq!(tree[1].data.display_name(), "B");
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(tree[0].children[0].data.display_name(), "A1");
        assert_eq!(tree[0].children[1].data.display_name(), "A2");
        assert_eq!(tree[0].children[0].children.len(), 1);
        assert_eq!(tree[0].children[0].children[0].data.display_name(), "A1a");

        // A depth that skips a level has no emitter to hang off, so it becomes a child of the
        // row above rather than being dropped or panicking on a missing parent.
        let flat: Vec<_> = ["A", "deep"].iter().map(|n| named(n)).collect();
        let tree = super::nest_by_depth(flat, &[0, 7]);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 1, "a skipped level clamps to one");
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
            // Set from the sizes printed just above, with room for the pools to shift: these three
            // cases measure 156 KB, 402 KB and 578 KB, against a 3.4 MB carrier base and donors of
            // 5.3 MB (Pickel) and 3.9 MB (Daisy). The bound was 8 MB while the comment here said
            // compaction was disabled and a single donor shipped its whole shader library — it is
            // enabled, so 8 MB had stopped catching anything: a carrier hauling an entire donor
            // across would have slipped under it. None of these three cases is mesh-backed; a
            // Kirby-style mesh carrier measures 3.1 MB and is covered separately by
            // `every_mesh_donor_keeps_its_geometry_in_one_carrier`.
            assert!(
                built.len() < 1_500_000,
                "{label}: carrier is {} B, over the 1.5 MB these stripped cases measure well \
                 under — stripping regressed",
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
            roster: None,
            entry_edit: None,
        };
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            std::slice::from_ref(&authored),
            &[],
            &[],
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
            roster: None,
            entry_edit: None,
        };
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            std::slice::from_ref(&authored),
            &[],
            &[],
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
                roster: None,
                entry_edit: None,
            };
            let mut skipped = Vec::new();
            let bytes = super::rebuild_runtime_carrier_eff_bytes_with_edits(
                &carrier_base,
                CARRIER,
                std::slice::from_ref(&op),
                &root,
                std::slice::from_ref(&authored),
                &[],
                &[],
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
            roster: None,
            entry_edit: None,
        };
        let mut warnings = Vec::new();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &ops,
            &root,
            std::slice::from_ref(&authored),
            &[],
            &[],
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
            roster: None,
            entry_edit: None,
        };
        let mut skipped = Vec::new();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_base,
            CARRIER,
            &[op],
            &root,
            std::slice::from_ref(&orphan),
            &[],
            &[],
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
            let id = descriptors.iter().find(|d| d.name == wanted).map(|d| d.id);
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
                k.time = row[3];
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
                k.time = row[3];
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
                k.time = row[1];
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
    // General attributes go on LAST, so that where one names the same field as a legacy record
    // above — `emission.rate` against `emission_rate`, say — the newer, more specific edit is
    // the one that survives. A project written by the current editor only carries these.
    for (id, value) in &f.attrs {
        if !crate::eff_attrs::write(d, id, *value) {
            eprintln!(
                "[EFF-EXPORT] warning: emitter '{}' carries an edit to unknown attribute '{id}' \
                 — skipped",
                d.display_name()
            );
        }
    }
    for edit in &f.subsections {
        let Some(section) = em.subsections.get_mut(edit.index) else {
            eprintln!(
                "[EFF-EXPORT] warning: emitter '{}' has no subsection index {} — skipped",
                d.display_name(),
                edit.index
            );
            continue;
        };
        if section.magic != edit.magic {
            eprintln!(
                "[EFF-EXPORT] warning: emitter '{}' subsection {} is '{}' rather than '{}' — skipped",
                d.display_name(), edit.index, section.magic, edit.magic
            );
            continue;
        }
        for (&offset, &value) in &edit.bytes {
            if let Some(byte) = section.data.get_mut(offset) {
                *byte = value;
            }
        }
    }
    // Serializer prefers the cached EMTR blob; clearing it forces a re-encode of `data`.
    em.cached_binary = None;
}

/// Rebuild one emitter set's emitter list from a roster: which emitters play, in what order,
/// nested how.
///
/// Every slot clones an emitter of the set AS IT STANDS, so a roster is idempotent — the source
/// emitters are read into a flat list first and the set is then rebuilt from that, which is what
/// makes "duplicate emitter 2" mean the same thing however many times it is applied.
///
/// Children are re-parented by depth rather than kept with their original parent: the editor
/// presents the set as a flat, parent-first list, and that is the only ordering an edit can be
/// expressed against.
fn apply_roster(
    ptcl: &mut effect_library::PtclFile,
    roster: &crate::mod_project::EmitterRoster,
) -> Result<()> {
    let sets = &mut ptcl.emitter_list.emitter_sets;
    let set_named = |s: &effect_library::structs::EmitterSet| {
        !roster.set_name.is_empty() && s.name == roster.set_name
    };
    let set_idx = if sets.get(roster.set_idx).map(set_named).unwrap_or(false) {
        roster.set_idx
    } else {
        sets.iter().position(set_named).unwrap_or(roster.set_idx)
    };
    let set = sets.get_mut(set_idx).ok_or_else(|| {
        anyhow!(
            "emitter set '{}' (idx {}) not found — cannot apply its emitter list",
            roster.set_name,
            roster.set_idx
        )
    })?;

    // Flatten the source, parent-first, keeping each emitter's own subsections but dropping the
    // nesting — the roster restates it.
    let mut flat: Vec<effect_library::structs::Emitter> = Vec::new();
    fn flatten(
        src: &[effect_library::structs::Emitter],
        out: &mut Vec<effect_library::structs::Emitter>,
    ) {
        for em in src {
            let mut copy = em.clone();
            copy.children = Vec::new();
            out.push(copy);
            flatten(&em.children, out);
        }
    }
    flatten(&set.emitters, &mut flat);

    let names: Vec<String> = flat.iter().map(|em| em.data.display_name()).collect();
    let mut built: Vec<effect_library::structs::Emitter> = Vec::new();
    let mut depths: Vec<u8> = Vec::new();
    for slot in &roster.slots {
        // Same resolution rule as an authored edit: the stored name at the stored index wins
        // outright, then the name anywhere, then the bare index.
        let named =
            |i: usize| !slot.source_name.is_empty() && names.get(i) == Some(&slot.source_name);
        let source = if named(slot.source_idx) {
            Some(slot.source_idx)
        } else if let Some(i) = (0..names.len()).find(|i| named(*i)) {
            Some(i)
        } else if slot.source_idx < flat.len() {
            Some(slot.source_idx)
        } else {
            None
        };
        let Some(source) = source else {
            eprintln!(
                "[EFF-EXPORT] warning: emitter list for '{}' names a source emitter '{}' \
                 (idx {}) this set does not have — slot dropped",
                roster.set_name, slot.source_name, slot.source_idx
            );
            continue;
        };
        let mut emitter = flat[source].clone();
        if !slot.name.is_empty() && slot.name != names[source] {
            set_emitter_name(&mut emitter.data, &slot.name);
            // The cached EMTR blob still spells the old name; drop it so the rename is encoded.
            emitter.cached_binary = None;
        }
        built.push(emitter);
        depths.push(slot.depth);
    }

    if built.is_empty() && !roster.slots.is_empty() {
        return Err(anyhow!(
            "emitter list for '{}' resolved to no emitters at all",
            roster.set_name
        ));
    }

    set.emitters = nest_by_depth(built, &depths);
    Ok(())
}

/// Apply one entry's spawn-structure edit: which parts play at which frame, and which external
/// model comes with the effect.
///
/// This is header data, not particle data, so it is written against the whole `NamcoEffectFile`
/// rather than the PTCL. Both tables it touches are shared by every entry in the file and indexed
/// by 1-based handles, so nothing is edited in place: the variants are APPENDED and the entry is
/// repointed at the new run. The old run is left where it is — some other entry may still point
/// into it, and the file's own writer recomputes the counts from the vectors' lengths.
fn apply_entry_edit(
    namco: &mut effect_library::NamcoEffectFile,
    edit: &crate::mod_project::EntryEdit,
) -> Result<()> {
    let entry_idx = namco
        .entry_names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(&edit.entry_name))
        .ok_or_else(|| {
            anyhow!(
                "spawn edit names entry '{}', which this eff does not have",
                edit.entry_name
            )
        })?;

    if let Some(set_name) = &edit.emitter_set {
        let set_id = if set_name.is_empty() {
            0
        } else {
            let idx = namco
                .ptcl_file
                .as_ref()
                .and_then(|p| {
                    p.emitter_list
                        .emitter_sets
                        .iter()
                        .position(|s| s.name == *set_name)
                })
                .ok_or_else(|| {
                    anyhow!(
                        "entry '{}' names primary emitter set '{}', which this eff does not have",
                        edit.entry_name,
                        set_name
                    )
                })?;
            u32::try_from(idx + 1)
                .map_err(|_| anyhow!("emitter set index {idx} does not fit a 32-bit id"))?
        };
        namco.entries[entry_idx].emitter_set_id = set_id;
    }

    if let Some(variants) = &edit.variants {
        // Resolve every set name BEFORE touching the file: a typo should leave the entry as it
        // was, not half-rewritten with the parts that happened to resolve.
        let resolved: Vec<(u16, u16, String)> = variants
            .iter()
            .map(|v| {
                let set_id = if v.set_name.is_empty() {
                    0u16
                } else {
                    let idx = namco
                        .ptcl_file
                        .as_ref()
                        .and_then(|p| {
                            p.emitter_list
                                .emitter_sets
                                .iter()
                                .position(|s| s.name == v.set_name)
                        })
                        .ok_or_else(|| {
                            anyhow!(
                                "part of '{}' names emitter set '{}', which this eff does not have",
                                edit.entry_name,
                                v.set_name
                            )
                        })?;
                    u16::try_from(idx + 1).map_err(|_| {
                        anyhow!("emitter set index {idx} does not fit an effect part's 16-bit id")
                    })?
                };
                Ok((v.start_frame, set_id, v.bone.clone()))
            })
            .collect::<Result<_>>()?;

        if resolved.is_empty() {
            namco.entries[entry_idx].variant_start_idx = 0;
            namco.entries[entry_idx].variant_count = 0;
        } else {
            let start = u16::try_from(namco.effect_variants.len() + 1)
                .map_err(|_| anyhow!("this eff already holds the most parts the format allows"))?;
            for (start_frame, emitter_set_id, bone) in resolved {
                namco
                    .effect_variants
                    .push(effect_library::namco_file::EffectVariant {
                        start_frame,
                        emitter_set_id,
                    });
                // The bone table runs parallel to the variant table — one name per part — so it
                // has to grow in lockstep or every later part reads the wrong bone.
                namco.external_bone_names.push(bone);
            }
            let count = namco.effect_variants.len() + 1 - start as usize;
            namco.entries[entry_idx].variant_start_idx = start;
            namco.entries[entry_idx].variant_count = count as u16;
        }
    }

    if let Some(model) = &edit.model {
        if model.name.is_empty() {
            namco.entries[entry_idx].external_model_idx = 0;
        } else {
            // A model row may be shared by several entries. Reusing a matching name with a
            // different flag would change all of them, even though this edit names one entry.
            // Reuse only an exact pair; otherwise append a private row for this entry.
            let idx = namco
                .external_model_names
                .iter()
                .zip(&namco.effect_models)
                .position(|(name, flag)| name == &model.name && *flag == model.flag)
                .unwrap_or_else(|| {
                    namco.external_model_names.push(model.name.clone());
                    namco.effect_models.push(model.flag);
                    namco.external_model_names.len() - 1
                });
            namco.entries[entry_idx].external_model_idx = idx as u32 + 1;
        }
    }
    Ok(())
}

/// Rename an emitter, writing whichever of the two name fields this file's version uses.
///
/// The emitter carries its name in a 64-byte slot below vfx version 40 and a 96-byte one above,
/// and `effect_library` exposes them as separate fields — `display_name` prefers the v40 one.
/// Both are written when both exist, so the name is the same whichever the writer picks. The cap
/// is one byte short of each slot so the string keeps its terminator.
fn set_emitter_name(data: &mut effect_library::EmitterData, name: &str) {
    fn clip(name: &str, max: usize) -> String {
        name.char_indices()
            .take_while(|(i, c)| i + c.len_utf8() <= max)
            .map(|(_, c)| c)
            .collect()
    }
    if data.namev40.is_some() {
        data.namev40 = Some(clip(name, 95));
    }
    if data.name.is_some() || data.namev40.is_none() {
        data.name = Some(clip(name, 63));
    }
}

/// Rebuild a tree from a parent-first list and its depths. A slot deeper than the one before it
/// by more than one step is clamped to "child of the previous emitter", because there is no
/// emitter at the depth it asks for — that is a malformed roster, not a new level.
fn nest_by_depth(
    emitters: Vec<effect_library::structs::Emitter>,
    depths: &[u8],
) -> Vec<effect_library::structs::Emitter> {
    let mut roots: Vec<effect_library::structs::Emitter> = Vec::new();
    // Path of indices from the root down to the emitter last placed at each depth.
    let mut path: Vec<usize> = Vec::new();
    for (emitter, &depth) in emitters.into_iter().zip(depths) {
        let depth = (depth as usize).min(path.len());
        path.truncate(depth);
        let mut level = &mut roots;
        for &step in &path {
            level = &mut level[step].children;
        }
        level.push(emitter);
        path.push(level.len() - 1);
    }
    roots
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
            ..Default::default()
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
            roster: None,
            entry_edit: None,
        };

        let t = Instant::now();
        let built = super::rebuild_runtime_carrier_eff_bytes_with_edits(
            &carrier_bytes,
            CARRIER,
            &ops,
            &root,
            std::slice::from_ref(&authored),
            &[],
            &[],
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
