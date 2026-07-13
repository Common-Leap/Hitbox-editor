/// Main egui application for the SSBU Hitbox Editor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use egui::{Color32, RichText, ScrollArea, Ui};
use glam;
use crate::data::{AppState, Hitbox, MoveEntry, fighter_display_name};
use crate::acmd::{fetch_script_body};
use crate::renderer::{HitboxRenderState, ViewportCallback};

// ── Enum combo helpers ────────────────────────────────────────────────────────

fn enum_combo<'a>(ui: &mut egui::Ui, value: &mut String, id: &str, label: &str, options: &[&'a str]) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(value.as_str())
            .show_ui(ui, |ui| {
                for &opt in options {
                    ui.selectable_value(value, opt.to_string(), opt);
                }
            });
    });
}

fn setoff_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Setoff Kind:", &[
        "ATTACK_SETOFF_KIND_ON", "ATTACK_SETOFF_KIND_OFF", "ATTACK_SETOFF_KIND_THRU",
    ]);
}

fn lr_check_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "LR Check:", &[
        "ATTACK_LR_CHECK_POS", "ATTACK_LR_CHECK_F", "ATTACK_LR_CHECK_B",
    ]);
}

fn situation_mask_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Situation Mask:", &[
        "COLLISION_SITUATION_MASK_GA",
        "COLLISION_SITUATION_MASK_G",
        "COLLISION_SITUATION_MASK_A",
        "COLLISION_SITUATION_MASK_GA_d",
    ]);
}

fn category_mask_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Category Mask:", &[
        "COLLISION_CATEGORY_MASK_ALL",
        "COLLISION_CATEGORY_MASK_FIGHTER",
        "COLLISION_CATEGORY_MASK_ITEM",
        "COLLISION_CATEGORY_MASK_OBJECT",
    ]);
}

fn part_mask_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Part Mask:", &[
        "COLLISION_PART_MASK_ALL",
        "COLLISION_PART_MASK_BODY",
        "COLLISION_PART_MASK_HEAD",
        "COLLISION_PART_MASK_BODY_HEAD",
    ]);
}

fn collision_attr_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Collision Attr:", &[
        "collision_attr_normal",
        "collision_attr_fire",
        "collision_attr_electric",
        "collision_attr_ice",
        "collision_attr_water",
        "collision_attr_grass",
        "collision_attr_darkness",
        "collision_attr_aura",
        "collision_attr_magic",
        "collision_attr_none",
        "collision_attr_coin",
        "collision_attr_bury",
        "collision_attr_sleep",
        "collision_attr_stun",
        "collision_attr_slip",
        "collision_attr_flower",
        "collision_attr_reverse",
        "collision_attr_reflector",
        "collision_attr_absorber",
        "collision_attr_absorber_needle",
        "collision_attr_sting",
        "collision_attr_bomb",
        "collision_attr_curse",
        "collision_attr_paralyze",
        "collision_attr_deaf",
        "collision_attr_rock",
        "collision_attr_turn",
        "collision_attr_cutup",
        "collision_attr_capcut",
        "collision_attr_shield_ignore",
        "collision_attr_ink",
        "collision_attr_rush",
        "collision_attr_saving",
    ]);
}

fn sound_level_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Sound Level:", &[
        "ATTACK_SOUND_LEVEL_S",
        "ATTACK_SOUND_LEVEL_M",
        "ATTACK_SOUND_LEVEL_L",
        "ATTACK_SOUND_LEVEL_LL",
        "ATTACK_SOUND_LEVEL_XL",
    ]);
}

fn sound_attr_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Sound Attr:", &[
        "COLLISION_SOUND_ATTR_PUNCH",
        "COLLISION_SOUND_ATTR_KICK",
        "COLLISION_SOUND_ATTR_FIRE",
        "COLLISION_SOUND_ATTR_ELECTRIC",
        "COLLISION_SOUND_ATTR_ICE",
        "COLLISION_SOUND_ATTR_WATER",
        "COLLISION_SOUND_ATTR_MAGIC",
        "COLLISION_SOUND_ATTR_COIN",
        "COLLISION_SOUND_ATTR_CUTUP",
        "COLLISION_SOUND_ATTR_BOMB",
        "COLLISION_SOUND_ATTR_NONE",
        "COLLISION_SOUND_ATTR_HEAVY",
        "COLLISION_SOUND_ATTR_BATBALL",
        "COLLISION_SOUND_ATTR_HARISEN",
        "COLLISION_SOUND_ATTR_ELEC",
        "COLLISION_SOUND_ATTR_SLEEP",
        "COLLISION_SOUND_ATTR_PARALYZE",
        "COLLISION_SOUND_ATTR_FLOWER",
        "COLLISION_SOUND_ATTR_SLIP",
        "COLLISION_SOUND_ATTR_STING",
        "COLLISION_SOUND_ATTR_RUSH",
    ]);
}

fn attack_region_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(ui, v, id, "Attack Region:", &[
        "ATTACK_REGION_PUNCH",
        "ATTACK_REGION_KICK",
        "ATTACK_REGION_SWORD",
        "ATTACK_REGION_HAMMER",
        "ATTACK_REGION_THROW",
        "ATTACK_REGION_ENERGY",
        "ATTACK_REGION_BITE",
        "ATTACK_REGION_HEAD",
        "ATTACK_REGION_BODY",
        "ATTACK_REGION_OBJECT",
        "ATTACK_REGION_FIRE",
        "ATTACK_REGION_ICE",
        "ATTACK_REGION_WATER",
        "ATTACK_REGION_ELECTRIC",
        "ATTACK_REGION_MAGIC",
        "ATTACK_REGION_ITEM",
        "ATTACK_REGION_NONE",
        "ATTACK_REGION_BOMB",
        "ATTACK_REGION_WHIP",
        "ATTACK_REGION_TAIL",
        "ATTACK_REGION_COIN",
        "ATTACK_REGION_PIKMIN",
        "ATTACK_REGION_WING",
        "ATTACK_REGION_BREATH",
        "ATTACK_REGION_NEEDLE",
        "ATTACK_REGION_HAND",
        "ATTACK_REGION_UMBRELLA",
        "ATTACK_REGION_PARASOL",
        "ATTACK_REGION_ROPE",
        "ATTACK_REGION_CONTAINER",
        "ATTACK_REGION_HURLING",
        "ATTACK_REGION_SUPERKICK",
    ]);
}

/// Special angles used in SSBU hitboxes.
/// Values 365-368 are autolink angles; 361 is the Sakurai angle.
/// Note: 366 and 367 swapped roles between Smash 4 and Ultimate.
const SPECIAL_ANGLES: &[(&str, i32)] = &[
    ("Sakurai (361)",        361), // horizontal at low KB, diagonal at high KB
    ("Autolink 363",         363), // matches attacker movement, no launch speed mod
    ("Autolink 365",         365), // matches attacker movement, 50% speed
    ("Autolink 366",         366), // pull + momentum, no speed cap (less common)
    ("Autolink 367",         367), // pull + momentum, speed capped — most common in Ultimate multi-hits
    ("Autolink 368",         368), // pull + position vector (e.g. Samus up smash)
];

/// Short angle label for the hitbox list.
fn angle_short_label(angle: i32) -> String {
    match angle {
        361 => "Sakurai".to_string(),
        363 => "AL:363".to_string(),
        365 => "AL:365".to_string(),
        366 => "AL:366".to_string(),
        367 => "AL:367".to_string(),
        368 => "AL:368".to_string(),
        a   => format!("{}°", a),
    }
}

/// Draw an angle picker: a special-angle dropdown + a circular drag widget.
/// Smash Ultimate angle convention: 0=right, 90=up, 180=left, 270=down.
fn angle_picker(ui: &mut egui::Ui, angle: &mut i32) {
    let special_label = SPECIAL_ANGLES.iter()
        .find(|&&(_, v)| v == *angle)
        .map(|&(name, _)| name)
        .unwrap_or("Custom");

    // ── Dropdown + drag value ─────────────────────────────────────────────
    // Use a popup instead of ComboBox — ComboBox caches selection state and
    // can silently write it back on subsequent frames, corrupting the angle.
    ui.horizontal(|ui| {
        ui.label("Angle:");
        let popup_id = ui.make_persistent_id("angle_popup");
        let btn = ui.button(format!("▾ {special_label}"));
        if btn.clicked() {
            #[allow(deprecated)]
            {
                ui.memory_mut(|m| { let _ = m.toggle_popup(popup_id); });
            }
        }
        #[allow(deprecated)]
        egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(160.0);
            if ui.selectable_label(special_label == "Custom", "Custom (0°)").clicked() {
                *angle = 0;
                ui.memory_mut(|m| { let _ = m.close_popup(popup_id); });
            }
            for &(name, val) in SPECIAL_ANGLES {
                if ui.selectable_label(*angle == val, name).clicked() {
                    *angle = val;
                    ui.memory_mut(|m| m.close_popup(popup_id));
                }
            }
        });
        ui.add(egui::DragValue::new(angle).range(0..=368).suffix("°"));
    });

    // ── Circle diagram ────────────────────────────────────────────────────
    // Smash convention: 0=right, 90=up, 180=left, 270=down (standard math, CCW).
    // On screen Y is flipped, so we negate the Y component when drawing.
    let is_special = SPECIAL_ANGLES.iter().any(|&(_, v)| v == *angle);
    let dial_size = egui::vec2(80.0, 80.0);
    let (rect, response) = ui.allocate_exact_size(dial_size, egui::Sense::click_and_drag());

    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.45;
    let painter = ui.painter_at(rect);

    painter.circle_filled(center, radius, egui::Color32::from_rgb(30, 30, 45));
    painter.circle_stroke(center, radius, egui::Stroke::new(1.5, egui::Color32::from_gray(80)));

    // Cardinal tick marks at 0/90/180/270 and diagonals
    for deg in [0u32, 45, 90, 135, 180, 225, 270, 315] {
        // smash angle → screen direction: x=cos(a), y=-sin(a) (flip Y for screen)
        let rad = (deg as f32).to_radians();
        let dir = egui::vec2(rad.cos(), -rad.sin());
        let tick = if deg % 90 == 0 { 6.0 } else { 3.0 };
        let outer = center + dir * radius;
        let inner = center + dir * (radius - tick);
        painter.line_segment([inner, outer], egui::Stroke::new(1.0, egui::Color32::from_gray(60)));
    }

    // Angle indicator
    let display_angle = if is_special { 0 } else { *angle };
    let rad = (display_angle as f32).to_radians();
    let dir = egui::vec2(rad.cos(), -rad.sin());
    let tip = center + dir * (radius - 4.0);
    let line_color = if is_special {
        egui::Color32::from_rgb(180, 180, 60)
    } else {
        egui::Color32::from_rgb(255, 100, 100)
    };
    painter.line_segment([center, tip], egui::Stroke::new(2.0, line_color));
    painter.circle_filled(tip, 4.0, line_color);
    painter.circle_filled(center, 3.0, egui::Color32::from_gray(180));

    // Label below dial
    let label_text = if is_special {
        SPECIAL_ANGLES.iter()
            .find(|&&(_, v)| v == *angle)
            .map(|&(name, _)| name.to_string())
            .unwrap_or_else(|| format!("{}°", angle))
    } else {
        format!("{}°", angle)
    };
    painter.text(
        center + egui::vec2(0.0, radius + 10.0),
        egui::Align2::CENTER_TOP,
        &label_text,
        egui::FontId::monospace(10.0),
        egui::Color32::from_gray(200),
    );

    // Drag/click to set angle — only for non-special angles
    if !is_special && (response.dragged() || response.clicked()) {
        if let Some(pos) = response.interact_pointer_pos() {
            let delta = pos - center;
            if delta.length() > 2.0 {
                // Screen delta → Smash angle:
                // screen x right = smash 0°, screen y up (negative on screen) = smash 90°
                // atan2 in Smash space: atan2(-delta.y, delta.x)
                let smash_angle = (-delta.y).atan2(delta.x).to_degrees();
                *angle = smash_angle.rem_euclid(360.0).round() as i32;
            }
        }
    }

    // Description for special angles
    if is_special {
        let desc = match *angle {
            361 => "Horizontal at low KB, diagonal at high KB",
            363 => "Matches attacker movement, no speed mod",
            365 => "Matches attacker movement, 50% speed",
            366 => "Pull + momentum, no speed cap",
            367 => "Pull + momentum, speed capped (most common)",
            368 => "Pull + position vector",
            _   => "",
        };
        if !desc.is_empty() {
            ui.label(egui::RichText::new(desc)
                .small()
                .color(egui::Color32::from_rgb(180, 180, 60)));
        }
    }
}

/// System/root bones in Smash Ultimate whose hitbox offsets are in world space,
/// not bone local space. For these we only use the bone's translation.
/// Effect-kind id for the game link: hash40 of the lowercase effect name, or the literal
/// hash for names ACMD parsing left unresolved ("0x…").
fn effect_name_hash(name: &str) -> u64 {
    if let Some(hex) = name.strip_prefix("0x") {
        if let Ok(h) = u64::from_str_radix(hex, 16) {
            return h;
        }
    }
    hash40::hash40(&name.to_lowercase()).0
}

fn is_system_bone(name: &str) -> bool {
    matches!(name.to_lowercase().as_str(),
        "top" | "trans" | "rot" | "throw" | "itemroot"
    )
}

fn hitbox_color(hitbox_type: u32) -> Color32 {
    match hitbox_type {
        0 => Color32::from_rgba_premultiplied(255, 68, 68, 180),
        1 => Color32::from_rgba_premultiplied(68, 136, 255, 180),
        2 => Color32::from_rgba_premultiplied(68, 255, 136, 180),
        3 => Color32::from_rgba_premultiplied(255, 221, 68, 180),
        _ => Color32::from_rgba_premultiplied(255, 255, 255, 180),
    }
}

/// Sliders that DON'T clamp typed input to the visual range — drag within the range, but
/// double-click and type any value (bigger or negative) to override it. Used across the
/// hitbox property editor so the ranges are just handles, not hard limits.
fn wide_slider_f32(
    ui: &mut Ui,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
) -> egui::Response {
    ui.add(
        egui::Slider::new(v, range)
            .text(text)
            .clamping(egui::SliderClamping::Never),
    )
}

fn wide_slider_i32(
    ui: &mut Ui,
    v: &mut i32,
    range: std::ops::RangeInclusive<i32>,
    text: &str,
) -> egui::Response {
    ui.add(
        egui::Slider::new(v, range)
            .text(text)
            .clamping(egui::SliderClamping::Never),
    )
}

fn wide_slider_u32(
    ui: &mut Ui,
    v: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    text: &str,
) -> egui::Response {
    ui.add(
        egui::Slider::new(v, range)
            .text(text)
            .clamping(egui::SliderClamping::Never),
    )
}

/// Preview color by collision family: attack keeps the per-type palette, grab = cyan,
/// wind = green. Used everywhere hitboxes are drawn so the three categories read distinctly.
fn hitbox_display_color(hb: &crate::data::Hitbox) -> Color32 {
    match hb.category {
        1 => Color32::from_rgba_premultiplied(80, 200, 255, 180),  // grab — cyan
        2 => Color32::from_rgba_premultiplied(120, 240, 140, 170), // wind — green
        _ => hitbox_color(hb.hitbox_type),
    }
}

/// Per-effect animation clock state, separate from the hitbox scrub frame.
/// Drives `ParticleSystem::step` with wall-clock time so effects animate over time.
struct ActiveEffect {
    /// Frames elapsed since this effect was spawned (advances at 60fps from wall clock).
    anim_clock: f32,
    /// Maximum particle lifetime in frames across all emitters in this effect.
    /// Clock stops advancing once `anim_clock >= max_lifetime`.
    max_lifetime: f32,
    /// Index into `ptcl.emitter_sets` for this effect.
    #[allow(dead_code)]
    emitter_set_idx: usize,
}

pub struct HitboxEditorApp {
    state: AppState,
    move_list: Vec<MoveEntry>,
    fetching_acmd: bool,
    acmd_error: Option<String>,
    show_add_hitbox: bool,
    add_bone: String,
    add_size: f32,
    add_damage: f32,
    add_angle: i32,
    add_kb_base: i32,
    add_kb_scaling: i32,
    selected_hitbox: Option<usize>,
    // Current model/anim paths for the viewport callback
    current_model_dir: Option<PathBuf>,
    current_anim_path: Option<PathBuf>,
    current_skel_path: Option<PathBuf>,
    // Pending model load (set when fighter selected, consumed in update)
    pending_model_load: Option<PathBuf>,
    last_frame_time: std::time::Instant,
    // Background move list loading
    move_list_receiver: Option<std::sync::mpsc::Receiver<Vec<MoveEntry>>>,
    // Cached bone names for dropdown
    bone_names: Vec<String>,
    show_debug: bool,
    show_edit_log: bool,
    export_dir: Option<PathBuf>,
    /// The selected fighter's resolved ef_*.eff — re-queued when the Eff Editor opens.
    current_eff_path: Option<PathBuf>,
    /// Effs opened from outside the game data root (most-recent first), persisted.
    recent_effs: Vec<PathBuf>,
    /// Background cargo-skyline build of an exported mod's source (status polled in update).
    export_build: Option<std::sync::Arc<std::sync::Mutex<ExportBuildState>>>,
    /// Connection the pin-sync check last ran for (plugin client id).
    pin_sync_client: Option<u64>,
    /// Set on new connection; when the settle window elapses, untracked game pins prompt.
    pin_sync_wait: Option<std::time::Instant>,
    /// Untracked in-game pins awaiting the user's Import / Reset / Ignore choice.
    pin_sync_prompt: Option<Vec<(u64, crate::game_link::LiveKind)>>,
    /// Live hitbox rules per "fighter/move" (the plugin gets the flattened union).
    hitbox_rules_store: HashMap<String, Vec<crate::game_link::HitboxRuleWire>>,
    /// Live effect spawn rules per "fighter/move" (suppress + per-spawn transform); the
    /// plugin gets the flattened union so edits persist across move switches.
    effect_rules_store: HashMap<String, Vec<crate::game_link::SpawnRuleWire>>,
    /// (move key, hitbox snapshot) — change detection for live hitbox pushes.
    hitbox_watch: Option<(String, Vec<crate::data::Hitbox>)>,
    hitbox_dirty_at: Option<std::time::Instant>,
    /// game_link.captures_seq at the last auto-populate check.
    captures_seen_seq: u64,
    /// One-Slot studio: entry pool across every eff under the export root.
    effect_pool: Option<crate::effect_pool::EffectPool>,
    show_one_slot: bool,
    one_slot_search: String,
    /// Search text for the Effects-panel effect-name picker (live kinds + pool).
    effect_pick_search: String,
    /// Whether the inline effect-name picker (next to the effect field) is expanded.
    effect_pick_open: bool,
    /// Selected donor: (file rel, entry name).
    one_slot_sel: Option<(String, String)>,
    one_slot_new_name: String,
    /// After a one-slot: uses of the donor effect offered for per-use redirect.
    redirect_prompt: Option<RedirectPrompt>,
    fighter_search: String,
    move_search: String,
    /// Last frame for which particles were simulated — used to detect backwards scrubs
    last_simulated_frame: u32,
    /// Per-effect animation clocks — each entry tracks one spawned effect's wall-clock time.
    active_effects: Vec<ActiveEffect>,
    /// Monotonic wall-clock accumulator for the particle simulation (seconds since last respawn).
    /// Used as fallback when active_effects is empty but emitters are present.
    particle_clock: f32,
    /// Set by respawn/load so the next sim pass runs integer catch-up 0..=current_frame.
    particles_need_catchup: bool,
    /// Instant of the last particle simulation step — used to compute dt independently
    /// of the hitbox scrub frame timer.
    particle_step_time: std::time::Instant,
    /// Eff-file editor with in-game live preview (replaces RPM).
    eff_editor: crate::eff_editor::EffEditor,
    /// TCP client to the slight_replica plugin (:7878).
    game_link: crate::game_link::GameLink,
    /// Debounced live-pin push for an edited effect call (index, last edit time).
    /// Shared per-kind runtime overrides (Effects panel + Eff Editor game panel).
    live_overrides: crate::game_link::LiveOverrides,
    /// Authored .eff edits per fighter (project store; synced from the eff editor).
    eff_mods: HashMap<String, crate::mod_project::EffMod>,
    project_name: String,
}

impl HitboxEditorApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        // Set dark theme with visible text
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        // Install image loaders — this also ensures font atlas is properly initialized
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let saved_data_root = load_config_path("data_root");
        let saved_export_dir = load_config_path("export_dir");

        let mut app = Self {
            state: AppState::default(),
            move_list: Vec::new(),
            fetching_acmd: false,
            acmd_error: None,
            show_add_hitbox: false,
            add_bone: "top".to_string(),
            add_size: 4.5,
            add_damage: 10.0,
            add_angle: 361,
            add_kb_base: 50,
            add_kb_scaling: 100,
            selected_hitbox: None,
            current_model_dir: None,
            current_anim_path: None,
            current_skel_path: None,
            pending_model_load: None,
            last_frame_time: std::time::Instant::now(),
            move_list_receiver: None,
            bone_names: Vec::new(),
            show_debug: false,
            show_edit_log: false,
            export_dir: saved_export_dir,
            current_eff_path: None,
            recent_effs: load_recent_effs(),
            export_build: None,
            pin_sync_client: None,
            pin_sync_wait: None,
            pin_sync_prompt: None,
            hitbox_rules_store: HashMap::new(),
            effect_rules_store: HashMap::new(),
            hitbox_watch: None,
            hitbox_dirty_at: None,
            captures_seen_seq: 0,
            effect_pool: None,
            show_one_slot: false,
            one_slot_search: String::new(),
            effect_pick_search: String::new(),
            effect_pick_open: false,
            one_slot_sel: None,
            one_slot_new_name: String::new(),
            redirect_prompt: None,
            fighter_search: String::new(),
            move_search: String::new(),
            last_simulated_frame: u32::MAX,
            active_effects: Vec::new(),
            particle_clock: 0.0,
            particles_need_catchup: false,
            particle_step_time: std::time::Instant::now(),
            eff_editor: crate::eff_editor::EffEditor::default(),
            game_link: crate::game_link::GameLink::default(),
            live_overrides: crate::game_link::LiveOverrides::default(),
            eff_mods: HashMap::new(),
            project_name: "unnamed_mod".into(),
        };

        if let Some(root) = saved_data_root {
            if root.is_dir() {
                app.set_data_root(root);
            }
        }

        app
    }

    fn set_data_root(&mut self, path: PathBuf) {
        save_config_path("data_root", &path);
        self.state.fighters.clear();
        self.state.labels.clear();
        self.state.status = format!("Loading from {}...", path.display());

        // Load ParamLabels.csv
        let param_labels = path.join("ParamLabels.csv");
        if param_labels.exists() {
            if let Ok(content) = std::fs::read_to_string(&param_labels) {
                for line in content.lines() {
                    let mut parts = line.splitn(2, ',');
                    if let (Some(hex), Some(label)) = (parts.next(), parts.next()) {
                        let hex = hex.trim().strip_prefix("0x").unwrap_or(hex.trim());
                        if let Ok(val) = u64::from_str_radix(hex, 16) {
                            if !label.trim().is_empty() {
                                self.state.labels.insert(val, label.trim().to_string());
                            }
                        }
                    }
                }
            }
        }

        // Load Labels.txt (motion labels)
        let labels_txt = path.join("Labels.txt");
        if labels_txt.exists() {
            if let Ok(content) = std::fs::read_to_string(&labels_txt) {
                for line in content.lines() {
                    let label = line.trim();
                    if label.is_empty() { continue; }
                    let bare = label.strip_suffix(".nuanmb").unwrap_or(label);
                    let hash = hash40::hash40(bare);
                    self.state.labels.entry(hash.0).or_insert_with(|| bare.to_string());
                    if bare != label {
                        let hash_full = hash40::hash40(label);
                        self.state.labels.entry(hash_full.0).or_insert_with(|| bare.to_string());
                    }
                }
            }
        }

        // Index fighters
        let fighter_dir = path.join("fighter");
        if !fighter_dir.is_dir() {
            self.state.status = "No fighter/ directory found.".to_string();
            return;
        }

        let skip = ["common", "ptrainer", "ptrainer_low", "pfushigisou", "pzenigame",
                    "plizardon", "nana", "popo", "miienemyf", "miienemyg", "miienemys",
                    "koopag", "master", "crazy"];

        if let Ok(entries) = std::fs::read_dir(&fighter_dir) {
            for entry in entries.flatten() {
                let fighter_path = entry.path();
                if !fighter_path.is_dir() { continue; }
                let name = match fighter_path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if skip.contains(&name.as_str()) { continue; }

                let param_path = {
                    let p1 = fighter_path.join("param").join("vl.prc");
                    let p2 = fighter_path.join("param").join("fighter_param.prc");
                    if p1.exists() { p1 } else if p2.exists() { p2 } else { continue; }
                };

                let motion_dir = fighter_path.join("motion").join("body").join("c00");
                let model_dir = fighter_path.join("model").join("body").join("c00");
                let display_name = fighter_display_name(&name);

                self.state.fighters.push(crate::data::FighterEntry {
                    name,
                    display_name,
                    param_path,
                    motion_dir,
                    model_dir,
                    effect_dir: None,
                });
            }
        }

        self.state.fighters.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        self.state.data_root = Some(path);
        self.state.status = format!("Loaded {} fighters.", self.state.fighters.len());
    }

    fn select_fighter(&mut self, idx: usize) {
        self.state.selected_fighter = Some(idx);
        self.state.selected_move = None;
        self.state.hitboxes.clear();
        self.state.current_frame = 0;
        self.state.total_frames = 0;
        self.move_list.clear();
        self.move_list_receiver = None;
        self.acmd_error = None;
        self.current_anim_path = None;

        let fighter = &self.state.fighters[idx];
        let model_dir = fighter.model_dir.clone();
        let motion_dir = fighter.motion_dir.clone();

        // Set skel path and eagerly load bone names for the dropdown
        let skel = model_dir.join("model.nusktb");
        self.current_skel_path = if skel.exists() { Some(skel.clone()) } else { None };
        self.bone_names = skel.exists()
            .then(|| ssbh_data::skel_data::SkelData::from_file(&skel).ok())
            .flatten()
            .map(|s| s.bones.into_iter().map(|b| b.name).collect())
            .unwrap_or_default();

        // Also collect weapon bone names from sibling model dirs (sword, hammer, etc.)
        // model_dir = fighter/{name}/model/body/c00 → model_root = fighter/{name}/model
        if let Some(model_root) = model_dir.parent().and_then(|p| p.parent()) {
            if let Ok(entries) = std::fs::read_dir(model_root) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name();
                    if dir_name.to_string_lossy() == "body" { continue; }
                    let weapon_skel_path = entry.path().join("c00").join("model.nusktb");
                    if let Ok(wskel) = ssbh_data::skel_data::SkelData::from_file(&weapon_skel_path) {
                        for bone in wskel.bones {
                            if !self.bone_names.contains(&bone.name) {
                                self.bone_names.push(bone.name);
                            }
                        }
                    }
                }
            }
        }
        self.current_model_dir = Some(model_dir.clone());

        // Queue model load for wgpu (done in update where we have device/queue access)
        self.pending_model_load = Some(model_dir.clone());

        // Load .eff index and embedded .ptcl for this fighter
        self.state.eff_index = None;
        self.state.ptcl = None;
        self.state.particle_system.reset();
        self.state.trail_system.reset();
        // Try effect_dir from fighter entry, then fall back to data_root/effect/fighter/
        let eff_path = fighter.effect_dir.as_ref()
            .map(|d| d.join(format!("ef_{}.eff", fighter.name)))
            .or_else(|| self.state.data_root.as_ref().map(|root| {
                root.join("effect").join("fighter").join(&fighter.name).join(format!("ef_{}.eff", fighter.name))
            }));
        eprintln!("[EFF] eff_path={:?}", eff_path.as_ref().map(|p| (p, p.exists())));

        // If not found, scan the effect directory to show what's actually there
        if eff_path.as_ref().map(|p| !p.exists()).unwrap_or(true) {
            if let Some(root) = &self.state.data_root {
                let effect_root = root.join("effect");
                eprintln!("[EFF] effect root exists={}", effect_root.exists());
                if let Ok(entries) = std::fs::read_dir(&effect_root) {
                    for e in entries.flatten().take(10) {
                        eprintln!("[EFF]   {:?}", e.path());
                    }
                }
                // Also try one level deeper
                let fighter_dir = effect_root.join("fighter");
                eprintln!("[EFF] effect/fighter exists={}", fighter_dir.exists());
                if let Ok(entries) = std::fs::read_dir(&fighter_dir) {
                    for e in entries.flatten().take(10) {
                        eprintln!("[EFF]   fighter/{:?}", e.file_name());
                    }
                }
            }
        }
        if let Some(eff_path) = eff_path.filter(|p| p.exists()) {
            // Keep the eff editor in step with the selected fighter (loads when open).
            self.current_eff_path = Some(eff_path.clone());
            self.eff_editor.queue_load(&eff_path);
            self.load_eff_file(&eff_path);
            // Merge ef_sys.eff for sys_* handles. ef_common is merged inside load_eff_file.
            if let Some(root) = &self.state.data_root.clone() {
                let sys_candidates = [
                    root.join("effect").join("fighter").join("sys").join("ef_sys.eff"),
                    root.join("effect").join("sys").join("ef_sys.eff"),
                    root.join("effect").join("common").join("ef_sys.eff"),
                    root.join("effect").join("ef_sys.eff"),
                ];
                // Scan effect/ subdirs for ef_sys.eff
                let mut found_sys = false;
                for p in &sys_candidates {
                    if p.exists() {
                        eprintln!("[EFF] merging sys eff with ptcl: {:?}", p);
                        if let (Some(eff_index), Some(ptcl)) = (&mut self.state.eff_index, &mut self.state.ptcl) {
                            let _ = eff_index.merge_from_file_with_ptcl(p, ptcl);
                        }
                        self.maybe_queue_bnsh_after_sys_merge(p);
                        found_sys = true;
                        break;
                    }
                }
                if !found_sys {
                    if let Ok(entries) = std::fs::read_dir(root.join("effect")) {
                        for entry in entries.flatten() {
                            let p = entry.path().join("ef_sys.eff");
                            if !p.exists() {
                                continue;
                            }
                            eprintln!("[EFF] scanning for sys: {:?} exists=true", p);
                            if let (Some(eff_index), Some(ptcl)) = (&mut self.state.eff_index, &mut self.state.ptcl) {
                                let _ = eff_index.merge_from_file_with_ptcl(&p, ptcl);
                            }
                            self.maybe_queue_bnsh_after_sys_merge(&p);
                            found_sys = true;
                            break;
                        }
                    }
                    if !found_sys {
                        eprintln!("[EFF] ef_sys.eff not found — injecting synthetic sys emitter sets");
                        // Append synthetic emitter sets for common sys effects and register their handles
                        if let (Some(eff_index), Some(ptcl)) = (&mut self.state.eff_index, &mut self.state.ptcl) {
                            let sys_effects: &[(&str, crate::effects::BlendType, f32, f32, f32, u32)] = &[
                                // (name, blend, scale, lifetime, speed, count)
                                ("sys_smash_flash",    crate::effects::BlendType::Add,    3.0, 12.0, 0.5, 20),
                                ("sys_attack_arc",     crate::effects::BlendType::Add,    2.0, 15.0, 0.4, 15),
                                ("sys_attack_arc_b",   crate::effects::BlendType::Add,    2.0, 15.0, 0.4, 15),
                                ("sys_attack_arc_lw",  crate::effects::BlendType::Add,    2.0, 15.0, 0.4, 15),
                                ("sys_hit_smoke",      crate::effects::BlendType::Normal, 1.5, 12.0, 0.3, 12),
                                ("sys_landing_smoke",  crate::effects::BlendType::Normal, 1.0, 10.0, 0.2, 10),
                            ];
                            for (name, blend, scale, lifetime, speed, count) in sys_effects {
                                let set_idx = ptcl.emitter_sets.len() as i32;
                                eff_index.handles.entry(name.to_string()).or_insert(set_idx);
                                eff_index.handles.entry(name.to_lowercase()).or_insert(set_idx);
                                ptcl.emitter_sets.push(crate::effects::EmitterSet {
                                    name: name.to_string(),
                                    emitters: vec![crate::effects::EmitterDef {
                                        name: name.to_string(),
                                        emit_type: crate::effects::EmitType::Sphere,
                                        blend_type: *blend,
                                        display_side: crate::effects::DisplaySide::Both,
                                        emission_rate: *count as f32,
                                        emission_rate_random: 0.0,
                                        initial_speed: *speed,
                                        speed_random: 0.4,
                                        accel: glam::Vec3::ZERO,
                                        lifetime: *lifetime,
                                        lifetime_random: 0.3,
                                        scale: *scale,
                                        scale_random: 0.3,
                                        rotation_speed: 0.0,
                                        rotation_init: 0.0,
                                        rotation_init_random: 0.0,
                                        color0: vec![crate::effects::ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 }],
                                        color1: Vec::new(),
                                        alpha0: crate::effects::AnimKey3v4k {
                                            start_value: 1.0,
                                            start_diff: 0.0,
                                            end_diff: -1.0,
                                            time2: 0.3,
                                            time3: 0.7,
                                        },
                                        alpha1: crate::effects::AnimKey3v4k::default(),
                                        alpha0_keys: vec![
                                            crate::effects::ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
                                            crate::effects::ColorKey { frame: 0.5, r: 0.8, g: 0.8, b: 0.8, a: 0.8 },
                                            crate::effects::ColorKey { frame: 1.0, r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
                                        ],
                                        alpha1_keys: vec![],
                                        scale_anim: crate::effects::AnimKey3v4k {
                                            start_value: 1.0,
                                            start_diff: 0.5,
                                            end_diff: -1.5,
                                            time2: 0.2,
                                            time3: 0.6,
                                        },
                                        textures: Vec::new(),
                                        mesh_type: 0,
                                        primitive_index: 0,
                                        texture_index: 0,
                                        emitter_offset: glam::Vec3::ZERO,
                                        emitter_rotation: glam::Vec3::ZERO,
                                        emitter_scale: glam::Vec3::ONE,
                                        trans_rand: glam::Vec3::ZERO,
                                        position_random: 0.0,
                                        follow_type: crate::effects::FollowType::Srt,
                                        is_update_matrix_by_emit: false,
                                        billboard_type: crate::effects::BillboardType::Billboard,
                                        rot_type: 0,
                                        rot_axis_x: false,
                                        rot_axis_y: false,
                                        rot_axis_z: false,
                                        offset_type: 0,
                                        volume_radius: glam::Vec3::ONE,
                                        volume_form_scale: glam::Vec3::ONE,
                                        line_length: 1.0,
                                        line_center: 0.0,
                                        volume_surface_pos_rand: 0.0,
                                        designated_dir: glam::Vec3::Z,
                                        use_omnidirectional: true,
                                        tex_scale_uv: [1.0, 1.0],
                                        tex_offset_uv: [0.0, 0.0],
                                        tex_scroll_uv: [0.0, 0.0],
                                        tex_pat_frame_count: 1,
                                        tex_pat_frame_table: Vec::new(),
                                        tex_pat_frequency: 1.0,
                                        tex_pattern_anim_type: 0,
                                        tex_is_scroll: false,
                                        tex_is_rotate: false,
                                        tex_is_scale: false,
                                        tex_scroll_rotation: 0.0,
                                        tex_scroll_rotation_add: 0.0,
                                        tex_inv_rand_u: false,
                                        tex_inv_rand_v: false,
                                        tex_pat_loop_random: false,
                                        tex_crossfade: false,
                                        indirect_anim: crate::effects::TextureAnimFlags::default(),
                                        indirect_pat_frame_count: 1,
                                        indirect_pat_frame_table: Vec::new(),
                                        indirect_pat_frequency: 1.0,
                                        tex2_anim: crate::effects::TextureAnimFlags::default(),
                                        tex2_pat_frequency: 1.0,
                                        tex_anims_extra: [crate::effects::TextureAnimFlags::default(); 3],
                                        tex_extra_slots: std::array::from_fn(|_| crate::effects::TexExtraSlotDef::default()),
                                        is_one_time: true,
                                        emission_start: 0,
                                        emission_timing: 0,
                                        emission_duration: 1,
                                        is_indirect_slot1: false,
                                        distortion_strength: 0.0,
                                        indirect_scroll_uv: [0.0, 0.0],
                                        indirect_tex_scale_uv: [1.0, 1.0],
                                        indirect_tex_offset_uv: [0.0, 0.0],
                                        tex2_scale_uv: [1.0, 1.0],
                                        tex2_offset_uv: [0.0, 0.0],
                                        tex2_scroll_uv: [0.0, 0.0],
                                        tex_wrap_u: 2,
                                        tex_wrap_v: 2,
                                        tex2_wrap_u: 2,
                                        tex2_wrap_v: 2,
                                        tex2_pat_frame_count: 1,
                                        tex2_pat_frame_table: Vec::new(),
                                        anim_translate: None,
                                        anim_rotation: None,
                                        anim_emit_scale: None,
                                        anim_tex_scale: None,
                                        anim_color0: None,
                                        anim_color1: None,
                                        anim_alpha: None,
                                        shader_index: -1,
                                        custom_shader_index: 0,
                                        user_shader_indices: [-1, -1],
                                        shader_key: 0,
                                        combiner: crate::shader_registry::CombinerState::default(),
                                        particle_color: crate::shader_registry::ParticleColorState::default(),
                                        particle_scale: crate::shader_registry::ParticleScaleState::default(),
                                        ..Default::default()
                                    }],
                                 });
                            }
                        }
                    }
                }
            }
        }

        // Build move list on a background thread — reads many .nuanmb files for frame counts
        let labels = self.state.labels.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.move_list_receiver = Some(rx);
        self.state.status = "Loading moves...".to_string();

        std::thread::spawn(move || {
            let motion_list_path = motion_dir.join("motion_list.bin");
            let Ok(mlist) = motion_lib::open(&motion_list_path) else { return; };

            let mut moves: Vec<MoveEntry> = mlist.list.iter().filter_map(|(hash_key, _)| {
                let hash_val = hash_key.0;
                let name = labels.get(&hash_val)
                    .cloned()
                    .unwrap_or_else(|| format!("{:#018x}", hash_val));

                // Filter early to avoid reading files for non-attack moves
                let n = name.to_lowercase();
                if !(n.contains("attack") || n.contains("special") ||
                     n.contains("throw") || n.contains("catch") ||
                     n.contains("cliff") || n.contains("final")) {
                    return None;
                }

                let anim_path = find_nuanmb(&motion_dir, &name, hash_val);
                let frame_count = anim_path.as_deref()
                    .and_then(|p| ssbh_data::anim_data::AnimData::from_file(p).ok())
                    .map(|a| a.final_frame_index as u32 + 1)
                    .unwrap_or(0);

                Some(MoveEntry { name, hash: hash_val, frame_count, anim_path })
            }).collect();

            moves.sort_by(|a, b| a.name.cmp(&b.name));
            let _ = tx.send(moves);
        });
    }

    fn select_move(&mut self, move_entry: MoveEntry) {
        self.state.current_frame = 0;
        self.state.total_frames = move_entry.frame_count;
        self.state.hitboxes.clear();
        self.state.script = crate::data::AcmdScript::default();
        self.state.effect_script = crate::data::EffectScript::default();
        self.state.effects = Vec::new();
        self.acmd_error = None;
        // Path was resolved at move list build time — no disk scan needed
        self.current_anim_path = move_entry.anim_path.clone();
        self.state.selected_move = Some(move_entry);
        // Reset particle/trail state for the new move
        self.state.particle_system.reset();
        self.state.trail_system.reset();
        self.last_simulated_frame = u32::MAX;
        self.active_effects.clear();
        self.particle_clock = 0.0;
        self.particle_step_time = std::time::Instant::now();
    }

    fn fetch_acmd(&mut self) {
        let (fighter_name, move_name) = match (
            self.state.selected_fighter.and_then(|i| self.state.fighters.get(i)),
            &self.state.selected_move,
        ) {
            (Some(f), Some(m)) => (f.name.clone(), m.name.clone()),
            _ => return,
        };

        self.fetching_acmd = true;
        self.acmd_error = None;

        match fetch_script_body(&fighter_name, &move_name) {
            Ok(body) => {
                let script = crate::acmd::parse_acmd_script(&body);
                let effect_script = crate::acmd::parse_effect_script(&body);

                let mut hitboxes = script.to_hitboxes();
                if hitboxes.is_empty() {
                    self.acmd_error = Some(format!("No hitboxes found for {}/{}", fighter_name, move_name));
                    self.state.effect_script = crate::data::EffectScript::default();
                    self.state.effects = Vec::new();
                } else {
                    // Normalize bone names to match the skel's casing
                    let bone_name_map: std::collections::HashMap<String, String> = self.bone_names
                        .iter()
                        .map(|n| (n.to_lowercase(), n.clone()))
                        .collect();

                    let virtual_bone_fallbacks: &[(&str, &str)] = &[
                        ("haver",     "HandR"),
                        ("havel",     "HandL"),
                        ("haver2",    "HandR"),
                        ("throw",     "Hip"),
                        ("itemroot",  "Hip"),
                        ("top",       "Trans"),
                        ("trans",     "Trans"),
                        ("rot",       "Rot"),
                    ];

                    for hb in &mut hitboxes {
                        let lower = hb.bone_name.to_lowercase();
                        if let Some(canonical) = bone_name_map.get(&lower) {
                            hb.bone_name = canonical.clone();
                        } else {
                            if let Some(&(_, fallback)) = virtual_bone_fallbacks.iter().find(|(v, _)| *v == lower) {
                                if let Some(canonical) = bone_name_map.get(&fallback.to_lowercase()) {
                                    hb.bone_name = canonical.clone();
                                }
                            }
                        }
                    }
                    self.state.hitboxes_pristine = hitboxes.clone();
                    self.state.acmd_source = "GitHub".into();
                    self.state.hitboxes = hitboxes;
                    self.state.script = script;

                    // Store effect data — pristine first, then re-apply this move's edits.
                    self.state.effects = effect_script.to_effect_calls();
                    self.state.effects_pristine = self.state.effects.clone();
                    self.state.selected_effect_call = None;
                    self.apply_effect_call_edits_to_current();
                    self.state.effect_script = effect_script;

                    // Jump the timeline to the earliest active window (hitbox or effect).
                    if let Some(first) = self.state.hitboxes.first() {
                        if first.active_start > self.state.current_frame {
                            self.state.current_frame = first.active_start;
                        }
                    }
                    if let Some(min_effect_start) =
                        self.state.effects.iter().map(|e| e.active_start).min()
                    {
                        if min_effect_start > self.state.current_frame {
                            self.state.current_frame = min_effect_start;
                        }
                    }
                    if let (Some(eff_index), Some(ptcl)) =
                        (&self.state.eff_index, &self.state.ptcl)
                    {
                        if let Some(emit_frame) = Self::compute_first_particle_frame(
                            &self.state.effects,
                            self.state.current_frame,
                            eff_index,
                            ptcl,
                        ) {
                            self.state.current_frame = emit_frame;
                        }
                    }

                    // Spawn effects into particle/trail systems
                    self.respawn_effects();
                }
            }
            Err(e) => {
                self.acmd_error = Some(format!("Fetch failed: {}", e));
                self.state.effect_script = crate::data::EffectScript::default();
                self.state.effects = Vec::new();
            }
        }
        self.fetching_acmd = false;
    }

    /// eff-editor branch: effect RENDERING removed (lives on game-accurate-sim).
    /// These stubs keep the load/merge call sites intact — shader decode, GPU particle
    /// renderer and texture upload simply do not happen here.
    fn queue_bnsh_reload_from_ptcl(&mut self, _source_label: &str) {}

    fn maybe_queue_bnsh_after_sys_merge(&mut self, _merged_path: &std::path::Path) {}

    fn apply_pending_gpu_effects(&mut self, _frame: &eframe::Frame) {
        // Nothing to upload — no particle renderer on this branch.
        self.state.pending_texture_upload = false;
    }

    /// Re-spawn all effects into the particle/trail systems using current eff_index + ptcl.
    /// Call this after loading a new .eff file or after fetching ACMD.
    fn load_eff_file(&mut self, path: &std::path::Path) {
        match crate::effects::EffIndex::from_file(path) {
            Ok(mut eff) => {
                eprintln!("[EFF] loaded {} handles, ptcl_data={} bytes", eff.handles.len(), eff.ptcl_data.len());
                for (k, v) in eff.handles.iter().take(8) {
                    eprintln!("[EFF]   handle {:?} -> set_idx {}", k, v);
                }
                if !eff.ptcl_data.is_empty() {
                    match crate::effects::PtclFile::parse(&eff.ptcl_data) {
                        Ok(mut ptcl) => {
                            if let Some(parent) = path.parent() {
                                // ── Task 45: load external trail/*.nutexb textures ──────────────
                                let trail_dir = parent.join("trail");
                                if trail_dir.is_dir() {
                                    let n = ptcl.merge_external_nutexb_dir(&trail_dir);
                                    eprintln!("[EFF] merged {n} trail textures from {:?}", trail_dir);
                                }
                                // ── Task 46: load external model/**/*.nutexb textures ───────────
                                let model_dir = parent.join("model");
                                if model_dir.is_dir() {
                                    let n = ptcl.merge_external_nutexb_dir_recursive(&model_dir, true);
                                    eprintln!("[EFF] merged {n} model textures from {:?}", model_dir);
                                }
                                // ── Task 48: auto-merge system/common/ef_common.eff ─────────────
                                // Derive path: fighter/<name>/ef_<name>.eff → ../../system/common/ef_common.eff
                                let common_eff = parent
                                    .parent()          // effect/fighter
                                    .and_then(|p| p.parent()) // effect
                                    .map(|p| p.join("system").join("common").join("ef_common.eff"));
                                if let Some(common_path) = common_eff {
                                    if common_path.exists() {
                                        // Merge common handles INTO eff (not a temp copy) so
                                        // spawn_effect can find sys_* handles at runtime.
                                        match eff.merge_from_file_with_ptcl(&common_path, &mut ptcl) {
                                            Ok(()) => eprintln!("[EFF] merged ef_common.eff: now {} handles, {} emitter sets",
                                                eff.handles.len(), ptcl.emitter_sets.len()),
                                            Err(e) => eprintln!("[EFF] ef_common.eff merge failed: {e}"),
                                        }
                                    }
                                }
                            }
                            eprintln!("[EFF] ptcl ok: {} emitter sets", ptcl.emitter_sets.len());

                            self.state.status = format!(
                                "Loaded {} effects ({} emitter sets)",
                                eff.handles.len(), ptcl.emitter_sets.len()
                            );
                            self.state.ptcl = Some(ptcl);
                            self.state.pending_texture_upload = true;
                            self.queue_bnsh_reload_from_ptcl(
                                path.file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("effect.eff"),
                            );
                        }
                        Err(e) => {
                            if crate::effect_converter::is_effect_io_error(&e) {
                                eprintln!("[EFF] FATAL: effect PTCL parse failed — {e}");
                                eprintln!(
                                    "[EFF] EffectConverter needs scratch space on disk (not /tmp). \
                                     Free space or set: export HITBOX_EFFECT_TMP=$PWD/target/hitbox-editor-cache"
                                );
                                self.state.status =
                                    "Effect load failed: disk full (set HITBOX_EFFECT_TMP to target/hitbox-editor-cache)"
                                        .to_string();
                                self.state.ptcl = None;
                                self.state.pending_texture_upload = false;
                            } else {
                                // VFXB (Switch format) — fall back to name-aware synthetic emitter sets
                                eprintln!("[EFF] ptcl parse error ({e}), using synthetic emitter sets");
                                let max_idx = eff.handles.values().copied().max().unwrap_or(0).max(0) as usize;
                                let mut idx_to_name: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
                                for (name, &idx) in &eff.handles {
                                    if name.chars().any(|c| c.is_uppercase()) { continue; }
                                    idx_to_name.entry(idx).or_insert_with(|| name.clone());
                                }
                                let ptcl = crate::effects::PtclFile::synthetic_named(max_idx, &idx_to_name);
                                self.state.status = format!(
                                    "Loaded {} effects (synthetic, no GPU shaders)",
                                    eff.handles.len()
                                );
                                self.state.ptcl = Some(ptcl);
                                self.state.pending_texture_upload = true;
                                self.queue_bnsh_reload_from_ptcl(
                                    path.file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("effect.eff.synthetic"),
                                );
                            }
                        }
                    }
                } else {
                    eprintln!("[EFF] ptcl_data is empty");
                }
                self.state.eff_index = Some(eff);
                // If ACMD effects are already loaded, re-spawn them with the new .eff data
                if !self.state.effects.is_empty() {
                    self.respawn_effects();
                }
            }
            Err(e) => {
                eprintln!("[EFF] load error: {e}");
                self.state.status = format!("EFF load error: {e}");
            }
        }
    }

    /// Open an arbitrary .eff from disk (outside the game data root): load + render it,
    /// make it the current eff, index it into the donor pool, and remember it as recent.
    /// Available regardless of whether a data root is set.
    fn open_external_eff(&mut self, path: PathBuf) {
        if !path.exists() {
            self.state.status = format!("Effect file not found: {}", path.display());
            return;
        }
        self.load_eff_file(&path);
        self.respawn_effects();
        self.current_eff_path = Some(path.clone());
        if self.eff_editor.open {
            self.eff_editor.queue_load(&path);
        }
        // Make its entries searchable in One-Slot + the effect-name picker.
        if self.effect_pool.is_none() {
            if let Some(root) = self.export_dir.clone().or_else(|| self.state.data_root.clone()) {
                self.effect_pool = Some(crate::effect_pool::EffectPool::new(root));
            }
        }
        if let Some(pool) = self.effect_pool.as_mut() {
            pool.add_file(&path);
        }
        // Recents: de-dupe, most-recent first, cap at 12.
        self.recent_effs.retain(|p| p != &path);
        self.recent_effs.insert(0, path);
        self.recent_effs.truncate(12);
        save_recent_effs(&self.recent_effs);
    }

    /// ACMD `active_start` / `active_end` for emitter lifecycle (local emission frame = target − start).
    fn effect_spawn_window(
        ec: &crate::data::EffectCall,
        eff_index: &crate::effects::EffIndex,
        ptcl: &crate::effects::PtclFile,
    ) -> (f32, f32) {
        crate::effects::acmd_spawn_window(
            &ec.effect_name,
            ec.active_start,
            ec.active_end,
            eff_index,
            ptcl,
        )
    }

    fn is_trail_effect(
        name: &str,
        name_lower: &str,
        follows_bone: bool,
        eff_index: &crate::effects::EffIndex,
        ptcl: &crate::effects::PtclFile,
    ) -> bool {
        let name_matches = follows_bone && (
            name_lower.contains("sword") || name_lower.contains("trail") ||
            name_lower.contains("after") || name_lower.contains("tex_") ||
            name_lower.contains("katana") || name_lower.contains("blade") ||
            name_lower.contains("slash") || name_lower.contains("arc") ||
            name_lower.contains("swing") || name_lower.contains("energy") ||
            name_lower.contains("aura") || name_lower.contains("ribbon")
        );
        if !name_matches {
            return false;
        }
        // Prefer REAL PTCL particles whenever the handle resolves to a non-empty emitter
        // set: the synthetic SwordTrail path uses a hardcoded white colour and no PTCL
        // texture (it exists as a fallback for effects with no data — e.g. sys effects
        // when ef_sys is missing). Name-based hijacking rendered arc/slash effects
        // (Samus aerials etc.) with wrong colour + texture.
        // Try both the original-case and lowercase keys — same as every other handle lookup.
        // Checking only lowercase sent cased arc/slash effects WITH real PTCL down the
        // synthetic hardcoded-white trail path.
        let has_ptcl = eff_index
            .handles
            .get(name)
            .or_else(|| eff_index.handles.get(name_lower))
            .copied()
            .filter(|&idx| idx >= 0)
            .and_then(|idx| ptcl.emitter_sets.get(idx as usize))
            .map(|set| !set.emitters.is_empty())
            .unwrap_or(false);
        !has_ptcl
    }

    /// Earliest global frame where any due effect would first emit (for timeline preview).
    fn compute_first_particle_frame(
        effects: &[crate::data::EffectCall],
        current_frame: u32,
        eff_index: &crate::effects::EffIndex,
        ptcl: &crate::effects::PtclFile,
    ) -> Option<u32> {
        let mut best: Option<u32> = None;
        for ec in effects {
            if ec.active_start > current_frame {
                continue;
            }
            let name_lower = ec.effect_name.to_lowercase();
            if Self::is_trail_effect(&ec.effect_name, &name_lower, ec.follows_bone, eff_index, ptcl) {
                continue;
            }
            let Some(global) = crate::effects::earliest_particle_frame_for_spawn(
                &ec.effect_name,
                ec.active_start,
                eff_index,
                ptcl,
            ) else {
                continue;
            };
            best = Some(best.map(|b| b.min(global)).unwrap_or(global));
        }
        best.filter(|&f| f > current_frame)
    }

    /// Spawn every non-trail ACMD effect whose active_start is at or before the current frame.
    fn spawn_active_particle_effects(&mut self) {
        let current_frame = self.state.current_frame;
        let bone_name_map: std::collections::HashMap<String, String> = self.bone_names
            .iter()
            .map(|n| (n.to_lowercase(), n.clone()))
            .collect();
        let Some(eff_index) = &self.state.eff_index else { return };
        let Some(ptcl) = &self.state.ptcl else { return };
        let mut spawned: std::collections::HashSet<(String, String, u32)> =
            std::collections::HashSet::new();
        for ec in &self.state.effects.clone() {
            if ec.active_start > current_frame {
                continue;
            }
            let name_lower = ec.effect_name.to_lowercase();
            if Self::is_trail_effect(&ec.effect_name, &name_lower, ec.follows_bone, eff_index, ptcl) {
                continue;
            }
            let canonical_bone = bone_name_map.get(&ec.bone_name.to_lowercase())
                .cloned()
                .unwrap_or_else(|| ec.bone_name.clone());
            let dedupe_key = (name_lower.clone(), canonical_bone.clone(), ec.active_start);
            if !spawned.insert(dedupe_key) {
                continue;
            }
            let set_idx_opt = eff_index.handles.get(&ec.effect_name)
                .or_else(|| eff_index.handles.get(&name_lower))
                .copied()
                .filter(|&idx| idx >= 0)
                .map(|idx| idx as usize)
                .filter(|&idx| idx < ptcl.emitter_sets.len());
            let (start_frame, end_frame) = Self::effect_spawn_window(ec, eff_index, ptcl);
            self.state.particle_system.spawn_effect(
                &ec.effect_name,
                &canonical_bone,
                glam::Vec3::from(ec.offset),
                glam::Vec3::from(ec.rotation),
                start_frame,
                end_frame,
                eff_index,
                ptcl,
            );
            if let Some(set_idx) = set_idx_opt {
                let max_lifetime = ptcl.emitter_sets[set_idx].emitters.iter()
                    .map(|e| {
                        let emit_end = e.emission_timing as f32 + e.emission_duration as f32;
                        emit_end + e.lifetime + e.lifetime_random
                    })
                    .fold(0.0f32, f32::max)
                    .max(1.0);
                self.active_effects.push(ActiveEffect {
                    anim_clock: 0.0,
                    max_lifetime,
                    emitter_set_idx: set_idx,
                });
            }
        }
    }

    /// Re-spawn particle emitters due at `frame` and queue integer catch-up 0..=frame.
    fn queue_particle_resimulate_to_frame(&mut self, frame: u32) {
        self.state.particle_system.reset();
        self.active_effects.clear();
        self.particle_clock = 0.0;
        self.particle_step_time = std::time::Instant::now();
        self.spawn_active_particle_effects();
        if let (Some(eff_index), Some(ptcl)) = (&self.state.eff_index, &self.state.ptcl) {
            for ec in &self.state.effects.clone() {
                if ec.active_start > frame {
                    continue;
                }
                let name_lower = ec.effect_name.to_lowercase();
                if Self::is_trail_effect(&ec.effect_name, &name_lower, ec.follows_bone, eff_index, ptcl) {
                    continue;
                }
                let set_idx_opt = eff_index
                    .handles
                    .get(&ec.effect_name)
                    .or_else(|| eff_index.handles.get(&name_lower))
                    .copied()
                    .filter(|&idx| idx >= 0)
                    .map(|idx| idx as usize)
                    .filter(|&idx| idx < ptcl.emitter_sets.len());
                if let Some(set_idx) = set_idx_opt {
                    let max_lifetime = ptcl.emitter_sets[set_idx]
                        .emitters
                        .iter()
                        .map(|e| {
                            let burst = crate::effects::emitter_first_burst_local_frame(e) as f32;
                            let emit_end = e.emission_start as f32 + e.emission_duration as f32;
                            emit_end.max(burst) + e.lifetime + e.lifetime_random
                        })
                        .fold(0.0f32, f32::max)
                        .max(1.0);
                    self.active_effects.push(ActiveEffect {
                        anim_clock: 0.0,
                        max_lifetime,
                        emitter_set_idx: set_idx,
                    });
                }
            }
        }
        self.last_simulated_frame = u32::MAX;
        self.particles_need_catchup = true;
    }

    fn respawn_effects(&mut self) {
        self.state.particle_system.reset();
        self.state.trail_system.reset();
        self.active_effects.clear();
        self.particle_clock = self.state.current_frame as f32;
        self.particle_step_time = std::time::Instant::now();
        // Keep the timeline position (Fetch ACMD may have jumped to active_start).
        self.last_simulated_frame = u32::MAX;
        self.state.playing = true;
        self.last_frame_time = std::time::Instant::now();
        eprintln!(
            "[RESPAWN] effects={} eff_index={} ptcl={} frame={}",
            self.state.effects.len(),
            self.state.eff_index.is_some(),
            self.state.ptcl.is_some(),
            self.state.current_frame,
        );
        // Set up trail effects (these follow bones continuously, not frame-triggered)
        let bone_name_map: std::collections::HashMap<String, String> = self.bone_names
            .iter()
            .map(|n| (n.to_lowercase(), n.clone()))
            .collect();
        if let (Some(eff_index), Some(ptcl)) = (&self.state.eff_index, &self.state.ptcl) {
            for ec in &self.state.effects {
                let name_lower = ec.effect_name.to_lowercase();
                let is_trail = Self::is_trail_effect(&ec.effect_name, &name_lower, ec.follows_bone, eff_index, ptcl);
                if !is_trail { continue; }
                let canonical_bone = bone_name_map.get(&ec.bone_name.to_lowercase())
                    .cloned()
                    .unwrap_or_else(|| ec.bone_name.clone());
                let (color, blend, draw_path) = eff_index.handles.get(&ec.effect_name)
                    .or_else(|| eff_index.handles.get(&name_lower))
                    .and_then(|&idx| if idx >= 0 { ptcl.emitter_sets.get(idx as usize) } else { None })
                    .and_then(|set| {
                        // Tint from the first non-white emitter (color0 × color1 — arc/trail
                        // tints often live in color1 or a child emitter, not first().color0).
                        let c = crate::effects::set_display_color(set, 0.0)?;
                        let (blend, draw_path) = set
                            .emitters
                            .first()
                            .map(|e| (e.blend_type, e.draw_path))
                            .unwrap_or((crate::effects::BlendType::Add, 0));
                        Some((c, blend, draw_path))
                    })
                    .unwrap_or(([1.0, 1.0, 1.0, 1.0], crate::effects::BlendType::Add, 0));
                let bone_lower = canonical_bone.to_lowercase();
                let tip_bone = self.bone_names.iter()
                    .find(|b| {
                        let bl = b.to_lowercase();
                        (bl.contains("top") || bl.contains("tip") || bl.contains("end"))
                            && (bl.contains(&bone_lower) || bone_lower.contains(&bl))
                    })
                    .cloned()
                    .unwrap_or_else(|| canonical_bone.clone());
                self.state.trail_system.start_trail(
                    &ec.effect_name,
                    &tip_bone,
                    &canonical_bone,
                    draw_path,
                    color,
                    blend,
                );
            }
        }
        // Spawn particle effects active at the current timeline frame and step on the next sim pass.
        self.spawn_active_particle_effects();
        self.particles_need_catchup = true;
        // Particle effects are also re-spawned when the timeline crosses active_start while playing.
    }

    fn draw_edit_log_window(&mut self, ctx: &egui::Context) {
        // Keep the authored-eff numbers current with the eff editor's live diff.
        self.sync_eff_mods_from_editor();

        // Collect pending actions to avoid borrow conflicts
        let mut remove_move: Option<(String, String)> = None;
        let mut remove_fighter: Option<String> = None;
        let mut export_move: Option<(String, String)> = None;
        let mut remove_call_key: Option<String> = None;
        let mut clear_eff_fighter: Option<String> = None;
        let mut clear_tweak_hash: Option<u64> = None;
        let mut export_all = false;

        // Union of fighters across all edit sources: hitboxes, effect calls, authored eff.
        let mut fighters: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for (name, display) in self.state.edit_log.fighters_sorted() {
            fighters.insert(name, display);
        }
        for key in self.state.effect_call_edits.keys() {
            if self.state.effect_call_edits[key].is_empty() {
                continue;
            }
            let fighter = key.split_once('/').map(|(f, _)| f).unwrap_or(key.as_str());
            fighters
                .entry(fighter.to_string())
                .or_insert_with(|| crate::data::fighter_display_name(fighter));
        }
        for (fighter, eff) in &self.eff_mods {
            if !eff.is_empty() {
                fighters
                    .entry(fighter.clone())
                    .or_insert_with(|| crate::data::fighter_display_name(fighter));
            }
        }

        let mut open = self.show_edit_log;
        egui::Window::new("Edits")
            .open(&mut open)
            .resizable(true)
            .default_size([460.0, 520.0])
            .show(ctx, |ui| {
                if fighters.is_empty() {
                    ui.label(egui::RichText::new("No edits recorded yet.")
                        .color(egui::Color32::GRAY));
                    return;
                }

                ui.label(egui::RichText::new(
                    "All edits across the toolkit — hitboxes (incl. live rules), effect \
                     spawns, live tweaks, and authored eff values. Saved automatically; \
                     use × to discard (also un-sends the live state)."
                ).small().color(egui::Color32::GRAY));
                ui.separator();

                // ── Live color/speed tweaks (kind-global runtime multipliers) ──
                let tweaks = self.live_overrides.tweaked();
                if !tweaks.is_empty() {
                    ui.label(egui::RichText::new("Live color/speed tweaks").small().strong());
                    for (hash, form) in &tweaks {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            let c = form.rainbow.color;
                            ui.label(egui::RichText::new(format!(
                                "{} — color ×[{:.2} {:.2} {:.2}] speed ×{:.2}",
                                form.effect_name, c.red, c.green, c.blue, form.speed
                            )).small().monospace());
                            if ui.small_button("×").on_hover_text("Revert (also in game)").clicked() {
                                clear_tweak_hash = Some(*hash);
                            }
                        });
                    }
                    ui.separator();
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (fighter_name, fighter_display) in &fighters {
                        let move_names = self.state.edit_log.moves_for(fighter_name);
                        let call_keys: Vec<String> = self
                            .state
                            .effect_call_edits
                            .iter()
                            .filter(|(k, v)| {
                                !v.is_empty()
                                    && k.split_once('/').map(|(f, _)| f == fighter_name).unwrap_or(false)
                            })
                            .map(|(k, _)| k.clone())
                            .collect();
                        let eff = self.eff_mods.get(fighter_name).filter(|e| !e.is_empty());
                        let total =
                            move_names.len() + call_keys.len() + eff.map(|_| 1).unwrap_or(0);

                        egui::CollapsingHeader::new(
                            egui::RichText::new(format!("{fighter_display}  ({total})")).strong(),
                        )
                        .id_salt(format!("edits_{fighter_name}"))
                        .default_open(true)
                        .show(ui, |ui| {
                            // ── Hitboxes / ACMD ────────────────────────────
                            if !move_names.is_empty() {
                                ui.label(egui::RichText::new("Hitboxes / ACMD").small().strong());
                                for move_name in &move_names {
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        let is_active = self.state.selected_fighter
                                            .and_then(|i| self.state.fighters.get(i))
                                            .map(|f| &f.name == fighter_name)
                                            .unwrap_or(false)
                                            && self.state.selected_move.as_ref()
                                                .map(|m| &m.name == move_name)
                                                .unwrap_or(false);
                                        let label = if is_active {
                                            egui::RichText::new(format!("▶ {}", move_name))
                                                .color(egui::Color32::from_rgb(100, 200, 255))
                                        } else {
                                            egui::RichText::new(move_name.clone())
                                        };
                                        ui.label(label);
                                        if let Some(record) = self.state.edit_log.entries
                                            .get(fighter_name)
                                            .and_then(|m| m.get(move_name))
                                        {
                                            ui.label(egui::RichText::new(
                                                format!("{} hb", record.hitboxes.len())
                                            ).small().color(egui::Color32::GRAY));
                                        }
                                        let rule_key = format!("{fighter_name}/{move_name}");
                                        if let Some(rules) = self.hitbox_rules_store.get(&rule_key)
                                        {
                                            if !rules.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "⚡{} live",
                                                        rules.len()
                                                    ))
                                                    .small()
                                                    .color(egui::Color32::from_rgb(90, 220, 90)),
                                                );
                                            }
                                        }
                                        if ui.small_button("Export")
                                            .on_hover_text("Export this move as smashline source")
                                            .clicked()
                                        {
                                            export_move =
                                                Some((fighter_name.clone(), move_name.clone()));
                                        }
                                        if ui.small_button("×").clicked() {
                                            remove_move =
                                                Some((fighter_name.clone(), move_name.clone()));
                                        }
                                    });
                                }
                            }

                            // ── Effect spawns ──────────────────────────────
                            if !call_keys.is_empty() {
                                ui.label(egui::RichText::new("Effect spawns").small().strong());
                                for key in &call_keys {
                                    let mv = key.split_once('/').map(|(_, m)| m).unwrap_or(key);
                                    let edits = &self.state.effect_call_edits[key];
                                    let (n_mod, n_add, n_rem, n_sup) = edits.iter().fold(
                                        (0, 0, 0, 0),
                                        |(m, a, r, s), e| match &e.op {
                                            crate::data::EffectCallOp::Modify(c) => {
                                                (m + 1, a, r, s + usize::from(c.disabled))
                                            }
                                            crate::data::EffectCallOp::Add(c) => {
                                                (m, a + 1, r, s + usize::from(c.disabled))
                                            }
                                            crate::data::EffectCallOp::Remove => (m, a, r + 1, s),
                                        },
                                    );
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(mv);
                                        let mut txt = format!(
                                            "{n_mod} edited · {n_add} added · {n_rem} removed"
                                        );
                                        if n_sup > 0 {
                                            txt.push_str(&format!(" · {n_sup} suppressed live"));
                                        }
                                        ui.label(egui::RichText::new(txt).small().color(egui::Color32::GRAY));
                                        if ui.small_button("×").clicked() {
                                            remove_call_key = Some(key.clone());
                                        }
                                    });
                                }
                            }

                            // ── Authored eff values ────────────────────────
                            if let Some(eff) = eff {
                                ui.label(egui::RichText::new("Authored eff").small().strong());
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    ui.label(egui::RichText::new(&eff.source_rel).small().monospace());
                                    if ui.small_button("×").on_hover_text("Discard all authored eff edits").clicked() {
                                        clear_eff_fighter = Some(fighter_name.clone());
                                    }
                                });
                                for a in &eff.authored {
                                    ui.horizontal(|ui| {
                                        ui.add_space(24.0);
                                        ui.label(egui::RichText::new(format!(
                                            "{} / {} — {} field(s)",
                                            if a.set_name.is_empty() { format!("set {}", a.set_idx) } else { a.set_name.clone() },
                                            if a.emitter_name.is_empty() { format!("emitter {}", a.emitter_idx) } else { a.emitter_name.clone() },
                                            a.fields.count(),
                                        )).small());
                                    });
                                }
                                for os in &eff.one_slot {
                                    ui.horizontal(|ui| {
                                        ui.add_space(24.0);
                                        ui.label(egui::RichText::new(format!(
                                            "one-slot: {} ← {}",
                                            os.new_entry_name, os.src_set_name
                                        )).small().color(egui::Color32::from_rgb(190, 160, 255)));
                                    });
                                }
                            }
                        });
                        ui.add_space(2.0);

                        // Fighter-wide discard (hitbox log only keeps its own remove API)
                        if move_names.len() > 1 {
                            ui.horizontal(|ui| {
                                ui.add_space(8.0);
                                if ui.small_button(format!("× all {fighter_display} hitbox edits")).clicked() {
                                    remove_fighter = Some(fighter_name.clone());
                                }
                            });
                        }
                    }
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Export All").on_hover_text("Export every logged hitbox edit to a folder").clicked() {
                        export_all = true;
                    }
                });
            });

        self.show_edit_log = open;

        // Apply deferred actions
        if export_all {
            self.export_all_edits();
        }
        if let Some((f, m)) = remove_move {
            self.state.edit_log.remove_move(&f, &m);
            // Un-send this move's live hitbox rules too.
            if self.hitbox_rules_store.remove(&format!("{f}/{m}")).is_some() {
                let all: Vec<crate::game_link::HitboxRuleWire> =
                    self.hitbox_rules_store.values().flatten().cloned().collect();
                self.game_link.send_hitbox_rules(&all);
            }
        }
        if let Some(f) = remove_fighter {
            self.state.edit_log.remove_fighter(&f);
            let before = self.hitbox_rules_store.len();
            self.hitbox_rules_store.retain(|k, _| !k.starts_with(&format!("{f}/")));
            if self.hitbox_rules_store.len() != before {
                let all: Vec<crate::game_link::HitboxRuleWire> =
                    self.hitbox_rules_store.values().flatten().cloned().collect();
                self.game_link.send_hitbox_rules(&all);
            }
        }
        if let Some(key) = remove_call_key {
            self.state.effect_call_edits.remove(&key);
            self.apply_effect_call_edits_to_current();
            self.push_effect_rules(); // discarded disabled-calls stop suppressing
        }
        if let Some(f) = clear_eff_fighter {
            self.eff_mods.remove(&f);
        }
        if let Some(hash) = clear_tweak_hash {
            self.live_overrides.clear_tweak(hash);
        }
        if let Some((fighter, move_name)) = export_move {
            self.export_logged_move(&fighter, &move_name);
        }
    }

    fn export_logged_move(&mut self, fighter: &str, move_name: &str) {
        let record = match self.state.edit_log.entries
            .get(fighter)
            .and_then(|m| m.get(move_name))
            .cloned()
        {
            Some(r) => r,
            None => return,
        };

        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = &self.export_dir {
            dialog = dialog.set_directory(dir);
        }
        let dest = match dialog.pick_folder() {
            Some(d) => d,
            None => return,
        };
        self.export_dir = Some(dest.clone());
        save_config_path("export_dir", &dest);

        let plugin_name = format!("{}_{}_mod", fighter, move_name.to_lowercase().replace(' ', "_"));
        let edits = vec![(fighter.to_string(), move_name.to_string(), record.script.clone())];
        let project = crate::acmd::build_mod_project(&edits, &plugin_name);
        match write_mod_project(&project, &dest) {
            Ok(root) => self.state.status = format!("Exported project to {}", root.display()),
            Err(e)   => self.state.status = format!("Export failed: {}", e),
        }
    }

    fn export_all_edits(&mut self) {
        if self.state.edit_log.is_empty() { return; }

        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = &self.export_dir {
            dialog = dialog.set_directory(dir);
        }
        let dest = match dialog.pick_folder() {
            Some(d) => d,
            None => return,
        };
        self.export_dir = Some(dest.clone());
        save_config_path("export_dir", &dest);

        let edits: Vec<(String, String, crate::data::AcmdScript)> = self.state.edit_log.entries
            .iter()
            .flat_map(|(fighter, moves)| {
                moves.iter().map(move |(move_name, record)| {
                    (fighter.clone(), move_name.clone(), record.script.clone())
                })
            })
            .collect();

        let plugin_name = "hitbox_mod";
        let project = crate::acmd::build_mod_project(&edits, plugin_name);
        match write_mod_project(&project, &dest) {
            Ok(root) => self.state.status = format!("Exported {} move(s) to {}", edits.len(), root.display()),
            Err(e)   => self.state.status = format!("Export failed: {}", e),
        }
    }

    /// Snapshot the current hitboxes/script into the edit log for the active fighter+move —
    /// but only when they actually DIFFER from the pristine load (the log is an edit tree,
    /// not a browsing history).
    fn commit_current_edits(&mut self) {
        let fighter = match self.state.selected_fighter.and_then(|i| self.state.fighters.get(i)) {
            Some(f) => f.clone(),
            None => return,
        };
        let move_name = match &self.state.selected_move {
            Some(m) => m.name.clone(),
            None => return,
        };
        if self.state.script.stmts.is_empty() && self.state.hitboxes.is_empty() {
            return;
        }
        let already_logged = self
            .state
            .edit_log
            .entries
            .get(&fighter.name)
            .map(|m| m.contains_key(&move_name))
            .unwrap_or(false);
        if self.state.hitboxes == self.state.hitboxes_pristine && !already_logged {
            return;
        }
        // Capture-sourced moves have no base script text — synthesize one from the hitboxes
        // so the export path works the same as for GitHub-fetched scripts.
        let script = if self.state.script.stmts.is_empty() {
            synthesize_script_from_hitboxes(&self.state.hitboxes)
        } else {
            rebuild_script_from_hitboxes(&self.state.script, &self.state.hitboxes)
        };
        self.state.edit_log.save(
            &fighter.name,
            &fighter.display_name,
            &move_name,
            script,
            self.state.hitboxes.clone(),
        );
    }

    fn export_acmd_source(&mut self) {
        let fighter = match self.state.selected_fighter.and_then(|i| self.state.fighters.get(i)) {
            Some(f) => f.name.clone(),
            None => return,
        };
        let move_name = match &self.state.selected_move {
            Some(m) => m.name.clone(),
            None => return,
        };

        let script = rebuild_script_from_hitboxes(&self.state.script, &self.state.hitboxes);

        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = &self.export_dir {
            dialog = dialog.set_directory(dir);
        }
        let dest = match dialog.pick_folder() {
            Some(d) => d,
            None => return,
        };
        self.export_dir = Some(dest.clone());
        save_config_path("export_dir", &dest);

        let plugin_name = format!("{}_{}_mod", fighter, move_name.to_lowercase().replace(' ', "_"));
        let edits = vec![(fighter.clone(), move_name.clone(), script)];
        let project = crate::acmd::build_mod_project(&edits, &plugin_name);
        match write_mod_project(&project, &dest) {
            Ok(root) => self.state.status = format!("Exported project to {}", root.display()),
            Err(e)   => self.state.status = format!("Export failed: {}", e),
        }
    }

    fn draw_left_panel(&mut self, ui: &mut Ui) {
        if self.state.data_root.is_none() {
            ui.label(egui::RichText::new("Click 'Open Data Root' above").color(egui::Color32::YELLOW));
            ui.label(egui::RichText::new("to load fighter files.").color(egui::Color32::YELLOW));
            return;
        }

        let available = ui.available_height();
        let half = (available - 80.0) / 2.0; // 80 accounts for headings + search bars + separator

        ui.heading("Fighters");
        ui.add(egui::TextEdit::singleline(&mut self.fighter_search)
            .hint_text("Search fighters…")
            .desired_width(f32::INFINITY));
        let fighter_query = self.fighter_search.to_lowercase();
        ScrollArea::vertical().id_salt("fighters").max_height(half).auto_shrink([false, false]).show(ui, |ui| {
            let fighters: Vec<(usize, String)> = self.state.fighters.iter()
                .enumerate()
                .filter(|(_, f)| fighter_query.is_empty() || f.display_name.to_lowercase().contains(&fighter_query))
                .map(|(i, f)| (i, f.display_name.clone()))
                .collect();
            for (i, name) in fighters {
                let selected = self.state.selected_fighter == Some(i);
                if ui.selectable_label(selected, &name).clicked() && !selected {
                    self.select_fighter(i);
                }
            }
        });

        ui.separator();
        ui.heading("Moves");
        ui.add(egui::TextEdit::singleline(&mut self.move_search)
            .hint_text("Search moves…")
            .desired_width(f32::INFINITY));
        let move_query = self.move_search.to_lowercase();
        ScrollArea::vertical().id_salt("moves").max_height(half).auto_shrink([false, false]).show(ui, |ui| {
            // Group the (filtered) moves into the familiar move families, preserving order.
            let mut groups: Vec<Vec<MoveEntry>> =
                (0..MOVE_CATEGORY_LABELS.len()).map(|_| Vec::new()).collect();
            for m in self.move_list.iter().filter(|m| {
                move_query.is_empty()
                    || m.name.to_lowercase().contains(&move_query)
                    || format_move_name(&m.name).to_lowercase().contains(&move_query)
            }) {
                groups[move_category_index(&m.name)].push(m.clone());
            }
            let mut to_select: Option<MoveEntry> = None;
            for (ci, group) in groups.iter().enumerate() {
                if group.is_empty() {
                    continue;
                }
                egui::CollapsingHeader::new(format!("{} ({})", MOVE_CATEGORY_LABELS[ci], group.len()))
                    .id_salt(("movecat", ci))
                    .default_open(true)
                    .show(ui, |ui| {
                        for m in group {
                            let selected = self
                                .state
                                .selected_move
                                .as_ref()
                                .map(|sm| sm.hash == m.hash)
                                .unwrap_or(false);
                            let label =
                                format!("{} ({}f)", format_move_name(&m.name), m.frame_count);
                            if ui.selectable_label(selected, &label).clicked() && !selected {
                                to_select = Some(m.clone());
                            }
                        }
                    });
            }
            if let Some(m) = to_select {
                self.select_move(m);
            }
        });
    }

    fn draw_right_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading("Hitboxes");
            if self.state.selected_move.is_some() {
                let btn_text = if self.fetching_acmd { "..." } else { "Fetch ACMD" };
                if ui.add_enabled(!self.fetching_acmd, egui::Button::new(btn_text))
                    .on_hover_text("Fetch hitboxes from GitHub ACMD scripts")
                    .clicked()
                {
                    self.fetch_acmd();
                }
                let has_capture = self
                    .current_motion_hash()
                    .map(|m| !self.game_link.captures_for(m).is_empty())
                    .unwrap_or(false);
                if ui
                    .add_enabled(has_capture, egui::Button::new("⟳ Live"))
                    .on_hover_text(
                        "Load this move's hitboxes + effects from the live game capture \
                         (exact values; perform the move in game to capture it)",
                    )
                    .clicked()
                {
                    self.load_from_capture();
                }
                if !self.state.acmd_source.is_empty() {
                    let (txt, color) = if self.state.acmd_source == "Live capture" {
                        ("● Live", egui::Color32::from_rgb(90, 220, 90))
                    } else {
                        ("● GitHub", egui::Color32::from_rgb(160, 160, 220))
                    };
                    ui.colored_label(color, txt)
                        .on_hover_text(format!("Data source: {}", self.state.acmd_source));
                }
            }
            if ui.button("+").clicked() {
                self.show_add_hitbox = !self.show_add_hitbox;
            }
        });

        if let Some(err) = &self.acmd_error.clone() {
            ui.colored_label(Color32::RED, err);
        }

        if self.show_add_hitbox {
            ui.group(|ui| {
                ui.label("Bone:");
                if self.bone_names.is_empty() {
                    ui.text_edit_singleline(&mut self.add_bone);
                } else {
                    egui::ComboBox::from_id_salt("add_bone_select")
                        .selected_text(&self.add_bone)
                        .show_ui(ui, |ui| {
                            for name in &self.bone_names.clone() {
                                ui.selectable_value(&mut self.add_bone, name.clone(), name);
                            }
                        });
                }
                ui.add(egui::Slider::new(&mut self.add_size, 0.1..=20.0).text("Size"));
                ui.add(egui::Slider::new(&mut self.add_damage, 0.0..=50.0).text("Damage"));
                angle_picker(ui, &mut self.add_angle);
                ui.add(egui::Slider::new(&mut self.add_kb_base, 0..=200).text("KB Base"));
                ui.add(egui::Slider::new(&mut self.add_kb_scaling, 0..=200).text("KB Scaling"));
                if ui.button("Add").clicked() {
                    let next_id = self.state.hitboxes.iter().map(|h| h.id).max().map(|m| m + 1).unwrap_or(0);
                    let mut hb = Hitbox::default();
                    hb.id = next_id;
                    hb.bone_name = self.add_bone.clone();
                    hb.damage = self.add_damage;
                    hb.angle = self.add_angle;
                    hb.kb_scaling = self.add_kb_scaling;
                    hb.kb_base = self.add_kb_base;
                    hb.size = self.add_size;
                    hb.active_start = self.state.current_frame;
                    hb.active_end = self.state.current_frame + 5;
                    self.state.hitboxes.push(hb);
                    self.show_add_hitbox = false;
                }
            });
        }

        ScrollArea::vertical().id_salt("hitboxes").show(ui, |ui| {
            let mut to_delete = None;
            for (i, hb) in self.state.hitboxes.iter().enumerate() {
                let color = hitbox_display_color(hb);
                let selected = self.selected_hitbox == Some(i);
                ui.horizontal(|ui| {
                    ui.colored_label(color, "*");
                    let shape = if hb.capsule_end.is_some() { "⬭" } else { "●" };
                    let label = match hb.category {
                        1 => format!(
                            "{} GRAB #{} {} [{}-{}]",
                            shape, hb.id, hb.bone_name, hb.active_start, hb.active_end
                        ),
                        2 => format!(
                            "{} WIND ~{:.1} [{}-{}]",
                            shape, hb.size, hb.active_start, hb.active_end
                        ),
                        _ => format!(
                            "{} #{} {} {:.1}dmg {} [{}-{}]",
                            shape,
                            hb.id,
                            hb.bone_name,
                            hb.damage,
                            angle_short_label(hb.angle),
                            hb.active_start,
                            hb.active_end
                        ),
                    };
                    if ui.selectable_label(selected, &label).clicked() {
                        self.selected_hitbox = if selected { None } else { Some(i) };
                    }
                    if ui.small_button("X").clicked() {
                        to_delete = Some(i);
                    }
                });
            }
            if let Some(i) = to_delete {
                self.state.hitboxes.remove(i);
                if self.selected_hitbox == Some(i) { self.selected_hitbox = None; }
            }
        });

        // Property editor for selected hitbox — fields shown depend on the collision family.
        if let Some(idx) = self.selected_hitbox {
            let bone_names = self.bone_names.clone();
            let max_frame = self.state.total_frames.saturating_sub(1).max(1);
            if let Some(hb) = self.state.hitboxes.get_mut(idx) {
                ui.separator();
                let (cat_label, cat_color) = match hb.category {
                    1 => ("Grab box", egui::Color32::from_rgb(80, 200, 255)),
                    2 => ("Wind box", egui::Color32::from_rgb(120, 240, 140)),
                    _ => ("Attack hitbox", egui::Color32::from_rgb(255, 120, 120)),
                };
                ui.horizontal(|ui| {
                    ui.heading("Properties");
                    ui.colored_label(cat_color, cat_label);
                });
                ScrollArea::vertical().id_salt("props").show(ui, |ui| {
                    // ── Bone (all families) ──────────────────────────────
                    ui.horizontal(|ui| {
                        ui.label("Bone:");
                        if bone_names.is_empty() {
                            ui.text_edit_singleline(&mut hb.bone_name);
                        } else {
                            egui::ComboBox::from_id_salt("edit_bone_select")
                                .selected_text(&hb.bone_name)
                                .show_ui(ui, |ui| {
                                    for name in &bone_names {
                                        ui.selectable_value(&mut hb.bone_name, name.clone(), name);
                                    }
                                });
                        }
                    });
                    // Wind has no id; attack + grab do.
                    if hb.category != 2 {
                        ui.horizontal(|ui| {
                            ui.label("ID:");
                            ui.add(egui::DragValue::new(&mut hb.id));
                            if hb.category == 0 {
                                ui.label("Part:");
                                ui.add(egui::DragValue::new(&mut hb.part));
                            }
                        });
                    }

                    // ── Attack-only combat fields ────────────────────────
                    if hb.category == 0 {
                        wide_slider_f32(ui, &mut hb.damage, 0.0..=50.0, "Damage");
                        angle_picker(ui, &mut hb.angle);
                        wide_slider_i32(ui, &mut hb.kb_base, 0..=200, "KB Base");
                        wide_slider_i32(ui, &mut hb.kb_scaling, 0..=200, "KB Scaling");
                        wide_slider_i32(ui, &mut hb.fkb, 0..=200, "Fixed KB");
                    }

                    // ── Size + position/shape (all families) ─────────────
                    wide_slider_f32(ui, &mut hb.size, 0.1..=20.0, "Size");
                    ui.collapsing("Position / Shape", |ui| {
                        wide_slider_f32(ui, &mut hb.offset_x, -20.0..=20.0, "Offset X");
                        wide_slider_f32(ui, &mut hb.offset_y, -20.0..=20.0, "Offset Y");
                        wide_slider_f32(ui, &mut hb.offset_z, -20.0..=20.0, "Offset Z");
                        let is_capsule = hb.capsule_end.is_some();
                        let mut toggle = is_capsule;
                        ui.checkbox(&mut toggle, "Capsule (second endpoint)");
                        if toggle && !is_capsule {
                            hb.capsule_end = Some([hb.offset_x, hb.offset_y, hb.offset_z]);
                        } else if !toggle && is_capsule {
                            hb.capsule_end = None;
                        }
                        if let Some(ref mut end) = hb.capsule_end {
                            wide_slider_f32(ui, &mut end[0], -20.0..=20.0, "End X");
                            wide_slider_f32(ui, &mut end[1], -20.0..=20.0, "End Y");
                            wide_slider_f32(ui, &mut end[2], -20.0..=20.0, "End Z");
                        }
                    });

                    // ── Attack-only detail sections ──────────────────────
                    if hb.category == 0 {
                        ui.collapsing("Hit Properties", |ui| {
                            wide_slider_f32(ui, &mut hb.hitlag_mult, 0.0..=5.0, "Hitlag Mult");
                            wide_slider_f32(ui, &mut hb.sdi_mult, 0.0..=5.0, "SDI Mult");
                            wide_slider_f32(ui, &mut hb.hitbox_attr, -10.0..=10.0, "Hitbox Attr");
                            ui.add(egui::DragValue::new(&mut hb.is_add_attack).prefix("Add Attack: "));
                            ui.add(egui::DragValue::new(&mut hb.ground_or_air).prefix("Ground/Air: "));

                            setoff_combo(ui, &mut hb.setoff_kind, "setoff_kind");
                            lr_check_combo(ui, &mut hb.lr_check, "lr_check");

                            ui.checkbox(&mut hb.is_clang, "Clang");
                            ui.checkbox(&mut hb.is_mtk, "MTK (intangible)");
                            ui.checkbox(&mut hb.is_shield_disable, "Shield Disable");
                            ui.checkbox(&mut hb.is_reflectable, "Reflectable");
                            ui.checkbox(&mut hb.is_absorbable, "Absorbable");
                            ui.checkbox(&mut hb.is_landing_attack, "Landing Attack");
                            ui.checkbox(&mut hb.no_finish_camera, "No Finish Camera");
                        });
                        ui.collapsing("Collision Masks", |ui| {
                            situation_mask_combo(ui, &mut hb.situation_mask, "sit_mask");
                            category_mask_combo(ui, &mut hb.category_mask, "cat_mask");
                            part_mask_combo(ui, &mut hb.part_mask, "part_mask");
                        });
                        ui.collapsing("Effect / Sound", |ui| {
                            collision_attr_combo(ui, &mut hb.collision_attr, "col_attr");
                            sound_level_combo(ui, &mut hb.sound_level, "snd_lvl");
                            sound_attr_combo(ui, &mut hb.sound_attr, "snd_attr");
                            attack_region_combo(ui, &mut hb.attack_region, "atk_region");
                        });
                    }

                    if hb.category == 2 {
                        ui.colored_label(
                            egui::Color32::GRAY,
                            "Wind boxes: size / position / timeline are editable and suppressible. \
                             The rest of the wind parameters use an undocumented arg layout and \
                             aren't exposed yet.",
                        );
                    }

                    // ── Timeline (all families) ──────────────────────────
                    wide_slider_u32(ui, &mut hb.active_start, 0..=max_frame, "Start Frame");
                    wide_slider_u32(ui, &mut hb.active_end, 0..=max_frame, "End Frame");
                });
            }
        }
    }

    fn draw_effects_panel(&mut self, ui: &mut Ui) {
        let current = self.state.current_frame;

        ui.horizontal(|ui| {
            ui.heading("Effect spawns");
            ui.label(egui::RichText::new(format!("— Frame {}", current))
                .color(egui::Color32::LIGHT_GRAY));
        });
        ui.checkbox(&mut self.state.show_all_effect_calls, "show all frames");
        ui.separator();

        let has_effect_data =
            !self.state.effect_script.stmts.is_empty() || !self.state.effects.is_empty();

        if !has_effect_data {
            ui.colored_label(egui::Color32::GRAY, "Effect data unavailable");
            ui.label(egui::RichText::new("Fetch ACMD to load effect data.")
                .small()
                .color(egui::Color32::DARK_GRAY));
        } else {
            let visible: Vec<usize> = self.state.effects.iter().enumerate()
                .filter(|(_, e)| {
                    if self.state.show_all_effect_calls {
                        return true;
                    }
                    // Follow effects use their real end; one-shots (end == start) get a short
                    // display window so they don't vanish one frame after they spawn.
                    let end = if e.follows_bone {
                        e.active_end
                    } else {
                        e.active_end.max(e.active_start.saturating_add(12))
                    };
                    current >= e.active_start && current <= end
                })
                .map(|(i, _)| i)
                .collect();

            if visible.is_empty() {
                ui.colored_label(egui::Color32::GRAY, "No effects on this frame");
            } else {
                egui::ScrollArea::vertical().id_salt("effects_list").max_height(180.0).show(ui, |ui| {
                    for &i in &visible {
                        let effect = &self.state.effects[i];
                        ui.horizontal(|ui| {
                            // Orange = follows bone, yellow = one-shot, gray = disabled
                            let dot_color = if effect.disabled {
                                egui::Color32::DARK_GRAY
                            } else if effect.follows_bone {
                                egui::Color32::from_rgb(255, 165, 0)
                            } else {
                                egui::Color32::from_rgb(255, 220, 0)
                            };
                            ui.colored_label(dot_color, "●");
                            let selected = self.state.selected_effect_call == Some(i);
                            let mut text = egui::RichText::new(&effect.effect_name).monospace();
                            if effect.disabled {
                                text = text.strikethrough().color(egui::Color32::DARK_GRAY);
                            }
                            if ui
                                .selectable_label(selected, text)
                                .on_hover_text(format!(
                                    "bone {} · f{}-{}",
                                    effect.bone_name, effect.active_start, effect.active_end
                                ))
                                .clicked()
                            {
                                self.state.selected_effect_call =
                                    if selected { None } else { Some(i) };
                            }
                        });
                    }
                });
            }

            if ui.small_button("＋ Add effect call").clicked() {
                let call = crate::data::EffectCall {
                    effect_name: "sys_hit_elec".into(),
                    bone_name: "top".into(),
                    offset: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: 1.0,
                    follows_bone: true,
                    active_start: current,
                    active_end: current.saturating_add(10),
                    disabled: false,
                };
                self.state.effects.push(call.clone());
                let idx = self.state.effects.len() - 1;
                if let Some(mv) = self.current_move_key() {
                    self.state
                        .effect_call_edits
                        .entry(mv.clone())
                        .or_default()
                        .push(crate::data::EffectCallEdit {
                            index: idx,
                            op: crate::data::EffectCallOp::Add(call),
                        });
                    self.state
                        .effect_call_full
                        .insert(mv, self.state.effects.clone());
                }
                self.state.selected_effect_call = Some(idx);
            }

            // Backspace / Delete removes the selected spawn (unless typing in a field).
            if let Some(sel) = self.state.selected_effect_call {
                let pressed = ui.input(|i| {
                    i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)
                });
                let typing = ui.memory(|m| m.focused().is_some());
                if pressed && !typing {
                    self.delete_effect_call(sel);
                }
            }

            // ── Selected-call editor ────────────────────────────────────────
            if let Some(i) = self
                .state
                .selected_effect_call
                .filter(|i| *i < self.state.effects.len())
            {
                ui.separator();
                let pristine = self.state.effects_pristine.get(i).cloned();
                let bone_names = self.bone_names.clone();
                let mut changed = false;
                // Discrete swaps (effect name / bone) need a preview rebuild; numeric drags don't.
                let mut respawn_needed = false;
                let mut toggle_pick = false;
                {
                    let ec = &mut self.state.effects[i];
                    let orig = |ui: &mut Ui, txt: String| {
                        ui.label(
                            egui::RichText::new(txt).small().color(egui::Color32::GRAY),
                        );
                    };
                    egui::Grid::new("effect_call_edit").num_columns(3).striped(true).show(ui, |ui| {
                        ui.label("Effect");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut ec.effect_name).desired_width(120.0))
                                .changed();
                            if ui
                                .small_button("▾")
                                .on_hover_text("Pick from live kinds + every eff")
                                .clicked()
                            {
                                toggle_pick = true;
                            }
                        });
                        if let Some(p) = &pristine {
                            orig(ui, format!("orig {}", p.effect_name));
                        } else {
                            ui.label(egui::RichText::new("added").small().color(egui::Color32::GRAY));
                        }
                        ui.end_row();

                        ui.label("Bone");
                        if bone_names.is_empty() {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut ec.bone_name).desired_width(140.0))
                                .changed();
                        } else {
                            egui::ComboBox::from_id_salt("effect_bone_select")
                                .selected_text(&ec.bone_name)
                                .width(140.0)
                                .show_ui(ui, |ui| {
                                    for name in &bone_names {
                                        if ui
                                            .selectable_value(&mut ec.bone_name, name.clone(), name)
                                            .clicked()
                                        {
                                            changed = true;
                                            respawn_needed = true;
                                        }
                                    }
                                });
                        }
                        if let Some(p) = &pristine {
                            orig(ui, format!("orig {}", p.bone_name));
                        } else {
                            ui.label("");
                        }
                        ui.end_row();

                        ui.label("Offset");
                        ui.horizontal(|ui| {
                            for v in ec.offset.iter_mut() {
                                changed |= ui.add(egui::DragValue::new(v).speed(0.05)).changed();
                            }
                        });
                        if let Some(p) = &pristine {
                            orig(ui, format!("orig [{:.2} {:.2} {:.2}]", p.offset[0], p.offset[1], p.offset[2]));
                        } else {
                            ui.label("");
                        }
                        ui.end_row();

                        ui.label("Rotation");
                        ui.horizontal(|ui| {
                            for v in ec.rotation.iter_mut() {
                                changed |= ui.add(egui::DragValue::new(v).speed(0.5)).changed();
                            }
                        });
                        if let Some(p) = &pristine {
                            orig(ui, format!("orig [{:.1} {:.1} {:.1}]", p.rotation[0], p.rotation[1], p.rotation[2]));
                        } else {
                            ui.label("");
                        }
                        ui.end_row();

                        ui.label("Scale");
                        changed |= ui.add(egui::DragValue::new(&mut ec.scale).speed(0.02)).changed();
                        if let Some(p) = &pristine {
                            orig(ui, format!("orig {:.2}", p.scale));
                        } else {
                            ui.label("");
                        }
                        ui.end_row();

                        // One-shot effects have no meaningful "end" (they play their own
                        // lifetime), so only follow effects show an end frame — otherwise the
                        // row showed confusing "30-30" or "30-9999" ranges.
                        ui.label("Spawn frame");
                        ui.horizontal(|ui| {
                            changed |= ui.add(egui::DragValue::new(&mut ec.active_start)).changed();
                            if ec.follows_bone {
                                ui.label("→ until");
                                changed |= ui.add(egui::DragValue::new(&mut ec.active_end)).changed();
                            } else {
                                ui.label(
                                    egui::RichText::new("(one-shot)")
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            }
                        });
                        if let Some(p) = &pristine {
                            if p.follows_bone {
                                orig(ui, format!("orig {}→{}", p.active_start, p.active_end));
                            } else {
                                orig(ui, format!("orig frame {}", p.active_start));
                            }
                        } else {
                            ui.label("");
                        }
                        ui.end_row();

                        ui.label("Disabled");
                        changed |= ui.checkbox(&mut ec.disabled, "don't spawn").changed();
                        ui.label("");
                        ui.end_row();
                    });
                }

                if toggle_pick {
                    self.effect_pick_open = !self.effect_pick_open;
                }
                // ── Inline searchable effect picker (opens next to the Effect field) ──
                if self.effect_pick_open {
                    if let Some(name) = self.draw_effect_name_picker(ui) {
                        self.state.effects[i].effect_name = name;
                        changed = true;
                        respawn_needed = true;
                        self.effect_pick_open = false;
                    }
                }

                let mut duplicate: Option<crate::data::EffectCall> = None;
                ui.horizontal(|ui| {
                    if pristine.is_some() && ui.small_button("Reset to original").clicked() {
                        if let Some(p) = &pristine {
                            self.state.effects[i] = p.clone();
                        }
                        if let Some(mv) = self.current_move_key() {
                            if let Some(edits) = self.state.effect_call_edits.get_mut(&mv) {
                                edits.retain(|e| e.index != i);
                            }
                            self.state
                                .effect_call_full
                                .insert(mv, self.state.effects.clone());
                        }
                        self.push_effect_rules();
                    }
                    if ui
                        .small_button("⧉ Duplicate")
                        .on_hover_text("Add a copy of this spawn as a new effect call")
                        .clicked()
                    {
                        duplicate = self.state.effects.get(i).cloned();
                    }
                });
                if let Some(mut call) = duplicate {
                    // A duplicate is a brand-new (authored) call, never a modify of pristine.
                    call.disabled = false;
                    self.state.effects.push(call.clone());
                    let new_idx = self.state.effects.len() - 1;
                    if let Some(mv) = self.current_move_key() {
                        self.state
                            .effect_call_edits
                            .entry(mv.clone())
                            .or_default()
                            .push(crate::data::EffectCallEdit {
                                index: new_idx,
                                op: crate::data::EffectCallOp::Add(call),
                            });
                        self.state
                            .effect_call_full
                            .insert(mv, self.state.effects.clone());
                    }
                    self.state.selected_effect_call = Some(new_idx);
                    self.push_effect_rules();
                }
                if changed {
                    self.record_effect_call_edit(i);
                    self.push_effect_rules();
                    // Swapping the effect or bone changes what spawns — rebuild the local
                    // preview so the new effect shows (numeric drags don't need this).
                    if respawn_needed {
                        self.respawn_effects();
                    }
                }
            }
        }

        ui.separator();

        // VFX file check
        let fighter_name = self.state.selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone());

        if let (Some(name), Some(root)) = (fighter_name, &self.state.data_root) {
            // Check common locations for the .eff file
            let candidates = [
                root.join("effect").join("fighter").join(&name).join(format!("ef_{}.eff", name)),
                root.join("fighter").join(&name).join("effect").join(format!("ef_{}.eff", name)),
            ];
            let found = candidates.iter().find(|p| p.exists());
            if found.is_some() {
                ui.colored_label(egui::Color32::from_rgb(100, 220, 100), "VFX file: present");
            } else if self.state.eff_index.is_some() {
                ui.colored_label(egui::Color32::from_rgb(100, 220, 100), "VFX file: loaded manually");
            } else {
                ui.colored_label(egui::Color32::GRAY, "VFX file: not found");
                ui.label(egui::RichText::new("Extract effect/fighter/ from data.arc, or:")
                    .small().color(egui::Color32::DARK_GRAY));
                if ui.button(format!("Browse for ef_{}.eff…", name)).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Effect file", &["eff"])
                        .set_title(format!("Open ef_{}.eff", name))
                        .pick_file()
                    {
                        self.load_eff_file(&path);
                        self.respawn_effects();
                    }
                }
            }
        }
    }

    /// Searchable effect picker: live in-game kinds first, then a scan of every eff in the
    /// pool. Returns the chosen effect name (to assign to the selected spawn) when clicked.
    fn draw_effect_name_picker(&mut self, ui: &mut Ui) -> Option<String> {
        // Ensure the donor pool is available for the full-eff search.
        if self.effect_pool.is_none() {
            if let Some(root) = self
                .export_dir
                .clone()
                .or_else(|| self.state.data_root.clone())
            {
                self.effect_pool = Some(crate::effect_pool::EffectPool::new(root));
            }
        }
        let scanning = self.effect_pool.as_mut().map(|p| p.tick(6)).unwrap_or(false);

        let mut picked: Option<String> = None;
        egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🔍 Pick effect").strong());
                    if ui.small_button("✕").on_hover_text("Close picker").clicked() {
                        self.effect_pick_open = false;
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.effect_pick_search)
                        .hint_text("search effect names")
                        .desired_width(220.0),
                );
                let q = self.effect_pick_search.to_lowercase();

                // Live in-game kinds matching the query (deduped), most-recently-updated first.
                let mut live: Vec<String> = self
                    .game_link
                    .kinds()
                    .into_iter()
                    .map(|(_, k)| k.name)
                    .filter(|n| q.is_empty() || n.to_lowercase().contains(&q))
                    .collect();
                live.sort();
                live.dedup();

                let pool_hits: Vec<String> = self
                    .effect_pool
                    .as_ref()
                    .map(|p| {
                        p.search(&self.effect_pick_search, 60)
                            .into_iter()
                            .map(|(_, name)| name)
                            .collect()
                    })
                    .unwrap_or_default();

                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    if !live.is_empty() {
                        ui.label(
                            egui::RichText::new("live in game")
                                .small()
                                .color(egui::Color32::from_rgb(120, 200, 120)),
                        );
                        for name in &live {
                            if ui.selectable_label(false, name).clicked() {
                                picked = Some(name.clone());
                            }
                        }
                        ui.separator();
                    }
                    let (done, total) = self
                        .effect_pool
                        .as_ref()
                        .map(|p| p.progress())
                        .unwrap_or((0, 0));
                    ui.label(
                        egui::RichText::new(if scanning {
                            format!("all effs (scanning {done}/{total})")
                        } else {
                            "all effs".into()
                        })
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    for name in pool_hits.iter().filter(|n| !live.contains(n)) {
                        if ui.selectable_label(false, name).clicked() {
                            picked = Some(name.clone());
                        }
                    }
                });
            });
        if scanning {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
        }
        picked
    }

    /// Key for `effect_call_edits`: fighter-scoped so moves with the same name on
    /// different fighters don't collide ("mario/attack_air_n").
    fn current_move_key(&self) -> Option<String> {
        let fighter = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone())?;
        let mv = self.state.selected_move.as_ref()?.name.clone();
        Some(format!("{fighter}/{mv}"))
    }

    /// Rebuild `state.effects` from the pristine parse + this move's saved edits.
    /// Idempotent — used at move load and after loading a project.
    fn apply_effect_call_edits_to_current(&mut self) {
        self.state.effects = self.state.effects_pristine.clone();
        let Some(mv) = self.current_move_key() else { return };
        let Some(edits) = self.state.effect_call_edits.get(&mv) else { return };
        for edit in edits {
            match &edit.op {
                crate::data::EffectCallOp::Modify(call) => {
                    if let Some(slot) = self.state.effects.get_mut(edit.index) {
                        *slot = call.clone();
                    }
                }
                crate::data::EffectCallOp::Add(call) => {
                    self.state.effects.push(call.clone());
                }
                crate::data::EffectCallOp::Remove => {
                    if let Some(slot) = self.state.effects.get_mut(edit.index) {
                        slot.disabled = true;
                    }
                }
            }
        }
    }

    /// Fold the eff editor's current diff into the per-fighter project store.
    fn sync_eff_mods_from_editor(&mut self) {
        let Some(rel) = self.eff_editor.loaded_rel() else { return };
        let authored = self.eff_editor.collect_authored_edits();
        let fighter = crate::mod_project::fighter_from_source_rel(&rel);
        let entry = self.eff_mods.entry(fighter).or_default();
        entry.source_rel = rel;
        entry.authored = authored; // one_slot records are preserved as-is
    }

    fn build_project(&mut self) -> crate::mod_project::ModProjectFile {
        use crate::mod_project::ModProjectFile;
        self.sync_eff_mods_from_editor();
        let mut project = ModProjectFile {
            version: crate::mod_project::PROJECT_VERSION,
            name: self.project_name.clone(),
            fighters: HashMap::new(),
        };
        for (fighter, moves) in &self.state.edit_log.entries {
            let fm = project.fighters.entry(fighter.clone()).or_default();
            fm.display = moves
                .values()
                .next()
                .map(|r| r.fighter_display.clone())
                .unwrap_or_else(|| fighter.clone());
            fm.acmd = moves.clone();
        }
        for (key, edits) in &self.state.effect_call_edits {
            if edits.is_empty() {
                continue;
            }
            let (fighter, mv) = key.split_once('/').unwrap_or(("unknown", key.as_str()));
            let fm = project.fighters.entry(fighter.to_string()).or_default();
            fm.effect_calls.insert(mv.to_string(), edits.clone());
            if let Some(full) = self.state.effect_call_full.get(key) {
                fm.effect_calls_full.insert(mv.to_string(), full.clone());
            }
        }
        for (fighter, eff) in &self.eff_mods {
            if eff.is_empty() {
                continue;
            }
            project.fighters.entry(fighter.clone()).or_default().eff = Some(eff.clone());
        }
        // User-set live color×/speed multipliers → LiveTweaks, attached to every fighter
        // whose spawn lists use the effect (falls back to the selected fighter).
        let tweaks = self.live_overrides.tweaked();
        if !tweaks.is_empty() {
            let selected = self
                .state
                .selected_fighter
                .and_then(|i| self.state.fighters.get(i))
                .map(|f| f.name.clone());
            for (hash, form) in tweaks {
                let identity_color = (form.rainbow.color.red - 1.0).abs() < 1e-4
                    && (form.rainbow.color.green - 1.0).abs() < 1e-4
                    && (form.rainbow.color.blue - 1.0).abs() < 1e-4;
                let identity_speed = (form.speed - 1.0).abs() < 1e-4;
                if identity_color && identity_speed {
                    continue;
                }
                let tweak = crate::mod_project::LiveTweak {
                    effect_name: form.effect_name.clone(),
                    color: (!identity_color).then(|| {
                        let c = form.rainbow.color;
                        [c.red, c.green, c.blue, c.alpha]
                    }),
                    speed: (!identity_speed).then_some(form.speed),
                };
                let mut owners: Vec<String> = self
                    .state
                    .effect_call_full
                    .iter()
                    .filter(|(_, calls)| {
                        calls.iter().any(|c| effect_name_hash(&c.effect_name) == hash)
                    })
                    .filter_map(|(key, _)| key.split_once('/').map(|(f, _)| f.to_string()))
                    .collect();
                owners.sort();
                owners.dedup();
                if owners.is_empty() {
                    if let Some(f) = &selected {
                        owners.push(f.clone());
                    }
                }
                for f in owners {
                    let fm = project.fighters.entry(f).or_default();
                    if !fm.live_tweaks.iter().any(|t| t.effect_name == tweak.effect_name) {
                        fm.live_tweaks.push(tweak.clone());
                    }
                }
            }
        }
        project
    }

    fn save_project(&mut self) {
        let project = self.build_project();
        if project.is_empty() {
            self.state.status = "No edits to save yet.".into();
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .set_file_name(crate::mod_project::PROJECT_FILE_NAME)
            .add_filter("Mod project", &["json"]);
        if let Some(dir) = &self.export_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else { return };
        match serde_json::to_string_pretty(&project)
            .map_err(anyhow::Error::from)
            .and_then(|json| std::fs::write(&path, json).map_err(anyhow::Error::from))
        {
            Ok(()) => self.state.status = format!("Project saved to {}", path.display()),
            Err(e) => self.state.status = format!("Project save failed: {e}"),
        }
    }

    fn load_project(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Mod project", &["json"])
            .pick_file()
        else {
            return;
        };
        self.load_project_from(&path);
    }

    /// Export everything as an installable mod:
    ///   mod/     — drop-in Arcropolis data mod (rebuilt eff files + info.toml)
    ///   source/  — buildable smashline project (also auto-compiled in the background;
    ///              the built .nro lands in plugin/, build output in build.log)
    ///   modproject.json — re-openable for further editing
    fn export_full_mod(&mut self) {
        let project = self.build_project();
        if project.is_empty() {
            self.state.status = "No edits to export yet.".into();
            return;
        }
        let mut dialog = rfd::FileDialog::new().set_title("Export mod into folder…");
        if let Some(dir) = &self.export_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(dest) = dialog.pick_folder() else { return };
        self.export_dir = Some(dest.clone());
        save_config_path("export_dir", &dest);

        let mut report: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        // 1. Data mod: rebuilt eff files under mod/effect/fighter/<name>/… + info.toml.
        let mod_dir = dest.join("mod");
        for (fighter, fm) in &project.fighters {
            match &fm.eff {
                Some(eff) if !eff.is_empty() => {
                    let src_path = self.eff_editor.export_root().join(&eff.source_rel);
                    let result = std::fs::read(&src_path)
                        .map_err(anyhow::Error::from)
                        .and_then(|bytes| {
                            crate::eff_export::rebuild_eff_bytes(
                                &bytes,
                                eff,
                                Some(&self.eff_editor.export_root()),
                            )
                        })
                        .and_then(|rebuilt| {
                            let out = mod_dir.join(&eff.source_rel);
                            if let Some(parent) = out.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            std::fs::write(&out, rebuilt)?;
                            Ok(())
                        });
                    match result {
                        Ok(()) => report.push(format!(
                            "{fighter}: eff written ({} authored, {} one-slot)",
                            eff.authored.len(),
                            eff.one_slot.len()
                        )),
                        Err(e) => errors.push(format!("{fighter} eff: {e}")),
                    }
                }
                _ => report.push(format!(
                    "{fighter}: no authored eff edits — spawn/live edits ship via the plugin"
                )),
            }
        }

        // 2. smashline source project (hitboxes + effect spawn scripts + live tweaks).
        let acmd_edits: Vec<(String, String, crate::data::AcmdScript)> = project
            .fighters
            .iter()
            .flat_map(|(f, fm)| {
                fm.acmd
                    .iter()
                    .map(move |(m, r)| (f.clone(), m.clone(), r.script.clone()))
            })
            .collect();
        let effect_edits: Vec<(String, String, Vec<crate::data::EffectCall>)> = project
            .fighters
            .iter()
            .flat_map(|(f, fm)| {
                fm.effect_calls_full
                    .iter()
                    .map(move |(m, calls)| (f.clone(), m.clone(), calls.clone()))
            })
            .collect();
        let live_tweaks: Vec<crate::mod_project::LiveTweak> = {
            let mut v: Vec<crate::mod_project::LiveTweak> = Vec::new();
            for fm in project.fighters.values() {
                for t in &fm.live_tweaks {
                    if !v.iter().any(|x| x.effect_name == t.effect_name) {
                        v.push(t.clone());
                    }
                }
            }
            v
        };
        let plugin_name = format!(
            "{}_plugin",
            self.project_name.to_lowercase().replace([' ', '-'], "_")
        );
        let has_source = !acmd_edits.is_empty() || !effect_edits.is_empty();
        let mut source_root: Option<std::path::PathBuf> = None;
        if has_source {
            let src_project = crate::acmd::build_mod_project_full(
                &acmd_edits,
                &effect_edits,
                &live_tweaks,
                &plugin_name,
            );
            match write_mod_project(&src_project, &dest.join("source")) {
                Ok(root) => {
                    report.push(format!(
                        "source: {} move script(s), {} effect script(s)",
                        acmd_edits.len(),
                        effect_edits.len()
                    ));
                    source_root = Some(root);
                }
                Err(e) => errors.push(format!("smashline source: {e}")),
            }
        } else {
            report.push("source: no hitbox/spawn edits — skipped".into());
        }

        // info.toml so mod/ drops straight into Arcropolis' mods folder.
        let _ = std::fs::create_dir_all(&mod_dir);
        let info = format!(
            "display_name = \"{name}\"\nauthors = \"SSBU Toolkit\"\nversion = \"1.0.0\"\ndescription = \"Exported by the SSBU hitbox/effects toolkit\"\ncategory = \"Misc\"\n",
            name = self.project_name
        );
        if let Err(e) = std::fs::write(mod_dir.join("info.toml"), info) {
            errors.push(format!("info.toml: {e}"));
        }

        // Top-level README: what goes where.
        let readme = format!(
            "# {name}\n\n\
             Exported by the SSBU hitbox/effects toolkit.\n\n\
             ## Install\n\n\
             1. `mod/` → copy to `sd:/ultimate/mods/{name}/` (Arcropolis data mod: eff files).\n\
             2. `plugin/lib{plugin}.nro` → copy to `atmosphere/contents/01006A800016E000/romfs/skyline/plugins/`\n   \
                (on emulator: the title's LayeredFS `romfs/skyline/plugins/`). Appears after the\n   \
                background build finishes — see build.log; or build manually with `source/build.sh`.\n\n\
             ## Re-editing\n\n\
             Open `modproject.json` via Mod → Load Project… in the toolkit.\n",
            name = self.project_name,
            plugin = plugin_name,
        );
        let _ = std::fs::write(dest.join("README.md"), readme);

        // 3. The project file itself — makes the exported mod re-openable for editing.
        match serde_json::to_string_pretty(&project) {
            Ok(json) => {
                if let Err(e) =
                    std::fs::write(dest.join(crate::mod_project::PROJECT_FILE_NAME), json)
                {
                    errors.push(format!("modproject.json: {e}"));
                }
            }
            Err(e) => errors.push(format!("modproject.json: {e}")),
        }

        // 4. Compile the source in the background; copy the nro into plugin/ on success.
        if let Some(src_root) = source_root {
            self.spawn_export_build(dest.clone(), src_root, plugin_name);
        }

        self.state.status = if errors.is_empty() {
            format!(
                "Mod exported to {} — {}{}",
                dest.display(),
                report.join(" · "),
                if has_source { " · building plugin…" } else { "" }
            )
        } else {
            format!(
                "Mod export finished with errors: {} — {}",
                errors.join("; "),
                report.join(" · ")
            )
        };
    }

    /// Run `cargo skyline build --release` on the exported source in a background thread.
    /// Progress lands in `self.export_build`; the finished nro is copied to `<dest>/plugin/`.
    fn spawn_export_build(
        &mut self,
        dest: std::path::PathBuf,
        src_root: std::path::PathBuf,
        plugin_name: String,
    ) {
        let state = std::sync::Arc::new(std::sync::Mutex::new(ExportBuildState {
            done: false,
            message: format!("building {plugin_name}…"),
        }));
        self.export_build = Some(state.clone());
        std::thread::spawn(move || {
            let out = std::process::Command::new("cargo")
                .args(["skyline", "build", "--release"])
                .current_dir(&src_root)
                .output();
            let msg = match out {
                Ok(out) => {
                    let log = format!(
                        "--- stdout ---\n{}\n--- stderr ---\n{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    let _ = std::fs::write(dest.join("build.log"), &log);
                    if out.status.success() {
                        let nro = src_root
                            .join("target/aarch64-skyline-switch/release")
                            .join(format!("lib{plugin_name}.nro"));
                        let plugin_dir = dest.join("plugin");
                        let _ = std::fs::create_dir_all(&plugin_dir);
                        match std::fs::copy(&nro, plugin_dir.join(format!("lib{plugin_name}.nro"))) {
                            Ok(_) => format!("plugin built → plugin/lib{plugin_name}.nro"),
                            Err(e) => format!(
                                "plugin built but nro copy failed ({e}) — see {}",
                                nro.display()
                            ),
                        }
                    } else {
                        "plugin build FAILED — see build.log in the export folder".to_string()
                    }
                }
                Err(e) => format!("plugin build could not start: {e}"),
            };
            if let Ok(mut s) = state.lock() {
                s.done = true;
                s.message = msg;
            }
        });
    }

    fn load_project_from(&mut self, path: &std::path::Path) {
        let project: crate::mod_project::ModProjectFile =
            match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|s| {
                serde_json::from_str(&s).map_err(anyhow::Error::from)
            }) {
                Ok(p) => p,
                Err(e) => {
                    self.state.status = format!("Project load failed: {e}");
                    return;
                }
            };
        self.project_name = project.name.clone();
        let mut n_acmd = 0;
        let mut n_calls = 0;
        let mut n_eff = 0;
        for (fighter, fm) in project.fighters {
            n_acmd += fm.acmd.len();
            if !fm.acmd.is_empty() {
                self.state
                    .edit_log
                    .entries
                    .entry(fighter.clone())
                    .or_default()
                    .extend(fm.acmd);
            }
            for (mv, edits) in fm.effect_calls {
                n_calls += edits.len();
                self.state
                    .effect_call_edits
                    .insert(format!("{fighter}/{mv}"), edits);
            }
            for (mv, full) in fm.effect_calls_full {
                self.state
                    .effect_call_full
                    .insert(format!("{fighter}/{mv}"), full);
            }
            if let Some(eff) = fm.eff {
                n_eff += eff.authored.len() + eff.one_slot.len();
                self.eff_mods.insert(fighter.clone(), eff);
            }
            // Live color/speed tweaks: restore into the override store and re-send.
            for t in fm.live_tweaks {
                let hash = effect_name_hash(&t.effect_name);
                let mut init = crate::game_link::RpmEffectData {
                    effect_name: t.effect_name.clone(),
                    ..Default::default()
                };
                if let Some([r, g, b, a]) = t.color {
                    init.rainbow.color =
                        crate::game_link::Color { red: r, green: g, blue: b, alpha: a };
                }
                if let Some(s) = t.speed {
                    init.speed = s;
                }
                self.live_overrides.restore_tweak(hash, init);
            }
        }

        // Re-apply to what's currently loaded and push it live.
        self.apply_effect_call_edits_to_current();
        self.push_effect_rules();
        if let Some(fighter) = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone())
        {
            if let Some(eff) = self.eff_mods.get(&fighter).cloned() {
                let src = self.eff_editor.export_root().join(&eff.source_rel);
                if self.eff_editor.loaded_rel().as_deref() == Some(eff.source_rel.as_str()) {
                    self.eff_editor.apply_authored_edits(&eff.authored);
                    self.eff_editor
                        .send_all_derived(&self.game_link, &mut self.live_overrides);
                } else if src.exists() {
                    self.eff_editor.queue_load(&src);
                    self.eff_editor.queue_edits(eff.authored.clone());
                    self.eff_editor.open = true;
                }
            }
        }
        self.state.status = format!(
            "Project '{}' loaded: {n_acmd} move edit(s), {n_calls} effect-call edit(s), {n_eff} eff edit(s)",
            self.project_name
        );
    }

    /// "Game has existing edits" prompt: the plugin persists pins across sessions, so a
    /// fresh toolkit instance may connect to a game already running modifications.
    fn draw_pin_sync_modal(&mut self, ctx: &egui::Context) {
        let Some(kinds) = self.pin_sync_prompt.clone() else { return };
        let mut choice: Option<&'static str> = None;
        egui::Window::new("Game has existing edits")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "The game booted with {} saved effect edit(s) still applied (the plugin \
                     keeps them on the SD card and re-applies them on launch). Keep them or \
                     remove them?",
                    kinds.len()
                ));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for (_, k) in &kinds {
                        let mut fields: Vec<&str> = Vec::new();
                        if let Some(p) = &k.pins {
                            if p.scale.is_some() { fields.push("size"); }
                            if p.rate.is_some() { fields.push("speed"); }
                            if p.pos.is_some() { fields.push("pos"); }
                            if p.rot.is_some() { fields.push("rot"); }
                            if p.visible.is_some() { fields.push("visible"); }
                            if p.frame.is_some() { fields.push("frame"); }
                            if p.color.is_some() { fields.push("color"); }
                        }
                        ui.label(
                            egui::RichText::new(format!("• {}  ({})", k.name, fields.join(", ")))
                                .monospace(),
                        );
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep & import").clicked() {
                        choice = Some("import");
                    }
                    if ui.button("Remove all").clicked() {
                        choice = Some("reset");
                    }
                    if ui.button("Keep (don't track)").clicked() {
                        choice = Some("ignore");
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Keep & import: leave them applied AND adopt them as tracked edits here. \
                         Remove all: clear them in-game. Keep (don't track): leave them running \
                         but don't pull them into this project.",
                    )
                    .small()
                    .color(egui::Color32::GRAY),
                );
            });
        match choice {
            Some("import") => {
                let n = kinds.len();
                for (hash, k) in kinds {
                    self.live_overrides.set_form(hash, k.data.clone());
                    let tweaked = k
                        .pins
                        .as_ref()
                        .map(|p| p.color.is_some() || p.rate.is_some())
                        .unwrap_or(false);
                    if tweaked {
                        self.live_overrides.mark_tweak(hash);
                    }
                }
                self.pin_sync_prompt = None;
                self.state.status = format!("Imported {n} in-game edit(s) into the toolkit");
            }
            Some("reset") => {
                self.game_link.send_reset_pins();
                self.push_effect_rules();
                self.pin_sync_prompt = None;
                self.state.status =
                    "Cleared the game's saved pins; this project's edits were re-sent".into();
            }
            Some("ignore") => {
                self.pin_sync_prompt = None;
            }
            _ => {}
        }
    }

    /// Clear EVERY edit the game is currently running: the plugin's persisted pins (SD-card
    /// effect edits that survive restarts), plus all live spawn + hitbox rules this session
    /// pushed. Use this when old edits keep re-appearing in-game.
    fn clear_all_game_edits(&mut self) {
        self.game_link.send_reset_pins();
        self.effect_rules_store.clear();
        self.hitbox_rules_store.clear();
        self.game_link.send_spawn_rules(&[]);
        self.game_link.send_hitbox_rules(&[]);
        self.pin_sync_prompt = None;
        self.state.status =
            "Cleared all in-game edits (saved pins + live spawn/hitbox rules).".into();
    }

    // ── Live ACMD capture + live hitbox rules ─────────────────────────────────

    /// hash40 of the current move's motion name (what MotionModule::motion_kind reports).
    fn current_motion_hash(&self) -> Option<u64> {
        self.state
            .selected_move
            .as_ref()
            .map(|m| hash40::hash40(&m.name.to_lowercase()).0)
    }

    /// Reverse-lookup maps: hash40(lowercase bone) → canonical bone name; effect hashes
    /// resolve through the loaded eff handles.
    fn bone_reverse_map(&self) -> HashMap<u64, String> {
        self.bone_names
            .iter()
            .map(|n| (hash40::hash40(&n.to_lowercase()).0, n.clone()))
            .collect()
    }

    fn effect_reverse_map(&self) -> HashMap<u64, String> {
        let mut m = HashMap::new();
        if let Some(idx) = &self.state.eff_index {
            for name in idx.handles.keys() {
                m.insert(hash40::hash40(&name.to_lowercase()).0, name.clone());
            }
        }
        for (h, k) in self.game_link.kinds() {
            m.entry(h).or_insert(k.name);
        }
        m
    }

    /// Build hitboxes + effect calls for the current move from the game's live ACMD capture,
    /// replacing the GitHub fetch as the data source ("Live capture" provenance).
    fn load_from_capture(&mut self) {
        let Some(motion) = self.current_motion_hash() else { return };
        let captures = self.game_link.captures_for(motion);
        if captures.is_empty() {
            self.state.status = "No live capture yet — perform the move in game first.".into();
            return;
        }
        let bone_rev = self.bone_reverse_map();
        let eff_rev = self.effect_reverse_map();

        let mut hitboxes: Vec<crate::data::Hitbox> = Vec::new();
        let mut effects: Vec<crate::data::EffectCall> = Vec::new();
        for line in &captures {
            if line.func.starts_with("ATTACK") {
                if let Some(hb) = Self::hitbox_from_capture(&line.args, line.frame, &bone_rev) {
                    // Same id re-captured (multi-part moves): keep the earliest frame.
                    if !hitboxes.iter().any(|h| h.id == hb.id && h.active_start == hb.active_start)
                    {
                        hitboxes.push(hb);
                    }
                }
            } else if line.func == "CATCH" {
                if let Some(hb) = Self::hitbox_from_capture_grab(&line.args, line.frame, &bone_rev) {
                    if !hitboxes.iter().any(|h| {
                        h.category == 1 && h.id == hb.id && h.active_start == hb.active_start
                    }) {
                        hitboxes.push(hb);
                    }
                }
            } else if line.func.starts_with("AREA_WIND") {
                if let Some(hb) = Self::hitbox_from_capture_wind(&line.args, line.frame) {
                    if !hitboxes
                        .iter()
                        .any(|h| h.category == 2 && h.active_start == hb.active_start)
                    {
                        hitboxes.push(hb);
                    }
                }
            } else if line.func.starts_with("EFFECT") {
                if let Some(ec) =
                    Self::effect_call_from_capture(&line.func, &line.args, line.frame, &bone_rev, &eff_rev)
                {
                    effects.push(ec);
                }
            }
        }
        if hitboxes.is_empty() && effects.is_empty() {
            self.state.status = "Capture has no ATTACK/EFFECT lines for this move yet.".into();
            return;
        }

        let n_hb = hitboxes.len();
        let n_fx = effects.len();
        if !hitboxes.is_empty() {
            self.state.hitboxes_pristine = hitboxes.clone();
            self.state.hitboxes = hitboxes;
        }
        if !effects.is_empty() {
            self.state.effects_pristine = effects.clone();
            self.state.effects = effects;
            self.state.selected_effect_call = None;
            self.apply_effect_call_edits_to_current();
            self.respawn_effects();
        }
        self.state.acmd_source = "Live capture".into();
        self.acmd_error = None;
        self.state.status =
            format!("Loaded {n_hb} hitbox(es) + {n_fx} effect call(s) from live game capture");
    }

    /// ATTACK capture args (positional, editor conventions) → display Hitbox.
    /// Const-name slots keep their raw numeric values as strings (exported bare).
    fn hitbox_from_capture(
        args: &[crate::game_link::LuaArgWire],
        frame: f32,
        bone_rev: &HashMap<u64, String>,
    ) -> Option<crate::data::Hitbox> {
        use crate::game_link::LuaArgWire as A;
        if args.len() < 17 {
            return None;
        }
        let f32_at = |i: usize| args.get(i).and_then(|a| a.as_f32());
        let i64_at = |i: usize| args.get(i).and_then(|a| a.as_i64());
        let str_at = |i: usize| i64_at(i).map(|v| v.to_string()).unwrap_or_default();
        let bool_at = |i: usize| matches!(args.get(i), Some(A::Bool(true)));
        let bone_hash = args.get(2).and_then(|a| a.as_hash())?;
        let bone_name = bone_rev
            .get(&bone_hash)
            .cloned()
            .unwrap_or_else(|| format!("{bone_hash:#x}"));
        let capsule_end = match (f32_at(12), f32_at(13), f32_at(14)) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => None,
        };
        let start = frame.max(0.0).round() as u32;
        Some(crate::data::Hitbox {
            id: i64_at(0)? as u32,
            part: i64_at(1).unwrap_or(0) as u32,
            bone_name,
            damage: f32_at(3).unwrap_or(0.0),
            angle: i64_at(4).unwrap_or(0) as i32,
            kb_scaling: i64_at(5).unwrap_or(0) as i32,
            fkb: i64_at(6).unwrap_or(0) as i32,
            kb_base: i64_at(7).unwrap_or(0) as i32,
            size: f32_at(8).unwrap_or(1.0),
            offset_x: f32_at(9).unwrap_or(0.0),
            offset_y: f32_at(10).unwrap_or(0.0),
            offset_z: f32_at(11).unwrap_or(0.0),
            capsule_end,
            hitlag_mult: f32_at(15).unwrap_or(1.0),
            sdi_mult: f32_at(16).unwrap_or(1.0),
            setoff_kind: str_at(17),
            lr_check: str_at(18),
            is_clang: bool_at(19),
            is_add_attack: i64_at(20).unwrap_or(0) as i32,
            hitbox_attr: f32_at(21).unwrap_or(0.0),
            ground_or_air: i64_at(22).unwrap_or(0) as i32,
            is_mtk: bool_at(23),
            is_shield_disable: bool_at(24),
            is_reflectable: bool_at(25),
            is_absorbable: bool_at(26),
            is_landing_attack: bool_at(27),
            situation_mask: str_at(28),
            category_mask: str_at(29),
            part_mask: str_at(30),
            no_finish_camera: bool_at(31),
            collision_attr: args
                .get(32)
                .and_then(|a| a.as_hash())
                .map(|h| format!("{h:#x}"))
                .unwrap_or_default(),
            sound_level: str_at(33),
            sound_attr: str_at(34),
            attack_region: str_at(35),
            active_start: start,
            active_end: start + 2,
            hitbox_type: 0,
            category: 0,
        })
    }

    /// CATCH (grabbox) capture → display Hitbox with category=1 (grab).
    /// Arg layout: 0 id, 1 bone(h), 2 size, 3 x, 4 y, 5 z, 6 x2, 7 y2, 8 z2, 9 status, 10 situation.
    fn hitbox_from_capture_grab(
        args: &[crate::game_link::LuaArgWire],
        frame: f32,
        bone_rev: &HashMap<u64, String>,
    ) -> Option<crate::data::Hitbox> {
        if args.len() < 6 {
            return None;
        }
        let f32_at = |i: usize| args.get(i).and_then(|a| a.as_f32());
        let i64_at = |i: usize| args.get(i).and_then(|a| a.as_i64());
        let bone_hash = args.get(1).and_then(|a| a.as_hash())?;
        let bone_name = bone_rev
            .get(&bone_hash)
            .cloned()
            .unwrap_or_else(|| format!("{bone_hash:#x}"));
        let capsule_end = match (f32_at(6), f32_at(7), f32_at(8)) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => None,
        };
        let start = frame.max(0.0).round() as u32;
        Some(crate::data::Hitbox {
            id: i64_at(0).unwrap_or(0) as u32,
            bone_name,
            size: f32_at(2).unwrap_or(2.0),
            offset_x: f32_at(3).unwrap_or(0.0),
            offset_y: f32_at(4).unwrap_or(0.0),
            offset_z: f32_at(5).unwrap_or(0.0),
            capsule_end,
            // Grabboxes deal no damage/knockback — zero the attack-only fields.
            damage: 0.0,
            angle: 0,
            kb_scaling: 0,
            fkb: 0,
            kb_base: 0,
            active_start: start,
            active_end: start + 2,
            category: 1,
            ..Default::default()
        })
    }

    /// AREA_WIND capture → display Hitbox with category=2 (wind). The arg semantics are
    /// undocumented (all floats); we render a best-effort sphere and keep the raw args so
    /// the size/offset mapping can be refined once confirmed from real captures. Heuristic:
    /// the largest-magnitude arg is treated as the reach/size, offsets left at origin.
    fn hitbox_from_capture_wind(
        args: &[crate::game_link::LuaArgWire],
        frame: f32,
    ) -> Option<crate::data::Hitbox> {
        if args.is_empty() {
            return None;
        }
        let vals: Vec<f32> = args.iter().filter_map(|a| a.as_f32()).collect();
        // Guess a visible size from the largest-magnitude float (clamped to a sane range).
        let size = vals
            .iter()
            .cloned()
            .map(f32::abs)
            .fold(0.0_f32, f32::max)
            .clamp(1.0, 25.0);
        let start = frame.max(0.0).round() as u32;
        Some(crate::data::Hitbox {
            id: 0,
            bone_name: "top".to_string(),
            size,
            offset_x: 0.0,
            offset_y: 0.0,
            offset_z: 0.0,
            capsule_end: None,
            damage: 0.0,
            angle: 0,
            kb_scaling: 0,
            fkb: 0,
            kb_base: 0,
            active_start: start,
            active_end: start + 2,
            category: 2,
            ..Default::default()
        })
    }

    /// EFFECT-family capture → EffectCall (arc layout: gfx, joint, pos xyz, rot zr/yr/xr, size).
    fn effect_call_from_capture(
        func: &str,
        args: &[crate::game_link::LuaArgWire],
        frame: f32,
        bone_rev: &HashMap<u64, String>,
        eff_rev: &HashMap<u64, String>,
    ) -> Option<crate::data::EffectCall> {
        let flip = func.contains("FLIP");
        let off = usize::from(flip);
        let eff_hash = args.first().and_then(|a| a.as_hash())?;
        let f32_at = |i: usize| args.get(i).and_then(|a| a.as_f32()).unwrap_or(0.0);
        let bone_hash = args.get(1 + off).and_then(|a| a.as_hash()).unwrap_or(0);
        let follows = func.contains("FOLLOW") || func.contains("FLW");
        let start = frame.max(0.0).round() as u32;
        Some(crate::data::EffectCall {
            effect_name: eff_rev
                .get(&eff_hash)
                .cloned()
                .unwrap_or_else(|| format!("{eff_hash:#x}")),
            bone_name: bone_rev
                .get(&bone_hash)
                .cloned()
                .unwrap_or_else(|| format!("{bone_hash:#x}")),
            offset: [f32_at(2 + off), f32_at(3 + off), f32_at(4 + off)],
            rotation: [f32_at(7 + off), f32_at(6 + off), f32_at(5 + off)],
            scale: args.get(8 + off).and_then(|a| a.as_f32()).unwrap_or(1.0),
            follows_bone: follows,
            active_start: start,
            active_end: if follows { 9999 } else { start },
            disabled: false,
        })
    }

    /// Derive + push the full live hitbox-rule set, matched PER-OCCURRENCE so multi-hit
    /// moves (which reuse the same hitbox id across frames) stay independent:
    ///   * exact (id, start-frame) pair changed → frame-scoped override,
    ///   * pristine occurrence with no current match → frame-scoped suppress (deleted/retimed),
    ///   * current occurrence with no pristine match → inject (added/retimed),
    /// each suppress/override windowed to its own frame so the other hits are untouched.
    fn push_hitbox_rules(&mut self) {
        let Some(mv_key) = self.current_move_key() else { return };
        let Some(motion) = self.current_motion_hash() else { return };
        let captures = self.game_link.captures_for(motion);
        // Donor capture (for injecting added/retimed collisions) matched by family + id.
        let fam_prefix = |cat: u8| match cat {
            1 => "CATCH",
            2 => "AREA_WIND",
            _ => "ATTACK",
        };
        let donor_for = |cat: u8, id: u32| {
            let pre = fam_prefix(cat);
            captures
                .iter()
                .find(|c| {
                    c.func.starts_with(pre)
                        && c.args.first().and_then(|a| a.as_i64()) == Some(id as i64)
                })
                .or_else(|| captures.iter().find(|c| c.func.starts_with(pre)))
        };
        // Frame window around a hit's spawn frame.
        let win = |frame: u32| (Some(frame as f32 - 0.5), Some(frame as f32 + 1.5));

        let pristine = self.state.hitboxes_pristine.clone();
        let current = self.state.hitboxes.clone();
        let mut cur_used = vec![false; current.len()];
        let mut pri_used = vec![false; pristine.len()];
        let mut rules: Vec<crate::game_link::HitboxRuleWire> = Vec::new();
        let mut missing_donor = false;

        // Phase 1 — exact (id, start) pairs: unchanged → nothing, changed → scoped override.
        for (pi, p) in pristine.iter().enumerate() {
            if let Some(ci) = current.iter().enumerate().position(|(ci, h)| {
                !cur_used[ci] && h.id == p.id && h.active_start == p.active_start
            }) {
                cur_used[ci] = true;
                pri_used[pi] = true;
                let h = &current[ci];
                if h != p {
                    let (fs, fe) = win(p.active_start);
                    rules.push(crate::game_link::HitboxRuleWire {
                        motion,
                        category: p.category,
                        // Wind has no id — match by frame only.
                        hitbox_id: (p.category != 2).then_some(p.id as u64),
                        suppress: false,
                        frame_start: fs,
                        frame_end: fe,
                        overrides: Some(Self::hitbox_overrides(h)),
                        inject: None,
                    });
                }
            }
        }

        // Phase 2 — unmatched pristine → suppress that specific hit (deleted or retimed away).
        for (pi, p) in pristine.iter().enumerate() {
            if pri_used[pi] {
                continue;
            }
            let (fs, fe) = win(p.active_start);
            rules.push(crate::game_link::HitboxRuleWire {
                motion,
                category: p.category,
                hitbox_id: (p.category != 2).then_some(p.id as u64),
                suppress: true,
                frame_start: fs,
                frame_end: fe,
                overrides: None,
                inject: None,
            });
        }

        // Phase 3 — unmatched current → inject at its frame (added or retimed here).
        for (ci, h) in current.iter().enumerate() {
            if cur_used[ci] {
                continue;
            }
            // Build the inject arg vector for this collision family.
            let args = match h.category {
                1 => Self::build_catch_args(h, donor_for(1, h.id)),
                2 => None, // wind injection unsupported (undocumented arg semantics)
                _ => Self::build_attack_args(h, donor_for(0, h.id)),
            };
            match args {
                Some(args) => rules.push(crate::game_link::HitboxRuleWire {
                    motion,
                    category: h.category,
                    hitbox_id: None,
                    suppress: false,
                    frame_start: None,
                    frame_end: None,
                    overrides: None,
                    inject: Some(crate::game_link::InjectRuleWire {
                        frame: h.active_start as f32,
                        args,
                    }),
                }),
                None => missing_donor = true,
            }
        }

        if rules.is_empty() {
            self.hitbox_rules_store.remove(&mv_key);
        } else {
            self.hitbox_rules_store.insert(mv_key, rules);
        }
        let all: Vec<crate::game_link::HitboxRuleWire> =
            self.hitbox_rules_store.values().flatten().cloned().collect();
        self.game_link.send_hitbox_rules(&all);
        if missing_donor {
            self.state.status =
                "Added/retimed hitbox needs live capture args — perform the move in game once."
                    .into();
        }
    }

    /// Every modeled ATTACK slot as an override (unchanged values are harmless rewrites).
    fn hitbox_overrides(h: &crate::data::Hitbox) -> crate::game_link::HbOverridesWire {
        crate::game_link::HbOverridesWire {
            damage: Some(h.damage),
            angle: Some(h.angle as i64),
            kbg: Some(h.kb_scaling as i64),
            fkb: Some(h.fkb as i64),
            bkb: Some(h.kb_base as i64),
            size: Some(h.size),
            x: Some(h.offset_x),
            y: Some(h.offset_y),
            z: Some(h.offset_z),
            x2: h.capsule_end.map(|c| c[0]),
            y2: h.capsule_end.map(|c| c[1]),
            z2: h.capsule_end.map(|c| c[2]),
            hitlag: Some(h.hitlag_mult),
            sdi: Some(h.sdi_mult),
        }
    }

    /// Full 36-slot ATTACK arg vector for injection: donor capture args (exact tail) with
    /// the modeled slots overwritten from the Hitbox. Without a donor: None (template
    /// injection is too risky — masks/flags would be guesses).
    fn build_attack_args(
        h: &crate::data::Hitbox,
        donor: Option<&crate::game_link::CaptureLine>,
    ) -> Option<Vec<crate::game_link::LuaArgWire>> {
        use crate::game_link::LuaArgWire as A;
        let mut args = donor.filter(|d| d.args.len() >= 33)?.args.clone();
        args[0] = A::Int(h.id as i64);
        args[1] = A::Int(h.part as i64);
        args[2] = A::Hash(hash40::hash40(&h.bone_name.to_lowercase()).0);
        args[3] = A::Num(h.damage);
        args[4] = A::Int(h.angle as i64);
        args[5] = A::Int(h.kb_scaling as i64);
        args[6] = A::Int(h.fkb as i64);
        args[7] = A::Int(h.kb_base as i64);
        args[8] = A::Num(h.size);
        args[9] = A::Num(h.offset_x);
        args[10] = A::Num(h.offset_y);
        args[11] = A::Num(h.offset_z);
        match h.capsule_end {
            Some([x, y, z]) => {
                args[12] = A::Num(x);
                args[13] = A::Num(y);
                args[14] = A::Num(z);
            }
            None => {
                args[12] = A::Nil;
                args[13] = A::Nil;
                args[14] = A::Nil;
            }
        }
        args[15] = A::Num(h.hitlag_mult);
        args[16] = A::Num(h.sdi_mult);
        Some(args)
    }

    /// Build a CATCH (grabbox) inject arg vector from a captured donor grab.
    /// Layout: 0 id, 1 bone(h), 2 size, 3 x, 4 y, 5 z, 6 x2, 7 y2, 8 z2, 9 status, 10 situation.
    fn build_catch_args(
        h: &crate::data::Hitbox,
        donor: Option<&crate::game_link::CaptureLine>,
    ) -> Option<Vec<crate::game_link::LuaArgWire>> {
        use crate::game_link::LuaArgWire as A;
        let mut args = donor.filter(|d| d.args.len() >= 6)?.args.clone();
        args[0] = A::Int(h.id as i64);
        args[1] = A::Hash(hash40::hash40(&h.bone_name.to_lowercase()).0);
        args[2] = A::Num(h.size);
        args[3] = A::Num(h.offset_x);
        args[4] = A::Num(h.offset_y);
        args[5] = A::Num(h.offset_z);
        if args.len() >= 9 {
            match h.capsule_end {
                Some([x, y, z]) => {
                    args[6] = A::Num(x);
                    args[7] = A::Num(y);
                    args[8] = A::Num(z);
                }
                None => {
                    args[6] = A::Nil;
                    args[7] = A::Nil;
                    args[8] = A::Nil;
                }
            }
        }
        Some(args)
    }

    // ── One-Slot studio ───────────────────────────────────────────────────────

    /// The One-Slot studio: pick ANY effect from any eff (pool), copy it into the current
    /// fighter's eff under a new name, then choose per-use which spawns redirect to it.
    fn draw_one_slot_studio(&mut self, ctx: &egui::Context) {
        if !self.show_one_slot {
            return;
        }
        if self.effect_pool.is_none() {
            self.effect_pool =
                Some(crate::effect_pool::EffectPool::new(
                    self.eff_editor.export_root().to_path_buf(),
                ));
        }
        let scanning = self
            .effect_pool
            .as_mut()
            .map(|p| p.tick(6))
            .unwrap_or(false);
        if scanning {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }

        let target = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone());
        let mut open = self.show_one_slot;
        let mut do_slot: Option<(String, String, String)> = None; // (rel, donor, new name)
        egui::Window::new("One-Slot Studio")
            .open(&mut open)
            .default_width(430.0)
            .show(ctx, |ui| {
                let Some(target) = &target else {
                    ui.colored_label(egui::Color32::GRAY, "Select a fighter first.");
                    return;
                };
                let pool = self.effect_pool.as_ref().unwrap();
                let (done, total) = pool.progress();
                ui.horizontal(|ui| {
                    ui.label(format!("Target: {target}"));
                    if scanning {
                        ui.label(
                            egui::RichText::new(format!("scanning effs… {done}/{total}"))
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.one_slot_search)
                        .hint_text("Search every effect entry (all fighters + sys/common)…")
                        .desired_width(f32::INFINITY),
                );

                // Live kinds that match — effects the running game has actually used.
                let q = self.one_slot_search.to_lowercase();
                let live_matches: Vec<String> = self
                    .game_link
                    .kinds()
                    .into_iter()
                    .map(|(_, k)| k.name)
                    .filter(|n| !n.starts_with("0x") && (q.is_empty() || n.to_lowercase().contains(&q)))
                    .take(6)
                    .collect();

                let results = pool.search(&self.one_slot_search, 200);
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    if !live_matches.is_empty() {
                        ui.label(egui::RichText::new("Live in game").strong().small());
                        for name in &live_matches {
                            // Resolve the live kind to a pool file if possible.
                            let file = results
                                .iter()
                                .find(|(_, n)| n.eq_ignore_ascii_case(name))
                                .map(|(rel, _)| rel.clone());
                            let sel = self
                                .one_slot_sel
                                .as_ref()
                                .map(|(_, n)| n == name)
                                .unwrap_or(false);
                            let label = match &file {
                                Some(rel) => format!("● {name}  ({rel})"),
                                None => format!("● {name}  (source unknown yet)"),
                            };
                            if ui.selectable_label(sel, label).clicked() {
                                if let Some(rel) = file {
                                    self.one_slot_sel = Some((rel, name.clone()));
                                    self.one_slot_new_name = format!("{name}_os");
                                }
                            }
                        }
                        ui.separator();
                    }
                    ui.label(egui::RichText::new("All effects").strong().small());
                    for (rel, name) in &results {
                        let sel = self
                            .one_slot_sel
                            .as_ref()
                            .map(|(r, n)| r == rel && n == name)
                            .unwrap_or(false);
                        if ui
                            .selectable_label(sel, format!("{name}  —  {rel}"))
                            .clicked()
                        {
                            self.one_slot_sel = Some((rel.clone(), name.clone()));
                            self.one_slot_new_name = format!("{name}_os");
                        }
                    }
                    if results.is_empty() {
                        ui.colored_label(egui::Color32::GRAY, "No matches (yet).");
                    }
                });

                ui.separator();
                if let Some((rel, donor)) = self.one_slot_sel.clone() {
                    ui.label(format!("Donor: {donor}  ({rel})"));
                    ui.horizontal(|ui| {
                        ui.label("New entry name:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.one_slot_new_name)
                                .desired_width(220.0),
                        );
                    });
                    let cross = !rel.contains(&format!("/{target}/"));
                    if cross {
                        ui.label(
                            egui::RichText::new(
                                "Cross-file: textures + shaders transfer automatically. \
                                 Donors using primitive models are refused (shown in status).",
                            )
                            .small()
                            .color(egui::Color32::from_rgb(200, 200, 120)),
                        );
                    }
                    ui.label(
                        egui::RichText::new(
                            "One-slotting previews the merged eff in-app immediately, then lets \
                             you pick which uses redirect to the copy.",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    let valid = !self.one_slot_new_name.trim().is_empty();
                    if ui
                        .add_enabled(valid, egui::Button::new(format!("One-slot into {target} + preview")))
                        .clicked()
                    {
                        do_slot = Some((rel, donor, self.one_slot_new_name.trim().to_string()));
                    }
                } else {
                    ui.colored_label(egui::Color32::GRAY, "Pick a donor effect above.");
                }
            });
        self.show_one_slot = open;

        if let (Some((rel, donor, new_name)), Some(fighter)) = (do_slot, target) {
            self.record_one_slot(&fighter, &rel, &donor, &new_name);
        }
    }

    /// Build the fighter's one-slotted eff IN MEMORY (source eff + recorded one-slot ops via
    /// the same `rebuild_eff_bytes` the exporter uses) and load it into the preview, so the
    /// copied effect is visible before export. Surfaces transfer errors (e.g. primitives) in
    /// the status line instead of failing silently.
    fn preview_one_slot_result(&mut self, fighter: &str) {
        let Some(eff) = self.eff_mods.get(fighter).cloned() else { return };
        let root = self.eff_editor.export_root().to_path_buf();
        // Prefer the actually-loaded target eff bytes; fall back to the export-root source.
        let src_bytes = self
            .current_eff_path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .or_else(|| std::fs::read(root.join(&eff.source_rel)).ok());
        let Some(src_bytes) = src_bytes else {
            self.state.status =
                "One-slot recorded, but the target eff isn't on disk to preview — export to apply.".into();
            return;
        };
        match crate::eff_export::rebuild_eff_bytes(&src_bytes, &eff, Some(&root)) {
            Ok(merged) => {
                // Write the preview NEXT TO the fighter's own eff so load_eff_file's sibling
                // merges (ef_common.eff sys effects, trail/ + model/ nutexb) still resolve —
                // writing to a scratch dir left the copied effect missing those shared
                // textures/shaders, which showed up as an invisible effect. The
                // `_oneslot_preview.eff` name is skipped by the donor-pool scan.
                let tmp = self
                    .current_eff_path
                    .as_ref()
                    .and_then(|p| p.parent())
                    .map(|dir| dir.join("_oneslot_preview.eff"))
                    .unwrap_or_else(|| {
                        crate::scratch_dirs::app_storage_root().join("_oneslot_preview.eff")
                    });
                if std::fs::write(&tmp, &merged).is_ok() {
                    self.load_eff_file(&tmp);
                    self.respawn_effects();
                    // Point the Eff Editor at the merged eff too, so the new slotted entry
                    // shows up in its entry list (was only reading the base file).
                    self.eff_editor.queue_load(&tmp);
                    self.state.status =
                        format!("One-slot applied — previewing merged eff for {fighter}");
                }
            }
            Err(e) => {
                self.state.status = format!("One-slot preview failed: {e}");
            }
        }
    }

    /// Record the OneSlotOp for `fighter` and open the per-use redirect prompt.
    fn record_one_slot(&mut self, fighter: &str, rel: &str, donor: &str, new_name: &str) {
        let own_rel = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
        let entry = self.eff_mods.entry(fighter.to_string()).or_default();
        if entry.source_rel.is_empty() {
            entry.source_rel = own_rel.clone();
        }
        entry.one_slot.push(crate::mod_project::OneSlotOp {
            new_entry_name: new_name.to_string(),
            src_file_rel: if rel == own_rel { String::new() } else { rel.to_string() },
            src_set_name: donor.to_string(),
            src_set_idx: 0,
        });
        self.state.status = format!("One-slot '{new_name}' recorded for {fighter}");

        // Full use discovery: reconstruct EVERY move performed live that spawns the donor
        // (not just moves already opened) into effect_call_full, so all real uses are listed
        // and redirectable — each move played in-game contributes its captured effect script.
        {
            let donor_hash = effect_name_hash(donor);
            let motion_name: HashMap<u64, String> =
                self.move_list.iter().map(|m| (m.hash, m.name.clone())).collect();
            let bone_rev = self.bone_reverse_map();
            let eff_rev = self.effect_reverse_map();
            let mut motions: Vec<u64> = self
                .game_link
                .all_captures()
                .into_iter()
                .filter(|(_, l)| {
                    l.func.starts_with("EFFECT")
                        && l.args.first().and_then(|a| a.as_hash()) == Some(donor_hash)
                })
                .map(|(m, _)| m)
                .collect();
            motions.sort();
            motions.dedup();
            for m in motions {
                let Some(name) = motion_name.get(&m) else { continue };
                let key = format!("{fighter}/{name}");
                if self.state.effect_call_full.contains_key(&key) {
                    continue;
                }
                let mut calls: Vec<crate::data::EffectCall> = Vec::new();
                for line in self.game_link.captures_for(m) {
                    if line.func.starts_with("EFFECT") {
                        if let Some(ec) = Self::effect_call_from_capture(
                            &line.func, &line.args, line.frame, &bone_rev, &eff_rev,
                        ) {
                            calls.push(ec);
                        }
                    }
                }
                if !calls.is_empty() {
                    self.state.effect_call_full.insert(key, calls);
                }
            }
        }

        // Preview the merged result in-app immediately (build the one-slotted eff in memory).
        self.preview_one_slot_result(fighter);

        // Gather every known use of the donor effect for the redirect picker.
        let donor_hash = effect_name_hash(donor);
        let mut uses: Vec<RedirectUse> = Vec::new();
        for (key, calls) in &self.state.effect_call_full {
            if !key.starts_with(&format!("{fighter}/")) {
                continue;
            }
            for (i, c) in calls.iter().enumerate() {
                if effect_name_hash(&c.effect_name) == donor_hash {
                    uses.push(RedirectUse {
                        move_key: key.clone(),
                        call_idx: i,
                        label: format!(
                            "{key} — frame {} on {}",
                            c.active_start, c.bone_name
                        ),
                        selected: true,
                    });
                }
            }
        }
        // Current move's calls (may not be snapshotted yet).
        if let Some(mv_key) = self.current_move_key() {
            if !self.state.effect_call_full.contains_key(&mv_key) {
                for (i, c) in self.state.effects.iter().enumerate() {
                    if effect_name_hash(&c.effect_name) == donor_hash {
                        uses.push(RedirectUse {
                            move_key: mv_key.clone(),
                            call_idx: i,
                            label: format!(
                                "{mv_key} — frame {} on {}",
                                c.active_start, c.bone_name
                            ),
                            selected: true,
                        });
                    }
                }
            }
        }
        self.redirect_prompt = Some(RedirectPrompt {
            donor_name: donor.to_string(),
            new_name: new_name.to_string(),
            uses,
        });
    }

    /// "Which uses go to the new effect?" — per-use checkboxes, applied as call edits.
    fn draw_redirect_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.redirect_prompt else { return };
        let mut action: Option<bool> = None; // Some(true)=apply, Some(false)=skip
        egui::Window::new("Redirect spawns to the one-slotted effect?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "'{}' was copied as '{}'. Choose which existing spawns should use the \
                     new effect (unchecked ones keep the original):",
                    prompt.donor_name, prompt.new_name
                ));
                ui.add_space(4.0);
                if prompt.uses.is_empty() {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        "No known uses yet — retarget calls later in the Effects panel \
                         (uses appear as moves are opened or captured live).",
                    );
                } else {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for u in prompt.uses.iter_mut() {
                            ui.checkbox(&mut u.selected, &u.label);
                        }
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let any = prompt.uses.iter().any(|u| u.selected);
                    if ui
                        .add_enabled(any, egui::Button::new("Redirect selected"))
                        .clicked()
                    {
                        action = Some(true);
                    }
                    if ui.button("Keep all on the original").clicked() {
                        action = Some(false);
                    }
                });
            });
        match action {
            Some(true) => {
                let prompt = self.redirect_prompt.take().unwrap();
                self.apply_redirects(&prompt);
            }
            Some(false) => self.redirect_prompt = None,
            None => {}
        }
    }

    fn apply_redirects(&mut self, prompt: &RedirectPrompt) {
        let mut n = 0usize;
        let current = self.current_move_key();
        for u in prompt.uses.iter().filter(|u| u.selected) {
            if Some(&u.move_key) == current.as_ref() {
                if let Some(c) = self.state.effects.get_mut(u.call_idx) {
                    c.effect_name = prompt.new_name.clone();
                }
                self.record_effect_call_edit(u.call_idx);
                self.push_effect_rules();
                n += 1;
                continue;
            }
            // Other moves: rewrite the stored full snapshot + upsert the Modify edit, AND
            // push a live swap rule now (suppress the donor + inject the new effect) so the
            // redirect takes effect in-game immediately instead of only when the move is opened.
            if let Some(calls) = self.state.effect_call_full.get_mut(&u.move_key) {
                if let Some(c) = calls.get_mut(u.call_idx) {
                    c.effect_name = prompt.new_name.clone();
                    let call = c.clone();
                    let edits = self
                        .state
                        .effect_call_edits
                        .entry(u.move_key.clone())
                        .or_default();
                    if let Some(e) = edits.iter_mut().find(|e| e.index == u.call_idx) {
                        e.op = crate::data::EffectCallOp::Modify(call.clone());
                    } else {
                        edits.push(crate::data::EffectCallEdit {
                            index: u.call_idx,
                            op: crate::data::EffectCallOp::Modify(call.clone()),
                        });
                    }
                    // Live swap rule for this other move (needs that move captured in-game).
                    let donor_hash = effect_name_hash(&prompt.donor_name);
                    if let Some(mname) = u.move_key.split_once('/').map(|(_, m)| m) {
                        let motion = hash40::hash40(&mname.to_lowercase()).0;
                        if let Some(inject) =
                            self.build_effect_inject(&call, Some(motion), donor_hash)
                        {
                            let fs = call.active_start as f32 - 0.5;
                            let fe = call.active_start as f32 + 1.5;
                            let store = self.effect_rules_store.entry(u.move_key.clone()).or_default();
                            store.push(crate::game_link::SpawnRuleWire {
                                eff_hash: donor_hash,
                                suppress: true,
                                motion: Some(motion),
                                frame_start: Some(fs),
                                frame_end: Some(fe),
                                pos: None,
                                rot: None,
                                scale: None,
                                inject: None,
                            });
                            store.push(crate::game_link::SpawnRuleWire {
                                eff_hash: effect_name_hash(&call.effect_name),
                                suppress: false,
                                motion: Some(motion),
                                frame_start: None,
                                frame_end: None,
                                pos: None,
                                rot: None,
                                scale: None,
                                inject: Some(inject),
                            });
                        }
                    }
                    n += 1;
                }
            }
        }
        // Flush the union of all moves' rules so cross-move redirects apply live at once.
        let all: Vec<crate::game_link::SpawnRuleWire> =
            self.effect_rules_store.values().flatten().cloned().collect();
        self.game_link.send_spawn_rules(&all);
        self.state.status = format!(
            "Redirected {n} spawn(s) from '{}' to '{}' (live where the move was captured)",
            prompt.donor_name, prompt.new_name
        );
    }

    /// Record (or update) the Modify edit for effect call `i` in the current move.
    /// Added calls keep their `Add` record updated instead.
    fn record_effect_call_edit(&mut self, i: usize) {
        let Some(mv) = self.current_move_key() else {
            return;
        };
        let Some(call) = self.state.effects.get(i).cloned() else { return };
        let is_added = i >= self.state.effects_pristine.len();
        let edits = self.state.effect_call_edits.entry(mv.clone()).or_default();
        if let Some(existing) = edits.iter_mut().find(|e| e.index == i) {
            existing.op = if is_added {
                crate::data::EffectCallOp::Add(call)
            } else {
                crate::data::EffectCallOp::Modify(call)
            };
        } else {
            edits.push(crate::data::EffectCallEdit {
                index: i,
                op: crate::data::EffectCallOp::Modify(call),
            });
        }
        self.state
            .effect_call_full
            .insert(mv, self.state.effects.clone());
    }

    /// Delete effect spawn `i` from the current move. An ADDED spawn is removed outright
    /// (its edit dropped, later added-edit indices shifted down). A script (pristine) spawn
    /// can't be removed from the ACMD, so it's recorded as `Remove` (suppressed in-game and on
    /// export) — distinct from the `disabled` toggle only in intent. Bound to Backspace/Delete.
    fn delete_effect_call(&mut self, i: usize) {
        if i >= self.state.effects.len() {
            return;
        }
        let Some(mv) = self.current_move_key() else { return };
        let is_added = i >= self.state.effects_pristine.len();
        if is_added {
            self.state.effects.remove(i);
            let edits = self.state.effect_call_edits.entry(mv.clone()).or_default();
            edits.retain(|e| e.index != i);
            for e in edits.iter_mut() {
                if e.index > i {
                    e.index -= 1;
                }
            }
            self.state.selected_effect_call = None;
        } else {
            let edits = self.state.effect_call_edits.entry(mv.clone()).or_default();
            if let Some(e) = edits.iter_mut().find(|e| e.index == i) {
                e.op = crate::data::EffectCallOp::Remove;
            } else {
                edits.push(crate::data::EffectCallEdit {
                    index: i,
                    op: crate::data::EffectCallOp::Remove,
                });
            }
            // Rebuild so the Remove op takes effect (script spawn becomes disabled).
            self.apply_effect_call_edits_to_current();
            self.state.selected_effect_call = None;
        }
        self.state
            .effect_call_full
            .insert(mv, self.state.effects.clone());
        self.push_effect_rules();
        self.respawn_effects();
    }

    /// Rebuild and push the current move's effect spawn rules: PER-SPAWN and frame/motion
    /// scoped, so editing one spawn's offset (or disabling it) never touches the other
    /// spawns of the same effect. Untouched spawns fire at their pristine frame with the
    /// script's values; a moved spawn gets a transform override; a RETIMED spawn is
    /// suppressed at its pristine frame and re-injected (from a live capture) at the new
    /// frame with its edited transform baked in. Only changed calls produce a rule.
    fn push_effect_rules(&mut self) {
        let Some(mv_key) = self.current_move_key() else { return };
        let motion = self.current_motion_hash();
        let effects = self.state.effects.clone();
        let pristines = self.state.effects_pristine.clone();
        let mut rules: Vec<crate::game_link::SpawnRuleWire> = Vec::new();
        let mut missing_capture = false;
        for (i, ec) in effects.iter().enumerate() {
            let pristine = pristines.get(i);
            let spawn_frame =
                pristine.map(|p| p.active_start).unwrap_or(ec.active_start) as f32;
            let hash = effect_name_hash(&ec.effect_name);
            // The effect the SCRIPT actually spawns (before edits) — what to suppress.
            let orig_hash = pristine
                .map(|p| effect_name_hash(&p.effect_name))
                .unwrap_or(hash);
            let window = (Some(spawn_frame - 0.5), Some(spawn_frame + 1.5));
            if ec.disabled {
                rules.push(crate::game_link::SpawnRuleWire {
                    eff_hash: orig_hash,
                    suppress: true,
                    motion,
                    frame_start: window.0,
                    frame_end: window.1,
                    pos: None,
                    rot: None,
                    scale: None,
                    inject: None,
                });
                continue;
            }
            // Swap and/or retime: the effect NAME or FRAME changed → suppress the original
            // spawn and inject the new effect at the new frame (transform baked in). The
            // injected call reuses the original spawn's captured args with the graphic hash
            // swapped to the new effect. Needs a live capture of the original; without one,
            // fall back to a transform rule (export still applies the swap) and flag it.
            let retimed = pristine
                .map(|p| p.active_start != ec.active_start)
                .unwrap_or(false);
            let swapped = orig_hash != hash;
            if retimed || swapped {
                if let Some(inject) = self.build_effect_inject(ec, motion, orig_hash) {
                    rules.push(crate::game_link::SpawnRuleWire {
                        eff_hash: orig_hash,
                        suppress: true,
                        motion,
                        frame_start: window.0,
                        frame_end: window.1,
                        pos: None,
                        rot: None,
                        scale: None,
                        inject: None,
                    });
                    rules.push(crate::game_link::SpawnRuleWire {
                        eff_hash: hash,
                        suppress: false,
                        motion,
                        frame_start: None,
                        frame_end: None,
                        pos: None,
                        rot: None,
                        scale: None,
                        inject: Some(inject),
                    });
                    continue;
                }
                missing_capture = true;
            }
            // Only push a transform when this spawn's offset/rot/scale actually differ
            // from pristine — untouched spawns keep the script's values.
            let moved = pristine
                .map(|p| {
                    p.offset != ec.offset || p.rotation != ec.rotation || p.scale != ec.scale
                })
                .unwrap_or(true);
            if moved {
                rules.push(crate::game_link::SpawnRuleWire {
                    eff_hash: hash,
                    suppress: false,
                    motion,
                    frame_start: window.0,
                    frame_end: window.1,
                    pos: Some(ec.offset),
                    rot: Some(ec.rotation),
                    scale: Some(ec.scale),
                    inject: None,
                });
            }
        }
        if rules.is_empty() {
            self.effect_rules_store.remove(&mv_key);
        } else {
            self.effect_rules_store.insert(mv_key, rules);
        }
        let all: Vec<crate::game_link::SpawnRuleWire> =
            self.effect_rules_store.values().flatten().cloned().collect();
        self.game_link.send_spawn_rules(&all);
        if missing_capture {
            self.state.status =
                "Swapped/retimed effect needs a live capture — perform the move in game once so \
                 the change previews live (export applies it regardless)."
                    .into();
        }
    }

    /// Build a live injection for a spawn from a captured EFFECT donor (matched by
    /// `donor_hash` — the ORIGINAL effect the script spawned, so a captured copy exists),
    /// swapping the graphic to `ec`'s (possibly different) effect and baking in the spawn's
    /// edited bone/offset/rotation/scale + new frame. Handles retime, effect-swap, or both.
    /// None when the original effect hasn't been captured live yet.
    fn build_effect_inject(
        &self,
        ec: &crate::data::EffectCall,
        motion: Option<u64>,
        donor_hash: u64,
    ) -> Option<crate::game_link::SpawnInjectWire> {
        use crate::game_link::LuaArgWire as A;
        let motion = motion?;
        let new_hash = effect_name_hash(&ec.effect_name);
        let captures = self.game_link.captures_for(motion);
        let donor = captures.iter().find(|c| {
            c.func.starts_with("EFFECT")
                && c.args.first().and_then(|a| a.as_hash()) == Some(donor_hash)
        })?;
        let flip = donor.func.contains("FLIP");
        let off = usize::from(flip);
        let mut args = donor.args.clone();
        // Vec layout (0-based, +off for flip): 0 gfx (0/1 for FLIP: gfxL/gfxR), 1 bone,
        // 2..4 pos xyz, 5..7 rot zr,yr,xr, 8 size.
        if args.len() < 9 + off {
            return None;
        }
        // Swap the graphic to the new effect (both left/right for FLIP variants).
        args[0] = A::Hash(new_hash);
        if flip {
            args[1] = A::Hash(new_hash);
        }
        args[1 + off] = A::Hash(hash40::hash40(&ec.bone_name.to_lowercase()).0);
        args[2 + off] = A::Num(ec.offset[0]);
        args[3 + off] = A::Num(ec.offset[1]);
        args[4 + off] = A::Num(ec.offset[2]);
        args[5 + off] = A::Num(ec.rotation[2]); // zr
        args[6 + off] = A::Num(ec.rotation[1]); // yr
        args[7 + off] = A::Num(ec.rotation[0]); // xr
        args[8 + off] = A::Num(ec.scale);
        Some(crate::game_link::SpawnInjectWire {
            frame: ec.active_start as f32,
            func: donor.func.clone(),
            args,
        })
    }

    fn draw_scrubber(&mut self, ui: &mut Ui) {
        if self.state.total_frames == 0 { return; }

        let total = self.state.total_frames;
        let current = self.state.current_frame;

        // Playback controls
        ui.horizontal(|ui| {
            let play_label = if self.state.playing { "⏸" } else { "▶" };
            if ui.button(play_label).clicked() {
                self.state.playing = !self.state.playing;
                // Reset the particle clock timer when unpausing to avoid a large
                // accumulated-time spike on the first step after resuming.
                if self.state.playing {
                    self.particle_step_time = std::time::Instant::now();
                }
            }
            if ui.button("|◀").clicked() {
                self.state.current_frame = 0;
                self.state.playing = false;
            }
            ui.label(format!("Frame {} / {}", current + 1, total));
        });

        // Timeline — 24px header + one 16px row per hitbox + a compact 7px row per effect
        // spawn at the bottom (start→end bars, like hitboxes but smaller).
        let n_fx = self.state.effects.len();
        let hb_band = if self.state.hitboxes.is_empty() { 0.0 } else { self.state.hitboxes.len() as f32 * 16.0 };
        let fx_band = if n_fx == 0 { 0.0 } else { 4.0 + n_fx as f32 * 7.0 };
        let timeline_height = (24.0 + hb_band + fx_band).max(24.0);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), timeline_height),
            egui::Sense::click_and_drag(),
        );

        let painter = ui.painter_at(rect);
        let w = rect.width();

        // Background
        painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(20, 20, 30));

        let frame_to_x = |f: u32| -> f32 {
            rect.left() + (f as f32 / total as f32) * w
        };

        // Hitbox bars
        for (row, hb) in self.state.hitboxes.iter().enumerate() {
            let y_top = rect.top() + 24.0 + row as f32 * 16.0;
            let y_bot = y_top + 14.0;
            let color = hitbox_display_color(hb);
            let is_selected = self.selected_hitbox == Some(row);

            let start_x = frame_to_x(hb.active_start);
            let end_x = if hb.active_end == 9999 {
                rect.right()
            } else {
                frame_to_x(hb.active_end + 1).min(rect.right())
            };

            if end_x > start_x {
                let bar_rect = egui::Rect::from_min_max(
                    egui::pos2(start_x, y_top),
                    egui::pos2(end_x, y_bot),
                );
                let alpha = if is_selected { 230 } else { 180 };
                painter.rect_filled(
                    bar_rect,
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
                );
                if is_selected {
                    painter.rect_stroke(
                        bar_rect,
                        2.0,
                        egui::Stroke::new(1.5, egui::Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                }
                // Label inside bar if wide enough
                let bar_w = end_x - start_x;
                if bar_w > 30.0 {
                    painter.text(
                        egui::pos2(start_x + 3.0, y_top + 7.0),
                        egui::Align2::LEFT_CENTER,
                        format!("#{} {}", hb.id, hb.bone_name),
                        egui::FontId::monospace(10.0),
                        egui::Color32::WHITE,
                    );
                }
            }
        }

        // Frame tick marks every 5 frames
        for f in (0..total).step_by(5) {
            let x = frame_to_x(f);
            let is_ten = f % 10 == 0;
            let tick_h = if is_ten { 8.0 } else { 4.0 };
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + tick_h)],
                egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
            );
            if is_ten {
                painter.text(
                    egui::pos2(x + 2.0, rect.top() + 10.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{}", f),
                    egui::FontId::monospace(9.0),
                    egui::Color32::from_gray(120),
                );
            }
        }

        // Effect spawn bars — a compact band below the hitbox rows showing each spawn's
        // start→end (one-shots get a short fixed window; follow effects extend to the end).
        let fx_band_top = rect.top() + 24.0 + hb_band + 2.0;
        for (row, e) in self.state.effects.iter().enumerate() {
            let y_top = fx_band_top + row as f32 * 7.0;
            let y_bot = y_top + 6.0;
            let end_frame = if e.follows_bone {
                if e.active_end >= total { total } else { e.active_end }
            } else {
                e.active_end.max(e.active_start.saturating_add(12)).min(total)
            };
            let start_x = frame_to_x(e.active_start.min(total));
            let end_x = frame_to_x(end_frame).max(start_x + 3.0).min(rect.right());
            let base = if e.disabled {
                egui::Color32::from_rgb(110, 110, 110)
            } else if e.follows_bone {
                egui::Color32::from_rgb(255, 165, 0)
            } else {
                egui::Color32::from_rgb(255, 220, 0)
            };
            let selected = self.state.selected_effect_call == Some(row);
            let alpha = if selected { 235 } else { 170 };
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(start_x, y_top), egui::pos2(end_x, y_bot)),
                1.0,
                egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha),
            );
            if selected {
                painter.rect_stroke(
                    egui::Rect::from_min_max(egui::pos2(start_x, y_top), egui::pos2(end_x, y_bot)),
                    1.0,
                    egui::Stroke::new(1.0, egui::Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
            }
        }

        // Playhead
        let px = frame_to_x(current);
        painter.line_segment(
            [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
            egui::Stroke::new(2.0, egui::Color32::WHITE),
        );
        // Playhead triangle
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(px, rect.top()),
                egui::pos2(px - 5.0, rect.top() - 7.0),
                egui::pos2(px + 5.0, rect.top() - 7.0),
            ],
            egui::Color32::WHITE,
            egui::Stroke::NONE,
        ));

        // Click/drag to scrub — but clicks on hitbox bars select that hitbox instead
        if response.dragged() || response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                // Only attempt hitbox selection on a clean click (not a drag)
                let clicked_hitbox = if response.clicked() {
                    let bar_area_top = rect.top() + 24.0;
                    if pos.y >= bar_area_top {
                        let row = ((pos.y - bar_area_top) / 16.0) as usize;
                        if row < self.state.hitboxes.len() {
                            let hb = &self.state.hitboxes[row];
                            let start_x = frame_to_x(hb.active_start);
                            let end_x = if hb.active_end == 9999 {
                                rect.right()
                            } else {
                                frame_to_x(hb.active_end + 1).min(rect.right())
                            };
                            if pos.x >= start_x && pos.x <= end_x {
                                Some(row)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(row) = clicked_hitbox {
                    self.selected_hitbox = if self.selected_hitbox == Some(row) {
                        None
                    } else {
                        Some(row)
                    };
                } else {
                    // Scrub the playhead
                    let t = ((pos.x - rect.left()) / w).clamp(0.0, 1.0);
                    self.state.current_frame = (t * total as f32) as u32;
                    self.state.playing = false;
                }
            }
        }
    }

    /// One-shot effect test: if `HITBOX_AUTOLOAD_FIGHTER` (+ optional
    /// `HITBOX_AUTOLOAD_EFFECT`) env vars are set, load that fighter and spawn the
    /// named emitter set at origin, then play. Runs once. Lets a single shell command
    /// open the editor straight onto a specific effect.
    fn maybe_autoload(&mut self) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let Ok(fighter) = std::env::var("HITBOX_AUTOLOAD_FIGHTER") else { return };

        // One-time: load the fighter and alias the effect-set name to a handle.
        static LOADED: AtomicBool = AtomicBool::new(false);
        if !LOADED.swap(true, Ordering::Relaxed) {
            let Some(idx) = self
                .state
                .fighters
                .iter()
                .position(|f| f.name.eq_ignore_ascii_case(fighter.trim()))
            else {
                eprintln!(
                    "[AUTOLOAD] fighter '{fighter}' not found among {} fighters",
                    self.state.fighters.len()
                );
                return;
            };
            eprintln!("[AUTOLOAD] loading fighter '{fighter}' (idx {idx})");
            self.select_fighter(idx);
            if let Ok(effect) = std::env::var("HITBOX_AUTOLOAD_EFFECT") {
                let effect = effect.trim().to_string();
                if let (Some(eff_index), Some(ptcl)) =
                    (self.state.eff_index.as_mut(), self.state.ptcl.as_ref())
                {
                    match ptcl.emitter_sets.iter().position(|s| s.name.eq_ignore_ascii_case(&effect)) {
                        Some(set_idx) => {
                            eff_index.handles.entry(effect.clone()).or_insert(set_idx as i32);
                            eff_index.handles.entry(effect.to_lowercase()).or_insert(set_idx as i32);
                        }
                        None => eprintln!(
                            "[AUTOLOAD] effect set '{effect}' not found; first sets: {:?}",
                            ptcl.emitter_sets.iter().map(|s| &s.name).take(10).collect::<Vec<_>>()
                        ),
                    }
                }
            }
        }

        // HITBOX_AUTOLOAD_MOVE=<name substring>: once the async move list has loaded,
        // select the matching move and Fetch ACMD so the *full* move effect list plays
        // exactly like clicking the move + "Fetch ACMD" in the UI. Runs once.
        if let Ok(move_query) = std::env::var("HITBOX_AUTOLOAD_MOVE") {
            static MOVE_DONE: AtomicBool = AtomicBool::new(false);
            if !MOVE_DONE.load(Ordering::Relaxed) {
                if self.move_list.is_empty() {
                    // Move list is still loading on a background thread (polled later in
                    // this same update). Retry next frame.
                    return;
                }
                // Normalize separators so a human query like "Attack Air F" matches an
                // internal move name like "attack_air_f".
                let norm = |s: &str| s.to_lowercase().replace([' ', '-'], "_");
                let q = norm(move_query.trim());
                match self
                    .move_list
                    .iter()
                    .find(|m| norm(&m.name).contains(&q))
                    .cloned()
                {
                    Some(entry) => {
                        eprintln!(
                            "[AUTOLOAD] move '{}' matched query '{}'",
                            entry.name,
                            move_query.trim()
                        );
                        MOVE_DONE.store(true, Ordering::Relaxed);
                        self.select_move(entry);
                        self.fetch_acmd();
                    }
                    None => {
                        MOVE_DONE.store(true, Ordering::Relaxed);
                        // Print candidates sharing the first query token to aid discovery.
                        let first_tok = q.split('_').next().unwrap_or("");
                        let candidates: Vec<&String> = self
                            .move_list
                            .iter()
                            .map(|m| &m.name)
                            .filter(|n| norm(n).contains(first_tok) && norm(n).contains("air"))
                            .collect();
                        eprintln!(
                            "[AUTOLOAD] move query '{}' matched nothing among {} moves; air candidates: {:?}",
                            move_query.trim(),
                            self.move_list.len(),
                            candidates
                        );
                    }
                }
            }
            return;
        }

        // Loop the effect for continuous preview: reset + re-spawn every ~1.8s of wall
        // clock (a one-shot like the bomb plays ~1s then vanishes). Resetting first
        // avoids unbounded emitter accumulation.
        let Ok(effect) = std::env::var("HITBOX_AUTOLOAD_EFFECT") else { return };
        let effect = effect.trim().to_string();
        static LAST_SPAWN: std::sync::Mutex<Option<std::time::Instant>> =
            std::sync::Mutex::new(None);
        let mut guard = LAST_SPAWN.lock().unwrap();
        let due = guard.map_or(true, |t| t.elapsed().as_secs_f32() > 1.8);
        if !due {
            return;
        }
        *guard = Some(std::time::Instant::now());
        drop(guard);

        self.state.particle_system.reset();
        let crate::data::AppState { particle_system, eff_index, ptcl, .. } = &mut self.state;
        if let (Some(eff_index), Some(ptcl)) = (eff_index.as_ref(), ptcl.as_ref()) {
            particle_system.spawn_effect(
                &effect,
                "top",
                glam::Vec3::ZERO,
                glam::Vec3::ZERO,
                0.0,
                9999.0,
                eff_index,
                ptcl,
            );
        }
        self.state.playing = true;
    }
}

impl eframe::App for HitboxEditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx();
        // Keep the game link alive whenever the app runs — live offsets, hitbox rules,
        // ACMD capture, the reconnect modal, and the One-Slot pool all need it, not just
        // the Eff Editor window (which used to be the only thing that started it).
        self.game_link.ensure_started();
        self.maybe_autoload();
        // Headless capture: HITBOX_SCREENSHOT=<png> saves exactly what the viewport renders
        // (full composite) after the effect has played a while, then exits. Works under Xvfb
        // (internal GPU readback, no swapchain capture needed).
        if let Ok(shot_path) = std::env::var("HITBOX_SCREENSHOT") {
            use std::sync::atomic::{AtomicU32, Ordering};
            static TICK: AtomicU32 = AtomicU32::new(0);
            let tick = TICK.fetch_add(1, Ordering::Relaxed);
            ctx.request_repaint();
            // HITBOX_SHOT_DETERMINISTIC=<handle>: at tick 60, freeze and rebuild the particle
            // system deterministically (integer-frame steps, identity bone) exactly like the
            // headless harness, so the LIVE render path draws the SAME sim state — isolating
            // render-path bugs from variable-dt sim differences.
            if tick >= 60 {
                if let Ok(handle) = std::env::var("HITBOX_SHOT_DETERMINISTIC") {
                    let frame: f32 = std::env::var("HITBOX_SHOT_FRAME").ok()
                        .and_then(|s| s.parse().ok()).unwrap_or(10.0);
                    if let (Some(eff), Some(ptcl)) =
                        (self.state.eff_index.as_ref(), self.state.ptcl.as_ref())
                    {
                        let bones: std::collections::HashMap<String, glam::Mat4> =
                            [("top".to_string(), glam::Mat4::IDENTITY),
                             ("Trans".to_string(), glam::Mat4::IDENTITY)].into();
                        self.state.particle_system.reset();
                        self.state.particle_system.spawn_effect(
                            &handle, "top", glam::Vec3::ZERO, glam::Vec3::ZERO, 0.0, 9999.0, eff, ptcl);
                        for f in 0..=(frame as u32) {
                            self.state.particle_system.step(f as f32, &bones, ptcl);
                        }
                        self.state.particle_system.particles.retain(|p| !p.is_dead());
                        if std::env::var("HITBOX_SHOT_NOEFFECT").is_ok() {
                            self.state.particle_system.particles.clear();
                            self.state.particle_system.active_emitters.clear();
                        }
                        self.state.playing = false;
                        self.particles_need_catchup = false;
                        if std::env::var("HITBOX_SHOT_MATCH_FRAME").is_ok() {
                            self.state.current_frame = frame as u32;
                        }
                        let ps = &self.state.particle_system.particles;
                        let (mut mn, mut mx, mut smin, mut smax) =
                            (glam::Vec3::splat(1e9), glam::Vec3::splat(-1e9), 1e9f32, -1e9f32);
                        for p in ps {
                            mn = mn.min(p.position); mx = mx.max(p.position);
                            smin = smin.min(p.size); smax = smax.max(p.size);
                        }
                        eprintln!("[SHOT-DET] rebuilt {handle} f{frame}: {} particles, cur_frame={}, pos min={:.1?} max={:.1?}, size [{:.2},{:.2}]",
                            ps.len(), self.state.current_frame, mn.to_array(), mx.to_array(), smin, smax);
                    }
                }
            }
            let np = self.state.particle_system.particles.len();
            if tick % 15 == 0 {
                eprintln!("[SHOT] tick={tick} particles={np} clock={:.1} playing={}",
                    self.particle_clock, self.state.playing);
            }
            if let Some(img) = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            }) {
                let (w, h) = (img.width() as u32, img.height() as u32);
                let rgba: Vec<u8> =
                    img.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b(), p.a()]).collect();
                let _ = ::image::save_buffer(&shot_path, &rgba, w, h, ::image::ColorType::Rgba8);
                eprintln!("[SCREENSHOT] saved {shot_path} {w}x{h} (particles={np})");
                std::process::exit(0);
            }
            // Deterministic mode: capture 2 ticks after the tick-60 rebuild, before the autoload
            // re-spawn (~tick 108) can clobber the frozen state. Otherwise capture at the peak.
            // HITBOX_SHOT_TICK=<n> forces the capture at an exact tick (playback debugging —
            // short-lived effects can die between the default triggers).
            let det = std::env::var("HITBOX_SHOT_DETERMINISTIC").is_ok();
            let forced_tick: Option<u32> = std::env::var("HITBOX_SHOT_TICK")
                .ok()
                .and_then(|s| s.parse().ok());
            let fire = match forced_tick {
                Some(t) => tick == t,
                None => (det && tick == 62) || (!det && ((tick > 20 && np >= 12) || tick > 240)),
            };
            if fire {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
        }
        // Poll background move list loader
        if let Some(rx) = &self.move_list_receiver {
            if let Ok(moves) = rx.try_recv() {
                let count = moves.len();
                self.move_list = moves;
                self.move_list_receiver = None;
                self.state.status = format!("Loaded {} moves.", count);
                ctx.request_repaint();
            }
        }

        // Handle pending model load — needs wgpu device/queue
        if let Some(model_dir) = self.pending_model_load.take() {
            if let Some(wgpu_state) = frame.wgpu_render_state() {
                let device = &wgpu_state.device;
                let queue = &wgpu_state.queue;

                // Only initialize 3D rendering if the device has the required features.
                if device.features().contains(ssbh_wgpu::REQUIRED_FEATURES) {
                    let mut renderer = wgpu_state.renderer.write();

                    // Initialize render state if not yet done
                    if renderer.callback_resources.get::<HitboxRenderState>().is_none() {
                        let rs = HitboxRenderState::new(device, queue, wgpu_state.target_format);
                        renderer.callback_resources.insert(rs);
                    }

                    if let Some(rs) = renderer.callback_resources.get_mut::<HitboxRenderState>() {
                        rs.load_model(device, queue, &model_dir);
                        // Eagerly load skeleton + anim so bone_world_matrices_at() is valid before
                        // the viewport prepare() pass (sim runs earlier in the same frame).
                        let skel_path = self.current_skel_path.as_deref();
                        if let Some(path) = skel_path {
                            rs.load_skeleton(path);
                        }
                        if let Some(anim_path) = &self.current_anim_path {
                            rs.apply_animation(
                                queue,
                                Some(anim_path.as_path()),
                                skel_path,
                                self.state.current_frame as f32,
                            );
                        }
                        let weapon_count = rs.weapon_skel_count();
                        if weapon_count > 0 {
                            self.state.status = format!("Model loaded ({} weapon skeleton{})",
                                weapon_count, if weapon_count == 1 { "" } else { "s" });
                        }
                        // bone_names already populated from skel file in select_fighter — don't overwrite
                    }
                    if !self.state.effects.is_empty() && self.state.ptcl.is_some() {
                        self.respawn_effects();
                    }
                } else {
                    self.state.status = "GPU lacks required features for 3D rendering (missing BC texture compression or similar).".to_string();
                }
            }
        }

        // Ensure viewport GPU state exists before shader/texture upload (eff may load before model).
        if let Some(wgpu_state) = frame.wgpu_render_state() {
            let mut renderer = wgpu_state.renderer.write();
            if renderer.callback_resources.get::<HitboxRenderState>().is_none() {
                let rs = HitboxRenderState::new(
                    &wgpu_state.device,
                    &wgpu_state.queue,
                    wgpu_state.target_format,
                );
                renderer.callback_resources.insert(rs);
            }
        }

        self.apply_pending_gpu_effects(frame);

        // Compute wall-clock dt for animation clocks (clamped to avoid huge jumps)
        let _anim_dt = {
            let elapsed = self.last_frame_time.elapsed().as_secs_f32();
            elapsed.clamp(0.0, 0.1)
        };

        // Advance playback
        if self.state.playing {
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(self.last_frame_time).as_secs_f32();
            if elapsed >= 1.0 / 24.0 {
                if self.state.total_frames > 0 {
                    self.state.current_frame = (self.state.current_frame + 1) % self.state.total_frames;
                } else {
                    // No animation loaded — still advance a virtual frame counter so
                    // particle simulation ticks forward (effects have active_start > 0).
                    // Cap at 9999 to avoid triggering the backwards-scrub reset.
                    self.state.current_frame = (self.state.current_frame + 1).min(9999);
                }
                self.last_frame_time = now;
            }
            // Always schedule next repaint while playing (particles need to animate)
            let next = std::time::Duration::from_secs_f32((1.0 / 24.0 - elapsed).max(0.0));
            ctx.request_repaint_after(next);
        }

        // Edit log window
        if self.show_edit_log {
            self.draw_edit_log_window(ctx);
        }

        // Top menu bar: File / Windows / Mod + status
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("SSBU Toolkit").size(16.0).color(egui::Color32::WHITE));
                ui.separator();

                ui.menu_button("File", |ui| {
                    if ui.button("Open Data Root…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.set_data_root(path);
                        }
                        ui.close();
                    }
                    if ui.button("Open Effect File…")
                        .on_hover_text("Load any .eff from disk (even outside the game data root) into the preview and donor pool")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Effect file", &["eff"])
                            .set_title("Open effect (.eff)")
                            .pick_file()
                        {
                            self.open_external_eff(path);
                        }
                        ui.close();
                    }
                    if !self.recent_effs.is_empty() {
                        ui.menu_button("Open Recent Eff", |ui| {
                            let recents = self.recent_effs.clone();
                            for p in recents {
                                let label = p
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or_else(|| p.to_str().unwrap_or("eff"));
                                if ui.button(label).on_hover_text(p.to_string_lossy()).clicked() {
                                    self.open_external_eff(p.clone());
                                    ui.close();
                                }
                            }
                        });
                    }
                });

                ui.menu_button("Windows", |ui| {
                    let eff_toggle = ui
                        .checkbox(&mut self.eff_editor.open, "Eff Editor")
                        .on_hover_text("Edit .eff authored values with in-game live preview (separate window)");
                    if eff_toggle.changed() && self.eff_editor.open {
                        // Opening the window always shows the selected fighter's eff (a
                        // queued load may have been consumed by an earlier open, or a
                        // different file loaded manually since).
                        if let Some(p) = self.current_eff_path.clone() {
                            self.eff_editor.queue_load(&p);
                        }
                    }
                    ui.checkbox(&mut self.state.show_effects_panel, "Effects panel")
                        .on_hover_text("Effect spawns of the current move");
                    ui.checkbox(&mut self.show_one_slot, "One-Slot Studio")
                        .on_hover_text(
                            "Copy any effect (any fighter / sys) into the current fighter's \
                             eff and redirect its uses",
                        );
                    let has_log = !self.state.edit_log.is_empty() || self.show_edit_log;
                    ui.add_enabled_ui(has_log, |ui| {
                        ui.checkbox(&mut self.show_edit_log, "Edit Log")
                            .on_hover_text("View and manage all saved edits");
                    });
                    ui.checkbox(&mut self.show_debug, "Debug");
                });

                ui.menu_button("Mod", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("name:");
                        ui.add(egui::TextEdit::singleline(&mut self.project_name).desired_width(140.0));
                    });
                    ui.separator();
                    if ui.button("Save Project…")
                        .on_hover_text("Save every edit (hitboxes, effect calls, eff values) as a project JSON")
                        .clicked()
                    {
                        self.save_project();
                        ui.close();
                    }
                    if ui.button("Load Project…")
                        .on_hover_text("Load a project/mod (modproject.json) for further editing; re-applies edits to the running game")
                        .clicked()
                    {
                        self.load_project();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Clear all game edits")
                        .on_hover_text("Wipe the game's saved pins (which survive restarts) + all live spawn/hitbox rules — use when old edits keep re-appearing")
                        .clicked()
                    {
                        self.clear_all_game_edits();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Export Full Mod…")
                        .on_hover_text("Rebuilt eff files + smashline source + project JSON into a folder you pick")
                        .clicked()
                    {
                        self.export_full_mod();
                        ui.close();
                    }
                    let can_export = self.state.selected_fighter.is_some()
                        && self.state.selected_move.is_some()
                        && !self.state.script.stmts.is_empty();
                    if ui.add_enabled(can_export, egui::Button::new("Export Source"))
                        .on_hover_text("Export edited hitboxes as smashline Rust source code")
                        .clicked()
                    {
                        self.export_acmd_source();
                        ui.close();
                    }
                });

                ui.separator();
                // Game-link status — same widget language as the Eff Editor header.
                let (dot, gtxt) = match self.game_link.status() {
                    crate::game_link::LinkStatus::Connected => {
                        (egui::Color32::from_rgb(90, 220, 90), "game")
                    }
                    crate::game_link::LinkStatus::Connecting => (egui::Color32::YELLOW, "game"),
                    crate::game_link::LinkStatus::Disconnected => {
                        (egui::Color32::from_rgb(220, 90, 90), "game")
                    }
                };
                ui.colored_label(dot, "●");
                ui.label(egui::RichText::new(gtxt).small());
                ui.separator();
                ui.label(egui::RichText::new(&self.state.status).color(egui::Color32::LIGHT_GRAY));
            });
        });

        // Bottom timeline
        egui::TopBottomPanel::bottom("scrubber")
            .min_height(60.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                self.draw_scrubber(ui);
            });

        // Eff editor (separate OS viewport; authored .eff editing + live game preview)
        let current_fighter = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone());
        self.eff_editor.set_target_fighter(current_fighter.clone());
        self.eff_editor
            .show(ctx, &self.game_link, &mut self.live_overrides);

        // Single debounced sender for every live override (color/speed kind multipliers;
        // per-spawn transforms go through push_effect_rules, not this store).
        self.live_overrides.flush_due(&self.game_link);
        if self.live_overrides.any_dirty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }

        // Live hitbox rules: watch the hitbox list for edits (sliders mutate in place) and
        // push the derived rule set debounced. Move switches reset the watch, not the rules.
        if let Some(mv) = self.current_move_key() {
            let same = self
                .hitbox_watch
                .as_ref()
                .map(|(k, v)| *k == mv && *v == self.state.hitboxes)
                .unwrap_or(false);
            if !same {
                let same_move = self
                    .hitbox_watch
                    .as_ref()
                    .map(|(k, _)| *k == mv)
                    .unwrap_or(false);
                self.hitbox_watch = Some((mv, self.state.hitboxes.clone()));
                if same_move {
                    self.hitbox_dirty_at = Some(std::time::Instant::now());
                }
            }
        }
        if let Some(t) = self.hitbox_dirty_at {
            if t.elapsed().as_millis() > 300 {
                self.hitbox_dirty_at = None;
                self.push_hitbox_rules();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        // Auto-adopt live capture when the current move has NO data yet (no GitHub fetch).
        let seq = self.game_link.captures_seq();
        if seq != self.captures_seen_seq {
            self.captures_seen_seq = seq;
            if self.state.acmd_source.is_empty()
                && self.state.hitboxes.is_empty()
                && self.state.effects.is_empty()
            {
                if let Some(m) = self.current_motion_hash() {
                    if !self.game_link.captures_for(m).is_empty() {
                        self.load_from_capture();
                    }
                }
            }
        }

        // Pin-sync check: on a new plugin connection, wait for the resync notifies to land,
        // then prompt about in-game pins this session doesn't know about ("ask on connect").
        let client = self.game_link.client_id();
        if client != self.pin_sync_client {
            self.pin_sync_client = client;
            self.pin_sync_prompt = None;
            self.pin_sync_wait = client.map(|_| std::time::Instant::now());
        }
        if let Some(t0) = self.pin_sync_wait {
            // The plugin persists pins on the SD card and re-applies them when the game boots,
            // so ANY pins present on a fresh connection are "old edits" the user should get to
            // keep or remove. Poll until the resync notifies land (they can take a few seconds)
            // rather than sampling once — a single 1.5s check missed late-arriving pins.
            let waited = t0.elapsed().as_millis();
            if waited > 1200 {
                let pinned = self.game_link.pinned_kinds();
                if !pinned.is_empty() {
                    self.pin_sync_wait = None;
                    self.pin_sync_prompt = Some(pinned);
                } else if waited > 6000 {
                    self.pin_sync_wait = None; // nothing pinned in-game — nothing to ask about
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(300));
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
        self.draw_pin_sync_modal(ctx);
        self.draw_one_slot_studio(ctx);
        self.draw_redirect_prompt(ctx);

        // Background mod-export build progress.
        if let Some(state) = &self.export_build {
            let finished = state.lock().ok().map(|s| (s.done, s.message.clone()));
            match finished {
                Some((true, msg)) => {
                    self.state.status = msg;
                    self.export_build = None;
                }
                _ => ctx.request_repaint_after(std::time::Duration::from_millis(500)),
            }
        }
        // Drain one-slot ops recorded in the eff editor into the project store.
        let ops = self.eff_editor.take_one_slots();
        if !ops.is_empty() {
            if let Some(fighter) = current_fighter {
                let entry = self.eff_mods.entry(fighter.clone()).or_default();
                if entry.source_rel.is_empty() {
                    entry.source_rel =
                        format!("effect/fighter/{fighter}/ef_{fighter}.eff");
                }
                for op in ops {
                    self.state.status =
                        format!("One-slot '{}' recorded for {fighter}", op.new_entry_name);
                    entry.one_slot.push(op);
                }
            }
        }

        // Effects panel (right side, shown when toggled)
        if self.state.show_effects_panel {
            egui::SidePanel::right("effects_panel").min_width(220.0).show(ctx, |ui| {
                self.draw_effects_panel(ui);
            });
        }

        // Left panel
        egui::SidePanel::left("left_panel").min_width(200.0).show(ctx, |ui| {
            self.draw_left_panel(ui);
        });

        // Right panel
        egui::SidePanel::right("right_panel").min_width(240.0).show(ctx, |ui| {
            self.draw_right_panel(ui);
        });

        // Step particle simulation and trail recording each frame
        if self.state.ptcl.is_some() {
            if hitbox_editor::fx_debug_enabled() {
                eprintln!("[SIM] ptcl present, active_emitters={} particles={} current_frame={}", 
                    self.state.particle_system.active_emitters.len(),
                    self.state.particle_system.particles.len(),
                    self.state.current_frame);
            }
            // Get bone matrices from the render state at a given simulation frame.
            let current_frame = self.state.current_frame;
            let get_bone_matrices = |sim_frame: u32| -> std::collections::HashMap<String, glam::Mat4> {
                if let Some(wgpu_state) = frame.wgpu_render_state() {
                    let renderer = wgpu_state.renderer.read();
                    renderer.callback_resources.get::<crate::renderer::HitboxRenderState>()
                        .map(|rs| rs.bone_world_matrices_at(sim_frame as f32))
                        .unwrap_or_default()
                } else {
                    std::collections::HashMap::new()
                }
            };
            let bone_matrices = get_bone_matrices(current_frame);
            if crate::fx_debug_enabled() {
                eprintln!("[SIM] bone_matrices count: {} (frame={})", bone_matrices.len(), current_frame);
            }

            if self.particles_need_catchup {
                let mut catchup_frame = current_frame;
                if let Some(ptcl) = &self.state.ptcl {
                    for f in 0..=catchup_frame {
                        let frame_bone_matrices = get_bone_matrices(f);
                        self.state.particle_system.step(f as f32, &frame_bone_matrices, ptcl);
                    }
                    self.particle_clock = catchup_frame as f32;
                    for effect in &mut self.active_effects {
                        effect.anim_clock = (catchup_frame as f32).min(effect.max_lifetime);
                    }
                }
                self.particles_need_catchup = false;
                self.last_simulated_frame = catchup_frame;
                self.state.trail_system.step(&bone_matrices);
                eprintln!(
                    "[SIM] catchup: {} particles, {} emitters at frame {}",
                    self.state.particle_system.particles.len(),
                    self.state.particle_system.active_emitters.len(),
                    catchup_frame,
                );
                ctx.request_repaint();
            } else {
            // Detect scrub-frame change (including backwards scrub / loop)
            let frame_changed = current_frame != self.last_simulated_frame;
            let scrub_backwards = self.last_simulated_frame != u32::MAX
                && current_frame < self.last_simulated_frame;

            if frame_changed {
                if scrub_backwards {
                    // Check if this is an animation loop (small backwards jump from near end to 0)
                    // vs a manual scrub backwards. For loops, only reset if effects have expired.
                    let is_loop = self.state.playing
                        && self.last_simulated_frame != u32::MAX
                        && current_frame == 0
                        && self.state.total_frames > 0
                        && self.last_simulated_frame >= self.state.total_frames.saturating_sub(2);
                    let effects_still_alive = !self.active_effects.is_empty()
                        && self.active_effects.iter().any(|e| e.anim_clock < e.max_lifetime);

                    if is_loop && effects_still_alive {
                        // Animation looped but effects are still running — don't reset,
                        // let the particle clock continue advancing.
                        // The effects will naturally expire when max_lifetime is reached.
                    } else {
                        // Manual scrub backwards or loop after effects expired — reset.
                        self.state.particle_system.reset();
                        self.state.trail_system.reset();
                        self.active_effects.clear();
                        self.particle_clock = 0.0;
                        self.particle_step_time = std::time::Instant::now();
                        if !self.state.playing {
                            self.queue_particle_resimulate_to_frame(current_frame);
                        }
                    }
                } else {
                    // Forward scrub — spawn any effects whose active_start frame was just crossed.
                    // This makes effects trigger each time the timeline passes their frame.
                    let prev_frame = self.last_simulated_frame;
                    if let (Some(eff_index), Some(ptcl)) = (&self.state.eff_index.clone(), &self.state.ptcl.clone()) {
                        let bone_name_map: std::collections::HashMap<String, String> = self.bone_names
                            .iter()
                            .map(|n| (n.to_lowercase(), n.clone()))
                            .collect();
                        let mut active_start_respawn = false;
                        for ec in &self.state.effects.clone() {
                            // Fire if active_start was crossed in this frame step
                            let crossed = if prev_frame == u32::MAX {
                                // After respawn last_simulated is MAX — spawn every effect already due.
                                ec.active_start <= current_frame
                            } else {
                                ec.active_start > prev_frame && ec.active_start <= current_frame
                            };
                            if !crossed { continue; }

                            let name_lower = ec.effect_name.to_lowercase();
                            let _canonical_bone = bone_name_map.get(&ec.bone_name.to_lowercase())
                                .cloned()
                                .unwrap_or_else(|| ec.bone_name.clone());
                            let is_trail = Self::is_trail_effect(&ec.effect_name, &name_lower, ec.follows_bone, eff_index, ptcl);
                            if is_trail { continue; } // trails handled separately

                            let _set_idx_opt = eff_index.handles.get(&ec.effect_name)
                                .or_else(|| eff_index.handles.get(&name_lower))
                                .copied()
                                .filter(|&idx| idx >= 0)
                                .map(|idx| idx as usize)
                                .filter(|&idx| idx < ptcl.emitter_sets.len());

                            // Reset particle system and clocks for a fresh burst
                            self.state.particle_system.reset();
                            self.active_effects.clear();
                            self.particle_clock = 0.0;
                            self.particle_step_time = std::time::Instant::now();
                            active_start_respawn = true;

                            // Re-spawn all effects that are active at this frame
                            for ec2 in &self.state.effects.clone() {
                                if ec2.active_start > current_frame { continue; }
                                let name_lower2 = ec2.effect_name.to_lowercase();
                                let canonical_bone2 = bone_name_map.get(&ec2.bone_name.to_lowercase())
                                    .cloned()
                                    .unwrap_or_else(|| ec2.bone_name.clone());
                                let is_trail2 = Self::is_trail_effect(&ec2.effect_name, &name_lower2, ec2.follows_bone, eff_index, ptcl);
                                if is_trail2 { continue; }
                                let set_idx_opt2 = eff_index.handles.get(&ec2.effect_name)
                                    .or_else(|| eff_index.handles.get(&name_lower2))
                                    .copied()
                                    .filter(|&idx| idx >= 0)
                                    .map(|idx| idx as usize)
                                    .filter(|&idx| idx < ptcl.emitter_sets.len());
                                let (start_frame, end_frame) = Self::effect_spawn_window(ec2, eff_index, ptcl);
                                self.state.particle_system.spawn_effect(
                                    &ec2.effect_name, &canonical_bone2,
                                    glam::Vec3::from(ec2.offset),
                                    glam::Vec3::from(ec2.rotation),
                                    start_frame, end_frame,
                                    eff_index, ptcl,
                                );
                                if let Some(set_idx2) = set_idx_opt2 {
                                    // max_lifetime must cover the latest emitter's full lifecycle:
                                    // emission_timing + emission_duration + particle_lifetime
                                    let max_lifetime2 = ptcl.emitter_sets[set_idx2].emitters.iter()
                                        .map(|e| {
                                            let emit_end = e.emission_timing as f32 + e.emission_duration as f32;
                                            emit_end + e.lifetime + e.lifetime_random
                                        })
                                        .fold(0.0f32, f32::max)
                                        .max(1.0);
                                    self.active_effects.push(ActiveEffect {
                                        anim_clock: 0.0,
                                        max_lifetime: max_lifetime2,
                                        emitter_set_idx: set_idx2,
                                    });
                                }
                            }
                            break; // only need to trigger once per frame step
                        }

                        // Playing skips the paused scrub block below — integer catch-up after respawn.
                        if active_start_respawn && self.state.playing {
                            for f in 0..=current_frame {
                                let frame_bone_matrices = get_bone_matrices(f);
                                self.state.particle_system.step(f as f32, &frame_bone_matrices, ptcl);
                            }
                            self.particle_clock = current_frame as f32;
                            for effect in &mut self.active_effects {
                                effect.anim_clock = (current_frame as f32).min(effect.max_lifetime);
                            }
                        }
                    }

                    // When scrubbing forward (not playing), advance the simulation to match the
                    // current frame. Three cases:
                    // 1. is_reset(): active_start handler just did reset+respawn — step 0..=current_frame
                    // 2. No emitters: first scrub, no prior playback — reset, respawn, then step
                    // 3. Has emitters: system is alive — step delta (last_frame+1..=current_frame)
                    if !self.state.playing && !scrub_backwards {
                        if let (Some(eff_index), Some(ptcl)) = (&self.state.eff_index.clone(), &self.state.ptcl.clone()) {
                            let mut is_reset = self.state.particle_system.is_reset();
                            let mut has_emitters = !self.state.particle_system.active_emitters.is_empty();
                            if !is_reset && !has_emitters {
                                // Case 2: no prior playback — full reset+respawn from frame 0
                                let bone_name_map: std::collections::HashMap<String, String> = self.bone_names
                                    .iter()
                                    .map(|n| (n.to_lowercase(), n.clone()))
                                    .collect();
                                self.state.particle_system.reset();
                                self.active_effects.clear();
                                self.particle_clock = 0.0;
                                for ec2 in &self.state.effects.clone() {
                                    if ec2.active_start > current_frame { continue; }
                                    let name_lower2 = ec2.effect_name.to_lowercase();
                                    let canonical_bone2 = bone_name_map.get(&ec2.bone_name.to_lowercase())
                                        .cloned()
                                        .unwrap_or_else(|| ec2.bone_name.clone());
                                    let is_trail2 = Self::is_trail_effect(&ec2.effect_name, &name_lower2, ec2.follows_bone, eff_index, ptcl);
                                    if is_trail2 { continue; }
                                    let set_idx_opt2 = eff_index.handles.get(&ec2.effect_name)
                                        .or_else(|| eff_index.handles.get(&name_lower2))
                                        .copied()
                                        .filter(|&idx| idx >= 0)
                                        .map(|idx| idx as usize)
                                        .filter(|&idx| idx < ptcl.emitter_sets.len());
                                    let (start_frame, end_frame) = Self::effect_spawn_window(ec2, eff_index, ptcl);
                                    self.state.particle_system.spawn_effect(
                                        &ec2.effect_name, &canonical_bone2,
                                        glam::Vec3::from(ec2.offset),
                                        glam::Vec3::from(ec2.rotation),
                                        start_frame, end_frame,
                                        eff_index, ptcl,
                                    );
                                    if let Some(set_idx2) = set_idx_opt2 {
                                        let max_lifetime2 = ptcl.emitter_sets[set_idx2].emitters.iter()
                                            .map(|e| {
                                                let emit_end = e.emission_timing as f32 + e.emission_duration as f32;
                                                emit_end + e.lifetime + e.lifetime_random
                                            })
                                            .fold(0.0f32, f32::max)
                                            .max(1.0);
                                        self.active_effects.push(ActiveEffect {
                                            anim_clock: 0.0,
                                            max_lifetime: max_lifetime2,
                                            emitter_set_idx: set_idx2,
                                        });
                                    }
                                }
                            }

                            is_reset = self.state.particle_system.is_reset();
                            has_emitters = !self.state.particle_system.active_emitters.is_empty();
                            if is_reset || !has_emitters {
                                // Full simulation from frame 0 (system was just reset or has no emitters).
                                // Compute bone matrices at each step's frame so early-spawning particles
                                // get the correct skeleton pose for their spawn frame.
                                for f in 0..=current_frame {
                                    let frame_bone_matrices = get_bone_matrices(f);
                                    self.state.particle_system.step(f as f32, &frame_bone_matrices, ptcl);
                                }
                            } else {
                                // Delta simulation: only step frames not yet simulated
                                let last_sim = self.state.particle_system.last_frame() as u32;
                                if current_frame > last_sim {
                                    for f in (last_sim + 1)..=current_frame {
                                        let frame_bone_matrices = get_bone_matrices(f);
                                        self.state.particle_system.step(f as f32, &frame_bone_matrices, ptcl);
                                    }
                                }
                            }

                            self.particle_clock = current_frame as f32;
                            for effect in &mut self.active_effects {
                                effect.anim_clock = (current_frame as f32).min(effect.max_lifetime);
                            }
                        }
                    }
                }

                self.last_simulated_frame = current_frame;
                // Step trail system on frame change
                self.state.trail_system.step(&bone_matrices);
            }

            // Advance per-effect animation clocks and step the particle simulation.
            // Clocks are in FRAMES (game runs at 60fps), so multiply wall-clock dt by 60.
            // Use particle_step_time (reset on each respawn) to avoid large dt spikes
            // from ACMD fetch latency or other delays.
            let particle_dt = {
                let elapsed = self.particle_step_time.elapsed().as_secs_f32();
                self.particle_step_time = std::time::Instant::now();
                // Only advance time when playing — pause freezes particles too
                if self.state.playing {
                    elapsed.clamp(0.0, 0.05) // cap at 3 frames (50ms) to prevent death-on-first-step
                } else {
                    0.0
                }
            };
            let anim_dt_frames = particle_dt * 60.0;
            let mut any_alive = false;
            if let Some(ptcl) = &self.state.ptcl.clone() {
                let has_emitters = !self.state.particle_system.active_emitters.is_empty();
                let has_particles = !self.state.particle_system.particles.is_empty();
                if has_emitters || has_particles {
                    // Advance the shared clock — always step while there's anything to simulate
                    // Always advance particle_clock so the simulation keeps running
                    // even after all effects' anim_clock reaches max_lifetime.
                    // Otherwise existing particles freeze mid-air (dt=0).
                    self.particle_clock += anim_dt_frames;
                    let max_clock = if !self.active_effects.is_empty() {
                        let mut max = 0.0f32;
                        for effect in &mut self.active_effects {
                            if effect.anim_clock < effect.max_lifetime {
                                effect.anim_clock += anim_dt_frames;
                                if effect.anim_clock > effect.max_lifetime {
                                    effect.anim_clock = effect.max_lifetime;
                                }
                            }
                            max = max.max(effect.anim_clock);
                        }
                        max.max(self.particle_clock)
                    } else {
                        self.particle_clock
                    };

                    // Never step backwards — ParticleSystem::step clears particles when
                    // target_frame < last_frame (scrub rewind), which also triggers when
                    // wall-clock max_clock lags integer catch-up last_frame.
                    let sim_target = max_clock
                        .max(self.state.particle_system.last_frame());
                    // Fixed 60 Hz timestep: advance the simulation only on whole-frame
                    // boundaries so the integrator always sees dt = 1.0, exactly like the
                    // game, independent of the render frame rate. particle_clock and
                    // anim_clock stay fractional accumulators for the UI/render clocks.
                    let last_frame_f = self.state.particle_system.last_frame();
                    let target_int = sim_target.floor() as i64;
                    if last_frame_f < 0.0 {
                        // First step after a (re)spawn: initialise at frame 0 (dt = 1).
                        self.state.particle_system.step(0.0, &bone_matrices, ptcl);
                    }
                    let last_int = last_frame_f.floor() as i64;
                    for f in (last_int.max(0) + 1)..=target_int {
                        self.state.particle_system.step(f as f32, &bone_matrices, ptcl);
                    }
                    // any_alive: true while particles are visible OR effects haven't fully expired
                    let effects_alive = self.active_effects.iter()
                        .any(|e| e.anim_clock < e.max_lifetime);
                    any_alive = !self.state.particle_system.particles.is_empty()
                        || effects_alive;
                    static SIM_EMPTY_DIAG: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !SIM_EMPTY_DIAG.swap(true, std::sync::atomic::Ordering::Relaxed)
                        && has_emitters
                        && self.state.particle_system.particles.is_empty()
                    {
                        eprintln!(
                            "[SIM] {} emitters active but 0 particles at frame {} (check active_start / emission window)",
                            self.state.particle_system.active_emitters.len(),
                            self.state.current_frame,
                        );
                    }
                }
            }

            // Request continuous repaint while any effect is still animating
            if any_alive {
                ctx.request_repaint();
            }
            } // particles_need_catchup else
        }

        // Commit any edits made this frame to the log
        self.commit_current_edits();

        // Central viewport
        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();

            if self.current_model_dir.is_some() {
                let w = rect.width();
                let h = rect.height();

                // Allocate the full rect as interactive so we can capture mouse input
                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

                // Camera controls — apply to render state
                if let Some(wgpu_state) = frame.wgpu_render_state() {
                    let mut renderer = wgpu_state.renderer.write();
                    if let Some(rs) = renderer.callback_resources.get_mut::<HitboxRenderState>() {
                        // Left drag: pan in camera plane (left/right + up/down)
                        if response.dragged_by(egui::PointerButton::Primary) {
                            let delta = response.drag_delta();
                            rs.camera.pan(delta.x, delta.y);
                            ctx.request_repaint();
                        }
                        // Middle drag or right drag: pan (disabled for now)
                        // Scroll: zoom
                        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                        if scroll != 0.0 && response.hovered() {
                            rs.camera.zoom(scroll * 0.05);
                            ctx.request_repaint();
                        }
                    }
                }

                // Paint the ssbh_wgpu scene via callback.
                let n_particles = self.state.particle_system.particles.len();
                let n_trails = self.state.trail_system.trails.len();
                let active_emitters = self.state.particle_system.active_emitters.clone();
                let bone_matrices = frame
                    .wgpu_render_state()
                    .and_then(|ws| {
                        ws.renderer
                            .read()
                            .callback_resources
                            .get::<HitboxRenderState>()
                            .map(|rs| rs.bone_world_matrices())
                    })
                    .unwrap_or_default();
                if n_particles > 0 || n_trails > 0 {
                }
                let callback = egui_wgpu::Callback::new_paint_callback(
                    rect,
                    ViewportCallback {
                        width: w,
                        height: h,
                        current_frame: self.state.current_frame as f32,
                        anim_path: self.current_anim_path.clone(),
                        skel_path: self.current_skel_path.clone(),
                        particles: self.state.particle_system.particles.clone(),
                        trails: self.state.trail_system.trails.clone(),
                        emitter_sets: self.state.ptcl.as_ref()
                            .map(|p| p.emitter_sets.clone())
                            .unwrap_or_default(),
                        bfres_models: self.state.ptcl.as_ref()
                            .map(|p| p.bfres_models.clone())
                            .unwrap_or_default(),
                        bone_matrices,
                        active_emitters,
                    },
                );
                ui.painter().add(callback);

                // Draw hitbox spheres as projected 2D circles
                let frame_num = self.state.current_frame;
                if let Some(wgpu_state) = frame.wgpu_render_state() {
                    let renderer = wgpu_state.renderer.read();
                    if let Some(rs) = renderer.callback_resources.get::<HitboxRenderState>() {
                        let bone_matrices = rs.bone_world_matrices();
                        // Keep a positions map for debug display
                        let bone_positions: std::collections::HashMap<String, glam::Vec3> = bone_matrices.iter()
                            .map(|(k, m)| (k.clone(), m.col(3).truncate()))
                            .collect();

                        if self.show_debug {
                            let mut names: Vec<&String> = bone_positions.keys().collect();
                            names.sort();
                            for (i, name) in names.iter().take(30).enumerate() {
                                ui.painter().text(
                                    rect.left_top() + egui::vec2(4.0, 4.0 + i as f32 * 12.0),
                                    egui::Align2::LEFT_TOP, name.as_str(),
                                    egui::FontId::monospace(9.0), egui::Color32::YELLOW,
                                );
                            }
                            for (i, hb) in self.state.hitboxes.iter().enumerate().take(5) {
                                let found = bone_matrices.contains_key(&hb.bone_name)
                                    || bone_matrices.contains_key(&hb.bone_name.to_lowercase());
                                ui.painter().text(
                                    rect.right_top() + egui::vec2(-220.0, 4.0 + i as f32 * 12.0),
                                    egui::Align2::LEFT_TOP,
                                    format!("{:?} found:{}", hb.bone_name, found),
                                    egui::FontId::monospace(9.0), egui::Color32::LIGHT_BLUE,
                                );
                            }
                            for (name, pos) in &bone_positions {
                                if let Some(sp) = rs.world_to_screen(*pos, rect) {
                                    ui.painter().circle_filled(sp, 3.0, egui::Color32::from_rgba_unmultiplied(0, 255, 0, 150));
                                    ui.painter().text(sp + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER,
                                        name, egui::FontId::monospace(8.0), egui::Color32::from_rgb(0, 220, 0));
                                }
                            }
                        }

                        for hb in &self.state.hitboxes {
                            let active = hb.active_frames_empty() ||
                                (frame_num >= hb.active_start && frame_num <= hb.active_end);
                            if !active { continue; }

                            let color = hitbox_display_color(hb);
                            let stroke = egui::Stroke::new(2.0, color);
                            let fill = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40);

                            // Get bone world matrix — offsets are in bone local space.
                            // For system/root bones (top, Trans, Rot, throw) the offsets
                            // are effectively in world space, so we only use translation.
                            let bone_mat = bone_matrices.get(&hb.bone_name)
                                .or_else(|| bone_matrices.get(&hb.bone_name.to_lowercase()))
                                .copied()
                                .unwrap_or(glam::Mat4::IDENTITY);

                            let bone_mat = if is_system_bone(&hb.bone_name) {
                                // Keep only the translation — offsets are world-space
                                glam::Mat4::from_translation(bone_mat.col(3).truncate())
                            } else {
                                bone_mat
                            };

                            // Transform offset from bone local space to world space
                            let offset = glam::Vec3::new(hb.offset_x, hb.offset_y, hb.offset_z);
                            let world_pos = bone_mat.transform_point3(offset);

                            if let Some([ex, ey, ez]) = hb.capsule_end {
                                let end_offset = glam::Vec3::new(ex, ey, ez);
                                let world_end = bone_mat.transform_point3(end_offset);
                                let sp1 = rs.world_to_screen(world_pos, rect);
                                let sp2 = rs.world_to_screen(world_end, rect);
                                let r1 = rs.world_radius_to_screen(world_pos, hb.size, rect)
                                    .unwrap_or(hb.size * 4.0).max(4.0);
                                let r2 = rs.world_radius_to_screen(world_end, hb.size, rect)
                                    .unwrap_or(hb.size * 4.0).max(4.0);

                                if let (Some(p1), Some(p2)) = (sp1, sp2) {
                                    let dir = (p2 - p1).normalized();
                                    let perp = egui::vec2(-dir.y, dir.x);
                                    ui.painter().line_segment([p1 + perp * r1, p2 + perp * r2], stroke);
                                    ui.painter().line_segment([p1 - perp * r1, p2 - perp * r2], stroke);
                                    ui.painter().add(egui::Shape::convex_polygon(
                                        vec![p1 + perp * r1, p2 + perp * r2, p2 - perp * r2, p1 - perp * r1],
                                        fill, egui::Stroke::NONE,
                                    ));
                                    ui.painter().circle(p1, r1, fill, stroke);
                                    ui.painter().circle(p2, r2, fill, stroke);
                                    let label_pos = p1 + (p2 - p1) * 0.5;
                                    ui.painter().text(
                                        label_pos + egui::vec2(r1.max(r2) + 2.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        format!("#{} {:.0}", hb.id, hb.damage),
                                        egui::FontId::monospace(11.0), color,
                                    );
                                } else if let Some(p) = sp1.or(sp2) {
                                    let r = r1.max(r2);
                                    ui.painter().circle(p, r, fill, stroke);
                                }
                            } else {
                                if let Some(screen_pos) = rs.world_to_screen(world_pos, rect) {
                                    let screen_radius = rs.world_radius_to_screen(world_pos, hb.size, rect)
                                        .unwrap_or(hb.size * 4.0)
                                        .max(4.0);
                                    ui.painter().circle(screen_pos, screen_radius, fill, stroke);
                                    ui.painter().text(
                                        screen_pos + egui::vec2(screen_radius + 2.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        format!("#{} {:.0}", hb.id, hb.damage),
                                        egui::FontId::monospace(11.0), color,
                                    );
                                }
                            }
                        }

                        // Effect spawn markers: blue circles at bone position + script offset.
                        let click_pos = ui.input(|i| {
                            i.pointer
                                .interact_pos()
                                .filter(|p| i.pointer.primary_clicked() && rect.contains(*p))
                        });
                        let mut best_pick: Option<(usize, f32)> = None;
                        for (i, ec) in self.state.effects.iter().enumerate() {
                            if ec.disabled {
                                continue;
                            }
                            let active =
                                frame_num >= ec.active_start && frame_num <= ec.active_end;
                            if !active && self.state.selected_effect_call != Some(i) {
                                continue;
                            }
                            // Same bone conventions as hitboxes; skip unresolvable bones
                            // (FOOT_/LANDING_ synthetic targets have no skeleton match).
                            let Some(bone_mat) = bone_matrices
                                .get(&ec.bone_name)
                                .or_else(|| bone_matrices.get(&ec.bone_name.to_lowercase()))
                                .copied()
                            else {
                                continue;
                            };
                            let bone_mat = if is_system_bone(&ec.bone_name) {
                                glam::Mat4::from_translation(bone_mat.col(3).truncate())
                            } else {
                                bone_mat
                            };
                            let world_pos =
                                bone_mat.transform_point3(glam::Vec3::from(ec.offset));
                            let Some(screen_pos) = rs.world_to_screen(world_pos, rect) else {
                                continue;
                            };
                            let radius = rs
                                .world_radius_to_screen(world_pos, ec.scale.max(0.4), rect)
                                .unwrap_or(ec.scale * 4.0)
                                .clamp(5.0, 120.0);

                            let selected = self.state.selected_effect_call == Some(i);
                            let blue = if selected {
                                egui::Color32::from_rgb(120, 190, 255)
                            } else {
                                egui::Color32::from_rgb(60, 130, 235)
                            };
                            let fill = egui::Color32::from_rgba_unmultiplied(
                                blue.r(),
                                blue.g(),
                                blue.b(),
                                if selected { 55 } else { 28 },
                            );
                            let stroke_w = if selected { 2.5 } else { 1.5 };
                            ui.painter()
                                .circle(screen_pos, radius, fill, egui::Stroke::new(stroke_w, blue));
                            // Crosshair dot at the exact spawn point
                            ui.painter().circle_filled(screen_pos, 2.0, blue);
                            ui.painter().text(
                                screen_pos + egui::vec2(radius + 3.0, -radius * 0.5),
                                egui::Align2::LEFT_CENTER,
                                &ec.effect_name,
                                egui::FontId::monospace(9.0),
                                blue,
                            );

                            if let Some(cp) = click_pos {
                                let d = cp.distance(screen_pos);
                                if d <= radius.max(12.0)
                                    && best_pick.map(|(_, bd)| d < bd).unwrap_or(true)
                                {
                                    best_pick = Some((i, d));
                                }
                            }
                        }
                        if let Some((i, _)) = best_pick {
                            self.state.selected_effect_call = Some(i);
                            self.state.show_effects_panel = true;
                        }

                        // Particles and trails are rendered by the GPU via ViewportCallback/ParticleRenderer.
                    }
                }
            } else {
                ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(17, 17, 34));
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Open a data root directory to begin").color(Color32::GRAY));
                });
            }
        });

        // Eff/VFX panel actions run above; apply any BNSH reload queued this frame.
        self.apply_pending_gpu_effects(frame);
    }
}

impl Hitbox {
    fn active_frames_empty(&self) -> bool {
        self.active_end == 9999
    }
}

fn find_nuanmb(motion_dir: &Path, label: &str, hash: u64) -> Option<PathBuf> {
    let p = motion_dir.join(format!("{}.nuanmb", label));
    if p.exists() { return Some(p); }

    let suffix = label.replace('_', "").to_lowercase();
    if let Ok(entries) = std::fs::read_dir(motion_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("nuanmb") { continue; }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
            if stem.ends_with(&suffix) { return Some(path); }
        }
    }

    let p = motion_dir.join(format!("{:#018x}.nuanmb", hash));
    if p.exists() { return Some(p); }
    None
}

/// Rebuild an AcmdScript by patching AttackCall values from the edited Hitbox list.
/// Non-attack statements (frame, wait, raw, etc.) are preserved verbatim.
/// Hitboxes are matched by `id`; the last edited Hitbox with a given id wins.
fn rebuild_script_from_hitboxes(
    original: &crate::data::AcmdScript,
    hitboxes: &[crate::data::Hitbox],
) -> crate::data::AcmdScript {
    use crate::data::{AcmdScript, AcmdStmt, AttackCall, ExcuteStmt};

    // Build a lookup: id → latest Hitbox
    let mut by_id: std::collections::HashMap<u32, &crate::data::Hitbox> = std::collections::HashMap::new();
    for hb in hitboxes {
        by_id.insert(hb.id, hb);
    }

    fn patch_attack(call: &AttackCall, by_id: &std::collections::HashMap<u32, &crate::data::Hitbox>) -> AttackCall {
        if let Some(hb) = by_id.get(&call.id) {
            AttackCall {
                id: hb.id,
                part: hb.part,
                bone_name: hb.bone_name.clone(),
                damage: hb.damage,
                angle: hb.angle,
                kb_scaling: hb.kb_scaling,
                fkb: hb.fkb,
                kb_base: hb.kb_base,
                size: hb.size,
                offset_x: hb.offset_x,
                offset_y: hb.offset_y,
                offset_z: hb.offset_z,
                capsule_end: hb.capsule_end,
                hitlag_mult: hb.hitlag_mult,
                sdi_mult: hb.sdi_mult,
                setoff_kind: hb.setoff_kind.clone(),
                lr_check: hb.lr_check.clone(),
                is_clang: hb.is_clang,
                is_add_attack: hb.is_add_attack,
                hitbox_attr: hb.hitbox_attr,
                ground_or_air: hb.ground_or_air,
                is_mtk: hb.is_mtk,
                is_shield_disable: hb.is_shield_disable,
                is_reflectable: hb.is_reflectable,
                is_absorbable: hb.is_absorbable,
                is_landing_attack: hb.is_landing_attack,
                situation_mask: hb.situation_mask.clone(),
                category_mask: hb.category_mask.clone(),
                part_mask: hb.part_mask.clone(),
                no_finish_camera: hb.no_finish_camera,
                collision_attr: hb.collision_attr.clone(),
                sound_level: hb.sound_level.clone(),
                sound_attr: hb.sound_attr.clone(),
                attack_region: hb.attack_region.clone(),
            }
        } else {
            call.clone()
        }
    }

    fn patch_stmts(
        stmts: &[AcmdStmt],
        by_id: &std::collections::HashMap<u32, &crate::data::Hitbox>,
    ) -> Vec<AcmdStmt> {
        stmts.iter().map(|stmt| match stmt {
            AcmdStmt::Excute(inner) => {
                let patched = inner.iter().map(|s| match s {
                    ExcuteStmt::Attack(call) => ExcuteStmt::Attack(patch_attack(call, by_id)),
                    other => other.clone(),
                }).collect();
                AcmdStmt::Excute(patched)
            }
            AcmdStmt::Loop { count, body } => AcmdStmt::Loop {
                count: *count,
                body: patch_stmts(body, by_id),
            },
            other => other.clone(),
        }).collect()
    }

    AcmdScript { stmts: patch_stmts(&original.stmts, &by_id) }
}

/// Build a whole ACMD script from a hitbox list (capture-sourced moves have no base script
/// to patch): frame-grouped ATTACKs, then clear_all after the last active window.
fn synthesize_script_from_hitboxes(hitboxes: &[crate::data::Hitbox]) -> crate::data::AcmdScript {
    use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
    if hitboxes.is_empty() {
        return AcmdScript::default();
    }
    let mut sorted: Vec<&crate::data::Hitbox> = hitboxes.iter().collect();
    sorted.sort_by_key(|h| (h.active_start, h.id));

    let mut stmts: Vec<AcmdStmt> = Vec::new();
    let mut current: Option<u32> = None;
    let mut group: Vec<ExcuteStmt> = Vec::new();
    for hb in &sorted {
        if current != Some(hb.active_start) {
            if !group.is_empty() {
                stmts.push(AcmdStmt::Excute(std::mem::take(&mut group)));
            }
            stmts.push(AcmdStmt::Frame(hb.active_start as f32));
            current = Some(hb.active_start);
        }
        group.push(ExcuteStmt::Attack(hb.to_attack_call()));
    }
    if !group.is_empty() {
        stmts.push(AcmdStmt::Excute(group));
    }
    let clear_at = sorted.iter().map(|h| h.active_end).max().unwrap_or(0) + 1;
    stmts.push(AcmdStmt::Frame(clear_at as f32));
    stmts.push(AcmdStmt::Excute(vec![ExcuteStmt::ClearAll]));
    AcmdScript { stmts }
}

/// Display labels for the move-list categories, indexed by `move_category_index`.
const MOVE_CATEGORY_LABELS: [&str; 12] = [
    "Specials",
    "Aerials",
    "Tilts",
    "Smashes",
    "Jabs",
    "Dash Attack",
    "Grabs & Throws",
    "Dodges & Rolls",
    "Ledge",
    "Get-ups",
    "Movement",
    "Other",
];

/// Bucket a raw motion name (e.g. "special_air_hi", "attack_s3_s") into a category index
/// matching `MOVE_CATEGORY_LABELS`, so the move list groups by the familiar move families.
fn move_category_index(name: &str) -> usize {
    let n = name;
    if n.starts_with("special") {
        0
    } else if n.starts_with("attack_air") {
        1
    } else if n.starts_with("attack_s3") || n.starts_with("attack_hi3") || n.starts_with("attack_lw3") {
        2 // tilts
    } else if n.starts_with("attack_s4") || n.starts_with("attack_hi4") || n.starts_with("attack_lw4") {
        3 // smashes
    } else if n.starts_with("attack_dash") {
        5 // dash attack
    } else if n.starts_with("attack_1") || n.starts_with("attack_9") {
        4 // jabs (attack_11/12/13, rapid-jab attack_100*)
    } else if n.starts_with("catch") || n.starts_with("throw") {
        6
    } else if n.starts_with("escape") {
        7 // spot dodge / rolls
    } else if n.starts_with("cliff") {
        8 // ledge
    } else if n.starts_with("down") || n.starts_with("lie") || n.starts_with("passive") {
        9 // get-ups
    } else if n.starts_with("walk")
        || n.starts_with("run")
        || n.starts_with("dash")
        || n.starts_with("turn")
        || n.starts_with("jump")
        || n.starts_with("fall")
        || n.starts_with("landing")
        || n.starts_with("step")
        || n.starts_with("guard")
        || n.starts_with("wait")
    {
        10 // movement / defense
    } else {
        11 // other
    }
}

fn format_move_name(name: &str) -> String {
    let stripped = if name.len() > 3 {
        let b = name.as_bytes();
        if b[0].is_ascii_alphabetic() && b[1].is_ascii_digit() && b[2].is_ascii_digit() {
            &name[3..]
        } else { name }
    } else { name };

    stripped.replace('_', " ")
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Mod project writer ────────────────────────────────────────────────────────

/// Write a `ModProject` into `parent_dir/{project.name}/` and return the root path.
/// Progress of a background `cargo skyline build` for an exported mod.
struct ExportBuildState {
    done: bool,
    message: String,
}

/// One known use of a one-slotted donor effect, offered for redirect to the new name.
struct RedirectUse {
    /// "fighter/move" key.
    move_key: String,
    /// Index into that move's full call list.
    call_idx: usize,
    label: String,
    selected: bool,
}

/// The "which uses go to the new effect?" prompt state.
struct RedirectPrompt {
    donor_name: String,
    new_name: String,
    uses: Vec<RedirectUse>,
}

fn write_mod_project(
    project: &crate::acmd::ModProject,
    parent_dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let root = parent_dir.join(&project.name);
    for file in &project.files {
        let dest = root.join(&file.rel_path);
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&dest, &file.contents)?;
    }
    Ok(root)
}

// ── Persistent config ─────────────────────────────────────────────────────────

fn config_path(key: &str) -> Option<std::path::PathBuf> {
    // Store in ~/.config/ssbu_hitbox_editor/ (or equivalent on each OS)
    let base = dirs::config_dir()?;
    Some(base.join("ssbu_hitbox_editor").join(key))
}

fn save_config_path(key: &str, path: &std::path::Path) {
    if let Some(dest) = config_path(key) {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&dest, path.to_string_lossy().as_bytes());
    }
}

fn load_config_path(key: &str) -> Option<std::path::PathBuf> {
    let dest = config_path(key)?;
    let s = std::fs::read_to_string(&dest).ok()?;
    let p = std::path::PathBuf::from(s.trim());
    if p.exists() { Some(p) } else { None }
}

/// Recently opened external eff files (most-recent first), one path per line.
fn load_recent_effs() -> Vec<PathBuf> {
    let Some(dest) = config_path("recent_effs") else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&dest) else { return Vec::new() };
    s.lines()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| !p.as_os_str().is_empty() && p.exists())
        .collect()
}

fn save_recent_effs(list: &[PathBuf]) {
    if let Some(dest) = config_path("recent_effs") {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body: String = list
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(&dest, body);
    }
}
