//! The Roster window: mod library, character select screen, new characters, and traits.
//!
//! A separate viewport rather than a panel in the main window, for the same reason the Eff
//! Editor is one: it is a whole workspace, and the main window's space belongs to the move
//! being edited. `app.rs` owns one field of this type and one menu checkbox; everything else
//! about the roster lives under `src/roster/`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use egui::{Color32, RichText, Ui};

use super::css_view::CssView;
use super::index::RosterIndex;
use super::library::{self, ModLibrary, ModSource, PreparedMod, ProviderId};
use super::new_character::{self, NewCharacterAction, NewCharacterView};
use super::traits_view::TraitsView;
use super::RosterKey;
use crate::data::FighterEntry;
use crate::mod_project::{ParamMod, RosterMod};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterTab {
    Library,
    CharacterSelect,
    NewCharacter,
    Traits,
}

impl RosterTab {
    const ALL: [Self; 4] = [
        Self::Library,
        Self::CharacterSelect,
        Self::NewCharacter,
        Self::Traits,
    ];

    fn icon(self) -> &'static str {
        match self {
            Self::Library => "▦",
            Self::CharacterSelect => "⧉",
            Self::NewCharacter => "＋",
            Self::Traits => "◈",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Library => "Mod library",
            Self::CharacterSelect => "Character select",
            Self::NewCharacter => "New character",
            Self::Traits => "Traits",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Library => {
                "Compiled mods Visionary reads fighters, skins, and effects from. Later mods in \
                 the list win any file two mods both provide."
            }
            Self::CharacterSelect => {
                "The character select screen as the game will build it. Drag a portrait to reorder — \
                 changes are saved with your project and included when you export a mod."
            }
            Self::NewCharacter => {
                "Add a character to the roster as a costume slot on a donor fighter, and get a \
                 place to put your own model, animations, and moveset."
            }
            Self::Traits => {
                "Fighter-wide values stored in the character's parameter files — weight, \
                 gravity, run speed, air acceleration, jumps, shield, and the rest."
            }
        }
    }

    fn badge_count(self, window: &RosterWindow) -> Option<usize> {
        match self {
            Self::Library => {
                let conflicts = window
                    .library
                    .conflicts_by_fighter()
                    .values()
                    .map(|v| v.len())
                    .sum::<usize>();
                let stale = window.index.stale_overrides.len();
                let total = conflicts + stale;
                (total > 0).then_some(total)
            }
            Self::CharacterSelect => {
                let n = window.project.order.len()
                    + window.project.hidden.len()
                    + window.project.names.len()
                    + window.project.name_variants.len()
                    + window
                        .project
                        .per_costume_names
                        .values()
                        .map(|m| m.len())
                        .sum::<usize>()
                    + window
                        .project
                        .ui_images
                        .values()
                        .map(|m| m.len())
                        .sum::<usize>()
                    + window.project.chara_overrides.len();
                (n > 0).then_some(n)
            }
            Self::NewCharacter => {
                let n = window.project.authored.len();
                (n > 0).then_some(n)
            }
            Self::Traits => {
                let n: usize = window.params.values().map(|p| p.field_count()).sum();
                (n > 0).then_some(n)
            }
        }
    }
}

/// What one import attempt did, kept so the whole batch can be reported at once.
///
/// One bad archive in a multi-select must not abort the rest, and it must not vanish either:
/// a mod that silently failed to import looks exactly like a mod that imported and provides
/// nothing.
#[derive(Debug, Clone)]
pub struct ImportNote {
    pub source: String,
    pub outcome: Result<String, String>,
}

enum ImportMsg {
    Progress {
        done: usize,
        total: usize,
        name: String,
    },
    Prepared(Box<PreparedMod>),
    Failed {
        source: String,
        error: String,
    },
    Finished,
}

struct ImportJob {
    receiver: Receiver<ImportMsg>,
    done: usize,
    total: usize,
    current: String,
}

pub struct RosterWindow {
    pub open: bool,
    pub library: ModLibrary,
    /// The open project's roster edits. Authored here, read by `app.rs` when it builds the
    /// project file, and replaced wholesale when a project is loaded.
    pub project: RosterMod,
    /// The resolved roster. Rebuilt from scratch every frame the window is drawn rather than
    /// cached: the index is derived, and a cache is the mechanism by which a derived view
    /// drifts from the project it is supposed to be showing.
    index: RosterIndex,
    /// The character select tab. Owns the loaded roster database and the portrait cache,
    /// which are the only two things here expensive enough to be worth holding.
    css: CssView,
    /// The trait tab.
    traits: TraitsView,
    /// The new-character tab.
    new_character: NewCharacterView,
    /// Moves the project has replaced for each character you added, supplied by `app.rs` from
    /// the edit log. Without it the readiness panel would report every character as having
    /// replaced nothing.
    pub authored_moves: std::collections::BTreeMap<RosterKey, std::collections::BTreeSet<String>>,
    /// The character you added whose moveset is currently being edited, if any. Moves saved
    /// while this is set are scoped to that character's costume.
    edit_target: Option<RosterKey>,
    /// Sparse fighter-value edits, fighter name → edits. Read by `app.rs` when it builds the
    /// project file and replaced wholesale when a project is loaded, exactly like `project`.
    pub params: std::collections::BTreeMap<String, ParamMod>,
    tab: RosterTab,
    status: String,
    import: Option<ImportJob>,
    /// Set when Deploy stages files: `app.rs` polls it after the window
    /// draws and saves the project (roster edits included) alongside.
    pub save_requested: bool,
    notes: Vec<ImportNote>,
    /// Set once the saved library has been loaded and pre-library mod roots adopted.
    initialized: bool,
    /// Set whenever the library changes in a way that invalidates the fighter index, so
    /// `app.rs` can rescan without this module reaching into it.
    pub library_dirty: bool,
    /// The mod whose file list is expanded in the library panel.
    expanded: Option<ProviderId>,
    /// The fighter whose conflicts are expanded.
    expanded_conflict: Option<String>,
    rename: Option<(ProviderId, String)>,
    /// Game roots in lookup order: the data root, then enabled mod roots in load order.
    /// Refreshed each frame from the library so a disabled mod stops providing files.
    roots: Vec<PathBuf>,
    /// hash40 → parameter field name, supplied by `app.rs` from the label download. The
    /// parameter files store hashes, and the full-field view has nothing to show without them.
    labels: std::collections::HashMap<u64, String>,
}

impl Default for RosterWindow {
    fn default() -> Self {
        Self {
            open: false,
            library: ModLibrary::default(),
            project: RosterMod::default(),
            index: RosterIndex::default(),
            css: CssView::default(),
            traits: TraitsView::default(),
            new_character: NewCharacterView::default(),
            authored_moves: Default::default(),
            edit_target: None,
            params: Default::default(),
            tab: RosterTab::Library,
            status: String::new(),
            import: None,
            save_requested: false,
            notes: Vec::new(),
            initialized: false,
            library_dirty: false,
            expanded: None,
            expanded_conflict: None,
            rename: None,
            roots: Vec::new(),
            labels: Default::default(),
        }
    }
}

impl RosterWindow {
    /// Load the saved library and adopt any pre-library `mod_roots` entries.
    ///
    /// Deferred to first use rather than done at construction: it walks every mod's files, and
    /// a user who never opens the Roster window should not pay for that at startup.
    pub fn ensure_initialized(&mut self, legacy_roots: &[PathBuf]) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        self.library = library::load();
        let adopted = library::adopt_legacy_roots(&mut self.library, legacy_roots);
        if !adopted.is_empty() {
            self.status = format!("Adopted {} existing mod root(s).", adopted.len());
            self.library_dirty = true;
            library::save(&self.library);
        }
    }

    /// Rebuild the derived state: the lookup roots, the loaded roster database, and the index.
    ///
    /// Called before drawing and before exporting. Exporting has to do it too — the index is
    /// only meaningful once it has been built, and a project loaded with roster edits by a
    /// user who never opened this window would otherwise export with an empty index and
    /// report every one of its own edits as unwritable.
    fn refresh(&mut self, fighters: &[FighterEntry], data_root: Option<&PathBuf>) {
        self.roots = data_root
            .into_iter()
            .cloned()
            .chain(self.library.enabled_roots())
            .collect();
        self.css.ensure_loaded(&self.roots);
        self.index = RosterIndex::build(fighters, &self.library, &self.project, self.css.db());
    }

    /// Write this project's roster edits into an exported mod folder.
    ///
    /// Takes the project's roster section rather than reading `self.project`, so that an
    /// export always emits what the project file being written contains.
    pub fn export_into(
        &mut self,
        mod_root: &std::path::Path,
        roster: &RosterMod,
        fighters: &[FighterEntry],
        data_root: Option<&PathBuf>,
    ) -> anyhow::Result<super::export::RosterExport> {
        if roster.is_empty()
            && self
                .params
                .values()
                .all(crate::mod_project::ParamMod::is_empty)
        {
            return Ok(super::export::RosterExport::default());
        }
        self.ensure_initialized(&[]);
        self.refresh(fighters, data_root);
        let ui_root = self.ui_root();
        let mut report =
            super::export::export_roster(mod_root, ui_root.as_deref(), &self.index, roster)?;
        super::export::export_params(
            mod_root,
            self.param_root().as_deref(),
            &self.params,
            &self.labels,
            &mut report,
        )?;
        super::export::export_authored_files(mod_root, &self.roots, &roster.authored, &mut report)?;
        Ok(report)
    }

    /// The arc root the shared fighter values file is read from, if one was found.
    ///
    /// Looked up separately from the roster database: `ui/` and `fighter/common/` are dumped
    /// independently, and a user who has one may well not have the other.
    fn param_root(&self) -> Option<PathBuf> {
        self.roots
            .iter()
            .find(|root| root.join(super::traits::FIGHTER_PARAM_PATH).is_file())
            .cloned()
    }

    /// The arc root the base roster database is read from, if one was found.
    fn ui_root(&self) -> Option<PathBuf> {
        super::css::locate_ui_root(&self.roots)
    }

    /// The costume slots new edits to `fighter` should belong to, if the user set one of their
    /// own characters as the edit target.
    ///
    /// Empty — the normal case — means an edit applies to every costume, which is what an
    /// ACMD replacement does. This is read on every edit save rather than latched, so turning
    /// the target off immediately stops scoping without a stale flag surviving anywhere.
    /// A multi-skin character returns all its slots, so one moveset covers c08–c15.
    pub fn slot_scopes_for(&self, fighter: &str) -> Vec<u8> {
        let Some(target) = self.edit_target.as_ref() else {
            return Vec::new();
        };
        let Some(entry) = self
            .project
            .authored
            .iter()
            .find(|authored| &authored.key == target)
        else {
            return Vec::new();
        };
        if !entry.donor.eq_ignore_ascii_case(fighter) {
            return Vec::new();
        }
        entry.all_slots()
    }

    /// The moves each added character has replaced, read out of the edit log.
    ///
    /// An edit belongs to a character when it is scoped to one of that character's
    /// costumes — the same fields the export gates on — so this cannot drift from
    /// what actually ships.
    pub fn replaced_moves(
        &self,
        log: &crate::data::EditLog,
    ) -> std::collections::BTreeMap<RosterKey, std::collections::BTreeSet<String>> {
        let mut out: std::collections::BTreeMap<RosterKey, std::collections::BTreeSet<String>> =
            Default::default();
        for authored in &self.project.authored {
            let Some(moves) = log.entries.get(&authored.donor) else {
                continue;
            };
            let owned = authored.all_slots();
            let replaced: std::collections::BTreeSet<String> = moves
                .iter()
                .filter(|(_, record)| {
                    let scopes = record.effective_scopes();
                    !scopes.is_empty() && scopes.iter().any(|slot| owned.contains(slot))
                })
                .map(|(move_name, _)| move_name.clone())
                .collect();
            if !replaced.is_empty() {
                out.insert(authored.key.clone(), replaced);
            }
        }
        out
    }

    /// Show the trait tab next time the window draws.
    pub fn focus_traits(&mut self) {
        self.tab = RosterTab::Traits;
    }

    /// Show the character select tab next time the window draws.
    pub fn focus_character_select(&mut self) {
        self.tab = RosterTab::CharacterSelect;
    }

    /// Show the new-character tab next time the window draws.
    pub fn focus_new_character(&mut self) {
        self.tab = RosterTab::NewCharacter;
    }

    /// Jump to Character Select with one entry selected and its editor
    /// scrolled into view — the landing end of cross-tab "finish look" jumps.
    pub fn select_css(&mut self, key: crate::roster::RosterKey) {
        self.tab = RosterTab::CharacterSelect;
        self.css.select_key(key);
    }

    /// Clear every position edit (edit-log revert path): the sparse order
    /// map plus any typed drafts showing in the CSS editor.
    pub fn clear_positions(&mut self) {
        self.project.order.clear();
        self.css.clear_order_drafts();
    }

    /// Enabled mod roots in load order — what the fighter scan consumes.
    pub fn enabled_roots(&self) -> Vec<PathBuf> {
        self.library.enabled_roots()
    }

    /// Summary for the export tree: how many roster edits this project holds.
    pub fn export_summary(&self) -> RosterExportSummary {
        let order = self.project.order.len();
        let hidden = self.project.hidden.len();
        let names = self.project.names.len()
            + self.project.name_variants.len()
            + self
                .project
                .per_costume_names
                .values()
                .map(|m| m.len())
                .sum::<usize>()
            + self
                .project
                .ui_images
                .values()
                .map(|m| m.len())
                .sum::<usize>()
            + self.project.chara_overrides.len();
        let authored = self.project.authored.len();
        let params: usize = self.params.values().map(|p| p.field_count()).sum();
        RosterExportSummary {
            order,
            hidden,
            names,
            authored,
            params,
        }
    }

    /// Any project edit the Edit Log should open for — roster or trait
    /// values. A roster-only project otherwise shows a disabled log with no
    /// way to review or revert what it holds.
    pub fn has_edits(&self) -> bool {
        let summary = self.export_summary();
        summary.order + summary.hidden + summary.names + summary.authored + summary.params > 0
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        fighters: &[FighterEntry],
        data_root: Option<&PathBuf>,
        labels: &std::collections::HashMap<u64, String>,
    ) {
        if !self.open {
            return;
        }
        // Labels arrive asynchronously and grow; clone when contents differ rather
        // than only on length – two maps of the same length with different keys
        // would otherwise keep showing stale names.
        if self.labels != *labels {
            self.labels = labels.clone();
            self.traits.invalidate();
        }
        self.ensure_initialized(&[]);
        self.poll_import(ctx);
        self.refresh(fighters, data_root);

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("roster"),
            egui::ViewportBuilder::default()
                .with_app_id(crate::app_icon::APP_ID)
                .with_icon(crate::app_icon::viewport_icon())
                .with_title("Roster — Visionary")
                .with_inner_size([1240.0, 800.0])
                .with_min_inner_size([860.0, 520.0]),
            |ui, class| {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    self.ui_contents(ui, fighters, data_root);
                });
                if class != egui::ViewportClass::EmbeddedWindow
                    && ui.ctx().input(|i| i.viewport().close_requested())
                {
                    self.open = false;
                }
            },
        );
    }

    fn ui_contents(&mut self, ui: &mut Ui, fighters: &[FighterEntry], data_root: Option<&PathBuf>) {
        // Larger type for the whole roster workspace, applied once here so
        // every tab inherits it without touching each label. `.small()` stays
        // relative — it still reads as secondary text, just legibly so.
        // (The main move-editor window keeps its own denser scale.)
        {
            let style = ui.style_mut();
            for font in style.text_styles.values_mut() {
                font.size *= 1.18;
            }
            style.spacing.item_spacing.y = (style.spacing.item_spacing.y + 1.0).max(5.0);
        }
        // One slim top bar: tabs with badges, edit count, and (only when
        // there is something to ship) what an export will contain. The old
        // layout stacked a title card, a metrics row, and a tab-description
        // line above the actual workspace — the grid and the editor below
        // are the focus, not this.
        self.draw_top_bar(ui, fighters, data_root);
        ui.add_space(8.0);

        // Content — one outer vertical scroll. Horizontal scrolling lives
        // inside the CSS grid only; the traits tab draws into this scroll
        // directly so two nested vertical scrollers never fight. Content-drag
        // scrolling is off here: the CSS tab owns reorder and marquee drags,
        // and a panning scroll area eats those (and sloppy clicks with them).
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(8, 8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("roster_main_scroll")
                    .auto_shrink([false, false])
                    .scroll_source(super::GESTURE_AREA_SCROLL)
                    .show(ui, |ui| {
                        ui.set_max_width((ui.available_width() - 4.0).max(400.0));
                        match self.tab {
                            RosterTab::Library => self.draw_library(ui),
                            RosterTab::CharacterSelect => {
                                let roots = self.roots.clone();
                                self.css.ui(ui, &roots, &self.index, &mut self.project);
                                if self.css.take_goto_new_character() {
                                    self.tab = RosterTab::NewCharacter;
                                }
                            }
                            RosterTab::NewCharacter => {
                                let roots = self.roots.clone();
                                let action = {
                                    let mut view = std::mem::take(&mut self.new_character);
                                    let action = view.ui(
                                        ui,
                                        &roots,
                                        &self.index,
                                        &self.project,
                                        self.edit_target.as_ref(),
                                        &self.labels,
                                        &self.authored_moves,
                                    );
                                    let goto_css = view.take_goto_character_select();
                                    self.new_character = view;
                                    if let Some(key) = goto_css {
                                        self.select_css(key);
                                    }
                                    action
                                };
                                self.handle_new_character(action);
                            }
                            RosterTab::Traits => {
                                let roots = self.roots.clone();
                                let labels = std::mem::take(&mut self.labels);
                                self.traits
                                    .ui(ui, &roots, &labels, &self.index, &mut self.params);
                                self.labels = labels;
                            }
                        }
                    });
            });

        if !self.status.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(120, 185, 235), "●");
                ui.label(RichText::new(&self.status).small().weak());
            });
        }
    }

    /// Take a pending "save the project as part of deploying" request.
    ///
    /// Deploy stages from in-memory state; without a save alongside it, the
    /// staged files can run ahead of the project file on disk, and a crash
    /// loses everything since the last save. `app.rs` polls this after the
    /// window draws and saves silently when the project has a path.
    pub fn take_save_request(&mut self) -> bool {
        std::mem::replace(&mut self.save_requested, false)
    }

    /// Note the outcome of the save `app.rs` performed for a deploy, so the
    /// status line reads as one story: what was persisted, then staged.
    /// `false` covers both "no path yet" and a failed write — the latter is
    /// already reported in the main window's own status line.
    pub fn note_deploy_saved(&mut self, saved: bool) {
        if saved {
            self.status = format!("Saved project. {}", self.status);
        } else {
            self.status = format!(
                "{}. Project isn't saved — Save it to keep these edits.",
                self.status
            );
        }
    }

    /// Stage the roster edits onto the emulator SD, for the edits no live
    /// path can carry (slots, models, order, names, traits). Reboot the
    /// emulator yourself afterwards — these files are read at boot.
    ///
    /// Synchronous: the staging is a few small files. Also raises
    /// `save_requested` so `app.rs` persists the project (roster edits
    /// included) alongside the staged files.
    fn deploy_to_emulator(&mut self, fighters: &[FighterEntry], data_root: Option<&PathBuf>) {
        let summary = self.export_summary();
        if summary.order + summary.hidden + summary.names + summary.authored + summary.params == 0 {
            self.status = "No roster or trait edits to deploy yet.".into();
            return;
        }
        let Some(sd) = crate::scratch_dirs::emulator_sd_root() else {
            self.status = "Emulator SD card not found. Set VISIONARY_SD_DIR to the folder \
                 containing your emulator's `ultimate` directory."
                .into();
            return;
        };
        let roster = self.project.clone();
        let fighters_owned: Vec<FighterEntry> = fighters.to_vec();
        let data_root_owned = data_root.cloned();
        let staged = crate::emulator::stage_dev_mod(&sd, |dest| {
            self.export_into(dest, &roster, &fighters_owned, data_root_owned.as_ref())
                .map(|report| (report.files, report.warnings))
        });
        let (_dest, (files, warnings)) = match staged {
            Ok(done) => done,
            Err(error) => {
                self.status = format!("Deploy failed: {error:#}");
                return;
            }
        };
        if files.is_empty() {
            // Warnings explain *why* nothing staged (a missing ui/ dump, a
            // character with no files yet) — dropping them here is how a
            // no-op deploy used to read as a success.
            self.status = match warnings.first() {
                Some(first) => format!("Nothing staged: {first}"),
                None => "Nothing staged — the export reported no files.".into(),
            };
            return;
        }
        // The status line fits one line; the full list lives in the manual
        // export tree. Name the count and the first warning, if any.
        let warn_note = match warnings.as_slice() {
            [] => String::new(),
            [first] => format!(" Note: {first}"),
            [first, rest @ ..] => format!(" Note: {first} (+{} more)", rest.len()),
        };
        let count = files.len();
        self.save_requested = true;
        self.status = format!(
            "Staged {count} file(s) to visionary_dev.{warn_note} Reboot the emulator to see them."
        );
    }

    fn draw_top_bar(
        &mut self,
        ui: &mut Ui,
        fighters: &[FighterEntry],
        data_root: Option<&PathBuf>,
    ) {
        let summary = self.export_summary();
        let total_edits =
            summary.order + summary.hidden + summary.names + summary.params + summary.authored;
        let lib_mods = self.library.mods.len();
        let enabled = self.library.mods.iter().filter(|m| m.enabled).count();

        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for tab in RosterTab::ALL {
                        let badge = tab.badge_count(self);
                        let mut text = format!("{}  {}", tab.icon(), tab.title());
                        if let Some(n) = badge {
                            text.push_str(&format!("  • {n}"));
                        }
                        // Full descriptions live on hover; the bar itself
                        // stays one row so the workspace below owns the view.
                        let resp = ui.selectable_value(&mut self.tab, tab, text);
                        resp.on_hover_text(tab.description());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                total_edits > 0,
                                egui::Button::new(RichText::new("Deploy")),
                            )
                            .on_hover_text(
                                "Roster edits need a reboot to take effect: saves the project, \
                                 stages the edits to the visionary_dev mod on the emulator SD. \
                                 Reboot the emulator yourself to see them.",
                            )
                            .clicked()
                        {
                            let owned: Vec<FighterEntry> = fighters.to_vec();
                            let root = data_root.cloned();
                            self.deploy_to_emulator(&owned, root.as_ref());
                        }
                        ui.label(
                            RichText::new(if total_edits == 0 {
                                "No edits".to_string()
                            } else {
                                format!(
                                    "{total_edits} edit{}",
                                    if total_edits == 1 { "" } else { "s" }
                                )
                            })
                            .small()
                            .strong()
                            .color(if total_edits == 0 {
                                ui.visuals().weak_text_color()
                            } else {
                                Color32::from_rgb(240, 210, 130)
                            }),
                        );
                    });
                });
                // One context line: roster size, library state, and what an
                // export would carry. Only the export part is conditional.
                let mut context =
                    format!("{} fighters · {enabled}/{lib_mods} mods", fighters.len());
                if total_edits > 0 {
                    let mut parts = Vec::new();
                    if summary.order + summary.hidden > 0 {
                        parts.push(format!(
                            "{} position/visibility",
                            summary.order + summary.hidden
                        ));
                    }
                    if summary.names > 0 {
                        parts.push(format!(
                            "{} name{}",
                            summary.names,
                            if summary.names == 1 { "" } else { "s" }
                        ));
                    }
                    if summary.authored > 0 {
                        parts.push(format!("{} new", summary.authored));
                    }
                    if summary.params > 0 {
                        parts.push(format!("{} traits", summary.params));
                    }
                    context.push_str(&format!(" · will export {}", parts.join(" · ")));
                }
                ui.label(RichText::new(context).small().weak());
            });
        ui.add_space(8.0);
    }

    /// Carry out what the new-character tab asked for.
    ///
    /// Performed here rather than inside the panel so that every mutation of the library and
    /// the project goes through one place — the library has to be saved and the fighter index
    /// invalidated on any change, and a second path that forgot either would leave the new
    /// character invisible until the next restart.
    fn handle_new_character(&mut self, action: NewCharacterAction) {
        match action {
            NewCharacterAction::None => {}
            NewCharacterAction::Create {
                donor,
                slots,
                display_name,
                name_id,
                destination,
            } => {
                let create_res = new_character::create_and_import_range(
                    &mut self.library,
                    &destination,
                    &donor,
                    &slots,
                    &display_name,
                );
                match create_res {
                    Ok(scaffolded) => {
                        let mut entry = new_character::authored_entry_multi(
                            &donor,
                            &slots,
                            &display_name,
                            &name_id,
                        );
                        // Remember where the files went: the panels reveal
                        // this folder and report on its contents. Computed the
                        // same way `create_and_import` computes it.
                        entry.files_root =
                            Some(destination.join(crate::mod_export::slugify(&display_name)));
                        // The name is recorded as a roster edit as well as on the entry, so it
                        // reaches the .xmsbt the export writes. The entry's own field is what
                        // the panel shows; the override is what the game reads.
                        self.project
                            .names
                            .insert(entry.key.clone(), display_name.clone());
                        self.project
                            .authored
                            .retain(|existing| existing.key != entry.key);
                        self.project.authored.push(entry);
                        self.new_character.note_created(&display_name, &scaffolded);
                        self.commit_library_change();
                    }
                    Err(error) => self.new_character.note_error(format!("{error:#}")),
                }
            }
            NewCharacterAction::SetEditTarget(target) => {
                self.status = match &target {
                    Some(key) => format!(
                        "Moves you edit now belong to {key}. The donor's other costumes keep \
                         their own version of anything you change."
                    ),
                    None => "Moves you edit now apply to every costume again.".to_string(),
                };
                self.edit_target = target;
            }
            NewCharacterAction::Remove(key) => {
                if self.edit_target.as_ref() == Some(&key) {
                    self.edit_target = None;
                }
                self.project.authored.retain(|entry| entry.key != key);
                self.project.names.remove(&key);
                self.project.name_variants.remove(&key);
                self.project.order.remove(&key);
                self.project.hidden.remove(&key);
                self.project.ui_images.remove(&key);
                self.project.chara_overrides.remove(&key);
                self.status =
                    format!("Removed {key} from the project. Its files are still on disk.");
            }
        }
    }

    // ── Library tab ─────────────────────────────────────────────────────────

    fn draw_library(&mut self, ui: &mut Ui) {
        // Header with import actions — uses Frame::group like the main sidebar headings
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("▦  Mod library");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(RichText::new("  ⟳ Rescan  ").small()))
                            .on_hover_text("Re-read every imported mod's files from disk")
                            .clicked()
                        {
                            self.rescan();
                        }
                        ui.add_space(4.0);
                        if ui
                            .add(egui::Button::new(RichText::new("  ＋ Import folder  ").small().strong()))
                            .on_hover_text(
                                "Pick a folder containing fighter/, effect/, or ui/ — or a folder that \
                                 wraps one.",
                            )
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("Import mod folder")
                                .pick_folder()
                            {
                                self.start_import(vec![path]);
                            }
                        }
                        if ui
                            .add(egui::Button::new(RichText::new("  ＋ Import archives  ").small().strong()))
                            .on_hover_text(
                                "Pick any number of .zip / .7z mod archives. Archives are extracted for you; \
                                 folders are used in place, so your own edits stay visible.",
                            )
                            .clicked()
                        {
                            if let Some(paths) = rfd::FileDialog::new()
                                .set_title("Import mod archives")
                                .add_filter("Mod archive", super::archive::SUPPORTED)
                                .pick_files()
                            {
                                self.start_import(paths);
                            }
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Import compiled mods here — folders or .zip/.7z archives, many at once. \
                         Load order decides conflicts: later mods win any file two mods both provide.",
                    )
                    .small()
                    .weak(),
                );
            });
        ui.add_space(8.0);

        if let Some(job) = &self.import {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "Importing {} of {} — {}",
                                    job.done + 1,
                                    job.total,
                                    job.current
                                ))
                                .small()
                                .strong(),
                            );
                            let frac = if job.total > 0 {
                                (job.done as f32) / job.total as f32
                            } else {
                                0.0
                            };
                            ui.add(
                                egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                                    .desired_width(ui.available_width().min(320.0))
                                    .desired_height(4.0),
                            );
                        });
                    });
                });
            ui.add_space(6.0);
        }

        self.draw_notes(ui);

        if self.library.is_empty() {
            ui.add_space(6.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("◇  No mods imported").size(13.0).strong());
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Visionary reads fighters, skins, and effects from your game data — mods \
                                 add or replace them. Import a mod folder or archive above to get started.",
                            )
                            .small()
                            .weak(),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Tip: your own authored characters appear here as imported mods, so load order and conflicts stay visible.")
                                .small()
                                .weak(),
                        );
                    });
                });
            // Still surface stale overrides even with empty library so hidden project state is not invisible
            self.draw_stale_overrides(ui);
            return;
        }

        self.draw_stale_overrides(ui);
        self.draw_conflicts(ui);
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new("⇅  Load order").small().strong());
            ui.label(
                RichText::new(
                    "— later mods win any file an earlier one also provides. Use ▲ / ▼ to reorder.",
                )
                .small()
                .weak(),
            );
        });
        ui.add_space(6.0);

        self.draw_mod_rows(ui);
    }

    fn draw_mod_rows(&mut self, ui: &mut Ui) {
        let ids: Vec<ProviderId> = self.library.mods.iter().map(|entry| entry.id).collect();
        let mut changed = false;
        let mut remove: Option<ProviderId> = None;
        let mut move_later: Option<ProviderId> = None;
        let mut move_earlier: Option<ProviderId> = None;

        for id in ids {
            let Some(index) = self.library.mods.iter().position(|entry| entry.id == id) else {
                continue;
            };
            let is_enabled = self.library.mods[index].enabled;
            // Disabled mods use the theme's faint background so they read as muted
            // without a custom grey; enabled uses the standard group frame.
            let mut frame = egui::Frame::group(ui.style());
            if !is_enabled {
                frame = frame.fill(ui.visuals().faint_bg_color.gamma_multiply(0.6));
            }
            frame
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let entry = &mut self.library.mods[index];
                        let mut enabled = entry.enabled;
                        if ui.checkbox(&mut enabled, "").on_hover_text("Enable / disable this mod").changed() {
                            entry.enabled = enabled;
                            changed = true;
                        }

                        let renaming = matches!(&self.rename, Some((rid, _)) if *rid == id);
                        if renaming {
                            if let Some((_, buffer)) = &mut self.rename {
                                let response = ui.add(
                                    egui::TextEdit::singleline(buffer)
                                        .desired_width(160.0)
                                        .hint_text("mod name"),
                                );
                                if response.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    let name = buffer.trim().to_string();
                                    if !name.is_empty() {
                                        self.library.mods[index].name = name;
                                        changed = true;
                                    }
                                    self.rename = None;
                                }
                                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    self.rename = None;
                                }
                            }
                        } else {
                            let entry = &self.library.mods[index];
                            let label = ui.selectable_label(false, RichText::new(&entry.name).strong());
                            if label.on_hover_text("Click to rename").clicked() {
                                self.rename = Some((id, entry.name.clone()));
                            }
                        }

                        let entry = &self.library.mods[index];
                        ui.label(
                            RichText::new(entry.summary())
                                .small()
                                .weak(),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(RichText::new(" ✕ ").small())
                                .on_hover_text("Remove this mod from the library")
                                .clicked()
                            {
                                remove = Some(id);
                            }
                            ui.add_space(4.0);
                            if ui
                                .add_enabled(
                                    index + 1 < self.library.mods.len(),
                                    egui::Button::new(RichText::new(" ▼ ").small()),
                                )
                                .on_hover_text("Later in load order — wins more conflicts")
                                .clicked()
                            {
                                move_later = Some(id);
                            }
                            if ui
                                .add_enabled(index > 0, egui::Button::new(RichText::new(" ▲ ").small()))
                                .on_hover_text("Earlier in load order — wins fewer conflicts")
                                .clicked()
                            {
                                move_earlier = Some(id);
                            }
                            if !is_enabled {
                                ui.colored_label(Color32::from_rgb(180, 160, 120), RichText::new("disabled").small());
                            }
                        });
                    });

                    let entry = &self.library.mods[index];

                    // Safety-critical and always visible: a shipped plugin
                    // means its movesets read as vanilla in the editor.
                    if entry.ships_plugin() {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.colored_label(Color32::from_rgb(240, 200, 120), "⚠");
                            ui.label(
                                RichText::new(format!(
                                    "Ships a compiled plugin ({}). The editor cannot read it, so its movesets appear as vanilla.",
                                    entry.manifest.plugins.join(", ")
                                ))
                                .small()
                                .weak(),
                            );
                        });
                    }

                    // Everything else about the mod — root guess, fighter
                    // list, ignored files, file list — lives behind one
                    // expander so the load-order list scans as one row per
                    // mod instead of a paragraph each.
                    ui.add_space(4.0);
                    let expanded = self.expanded == Some(id);
                    if ui
                        .small_button(if expanded { "▾ Hide details" } else { "▸ Details & files" })
                        .on_hover_text("Root detection, fighters, and the file list")
                        .clicked()
                    {
                        self.expanded = (!expanded).then_some(id);
                    }
                    if expanded {
                        ui.add_space(4.0);
                        let entry = &self.library.mods[index];
                        // A mod whose root was guessed one level too high provides nothing and looks
                        // identical to a mod that contains no fighters. Say which happened.
                        if !matches!(entry.detection, library::RootDetection::AsGiven) {
                            ui.label(
                                RichText::new(format!("↳ Root: {}", entry.detection.describe()))
                                    .small()
                                    .weak(),
                            );
                        }
                        if !entry.manifest.fighters.is_empty() {
                            let summary = entry
                                .manifest
                                .fighters
                                .iter()
                                .map(|(fighter, provision)| {
                                    if provision.slots.is_empty() {
                                        fighter.clone()
                                    } else {
                                        let slots = provision
                                            .slots
                                            .iter()
                                            .map(|slot| format!("c{slot:02}"))
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        format!("{fighter} ({slots})")
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("  ·  ");
                            ui.label(RichText::new(summary).small().weak());
                        }
                        if entry.manifest.unrecognized > 0 {
                            ui.label(
                                RichText::new(format!(
                                    "{} file(s) outside game folders were ignored (readmes, screenshots, etc.).",
                                    entry.manifest.unrecognized
                                ))
                                .small()
                                .weak(),
                            );
                        }
                        ui.add_space(4.0);
                        let paths: Vec<String> = self.library.mods[index]
                            .manifest
                            .paths
                            .iter()
                            .cloned()
                            .collect();
                        egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt(("roster_mod_files", id.0))
                                    .max_height(180.0)
                                    .show(ui, |ui| {
                                        for path in paths {
                                            ui.label(RichText::new(path).small().monospace().weak());
                                        }
                                    });
                            });
                    }
                });
            ui.add_space(6.0);
        }

        if let Some(id) = move_later {
            self.library.move_later(id);
            changed = true;
        }
        if let Some(id) = move_earlier {
            self.library.move_earlier(id);
            changed = true;
        }
        if let Some(id) = remove {
            self.library.remove(id);
            changed = true;
        }
        if changed {
            self.commit_library_change();
        }
    }

    fn draw_conflicts(&mut self, ui: &mut Ui) {
        let by_fighter = self.library.conflicts_by_fighter();
        let total: usize = by_fighter.values().map(|list| list.len()).sum();
        if total == 0 {
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(130, 225, 150), "✓");
                ui.label(
                    RichText::new("No file conflicts between enabled mods.")
                        .small()
                        .weak(),
                );
            });
            return;
        }
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(240, 200, 120), "⚠");
                    ui.label(
                        RichText::new(format!(
                            "{total} file(s) provided by more than one enabled mod — later in load order wins."
                        ))
                        .small()
                        .strong(),
                    );
                });
                ui.add_space(4.0);
                for (fighter, conflicts) in by_fighter {
                    let expanded = self.expanded_conflict.as_deref() == Some(fighter.as_str());
                    let winners: Vec<String> = {
                        let mut names: Vec<String> = conflicts
                            .iter()
                            .filter_map(|conflict| conflict.winner())
                            .map(|id| self.library.name_of(id))
                            .collect();
                        names.sort();
                        names.dedup();
                        names
                    };
                    let header = format!(
                        "{fighter}: {} file(s), won by {}",
                        conflicts.len(),
                        winners.join(", ")
                    );
                    let resp = ui.selectable_label(expanded, RichText::new(header).small());
                    if resp.clicked() {
                        self.expanded_conflict = (!expanded).then(|| fighter.clone());
                    }
                    if expanded {
                        egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt(("roster_conflicts", fighter.clone()))
                                    .max_height(160.0)
                                    .show(ui, |ui| {
                                        for conflict in &conflicts {
                                            let chain = conflict
                                                .providers
                                                .iter()
                                                .map(|id| self.library.name_of(*id))
                                                .collect::<Vec<_>>()
                                                .join(" → ");
                                            ui.label(
                                                RichText::new(format!("{}  [{}]", conflict.game_path, chain))
                                                    .small()
                                                    .monospace()
                                                    .weak(),
                                            );
                                        }
                                    });
                            });
                    }
                }
            });
    }

    /// Project edits that no longer have an entry behind them.
    ///
    /// Shown in the library panel because disabling a mod is how they usually become stale,
    /// and because the fix — re-enable the mod — is right here.
    fn draw_stale_overrides(&mut self, ui: &mut Ui) {
        if self.index.stale_overrides.is_empty() {
            return;
        }
        ui.add_space(6.0);
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(240, 140, 140), "⚠");
                    ui.label(
                        RichText::new(format!(
                            "{} project edit(s) with no matching character — kept, not discarded. Re-enable the mod that provided it to restore them.",
                            self.index.stale_overrides.len()
                        ))
                        .small()
                        .strong(),
                    );
                });
                ui.add_space(4.0);
                for stale in &self.index.stale_overrides {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("•").small().weak());
                        ui.label(
                            RichText::new(format!("{} — {}", stale.key, stale.kind.describe()))
                                .small()
                                .weak(),
                        );
                    });
                }
                if ui.small_button("Clear stale edits").on_hover_text("Discard edits that have no character behind them").clicked() {
                    // Remove stale keys from all maps; keep authored list filtered
                    let stale_keys: std::collections::BTreeSet<_> = self.index.stale_overrides.iter().map(|s| &s.key).collect();
                    self.project.order.retain(|k, _| !stale_keys.contains(k));
                    self.project.hidden.retain(|k| !stale_keys.contains(k));
                    self.project.names.retain(|k, _| !stale_keys.contains(k));
                    self.project.name_variants.retain(|k, _| !stale_keys.contains(k));
                    self.project.ui_images.retain(|k, _| !stale_keys.contains(k));
                    self.project.chara_overrides.retain(|k, _| !stale_keys.contains(k));
                    self.project.authored.retain(|e| !stale_keys.contains(&e.key));
                    // Per-costume names are keyed by fighter string, not RosterKey; drop fighters that are now stale.
                    self.project.per_costume_names.retain(|fighter, _| {
                        let key = crate::roster::RosterKey::fighter(fighter);
                        !stale_keys.contains(&key)
                    });
                }
            });
    }

    fn draw_notes(&mut self, ui: &mut Ui) {
        if self.notes.is_empty() {
            return;
        }
        ui.add_space(4.0);
        let failures = self
            .notes
            .iter()
            .filter(|note| note.outcome.is_err())
            .count();
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let ok = self.notes.len() - failures;
                    ui.label(
                        RichText::new(format!("Last import: {ok} succeeded, {failures} failed"))
                            .small()
                            .strong()
                            .color(if failures == 0 {
                                Color32::from_rgb(130, 225, 150)
                            } else {
                                Color32::from_rgb(240, 200, 120)
                            }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Clear").clicked() {
                            self.notes.clear();
                        }
                    });
                });
                ui.add_space(4.0);
                for note in &self.notes {
                    match &note.outcome {
                        Ok(detail) => ui.horizontal(|ui| {
                            ui.colored_label(Color32::from_rgb(130, 225, 150), "✔");
                            ui.label(
                                RichText::new(format!("{} — {detail}", note.source))
                                    .small()
                                    .weak(),
                            );
                        }),
                        Err(error) => ui.horizontal(|ui| {
                            ui.colored_label(Color32::from_rgb(240, 140, 140), "✘");
                            ui.label(
                                RichText::new(format!("{} — {error}", note.source))
                                    .small()
                                    .weak(),
                            );
                        }),
                    };
                }
            });
        ui.add_space(4.0);
    }

    // ── Import ──────────────────────────────────────────────────────────────

    fn start_import(&mut self, paths: Vec<PathBuf>) {
        if self.import.is_some() {
            self.status = "An import is already running — wait for it to finish.".into();
            return;
        }
        if paths.is_empty() {
            return;
        }
        let total = paths.len();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.notes.clear();
        std::thread::spawn(move || import_worker(paths, sender));
        self.import = Some(ImportJob {
            receiver,
            done: 0,
            total,
            current: String::new(),
        });
    }

    fn poll_import(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.import else { return };
        let mut finished = false;
        let mut inserted = Vec::new();
        let mut failures = Vec::new();
        loop {
            match job.receiver.try_recv() {
                Ok(ImportMsg::Progress { done, total, name }) => {
                    job.done = done;
                    job.total = total;
                    job.current = name;
                }
                Ok(ImportMsg::Prepared(prepared)) => inserted.push(*prepared),
                Ok(ImportMsg::Failed { source, error }) => failures.push((source, error)),
                Ok(ImportMsg::Finished) => finished = true,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }

        let progressed = !inserted.is_empty() || !failures.is_empty();
        for prepared in inserted {
            let source = prepared.source.origin_path().display().to_string();
            let detail = format!(
                "{} file(s), {} fighter(s); {}",
                prepared.manifest.paths.len(),
                prepared.manifest.fighters.len(),
                prepared.detection.describe()
            );
            self.library.insert(prepared);
            self.notes.push(ImportNote {
                source,
                outcome: Ok(detail),
            });
        }
        for (source, error) in failures {
            self.notes.push(ImportNote {
                source,
                outcome: Err(error),
            });
        }
        if progressed {
            self.commit_library_change();
        }
        if finished {
            self.import = None;
            let failed = self
                .notes
                .iter()
                .filter(|note| note.outcome.is_err())
                .count();
            self.status = format!(
                "Imported {} mod(s), {failed} failed.",
                self.notes.len() - failed
            );
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn rescan(&mut self) {
        let mut rescanned = 0;
        let mut lost = Vec::new();
        for entry in &mut self.library.mods {
            match library::scan_manifest(&entry.root) {
                Ok(manifest) => {
                    entry.manifest = manifest;
                    rescanned += 1;
                }
                Err(error) => lost.push(format!("{}: {error}", entry.name)),
            }
        }
        self.commit_library_change();
        self.status = if lost.is_empty() {
            format!("Rescanned {rescanned} mod(s).")
        } else {
            format!("Rescanned {rescanned}; failed: {}", lost.join("; "))
        };
    }

    /// Persist the library and tell `app.rs` its fighter index is stale.
    ///
    /// Every mutation routes through here. A path that changed the library without setting
    /// the dirty flag would leave the fighter list showing a mod the user just disabled.
    fn commit_library_change(&mut self) {
        library::save(&self.library);
        self.library_dirty = true;
        // A newly enabled mod may now provide the roster database, or win a portrait an
        // earlier root was providing. Both are cached, so both have to be dropped here.
        self.css.invalidate();
        self.traits.invalidate();
    }
}

/// Summary of roster edits for the export tree and header badges.
#[derive(Debug, Clone, Default)]
pub struct RosterExportSummary {
    pub order: usize,
    pub hidden: usize,
    pub names: usize,
    pub authored: usize,
    pub params: usize,
}

/// One roster edit kind the Edit Log can revert wholesale. New characters
/// are listed but never reverted from the log — removing a character
/// deserves its own confirmation where its files live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterLogClear {
    Positions,
    Hidden,
    DisplayNames,
    NameVariants,
    CostumeNames,
    Images,
    RowPatches,
}

/// Prepare each import on a worker thread.
///
/// Extraction and the manifest walk are both filesystem-bound and a large mod archive takes
/// seconds; doing this inline froze the window for the whole batch. Each item reports its own
/// outcome, so one corrupt archive costs that archive and nothing else.
fn import_worker(paths: Vec<PathBuf>, sender: Sender<ImportMsg>) {
    let total = paths.len();
    for (index, path) in paths.into_iter().enumerate() {
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        if sender
            .send(ImportMsg::Progress {
                done: index,
                total,
                name: label.clone(),
            })
            .is_err()
        {
            return;
        }

        let source = if super::archive::is_archive(&path) {
            match super::archive::extract(&path) {
                Ok(extracted) => ModSource::Archive {
                    archive: path.clone(),
                    extracted,
                },
                Err(error) => {
                    let _ = sender.send(ImportMsg::Failed {
                        source: label,
                        error: format!("{error:#}"),
                    });
                    continue;
                }
            }
        } else {
            ModSource::Folder(path.clone())
        };

        match library::prepare(source, None) {
            Ok(prepared) => {
                let _ = sender.send(ImportMsg::Prepared(Box::new(prepared)));
            }
            Err(error) => {
                let _ = sender.send(ImportMsg::Failed {
                    source: label,
                    error: format!("{error:#}"),
                });
            }
        }
    }
    let _ = sender.send(ImportMsg::Finished);
}
