//! Buffered CSV commands from SD — Jorge excommand facade (FUN_7100110ff8 CSV + enums).

use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::VecDeque;
use std::sync::LazyLock;

use smash::app::lua_bind::{ControlModule, PostureModule};
use smash::app::sv_battle_object;

use crate::slight::csv;
use crate::slight::effect_viewer::apply::{apply_rpm_edit, ParsedEdit};
use crate::slight::effect_viewer::effect_data::EffectData;
use crate::slight::slight_consts::{commands::CommandName, common_status, parse_command};

pub const USER_DIR: &str = "sd:/slight/user/";

/// Stick reads below this magnitude are treated as neutral (no manipulation this frame).
const STICK_DEADZONE: f32 = 0.2;
/// Degrees of rotation per frame at full stick deflection (ROT live mode).
const ROT_RATE: f32 = 6.0;
/// Effect-space units the stick can drive position to at full deflection (STICK live mode).
const POS_RANGE: f32 = 8.0;
/// Effect-space units of positional nudge per frame at full stick (TOP / FORWARD live mode).
const MOVE_RATE: f32 = 0.25;
/// Scale change per frame at full stick (ALL live mode).
const SCALE_RATE: f32 = 0.05;

/// Live input snapshot for the controlling fighter — Jorge FUN_71001082b8
/// (`ControlModule::get_stick_x/y` + `PostureModule::lr`). `lr` is the facing
/// direction (±1) used to keep "forward" consistent regardless of which way the
/// fighter faces.
#[derive(Clone, Copy, Default)]
pub struct InputSnapshot {
    pub stick_x: f32,
    pub stick_y: f32,
    pub lr: f32,
}

impl InputSnapshot {
    /// Stick X with the deadzone applied.
    fn x(&self) -> f32 {
        if self.stick_x.abs() < STICK_DEADZONE {
            0.0
        } else {
            self.stick_x
        }
    }
    /// Stick Y with the deadzone applied.
    fn y(&self) -> f32 {
        if self.stick_y.abs() < STICK_DEADZONE {
            0.0
        } else {
            self.stick_y
        }
    }
    fn neutral(&self) -> bool {
        self.x() == 0.0 && self.y() == 0.0
    }
}

/// Read the live stick + facing for a player boid. `None` when the fighter isn't live.
pub fn read_input(boid: u32) -> Option<InputSnapshot> {
    let boma = unsafe { sv_battle_object::module_accessor(boid) };
    if boma.is_null() {
        return None;
    }
    unsafe {
        Some(InputSnapshot {
            stick_x: ControlModule::get_stick_x(boma),
            stick_y: ControlModule::get_stick_y(boma),
            lr: PostureModule::lr(boma),
        })
    }
}

static BUFFER: LazyLock<Mutex<VecDeque<BufferedCommand>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

#[derive(Clone, Debug, Deserialize)]
pub struct CommandRead {
    pub read_frames: i32,
    pub accomplish_frames: i32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BufferedCommand {
    pub name: String,
    #[serde(default, rename = "remove_if_len")]
    pub remove_if_len: usize,
    #[serde(default = "default_ttl")]
    pub ttl: i32,
    #[serde(default)]
    pub read: Option<CommandRead>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(skip)]
    frames_waited: i32,
}

fn default_ttl() -> i32 {
    300
}

#[derive(Deserialize)]
struct JsonCommandRead {
    read_frames: i32,
    accomplish_frames: i32,
}

#[derive(Deserialize)]
struct JsonBufferedCommand {
    name: String,
    #[serde(default, rename = "remove_if_len")]
    remove_if_len: usize,
    #[serde(default = "default_ttl")]
    ttl: i32,
    read: Option<JsonCommandRead>,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Deserialize)]
struct PatternMultiplierRow {
    id: u64,
    pattern: String,
    field: String,
    #[serde(default = "one_f")]
    factor: f32,
    #[serde(default)]
    min: f32,
    #[serde(default = "max_f")]
    max: f32,
}

fn one_f() -> f32 {
    1.0
}

fn max_f() -> f32 {
    f32::MAX
}

impl BufferedCommand {
    fn tick(&mut self) -> bool {
        self.ttl -= 1;
        if self.ttl <= 0 {
            return false;
        }
        if let Some(read) = &self.read {
            self.frames_waited += 1;
            if self.frames_waited < read.read_frames {
                return true;
            }
            if self.frames_waited < read.read_frames + read.accomplish_frames {
                return true;
            }
        }
        true
    }

    fn ready_to_run(&self) -> bool {
        match &self.read {
            None => true,
            Some(read) => self.frames_waited >= read.read_frames + read.accomplish_frames,
        }
    }

    /// A direction command with no explicit numeric value is driven live from the stick
    /// each frame (Jorge in-game effect manipulation), instead of being run once with a
    /// fixed value. e.g. `ROT 5` live-rotates effect 5; `ROT 5 1.5 x` is a one-shot set.
    fn is_live(&self) -> bool {
        if !is_direction(parse_command(&self.name)) {
            return false;
        }
        let explicit = self
            .args
            .get(1)
            .and_then(|s| s.parse::<f32>().ok())
            .is_some();
        !explicit
    }
}

fn is_direction(kind: CommandName) -> bool {
    matches!(
        kind,
        CommandName::Rot
            | CommandName::Stick
            | CommandName::Top
            | CommandName::Forward
            | CommandName::All
    )
}

pub fn install() {
    let _ = std::fs::create_dir_all(USER_DIR);
}

pub fn push(cmd: BufferedCommand) {
    BUFFER.lock().push_back(cmd);
}

pub fn on_frame() {
    // `poll_sd` runs on the throttled SD tick (see `slight::sd_poll`), not here — it is a
    // directory enumeration, and one per frame is what put Windows testers at 10 fps. The rest
    // of this function is pure memory work and must keep running every frame: these commands
    // drive live effect manipulation from the stick, so losing frames would be visible.
    drive_live_commands();
    run_ready_commands();
    BUFFER.lock().retain_mut(|c| c.tick());
}

/// Each frame, read the controlling fighter's live stick and apply it to the effects
/// targeted by every active live direction command. These commands persist (driven
/// continuously) until their `ttl` expires, rather than running once.
fn drive_live_commands() {
    if crate::slight::frame_context::is_after_win() {
        return;
    }
    let live: Vec<BufferedCommand> = {
        let buf = BUFFER.lock();
        buf.iter().filter(|c| c.is_live()).cloned().collect()
    };
    for cmd in live {
        let kind = parse_command(&cmd.name);
        let boid_filter = cmd.args.first().and_then(|s| s.parse::<u32>().ok());
        // The fighter whose stick drives the manipulation: the targeted boid when one is
        // given, otherwise the host's player (boid 0).
        let control = boid_filter.unwrap_or(0);
        let Some(snap) = read_input(control) else {
            continue;
        };
        if snap.neutral() {
            continue;
        }
        let axis = cmd
            .args
            .get(2)
            .and_then(|s| common_status::parse_axis(s))
            .unwrap_or('y');
        for id in collect_target_ids(kind, boid_filter) {
            drive_effect_live(id, kind, axis, snap);
        }
    }
}

/// Apply one frame of live stick input to a single tracked effect.
fn drive_effect_live(id: u64, kind: CommandName, axis: char, snap: InputSnapshot) {
    let mut data = match crate::slight::effect_viewer::tracker::EFFECT_TRACKER
        .lock()
        .get(id)
        .map(|e| e.data.clone())
    {
        Some(d) => d,
        None => return,
    };
    let mut edit = ParsedEdit {
        id,
        ..Default::default()
    };
    match kind {
        CommandName::Rot => {
            let delta = snap.x() * snap.lr * ROT_RATE;
            let mut rot = data.rot.clone();
            match axis {
                'x' => rot.x += delta,
                'y' => rot.y += delta,
                _ => rot.z += delta,
            }
            edit.rot = Some(rot);
        }
        CommandName::Stick => {
            // Absolute 2D position that follows the stick.
            data.pos.x = snap.x() * snap.lr * POS_RANGE;
            data.pos.y = snap.y() * POS_RANGE;
            edit.pos = Some(data.pos.clone());
        }
        CommandName::Top => {
            data.pos.y += snap.y() * MOVE_RATE;
            edit.pos = Some(data.pos.clone());
        }
        CommandName::Forward => {
            data.pos.z += snap.y() * MOVE_RATE;
            edit.pos = Some(data.pos.clone());
        }
        CommandName::All => {
            edit.scale = Some((data.scale + snap.y() * SCALE_RATE).max(0.01));
        }
        _ => return,
    }
    let _ = apply_rpm_edit(&edit);
}

fn collect_target_ids(kind: CommandName, boid_filter: Option<u32>) -> Vec<u64> {
    let tracker = crate::slight::effect_viewer::tracker::EFFECT_TRACKER.lock();
    tracker
        .iter()
        .filter(|e| match (kind, boid_filter) {
            (CommandName::All, _) => true,
            (_, Some(boid)) => e.boid == boid,
            _ => false,
        })
        .map(|e| e.id)
        .collect()
}

/// Plugin STATE files that live in `USER_DIR` but are not excommand scripts.
///
/// `poll_sd` consumes every `.txt` it finds, so without this it ate the files the plugin
/// itself writes there. `client_id.txt` is rewritten on every editor connect, which made
/// this permanent: the poller retried it every tick forever (see the rename note below).
///
/// `win_detect.txt` and `effect_names.txt` are the user's own configuration, read by
/// `systems::win_screen` and `effect_viewer::effect_names`. They are plain `.txt` in this
/// directory too, so the poller would parse them as commands and move them away — silently
/// reverting win detection to its built-in defaults and the name dictionary to hex.
const RESERVED_USER_FILES: &[&str] = &[
    "client_id.txt",
    "gateway.txt",
    "win_detect.txt",
    "effect_names.txt",
];

/// Where consumed scripts are parked, so they stop inflating the per-tick enumeration.
fn consumed_dir() -> std::path::PathBuf {
    std::path::Path::new(USER_DIR).join("consumed")
}

pub fn poll_sd() {
    let Ok(entries) = std::fs::read_dir(USER_DIR) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name.ends_with(".done") {
            continue;
        }
        if RESERVED_USER_FILES
            .iter()
            .any(|r| name.eq_ignore_ascii_case(r))
        {
            continue;
        }
        if !path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in csv::split_lines(&text) {
            parse_line(&line);
        }
        // Consumed scripts move to a subdirectory rather than being renamed in place. A
        // `.done` left beside the scripts stayed in this directory forever, so the enumeration
        // above grew for the whole session — every consumed command made the poll a little
        // more expensive. `consumed/` has no extension, so the filter above skips it.
        //
        // Clear the destination first. The Switch/emulator FS fails RenameFile when the
        // target exists (POSIX would silently replace it), so a leftover `.done` from an
        // earlier run made this rename fail EVERY tick — the script was re-read, re-parsed
        // and re-renamed forever, several hundred times a second, on the game thread.
        let done = consumed_dir().join(format!("{name}.done"));
        let _ = std::fs::create_dir_all(consumed_dir());
        let _ = std::fs::remove_file(&done);
        if std::fs::rename(&path, &done).is_err() {
            // Still stuck: delete outright rather than let it spin. Losing one consumed
            // command file is strictly better than an unbounded per-frame retry.
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn parse_line(line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return;
    }
    if trimmed.starts_with('{') {
        parse_json_line(trimmed);
        return;
    }
    parse_csv_line(trimmed);
}

/// Jorge FUN_71000e5ce4 — `{effect_name:..., scale:...}` EffectData rows.
fn parse_json_line(trimmed: &str) {
    if let Ok(ed) = serde_json::from_str::<EffectData>(trimmed) {
        apply_effect_data_row(&ed);
        return;
    }
    if let Ok(row) = serde_json::from_str::<PatternMultiplierRow>(trimmed) {
        crate::slight::systems::multipliers::set_pattern_rule(
            row.id,
            &row.pattern,
            &row.field,
            row.factor,
            row.min,
            row.max,
        );
        return;
    }
    if let Ok(json) = serde_json::from_str::<JsonBufferedCommand>(trimmed) {
        let read = json.read.map(|r| CommandRead {
            read_frames: r.read_frames,
            accomplish_frames: r.accomplish_frames,
        });
        push(BufferedCommand {
            name: json.name,
            remove_if_len: json.remove_if_len,
            ttl: json.ttl,
            read,
            args: json.args,
            frames_waited: 0,
        });
        return;
    }
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Excommand: unparseable JSON line: {trimmed}");
    }
}

fn apply_effect_data_row(ed: &EffectData) {
    if ed.index != 0 {
        crate::slight::systems::fighter_data_space::set_effect_data(ed.index, ed.clone());
    } else if let Some(rec) = crate::slight::frame_context::current_agent() {
        crate::slight::systems::fighter_data_space::set_effect_data(rec.boid, ed.clone());
    }
    let targets: Vec<u64> = {
        let tracker = crate::slight::effect_viewer::tracker::EFFECT_TRACKER.lock();
        tracker
            .iter()
            .filter(|e| effect_row_matches(e, ed))
            .map(|e| e.id)
            .collect()
    };
    for id in targets {
        let mut edit = ParsedEdit {
            id,
            ..Default::default()
        };
        if ed.scale != 1.0 {
            edit.scale = Some(ed.scale);
        }
        if ed.rate != 1.0 {
            edit.rate = Some(ed.rate);
        }
        if ed.frame != 0.0 {
            edit.frame = Some(ed.frame);
        }
        if ed.pos.x != 0.0 || ed.pos.y != 0.0 || ed.pos.z != 0.0 {
            edit.pos = Some(ed.pos.clone());
        }
        if ed.rot.x != 0.0 || ed.rot.y != 0.0 || ed.rot.z != 0.0 {
            edit.rot = Some(ed.rot.clone());
        }
        edit.visible = Some(ed.visible);
        edit.is_follow = Some(ed.is_follow);
        if ed.rainbow.color.red != 1.0
            || ed.rainbow.color.green != 1.0
            || ed.rainbow.color.blue != 1.0
            || ed.rainbow.color.alpha != 1.0
        {
            edit.color = Some(ed.rainbow.color.clone());
        }
        if ed.rainbow.movement_state != 0.0 {
            edit.movement_state = Some(ed.rainbow.movement_state);
        }
        apply_rpm_edit(&edit);
    }
}

fn effect_row_matches(
    tracked: &crate::slight::effect_viewer::tracker::TrackedEffect,
    ed: &EffectData,
) -> bool {
    if ed.index != 0 && tracked.data.index == ed.index {
        return true;
    }
    if !ed.effect_name.is_empty()
        && ed.effect_name != "0x0"
        && tracked.data.effect_name == ed.effect_name
    {
        if ed.bone_name.is_empty() || ed.bone_name == "0x0" {
            return true;
        }
        return tracked.data.bone_name == ed.bone_name;
    }
    false
}

fn parse_csv_line(line: &str) {
    let parts = csv::split_record(line);
    if parts.is_empty() {
        return;
    }
    let name = parts[0].clone();
    let remove_if_len = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let ttl = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let mut idx = 3usize;
    let read = if parts.len() > idx + 1 {
        if let (Some(rf), Some(af)) = (
            parts.get(idx).and_then(|s| s.parse().ok()),
            parts.get(idx + 1).and_then(|s| s.parse().ok()),
        ) {
            idx += 2;
            Some(CommandRead {
                read_frames: rf,
                accomplish_frames: af,
            })
        } else {
            None
        }
    } else {
        None
    };
    let args: Vec<String> = parts.iter().skip(idx).cloned().collect();
    push(BufferedCommand {
        name,
        remove_if_len,
        ttl,
        read,
        args,
        frames_waited: 0,
    });
    if crate::slight::smash_utils::debug_logging_enabled() {
        skyline::println!("[SLight] Excommand queued: {line}");
    }
}

fn run_ready_commands() {
    let mut buf = BUFFER.lock();
    let mut i = 0;
    while i < buf.len() {
        if buf[i].is_live() {
            // Live commands are driven every frame by `drive_live_commands`, not run once.
            i += 1;
            continue;
        }
        if buf[i].ready_to_run() {
            let cmd = buf[i].clone();
            drop(buf);
            execute(&cmd);
            buf = BUFFER.lock();
            if cmd.remove_if_len > 0 && buf.len() > cmd.remove_if_len {
                buf.pop_front();
            } else {
                buf.remove(i);
            }
            continue;
        }
        i += 1;
    }
}

fn execute(cmd: &BufferedCommand) {
    let parsed = parse_command(&cmd.name);
    match parsed {
        CommandName::SetMultiplier => dispatch_multiplier(cmd),
        CommandName::ClearMultipliers => crate::slight::systems::multipliers::clear(),
        CommandName::DebugLog => {
            let on = cmd
                .args
                .first()
                .map(|s| s != "0" && !s.eq_ignore_ascii_case("off"));
            if let Some(on) = on {
                crate::slight::smash_utils::set_debug_logging(on);
            }
        }
        CommandName::Rot | CommandName::Stick | CommandName::Top | CommandName::Forward => {
            dispatch_direction(cmd, parsed);
        }
        CommandName::All => dispatch_direction(cmd, CommandName::All),
        CommandName::Unknown => {
            if crate::slight::smash_utils::debug_logging_enabled() {
                skyline::println!("[SLight] Excommand run: {} {:?}", cmd.name, cmd.args);
            }
        }
    }
}

fn dispatch_direction(cmd: &BufferedCommand, kind: CommandName) {
    let boid_filter = cmd.args.first().and_then(|s| s.parse::<u32>().ok());
    let value = cmd
        .args
        .get(1)
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.0);
    let axis = cmd
        .args
        .get(2)
        .and_then(|s| common_status::parse_axis(s))
        .unwrap_or('y');

    for id in collect_target_ids(kind, boid_filter) {
        let mut edit = ParsedEdit {
            id,
            ..Default::default()
        };
        match kind {
            CommandName::Rot => {
                let mut rot = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
                    .lock()
                    .get(id)
                    .map(|e| e.data.rot.clone())
                    .unwrap_or_default();
                match axis {
                    'x' => rot.x = value,
                    'y' => rot.y = value,
                    _ => rot.z = value,
                }
                edit.rot = Some(rot);
            }
            CommandName::Stick => {
                let mut pos = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
                    .lock()
                    .get(id)
                    .map(|e| e.data.pos.clone())
                    .unwrap_or_default();
                match axis {
                    'x' => pos.x = value,
                    'y' => pos.y = value,
                    _ => pos.z = value,
                }
                edit.pos = Some(pos);
            }
            CommandName::Top => {
                let mut pos = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
                    .lock()
                    .get(id)
                    .map(|e| e.data.pos.clone())
                    .unwrap_or_default();
                pos.y += value;
                edit.pos = Some(pos);
            }
            CommandName::Forward => {
                let mut pos = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
                    .lock()
                    .get(id)
                    .map(|e| e.data.pos.clone())
                    .unwrap_or_default();
                pos.z += value;
                edit.pos = Some(pos);
            }
            CommandName::All => {
                edit.scale = Some(value.max(0.01));
            }
            _ => continue,
        }
        if let Some(boid) = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
            .lock()
            .get(id)
            .map(|e| e.boid)
        {
            if let Some(data) = crate::slight::effect_viewer::tracker::EFFECT_TRACKER
                .lock()
                .get(id)
                .map(|e| e.data.clone())
            {
                crate::slight::systems::fighter_data_space::set_effect_data(boid, data);
            }
        }
        let _ = apply_rpm_edit(&edit);
    }
}

fn dispatch_multiplier(cmd: &BufferedCommand) {
    if cmd.args.len() >= 6 {
        if let (Some(id), Some(pattern), Some(field), Some(factor), Some(min), Some(max)) = (
            cmd.args.first().and_then(|s| s.parse().ok()),
            cmd.args.get(1).map(String::as_str),
            cmd.args.get(2).map(String::as_str),
            cmd.args.get(3).and_then(|s| s.parse().ok()),
            cmd.args.get(4).and_then(|s| s.parse().ok()),
            cmd.args.get(5).and_then(|s| s.parse().ok()),
        ) {
            crate::slight::systems::multipliers::set_pattern_rule(
                id, pattern, field, factor, min, max,
            );
            return;
        }
    }
    let boid = cmd.args.first().and_then(|s| s.parse().ok());
    let field = cmd.args.get(1).map(String::as_str);
    let value = cmd.args.get(2).and_then(|s| s.parse().ok());
    if let (Some(boid), Some(field), Some(value)) = (boid, field, value) {
        crate::slight::systems::multipliers::set_rule(boid, field, value);
        return;
    }
    if cmd.args.len() >= 4 {
        if let (Some(_), Some(field), Some(value)) = (
            crate::slight::slight_consts::fighters::parse_fighter(&cmd.args[1]),
            cmd.args.get(2).map(String::as_str),
            cmd.args.get(3).and_then(|s| s.parse().ok()),
        ) {
            crate::slight::systems::multipliers::set_fighter_rule(&cmd.args[1], field, value);
        }
    }
}

pub fn pending() -> Vec<BufferedCommand> {
    BUFFER.lock().iter().cloned().collect()
}

pub fn clear() {
    BUFFER.lock().clear();
}
