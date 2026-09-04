//! The new-character tab: create a costume-backed character and see what it still needs.
//!
//! The first thing this panel says is what it is not. A slot-backed character gets its own
//! model, animations, effects, name, and moveset, and it is selected as a **costume of the
//! donor** — not as its own cell on the character select grid.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{Color32, RichText, Ui};

use crate::mod_project::{AuthoredEntry, RosterMod};

use super::index::RosterIndex;
use super::library::{ModLibrary, ModSource};
use super::{scaffold, RosterKey};

/// What the per-character rows asked for.
enum ExistingAction {
    None,
    Remove(RosterKey),
    Target(Option<RosterKey>),
    /// Jump to Character Select with this character's editor open.
    GotoCss(RosterKey),
}

/// Which creation button was pressed. The folder picker (Create) and the
/// no-dialog path (Quick) funnel into the same `NewCharacterAction::Create.
enum CreatorAsk {
    None,
    PickFolder,
    Quick,
}

#[derive(Default)]
pub struct NewCharacterView {
    donor: Option<String>,
    slot: Option<u8>,
    display_name: String,
    status: String,
    error: Option<String>,
    /// Jump to Character Select with this character selected. Read and
    /// cleared by the window each frame (this tab cannot switch tabs).
    goto_css: Option<RosterKey>,
}

impl NewCharacterView {
    /// Take the pending Character Select jump, if any.
    pub fn take_goto_character_select(&mut self) -> Option<RosterKey> {
        self.goto_css.take()
    }
}

/// What the panel wants done after it draws. Returned rather than performed inline so the
/// window owns every mutation of the library and project.
pub enum NewCharacterAction {
    None,
    /// Send moveset edits to this character's costume, or (with `None`) back to the fighter
    /// as a whole.
    SetEditTarget(Option<RosterKey>),
    Create {
        donor: String,
        slot: u8,
        display_name: String,
        name_id: String,
        destination: PathBuf,
    },
    Remove(RosterKey),
}

impl NewCharacterView {
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        roots: &[PathBuf],
        index: &RosterIndex,
        project: &RosterMod,
        edit_target: Option<&RosterKey>,
        labels: &std::collections::HashMap<u64, String>,
        authored_moves: &BTreeMap<RosterKey, std::collections::BTreeSet<String>>,
    ) -> NewCharacterAction {
        let mut action = NewCharacterAction::None;

        self.draw_limitation(ui);
        ui.add_space(8.0);

        let creator = self.draw_creator(ui, roots, index);
        match creator {
            CreatorAsk::None => {}
            CreatorAsk::PickFolder => action = self.build_create_action(),
            CreatorAsk::Quick => action = self.build_quick_create_action(project),
        }
        ui.add_space(8.0);
        match self.draw_existing(ui, roots, index, project, edit_target, labels, authored_moves) {
            ExistingAction::None => {}
            ExistingAction::Remove(key) => action = NewCharacterAction::Remove(key),
            ExistingAction::Target(key) => action = NewCharacterAction::SetEditTarget(key),
            ExistingAction::GotoCss(key) => self.goto_css = Some(key),
        }

        if let Some(error) = &self.error.clone() {
            ui.add_space(6.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::from_rgb(240, 120, 120), "✘");
                        ui.label(RichText::new(error).small().weak());
                    });
                });
        } else if !self.status.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(130, 225, 150), "●");
                ui.label(RichText::new(&self.status).small().weak());
            });
        }
        action
    }

    fn draw_limitation(&self, ui: &mut Ui) {
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.label(RichText::new("New characters are costumes on a donor fighter.").small());
                ui.label(
                    RichText::new("Select via donor's costume. Roster position is set in Character Select.")
                        .small()
                        .weak(),
                );
            });
    }

    fn draw_creator(&mut self, ui: &mut Ui, roots: &[PathBuf], index: &RosterIndex) -> CreatorAsk {
        let mut pending = CreatorAsk::None;
        let donors: Vec<&super::RosterEntry> = index
            .sorted()
            .into_iter()
            .filter(|entry| entry.fighter.is_some() && !entry.backing.shares_engine_fighter())
            .collect();

        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.heading("Step 1 — Base character");
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Donor, costume slot, and name — one row each.")
                        .small()
                        .weak(),
                );
                ui.add_space(8.0);

                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Based on").small().strong());
                    ui.add_space(8.0);
                    // Flex with the window instead of forcing a 220px box.
                    let combo_w = (ui.available_width() - 110.0).clamp(140.0, 220.0);
                    egui::ComboBox::from_id_salt("newchar_donor")
                        .selected_text(
                            self.donor
                                .clone()
                                .unwrap_or_else(|| "Pick a fighter…".to_string()),
                        )
                        .width(combo_w)
                        .show_ui(ui, |ui| {
                            for entry in &donors {
                                let Some(fighter) = &entry.fighter else { continue };
                                let is_selected = self.donor.as_deref() == Some(fighter.as_str());
                                if ui
                                    .selectable_label(
                                        is_selected,
                                        format!("{}  ({fighter})", entry.display_name),
                                    )
                                    .clicked()
                                {
                                    self.donor = Some(fighter.clone());
                                    self.slot = None;
                                }
                            }
                        });
                });
                ui.add_space(6.0);

                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Costume slot").small().strong());
                    ui.add_space(8.0);
                    let mut slot = self.slot.unwrap_or(8);
                    if ui
                        .add(egui::DragValue::new(&mut slot).range(0..=255).prefix("c").speed(1))
                        .changed()
                    {
                        self.slot = Some(slot);
                    }
                    self.slot.get_or_insert(slot);
                    if let Some(taken) = self.slot_conflict(roots, index) {
                        ui.colored_label(Color32::from_rgb(240, 200, 120), RichText::new(taken).small());
                    } else if self.slot.is_some() {
                        ui.colored_label(Color32::from_rgb(130, 225, 150), RichText::new("✓ free").small());
                    }
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Display name").small().strong());
                    ui.add_space(8.0);
                    let name_w = (ui.available_width() - 140.0).clamp(100.0, 200.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.display_name)
                            .desired_width(name_w)
                            .hint_text("shown in game"),
                    );
                    if !self.display_name.trim().is_empty() {
                        ui.label(
                            RichText::new(format!("id: {}", name_id_for(&self.display_name)))
                                .small()
                                .weak(),
                        );
                    }
                });

                ui.add_space(10.0);
                let ready = self.donor.is_some() && !self.display_name.trim().is_empty();
                let slot_taken = ready && self.slot_conflict(roots, index).is_some();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(RichText::new("＋ Create character…").strong()),
                        )
                        .on_hover_text("Pick where this character's files go")
                        .clicked()
                    {
                        pending = CreatorAsk::PickFolder;
                    }
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(RichText::new("Quick create").small()),
                        )
                        .on_hover_text(
                            "No folder picker — files go to Visionary's authored folder and the character imports immediately",
                        )
                        .clicked()
                    {
                        pending = CreatorAsk::Quick;
                    }
                    if !ready {
                        ui.label(RichText::new("Choose a donor and name.").small().weak());
                    } else if slot_taken {
                        ui.colored_label(Color32::from_rgb(240, 200, 120), RichText::new("Slot already used — pick a free one.").small());
                    }
                });
            });

        pending
    }

    /// Turn the form into a real action, asking for the destination folder.
    fn build_create_action(&mut self) -> NewCharacterAction {
        let (Some(donor), Some(slot)) = (self.donor.clone(), self.slot) else {
            return NewCharacterAction::None;
        };
        let display_name = self.display_name.trim().to_string();
        if self.display_name.trim().is_empty() {
            return NewCharacterAction::None;
        }
        let Some(destination) = rfd::FileDialog::new()
            .set_title("Where should this character's files go?")
            .pick_folder()
        else {
            return NewCharacterAction::None;
        };
        NewCharacterAction::Create {
            name_id: name_id_for(&display_name),
            donor,
            slot,
            display_name,
            destination,
        }
    }

    /// Turn the form into a real action without a folder picker: files go to
    /// Visionary's authored folder. Re-creating a character already in the
    /// project would import the same files twice, so that is refused up front.
    fn build_quick_create_action(&mut self, project: &RosterMod) -> NewCharacterAction {
        let (Some(donor), Some(slot)) = (self.donor.clone(), self.slot) else {
            return NewCharacterAction::None;
        };
        let display_name = self.display_name.trim().to_string();
        if display_name.is_empty() {
            return NewCharacterAction::None;
        }
        if project
            .authored
            .iter()
            .any(|entry| entry.donor == donor.to_ascii_lowercase() && entry.slot == slot)
        {
            self.error = Some(format!(
                "{display_name} is already in the project as c{slot:02} of {donor} — pick a free slot."
            ));
            return NewCharacterAction::None;
        }
        let base = crate::scratch_dirs::app_storage_root().join("authored");
        if let Err(error) = std::fs::create_dir_all(&base) {
            self.error = Some(format!("Could not make the authored folder: {error:#}"));
            return NewCharacterAction::None;
        }
        NewCharacterAction::Create {
            name_id: name_id_for(&display_name),
            donor,
            slot,
            display_name: display_name.clone(),
            destination: base.join(crate::mod_export::slugify(&display_name)),
        }
    }

    fn slot_conflict(&self, roots: &[PathBuf], index: &RosterIndex) -> Option<String> {
        let donor = self.donor.as_ref()?;
        let slot = self.slot?;
        if let Some(entry) = index.entries.iter().find(|entry| {
            entry.fighter.as_deref() == Some(donor.as_str()) && entry.backing.slot() == Some(slot)
        }) {
            return Some(format!("c{slot:02} used by {}", entry.display_name));
        }
        let occupied = crate::data::discover_costume_slots(roots, donor);
        if occupied.contains(&slot) {
            return Some(format!("c{slot:02} already exists for {donor}"));
        }
        None
    }

    fn draw_existing(
        &mut self,
        ui: &mut Ui,
        roots: &[PathBuf],
        index: &RosterIndex,
        project: &RosterMod,
        edit_target: Option<&RosterKey>,
        labels: &std::collections::HashMap<u64, String>,
        authored_moves: &BTreeMap<RosterKey, std::collections::BTreeSet<String>>,
    ) -> ExistingAction {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Step 2 — Finish them: files, moves, roster").strong());
            ui.label(
                RichText::new(format!("({})", project.authored.len()))
                    .small()
                    .weak(),
            );
        });
        ui.add_space(6.0);
        if project.authored.is_empty() {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("◇  A blank stage").strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Dream one up in Step 1 — donor, slot, name — and it lands here as a costume you finish: files, moves, look, ship.")
                                .small()
                                .weak(),
                        );
                    });
                });
            return ExistingAction::None;
        }

        let mut result = ExistingAction::None;
        for authored in &project.authored {
            let has_name = project.names.contains_key(&authored.key);
            let replaced = authored_moves
                .get(&authored.key)
                .cloned()
                .unwrap_or_default();
            let mut readiness = scaffold::measure(roots, &authored.donor, authored.slot, has_name);
            readiness.authored_moves = replaced.len();
            readiness.registered_moves = replaced.len();

            // Theme-following card — no hard-coded dark fills, so light and
            // high-contrast themes read it the same way.
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("◈  {}", authored.display_name)).strong());
                        ui.label(
                            RichText::new(format!(
                                "c{:02} of {} · {}",
                                authored.slot, authored.donor, authored.name_id
                            ))
                            .small()
                            .weak(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(RichText::new(" ✕ Remove ").small())
                                .on_hover_text("Remove this character from the project. Its files are left on disk.")
                                .clicked()
                            {
                                result = ExistingAction::Remove(authored.key.clone());
                            }
                            if readiness.is_ready() {
                                ui.colored_label(Color32::from_rgb(130, 225, 150), RichText::new("Ready").small().strong());
                            } else {
                                ui.colored_label(Color32::from_rgb(240, 200, 120), RichText::new("Needs work").small().strong());
                            }
                        });
                    });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // ── Stage strip: where this character stands ─────────
                    // Files → Moveset → Look → Ship. The strip answers "what
                    // next" at a glance; each unfinished stage points at the
                    // place that finishes it.
                    let inv = scaffold::inventory(
                        roots,
                        &authored.donor,
                        authored.slot,
                        Some(authored.name_id.as_str()),
                    );
                    let files_done =
                        inv.meshes > 0 && inv.has_motion_list && has_name;
                    let remaining_starting =
                        readiness.remaining_starting_moves(&replaced);
                    let moves_done_count = scaffold::MOVESET_TEMPLATE.len()
                        - remaining_starting.len();
                    let moves_done = remaining_starting.is_empty();
                    let has_portrait = project
                        .ui_images
                        .get(&authored.key)
                        .is_some_and(|m| !m.is_empty());
                    let look_done = has_name && has_portrait;
                    self.draw_stages(
                        ui,
                        files_done,
                        moves_done_count,
                        moves_done,
                        look_done,
                        readiness.is_ready(),
                        &mut result,
                        authored,
                    );

                    ui.add_space(6.0);

                    let targeted = edit_target == Some(&authored.key);
                    egui::Frame::group(ui.style())
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let mut on = targeted;
                                if ui
                                    .checkbox(&mut on, RichText::new("Edit this character's moves").small().strong())
                                    .changed()
                                {
                                    result = ExistingAction::Target(on.then(|| authored.key.clone()));
                                }
                                if targeted {
                                    ui.colored_label(Color32::from_rgb(130, 225, 150), RichText::new("● active").small().strong());
                                }
                            });
                            if targeted {
                                ui.label(
                                    RichText::new(format!("Edits affect c{:02} only.", authored.slot))
                                        .small()
                                        .weak(),
                                );
                            } else {
                                ui.label(
                                    RichText::new("Scope edits to this costume.")
                                        .small()
                                        .weak(),
                                );
                            }
                        });

                    ui.add_space(6.0);

                    // ── Files: everything this character is made of ──────
                    egui::CollapsingHeader::new(RichText::new("Files").small())
                        .default_open(!files_done)
                        .id_salt(format!("files_{}", authored.key.as_str()))
                        .show(ui, |ui| {
                            self.draw_files(ui, authored, project, &inv);
                        });

                    ui.add_space(6.0);

                    let outstanding = readiness.outstanding();
                    if readiness.is_ready() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("✓").small().color(Color32::from_rgb(130,225,150)));
                            ui.label(
                                RichText::new("Ready — model, animations, and name are all in place.")
                                    .small()
                                    .color(Color32::from_rgb(170, 210, 180)),
                            );
                        });
                    } else {
                        for note in outstanding {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("•").small().color(Color32::from_rgb(240,200,120)));
                                ui.label(RichText::new(note).small().color(Color32::from_rgb(220,190,150)));
                            });
                        }
                    }
                    ui.add_space(4.0);
                    let remaining = readiness.remaining_starting_moves(&replaced);
                    let done = scaffold::MOVESET_TEMPLATE.len() - remaining.len();
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{done}/{} starting moves replaced", scaffold::MOVESET_TEMPLATE.len()))
                                .small()
                                .strong(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(
                                egui::ProgressBar::new(done as f32 / scaffold::MOVESET_TEMPLATE.len() as f32)
                                    .desired_width(80.0),
                            );
                        });
                    });
                    ui.label(
                        RichText::new(format!(
                            "The rest still play {}'s version — normal until you change them.",
                            authored.donor
                        ))
                        .small()
                        .weak(),
                    );
                    if !remaining.is_empty() {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!("Still donor's: {}", remaining.join(", ")))
                                .small()
                                .weak(),
                        );
                    }

                    if let Some(binding) = Self::animation_binding(roots, authored, labels) {
                        ui.add_space(4.0);
                        ui.label(RichText::new(binding.summary()).small().weak());
                        if !binding.unreferenced_files.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "{} animation file(s) that no motion list names, so they will \
                                     never play: {}",
                                    binding.unreferenced_files.len(),
                                    binding.unreferenced_files.join(", ")
                                ))
                                .small()
                                .weak(),
                            );
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "{} model file(s)  ·  {} animation(s){}",
                            readiness.model_files,
                            readiness.motion_files,
                            if readiness.has_effect { "  ·  own effects" } else { "" }
                        ))
                        .small()
                        .weak(),
                    );
                    if index.by_key(&authored.key).is_none() {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.colored_label(Color32::from_rgb(240,140,140), "⚠");
                            ui.label(
                                RichText::new(
                                    "Its donor fighter is not in the current data root, so it cannot be \
                                     edited right now.",
                                )
                                .small()
                                .weak(),
                            );
                        });
                    }
                });
            ui.add_space(8.0);
        }
        result
    }

    /// The stage strip: Files → Moves → Look → Ship, plus the single next
    /// action. Answers "what do I do now" without reading the whole card.
    #[allow(clippy::too_many_arguments)]
    fn draw_stages(
        &mut self,
        ui: &mut Ui,
        files_done: bool,
        moves_done_count: usize,
        moves_done: bool,
        look_done: bool,
        ready: bool,
        result: &mut ExistingAction,
        authored: &crate::mod_project::AuthoredEntry,
    ) {
        fn chip(ui: &mut Ui, label: String, done: bool) {
            ui.label(
                RichText::new(label).small().strong().color(if done {
                    Color32::from_rgb(130, 225, 150)
                } else {
                    ui.visuals().weak_text_color()
                }),
            );
        }
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    chip(
                        ui,
                        format!("{} Files", if files_done { "✓" } else { "○" }),
                        files_done,
                    );
                    ui.label(RichText::new("→").small().weak());
                    chip(
                        ui,
                        format!(
                            "{} Moves {moves_done_count}/{}",
                            if moves_done { "✓" } else { "○" },
                            scaffold::MOVESET_TEMPLATE.len()
                        ),
                        moves_done,
                    );
                    ui.label(RichText::new("→").small().weak());
                    chip(ui, format!("{} Look", if look_done { "✓" } else { "○" }), look_done);
                    ui.label(RichText::new("→").small().weak());
                    chip(ui, format!("{} Ship", if ready { "✓" } else { "○" }), ready);
                });
                ui.add_space(2.0);
                if !files_done {
                    ui.label(
                        RichText::new("Next: drop the model and animations into Files below.")
                            .small()
                            .weak(),
                    );
                } else if !look_done {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("Next: name and portrait live in Character Select.")
                                .small()
                                .weak(),
                        );
                        if ui
                            .small_button(RichText::new("Finish look →").small())
                            .clicked()
                        {
                            *result = ExistingAction::GotoCss(authored.key.clone());
                        }
                    });
                } else if !moves_done {
                    ui.label(
                        RichText::new(
                            "Next: tick “Edit this character's moves”, then replace moves in the main editor.",
                        )
                        .small()
                        .weak(),
                    );
                } else if ready {
                    ui.label(
                        RichText::new("Ready to ship ✓ — Mod → Export Mod Folder.")
                            .small()
                            .strong()
                            .color(Color32::from_rgb(130, 225, 150)),
                    );
                }
            });
    }

    /// The files this character is made of: what is there, what is missing,
    /// and a button to open each one. Folders come from the scaffold root
    /// this project recorded, falling back to wherever the files were found.
    fn draw_files(
        &mut self,
        ui: &mut Ui,
        authored: &crate::mod_project::AuthoredEntry,
        project: &RosterMod,
        inv: &scaffold::SlotInventory,
    ) {
        ui.label(
            RichText::new("Fill these in, in any order — the checklist above tracks what is left.")
                .small()
                .weak(),
        );
        ui.add_space(4.0);
        let model_target = authored
            .files_root
            .as_ref()
            .map(|root| {
                root.join(format!(
                    "fighter/{}/model/body/c{:02}",
                    authored.donor, authored.slot
                ))
            })
            .or_else(|| inv.model_dir.clone());
        let motion_target = authored
            .files_root
            .as_ref()
            .map(|root| {
                root.join(format!(
                    "fighter/{}/motion/body/c{:02}",
                    authored.donor, authored.slot
                ))
            })
            .or_else(|| inv.motion_dir.clone());
        let effect_target = authored.files_root.as_ref().map(|root| {
            root.join(scaffold::effect_file(&authored.donor, authored.slot))
        });
        let portrait_png = project
            .ui_images
            .get(&authored.key)
            .and_then(|map| map.values().next())
            .map(|ov| PathBuf::from(&ov.png_path));

        let mut reveal_ask: Option<PathBuf> = None;
        let model_state = if inv.meshes > 0 {
            format!("{} mesh, {} texture(s)", inv.meshes, inv.textures)
        } else {
            "empty — needs .numdlb + .nutexb".to_string()
        };
        if let Some(path) =
            file_row(ui, "Model", &model_state, inv.meshes > 0, model_target)
        {
            reveal_ask = Some(path);
        }
        let anim_state = if inv.anims > 0 {
            format!(
                "{} animation(s){}",
                inv.anims,
                if inv.has_motion_list { "" } else {
                    " — NO motion_list.bin"
                }
            )
        } else {
            "empty — needs .nuanmb + motion_list.bin".to_string()
        };
        if let Some(path) = file_row(
            ui,
            "Animations",
            &anim_state,
            inv.anims > 0 && inv.has_motion_list,
            motion_target,
        ) {
            reveal_ask = Some(path);
        }
        if let Some(path) = file_row(
            ui,
            "Effect",
            if inv.has_effect {
                "own slot effect present"
            } else {
                "optional — plays the donor's without one"
            },
            inv.has_effect,
            effect_target,
        ) {
            reveal_ask = Some(path);
        }
        if let Some(path) = file_row(
            ui,
            "Portrait",
            if portrait_png.is_some() {
                "picked — see Character Select → Images"
            } else {
                "optional — pick one in Character Select → Images"
            },
            portrait_png.is_some() || inv.has_portrait,
            portrait_png,
        ) {
            reveal_ask = Some(path);
        }
        if let Some(path) = reveal_ask {
            match super::reveal::reveal(&path) {
                Ok(()) => self.status = format!("Opened {}.", path.display()),
                Err(error) => self.status = format!("Could not open files: {error:#}"),
            }
        }
    }

    /// How this character's animation files line up with the donor's motion list.
    fn animation_binding(
        roots: &[PathBuf],
        authored: &crate::mod_project::AuthoredEntry,
        labels: &std::collections::HashMap<u64, String>,
    ) -> Option<scaffold::AnimationBinding> {
        let relative = format!("fighter/{}/motion/body", authored.donor);
        let list = roots
            .iter()
            .map(|root| root.join(&relative).join("motion_list.bin"))
            .find(|path| path.is_file())?;
        let slot_dir = roots
            .iter()
            .map(|root| root.join(format!("{relative}/c{:02}", authored.slot)))
            .find(|path| path.is_dir())?;
        scaffold::bind_animations(&list, &slot_dir, labels).ok()
    }

    pub fn note_created(&mut self, display_name: &str, scaffolded: &scaffold::Scaffold) {
        self.error = None;
        self.display_name.clear();
        self.donor = None;
        self.slot = None;
        self.status = format!(
            "Created {display_name} as costume c{:02} of {}: {} folder(s), added to the mod \
             library. Put your model and animations in the folders it made, then use the main \
             editor to build its moves.",
            scaffolded.slot,
            scaffolded.donor,
            scaffolded.created.len()
        );
    }

    pub fn note_error(&mut self, error: String) {
        self.error = Some(error);
    }
}

/// One row of the Files checklist: check or hollow dot, what it is, its
/// state, and an Open button for its folder. Returns the path to reveal
/// when Open is pressed, so the caller owns the status message.
fn file_row(
    ui: &mut Ui,
    name: &str,
    detail: &str,
    done: bool,
    target: Option<PathBuf>,
) -> Option<PathBuf> {
    let mut ask = None;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(if done { "✓" } else { "○" })
                .small()
                .strong()
                .color(if done {
                    Color32::from_rgb(130, 225, 150)
                } else {
                    ui.visuals().weak_text_color()
                }),
        );
        ui.label(RichText::new(name).small().strong());
        ui.label(RichText::new(detail).small().weak());
        if let Some(path) = target {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(RichText::new("Open").small())
                    .on_hover_text(format!("Show {} in the file manager", path.display()))
                    .clicked()
                {
                    ask = Some(path);
                }
            });
        }
    });
    ask
}

/// The internal id a character gets, derived from its display name.
pub fn name_id_for(display_name: &str) -> String {
    let mut out = String::new();
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let trimmed = out.trim_end_matches('_').to_string();
    if trimmed.is_empty() {
        "new_character".to_string()
    } else {
        trimmed
    }
}

/// Build the project record for a newly created character.
pub fn authored_entry(donor: &str, slot: u8, display_name: &str, name_id: &str) -> AuthoredEntry {
    AuthoredEntry {
        key: RosterKey::slot(donor, slot),
        donor: donor.to_ascii_lowercase(),
        slot,
        display_name: display_name.to_string(),
        name_id: name_id.to_string(),
        moveset_scaffolded: false,
        // Set by the window when it creates the scaffold; the helper alone
        // knows no destination.
        files_root: None,
    }
}

/// Create the files and register the result as a mod, so the new slot is immediately editable.
pub fn create_and_import(
    library: &mut ModLibrary,
    destination: &std::path::Path,
    donor: &str,
    slot: u8,
    display_name: &str,
) -> anyhow::Result<scaffold::Scaffold> {
    let root = destination.join(crate::mod_export::slugify(display_name));
    let scaffolded = scaffold::create(&root, donor, slot)?;
    library.import_directory(ModSource::Folder(root), Some(display_name.to_string()))?;
    Ok(scaffolded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_display_name_becomes_a_safe_stable_id() {
        assert_eq!(name_id_for("Vision"), "vision");
        assert_eq!(name_id_for("Dark Samus"), "dark_samus");
        assert_eq!(name_id_for("R.O.B. 64!"), "r_o_b_64");
        assert_eq!(name_id_for("  spaced  out  "), "spaced_out");
        assert_eq!(name_id_for("???"), "new_character");
        assert_eq!(name_id_for("Vision"), name_id_for("Vision"));
    }

    #[test]
    fn an_authored_entry_is_keyed_by_its_donor_and_slot() {
        let entry = authored_entry("Mario", 8, "Vision", "vision");
        assert_eq!(entry.key, RosterKey::slot("mario", 8));
        assert_eq!(entry.donor, "mario");
        assert_eq!(entry.name_id, "vision");
        assert!(!entry.moveset_scaffolded);
    }

    /// Quick-creating the same donor slot twice would import the same files
    /// as a second mod, so the form refuses it before anything is written.
    #[test]
    fn quick_create_refuses_a_character_already_in_the_project() {
        let mut view = NewCharacterView::default();
        view.donor = Some("mario".into());
        view.slot = Some(8);
        view.display_name = "Vision".into();
        let mut project = RosterMod::default();
        project
            .authored
            .push(authored_entry("mario", 8, "Vision", "vision"));
        // The guard returns before the authored folder is touched, so this
        // test writes nothing outside tempdirs.
        assert!(matches!(
            view.build_quick_create_action(&project),
            NewCharacterAction::None
        ));
        assert!(view.error.is_some());
    }

    #[test]
    fn creating_a_character_also_makes_it_visible_to_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ModLibrary::default();
        let scaffolded = create_and_import(&mut library, dir.path(), "mario", 8, "Vision").unwrap();

        assert_eq!(scaffolded.slot, 8);
        assert_eq!(library.mods.len(), 1);
        let imported = &library.mods[0];
        assert_eq!(imported.name, "Vision");
        assert!(imported.enabled);
        let provision = &imported.manifest.fighters["mario"];
        assert!(
            provision.slots.contains(&8),
            "the new slot was not visible in the imported mod's manifest"
        );
    }

    #[test]
    fn on_the_fly_creation_is_properly_scoped_per_slot() {
        let mut library = ModLibrary::default();
        let dir = tempfile::tempdir().unwrap();
        let a =
            create_and_import(&mut library, &dir.path().join("a"), "mario", 8, "Vision").unwrap();
        let b =
            create_and_import(&mut library, &dir.path().join("b"), "mario", 9, "Vision2").unwrap();
        assert_eq!(a.slot, 8);
        assert_eq!(b.slot, 9);
        assert_ne!(
            RosterKey::slot("mario", 8),
            RosterKey::slot("mario", 9),
            "slot keys must not collide"
        );
        assert_eq!(library.mods.len(), 2);
        assert!(library.mods[0].manifest.fighters["mario"]
            .slots
            .contains(&8));
        assert!(!library.mods[0].manifest.fighters["mario"]
            .slots
            .contains(&9));
        assert!(library.mods[1].manifest.fighters["mario"]
            .slots
            .contains(&9));
        let k8 = RosterKey::slot("mario", 8);
        let k9 = RosterKey::slot("mario", 9);
        assert_ne!(k8, k9);
    }

    #[test]
    fn quick_create_uses_authored_cache_without_picker() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ModLibrary::default();
        let root = dir.path().join("authored").join("vision");
        let s = crate::roster::scaffold::create(&root, "mario", 12).unwrap();
        library
            .import_directory(ModSource::Folder(root), Some("Vision".into()))
            .unwrap();
        assert_eq!(s.slot, 12);
        assert_eq!(s.donor, "mario");
        assert!(library.mods[0].manifest.fighters["mario"]
            .slots
            .contains(&12));
    }
}
