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

/// Minimum number of skins a new character needs. Slot-pack mods ship in
/// multiples of the vanilla count, and a character below this number reads as
/// "the same donor with one skin" — which is what they would be in-game.
const MIN_SKINS: u8 = 8;

/// Largest block the Skins stepper offers. Past this a character is really a
/// slot pack, and the stepper becomes fiddly; the slot map still allows any
/// start so odd sizes stay reachable via a smaller block plus manual growth.
const MAX_SKINS: u8 = 32;

/// Inclusive `[start, end]` slot range. A range of `(8, 15)` means 8 skins on
/// c08..=c15. The form keeps both endpoints and rebuilds the inclusive vec
/// when it needs to scaffold or report conflicts; storing a vec would let
/// them drift, and a hole-bearing set would need the same validation.
type SlotRange = (u8, u8);

fn range_slots(range: SlotRange) -> Vec<u8> {
    (range.0..=range.1).collect()
}

fn range_count(range: SlotRange) -> u8 {
    range.1.saturating_sub(range.0).saturating_add(1)
}

#[derive(Default)]
pub struct NewCharacterView {
    donor: Option<String>,
    /// Inclusive `[start, end]` slot range. `None` until the donor is picked,
    /// at which point it is initialised to the lowest free run of [`MIN_SKINS`]
    /// slots so a new donor never opens with a range that is already taken.
    slot_range: Option<SlotRange>,
    /// Whether the slot map (the full chip grid) is expanded. Collapsed by
    /// default so the form reads as one line plus one status line; the map
    /// is there when the auto-picked block is not where the user wants it.
    show_slot_map: bool,
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
        /// Every costume slot this character owns, ascending and deduplicated.
        slots: Vec<u8>,
        display_name: String,
        name_id: String,
        destination: PathBuf,
    },
    Remove(RosterKey),
}

impl NewCharacterView {
    // Traversal bundle (ui, roots, index, project, target, labels, moves) —
    // same shape as every roster draw_*, allowed like `draw_stages` below.
    #[allow(clippy::too_many_arguments)]
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

        let creator = self.draw_creator(ui, roots, index);
        match creator {
            CreatorAsk::None => {}
            CreatorAsk::PickFolder => action = self.build_create_action(),
            CreatorAsk::Quick => action = self.build_quick_create_action(project),
        }
        ui.add_space(8.0);
        match self.draw_existing(
            ui,
            roots,
            index,
            project,
            edit_target,
            labels,
            authored_moves,
        ) {
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
                ui.heading("＋ New character");
                ui.add_space(2.0);
                ui.label(
                    RichText::new("A costume on a donor fighter, with its own model, moves, and name.")
                        .small()
                        .weak(),
                );
                ui.add_space(8.0);

                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Based on").small().strong());
                    ui.add_space(8.0);
                    // Flex with the window instead of forcing a 220px box.
                    let combo_w = (ui.available_width() - 110.0).clamp(140.0, 220.0);
                    let previous_donor = self.donor.clone();
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
                                }
                            }
                        });
                    if self.donor != previous_donor {
                        // Donor changed — pick a fresh range so the form never
                        // opens with a range that crosses a taken slot. Falls
                        // back to the conventional c08–c15 pack (which then
                        // shows its conflicts) when no clear run exists.
                        self.slot_range = self.donor.as_deref().map(|d| {
                            lowest_free_run(roots, index, d, MIN_SKINS).unwrap_or((8, 15))
                        });
                        self.show_slot_map = false;
                    }
                });
                ui.add_space(6.0);

                self.draw_slot_range(ui, roots, index);
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
                let range_ok = self.slot_range.map(range_count).unwrap_or(0) >= MIN_SKINS;
                let slot_taken = ready && self.slot_range_conflict(roots, index).is_some();
                let can_create = ready && range_ok && !slot_taken;
                ui.horizontal_wrapped(|ui| {
                    // One primary action: most users want the character made,
                    // not a destination decision. Quick create imports
                    // immediately; the folder picker is the rare override.
                    if ui
                        .add_enabled(
                            can_create,
                            egui::Button::new(RichText::new("＋ Create character").strong()),
                        )
                        .on_hover_text(
                            "Files go to Visionary's authored folder and the character imports immediately",
                        )
                        .clicked()
                    {
                        pending = CreatorAsk::Quick;
                    }
                    if ui
                        .add_enabled(
                            can_create,
                            egui::Button::new(RichText::new("Custom folder…").small()),
                        )
                        .on_hover_text("Pick where this character's files go")
                        .clicked()
                    {
                        pending = CreatorAsk::PickFolder;
                    }
                    // The slot row above already details range problems; here
                    // is only the one-line reason the button is grey.
                    if !ready {
                        ui.label(RichText::new("Pick a donor and a name.").small().weak());
                    } else if !range_ok || slot_taken {
                        ui.colored_label(
                            Color32::from_rgb(240, 200, 120),
                            RichText::new("Fix the slots above.").small(),
                        );
                    }
                });
            });

        pending
    }

    /// The slot picker, deliberately quiet: one control row (first slot +
    /// skin count), one status line, and a "next free block" escape hatch.
    /// The full slot map lives behind a collapsed toggle so a donor with a
    /// busy slot table does not push the name field and the create buttons
    /// off-screen. There is a single mental model — a block starting at
    /// `First` holding `Skins` costumes — so an inverted range is
    /// unrepresentable and the 8-skin minimum is just the stepper's floor.
    fn draw_slot_range(&mut self, ui: &mut Ui, roots: &[PathBuf], index: &RosterIndex) {
        let Some(donor) = self.donor.clone() else {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Costume slots").small().strong());
                ui.label(
                    RichText::new("Pick a fighter first — slots appear after.")
                        .small()
                        .weak(),
                );
            });
            return;
        };
        // One filesystem scan per frame; the conflict list below reuses it
        // rather than rescanning.
        let statuses = slot_statuses(roots, index, &donor);
        let mut range = clamp_range(self.slot_range.unwrap_or((8, 15)));

        // ── Row 1: the only controls ──────────────────────────────
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Costume slots").small().strong());
            ui.add_space(8.0);
            let mut start = range.0;
            let mut count = range_count(range).max(MIN_SKINS);
            let start_resp = ui
                .add(
                    egui::DragValue::new(&mut start)
                        .range(0..=255)
                        .prefix("c")
                        .speed(1),
                )
                .on_hover_text("First costume slot — c00–c07 are the donor's own");
            ui.label(RichText::new("×").small().weak());
            let count_resp = ui
                .add(
                    egui::DragValue::new(&mut count)
                        .range(MIN_SKINS..=MAX_SKINS)
                        .suffix(" skins")
                        .speed(1),
                )
                .on_hover_text(format!("How many costumes — at least {MIN_SKINS}"));
            if start_resp.changed() || count_resp.changed() {
                // saturating_add caps at 255 by type; end >= start always.
                let end = start.saturating_add(count.saturating_sub(1));
                range = (start, end.max(start));
            }
            self.slot_range = Some(range);
        });

        // ── Row 2: the single status line ─────────────────────────
        // One line covers the three states (too small / taken / free) so the
        // eye has one place to check instead of three badges to reconcile.
        let taken = conflicts_in_range(&statuses, range);
        let actual = range_count(range);
        if actual < MIN_SKINS {
            ui.colored_label(
                Color32::from_rgb(240, 200, 120),
                RichText::new(format!(
                    "No room for {MIN_SKINS} skins from c{:02} — pick a lower first slot.",
                    range.0
                ))
                .small(),
            );
        } else if !taken.is_empty() {
            ui.colored_label(
                Color32::from_rgb(240, 200, 120),
                RichText::new(format!("Taken: {}", summarize_taken(&taken))).small(),
            );
        } else {
            ui.colored_label(
                Color32::from_rgb(130, 225, 150),
                RichText::new(format!("✓ c{:02}…c{:02} all free", range.0, range.1)).small(),
            );
        }

        // ── Row 3: escape hatches ─────────────────────────────────
        ui.horizontal_wrapped(|ui| {
            let next = next_free_run_after(&statuses, actual.max(MIN_SKINS), range.0);
            let next_btn = ui
                .add_enabled(
                    next.is_some(),
                    egui::Button::new(RichText::new("Next free block →").small()),
                )
                .on_hover_text(if next.is_some() {
                    "Jump to the next clear block of this size"
                } else {
                    "No clear block of this size anywhere"
                });
            if next_btn.clicked() {
                if let Some(found) = next {
                    self.slot_range = Some(found);
                }
            }
            let map_open = self.show_slot_map;
            ui.toggle_value(
                &mut self.show_slot_map,
                RichText::new(if map_open {
                    "Slot map ▾"
                } else {
                    "Slot map ▸"
                })
                .small(),
            )
            .on_hover_text("Show every slot — grey means taken");
        });

        // ── The map, only on request ──────────────────────────────
        if self.show_slot_map {
            ui.add_space(4.0);
            self.draw_slot_map(ui, &statuses, range);
        }
    }

    /// The full slot grid, shown only while the map toggle is open. Rows of
    /// 8 so packs read as packs; two visual states only (white = free, grey
    /// = taken) with the reason on hover, so there is no legend to learn.
    /// Clicking a white slot moves the whole block there, keeping the skin
    /// count — one click, no modifier keys.
    fn draw_slot_map(&mut self, ui: &mut Ui, statuses: &[SlotStatus], range: SlotRange) {
        let count = range_count(range).max(MIN_SKINS);
        // Cover the block plus one pack of headroom, capped so a pathological
        // slot table cannot turn the form into an endless scroll.
        let map_end = range.1.saturating_add(8).clamp(31, 63) as usize;
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.style_mut().spacing.item_spacing = egui::vec2(4.0, 4.0);
                for row in statuses
                    .iter()
                    .take(map_end + 1)
                    .enumerate()
                    .collect::<Vec<_>>()
                    .chunks(8)
                {
                    ui.horizontal(|ui| {
                        for (i, status) in row {
                            let slot = *i as u8;
                            let in_range = slot >= range.0 && slot <= range.1;
                            slot_chip(ui, slot, status, in_range, count, &mut self.slot_range);
                        }
                    });
                }
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Click a white slot to move your block there.")
                        .small()
                        .weak(),
                );
            });
    }

    /// Turn the form into a real action, asking for the destination folder.
    fn build_create_action(&mut self) -> NewCharacterAction {
        let (Some(donor), Some(range)) = (self.donor.clone(), self.slot_range) else {
            return NewCharacterAction::None;
        };
        let display_name = self.display_name.trim().to_string();
        if self.display_name.trim().is_empty() {
            return NewCharacterAction::None;
        }
        let slots = range_slots(range);
        let Some(destination) = rfd::FileDialog::new()
            .set_title("Where should this character's files go?")
            .pick_folder()
        else {
            return NewCharacterAction::None;
        };
        NewCharacterAction::Create {
            name_id: name_id_for(&display_name),
            donor,
            slots,
            display_name,
            destination,
        }
    }

    /// Turn the form into a real action without a folder picker: files go to
    /// Visionary's authored folder. Re-creating a character already in the
    /// project would import the same files twice, so that is refused up front.
    fn build_quick_create_action(&mut self, project: &RosterMod) -> NewCharacterAction {
        let (Some(donor), Some(range)) = (self.donor.clone(), self.slot_range) else {
            return NewCharacterAction::None;
        };
        let display_name = self.display_name.trim().to_string();
        if display_name.is_empty() {
            return NewCharacterAction::None;
        }
        let slots = range_slots(range);
        let donor_lc = donor.to_ascii_lowercase();
        if project.authored.iter().any(|entry| {
            entry.donor == donor_lc
                && (entry.slot == range.0
                    || entry.slot == range.1
                    || entry.slots.iter().any(|s| *s >= range.0 && *s <= range.1))
        }) {
            self.error = Some(format!(
                "{display_name} overlaps a character already in the project on {donor} — pick a free range."
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
            slots,
            display_name: display_name.clone(),
            destination: base.join(crate::mod_export::slugify(&display_name)),
        }
    }

    /// Every slot in the form's range that is already taken, with the name of
    /// what owns it. The status line shows the summary; the create buttons
    /// stay disabled until this is empty.
    fn slot_range_conflict(
        &self,
        roots: &[PathBuf],
        index: &RosterIndex,
    ) -> Option<Vec<(u8, String)>> {
        let donor = self.donor.as_ref()?;
        let range = self.slot_range?;
        let statuses = slot_statuses(roots, index, donor);
        let out = conflicts_in_range(&statuses, range);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    // Same traversal bundle as `ui` above.
    #[allow(clippy::too_many_arguments)]
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
            ui.label(RichText::new("Your characters").strong());
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
                        ui.label(RichText::new("◇  No characters yet").strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Create one above — it lands here with its next step.")
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

                    // One row: tick to scope move edits to this costume, untick
                    // for fighter-wide. The trailing note names the scope so no
                    // second explanation line is needed.
                    let targeted = edit_target == Some(&authored.key);
                    ui.horizontal_wrapped(|ui| {
                        let mut on = targeted;
                        if ui
                            .checkbox(&mut on, RichText::new("Edit moves").small().strong())
                            .on_hover_text(format!(
                                "Scope move edits to c{:02} of {} instead of the whole fighter",
                                authored.slot, authored.donor,
                            ))
                            .changed()
                        {
                            result = ExistingAction::Target(on.then(|| authored.key.clone()));
                        }
                        if targeted {
                            ui.colored_label(
                                Color32::from_rgb(130, 225, 150),
                                RichText::new(format!("● c{:02} only", authored.slot)).small().strong(),
                            );
                        } else {
                            ui.label(RichText::new("fighter-wide").small().weak());
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

                    // ── Details: the diagnostics behind the strip ──────
                    // The stage strip above already answers "what next" and the
                    // Files checklist covers the folders; everything below is
                    // the fine print, collapsed so a finished character reads
                    // as one short card instead of a wall of checklists.
                    egui::CollapsingHeader::new(RichText::new("Details").small())
                        .default_open(false)
                        .id_salt(format!("details_{}", authored.key.as_str()))
                        .show(ui, |ui| {
                            self.draw_details(ui, roots, index, authored, labels, &readiness, &replaced);
                        });
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
            ui.label(RichText::new(label).small().strong().color(if done {
                Color32::from_rgb(130, 225, 150)
            } else {
                ui.visuals().weak_text_color()
            }));
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
        let effect_target = authored
            .files_root
            .as_ref()
            .map(|root| root.join(scaffold::effect_file(&authored.donor, authored.slot)));
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
        if let Some(path) = file_row(ui, "Model", &model_state, inv.meshes > 0, model_target) {
            reveal_ask = Some(path);
        }
        let anim_state = if inv.anims > 0 {
            format!(
                "{} animation(s){}",
                inv.anims,
                if inv.has_motion_list {
                    ""
                } else {
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

    /// The fine print behind one character's stage strip: readiness notes,
    /// moveset progress, animation binding, and file counts. Collapsed by
    /// default — nothing here gates creating or editing, it only diagnoses.
    #[allow(clippy::too_many_arguments)]
    fn draw_details(
        &self,
        ui: &mut Ui,
        roots: &[PathBuf],
        index: &RosterIndex,
        authored: &crate::mod_project::AuthoredEntry,
        labels: &std::collections::HashMap<u64, String>,
        readiness: &scaffold::Readiness,
        replaced: &std::collections::BTreeSet<String>,
    ) {
        let outstanding = readiness.outstanding();
        if readiness.is_ready() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("✓")
                        .small()
                        .color(Color32::from_rgb(130, 225, 150)),
                );
                ui.label(
                    RichText::new("Ready — model, animations, and name are all in place.")
                        .small()
                        .color(Color32::from_rgb(170, 210, 180)),
                );
            });
        } else {
            for note in outstanding {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("•")
                            .small()
                            .color(Color32::from_rgb(240, 200, 120)),
                    );
                    ui.label(
                        RichText::new(note)
                            .small()
                            .color(Color32::from_rgb(220, 190, 150)),
                    );
                });
            }
        }
        ui.add_space(4.0);
        let remaining = readiness.remaining_starting_moves(replaced);
        let done = scaffold::MOVESET_TEMPLATE.len() - remaining.len();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{done}/{} starting moves replaced",
                    scaffold::MOVESET_TEMPLATE.len()
                ))
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
                if readiness.has_effect {
                    "  ·  own effects"
                } else {
                    ""
                }
            ))
            .small()
            .weak(),
        );
        if index.by_key(&authored.key).is_none() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(240, 140, 140), "⚠");
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
        self.slot_range = None;
        let first = scaffolded.slots.first().copied().unwrap_or(scaffolded.slot);
        let last = scaffolded.slots.last().copied().unwrap_or(scaffolded.slot);
        let span = if first == last {
            format!("c{first:02}")
        } else {
            format!("c{first:02}…c{last:02} ({} skins)", scaffolded.slots.len())
        };
        self.status = format!(
            "Created {display_name} as costume {span} of {}: {} folder(s). Put your model and \
             animations in the folders it made.",
            scaffolded.donor,
            scaffolded.created.len()
        );
    }

    pub fn note_error(&mut self, error: String) {
        self.error = Some(error);
    }
}

/// What occupies a costume slot, for the slot strip and the conflict list.
///
/// Order is `Free` first so the strip's "nothing to do here" reading is
/// available without scanning. The `taken_label` returns `Some` for every
/// non-`Free` variant with the name to show the user, so the conflict list
/// and the legend share one source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotStatus {
    Free,
    /// Donor's own costume, 0..VANILLA_SLOT_COUNT. The donor's files live
    /// there; a new character would clobber them.
    Donor,
    /// A modded slot contributed by a loaded mod, with no roster row.
    Modded,
    /// The current project already authors this slot.
    Authored,
    /// A different character (imported mod) owns the slot — its roster row
    /// points at the same donor + slot.
    Imported,
}

impl SlotStatus {
    fn taken_label(self) -> Option<String> {
        match self {
            Self::Free => None,
            Self::Donor => Some("donor's own costume".to_string()),
            Self::Modded => Some("a loaded mod".to_string()),
            Self::Authored => Some("already in this project".to_string()),
            Self::Imported => Some("another imported character".to_string()),
        }
    }
}

/// `start <= end`, both `u8`. Out-of-order input is repaired.
fn clamp_range(range: SlotRange) -> SlotRange {
    if range.0 <= range.1 {
        range
    } else {
        (range.1, range.0)
    }
}

/// The lowest `start` such that `start..=start + width - 1` is entirely free
/// for `donor`. Returns `None` when no `width`-sized run exists before
/// `u8::MAX`. Used to seed a default range when the donor changes.
fn lowest_free_run(
    roots: &[PathBuf],
    index: &RosterIndex,
    donor: &str,
    width: u8,
) -> Option<SlotRange> {
    let statuses = slot_statuses(roots, index, donor);
    // Scanning "after the last slot" wraps to the start, which is exactly
    // the lowest run.
    next_free_run_after(&statuses, width, u8::MAX)
}

/// Status of every slot 0..=255 for `donor`. The donor's vanilla 0..7
/// default is the floor; a mod that adds a slot overrides the corresponding
/// entry. Authored and imported rows from the index take precedence so the
/// rule "I already have a character here" reads as the user wrote it.
fn slot_statuses(roots: &[PathBuf], index: &RosterIndex, donor: &str) -> Vec<SlotStatus> {
    let mut out: Vec<SlotStatus> = (0..=255u8)
        .map(|i| {
            if i < crate::data::VANILLA_SLOT_COUNT {
                SlotStatus::Donor
            } else {
                SlotStatus::Free
            }
        })
        .collect();
    for slot in crate::data::discover_costume_slots(roots, donor) {
        // Modded overrides Donor (the donor's own c00..c07 cannot be a
        // modded slot, but be defensive — if a mod adds files for c00, the
        // strip should still show "modded", not the donor's own).
        if (out[slot as usize] == SlotStatus::Donor || out[slot as usize] == SlotStatus::Free)
            && !index_slots_for(index, donor).contains(&slot)
            && !authored_slots_for(index, donor).contains(&slot)
        {
            out[slot as usize] = SlotStatus::Modded;
        }
    }
    for slot in index_slots_for(index, donor) {
        out[slot as usize] = SlotStatus::Imported;
    }
    for slot in authored_slots_for(index, donor) {
        out[slot as usize] = SlotStatus::Authored;
    }
    out
}

/// Every slot an index entry on this donor occupies, including a
/// multi-skin character's additional slots.
fn index_slots_for(index: &RosterIndex, donor: &str) -> Vec<u8> {
    index
        .entries
        .iter()
        .filter(|e| e.fighter.as_deref() == Some(donor))
        .flat_map(|e| e.backing.all_slots().into_iter())
        .collect()
}

/// Every slot the current project has authored on this donor.
fn authored_slots_for(index: &RosterIndex, donor: &str) -> Vec<u8> {
    index
        .entries
        .iter()
        .filter(|e| {
            e.fighter.as_deref() == Some(donor) && matches!(e.origin, super::EntryOrigin::Authored)
        })
        .flat_map(|e| e.backing.all_slots().into_iter())
        .collect()
}

/// Every taken slot in `range`, with its owner label. Pure over a status
/// slice so the panel computes statuses once per frame and reuses them.
fn conflicts_in_range(statuses: &[SlotStatus], range: SlotRange) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    for slot in range.0..=range.1 {
        if let Some(label) = statuses[slot as usize].taken_label() {
            out.push((slot, label));
        }
    }
    out
}

/// One-line rendering of a conflict list for the status line. Caps at six
/// entries so a fully-blocked range does not flood the form; the map shows
/// the rest.
fn summarize_taken(taken: &[(u8, String)]) -> String {
    const SHOW: usize = 6;
    let mut parts: Vec<String> = taken
        .iter()
        .take(SHOW)
        .map(|(slot, owner)| format!("c{slot:02} ({owner})"))
        .collect();
    if taken.len() > SHOW {
        parts.push(format!("+{} more", taken.len() - SHOW));
    }
    parts.join(", ")
}

/// The first clear block of `width` slots starting strictly after `after`,
/// wrapping to the start. `None` when no block fits anywhere.
fn next_free_run_after(statuses: &[SlotStatus], width: u8, after: u8) -> Option<SlotRange> {
    let width = width.max(1) as usize;
    let cap = statuses.len();
    // Scan forward from `after + 1`, then wrap: the first start whose whole
    // block is free wins, so "next" means nearest, with wrap-around.
    let order = ((after as usize + 1)..cap).chain(0..=(after as usize).min(cap - 1));
    for start in order {
        if start + width > cap {
            continue;
        }
        if statuses[start..start + width]
            .iter()
            .all(|s| *s == SlotStatus::Free)
        {
            let end = (start + width - 1) as u8;
            return Some((start as u8, end));
        }
    }
    None
}

/// One slot chip in the map grid. Two visual states only — white for free,
/// grey for taken — with the taken reason on hover. A free chip inside the
/// current block is green so the block reads as one shape. Clicking a free
/// chip moves the whole block there, keeping the skin count.
fn slot_chip(
    ui: &mut Ui,
    slot: u8,
    status: &SlotStatus,
    in_range: bool,
    count: u8,
    slot_range: &mut Option<SlotRange>,
) {
    let label = format!("c{slot:02}");
    let free = matches!(status, SlotStatus::Free);
    let (fg, bg) = match (free, in_range) {
        (true, true) => (
            Color32::from_rgb(20, 60, 30),
            Color32::from_rgb(130, 225, 150),
        ),
        (true, false) => (
            Color32::from_rgb(60, 60, 60),
            Color32::from_rgb(250, 250, 250),
        ),
        (false, true) => (
            Color32::from_rgb(120, 50, 30),
            Color32::from_rgb(245, 200, 190),
        ),
        (false, false) => (
            Color32::from_rgb(150, 150, 150),
            Color32::from_rgb(228, 228, 228),
        ),
    };
    let button = egui::Button::new(RichText::new(label).small().color(fg))
        .fill(bg)
        .sense(if free {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        });
    let end = slot.saturating_add(count.saturating_sub(1));
    let hover = match status.taken_label() {
        Some(owner) => format!("c{slot:02}: {owner}"),
        None => format!("Move block here (c{slot:02}…c{end:02})"),
    };
    let resp = ui.add(button).on_hover_text(hover);
    if resp.clicked() && free {
        *slot_range = Some((slot, end.max(slot)));
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

/// Build the project record for a multi-skin character: one entry owning every
/// slot in `slots` (which must be non-empty and already validated free).
/// The key and primary slot are the lowest slot, so the entry sorts with it.
/// A single-element slice is a one-skin character; there is no separate
/// single-slot constructor.
pub fn authored_entry_multi(
    donor: &str,
    slots: &[u8],
    display_name: &str,
    name_id: &str,
) -> AuthoredEntry {
    let mut sorted: Vec<u8> = slots.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let primary = sorted.first().copied().unwrap_or(0);
    AuthoredEntry {
        key: RosterKey::slot(donor, primary),
        donor: donor.to_ascii_lowercase(),
        slot: primary,
        slots: sorted.into_iter().filter(|slot| *slot != primary).collect(),
        display_name: display_name.to_string(),
        name_id: name_id.to_string(),
        moveset_scaffolded: false,
        // Set by the window when it creates the scaffold; the helper alone
        // knows no destination.
        files_root: None,
    }
}

/// Create the files for every slot in `slots` and register the result as a
/// mod. The mod's manifest declares every slot, so each skin is immediately
/// editable. The folder layout is one mod root holding all of them — that
/// matches how slot-pack mods are organised.
pub fn create_and_import_range(
    library: &mut ModLibrary,
    destination: &std::path::Path,
    donor: &str,
    slots: &[u8],
    display_name: &str,
) -> anyhow::Result<scaffold::Scaffold> {
    if slots.is_empty() {
        anyhow::bail!("a new character needs at least one costume slot");
    }
    let root = destination.join(crate::mod_export::slugify(display_name));
    let scaffolded = scaffold::create_many(&root, donor, slots)?;
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
        let entry = authored_entry_multi("Mario", &[8], "Vision", "vision");
        assert_eq!(entry.key, RosterKey::slot("mario", 8));
        assert_eq!(entry.donor, "mario");
        assert_eq!(entry.name_id, "vision");
        assert!(!entry.moveset_scaffolded);
    }

    /// Quick-creating a range that overlaps a character already in the project
    /// would import the same files as a second mod, so the form refuses it
    /// before anything is written.
    #[test]
    fn quick_create_refuses_a_range_overlapping_an_authored_character() {
        let mut view = NewCharacterView {
            donor: Some("mario".into()),
            slot_range: Some((8, 15)),
            display_name: "Vision".into(),
            ..Default::default()
        };
        let mut project = RosterMod::default();
        project.authored.push(authored_entry_multi(
            "mario",
            &[8, 9, 10, 11, 12, 13, 14, 15],
            "Vision",
            "vision",
        ));
        // The guard returns before the authored folder is touched, so this
        // test writes nothing outside tempdirs.
        assert!(matches!(
            view.build_quick_create_action(&project),
            NewCharacterAction::None
        ));
        assert!(view.error.is_some());
    }

    #[test]
    fn a_multi_skin_range_creates_every_skin_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ModLibrary::default();
        let scaffolded = create_and_import_range(
            &mut library,
            dir.path(),
            "mario",
            &[8, 9, 10, 11, 12, 13, 14, 15],
            "Vision",
        )
        .unwrap();

        assert_eq!(scaffolded.slots, vec![8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(scaffolded.slot, 8);
        assert_eq!(library.mods.len(), 1);
        let mod_root = dir.path().join(crate::mod_export::slugify("Vision"));
        for slot in 8..=15 {
            assert!(
                mod_root
                    .join(format!("fighter/mario/model/body/c{slot:02}"))
                    .is_dir(),
                "model dir for c{slot:02} missing"
            );
            assert!(
                mod_root
                    .join(format!("fighter/mario/motion/body/c{slot:02}"))
                    .is_dir(),
                "motion dir for c{slot:02} missing"
            );
        }
    }

    #[test]
    fn slot_statuses_separate_free_donor_modded_and_authored() {
        // Donor always owns c00..c07. A mod can add files beyond that. The
        // project can author its own. The index can hold an imported
        // character. None of these should be conflated.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for slot in 8..=11 {
            std::fs::create_dir_all(root.join(format!("fighter/mario/model/body/c{slot:02}")))
                .unwrap();
        }
        std::fs::create_dir_all(root.join("effect/fighter/mario")).unwrap();
        std::fs::write(root.join("effect/fighter/mario/ef_mario_c20.eff"), b"").unwrap();

        let index = RosterIndex::default();
        let s = slot_statuses(&[root.to_path_buf()], &index, "mario");
        for (i, status) in s.iter().enumerate().take(8) {
            assert_eq!(*status, SlotStatus::Donor, "c{i:02} should be donor's");
        }
        for (i, status) in s.iter().enumerate().take(12).skip(8) {
            assert_eq!(*status, SlotStatus::Modded, "c{i:02} should be modded");
        }
        assert_eq!(s[12], SlotStatus::Free);
        assert_eq!(s[19], SlotStatus::Free);
        assert_eq!(s[20], SlotStatus::Modded, "eff-only slot is modded");
    }

    #[test]
    fn lowest_free_run_picks_the_first_clear_run_of_the_needed_width() {
        // Donor owns 0..7. A mod occupies 8..11 (a 4-slot run). The lowest
        // free run of 8 slots starts at 12 and ends at 19.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for slot in 8..=11 {
            std::fs::create_dir_all(root.join(format!("fighter/mario/model/body/c{slot:02}")))
                .unwrap();
        }
        let index = RosterIndex::default();
        let run = lowest_free_run(&[root.to_path_buf()], &index, "mario", 8);
        assert_eq!(run, Some((12, 19)));
    }

    #[test]
    fn lowest_free_run_returns_none_when_no_clear_run_exists() {
        // A mod occupies 8..=255 (every slot past the donor's 8). No
        // 8-wide run exists anywhere.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for slot in 8..=255 {
            std::fs::create_dir_all(root.join(format!("fighter/mario/model/body/c{slot:02}")))
                .unwrap();
        }
        let index = RosterIndex::default();
        assert_eq!(
            lowest_free_run(&[root.to_path_buf()], &index, "mario", 8),
            None
        );
    }

    #[test]
    fn range_conflict_names_every_taken_slot_in_the_range() {
        let view = NewCharacterView {
            donor: Some("mario".into()),
            slot_range: Some((8, 15)),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // c08 and c10 are occupied by a mod.
        for slot in [8, 10] {
            std::fs::create_dir_all(root.join(format!("fighter/mario/model/body/c{slot:02}")))
                .unwrap();
        }
        let index = RosterIndex::default();
        let taken = view
            .slot_range_conflict(&[root.to_path_buf()], &index)
            .expect("conflict expected");
        let slots: Vec<u8> = taken.iter().map(|(s, _)| *s).collect();
        assert_eq!(slots, vec![8, 10]);
    }

    #[test]
    fn range_conflict_returns_none_when_the_range_is_fully_free() {
        let view = NewCharacterView {
            donor: Some("mario".into()),
            slot_range: Some((8, 15)),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let index = RosterIndex::default();
        assert!(view
            .slot_range_conflict(&[root.to_path_buf()], &index)
            .is_none());
    }

    fn free_statuses(taken: &[u8]) -> Vec<SlotStatus> {
        let mut out = vec![SlotStatus::Free; 256];
        for slot in taken {
            out[*slot as usize] = SlotStatus::Modded;
        }
        out
    }

    #[test]
    fn next_free_block_jumps_past_the_current_block() {
        // c08–c11 taken by a mod: from c08 the next clear 8-block is c12–c19.
        let statuses = free_statuses(&[8, 9, 10, 11]);
        assert_eq!(next_free_run_after(&statuses, 8, 8), Some((12, 19)));
    }

    #[test]
    fn next_free_block_wraps_to_the_start() {
        // c00–c07 are the donor's own and everything from c16 up is taken:
        // from c20 the only clear 8-block is back at c08–c15.
        let mut taken: Vec<u8> = (0..=7).collect();
        taken.extend(16..=255);
        let statuses = free_statuses(&taken);
        assert_eq!(next_free_run_after(&statuses, 8, 20), Some((8, 15)));
        let mut taken_all: Vec<u8> = (0..=7).collect();
        taken_all.extend(8..=255);
        let full = free_statuses(&taken_all);
        assert_eq!(next_free_run_after(&full, 8, 20), None);
    }

    #[test]
    fn taken_summary_caps_long_lists() {
        let taken: Vec<(u8, String)> = (8..=15).map(|s| (s, "a loaded mod".to_string())).collect();
        let summary = summarize_taken(&taken);
        assert!(summary.contains("c08"), "first conflict named: {summary}");
        assert!(summary.contains("+2 more"), "overflow counted: {summary}");
        let short = summarize_taken(&taken[..2]);
        assert!(!short.contains("more"), "short lists shown whole: {short}");
    }

    #[test]
    fn clamp_range_repairs_an_inverted_range() {
        assert_eq!(clamp_range((5, 3)), (3, 5));
        assert_eq!(clamp_range((5, 5)), (5, 5));
        assert_eq!(clamp_range((0, 255)), (0, 255));
    }

    #[test]
    fn range_count_and_slots_round_trip() {
        assert_eq!(range_count((8, 15)), 8);
        assert_eq!(range_count((0, 0)), 1);
        assert_eq!(range_count((0, 255)), 256.min(u8::MAX as usize) as u8);
        assert_eq!(range_slots((8, 11)), vec![8, 9, 10, 11]);
    }

    #[test]
    fn creating_a_character_also_makes_it_visible_to_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = ModLibrary::default();
        let scaffolded =
            create_and_import_range(&mut library, dir.path(), "mario", &[8], "Vision").unwrap();

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
            create_and_import_range(&mut library, &dir.path().join("a"), "mario", &[8], "Vision")
                .unwrap();
        let b = create_and_import_range(
            &mut library,
            &dir.path().join("b"),
            "mario",
            &[9],
            "Vision2",
        )
        .unwrap();
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
        let s = crate::roster::scaffold::create_many(&root, "mario", &[12]).unwrap();
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
