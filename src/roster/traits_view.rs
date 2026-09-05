//! The trait editor: fighter-wide values, grouped and explained.
//!
//! Two things are always visible here because both are easy to get wrong silently:
//!
//!  * **What the value was.** Every field shows the base file's value beside the edited one,
//!    so an override is never mistaken for the game's own number.
//!  * **Who it affects.** These values are keyed by fighter and nothing below it, so a
//!    slot-backed character shares every one of them with its donor. That is stated once and
//!    prominently rather than as a per-field badge, because a badge on some fields would imply
//!    the unbadged ones are safe. None are.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use egui::{Color32, RichText, Ui};

use crate::mod_project::{ParamMod, ParamValue};

use super::index::RosterIndex;
use super::traits::{self, FighterTraits};
use super::RosterKey;

#[derive(Default)]
pub struct TraitsView {
    /// The entry whose traits are shown.
    selected: Option<RosterKey>,
    /// The loaded row, and which fighter it belongs to.
    loaded: Option<FighterTraits>,
    load_error: Option<String>,
    /// Text being typed, per field. Held apart from the committed value so a half-typed
    /// number does not get written into the project on every keystroke.
    drafts: HashMap<String, String>,
    show_all_fields: bool,
    filter: String,
    status: String,
}

impl TraitsView {
    /// Drop the loaded file. Called when the library changes, since a mod may now provide it.
    pub fn invalidate(&mut self) {
        self.loaded = None;
        self.load_error = None;
        self.drafts.clear();
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        roots: &[PathBuf],
        labels: &HashMap<u64, String>,
        index: &RosterIndex,
        params: &mut BTreeMap<String, ParamMod>,
    ) {
        let Some(path) = FighterTraits::locate(roots) else {
            self.draw_missing_file(ui);
            return;
        };

        // Header with picker and global controls — same group styling as the main editor
        self.draw_traits_header(ui, index, params);
        let Some(key) = self.selected.clone() else {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("◈  Pick a character").size(13.0).strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Choose a fighter above to edit its fighter-wide values — weight, speed, jumps, shield and more.")
                                .small()
                                .weak(),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Values are per fighter, not per costume. A new character that shares a donor shares all of them.")
                                .small()
                                .weak(),
                        );
                    });
                });
            return;
        };
        let Some(entry) = index.by_key(&key) else {
            return;
        };
        let Some(fighter) = entry.fighter.clone() else {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::from_rgb(240, 180, 120), "⚠");
                        ui.label(
                            RichText::new(
                                "This roster entry has no fighter behind it, so it has no fighter values to edit.",
                            )
                            .small()
                            .weak(),
                        );
                    });
                });
            return;
        };

        // Reload when the selection moved to a different fighter.
        if self.loaded.as_ref().map(|traits| traits.fighter.as_str()) != Some(fighter.as_str()) {
            self.drafts.clear();
            match FighterTraits::open(&path, &fighter, labels) {
                Ok(traits) => {
                    self.loaded = Some(traits);
                    self.load_error = None;
                }
                Err(error) => {
                    self.loaded = None;
                    self.load_error = Some(format!("{error:#}"));
                }
            }
        }

        if let Some(error) = &self.load_error.clone() {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::from_rgb(240, 120, 120), "✘");
                        ui.label(RichText::new(error).small().weak());
                    });
                });
            return;
        }
        // Lifted out of `self` for the duration of the draw and put back below: the field
        // rows need `&mut self` for the per-field text drafts, and the loaded row at the same
        // time. Taking it is cheaper and clearer than splitting the state into two structs.
        let Some(loaded) = self.loaded.take() else {
            return;
        };

        if entry.backing.shares_engine_fighter() {
            self.draw_shared_notice(ui, entry.display_name.as_str(), &fighter);
        }

        let edits = params.entry(fighter.clone()).or_default();
        let count = traits::edits_for(edits).len();

        // Toolbar: filter + toggles + reset — matches main editor's horizontal_wrapped toolbars
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.show_all_fields, RichText::new("Show every field").small())
                        .on_hover_text(
                            "The grouped sections cover the values most edits touch. This shows all of \
                             them, including ones with no plain-language explanation.",
                        );
                    ui.separator();
                    ui.label(RichText::new("⌕").small().weak());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .desired_width(160.0)
                            .hint_text("filter fields…"),
                    );
                    if !self.filter.is_empty() && ui.small_button(RichText::new("✕").small()).clicked() {
                        self.filter.clear();
                        resp.request_focus();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                count > 0,
                                egui::Button::new(RichText::new("  ↺ Reset all  ").small()),
                            )
                            .on_hover_text("Return every value to the game's own")
                            .clicked()
                        {
                            edits.files.remove(traits::FIGHTER_PARAM_PATH);
                            self.drafts.clear();
                            self.status = format!("{fighter} returned to the game's values.");
                        }
                        if count > 0 {
                            ui.colored_label(
                                Color32::from_rgb(240, 200, 120),
                                format!("{count} change{}", if count == 1 { "" } else { "s" }),
                            );
                        } else {
                            ui.label(RichText::new("No changes").small().weak());
                        }
                    });
                });
            });
        ui.add_space(6.0);

        // Info banner about saved/export model — uses group style like main editor banners
        ui.horizontal(|ui| {
            ui.colored_label(Color32::from_rgb(120, 185, 235), "ⓘ");
            ui.label(
                RichText::new(
                    "Edits are saved with your project and included when you export — no separate apply step.",
                )
                .small()
                .weak(),
            );
        });
        ui.add_space(4.0);

        let mut pending: Vec<(String, Option<ParamValue>)> = Vec::new();
        // No inner scroll: the window already scrolls vertically, and a second
        // vertical scroller inside it fights for the wheel.
        if self.show_all_fields {
            self.draw_all_fields(ui, &loaded, edits, &mut pending);
        } else {
            self.draw_sections(ui, &loaded, edits, &mut pending);
        }

        for (key, value) in pending {
            let base = loaded.get(&key).copied();
            match value {
                Some(value) => {
                    traits::record_edit(edits, &key, value, base);
                    // The committed text may spell the value differently from
                    // how it renders ("3" vs "3.0") — drop the draft so the
                    // field shows the canonical spelling next frame.
                    self.drafts.remove(&key);
                }
                None => {
                    if let Some(file) = edits.files.get_mut(traits::FIGHTER_PARAM_PATH) {
                        file.remove(&key);
                    }
                    self.drafts.remove(&key);
                }
            }
        }
        if params.get(&fighter).is_some_and(ParamMod::is_empty) {
            params.remove(&fighter);
        }
        self.loaded = Some(loaded);

        if !self.status.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(120, 185, 235), "●");
                ui.label(RichText::new(&self.status).small().weak());
            });
        }
    }

    fn draw_traits_header(
        &mut self,
        ui: &mut Ui,
        index: &RosterIndex,
        params: &BTreeMap<String, ParamMod>,
    ) {
        // Header card — uses group frame like the main sidebar headings
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.heading("◈  Traits");
                    ui.add_space(4.0);
                    self.draw_picker(ui, index);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let total: usize = params.values().map(|p| p.field_count()).sum();
                        if total > 0 {
                            ui.colored_label(
                                Color32::from_rgb(240, 200, 120),
                                format!("{total} edited"),
                            );
                        }
                        ui.label(
                            RichText::new(format!("{} fighter(s) have edits", params.len()))
                                .small()
                                .weak(),
                        );
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Grouped by purpose — Movement, Jumps, Shield, Damage and more. Switch on \"Show every field\" for the full list.",
                    )
                    .small()
                    .weak(),
                );
            });
        ui.add_space(6.0);
    }

    fn draw_missing_file(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(230, 180, 80), RichText::new("⚠").size(18.0));
                    ui.label(RichText::new("No fighter values file found").heading());
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Weight, gravity, speeds, and the rest live in one shared file, {}. Dump the \
                         fighter/common folder along with the rest of fighter/ to edit them.",
                        traits::FIGHTER_PARAM_PATH
                    ))
                    .small(),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "This file is fighter/common/param/fighter_param.prc. Without it, trait edits cannot be loaded or exported \
                         — the project will keep them, but the export will note which values could not be written.",
                    )
                    .small()
                    .weak(),
                );
            });
    }

    /// The one notice that matters for a slot-backed character.
    fn draw_shared_notice(&self, ui: &mut Ui, display_name: &str, donor: &str) {
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(240, 200, 120), "⚠");
                    ui.label(
                        RichText::new(format!(
                            "{display_name} is a costume of {donor} — these values belong to {donor} as a whole."
                        ))
                        .strong(),
                    );
                });
                ui.add_space(2.0);
                ui.label(
                    RichText::new(
                        "The game stores them per fighter, not per costume — there is no \
                         per-costume version of any of them. Changing a value here changes it \
                         for the donor and for every other costume of the donor, in the same \
                         match.",
                    )
                    .small()
                    .weak(),
                );
            });
        ui.add_space(6.0);
    }

    fn draw_sections(
        &mut self,
        ui: &mut Ui,
        loaded: &FighterTraits,
        edits: &ParamMod,
        pending: &mut Vec<(String, Option<ParamValue>)>,
    ) {
        let filter = self.filter.to_lowercase();
        let mut any_visible = false;
        for section in traits::SECTIONS {
            let matching: Vec<&traits::TraitField> = section
                .fields
                .iter()
                .filter(|field| {
                    filter.is_empty()
                        || field.key.to_lowercase().contains(&filter)
                        || field.label.to_lowercase().contains(&filter)
                })
                .collect();
            if matching.is_empty() {
                continue;
            }
            any_visible = true;
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(section.title).size(12.0).strong());
                        let edited_in_section = matching
                            .iter()
                            .filter(|f| traits::edits_for(edits).contains_key(f.key))
                            .count();
                        if edited_in_section > 0 {
                            ui.colored_label(
                                Color32::from_rgb(240, 200, 120),
                                format!("{edited_in_section} edited"),
                            );
                        }
                    });
                    ui.label(RichText::new(section.description).small().weak());
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    for field in matching {
                        self.draw_field(
                            ui,
                            loaded,
                            edits,
                            field.key,
                            field.label,
                            Some(field.description),
                            pending,
                        );
                        ui.add_space(2.0);
                    }
                });
            ui.add_space(8.0);
        }
        if !any_visible && !filter.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(format!("No fields match \"{}\"", self.filter)).weak());
                ui.label(
                    RichText::new("Try a shorter term or clear the filter.")
                        .small()
                        .weak(),
                );
            });
        }
    }

    fn draw_all_fields(
        &mut self,
        ui: &mut Ui,
        loaded: &FighterTraits,
        edits: &ParamMod,
        pending: &mut Vec<(String, Option<ParamValue>)>,
    ) {
        let filter = self.filter.to_lowercase();
        let keys: Vec<String> = loaded
            .values()
            .iter()
            .map(|value| value.key.clone())
            .filter(|key| filter.is_empty() || key.to_lowercase().contains(&filter))
            .collect();
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("All fields").strong());
                    ui.label(
                        RichText::new(format!("{} field(s)", keys.len()))
                            .small()
                            .weak(),
                    );
                });
                ui.add_space(6.0);
                for key in keys {
                    self.draw_field(ui, loaded, edits, &key, &key, None, pending);
                    ui.add_space(1.0);
                }
            });
    }

    // One row needs ui, data, edits, key, label, description, draft state,
    // and the pending queue — the traversal bundle again, not a real smell.
    #[allow(clippy::too_many_arguments)]
    fn draw_field(
        &mut self,
        ui: &mut Ui,
        loaded: &FighterTraits,
        edits: &ParamMod,
        key: &str,
        label: &str,
        description: Option<&str>,
        pending: &mut Vec<(String, Option<ParamValue>)>,
    ) {
        let Some(base) = loaded.get(key).copied() else {
            return;
        };
        let edited = traits::edits_for(edits).get(key).copied();
        let current = edited.unwrap_or(base);
        let is_edited = edited.is_some();

        let bg = if is_edited {
            Color32::from_rgb(42, 40, 32)
        } else {
            ui.visuals().faint_bg_color
        };
        let stroke = if is_edited {
            Color32::from_rgb(80, 70, 40)
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        };
        egui::Frame::new()
            .fill(bg)
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(8, 5))
            .stroke(egui::Stroke::new(1.0, stroke))
            .show(ui, |ui| {
                // One layout that wraps: label row on top at any width, value
                // row below. The old narrow/wide split duplicated this whole
                // body for a 420px breakpoint.
                ui.horizontal_wrapped(|ui| {
                    let name =
                        ui.label(RichText::new(label).small().strong().color(if is_edited {
                            Color32::from_rgb(240, 200, 120)
                        } else {
                            ui.visuals().text_color()
                        }));
                    if let Some(description) = description {
                        name.on_hover_text(description);
                    }
                    if is_edited {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(68, 55, 20))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(5, 1))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("edited")
                                        .small()
                                        .strong()
                                        .color(Color32::from_rgb(240, 200, 120)),
                                );
                            });
                    }
                    if let Some(desc) = description {
                        ui.label(
                            RichText::new("ⓘ")
                                .small()
                                .weak()
                                .color(Color32::from_rgb(120, 140, 170)),
                        )
                        .on_hover_text(desc);
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let draft = self
                        .drafts
                        .entry(key.to_string())
                        .or_insert_with(|| render(current));
                    let resp = ui.add(
                        egui::TextEdit::singleline(draft)
                            .desired_width(96.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    if resp.lost_focus() || resp.ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        match parse(draft, base) {
                            Some(value) if Some(value) != edited.or(Some(base)) => {
                                pending.push((key.to_string(), Some(value)));
                            }
                            Some(_) => {
                                *draft = render(current);
                            }
                            None => {
                                *draft = render(current);
                            }
                        }
                    }
                    if is_edited {
                        if ui
                            .small_button(RichText::new(" ↺ ").small())
                            .on_hover_text("Return to the game's value")
                            .clicked()
                        {
                            pending.push((key.to_string(), None));
                        }
                        ui.label(
                            RichText::new(format!("was {}", render(base)))
                                .small()
                                .weak(),
                        );
                    }
                });
            });
    }

    fn draw_picker(&mut self, ui: &mut Ui, index: &RosterIndex) {
        let entries: Vec<&super::RosterEntry> = index
            .sorted()
            .into_iter()
            .filter(|entry| entry.fighter.is_some())
            .collect();
        let current = self
            .selected
            .as_ref()
            .and_then(|key| index.by_key(key))
            .map(|entry| entry.display_name.clone())
            .unwrap_or_else(|| "Select a character".to_string());
        let current_key = self.selected.clone();
        let picker_w = (ui.available_width() - 200.0).clamp(140.0, 220.0);
        egui::ComboBox::from_id_salt("traits_character_picker")
            .selected_text(RichText::new(current).strong())
            .width(picker_w)
            .show_ui(ui, |ui| {
                for entry in entries {
                    let is_selected = current_key.as_ref() == Some(&entry.key);
                    let mut label = entry.display_name.clone();
                    if entry.backing.shares_engine_fighter() {
                        label.push_str("  ◈ costume");
                    }
                    let resp = ui.selectable_label(is_selected, label);
                    if resp.clicked() {
                        self.selected = Some(entry.key.clone());
                    }
                    if is_selected {
                        resp.scroll_to_me(Some(egui::Align::Center));
                    }
                }
            });
    }
}

/// How a value is shown and typed.
fn render(value: ParamValue) -> String {
    match value {
        ParamValue::Bool(flag) => flag.to_string(),
        ParamValue::Float(number) => {
            if number.fract() == 0.0 {
                format!("{number:.1}")
            } else {
                format!("{number}")
            }
        }
        ParamValue::I8(number) => number.to_string(),
        ParamValue::U8(number) => number.to_string(),
        ParamValue::I16(number) => number.to_string(),
        ParamValue::U16(number) => number.to_string(),
        ParamValue::I32(number) => number.to_string(),
        ParamValue::U32(number) => number.to_string(),
        ParamValue::Hash(raw) => format!("{raw:#x}"),
    }
}

/// Parse typed text back into the type the field already holds.
///
/// The base value decides the type rather than the text: typing `3` into a decimal field means
/// `3.0`, not an integer that a later write would have to guess about.
fn parse(text: &str, base: ParamValue) -> Option<ParamValue> {
    let text = text.trim();
    Some(match base {
        ParamValue::Bool(_) => ParamValue::Bool(match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => true,
            "false" | "no" | "off" | "0" => false,
            _ => return None,
        }),
        ParamValue::Float(_) => ParamValue::Float(text.parse().ok()?),
        ParamValue::I8(_) => ParamValue::I8(text.parse().ok()?),
        ParamValue::U8(_) => ParamValue::U8(text.parse().ok()?),
        ParamValue::I16(_) => ParamValue::I16(text.parse().ok()?),
        ParamValue::U16(_) => ParamValue::U16(text.parse().ok()?),
        ParamValue::I32(_) => ParamValue::I32(text.parse().ok()?),
        ParamValue::U32(_) => ParamValue::U32(text.parse().ok()?),
        ParamValue::Hash(_) => {
            let raw = text.strip_prefix("0x").unwrap_or(text);
            ParamValue::Hash(u64::from_str_radix(raw, 16).ok()?)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed text is interpreted as the field's own type, not guessed from the text. `3`
    /// in a decimal field is `3.0`; an integer there would have to be re-guessed on write.
    #[test]
    fn typed_text_takes_the_type_of_the_field_it_is_typed_into() {
        assert_eq!(
            parse("3", ParamValue::Float(1.0)),
            Some(ParamValue::Float(3.0))
        );
        assert_eq!(parse("3", ParamValue::I32(1)), Some(ParamValue::I32(3)));
        assert_eq!(parse("3.5", ParamValue::I32(1)), None);
    }

    #[test]
    fn yes_no_fields_accept_the_words_people_type() {
        for text in ["true", "yes", "on", "1"] {
            assert_eq!(
                parse(text, ParamValue::Bool(false)),
                Some(ParamValue::Bool(true))
            );
        }
        for text in ["false", "no", "off", "0"] {
            assert_eq!(
                parse(text, ParamValue::Bool(true)),
                Some(ParamValue::Bool(false))
            );
        }
        assert_eq!(parse("maybe", ParamValue::Bool(true)), None);
    }

    /// A value has to survive being shown and typed back unchanged, or every field would
    /// register as edited the moment it is focused.
    #[test]
    fn every_value_type_round_trips_through_its_own_display() {
        let values = [
            ParamValue::Float(98.0),
            ParamValue::Float(0.10978),
            ParamValue::I32(-3),
            ParamValue::I8(-128),
            ParamValue::U8(255),
            ParamValue::I16(-32768),
            ParamValue::U16(65535),
            ParamValue::U32(4_000_000_000),
            ParamValue::Bool(true),
            ParamValue::Hash(0x1234abcd),
        ];
        for value in values {
            assert_eq!(parse(&render(value), value), Some(value), "{value:?}");
        }
    }
}
