use crate::data::{fighter_display_name, AppState, Hitbox, MoveEntry};
use crate::renderer::{HitboxRenderState, ViewportCallback};
use egui::{Color32, RichText, ScrollArea, Ui};
/// Main egui application for Visionary.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Whether the game has reached the terminal state requested by a carrier send.
///
/// A populated snapshot needs a newly disk-loaded, spawned carrier. An empty snapshot is a
/// teardown command, and is acknowledged by a fresh status report showing the idle state.
/// Keeping this distinction in one pure predicate makes it difficult for the status text and
/// the timeout path to disagree about whether a restore actually succeeded.
fn carrier_send_reached_goal(
    carrier_expected: bool,
    state: u8,
    kinds: usize,
    spawned: bool,
    took_our_bytes: bool,
    fresh_report: bool,
) -> bool {
    if carrier_expected {
        state == 2 && spawned && took_our_bytes
    } else {
        state == 0 && kinds == 0 && !spawned && fresh_report
    }
}

/// The editor, ACMD source, exports, and user-facing timeline all use one-based game frames.
const FIRST_GAME_FRAME: u32 = 1;

/// Convert a game-frame playhead to the zero-based frame index stored in `.nuanmb` animations.
fn animation_frame_for_game_frame(frame: u32) -> f32 {
    crate::data::script_to_motion_frame(frame)
}

/// Left edge of a one-based game frame in a timeline containing `total` frame cells.
fn timeline_frame_start_fraction(frame: u32, total: u32) -> f32 {
    if total == 0 {
        return 0.0;
    }
    frame.saturating_sub(FIRST_GAME_FRAME).min(total) as f32 / total as f32
}

/// Right edge of an inclusive one-based game frame range.
fn timeline_frame_end_fraction(frame: u32, total: u32) -> f32 {
    if total == 0 {
        return 0.0;
    }
    frame.min(total) as f32 / total as f32
}

/// Resolve a pointer position to a valid one-based game frame, including the far-right edge.
fn timeline_frame_at_fraction(fraction: f32, total: u32) -> u32 {
    if total == 0 {
        return FIRST_GAME_FRAME;
    }
    ((fraction.clamp(0.0, 1.0) * total as f32).floor() as u32 + FIRST_GAME_FRAME).min(total)
}

const TIMELINE_ROW_HEIGHT: f32 = 12.0;

fn timeline_content_height(hitboxes: usize, effects: usize) -> f32 {
    let hitbox_band = hitboxes as f32 * TIMELINE_ROW_HEIGHT;
    let effect_height = (TIMELINE_ROW_HEIGHT * 0.45).max(3.0);
    let effect_band = if effects == 0 {
        0.0
    } else {
        4.0 + effects as f32 * effect_height
    };
    (24.0 + hitbox_band + effect_band).max(24.0)
}

fn timeline_frame_extent(
    hitboxes: &[crate::data::Hitbox],
    effects: &[crate::data::EffectCall],
) -> u32 {
    let hitbox_frames = hitboxes.iter().flat_map(|hitbox| {
        [
            Some(hitbox.active_start),
            (hitbox.active_end < 9999).then_some(hitbox.active_end),
        ]
        .into_iter()
        .flatten()
    });
    let effect_frames = effects.iter().flat_map(|effect| {
        [
            Some(effect.active_start),
            (effect.active_end < 9999).then_some(effect.active_end),
        ]
        .into_iter()
        .flatten()
    });
    hitbox_frames.chain(effect_frames).max().unwrap_or(0)
}

fn timeline_scroll_area(viewport_height: f32) -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .id_salt("hitbox_timeline_scroll")
        .auto_shrink([false, false])
        .max_height(viewport_height.max(1.0))
}

fn hitbox_menu_scroll_area(viewport_height: f32) -> egui::ScrollArea {
    let viewport_height = viewport_height.max(1.0);
    egui::ScrollArea::vertical()
        .id_salt("hitbox_menu_scroll")
        .auto_shrink([false, false])
        .min_scrolled_height(viewport_height)
        .max_height(viewport_height)
}

/// AREA_WIND uses the gameplay plane's horizontal/vertical coordinates. Model space represents
/// that plane as Z/Y (the same convention used by `top`-bone ACMD offsets), while model X is
/// depth. Mapping wind X directly to model X makes the area appear edge-on on the wrong plane.
fn wind_area_world_point(root: glam::Mat4, horizontal: f32, vertical: f32) -> glam::Vec3 {
    root.transform_point3(glam::Vec3::new(0.0, vertical, horizontal))
}

// ── Enum combo helpers ────────────────────────────────────────────────────────

fn enum_combo(ui: &mut egui::Ui, value: &mut String, id: &str, label: &str, options: &[&str]) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(value.as_str())
            .show_ui(ui, |ui| {
                // A current value the tables cannot name (a raw number from an exotic
                // capture) is offered first, so the list ALWAYS contains — and highlights —
                // what is actually active instead of showing nothing as selected.
                if !value.is_empty() && !options.contains(&value.as_str()) {
                    let current = value.clone();
                    ui.selectable_value(value, current.clone(), current.as_str());
                    ui.separator();
                }
                for &opt in options {
                    ui.selectable_value(value, opt.to_string(), opt);
                }
            });
    });
}

/// Dropdown backed by a lua-const table: the offered names are exactly the ones the
/// fetch path decodes into, so a fetched value always matches an entry.
fn const_combo(
    ui: &mut egui::Ui,
    value: &mut String,
    id: &str,
    label: &str,
    table: crate::param_labels::ConstTable,
) {
    let names: Vec<&'static str> = table.iter().map(|(n, _)| *n).collect();
    enum_combo(ui, value, id, label, &names);
}

fn setoff_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(ui, v, id, "Setoff Kind:", crate::param_labels::SETOFF_KIND);
}

fn lr_check_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(ui, v, id, "LR Check:", crate::param_labels::LR_CHECK);
}

fn situation_mask_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(
        ui,
        v,
        id,
        "Situation Mask:",
        crate::param_labels::SITUATION_MASK,
    );
}

fn category_mask_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(
        ui,
        v,
        id,
        "Category Mask:",
        crate::param_labels::CATEGORY_MASK,
    );
}

fn part_mask_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(ui, v, id, "Part Mask:", crate::param_labels::PART_MASK);
}

/// collision_attr is a hash40 (not an int), so it gets a plain name list.
fn collision_attr_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    enum_combo(
        ui,
        v,
        id,
        "Collision Attr:",
        crate::param_labels::COLLISION_ATTRS,
    );
}

fn sound_level_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(ui, v, id, "Sound Level:", crate::param_labels::SOUND_LEVEL);
}

fn sound_attr_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(ui, v, id, "Sound Attr:", crate::param_labels::SOUND_ATTR);
}

fn attack_region_combo(ui: &mut egui::Ui, v: &mut String, id: &str) {
    const_combo(
        ui,
        v,
        id,
        "Attack Region:",
        crate::param_labels::ATTACK_REGION,
    );
}

/// Special angles used in SSBU hitboxes.
/// Values 365-368 are autolink angles; 361 is the Sakurai angle.
/// Note: 366 and 367 swapped roles between Smash 4 and Ultimate.
const SPECIAL_ANGLES: &[(&str, i32)] = &[
    ("Sakurai (361)", 361), // horizontal at low KB, diagonal at high KB
    ("Autolink 363", 363),  // matches attacker movement, no launch speed mod
    ("Autolink 365", 365),  // matches attacker movement, 50% speed
    ("Autolink 366", 366),  // pull + momentum, no speed cap (less common)
    ("Autolink 367", 367),  // pull + momentum, speed capped — most common in Ultimate multi-hits
    ("Autolink 368", 368),  // pull + position vector (e.g. Samus up smash)
];

/// Direction shown by the 3D viewport for an ATTACK angle.
///
/// Hitbox offsets follow their animated bone, but knockback angles do not inherit that bone's
/// rotation. The gameplay plane is world Z/Y in the model viewer: 0 degrees points along +Z and
/// 90 degrees points along +Y. Special angles are state-dependent in game, so their viewport line
/// uses a representative horizontal direction.
fn hitbox_launch_direction(angle: i32) -> (glam::Vec3, bool) {
    let dynamic = SPECIAL_ANGLES.iter().any(|&(_, value)| value == angle);
    let display_angle = if dynamic { 0 } else { angle.rem_euclid(360) };
    let radians = (display_angle as f32).to_radians();
    (glam::Vec3::new(0.0, radians.sin(), radians.cos()), dynamic)
}

fn lighten_hitbox_color(color: egui::Color32) -> egui::Color32 {
    const WHITE_BLEND: f32 = 0.32;
    let lighten =
        |channel: u8| (channel as f32 + (255.0 - channel as f32) * WHITE_BLEND).round() as u8;
    egui::Color32::from_rgba_unmultiplied(
        lighten(color.r()),
        lighten(color.g()),
        lighten(color.b()),
        color.a(),
    )
}

/// Short angle label for the hitbox list.
fn angle_short_label(angle: i32) -> String {
    match angle {
        361 => "Sakurai".to_string(),
        363 => "AL:363".to_string(),
        365 => "AL:365".to_string(),
        366 => "AL:366".to_string(),
        367 => "AL:367".to_string(),
        368 => "AL:368".to_string(),
        a => format!("{}°", a),
    }
}

/// Draw an angle picker: a special-angle dropdown + a circular drag widget.
/// Smash Ultimate angle convention: 0=right, 90=up, 180=left, 270=down.
fn angle_picker(ui: &mut egui::Ui, angle: &mut i32) {
    let special_label = SPECIAL_ANGLES
        .iter()
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
                ui.memory_mut(|m| {
                    m.toggle_popup(popup_id);
                });
            }
        }
        #[allow(deprecated)]
        egui::popup_below_widget(
            ui,
            popup_id,
            &btn,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(160.0);
                if ui
                    .selectable_label(special_label == "Custom", "Custom (0°)")
                    .clicked()
                {
                    *angle = 0;
                    ui.memory_mut(|m| {
                        m.close_popup(popup_id);
                    });
                }
                for &(name, val) in SPECIAL_ANGLES {
                    if ui.selectable_label(*angle == val, name).clicked() {
                        *angle = val;
                        ui.memory_mut(|m| m.close_popup(popup_id));
                    }
                }
            },
        );
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
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(1.5, egui::Color32::from_gray(80)),
    );

    // Cardinal tick marks at 0/90/180/270 and diagonals
    for deg in [0u32, 45, 90, 135, 180, 225, 270, 315] {
        // smash angle → screen direction: x=cos(a), y=-sin(a) (flip Y for screen)
        let rad = (deg as f32).to_radians();
        let dir = egui::vec2(rad.cos(), -rad.sin());
        let tick = if deg % 90 == 0 { 6.0 } else { 3.0 };
        let outer = center + dir * radius;
        let inner = center + dir * (radius - tick);
        painter.line_segment(
            [inner, outer],
            egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
        );
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
        SPECIAL_ANGLES
            .iter()
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
            _ => "",
        };
        if !desc.is_empty() {
            ui.label(
                egui::RichText::new(desc)
                    .small()
                    .color(egui::Color32::from_rgb(180, 180, 60)),
            );
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

/// The name a transplant's entry is stored under INSIDE the live carrier.
///
/// Copies of a FOREIGN donor collapse onto that donor's own name: several transplants of one
/// donor need only one stored kind, and each copy's alias points at it.
///
/// A copy of the carrier fighter's OWN entry must not collapse, because the donor name is
/// already a live kind in that fighter's resident eff — storing the carrier's copy under it
/// would give two different emitter sets the same hash40. Those keep their distinct name
/// (`kirby_dash_tp`, or the reserved `vsnedit_` clone name for an authored edit) instead.
///
/// `own_prefix` is `effect/fighter/<carrier fighter>/`, or None when no fighter is selected.
///
/// Shared by the carrier build and the authored-edit pass on purpose: when those two disagreed
/// about a name, an edit was attached to an entry the carrier never created, which failed the
/// build and silently took every unrelated transplant with it.
/// Does a transplant with this donor path need its content carried in the live carrier?
///
/// Everything with a real donor does, including `effect/system/` (`ef_common`, `ef_item`).
///
/// System donors were excluded for a long time, and the nearby comment explained it as "those
/// effs are always resident". That is true, and it does mean their BYTES need not be co-loaded —
/// but it was also read as "so the carrier need not hold them", which does not follow. A
/// transplant introduces a new entry name that exists in no loaded eff, so nothing can serve it;
/// the snapshot then came out empty and the deploy fell through to the merged-fighter-eff path,
/// which is the mechanism this design exists to avoid. That is the same defect the own-eff
/// exclusion had, for the same reason, and it is why this filter is otherwise unconditional.
///
/// What actually made system transplants destructive was never the carrying — it was the NAME
/// they were carried under. See `carrier_stored_name`: a copy stored as `sys_bomb_b` shadows
/// ef_common's resident kind. Fixing the name is what makes this safe; excluding the donor only
/// traded one failure for another.
///
/// Callers must SAY when they drop an op. Dropping one silently is what produced the original
/// report: no carrier op meant no carrier, no spec, and an editor waiting forever on a carrier
/// generation that was never coming.
fn rides_the_carrier(src_file_rel: &str) -> bool {
    !src_file_rel.is_empty()
}

fn carrier_stored_name(op: &crate::mod_project::TransplantOp, own_prefix: Option<&str>) -> String {
    let donor_rel = op.src_file_rel.to_lowercase();
    let donor_name = op.src_set_name.to_lowercase();
    // Reusing the donor's name is only safe when nothing else in the match answers to it. The
    // carrier's entry names become live effect KINDS, so storing a copy under a name the game
    // already has registered puts two effects on one hash.
    //
    // "The carrier fighter's own eff" was the original test for that, and it is right as far as
    // it goes — a foreign fighter or assist is not in the match, so `pickel_tnt` collides with
    // nothing. It misses the effs that are resident in EVERY match: `ef_common` and `ef_item`.
    // Storing `sys_bomb_b` in the carrier shadowed ef_common's own kind, which made the game's
    // SYS_* effects resolve to a carrier copy that was not up (they stopped rendering), and left
    // the retire waiting on a kind that can never unregister because ef_common still holds it
    // (`AUTO_CARRIER_AWAIT_KIND_UNREGISTER registered=1`, forever).
    let collides_with_a_resident_kind = own_prefix.is_some_and(|p| donor_rel.starts_with(p))
        || donor_rel.starts_with("effect/system");
    if !collides_with_a_resident_kind {
        return donor_name;
    }
    let clone = op.new_entry_name.to_lowercase();
    // A transplant onto ITSELF (replacing an entry with its own content) carries no distinct
    // name, so fall back to the reserved internal namespace, which nothing else can name.
    if clone.is_empty() || clone == donor_name {
        format!("{}{donor_name}", crate::mod_project::EDIT_CLONE_PREFIX)
    } else {
        clone
    }
}

/// ACMD effect functions whose first arguments use the common
/// `(graphic[, graphic_flip], joint, pos, rot, scale, ...)` layout.
///
/// Keeping this explicit prevents control functions such as EFFECT_OFF_KIND from being
/// misread as spawns while accounting for the alpha/attribute/random, ground-contact, and
/// no-stop variants emitted by the game's dumped scripts.
fn effect_capture_layout(func: &str) -> Option<(bool, bool)> {
    let layout = match func {
        // (flip layout, follows source bone)
        "EFFECT" | "EFFECT_ALPHA" | "EFFECT_ATTR" | "DOWN_EFFECT" | "FOOT_EFFECT"
        | "LANDING_EFFECT" => (false, false),
        "EFFECT_FLIP" | "EFFECT_FLIP_ALPHA" | "FOOT_EFFECT_FLIP" | "LANDING_EFFECT_FLIP" => {
            (true, false)
        }
        "EFFECT_FOLLOW"
        | "EFFECT_FOLLOW_ALPHA"
        | "EFFECT_FOLLOW_COLOR"
        | "EFFECT_FOLLOW_NO_SCALE"
        | "EFFECT_FOLLOW_NO_STOP"
        | "EFFECT_FLW_POS"
        | "EFFECT_FLW_POS_NO_STOP"
        | "EFFECT_FLW_POS_UNSYNC_VIS"
        | "EFFECT_FLW_UNSYNC_VIS" => (false, true),
        "EFFECT_FOLLOW_FLIP"
        | "EFFECT_FOLLOW_FLIP_ALPHA"
        | "EFFECT_FOLLOW_FLIP_COLOR"
        | "EFFECT_FOLLOW_FLIP_RND"
        | "EFFECT_FOLLOW_NO_STOP_FLIP" => (true, true),
        _ => return None,
    };
    Some(layout)
}

/// Runtime fighter kind for an extracted fighter directory name. Live ACMD capture is global;
/// filtering by this id prevents another fighter performing the same motion (for example
/// `catch_dash`) from contaminating the selected fighter's timeline.
fn fighter_kind_id(name: &str) -> Option<i32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "mario" => 0x00,
        "donkey" => 0x01,
        "link" => 0x02,
        "samus" => 0x03,
        "samusd" => 0x04,
        "yoshi" => 0x05,
        "kirby" => 0x06,
        "fox" => 0x07,
        "pikachu" => 0x08,
        "luigi" => 0x09,
        "ness" => 0x0a,
        "captain" => 0x0b,
        "purin" => 0x0c,
        "peach" => 0x0d,
        "daisy" => 0x0e,
        "koopa" => 0x0f,
        "sheik" => 0x10,
        "zelda" => 0x11,
        "mariod" => 0x12,
        "pichu" => 0x13,
        "falco" => 0x14,
        "marth" => 0x15,
        "lucina" => 0x16,
        "younglink" => 0x17,
        "ganon" => 0x18,
        "mewtwo" => 0x19,
        "roy" => 0x1a,
        "chrom" => 0x1b,
        "gamewatch" => 0x1c,
        "metaknight" => 0x1d,
        "pit" => 0x1e,
        "pitb" => 0x1f,
        "szerosuit" => 0x20,
        "wario" => 0x21,
        "snake" => 0x22,
        "ike" => 0x23,
        "pzenigame" | "zenigame" => 0x24,
        "pfushigisou" | "fushigisou" => 0x25,
        "plizardon" | "lizardon" => 0x26,
        "diddy" => 0x27,
        "lucas" => 0x28,
        "sonic" => 0x29,
        "dedede" => 0x2a,
        "pikmin" => 0x2b,
        "lucario" => 0x2c,
        "robot" => 0x2d,
        "toonlink" => 0x2e,
        "wolf" => 0x2f,
        "murabito" => 0x30,
        "rockman" => 0x31,
        "wiifit" => 0x32,
        "rosetta" => 0x33,
        "littlemac" => 0x34,
        "gekkouga" => 0x35,
        "palutena" => 0x36,
        "pacman" => 0x37,
        "reflet" => 0x38,
        "shulk" => 0x39,
        "koopajr" => 0x3a,
        "duckhunt" => 0x3b,
        "ryu" => 0x3c,
        "ken" => 0x3d,
        "cloud" => 0x3e,
        "kamui" => 0x3f,
        "bayonetta" => 0x40,
        "inkling" => 0x41,
        "ridley" => 0x42,
        "simon" => 0x43,
        "richter" => 0x44,
        "krool" => 0x45,
        "shizue" => 0x46,
        "gaogaen" => 0x47,
        "miifighter" => 0x48,
        "miiswordsman" => 0x49,
        "miigunner" => 0x4a,
        "popo" | "ice_climber" => 0x4b,
        "packun" => 0x51,
        "jack" => 0x52,
        "brave" => 0x53,
        "buddy" => 0x54,
        "dolly" => 0x55,
        "master" => 0x56,
        "tantan" => 0x57,
        "pickel" => 0x58,
        "edge" => 0x59,
        "eflame" => 0x5a,
        "elight" => 0x5b,
        "demon" => 0x5c,
        "trail" => 0x5d,
        _ => return None,
    })
}

/// Spawn identity for matching effect calls across reloads: (kind hash, frame, bone).
/// Case-insensitive via the lowercase hashes.
fn call_sig(c: &crate::data::EffectCall) -> (u64, u64, u64, u32, u64) {
    (
        effect_name_hash(&c.effect_name),
        c.effect_name_alt
            .as_deref()
            .map(effect_name_hash)
            .unwrap_or(0),
        hash40::hash40(&c.spawn_func).0,
        c.active_start,
        hash40::hash40(&c.bone_name.to_lowercase()).0,
    )
}

fn effect_call_display_name(call: &crate::data::EffectCall) -> String {
    match call
        .effect_name_alt
        .as_deref()
        .filter(|alternate| *alternate != call.effect_name)
    {
        Some(alternate) if call.effect_name == "null" => {
            format!("{alternate} (flip; other side none)")
        }
        Some(alternate) => format!("{} / {alternate}", call.effect_name),
        None => call.effect_name.clone(),
    }
}

fn is_system_bone(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
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
        1 => Color32::from_rgba_premultiplied(80, 200, 255, 180), // grab — cyan
        2 => Color32::from_rgba_premultiplied(120, 240, 140, 170), // wind — green
        _ => hitbox_color(hb.hitbox_type),
    }
}

type AcmdFetchResult = (String, String, Result<String, String>);

/// One ACMD function checked out of the linked project for editing.
///
/// Only the function's own text is held, not the whole file: saving splices it back into the
/// span it came from, so anything else in the file — other moves, imports, the install block
/// — cannot be clobbered by an editor that never showed it.
struct SourceBuffer {
    fighter: String,
    move_name: String,
    /// ACMD script name, e.g. `effect_attackairn`.
    script: String,
    file: PathBuf,
    span: std::ops::Range<usize>,
    text: String,
    /// The text as loaded, for change detection and Revert.
    original: String,
    /// The text the editor panels were last reconciled against.
    ///
    /// The two sides drive each other, so each needs to tell "I changed this" from "the
    /// other side changed this" — otherwise adopting a change looks like a new edit and the
    /// two ping-pong forever.
    synced: String,
    /// Candidate text and when it appeared, for debouncing the re-parse. Half-typed source
    /// parses to nonsense, and blanking the panels on every keystroke is unusable.
    settling: Option<(String, std::time::Instant)>,
    /// The panel state the buffer text currently reflects.
    mirrored: Option<(Vec<crate::data::Hitbox>, Vec<crate::data::EffectCall>)>,
    /// Why the last reconcile declined to write, shown under the editor.
    note: Option<String>,
}

/// Why the source pane has no script to edit. Each state gets its own answer, because
/// "nothing here" is exactly the unhelpful thing this window used to say.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoSource {
    /// No project linked at all.
    NotLinked,
    /// Linked, but it has nothing for this fighter — the usual case for a live capture.
    FighterAbsent,
    /// Linked and has the fighter, but not this move.
    MoveAbsent,
    /// The move IS in the project; the user just has not opened either script yet.
    NothingOpen,
}

/// Which panes the ACMD Source window shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceView {
    /// The user's own code, editable.
    Source,
    /// What an export would emit for this move, read-only.
    Generated,
    /// Both, for comparing them directly — the question issue #4 was really asking.
    Split,
}

impl SourceBuffer {
    fn dirty(&self) -> bool {
        self.text != self.original
    }

    /// Whether this buffer holds the `game_*` (hitbox) side of the move.
    fn is_game_script(&self) -> bool {
        self.script.starts_with("game_")
    }
}

pub struct VisionaryApp {
    /// Keeps the Wayland shared-memory icon buffer alive for the root toplevel. winit 0.30
    /// handles X11 and Windows icons itself; Wayland compositors use the native icon protocol.
    #[cfg(target_os = "linux")]
    _wayland_icon: Option<crate::wayland_icon::WaylandIcon>,
    state: AppState,
    move_list: Vec<MoveEntry>,
    fetching_acmd: bool,
    acmd_error: Option<String>,
    show_add_hitbox: bool,
    add_hitbox_category: u8,
    add_wind_radial: bool,
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
    /// In-flight "Fetch ACMD" result: `(fighter, move, body or error)`. The fetch used to run
    /// inline and blocked the UI thread for a whole GitHub round trip on every click.
    acmd_receiver: Option<std::sync::mpsc::Receiver<AcmdFetchResult>>,
    // Cached bone names for dropdown
    bone_names: Vec<String>,
    show_debug: bool,
    credits: crate::credits::CreditsWindow,
    show_edit_log: bool,
    export_dir: Option<PathBuf>,
    /// Extra roots holding modded content — added-character mods and slot-add packs. Each
    /// has the same `fighter/<name>/…` + `effect/fighter/<name>/…` layout as the data root.
    extra_roots: Vec<PathBuf>,
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
    /// Live capture that is still streaming in for the open move. Auto-adoption waits for it
    /// to settle so the adopted script isn't truncated mid-move (see `PendingCapture`).
    pending_capture: Option<PendingCapture>,
    /// Transplant studio: entry pool across every eff under the export root.
    effect_pool: Option<crate::effect_pool::EffectPool>,
    show_transplant: bool,
    transplant_search: String,
    /// Search text for the Effects-panel effect-name picker (live kinds + pool).
    effect_pick_search: String,
    /// Whether the inline effect-name picker (next to the effect field) is expanded.
    effect_pick_open: bool,
    /// Selected donor: (file rel, entry name).
    transplant_sel: Option<(String, String)>,
    transplant_new_name: String,
    /// Target fighter override for the studio (None = the currently selected fighter).
    transplant_target: Option<String>,
    /// ONE-SLOT scoping for the transplant being staged: the costume slots it applies to.
    /// EMPTY = every costume (the transplant lands in the base eff file). This is the one
    /// piece of studio state that is genuinely about one-slotting rather than transplanting.
    /// A set, not a bitmask: slot indices are not bounded by the vanilla 8 and mods use
    /// large ones, so there is no width to fit them all into.
    one_slot_slots: std::collections::BTreeSet<u8>,
    /// One-slot mode only: which existing target entry the donor replaces in place.
    transplant_replace: Option<String>,
    transplant_replace_search: String,
    /// After a transplant: uses of the donor effect offered for per-use redirect.
    redirect_prompt: Option<RedirectPrompt>,
    /// Fighters whose merged eff is live-served to the running game (Eden SD +
    /// arcropolis callback) — their cross-fighter aliases are dropped so the REAL
    /// entry (loaded on match re-entry) isn't masked.
    live_eff_deployed: std::collections::HashSet<String>,
    /// Fighters whose merged-preview build failed this session (retried after the next
    /// transplant record instead of every frame; shown as ⚠ in the eff editor).
    merged_build_failed: std::collections::HashSet<String>,
    /// When set, send a `live_eff_probe` to the plugin at this time (a few seconds after
    /// a deploy, so the game's async re-loads have settled before the diagnosis runs).
    live_eff_probe_due: Option<std::time::Instant>,
    /// Background param-label download (ultimate-research/param-labels); None once done.
    param_labels_rx: Option<std::sync::mpsc::Receiver<crate::param_labels::Msg>>,
    /// Labels from the downloaded/cached ParamLabels.csv — re-merged after any
    /// `set_data_root` (which clears `state.labels`). Takes precedence over files
    /// found in the export folder.
    downloaded_labels: HashMap<u64, String>,
    /// Background fighter-wide ACMD scan feeding the redirect prompt with EVERY use of
    /// the donor (GitHub scripts, disk-cached) — not just moves already played/opened.
    use_scan: Option<UseScan>,
    fighter_search: String,
    move_search: String,
    /// The user's own smashline project, when linked. Scripts are read from here in
    /// preference to the dumped-script mirror — see `acmd_src`.
    acmd_src: Option<crate::acmd_src::SourceIndex>,
    show_acmd_src: bool,
    /// The script currently open in the source editor window.
    acmd_src_buffer: Option<SourceBuffer>,
    acmd_src_view: SourceView,
    /// Eff-file editor with in-game live preview (replaces RPM).
    eff_editor: crate::eff_editor::EffEditor,
    /// TCP client to the slight_replica plugin (:7878).
    game_link: crate::game_link::GameLink,
    /// Debounced live-pin push for an edited effect call (index, last edit time).
    /// Shared per-kind runtime overrides (Effects panel + Eff Editor game panel).
    live_overrides: crate::game_link::LiveOverrides,
    /// Authored .eff edits per fighter (project store; synced from the eff editor).
    eff_mods: HashMap<String, crate::mod_project::EffMod>,
    /// Call sites that asked to publish a donor/carrier snapshot this UI frame. Publication is
    /// coalesced to one per frame: several handlers can run in a single frame and the earlier
    /// ones observe half-updated project state. The editor's transplant drain, for instance,
    /// publishes before `record_transplant` has attached the op to the target fighter, so it ships
    /// an EMPTY carrier — which the plugin correctly reads as "user removed the transplant" and
    /// tears the live carrier down, only to rebuild it microseconds later. That destroy/recreate
    /// is the entire transplant delay, and recreating the same assist kind while the previous
    /// object is still dying can hang the game's loader outright.
    carrier_push_callers: Vec<String>,
    project_name: String,
    /// Kept so background workers can wake the (reactive) UI when their result lands.
    egui_ctx: egui::Context,
    /// Saved window geometry (`"main"`, `"transplant"`, `"edits"`), persisted between runs.
    window_geometry: std::collections::BTreeMap<String, WindowGeometry>,
    /// Geometry writes are debounced — this is per-frame data and the config file must not be
    /// rewritten on every mouse move.
    geometry_dirty: bool,
    geometry_saved_at: std::time::Instant,
    /// Set once the saved main-window geometry has been re-applied and clamped on screen.
    main_geometry_restored: bool,
    /// Live geometry of the transplant viewport, captured while it is open.
    transplant_geometry: Option<WindowGeometry>,
    /// Per-frame timing breakdown, active only when `VISIONARY_PROFILE` is set.
    perf: FrameProfiler,
}

/// Coarse per-section frame timing, printed periodically to stderr.
///
/// Off (and effectively free) unless `VISIONARY_PROFILE` is set in the environment. This
/// exists because the editor's frame cost is spread across a very large `ui()` and guessing
/// at the expensive part has already proven wrong once — measure, then optimise.
#[derive(Default)]
pub struct FrameProfiler {
    enabled: bool,
    /// section name -> (total ms, call count)
    totals: std::collections::BTreeMap<&'static str, (f64, u32)>,
    frames: u32,
    frame_ms: f64,
    frame_max_ms: f64,
    last_report: Option<std::time::Instant>,
    /// Sections timed in the frame currently being built, so a hitch can name its cause.
    current: Vec<(&'static str, f64)>,
}

/// A frame slower than this is a visible stutter rather than normal jitter, and gets its own
/// immediate breakdown — sustained averages hide exactly the hitches users notice.
const HITCH_MS: f64 = 12.0;

impl FrameProfiler {
    fn new() -> Self {
        Self {
            enabled: std::env::var_os("VISIONARY_PROFILE").is_some(),
            last_report: Some(std::time::Instant::now()),
            ..Default::default()
        }
    }

    /// Start a section; pair with [`FrameProfiler::end`].
    #[inline]
    fn start(&self) -> Option<std::time::Instant> {
        self.enabled.then(std::time::Instant::now)
    }

    #[inline]
    fn end(&mut self, name: &'static str, t: Option<std::time::Instant>) {
        let Some(t) = t else { return };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let e = self.totals.entry(name).or_default();
        e.0 += ms;
        e.1 += 1;
        self.current.push((name, ms));
    }

    /// Record a completed frame and print a breakdown every couple of seconds.
    fn end_frame(&mut self, t: Option<std::time::Instant>) {
        let Some(t) = t else { return };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms >= HITCH_MS {
            let mut worst = self.current.clone();
            worst.sort_by(|a, b| b.1.total_cmp(&a.1));
            worst.truncate(4);
            let named: f64 = self.current.iter().map(|(_, m)| m).sum();
            eprintln!(
                "[perf] HITCH {ms:.1} ms frame — {} | unattributed {:.1} ms",
                worst
                    .iter()
                    .filter(|(_, m)| *m > 0.5)
                    .map(|(n, m)| format!("{n} {m:.1} ms"))
                    .collect::<Vec<_>>()
                    .join(", "),
                ms - named,
            );
        }
        self.current.clear();
        self.frame_ms += ms;
        self.frame_max_ms = self.frame_max_ms.max(ms);
        self.frames += 1;
        let due = self
            .last_report
            .map(|r| r.elapsed().as_secs_f64() >= 2.0)
            .unwrap_or(false);
        if !due || self.frames == 0 {
            return;
        }
        let n = self.frames as f64;
        // Worst share of the average frame first — that is the thing worth fixing.
        let mut rows: Vec<(&'static str, f64, f64, u32)> = self
            .totals
            .iter()
            .map(|(name, (total, count))| {
                (*name, total / n, total / (*count).max(1) as f64, *count)
            })
            .collect();
        rows.sort_by(|a, b| b.1.total_cmp(&a.1));
        let lines: Vec<String> = rows
            .iter()
            .map(|(name, per_frame, per_call, count)| {
                format!(
                    "    {name:<22} {per_frame:7.3} ms/frame  ({per_call:6.3} ms/call, {count} calls)"
                )
            })
            .collect();
        eprintln!(
            "[perf] {:.1} fps | frame avg {:.3} ms, max {:.3} ms over {} frames\n{}",
            n / self
                .last_report
                .map(|r| r.elapsed().as_secs_f64())
                .unwrap_or(1.0),
            self.frame_ms / n,
            self.frame_max_ms,
            self.frames,
            lines.join("\n"),
        );
        self.totals.clear();
        self.frames = 0;
        self.frame_ms = 0.0;
        self.frame_max_ms = 0.0;
        self.last_report = Some(std::time::Instant::now());
    }
}

impl VisionaryApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        // Set dark theme with visible text
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        // Install image loaders — this also ensures font atlas is properly initialized
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // Before anything touches the SD card — `clear_stale_live_state` just below is the
        // first caller — publish the user's explicit emulator SD root, if they set one.
        // Probing finds the standard install locations; portable emulator installs keep their
        // SD next to the executable and can only be found by being told.
        crate::scratch_dirs::set_emulator_sd_override(load_config_path(SD_ROOT_CONFIG_KEY));

        // Live edits must not survive a restart — see `clear_stale_live_state`.
        Self::clear_stale_live_state();

        let saved_data_root = load_config_path("data_root");
        let saved_export_dir = load_config_path("export_dir");
        // Read before `set_data_root` runs below — that call re-points (and re-saves) the eff
        // editor's root, so a folder the user picked in that window has to be captured first.
        let saved_eff_root = load_config_path(EFF_ROOT_CONFIG_KEY);
        let saved_mod_roots = load_mod_roots();

        #[cfg(target_os = "linux")]
        let wayland_icon = cc.winit_window().and_then(|window| {
            match crate::wayland_icon::WaylandIcon::attach(
                window,
                crate::app_icon::viewport_icon().as_ref(),
            ) {
                Ok(icon) => icon,
                Err(error) => {
                    if crate::debug_enabled() {
                        eprintln!("Wayland window icon unavailable: {error:#}");
                    }
                    None
                }
            }
        });

        let mut app = Self {
            #[cfg(target_os = "linux")]
            _wayland_icon: wayland_icon,
            state: AppState::default(),
            move_list: Vec::new(),
            fetching_acmd: false,
            acmd_error: None,
            show_add_hitbox: false,
            add_hitbox_category: 0,
            add_wind_radial: false,
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
            acmd_receiver: None,
            bone_names: Vec::new(),
            show_debug: false,
            credits: crate::credits::CreditsWindow::default(),
            show_edit_log: false,
            export_dir: saved_export_dir,
            extra_roots: saved_mod_roots,
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
            pending_capture: None,
            effect_pool: None,
            show_transplant: false,
            transplant_search: String::new(),
            effect_pick_search: String::new(),
            effect_pick_open: false,
            transplant_sel: None,
            transplant_new_name: String::new(),
            transplant_target: None,
            one_slot_slots: std::collections::BTreeSet::new(),
            transplant_replace: None,
            transplant_replace_search: String::new(),
            redirect_prompt: None,
            live_eff_deployed: std::collections::HashSet::new(),
            merged_build_failed: std::collections::HashSet::new(),
            live_eff_probe_due: None,
            param_labels_rx: Some(crate::param_labels::spawn_fetch()),
            downloaded_labels: HashMap::new(),
            use_scan: None,
            fighter_search: String::new(),
            move_search: String::new(),
            acmd_src: None,
            show_acmd_src: false,
            acmd_src_buffer: None,
            acmd_src_view: SourceView::Source,
            eff_editor: crate::eff_editor::EffEditor::default(),
            game_link: crate::game_link::GameLink::default(),
            live_overrides: crate::game_link::LiveOverrides::default(),
            eff_mods: HashMap::new(),
            carrier_push_callers: Vec::new(),
            project_name: "unnamed_mod".into(),
            egui_ctx: cc.egui_ctx.clone(),
            window_geometry: load_window_geometry(),
            geometry_dirty: false,
            geometry_saved_at: std::time::Instant::now(),
            main_geometry_restored: false,
            transplant_geometry: None,
            perf: FrameProfiler::new(),
        };

        match saved_data_root {
            Some(root) if root.is_dir() => app.set_data_root(root),
            // No data root but saved mod roots: index those alone, so a session that only
            // ever works on modded characters still comes up with a populated fighter list.
            _ if !app.extra_roots.is_empty() => app.reindex_fighters(),
            _ => {}
        }

        // The eff editor's dump root, in order of authority: a folder explicitly picked in that
        // window, else the data root the main window is already showing (`set_data_root` above
        // has pointed it there), else the mod export folder. Its own default is the process cwd,
        // which meant the window came up listing whatever folder the app was launched from.
        let eff_root = saved_eff_root.or_else(|| match app.state.data_root {
            // Already pointed there by `set_data_root` — don't pull it back to the export folder.
            Some(_) => None,
            None => app.export_dir.clone(),
        });
        if let Some(root) = eff_root {
            app.eff_editor.set_export_root(root);
        }

        // Re-link the user's own ACMD source, quietly: a project that has moved or been
        // deleted since last session should not open with an error over an unrelated task.
        if let Some(root) = load_config_path(ACMD_SRC_CONFIG_KEY) {
            app.acmd_src = crate::acmd_src::SourceIndex::build(&root)
                .ok()
                .filter(|index| index.script_count() > 0);
        }

        // Profiling helper: preselect a fighter by internal name so a `VISIONARY_PROFILE` run
        // measures the loaded-model path (3D viewport, bone overlays) instead of the empty
        // "open a data root" viewport, which exercises almost none of the per-frame work.
        if let Some(want) = std::env::var_os("VISIONARY_PROFILE_FIGHTER") {
            let want = want.to_string_lossy().to_lowercase();
            match app
                .state
                .fighters
                .iter()
                .position(|f| f.name.eq_ignore_ascii_case(&want))
            {
                Some(i) => app.select_fighter(i),
                None => eprintln!("[perf] VISIONARY_PROFILE_FIGHTER={want}: no such fighter"),
            }
        }

        app
    }

    /// Resolve a project-relative eff path ("effect/fighter/mario/ef_mario.eff") to a file on
    /// disk. The eff editor's export root (the vanilla dump) is tried first, then the data
    /// root and every mod root — a MODDED character's eff only exists under its mod root, so
    /// resolving against the dump alone silently produced a missing source.
    ///
    /// Falls back to the export-root path when nothing exists, preserving the previous
    /// behaviour (and the previous error message) for genuinely missing files.
    fn resolve_eff_source(&self, source_rel: &str) -> PathBuf {
        let primary = self.eff_editor.export_root().join(source_rel);
        if primary.exists() {
            return primary;
        }
        let mut roots = self.all_roots();
        if let Some(d) = &self.export_dir {
            roots.push(d.clone());
        }
        roots
            .iter()
            .map(|r| r.join(source_rel))
            .find(|p| p.exists())
            .unwrap_or(primary)
    }

    /// Every root fighters and their costume slots are discovered from: the game-data root
    /// first (so vanilla files win on path lookups), then user-added mod roots.
    fn all_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(root) = &self.state.data_root {
            roots.push(root.clone());
        }
        roots.extend(self.extra_roots.iter().cloned());
        roots
    }

    /// Add a mod root (a folder containing `fighter/<name>/…`, i.e. the layout an
    /// Arcropolis mod uses) and re-index. Added-character mods live here; the game-data root
    /// stays untouched.
    fn add_mod_root(&mut self, path: PathBuf) {
        if self.extra_roots.contains(&path) {
            self.state.status = format!("Mod root already added: {}", path.display());
            return;
        }
        if !path.join("fighter").is_dir() && !path.join("effect").is_dir() {
            self.state.status = format!(
                "{} has no fighter/ or effect/ subdirectory — pick the folder that CONTAINS \
                 fighter/<name>, not the fighter folder itself",
                path.display()
            );
            return;
        }
        self.extra_roots.push(path);
        save_mod_roots(&self.extra_roots);
        self.reindex_fighters();
    }

    fn remove_mod_root(&mut self, path: &std::path::Path) {
        self.extra_roots.retain(|p| p != path);
        save_mod_roots(&self.extra_roots);
        self.reindex_fighters();
    }

    fn set_data_root(&mut self, path: PathBuf) {
        save_config_path("data_root", &path);
        self.state.fighters.clear();
        self.state.labels.clear();
        self.state.status = format!("Loading from {}...", path.display());

        // Legacy fallback: a ParamLabels.csv left in the export folder still loads, but
        // the downloaded copy (merged below) takes precedence.
        let param_labels = path.join("ParamLabels.csv");
        if param_labels.exists() {
            if let Ok(content) = std::fs::read_to_string(&param_labels) {
                self.state
                    .labels
                    .extend(crate::param_labels::parse_csv(&content));
            }
        }

        // Load Labels.txt (motion labels)
        let labels_txt = path.join("Labels.txt");
        if labels_txt.exists() {
            if let Ok(content) = std::fs::read_to_string(&labels_txt) {
                for line in content.lines() {
                    let label = line.trim();
                    if label.is_empty() {
                        continue;
                    }
                    let bare = label.strip_suffix(".nuanmb").unwrap_or(label);
                    let hash = hash40::hash40(bare);
                    self.state
                        .labels
                        .entry(hash.0)
                        .or_insert_with(|| bare.to_string());
                    if bare != label {
                        let hash_full = hash40::hash40(label);
                        self.state
                            .labels
                            .entry(hash_full.0)
                            .or_insert_with(|| bare.to_string());
                    }
                }
            }
        }

        // Runtime-downloaded ParamLabels.csv (ultimate-research/param-labels) — the
        // primary label source; `set_data_root` cleared `state.labels`, so re-merge.
        for (h, l) in &self.downloaded_labels {
            self.state.labels.insert(*h, l.clone());
        }

        // Index fighters
        let fighter_dir = path.join("fighter");
        if !fighter_dir.is_dir() {
            self.state.status = "No fighter/ directory found.".to_string();
            return;
        }

        // One path source: the eff editor reads base .eff files out of the same dump the rest of
        // the app indexes, so opening a data root re-points (and remembers) its root too. Left
        // alone, that window kept listing the previous dump's files — or the launch folder's.
        // A later pick inside the editor overwrites this; last choice wins either way.
        self.eff_editor.set_export_root(path.clone());
        save_config_path(EFF_ROOT_CONFIG_KEY, &path);

        self.state.data_root = Some(path);
        self.reindex_fighters();
    }

    /// Rebuild the fighter list from the data root plus every mod root. Idempotent — called
    /// on data-root change and whenever the mod-root set changes.
    fn reindex_fighters(&mut self) {
        // Sub-fighters, bosses and enemies the roster list has never shown. This filters by
        // exact vanilla name only, so a mod that happens to be called e.g. "nanaex" is kept.
        let skip = [
            "common",
            "ptrainer",
            "ptrainer_low",
            "pfushigisou",
            "pzenigame",
            "plizardon",
            "nana",
            "popo",
            "miienemyf",
            "miienemyg",
            "miienemys",
            "koopag",
            "master",
            "crazy",
        ];

        let roots = self.all_roots();
        self.state.fighters.clear();

        // Fighter name → the root that owns it. A mod root may either ADD a new fighter or
        // extend an existing one with more slots; the first root to define a fighter owns
        // its model/motion paths, so vanilla files stay authoritative when both exist.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut modded_added = 0usize;

        for (root_idx, root) in roots.iter().enumerate() {
            let fighter_dir = root.join("fighter");
            let Ok(entries) = std::fs::read_dir(&fighter_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let fighter_path = entry.path();
                if !fighter_path.is_dir() {
                    continue;
                }
                let name = match fighter_path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if skip.contains(&name.as_str()) || !seen.insert(name.clone()) {
                    continue;
                }

                // Slots come from EVERY root, not just the one that owns the fighter: a
                // slot-add mod for a vanilla fighter lives in a different folder than the
                // dump it extends.
                let slots = crate::data::discover_costume_slots(&roots, &name);

                // Motion data is what the move list needs; param/ is nice to have. Vanilla
                // dumps always carry both, but added-character mods frequently ship without
                // a param prc, and gating on it used to make them invisible.
                if !crate::data::fighter_dir_is_loadable(&fighter_path, &slots) {
                    continue;
                }

                let param_path = {
                    let p1 = fighter_path.join("param").join("vl.prc");
                    let p2 = fighter_path.join("param").join("fighter_param.prc");
                    if p1.exists() {
                        p1
                    } else {
                        p2
                    }
                };

                // The lowest EXISTING slot represents the fighter — a mod may ship only c08+.
                let slots = if slots.is_empty() {
                    crate::data::default_slots()
                } else {
                    slots
                };
                let base = format!("c{:02}", slots.first().copied().unwrap_or(0));
                let motion_dir = fighter_path.join("motion").join("body").join(&base);
                let model_dir = fighter_path.join("model").join("body").join(&base);
                let display_name = fighter_display_name(&name);
                let source = if root_idx == 0 && self.state.data_root.is_some() {
                    crate::data::FighterSource::DataRoot
                } else {
                    crate::data::FighterSource::ModRoot
                };

                // Prefer the effect folder from whichever root actually has one.
                let effect_dir = roots
                    .iter()
                    .map(|r| r.join("effect").join("fighter").join(&name))
                    .find(|d| d.is_dir());

                let entry = crate::data::FighterEntry {
                    name,
                    display_name,
                    param_path,
                    motion_dir,
                    model_dir,
                    effect_dir,
                    slots,
                    fighter_dir: fighter_path,
                    source,
                };
                if entry.is_modded() {
                    modded_added += 1;
                }
                self.state.fighters.push(entry);
            }
        }

        self.state
            .fighters
            .sort_by(|a, b| a.display_name.cmp(&b.display_name));

        let extra_slots: usize = self
            .state
            .fighters
            .iter()
            .filter(|f| {
                f.slots
                    .iter()
                    .any(|s| *s >= crate::data::VANILLA_SLOT_COUNT)
            })
            .count();
        let mut status = format!("Loaded {} fighters", self.state.fighters.len());
        if modded_added > 0 {
            status.push_str(&format!(" ({modded_added} modded)"));
        }
        if extra_slots > 0 {
            status.push_str(&format!(", {extra_slots} with slots past c07"));
        }
        status.push('.');
        self.state.status = status;
    }

    /// Re-scan costume slots for every indexed fighter — after a mod root is added/removed,
    /// or after an export writes new slot-scoped eff files.
    fn rescan_costume_slots(&mut self) {
        let roots = self.all_roots();
        for fighter in &mut self.state.fighters {
            let found = crate::data::discover_costume_slots(&roots, &fighter.name);
            if !found.is_empty() {
                fighter.slots = found;
            }
        }
    }

    fn select_fighter(&mut self, idx: usize) {
        self.state.selected_fighter = Some(idx);
        self.state.selected_move = None;
        self.state.hitboxes.clear();
        self.state.current_frame = FIRST_GAME_FRAME;
        self.state.total_frames = 0;
        self.move_list.clear();
        self.move_list_receiver = None;
        self.acmd_error = None;
        self.current_anim_path = None;

        let fighter = &self.state.fighters[idx];
        let model_dir = fighter.model_dir.clone();
        let motion_dir = fighter.motion_dir.clone();
        // A mod may not ship c00 at all, so weapon lookups follow the fighter's own base
        // slot rather than assuming slot 0 exists.
        let base_slot = fighter.base_slot();
        let model_root = fighter.fighter_dir.join("model");

        // Set skel path and eagerly load bone names for the dropdown
        let skel = model_dir.join("model.nusktb");
        self.current_skel_path = if skel.exists() {
            Some(skel.clone())
        } else {
            None
        };
        self.bone_names = skel
            .exists()
            .then(|| ssbh_data::skel_data::SkelData::from_file(&skel).ok())
            .flatten()
            .map(|s| s.bones.into_iter().map(|b| b.name).collect())
            .unwrap_or_default();

        // Also collect weapon bone names from sibling model dirs (sword, hammer, etc.)
        // model_root = fighter/{name}/model, each part holding cNN subdirs.
        if let Ok(entries) = std::fs::read_dir(&model_root) {
            for entry in entries.flatten() {
                let dir_name = entry.file_name();
                if dir_name.to_string_lossy() == "body" {
                    continue;
                }
                // The weapon's slot set can differ from the body's, so fall back to any slot
                // it does have rather than giving up when the base slot is missing.
                let part_dir = entry.path();
                let Some(weapon_skel_path) = crate::data::find_part_skel(&part_dir, base_slot)
                else {
                    continue;
                };
                if let Ok(wskel) = ssbh_data::skel_data::SkelData::from_file(&weapon_skel_path) {
                    for bone in wskel.bones {
                        if !self.bone_names.contains(&bone.name) {
                            self.bone_names.push(bone.name);
                        }
                    }
                }
            }
        }
        self.current_model_dir = Some(model_dir.clone());

        // Queue model load for wgpu (done in update where we have device/queue access)
        self.pending_model_load = Some(model_dir.clone());

        // The desktop no longer simulates or renders particles. Keep the selected fighter's
        // effect file queued for the editor; live preview is provided by slight_replica in game.
        let eff_path = fighter
            .effect_dir
            .as_ref()
            .map(|d| d.join(format!("ef_{}.eff", fighter.name)))
            .or_else(|| {
                self.state.data_root.as_ref().map(|root| {
                    root.join("effect")
                        .join("fighter")
                        .join(&fighter.name)
                        .join(format!("ef_{}.eff", fighter.name))
                })
            })
            .filter(|path| path.exists());
        self.current_eff_path = eff_path.clone();
        if let Some(path) = eff_path {
            self.eff_editor.queue_load(&path);
        }

        // Build move list on a background thread — reads many .nuanmb files for frame counts
        let labels = self.state.labels.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.move_list_receiver = Some(rx);
        self.state.status = "Loading moves...".to_string();

        std::thread::spawn(move || {
            let motion_list_path = motion_dir.join("motion_list.bin");
            let Ok(mlist) = motion_lib::open(&motion_list_path) else {
                return;
            };

            let mut moves: Vec<MoveEntry> = mlist
                .list
                .iter()
                .filter_map(|(hash_key, _)| {
                    let hash_val = hash_key.0;
                    let name = labels
                        .get(&hash_val)
                        .cloned()
                        .unwrap_or_else(|| format!("{:#018x}", hash_val));

                    // Filter early to avoid reading files for non-attack moves
                    let n = name.to_lowercase();
                    if !(n.contains("attack")
                        || n.contains("special")
                        || n.contains("throw")
                        || n.contains("catch")
                        || n.contains("cliff")
                        || n.contains("final"))
                    {
                        return None;
                    }

                    let anim_path = find_nuanmb(&motion_dir, &name, hash_val);
                    let frame_count = anim_path
                        .as_deref()
                        .and_then(|p| ssbh_data::anim_data::AnimData::from_file(p).ok())
                        .map(|a| a.final_frame_index as u32 + 1)
                        .unwrap_or(0);

                    Some(MoveEntry {
                        name,
                        hash: hash_val,
                        frame_count,
                        anim_path,
                    })
                })
                .collect();

            moves.sort_by(|a, b| a.name.cmp(&b.name));
            let _ = tx.send(moves);
        });
    }

    fn select_move(&mut self, move_entry: MoveEntry) {
        self.state.current_frame = FIRST_GAME_FRAME;
        self.state.total_frames = move_entry.frame_count;
        self.state.hitboxes.clear();
        self.state.script = crate::data::AcmdScript::default();
        self.state.effect_script = crate::data::EffectScript::default();
        self.state.effects = Vec::new();
        self.acmd_error = None;
        // Path was resolved at move list build time — no disk scan needed
        self.current_anim_path = move_entry.anim_path.clone();
        self.state.selected_move = Some(move_entry);
    }

    /// Kick off a "Fetch ACMD" in the background.
    ///
    /// This used to call the network inline, which froze the whole editor for the duration of
    /// a GitHub round trip (measured 56–293 ms on a warm connection, far worse on a slow one)
    /// on *every* click — it also called the uncached fetch, so re-opening a move you had
    /// already fetched paid the same cost again. Now the request runs on a worker thread
    /// against the disk cache and the result is applied on the UI thread in
    /// [`Self::poll_acmd_fetch`].
    fn fetch_acmd(&mut self) {
        let (fighter_name, move_name) = match (
            self.state
                .selected_fighter
                .and_then(|i| self.state.fighters.get(i)),
            &self.state.selected_move,
        ) {
            (Some(f), Some(m)) => (f.name.clone(), m.name.clone()),
            _ => return,
        };

        self.acmd_error = None;

        // A linked project is the authoritative script for anyone who has already modded the
        // move — the mirror only ever holds vanilla. It is a local file read, so it resolves
        // inline instead of paying for a thread and a repaint.
        if let Some(body) = self
            .acmd_src
            .as_ref()
            .and_then(|index| index.script_body(&fighter_name, &move_name))
        {
            // Drop any mirror fetch still in flight for this same move, or it would land
            // afterwards and overwrite the source-loaded script with the vanilla one.
            self.acmd_receiver = None;
            self.apply_acmd_body(&fighter_name, &move_name, Ok(body), "Project source");
            return;
        }

        self.fetching_acmd = true;
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let body = crate::acmd::fetch_script_body_cached(&fighter_name, &move_name)
                .map_err(|e| e.to_string());
            let _ = tx.send((fighter_name, move_name, body));
            // The app is reactive and may be sitting idle; without this the finished fetch
            // would not appear until the user happened to move the mouse.
            ctx.request_repaint();
        });
        self.acmd_receiver = Some(rx);
    }

    // ── Linked ACMD source project ────────────────────────────────────────────

    /// Index `root` as the ACMD source project and remember it for next session.
    fn link_acmd_source(&mut self, root: PathBuf) {
        match crate::acmd_src::SourceIndex::build(&root) {
            Ok(index) => {
                let scripts = index.script_count();
                let fighters = index.fighters.len();
                if scripts == 0 {
                    self.state.status = format!(
                        "No ACMD scripts found under {} — expected a smashline project with \
                         `unsafe extern \"C\" fn game_*` functions",
                        root.display()
                    );
                    return;
                }
                self.state.status = format!(
                    "Linked {scripts} script{} across {fighters} fighter{} from {}",
                    if scripts == 1 { "" } else { "s" },
                    if fighters == 1 { "" } else { "s" },
                    root.display()
                );
                self.acmd_src = Some(index);
                save_config_path(ACMD_SRC_CONFIG_KEY, &root);
                // The open move was loaded from the mirror; re-read it from source now.
                self.reload_move_scripts();
            }
            Err(e) => self.state.status = format!("Could not read that source project: {e}"),
        }
    }

    /// Re-read the linked project from disk, keeping the same root.
    fn rescan_acmd_source(&mut self) {
        if let Some(root) = self.acmd_src.as_ref().map(|index| index.root.clone()) {
            self.link_acmd_source(root);
        }
    }

    fn unlink_acmd_source(&mut self) {
        self.acmd_src = None;
        self.acmd_src_buffer = None;
        clear_config_path(ACMD_SRC_CONFIG_KEY);
        self.state.status = "Unlinked the ACMD source project — scripts come from the \
                             online archive again"
            .into();
        self.reload_move_scripts();
    }

    /// Re-load the open move's scripts from whichever source is now authoritative.
    fn reload_move_scripts(&mut self) {
        if self.state.selected_move.is_some() {
            self.fetch_acmd();
        }
    }

    /// Check the open move's script out of the linked project into the source editor.
    fn open_source_buffer(&mut self, script_prefix: &str) {
        let Some(index) = self.acmd_src.as_ref() else {
            return;
        };
        let (Some(fighter), Some(move_entry)) = (
            self.state
                .selected_fighter
                .and_then(|i| self.state.fighters.get(i)),
            self.state.selected_move.as_ref(),
        ) else {
            return;
        };
        let (fighter, move_name) = (fighter.name.clone(), move_entry.name.clone());
        let script = crate::acmd::acmd_script_name(script_prefix, &move_name);
        let Some(site) = index.script(&fighter, &script) else {
            self.state.status = format!("{script} is not in the linked project");
            return;
        };
        let (file, span) = (site.file.clone(), site.span.clone());
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) => {
                self.state.status = format!("Could not read {}: {e}", file.display());
                return;
            }
        };
        let Some(body) = text.get(span.clone()) else {
            self.state.status = format!(
                "{} changed on disk since it was indexed — rescan the project",
                file.display()
            );
            return;
        };
        let body = body.to_string();
        self.acmd_src_buffer = Some(SourceBuffer {
            fighter,
            move_name,
            script,
            file,
            span,
            original: body.clone(),
            synced: body.clone(),
            text: body,
            settling: None,
            // Left unset so the first reconcile adopts the panels as-is rather than reading
            // the freshly-opened text as a pending GUI edit.
            mirrored: None,
            note: None,
        });
    }

    /// Write the source editor's buffer back into the file it came from.
    ///
    /// The buffer covers one function, so the rest of the file is preserved by splicing
    /// rather than rewriting. The span is re-validated against the file's CURRENT contents
    /// first: an external edit between checkout and save would otherwise have its text
    /// overwritten by whatever the stale span happened to point at.
    fn save_source_buffer(&mut self) {
        let Some(buffer) = self.acmd_src_buffer.as_mut() else {
            return;
        };
        let text = match std::fs::read_to_string(&buffer.file) {
            Ok(text) => text,
            Err(e) => {
                self.state.status = format!("Could not read {}: {e}", buffer.file.display());
                return;
            }
        };
        if text.get(buffer.span.clone()) != Some(buffer.original.as_str()) {
            self.state.status = format!(
                "{} changed on disk since this script was opened — reopen it to avoid \
                 discarding that edit",
                buffer.file.display()
            );
            return;
        }
        let mut updated = String::with_capacity(text.len());
        updated.push_str(&text[..buffer.span.start]);
        updated.push_str(&buffer.text);
        updated.push_str(&text[buffer.span.end..]);
        if let Err(e) = std::fs::write(&buffer.file, &updated) {
            self.state.status = format!("Could not write {}: {e}", buffer.file.display());
            return;
        }
        buffer.span = buffer.span.start..buffer.span.start + buffer.text.len();
        buffer.original = buffer.text.clone();
        let (script, file) = (buffer.script.clone(), buffer.file.clone());
        self.state.status = format!("Saved {script} to {}", file.display());
        // Spans past the edit have all shifted, so the index has to be rebuilt before it is
        // used again — including by the reload below.
        self.rescan_acmd_source();
    }

    /// The function(s) an export would write for the selected move, and anything it could
    /// not reproduce faithfully.
    ///
    /// Built from the CURRENT editor state through the same emitters `mod_export` uses, so
    /// this answers "what do I get if I export right now" rather than "what was exported
    /// last time". The `game_*` script is rebuilt from the hitboxes exactly the way
    /// [`Self::commit_current_edits`] does before it reaches the edit log — otherwise the
    /// preview would show a script the export would never actually produce.
    ///
    /// Deliberately independent of the linked project: a move that came from live capture
    /// has no source file anywhere, and that is exactly the case where seeing the generated
    /// code matters most. An open source buffer only narrows the preview to that buffer's
    /// counterpart, so the two panes line up side by side.
    fn generated_source_for_move(&self) -> (String, crate::acmd_verify::Report) {
        let Some(move_name) = self.state.selected_move.as_ref().map(|m| m.name.clone()) else {
            return (String::new(), Default::default());
        };
        let only_game = self
            .acmd_src_buffer
            .as_ref()
            .filter(|buffer| buffer.move_name == move_name)
            .map(SourceBuffer::is_game_script);

        let mut out = String::new();
        let mut report = crate::acmd_verify::Report::default();

        let has_hitbox_script =
            !self.state.hitboxes.is_empty() || !self.state.script.stmts.is_empty();
        if only_game != Some(false) && has_hitbox_script {
            let script = if self.state.script.stmts.is_empty() {
                synthesize_script_from_hitboxes(&self.state.hitboxes)
            } else {
                rebuild_script_from_hitboxes(&self.state.script, &self.state.hitboxes)
            };
            let emitted = crate::acmd::preview_game_fn(&script, &move_name);
            crate::acmd_verify::verify_move(&move_name, &script, &emitted, &mut report);
            out.push_str(&emitted);
        }

        if only_game != Some(true) && !self.state.effects.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            let tweaks: Vec<crate::mod_project::LiveTweak> = self
                .live_overrides
                .tweaked()
                .iter()
                .filter_map(|(_, form)| live_tweak_from_override(form))
                .collect();
            let emitted = crate::acmd::preview_effect_fn(&self.state.effects, &move_name, &tweaks);
            crate::acmd_verify::verify_effect_move(
                &move_name,
                &self.state.effects,
                &emitted,
                &mut report,
            );
            out.push_str(&emitted);
        }
        (out, report)
    }

    /// Keep the source editor and the editor panels showing the same move.
    ///
    /// Runs once per frame, in one place, rather than hooking every widget that can mutate
    /// the move — the panels change `state.hitboxes` and `state.effects` from a dozen call
    /// sites, and a watcher catches all of them including the ones added later.
    ///
    /// Exactly one side wins per frame. Typing in the source is adopted into the panels
    /// (after the text settles); otherwise a panel edit is written into the source text. The
    /// `synced`/`mirrored` marks are what stop the two from echoing each other forever.
    fn reconcile_source_buffer(&mut self, ctx: &egui::Context) {
        /// Long enough that a burst of typing reparses once, short enough to feel live.
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

        let Some(buffer) = self.acmd_src_buffer.as_ref() else {
            return;
        };
        // A buffer left open from another move is a viewer, not a live mirror: adopting its
        // text would overwrite the move the user is actually looking at.
        let current = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .zip(self.state.selected_move.as_ref())
            .is_some_and(|(f, m)| f.name == buffer.fighter && m.name == buffer.move_name);
        if !current {
            // Drop the baseline: coming back to this move reloads the panels, and comparing
            // that against a snapshot from before the switch would read as a panel edit and
            // write it into the source.
            if let Some(buffer) = self.acmd_src_buffer.as_mut() {
                buffer.mirrored = None;
            }
            return;
        }

        // ── Source → panels ───────────────────────────────────────────────────
        if buffer.text != buffer.synced {
            let buffer = self.acmd_src_buffer.as_mut().expect("checked above");
            let settled = match &buffer.settling {
                Some((text, since)) if *text == buffer.text => since.elapsed() >= SETTLE,
                _ => {
                    buffer.settling = Some((buffer.text.clone(), std::time::Instant::now()));
                    false
                }
            };
            if !settled {
                ctx.request_repaint_after(SETTLE);
                return;
            }
            buffer.settling = None;
            self.adopt_source_buffer_into_panels();
            return;
        }
        if let Some(buffer) = self.acmd_src_buffer.as_mut() {
            buffer.settling = None;
        }

        // ── Panels → source ───────────────────────────────────────────────────
        // Compared by reference before cloning: this runs every frame, and the snapshot is
        // a couple of dozen structs carrying eight owned strings apiece.
        let buffer = self.acmd_src_buffer.as_mut().expect("checked above");
        let had_baseline = match &buffer.mirrored {
            Some((hitboxes, effects)) => {
                if *hitboxes == self.state.hitboxes && *effects == self.state.effects {
                    return;
                }
                true
            }
            None => false,
        };
        buffer.mirrored = Some((self.state.hitboxes.clone(), self.state.effects.clone()));
        // With no baseline this is the first pass after a checkout or a move switch, and the
        // text already matches the panels — there is nothing to write, only a mark to set.
        if had_baseline {
            self.mirror_panels_into_source_buffer();
        }
    }

    /// Re-parse the source buffer and make the editor panels show it.
    ///
    /// The parsed script becomes the new pristine as well as the current state: the user is
    /// editing the code itself, so the code IS the baseline, and keeping an older one would
    /// make the panels' "orig" columns describe a script that no longer exists.
    fn adopt_source_buffer_into_panels(&mut self) {
        let Some(buffer) = self.acmd_src_buffer.as_ref() else {
            return;
        };
        let (text, is_game) = (buffer.text.clone(), buffer.is_game_script());

        let note = if is_game {
            let script = crate::acmd::parse_acmd_script(&text);
            let mut hitboxes = script.to_hitboxes();
            // Mid-edit source parses to nothing. Holding the last good state is much better
            // than emptying the timeline under the user between keystrokes.
            if hitboxes.is_empty() && !self.state.hitboxes.is_empty() {
                Some("Source has no readable hitboxes — showing the last version that did")
            } else {
                self.normalize_hitbox_bones(&mut hitboxes);
                self.state.hitboxes_pristine = hitboxes.clone();
                self.state.hitboxes = hitboxes;
                self.state.script = script;
                None
            }
        } else {
            let effect_script = crate::acmd::parse_effect_script(&text);
            let calls = effect_script.to_effect_calls();
            if calls.is_empty() && !self.state.effects.is_empty() {
                Some("Source has no readable effect spawns — showing the last version that did")
            } else {
                self.state.effects_pristine = calls.clone();
                self.state.effects = calls;
                self.state.selected_effect_call = None;
                self.state.effect_script = effect_script;
                self.push_effect_rules();
                None
            }
        };

        self.state.total_frames = self.state.total_frames.max(timeline_frame_extent(
            &self.state.hitboxes,
            &self.state.effects,
        ));
        if let Some(buffer) = self.acmd_src_buffer.as_mut() {
            buffer.synced = buffer.text.clone();
            buffer.note = note.map(str::to_string);
            // The panels now match the text, so this is not a pending panel edit.
            buffer.mirrored = None;
        }
        // Hitbox live rules ride the existing per-frame watcher; effects pushed above.
    }

    /// Write the panels' current values into the open source buffer's text.
    fn mirror_panels_into_source_buffer(&mut self) {
        let Some(buffer) = self.acmd_src_buffer.as_ref() else {
            return;
        };
        let label = format!("{}/{}", buffer.fighter, buffer.move_name);
        let outcome = if buffer.is_game_script() {
            crate::acmd_src::rewrite_hitboxes(
                &buffer.text,
                &label,
                &self.state.hitboxes_pristine,
                &self.state.hitboxes,
            )
        } else {
            crate::acmd_src::rewrite_effect_calls(
                &buffer.text,
                &label,
                &self.state.effects_pristine,
                &self.state.effects,
            )
        };
        let Some(buffer) = self.acmd_src_buffer.as_mut() else {
            return;
        };
        match outcome {
            Ok((updated, report)) => {
                buffer.text = updated;
                buffer.note = (!report.skipped.is_empty()).then(|| report.skipped.join("; "));
            }
            Err(e) => buffer.note = Some(e.to_string()),
        }
        // Whichever way it went, the text now reflects this panel state — do not read the
        // write back as fresh typing on the next frame.
        buffer.synced = buffer.text.clone();
    }

    /// Write the editor's hitbox and effect-spawn edits for the open move back into source.
    fn sync_edits_to_source(&mut self) {
        let Some(index) = self.acmd_src.as_ref() else {
            return;
        };
        let (Some(fighter), Some(move_entry)) = (
            self.state
                .selected_fighter
                .and_then(|i| self.state.fighters.get(i)),
            self.state.selected_move.as_ref(),
        ) else {
            return;
        };
        let (fighter, move_name) = (fighter.name.clone(), move_entry.name.clone());

        let mut changed = 0usize;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut ran = false;

        if index
            .script(
                &fighter,
                &crate::acmd::acmd_script_name("effect", &move_name),
            )
            .is_some()
        {
            ran = true;
            match crate::acmd_src::sync_effect_calls(
                index,
                &fighter,
                &move_name,
                &self.state.effects_pristine,
                &self.state.effects,
            ) {
                Ok(report) => {
                    changed += report.changed;
                    files.extend(report.files);
                    notes.extend(report.skipped);
                }
                Err(e) => notes.push(e.to_string()),
            }
        }
        if index
            .script(&fighter, &crate::acmd::acmd_script_name("game", &move_name))
            .is_some()
        {
            ran = true;
            match crate::acmd_src::sync_hitboxes(
                index,
                &fighter,
                &move_name,
                &self.state.hitboxes_pristine,
                &self.state.hitboxes,
            ) {
                Ok(report) => {
                    changed += report.changed;
                    files.extend(report.files);
                    notes.extend(report.skipped);
                }
                Err(e) => notes.push(e.to_string()),
            }
        }

        if !ran {
            self.state.status =
                format!("{fighter}/{move_name} is not in the linked project — nothing to sync");
            return;
        }
        files.sort();
        files.dedup();
        let mut status = format!(
            "Synced {changed} value{} into {} file{}",
            if changed == 1 { "" } else { "s" },
            files.len(),
            if files.len() == 1 { "" } else { "s" },
        );
        if !notes.is_empty() {
            status.push_str(" — ");
            status.push_str(&notes.join("; "));
        }
        self.state.status = status;
        if changed > 0 {
            // The written values are now the pristine ones; re-reading also refreshes the
            // spans the next sync will target.
            self.rescan_acmd_source();
        }
    }

    /// Restore the saved main-window geometry (once), then track and persist geometry changes.
    ///
    /// Restoring happens on the first frame rather than in `main()` because the monitor size
    /// is only known once a window exists — that is what makes the off-screen clamp possible.
    fn update_window_geometry(&mut self, ctx: &egui::Context) {
        let monitor = ctx.input(|i| i.viewport().monitor_size);

        if !self.main_geometry_restored {
            self.main_geometry_restored = true;
            if let Some(saved) = self.window_geometry.get("main").copied() {
                let g = match monitor {
                    Some(m) => saved.clamped_to_screen(m.x, m.y),
                    None => saved,
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(g.w, g.h)));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(g.x, g.y)));
            }
        } else if let Some(g) = WindowGeometry::from_viewport(ctx) {
            // Only record after the restore frame, or we would immediately overwrite the saved
            // position with wherever the WM happened to open the window first.
            self.remember_geometry("main", g);
        }

        if let Some(g) = self.transplant_geometry.take() {
            self.remember_geometry("transplant", g);
        }

        // Debounced flush: this runs every frame, and rewriting the config file that often is
        // exactly the sort of per-frame disk I/O that makes an editor feel sluggish.
        if self.geometry_dirty && self.geometry_saved_at.elapsed().as_secs_f32() > 2.0 {
            self.flush_window_geometry();
        }
    }

    fn remember_geometry(&mut self, key: &str, g: WindowGeometry) {
        if self.window_geometry.get(key) == Some(&g) {
            return;
        }
        self.window_geometry.insert(key.to_string(), g);
        self.geometry_dirty = true;
    }

    fn flush_window_geometry(&mut self) {
        if !self.geometry_dirty {
            return;
        }
        save_window_geometry(&self.window_geometry);
        self.geometry_dirty = false;
        self.geometry_saved_at = std::time::Instant::now();
    }

    /// Apply a finished background fetch, if one has landed.
    fn poll_acmd_fetch(&mut self) {
        let Some(rx) = &self.acmd_receiver else {
            return;
        };
        let Ok((fighter_name, move_name, body)) = rx.try_recv() else {
            return;
        };
        self.acmd_receiver = None;
        self.fetching_acmd = false;

        // The user can switch fighter or move while a fetch is in flight; applying a stale
        // result would silently overwrite the new selection's hitboxes.
        let still_current = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .is_some_and(|f| f.name == fighter_name)
            && self
                .state
                .selected_move
                .as_ref()
                .is_some_and(|m| m.name == move_name);
        if !still_current {
            return;
        }
        self.apply_acmd_body(&fighter_name, &move_name, body, "GitHub");
    }

    /// Put both fetch paths on the first frame where the move actually shows something.
    /// Hitboxes and effects are already in the same one-based game-frame space here.
    fn jump_to_earliest_active_frame(&mut self) {
        if let Some(first) = self
            .state
            .hitboxes
            .iter()
            .map(|hitbox| hitbox.active_start)
            .chain(self.state.effects.iter().map(|effect| effect.active_start))
            .min()
        {
            self.state.current_frame = first.max(FIRST_GAME_FRAME);
        }
    }

    /// Rewrite parsed ACMD bone names to the skeleton's own casing.
    ///
    /// Scripts hash the lowercase resource name, while the skel exposes display case, and
    /// several ACMD joints are virtual — they have no bone of their own and have to resolve
    /// to the one the game attaches them to.
    fn normalize_hitbox_bones(&self, hitboxes: &mut [crate::data::Hitbox]) {
        let bone_name_map: std::collections::HashMap<String, String> = self
            .bone_names
            .iter()
            .map(|n| (n.to_lowercase(), n.clone()))
            .collect();

        const VIRTUAL_BONE_FALLBACKS: &[(&str, &str)] = &[
            ("haver", "HandR"),
            ("havel", "HandL"),
            ("haver2", "HandR"),
            ("throw", "Hip"),
            ("itemroot", "Hip"),
            ("top", "Trans"),
            ("trans", "Trans"),
            ("rot", "Rot"),
        ];

        for hb in hitboxes {
            let lower = hb.bone_name.to_lowercase();
            if let Some(canonical) = bone_name_map.get(&lower) {
                hb.bone_name = canonical.clone();
            } else if let Some(&(_, fallback)) =
                VIRTUAL_BONE_FALLBACKS.iter().find(|(v, _)| *v == lower)
            {
                if let Some(canonical) = bone_name_map.get(&fallback.to_lowercase()) {
                    hb.bone_name = canonical.clone();
                }
            }
        }
    }

    fn apply_acmd_body(
        &mut self,
        fighter_name: &str,
        move_name: &str,
        body: Result<String, String>,
        source_label: &str,
    ) {
        match body {
            Ok(body) => {
                let script = crate::acmd::parse_acmd_script(&body);
                let effect_script = crate::acmd::parse_effect_script(&body);

                let mut hitboxes = script.to_hitboxes();
                // A move with effects but no hitboxes is a real script, not a failed load.
                // Throwing its effects away used to make a linked source project that only
                // carries `effect_*` functions — the common shape for a VFX-only mod — look
                // like it had loaded nothing at all.
                let effects_only = hitboxes.is_empty() && !effect_script.stmts.is_empty();
                if hitboxes.is_empty() && !effects_only {
                    self.acmd_error = Some(format!(
                        "No hitboxes found for {}/{}",
                        fighter_name, move_name
                    ));
                    self.state.effect_script = crate::data::EffectScript::default();
                    self.state.effects = Vec::new();
                } else {
                    if effects_only {
                        self.acmd_error = Some(format!(
                            "{fighter_name}/{move_name} has effect spawns but no hitboxes"
                        ));
                    }
                    self.normalize_hitbox_bones(&mut hitboxes);
                    self.state.hitboxes_pristine = hitboxes.clone();
                    self.state.acmd_source = source_label.into();
                    self.state.hitboxes = hitboxes;
                    self.state.script = script;

                    // Store effect data — pristine first, then re-apply this move's edits.
                    self.state.effects = effect_script.to_effect_calls();
                    self.state.effects_pristine = self.state.effects.clone();
                    self.state.selected_effect_call = None;
                    self.apply_effect_call_edits_to_current();
                    self.push_effect_rules();
                    self.state.effect_script = effect_script;
                    if self.apply_saved_hitbox_edits_to_current() {
                        self.push_hitbox_rules();
                    }

                    self.state.total_frames = self.state.total_frames.max(timeline_frame_extent(
                        &self.state.hitboxes,
                        &self.state.effects,
                    ));

                    self.jump_to_earliest_active_frame();
                }
            }
            Err(e) => {
                // The script archive only covers the vanilla roster, so an added-character
                // mod ALWAYS misses here. That is expected, not a malfunction — say so and
                // point at live capture, which reads the script off the running game and
                // works for any fighter.
                let modded = self
                    .state
                    .selected_fighter
                    .and_then(|i| self.state.fighters.get(i))
                    .is_some_and(|f| f.is_modded());
                self.acmd_error = Some(if modded {
                    format!(
                        "'{fighter_name}' is a modded character — the online script archive \
                         only covers the vanilla roster. Use live capture (play the move in \
                         game with the plugin connected) to load {move_name}. ({e})"
                    )
                } else {
                    format!("Fetch failed: {e}")
                });
                self.state.effect_script = crate::data::EffectScript::default();
                self.state.effects = Vec::new();
            }
        }
        self.fetching_acmd = false;
    }

    /// The ACMD Source window: link a project, read the open move's script out of it, edit
    /// that script in place, and push editor edits back into it.
    fn draw_acmd_source_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_acmd_src;
        let saved = self.window_geometry.get("acmd_source").copied();
        let mut window = egui::Window::new("ACMD Source")
            .open(&mut open)
            .resizable(true)
            .default_size(saved.map_or([720.0, 560.0], |g| [g.w, g.h]));
        if let Some(g) = saved {
            window = window.default_pos([g.x, g.y]);
        }

        // Actions are recorded and run after the closure: every one of them needs `&mut self`,
        // which the borrow of `self.acmd_src` for display rules out inside it.
        let mut link: Option<PathBuf> = None;
        let mut rescan = false;
        let mut unlink = false;
        let mut checkout: Option<&'static str> = None;
        let mut save = false;
        let mut revert = false;
        let mut sync = false;

        // Both are read off `self` before the closure takes its mutable borrow of the open
        // buffer. Generating is cheap, but only pay for it when a pane is actually showing it.
        let mut view = self.acmd_src_view;
        let generated = (view != SourceView::Source).then(|| self.generated_source_for_move());

        let response = window.show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    "Read ACMD scripts from your own smashline project instead of the online \
                     archive of vanilla scripts, so the editor shows the code your game \
                     actually runs.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
            ui.separator();

            // The linked project is a HEADER, not a gate. It used to return early when
            // nothing was linked, when the fighter was not in the project, or when no script
            // was open — which meant a live-captured move, the one case with no source file
            // anywhere, showed an empty window instead of the code it would export.
            ui.horizontal_wrapped(|ui| match self.acmd_src.as_ref() {
                None => {
                    ui.label(
                        egui::RichText::new("No source project linked")
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                    if ui
                        .small_button("Link Source Project…")
                        .on_hover_text(
                            "Point at the root of the Rust project that builds your ACMD \
                                 plugin — the folder holding Cargo.toml",
                        )
                        .clicked()
                    {
                        link = rfd::FileDialog::new()
                            .set_title("Link ACMD source project")
                            .pick_folder();
                    }
                }
                Some(index) => {
                    ui.label(
                        egui::RichText::new(index.root.display().to_string())
                            .small()
                            .monospace(),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "· {} scripts · {} fighters",
                            index.script_count(),
                            index.fighters.len()
                        ))
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    if ui
                        .small_button("Rescan")
                        .on_hover_text(
                            "Re-read the project from disk, picking up edits made elsewhere",
                        )
                        .clicked()
                    {
                        rescan = true;
                    }
                    if ui.small_button("Unlink").clicked() {
                        unlink = true;
                    }
                }
            });
            ui.separator();

            let selection = self
                .state
                .selected_fighter
                .and_then(|i| self.state.fighters.get(i))
                .zip(self.state.selected_move.as_ref())
                .map(|(f, m)| (f.name.clone(), m.name.clone()));
            let Some((fighter, move_name)) = selection else {
                ui.label(
                    egui::RichText::new("Select a fighter and a move to see its script.")
                        .color(egui::Color32::GRAY),
                );
                return;
            };

            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("{fighter} / {move_name}")).strong());
                if !self.state.acmd_source.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("from {}", self.state.acmd_source))
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }
            });

            ui.horizontal(|ui| {
                ui.label("Show:");
                ui.selectable_value(&mut view, SourceView::Source, "Your source")
                    .on_hover_text("The move's own code in your linked project");
                ui.selectable_value(&mut view, SourceView::Generated, "Generated")
                    .on_hover_text(
                        "The code Visionary would write into acmd_source/ if you exported \
                         right now, from the move as it stands in the editor. Works for any \
                         move the editor has loaded, including live captures.",
                    );
                ui.selectable_value(&mut view, SourceView::Split, "Side by side");
            });
            ui.separator();

            // Everything that needs `&self` is resolved before the buffer is borrowed
            // mutably below — closures capturing both would not borrow-check.
            let scripts: Vec<(&str, String, bool, bool)> =
                [("game", "Hitboxes"), ("effect", "Effects")]
                    .into_iter()
                    .map(|(prefix, label)| {
                        let name = crate::acmd::acmd_script_name(prefix, &move_name);
                        let present = self
                            .acmd_src
                            .as_ref()
                            .is_some_and(|index| index.script(&fighter, &name).is_some());
                        let showing = self
                            .acmd_src_buffer
                            .as_ref()
                            .is_some_and(|b| b.script == name);
                        (label, name, present, showing)
                    })
                    .collect();
            let any_script = scripts.iter().any(|(_, _, present, _)| *present);
            let reason = match self.acmd_src.as_ref() {
                None => NoSource::NotLinked,
                Some(index) if !index.has_fighter(&fighter) => NoSource::FighterAbsent,
                Some(_) if !any_script => NoSource::MoveAbsent,
                Some(_) => NoSource::NothingOpen,
            };

            if view != SourceView::Generated {
                ui.horizontal_wrapped(|ui| {
                    for (index, (label, name, present, showing)) in scripts.iter().enumerate() {
                        if ui
                            .add_enabled(
                                *present && !*showing,
                                egui::Button::new(format!("{label}  {name}")),
                            )
                            .on_hover_text(if *present {
                                format!("Open {name} from the project")
                            } else {
                                format!("{name} is not in the linked project")
                            })
                            .clicked()
                        {
                            checkout = Some(if index == 0 { "game" } else { "effect" });
                        }
                    }
                    if ui
                        .add_enabled(any_script, egui::Button::new("Sync edits into source"))
                        .on_hover_text(
                            "Write this move's edited hitbox and spawn VALUES back into your \
                             source. The macros you called, your comments, and your formatting \
                             are left exactly as written; anything that would need the code \
                             restructured is reported instead of guessed at.",
                        )
                        .clicked()
                    {
                        sync = true;
                    }
                });
            }

            if let Some((text, report)) = generated.as_ref() {
                draw_verification(ui, report, !text.is_empty());
            }

            let mut generated_text = generated.map(|(text, _)| text).unwrap_or_default();
            let buffer = self.acmd_src_buffer.as_mut();
            match view {
                SourceView::Source => draw_source_pane(
                    ui,
                    buffer,
                    &fighter,
                    &move_name,
                    reason,
                    &mut save,
                    &mut revert,
                ),
                SourceView::Generated => draw_generated_pane(ui, &mut generated_text),
                SourceView::Split => {
                    let (mut save_split, mut revert_split) = (false, false);
                    ui.columns(2, |columns| {
                        columns[0].label(egui::RichText::new("Your source").small().strong());
                        draw_source_pane(
                            &mut columns[0],
                            buffer,
                            &fighter,
                            &move_name,
                            reason,
                            &mut save_split,
                            &mut revert_split,
                        );
                        columns[1]
                            .label(egui::RichText::new("Generated by export").small().strong());
                        draw_generated_pane(&mut columns[1], &mut generated_text);
                    });
                    save |= save_split;
                    revert |= revert_split;
                }
            }
        });

        if let Some(response) = response {
            let rect = response.response.rect;
            self.remember_geometry(
                "acmd_source",
                WindowGeometry {
                    x: rect.min.x,
                    y: rect.min.y,
                    w: rect.width(),
                    h: rect.height(),
                },
            );
        }
        self.show_acmd_src = open;
        self.acmd_src_view = view;

        if let Some(root) = link {
            self.link_acmd_source(root);
        }
        if rescan {
            self.rescan_acmd_source();
        }
        if unlink {
            self.unlink_acmd_source();
        }
        if let Some(prefix) = checkout {
            self.open_source_buffer(prefix);
        }
        if save {
            // Re-indexing after the write reloads the move, so the panels follow.
            self.save_source_buffer();
        }
        if revert {
            if let Some(buffer) = self.acmd_src_buffer.as_mut() {
                buffer.text = buffer.original.clone();
                buffer.note = None;
                // Left differing from `synced`, so the reconcile adopts the restored text
                // and the panels come back with it.
            }
        }
        if sync {
            self.sync_edits_to_source();
        }
    }

    /// Open an arbitrary .eff from disk and make it available to the editor and donor pool.
    /// Visual preview happens live in game through slight_replica.
    fn open_external_eff(&mut self, path: PathBuf) {
        if !path.exists() {
            self.state.status = format!("Effect file not found: {}", path.display());
            return;
        }
        self.current_eff_path = Some(path.clone());
        self.eff_editor.queue_load(&path);
        if self.effect_pool.is_none() {
            if let Some(root) = self
                .export_dir
                .clone()
                .or_else(|| self.state.data_root.clone())
            {
                self.effect_pool = Some(crate::effect_pool::EffectPool::new(root));
            }
        }
        if let Some(pool) = self.effect_pool.as_mut() {
            pool.add_file(&path);
        }
        self.recent_effs.retain(|recent| recent != &path);
        self.recent_effs.insert(0, path);
        self.recent_effs.truncate(12);
        save_recent_effs(&self.recent_effs);
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
        let mut fighters: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
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
        let saved = self.window_geometry.get("edits").copied();
        let mut window = egui::Window::new("Edits")
            .open(&mut open)
            .resizable(true)
            .default_size(saved.map_or([460.0, 520.0], |g| [g.w, g.h]));
        if let Some(g) = saved {
            // In-canvas window: the saved position is relative to the main window, so clamping
            // is against the canvas rather than the monitor. `default_pos` only applies until
            // egui has its own remembered position for this window.
            window = window.default_pos([g.x, g.y]);
        }
        let edits_response = window.show(ctx, |ui| {
            if fighters.is_empty() {
                ui.label(egui::RichText::new("No edits recorded yet.").color(egui::Color32::GRAY));
                return;
            }

            ui.label(
                egui::RichText::new(
                    "All edits across the toolkit — hitboxes (incl. live rules), effect \
                     spawns, live tweaks, and authored eff values. Saved automatically; \
                     use ↶ to restore the source (also un-sends the live state).",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
            ui.separator();

            // ── Live color/speed tweaks (kind-global runtime multipliers) ──
            let tweaks = self.live_overrides.tweaked();
            if !tweaks.is_empty() {
                ui.label(
                    egui::RichText::new("Live color/speed tweaks")
                        .small()
                        .strong(),
                );
                for (hash, form) in &tweaks {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        let c = form.rainbow.color;
                        ui.label(
                            egui::RichText::new(format!(
                                "{} — color ×[{:.2} {:.2} {:.2}] speed ×{:.2}",
                                form.effect_name, c.red, c.green, c.blue, form.speed
                            ))
                            .small()
                            .monospace(),
                        );
                        if ui
                            .small_button("×")
                            .on_hover_text("Revert (also in game)")
                            .clicked()
                        {
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
                                && k.split_once('/')
                                    .map(|(f, _)| f == fighter_name)
                                    .unwrap_or(false)
                        })
                        .map(|(k, _)| k.clone())
                        .collect();
                    let eff = self.eff_mods.get(fighter_name).filter(|e| !e.is_empty());
                    let total = move_names.len() + call_keys.len() + eff.map(|_| 1).unwrap_or(0);

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
                                    let is_active = self
                                        .state
                                        .selected_fighter
                                        .and_then(|i| self.state.fighters.get(i))
                                        .map(|f| &f.name == fighter_name)
                                        .unwrap_or(false)
                                        && self
                                            .state
                                            .selected_move
                                            .as_ref()
                                            .map(|m| &m.name == move_name)
                                            .unwrap_or(false);
                                    let label = if is_active {
                                        egui::RichText::new(format!("▶ {}", move_name))
                                            .color(egui::Color32::from_rgb(100, 200, 255))
                                    } else {
                                        egui::RichText::new(move_name.clone())
                                    };
                                    ui.label(label);
                                    if let Some(record) = self
                                        .state
                                        .edit_log
                                        .entries
                                        .get(fighter_name)
                                        .and_then(|m| m.get(move_name))
                                    {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} hb",
                                                record.hitboxes.len()
                                            ))
                                            .small()
                                            .color(egui::Color32::GRAY),
                                        );
                                    }
                                    let rule_key = format!("{fighter_name}/{move_name}");
                                    if let Some(rules) = self.hitbox_rules_store.get(&rule_key) {
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
                                    if ui
                                        .small_button("Export")
                                        .on_hover_text("Export this move as smashline source")
                                        .clicked()
                                    {
                                        export_move =
                                            Some((fighter_name.clone(), move_name.clone()));
                                    }
                                    if ui
                                        .small_button("↶")
                                        .on_hover_text(
                                            "Discard this move's hitbox edits and restore the source",
                                        )
                                        .clicked()
                                    {
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
                                let (n_mod, n_add, n_rem, n_sup) =
                                    edits.iter().fold((0, 0, 0, 0), |(m, a, r, s), e| {
                                        match &e.op {
                                            crate::data::EffectCallOp::Modify(c) => {
                                                (m + 1, a, r, s + usize::from(c.disabled))
                                            }
                                            crate::data::EffectCallOp::Add(c) => {
                                                (m, a + 1, r, s + usize::from(c.disabled))
                                            }
                                            crate::data::EffectCallOp::Remove => (m, a, r + 1, s),
                                        }
                                    });
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    ui.label(mv);
                                    let mut txt =
                                        format!("{n_mod} edited · {n_add} added · {n_rem} removed");
                                    if n_sup > 0 {
                                        txt.push_str(&format!(" · {n_sup} suppressed live"));
                                    }
                                    ui.label(
                                        egui::RichText::new(txt).small().color(egui::Color32::GRAY),
                                    );
                                    if ui
                                        .small_button("↶")
                                        .on_hover_text("Discard these effect-spawn edits")
                                        .clicked()
                                    {
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
                                if ui
                                    .small_button("↶")
                                    .on_hover_text("Discard all authored eff edits")
                                    .clicked()
                                {
                                    clear_eff_fighter = Some(fighter_name.clone());
                                }
                            });
                            for a in &eff.authored {
                                ui.horizontal(|ui| {
                                    ui.add_space(24.0);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} / {} — {} field(s)",
                                            if a.set_name.is_empty() {
                                                format!("set {}", a.set_idx)
                                            } else {
                                                a.set_name.clone()
                                            },
                                            if a.emitter_name.is_empty() {
                                                format!("emitter {}", a.emitter_idx)
                                            } else {
                                                a.emitter_name.clone()
                                            },
                                            a.fields.count(),
                                        ))
                                        .small(),
                                    );
                                });
                            }
                            for os in &eff.transplants {
                                // Every op here is a transplant; the slot list, when
                                // present, is the one-slot scoping riding on top of it.
                                let operation = if os.one_slot_slots.len() == 1 {
                                    format!("one-slot transplant c{:02}", os.one_slot_slots[0])
                                } else {
                                    "EFF transplant".to_string()
                                };
                                ui.horizontal(|ui| {
                                    ui.add_space(24.0);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{operation}: {} ← {}",
                                            os.new_entry_name, os.src_set_name,
                                        ))
                                        .small()
                                        .color(egui::Color32::from_rgb(190, 160, 255)),
                                    );
                                });
                            }
                        }
                    });
                    ui.add_space(2.0);

                    // Fighter-wide discard (hitbox log only keeps its own remove API)
                    if move_names.len() > 1 {
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            if ui
                                .small_button(format!("↶ Restore all {fighter_display} hitboxes"))
                                .on_hover_text(
                                    "Discard every hitbox edit for this fighter and restore source data",
                                )
                                .clicked()
                            {
                                remove_fighter = Some(fighter_name.clone());
                            }
                        });
                    }
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button("Export All")
                    .on_hover_text("Export every logged hitbox edit to a folder")
                    .clicked()
                {
                    export_all = true;
                }
            });
        });

        if let Some(r) = &edits_response {
            let rect = r.response.rect;
            self.remember_geometry(
                "edits",
                WindowGeometry {
                    x: rect.min.x,
                    y: rect.min.y,
                    w: rect.width(),
                    h: rect.height(),
                },
            );
        }
        self.show_edit_log = open;

        // Apply deferred actions
        if export_all {
            self.export_all_edits();
        }
        if let Some((f, m)) = remove_move {
            let key = format!("{f}/{m}");
            let is_active = self.current_move_key().as_deref() == Some(key.as_str());
            self.state.edit_log.remove_move(&f, &m);
            self.hitbox_rules_store.remove(&key);
            if is_active {
                // The autosave pass runs later in this frame. Restore the working copy now;
                // otherwise it sees the still-edited boxes and immediately recreates the edit
                // the user just discarded.
                self.state.hitboxes = self.state.hitboxes_pristine.clone();
                self.selected_hitbox = None;
                self.hitbox_watch = Some((key.clone(), self.state.hitboxes.clone()));
                self.hitbox_dirty_at = None;
            }
            let all: Vec<crate::game_link::HitboxRuleWire> = self
                .hitbox_rules_store
                .values()
                .flatten()
                .cloned()
                .collect();
            self.game_link.send_hitbox_rules(&all);
            self.state.status = format!("Restored {f}/{m} hitboxes to the source data.");
        }
        if let Some(f) = remove_fighter {
            let active_key = self.current_move_key();
            let active_is_removed = active_key
                .as_deref()
                .is_some_and(|key| key.starts_with(&format!("{f}/")));
            self.state.edit_log.remove_fighter(&f);
            self.hitbox_rules_store
                .retain(|k, _| !k.starts_with(&format!("{f}/")));
            if active_is_removed {
                self.state.hitboxes = self.state.hitboxes_pristine.clone();
                self.selected_hitbox = None;
                if let Some(key) = active_key {
                    self.hitbox_watch = Some((key, self.state.hitboxes.clone()));
                }
                self.hitbox_dirty_at = None;
            }
            let all: Vec<crate::game_link::HitboxRuleWire> = self
                .hitbox_rules_store
                .values()
                .flatten()
                .cloned()
                .collect();
            self.game_link.send_hitbox_rules(&all);
            self.state.status = format!("Restored all {f} hitboxes to their source data.");
        }
        if let Some(key) = remove_call_key {
            self.state.effect_call_edits.remove(&key);
            self.apply_effect_call_edits_to_current();
            self.push_effect_rules(); // discarded disabled-calls stop suppressing
        }
        if let Some(f) = clear_eff_fighter {
            if let Some(eff) = self.eff_mods.remove(&f) {
                // Drop the merged overlay + preview file — the base eff is canonical again.
                let base = self.resolve_eff_source(&eff.source_rel);
                self.eff_editor.set_merged_overlay(&base, None);
                if let Some(dir) = base.parent() {
                    crate::scratch_dirs::remove_transplant_previews(dir);
                }
                self.eff_editor.mark_unsent();
            }
        }
        if let Some(hash) = clear_tweak_hash {
            self.live_overrides.clear_tweak(hash);
        }
        if let Some((fighter, move_name)) = export_move {
            self.export_logged_move(&fighter, &move_name);
        }
    }

    fn export_logged_move(&mut self, fighter: &str, move_name: &str) {
        let record = match self
            .state
            .edit_log
            .entries
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

        let plugin_name = format!(
            "{}_{}_mod",
            fighter,
            move_name.to_lowercase().replace(' ', "_")
        );
        let edits = vec![(
            fighter.to_string(),
            move_name.to_string(),
            record.script.clone(),
        )];
        let project = crate::acmd::build_mod_project(&edits, &plugin_name);
        match write_mod_project(&project, &dest) {
            Ok(root) => self.state.status = format!("Exported project to {}", root.display()),
            Err(e) => self.state.status = format!("Export failed: {}", e),
        }
    }

    fn export_all_edits(&mut self) {
        if self.state.edit_log.is_empty() {
            return;
        }

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

        let edits: Vec<(String, String, crate::data::AcmdScript)> = self
            .state
            .edit_log
            .entries
            .iter()
            .flat_map(|(fighter, moves)| {
                moves.iter().map(move |(move_name, record)| {
                    (fighter.clone(), move_name.clone(), record.script.clone())
                })
            })
            .collect();

        let plugin_name = "visionary_mod";
        let project = crate::acmd::build_mod_project(&edits, plugin_name);
        match write_mod_project(&project, &dest) {
            Ok(root) => {
                self.state.status =
                    format!("Exported {} move(s) to {}", edits.len(), root.display())
            }
            Err(e) => self.state.status = format!("Export failed: {}", e),
        }
    }

    /// Snapshot the current hitboxes/script into the edit log for the active fighter+move —
    /// but only when they actually DIFFER from the pristine load (the log is an edit tree,
    /// not a browsing history).
    fn commit_current_edits(&mut self) {
        let fighter = match self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
        {
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
            self.state.hitboxes_pristine.clone(),
            self.state.hitboxes.clone(),
        );
    }

    fn draw_left_panel(&mut self, ui: &mut Ui) {
        if self.state.data_root.is_none() {
            ui.label(
                egui::RichText::new("Click 'Open Data Root' above").color(egui::Color32::YELLOW),
            );
            ui.label(egui::RichText::new("to load fighter files.").color(egui::Color32::YELLOW));
            return;
        }

        let available = ui.available_height();
        let half = (available - 80.0) / 2.0; // 80 accounts for headings + search bars + separator

        ui.heading("Fighters");
        ui.add(
            egui::TextEdit::singleline(&mut self.fighter_search)
                .hint_text("Search fighters…")
                .desired_width(f32::INFINITY),
        );
        let fighter_query = self.fighter_search.to_lowercase();
        ScrollArea::vertical()
            .id_salt("fighters")
            .max_height(half)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // (index, label, modded, slot count, base slot) — modded fighters and
                // non-vanilla skin counts are called out so an added-character mod is
                // visibly picked up rather than silently indistinguishable from vanilla.
                let fighters: Vec<(usize, String, bool, usize, u8)> = self
                    .state
                    .fighters
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| {
                        fighter_query.is_empty()
                            || f.display_name.to_lowercase().contains(&fighter_query)
                            || f.name.to_lowercase().contains(&fighter_query)
                    })
                    .map(|(i, f)| {
                        (
                            i,
                            f.display_name.clone(),
                            f.is_modded(),
                            f.slots.len(),
                            f.base_slot(),
                        )
                    })
                    .collect();
                for (i, name, modded, slot_count, base) in fighters {
                    let selected = self.state.selected_fighter == Some(i);
                    let mut label = name.clone();
                    if modded {
                        label.push_str("  [mod]");
                    }
                    if slot_count != crate::data::VANILLA_SLOT_COUNT as usize {
                        label.push_str(&format!("  ({slot_count} skins)"));
                    }
                    let mut hover = format!("{slot_count} costume slot(s), base c{base:02}");
                    if modded {
                        hover.push_str("\nModded character (not in the vanilla roster)");
                    }
                    if ui
                        .selectable_label(selected, &label)
                        .on_hover_text(hover)
                        .clicked()
                        && !selected
                    {
                        self.select_fighter(i);
                    }
                }
            });

        ui.separator();
        ui.heading("Moves");
        ui.add(
            egui::TextEdit::singleline(&mut self.move_search)
                .hint_text("Search moves…")
                .desired_width(f32::INFINITY),
        );
        let move_query = self.move_search.to_lowercase();
        ScrollArea::vertical()
            .id_salt("moves")
            .max_height(half)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Group the (filtered) moves into the familiar move families, preserving order.
                let mut groups: Vec<Vec<MoveEntry>> = (0..MOVE_CATEGORY_LABELS.len())
                    .map(|_| Vec::new())
                    .collect();
                for m in self.move_list.iter().filter(|m| {
                    move_query.is_empty()
                        || m.name.to_lowercase().contains(&move_query)
                        || format_move_name(&m.name)
                            .to_lowercase()
                            .contains(&move_query)
                }) {
                    groups[move_category_index(&m.name)].push(m.clone());
                }
                let mut to_select: Option<MoveEntry> = None;
                for (ci, group) in groups.iter().enumerate() {
                    if group.is_empty() {
                        continue;
                    }
                    let mut header = egui::CollapsingHeader::new(format!(
                        "{} ({})",
                        MOVE_CATEGORY_LABELS[ci],
                        group.len()
                    ))
                    .id_salt(("movecat", ci))
                    // Start collapsed: a full move list is hundreds of entries, and opening
                    // every category by default buries the categories themselves.
                    .default_open(false);
                    if !move_query.is_empty() {
                        // While searching, force categories open — otherwise the matches the
                        // user just filtered for stay hidden behind a collapsed header.
                        header = header.open(Some(true));
                    }
                    header.show(ui, |ui| {
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
                let btn_text = if self.fetching_acmd {
                    "..."
                } else {
                    "Fetch ACMD"
                };
                if ui
                    .add_enabled(!self.fetching_acmd, egui::Button::new(btn_text))
                    .on_hover_text("Fetch hitboxes from GitHub ACMD scripts")
                    .clicked()
                {
                    self.fetch_acmd();
                }
                let has_capture = self
                    .current_motion_hash()
                    .map(|m| !self.captures_for_selected_fighter(m).is_empty())
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
                if ui
                    .add_enabled(has_capture, egui::Button::new("⌫"))
                    .on_hover_text(
                        "Forget the locked live-ACMD snapshots. Each move is captured from its \
                         first playback only; clear when you intentionally want to \
                         record a fresh set.",
                    )
                    .clicked()
                {
                    self.game_link.clear_captures();
                    self.state.status = "Cleared the live capture — perform the move again \
                                         to record a clean run."
                        .into();
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
                ui.horizontal(|ui| {
                    ui.label("Type:");
                    egui::ComboBox::from_id_salt("add_hitbox_category")
                        .selected_text(match self.add_hitbox_category {
                            1 => "Grab",
                            2 => "Wind",
                            _ => "Attack",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.add_hitbox_category, 0, "Attack");
                            ui.selectable_value(&mut self.add_hitbox_category, 1, "Grab");
                            ui.selectable_value(&mut self.add_hitbox_category, 2, "Wind");
                        });
                    if self.add_hitbox_category == 2 {
                        egui::ComboBox::from_id_salt("add_wind_shape")
                            .selected_text(if self.add_wind_radial {
                                "Radial"
                            } else {
                                "Rectangle"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.add_wind_radial, false, "Rectangle");
                                ui.selectable_value(&mut self.add_wind_radial, true, "Radial");
                            });
                    }
                });
                if self.add_hitbox_category != 2 {
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
                }
                let size_label = match self.add_hitbox_category {
                    1 => "Grab size",
                    2 if self.add_wind_radial => "Radius",
                    2 => "Half-size",
                    _ => "Size",
                };
                ui.add(egui::Slider::new(&mut self.add_size, 0.1..=50.0).text(size_label));
                if self.add_hitbox_category == 0 {
                    ui.add(egui::Slider::new(&mut self.add_damage, 0.0..=50.0).text("Damage"));
                    angle_picker(ui, &mut self.add_angle);
                    ui.add(egui::Slider::new(&mut self.add_kb_base, 0..=200).text("KB Base"));
                    ui.add(egui::Slider::new(&mut self.add_kb_scaling, 0..=200).text("KB Scaling"));
                }
                let add_label = match self.add_hitbox_category {
                    1 => "Add Grab Box",
                    2 => "Add Wind Box",
                    _ => "Add Attack Hitbox",
                };
                if ui.button(add_label).clicked() {
                    let next_id = self
                        .state
                        .hitboxes
                        .iter()
                        .filter(|hitbox| hitbox.category == self.add_hitbox_category)
                        .map(|h| h.id)
                        .max()
                        .map(|m| m + 1)
                        .unwrap_or(0);
                    let active_start = self.state.current_frame.max(FIRST_GAME_FRAME);
                    let active_end = active_start.saturating_add(5);
                    let mut hb = Hitbox {
                        id: next_id,
                        bone_name: self.add_bone.clone(),
                        damage: self.add_damage,
                        angle: self.add_angle,
                        kb_scaling: self.add_kb_scaling,
                        kb_base: self.add_kb_base,
                        size: self.add_size,
                        active_start,
                        active_end,
                        category: self.add_hitbox_category,
                        ..Default::default()
                    };
                    if self.add_hitbox_category == 1 {
                        hb.damage = 0.0;
                        hb.angle = 0;
                        hb.kb_scaling = 0;
                        hb.kb_base = 0;
                    } else if self.add_hitbox_category == 2 {
                        let args = if self.add_wind_radial {
                            vec![
                                next_id as f32,
                                1.0,
                                0.0,
                                1000.0,
                                1.0,
                                0.0,
                                0.0,
                                self.add_size,
                            ]
                        } else {
                            vec![
                                next_id as f32,
                                1.0,
                                0.0,
                                1000.0,
                                1.0,
                                0.0,
                                0.0,
                                self.add_size * 2.0,
                                self.add_size * 2.0,
                            ]
                        };
                        let wind = crate::data::WindboxData {
                            command: if self.add_wind_radial {
                                "AREA_WIND_2ND_RAD".into()
                            } else {
                                "AREA_WIND_2ND".into()
                            },
                            args,
                        };
                        hb = wind.to_hitbox(active_start);
                        hb.active_end = active_end;
                    }
                    self.state.hitboxes.push(hb);
                    self.selected_hitbox = self.state.hitboxes.len().checked_sub(1);
                    self.show_add_hitbox = false;
                }
            });
        }

        let menu_height = ui.available_height();
        hitbox_menu_scroll_area(menu_height).show(ui, |ui| {
            let mut to_delete = None;
            for (i, hb) in self.state.hitboxes.iter().enumerate() {
                let color = hitbox_display_color(hb);
                let selected = self.selected_hitbox == Some(i);
                ui.horizontal(|ui| {
                    ui.colored_label(color, "*");
                    let shape = if hb.category == 2 {
                        if hb.wind.as_ref().is_some_and(|wind| wind.is_radial()) {
                            "◯"
                        } else {
                            "▭"
                        }
                    } else if hb.capsule_end.is_some() {
                        "⬭"
                    } else {
                        "●"
                    };
                    let label = match hb.category {
                        1 => format!(
                            "{} GRAB #{} {} [{}-{}]",
                            shape, hb.id, hb.bone_name, hb.active_start, hb.active_end
                        ),
                        2 => format!(
                            "{} WIND #{} {} [{}-{}]",
                            shape,
                            hb.id,
                            hb.wind
                                .as_ref()
                                .map(|wind| if wind.is_radial() {
                                    format!("RAD r{:.1}", wind.radius())
                                } else {
                                    let [width, height] = wind.dimensions();
                                    format!("RECT {width:.1}×{height:.1}")
                                })
                                .unwrap_or_else(|| format!("~{:.1}", hb.size)),
                            hb.active_start,
                            hb.active_end
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
                    if ui
                        .small_button("🗑")
                        .on_hover_text("Delete this collision box")
                        .clicked()
                    {
                        to_delete = Some(i);
                    }
                });
            }
            if let Some(i) = to_delete {
                self.state.hitboxes.remove(i);
                self.selected_hitbox = match self.selected_hitbox {
                    Some(selected) if selected == i => None,
                    Some(selected) if selected > i => Some(selected - 1),
                    selected => selected,
                };
            }

            // Property editor for selected hitbox — fields shown depend on the collision
            // family. It shares this one full-height scrollbar with the collision list.
            if let Some(idx) = self.selected_hitbox {
                let bone_names = self.bone_names.clone();
                let max_frame = self
                    .state
                    .total_frames
                    .max(timeline_frame_extent(
                        &self.state.hitboxes,
                        &self.state.effects,
                    ))
                    .max(FIRST_GAME_FRAME);
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
                    // Wind areas are object-relative 2D regions rather than bone-local attack
                    // spheres. Attack/grab retain the normal bone selector.
                    if hb.category != 2 {
                        ui.horizontal(|ui| {
                            ui.label("Bone:");
                            if bone_names.is_empty() {
                                ui.text_edit_singleline(&mut hb.bone_name);
                            } else {
                                egui::ComboBox::from_id_salt("edit_bone_select")
                                    .selected_text(&hb.bone_name)
                                    .show_ui(ui, |ui| {
                                        for name in &bone_names {
                                            ui.selectable_value(
                                                &mut hb.bone_name,
                                                name.clone(),
                                                name,
                                            );
                                        }
                                    });
                            }
                        });
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

                    // ── Size + position/shape ────────────────────────────
                    if hb.category == 2 {
                        if let Some(wind) = hb.wind.as_mut().filter(|wind| wind.is_valid()) {
                            let radial = wind.is_radial();
                            ui.horizontal(|ui| {
                                ui.label("ID:");
                                let mut id = wind.id();
                                if ui
                                    .add(egui::DragValue::new(&mut id).range(0..=255))
                                    .changed()
                                {
                                    wind.args[0] = id as f32;
                                }
                            });
                            ui.collapsing("Wind Physics", |ui| {
                                wide_slider_f32(ui, &mut wind.args[1], 0.0..=5.0, "Strength");
                                let second = if radial {
                                    "Radial Falloff"
                                } else {
                                    "Direction (degrees)"
                                };
                                wide_slider_f32(ui, &mut wind.args[2], -360.0..=360.0, second);
                                wide_slider_f32(ui, &mut wind.args[3], 0.0..=1000.0, "Speed Limit");
                                wide_slider_f32(ui, &mut wind.args[4], 0.0..=5.0, "Acceleration");
                            });
                            ui.collapsing("Position / Shape", |ui| {
                                wide_slider_f32(ui, &mut wind.args[5], -50.0..=50.0, "Offset X");
                                wide_slider_f32(ui, &mut wind.args[6], -50.0..=50.0, "Offset Y");
                                if radial {
                                    wide_slider_f32(ui, &mut wind.args[7], 0.1..=100.0, "Radius");
                                } else {
                                    wide_slider_f32(ui, &mut wind.args[7], 0.1..=100.0, "Width");
                                    wide_slider_f32(ui, &mut wind.args[8], 0.1..=100.0, "Height");
                                }
                            });
                            if wind.has_lifetime() {
                                let last = wind.args.len() - 1;
                                wide_slider_f32(ui, &mut wind.args[last], 0.0..=600.0, "Lifetime");
                            } else {
                                ui.label(
                                    egui::RichText::new("Lifetime: until AreaModule::erase_wind")
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            }
                            hb.id = wind.id();
                            let [x, y] = wind.offset();
                            hb.offset_x = x;
                            hb.offset_y = y;
                            hb.size = if radial {
                                wind.radius()
                            } else {
                                let [width, height] = wind.dimensions();
                                width.max(height) * 0.5
                            };
                        } else {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "This legacy project has no wind payload. Fetch the move again.",
                            );
                        }
                    } else {
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
                    }

                    // ── Attack-only detail sections ──────────────────────
                    if hb.category == 0 {
                        ui.collapsing("Hit Properties", |ui| {
                            wide_slider_f32(ui, &mut hb.hitlag_mult, 0.0..=5.0, "Hitlag Mult");
                            wide_slider_f32(ui, &mut hb.sdi_mult, 0.0..=5.0, "SDI Mult");
                            wide_slider_f32(ui, &mut hb.hitbox_attr, -10.0..=10.0, "Hitbox Attr");
                            ui.add(
                                egui::DragValue::new(&mut hb.is_add_attack).prefix("Add Attack: "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut hb.ground_or_air).prefix("Ground/Air: "),
                            );

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

                    // ── Timeline (all families) ──────────────────────────
                    wide_slider_u32(
                        ui,
                        &mut hb.active_start,
                        FIRST_GAME_FRAME..=max_frame,
                        "Start Frame",
                    );
                    wide_slider_u32(
                        ui,
                        &mut hb.active_end,
                        FIRST_GAME_FRAME..=max_frame,
                        "End Frame",
                    );
                }
            }
        });
    }

    fn draw_effects_panel(&mut self, ui: &mut Ui) {
        let current = self.state.current_frame;

        ui.horizontal(|ui| {
            ui.heading("Effect spawns");
            ui.label(
                egui::RichText::new(format!("— Frame {}", current))
                    .color(egui::Color32::LIGHT_GRAY),
            );
        });
        ui.checkbox(&mut self.state.show_all_effect_calls, "show all frames");
        ui.separator();

        let has_effect_data =
            !self.state.effect_script.stmts.is_empty() || !self.state.effects.is_empty();

        if !has_effect_data {
            ui.colored_label(egui::Color32::GRAY, "Effect data unavailable");
            ui.label(
                egui::RichText::new("Fetch ACMD to load effect data.")
                    .small()
                    .color(egui::Color32::DARK_GRAY),
            );
        } else {
            let visible: Vec<usize> = self
                .state
                .effects
                .iter()
                .enumerate()
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
                egui::ScrollArea::vertical()
                    .id_salt("effects_list")
                    .max_height(180.0)
                    .show(ui, |ui| {
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
                                let display_name = effect_call_display_name(effect);
                                let mut text = egui::RichText::new(display_name).monospace();
                                if effect.disabled {
                                    text = text.strikethrough().color(egui::Color32::DARK_GRAY);
                                }
                                if ui
                                    .selectable_label(selected, text)
                                    .on_hover_text(format!(
                                        "{} · bone {} · f{}-{}",
                                        effect.spawn_func,
                                        effect.bone_name,
                                        effect.active_start,
                                        effect.active_end
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
                    effect_name_alt: None,
                    spawn_func: "EFFECT_FOLLOW".into(),
                    bone_name: "top".into(),
                    offset: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: 1.0,
                    follows_bone: true,
                    active_start: current,
                    active_end: current.saturating_add(10),
                    disabled: false,
                    // No originating macro — exports emit the plain EFFECT_FOLLOW form.
                    extra_args: None,
                    raw_line: None,
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
                            pristine: None,
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
                    // A trail is placed by the joints it names — its call has no position,
                    // rotation, or scale arguments at all. Offering those fields let the user
                    // drag values that no export and no write-back could ever put anywhere.
                    let is_trail = ec.raw_line.is_some();
                    let orig = |ui: &mut Ui, txt: String| {
                        ui.label(egui::RichText::new(txt).small().color(egui::Color32::GRAY));
                    };
                    egui::Grid::new("effect_call_edit")
                        .num_columns(3)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.label("Effect");
                            ui.horizontal(|ui| {
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut ec.effect_name)
                                            .desired_width(120.0),
                                    )
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
                                ui.label(
                                    egui::RichText::new("added")
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            }
                            ui.end_row();

                            if let Some(effect_name_alt) = ec.effect_name_alt.as_mut() {
                                ui.label("Flip effect");
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(effect_name_alt)
                                            .desired_width(140.0),
                                    )
                                    .on_hover_text(
                                        "Alternate graphic selected by the ACMD flip command",
                                    )
                                    .changed();
                                if let Some(p) = &pristine {
                                    orig(
                                        ui,
                                        format!(
                                            "orig {}",
                                            p.effect_name_alt.as_deref().unwrap_or("none")
                                        ),
                                    );
                                } else {
                                    ui.label("");
                                }
                                ui.end_row();
                            }

                            ui.label("Spawn command");
                            let spawn_command = if ec.spawn_func.is_empty() {
                                if ec.follows_bone {
                                    "EFFECT_FOLLOW (legacy)"
                                } else {
                                    "EFFECT (legacy)"
                                }
                            } else {
                                &ec.spawn_func
                            };
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(spawn_command).monospace())
                                    .on_hover_text(
                                        "The exact ACMD function this spawn came from. Its \
                                         alpha, attribute, contact, random, flip, and no-stop \
                                         arguments are preserved both when the spawn is \
                                         replayed live and when the mod is exported.",
                                    );
                                // A flipped spawn carries a second graphic that has no field of
                                // its own. Show it, so renaming the first one is not mistaken
                                // for renaming the effect on both sides of the flip.
                                if let Some(alt) = &ec.effect_name_alt {
                                    ui.label(
                                        egui::RichText::new(format!("flipped: {alt}"))
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    )
                                    .on_hover_text(
                                        "The graphic this macro uses for the other facing. \
                                         Preserved as-is by live replay and export.",
                                    );
                                }
                            });
                            if let Some(p) = &pristine {
                                orig(
                                    ui,
                                    format!(
                                        "orig {}",
                                        if p.spawn_func.is_empty() {
                                            "legacy"
                                        } else {
                                            &p.spawn_func
                                        }
                                    ),
                                );
                            } else {
                                ui.label("");
                            }
                            ui.end_row();

                            ui.label("Bone");
                            if bone_names.is_empty() {
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut ec.bone_name)
                                            .desired_width(140.0),
                                    )
                                    .changed();
                            } else {
                                egui::ComboBox::from_id_salt("effect_bone_select")
                                    .selected_text(&ec.bone_name)
                                    .width(140.0)
                                    .show_ui(ui, |ui| {
                                        for name in &bone_names {
                                            if ui
                                                .selectable_value(
                                                    &mut ec.bone_name,
                                                    name.clone(),
                                                    name,
                                                )
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

                            if is_trail {
                                ui.label("Transform");
                                ui.label(
                                    egui::RichText::new("set by its joints")
                                        .small()
                                        .color(egui::Color32::GRAY),
                                )
                                .on_hover_text(
                                    "A trail has no position, rotation, or scale arguments — it \
                                     is drawn between the joints named in the call. Change the \
                                     joint above to move it.",
                                );
                                ui.label("");
                                ui.end_row();
                            } else {
                                ui.label("Offset");
                                ui.horizontal(|ui| {
                                    for v in ec.offset.iter_mut() {
                                        changed |=
                                            ui.add(egui::DragValue::new(v).speed(0.05)).changed();
                                    }
                                });
                                if let Some(p) = &pristine {
                                    orig(
                                        ui,
                                        format!(
                                            "orig [{:.2} {:.2} {:.2}]",
                                            p.offset[0], p.offset[1], p.offset[2]
                                        ),
                                    );
                                } else {
                                    ui.label("");
                                }
                                ui.end_row();

                                ui.label("Rotation");
                                ui.horizontal(|ui| {
                                    for v in ec.rotation.iter_mut() {
                                        changed |=
                                            ui.add(egui::DragValue::new(v).speed(0.5)).changed();
                                    }
                                });
                                if let Some(p) = &pristine {
                                    orig(
                                        ui,
                                        format!(
                                            "orig [{:.1} {:.1} {:.1}]",
                                            p.rotation[0], p.rotation[1], p.rotation[2]
                                        ),
                                    );
                                } else {
                                    ui.label("");
                                }
                                ui.end_row();

                                ui.label("Scale");
                                changed |= ui
                                    .add(egui::DragValue::new(&mut ec.scale).speed(0.02))
                                    .changed();
                                if let Some(p) = &pristine {
                                    orig(ui, format!("orig {:.2}", p.scale));
                                } else {
                                    ui.label("");
                                }
                                ui.end_row();
                            }

                            // One-shot effects have no meaningful "end" (they play their own
                            // lifetime), so only follow effects show an end frame — otherwise the
                            // row showed confusing "30-30" or "30-9999" ranges.
                            ui.label("Spawn frame");
                            ui.horizontal(|ui| {
                                changed |=
                                    ui.add(egui::DragValue::new(&mut ec.active_start)).changed();
                                if ec.follows_bone {
                                    ui.label("→ until");
                                    changed |=
                                        ui.add(egui::DragValue::new(&mut ec.active_end)).changed();
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

                // ── Foreign-effect warning: effect folders only load with their OWNER
                // (fighter in match / assist summoned), so a spawn naming another
                // character's effect is invisible both live and in an exported mod.
                // EFF transplanting bakes the donor content into THIS fighter's eff instead.
                {
                    let name = self.state.effects[i].effect_name.to_lowercase();
                    let fighter = self
                        .state
                        .selected_fighter
                        .and_then(|fi| self.state.fighters.get(fi))
                        .map(|f| f.name.clone())
                        .unwrap_or_default();
                    let own = !fighter.is_empty() && name.starts_with(&format!("{fighter}_"));
                    let is_baked_copy = self
                        .eff_mods
                        .get(&fighter)
                        .map(|e| e.transplants.iter().any(|op| op.new_entry_name == name))
                        .unwrap_or(false);
                    if !name.is_empty()
                        && !name.starts_with("sys_")
                        && !name.starts_with("0x")
                        && !own
                        && !is_baked_copy
                    {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 170, 60),
                                "⚠ foreign effect — it belongs to another character. \
                                 Transplant it into this fighter's EFF for the \
                                 export, and loaded as a stripped-down copy for the live \
                                 preview.",
                            );
                            match self
                                .effect_pool
                                .as_ref()
                                .and_then(|p| p.file_of_entry(&name))
                            {
                                Some(rel) => {
                                    if ui
                                        .small_button(format!("Transplant into {fighter}"))
                                        .on_hover_text(
                                            "Transplant the donor entry into this fighter (baked \
                                             into the exported eff; a stripped copy loaded \
                                             live), then redirect this spawn to it.",
                                        )
                                        .clicked()
                                    {
                                        self.transplant_sel = Some((rel, name.clone()));
                                        self.transplant_new_name = format!("{name}{}", crate::mod_project::TRANSPLANT_SUFFIX);
                                        self.transplant_target = Some(fighter.clone());
                                        self.show_transplant = true;
                                    }
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new(
                                            "(owning eff not found yet — let the effect scan finish)",
                                        )
                                        .small()
                                        .color(egui::Color32::GRAY),
                                    );
                                }
                            }
                        });
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
                                pristine: None,
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
                    if respawn_needed {
                        // A foreign effect name may need its eff co-loaded in-game.
                        self.push_effect_aliases();
                    }
                }
            }
        }

        ui.separator();

        // VFX file check
        let fighter_name = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone());

        if let (Some(name), Some(root)) = (fighter_name, &self.state.data_root) {
            // Check common locations for the .eff file
            let candidates = [
                root.join("effect")
                    .join("fighter")
                    .join(&name)
                    .join(format!("ef_{}.eff", name)),
                root.join("fighter")
                    .join(&name)
                    .join("effect")
                    .join(format!("ef_{}.eff", name)),
            ];
            let found = candidates.iter().find(|p| p.exists());
            if found.is_some() {
                ui.colored_label(egui::Color32::from_rgb(100, 220, 100), "VFX file: present");
            } else if self.current_eff_path.is_some() {
                ui.colored_label(
                    egui::Color32::from_rgb(100, 220, 100),
                    "VFX file: loaded manually",
                );
            } else {
                ui.colored_label(egui::Color32::GRAY, "VFX file: not found");
                ui.label(
                    egui::RichText::new("Extract effect/fighter/ from data.arc, or:")
                        .small()
                        .color(egui::Color32::DARK_GRAY),
                );
                if ui.button(format!("Browse for ef_{}.eff…", name)).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Effect file", &["eff"])
                        .set_title(format!("Open ef_{}.eff", name))
                        .pick_file()
                    {
                        self.open_external_eff(path);
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
        let scanning = self
            .effect_pool
            .as_mut()
            .map(|p| p.tick(6))
            .unwrap_or(false);

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

            egui::ScrollArea::vertical()
                .max_height(180.0)
                .show(ui, |ui| {
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
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(30));
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
        let Some(mv) = self.current_move_key() else {
            return;
        };
        let Some(edits) = self.state.effect_call_edits.get(&mv) else {
            return;
        };
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

    /// Reapply the saved hitbox/script record for the move that is currently open. The source
    /// loader always establishes `hitboxes_pristine` first, so live rules still compare against
    /// the game's real script rather than against a previously edited project snapshot.
    fn apply_saved_hitbox_edits_to_current(&mut self) -> bool {
        let Some(key) = self.current_move_key() else {
            return false;
        };
        let Some((fighter, move_name)) = key.split_once('/') else {
            return false;
        };
        let Some(record) = self
            .state
            .edit_log
            .entries
            .get(fighter)
            .and_then(|moves| moves.get(move_name))
            .cloned()
        else {
            return false;
        };
        if self.state.hitboxes_pristine.is_empty() && !record.hitboxes_pristine.is_empty() {
            self.state.hitboxes_pristine = record.hitboxes_pristine;
        }
        self.state.script = record.script;
        self.state.hitboxes = record.hitboxes;
        true
    }

    /// Fold the eff editor's current diff into the per-fighter project store.
    fn sync_eff_mods_from_editor(&mut self) {
        let Some(rel) = self.eff_editor.loaded_rel() else {
            return;
        };
        let authored = self.eff_editor.collect_authored_edits();
        let rosters = self.eff_editor.collect_rosters();
        let entry_edits = self.eff_editor.collect_entry_edits();
        let fighter = crate::mod_project::fighter_from_source_rel(&rel);
        let entry = self.eff_mods.entry(fighter.clone()).or_default();
        // NEVER key the project to the transient merged preview — rebuilding from it
        // would re-apply the transplant ops onto an already-merged file.
        if !crate::scratch_dirs::is_transplant_preview_name(&rel) {
            entry.source_rel = rel;
        } else if entry.source_rel.is_empty() {
            entry.source_rel = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
        }
        entry.authored = authored; // transplant records are preserved as-is
        entry.rosters = rosters;
        entry.entry_edits = entry_edits;
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
                let Some(tweak) = live_tweak_from_override(&form) else {
                    continue;
                };
                let mut owners: Vec<(String, String, Vec<crate::data::EffectCall>)> = self
                    .state
                    .effect_call_full
                    .iter()
                    .filter(|(_, calls)| {
                        calls
                            .iter()
                            .any(|c| effect_name_hash(&c.effect_name) == hash)
                    })
                    .filter_map(|(key, calls)| {
                        key.split_once('/').map(|(fighter, move_name)| {
                            (fighter.to_string(), move_name.to_string(), calls.clone())
                        })
                    })
                    .collect();
                owners.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
                owners.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
                if owners.is_empty() {
                    if let Some(f) = &selected {
                        let fm = project.fighters.entry(f.clone()).or_default();
                        if !fm
                            .live_tweaks
                            .iter()
                            .any(|existing| existing.effect_name == tweak.effect_name)
                        {
                            fm.live_tweaks.push(tweak.clone());
                        }
                    }
                }
                for (fighter, move_name, calls) in owners {
                    let fm = project.fighters.entry(fighter).or_default();
                    // A live tweak is emitted immediately after each affected ACMD spawn. Keep
                    // the complete effect script even when the spawn itself was not edited, or
                    // a tweak-only project would serialize correctly but export no code.
                    fm.effect_calls_full.entry(move_name).or_insert(calls);
                    if !fm
                        .live_tweaks
                        .iter()
                        .any(|t| t.effect_name == tweak.effect_name)
                    {
                        fm.live_tweaks.push(tweak.clone());
                    }
                }
            }
        }
        project
    }

    fn export_project(&mut self) {
        let project = self.build_project();
        if project.is_empty() {
            self.state.status = "No edits to export yet.".into();
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export editable mod project…")
            .set_file_name(crate::mod_project::PROJECT_FILE_NAME)
            .add_filter("Mod project", &["json"]);
        if let Some(dir) = &self.export_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(crate::mod_export::slugify)
            .unwrap_or_else(|| "project".into());
        let asset_dir = format!("{stem}_assets");
        match crate::mod_export::write_portable_project(&project, &path, &asset_dir) {
            Ok(()) => self.state.status = format!("Project exported to {}", path.display()),
            Err(e) => self.state.status = format!("Project export failed: {e}"),
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

    /// Export one directly installable ARCropolis mod folder. The generated Skyline plugin
    /// supplies ACMD/effect-spawn edits and rebuilt EFF files live beside it in the same mod.
    fn export_full_mod(&mut self) {
        self.export_mod(false);
    }

    /// Export editable artifacts with rebuilt effect data and Rust ACMD source in clearly
    /// separate folders. No automatic build is started for this developer-facing path.
    fn export_developer_files(&mut self) {
        self.export_mod(true);
    }

    fn export_mod(&mut self, developer: bool) {
        let project = self.build_project();
        if project.is_empty() {
            self.state.status = "No edits to export yet.".into();
            return;
        }
        let title = if developer {
            "Export developer files into folder…"
        } else {
            "Export ARCropolis mod folder…"
        };
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some(dir) = &self.export_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(parent) = dialog.pick_folder() else {
            return;
        };
        self.export_dir = Some(parent.clone());
        save_config_path("export_dir", &parent);
        let slug = crate::mod_export::slugify(&project.name);
        let export_name = if developer {
            format!("{slug}_developer_files")
        } else {
            slug.clone()
        };
        let dest = unused_export_root(&parent, &export_name);

        let mut report: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        // 1. Data mod: rebuilt eff files under effect/fighter/<name>/… + info.toml.
        //    One-slot-scoped transplants additionally write ef_<fighter>_cXX.eff per slot
        //    (the "One-Slot Effects" plugin's file naming; the slotted file replaces the
        //    base file for that costume, so it carries the unscoped transplants too).
        let mod_dir = if developer {
            dest.join("effect_mod")
        } else {
            dest.clone()
        };
        let has_eff = project
            .fighters
            .values()
            .any(|fighter| fighter.eff.as_ref().is_some_and(|eff| !eff.is_empty()));
        for (fighter, fm) in &project.fighters {
            match &fm.eff {
                Some(eff) if !eff.is_empty() => {
                    let src_path = self.resolve_eff_source(&eff.source_rel);
                    let root = self.eff_editor.export_root().to_path_buf();
                    let write_variant = |slot: Option<u8>| -> anyhow::Result<()> {
                        let bytes = std::fs::read(&src_path)?;
                        let rebuilt = crate::eff_export::rebuild_eff_bytes_for_slot(
                            &bytes,
                            eff,
                            Some(&root),
                            slot,
                        )?;
                        let rel = match slot {
                            None => eff.source_rel.clone(),
                            Some(s) => {
                                // effect/fighter/mario/ef_mario.eff → …/ef_mario_c0X.eff
                                let p = std::path::Path::new(&eff.source_rel);
                                let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("ef");
                                let file = format!("{stem}_c{s:02}.eff");
                                p.parent()
                                    .map(|d| d.join(&file).to_string_lossy().replace('\\', "/"))
                                    .unwrap_or(file)
                            }
                        };
                        let out = mod_dir.join(&rel);
                        if let Some(parent) = out.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&out, rebuilt)?;
                        Ok(())
                    };
                    // Base file: authored edits + costume-unscoped ops. Skip only when
                    // literally nothing lands in it.
                    let base_has_content = crate::mod_export::base_eff_has_content(eff);
                    let mut ok = true;
                    if base_has_content {
                        if let Err(e) = write_variant(None) {
                            errors.push(format!("{fighter} eff: {e}"));
                            ok = false;
                        }
                    }
                    // One-slot-scoped files: union of every transplant's costume slots.
                    let mut slots: Vec<u8> = eff
                        .transplants
                        .iter()
                        .flat_map(|op| op.one_slot_slots.iter().copied())
                        .collect();
                    slots.sort();
                    slots.dedup();
                    for s in &slots {
                        if let Err(e) = write_variant(Some(*s)) {
                            errors.push(format!("{fighter} eff c{s:02}: {e}"));
                            ok = false;
                        }
                    }
                    if ok {
                        let mut msg = format!(
                            "{fighter}: eff written ({} authored, {} transplant(s)",
                            eff.authored.len(),
                            eff.transplants.len()
                        );
                        if !slots.is_empty() {
                            msg.push_str(&format!(
                                ", skin EFFs: {}",
                                slots
                                    .iter()
                                    .map(|s| format!("c{s:02}"))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            ));
                        }
                        msg.push(')');
                        report.push(msg);
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
        let plugin_name = crate::mod_export::plugin_name(&project);
        let generated_source = match crate::mod_export::source_project(&project) {
            Ok(source) => source,
            Err(error) => {
                self.state.status = format!("Export failed: {error}");
                return;
            }
        };
        let has_source = generated_source.is_some();
        let mut source_root: Option<std::path::PathBuf> = None;
        if let Some(src_project) = generated_source {
            let root = if developer {
                dest.join("acmd_source")
            } else {
                crate::scratch_dirs::app_storage_root()
                    .join("export-source")
                    .join(&plugin_name)
            };
            match crate::mod_export::write_source_project(&src_project, &root) {
                Ok(()) => {
                    report.push(format!(
                        "ACMD source: {} move script(s), {} effect script(s) — verified to \
                         read back as the moves in the editor",
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

        // info.toml sits beside rebuilt EFF files so Arcropolis can load that folder directly.
        if has_eff || has_source {
            let _ = std::fs::create_dir_all(&mod_dir);
            let display_name = serde_json::to_string(&self.project_name)
                .unwrap_or_else(|_| "\"Visionary mod\"".into());
            let info = format!(
                "display_name = {display_name}\nauthors = \"Visionary\"\nversion = \"1.0.0\"\ndescription = \"Exported by Visionary\"\ncategory = \"Misc\"\n"
            );
            if let Err(e) = std::fs::write(mod_dir.join("info.toml"), info) {
                errors.push(format!("info.toml: {e}"));
            }
        }

        // Top-level README: what goes where.
        let readme = if developer {
            let effect_note = if has_eff {
                "`effect_mod/` contains the rebuilt Arcropolis effect files.\n"
            } else {
                "This project has no EFF data edits, so no effect mod folder is needed.\n"
            };
            let source_note = if has_source {
                "`acmd_source/` contains the editable Skyline/Smashline Rust project for hitbox and effect-spawn edits.\n\
                 Build it with `acmd_source/build.bat` on Windows or `bash acmd_source/build.sh` on Linux.\n"
            } else {
                "This project has no hitbox or effect-spawn edits, so no ACMD source folder is needed.\n"
            };
            format!(
                "# {name} developer files\n\n\
                 {effect_note}\
                 {source_note}",
                name = self.project_name,
            )
        } else {
            let plugin_note = if has_source {
                "ARCropolis chainloads the generated ACMD plugin from this mod's root `plugin.nro`."
            } else {
                "This project has no ACMD edits, so this mod does not need a Skyline plugin."
            };
            let copy_note =
                format!("Copy this entire `{slug}/` folder into `<SD root>/ultimate/mods/`.");
            let effect_note = if has_eff {
                "ARCropolis loads the effect files from this mod's `effect/` folder.".to_string()
            } else {
                "This project has no Arcropolis effect files.".to_string()
            };
            format!(
                "# {name}\n\n\
                 {copy_note}\n\n\
                 {effect_note} {plugin_note}\n",
                name = self.project_name,
            )
        };
        let _ = std::fs::write(dest.join("README.md"), readme);

        // 3. Mod-folder exports compile the generated plugin in the background and keep the NRO
        // inside that ARCropolis mod. Developer exports intentionally stop at editable source.
        if !developer && errors.is_empty() {
            if let Some(src_root) = source_root {
                let nro = dest.join("plugin.nro");
                self.spawn_export_build(dest.clone(), src_root, plugin_name, nro);
            }
        }

        self.state.status = if errors.is_empty() {
            let result = if developer {
                "Developer files exported"
            } else if has_source {
                "Mod folder staged"
            } else {
                "Mod folder ready"
            };
            format!(
                "{result} at {} — {}{}",
                dest.display(),
                report.join(" · "),
                if has_source && !developer {
                    " · building plugin…"
                } else {
                    ""
                }
            )
        } else {
            format!(
                "Mod export finished with errors: {} — {}",
                errors.join("; "),
                report.join(" · ")
            )
        };
    }

    /// Run `cargo skyline build --release` on generated source in a background thread.
    /// Progress lands in `self.export_build`; the finished NRO is copied inside the mod folder.
    fn spawn_export_build(
        &mut self,
        dest: std::path::PathBuf,
        src_root: std::path::PathBuf,
        plugin_name: String,
        nro_destination: std::path::PathBuf,
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
                            .join("target")
                            .join("aarch64-skyline-switch")
                            .join("release")
                            .join(format!("lib{plugin_name}.nro"));
                        let parent = nro_destination.parent().unwrap_or(&dest);
                        let _ = std::fs::create_dir_all(parent);
                        match std::fs::copy(&nro, &nro_destination) {
                            Ok(_) => format!("Mod folder ready → {}", dest.display()),
                            Err(e) => format!(
                                "plugin built but the mod folder is incomplete ({e}) — see {}",
                                dest.join("build.log").display()
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
        let mut project: crate::mod_project::ModProjectFile = match std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|s| serde_json::from_str(&s).map_err(anyhow::Error::from))
        {
            Ok(p) => p,
            Err(e) => {
                self.state.status = format!("Project load failed: {e}");
                return;
            }
        };
        if let Err(error) = crate::mod_export::validate_project(&project)
            .and_then(|_| crate::mod_export::resolve_project_assets(&mut project, path))
        {
            self.state.status = format!("Project load failed: {error}");
            return;
        }

        // Loading is replacement, not an undocumented merge. Clear the previous project's
        // runtime rules/carrier first, then replace every local edit store so saving immediately
        // after a load reproduces exactly the file that was opened.
        self.clear_all_game_edits();
        let previously_loaded_eff = self
            .eff_editor
            .loaded_rel()
            .filter(|relative| !crate::scratch_dirs::is_transplant_preview_name(relative));
        self.eff_editor.reset_project_state();
        self.state.edit_log.entries.clear();
        self.state.effect_call_edits.clear();
        self.state.effect_call_full.clear();
        self.eff_mods.clear();
        self.live_overrides = crate::game_link::LiveOverrides::default();
        self.state.hitboxes = self.state.hitboxes_pristine.clone();
        if let Some(relative) = previously_loaded_eff {
            let base = self.resolve_eff_source(&relative);
            if base.is_file() {
                self.eff_editor.queue_load(&base);
            }
        }
        self.project_name = project.name.clone();
        let mut n_acmd = 0;
        let mut n_calls = 0;
        let mut n_eff = 0;
        for (fighter, mut fm) in project.fighters {
            for (move_name, record) in &mut fm.acmd {
                if record.hitboxes_pristine.is_empty() {
                    if let Some(body) = crate::acmd::cached_script_body(&fighter, move_name) {
                        record.hitboxes_pristine =
                            crate::acmd::parse_acmd_script(&body).to_hitboxes();
                    }
                }
            }
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
            if let Some(mut eff) = fm.eff {
                // Older saves recorded mixed-case names ("SYS_ICE_os"); entry names are
                // lowercase everywhere now (kind hashes are computed on lowercase).
                for op in &mut eff.transplants {
                    op.new_entry_name = op.new_entry_name.to_lowercase();
                    op.src_set_name = op.src_set_name.to_lowercase();
                    if let Some(r) = &mut op.replace_entry {
                        *r = r.to_lowercase();
                    }
                }
                n_eff += eff.authored.len()
                    + eff.transplants.len()
                    + eff.textures.len()
                    + eff.textures_added.len()
                    + eff.textures_removed.len()
                    + eff.rosters.len()
                    + eff.entry_edits.len();
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
                    init.rainbow.color = crate::game_link::Color {
                        red: r,
                        green: g,
                        blue: b,
                        alpha: a,
                    };
                }
                if let Some(s) = t.speed {
                    init.speed = s;
                }
                self.live_overrides.restore_tweak(hash, init);
            }
        }

        // Re-apply to what's currently loaded and push it live.
        self.apply_saved_hitbox_edits_to_current();
        self.apply_effect_call_edits_to_current();
        self.push_hitbox_rules();
        let pending_hitbox_live = self.push_all_saved_hitbox_rules();
        self.push_effect_rules();
        self.push_effect_aliases();
        // Rebuild merged views for every fighter with transplant ops so the eff editor and
        // viewport show them (character-centric overlays survive project reloads).
        let slotted: Vec<String> = self
            .eff_mods
            .iter()
            .filter(|(_, e)| !e.transplants.is_empty())
            .map(|(f, _)| f.clone())
            .collect();
        for f in slotted {
            self.build_merged_preview(&f);
        }
        if let Some(fighter) = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone())
        {
            if let Some(eff) = self.eff_mods.get(&fighter).cloned() {
                let src = self.resolve_eff_source(&eff.source_rel);
                if src.exists() {
                    // Always start a loaded project from the canonical base EFF. Applying a new
                    // project directly onto the previous project's in-memory diff compounds
                    // authored values and can leave removed structures behind.
                    self.eff_editor.queue_load(&src);
                    self.eff_editor
                        .queue_structure_edits(eff.rosters.clone(), eff.entry_edits.clone());
                    self.eff_editor.queue_edits(eff.authored.clone());
                    self.eff_editor.open = true;
                }
            }
        }
        self.state.status = format!(
            "Project '{}' loaded: {n_acmd} move edit(s), {n_calls} effect-call edit(s), {n_eff} eff edit(s)",
            self.project_name
        );
        if pending_hitbox_live > 0 {
            self.state.status.push_str(&format!(
                " — {pending_hitbox_live} hitbox change(s) need the move opened or performed once for live preview"
            ));
        }
    }

    /// "Game has existing edits" prompt: the plugin persists pins across sessions, so a
    /// fresh toolkit instance may connect to a game already running modifications.
    fn draw_pin_sync_modal(&mut self, ctx: &egui::Context) {
        let Some(kinds) = self.pin_sync_prompt.clone() else {
            return;
        };
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
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for (_, k) in &kinds {
                            let mut fields: Vec<&str> = Vec::new();
                            if let Some(p) = &k.pins {
                                if p.scale.is_some() {
                                    fields.push("size");
                                }
                                if p.rate.is_some() {
                                    fields.push("speed");
                                }
                                if p.pos.is_some() {
                                    fields.push("pos");
                                }
                                if p.rot.is_some() {
                                    fields.push("rot");
                                }
                                if p.visible.is_some() {
                                    fields.push("visible");
                                }
                                if p.frame.is_some() {
                                    fields.push("frame");
                                }
                                if p.color.is_some() {
                                    fields.push("color");
                                }
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "• {}  ({})",
                                    k.name,
                                    fields.join(", ")
                                ))
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
    /// Wipe every artefact a previous session left on the SD that the game would otherwise
    /// pick up at boot.
    ///
    /// Live edits are SESSION state. Nothing the editor pushes should survive a restart —
    /// only an explicitly loaded project (or an exported mod) is persistent. Three things
    /// used to leak across boots:
    ///
    ///  * `ultimate/mods/effect_viewer_live/` — a real Arcropolis mod. This is the bad one:
    ///    Arcropolis loads it every boot, so a fighter stayed modified forever, with nothing
    ///    in the editor showing why.
    ///  * `effect_viewer/live_eff/` — the merged-eff manifest the plugin re-registers at boot.
    ///  * `slight/user/pinned_edits.json` — the plugin re-applies these before the editor is
    ///    even connected, so resetting over the wire is too late; the file has to go.
    ///
    /// Called once at startup. Loading a project re-publishes everything it needs.
    fn clear_stale_live_state() {
        let Some(sd) = crate::scratch_dirs::emulator_sd_root() else {
            return;
        };
        for dir in [
            sd.join("ultimate").join("mods").join("effect_viewer_live"),
            sd.join("effect_viewer").join("live_eff"),
        ] {
            if dir.is_dir() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        let pins = sd.join("slight").join("user").join("pinned_edits.json");
        if pins.is_file() {
            // Truncate rather than delete: the plugin opens this path on boot and an empty
            // list is the documented "no pins" state.
            let _ = std::fs::write(&pins, "[]");
        }
    }

    fn clear_all_game_edits(&mut self) {
        self.game_link.send_reset_pins();
        self.effect_rules_store.clear();
        self.hitbox_rules_store.clear();
        self.game_link.send_spawn_rules(&[]);
        self.game_link.send_hitbox_rules(&[]);
        self.game_link.send_effect_aliases(&[]);
        // Tear the live CARRIER down too. Authored colour edits are cloned into it (the
        // fighter's own eff cannot be reloaded mid-match), so without this they survive a
        // "clear all" and keep rendering — the carrier's donor bytes stay resident in the
        // plugin and its aliases get re-registered on the next spawn.
        //
        // Same order as the publish path: bytes first, then specs. Empty is meaningful — it
        // is what tells the plugin to drop the old carrier rather than keep serving it.
        self.game_link.send_donor_bytes(&[]);
        self.game_link.send_donor_effs(&[]);
        self.carrier_push_callers.clear();
        // Stop serving merged eff files: wipe the SD-side manifest + files (the plugin's
        // registrations stay for this boot but resolve to nothing → vanilla files load).
        if let Some(dir) = crate::scratch_dirs::emulator_sd_root()
            .map(|sd| sd.join("effect_viewer").join("live_eff"))
            .filter(|d| d.is_dir())
        {
            let _ = std::fs::remove_dir_all(&dir);
            self.game_link.send_live_eff_reload();
        }
        // Also remove the guaranteed-fallback Arcropolis mod staged by deploy_live_eff
        // (takes effect on the next boot — the off-switch mirror of the staged mod).
        if let Some(dir) = crate::scratch_dirs::emulator_sd_root()
            .map(|sd| sd.join("ultimate").join("mods").join("effect_viewer_live"))
            .filter(|d| d.is_dir())
        {
            let _ = std::fs::remove_dir_all(&dir);
        }
        self.live_eff_deployed.clear();
        self.pin_sync_prompt = None;
        self.state.status = "Cleared all in-game edits (saved pins + live rules + aliases + \
             live carrier + live eff files)."
            .into();
    }

    // ── Live ACMD capture + live hitbox rules ─────────────────────────────────

    // FRAME SPACES
    //
    // The editor works in ACMD SCRIPT frames throughout — that is what the timeline shows and
    // what an exported script's `frame(N)` calls mean. The plugin reports MOTION frames
    // (`MotionModule::frame`), and the two are offset by one:
    //
    //   * every dumped script opens at `frame(1.0)`, never `frame(0.0)`;
    //   * a motion's first frame reads 0.0;
    //   * measured against the GitHub scripts, captured spawns come in exactly one frame
    //     early, uniformly across a move (see `capture_vs_script_offset`, which reports it).
    //
    // Neither source is wrong; they count from different places. The editor adopts the
    // script's convention because that is what it round-trips, so captures convert on the way
    // IN and rules convert back on the way OUT. Before this existed the two spaces were
    // conflated, and the live-rule frame windows carried an asymmetric `+1.5` fudge to cover
    // whichever source the frame had come from.

    /// A plugin-reported motion frame → the script frame the editor shows.
    fn motion_to_script_frame(frame: f32) -> u32 {
        crate::data::motion_to_script_frame(frame)
    }

    /// A script frame the editor holds → the motion frame the plugin will observe.
    fn script_to_motion_frame(frame: u32) -> f32 {
        crate::data::script_to_motion_frame(frame)
    }

    /// Frame window for a live rule scoped to one script frame.
    ///
    /// Half-open either side of the motion frame, which is exactly the rounding bucket that
    /// produced the script frame: any observation that would round to this frame falls inside,
    /// and the neighbouring frames' spawns do not.
    fn rule_frame_window(script_frame: u32) -> (Option<f32>, Option<f32>) {
        let motion = Self::script_to_motion_frame(script_frame);
        (Some(motion - 0.5), Some(motion + 0.5))
    }

    /// hash40 of the current move's motion name (what MotionModule::motion_kind reports).
    ///
    /// (Frame-space helpers live just below — see [`Self::motion_to_script_frame`].)
    fn current_motion_hash(&self) -> Option<u64> {
        self.state
            .selected_move
            .as_ref()
            .map(|m| hash40::hash40(&m.name.to_lowercase()).0)
    }

    fn current_fighter_kind(&self) -> Option<i32> {
        self.state
            .selected_fighter
            .and_then(|index| self.state.fighters.get(index))
            .and_then(|fighter| fighter_kind_id(&fighter.name))
    }

    /// Captures for the selected fighter, from ONE performance of the move.
    ///
    /// Two filters, for two different ways the bucket used to merge unrelated things:
    ///   * fighter kind — motion hashes are shared across the roster, so the raw bucket can
    ///     hold another character's ATTACK/EFFECT script under the same motion;
    ///   * run — the plugin tags each playback, and only the newest is loaded. Without it,
    ///     performing a move grounded and then aerial left the union of both branches and the
    ///     editor showed spawns that never occur together.
    fn captures_for_selected_fighter(&self, motion: u64) -> Vec<crate::game_link::CaptureLine> {
        self.game_link
            .latest_run_for(motion, self.current_fighter_kind())
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
        // Resolved names are LOWERCASE everywhere (matches live-kind names and the case
        // the hashes are computed on) — the file's original case leaked UPPERCASE names
        // into the effects panel for captured spawns.
        //
        // ParamLabels is the broad fallback for system/common and foreign-fighter effects
        // that are not present in the currently opened EFF. Prefer concrete EFF/live names
        // below, but never discard this already-loaded hash dictionary.
        let mut m: HashMap<u64, String> = self
            .state
            .labels
            .iter()
            .filter_map(|(hash, label)| {
                let label = label.trim();
                (!label.is_empty()).then(|| (*hash, label.to_lowercase()))
            })
            .collect();
        if let Some(idx) = self
            .current_eff_path
            .as_deref()
            .and_then(|path| crate::effects::EffIndex::from_file(path).ok())
        {
            for name in idx.handles.keys() {
                let lower = name.to_lowercase();
                m.insert(hash40::hash40(&lower).0, lower);
            }
        }
        for (h, k) in self.game_link.kinds() {
            let name = k.name.to_lowercase();
            // The plugin also falls back to `0x...`; do not let that replace a cracked
            // ParamLabels name for the same hash.
            if name.starts_with("0x") {
                m.entry(h).or_insert(name);
            } else {
                m.insert(h, name);
            }
        }
        m
    }

    /// Register newly-arrived capture lines for the open move, arming (or extending) the
    /// settle window. Cheap — only does work when the link's capture counter actually moved.
    fn note_new_captures(&mut self) {
        let seq = self.game_link.captures_seq();
        if seq == self.captures_seen_seq {
            return;
        }
        self.captures_seen_seq = seq;
        let Some(motion) = self.current_motion_hash() else {
            return;
        };
        let kind = self.current_fighter_kind();
        let lines = self.game_link.captures_count(motion, kind);
        if lines == 0 {
            return;
        }
        let now = std::time::Instant::now();
        match &mut self.pending_capture {
            // Same locked playback, more lines: the script is still running — keep waiting.
            Some(p) if p.motion == motion => {
                if lines != p.lines {
                    p.lines = lines;
                    p.last_line = now;
                }
            }
            // Another move is settling; `settle_pending_capture` drops it on the next tick.
            Some(_) => {}
            None => {
                // Arm on an EMPTY move (nothing to lose) or on its UNEDITED live snapshot. A
                // GitHub fetch, or a live capture the user has since edited, is never clobbered.
                let empty = self.state.acmd_source.is_empty()
                    && self.state.hitboxes.is_empty()
                    && self.state.effects.is_empty();
                let live_unedited = self.state.acmd_source == "Live capture"
                    && self.state.hitboxes == self.state.hitboxes_pristine;
                if empty || live_unedited {
                    self.pending_capture = Some(PendingCapture {
                        motion,
                        kind,
                        run: self.game_link.latest_run_id(motion, kind).unwrap_or(0),
                        lines,
                        first_line: now,
                        last_line: now,
                    });
                }
            }
        }
    }

    /// Adopt a deferred live capture once the move has actually finished.
    ///
    /// Primary signal is the plugin's `AcmdCaptureEnd` — the game itself reporting that the
    /// motion reached its end frame or was cancelled into another motion, which is exactly
    /// when its ACMD script stops emitting. The quiet period and the hard timeout are only
    /// fallbacks (older plugin builds, or a motion that never reports an end).
    fn settle_pending_capture(&mut self, ctx: &egui::Context) {
        let Some(p) = self.pending_capture.as_ref() else {
            return;
        };
        let (motion, kind, run, first_line, last_line) =
            (p.motion, p.kind, p.run, p.first_line, p.last_line);

        // Moved off the move, or real data arrived from elsewhere (GitHub fetch) — drop it.
        // A manual "⟳ Live" click mid-window is deliberately NOT a cancel: letting the window
        // run to completion means the click still works and then self-corrects to the full
        // script instead of leaving the truncated one in place.
        if self.current_motion_hash() != Some(motion)
            || (!self.state.acmd_source.is_empty() && self.state.acmd_source != "Live capture")
        {
            self.pending_capture = None;
            return;
        }

        let ended = self.game_link.capture_run_complete(kind, motion, run);
        if ended || last_line.elapsed() >= CAPTURE_QUIET || first_line.elapsed() >= CAPTURE_MAX_WAIT
        {
            self.pending_capture = None;
            // Full rebuild from the capture bucket — replaces, never merges into, whatever
            // partial state an earlier adoption or manual click left behind.
            self.load_from_capture();
            return;
        }
        self.state.status = "Capturing live ACMD — waiting for the move to finish…".into();
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// Build hitboxes + effect calls for the current move from the game's live ACMD capture,
    /// replacing the GitHub fetch as the data source ("Live capture" provenance).
    fn load_from_capture(&mut self) {
        let Some(motion) = self.current_motion_hash() else {
            return;
        };
        let captures = self.captures_for_selected_fighter(motion);
        if captures.is_empty() {
            self.state.status = "No live capture yet — perform the move in game first.".into();
            return;
        }
        let bone_rev = self.bone_reverse_map();
        let eff_rev = self.effect_reverse_map();
        // Only the collision_attr labels are needed to name the captured attr hash.
        let attr_labels: HashMap<u64, String> = self
            .state
            .labels
            .iter()
            .filter(|(_, l)| l.starts_with("collision_attr_"))
            .map(|(h, l)| (*h, l.clone()))
            .collect();

        let mut effects = Self::effect_calls_from_captures(&captures, &bone_rev, &eff_rev);
        let hitboxes = Self::hitboxes_from_captures(&captures, &bone_rev, &attr_labels);
        // Scrub our own ghosts: live retime/rename/add rules re-fire spawns through the
        // game's EFFECT functions, and captures taken before the plugin's inject guard
        // recorded those replays as if the script contained them. Any captured spawn
        // matching the OUTPUT signature of a saved edit that created a NEW signature
        // (retimed/renamed/added) is our own edit echoing back — without this, the edited
        // effect shows up once as "original" and once as the edit.
        if let Some(mv) = self.current_move_key() {
            if let Some(edits) = self.state.effect_call_edits.get(&mv) {
                let ghost_sigs: Vec<(u64, u64, u64, u32, u64)> = edits
                    .iter()
                    .filter_map(|e| match &e.op {
                        crate::data::EffectCallOp::Add(c) => Some(call_sig(c)),
                        crate::data::EffectCallOp::Modify(c) => e
                            .pristine
                            .as_ref()
                            .is_some_and(|p| call_sig(p) != call_sig(c))
                            .then(|| call_sig(c)),
                        crate::data::EffectCallOp::Remove => None,
                    })
                    .collect();
                if !ghost_sigs.is_empty() {
                    effects.retain(|c| !ghost_sigs.contains(&call_sig(c)));
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
            self.push_effect_rules();
        }
        self.state.acmd_source = "Live capture".into();
        self.acmd_error = None;
        if self.apply_saved_hitbox_edits_to_current() {
            self.push_hitbox_rules();
        }
        self.state.total_frames = self.state.total_frames.max(timeline_frame_extent(
            &self.state.hitboxes,
            &self.state.effects,
        ));
        self.jump_to_earliest_active_frame();
        let mut status =
            format!("Loaded {n_hb} hitbox(es) + {n_fx} effect call(s) from live game capture");
        if let Some(note) = self.capture_vs_script_offset() {
            status.push_str(" — ");
            status.push_str(&note);
        }
        self.state.status = status;
    }

    /// Compare the just-loaded live capture against the GitHub script for the same move and
    /// describe how their frames differ.
    ///
    /// This REPORTS; it does not correct. The two sources measure different things — the
    /// script's `frame(N)` is a target the ACMD coroutine resumes at, the capture is
    /// `MotionModule::frame` at the instant the call ran — and which one the editor's timeline
    /// should be denominated in is not something to decide silently. Dumped scripts start at
    /// `frame(1.0)` while a motion's first frame reads 0.0, so a uniform one-frame difference
    /// is expected rather than a fault in either source; a VARYING difference is the
    /// interesting case and this is what will show it.
    ///
    /// Cache-only: never fetches, so loading a capture stays instant and offline.
    fn capture_vs_script_offset(&self) -> Option<String> {
        let fighter = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))?;
        let move_name = self.state.selected_move.as_ref()?;
        let body = crate::acmd::cached_script_body(&fighter.name, &move_name.name)?;
        let script = crate::acmd::parse_effect_script(&body).to_effect_calls();
        if script.is_empty() {
            return None;
        }
        // Match on what both sources agree about — which effect, spawned how — and take the
        // first unused script call for each, so a repeated effect pairs up in order.
        let mut used = vec![false; script.len()];
        let mut deltas: Vec<i64> = Vec::new();
        for live in &self.state.effects {
            if let Some(i) = script.iter().enumerate().position(|(i, s)| {
                !used[i] && s.effect_name == live.effect_name && s.spawn_func == live.spawn_func
            }) {
                used[i] = true;
                deltas.push(live.active_start as i64 - script[i].active_start as i64);
            }
        }
        if deltas.is_empty() {
            return None;
        }
        let matched = deltas.len();
        let first = deltas[0];
        if deltas.iter().all(|d| *d == first) {
            if first == 0 {
                return Some(format!(
                    "frames agree with the GitHub script ({matched} matched)"
                ));
            }
            let (n, dir) = if first > 0 {
                (first, "later than")
            } else {
                (-first, "earlier than")
            };
            return Some(format!(
                "every captured spawn is {n} frame(s) {dir} the GitHub script ({matched} matched)"
            ));
        }
        let lo = deltas.iter().min().copied().unwrap_or(0);
        let hi = deltas.iter().max().copied().unwrap_or(0);
        Some(format!(
            "frames differ from the GitHub script by {lo}..{hi} frames across {matched} spawn(s) \
             — not a constant offset, so one of the two is wrong for this move"
        ))
    }

    /// ATTACK capture args (positional, editor conventions) → display Hitbox.
    ///
    /// The attribute slots are plain lua numbers on the wire; they are DECODED here into the
    /// same symbolic names the property dropdowns offer, so a fetched hitbox shows its live
    /// value as the selected entry. A number with no known name is kept verbatim (and the
    /// dropdown offers it) rather than being dropped.
    fn hitbox_from_capture(
        args: &[crate::game_link::LuaArgWire],
        frame: f32,
        bone_rev: &HashMap<u64, String>,
        labels: &HashMap<u64, String>,
    ) -> Option<crate::data::Hitbox> {
        if args.len() < 17 {
            return None;
        }
        let f32_at = |i: usize| args.get(i).and_then(|a| a.as_f32());
        let i64_at = |i: usize| args.get(i).and_then(|a| a.as_i64());
        let const_at = |i: usize, table: crate::param_labels::ConstTable| {
            i64_at(i)
                .map(|v| {
                    crate::param_labels::const_name(table, v)
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default()
        };
        // Lua scripts do not consistently push flag slots as Bool. Some use Int/Num, and the
        // plugin deliberately preserves that source type when rewriting. Decode all scalar
        // representations here so an integer-backed true flag does not arrive in the editor as
        // false and get written back incorrectly on the next edit.
        let bool_at = |i: usize| i64_at(i).is_some_and(|v| v != 0);
        let bone_hash = args.get(2).and_then(|a| a.as_hash())?;
        let bone_name = bone_rev
            .get(&bone_hash)
            .cloned()
            .unwrap_or_else(|| format!("{bone_hash:#x}"));
        let capsule_end = match (f32_at(12), f32_at(13), f32_at(14)) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => None,
        };
        let start = Self::motion_to_script_frame(frame);
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
            setoff_kind: const_at(17, crate::param_labels::SETOFF_KIND),
            lr_check: const_at(18, crate::param_labels::LR_CHECK),
            is_clang: bool_at(19),
            is_add_attack: i64_at(20).unwrap_or(0) as i32,
            hitbox_attr: f32_at(21).unwrap_or(0.0),
            ground_or_air: i64_at(22).unwrap_or(0) as i32,
            is_mtk: bool_at(23),
            is_shield_disable: bool_at(24),
            is_reflectable: bool_at(25),
            is_absorbable: bool_at(26),
            is_landing_attack: bool_at(27),
            situation_mask: const_at(28, crate::param_labels::SITUATION_MASK),
            category_mask: const_at(29, crate::param_labels::CATEGORY_MASK),
            part_mask: const_at(30, crate::param_labels::PART_MASK),
            no_finish_camera: bool_at(31),
            collision_attr: args
                .get(32)
                .and_then(|a| a.as_hash())
                .map(|h| crate::param_labels::decode_collision_attr(h, labels))
                .unwrap_or_default(),
            sound_level: const_at(33, crate::param_labels::SOUND_LEVEL),
            sound_attr: const_at(34, crate::param_labels::SOUND_ATTR),
            attack_region: const_at(35, crate::param_labels::ATTACK_REGION),
            active_start: start,
            // Open until a clear (or another hitbox with this id) ends it.
            active_end: u32::MAX,
            hitbox_type: 0,
            category: 0,
            wind: None,
            catch: None,
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
        let start = Self::motion_to_script_frame(frame);
        // Slots 9 and 10 are the grab's status kind and situation mask. Nothing in the editor
        // exposes them, but keeping them means an export writes the values the game actually
        // used rather than the plugin's stand-ins. They have no const table, so they ride
        // along as the bare integers they arrived as — which is what `CATCH` takes.
        let catch = crate::data::CatchExtras {
            status: i64_at(9)
                .map(|v| v.to_string())
                .unwrap_or_else(|| crate::data::CATCH_DEFAULT_STATUS.to_string()),
            situation: i64_at(10)
                .map(|v| v.to_string())
                .unwrap_or_else(|| crate::data::CATCH_DEFAULT_SITUATION.to_string()),
        };
        Some(crate::data::Hitbox {
            id: i64_at(0).unwrap_or(0) as u32,
            bone_name,
            size: f32_at(2).unwrap_or(2.0),
            offset_x: f32_at(3).unwrap_or(0.0),
            offset_y: f32_at(4).unwrap_or(0.0),
            offset_z: f32_at(5).unwrap_or(0.0),
            capsule_end,
            catch: Some(catch),
            // Grabboxes deal no damage/knockback — zero the attack-only fields.
            damage: 0.0,
            angle: 0,
            kb_scaling: 0,
            fkb: 0,
            kb_base: 0,
            active_start: start,
            // Open until a clear (or another hitbox with this id) ends it.
            active_end: u32::MAX,
            category: 1,
            ..Default::default()
        })
    }

    /// AREA_WIND capture → its exact 2D rectangle/radial payload.
    fn hitbox_from_capture_wind(
        command: &str,
        args: &[crate::game_link::LuaArgWire],
        frame: f32,
    ) -> Option<crate::data::Hitbox> {
        let expected = match command {
            "AREA_WIND_2ND_RAD" => 8,
            "AREA_WIND_2ND_RAD_arg9" => 9,
            "AREA_WIND_2ND" => 9,
            "AREA_WIND_2ND_arg10" => 10,
            _ => return None,
        };
        if args.len() != expected {
            return None;
        }
        let values: Option<Vec<f32>> = args.iter().map(|arg| arg.as_f32()).collect();
        let wind = crate::data::WindboxData {
            command: command.into(),
            args: values?,
        };
        Some(wind.to_hitbox(Self::motion_to_script_frame(frame)))
    }

    /// EFFECT-family capture → EffectCall (arc layout: gfx, joint, pos xyz, rot zr/yr/xr, size).
    fn effect_call_from_capture(
        func: &str,
        args: &[crate::game_link::LuaArgWire],
        frame: f32,
        bone_rev: &HashMap<u64, String>,
        eff_rev: &HashMap<u64, String>,
    ) -> Option<crate::data::EffectCall> {
        let (flip, follows) = effect_capture_layout(func)?;
        let off = usize::from(flip);
        let eff_hash = args.first().and_then(|a| a.as_hash())?;
        let effect_name_alt = flip.then(|| {
            args.get(1)
                .and_then(|arg| arg.as_hash())
                .and_then(|hash| {
                    eff_rev
                        .get(&hash)
                        .cloned()
                        .or_else(|| Some(format!("{hash:#x}")))
                })
                .unwrap_or_else(|| "null".into())
        });
        let f32_at = |i: usize| args.get(i).and_then(|a| a.as_f32()).unwrap_or(0.0);
        let bone_hash = args.get(1 + off).and_then(|a| a.as_hash()).unwrap_or(0);
        let start = Self::motion_to_script_frame(frame);
        Some(crate::data::EffectCall {
            effect_name: eff_rev
                .get(&eff_hash)
                .cloned()
                .unwrap_or_else(|| format!("{eff_hash:#x}")),
            effect_name_alt,
            spawn_func: func.to_string(),
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
            // Everything past the size argument, so an export can reissue THIS macro rather
            // than substituting plain EFFECT/EFFECT_FOLLOW. All-or-nothing: a tail we cannot
            // spell in full is dropped, and the export falls back to the known-good pair.
            extra_args: args.get(9 + off..).and_then(|tail| {
                tail.iter()
                    .map(crate::game_link::LuaArgWire::to_source_arg)
                    .collect::<Option<Vec<_>>>()
            }),
            raw_line: None,
        })
    }

    /// Build the move's collisions from a live capture, with lifetimes.
    ///
    /// Modelled the same way the GitHub script path models it (`data::eval_stmts`): a
    /// collision stays open until something ends it, and ends on the frame BEFORE that.
    /// Captures used to give every hitbox a hardcoded two-frame lifetime, because nothing told
    /// the editor when hitboxes stop — so a live-fetched hitbox ended far earlier than the
    /// script said. The plugin now captures `AttackModule::clear_all`, which is what scripts
    /// actually use to end them.
    fn hitboxes_from_captures(
        captures: &[crate::game_link::CaptureLine],
        bone_rev: &HashMap<u64, String>,
        attr_labels: &HashMap<u64, String>,
    ) -> Vec<crate::data::Hitbox> {
        let mut hitboxes: Vec<crate::data::Hitbox> = Vec::new();
        // Capture entries arrive as each runtime branch is observed. Sort them into script
        // time while retaining capture order at equal frames, so a same-frame CLEAR/ATTACK
        // pair keeps the ordering the game actually executed.
        let mut ordered: Vec<_> = captures.iter().enumerate().collect();
        ordered.sort_by(|(ai, a), (bi, b)| a.frame.total_cmp(&b.frame).then_with(|| ai.cmp(bi)));
        for (_, line) in ordered {
            if line.func == "ATTACK_CLEAR_ALL" {
                let end = Self::motion_to_script_frame(line.frame).saturating_sub(1);
                for hb in hitboxes
                    .iter_mut()
                    .filter(|h| h.category != 2 && h.active_end == u32::MAX)
                {
                    hb.active_end = end.max(hb.active_start);
                }
            } else if line.func == "AREA_WIND_ERASE" {
                let Some(id) = line.args.first().and_then(|arg| arg.as_i64()) else {
                    continue;
                };
                let end = Self::motion_to_script_frame(line.frame).saturating_sub(1);
                for hitbox in hitboxes.iter_mut().filter(|hitbox| {
                    hitbox.category == 2
                        && hitbox.id == id.max(0) as u32
                        && hitbox.active_start <= end.saturating_add(1)
                }) {
                    hitbox.active_end = hitbox.active_end.min(end).max(hitbox.active_start);
                }
            } else if line.func.starts_with("ATTACK") {
                if let Some(hb) =
                    Self::hitbox_from_capture(&line.args, line.frame, bone_rev, attr_labels)
                {
                    // Same id re-captured (multi-part moves): keep the earliest frame.
                    if !hitboxes
                        .iter()
                        .any(|h| h.id == hb.id && h.active_start == hb.active_start)
                    {
                        // Re-using an id replaces the previous hitbox with that id.
                        let end = hb.active_start.saturating_sub(1);
                        if let Some(open) = hitboxes
                            .iter_mut()
                            .find(|h| h.id == hb.id && h.active_end == u32::MAX)
                        {
                            open.active_end = end.max(open.active_start);
                        }
                        hitboxes.push(hb);
                    }
                }
            } else if line.func == "CATCH" {
                if let Some(hb) = Self::hitbox_from_capture_grab(&line.args, line.frame, bone_rev) {
                    if !hitboxes.iter().any(|h| {
                        h.category == 1 && h.id == hb.id && h.active_start == hb.active_start
                    }) {
                        hitboxes.push(hb);
                    }
                }
            } else if line.func.starts_with("AREA_WIND") {
                if let Some(hb) =
                    Self::hitbox_from_capture_wind(line.func.as_str(), &line.args, line.frame)
                {
                    if !hitboxes.iter().any(|h| {
                        h.category == 2 && h.id == hb.id && h.active_start == hb.active_start
                    }) {
                        let end = hb.active_start.saturating_sub(1);
                        if let Some(open) = hitboxes.iter_mut().rev().find(|hitbox| {
                            hitbox.category == 2
                                && hitbox.id == hb.id
                                && hitbox.active_end >= hb.active_start
                        }) {
                            open.active_end = end.max(open.active_start);
                        }
                        hitboxes.push(hb);
                    }
                }
            }
        }
        // Anything still open was never cleared during the capture — same meaning, and the
        // same 9999 sentinel, the script path uses for a hitbox the script never clears. With
        // a plugin build predating the clear-all hook this is EVERY hitbox, which reads as a
        // bar running to the end of the timeline rather than a plausible-looking wrong answer.
        for hb in hitboxes.iter_mut().filter(|h| h.active_end == u32::MAX) {
            hb.active_end = 9999;
        }
        hitboxes
    }

    /// Reconstruct effect timeline spans from the locked playback's spawn and stop events.
    /// Every distinct spawn from that one execution is retained; time ordering and kill-kind
    /// semantics are then applied to the complete snapshot.
    fn effect_calls_from_captures(
        captures: &[crate::game_link::CaptureLine],
        bone_rev: &HashMap<u64, String>,
        eff_rev: &HashMap<u64, String>,
    ) -> Vec<crate::data::EffectCall> {
        let mut ordered: Vec<_> = captures.iter().enumerate().collect();
        ordered.sort_by(|(ai, a), (bi, b)| a.frame.total_cmp(&b.frame).then_with(|| ai.cmp(bi)));

        let mut effects = Vec::new();
        for (_, line) in ordered {
            if effect_capture_layout(&line.func).is_some() {
                if let Some(effect) = Self::effect_call_from_capture(
                    &line.func, &line.args, line.frame, bone_rev, eff_rev,
                ) {
                    // `null` is an explicit no-effect sentinel used by FOOT/LANDING scripts.
                    // Keep a FLIP call when its alternate side is real, but do not present a
                    // no-op as an editable smoke/effect spawn.
                    let primary_null = effect.effect_name == "null";
                    let alternate_real = effect
                        .effect_name_alt
                        .as_deref()
                        .is_some_and(|name| name != "null");
                    if !primary_null || alternate_real {
                        effects.push(effect);
                    }
                }
                continue;
            }
            if line.func != "EFFECT_OFF_KIND" {
                continue;
            }
            let Some(stop_hash) = line.args.first().and_then(|arg| arg.as_hash()) else {
                continue;
            };
            let stop_frame = Self::motion_to_script_frame(line.frame);
            // EffectModule::kill_kind terminates every live instance of the kind.
            for effect in effects.iter_mut().filter(|effect| {
                effect.active_end == 9999 && effect_name_hash(&effect.effect_name) == stop_hash
            }) {
                effect.active_end = stop_frame.max(effect.active_start);
            }
        }
        effects
    }

    /// Replace an existing wind command instead of mutating its Lua stack in place. This gives
    /// live edits the same exact command/payload path as newly added wind boxes.
    fn append_wind_replacement_rules(
        rules: &mut Vec<crate::game_link::HitboxRuleWire>,
        motion: u64,
        original: &crate::data::Hitbox,
        edited: &crate::data::Hitbox,
    ) -> bool {
        let Some(args) = Self::build_wind_args(edited) else {
            return false;
        };
        let (frame_start, frame_end) = Self::rule_frame_window(original.active_start);
        rules.push(crate::game_link::HitboxRuleWire {
            motion,
            category: 2,
            hitbox_id: Some(original.id as u64),
            suppress: true,
            frame_start,
            frame_end,
            overrides: None,
            inject: None,
        });
        rules.push(crate::game_link::HitboxRuleWire {
            motion,
            category: 2,
            hitbox_id: None,
            suppress: false,
            frame_start: None,
            frame_end: None,
            overrides: None,
            inject: Some(crate::game_link::InjectRuleWire {
                frame: Self::script_to_motion_frame(edited.active_start),
                args,
                command: edited.wind.as_ref().map(|wind| wind.command.clone()),
            }),
        });
        if let Some(end) = Self::collision_end_injection(edited) {
            rules.push(crate::game_link::HitboxRuleWire {
                motion,
                category: 2,
                hitbox_id: None,
                suppress: false,
                frame_start: None,
                frame_end: None,
                overrides: None,
                inject: Some(end),
            });
        }
        true
    }

    /// Derive + push the full live hitbox-rule set, matched PER-OCCURRENCE so multi-hit
    /// moves (which reuse the same hitbox id across frames) stay independent:
    ///   * exact (id, start-frame) pair changed → frame-scoped override,
    ///   * pristine occurrence with no current match → frame-scoped suppress (deleted/retimed),
    ///   * current occurrence with no pristine match → inject (added/retimed),
    ///     each suppress/override windowed to its own frame so the other hits are untouched.
    fn push_hitbox_rules(&mut self) {
        let Some(mv_key) = self.current_move_key() else {
            return;
        };
        let Some(motion) = self.current_motion_hash() else {
            return;
        };
        let captures = self.captures_for_selected_fighter(motion);
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
        // Frame window around a hit's spawn frame, converted into the MOTION frames the
        // plugin matches against. The `+1.5` this used to carry was covering the one-frame
        // script/motion offset by widening the window instead of converting; that made every
        // rule match two frames and could not tell adjacent hits apart.
        let win = Self::rule_frame_window;

        let pristine = self.state.hitboxes_pristine.clone();
        let current = self.state.hitboxes.clone();
        let mut cur_used = vec![false; current.len()];
        let mut pri_used = vec![false; pristine.len()];
        let mut rules: Vec<crate::game_link::HitboxRuleWire> = Vec::new();
        let mut missing_donor = false;

        // Phase 1 — exact (id, start) pairs: unchanged → nothing, changed → scoped override.
        for (pi, p) in pristine.iter().enumerate() {
            if let Some(ci) = current.iter().enumerate().position(|(ci, h)| {
                !cur_used[ci]
                    && h.category == p.category
                    && h.id == p.id
                    && h.active_start == p.active_start
            }) {
                cur_used[ci] = true;
                pri_used[pi] = true;
                let h = &current[ci];
                if h != p {
                    if p.category == 2 {
                        missing_donor |=
                            !Self::append_wind_replacement_rules(&mut rules, motion, p, h);
                    } else {
                        let (fs, fe) = win(p.active_start);
                        rules.push(crate::game_link::HitboxRuleWire {
                            motion,
                            category: p.category,
                            hitbox_id: Some(p.id as u64),
                            suppress: false,
                            frame_start: fs,
                            frame_end: fe,
                            overrides: Some(Self::hitbox_overrides(h, p)),
                            inject: None,
                        });
                    }
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
                hitbox_id: Some(p.id as u64),
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
            let (args, command) = match h.category {
                1 => (Self::build_catch_args(h, donor_for(1, h.id)), None),
                2 => (
                    Self::build_wind_args(h),
                    h.wind.as_ref().map(|wind| wind.command.clone()),
                ),
                _ => (Self::build_attack_args(h, donor_for(0, h.id)), None),
            };
            match args {
                Some(args) => {
                    rules.push(crate::game_link::HitboxRuleWire {
                        motion,
                        category: h.category,
                        hitbox_id: None,
                        suppress: false,
                        frame_start: None,
                        frame_end: None,
                        overrides: None,
                        inject: Some(crate::game_link::InjectRuleWire {
                            frame: Self::script_to_motion_frame(h.active_start),
                            args,
                            command,
                        }),
                    });
                    if let Some(end) = Self::collision_end_injection(h) {
                        rules.push(crate::game_link::HitboxRuleWire {
                            motion,
                            category: h.category,
                            hitbox_id: None,
                            suppress: false,
                            frame_start: None,
                            frame_end: None,
                            overrides: None,
                            inject: Some(end),
                        });
                    }
                }
                None => missing_donor = true,
            }
        }

        if rules.is_empty() {
            self.hitbox_rules_store.remove(&mv_key);
        } else {
            self.hitbox_rules_store.insert(mv_key, rules);
        }
        let all: Vec<crate::game_link::HitboxRuleWire> = self
            .hitbox_rules_store
            .values()
            .flatten()
            .cloned()
            .collect();
        self.game_link.send_hitbox_rules(&all);
        if missing_donor {
            self.state.status =
                "Added/retimed hitbox needs live capture args — perform the move in game once."
                    .into();
        }
    }

    /// Rebuild live hitbox rules for every move in a loaded project, not just the move currently
    /// open in the UI. Projects written before `hitboxes_pristine` was added cannot safely
    /// derive suppress/retime rules; those moves are counted so the load status can tell the
    /// user to open or perform them once and recover a baseline.
    fn push_all_saved_hitbox_rules(&mut self) -> usize {
        let records: Vec<(String, String, crate::data::EditRecord)> = self
            .state
            .edit_log
            .entries
            .iter()
            .flat_map(|(fighter, moves)| {
                moves.iter().map(move |(move_name, record)| {
                    (fighter.clone(), move_name.clone(), record.clone())
                })
            })
            .collect();
        let mut incomplete = 0;
        for (fighter, move_name, record) in records {
            if record.hitboxes_pristine.is_empty() {
                incomplete += 1;
                continue;
            }
            let motion = hash40::hash40(&move_name.to_lowercase()).0;
            let captures = self
                .game_link
                .latest_run_for(motion, fighter_kind_id(&fighter));
            let family_prefix = |category: u8| match category {
                1 => "CATCH",
                2 => "AREA_WIND",
                _ => "ATTACK",
            };
            let donor_for = |category: u8, id: u32| {
                let prefix = family_prefix(category);
                captures
                    .iter()
                    .find(|capture| {
                        capture.func.starts_with(prefix)
                            && capture.args.first().and_then(|arg| arg.as_i64()) == Some(id as i64)
                    })
                    .or_else(|| {
                        captures
                            .iter()
                            .find(|capture| capture.func.starts_with(prefix))
                    })
            };
            let pristine = &record.hitboxes_pristine;
            let current = &record.hitboxes;
            let mut current_used = vec![false; current.len()];
            let mut pristine_used = vec![false; pristine.len()];
            let mut rules = Vec::new();

            for (pristine_index, original) in pristine.iter().enumerate() {
                if let Some(current_index) =
                    current.iter().enumerate().position(|(index, hitbox)| {
                        !current_used[index]
                            && hitbox.category == original.category
                            && hitbox.id == original.id
                            && hitbox.active_start == original.active_start
                    })
                {
                    current_used[current_index] = true;
                    pristine_used[pristine_index] = true;
                    let edited = &current[current_index];
                    if edited != original {
                        if original.category == 2 {
                            if !Self::append_wind_replacement_rules(
                                &mut rules, motion, original, edited,
                            ) {
                                incomplete += 1;
                            }
                        } else {
                            let (frame_start, frame_end) =
                                Self::rule_frame_window(original.active_start);
                            rules.push(crate::game_link::HitboxRuleWire {
                                motion,
                                category: original.category,
                                hitbox_id: Some(original.id as u64),
                                suppress: false,
                                frame_start,
                                frame_end,
                                overrides: Some(Self::hitbox_overrides(edited, original)),
                                inject: None,
                            });
                        }
                    }
                }
            }
            for (index, original) in pristine.iter().enumerate() {
                if pristine_used[index] {
                    continue;
                }
                let (frame_start, frame_end) = Self::rule_frame_window(original.active_start);
                rules.push(crate::game_link::HitboxRuleWire {
                    motion,
                    category: original.category,
                    hitbox_id: Some(original.id as u64),
                    suppress: true,
                    frame_start,
                    frame_end,
                    overrides: None,
                    inject: None,
                });
            }
            for (index, edited) in current.iter().enumerate() {
                if current_used[index] {
                    continue;
                }
                let (args, command) = match edited.category {
                    1 => (
                        Self::build_catch_args(edited, donor_for(1, edited.id)),
                        None,
                    ),
                    2 => (
                        Self::build_wind_args(edited),
                        edited.wind.as_ref().map(|wind| wind.command.clone()),
                    ),
                    _ => (
                        Self::build_attack_args(edited, donor_for(0, edited.id)),
                        None,
                    ),
                };
                if let Some(args) = args {
                    rules.push(crate::game_link::HitboxRuleWire {
                        motion,
                        category: edited.category,
                        hitbox_id: None,
                        suppress: false,
                        frame_start: None,
                        frame_end: None,
                        overrides: None,
                        inject: Some(crate::game_link::InjectRuleWire {
                            frame: Self::script_to_motion_frame(edited.active_start),
                            args,
                            command,
                        }),
                    });
                    if let Some(end) = Self::collision_end_injection(edited) {
                        rules.push(crate::game_link::HitboxRuleWire {
                            motion,
                            category: edited.category,
                            hitbox_id: None,
                            suppress: false,
                            frame_start: None,
                            frame_end: None,
                            overrides: None,
                            inject: Some(end),
                        });
                    }
                } else {
                    incomplete += 1;
                }
            }
            let key = format!("{fighter}/{move_name}");
            if rules.is_empty() {
                self.hitbox_rules_store.remove(&key);
            } else {
                self.hitbox_rules_store.insert(key, rules);
            }
        }
        let all: Vec<_> = self
            .hitbox_rules_store
            .values()
            .flatten()
            .cloned()
            .collect();
        self.game_link.send_hitbox_rules(&all);
        incomplete
    }

    /// Only modeled ATTACK slots that differ from the captured/source hitbox.
    ///
    /// Sparse values are required for correctness, not just smaller JSON. A few script slots use
    /// a Hash sentinel for "not supplied" even though the editor exposes a numeric value. If all
    /// fields are sent every time, editing Damage also destroys those sentinels. With a sparse
    /// diff, an explicitly edited value can replace a sentinel while every untouched slot keeps
    /// its exact Lua value and type.
    fn hitbox_overrides(
        h: &crate::data::Hitbox,
        p: &crate::data::Hitbox,
    ) -> crate::game_link::HbOverridesWire {
        use crate::param_labels as pl;
        fn changed<T: PartialEq + Copy>(now: T, was: T) -> Option<T> {
            (now != was).then_some(now)
        }
        let attr = |now: &str, was: &str, table: crate::param_labels::ConstTable| {
            (now != was).then(|| pl::encode_const(table, now)).flatten()
        };
        let capsule_changed = h.capsule_end != p.capsule_end;
        crate::game_link::HbOverridesWire {
            wind_args: (h.wind != p.wind)
                .then(|| Self::build_wind_args(h))
                .flatten(),
            part: changed(h.part as i64, p.part as i64),
            bone: (h.bone_name != p.bone_name)
                .then(|| hash40::hash40(&h.bone_name.to_lowercase()).0),
            damage: changed(h.damage, p.damage),
            angle: changed(h.angle as i64, p.angle as i64),
            kbg: changed(h.kb_scaling as i64, p.kb_scaling as i64),
            fkb: changed(h.fkb as i64, p.fkb as i64),
            bkb: changed(h.kb_base as i64, p.kb_base as i64),
            size: changed(h.size, p.size),
            x: changed(h.offset_x, p.offset_x),
            y: changed(h.offset_y, p.offset_y),
            z: changed(h.offset_z, p.offset_z),
            x2: capsule_changed
                .then(|| h.capsule_end.map(|c| c[0]))
                .flatten(),
            y2: capsule_changed
                .then(|| h.capsule_end.map(|c| c[1]))
                .flatten(),
            z2: capsule_changed
                .then(|| h.capsule_end.map(|c| c[2]))
                .flatten(),
            capsule: capsule_changed.then_some(h.capsule_end.is_some()),
            hitlag: changed(h.hitlag_mult, p.hitlag_mult),
            sdi: changed(h.sdi_mult, p.sdi_mult),
            setoff: attr(&h.setoff_kind, &p.setoff_kind, pl::SETOFF_KIND),
            lr_check: attr(&h.lr_check, &p.lr_check, pl::LR_CHECK),
            clang: changed(h.is_clang, p.is_clang),
            add_attack: changed(h.is_add_attack as i64, p.is_add_attack as i64),
            hitbox_attr: changed(h.hitbox_attr, p.hitbox_attr),
            ground_or_air: changed(h.ground_or_air as i64, p.ground_or_air as i64),
            mtk: changed(h.is_mtk, p.is_mtk),
            shield_disable: changed(h.is_shield_disable, p.is_shield_disable),
            reflectable: changed(h.is_reflectable, p.is_reflectable),
            absorbable: changed(h.is_absorbable, p.is_absorbable),
            landing_attack: changed(h.is_landing_attack, p.is_landing_attack),
            situation_mask: attr(&h.situation_mask, &p.situation_mask, pl::SITUATION_MASK),
            category_mask: attr(&h.category_mask, &p.category_mask, pl::CATEGORY_MASK),
            part_mask: attr(&h.part_mask, &p.part_mask, pl::PART_MASK),
            no_finish_camera: changed(h.no_finish_camera, p.no_finish_camera),
            collision_attr: (h.collision_attr != p.collision_attr)
                .then(|| pl::encode_collision_attr(&h.collision_attr))
                .flatten(),
            sound_level: attr(&h.sound_level, &p.sound_level, pl::SOUND_LEVEL),
            sound_attr: attr(&h.sound_attr, &p.sound_attr, pl::SOUND_ATTR),
            attack_region: attr(&h.attack_region, &p.attack_region, pl::ATTACK_REGION),
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
        let mut args = donor.filter(|d| d.args.len() >= 36)?.args.clone();
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

        // Attribute slots: keep the donor's TYPE for each slot (the lua side is type
        // sensitive) and swap the value in. Names this build cannot resolve leave the donor's
        // own value in place. A sentinel/unexpected source type gets the editor's declared
        // type: injection is used for an added or retimed attack and must carry every modeled
        // property, including one the source represented with `Hash40("no")`/Nil.
        use crate::param_labels as pl;
        let mut set_scalar = |idx: usize, v: Option<i64>| {
            let Some(v) = v else { return };
            if idx >= args.len() {
                return;
            }
            let next = match args[idx] {
                A::Int(_) => A::Int(v),
                A::Num(_) => A::Num(v as f32),
                A::Bool(_) => A::Bool(v != 0),
                _ => A::Int(v),
            };
            args[idx] = next;
        };
        set_scalar(17, pl::encode_const(pl::SETOFF_KIND, &h.setoff_kind));
        set_scalar(18, pl::encode_const(pl::LR_CHECK, &h.lr_check));
        set_scalar(19, Some(h.is_clang as i64));
        set_scalar(20, Some(h.is_add_attack as i64));
        set_scalar(22, Some(h.ground_or_air as i64));
        set_scalar(23, Some(h.is_mtk as i64));
        set_scalar(24, Some(h.is_shield_disable as i64));
        set_scalar(25, Some(h.is_reflectable as i64));
        set_scalar(26, Some(h.is_absorbable as i64));
        set_scalar(27, Some(h.is_landing_attack as i64));
        set_scalar(28, pl::encode_const(pl::SITUATION_MASK, &h.situation_mask));
        set_scalar(29, pl::encode_const(pl::CATEGORY_MASK, &h.category_mask));
        set_scalar(30, pl::encode_const(pl::PART_MASK, &h.part_mask));
        set_scalar(31, Some(h.no_finish_camera as i64));
        set_scalar(33, pl::encode_const(pl::SOUND_LEVEL, &h.sound_level));
        set_scalar(34, pl::encode_const(pl::SOUND_ATTR, &h.sound_attr));
        set_scalar(35, pl::encode_const(pl::ATTACK_REGION, &h.attack_region));
        match args.get(21) {
            Some(A::Num(_)) => args[21] = A::Num(h.hitbox_attr),
            Some(A::Int(_)) => args[21] = A::Int(h.hitbox_attr as i64),
            Some(_) => args[21] = A::Num(h.hitbox_attr),
            None => {}
        }
        if let Some(hash) = pl::encode_collision_attr(&h.collision_attr) {
            if args.len() > 32 {
                args[32] = A::Hash(hash);
            }
        }
        Some(args)
    }

    /// Build a CATCH (grabbox) inject arg vector from a captured donor grab.
    /// Layout: 0 id, 1 bone(h), 2 size, 3 x, 4 y, 5 z, 6 x2, 7 y2, 8 z2, 9 status, 10 situation.
    fn build_catch_args(
        h: &crate::data::Hitbox,
        donor: Option<&crate::game_link::CaptureLine>,
    ) -> Option<Vec<crate::game_link::LuaArgWire>> {
        use crate::game_link::LuaArgWire as A;
        // Preserve a captured grab's status/situation slots when available. A brand-new grab
        // has no donor, so send its nine geometry slots; the plugin appends the game's normal
        // CAPTURE_PULLED + GA constants immediately before invoking CATCH.
        let mut args = donor
            .filter(|d| d.args.len() == 11)
            .map(|d| d.args.clone())
            .unwrap_or_else(|| {
                vec![
                    A::Int(0),
                    A::Hash(0),
                    A::Num(0.0),
                    A::Num(0.0),
                    A::Num(0.0),
                    A::Num(0.0),
                    A::Nil,
                    A::Nil,
                    A::Nil,
                ]
            });
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

    fn build_wind_args(h: &crate::data::Hitbox) -> Option<Vec<crate::game_link::LuaArgWire>> {
        h.wind.as_ref().filter(|wind| wind.is_valid()).map(|wind| {
            wind.args
                .iter()
                .copied()
                .map(crate::game_link::LuaArgWire::Num)
                .collect()
        })
    }

    fn collision_end_injection(
        h: &crate::data::Hitbox,
    ) -> Option<crate::game_link::InjectRuleWire> {
        use crate::game_link::LuaArgWire as A;
        if h.active_end >= 9999 {
            return None;
        }
        let (command, args) = match h.category {
            1 => ("GRAB_CLEAR_ALL", Vec::new()),
            2 => ("AREA_WIND_ERASE", vec![A::Int(h.id as i64)]),
            _ => return None,
        };
        Some(crate::game_link::InjectRuleWire {
            // active_end is inclusive; the clear belongs on the following game frame.
            frame: Self::script_to_motion_frame(h.active_end.saturating_add(1)),
            args,
            command: Some(command.into()),
        })
    }

    // ── EFF Transplant Studio ─────────────────────────────────────────────────

    /// Pick any effect from any EFF, transplant it into the current fighter's EFF, and
    /// optionally scope the replacement to one or more skins.
    fn draw_transplant_studio(&mut self, ctx: &egui::Context) {
        if !self.show_transplant {
            return;
        }
        if self.effect_pool.is_none() {
            self.effect_pool = Some(crate::effect_pool::EffectPool::new(
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

        let selected_fighter = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone());
        let fighter_names: Vec<String> =
            self.state.fighters.iter().map(|f| f.name.clone()).collect();
        // Import into ANY character — the picker defaults to the selected fighter.
        let target = self.transplant_target.clone().or(selected_fighter);
        // The target's ACTUAL costume slots, however many and however numbered. Indexed
        // fighters carry a scanned list; anything else (an eff opened standalone) is scanned
        // on the spot, and only a total miss falls back to the vanilla 0..=7.
        let avail_slots: Vec<u8> = match target
            .as_ref()
            .and_then(|name| self.state.fighters.iter().find(|f| f.name == *name))
        {
            Some(f) => f.slots.clone(),
            None => match target.as_ref() {
                Some(name) => crate::data::costume_slots_or_default(&self.all_roots(), name),
                None => crate::data::default_slots(),
            },
        };
        // Restore where the user last left this window, clamped onto the current screen.
        let saved = self.window_geometry.get("transplant").copied();
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        let saved = match (saved, monitor) {
            (Some(g), Some(m)) => Some(g.clamped_to_screen(m.x, m.y)),
            (g, _) => g,
        };
        let transplant_size = saved.map_or([560.0, 720.0], |g| [g.w, g.h]);
        let transplant_pos = saved.map_or([200.0, 120.0], |g| [g.x, g.y]);

        let mut do_transplant: Option<(String, String, String)> = None; // (rel, donor, new name)
                                                                        // Snapshot the transplants ALREADY recorded for the target, so the studio shows what
                                                                        // will actually be baked (they ACCUMULATE — a stale donor from a prior pick otherwise
                                                                        // silently rides along). `remove_op` / `clear_ops` are applied after the window.
        let recorded: Vec<crate::mod_project::TransplantOp> = target
            .as_ref()
            .and_then(|f| self.eff_mods.get(f))
            .map(|e| e.transplants.clone())
            .unwrap_or_default();
        // Global view across ALL fighters — so "Clear every fighter" is reachable even when the
        // CURRENT fighter has no recorded transplants (stale donors on OTHER fighters were the bug).
        let all_transplants: usize = self.eff_mods.values().map(|e| e.transplants.len()).sum();
        let all_transplant_fighters: usize = self
            .eff_mods
            .values()
            .filter(|e| !e.transplants.is_empty())
            .count();
        // Direct spawn edits that reference a FOREIGN effect also trigger a donor co-load (the
        // other source of stale ridley/bomberman). Count them so the studio can purge those too.
        let foreign_spawn_donors: usize = self
            .state
            .effect_call_edits
            .iter()
            .map(|(mv_key, edits)| {
                let fighter = mv_key.split('/').next().unwrap_or("");
                edits
                    .iter()
                    .filter(|e| match &e.op {
                        crate::data::EffectCallOp::Modify(c)
                        | crate::data::EffectCallOp::Add(c) => {
                            let n = c.effect_name.to_lowercase();
                            !n.starts_with("sys_") && !n.starts_with(&format!("{fighter}_"))
                        }
                        crate::data::EffectCallOp::Remove => false,
                    })
                    .count()
            })
            .sum();
        let mut remove_op: Option<usize> = None;
        let mut clear_ops = false;
        let mut clear_ops_all = false;
        let mut clear_foreign_edits = false;
        // A real OS window rather than an in-canvas `egui::Window`, so it can be moved to
        // another monitor. This must be the *immediate* flavour: the deferred one requires a
        // `'static + Send + Sync` closure, and this body borrows `&mut self` (the whole app
        // state) throughout. The eff editor viewport is built the same way for the same reason.
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("eff_transplant"),
            egui::ViewportBuilder::default()
                .with_app_id(crate::app_icon::APP_ID)
                .with_icon(crate::app_icon::viewport_icon())
                .with_title("Transplant Effects — Visionary")
                .with_inner_size(transplant_size)
                .with_position(transplant_pos)
                .with_min_inner_size([380.0, 320.0]),
            |ui, class| {
                // Draw inside a CentralPanel so the viewport gets the normal panel background.
                egui::CentralPanel::default().show_inside(ui, |ui| {
                if fighter_names.is_empty() {
                    ui.colored_label(egui::Color32::GRAY, "Load a game data root first.");
                    return;
                }
                let pool = self.effect_pool.as_ref().unwrap();
                let (done, total) = pool.progress();
                ui.horizontal(|ui| {
                    ui.label("Target:");
                    egui::ComboBox::from_id_salt("transplant_target_combo")
                        .selected_text(target.clone().unwrap_or_else(|| "— pick fighter —".into()))
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for name in &fighter_names {
                                let is = target.as_deref() == Some(name.as_str());
                                if ui.selectable_label(is, name).clicked() {
                                    self.transplant_target = Some(name.clone());
                                    self.transplant_replace = None;
                                }
                            }
                        });
                    if scanning {
                        ui.label(
                            egui::RichText::new(format!("scanning effs… {done}/{total}"))
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    }
                });
                // Show what's ALREADY recorded for this fighter (these all get baked/co-loaded
                // together). Prevents the "I picked X but it applied Y" confusion — a prior
                // donor stays until removed.
                if !recorded.is_empty() {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Transplanted effects ({}):", recorded.len()))
                                .strong()
                                .small(),
                        );
                        if ui
                            .small_button("Clear this fighter")
                            .on_hover_text(
                                "Remove every transplanted effect listed above from this fighter \
                                 and unload them from the game",
                            )
                            .clicked()
                        {
                            clear_ops = true;
                        }
                    });
                    for (i, op) in recorded.iter().enumerate() {
                        let scope = if op.one_slot_slots.len() == 1 {
                            format!("one-slot transplant, c{:02}", op.one_slot_slots[0])
                        } else if op.one_slot_slots.is_empty() {
                            "EFF transplant, all skins".to_string()
                        } else {
                            format!(
                                "skin-scoped transplant: {}",
                                op.one_slot_slots
                                    .iter()
                                    .map(|s| format!("c{s:02}"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        };
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("Remove")
                                .on_hover_text(
                                    "Remove this transplant and immediately rebuild the live carrier",
                                )
                                .clicked()
                            {
                                remove_op = Some(i);
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} → {} ({scope})",
                                    op.src_set_name, op.new_entry_name
                                ))
                                .small(),
                            );
                        });
                    }
                    ui.separator();
                }
                // GLOBAL purge — ALWAYS visible so stale donors can be cleared even when the
                // current fighter has none recorded. Two sources feed the co-load: recorded
                // transplants (any fighter) AND direct spawn edits that name a foreign effect. Both
                // silently rode along as the ridley/bomberman bug; both are purgeable here.
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "Staged for the game — {all_transplants} transplant(s) on {all_transplant_fighters} fighter(s), \
                         {foreign_spawn_donors} foreign spawn-edit(s)"
                    ))
                    .small()
                    .color(egui::Color32::GRAY),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(all_transplants > 0, egui::Button::new("Clear every fighter").small())
                        .on_hover_text(
                            "Remove ALL recorded transplants across every fighter and unload \
                             their runtime carriers",
                        )
                        .clicked()
                    {
                        clear_ops_all = true;
                    }
                    if ui
                        .add_enabled(
                            foreign_spawn_donors > 0,
                            egui::Button::new("Clear foreign spawn-edits").small(),
                        )
                        .on_hover_text(
                            "Remove every spawn edit that references another fighter's / assist's \
                             effect — the OTHER source of stale donor co-loads",
                        )
                        .clicked()
                    {
                        clear_foreign_edits = true;
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Apply to:");
                    if ui
                        .selectable_label(self.one_slot_slots.is_empty(), "All skins")
                        .on_hover_text("Transplant into the base EFF for every skin")
                        .clicked()
                    {
                        self.one_slot_slots.clear();
                    }
                    ui.label(
                        egui::RichText::new(format!("{} skin(s) on disk", avail_slots.len()))
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                    // Bulk controls matter once a fighter has dozens of slots.
                    if avail_slots.len() > crate::data::VANILLA_SLOT_COUNT as usize {
                        if ui.small_button("Select all").clicked() {
                            self.one_slot_slots = avail_slots.iter().copied().collect();
                        }
                        if ui
                            .small_button("Extra only")
                            .on_hover_text("Select every slot past the vanilla c00–c07")
                            .clicked()
                        {
                            self.one_slot_slots = avail_slots
                                .iter()
                                .copied()
                                .filter(|s| *s >= crate::data::VANILLA_SLOT_COUNT)
                                .collect();
                        }
                    }
                });
                // A slot list is now unbounded (mods use large indices), so the buttons wrap
                // and, past a few rows' worth, scroll instead of stretching the window.
                let slot_grid = |ui: &mut egui::Ui, sel: &mut std::collections::BTreeSet<u8>| {
                    ui.horizontal_wrapped(|ui| {
                        for s in &avail_slots {
                            let on = sel.contains(s);
                            if ui.selectable_label(on, format!("c{s:02}")).clicked() {
                                if on {
                                    sel.remove(s);
                                } else {
                                    sel.insert(*s);
                                }
                            }
                        }
                    });
                };
                if avail_slots.len() > 24 {
                    egui::ScrollArea::vertical()
                        .id_salt("one_slot_slot_grid")
                        .max_height(110.0)
                        .show(ui, |ui| slot_grid(ui, &mut self.one_slot_slots));
                } else {
                    slot_grid(ui, &mut self.one_slot_slots);
                }
                if !self.one_slot_slots.is_empty() {
                    let selected_skin_count = self.one_slot_slots.len();
                    // One selected slot is the classic one-slot; several is the same
                    // transplant, just scoped to more than one costume.
                    let scope_term = if selected_skin_count == 1 {
                        "One-slot transplant"
                    } else {
                        "Skin-scoped transplant"
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{scope_term}: the donor REPLACES a chosen entry in place (all its \
                             uses switch on those costumes, no redirect step) and exports as \
                             ef_<fighter>_cXX.eff — loading costume-specific EFF files in-game \
                             requires costume-specific EFF loading support."
                        ))
                        .small()
                        .color(egui::Color32::from_rgb(200, 200, 120)),
                    );
                }
                let Some(target) = &target else {
                    ui.colored_label(egui::Color32::GRAY, "Pick a target fighter.");
                    return;
                };
                let search_response = ui.add(
                    egui::TextEdit::singleline(&mut self.transplant_search)
                        .hint_text("Search every effect entry (all fighters + sys/common)…")
                        .desired_width(f32::INFINITY),
                );
                if search_response.changed() {
                    self.transplant_sel = None;
                    self.transplant_new_name.clear();
                    self.transplant_replace = None;
                }

                // Live kinds that match — effects the running game has actually used.
                let q = self.transplant_search.to_lowercase();
                let live_matches: Vec<String> = self
                    .game_link
                    .kinds()
                    .into_iter()
                    .map(|(_, k)| k.name)
                    .filter(|n| !n.starts_with("0x") && (q.is_empty() || n.to_lowercase().contains(&q)))
                    .take(6)
                    .collect();

                let results = pool.search(&self.transplant_search, 200);
                let pool_root = pool.root().to_path_buf();
                let own_rel = format!("effect/fighter/{target}/ef_{target}.eff");
                let own_entries: Vec<String> = pool.entries_of(&own_rel);
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
                                .transplant_sel
                                .as_ref()
                                .map(|(_, n)| n == name)
                                .unwrap_or(false);
                            let label = match &file {
                                Some(rel) => format!("● {name}  ({rel})"),
                                None => format!("● {name}"),
                            };
                            if ui.selectable_label(sel, label).clicked() {
                                // Unscanned live kinds: derive the eff from the name's
                                // fighter prefix ("mario_fb_shoot" → ef_mario.eff).
                                let resolved = file.or_else(|| {
                                    let toks: Vec<&str> = name.split('_').collect();
                                    (1..=2.min(toks.len())).rev().find_map(|n| {
                                        let f = toks[..n].join("_");
                                        let rel = format!("effect/fighter/{f}/ef_{f}.eff");
                                        pool_root.join(&rel).exists().then_some(rel)
                                    })
                                });
                                if let Some(rel) = resolved {
                                    self.transplant_sel = Some((rel, name.clone()));
                                    self.transplant_new_name =
                                        format!(
                                            "{}{}",
                                            name.to_lowercase(),
                                            crate::mod_project::TRANSPLANT_SUFFIX
                                        );
                                } else {
                                    self.state.status = format!(
                                        "Couldn't locate the eff file holding '{name}' — let the eff scan finish."
                                    );
                                }
                            }
                        }
                        ui.separator();
                    }
                    ui.label(egui::RichText::new("All effects").strong().small());
                    for (rel, name) in &results {
                        let sel = self
                            .transplant_sel
                            .as_ref()
                            .map(|(r, n)| r == rel && n == name)
                            .unwrap_or(false);
                        if ui
                            .selectable_label(sel, format!("{name}  —  {rel}"))
                            .clicked()
                        {
                            self.transplant_sel = Some((rel.clone(), name.clone()));
                            self.transplant_new_name = format!(
                                            "{}{}",
                                            name.to_lowercase(),
                                            crate::mod_project::TRANSPLANT_SUFFIX
                                        );
                        }
                    }
                    if results.is_empty() {
                        ui.colored_label(egui::Color32::GRAY, "No matches (yet).");
                    }
                });

                ui.separator();
                // One-slot scoping selected: the transplant REPLACES an existing entry on
                // just those costumes, instead of appending a new entry for every skin.
                let one_slot_mode = !self.one_slot_slots.is_empty();
                if let Some((rel, donor)) = self.transplant_sel.clone() {
                    ui.label(format!("Donor: {donor}  ({rel})"));
                    if !one_slot_mode {
                        ui.horizontal(|ui| {
                            ui.label("New entry name:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.transplant_new_name)
                                    .desired_width(220.0),
                            );
                        });
                        // A transplant may take ANY name — except Visionary's own reserved
                        // namespace, which the editor generates for edited-effect clones.
                        // Allowing an overlap would let a user-named transplant and a
                        // generated clone claim the same kind, and the alias table can only
                        // hold one mapping per kind.
                        if crate::mod_project::is_reserved_entry_name(&self.transplant_new_name) {
                            ui.label(
                                egui::RichText::new(format!(
                                    "'{}' is reserved for edited-effect clones — pick another \
                                     name.",
                                    crate::mod_project::EDIT_CLONE_PREFIX
                                ))
                                .small()
                                .color(egui::Color32::from_rgb(0xE0, 0x60, 0x50)),
                            );
                        }
                    }
                    let cross = !rel.contains(&format!("/{target}/"));
                    if cross {
                        ui.label(
                            egui::RichText::new(
                                "EFF transplant: only this effect and its referenced textures, \
                                 primitives, and shaders are copied into runtime storage.",
                            )
                            .small()
                            .color(egui::Color32::from_rgb(200, 200, 120)),
                        );
                    }
                    if one_slot_mode {
                        ui.label(
                            egui::RichText::new(format!("Replaces which {target} effect?"))
                                .strong()
                                .small(),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.transplant_replace_search)
                                .hint_text("filter target entries…")
                                .desired_width(f32::INFINITY),
                        );
                        let filt = self.transplant_replace_search.to_lowercase();
                        egui::ScrollArea::vertical()
                            .id_salt("transplant_replace_list")
                            .max_height(140.0)
                            .show(ui, |ui| {
                                if own_entries.is_empty() {
                                    ui.colored_label(
                                        egui::Color32::GRAY,
                                        "Target entries not scanned yet — wait for the eff scan.",
                                    );
                                }
                                for name in own_entries
                                    .iter()
                                    .filter(|n| filt.is_empty() || n.to_lowercase().contains(&filt))
                                {
                                    let is = self.transplant_replace.as_deref() == Some(name.as_str());
                                    if ui.selectable_label(is, name).clicked() {
                                        self.transplant_replace = Some(name.clone());
                                    }
                                }
                            });
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "EFF transplanting previews the merged EFF immediately, then lets \
                                 you choose which spawns redirect to the transplanted copy.",
                            )
                            .small()
                            .color(egui::Color32::GRAY),
                        );
                    }
                    let selected_skin_count = self.one_slot_slots.len();
                    let (valid, button_label) = if one_slot_mode {
                        (
                            self.transplant_replace.is_some(),
                            if selected_skin_count == 1 {
                                format!(
                                    "One-slot transplant over {} + preview",
                                    self.transplant_replace.as_deref().unwrap_or("…")
                                )
                            } else {
                                format!(
                                    "Transplant {} into selected skins + preview",
                                    self.transplant_replace.as_deref().unwrap_or("…")
                                )
                            },
                        )
                    } else {
                        (
                            !self.transplant_new_name.trim().is_empty()
                                && !crate::mod_project::is_reserved_entry_name(
                                    &self.transplant_new_name,
                                ),
                            format!("Transplant into {target} + preview"),
                        )
                    };
                    if ui.add_enabled(valid, egui::Button::new(button_label)).clicked() {
                        let new_name = if one_slot_mode {
                            // Internal clone-set name (kept unique per replace target).
                            format!(
                                "{donor}_for_{}",
                                self.transplant_replace.as_deref().unwrap_or("slot")
                            )
                        } else {
                            self.transplant_new_name.trim().to_string()
                        };
                        do_transplant = Some((rel, donor, new_name));
                    }
                } else {
                    ui.colored_label(egui::Color32::GRAY, "Pick a donor effect above.");
                }
                });
                // Closing the OS window unticks the Windows-menu toggle.
                if class != egui::ViewportClass::EmbeddedWindow
                    && ui.ctx().input(|i| i.viewport().close_requested())
                {
                    self.show_transplant = false;
                }
                // Remember where the user put it, for the next launch.
                if let Some(g) = WindowGeometry::from_viewport(ui.ctx()) {
                    self.transplant_geometry = Some(g);
                }
            },
        );

        // Apply recorded-op edits (remove one / clear all) requested in the studio.
        // Every affected fighter is refreshed below so project state, preview, staged EFF,
        // aliases, and the runtime carrier all stop retaining the removed effect together.
        let mut changed_fighters: Vec<String> = Vec::new();
        let mut removed_spawn_names: Vec<(String, String)> = Vec::new();
        let mut change_status: Option<String> = None;
        if clear_ops_all {
            // Purge EVERY fighter's recorded transplants + any direct spawn-edit donors. This is the
            // fix for stale cross-fighter donors (ridley/bomberman) silently riding along into the
            // co-load because they were recorded on a fighter other than the one being tested.
            let n: usize = self.eff_mods.values().map(|e| e.transplants.len()).sum();
            for (fighter, e) in &mut self.eff_mods {
                if !e.transplants.is_empty() {
                    removed_spawn_names.extend(
                        e.transplants
                            .iter()
                            .map(|op| (fighter.clone(), op.new_entry_name.clone())),
                    );
                    changed_fighters.push(fighter.clone());
                    e.transplants.clear();
                }
            }
            change_status = Some(format!(
                "Removed {n} transplanted effect(s) across all fighters and refreshed the game"
            ));
        } else if clear_foreign_edits {
            // Remove every spawn edit that references a foreign effect (the other stale-donor
            // source). Keep sys_/own-fighter edits + Removes; drop now-empty move entries.
            let mut removed = 0usize;
            for (mv_key, edits) in self.state.effect_call_edits.iter_mut() {
                let fighter = mv_key.split('/').next().unwrap_or("").to_string();
                edits.retain(|e| match &e.op {
                    crate::data::EffectCallOp::Modify(c) | crate::data::EffectCallOp::Add(c) => {
                        let n = c.effect_name.to_lowercase();
                        let foreign =
                            !n.starts_with("sys_") && !n.starts_with(&format!("{fighter}_"));
                        if foreign {
                            removed += 1;
                        }
                        !foreign
                    }
                    crate::data::EffectCallOp::Remove => true,
                });
            }
            self.state
                .effect_call_edits
                .retain(|_, edits| !edits.is_empty());
            self.push_effect_rules();
            self.push_effect_aliases();
            change_status = Some(format!(
                "Removed {removed} foreign spawn-edit donor(s) from the game"
            ));
        } else if let Some(f) = target.as_ref() {
            if clear_ops {
                if let Some(e) = self.eff_mods.get_mut(f) {
                    if !e.transplants.is_empty() {
                        removed_spawn_names.extend(
                            e.transplants
                                .iter()
                                .map(|op| (f.clone(), op.new_entry_name.clone())),
                        );
                        e.transplants.clear();
                        changed_fighters.push(f.clone());
                    }
                }
                change_status = Some(format!(
                    "Removed all transplanted effects from {f} and refreshed the game"
                ));
            } else if let Some(i) = remove_op {
                if let Some(e) = self.eff_mods.get_mut(f) {
                    if i < e.transplants.len() {
                        let removed = e.transplants.remove(i);
                        removed_spawn_names.push((f.clone(), removed.new_entry_name.clone()));
                        changed_fighters.push(f.clone());
                        change_status = Some(format!(
                            "Removed transplanted effect '{}' from {f} and refreshed the game",
                            removed.new_entry_name
                        ));
                    }
                }
            }
        }
        if !changed_fighters.is_empty() {
            self.remove_transplant_spawn_edits(&removed_spawn_names);
            changed_fighters.sort();
            changed_fighters.dedup();
            for fighter in &changed_fighters {
                self.refresh_transplant_preview(fighter);
            }
            // Not published here — Send does that, same as recording a transplant. The empty
            // list stays meaningful once it gets there: it is what makes the plugin drop the
            // carrier bytes and release the removed effects, which is why Send publishes even
            // when nothing is left.
            self.eff_editor.mark_unsent();
        }
        if let Some(status) = change_status {
            self.state.status = status;
        }

        if let (Some((rel, donor, new_name)), Some(fighter)) = (do_transplant, target) {
            // Already an ascending, deduplicated set of real slot indices — no bitmask to
            // unpack, so slots past c07 (and past c15) survive to the recorded op.
            let one_slot_slots: Vec<u8> = self.one_slot_slots.iter().copied().collect();
            let replace = if one_slot_slots.is_empty() {
                None
            } else {
                self.transplant_replace.clone()
            };
            self.record_transplant(&fighter, &rel, &donor, &new_name, one_slot_slots, replace);
        }
    }

    /// Drop spawn redirects that point at entries being removed. Redirect creation mutates
    /// both the saved edit and the cached full move snapshot, so restore both and invalidate
    /// the corresponding live rule sets before the carrier disappears.
    fn remove_transplant_spawn_edits(&mut self, removed: &[(String, String)]) {
        let mut by_fighter: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for (fighter, name) in removed {
            by_fighter
                .entry(fighter.to_lowercase())
                .or_default()
                .insert(name.to_lowercase());
        }
        let mut restores: Vec<(String, usize, crate::data::EffectCall)> = Vec::new();
        let mut affected_moves: Vec<String> = Vec::new();
        for (move_key, edits) in &mut self.state.effect_call_edits {
            let fighter = move_key.split('/').next().unwrap_or("").to_lowercase();
            let Some(names) = by_fighter.get(&fighter) else {
                continue;
            };
            let before = edits.len();
            edits.retain(|edit| {
                let referenced = match &edit.op {
                    crate::data::EffectCallOp::Modify(call)
                    | crate::data::EffectCallOp::Add(call) => {
                        names.contains(&call.effect_name.to_lowercase())
                    }
                    crate::data::EffectCallOp::Remove => false,
                };
                if referenced {
                    if let Some(pristine) = &edit.pristine {
                        restores.push((move_key.clone(), edit.index, pristine.clone()));
                    }
                }
                !referenced
            });
            if edits.len() != before {
                affected_moves.push(move_key.clone());
            }
        }
        self.state
            .effect_call_edits
            .retain(|_, edits| !edits.is_empty());
        for (move_key, index, pristine) in restores {
            if let Some(call) = self
                .state
                .effect_call_full
                .get_mut(&move_key)
                .and_then(|calls| calls.get_mut(index))
            {
                *call = pristine;
            }
        }
        for move_key in &affected_moves {
            self.effect_rules_store.remove(move_key);
        }
        let current_affected = self
            .current_move_key()
            .as_ref()
            .map(|key| affected_moves.contains(key))
            .unwrap_or(false);
        if current_affected {
            self.apply_effect_call_edits_to_current();
            self.push_effect_rules();
        } else if !affected_moves.is_empty() {
            let all: Vec<crate::game_link::SpawnRuleWire> = self
                .effect_rules_store
                .values()
                .flatten()
                .cloned()
                .collect();
            self.game_link.send_spawn_rules(&all);
        }
    }

    fn refresh_transplant_preview(&mut self, fighter: &str) {
        let Some(eff) = self.eff_mods.get(fighter).cloned() else {
            return;
        };
        if !eff.transplants.is_empty() {
            self.build_merged_preview(fighter);
            return;
        }
        let base = self.resolve_eff_source(&eff.source_rel);
        self.eff_editor.set_merged_overlay(&base, None);
        if let Some(dir) = base.parent() {
            crate::scratch_dirs::remove_transplant_previews(dir);
        }
    }

    fn remove_transplant_from_editor(&mut self, request: crate::eff_editor::EffTransplantRemoval) {
        let Some(eff) = self.eff_mods.get_mut(&request.fighter) else {
            self.state.status = format!(
                "Couldn't remove '{}': its transplant record is no longer present",
                request.entry_name
            );
            return;
        };
        // The index is from this frame's project snapshot. Fall back to entry+donor matching
        // if another UI action changed ordering before the request was drained.
        let at_index_matches = eff
            .transplants
            .get(request.op_index)
            .map(|op| {
                let visible = op.replace_entry.as_deref().unwrap_or(&op.new_entry_name);
                visible.eq_ignore_ascii_case(&request.entry_name)
                    && op.src_set_name.eq_ignore_ascii_case(&request.donor_name)
            })
            .unwrap_or(false);
        let remove_index = if at_index_matches {
            Some(request.op_index)
        } else {
            eff.transplants.iter().position(|op| {
                let visible = op.replace_entry.as_deref().unwrap_or(&op.new_entry_name);
                visible.eq_ignore_ascii_case(&request.entry_name)
                    && op.src_set_name.eq_ignore_ascii_case(&request.donor_name)
            })
        };
        let Some(remove_index) = remove_index else {
            self.state.status = format!(
                "Couldn't remove '{}': its transplant record changed",
                request.entry_name
            );
            return;
        };
        let removed = eff.transplants.remove(remove_index);
        self.merged_build_failed.remove(&request.fighter);
        self.remove_transplant_spawn_edits(&[(
            request.fighter.clone(),
            removed.new_entry_name.clone(),
        )]);
        self.refresh_transplant_preview(&request.fighter);
        self.eff_editor.mark_unsent();
        self.state.status = format!(
            "Removed transplanted effect '{}' from {} — press Send to game to apply it there",
            request.entry_name, request.fighter
        );
    }

    /// Build the fighter's transplanted eff (source eff + every recorded transplant, via the
    /// same `rebuild_eff_bytes` the exporter uses) so the copied effect is visible before
    /// export. Surfaces transfer errors (e.g. primitives) in the status line instead of
    /// failing silently.
    ///
    /// The merged file is written as `_transplant_preview.eff` NEXT TO the source (so sibling
    /// merges — ef_common sys effects, trail/model textures — still resolve) and registered as
    /// the eff editor's overlay for the base file. Returns (base path, merged path).
    fn build_merged_preview(
        &mut self,
        fighter: &str,
    ) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let mut eff = self.eff_mods.get(fighter).cloned()?;
        if eff.transplants.is_empty() {
            return None;
        }
        // TRANSPLANTS ONLY: the merged view is the eff editor's PRISTINE baseline. Baking
        // authored edits in would make its working-vs-pristine diff empty and silently
        // wipe `authored` on the next sync. (deploy_live_eff still bakes them for the
        // game — live parity — this file is the editing baseline.)
        eff.authored.clear();
        // Emitter lists and spawn structures are diffed against this baseline too, so baking
        // them in would make the editor read them as "same as the file" and drop them on the
        // next sync — exactly the failure the authored edits above are held back to avoid.
        eff.rosters.clear();
        eff.entry_edits.clear();
        let root = self.eff_editor.export_root().to_path_buf();
        let src_path = root.join(&eff.source_rel);
        let tmp = src_path
            .parent()
            .map(|dir| dir.join(crate::scratch_dirs::TRANSPLANT_PREVIEW_FILE))
            .unwrap_or_else(|| {
                crate::scratch_dirs::app_storage_root()
                    .join(crate::scratch_dirs::TRANSPLANT_PREVIEW_FILE)
            });
        // A preview written by a pre-rename build sits under the old name in the same
        // directory. Sweep it now so it isn't orphaned next to the user's dump forever.
        if let Some(dir) = tmp.parent() {
            let _ =
                std::fs::remove_file(dir.join(crate::scratch_dirs::LEGACY_TRANSPLANT_PREVIEW_FILE));
        }
        let src_bytes = match std::fs::read(&src_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.eff_editor.set_merged_overlay(&src_path, None);
                let _ = std::fs::remove_file(&tmp);
                self.state.status = format!(
                    "EFF transplant preview failed: couldn't read '{}': {e}",
                    src_path.display()
                );
                return None;
            }
        };
        match crate::eff_export::rebuild_eff_bytes(&src_bytes, &eff, Some(&root)) {
            Ok(merged) => {
                // Publish only a complete new preview. A failed/partial write must never leave
                // the editor pointed at the previous donor's bytes.
                let next = tmp.with_extension("eff.next");
                if let Err(e) =
                    std::fs::write(&next, &merged).and_then(|_| std::fs::rename(&next, &tmp))
                {
                    let _ = std::fs::remove_file(&next);
                    self.eff_editor.set_merged_overlay(&src_path, None);
                    let _ = std::fs::remove_file(&tmp);
                    self.state.status = format!(
                        "EFF transplant preview failed writing '{}': {e}",
                        tmp.display()
                    );
                    return None;
                }
                self.eff_editor.set_merged_overlay(&src_path, Some(&tmp));
                Some((src_path, tmp))
            }
            Err(e) => {
                self.eff_editor.set_merged_overlay(&src_path, None);
                let _ = std::fs::remove_file(&tmp);
                self.state.status = format!("EFF transplant merge failed: {e}");
                None
            }
        }
    }

    fn preview_transplant_result(&mut self, fighter: &str) {
        let Some(eff) = self.eff_mods.get(fighter).cloned() else {
            return;
        };
        let Some((base, _tmp)) = self.build_merged_preview(fighter) else {
            if !eff.transplants.is_empty() {
                self.state.status =
                    "EFF transplant recorded, but the target EFF isn't on disk to preview — export to apply.".into();
            }
            return;
        };
        // Open the base file in the eff editor — the overlay resolves it to the merged
        // view — and land on the new/replaced entry.
        self.eff_editor.queue_load(&base);
        if let Some(op) = eff.transplants.last() {
            let focus = op
                .replace_entry
                .clone()
                .unwrap_or_else(|| op.new_entry_name.clone());
            self.eff_editor.queue_select(&focus);
        }
        self.state.status = format!("EFF transplant applied — previewing merged EFF for {fighter}");
    }

    /// Push the full live-alias list to the plugin: every transplant op maps its
    /// copy/replaced entry hash to the donor's hash, so spawns of entries that only
    /// exist after export are LIVE-substituted by their (content-identical) donor —
    /// the running game matches the export. Costume-scoped ops carry their slots.
    ///
    /// Kirby's cross-file ops remain aliased because the hidden live carrier makes every
    /// transplanted ORIGINAL kind resident. Other fighters still exclude foreign aliases.
    ///
    /// `#[track_caller]` so `donor_send_log.txt` records which of the ~10 call sites produced
    /// each snapshot: a snapshot that omits the carrier target destroys and recreates the live
    /// carrier in-game, and the caller identifies the path that publishes an incomplete one.
    /// Request a donor/carrier publication. The snapshot is built and sent once per UI frame by
    /// [`Self::flush_effect_aliases`], from the final project state.
    #[track_caller]
    fn push_effect_aliases(&mut self) {
        let caller = std::panic::Location::caller();
        let caller = format!("{}:{}", caller.file(), caller.line());
        if !self.carrier_push_callers.contains(&caller) {
            self.carrier_push_callers.push(caller);
        }
    }

    /// Hand a payload to the plugin by DISK where possible, base64 otherwise.
    ///
    /// The emulator's sdmc is a directory on this machine, so the plugin can read the bytes
    /// straight out of it. Sending them instead meant base64 (+33%) inside a JSON frame over a
    /// socket the emulator reads in 8 KB chunks — several MB per donor, and the carrier on top.
    /// base64 stays as the fallback for when that directory cannot be found.
    fn donor_payload_wire(
        &self,
        arc_path: String,
        bytes: &[u8],
        dbg: &mut String,
    ) -> crate::game_link::DonorBytesWire {
        // One file per arc path, overwritten each send: the plugin reads it immediately, and a
        // stable name keeps the directory from growing without bound.
        let name = format!("{}.eff", arc_path.replace(['/', '\\', ':'], "_"));
        let rel = format!("effect_viewer/payload/{name}");
        // Only ever the REAL SD root. This used to build the path itself and `create_dir_all`
        // it, which on Windows quietly created a junk tree in the user's profile and reported
        // success — so this branch was always taken, the base64 fallback below was never
        // reached, and the plugin was told to read a payload it could not see.
        if let Some(sd) = crate::scratch_dirs::emulator_sd_root() {
            let dir = sd.join("effect_viewer").join("payload");
            // Write-then-rename: the plugin must never read a half-written payload.
            let final_path = dir.join(&name);
            let staging = dir.join(format!("{name}.next"));
            let wrote = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&staging, bytes))
                .and_then(|_| std::fs::rename(&staging, &final_path));
            match wrote {
                Ok(()) => {
                    dbg.push_str(&format!("  via disk: {rel} ({} B)\n", bytes.len()));
                    return crate::game_link::DonorBytesWire {
                        path: arc_path,
                        b64: String::new(),
                        file: rel,
                    };
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&staging);
                    dbg.push_str(&format!(
                        "  disk write failed ({e}); falling back to base64\n"
                    ));
                }
            }
        }
        use base64::Engine;
        crate::game_link::DonorBytesWire {
            path: arc_path,
            b64: base64::engine::general_purpose::STANDARD.encode(bytes),
            file: String::new(),
        }
    }

    fn flush_effect_aliases(&mut self) {
        if self.carrier_push_callers.is_empty() {
            return;
        }
        let caller = std::mem::take(&mut self.carrier_push_callers).join(",");
        let mut aliases: Vec<crate::game_link::EffectAliasWire> = Vec::new();
        // The carrier attaches to the fighter the user currently has selected — that is the
        // one whose effects are on screen and being edited. (This was hardcoded to Kirby
        // while the mechanism was being proven.)
        let carrier_fighter: Option<String> = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.to_lowercase())
            .or_else(|| {
                // No selection yet: fall back to the fighter whose eff the editor has open,
                // so an "Apply to game" straight from the eff editor still lands.
                self.eff_editor
                    .loaded_rel()
                    .map(|rel| crate::mod_project::fighter_from_source_rel(&rel))
                    .filter(|f| !f.is_empty())
            });
        for (fighter, eff) in &self.eff_mods {
            let own_rel = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
            for op in &eff.transplants {
                // Is the donor's ORIGINAL kind resident in a normal match? Same-file, same-
                // fighter, and sys/common donors are. A FOREIGN donor (another fighter, an
                // assist, an enemy…) is NOT — that character isn't in the match.
                let rel = op.src_file_rel.to_lowercase();
                let donor_resident = rel.is_empty()
                    || rel == own_rel
                    || op.src_set_name.to_lowercase().starts_with("sys_")
                    || rel.starts_with(&format!("effect/fighter/{fighter}/"));
                // The CARRIER fighter is the exception: its carrier (built below) stores every
                // selected foreign original, so both default `<donor>_tp` and custom transplant
                // names can safely alias onto those loaded kinds even though the donor's own eff
                // is not resident. Any other fighter has no carrier, so a non-resident donor
                // would alias onto a kind that does not exist and the spawn would silently die.
                let is_carrier_fighter = carrier_fighter.as_deref() == Some(fighter.as_str());
                if !donor_resident && !is_carrier_fighter {
                    continue;
                }
                let to = effect_name_hash(&op.src_set_name);
                let from = match &op.replace_entry {
                    Some(t) => effect_name_hash(t),
                    None => effect_name_hash(&op.new_entry_name),
                };
                if from == to {
                    continue;
                }
                let wire = crate::game_link::EffectAliasWire {
                    from,
                    to,
                    slots: op.one_slot_slots.clone(),
                };
                if !aliases.contains(&wire) {
                    aliases.push(wire);
                }
            }
        }
        // NOTE: aliases are SENT further down, after the authored-edit clones have added
        // theirs. Sending here would ship a list missing every authored redirect (the plugin
        // does a full-list replace, so a partial list silently disables them).
        const LIVE_CARRIER_REL: &str = "effect/assist/bomberman/ef_bomberman.eff";
        // EVERY transplant of the carrier fighter rides the carrier, including one sourced from
        // that fighter's own eff.
        //
        // Own-eff sources used to be filtered out here, on the reasoning that the bytes are
        // "already resident so no carrier is needed". That is false whenever the transplant
        // introduces a new name: `kirby_dash_tp` exists in no loaded eff, so nothing can serve
        // it. The snapshot came out empty, the deploy fell through to the merged-fighter-eff
        // path, and that path reparses the live fighter's effect slot mid-match — the mechanism
        // we ruled out. It froze the game.
        //
        // The filter is now unconditional on purpose. A narrower rule (skip own-eff REPLACE
        // ops, which genuinely can alias onto the resident kind) left `carrier_ops` out of step
        // with the project's transplant list, and the authored-edit pass — which searched the
        // latter — then attached an edit to an entry the carrier never built. That failed the
        // whole build and made an unrelated, previously working transplant invisible. One list,
        // one rule: whatever is here is what the carrier holds.
        let own_prefix: Option<String> = carrier_fighter
            .as_ref()
            .map(|f| format!("effect/fighter/{f}/"));
        let mut carrier_ops: Vec<crate::mod_project::TransplantOp> = carrier_fighter
            .as_ref()
            .and_then(|f| self.eff_mods.get(f))
            .map(|eff| {
                eff.transplants
                    .iter()
                    .filter(|op| rides_the_carrier(&op.src_file_rel))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // An op the carrier cannot take has to be reported, not dropped in silence. Silence here
        // reads as "sent" and leaves the user waiting on a carrier that was never going to exist.
        // Nothing with a real donor path is refused today; this stays so that if a donor class is
        // ever excluded again, it cannot be excluded quietly.
        let skipped_donors: Vec<String> = carrier_fighter
            .as_ref()
            .and_then(|f| self.eff_mods.get(f))
            .map(|eff| {
                eff.transplants
                    .iter()
                    .filter(|op| !op.src_file_rel.is_empty())
                    .filter(|op| !rides_the_carrier(&op.src_file_rel))
                    .map(|op| format!("{} ({})", op.src_set_name, op.src_file_rel))
                    .collect()
            })
            .unwrap_or_default();

        // AUTHORED EDITS RIDE THE CARRIER TOO.
        //
        // The fighter's own eff cannot be hot-reloaded mid-match (`reparse_game_path` rebuilds
        // from the resident buffer and never re-requests the file, so edited bytes were never
        // read — `cb_game=0`). The carrier's eff IS reloadable, which is why transplants work.
        // So an edited entry is cloned INTO the carrier under a `_tp` name with its edits baked
        // in, and the original kind is aliased onto that clone. Same mechanism as a
        // cross-fighter transplant; the "donor" just happens to be the fighter itself.
        //
        // The loop below APPENDS clone ops to `carrier_ops`, so "is this entry already
        // transplanted?" is asked against a snapshot taken first. Asking the live vector would
        // let one edit's clone answer for the next edit's lookup.
        let transplant_ops = carrier_ops.clone();
        // Texture imports ride the carrier whole: they name a pool texture, not an entry, so
        // there is nothing to clone or alias — the carrier's pool already holds the texture
        // under the donor's name once the transplant has copied it.
        let carrier_textures: Vec<crate::mod_project::TextureImport> = self
            .eff_mods
            .iter()
            .filter(|(fighter, _)| Some(fighter.to_lowercase()) == carrier_fighter)
            .flat_map(|(_, eff)| eff.textures.iter().cloned())
            .collect();
        // Added and removed pool textures ride along the same way. An addition also has to reach
        // the carrier for a swap onto it to resolve, so these travel as one set.
        let carrier_new_textures: Vec<crate::mod_project::TextureAddition> = self
            .eff_mods
            .iter()
            .filter(|(fighter, _)| Some(fighter.to_lowercase()) == carrier_fighter)
            .flat_map(|(_, eff)| eff.textures_added.iter().cloned())
            .collect();
        let carrier_gone_textures: Vec<String> = self
            .eff_mods
            .iter()
            .filter(|(fighter, _)| Some(fighter.to_lowercase()) == carrier_fighter)
            .flat_map(|(_, eff)| eff.textures_removed.iter().cloned())
            .collect();
        // Where donor and fighter effs may live: the effect pool's root, the eff editor's export
        // root, the data root, plus mod roots (a modded character's own eff lives there, not in
        // the vanilla dump). An assist eff under a different root than the fighter data was the
        // silent-skip bug, so every lookup tries all of them.
        //
        // Built HERE rather than at the donor-stripping loop below because the texture-import
        // pass needs to read the fighter's own eff, and that has to happen before the aliases
        // are sent a few lines down.
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        if let Some(p) = self.effect_pool.as_ref() {
            roots.push(p.root().to_path_buf());
        }
        roots.push(self.eff_editor.export_root().to_path_buf());
        if let Some(d) = &self.export_dir {
            roots.push(d.clone());
        }
        if let Some(d) = &self.state.data_root {
            roots.push(d.clone());
        }
        roots.extend(self.extra_roots.iter().cloned());
        let mut carrier_authored: Vec<crate::eff_export::CarrierAuthored> = Vec::new();
        let mut authored_aliases: Vec<(String, String)> = Vec::new();
        let mut carrier_name_conflicts: Vec<String> = Vec::new();
        for (fighter, eff) in &self.eff_mods {
            // Only the selected fighter's edits ride the carrier: there is ONE live carrier
            // and it is attached to that fighter's target, so folding another fighter's
            // clones in would bloat it with entries nothing can spawn.
            if Some(fighter.to_lowercase()) != carrier_fighter
                || (eff.authored.is_empty() && eff.rosters.is_empty() && eff.entry_edits.is_empty())
            {
                continue;
            }
            let src_rel = if eff.source_rel.is_empty() {
                format!("effect/fighter/{fighter}/ef_{fighter}.eff")
            } else {
                eff.source_rel.to_lowercase()
            };
            // Group this fighter's edits by the set they target — one clone per edited set,
            // however many emitters within it were touched.
            // Group by the ENTRY (kind) name — the unit the game spawns and the unit a
            // transplant clones. Edits with no kind recorded (projects saved before
            // `entry_name` existed) are SKIPPED rather than guessed: `src_set_name` resolves
            // against `entry_names`, so passing an emitter-set name there names a kind that
            // does not exist and ships a carrier the game's loader can hang on.
            //
            // An edited emitter LIST rides the same clone, and is grouped the same way: it is a
            // change to the same entry, and shipping it separately would mean two clones of one
            // effect racing to be the one the alias points at.
            type CarrierGroup = (
                String,
                usize,
                Vec<crate::mod_project::AuthoredEdit>,
                Option<crate::mod_project::EmitterRoster>,
                Option<crate::mod_project::EntryEdit>,
            );
            let mut by_entry: Vec<CarrierGroup> = Vec::new();
            for edit in &eff.authored {
                if edit.entry_name.is_empty() {
                    continue;
                }
                match by_entry.iter_mut().find(|(n, ..)| *n == edit.entry_name) {
                    Some((_, _, v, _, _)) => v.push(edit.clone()),
                    None => by_entry.push((
                        edit.entry_name.clone(),
                        edit.set_idx,
                        vec![edit.clone()],
                        None,
                        None,
                    )),
                }
            }
            for roster in &eff.rosters {
                if roster.entry_name.is_empty() {
                    continue;
                }
                match by_entry.iter_mut().find(|(n, ..)| *n == roster.entry_name) {
                    Some((_, _, _, slot, _)) => *slot = Some(roster.clone()),
                    None => by_entry.push((
                        roster.entry_name.clone(),
                        roster.set_idx,
                        Vec::new(),
                        Some(roster.clone()),
                        None,
                    )),
                }
            }
            for header in &eff.entry_edits {
                match by_entry
                    .iter_mut()
                    .find(|(name, ..)| name.eq_ignore_ascii_case(&header.entry_name))
                {
                    Some((_, _, _, _, slot)) => *slot = Some(header.clone()),
                    None => by_entry.push((
                        header.entry_name.clone(),
                        0,
                        Vec::new(),
                        None,
                        Some(header.clone()),
                    )),
                }
            }
            for (entry_name, set_idx, edits, roster, entry_edit) in by_entry {
                let entry_lc = entry_name.to_lowercase();

                // Is this edit targeting an entry that is ITSELF a transplant? The editor
                // works on the MERGED eff, so an edited entry may be a copy the user pulled
                // in from a donor. That copy does not exist in the fighter's vanilla eff, so
                // cloning "from the fighter" would fail to resolve and take the whole carrier
                // build down. The carrier already stores that donor entry (under the donor's
                // own name) and the transplant's alias already points at it, so all this case
                // needs is the edits applied to that existing entry — no second clone, no
                // extra alias.
                //
                // Search the ops that will ACTUALLY be built, not the project's raw transplant
                // list. Those two diverged once, and the edit then named an entry no transplant
                // had created, which failed the whole carrier build and took every unrelated
                // transplant down with it. And use the same stored-name rule the build uses, or
                // the edit lands on a name the carrier does not hold.
                let via_transplant = transplant_ops.iter().find(|t| {
                    t.new_entry_name.eq_ignore_ascii_case(&entry_lc)
                        || t.replace_entry
                            .as_deref()
                            .is_some_and(|r| r.eq_ignore_ascii_case(&entry_lc))
                });
                if let Some(t) = via_transplant {
                    carrier_authored.push(crate::eff_export::CarrierAuthored {
                        set_name: carrier_stored_name(t, own_prefix.as_deref()),
                        edits,
                        roster,
                        entry_edit,
                    });
                    continue;
                }

                // Reserved internal namespace — NOT the transplant suffix, which the user
                // may type themselves. See `EDIT_CLONE_PREFIX`.
                let clone_name = format!("{}{entry_lc}", crate::mod_project::EDIT_CLONE_PREFIX);
                carrier_ops.push(crate::mod_project::TransplantOp {
                    new_entry_name: clone_name.clone(),
                    src_file_rel: src_rel.clone(),
                    // Resolved against the donor's `entry_names`: KIND name, never set name.
                    src_set_name: entry_lc.clone(),
                    src_set_idx: set_idx,
                    one_slot_slots: Vec::new(),
                    replace_entry: None,
                });
                // The clone's emitter set takes the transplant's new entry name, which is what
                // `apply_authored` matches on inside the carrier.
                carrier_authored.push(crate::eff_export::CarrierAuthored {
                    set_name: clone_name.clone(),
                    edits,
                    roster,
                    entry_edit,
                });
                authored_aliases.push((entry_lc, clone_name));
            }
        }

        // A TEXTURE IMPORT HAS TO BRING ITS EFFECTS WITH IT.
        //
        // `apply_texture_imports` resolves the texture name against the CARRIER's pool, and the
        // carrier only holds textures its own entries reference. So replacing a texture in the
        // fighter's eff did nothing unless an entry sampling it happened to already be
        // transplanted: the import was dropped with a warning and the user watched the original
        // render in game. Clone the entries that actually sample the texture, exactly as an
        // authored edit clones the entry it edits — the clone carries the texture into the
        // carrier's pool, the pruner keeps it (something now samples it), and the import lands.
        //
        // Entries already riding the carrier are skipped: a transplant or an authored edit has
        // brought the texture in already, and a second clone would only add a duplicate entry
        // and a competing alias for the same kind.
        if !carrier_textures.is_empty() {
            if let Some(fighter) = carrier_fighter.as_ref() {
                let src_rel = self
                    .eff_mods
                    .get(fighter)
                    .filter(|eff| !eff.source_rel.is_empty())
                    .map(|eff| eff.source_rel.to_lowercase())
                    .unwrap_or_else(|| format!("effect/fighter/{fighter}/ef_{fighter}.eff"));
                let bytes = roots
                    .iter()
                    .map(|r| r.join(&src_rel))
                    .find(|p| p.is_file())
                    .and_then(|p| std::fs::read(p).ok());
                let parsed = bytes.and_then(|b| effect_library::NamcoEffectFile::load(&b).ok());
                if let Some(file) = parsed {
                    for import in &carrier_textures {
                        for (entry_name, set_idx) in
                            crate::eff_export::entries_sampling_texture(&file, &import.texture_name)
                        {
                            let entry_lc = entry_name.to_lowercase();
                            // Already on the carrier, by transplant or by authored edit? Match
                            // the SOURCE FILE too: an entry of the same name in a different eff
                            // is a different effect sampling a different pool, and treating it
                            // as "already carried" would skip the clone that actually brings
                            // this texture in — the silent no-op again, just better hidden.
                            let riding = transplant_ops.iter().any(|t| {
                                t.src_file_rel.eq_ignore_ascii_case(&src_rel)
                                    && t.src_set_name.eq_ignore_ascii_case(&entry_lc)
                            }) || authored_aliases
                                .iter()
                                .any(|(from, _)| from.eq_ignore_ascii_case(&entry_lc));
                            if riding {
                                continue;
                            }
                            let clone_name =
                                format!("{}{entry_lc}", crate::mod_project::EDIT_CLONE_PREFIX);
                            if carrier_ops.iter().any(|op| op.new_entry_name == clone_name) {
                                continue; // two imports sharing one effect
                            }
                            carrier_ops.push(crate::mod_project::TransplantOp {
                                new_entry_name: clone_name.clone(),
                                src_file_rel: src_rel.clone(),
                                src_set_name: entry_lc.clone(),
                                src_set_idx: set_idx,
                                one_slot_slots: Vec::new(),
                                replace_entry: None,
                            });
                            authored_aliases.push((entry_lc, clone_name));
                        }
                    }
                }
            }
        }

        // Redirect spawns of the original kind onto the edited clone.
        for (from_name, to_name) in &authored_aliases {
            let wire = crate::game_link::EffectAliasWire {
                from: effect_name_hash(from_name),
                to: effect_name_hash(to_name),
                slots: Vec::new(),
            };
            if wire.from == wire.to {
                continue;
            }
            // One `from` may only map to ONE `to`. A transplant named `<x>_tp` and an authored
            // clone of `<x>` compute the same name, and shipping two wires with the same `from`
            // leaves the plugin picking arbitrarily. The transplant alias is registered first
            // and wins; the authored edit then has nowhere to live, so say so rather than
            // silently rendering the unedited copy.
            if let Some(existing) = aliases.iter().find(|a| a.from == wire.from) {
                if existing.to != wire.to {
                    carrier_name_conflicts.push(from_name.clone());
                }
                continue;
            }
            aliases.push(wire);
        }
        if !carrier_name_conflicts.is_empty() {
            self.state.status = format!(
                "Effect name clash — rename the transplant(s) for: {}. The authored edit was \
                 not applied because another alias already claims that kind.",
                carrier_name_conflicts.join(", ")
            );
        }
        self.game_link.send_effect_aliases(&aliases);
        // Register every name the plugin may need to resolve for display: transplant copies
        // and the authored clones.
        let names: Vec<String> = self
            .eff_mods
            .values()
            .flat_map(|eff| eff.transplants.iter().map(|op| op.new_entry_name.clone()))
            .chain(carrier_authored.iter().map(|c| c.set_name.clone()))
            .collect();
        self.game_link.send_effect_names(&names);
        let live_carrier_rel = LIVE_CARRIER_REL;
        // Does anything need the carrier at all? An imported texture counts on its own.
        //
        // This used to ask `!carrier_ops.is_empty()`, which is only true for transplants and
        // authored edits. A texture import is neither: it names a pool texture, not an entry, so
        // it adds no op. A texture-only send therefore built no carrier and published no carrier
        // spec — and the plugin reads a snapshot with no carrier, plus withdrawn bytes, as
        // "retire the live carrier". Nothing replaces it, so the editor sits on "retiring the
        // previous carrier" until the user gives up. The import has to arm the carrier itself.
        let carrier_needed = !carrier_ops.is_empty()
            || !carrier_textures.is_empty()
            || !carrier_new_textures.is_empty()
            || !carrier_gone_textures.is_empty();
        // Cross-fighter donors: have the plugin co-load each donor's VANILLA eff with the
        // target fighter (system effs are always resident — no need to co-load those).
        let mut per_target: HashMap<String, Vec<String>> = HashMap::new();
        // donor eff path (lowercase) → the ORIGINAL entry names referenced from it. Used to
        // build the small stripped eff the plugin injects as resident data.
        let mut donor_wants: HashMap<String, Vec<String>> = HashMap::new();
        for (fighter, eff) in &self.eff_mods {
            let target = if eff.source_rel.is_empty() {
                format!("effect/fighter/{fighter}/ef_{fighter}.eff")
            } else {
                eff.source_rel.to_lowercase()
            };
            let donors = per_target.entry(target.clone()).or_default();
            for op in &eff.transplants {
                let rel = op.src_file_rel.to_lowercase();
                donors.push(rel.clone());
                donor_wants
                    .entry(rel)
                    .or_default()
                    .push(op.src_set_name.to_lowercase());
            }
        }
        // Also cover DIRECT references: a spawn edit naming a foreign effect (another
        // fighter's, an assist trophy's, …) needs that eff resident too — otherwise the
        // spawn silently does nothing in-game.
        for (mv_key, edits) in &self.state.effect_call_edits {
            let Some(fighter) = mv_key.split('/').next() else {
                continue;
            };
            let target = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
            for edit in edits {
                let name = match &edit.op {
                    crate::data::EffectCallOp::Modify(c) | crate::data::EffectCallOp::Add(c) => {
                        c.effect_name.to_lowercase()
                    }
                    crate::data::EffectCallOp::Remove => continue,
                };
                if name.starts_with("sys_") || name.starts_with(&format!("{fighter}_")) {
                    continue; // resident with the fighter already
                }
                if let Some(rel) = self
                    .effect_pool
                    .as_ref()
                    .and_then(|p| p.file_of_entry(&name))
                {
                    let rel = rel.to_lowercase();
                    per_target
                        .entry(target.clone())
                        .or_default()
                        .push(rel.clone());
                    donor_wants.entry(rel).or_default().push(name.clone());
                }
            }
        }
        // The carrier attaches to the SELECTED fighter's eff target, so the plugin co-loads it
        // alongside whichever fighter is actually on screen.
        let carrier_target: Option<String> = carrier_fighter
            .as_ref()
            .map(|f| format!("effect/fighter/{f}/ef_{f}.eff"));
        if carrier_needed {
            if let Some(target) = &carrier_target {
                // The old detached co-loader is deliberately disabled: send only the selected
                // fresh carrier path to the plugin; its bytes are built below.
                per_target.insert(target.clone(), vec![live_carrier_rel.into()]);
            }
        }
        let mut donor_specs: Vec<crate::game_link::DonorEffWire> = Vec::new();
        for (target, mut donors) in per_target {
            donors.retain(|rel| {
                !rel.is_empty() && *rel != target && !rel.starts_with("effect/system")
            });
            donors.sort();
            donors.dedup();
            if !donors.is_empty() {
                donor_specs.push(crate::game_link::DonorEffWire { target, donors });
            }
        }
        donor_specs.sort_by(|a, b| a.target.cmp(&b.target));

        let mut donor_bytes: Vec<crate::game_link::DonorBytesWire> = Vec::new();
        let mut dbg = String::new();
        let mut carrier_added = false;
        // A refused carrier build (unsafe shader container, incompatible compute variation) drops
        // every live transplant. That must be visible, not just a line in the debug dump.
        let mut carrier_error: Option<String> = None;
        // Authored edits the build could not place. The carrier still ships without them, so
        // these are reported separately from an outright build failure.
        let mut carrier_dropped_edits: Vec<String> = Vec::new();
        // Two mesh-backed sources competing for the carrier's single model container.
        let mut carrier_mesh_conflict: Option<String> = None;
        // Texture imports the carrier's pool had no texture for.
        let mut carrier_dropped_textures: Vec<String> = Vec::new();
        // Textures the build would not delete because something still samples them.
        let mut carrier_textures_in_use: Vec<String> = Vec::new();
        if carrier_needed {
            // Sources may legitimately live under DIFFERENT roots: the carrier is an assist
            // eff from the vanilla dump, while a modded fighter's own eff is under a mod
            // root. Requiring one root that holds everything silently skipped the carrier in
            // that case (logged only as CARRIER ROOT MISS). Resolve per file instead, and
            // only fall back to the all-in-one-root requirement for the builder's `donor_root`
            // argument, which still takes a single base.
            let resolve = |rel: &str| -> Option<std::path::PathBuf> {
                let rel = rel.to_lowercase();
                roots.iter().map(|r| r.join(&rel)).find(|p| p.is_file())
            };
            let carrier_path = resolve(live_carrier_rel);
            let missing: Vec<String> = carrier_ops
                .iter()
                .filter(|op| resolve(&op.src_file_rel).is_none())
                .map(|op| op.src_file_rel.clone())
                .collect();
            // The builder resolves donors relative to ONE root, so prefer a root that holds
            // every source; otherwise fall back to the root holding the carrier itself and
            // let the builder report precisely what it could not read.
            let shared_root = roots
                .iter()
                .find(|root| {
                    root.join(live_carrier_rel).is_file()
                        && carrier_ops
                            .iter()
                            .all(|op| root.join(op.src_file_rel.to_lowercase()).is_file())
                })
                .or_else(|| {
                    missing
                        .is_empty()
                        .then(|| roots.iter().find(|r| r.join(live_carrier_rel).is_file()))
                        .flatten()
                });
            if shared_root.is_none() && carrier_path.is_some() && !missing.is_empty() {
                carrier_error = Some(format!(
                    "carrier sources not found under any effect root: {}",
                    missing.join(", ")
                ));
            }
            if let Some(root) = shared_root {
                match std::fs::read(root.join(live_carrier_rel)) {
                    Ok(carrier_base) => {
                        // Names the carrier must hold VERBATIM: an authored clone is a copy
                        // of the fighter's OWN entry, so storing it under the donor name would
                        // collide with the fighter's resident kind — and `carrier_authored`
                        // resolves its edits by this exact name. `carrier_stored_name` is that
                        // rule, shared with the authored-edit pass so the two cannot drift.
                        let mut seen_names = std::collections::HashSet::new();
                        let carrier_transplants: Vec<crate::mod_project::TransplantOp> =
                            carrier_ops
                                .iter()
                                .filter_map(|op| {
                                    let donor_rel = op.src_file_rel.to_lowercase();
                                    let donor_name = op.src_set_name.to_lowercase();
                                    let stored = carrier_stored_name(op, own_prefix.as_deref());
                                    // Dedup on what actually gets stored, so an authored clone
                                    // and a transplant can never overwrite each other.
                                    if !seen_names.insert(stored.clone()) {
                                        return None;
                                    }
                                    Some(crate::mod_project::TransplantOp {
                                        new_entry_name: stored,
                                        src_file_rel: donor_rel,
                                        src_set_name: donor_name,
                                        src_set_idx: op.src_set_idx,
                                        one_slot_slots: Vec::new(),
                                        replace_entry: None,
                                    })
                                })
                                .collect();
                        let mut warnings: Vec<String> = Vec::new();
                        let carrier_bytes =
                            crate::eff_export::rebuild_runtime_carrier_eff_bytes_with_edits(
                                &carrier_base,
                                live_carrier_rel,
                                &carrier_transplants,
                                root,
                                &carrier_authored,
                                &carrier_textures,
                                &carrier_new_textures,
                                &carrier_gone_textures,
                                &mut warnings,
                            );
                        for w in &warnings {
                            dbg.push_str(&format!("CARRIER WARNING: {w}\n"));
                        }
                        // An edit whose target was missing is not fatal — the transplants still
                        // ship — but it did not reach the game, so it must never be counted as
                        // sent below.
                        let skipped_authored: Vec<String> = warnings
                            .iter()
                            .filter_map(|w| w.strip_prefix("edit:").map(str::to_string))
                            .collect();
                        carrier_authored.retain(|c| !skipped_authored.contains(&c.set_name));
                        if !skipped_authored.is_empty() {
                            carrier_dropped_edits = skipped_authored;
                        }
                        // An import whose texture is not in the CARRIER's pool. The carrier only
                        // holds the textures its transplanted entries brought with them, so a
                        // texture the user replaced in the fighter's own eff is absent unless
                        // some entry sampling it also rides the carrier. Silently shipping a
                        // carrier without it looks identical to the import not working.
                        carrier_dropped_textures = warnings
                            .iter()
                            .filter_map(|w| w.strip_prefix("texture:").map(str::to_string))
                            .collect();
                        // A delete the build refused. The editor's own guard only sees sampler0
                        // (its emitter model discards the other five), so a texture used as a
                        // distortion or mask input can pass that check and still be in use here.
                        // Without this the delete would look like it worked and quietly not have.
                        carrier_textures_in_use = warnings
                            .iter()
                            .filter_map(|w| w.strip_prefix("texture-in-use:").map(str::to_string))
                            .collect();
                        // A model-data conflict ships a carrier whose geometry is known-wrong.
                        // That has to reach the user directly: it is not something they can
                        // diagnose from what they see in game.
                        carrier_mesh_conflict = warnings
                            .iter()
                            .find_map(|w| w.strip_prefix("mesh-conflict:").map(str::to_string));
                        match carrier_bytes {
                            Ok(bytes) => {
                                // Preserve the exact outbound carrier for post-test structural
                                // inspection. This is cache-only and is replaced on every build.
                                let _ = std::fs::write(
                                    crate::scratch_dirs::app_storage_root()
                                        .join("carrier_send_debug.eff"),
                                    &bytes,
                                );
                                let stored = carrier_ops
                                    .iter()
                                    .map(|op| {
                                        format!("{} from {}", op.src_set_name, op.src_file_rel)
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                dbg.push_str(&format!(
                                    "CARRIER {live_carrier_rel}: {} stored effect(s) [{stored}] = {} B\n",
                                    carrier_ops.len(),
                                    bytes.len()
                                ));
                                donor_bytes.push(self.donor_payload_wire(
                                    live_carrier_rel.into(),
                                    &bytes,
                                    &mut dbg,
                                ));
                                carrier_added = true;
                            }
                            Err(e) => {
                                dbg.push_str(&format!("CARRIER MULTI-FILE MERGE FAILED: {e}\n"));
                                carrier_error = Some(format!("{e}"));
                            }
                        }
                    }
                    Err(e) => dbg.push_str(&format!(
                        "CARRIER BASE READ FAILED {live_carrier_rel}: {e}\n"
                    )),
                }
            } else {
                let needed = carrier_ops
                    .iter()
                    .map(|op| op.src_file_rel.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                dbg.push_str(&format!(
                    "CARRIER ROOT MISS: need {live_carrier_rel} and [{needed}] under one effect root\n"
                ));
            }
        }
        for (rel, mut names) in donor_wants {
            if rel.is_empty() || rel.starts_with("effect/system") {
                continue;
            }
            if carrier_added
                && (rel == live_carrier_rel
                    || carrier_ops
                        .iter()
                        .any(|op| op.src_file_rel.eq_ignore_ascii_case(&rel)))
            {
                continue;
            }
            names.sort();
            names.dedup();
            // Resolve the donor file: the rel may be pool-relative, or already absolute.
            let bytes = {
                let abs = std::path::Path::new(&rel);
                let mut found = if abs.is_absolute() {
                    std::fs::read(abs).ok()
                } else {
                    None
                };
                if found.is_none() {
                    for r in &roots {
                        if let Ok(b) = std::fs::read(r.join(&rel)) {
                            found = Some(b);
                            break;
                        }
                    }
                }
                found
            };
            let Some(bytes) = bytes else {
                dbg.push_str(&format!("MISS file {rel} (names {names:?})\n"));
                continue;
            };
            // Strip the donor to the entries actually wanted before sending it.
            //
            // This used to send the FULL eff, because stripping once made alucard_backdash spawn
            // and render nothing. That was the BFRES relocation defect, since fixed and
            // calibrated against the game's own containers — a stripped pool arrived
            // structurally intact and unusable on hardware, which looks exactly like "dropped
            // resources". The transport base64s each donor into a single JSON frame, so the full
            // file was extremely expensive: ef_marx.eff is 20 MB, ~27 MB on the wire, and it
            // strips to 542 KB (2.7%) in ~5 ms.
            //
            // A strip failure is NOT fatal: fall back to the whole file, which is slow but
            // always correct, rather than dropping the donor and rendering nothing.
            let (payload, how) = match crate::eff_export::strip_donor_eff_bytes(
                &bytes,
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            ) {
                Ok(stripped) if stripped.len() < bytes.len() => (stripped, "stripped"),
                Ok(_) => (bytes.clone(), "whole (strip saved nothing)"),
                Err(e) => {
                    dbg.push_str(&format!("STRIP FAILED {rel}: {e}\n"));
                    (bytes.clone(), "whole (strip failed)")
                }
            };
            dbg.push_str(&format!(
                "OK {rel}: {how} {} B from {} B ({} names)\n",
                payload.len(),
                bytes.len(),
                names.len()
            ));
            donor_bytes.push(self.donor_payload_wire(rel, &payload, &mut dbg));
        }
        let _ = std::fs::write(
            crate::scratch_dirs::app_storage_root().join("donor_send_debug.txt"),
            format!("sending {} donor buffer(s)\n{dbg}", donor_bytes.len()),
        );
        // Make the carrier's outcome VISIBLE. When an authored edit does not show up in
        // game, the first question is always "was the carrier actually built and did it
        // contain the edited entry" — that used to be answerable only by reading a debug
        // file on the SD card.
        // Ordered most-actionable first: a mesh conflict means what shipped is visibly wrong in
        // a way nothing else will explain, so it outranks the ordinary success line.
        //
        // A refused transplant outranks even that. Nothing was sent for it, so every other line
        // here would be describing a build the user's effect is not in.
        if !skipped_donors.is_empty() {
            self.state.status = format!(
                "NOT sent — the live carrier cannot take {}. Export the eff to use these.",
                skipped_donors.join(", ")
            );
        } else if let Some(conflict) = &carrier_mesh_conflict {
            self.state.status = format!("Model data — {conflict}");
        } else if carrier_added && !carrier_textures_in_use.is_empty() {
            self.state.status = format!(
                "Texture(s) {} were NOT deleted — emitters still sample them, through a sampler \
                 slot the editor cannot see. Point those emitters elsewhere first.",
                carrier_textures_in_use.join(", ")
            );
        } else if carrier_added && !carrier_dropped_textures.is_empty() {
            // Ranked above the ordinary success line: the carrier shipped, but the replacement
            // the user is waiting to see is not in it, and nothing in game will explain why.
            self.state.status = format!(
                "Carrier sent, but the replaced texture(s) {} are not in it — the carrier only \
                 carries textures belonging to effects that ride it. Transplant or edit an \
                 effect that uses the texture and send again.",
                carrier_dropped_textures.join(", ")
            );
        } else if carrier_added && !carrier_authored.is_empty() {
            let names: Vec<&str> = carrier_authored
                .iter()
                .map(|c| c.set_name.as_str())
                .collect();
            self.state.status = format!(
                "Live carrier sent: {} transplant(s) + {} edited effect(s) [{}]",
                carrier_ops.len() - carrier_authored.len().min(carrier_ops.len()),
                carrier_authored.len(),
                names.join(", ")
            );
        } else if carrier_added && !carrier_dropped_edits.is_empty() {
            // The carrier shipped, but every edit in it was dropped. Saying nothing here would
            // read as success.
            self.state.status = format!(
                "Carrier sent, but edits to {} could not be placed and were NOT applied",
                carrier_dropped_edits.join(", ")
            );
        } else if carrier_added && !carrier_textures.is_empty() {
            // Texture-only send: no authored edit and no transplant to report, so without this
            // branch a successful send said nothing at all.
            self.state.status = format!(
                "Live carrier sent: {} replaced texture(s)",
                carrier_textures.len()
            );
        } else if (!carrier_authored.is_empty() || !carrier_textures.is_empty()) && !carrier_added {
            let what = if carrier_authored.is_empty() {
                "Texture replacements"
            } else {
                "Edited effects"
            };
            self.state.status = match &carrier_error {
                Some(e) => format!("{what} NOT sent — carrier build failed: {e}"),
                None => {
                    format!("{what} NOT sent — carrier was not built (see donor debug log)")
                }
            };
        }
        if carrier_needed && !carrier_added {
            // Never arm the stable carrier path with bytes left over from an older selection.
            if let Some(target) = &carrier_target {
                donor_specs.retain(|spec| &spec.target != target);
            }
            if let Some(e) = &carrier_error {
                self.state.status = format!("Live transplant carrier not built — {e}");
            }
        }
        // Bytes first: a same-path carrier replacement must be staged before the game-thread
        // receives the new spec and creates its object. Empty is meaningful and clears old data.
        donor_bytes.sort_by(|a, b| a.path.cmp(&b.path));
        self.log_donor_push(&caller, &carrier_ops, carrier_added, &donor_specs);
        self.game_link.send_donor_bytes(&donor_bytes);
        self.game_link.send_donor_effs(&donor_specs);
    }

    /// Append one line per published donor snapshot to `donor_send_log.txt`. Pairs each push
    /// with its call site so a snapshot that silently drops the Kirby carrier target can be
    /// traced back to the action that produced it, and records the exact recorded transplant
    /// names so a mismatch between the editor's display and the runtime carrier is attributable
    /// to one side or the other.
    fn log_donor_push(
        &self,
        caller: &str,
        carrier_ops: &[crate::mod_project::TransplantOp],
        carrier_added: bool,
        donor_specs: &[crate::game_link::DonorEffWire],
    ) {
        use std::io::Write;
        // Whether the snapshot carries a fighter-eff target at all (the carrier attaches to
        // one). Kept generic: this used to look for Kirby specifically.
        let has_fighter_spec = donor_specs
            .iter()
            .any(|spec| spec.target.starts_with("effect/fighter/"));
        let targets = donor_specs
            .iter()
            .map(|spec| spec.target.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let path = crate::scratch_dirs::app_storage_root().join("donor_send_log.txt");
        // Keep the file bounded: it is written on every user action for the whole session.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let ops = carrier_ops
                .iter()
                .map(|op| format!("{}<-{}", op.new_entry_name, op.src_set_name))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(
                f,
                "caller={caller} carrier_ops={} carrier_added={carrier_added} \
                 fighter_spec={has_fighter_spec} ops=[{ops}] targets=[{targets}]",
                carrier_ops.len(),
            );
        }
    }

    /// Serve this fighter's MERGED eff (transplants + authored edits baked) to the running
    /// game: write it + a manifest onto the Eden SD, where the plugin's Arcropolis file
    /// callback provides it whenever the game loads `effect/fighter/<f>/ef_<f>.eff`.
    /// Cross-fighter donor content thereby loads WITH the fighter — no export, no donor
    /// fighter in the match; re-entering the match picks it up. Returns success.
    fn deploy_live_eff(&mut self, fighter: &str) -> bool {
        let Some(eff) = self.eff_mods.get(fighter).cloned() else {
            return false;
        };
        // An edit-free EffMod is intentional after the last transplant is removed: serving
        // the pristine source bytes replaces the previously merged resident file and makes
        // removal take effect immediately instead of leaving the old live payload staged.
        let root = self.eff_editor.export_root().to_path_buf();
        let src = root.join(&eff.source_rel);
        let Ok(bytes) = std::fs::read(&src) else {
            self.state.status = format!("Live deploy: source eff missing ({})", src.display());
            return false;
        };
        let merged = match crate::eff_export::rebuild_eff_bytes(&bytes, &eff, Some(&root)) {
            Ok(m) => m,
            Err(e) => {
                self.state.status = format!("Live deploy failed to merge: {e}");
                return false;
            }
        };
        // The `ultimate/` validation this used to do inline now lives in `emulator_sd_root`,
        // which is what every SD-writing path in the editor goes through.
        let Some(sd) = crate::scratch_dirs::emulator_sd_root() else {
            self.state.status = "Live deploy: emulator SD card not found. Set VISIONARY_SD_DIR \
                 to the folder containing your emulator's `ultimate` directory."
                .into();
            return false;
        };
        let dir = sd.join("effect_viewer").join("live_eff");
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let fname = format!("ef_{fighter}_merged.eff");
        if let Err(e) = std::fs::write(dir.join(&fname), &merged) {
            self.state.status = format!("Live deploy write failed: {e}");
            return false;
        }
        // DELIBERATELY NOT STAGED as an Arcropolis mod any more.
        //
        // This used to also write the merged eff to `ultimate/mods/effect_viewer_live/<arc
        // path>` as a "guaranteed fallback", because Arcropolis rewrites the arc file table
        // at BOOT and serves it straight from disk. That worked — too well: it is a real,
        // permanent mod, so every live edit silently persisted across restarts with nothing
        // in the editor indicating why a fighter was still modified. Live edits are session
        // state; only an explicitly exported mod (or a loaded project) may persist.
        //
        // The live path is the carrier, which is pushed over the wire and dies with the
        // session. `clear_stale_live_state` removes anything older builds left behind.
        // Merge into the manifest the plugin reads (path = the arc path the game asks for).
        let mpath = dir.join("manifest.json");
        let mut entries: Vec<serde_json::Value> = std::fs::read_to_string(&mpath)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let arc_path = eff.source_rel.to_lowercase();
        entries.retain(|e| e.get("path").and_then(|p| p.as_str()) != Some(arc_path.as_str()));
        entries.push(serde_json::json!({ "path": arc_path, "file": fname }));
        if let Ok(json) = serde_json::to_string_pretty(&entries) {
            let _ = std::fs::write(&mpath, json);
        }
        // Refresh the plugin's served map so the staged bytes are what loads at the NEXT
        // match entry. This part is safe and is how transplants have always loaded.
        self.game_link.send_live_eff_reload();
        //
        // DELIBERATELY NOT SENT: `send_force_reread(&arc_path)`.
        //
        // That asked the plugin to swap the FIGHTER's resident eff and reparse it mid-match.
        // It does not work and is not safe: `reparse_game_path` rebuilds the parsed emitter
        // structs from the resident buffer and never re-requests the file, so the merged
        // bytes were never read (`cb_game=0` in `effect_viewer_cb.txt`) — and driving that
        // path froze the game. Live changes go through the CARRIER instead
        // (`push_effect_aliases`), which is reloadable by design.
        let _ = &arc_path;
        self.live_eff_deployed.insert(fighter.to_string());
        // Serving-chain diagnosis a few seconds later, once async re-loads settled.
        self.live_eff_probe_due =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
        true
    }

    /// Service an eff-editor "apply authored edits to the game" request.
    ///
    /// Authored PTCL values are per emitter and the runtime modifier protocol has no
    /// per-emitter message — a kind-level `rainbow.color` multiplier necessarily recolours
    /// the WHOLE effect. So the only correct application is the one the exporter already
    /// uses: fold the editor's diff into the project store, rebuild the fighter's eff with
    /// `rebuild_eff_bytes` (per-emitter exact), and hot-reload it in the running game.
    /// Returns the status note the editor shows in its own feedback line.
    fn apply_authored_eff_live(&mut self) -> String {
        let Some(rel) = self.eff_editor.loaded_rel() else {
            return "no eff loaded — nothing to apply".to_string();
        };
        // Fold the editor's current diff into the project store first: the carrier snapshot
        // is built from `eff_mods`, not from the editor's working copy.
        self.sync_eff_mods_from_editor();
        let fighter = crate::mod_project::fighter_from_source_rel(&rel);
        let entry = self.eff_mods.get(&fighter);
        let known = entry.is_some();
        let (edits, transplants) = entry
            .map(|e| (e.authored.len(), e.transplants.len()))
            .unwrap_or((0, 0));
        // The completion condition depends on what we are asking the plugin to do. A populated
        // snapshot succeeds when a new live carrier object appears; an empty snapshot succeeds
        // when the old object and all of its kinds are gone. Treating both as "wait for live"
        // made a successful restore sit for 25 seconds and then report a failure.
        self.eff_editor.carrier_expected = entry.is_some_and(|e| !e.is_empty());
        // Publish through the CARRIER, not the fighter's own eff. The fighter eff cannot be
        // reloaded mid-match (`reparse_game_path` rebuilds from the resident buffer and never
        // re-requests the file — `cb_game=0`), so edits only appeared after a full reboot. The
        // carrier's eff reloads for real; the snapshot clones each edited entry into it with
        // the edits baked in and aliases the original kind onto the clone.
        //
        // Unconditional, INCLUDING when both counts are zero. Recording and removing a
        // transplant no longer touch the game, so this is the only route there — and an EMPTY
        // snapshot is exactly how the plugin is told to drop the carrier bytes and destroy its
        // hidden object. Gating on "has content" (as this once did, on authored edits alone)
        // would make both a transplant-only project and the last removal unsendable.
        self.push_effect_aliases();
        // Stage the merged eff as well, whenever this fighter is one we track. The carrier
        // covers the running match, but the fighter's OWN eff is what loads on the next match
        // entry, and cross-fighter donor content exists only in the merged bytes. An
        // edit-free entry is deliberate too: it serves the pristine source bytes back, which
        // is what replaces a previously staged merged file after the last removal.
        if known {
            self.deploy_live_eff(&fighter);
        }
        let note = match (edits, transplants) {
            (0, 0) => format!("{fighter}: cleared — the live carrier is dropped"),
            (0, t) => format!("{fighter}: {t} transplant(s) queued onto the live carrier"),
            (e, 0) => format!("{fighter}: {e} emitter edit(s) queued onto the live carrier"),
            (e, t) => format!(
                "{fighter}: {e} emitter edit(s) + {t} transplant(s) queued onto the live carrier"
            ),
        };
        self.state.status = note.clone();
        note
    }

    /// Record the [`TransplantOp`](crate::mod_project::TransplantOp) for `fighter` and open
    /// the per-use redirect prompt (append mode) or finish immediately (replace mode).
    ///
    /// `one_slot_slots` is the ONE-SLOT scoping for this transplant: empty means every
    /// costume (base eff), otherwise the transplant only applies to those costume slots and
    /// `replace` names the entry it replaces in place on them.
    fn record_transplant(
        &mut self,
        fighter: &str,
        rel: &str,
        donor: &str,
        new_name: &str,
        one_slot_slots: Vec<u8>,
        replace: Option<String>,
    ) {
        // Entry names are recorded (and written into eff files) lowercase: ACMD/live kinds
        // hash lowercase names, so a mixed-case copy ("SYS_ICE_os") risks a kind hash the
        // game never matches — and reads inconsistently next to live-kind names.
        let new_name = new_name.to_lowercase();
        let new_name = new_name.as_str();
        let own_rel = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
        self.merged_build_failed.remove(fighter);
        let entry = self.eff_mods.entry(fighter.to_string()).or_default();
        if entry.source_rel.is_empty() {
            entry.source_rel = own_rel.clone();
        }
        let replace_mode = replace.is_some();
        entry.transplants.push(crate::mod_project::TransplantOp {
            new_entry_name: new_name.to_string(),
            src_file_rel: if rel == own_rel {
                String::new()
            } else {
                rel.to_string()
            },
            src_set_name: donor.to_lowercase(),
            src_set_idx: 0,
            one_slot_slots: one_slot_slots.clone(),
            replace_entry: replace.as_ref().map(|r| r.to_lowercase()),
        });
        // Recording a transplant does NOT hand it to the running game. Pushing the alias list
        // and deploying here rebuilt the carrier on every transplant, which respawns the
        // carrier item mid-match; sending is the Send button's job. The game keeps what it was
        // last given until then, and the unsent marker says so rather than leaving it silent.
        self.eff_editor.mark_unsent();
        // Cross-FIGHTER donors are a new structural entry in the fighter's eff: the game only
        // reads the merged bytes when the fighter's eff LOADS (match entry), and an in-match
        // reparse re-parses the already-resident buffer — it can't pull the new bytes in. So
        // those still need a match re-entry AFTER sending. Same-fighter/sys donors render
        // immediately via the donor alias (the donor is already resident).
        let cross_fighter = rel
            .strip_prefix("effect/fighter/")
            .and_then(|r| r.split('/').next())
            .is_some_and(|df| df != fighter);
        let live_note = if cross_fighter {
            "not sent to the game yet — press Send to game; a cross-fighter donor also needs a \
             match re-entry to load"
        } else {
            "not sent to the game yet — press Send to game"
        };
        if replace_mode {
            // One-slot-scoped replacement: every use of the entry switches on those
            // costumes automatically — no redirect step. Preview + done.
            let slot_list = one_slot_slots
                .iter()
                .map(|s| format!("c{s:02}"))
                .collect::<Vec<_>>()
                .join(", ");
            self.preview_transplant_result(fighter);
            self.state.status = if one_slot_slots.len() == 1 {
                format!(
                    "One-slot transplant: '{donor}' now replaces '{}' on {slot_list} — {live_note}; export writes the skin EFF",
                    replace.as_deref().unwrap_or("?")
                )
            } else {
                format!(
                    "Skin-scoped EFF transplant: '{donor}' now replaces '{}' on {slot_list} — {live_note}; export writes the skin EFFs",
                    replace.as_deref().unwrap_or("?")
                )
            };
            return;
        }
        self.state.status =
            format!("EFF transplant '{new_name}' recorded for {fighter} — {live_note}");

        // Full use discovery: reconstruct EVERY move performed live that spawns the donor
        // (not just moves already opened) into effect_call_full, so all real uses are listed
        // and redirectable — each move played in-game contributes its captured effect script.
        {
            let donor_hash = effect_name_hash(donor);
            let motion_name: HashMap<u64, String> = self
                .move_list
                .iter()
                .map(|m| (m.hash, m.name.clone()))
                .collect();
            let bone_rev = self.bone_reverse_map();
            let eff_rev = self.effect_reverse_map();
            let fighter_kind = self.current_fighter_kind();
            let mut motions: Vec<u64> = self
                .game_link
                .all_captures()
                .into_iter()
                .filter(|(_, l)| {
                    fighter_kind.is_none_or(|kind| l.kind == kind)
                        && effect_capture_layout(&l.func).is_some()
                        && l.args.first().and_then(|a| a.as_hash()) == Some(donor_hash)
                })
                .map(|(m, _)| m)
                .collect();
            motions.sort();
            motions.dedup();
            for m in motions {
                let Some(name) = motion_name.get(&m) else {
                    continue;
                };
                let key = format!("{fighter}/{name}");
                if self.state.effect_call_full.contains_key(&key) {
                    continue;
                }
                let motion_captures = self.captures_for_selected_fighter(m);
                let calls = Self::effect_calls_from_captures(&motion_captures, &bone_rev, &eff_rev);
                if !calls.is_empty() {
                    self.state.effect_call_full.insert(key, calls);
                }
            }
        }

        // Preview the merged result in-app immediately (build the transplanted eff in memory).
        self.preview_transplant_result(fighter);

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
                        label: format!("{key} — frame {} on {}", c.active_start, c.bone_name),
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

        // Full-use discovery: scan EVERY move script of this fighter in the background
        // (GitHub dump, disk-cached) so the prompt lists all real uses — not only the
        // moves that were already opened or performed live.
        // The scan used to fetch all ~450 move scripts serially through
        // `reqwest::blocking::get`, which builds a fresh client (DNS + TCP + TLS) per call —
        // measured ~236 ms each, so a cold cache cost roughly two minutes. Now: resolve
        // everything already on disk first (no network at all, ~5 ms for a whole fighter),
        // then pull only the misses over a shared pooled client from a small thread pool.
        let (tx, rx) = std::sync::mpsc::channel();
        let fighter_s = fighter.to_string();
        std::thread::spawn(move || {
            let moves = match fetch_move_index(&fighter_s) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[USE-SCAN] move index fetch failed for {fighter_s}: {e}");
                    return;
                }
            };
            let _ = tx.send(UseScanMsg::Total(moves.len()));

            let scan_body = |body: &str| crate::acmd::parse_effect_script(body).to_effect_calls();

            // Pass 1 — disk cache only. Reports progress for every already-known move
            // immediately, so a warm scan finishes before the prompt has even repainted.
            let mut misses: Vec<String> = Vec::new();
            for pascal in moves {
                let snake = pascal_to_snake(&pascal);
                match crate::acmd::cached_script_body(&fighter_s, &snake) {
                    Some(body) => {
                        if tx.send(UseScanMsg::Move(snake, scan_body(&body))).is_err() {
                            return;
                        }
                    }
                    None => misses.push(snake),
                }
            }
            if misses.is_empty() {
                return;
            }

            // Pass 2 — network, in parallel over a shared connection pool. Workers pull from
            // one queue so a slow request never stalls the others.
            let queue = std::sync::Arc::new(std::sync::Mutex::new(misses));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let workers = crate::acmd::SCRIPT_FETCH_THREADS.min(
                queue
                    .lock()
                    .map(|q| q.len())
                    .unwrap_or(crate::acmd::SCRIPT_FETCH_THREADS),
            );
            let mut handles = Vec::with_capacity(workers);
            for _ in 0..workers {
                let queue = std::sync::Arc::clone(&queue);
                let cancel = std::sync::Arc::clone(&cancel);
                let tx = tx.clone();
                let fighter_s = fighter_s.clone();
                handles.push(std::thread::spawn(move || loop {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let Some(snake) = queue.lock().ok().and_then(|mut q| q.pop()) else {
                        return;
                    };
                    let calls = crate::acmd::fetch_script_body_cached(&fighter_s, &snake)
                        .map(|b| crate::acmd::parse_effect_script(&b).to_effect_calls())
                        .unwrap_or_default();
                    if tx.send(UseScanMsg::Move(snake, calls)).is_err() {
                        // The UI dropped the receiver (prompt closed) — stop the whole scan.
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                }));
            }
            // Drop the spawner's sender so the channel disconnects (= "scan finished") as
            // soon as the last worker exits.
            drop(tx);
            for h in handles {
                let _ = h.join();
            }
        });
        self.use_scan = Some(UseScan {
            rx,
            fighter: fighter.to_string(),
            done: 0,
            total: 0,
        });
    }

    /// Drain the background full-use scan: donor-using moves land in
    /// `effect_call_full` and (while it is open) in the redirect prompt.
    fn poll_use_scan(&mut self) {
        let Some(scan) = &mut self.use_scan else {
            return;
        };
        let fighter = scan.fighter.clone();
        let mut msgs: Vec<UseScanMsg> = Vec::new();
        let mut finished = false;
        loop {
            match scan.rx.try_recv() {
                Ok(m) => msgs.push(m),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        for m in msgs {
            match m {
                UseScanMsg::Total(t) => {
                    if let Some(s) = &mut self.use_scan {
                        s.total = t;
                    }
                }
                UseScanMsg::Move(mv, calls) => {
                    if let Some(s) = &mut self.use_scan {
                        s.done += 1;
                    }
                    if calls.is_empty() {
                        continue;
                    }
                    let Some(prompt) = &self.redirect_prompt else {
                        continue;
                    };
                    let donor_hash = effect_name_hash(&prompt.donor_name);
                    if !calls
                        .iter()
                        .any(|c| effect_name_hash(&c.effect_name) == donor_hash)
                    {
                        continue;
                    }
                    let key = format!("{fighter}/{mv}");
                    if self.state.effect_call_full.contains_key(&key) {
                        continue; // captured/opened already — its uses are listed
                    }
                    let new_uses: Vec<RedirectUse> = calls
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| effect_name_hash(&c.effect_name) == donor_hash)
                        .map(|(i, c)| RedirectUse {
                            move_key: key.clone(),
                            call_idx: i,
                            label: format!("{key} — frame {} on {}", c.active_start, c.bone_name),
                            selected: true,
                        })
                        .collect();
                    self.state.effect_call_full.insert(key.clone(), calls);
                    if let Some(prompt) = &mut self.redirect_prompt {
                        for u in new_uses {
                            if !prompt
                                .uses
                                .iter()
                                .any(|e| e.move_key == u.move_key && e.call_idx == u.call_idx)
                            {
                                prompt.uses.push(u);
                            }
                        }
                    }
                }
            }
        }
        if finished {
            if let Some(s) = &self.use_scan {
                if s.total > 0 {
                    self.state.status = format!("Move scan finished ({} scripts checked)", s.done);
                }
            }
            self.use_scan = None;
        }
    }

    /// "Which uses go to the new effect?" — per-use checkboxes, applied as call edits.
    fn draw_redirect_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.redirect_prompt else {
            return;
        };
        let mut action: Option<bool> = None; // Some(true)=apply, Some(false)=skip
        egui::Window::new("Redirect spawns to the transplanted effect?")
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
                if let Some(scan) = &self.use_scan {
                    ui.label(
                        egui::RichText::new(if scan.total > 0 {
                            format!("Scanning every move script… {}/{}", scan.done, scan.total)
                        } else {
                            "Scanning every move script…".to_string()
                        })
                        .small()
                        .color(egui::Color32::LIGHT_BLUE),
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(150));
                }
                if prompt.uses.is_empty() {
                    ui.colored_label(
                        egui::Color32::GRAY,
                        if self.use_scan.is_some() {
                            "No uses found yet — the scan is still running."
                        } else {
                            "No known uses — retarget calls later in the Effects panel."
                        },
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
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
                    let pristine_call = c.clone();
                    c.effect_name = prompt.new_name.clone();
                    let call = c.clone();
                    let edits = self
                        .state
                        .effect_call_edits
                        .entry(u.move_key.clone())
                        .or_default();
                    if let Some(e) = edits.iter_mut().find(|e| e.index == u.call_idx) {
                        e.op = crate::data::EffectCallOp::Modify(call.clone());
                        e.pristine.get_or_insert(pristine_call);
                    } else {
                        edits.push(crate::data::EffectCallEdit {
                            index: u.call_idx,
                            op: crate::data::EffectCallOp::Modify(call.clone()),
                            pristine: Some(pristine_call),
                        });
                    }
                    // Live swap rule for this other move (needs that move captured in-game).
                    let donor_hash = effect_name_hash(&prompt.donor_name);
                    if let Some(mname) = u.move_key.split_once('/').map(|(_, m)| m) {
                        let motion = hash40::hash40(&mname.to_lowercase()).0;
                        if let Some(inject) =
                            self.build_effect_inject(&call, Some(motion), donor_hash)
                        {
                            let (fs, fe) = Self::rule_frame_window(call.active_start);
                            let (fs, fe) = (fs.unwrap_or_default(), fe.unwrap_or_default());
                            let store = self
                                .effect_rules_store
                                .entry(u.move_key.clone())
                                .or_default();
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
        let all: Vec<crate::game_link::SpawnRuleWire> = self
            .effect_rules_store
            .values()
            .flatten()
            .cloned()
            .collect();
        self.game_link.send_spawn_rules(&all);
        // Live parity: the copy doesn't exist in the running game yet, but the plugin
        // aliases its hash to the donor's, so redirected spawns show in-game NOW and
        // match the export (a fresh copy is content-identical to its donor).
        self.push_effect_aliases();
        self.state.status = format!(
            "Redirected {n} spawn(s) from '{}' to '{}' — live in-game via donor alias; \
             export bakes the real entry",
            prompt.donor_name, prompt.new_name
        );
    }

    /// Record (or update) the Modify edit for effect call `i` in the current move.
    /// Added calls keep their `Add` record updated instead.
    fn record_effect_call_edit(&mut self, i: usize) {
        let Some(mv) = self.current_move_key() else {
            return;
        };
        let Some(call) = self.state.effects.get(i).cloned() else {
            return;
        };
        let is_added = i >= self.state.effects_pristine.len();
        let pristine_call = self.state.effects_pristine.get(i).cloned();
        let edits = self.state.effect_call_edits.entry(mv.clone()).or_default();
        if let Some(existing) = edits.iter_mut().find(|e| e.index == i) {
            existing.op = if is_added {
                crate::data::EffectCallOp::Add(call)
            } else {
                crate::data::EffectCallOp::Modify(call)
            };
            if let Some(p) = pristine_call {
                existing.pristine.get_or_insert(p);
            }
        } else {
            edits.push(crate::data::EffectCallEdit {
                index: i,
                op: crate::data::EffectCallOp::Modify(call),
                pristine: pristine_call,
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
        let Some(mv) = self.current_move_key() else {
            return;
        };
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
            let pristine_call = self.state.effects_pristine.get(i).cloned();
            let edits = self.state.effect_call_edits.entry(mv.clone()).or_default();
            if let Some(e) = edits.iter_mut().find(|e| e.index == i) {
                e.op = crate::data::EffectCallOp::Remove;
                if let Some(p) = pristine_call {
                    e.pristine.get_or_insert(p);
                }
            } else {
                edits.push(crate::data::EffectCallEdit {
                    index: i,
                    op: crate::data::EffectCallOp::Remove,
                    pristine: pristine_call,
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
    }

    /// Rebuild and push the current move's effect spawn rules: PER-SPAWN and frame/motion
    /// scoped, so editing one spawn's offset (or disabling it) never touches the other
    /// spawns of the same effect. Untouched spawns fire at their pristine frame with the
    /// script's values; a moved spawn gets a transform override; a RETIMED spawn is
    /// suppressed at its pristine frame and re-injected (from a live capture) at the new
    /// frame with its edited transform baked in. Only changed calls produce a rule.
    fn push_effect_rules(&mut self) {
        let Some(mv_key) = self.current_move_key() else {
            return;
        };
        let motion = self.current_motion_hash();
        let effects = self.state.effects.clone();
        let pristines = self.state.effects_pristine.clone();
        let mut rules: Vec<crate::game_link::SpawnRuleWire> = Vec::new();
        let mut missing_capture = false;
        for (i, ec) in effects.iter().enumerate() {
            let pristine = pristines.get(i);
            let spawn_frame = pristine.map(|p| p.active_start).unwrap_or(ec.active_start);
            let hash = effect_name_hash(&ec.effect_name);
            // The effect the SCRIPT actually spawns (before edits) — what to suppress.
            let orig_hash = pristine
                .map(|p| effect_name_hash(&p.effect_name))
                .unwrap_or(hash);
            let window = Self::rule_frame_window(spawn_frame);
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
            // A follow emitter has no automatic relationship to the editor's end frame.
            // Schedule the same EFFECT_OFF_KIND that exported ACMD emits. This dispatches
            // through the plugin's normal kill-kind hook, which redirects transplanted kinds
            // from the fighter to their hidden carrier owner.
            if ec.follows_bone && ec.active_end != 9999 {
                rules.push(crate::game_link::SpawnRuleWire {
                    eff_hash: hash,
                    suppress: false,
                    motion,
                    frame_start: None,
                    frame_end: None,
                    pos: None,
                    rot: None,
                    scale: None,
                    inject: Some(Self::build_effect_stop_inject(ec)),
                });
            }
            // Swap and/or retime: the effect NAME or FRAME changed → suppress the original
            // spawn and inject the new effect at the new frame (transform baked in). The
            // injected call reuses the original spawn's captured args with the graphic hash
            // swapped to the new effect. Needs a live capture of the original; without one,
            // fall back to a transform rule (export still applies the swap) and flag it.
            let retimed = pristine
                .map(|p| p.active_start != ec.active_start)
                .unwrap_or(false);
            let swapped = pristine
                .map(|p| {
                    orig_hash != hash
                        || p.effect_name_alt != ec.effect_name_alt
                        || p.spawn_func != ec.spawn_func
                })
                .unwrap_or(true);
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
                .map(|p| p.offset != ec.offset || p.rotation != ec.rotation || p.scale != ec.scale)
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
        let all: Vec<crate::game_link::SpawnRuleWire> = self
            .effect_rules_store
            .values()
            .flatten()
            .cloned()
            .collect();
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
        let captures = self.captures_for_selected_fighter(motion);
        let donor = captures.iter().find(|c| {
            effect_capture_layout(&c.func).is_some()
                && c.args.first().and_then(|a| a.as_hash()) == Some(donor_hash)
        })?;
        let (flip, _) = effect_capture_layout(&donor.func)?;
        let off = usize::from(flip);
        let mut args = donor.args.clone();
        // Vec layout (0-based, +off for flip): 0 gfx (0/1 for FLIP: gfxL/gfxR), 1 bone,
        // 2..4 pos xyz, 5..7 rot zr,yr,xr, 8 size.
        if args.len() < 9 + off {
            return None;
        }
        // Swap each graphic independently for FLIP variants. One side is often `null`, so
        // collapsing both slots to the primary name changes the move's facing-dependent VFX.
        args[0] = A::Hash(new_hash);
        if flip {
            args[1] = A::Hash(
                ec.effect_name_alt
                    .as_deref()
                    .map(effect_name_hash)
                    .unwrap_or(new_hash),
            );
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
            frame: Self::script_to_motion_frame(ec.active_start),
            // Existing captures carry the exact trailing args for this command. Preserve that
            // command unless an authored call explicitly supplies another compatible type.
            func: if ec.spawn_func.is_empty() {
                donor.func.clone()
            } else {
                ec.spawn_func.clone()
            },
            args,
        })
    }

    fn build_effect_stop_inject(ec: &crate::data::EffectCall) -> crate::game_link::SpawnInjectWire {
        use crate::game_link::LuaArgWire as A;
        crate::game_link::SpawnInjectWire {
            frame: Self::script_to_motion_frame(ec.active_end.max(ec.active_start)),
            func: "EFFECT_OFF_KIND".into(),
            args: vec![
                A::Hash(effect_name_hash(&ec.effect_name)),
                A::Bool(false),
                A::Bool(true),
            ],
        }
    }

    fn draw_scrubber(&mut self, ui: &mut Ui) {
        let total = self.state.total_frames.max(timeline_frame_extent(
            &self.state.hitboxes,
            &self.state.effects,
        ));
        if total == 0 {
            return;
        }

        let current = self.state.current_frame.clamp(FIRST_GAME_FRAME, total);

        // Playback controls
        ui.horizontal(|ui| {
            let play_label = if self.state.playing { "⏸" } else { "▶" };
            if ui.button(play_label).clicked() {
                self.state.playing = !self.state.playing;
            }
            if ui.button("|◀").clicked() {
                self.state.current_frame = FIRST_GAME_FRAME;
                self.state.playing = false;
            }
            ui.label(format!("Frame {} / {}", current, total));
        });

        // The existing bottom panel remains the only size control. Dense moves use compact rows
        // and scroll vertically inside it instead of growing the panel over the editor.
        let n_fx = self.state.effects.len();
        let row_height = TIMELINE_ROW_HEIGHT;
        let bar_height = (row_height - 2.0).max(3.0);
        let effect_height = (row_height * 0.45).max(3.0);
        let hb_band = self.state.hitboxes.len() as f32 * row_height;
        let timeline_height = timeline_content_height(self.state.hitboxes.len(), n_fx);
        let timeline_width = ui.available_width().max(1.0);
        let viewport_height = ui.available_height().max(1.0);

        timeline_scroll_area(viewport_height).show(ui, |ui| {
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(timeline_width, timeline_height),
                egui::Sense::click_and_drag(),
            );

            let painter = ui.painter_at(rect);
            let w = rect.width();

            // Background
            painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(20, 20, 30));

            let frame_start_to_x =
                |f: u32| -> f32 { rect.left() + timeline_frame_start_fraction(f, total) * w };
            let frame_end_to_x =
                |f: u32| -> f32 { rect.left() + timeline_frame_end_fraction(f, total) * w };

            // Hitbox bars
            for (row, hb) in self.state.hitboxes.iter().enumerate() {
                let y_top = rect.top() + 24.0 + row as f32 * row_height;
                let y_bot = y_top + bar_height;
                let color = hitbox_display_color(hb);
                let is_selected = self.selected_hitbox == Some(row);

                let start_x = frame_start_to_x(hb.active_start);
                let end_x = if hb.active_end == 9999 {
                    rect.right()
                } else {
                    frame_end_to_x(hb.active_end).min(rect.right())
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
                        egui::Color32::from_rgba_unmultiplied(
                            color.r(),
                            color.g(),
                            color.b(),
                            alpha,
                        ),
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
                    if bar_w > 30.0 && row_height >= 9.0 {
                        painter.text(
                            egui::pos2(start_x + 3.0, y_top + bar_height * 0.5),
                            egui::Align2::LEFT_CENTER,
                            format!("#{} {}", hb.id, hb.bone_name),
                            egui::FontId::monospace(10.0),
                            egui::Color32::WHITE,
                        );
                    }
                }
            }

            // Frame tick marks use the same one-based game-frame labels as ACMD and the game UI.
            for f in FIRST_GAME_FRAME..=total {
                if f != FIRST_GAME_FRAME && f % 5 != 0 {
                    continue;
                }
                let x = frame_start_to_x(f);
                let is_ten = f == FIRST_GAME_FRAME || f % 10 == 0;
                let tick_h = if is_ten { 8.0 } else { 4.0 };
                painter.line_segment(
                    [
                        egui::pos2(x, rect.top()),
                        egui::pos2(x, rect.top() + tick_h),
                    ],
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
                let y_top = fx_band_top + row as f32 * effect_height;
                let y_bot = y_top + (effect_height - 1.0).max(2.0);
                let end_frame = if e.follows_bone {
                    if e.active_end >= total {
                        total
                    } else {
                        e.active_end
                    }
                } else {
                    e.active_end
                        .max(e.active_start.saturating_add(12))
                        .min(total)
                };
                let start_x = frame_start_to_x(e.active_start.min(total));
                let end_x = frame_end_to_x(end_frame)
                    .max(start_x + 3.0)
                    .min(rect.right());
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
                        egui::Rect::from_min_max(
                            egui::pos2(start_x, y_top),
                            egui::pos2(end_x, y_bot),
                        ),
                        1.0,
                        egui::Stroke::new(1.0, egui::Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                }
            }

            // Playhead
            let px = frame_start_to_x(current);
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
                            let row = ((pos.y - bar_area_top) / row_height) as usize;
                            if row < self.state.hitboxes.len() {
                                let hb = &self.state.hitboxes[row];
                                let start_x = frame_start_to_x(hb.active_start);
                                let end_x = if hb.active_end == 9999 {
                                    rect.right()
                                } else {
                                    frame_end_to_x(hb.active_end).min(rect.right())
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
                        self.state.current_frame = timeline_frame_at_fraction(t, total);
                        self.state.playing = false;
                    }
                }
            }
        });
    }
}

impl eframe::App for VisionaryApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let frame_t = self.perf.start();
        let ctx = ui.ctx().clone();
        // Keep the game link alive whenever the app runs — live offsets, hitbox rules,
        // ACMD capture, the reconnect modal, and the Transplant pool all need it, not just
        // the Eff Editor window (which used to be the only thing that started it).
        self.game_link.ensure_started();
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

        // Restore/track persisted window geometry.
        self.update_window_geometry(&ctx);

        // The editor is a separate viewport, so give it a real keyboard route in addition to
        // the Windows menu. This is useful when the pointer is busy on another monitor and also
        // makes the most important live-edit surface reachable without precise menu clicking.
        if ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::E)
        }) {
            let was_closed = !self.eff_editor.open;
            self.eff_editor.open = true;
            if was_closed {
                if let Some(path) = self.current_eff_path.clone() {
                    self.eff_editor.queue_load(&path);
                }
            }
        }

        // Background "Fetch ACMD" result
        self.poll_acmd_fetch();

        // Handle pending model load — needs wgpu device/queue
        let t_model = self.perf.start();
        if let Some(model_dir) = self.pending_model_load.take() {
            if let Some(wgpu_state) = frame.wgpu_render_state() {
                let device = &wgpu_state.device;
                let queue = &wgpu_state.queue;

                // Only initialize 3D rendering if the device has the required features.
                if device.features().contains(ssbh_wgpu::REQUIRED_FEATURES) {
                    let mut renderer = wgpu_state.renderer.write();

                    // Initialize render state if not yet done
                    if renderer
                        .callback_resources
                        .get::<HitboxRenderState>()
                        .is_none()
                    {
                        let rs = HitboxRenderState::new(device, queue, wgpu_state.target_format);
                        renderer.callback_resources.insert(rs);
                    }

                    if let Some(rs) = renderer.callback_resources.get_mut::<HitboxRenderState>() {
                        rs.load_model(device, queue, &model_dir);
                        // Eagerly load skeleton and animation for projected circle overlays.
                        let skel_path = self.current_skel_path.as_deref();
                        if let Some(path) = skel_path {
                            rs.load_skeleton(path);
                        }
                        if let Some(anim_path) = &self.current_anim_path {
                            rs.apply_animation(
                                queue,
                                Some(anim_path.as_path()),
                                skel_path,
                                animation_frame_for_game_frame(self.state.current_frame),
                            );
                        }
                        let weapon_count = rs.weapon_skel_count();
                        if weapon_count > 0 {
                            self.state.status = format!(
                                "Model loaded ({} weapon skeleton{})",
                                weapon_count,
                                if weapon_count == 1 { "" } else { "s" }
                            );
                        }
                        // bone_names already populated from skel file in select_fighter — don't overwrite
                    }
                } else {
                    self.state.status = "GPU lacks required features for 3D rendering (missing BC texture compression or similar).".to_string();
                }
            }
        }

        self.perf.end("model_load", t_model);

        // Ensure viewport GPU state exists even before a model is selected.
        if let Some(wgpu_state) = frame.wgpu_render_state() {
            let mut renderer = wgpu_state.renderer.write();
            if renderer
                .callback_resources
                .get::<HitboxRenderState>()
                .is_none()
            {
                let rs = HitboxRenderState::new(
                    &wgpu_state.device,
                    &wgpu_state.queue,
                    wgpu_state.target_format,
                );
                renderer.callback_resources.insert(rs);
            }
        }

        // Advance playback
        if self.state.playing {
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(self.last_frame_time).as_secs_f32();
            if elapsed >= 1.0 / 24.0 {
                if self.state.total_frames > 0 {
                    self.state.current_frame =
                        if self.state.current_frame >= self.state.total_frames {
                            FIRST_GAME_FRAME
                        } else {
                            self.state.current_frame + 1
                        };
                } else {
                    // Keep the timeline moving even when only live capture data is loaded.
                    self.state.current_frame = (self.state.current_frame + 1).min(9999);
                }
                self.last_frame_time = now;
            }
            // Keep character animation and timeline overlays moving during playback.
            let next = std::time::Duration::from_secs_f32((1.0 / 24.0 - elapsed).max(0.0));
            ctx.request_repaint_after(next);
        }

        // Edit log window
        if self.show_edit_log {
            let t = self.perf.start();
            self.draw_edit_log_window(&ctx);
            self.perf.end("edit_log_window", t);
        }

        if self.show_acmd_src {
            let t = self.perf.start();
            self.draw_acmd_source_window(&ctx);
            self.perf.end("acmd_source_window", t);
        }

        self.credits.show(&ctx);

        // Top menu bar: File / Windows / Mod + status
        let t_menu = self.perf.start();
        egui::Panel::top("menu").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Visionary").size(16.0).color(egui::Color32::WHITE));
                ui.separator();

                ui.menu_button("File", |ui| {
                    if ui.button("Open Data Root…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.set_data_root(path);
                        }
                        ui.close();
                    }
                    // ── Modded characters + extra skins ──────────────────────────────
                    if ui
                        .button("Add Mod Root…")
                        .on_hover_text(
                            "Point at a mod folder that CONTAINS fighter/<name>/ (an \
                             Arcropolis-style mod). Added-character mods appear in the fighter \
                             list; slot-add mods extend an existing fighter's skin list.",
                        )
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Add mod root (folder containing fighter/…)")
                            .pick_folder()
                        {
                            self.add_mod_root(path);
                        }
                        ui.close();
                    }
                    if !self.extra_roots.is_empty() {
                        let roots = self.extra_roots.clone();
                        ui.menu_button(format!("Mod Roots ({})", roots.len()), |ui| {
                            let mut drop: Option<PathBuf> = None;
                            for p in &roots {
                                ui.horizontal(|ui| {
                                    if ui
                                        .small_button("✖")
                                        .on_hover_text("Remove this mod root and re-index")
                                        .clicked()
                                    {
                                        drop = Some(p.clone());
                                    }
                                    ui.label(
                                        egui::RichText::new(p.to_string_lossy().to_string())
                                            .small(),
                                    );
                                });
                            }
                            ui.separator();
                            if ui
                                .button("Rescan skins")
                                .on_hover_text(
                                    "Re-read every fighter's costume slots from disk (after \
                                     installing a new skin)",
                                )
                                .clicked()
                            {
                                self.rescan_costume_slots();
                                self.state.status = "Rescanned costume slots.".into();
                                ui.close();
                            }
                            if let Some(p) = drop {
                                self.remove_mod_root(&p);
                                ui.close();
                            }
                        });
                    }
                    ui.separator();
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
                        .checkbox(&mut self.eff_editor.open, "Eff Editor  Ctrl+Shift+E")
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
                    ui.checkbox(&mut self.show_transplant, "Transplant Effects")
                        .on_hover_text(
                            "Transplant any effect from another EFF into the current fighter's \
                             EFF and redirect its uses",
                        );
                    let has_log = !self.state.edit_log.is_empty() || self.show_edit_log;
                    ui.add_enabled_ui(has_log, |ui| {
                        ui.checkbox(&mut self.show_edit_log, "Edit Log")
                            .on_hover_text("View and manage all saved edits");
                    });
                    ui.checkbox(&mut self.show_acmd_src, "ACMD Source")
                        .on_hover_text(
                            "Read and edit ACMD scripts from your own smashline project, \
                             instead of the online archive of vanilla scripts",
                        );
                    ui.checkbox(&mut self.show_debug, "Debug");
                    ui.checkbox(&mut self.credits.open, "Credits")
                        .on_hover_text("The people who helped make Visionary possible");
                });

                ui.menu_button("Mod", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("name:");
                        ui.add(egui::TextEdit::singleline(&mut self.project_name).desired_width(140.0));
                    });
                    ui.separator();
                    if ui.button("Export Project…")
                        .on_hover_text("Export an editable modproject.json and its assets separately from installable mod and developer files")
                        .clicked()
                    {
                        self.export_project();
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
                    // Was "Deploy live eff to Eden", which rebuilt the FIGHTER's own eff and
                    // asked the plugin to reparse it. That path is a dead end: the reparse
                    // rebuilds from the resident buffer and never re-requests the file, so the
                    // edited bytes were never read (`cb_game=0`) — and it could hang the game.
                    // Both routes now go through the live carrier.
                    if ui.button("Send edits to game")
                        .on_hover_text("Rebuild the live carrier with this fighter's transplants and authored edits baked in, and hand it to the running game. Re-trigger the move to see it on a fresh spawn.")
                        .clicked()
                    {
                        self.sync_eff_mods_from_editor();
                        self.push_effect_aliases();
                        ui.close();
                    }
                    if ui.button("Clear all game edits")
                        .on_hover_text("Wipe the game's saved pins (which survive restarts) + all live spawn/hitbox rules — use when old edits keep re-appearing")
                        .clicked()
                    {
                        self.clear_all_game_edits();
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.export_build.is_none(),
                            egui::Button::new("Export Mod Folder…"),
                        )
                        .on_hover_text("Export one complete ARCropolis mod folder, including effects and a built ACMD plugin at plugin.nro")
                        .clicked()
                    {
                        self.export_full_mod();
                        ui.close();
                    }
                    if ui.button("Export Developer Files…")
                        .on_hover_text("Export rebuilt effect files and editable ACMD Rust source in separate folders")
                        .clicked()
                    {
                        self.export_developer_files();
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.acmd_src.is_some(),
                            egui::Button::new("Sync Edits Into Source"),
                        )
                        .on_hover_text(
                            "Write this move's edited hitbox and spawn values back into your \
                             own linked ACMD project, leaving the macros you called and your \
                             formatting untouched. Link a project in Windows → ACMD Source.",
                        )
                        .clicked()
                    {
                        self.sync_edits_to_source();
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
        self.perf.end("menu_bar", t_menu);

        // Bottom timeline
        let t = self.perf.start();
        let max_scrubber_height = (ui.available_height() * 0.55).max(100.0);
        egui::Panel::bottom("scrubber")
            .default_size(180.0)
            .min_size(72.0)
            .max_size(max_scrubber_height)
            .resizable(true)
            .show_inside(ui, |ui| {
                ui.add_space(4.0);
                self.draw_scrubber(ui);
            });
        self.perf.end("scrubber", t);

        // Eff editor (separate OS viewport; authored .eff editing + live game preview)
        let current_fighter = self
            .state
            .selected_fighter
            .and_then(|i| self.state.fighters.get(i))
            .map(|f| f.name.clone());
        self.eff_editor.set_target_fighter(current_fighter.clone());
        // Edits-first selector: hand the editor the project's edit sources. Base + alt
        // (data-root) paths BOTH redirect to the merged build, so transplanted entries
        // show no matter which root a load request came through.
        {
            let root = self.eff_editor.export_root().to_path_buf();
            let data_root = self.state.data_root.clone();
            // Self-heal: a slotted fighter without its merged build on disk (fresh
            // session, deleted preview) gets one rebuild attempt per session; failures
            // stay visible as the ⚠ chip instead of retrying every frame.
            let need_build: Vec<String> = self
                .eff_mods
                .iter()
                .filter(|(f, e)| {
                    !e.transplants.is_empty()
                        && !self.merged_build_failed.contains(*f)
                        && !root
                            .join(&e.source_rel)
                            .parent()
                            .map(|d| {
                                d.join(crate::scratch_dirs::TRANSPLANT_PREVIEW_FILE)
                                    .exists()
                            })
                            .unwrap_or(false)
                })
                .map(|(f, _)| f.clone())
                .collect();
            for f in need_build {
                if self.build_merged_preview(&f).is_none() {
                    self.merged_build_failed.insert(f);
                }
            }
            let sources: Vec<crate::eff_editor::EditSource> = self
                .eff_mods
                .iter()
                .filter(|(_, e)| !e.is_empty())
                .map(|(f, e)| {
                    let base = root.join(&e.source_rel);
                    let alt_base = data_root
                        .as_ref()
                        .map(|r| {
                            r.join("effect")
                                .join("fighter")
                                .join(f)
                                .join(format!("ef_{f}.eff"))
                        })
                        .filter(|p| *p != base);
                    let merged = if e.transplants.is_empty() {
                        None
                    } else {
                        base.parent()
                            .map(|d| d.join(crate::scratch_dirs::TRANSPLANT_PREVIEW_FILE))
                            .filter(|p| p.exists())
                    };
                    crate::eff_editor::EditSource {
                        fighter: f.clone(),
                        base,
                        alt_base,
                        merged,
                        transplant_count: e.transplants.len(),
                        authored: e.authored.len(),
                        textures: e.textures.len(),
                        transplants: e
                            .transplants
                            .iter()
                            .enumerate()
                            .map(|(op_index, op)| crate::eff_editor::EffTransplant {
                                op_index,
                                entry_name: op
                                    .replace_entry
                                    .clone()
                                    .unwrap_or_else(|| op.new_entry_name.clone()),
                                donor_name: op.src_set_name.clone(),
                                donor_file: if op.src_file_rel.is_empty() {
                                    e.source_rel.clone()
                                } else {
                                    op.src_file_rel.clone()
                                },
                                one_slot_slots: op.one_slot_slots.clone(),
                            })
                            .collect(),
                    }
                })
                .collect();
            self.eff_editor.set_edit_sources(sources);
            // Which textures are already replaced, so the panel can say so and offer to
            // restore them. Scoped to the loaded eff by its `source_rel`, not to the selected
            // fighter — the editor can be showing a file the main window is not.
            let loaded_rel = self.eff_editor.loaded_rel().unwrap_or_default();
            let imports = self
                .eff_mods
                .values()
                .find(|e| e.source_rel.eq_ignore_ascii_case(&loaded_rel))
                .map(|e| e.textures.clone())
                .unwrap_or_default();
            self.eff_editor.set_texture_imports(imports);
        }
        let t = self.perf.start();
        self.eff_editor.show(&ctx, &self.game_link);
        self.perf.end("eff_editor", t);
        for removal in self.eff_editor.take_transplant_removals() {
            self.remove_transplant_from_editor(removal);
        }
        // Authored eff edits reaching the game: rebuild + hot-reload (see below). The editor
        // debounces the request; this side just services it.
        if self.eff_editor.take_live_deploy_request() {
            let note = self.apply_authored_eff_live();
            self.eff_editor.set_sent_note(note);
            // The carrier snapshot is built and pushed by `flush_effect_aliases` below, in
            // this same frame, so the send is complete once that has run.
            self.eff_editor.sending = false;
            // The bytes are away; the game has not necessarily taken them yet. Record which
            // carrier generation is live RIGHT NOW — the send is complete when the game reports
            // a newer one, not merely when it reports "ready", which the previous carrier
            // already does.
            self.eff_editor.gen_at_send = self.game_link.carrier_gen();
            self.eff_editor.carrier_reports_at_send = self.game_link.carrier_status().2;
            // Discard any rejection left over from an earlier send, so it cannot immediately
            // abort this one.
            let _ = self.game_link.take_carrier_error();
            // Start the stall clock fresh. Between sends the carrier sits in its live state for
            // as long as the user is editing, so a timer left running from the PREVIOUS send is
            // already past the threshold — and `live` is false on the first frame of a new send
            // (the game has not taken our bytes yet), so the stall check fired instantly and
            // reported "the previous carrier is not releasing" after 0.0s.
            self.eff_editor.carrier_state_since = None;
            self.eff_editor.awaiting_game = Some(std::time::Instant::now());
        } else if self.eff_editor.sending {
            // Deferred by one frame so the spinner paints before the (synchronous) rebuild
            // blocks the UI thread. Keep the frames coming so it actually animates.
            ctx.request_repaint();
        }

        // Resolve "waiting for game": the plugin reports carrier readiness as it changes.
        if let Some(since) = self.eff_editor.awaiting_game {
            let (state, kinds, reports, spawned) = self.game_link.carrier_status();
            self.eff_editor.carrier_report = (state, kinds, spawned);
            self.eff_editor.carrier_gen_now = self.game_link.carrier_gen();
            // Wait for the OBJECT, not just the staged state — the spinner used to clear
            // while the carrier still could not spawn anything.
            // State 2 is the carrier's terminal "live" state, and `spawned` means its battle
            // object actually resolves — which is the same check `spawn_via_carrier` makes.
            // Both are required: state alone used to clear the spinner while nothing could
            // spawn yet.
            let generation = self.game_link.carrier_gen();
            let took_our_bytes = generation > self.eff_editor.gen_at_send;
            let fresh_report = reports > self.eff_editor.carrier_reports_at_send;
            let live = carrier_send_reached_goal(
                self.eff_editor.carrier_expected,
                state,
                kinds,
                spawned,
                took_our_bytes,
                fresh_report,
            );
            // No deadline on a swap that is making progress. The game stays playable for the
            // whole teardown, so giving up bought nothing — it only abandoned a swap that was
            // still going to land, and the give-up path itself is the disruptive one.
            //
            // The silence bail stays: if the plugin has reported NOTHING, no swap is in flight
            // and waiting forever would hang the indicator on a match that is not running (or
            // an .nro too old to report). Reports are frame-driven, so at 30 fps under load the
            // first beat can be several seconds out — hence 12s, not 3.
            let silent = reports == 0 && since.elapsed().as_secs() >= 12;
            // Reporting but not MOVING is a third outcome, and it used to wait forever.
            //
            // The no-deadline rule above is right about a swap in progress, but "in progress" is
            // not the same as "reporting". A swap that is working walks through its states; a
            // wedged one repeats a single state indefinitely. Both were indistinguishable here,
            // so two different dead ends waited forever: state 0 (bytes sent, plugin alive, swap
            // never started) and a teardown that stops advancing (the editor sat on "retiring the
            // previous carrier"). Timing the CURRENT state rather than the whole wait separates
            // them from a slow-but-advancing swap, which resets the clock every transition.
            //
            // Clearing only stops the indicator — it tears nothing down — so this cannot abandon
            // a swap that would still have landed; it just stops claiming one is coming.
            //
            // 25s per state is well past the 12s silence bail and past any single transition
            // observed on a loaded match: a false "stuck" would be worse than a slow spinner.
            let state_since = match self.eff_editor.carrier_state_since {
                Some((seen, at)) if seen == state => at,
                _ => {
                    let now = std::time::Instant::now();
                    self.eff_editor.carrier_state_since = Some((state, now));
                    now
                }
            };
            // BOTH clocks must pass. `carrier_state_since` is reset when a send is armed, so the
            // per-state timer alone would do — but gating on THIS wait's age too means no stale
            // timer, from any path, can report a stall before the user has actually waited.
            let stalled = reports > 0
                && !live
                && state_since.elapsed().as_secs() >= 25
                && since.elapsed().as_secs() >= 25;
            // The plugin refused the push — it could not read the payload we pointed it at.
            // Nothing is coming, so stop waiting now instead of sitting out the silence
            // timeout and blaming the match for it.
            let rejected = self.game_link.take_carrier_error();
            if live || silent || stalled || rejected.is_some() {
                self.eff_editor.awaiting_game = None;
                // Keep the final reading visible. The indicator vanished too fast to read,
                // which made the one signal that could settle this undiagnosable.
                let why = if live {
                    ""
                } else if rejected.is_some() {
                    " (the game could not read the payload)"
                } else if stalled && state == 0 {
                    // Bytes went out, plugin is alive, but no swap ever began. Name the two
                    // things that actually produce this — neither is visible from the editor.
                    " (plugin is reporting but never staged the carrier — is the carrier item \
                     held, and is the .nro current?)"
                } else if stalled {
                    // Mid-swap and no longer advancing. The usual cause is the previous carrier
                    // refusing to release, which a reload of the match clears.
                    " (the swap stopped advancing in this state — the previous carrier is not \
                     releasing; reload the match)"
                } else {
                    // Reports are frame-driven, so silence means the per-frame driver never
                    // ran — no match in progress, or an .nro too old to report at all.
                    " (no reports — is a match running?)"
                };
                self.eff_editor.last_carrier_result = Some(format!(
                    "carrier: state={state} kinds={kinds} object={} gen={generation} after \
                     {:.1}s{why}",
                    if spawned { "up" } else { "down" },
                    since.elapsed().as_secs_f32(),
                ));
                self.eff_editor.carrier_ok = live;
                if live {
                    if self.eff_editor.carrier_expected {
                        self.eff_editor.set_sent_note(
                            "carrier live in game — re-trigger the move".to_string(),
                        );
                    } else {
                        self.eff_editor.set_sent_note(
                            "carrier cleared in game — original effects restored".to_string(),
                        );
                    }
                } else if let Some(reason) = rejected {
                    self.eff_editor.set_sent_note(format!(
                        "the game could not read the payload ({reason}) — check that Visionary \
                         is writing to your emulator's SD folder, or set VISIONARY_SD_DIR"
                    ));
                } else if silent {
                    self.eff_editor.set_sent_note(
                        "sent, but the game never reported back — start a match and send again"
                            .to_string(),
                    );
                } else {
                    self.eff_editor.set_sent_note(
                        "sent, but the game has not brought the carrier up after 30s".to_string(),
                    );
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }

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

        // Delayed serving-chain probe after a live-eff deploy.
        if let Some(due) = self.live_eff_probe_due {
            if std::time::Instant::now() >= due {
                self.live_eff_probe_due = None;
                self.game_link.send_live_eff_probe();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }

        // Param labels arriving from the background download (cache first, then GitHub).
        if let Some(rx) = self.param_labels_rx.take() {
            let mut keep = true;
            loop {
                match rx.try_recv() {
                    Ok(crate::param_labels::Msg::Loaded { labels, .. }) => {
                        // Downloaded labels win over anything read from the export folder.
                        for (h, l) in &labels {
                            self.state.labels.insert(*h, l.clone());
                        }
                        self.downloaded_labels = labels;
                    }
                    Ok(crate::param_labels::Msg::Status(s)) => self.state.status = s,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        keep = false;
                        break;
                    }
                }
            }
            if keep {
                self.param_labels_rx = Some(rx);
            }
        }

        // Auto-adopt live capture when the current move has NO data yet (no GitHub fetch).
        //
        // Adoption is DEFERRED until the move has finished: the plugin streams a move's ACMD
        // calls as they execute, so loading on the first line captured only the opening frames
        // and later hitboxes were missing (the "click live fetch again" annoyance).
        self.note_new_captures();
        self.settle_pending_capture(&ctx);

        // Pin-sync check: on a new plugin connection, wait for the resync notifies to land,
        // then prompt about in-game pins this session doesn't know about ("ask on connect").
        let client = self.game_link.client_id();
        if client != self.pin_sync_client {
            self.pin_sync_client = client;
            self.pin_sync_prompt = None;
            self.pin_sync_wait = client.map(|_| std::time::Instant::now());
            if client.is_some() {
                // Fresh plugin connection (game restarted): its RAM-held state is gone —
                // re-push the live spawn/hitbox rules and transplant aliases we hold.
                let spawn: Vec<crate::game_link::SpawnRuleWire> = self
                    .effect_rules_store
                    .values()
                    .flatten()
                    .cloned()
                    .collect();
                if !spawn.is_empty() {
                    self.game_link.send_spawn_rules(&spawn);
                }
                let hit: Vec<crate::game_link::HitboxRuleWire> = self
                    .hitbox_rules_store
                    .values()
                    .flatten()
                    .cloned()
                    .collect();
                if !hit.is_empty() {
                    self.game_link.send_hitbox_rules(&hit);
                }
                self.push_effect_aliases();
            }
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
        self.draw_pin_sync_modal(&ctx);
        let t = self.perf.start();
        self.draw_transplant_studio(&ctx);
        self.perf.end("transplant_window", t);
        let t = self.perf.start();
        self.poll_use_scan();
        self.perf.end("poll_use_scan", t);
        self.draw_redirect_prompt(&ctx);

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
        // Drain texture replacements picked in the eff editor into the project store. An
        // empty `png_path` is the editor's "restore the original" signal — the entry is
        // dropped rather than recorded, so a restored texture leaves no trace in the project.
        let texture_imports = self.eff_editor.take_texture_imports();
        if !texture_imports.is_empty() {
            if let Some(fighter) = current_fighter.clone() {
                let entry = self.eff_mods.entry(fighter.clone()).or_default();
                if entry.source_rel.is_empty() {
                    entry.source_rel = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
                }
                for import in texture_imports {
                    entry
                        .textures
                        .retain(|t| t.texture_name != import.texture_name);
                    self.state.status = if import.png_path.is_empty() {
                        format!("Texture '{}' restored for {fighter}", import.texture_name)
                    } else {
                        let msg =
                            format!("Texture '{}' replaced for {fighter}", import.texture_name);
                        entry.textures.push(import);
                        msg
                    };
                }
                // Like a transplant, this previews in-app and sends nothing until asked.
                self.eff_editor.mark_unsent();
            } else {
                self.state.status =
                    "Select a fighter before replacing a texture — the replacement has to be \
                     recorded against one."
                        .to_string();
            }
        }

        // Drain pool-shape changes the same way. These are ordered additions-then-removals to
        // match how the build applies them, and both are recorded against the loaded eff's
        // fighter, not the texture — a pool texture belongs to the file, not to one effect.
        let texture_additions = self.eff_editor.take_texture_additions();
        let texture_removals = self.eff_editor.take_texture_removals();
        if !texture_additions.is_empty() || !texture_removals.is_empty() {
            if let Some(fighter) = current_fighter.clone() {
                let entry = self.eff_mods.entry(fighter.clone()).or_default();
                if entry.source_rel.is_empty() {
                    entry.source_rel = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
                }
                for add in texture_additions {
                    self.state.status =
                        format!("Texture '{}' added for {fighter}", add.texture_name);
                    entry
                        .textures_added
                        .retain(|t| t.texture_name != add.texture_name);
                    entry.textures_added.push(add);
                }
                for name in texture_removals {
                    // A texture the user ADDED and then deleted leaves nothing behind: dropping
                    // the addition is the whole undo. Recording a removal for it as well would
                    // ask the build to delete a texture the eff never had.
                    let was_ours = entry.textures_added.iter().any(|t| t.texture_name == name);
                    entry.textures_added.retain(|t| t.texture_name != name);
                    entry.textures.retain(|t| t.texture_name != name);
                    if !was_ours && !entry.textures_removed.contains(&name) {
                        entry.textures_removed.push(name.clone());
                    }
                    self.state.status = format!("Texture '{name}' removed for {fighter}");
                }
                // Like a transplant, this previews in-app and sends nothing until asked.
                self.eff_editor.mark_unsent();
            } else {
                self.state.status =
                    "Select a fighter before changing the texture pool — the change has to be \
                     recorded against one."
                        .to_string();
            }
        }

        // Drain transplant ops recorded in the eff editor into the project store.
        let ops = self.eff_editor.take_transplants();
        let mut editor_transplant_fighter = None;
        if !ops.is_empty() {
            if let Some(fighter) = current_fighter {
                let entry = self.eff_mods.entry(fighter.clone()).or_default();
                if entry.source_rel.is_empty() {
                    entry.source_rel = format!("effect/fighter/{fighter}/ef_{fighter}.eff");
                }
                for op in ops {
                    self.state.status = if op.one_slot_slots.len() == 1 {
                        format!(
                            "One-slot transplant '{}' recorded for {fighter} c{:02}",
                            op.new_entry_name, op.one_slot_slots[0]
                        )
                    } else {
                        format!(
                            "EFF transplant '{}' recorded for {fighter}",
                            op.new_entry_name
                        )
                    };
                    entry.transplants.push(op);
                }
                editor_transplant_fighter = Some(fighter);
            }
        }
        if let Some(fighter) = editor_transplant_fighter {
            // The EFF editor can create transplants directly, bypassing Transplant Studio. Like
            // that path, this one previews in-app and sends nothing: the plugin keeps the
            // carrier it was last given until Send. That divergence used to be silent (Daisy in
            // the merged preview, Bomberman still in game), which is what the unsent marker is
            // for — the answer is to say so, not to rebuild the carrier behind the user.
            self.eff_editor.mark_unsent();
            self.preview_transplant_result(&fighter);
        }

        // Effects panel (right side, shown when toggled)
        if self.state.show_effects_panel {
            let t = self.perf.start();
            egui::Panel::right("effects_panel")
                .min_size(220.0)
                .show_inside(ui, |ui| {
                    self.draw_effects_panel(ui);
                });
            self.perf.end("effects_panel", t);
        } else {
            // Keep the auto-generated ids of every panel below identical whether or not this
            // panel is shown. Skipping a child `Ui` shifts the id of everything drawn after it,
            // and egui reacts by outlining all the "moved" widgets in red for a frame or two —
            // that is the red flash seen when the effects panel is toggled. egui's own
            // `Panel::show_animated_inside` does exactly this for the same reason.
            ui.skip_ahead_auto_ids(1);
        }

        // Left panel
        let t = self.perf.start();
        egui::Panel::left("left_panel")
            .min_size(200.0)
            .show_inside(ui, |ui| {
                self.draw_left_panel(ui);
            });
        self.perf.end("left_panel", t);

        // Right panel
        let t = self.perf.start();
        egui::Panel::right("right_panel")
            .min_size(240.0)
            .show_inside(ui, |ui| {
                self.draw_right_panel(ui);
            });
        self.perf.end("right_panel", t);

        // Commit any edits made this frame to the log
        let t = self.perf.start();
        self.commit_current_edits();
        self.perf.end("commit_edits", t);

        // Central viewport
        let t_central = self.perf.start();
        egui::CentralPanel::default().show_inside(ui, |ui| {
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

                // Paint the animated character. Circle overlays are drawn below by egui.
                let animation_frame = animation_frame_for_game_frame(self.state.current_frame);
                let callback = egui_wgpu::Callback::new_paint_callback(
                    rect,
                    ViewportCallback {
                        width: w,
                        height: h,
                        animation_frame,
                        anim_path: self.current_anim_path.clone(),
                        skel_path: self.current_skel_path.clone(),
                    },
                );
                ui.painter().add(callback);

                // Draw hitbox spheres as projected 2D circles
                let frame_num = self.state.current_frame;
                if let Some(wgpu_state) = frame.wgpu_render_state() {
                    let renderer = wgpu_state.renderer.read();
                    if let Some(rs) = renderer.callback_resources.get::<HitboxRenderState>() {
                        // The WGPU model callback runs after egui builds these overlays, so
                        // `last_frame` is one paint behind. Evaluate the requested frame
                        // directly to keep moving bones/root motion and grabboxes synchronized.
                        let t = self.perf.start();
                        let bone_matrices = rs.bone_world_matrices_at(animation_frame);
                        self.perf.end("bone_matrices", t);
                        // Keep a positions map for debug display
                        let bone_positions: std::collections::HashMap<String, glam::Vec3> =
                            bone_matrices
                                .iter()
                                .map(|(k, m)| (k.clone(), m.col(3).truncate()))
                                .collect();

                        if self.show_debug {
                            let mut names: Vec<&String> = bone_positions.keys().collect();
                            names.sort();
                            for (i, name) in names.iter().take(30).enumerate() {
                                ui.painter().text(
                                    rect.left_top() + egui::vec2(4.0, 4.0 + i as f32 * 12.0),
                                    egui::Align2::LEFT_TOP,
                                    name.as_str(),
                                    egui::FontId::monospace(9.0),
                                    egui::Color32::YELLOW,
                                );
                            }
                            for (i, hb) in self.state.hitboxes.iter().enumerate().take(5) {
                                let found = bone_matrices.contains_key(&hb.bone_name)
                                    || bone_matrices.contains_key(&hb.bone_name.to_lowercase());
                                ui.painter().text(
                                    rect.right_top() + egui::vec2(-220.0, 4.0 + i as f32 * 12.0),
                                    egui::Align2::LEFT_TOP,
                                    format!("{:?} found:{}", hb.bone_name, found),
                                    egui::FontId::monospace(9.0),
                                    egui::Color32::LIGHT_BLUE,
                                );
                            }
                            for (name, pos) in &bone_positions {
                                if let Some(sp) = rs.world_to_screen(*pos, rect) {
                                    ui.painter().circle_filled(
                                        sp,
                                        3.0,
                                        egui::Color32::from_rgba_unmultiplied(0, 255, 0, 150),
                                    );
                                    ui.painter().text(
                                        sp + egui::vec2(4.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        name,
                                        egui::FontId::monospace(8.0),
                                        egui::Color32::from_rgb(0, 220, 0),
                                    );
                                }
                            }
                        }

                        for hb in &self.state.hitboxes {
                            let active = hb.active_frames_empty()
                                || (frame_num >= hb.active_start && frame_num <= hb.active_end);
                            if !active {
                                continue;
                            }

                            let color = hitbox_display_color(hb);
                            let stroke = egui::Stroke::new(2.0, color);
                            let fill = egui::Color32::from_rgba_unmultiplied(
                                color.r(),
                                color.g(),
                                color.b(),
                                40,
                            );

                            // Wind areas are defined in the fighter's 2D object space. Render
                            // their real circle/AABB footprint rather than reusing the 3D ATTACK
                            // sphere placeholder.
                            if let Some(wind) = hb
                                .wind
                                .as_ref()
                                .filter(|wind| hb.category == 2 && wind.is_valid())
                            {
                                let root = bone_matrices
                                    .get("top")
                                    .or_else(|| bone_matrices.get("Top"))
                                    .copied()
                                    .unwrap_or(glam::Mat4::IDENTITY);
                                let root = glam::Mat4::from_translation(root.col(3).truncate());
                                let [x, y] = wind.offset();
                                let center = wind_area_world_point(root, x, y);

                                if wind.is_radial() {
                                    if let Some(screen_center) = rs.world_to_screen(center, rect) {
                                        let screen_radius = rs
                                            .world_radius_to_screen(center, wind.radius(), rect)
                                            .unwrap_or(wind.radius() * 4.0)
                                            .max(4.0);
                                        ui.painter().circle(
                                            screen_center,
                                            screen_radius,
                                            fill,
                                            stroke,
                                        );
                                        ui.painter().line_segment(
                                            [
                                                screen_center - egui::vec2(screen_radius, 0.0),
                                                screen_center + egui::vec2(screen_radius, 0.0),
                                            ],
                                            egui::Stroke::new(1.0, color.gamma_multiply(0.65)),
                                        );
                                        ui.painter().line_segment(
                                            [
                                                screen_center - egui::vec2(0.0, screen_radius),
                                                screen_center + egui::vec2(0.0, screen_radius),
                                            ],
                                            egui::Stroke::new(1.0, color.gamma_multiply(0.65)),
                                        );
                                        ui.painter().text(
                                            screen_center + egui::vec2(screen_radius + 3.0, 0.0),
                                            egui::Align2::LEFT_CENTER,
                                            format!("WIND #{} RAD", wind.id()),
                                            egui::FontId::monospace(11.0),
                                            color,
                                        );
                                    }
                                } else {
                                    let [width, height] = wind.dimensions();
                                    let half = glam::Vec2::new(width * 0.5, height * 0.5);
                                    let corners = [
                                        glam::Vec2::new(-half.x, -half.y),
                                        glam::Vec2::new(half.x, -half.y),
                                        glam::Vec2::new(half.x, half.y),
                                        glam::Vec2::new(-half.x, half.y),
                                    ];
                                    let screen_corners: Option<Vec<egui::Pos2>> = corners
                                        .iter()
                                        .map(|corner| {
                                            let point = wind_area_world_point(
                                                root,
                                                x + corner.x,
                                                y + corner.y,
                                            );
                                            rs.world_to_screen(point, rect)
                                        })
                                        .collect();
                                    if let Some(screen_corners) = screen_corners {
                                        ui.painter().add(egui::Shape::convex_polygon(
                                            screen_corners,
                                            fill,
                                            stroke,
                                        ));
                                    }
                                    if let Some(screen_center) = rs.world_to_screen(center, rect) {
                                        let angle = wind.args[2].to_radians();
                                        let length = width.min(height).max(1.0) * 0.35;
                                        let arrow_end = wind_area_world_point(
                                            root,
                                            x + angle.cos() * length,
                                            y + angle.sin() * length,
                                        );
                                        if let Some(screen_end) =
                                            rs.world_to_screen(arrow_end, rect)
                                        {
                                            ui.painter().arrow(
                                                screen_center,
                                                screen_end - screen_center,
                                                stroke,
                                            );
                                        }
                                        ui.painter().text(
                                            screen_center + egui::vec2(4.0, -4.0),
                                            egui::Align2::LEFT_BOTTOM,
                                            format!("WIND #{} RECT", wind.id()),
                                            egui::FontId::monospace(11.0),
                                            color,
                                        );
                                    }
                                }
                                continue;
                            }

                            // Get bone world matrix — offsets are in bone local space.
                            // For system/root bones (top, Trans, Rot, throw) the offsets
                            // are effectively in world space, so we only use translation.
                            let bone_mat = bone_matrices
                                .get(&hb.bone_name)
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

                            let capsule_world_end = hb.capsule_end.map(|[ex, ey, ez]| {
                                bone_mat.transform_point3(glam::Vec3::new(ex, ey, ez))
                            });
                            let launch_center = capsule_world_end
                                .map(|world_end| (world_pos + world_end) * 0.5)
                                .unwrap_or(world_pos);

                            if let Some(world_end) = capsule_world_end {
                                let sp1 = rs.world_to_screen(world_pos, rect);
                                let sp2 = rs.world_to_screen(world_end, rect);
                                let r1 = rs
                                    .world_radius_to_screen(world_pos, hb.size, rect)
                                    .unwrap_or(hb.size * 4.0)
                                    .max(4.0);
                                let r2 = rs
                                    .world_radius_to_screen(world_end, hb.size, rect)
                                    .unwrap_or(hb.size * 4.0)
                                    .max(4.0);

                                if let (Some(p1), Some(p2)) = (sp1, sp2) {
                                    let dir = (p2 - p1).normalized();
                                    let perp = egui::vec2(-dir.y, dir.x);
                                    ui.painter()
                                        .line_segment([p1 + perp * r1, p2 + perp * r2], stroke);
                                    ui.painter()
                                        .line_segment([p1 - perp * r1, p2 - perp * r2], stroke);
                                    ui.painter().add(egui::Shape::convex_polygon(
                                        vec![
                                            p1 + perp * r1,
                                            p2 + perp * r2,
                                            p2 - perp * r2,
                                            p1 - perp * r1,
                                        ],
                                        fill,
                                        egui::Stroke::NONE,
                                    ));
                                    ui.painter().circle(p1, r1, fill, stroke);
                                    ui.painter().circle(p2, r2, fill, stroke);
                                    let label_pos = p1 + (p2 - p1) * 0.5;
                                    ui.painter().text(
                                        label_pos + egui::vec2(r1.max(r2) + 2.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        format!("#{} {:.0}", hb.id, hb.damage),
                                        egui::FontId::monospace(11.0),
                                        color,
                                    );
                                } else if let Some(p) = sp1.or(sp2) {
                                    let r = r1.max(r2);
                                    ui.painter().circle(p, r, fill, stroke);
                                }
                            } else {
                                if let Some(screen_pos) = rs.world_to_screen(world_pos, rect) {
                                    let screen_radius = rs
                                        .world_radius_to_screen(world_pos, hb.size, rect)
                                        .unwrap_or(hb.size * 4.0)
                                        .max(4.0);
                                    ui.painter().circle(screen_pos, screen_radius, fill, stroke);
                                    ui.painter().text(
                                        screen_pos + egui::vec2(screen_radius + 2.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        format!("#{} {:.0}", hb.id, hb.damage),
                                        egui::FontId::monospace(11.0),
                                        color,
                                    );
                                }
                            }

                            if hb.category == 0 {
                                let (direction, _) = hitbox_launch_direction(hb.angle);
                                let launch_end = launch_center + direction * hb.size;
                                if let (Some(screen_center), Some(screen_end)) = (
                                    rs.world_to_screen(launch_center, rect),
                                    rs.world_to_screen(launch_end, rect),
                                ) {
                                    ui.painter().line_segment(
                                        [screen_center, screen_end],
                                        egui::Stroke::new(2.0, lighten_hitbox_color(color)),
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
                            let active = frame_num >= ec.active_start && frame_num <= ec.active_end;
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
                            let world_pos = bone_mat.transform_point3(glam::Vec3::from(ec.offset));
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
                            ui.painter().circle(
                                screen_pos,
                                radius,
                                fill,
                                egui::Stroke::new(stroke_w, blue),
                            );
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
                    }
                }
            } else {
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::from_rgb(17, 17, 34));
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("Open a data root directory to begin").color(Color32::GRAY),
                    );
                });
            }
        });
        self.perf.end("central_viewport", t_central);
        if self.perf.enabled {
            // Profiling wants a continuous frame stream to average over; normal runs stay
            // reactive (this is exactly the "repaint on a timer" cost we do NOT want by default).
            ctx.request_repaint();
        }
        // Every handler for this frame has run, so the project state is final: publish at most
        // one donor/carrier snapshot rather than the transient sequence the handlers produce.
        self.flush_effect_aliases();
        // Same reason: the source window and the editor panels have both had their turn, so
        // a value changed in either one is reconciled against the other before the next frame.
        self.reconcile_source_buffer(&ctx);
        self.perf.end_frame(frame_t);
    }

    /// Persist any window geometry the 2-second debounce has not written yet.
    fn on_exit(&mut self) {
        self.flush_window_geometry();
    }
}

impl Hitbox {
    fn active_frames_empty(&self) -> bool {
        self.active_end == 9999
    }
}

fn find_nuanmb(motion_dir: &Path, label: &str, hash: u64) -> Option<PathBuf> {
    let p = motion_dir.join(format!("{}.nuanmb", label));
    if p.exists() {
        return Some(p);
    }

    let suffix = label.replace('_', "").to_lowercase();
    if let Ok(entries) = std::fs::read_dir(motion_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("nuanmb") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if stem.ends_with(&suffix) {
                return Some(path);
            }
        }
    }

    let p = motion_dir.join(format!("{:#018x}.nuanmb", hash));
    if p.exists() {
        return Some(p);
    }
    None
}

/// Rebuild an ACMD script from the complete edited collision list while retaining every
/// non-collision statement from the source script.
///
/// ATTACK ids and spawn frames are both editable, so neither can identify a source command after
/// an edit. Patching source commands by either value also cannot represent additions or removals.
/// Instead, flatten the finite source timeline, discard its old collision commands, and merge a
/// fresh collision schedule into the retained statements. Finite loops and relative waits become
/// absolute frame groups, preserving their execution order without tying an edit to its old slot.
fn rebuild_script_from_hitboxes(
    original: &crate::data::AcmdScript,
    hitboxes: &[crate::data::Hitbox],
) -> crate::data::AcmdScript {
    use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
    use std::collections::BTreeMap;

    /// Expand the source's finite control flow into timestamped non-collision statements.
    fn retain_non_collisions(
        stmts: &[AcmdStmt],
        start_frame: f32,
        out: &mut Vec<(f32, AcmdStmt)>,
    ) -> f32 {
        let mut frame = start_frame;
        for stmt in stmts {
            match stmt {
                AcmdStmt::Frame(value) => frame = *value,
                AcmdStmt::Wait(value) => frame += *value,
                AcmdStmt::Excute(inner) => {
                    let retained: Vec<_> = inner
                        .iter()
                        .filter(|stmt| matches!(stmt, ExcuteStmt::Raw(_)))
                        .cloned()
                        .collect();
                    if !retained.is_empty() {
                        out.push((frame, AcmdStmt::Excute(retained)));
                    }
                }
                AcmdStmt::Loop { count, body } => {
                    for _ in 0..*count {
                        frame = retain_non_collisions(body, frame, out);
                    }
                }
                AcmdStmt::WaitLoopClear | AcmdStmt::Raw(_) => {
                    out.push((frame, stmt.clone()));
                }
            }
        }
        frame
    }

    // End events run before spawns at the same frame. This matters when an id is reused: clear the
    // old collision first, then create its replacement.
    let mut ends: BTreeMap<u32, Vec<ExcuteStmt>> = BTreeMap::new();
    let mut spawns: BTreeMap<u32, Vec<ExcuteStmt>> = BTreeMap::new();
    for hitbox in hitboxes {
        let start = hitbox.active_start.max(FIRST_GAME_FRAME);
        let end = hitbox.active_end.max(start);
        match hitbox.category {
            0 => {
                spawns
                    .entry(start)
                    .or_default()
                    .push(ExcuteStmt::Attack(hitbox.to_attack_call()));
                if end < 9999 {
                    ends.entry(end.saturating_add(1))
                        .or_default()
                        .push(ExcuteStmt::Clear(hitbox.id));
                }
            }
            2 => {
                let Some(mut wind) = hitbox.wind.clone().filter(|wind| wind.is_valid()) else {
                    continue;
                };
                wind.args[0] = hitbox.id as f32;
                if wind.has_lifetime() {
                    let last = wind.args.len() - 1;
                    wind.args[last] = end.saturating_sub(start).saturating_add(1) as f32;
                }
                spawns
                    .entry(start)
                    .or_default()
                    .push(ExcuteStmt::Wind(wind));
                if end < 9999 {
                    ends.entry(end.saturating_add(1))
                        .or_default()
                        .push(ExcuteStmt::EraseWind(hitbox.id));
                }
            }
            1 => {
                spawns
                    .entry(start)
                    .or_default()
                    .push(ExcuteStmt::Catch(hitbox.to_catch_call()));
                // GrabModule, not AttackModule — an attack clear leaves a grab box open.
                if end < 9999 {
                    ends.entry(end.saturating_add(1))
                        .or_default()
                        .push(ExcuteStmt::GrabClearAll);
                }
            }
            _ => {}
        }
    }

    let mut timed = Vec::new();
    retain_non_collisions(&original.stmts, 0.0, &mut timed);
    let mut event_frames: Vec<u32> = ends.keys().chain(spawns.keys()).copied().collect();
    event_frames.sort_unstable();
    event_frames.dedup();
    for frame in event_frames {
        let mut events = ends.remove(&frame).unwrap_or_default();
        events.extend(spawns.remove(&frame).unwrap_or_default());
        if !events.is_empty() {
            timed.push((frame as f32, AcmdStmt::Excute(events)));
        }
    }
    // `sort_by` is stable, so retained source statements stay ahead of regenerated collisions at
    // the same frame. This preserves setup calls that originally preceded ATTACK in an excute.
    timed.sort_by(|(left, _), (right, _)| left.total_cmp(right));

    let mut stmts = Vec::new();
    let mut frame = 0.0_f32;
    for (at, stmt) in timed {
        if at > 0.0 && at != frame {
            stmts.push(AcmdStmt::Frame(at));
            frame = at;
        }
        stmts.push(stmt);
    }
    AcmdScript { stmts }
}

/// The editable half of the ACMD Source window: the open script, or why there is none.
///
/// A free function rather than a method because it needs `&mut` on the open buffer while the
/// window is already holding other parts of the app — and because it must render something
/// useful in all four states, not bail out of the window as it once did.
fn draw_source_pane(
    ui: &mut egui::Ui,
    buffer: Option<&mut SourceBuffer>,
    fighter: &str,
    move_name: &str,
    reason: NoSource,
    save: &mut bool,
    revert: &mut bool,
) {
    let Some(buffer) = buffer else {
        let message = match reason {
            NoSource::NotLinked => "No source project is linked, so this move has no code of \
                 its own to show. Link one above, or switch to Generated to see what an \
                 export would write."
                .to_string(),
            NoSource::FighterAbsent => format!(
                "The linked project has no scripts for {fighter} at all. If this move came \
                 from live capture, that is expected — switch to Generated to see what an \
                 export would write for it."
            ),
            NoSource::MoveAbsent => format!(
                "The linked project has {fighter}, but not {move_name}. Switch to Generated \
                 to see what an export would write for it."
            ),
            NoSource::NothingOpen => {
                "Open one of this move's scripts above to edit it.".to_string()
            }
        };
        ui.label(egui::RichText::new(message).color(egui::Color32::GRAY));
        return;
    };

    if buffer.move_name != move_name || buffer.fighter != fighter {
        ui.label(
            egui::RichText::new(format!(
                "Showing {} / {} — open a script for the selected move to edit it here.",
                buffer.fighter, buffer.move_name
            ))
            .small()
            .color(egui::Color32::from_rgb(230, 190, 90)),
        );
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(buffer.file.display().to_string())
                .small()
                .color(egui::Color32::GRAY),
        );
        if buffer.dirty() {
            ui.colored_label(egui::Color32::from_rgb(230, 190, 90), "● unsaved");
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(buffer.dirty(), egui::Button::new("Save"))
            .on_hover_text("Write this function back into the file it came from")
            .clicked()
        {
            *save = true;
        }
        if ui
            .add_enabled(buffer.dirty(), egui::Button::new("Revert"))
            .clicked()
        {
            *revert = true;
        }
        ui.label(
            egui::RichText::new("↕ live")
                .small()
                .color(egui::Color32::from_rgb(120, 190, 130)),
        )
        .on_hover_text(
            "Typing here updates the timeline, the viewport, and the game preview once the \
             text settles. Dragging a value in the editor panels writes it back into this \
             text. Nothing touches the file until you press Save.",
        );
    });
    if let Some(note) = &buffer.note {
        ui.label(
            egui::RichText::new(note.as_str())
                .small()
                .color(egui::Color32::from_rgb(230, 190, 90)),
        );
    }
    egui::ScrollArea::both()
        .id_salt("acmd_src_yours")
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut buffer.text)
                    .font(egui::TextStyle::Monospace)
                    .code_editor()
                    .desired_rows(24)
                    .desired_width(f32::INFINITY),
            );
        });
}

/// What the export verifier made of the code above.
///
/// The same check the export itself runs, shown live so a problem is visible while the move is
/// still open rather than as a failed export later. A clean result is stated explicitly: the
/// useful thing to know about a generated file is usually that it is fine.
fn draw_verification(ui: &mut egui::Ui, report: &crate::acmd_verify::Report, has_code: bool) {
    use crate::acmd_verify::Severity;

    if !has_code {
        return;
    }
    if report.is_clean() {
        ui.label(
            egui::RichText::new(
                "✔ Verified — this reads back as exactly the move on screen, and compiles.",
            )
            .small()
            .color(egui::Color32::from_rgb(120, 200, 120)),
        );
        return;
    }
    for finding in &report.findings {
        let (mark, color) = match finding.severity {
            Severity::Blocker => ("✖", egui::Color32::from_rgb(240, 110, 110)),
            Severity::Warning => ("⚠", egui::Color32::from_rgb(255, 170, 60)),
        };
        ui.label(
            egui::RichText::new(format!("{mark} {}", finding.message))
                .small()
                .color(color),
        );
    }
    if report.has_blockers() {
        ui.label(
            egui::RichText::new("An export will refuse to write this until it is resolved.")
                .small()
                .color(egui::Color32::GRAY),
        );
    }
}

/// The read-only half: what an export would write.
///
/// `interactive(false)` rather than a label, so the generated text stays selectable and
/// copyable — which is most of the point of showing it.
fn draw_generated_pane(ui: &mut egui::Ui, text: &mut String) {
    if text.is_empty() {
        ui.label(
            egui::RichText::new(
                "This move has no hitboxes or effect spawns loaded yet, so an export would \
                 write nothing for it. Fetch the move, or perform it in game with the plugin \
                 connected to capture it live.",
            )
            .color(egui::Color32::GRAY),
        );
        return;
    }
    egui::ScrollArea::both()
        .id_salt("acmd_src_generated")
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(text)
                    .font(egui::TextStyle::Monospace)
                    .code_editor()
                    .interactive(false)
                    .desired_rows(24)
                    .desired_width(f32::INFINITY),
            );
        });
}

/// One live color/speed override as the export records it.
///
/// `None` when both multipliers are identity: that is the absence of an edit, and shipping it
/// would emit LAST_EFFECT_SET_COLOR/RATE calls that do nothing. Shared by the export and by
/// the generated-source preview so the preview cannot promise code the export will not write.
fn live_tweak_from_override(
    form: &crate::game_link::RpmEffectData,
) -> Option<crate::mod_project::LiveTweak> {
    let color = form.rainbow.color;
    let identity_color = (color.red - 1.0).abs() < 1e-4
        && (color.green - 1.0).abs() < 1e-4
        && (color.blue - 1.0).abs() < 1e-4;
    let identity_speed = (form.speed - 1.0).abs() < 1e-4;
    if identity_color && identity_speed {
        return None;
    }
    Some(crate::mod_project::LiveTweak {
        effect_name: form.effect_name.clone(),
        color: (!identity_color).then_some([color.red, color.green, color.blue, color.alpha]),
        speed: (!identity_speed).then_some(form.speed),
    })
}

/// Build a whole ACMD script from a collision list (capture-sourced moves have no base script
/// to patch). Wind areas retain their exact command family/payload and use `erase_wind`; ATTACK
/// entries retain the existing frame-grouped `clear_all` behavior.
pub fn synthesize_script_from_hitboxes(
    hitboxes: &[crate::data::Hitbox],
) -> crate::data::AcmdScript {
    use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
    use std::collections::BTreeMap;
    if hitboxes.is_empty() {
        return AcmdScript::default();
    }

    // End events are inserted before spawns at the same frame so reusing a wind id does not
    // immediately erase its replacement.
    let mut ends: BTreeMap<u32, Vec<ExcuteStmt>> = BTreeMap::new();
    let mut spawns: BTreeMap<u32, Vec<ExcuteStmt>> = BTreeMap::new();
    let mut attack_clear_at: Option<u32> = None;
    let mut grab_clear_at: Option<u32> = None;
    for hb in hitboxes {
        if hb.category == 2 {
            let Some(wind) = hb.wind.clone().filter(|wind| wind.is_valid()) else {
                continue;
            };
            spawns
                .entry(hb.active_start)
                .or_default()
                .push(ExcuteStmt::Wind(wind));
            ends.entry(hb.active_end.saturating_add(1))
                .or_default()
                .push(ExcuteStmt::EraseWind(hb.id));
        } else if hb.category == 1 {
            // A grab is a CATCH call. Sending it through `to_attack_call` exported a
            // zero-damage ATTACK where the user had a grab box, so the move stopped
            // grabbing — the same mistake the wind path above exists to avoid.
            spawns
                .entry(hb.active_start)
                .or_default()
                .push(ExcuteStmt::Catch(hb.to_catch_call()));
            // A grab the capture never saw cleared runs to the end of the timeline, the
            // same reading `collision_end_injection` gives 9999 on the live path.
            if hb.active_end < 9999 {
                grab_clear_at = Some(
                    grab_clear_at
                        .unwrap_or(0)
                        .max(hb.active_end.saturating_add(1)),
                );
            }
        } else {
            spawns
                .entry(hb.active_start)
                .or_default()
                .push(ExcuteStmt::Attack(hb.to_attack_call()));
            attack_clear_at = Some(
                attack_clear_at
                    .unwrap_or(0)
                    .max(hb.active_end.saturating_add(1)),
            );
        }
    }
    if let Some(frame) = grab_clear_at {
        ends.entry(frame)
            .or_default()
            .insert(0, ExcuteStmt::GrabClearAll);
    }
    if let Some(frame) = attack_clear_at {
        ends.entry(frame)
            .or_default()
            .insert(0, ExcuteStmt::ClearAll);
    }

    let mut frames: Vec<u32> = ends.keys().chain(spawns.keys()).copied().collect();
    frames.sort_unstable();
    frames.dedup();
    let mut stmts = Vec::new();
    for frame in frames {
        let mut events = ends.remove(&frame).unwrap_or_default();
        events.extend(spawns.remove(&frame).unwrap_or_default());
        if !events.is_empty() {
            stmts.push(AcmdStmt::Frame(frame as f32));
            stmts.push(AcmdStmt::Excute(events));
        }
    }
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
    } else if n.starts_with("attack_s3")
        || n.starts_with("attack_hi3")
        || n.starts_with("attack_lw3")
    {
        2 // tilts
    } else if n.starts_with("attack_s4")
        || n.starts_with("attack_hi4")
        || n.starts_with("attack_lw4")
    {
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
        } else {
            name
        }
    } else {
        name
    };

    stripped
        .replace('_', " ")
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

/// One known use of a transplanted donor effect, offered for redirect to the new name.
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

/// Background fighter-wide ACMD scan (redirect full-use discovery).
/// A live ACMD capture for the open move that is still arriving.
///
/// The plugin streams capture lines as the move executes, so adopting on the FIRST line
/// yields a script truncated at whatever frame the move had reached. This defers adoption
/// until the move has actually finished.
struct PendingCapture {
    motion: u64,
    /// Fighter kind the capture is filtered to (None = unknown fighter).
    kind: Option<i32>,
    /// Exact claimed playback. Only its matching end marker can complete this snapshot.
    run: u32,
    /// Lines held for this motion at the last observed growth.
    lines: usize,
    first_line: std::time::Instant,
    last_line: std::time::Instant,
}

/// Quiet period after the last capture line before a move counts as finished. This is only
/// the FALLBACK for plugin builds that predate `AcmdCaptureEnd` — with a current plugin the
/// end marker resolves the wait immediately. It has to clear the largest realistic gap
/// between two ACMD lines of one move (a multi-hit or charge move can leave ~1 s between
/// hits at 60 fps) on top of the plugin's ~6 Hz batching, hence the generous value.
const CAPTURE_QUIET: std::time::Duration = std::time::Duration::from_millis(1200);
/// Hard ceiling on the wait, so a motion that never reports an end still resolves.
const CAPTURE_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

struct UseScan {
    rx: std::sync::mpsc::Receiver<UseScanMsg>,
    fighter: String,
    done: usize,
    total: usize,
}

enum UseScanMsg {
    /// Total number of move scripts the fighter has (sent first).
    Total(usize),
    /// One move scanned: its parsed effect calls (empty vec = no EFFECT lines).
    Move(String, Vec<crate::data::EffectCall>),
}

/// "AttackAirN" → "attack_air_n" (inverse of `move_name_to_pascal`; digits stay attached).
fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// List every move-script filename (Pascal case, no extension) the dump repo has for
/// `fighter`, via the GitHub contents API; disk-cached forever alongside the script cache.
fn fetch_move_index(fighter: &str) -> anyhow::Result<Vec<String>> {
    let dir = crate::scratch_dirs::app_storage_root()
        .join("script-cache")
        .join(fighter);
    let cache = dir.join("_index.json");
    let body = match std::fs::read_to_string(&cache) {
        Ok(b) => b,
        Err(_) => {
            let url = format!(
                "https://api.github.com/repos/WuBoytH/SSBU-Dumped-Scripts/contents/smashline/lua2cpp_{fighter}/{fighter}"
            );
            let b = crate::acmd::http_client()
                .get(&url)
                .header("User-Agent", "visionary")
                .send()?
                .text()?;
            // Only cache real listings (the API returns an object on error).
            if b.trim_start().starts_with('[') {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&cache, &b);
            }
            b
        }
    };
    let entries: Vec<serde_json::Value> = serde_json::from_str(&body)?;
    Ok(entries
        .iter()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()))
        .filter_map(|n| n.strip_suffix(".txt"))
        .map(|n| n.to_string())
        .collect())
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

/// Pick a fresh export directory without deleting or mixing with an older package. Re-exporting
/// to the same parent therefore cannot leave stale EFF or plugin files in the new result.
fn unused_export_root(parent: &std::path::Path, base_name: &str) -> std::path::PathBuf {
    let first = parent.join(base_name);
    if !first.exists() {
        return first;
    }
    for suffix in 2..=9999 {
        let candidate = parent.join(format!("{base_name}_{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{base_name}_{}", std::process::id()))
}

// ── Persistent config ─────────────────────────────────────────────────────────

fn config_path(key: &str) -> Option<std::path::PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("visionary").join(key))
}

fn legacy_config_path(key: &str) -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|base| base.join("ssbu_hitbox_editor").join(key))
}

/// Config key for the eff editor's dump root. Separate from `data_root` because that window
/// may point at a different folder than the main one (a mod dump, an older extract), and the
/// pick has to survive a restart — it used to fall back to the process cwd, i.e. whatever
/// folder the app happened to be launched from.
pub(crate) const EFF_ROOT_CONFIG_KEY: &str = "eff_root";

/// Config key for the emulator's SD root — the folder holding `ultimate/`, which is where live
/// edits are staged for the plugin to read. Left unset, `scratch_dirs::emulator_sd_root` probes
/// the standard Eden / yuzu / Ryujinx locations for the host platform; set it when the emulator
/// is a portable install that lives somewhere else entirely.
pub(crate) const SD_ROOT_CONFIG_KEY: &str = "sd_root";

/// Config key for the user's own ACMD source project — the smashline crate that builds their
/// plugin. When set, its scripts are read in preference to the dumped-script mirror.
pub(crate) const ACMD_SRC_CONFIG_KEY: &str = "acmd_src_root";

pub(crate) fn save_config_path(key: &str, path: &std::path::Path) {
    if let Some(dest) = config_path(key) {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&dest, path.to_string_lossy().as_bytes());
    }
}

fn clear_config_path(key: &str) {
    if let Some(dest) = config_path(key) {
        let _ = std::fs::remove_file(&dest);
    }
    if let Some(dest) = legacy_config_path(key) {
        let _ = std::fs::remove_file(&dest);
    }
}

// ── Window geometry ───────────────────────────────────────────────────────────

/// Outer position and inner size of a window, in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Keep at least this much of a window on screen, so it can always be grabbed and dragged
/// back even if it was last closed on a monitor that is no longer connected.
const MIN_VISIBLE_PX: f32 = 80.0;
/// Never restore a window smaller than this — a saved 0×0 (or a garbage config line) would
/// otherwise come back as an invisible window.
const MIN_WINDOW_SIZE: f32 = 240.0;

impl WindowGeometry {
    /// Parse an `x,y,w,h` line. Rejects non-numeric and non-finite values so a corrupt config
    /// falls back to defaults instead of producing a NaN-positioned window.
    fn parse(s: &str) -> Option<Self> {
        let mut it = s.split(',');
        let mut next = || -> Option<f32> {
            let v: f32 = it.next()?.trim().parse().ok()?;
            v.is_finite().then_some(v)
        };
        let (x, y, w, h) = (next()?, next()?, next()?, next()?);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        Some(Self { x, y, w, h })
    }

    fn encode(&self) -> String {
        format!("{:.0},{:.0},{:.0},{:.0}", self.x, self.y, self.w, self.h)
    }

    /// Live geometry of the viewport currently being drawn, if the backend reports it.
    fn from_viewport(ctx: &egui::Context) -> Option<Self> {
        ctx.input(|i| {
            let info = i.viewport();
            // Skip while maximised/fullscreen/minimised: the reported rect is the temporary
            // state, and restoring into it would lose the user's real window layout.
            if info.maximized == Some(true)
                || info.fullscreen == Some(true)
                || info.minimized == Some(true)
            {
                return None;
            }
            let outer = info.outer_rect?;
            let size = info.inner_rect.map(|r| r.size()).unwrap_or(outer.size());
            Some(Self {
                x: outer.min.x,
                y: outer.min.y,
                w: size.x,
                h: size.y,
            })
        })
    }

    /// Clamp onto a screen of the given size so the window is both usable and reachable.
    ///
    /// A saved position can easily be off-screen by the next launch — an unplugged second
    /// monitor, a resolution change, or a laptop docked differently — and a window restored
    /// out there is invisible and unrecoverable without editing the config by hand.
    pub fn clamped_to_screen(self, screen_w: f32, screen_h: f32) -> Self {
        // Unknown/degenerate screen size: leave the geometry alone rather than snapping it
        // to a bogus origin.
        if !(screen_w > 1.0 && screen_h > 1.0) {
            return self;
        }
        let w = self.w.clamp(MIN_WINDOW_SIZE.min(screen_w), screen_w);
        let h = self.h.clamp(MIN_WINDOW_SIZE.min(screen_h), screen_h);
        // Horizontally the window may hang off either edge, as long as a grabbable strip
        // remains. Vertically the title bar must stay reachable, so never go above y = 0.
        let x = self
            .x
            .clamp(MIN_VISIBLE_PX - w, (screen_w - MIN_VISIBLE_PX).max(0.0));
        let y = self.y.clamp(0.0, (screen_h - MIN_VISIBLE_PX).max(0.0));
        Self { x, y, w, h }
    }
}

/// Window geometry persists as `name=x,y,w,h` lines in one `window_geometry` file (same
/// config dir as the single-path keys). One file covers every window, so adding a window
/// later does not mean adding another config key.
fn save_window_geometry(all: &std::collections::BTreeMap<String, WindowGeometry>) {
    if let Some(dest) = config_path("window_geometry") {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = all
            .iter()
            .map(|(k, g)| format!("{k}={}", g.encode()))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(&dest, body);
    }
}

fn load_window_geometry() -> std::collections::BTreeMap<String, WindowGeometry> {
    let Some(dest) = config_path("window_geometry") else {
        return Default::default();
    };
    let Ok(body) = std::fs::read_to_string(&dest) else {
        return Default::default();
    };
    body.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), WindowGeometry::parse(value)?))
        })
        .collect()
}

/// Mod roots persist as one absolute path per line (same config dir as the single-path keys;
/// a list needs its own format because `save_config_path` stores exactly one path).
fn save_mod_roots(roots: &[std::path::PathBuf]) {
    if let Some(dest) = config_path("mod_roots") {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = roots
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(&dest, body);
    }
}

/// Saved mod roots, dropping any that no longer exist (an unplugged SD card should not
/// leave a permanently broken entry in the list).
fn load_mod_roots() -> Vec<std::path::PathBuf> {
    let Some(dest) = config_path("mod_roots") else {
        return Vec::new();
    };
    let Ok(body) = std::fs::read_to_string(&dest) else {
        return Vec::new();
    };
    body.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .collect()
}

fn load_config_path(key: &str) -> Option<std::path::PathBuf> {
    let dest = config_path(key)?;
    let dest = if dest.exists() {
        dest
    } else {
        legacy_config_path(key)?
    };
    let s = std::fs::read_to_string(&dest).ok()?;
    let p = std::path::PathBuf::from(s.trim());
    if p.exists() {
        if dest
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("ssbu_hitbox_editor")
        {
            save_config_path(key, &p);
        }
        Some(p)
    } else {
        None
    }
}

/// Recently opened external eff files (most-recent first), one path per line.
fn load_recent_effs() -> Vec<PathBuf> {
    let Some(primary) = config_path("recent_effs") else {
        return Vec::new();
    };
    let dest = if primary.exists() {
        primary
    } else if let Some(legacy) = legacy_config_path("recent_effs") {
        legacy
    } else {
        return Vec::new();
    };
    let Ok(s) = std::fs::read_to_string(&dest) else {
        return Vec::new();
    };
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

#[cfg(test)]
mod carrier_send_state_tests {
    use super::carrier_send_reached_goal;

    #[test]
    fn populated_snapshot_requires_its_new_live_object() {
        assert!(carrier_send_reached_goal(true, 2, 5, true, true, true));
        assert!(!carrier_send_reached_goal(true, 2, 5, true, false, true));
        assert!(!carrier_send_reached_goal(true, 2, 5, false, true, true));
    }

    #[test]
    fn empty_snapshot_succeeds_on_a_fresh_idle_report() {
        // Clearing does not read a replacement EFF from disk, so its generation need not move.
        assert!(carrier_send_reached_goal(false, 0, 0, false, false, true));
        // A status that predates the send is not an acknowledgement.
        assert!(!carrier_send_reached_goal(false, 0, 0, false, false, false));
        // An old object or any lingering kind means teardown is not complete yet.
        assert!(!carrier_send_reached_goal(false, 0, 1, false, false, true));
        assert!(!carrier_send_reached_goal(false, 0, 0, true, false, true));
        assert!(!carrier_send_reached_goal(false, 2, 5, true, true, true));
    }
}

#[cfg(test)]
mod window_geometry_tests {
    use super::{WindowGeometry, MIN_VISIBLE_PX, MIN_WINDOW_SIZE};

    fn g(x: f32, y: f32, w: f32, h: f32) -> WindowGeometry {
        WindowGeometry { x, y, w, h }
    }

    #[test]
    fn round_trips_through_the_config_encoding() {
        let original = g(120.0, 64.0, 1400.0, 900.0);
        let decoded = WindowGeometry::parse(&original.encode()).expect("should parse");
        assert_eq!(decoded, original);
    }

    #[test]
    fn rejects_corrupt_and_degenerate_config_lines() {
        // A corrupt config must fall back to defaults, never to a NaN-positioned or
        // zero-sized (invisible) window.
        for bad in [
            "",
            "1,2,3",
            "a,b,c,d",
            "1,2,0,600",
            "1,2,800,0",
            "1,2,-800,600",
            "NaN,2,800,600",
            "1,inf,800,600",
        ] {
            assert!(
                WindowGeometry::parse(bad).is_none(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn on_screen_geometry_is_left_alone() {
        let on_screen = g(100.0, 80.0, 1200.0, 800.0);
        assert_eq!(on_screen.clamped_to_screen(1920.0, 1080.0), on_screen);
    }

    #[test]
    fn window_saved_on_a_now_missing_monitor_comes_back_reachable() {
        // Saved on a second monitor at x = 2600; that monitor is gone and the desktop is
        // now a single 1920x1080 screen. The window must not be restored out of reach.
        let clamped = g(2600.0, 40.0, 1400.0, 900.0).clamped_to_screen(1920.0, 1080.0);
        assert!(
            clamped.x <= 1920.0 - MIN_VISIBLE_PX,
            "left edge {} leaves nothing grabbable on a 1920px screen",
            clamped.x
        );
        assert!(clamped.x + clamped.w > 0.0, "window is off the left edge");
        assert!(clamped.y >= 0.0, "title bar is above the top of the screen");
    }

    #[test]
    fn negative_position_keeps_a_grabbable_strip_and_a_reachable_title_bar() {
        let clamped = g(-5000.0, -900.0, 800.0, 600.0).clamped_to_screen(1920.0, 1080.0);
        // Some of the window may hang off the left, but not all of it.
        assert!(
            clamped.x + clamped.w >= MIN_VISIBLE_PX,
            "nothing left on screen to grab: x={} w={}",
            clamped.x,
            clamped.w
        );
        // The title bar must never end up above the top edge.
        assert_eq!(clamped.y, 0.0);
    }

    #[test]
    fn oversized_window_is_shrunk_to_fit_the_screen() {
        let clamped = g(0.0, 0.0, 6000.0, 4000.0).clamped_to_screen(1920.0, 1080.0);
        assert_eq!((clamped.w, clamped.h), (1920.0, 1080.0));
    }

    #[test]
    fn tiny_saved_size_is_restored_to_something_usable() {
        let clamped = g(10.0, 10.0, 1.0, 1.0).clamped_to_screen(1920.0, 1080.0);
        assert_eq!((clamped.w, clamped.h), (MIN_WINDOW_SIZE, MIN_WINDOW_SIZE));
    }

    #[test]
    fn unknown_screen_size_leaves_geometry_untouched() {
        // Better to restore where the user left it than to snap to a guessed origin.
        let saved = g(2600.0, 40.0, 1400.0, 900.0);
        assert_eq!(saved.clamped_to_screen(0.0, 0.0), saved);
    }

    #[test]
    fn clamping_is_idempotent() {
        let once = g(2600.0, 2000.0, 1400.0, 900.0).clamped_to_screen(1920.0, 1080.0);
        assert_eq!(once.clamped_to_screen(1920.0, 1080.0), once);
    }

    #[test]
    fn multi_window_config_body_round_trips() {
        // Mirrors the on-disk `name=x,y,w,h` per line format.
        let body = "main=0,0,1400,900\ntransplant=1500,80,560,720\n\nbogus\nbad=1,2\n";
        let parsed: std::collections::BTreeMap<String, WindowGeometry> = body
            .lines()
            .filter_map(|line| {
                let (k, v) = line.split_once('=')?;
                Some((k.trim().to_string(), WindowGeometry::parse(v)?))
            })
            .collect();
        assert_eq!(
            parsed.len(),
            2,
            "malformed lines must be skipped, not fatal"
        );
        assert_eq!(parsed["main"], g(0.0, 0.0, 1400.0, 900.0));
        assert_eq!(parsed["transplant"], g(1500.0, 80.0, 560.0, 720.0));
    }
}

#[cfg(test)]
mod carrier_op_selection_tests {
    use super::{carrier_stored_name, rides_the_carrier};
    use crate::mod_project::TransplantOp;

    fn op(donor_rel: &str, donor_name: &str, new_name: &str) -> TransplantOp {
        TransplantOp {
            new_entry_name: new_name.to_string(),
            src_file_rel: donor_rel.to_string(),
            src_set_name: donor_name.to_string(),
            src_set_idx: 0,
            one_slot_slots: Vec::new(),
            replace_entry: None,
        }
    }

    /// Every transplant with a real donor rides the carrier, system effs included.
    ///
    /// Excluding a donor class is not a neutral act: a transplant names an entry no loaded eff
    /// has, so an excluded op leaves the carrier snapshot empty and the deploy falls through to
    /// the merged-fighter-eff path this design exists to avoid.
    #[test]
    fn every_real_donor_rides_the_carrier() {
        for rel in [
            "effect/fighter/kirby/ef_kirby.eff",
            "effect/assist/bomberman/ef_bomberman.eff",
            "effect/system/common/ef_common.eff",
            "effect/system/item/ef_item.eff",
            "EFFECT/SYSTEM/COMMON/EF_COMMON.EFF",
        ] {
            assert!(rides_the_carrier(rel), "'{rel}' must ride the carrier");
        }
        assert!(
            !rides_the_carrier(""),
            "the empty placeholder carries nothing"
        );
    }

    /// The carrier must never store a copy under a name the running game already answers to.
    ///
    /// Carrier entry names become live effect kinds. `ef_common` and `ef_item` are resident in
    /// EVERY match, so storing `sys_bomb_b` puts the carrier's copy on the same hash as the
    /// game's own: SYS_* spawns resolved to a carrier that was not up and stopped rendering, and
    /// the carrier could never retire because `retiring_carrier_kinds_present` kept finding
    /// ef_common's copy of the kind (`AUTO_CARRIER_AWAIT_KIND_UNREGISTER registered=1`, forever).
    #[test]
    fn a_resident_donors_copy_is_never_stored_under_its_own_name() {
        let own = Some("effect/fighter/kirby/");
        for (rel, donor, new_name) in [
            (
                "effect/system/common/ef_common.eff",
                "sys_bomb_b",
                "sys_bomb_b_tp",
            ),
            (
                "effect/system/item/ef_item.eff",
                "sys_item_hit",
                "sys_item_hit_tp",
            ),
            (
                "effect/fighter/kirby/ef_kirby.eff",
                "kirby_dash",
                "kirby_dash_tp",
            ),
        ] {
            let stored = carrier_stored_name(&op(rel, donor, new_name), own);
            assert_ne!(
                stored, donor,
                "'{donor}' from {rel} is resident in the match — storing the carrier copy under \
                 that name shadows the game's own kind and wedges the carrier retire"
            );
        }
    }

    /// A resident donor with no distinct clone name falls back to the reserved namespace rather
    /// than reusing the donor's, which is the collision this rule exists to prevent.
    #[test]
    fn a_resident_donor_without_a_clone_name_uses_the_reserved_namespace() {
        let stored = carrier_stored_name(
            &op(
                "effect/system/common/ef_common.eff",
                "sys_bomb_b",
                "sys_bomb_b",
            ),
            Some("effect/fighter/kirby/"),
        );
        assert!(
            stored.starts_with(crate::mod_project::EDIT_CLONE_PREFIX),
            "expected the reserved namespace, got '{stored}'"
        );
        assert_ne!(stored, "sys_bomb_b");
    }

    /// A donor that is NOT in the match keeps its own name — that is what the live remap expects,
    /// and there is no kind to collide with.
    #[test]
    fn a_foreign_donor_keeps_its_donor_name() {
        let stored = carrier_stored_name(
            &op(
                "effect/fighter/pickel/ef_pickel.eff",
                "pickel_tnt",
                "pickel_tnt_tp",
            ),
            Some("effect/fighter/kirby/"),
        );
        assert_eq!(stored, "pickel_tnt");
    }
}

#[cfg(test)]
mod live_effect_capture_tests {
    use super::*;
    use crate::game_link::{CaptureLine, LuaArgWire as A};

    fn spawn(func: &str, frame: f32, effect: u64) -> CaptureLine {
        let flip = effect_capture_layout(func).unwrap().0;
        let mut args = vec![A::Hash(effect)];
        if flip {
            args.push(A::Hash(effect));
        }
        args.extend([
            A::Hash(hash40::hash40("top").0),
            A::Num(1.0),
            A::Num(2.0),
            A::Num(3.0),
            A::Num(4.0),
            A::Num(5.0),
            A::Num(6.0),
            A::Num(0.75),
        ]);
        CaptureLine {
            kind: 6,
            motion: hash40::hash40("attack_air_n").0,
            frame,
            func: func.into(),
            args,
            run: 1,
        }
    }

    fn attack_capture(id: i64, frame: f32, bone: u64) -> CaptureLine {
        let mut args = vec![A::Int(id), A::Int(0), A::Hash(bone)];
        // damage, angle, kbg, fkb, bkb, size, x, y, z, then nil capsule/property slots.
        args.extend([
            A::Num(8.0),
            A::Int(361),
            A::Int(100),
            A::Int(0),
            A::Int(40),
            A::Num(4.0),
            A::Num(0.0),
            A::Num(8.0),
            A::Num(6.0),
        ]);
        args.extend(std::iter::repeat_n(A::Nil, 24));
        CaptureLine {
            kind: 6,
            motion: hash40::hash40("attack_air_n").0,
            frame,
            func: "ATTACK".into(),
            args,
            run: 1,
        }
    }

    fn clear_capture(frame: f32) -> CaptureLine {
        CaptureLine {
            kind: 6,
            motion: hash40::hash40("attack_air_n").0,
            frame,
            func: "ATTACK_CLEAR_ALL".into(),
            args: Vec::new(),
            run: 1,
        }
    }

    fn wind_capture(command: &str, frame: f32, args: &[f32]) -> CaptureLine {
        CaptureLine {
            kind: 6,
            motion: hash40::hash40("attack_air_n").0,
            frame,
            func: command.into(),
            args: args.iter().copied().map(A::Num).collect(),
            run: 1,
        }
    }

    fn erase_wind_capture(id: i64, frame: f32) -> CaptureLine {
        CaptureLine {
            kind: 6,
            motion: hash40::hash40("attack_air_n").0,
            frame,
            func: "AREA_WIND_ERASE".into(),
            args: vec![A::Int(id)],
            run: 1,
        }
    }

    #[test]
    fn live_effect_layout_accounts_for_dumped_spawn_families() {
        for func in [
            "EFFECT",
            "EFFECT_ALPHA",
            "EFFECT_ATTR",
            "EFFECT_FLIP_ALPHA",
            "EFFECT_FOLLOW",
            "EFFECT_FOLLOW_ALPHA",
            "EFFECT_FOLLOW_COLOR",
            "EFFECT_FOLLOW_NO_SCALE",
            "EFFECT_FOLLOW_NO_STOP",
            "EFFECT_FOLLOW_NO_STOP_FLIP",
            "EFFECT_FOLLOW_FLIP_RND",
            "EFFECT_FLW_POS",
            "EFFECT_FLW_POS_NO_STOP",
            "DOWN_EFFECT",
            "FOOT_EFFECT",
            "FOOT_EFFECT_FLIP",
            "LANDING_EFFECT",
            "LANDING_EFFECT_FLIP",
        ] {
            assert!(
                effect_capture_layout(func).is_some(),
                "{func} must be reconstructed as an effect spawn"
            );
        }
        assert!(effect_capture_layout("EFFECT_OFF_KIND").is_none());
        assert!(effect_capture_layout("LAST_EFFECT_SET_RATE").is_none());
        assert_eq!(fighter_kind_id("kirby"), Some(6));
        assert_eq!(fighter_kind_id("mario"), Some(0));
    }

    #[test]
    fn live_flip_effect_keeps_both_graphics_and_null_noops_are_hidden() {
        let null = hash40::hash40("null").0;
        let smoke = hash40::hash40("sys_dash_smoke").0;
        let mut flip = spawn("LANDING_EFFECT_FLIP", 4.0, null);
        flip.args[1] = A::Hash(smoke);
        let no_op = spawn("FOOT_EFFECT", 7.0, null);

        let bones = HashMap::from([(hash40::hash40("top").0, "top".into())]);
        let effects = HashMap::from([(null, "null".into()), (smoke, "sys_dash_smoke".into())]);
        let calls = VisionaryApp::effect_calls_from_captures(&[flip, no_op], &bones, &effects);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].effect_name, "null");
        assert_eq!(calls[0].effect_name_alt.as_deref(), Some("sys_dash_smoke"));
        assert_eq!(calls[0].spawn_func, "LANDING_EFFECT_FLIP");
        assert_eq!(
            effect_call_display_name(&calls[0]),
            "sys_dash_smoke (flip; other side none)"
        );
    }

    #[test]
    fn live_effect_stop_closes_every_open_instance_of_the_kind() {
        let effect = hash40::hash40("moon_explosion").0;
        let mut captures = vec![
            spawn("EFFECT_FOLLOW_NO_STOP", 10.0, effect),
            spawn("LANDING_EFFECT", 7.0, hash40::hash40("sys_down_smoke").0),
            spawn("EFFECT_FOLLOW_ALPHA", 5.0, effect),
        ];
        captures.push(CaptureLine {
            kind: 6,
            motion: hash40::hash40("attack_air_n").0,
            frame: 20.0,
            func: "EFFECT_OFF_KIND".into(),
            args: vec![A::Hash(effect), A::Bool(false), A::Bool(true)],
            run: 1,
        });

        let mut bones = HashMap::new();
        bones.insert(hash40::hash40("top").0, "top".into());
        let mut effects = HashMap::new();
        effects.insert(effect, "moon_explosion".into());
        effects.insert(hash40::hash40("sys_down_smoke").0, "sys_down_smoke".into());

        let calls = VisionaryApp::effect_calls_from_captures(&captures, &bones, &effects);
        assert_eq!(calls.len(), 3);
        let moon: Vec<_> = calls
            .iter()
            .filter(|call| call.effect_name == "moon_explosion")
            .collect();
        assert_eq!(moon.len(), 2);
        // Motion frame 20 is SCRIPT frame 21 — captures arrive in the plugin's 0-based motion
        // frames and the editor works in the script's 1-based ones. See
        // `VisionaryApp::motion_to_script_frame`.
        assert!(moon.iter().all(|call| call.active_end == 21));
        let landing = calls
            .iter()
            .find(|call| call.effect_name == "sys_down_smoke")
            .unwrap();
        assert_eq!(landing.active_start, 8);
        assert_eq!(landing.active_end, 8);
    }

    /// Captured frames are converted into the script's frame space, and rules pushed back to
    /// the plugin are converted out of it again. A round trip must land where it started, or a
    /// suppress/retime rule targets the wrong frame.
    #[test]
    fn capture_frames_round_trip_through_the_script_frame_space() {
        for motion in [0.0f32, 1.0, 4.999_998, 9.0, 9.6, 25.0] {
            let script = VisionaryApp::motion_to_script_frame(motion);
            let back = VisionaryApp::script_to_motion_frame(script);
            assert!(
                (back - motion.round()).abs() < f32::EPSILON,
                "motion {motion} → script {script} → {back}"
            );
            let (lo, hi) = VisionaryApp::rule_frame_window(script);
            let (lo, hi) = (lo.unwrap(), hi.unwrap());
            assert!(
                motion >= lo && motion <= hi,
                "rule window [{lo}, {hi}] must contain the motion frame {motion} that produced it"
            );
        }
        // The first script frame is 1, and it maps back to motion frame 0 — the whole reason
        // the two spaces differ. Nothing may go negative.
        assert_eq!(VisionaryApp::motion_to_script_frame(0.0), 1);
        assert_eq!(VisionaryApp::script_to_motion_frame(0), 0.0);
    }

    #[test]
    fn timeline_game_frames_map_to_the_matching_animation_pose() {
        assert_eq!(animation_frame_for_game_frame(1), 0.0);
        assert_eq!(animation_frame_for_game_frame(10), 9.0);

        assert_eq!(timeline_frame_start_fraction(1, 60), 0.0);
        assert_eq!(timeline_frame_start_fraction(10, 60), 9.0 / 60.0);
        assert_eq!(timeline_frame_end_fraction(15, 60), 15.0 / 60.0);

        assert_eq!(timeline_frame_at_fraction(0.0, 60), 1);
        assert_eq!(timeline_frame_at_fraction(9.0 / 60.0, 60), 10);
        assert_eq!(timeline_frame_at_fraction(1.0, 60), 60);
    }

    #[test]
    fn wind_coordinates_use_the_same_gameplay_plane_as_top_bone_offsets() {
        let root = glam::Mat4::from_translation(glam::Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(
            wind_area_world_point(root, 6.0, 7.0),
            glam::Vec3::new(2.0, 10.0, 10.0)
        );
    }

    #[test]
    fn launch_direction_uses_the_gameplay_plane_instead_of_bone_rotation() {
        let (forward, dynamic) = hitbox_launch_direction(0);
        assert_eq!(forward, glam::Vec3::Z);
        assert!(!dynamic);

        let (up, _) = hitbox_launch_direction(90);
        assert!((up - glam::Vec3::Y).length() < 0.0001);
        let (back, _) = hitbox_launch_direction(180);
        assert!((back + glam::Vec3::Z).length() < 0.0001);

        let twisted_bone = glam::Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2);
        let center = twisted_bone.transform_point3(glam::Vec3::new(1.0, 2.0, 3.0));
        let launch_end = center + forward * 10.0;
        assert_eq!(launch_end - center, glam::Vec3::Z * 10.0);
        assert!((twisted_bone.transform_vector3(forward) - forward).length() > 1.0);

        for special in [361, 363, 365, 366, 367, 368] {
            let (representative, dynamic) = hitbox_launch_direction(special);
            assert_eq!(representative, glam::Vec3::Z);
            assert!(dynamic);
        }

        let original = egui::Color32::from_rgb(80, 120, 200);
        let lighter = lighten_hitbox_color(original);
        assert!(lighter.r() > original.r());
        assert!(lighter.g() > original.g());
        assert!(lighter.b() > original.b());
        assert_eq!(lighter.a(), original.a());
    }

    #[test]
    fn github_and_live_fetch_produce_the_same_hitbox_window() {
        let bone = hash40::hash40("top").0;
        let source = crate::data::AcmdScript {
            stmts: vec![
                crate::data::AcmdStmt::Frame(10.0),
                crate::data::AcmdStmt::Excute(vec![crate::data::ExcuteStmt::Attack(
                    Hitbox::default().to_attack_call(),
                )]),
                crate::data::AcmdStmt::Frame(16.0),
                crate::data::AcmdStmt::Excute(vec![crate::data::ExcuteStmt::ClearAll]),
            ],
        };
        let github = source.to_hitboxes();
        let live = VisionaryApp::hitboxes_from_captures(
            &[attack_capture(0, 9.0, bone), clear_capture(15.0)],
            &HashMap::from([(bone, "top".to_string())]),
            &HashMap::new(),
        );

        assert_eq!(github.len(), 1);
        assert_eq!(live.len(), 1);
        assert_eq!(
            (github[0].active_start, github[0].active_end),
            (live[0].active_start, live[0].active_end)
        );
        assert_eq!((github[0].active_start, github[0].active_end), (10, 15));
        assert_eq!(animation_frame_for_game_frame(github[0].active_start), 9.0);
    }

    #[test]
    fn live_wind_capture_preserves_geometry_payload_and_erase_lifetime() {
        let raw = [3.0, 1.0, 80.0, 300.0, 0.8, 4.0, 12.0, 24.0, 16.0, 50.0];
        let boxes = VisionaryApp::hitboxes_from_captures(
            &[
                wind_capture("AREA_WIND_2ND_arg10", 12.0, &raw),
                // ATTACK_CLEAR_ALL must not end an AreaModule wind area.
                clear_capture(15.0),
                erase_wind_capture(3, 20.0),
            ],
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(boxes.len(), 1);
        let hitbox = &boxes[0];
        let wind = hitbox.wind.as_ref().unwrap();
        assert_eq!(wind.command, "AREA_WIND_2ND_arg10");
        assert_eq!(wind.args, raw);
        assert_eq!(wind.offset(), [4.0, 12.0]);
        assert_eq!(wind.dimensions(), [24.0, 16.0]);
        assert_eq!((hitbox.active_start, hitbox.active_end), (13, 20));

        let wire = VisionaryApp::build_wind_args(hitbox).unwrap();
        assert_eq!(wire, raw.into_iter().map(A::Num).collect::<Vec<_>>());

        // Capture-sourced moves have no original script to patch. Synthesis must use the wind
        // command and erase event, never silently turn the area into ATTACK/clear_all.
        let synthesized = synthesize_script_from_hitboxes(&boxes);
        assert_eq!(synthesized.to_hitboxes(), boxes);
        let exported = crate::acmd::export_acmd_source(&synthesized, "mario", "attack_air_n");
        assert!(exported.contains("macros::AREA_WIND_2ND_arg10"));
        assert!(exported.contains("AreaModule::erase_wind"));
    }

    /// A grab box is a `CATCH` call, not an `ATTACK`. Synthesis used to send every
    /// non-wind hitbox through `to_attack_call`, so exporting a captured grab wrote a
    /// zero-damage attack hitbox and the move stopped grabbing entirely.
    #[test]
    fn a_captured_grab_box_exports_as_a_catch_call() {
        let boxes = vec![crate::data::Hitbox {
            id: 0,
            bone_name: "Top".into(),
            size: 5.5,
            offset_x: 0.0,
            offset_y: 6.4,
            offset_z: 10.2,
            active_start: 8,
            active_end: 12,
            category: 1,
            ..Default::default()
        }];

        let script = synthesize_script_from_hitboxes(&boxes);
        let exported = crate::acmd::export_acmd_source(&script, "mario", "catch");
        assert!(exported.contains("macros::CATCH(agent, 0,"), "{exported}");
        // Never as an attack, and never cleared as one.
        assert!(!exported.contains("macros::ATTACK("), "{exported}");
        assert!(!exported.contains("AttackModule::clear_all"), "{exported}");
        assert!(exported.contains("GrabModule::clear_all"), "{exported}");
        // Joints hash lowercase, and a sphere spells its capsule slots `None` — CATCH takes
        // `Option<f32>` there, exactly as ATTACK does.
        assert!(
            exported.contains(r#"Hash40::new("top"), 5.5, 0.0, 6.4, 10.2, None, None, None"#),
            "{exported}"
        );
    }

    /// A grab the capture never saw cleared runs to the end of the timeline rather than
    /// emitting a clear at frame 10000.
    #[test]
    fn an_uncleared_grab_box_gets_no_clear() {
        let boxes = vec![crate::data::Hitbox {
            id: 0,
            bone_name: "top".into(),
            active_start: 8,
            active_end: 9999,
            category: 1,
            ..Default::default()
        }];
        let exported = crate::acmd::export_acmd_source(
            &synthesize_script_from_hitboxes(&boxes),
            "mario",
            "catch",
        );
        assert!(exported.contains("macros::CATCH("), "{exported}");
        assert!(!exported.contains("GrabModule::clear_all"), "{exported}");
    }

    /// A live-captured move exists only in memory — there is no script file for it anywhere,
    /// on GitHub or in anyone's project. That is precisely when seeing the code an export
    /// would write matters most, so the preview must not need a linked source project.
    ///
    /// The ACMD Source window used to return early when nothing was linked, so a captured
    /// move showed an empty window.
    #[test]
    fn a_captured_move_still_previews_the_code_an_export_would_write() {
        let bone = hash40::hash40("top").0;
        let bones = HashMap::from([(bone, "top".to_string())]);
        let boxes = VisionaryApp::hitboxes_from_captures(
            &[attack_capture(0, 9.0, bone), clear_capture(15.0)],
            &bones,
            &HashMap::new(),
        );
        assert!(!boxes.is_empty());

        // Exactly what `generated_source_for_move` does for a move whose `state.script` is
        // empty, which is every capture-sourced move.
        let script = synthesize_script_from_hitboxes(&boxes);
        let preview = crate::acmd::preview_game_fn(&script, "dash");
        assert!(
            preview.contains("unsafe extern \"C\" fn game_dash")
                && preview.contains("macros::ATTACK("),
            "a captured move must preview real code:\n{preview}"
        );
    }

    /// A hitbox lasts until it is cleared. This used to be a hardcoded two frames, which is
    /// what made live-fetched hitboxes end far earlier than the script says.
    #[test]
    fn captured_hitboxes_last_until_they_are_cleared() {
        let bone = hash40::hash40("top").0;
        let bones = HashMap::from([(bone, "top".to_string())]);
        let attrs = HashMap::new();

        // Out at motion 9 (script 10), cleared at motion 15 (script 16) → ends the frame
        // before, 15. The old code would have said 12.
        let boxes = VisionaryApp::hitboxes_from_captures(
            &[attack_capture(0, 9.0, bone), clear_capture(15.0)],
            &bones,
            &attrs,
        );
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].active_start, 10);
        assert_eq!(boxes[0].active_end, 15);

        // Re-using an id ends the previous hitbox, like the script path does.
        let boxes = VisionaryApp::hitboxes_from_captures(
            &[
                attack_capture(0, 9.0, bone),
                attack_capture(0, 14.0, bone),
                clear_capture(20.0),
            ],
            &bones,
            &attrs,
        );
        assert_eq!(boxes.len(), 2);
        assert_eq!((boxes[0].active_start, boxes[0].active_end), (10, 14));
        assert_eq!((boxes[1].active_start, boxes[1].active_end), (15, 20));

        // Never cleared — the same 9999 sentinel the script path uses, not a made-up length.
        let boxes =
            VisionaryApp::hitboxes_from_captures(&[attack_capture(0, 9.0, bone)], &bones, &attrs);
        assert_eq!(boxes[0].active_end, 9999);
    }

    /// Every control in the attack property editor must have a live-wire representation, and
    /// an untouched control must not be serialized at all. The latter is what preserves the
    /// source Lua slot's exact type/sentinel while a neighbouring field is edited.
    #[test]
    fn every_attack_property_has_a_sparse_live_override() {
        let pristine = Hitbox::default();
        let mut edited = pristine.clone();
        edited.part = 2;
        edited.bone_name = "handr".into();
        edited.damage = 17.5;
        edited.angle = 45;
        edited.kb_scaling = 123;
        edited.fkb = 4;
        edited.kb_base = 67;
        edited.size = 8.25;
        edited.offset_x = 1.0;
        edited.offset_y = 2.0;
        edited.offset_z = 3.0;
        edited.capsule_end = Some([4.0, 5.0, 6.0]);
        edited.hitlag_mult = 1.5;
        edited.sdi_mult = 0.5;
        edited.setoff_kind = "ATTACK_SETOFF_KIND_THRU".into();
        edited.lr_check = "ATTACK_LR_CHECK_B".into();
        edited.is_clang = true;
        edited.is_add_attack = 3;
        edited.hitbox_attr = 0.75;
        edited.ground_or_air = 2;
        edited.is_mtk = true;
        edited.is_shield_disable = true;
        edited.is_reflectable = true;
        edited.is_absorbable = true;
        edited.is_landing_attack = false;
        edited.situation_mask = "COLLISION_SITUATION_MASK_A".into();
        edited.category_mask = "COLLISION_CATEGORY_MASK_FIGHTER".into();
        edited.part_mask = "COLLISION_PART_MASK_BODY_HEAD".into();
        edited.no_finish_camera = true;
        edited.collision_attr = "collision_attr_fire".into();
        edited.sound_level = "ATTACK_SOUND_LEVEL_L".into();
        edited.sound_attr = "COLLISION_SOUND_ATTR_FIRE".into();
        edited.attack_region = "ATTACK_REGION_MAGIC".into();

        let ov = VisionaryApp::hitbox_overrides(&edited, &pristine);
        let json = serde_json::to_value(&ov).unwrap();
        for key in [
            "part",
            "bone",
            "damage",
            "angle",
            "kbg",
            "fkb",
            "bkb",
            "size",
            "x",
            "y",
            "z",
            "x2",
            "y2",
            "z2",
            "capsule",
            "hitlag",
            "sdi",
            "setoff",
            "lr_check",
            "clang",
            "add_attack",
            "hitbox_attr",
            "ground_or_air",
            "mtk",
            "shield_disable",
            "reflectable",
            "absorbable",
            "landing_attack",
            "situation_mask",
            "category_mask",
            "part_mask",
            "no_finish_camera",
            "collision_attr",
            "sound_level",
            "sound_attr",
            "attack_region",
        ] {
            assert!(json.get(key).is_some(), "edited field '{key}' was not sent");
        }

        let untouched =
            serde_json::to_value(VisionaryApp::hitbox_overrides(&pristine, &pristine)).unwrap();
        assert_eq!(untouched.as_object().unwrap().len(), 0);

        let mut capsule = pristine.clone();
        capsule.capsule_end = Some([1.0, 2.0, 3.0]);
        let removed = VisionaryApp::hitbox_overrides(&pristine, &capsule);
        assert_eq!(removed.capsule, Some(false));
        assert_eq!((removed.x2, removed.y2, removed.z2), (None, None, None));

        // The same complete property set must survive the source/export representation.
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let mut source_hb = pristine.clone();
        source_hb.active_start = 3;
        edited.active_start = 3;
        let source = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::Attack(source_hb.to_attack_call())]),
            ],
        };
        let round_trip = rebuild_script_from_hitboxes(&source, &[edited.clone()]).to_hitboxes();
        assert_eq!(round_trip, vec![edited]);
    }

    /// Captured flag slots occur as Bool, Int, or Num depending on the script. All three must
    /// show the same value in the editor or merely loading a move changes its attributes.
    #[test]
    fn live_capture_accepts_every_scalar_flag_representation() {
        let bone = hash40::hash40("top").0;
        let mut args = vec![A::Nil; 36];
        args[0] = A::Int(0);
        args[1] = A::Int(0);
        args[2] = A::Hash(bone);
        args[19] = A::Int(1);
        args[23] = A::Num(1.0);
        args[24] = A::Bool(true);
        args[25] = A::Int(0);
        args[26] = A::Num(1.0);
        args[27] = A::Bool(true);
        args[31] = A::Int(1);
        let hb = VisionaryApp::hitbox_from_capture(
            &args,
            0.0,
            &HashMap::from([(bone, "top".into())]),
            &HashMap::new(),
        )
        .unwrap();
        assert!(hb.is_clang);
        assert!(hb.is_mtk);
        assert!(hb.is_shield_disable);
        assert!(!hb.is_reflectable);
        assert!(hb.is_absorbable);
        assert!(hb.is_landing_attack);
        assert!(hb.no_finish_camera);
    }

    /// Reusing an ATTACK id at a later frame creates another hit, not another reference to one
    /// global record. Export must retain independent attributes for both phases.
    #[test]
    fn acmd_export_keeps_reused_hitbox_ids_independent() {
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let mut first = Hitbox {
            id: 0,
            active_start: 3,
            ..Default::default()
        };
        let mut second = first.clone();
        second.active_start = 8;
        let original = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::Attack(first.to_attack_call())]),
                AcmdStmt::Frame(8.0),
                AcmdStmt::Excute(vec![ExcuteStmt::Attack(second.to_attack_call())]),
            ],
        };
        first.damage = 4.0;
        second.damage = 19.0;
        second.angle = 80;
        let rebuilt = rebuild_script_from_hitboxes(&original, &[first, second]);
        let hitboxes = rebuilt.to_hitboxes();
        assert_eq!(hitboxes.len(), 2);
        assert_eq!((hitboxes[0].damage, hitboxes[0].angle), (4.0, 361));
        assert_eq!((hitboxes[1].damage, hitboxes[1].angle), (19.0, 80));
    }

    /// Renumbering a hitbox is an ordinary edit made from the ID drag box. The rebuild used to
    /// look the collision back up by its *new* id, miss, and silently emit the untouched original
    /// call — so an export carried none of the move's edits.
    #[test]
    fn acmd_export_survives_renumbering_a_hitbox() {
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let original_hb = Hitbox {
            id: 0,
            active_start: 3,
            bone_name: "footl".into(),
            damage: 9.0,
            ..Default::default()
        };
        let original = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::Attack(original_hb.to_attack_call())]),
            ],
        };
        let mut edited = original_hb.clone();
        edited.id = 3;
        edited.damage = 14.0;
        let hitboxes = rebuild_script_from_hitboxes(&original, &[edited]).to_hitboxes();
        assert_eq!(hitboxes.len(), 1);
        assert_eq!(hitboxes[0].id, 3);
        assert_eq!(hitboxes[0].damage, 14.0);
    }

    /// Retiming a collision from the Start Frame slider moved it away from the frame the rebuild
    /// keyed on, which dropped every other attribute edit with it.
    #[test]
    fn acmd_export_survives_retiming_a_hitbox() {
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let original_hb = Hitbox {
            id: 0,
            active_start: 3,
            damage: 9.0,
            ..Default::default()
        };
        let original = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::Attack(original_hb.to_attack_call())]),
            ],
        };
        let mut edited = original_hb.clone();
        edited.active_start = 7;
        edited.damage = 14.0;
        let hitboxes = rebuild_script_from_hitboxes(&original, &[edited]).to_hitboxes();
        assert_eq!(hitboxes.len(), 1);
        assert_eq!(hitboxes[0].active_start, 7);
        assert_eq!(hitboxes[0].damage, 14.0);
    }

    /// Two collisions spawned by one `excute` block must not be able to swap attributes just
    /// because the user gave them each other's ids.
    #[test]
    fn acmd_export_keeps_swapped_ids_attached_to_their_own_collision() {
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let first = Hitbox {
            id: 0,
            active_start: 3,
            damage: 9.0,
            ..Default::default()
        };
        let mut second = first.clone();
        second.id = 1;
        second.damage = 5.0;
        let original = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![
                    ExcuteStmt::Attack(first.to_attack_call()),
                    ExcuteStmt::Attack(second.to_attack_call()),
                ]),
            ],
        };
        let (mut a, mut b) = (first, second);
        a.id = 1;
        b.id = 0;
        let hitboxes = rebuild_script_from_hitboxes(&original, &[a, b]).to_hitboxes();
        assert_eq!(hitboxes.len(), 2);
        assert_eq!((hitboxes[0].id, hitboxes[0].damage), (1, 9.0));
        assert_eq!((hitboxes[1].id, hitboxes[1].damage), (0, 5.0));
    }

    #[test]
    fn acmd_export_removes_deleted_collisions_without_dropping_other_calls() {
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let first = Hitbox {
            id: 0,
            active_start: 3,
            active_end: 5,
            ..Default::default()
        };
        let second = Hitbox {
            id: 1,
            active_start: 3,
            active_end: 5,
            ..Default::default()
        };
        let original = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![
                    ExcuteStmt::Raw("WorkModule::on_flag(agent.module_accessor, 7);".into()),
                    ExcuteStmt::Attack(first.to_attack_call()),
                    ExcuteStmt::Attack(second.to_attack_call()),
                ]),
                AcmdStmt::Frame(6.0),
                AcmdStmt::Excute(vec![ExcuteStmt::ClearAll]),
            ],
        };

        let rebuilt = rebuild_script_from_hitboxes(&original, &[second]);
        let hitboxes = rebuilt.to_hitboxes();
        assert_eq!(hitboxes.len(), 1);
        assert_eq!(hitboxes[0].id, 1);
        assert!(rebuilt.stmts.iter().any(|stmt| matches!(
            stmt,
            AcmdStmt::Excute(inner)
                if inner.iter().any(|stmt| matches!(stmt, ExcuteStmt::Raw(line) if line.contains("WorkModule::on_flag")))
        )));
    }

    #[test]
    fn acmd_export_inserts_added_collisions_at_their_edited_frames() {
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let original_hitbox = Hitbox {
            id: 0,
            active_start: 3,
            active_end: 4,
            ..Default::default()
        };
        let original = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::Attack(original_hitbox.to_attack_call())]),
                AcmdStmt::Frame(5.0),
                AcmdStmt::Excute(vec![ExcuteStmt::ClearAll]),
            ],
        };
        let added = Hitbox {
            id: 3,
            active_start: 7,
            active_end: 9,
            damage: 18.0,
            ..Default::default()
        };

        let hitboxes =
            rebuild_script_from_hitboxes(&original, &[original_hitbox, added]).to_hitboxes();
        assert_eq!(hitboxes.len(), 2);
        assert_eq!(
            (
                hitboxes[1].id,
                hitboxes[1].active_start,
                hitboxes[1].active_end,
                hitboxes[1].damage,
            ),
            (3, 7, 9, 18.0)
        );
    }

    #[test]
    fn acmd_export_applies_edited_end_frames_per_collision_id() {
        use crate::data::{AcmdScript, AcmdStmt, ExcuteStmt};
        let original_hitbox = Hitbox {
            id: 0,
            active_start: 3,
            active_end: 5,
            ..Default::default()
        };
        let original = AcmdScript {
            stmts: vec![
                AcmdStmt::Frame(3.0),
                AcmdStmt::Excute(vec![ExcuteStmt::Attack(original_hitbox.to_attack_call())]),
                AcmdStmt::Frame(6.0),
                AcmdStmt::Excute(vec![ExcuteStmt::ClearAll]),
            ],
        };
        let mut edited = original_hitbox;
        edited.active_end = 12;

        let hitboxes = rebuild_script_from_hitboxes(&original, &[edited]).to_hitboxes();
        assert_eq!(hitboxes.len(), 1);
        assert_eq!((hitboxes[0].active_start, hitboxes[0].active_end), (3, 12));
    }

    /// A partial/old capture cannot safely seed all 36 ATTACK slots. It must be rejected rather
    /// than indexing past the captured vector while building an injected hitbox.
    #[test]
    fn attack_injection_requires_the_complete_argument_vector() {
        let donor = CaptureLine {
            kind: 6,
            motion: 1,
            frame: 1.0,
            func: "ATTACK".into(),
            args: vec![A::Nil; 35],
            run: 1,
        };
        assert!(VisionaryApp::build_attack_args(&Hitbox::default(), Some(&donor)).is_none());
    }

    #[test]
    fn a_new_grab_without_a_donor_builds_the_geometry_form() {
        let grab = Hitbox {
            id: 4,
            category: 1,
            bone_name: "top".into(),
            size: 6.5,
            offset_x: 1.0,
            offset_y: 8.0,
            offset_z: -2.0,
            capsule_end: Some([1.0, 3.0, -2.0]),
            active_start: 1,
            active_end: 5,
            ..Default::default()
        };
        let args = VisionaryApp::build_catch_args(&grab, None).unwrap();
        assert_eq!(args.len(), 9);
        assert_eq!(args[0], A::Int(4));
        assert_eq!(args[2], A::Num(6.5));
        assert_eq!(args[6], A::Num(1.0));
        assert_eq!(args[7], A::Num(3.0));
        assert_eq!(args[8], A::Num(-2.0));

        let end = VisionaryApp::collision_end_injection(&grab).unwrap();
        assert_eq!(end.command.as_deref(), Some("GRAB_CLEAR_ALL"));
        assert_eq!(end.frame, VisionaryApp::script_to_motion_frame(6));
        assert!(end.args.is_empty());
    }

    #[test]
    fn an_added_wind_box_gets_an_id_scoped_end_command() {
        let wind = Hitbox {
            id: 7,
            category: 2,
            active_start: 10,
            active_end: 14,
            ..Default::default()
        };
        let end = VisionaryApp::collision_end_injection(&wind).unwrap();
        assert_eq!(end.command.as_deref(), Some("AREA_WIND_ERASE"));
        assert_eq!(end.args, vec![A::Int(7)]);
        assert_eq!(end.frame, VisionaryApp::script_to_motion_frame(15));
    }

    #[test]
    fn an_existing_wind_edit_replaces_the_original_with_the_exact_payload() {
        let args = vec![3.0, 1.0, 80.0, 300.0, 0.8, 4.0, 12.0, 24.0, 16.0, 50.0];
        let original = Hitbox {
            id: 3,
            category: 2,
            active_start: 10,
            active_end: 14,
            wind: Some(crate::data::WindboxData {
                command: "AREA_WIND_2ND_arg10".into(),
                args: args.clone(),
            }),
            ..Default::default()
        };
        let mut edited = original.clone();
        edited.wind.as_mut().unwrap().args[1] = 2.5;

        let mut rules = Vec::new();
        assert!(VisionaryApp::append_wind_replacement_rules(
            &mut rules, 0x1234, &original, &edited
        ));
        assert_eq!(rules.len(), 3);
        assert!(rules[0].suppress);
        assert!(rules[0].overrides.is_none());
        let inject = rules[1].inject.as_ref().unwrap();
        assert_eq!(inject.command.as_deref(), Some("AREA_WIND_2ND_arg10"));
        assert_eq!(inject.args[1], A::Num(2.5));
        assert_eq!(
            rules[2].inject.as_ref().unwrap().command.as_deref(),
            Some("AREA_WIND_ERASE")
        );
    }

    #[test]
    fn dense_timeline_rows_scale_without_changing_frame_space() {
        assert_eq!(timeline_content_height(0, 0), 24.0);
        assert_eq!(timeline_content_height(40, 0), 504.0);
        assert_eq!(timeline_frame_at_fraction(0.0, 60), FIRST_GAME_FRAME);

        let dense_capture: Vec<Hitbox> = (0..40)
            .map(|i| Hitbox {
                id: i,
                active_start: 5 + i,
                active_end: 6 + i,
                ..Default::default()
            })
            .collect();
        assert_eq!(timeline_frame_extent(&dense_capture, &[]), 45);

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 180.0),
            )),
            ..Default::default()
        };
        let mut measured = None;
        let _ = context.run_ui(input, |ui| {
            let output = timeline_scroll_area(120.0).show(ui, |ui| {
                ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), timeline_content_height(40, 0)),
                    egui::Sense::hover(),
                );
            });
            measured = Some((output.inner_rect.height(), output.content_size.y));
        });
        let (viewport_height, content_height) = measured.unwrap();
        assert!(viewport_height <= 120.01);
        assert!(content_height >= 504.0);
    }

    #[test]
    fn hitbox_menu_scrollbar_uses_the_full_remaining_panel_height() {
        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(420.0, 500.0),
            )),
            ..Default::default()
        };
        let mut measured = None;
        let _ = context.run_ui(input, |ui| {
            let output = hitbox_menu_scroll_area(360.0).show(ui, |ui| {
                ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 900.0),
                    egui::Sense::hover(),
                );
            });
            measured = Some((output.inner_rect.height(), output.content_size.y));
        });
        let (viewport_height, content_height) = measured.unwrap();
        assert!((viewport_height - 360.0).abs() < 0.01);
        assert!(content_height >= 900.0);
    }

    /// Added and retimed attacks are replayed from a captured 36-slot donor. Even when that
    /// donor used Nil/Hash sentinels, every property represented by the editor must be written
    /// into the injected call rather than silently inheriting or dropping the edit.
    #[test]
    fn attack_injection_writes_every_property_over_sentinel_slots() {
        let donor = CaptureLine {
            kind: 6,
            motion: 1,
            frame: 1.0,
            func: "ATTACK".into(),
            args: vec![A::Nil; 36],
            run: 1,
        };
        let h = Hitbox {
            id: 7,
            part: 2,
            bone_name: "handr".into(),
            damage: 17.5,
            angle: 45,
            kb_scaling: 123,
            fkb: 4,
            kb_base: 67,
            size: 8.25,
            offset_x: 1.0,
            offset_y: 2.0,
            offset_z: 3.0,
            capsule_end: Some([4.0, 5.0, 6.0]),
            hitlag_mult: 1.5,
            sdi_mult: 0.5,
            setoff_kind: "ATTACK_SETOFF_KIND_THRU".into(),
            lr_check: "ATTACK_LR_CHECK_B".into(),
            is_clang: true,
            is_add_attack: 3,
            hitbox_attr: 0.75,
            ground_or_air: 2,
            is_mtk: true,
            is_shield_disable: true,
            is_reflectable: true,
            is_absorbable: true,
            is_landing_attack: false,
            situation_mask: "COLLISION_SITUATION_MASK_A".into(),
            category_mask: "COLLISION_CATEGORY_MASK_FIGHTER".into(),
            part_mask: "COLLISION_PART_MASK_BODY_HEAD".into(),
            no_finish_camera: true,
            collision_attr: "collision_attr_fire".into(),
            sound_level: "ATTACK_SOUND_LEVEL_L".into(),
            sound_attr: "COLLISION_SOUND_ATTR_FIRE".into(),
            attack_region: "ATTACK_REGION_MAGIC".into(),
            ..Default::default()
        };

        let args = VisionaryApp::build_attack_args(&h, Some(&donor)).unwrap();
        assert_eq!(args.len(), 36);
        assert!(args.iter().all(|arg| !matches!(arg, A::Nil)));
        assert_eq!(args[0], A::Int(7));
        assert_eq!(args[1], A::Int(2));
        assert_eq!(args[20], A::Int(3));
        assert_eq!(args[21], A::Num(0.75));
        assert_eq!(args[32], A::Hash(hash40::hash40("collision_attr_fire").0));
    }
}
