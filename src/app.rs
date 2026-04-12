/// Main egui application for the SSBU Hitbox Editor.

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
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(160.0);
            if ui.selectable_label(special_label == "Custom", "Custom (0°)").clicked() {
                *angle = 0;
                ui.memory_mut(|m| m.close_popup(popup_id));
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
    fighter_search: String,
    move_search: String,
    /// Last frame for which particles were simulated — used to detect backwards scrubs
    last_simulated_frame: u32,
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
            fighter_search: String::new(),
            move_search: String::new(),
            last_simulated_frame: u32::MAX,
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
            self.load_eff_file(&eff_path);
            // Also load ef_common.eff (system-wide effects: sys_smash_flash, sys_attack_arc, etc.)
            if let Some(root) = &self.state.data_root.clone() {
                // Try known locations for the common/sys eff file
                let sys_candidates = [
                    root.join("effect").join("system").join("common").join("ef_common.eff"),
                    root.join("effect").join("fighter").join("sys").join("ef_sys.eff"),
                    root.join("effect").join("sys").join("ef_sys.eff"),
                    root.join("effect").join("common").join("ef_sys.eff"),
                    root.join("effect").join("ef_sys.eff"),
                ];
                // Also scan effect/ subdirs for any ef_sys.eff
                let mut found_sys = false;
                for p in &sys_candidates {
                    if p.exists() {
                        eprintln!("[EFF] merging sys eff with ptcl: {:?}", p);
                        if let (Some(eff_index), Some(ptcl)) = (&mut self.state.eff_index, &mut self.state.ptcl) {
                            let _ = eff_index.merge_from_file_with_ptcl(p, ptcl);
                        }
                        self.state.pending_texture_upload = true;
                        found_sys = true;
                        break;
                    }
                }
                if !found_sys {
                    // Scan effect/ subdirs for ef_sys.eff or ef_common.eff one level deep
                    if let Ok(entries) = std::fs::read_dir(root.join("effect")) {
                        for entry in entries.flatten() {
                            let p1 = entry.path().join("ef_sys.eff");
                            let p2 = entry.path().join("ef_common.eff");
                            let p = if p1.exists() { p1 } else if p2.exists() { p2 } else { continue };
                            eprintln!("[EFF] scanning for sys: {:?} exists=true", p);
                            if let (Some(eff_index), Some(ptcl)) = (&mut self.state.eff_index, &mut self.state.ptcl) {
                                let _ = eff_index.merge_from_file_with_ptcl(&p, ptcl);
                            }
                            self.state.pending_texture_upload = true;
                            found_sys = true;
                            break;
                        }
                    }
                    if !found_sys {
                        eprintln!("[EFF] ef_sys.eff not found — injecting synthetic sys emitter sets");
                        // Append synthetic emitter sets for common sys effects and register their handles
                        if let (Some(eff_index), Some(ptcl)) = (&mut self.state.eff_index, &mut self.state.ptcl) {
                            let sys_effects: &[(&str, crate::effects::BlendType, f32, f32)] = &[
                                // (name, blend, scale, lifetime)
                                ("sys_smash_flash",    crate::effects::BlendType::Add,    0.4,  8.0),
                                ("sys_attack_arc",     crate::effects::BlendType::Add,    0.3, 12.0),
                                ("sys_attack_arc_b",   crate::effects::BlendType::Add,    0.3, 12.0),
                                ("sys_attack_arc_lw",  crate::effects::BlendType::Add,    0.3, 12.0),
                                ("sys_hit_smoke",      crate::effects::BlendType::Normal, 0.3, 10.0),
                                ("sys_landing_smoke",  crate::effects::BlendType::Normal, 0.2,  8.0),
                            ];
                            for (name, blend, scale, lifetime) in sys_effects {
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
                                        emission_rate: 6.0,
                                        emission_rate_random: 0.0,
                                        initial_speed: 0.15,
                                        speed_random: 0.3,
                                        accel: glam::Vec3::ZERO,
                                        lifetime: *lifetime,
                                        lifetime_random: 0.0,
                                        scale: *scale,
                                        scale_random: 0.0,
                                        rotation_speed: 0.0,
                                        color0: vec![crate::effects::ColorKey { frame: 0.0, r: 1.0, g: 1.0, b: 1.0, a: 1.0 }],
                                        color1: Vec::new(),
                                        alpha0: crate::effects::AnimKey3v4k::default(),
                                        alpha1: crate::effects::AnimKey3v4k::default(),
                                        scale_anim: crate::effects::AnimKey3v4k::default(),
                                        textures: Vec::new(),
                                        mesh_type: 0,
                                        primitive_index: 0,
                                        texture_index: 0,
                                        is_one_time: true,
                                        emission_timing: 0,
                                        emission_duration: 1,
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
                    if let Some(first) = hitboxes.first() {
                        if first.active_start > 0 {
                            self.state.current_frame = first.active_start;
                        }
                    }
                    self.state.hitboxes = hitboxes;
                    self.state.script = script;

                    // Store effect data
                    self.state.effects = effect_script.to_effect_calls();
                    self.state.effect_script = effect_script;

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

    /// Re-spawn all effects into the particle/trail systems using current eff_index + ptcl.
    /// Call this after loading a new .eff file or after fetching ACMD.
    fn load_eff_file(&mut self, path: &std::path::Path) {
        match crate::effects::EffIndex::from_file(path) {
            Ok(eff) => {
                eprintln!("[EFF] loaded {} handles, ptcl_data={} bytes", eff.handles.len(), eff.ptcl_data.len());
                for (k, v) in eff.handles.iter().take(8) {
                    eprintln!("[EFF]   handle {:?} -> set_idx {}", k, v);
                }
                if !eff.ptcl_data.is_empty() {
                    match crate::effects::PtclFile::parse(&eff.ptcl_data) {
                        Ok(ptcl) => {
                            eprintln!("[EFF] ptcl ok: {} emitter sets", ptcl.emitter_sets.len());
                            self.state.status = format!(
                                "Loaded {} effects ({} emitter sets)",
                                eff.handles.len(), ptcl.emitter_sets.len()
                            );
                            self.state.ptcl = Some(ptcl);
                            self.state.pending_texture_upload = true;
                        }
                        Err(e) => {
                            // VFXB (Switch format) — fall back to name-aware synthetic emitter sets
                            eprintln!("[EFF] ptcl parse error ({e}), using synthetic emitter sets");
                            let max_idx = eff.handles.values().copied().max().unwrap_or(0).max(0) as usize;
                            // Build a reverse map: set_index -> handle_name for color hinting
                            let mut idx_to_name: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
                            for (name, &idx) in &eff.handles {
                                // Only store lowercase names (skip duplicates)
                                if name.chars().any(|c| c.is_uppercase()) { continue; }
                                idx_to_name.entry(idx).or_insert_with(|| name.clone());
                            }
                            let ptcl = crate::effects::PtclFile::synthetic_named(max_idx, &idx_to_name);
                            self.state.status = format!(
                                "Loaded {} effects (synthetic, VFXB format)",
                                eff.handles.len()
                            );
                            self.state.ptcl = Some(ptcl);
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

    fn respawn_effects(&mut self) {
        self.state.particle_system.reset();
        self.state.trail_system.reset();
        // Reset to frame 0 and auto-play so the simulation ticks forward with repaints
        self.state.current_frame = 0;
        self.last_simulated_frame = u32::MAX;
        self.state.playing = true;
        self.last_frame_time = std::time::Instant::now();
        eprintln!("[RESPAWN] effects={} eff_index={} ptcl={}", 
            self.state.effects.len(),
            self.state.eff_index.is_some(),
            self.state.ptcl.is_some());
        if let (Some(eff_index), Some(ptcl)) = (&self.state.eff_index, &self.state.ptcl) {
            for ec in &self.state.effects {
                let name_lower = ec.effect_name.to_lowercase();

                // Determine if this effect should be a trail ribbon.
                // Trail effects are ones that follow a bone continuously and look like
                // a swept surface — sword slashes, energy arcs, after-images, etc.
                let is_trail = ec.follows_bone && (
                    name_lower.contains("sword") ||
                    name_lower.contains("trail") ||
                    name_lower.contains("after") ||
                    name_lower.contains("tex_") ||
                    name_lower.contains("katana") ||
                    name_lower.contains("blade") ||
                    name_lower.contains("slash") ||
                    name_lower.contains("arc") ||
                    name_lower.contains("swing") ||
                    name_lower.contains("energy") ||
                    name_lower.contains("aura") ||
                    name_lower.contains("ribbon")
                );

                if is_trail {
                    // Look up the emitter color from ptcl
                    let (color, blend) = eff_index.handles.get(&ec.effect_name)
                        .or_else(|| eff_index.handles.get(&name_lower))
                        .and_then(|&idx| if idx >= 0 { ptcl.emitter_sets.get(idx as usize) } else { None })
                        .and_then(|set| set.emitters.first())
                        .map(|emitter| {
                            let c = crate::effects::sample_color_pub(&emitter.color0, 0.0);
                            ([c[0], c[1], c[2], c[3]], emitter.blend_type)
                        })
                        .unwrap_or(([1.0, 1.0, 1.0, 1.0], crate::effects::BlendType::Add));

                    // Find tip bone: prefer a "top" or "end" variant of the attach bone,
                    // or a weapon tip bone if available.
                    let bone_lower = ec.bone_name.to_lowercase();
                    let tip_bone = self.bone_names.iter()
                        .find(|b| {
                            let bl = b.to_lowercase();
                            (bl.contains("top") || bl.contains("tip") || bl.contains("end"))
                                && (bl.contains(&bone_lower) || bone_lower.contains(&bl))
                        })
                        .cloned()
                        .unwrap_or_else(|| ec.bone_name.clone());

                    self.state.trail_system.start_trail(
                        &ec.effect_name, &tip_bone, &ec.bone_name, color, blend,
                    );
                } else {
                    self.state.particle_system.spawn_effect(
                        &ec.effect_name, &ec.bone_name,
                        glam::Vec3::from(ec.offset),
                        ec.active_start as f32, ec.active_end as f32,
                        eff_index, ptcl,
                    );
                }
            }
        }
    }

    fn draw_edit_log_window(&mut self, ctx: &egui::Context) {
        // Collect pending actions to avoid borrow conflicts
        let mut remove_move: Option<(String, String)> = None;
        let mut remove_fighter: Option<String> = None;
        let mut export_move: Option<(String, String)> = None;

        let mut open = self.show_edit_log;
        egui::Window::new("Edit Log")
            .open(&mut open)
            .resizable(true)
            .default_size([420.0, 480.0])
            .show(ctx, |ui| {
                if self.state.edit_log.is_empty() {
                    ui.label(egui::RichText::new("No edits recorded yet.")
                        .color(egui::Color32::GRAY));
                    return;
                }

                ui.label(egui::RichText::new(
                    "Edits are saved automatically. Use × to discard before exporting."
                ).small().color(egui::Color32::GRAY));
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (fighter_name, fighter_display) in self.state.edit_log.fighters_sorted() {
                        let move_names = self.state.edit_log.moves_for(&fighter_name);

                        // Fighter row
                        ui.horizontal(|ui| {
                            ui.strong(&fighter_display);
                            ui.label(egui::RichText::new(
                                format!("({} move{})", move_names.len(), if move_names.len() == 1 { "" } else { "s" })
                            ).small().color(egui::Color32::GRAY));
                            if ui.small_button("× all").on_hover_text("Remove all edits for this fighter").clicked() {
                                remove_fighter = Some(fighter_name.clone());
                            }
                        });

                        // Move rows
                        for move_name in &move_names {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                // Highlight if this is the currently loaded move
                                let is_active = self.state.selected_fighter
                                    .and_then(|i| self.state.fighters.get(i))
                                    .map(|f| f.name == fighter_name)
                                    .unwrap_or(false)
                                    && self.state.selected_move.as_ref()
                                        .map(|m| &m.name == move_name)
                                        .unwrap_or(false);

                                let label = if is_active {
                                    egui::RichText::new(format!("▶ {}", move_name))
                                        .color(egui::Color32::from_rgb(100, 200, 255))
                                } else {
                                    egui::RichText::new(format!("  {}", move_name))
                                };
                                ui.label(label);

                                // Hitbox count badge
                                if let Some(record) = self.state.edit_log.entries
                                    .get(&fighter_name)
                                    .and_then(|m| m.get(move_name))
                                {
                                    ui.label(egui::RichText::new(
                                        format!("{} hb", record.hitboxes.len())
                                    ).small().color(egui::Color32::GRAY));
                                }

                                if ui.small_button("Export")
                                    .on_hover_text("Export this move as smashline source")
                                    .clicked()
                                {
                                    export_move = Some((fighter_name.clone(), move_name.clone()));
                                }
                                if ui.small_button("×")
                                    .on_hover_text("Remove this edit from the log")
                                    .clicked()
                                {
                                    remove_move = Some((fighter_name.clone(), move_name.clone()));
                                }
                            });
                        }
                        ui.add_space(4.0);
                    }
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Export All").on_hover_text("Export every logged edit to a folder").clicked() {
                        self.export_all_edits();
                    }
                });
            });

        self.show_edit_log = open;

        // Apply deferred actions
        if let Some((f, m)) = remove_move {
            self.state.edit_log.remove_move(&f, &m);
        }
        if let Some(f) = remove_fighter {
            self.state.edit_log.remove_fighter(&f);
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

    /// Snapshot the current hitboxes/script into the edit log for the active fighter+move.
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
        let script = rebuild_script_from_hitboxes(&self.state.script, &self.state.hitboxes);
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
            let moves: Vec<MoveEntry> = self.move_list.iter()
                .filter(|m| move_query.is_empty() || m.name.to_lowercase().contains(&move_query)
                    || format_move_name(&m.name).to_lowercase().contains(&move_query))
                .cloned()
                .collect();
            for m in moves {
                let selected = self.state.selected_move.as_ref()
                    .map(|sm| sm.hash == m.hash)
                    .unwrap_or(false);
                let label = format!("{} ({}f)", format_move_name(&m.name), m.frame_count);
                if ui.selectable_label(selected, &label).clicked() && !selected {
                    self.select_move(m);
                }
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
                let color = hitbox_color(hb.hitbox_type);
                let selected = self.selected_hitbox == Some(i);
                ui.horizontal(|ui| {
                    ui.colored_label(color, "*");
                    let shape = if hb.capsule_end.is_some() { "⬭" } else { "●" };
                    let angle_label = angle_short_label(hb.angle);
                    let label = format!("{} #{} {} {:.1}dmg {} [{}-{}]",
                        shape, hb.id, hb.bone_name, hb.damage, angle_label, hb.active_start, hb.active_end);
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

        // Property editor for selected hitbox
        if let Some(idx) = self.selected_hitbox {
            if let Some(hb) = self.state.hitboxes.get_mut(idx) {
                ui.separator();
                ui.heading("Properties");
                ScrollArea::vertical().id_salt("props").show(ui, |ui| {
                    // ── Core ─────────────────────────────────────────────
                    ui.horizontal(|ui| {
                        ui.label("Bone:");
                        if self.bone_names.is_empty() {
                            ui.text_edit_singleline(&mut hb.bone_name);
                        } else {
                            egui::ComboBox::from_id_salt("edit_bone_select")
                                .selected_text(&hb.bone_name)
                                .show_ui(ui, |ui| {
                                    for name in &self.bone_names.clone() {
                                        ui.selectable_value(&mut hb.bone_name, name.clone(), name);
                                    }
                                });
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("ID:");
                        ui.add(egui::DragValue::new(&mut hb.id));
                        ui.label("Part:");
                        ui.add(egui::DragValue::new(&mut hb.part));
                    });
                    ui.add(egui::Slider::new(&mut hb.damage, 0.0..=50.0).text("Damage"));
                    angle_picker(ui, &mut hb.angle);
                    ui.add(egui::Slider::new(&mut hb.kb_base, 0..=200).text("KB Base"));
                    ui.add(egui::Slider::new(&mut hb.kb_scaling, 0..=200).text("KB Scaling"));
                    ui.add(egui::Slider::new(&mut hb.fkb, 0..=200).text("Fixed KB"));
                    ui.add(egui::Slider::new(&mut hb.size, 0.1..=20.0).text("Size"));

                    // ── Position / Shape ─────────────────────────────────
                    ui.collapsing("Position / Shape", |ui| {
                        ui.add(egui::Slider::new(&mut hb.offset_x, -20.0..=20.0).text("Offset X"));
                        ui.add(egui::Slider::new(&mut hb.offset_y, -20.0..=20.0).text("Offset Y"));
                        ui.add(egui::Slider::new(&mut hb.offset_z, -20.0..=20.0).text("Offset Z"));
                        let is_capsule = hb.capsule_end.is_some();
                        let mut toggle = is_capsule;
                        ui.checkbox(&mut toggle, "Capsule (second endpoint)");
                        if toggle && !is_capsule {
                            hb.capsule_end = Some([hb.offset_x, hb.offset_y, hb.offset_z]);
                        } else if !toggle && is_capsule {
                            hb.capsule_end = None;
                        }
                        if let Some(ref mut end) = hb.capsule_end {
                            ui.add(egui::Slider::new(&mut end[0], -20.0..=20.0).text("End X"));
                            ui.add(egui::Slider::new(&mut end[1], -20.0..=20.0).text("End Y"));
                            ui.add(egui::Slider::new(&mut end[2], -20.0..=20.0).text("End Z"));
                        }
                    });

                    // ── Hit Properties ───────────────────────────────────
                    ui.collapsing("Hit Properties", |ui| {
                        ui.add(egui::Slider::new(&mut hb.hitlag_mult, 0.0..=5.0).text("Hitlag Mult"));
                        ui.add(egui::Slider::new(&mut hb.sdi_mult, 0.0..=5.0).text("SDI Mult"));
                        ui.add(egui::Slider::new(&mut hb.hitbox_attr, -10.0..=10.0).text("Hitbox Attr"));
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

                    // ── Collision Masks ──────────────────────────────────
                    ui.collapsing("Collision Masks", |ui| {
                        situation_mask_combo(ui, &mut hb.situation_mask, "sit_mask");
                        category_mask_combo(ui, &mut hb.category_mask, "cat_mask");
                        part_mask_combo(ui, &mut hb.part_mask, "part_mask");
                    });

                    // ── Effect / Sound ───────────────────────────────────
                    ui.collapsing("Effect / Sound", |ui| {
                        collision_attr_combo(ui, &mut hb.collision_attr, "col_attr");
                        sound_level_combo(ui, &mut hb.sound_level, "snd_lvl");
                        sound_attr_combo(ui, &mut hb.sound_attr, "snd_attr");
                        attack_region_combo(ui, &mut hb.attack_region, "atk_region");
                    });

                    // ── Timeline ─────────────────────────────────────────
                    let max_frame = self.state.total_frames.saturating_sub(1);
                    ui.add(egui::Slider::new(&mut hb.active_start, 0..=max_frame).text("Start Frame"));
                    ui.add(egui::Slider::new(&mut hb.active_end, 0..=max_frame).text("End Frame"));
                });
            }
        }
    }

    fn draw_effects_panel(&mut self, ui: &mut Ui) {
        let current = self.state.current_frame;

        ui.horizontal(|ui| {
            ui.heading("Effects");
            ui.label(egui::RichText::new(format!("— Frame {}", current))
                .color(egui::Color32::LIGHT_GRAY));
        });
        ui.separator();

        let has_effect_data = !self.state.effect_script.stmts.is_empty();

        if !has_effect_data {
            ui.colored_label(egui::Color32::GRAY, "Effect data unavailable");
            ui.label(egui::RichText::new("Fetch ACMD to load effect data.")
                .small()
                .color(egui::Color32::DARK_GRAY));
        } else {
            let active_effects: Vec<&crate::data::EffectCall> = self.state.effects.iter()
                .filter(|e| current >= e.active_start && current <= e.active_end)
                .collect();

            if active_effects.is_empty() {
                ui.colored_label(egui::Color32::GRAY, "No effects on this frame");
            } else {
                egui::ScrollArea::vertical().id_salt("effects_list").max_height(200.0).show(ui, |ui| {
                    for effect in &active_effects {
                        ui.horizontal(|ui| {
                            // Colored dot: orange for follows_bone, yellow for one-shot
                            let dot_color = if effect.follows_bone {
                                egui::Color32::from_rgb(255, 165, 0)
                            } else {
                                egui::Color32::from_rgb(255, 220, 0)
                            };
                            ui.colored_label(dot_color, "●");
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&effect.effect_name)
                                    .monospace()
                                    .color(egui::Color32::WHITE));
                                ui.label(egui::RichText::new(format!(
                                    "bone: {}  [{:.2}, {:.2}, {:.2}]",
                                    effect.bone_name,
                                    effect.offset[0], effect.offset[1], effect.offset[2]
                                )).small().color(egui::Color32::LIGHT_GRAY));
                            });
                        });
                        ui.add_space(2.0);
                    }
                });
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

    fn draw_scrubber(&mut self, ui: &mut Ui) {
        if self.state.total_frames == 0 { return; }

        let total = self.state.total_frames;
        let current = self.state.current_frame;

        // Playback controls
        ui.horizontal(|ui| {
            let play_label = if self.state.playing { "⏸" } else { "▶" };
            if ui.button(play_label).clicked() {
                self.state.playing = !self.state.playing;
            }
            if ui.button("|◀").clicked() {
                self.state.current_frame = 0;
                self.state.playing = false;
            }
            ui.label(format!("Frame {} / {}", current + 1, total));
        });

        // Timeline
        let timeline_height = if self.state.hitboxes.is_empty() { 24.0 } else {
            24.0 + self.state.hitboxes.len() as f32 * 16.0
        };
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
            let color = hitbox_color(hb.hitbox_type);
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

        // Effect event tick marks — orange ticks below the scrubber bar
        if !self.state.effects.is_empty() {
            let effect_frames: std::collections::HashSet<u32> = self.state.effects.iter()
                .map(|e| e.active_start)
                .collect();
            for f in effect_frames {
                if f < total {
                    let x = frame_to_x(f);
                    painter.line_segment(
                        [egui::pos2(x, rect.bottom() - 6.0), egui::pos2(x, rect.bottom())],
                        egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 165, 0)),
                    );
                }
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
}

impl eframe::App for HitboxEditorApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
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
                        let weapon_count = rs.weapon_skel_count();
                        if weapon_count > 0 {
                            self.state.status = format!("Model loaded ({} weapon skeleton{})",
                                weapon_count, if weapon_count == 1 { "" } else { "s" });
                        }
                        // bone_names already populated from skel file in select_fighter — don't overwrite
                    }
                } else {
                    self.state.status = "GPU lacks required features for 3D rendering (missing BC texture compression or similar).".to_string();
                }
            }
        }

        // Upload particle textures to GPU when a new ptcl file has been loaded
        if self.state.pending_texture_upload {
            if let Some(ptcl) = &self.state.ptcl {
                if let Some(wgpu_state) = frame.wgpu_render_state() {
                    let mut renderer = wgpu_state.renderer.write();
                    if let Some(rs) = renderer.callback_resources.get_mut::<HitboxRenderState>() {
                        if let Some(pr) = rs.particle_renderer.as_mut() {
                            pr.upload_textures(&wgpu_state.device, &wgpu_state.queue, ptcl);
                            pr.upload_meshes(&wgpu_state.device, ptcl);
                            eprintln!("[TEX] texture upload complete");
                        }
                    }
                }
                self.state.pending_texture_upload = false;
            }
        }

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

        // Step particle simulation and trail recording each frame
        if self.state.ptcl.is_some() {
            eprintln!("[SIM] ptcl present, active_emitters={} particles={} current_frame={}", 
                self.state.particle_system.active_emitters.len(),
                self.state.particle_system.particles.len(),
                self.state.current_frame);
            // Get bone matrices from the render state
            let bone_matrices = if let Some(wgpu_state) = frame.wgpu_render_state() {
                let renderer = wgpu_state.renderer.read();
                renderer.callback_resources.get::<crate::renderer::HitboxRenderState>()
                    .map(|rs| rs.bone_world_matrices())
                    .unwrap_or_default()
            } else {
                std::collections::HashMap::new()
            };

            let current_frame = self.state.current_frame;

            // Only advance simulation when the frame actually changes
            if current_frame != self.last_simulated_frame {
                if self.last_simulated_frame != u32::MAX && current_frame < self.last_simulated_frame {
                    // Scrubbing backwards (or looping) — reset, re-spawn, re-simulate from frame 0
                    self.state.particle_system.reset();
                    self.state.trail_system.reset();
                    // Re-spawn all effects so emitters are present for re-simulation
                    if let (Some(eff_index), Some(ptcl)) = (&self.state.eff_index.clone(), &self.state.ptcl.clone()) {
                        for ec in &self.state.effects.clone() {
                            let name_lower = ec.effect_name.to_lowercase();
                            let is_trail = ec.follows_bone && (
                                name_lower.contains("sword") || name_lower.contains("trail") ||
                                name_lower.contains("after") || name_lower.contains("tex_") ||
                                name_lower.contains("katana") || name_lower.contains("blade") ||
                                name_lower.contains("slash") || name_lower.contains("arc") ||
                                name_lower.contains("swing") || name_lower.contains("energy") ||
                                name_lower.contains("aura") || name_lower.contains("ribbon")
                            );
                            if is_trail {
                                let (color, blend) = eff_index.handles.get(&ec.effect_name)
                                    .or_else(|| eff_index.handles.get(&name_lower))
                                    .and_then(|&idx| if idx >= 0 { ptcl.emitter_sets.get(idx as usize) } else { None })
                                    .and_then(|set| set.emitters.first())
                                    .map(|emitter| {
                                        let c = crate::effects::sample_color_pub(&emitter.color0, 0.0);
                                        (c, emitter.blend_type)
                                    })
                                    .unwrap_or(([1.0, 1.0, 1.0, 1.0], crate::effects::BlendType::Add));
                                let tip_bone = self.bone_names.iter()
                                    .find(|b| {
                                        let bl = b.to_lowercase();
                                        (bl.contains("top") || bl.contains("tip") || bl.contains("end"))
                                            && (bl.contains(&name_lower) || name_lower.contains(&bl))
                                    })
                                    .cloned()
                                    .unwrap_or_else(|| ec.bone_name.clone());
                                self.state.trail_system.start_trail(&ec.effect_name, &tip_bone, &ec.bone_name, color, blend);
                            } else {
                                self.state.particle_system.spawn_effect(
                                    &ec.effect_name, &ec.bone_name,
                                    glam::Vec3::from(ec.offset),
                                    ec.active_start as f32, ec.active_end as f32,
                                    eff_index, ptcl,
                                );
                            }
                        }
                    }
                    if let Some(ptcl) = &self.state.ptcl.clone() {
                        for f in 0..=current_frame {
                            self.state.particle_system.step(f as f32, &bone_matrices, ptcl);
                            self.state.trail_system.step(&bone_matrices);
                        }
                    }
                } else {
                    // Normal forward step — step each frame individually so dt=1 always
                    if let Some(ptcl) = &self.state.ptcl.clone() {
                        let from = if self.last_simulated_frame == u32::MAX {
                            current_frame
                        } else {
                            self.last_simulated_frame + 1
                        };
                        for f in from..=current_frame {
                            self.state.particle_system.step(f as f32, &bone_matrices, ptcl);
                        }
                    }
                    self.state.trail_system.step(&bone_matrices);
                }

                self.last_simulated_frame = current_frame;
            }
        }

        // Edit log window
        if self.show_edit_log {
            self.draw_edit_log_window(ctx);
        }

        // Top menu bar
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("SSBU Hitbox Editor").size(16.0).color(egui::Color32::WHITE));
                ui.separator();
                if ui.button(egui::RichText::new("Open Data Root").color(egui::Color32::WHITE)).clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.set_data_root(path);
                    }
                }
                ui.separator();
                let debug_label = if self.show_debug { "Debug ✓" } else { "Debug" };
                if ui.button(debug_label).clicked() {
                    self.show_debug = !self.show_debug;
                }
                ui.separator();
                let can_export = self.state.selected_fighter.is_some()
                    && self.state.selected_move.is_some()
                    && !self.state.script.stmts.is_empty();
                if ui.add_enabled(can_export, egui::Button::new("Export Source"))
                    .on_hover_text("Export edited hitboxes as smashline Rust source code")
                    .clicked()
                {
                    self.export_acmd_source();
                }
                ui.separator();
                let log_label = if self.show_edit_log { "Edit Log ✓" } else { "Edit Log" };
                let log_btn = ui.add_enabled(
                    !self.state.edit_log.is_empty() || self.show_edit_log,
                    egui::Button::new(log_label),
                ).on_hover_text("View and manage all saved edits");
                if log_btn.clicked() {
                    self.show_edit_log = !self.show_edit_log;
                }
                ui.separator();
                let effects_label = if self.state.show_effects_panel { "Effects ✓" } else { "Effects" };
                if ui.button(effects_label).on_hover_text("Toggle effects panel").clicked() {
                    self.state.show_effects_panel = !self.state.show_effects_panel;
                }
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
                if n_particles > 0 || n_trails > 0 {
                    eprintln!("[CB] passing {} particles, {} trails to ViewportCallback", n_particles, n_trails);
                    if n_particles > 0 {
                        let p = &self.state.particle_system.particles[0];
                        eprintln!("[CB] particle[0] pos={:?} size={} color={:?}", p.position, p.size, p.color);
                    }
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

                            let color = hitbox_color(hb.hitbox_type);
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
