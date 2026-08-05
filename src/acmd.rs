/// Parse ACMD scripts into a structured IR that preserves loops and can be re-exported.
use crate::data::{
    AcmdScript, AcmdStmt, AttackCall, EffectMacro, EffectScript, EffectStmt, ExcuteStmt,
};

/// Convert snake_case motion name to PascalCase filename.
pub fn move_name_to_pascal(name: &str) -> String {
    name.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

/// Fetch and parse hitboxes for a fighter+move from GitHub.
#[allow(dead_code)]
pub fn fetch_acmd_script(fighter: &str, move_name: &str) -> anyhow::Result<AcmdScript> {
    let body = fetch_script_body(fighter, move_name)?;
    Ok(parse_acmd_script(&body))
}

/// Worker threads used for fighter-wide script scans (and the connection-pool size).
/// GitHub's raw host serves this happily; more stops helping once the pipe is saturated.
pub const SCRIPT_FETCH_THREADS: usize = 8;

/// Shared blocking HTTP client for every GitHub script/index fetch.
///
/// `reqwest::blocking::get` builds a THROWAWAY client per call, so a fighter-wide scan paid a
/// fresh DNS + TCP + TLS handshake for all ~450 moves (measured ~236 ms per request, against
/// ~32 ms on a kept-alive connection). One shared client pools connections across every
/// request and is safe to use from several threads at once.
static HTTP: std::sync::LazyLock<reqwest::blocking::Client> = std::sync::LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .user_agent("visionary")
        .pool_max_idle_per_host(SCRIPT_FETCH_THREADS)
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
});

/// The shared pooled HTTP client (see [`HTTP`]).
pub fn http_client() -> &'static reqwest::blocking::Client {
    &HTTP
}

/// Disk path a fighter+move's cached script body lives at.
fn script_cache_path(fighter: &str, move_name: &str) -> std::path::PathBuf {
    crate::scratch_dirs::app_storage_root()
        .join("script-cache")
        .join(fighter)
        .join(format!("{}.txt", move_name_to_pascal(move_name)))
}

/// Cached script body, if this move was fetched before. Never touches the network — lets a
/// scan resolve every already-known move up front and spend threads only on the rest.
pub fn cached_script_body(fighter: &str, move_name: &str) -> Option<String> {
    std::fs::read_to_string(script_cache_path(fighter, move_name)).ok()
}

/// Fetch the raw script body text for a fighter+move from GitHub.
pub fn fetch_script_body(fighter: &str, move_name: &str) -> anyhow::Result<String> {
    let pascal = move_name_to_pascal(move_name);
    let url = format!(
        "https://raw.githubusercontent.com/WuBoytH/SSBU-Dumped-Scripts/main/smashline/lua2cpp_{fighter}/{fighter}/{pascal}.txt"
    );
    Ok(HTTP.get(&url).send()?.text()?)
}

/// Disk-cached [`fetch_script_body`]: bodies (including "404: Not Found" misses) are
/// stored under `{app_storage_root}/script-cache/{fighter}/`, so fighter-wide scans
/// (the transplant studio's full-use discovery) only hit the network once per move ever.
pub fn fetch_script_body_cached(fighter: &str, move_name: &str) -> anyhow::Result<String> {
    if let Some(body) = cached_script_body(fighter, move_name) {
        return Ok(body);
    }
    let body = fetch_script_body(fighter, move_name)?;
    let path = script_cache_path(fighter, move_name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, &body);
    Ok(body)
}

pub fn parse_acmd_script(source: &str) -> AcmdScript {
    let game_fn = extract_game_function(source);
    let source = game_fn.as_deref().unwrap_or(source);
    let lines: Vec<&str> = source.lines().collect();
    // Skip the function signature line and closing brace
    let body_lines = if lines.len() >= 2 {
        &lines[1..lines.len() - 1]
    } else {
        &lines[..]
    };
    let (stmts, _) = parse_stmts(body_lines, 0);
    AcmdScript { stmts }
}

/// Parse statements from a slice of lines starting at `pos`.
/// Returns (statements, lines_consumed).
fn parse_stmts(lines: &[&str], mut pos: usize) -> (Vec<AcmdStmt>, usize) {
    let mut stmts = Vec::new();

    while pos < lines.len() {
        let line = lines[pos].trim();

        // Skip empty lines and closing braces handled by caller
        if line.is_empty() || line == "}" {
            pos += 1;
            continue;
        }

        // for _ in 0..N { ... }
        if let Some(count) = parse_for_loop_header(line) {
            // Find the matching closing brace
            let body_start = pos + 1;
            let (body_lines_end, _) = find_block_end(lines, pos);
            let body_slice = &lines[body_start..body_lines_end];
            let (body, _) = parse_stmts(body_slice, 0);
            stmts.push(AcmdStmt::Loop { count, body });
            pos = body_lines_end + 1;
            continue;
        }

        // if macros::is_excute(agent) { ... }
        if line.contains("is_excute") {
            let body_start = pos + 1;
            let (body_end, _) = find_block_end(lines, pos);
            let excute_stmts = parse_excute_block(&lines[body_start..body_end]);
            stmts.push(AcmdStmt::Excute(excute_stmts));
            pos = body_end + 1;
            continue;
        }

        // frame(lua_state, N)
        if line.contains("frame(") && !line.contains("is_excute") {
            if let Some(f) = parse_frame_call(line) {
                stmts.push(AcmdStmt::Frame(f));
                pos += 1;
                continue;
            }
        }

        // wait_loop_clear
        if line.contains("wait_loop_clear") {
            stmts.push(AcmdStmt::WaitLoopClear);
            pos += 1;
            continue;
        }

        // wait(lua_state, N)
        if line.contains("wait(") {
            if let Some(w) = parse_wait_call(line) {
                stmts.push(AcmdStmt::Wait(w));
                pos += 1;
                continue;
            }
        }

        // Everything else — preserve verbatim
        if !line.is_empty() {
            stmts.push(AcmdStmt::Raw(line.to_string()));
        }
        pos += 1;
    }

    (stmts, pos)
}

/// Parse the contents of an is_excute block.
fn parse_excute_block(lines: &[&str]) -> Vec<ExcuteStmt> {
    let mut stmts = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if ATTACK_FUNCS
            .iter()
            .any(|name| line.contains(&format!("macros::{name}(")))
        {
            if let Some(call) = parse_attack_call(line) {
                stmts.push(ExcuteStmt::Attack(call));
                continue;
            }
        }
        if line.contains("macros::CATCH(") {
            if let Some(call) = parse_catch_call(line) {
                stmts.push(ExcuteStmt::Catch(call));
                continue;
            }
        }
        // Safe below the `ATTACK_FUNCS` test above only because that one matches on
        // `macros::NAME(` with the paren: `macros::ATTACK_ABS(` does not contain
        // `macros::ATTACK(`. Without the paren this call would be read through `ATTACK`'s
        // 36-slot layout, which is the cross-family corruption this file keeps warning about.
        if line.contains("macros::ATTACK_ABS(") {
            if let Some(call) = parse_attack_abs_call(line) {
                stmts.push(ExcuteStmt::AttackAbs(call));
                continue;
            }
        }
        if line.contains("AREA_WIND_2ND") {
            if let Some(call) = parse_wind_call(line) {
                stmts.push(ExcuteStmt::Wind(call));
                continue;
            }
        }
        if line.contains("erase_wind") {
            if let Some(id) = parse_erase_wind(line) {
                stmts.push(ExcuteStmt::EraseWind(id));
                continue;
            }
        }
        if line.contains("AttackModule::clear(") {
            if let Some(id) = parse_attack_clear(line) {
                stmts.push(ExcuteStmt::Clear(id));
                continue;
            }
        }
        if let Some(stmt) = parse_hurtbox_call(line) {
            stmts.push(stmt);
            continue;
        }
        if let Some(stmt) = parse_attack_mod_call(line) {
            stmts.push(stmt);
            continue;
        }
        // GrabModule and AttackModule clear different things, so they are different
        // statements. Matching `clear_all` alone re-emitted a grab clear as an attack clear.
        if line.contains("GrabModule::clear_all") {
            stmts.push(ExcuteStmt::GrabClearAll);
            continue;
        }
        if line.contains("clear_all") {
            stmts.push(ExcuteStmt::ClearAll);
            continue;
        }
        stmts.push(ExcuteStmt::Raw(line.to_string()));
    }
    stmts
}

/// Find the line index of the closing `}` that matches the opening `{` on `lines[start]`.
/// Returns (closing_line_index, depth_at_end).
fn find_block_end(lines: &[&str], start: usize) -> (usize, i32) {
    let mut depth = 0i32;
    for (i, line) in lines[start..].iter().enumerate() {
        for ch in line.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if depth == 0 {
            return (start + i, 0);
        }
    }
    (lines.len().saturating_sub(1), depth)
}

/// Extract only the `game_` function body.
fn extract_game_function(source: &str) -> Option<String> {
    let mut result = String::new();
    let mut in_game_fn = false;
    let mut depth: i32 = 0;
    let mut found = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if !in_game_fn
            && trimmed.contains("game_")
            && !trimmed.contains("effect_")
            && !trimmed.contains("sound_")
            && !trimmed.contains("expression_")
            && (trimmed.contains("fn game_") || trimmed.starts_with("unsafe extern"))
        {
            in_game_fn = true;
            found = true;
        }
        if in_game_fn {
            result.push_str(line);
            result.push('\n');
            for ch in line.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            if depth == 0 {
                break;
            }
        }
    }
    if found {
        Some(result)
    } else {
        None
    }
}

/// Extract only the `effect_` function body (mirrors `extract_game_function`).
fn extract_effect_function(source: &str) -> Option<String> {
    let mut result = String::new();
    let mut in_effect_fn = false;
    let mut depth: i32 = 0;
    let mut found = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if !in_effect_fn
            && trimmed.contains("effect_")
            && !trimmed.contains("game_")
            && !trimmed.contains("sound_")
            && !trimmed.contains("expression_")
            && (trimmed.contains("fn effect_") || trimmed.starts_with("unsafe extern"))
        {
            in_effect_fn = true;
            found = true;
        }
        if in_effect_fn {
            result.push_str(line);
            result.push('\n');
            for ch in line.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            if depth == 0 {
                break;
            }
        }
    }
    if found {
        Some(result)
    } else {
        None
    }
}

/// Parse the contents of an is_excute block from an effect_ script.
fn parse_excute_block_effects(lines: &[&str]) -> Vec<EffectMacro> {
    let mut macros = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Helper: extract args string from a macro call like `macros::FOO(...)`
        let try_extract = |prefix: &str| -> Option<Vec<String>> {
            let start = line.find(prefix)?;
            let after = &line[start + prefix.len()..];
            // find matching closing paren
            let end = after.rfind(')')?;
            Some(tokenize_args(&after[..end]))
        };

        // All of these spawn families begin with the same editable payload: agent,
        // graphic[, flipped graphic], joint, position xyz, rotation xyz, scale. Their
        // remaining alpha/attribute/random/contact arguments do not alter the timeline
        // transform, so they ride along as text in `extra_args` for the export to replay.
        if let Some((name, flip, follows_bone)) = effect_spawn_macro_layout(line) {
            let prefix = format!("macros::{name}(");
            if let Some(t) = try_extract(&prefix) {
                let off = usize::from(flip);
                if t.len() > 9 + off {
                    let effect_name =
                        extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    let effect_name_alt = flip.then(|| {
                        extract_hash40_string(&t[2]).unwrap_or_else(|| t[2].trim().to_string())
                    });
                    let bone_name = extract_hash40_string(&t[2 + off])
                        .unwrap_or_else(|| t[2 + off].trim().to_string());
                    let num = |i: usize, default: f32| {
                        t.get(i)
                            .and_then(|value| value.trim().parse::<f32>().ok())
                            .unwrap_or(default)
                    };
                    macros.push(EffectMacro::Effect {
                        effect_name,
                        effect_name_alt,
                        spawn_func: name.to_string(),
                        bone_name,
                        offset: [num(3 + off, 0.0), num(4 + off, 0.0), num(5 + off, 0.0)],
                        // The three rotation slots run zr, yr, xr — reversed from the
                        // [x, y, z] the rest of the editor uses. This used to read them left
                        // to right, silently swapping the X and Z angles of every script
                        // loaded from source and disagreeing with the live-capture and
                        // live-pin paths (app.rs) and the plugin (acmd_hooks.rs parse_args)
                        // about the very same call.
                        rotation: [num(8 + off, 0.0), num(7 + off, 0.0), num(6 + off, 0.0)],
                        scale: num(9 + off, 1.0),
                        follows_bone,
                        extra_args: t[10 + off..].to_vec(),
                    });
                    continue;
                }
            }
        }

        if line.contains("macros::EFFECT_OFF_KIND(") {
            if let Some(t) = try_extract("macros::EFFECT_OFF_KIND(") {
                if t.len() > 1 {
                    let effect_name =
                        extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    macros.push(EffectMacro::EffectOffKind { effect_name });
                    continue;
                }
            }
        }

        // AFTER_IMAGE4_ON / AFTER_IMAGE4_ON_arg29 / AFTER_IMAGE_ON — sword/weapon trail effects.
        // Signature: AFTER_IMAGE4_ON_arg29(agent, tex1, tex2, count, bone, ...)
        // We extract the bone name (arg[4]) and use tex1 (arg[1]) as the effect name.
        if line.contains("macros::AFTER_IMAGE4_ON") || line.contains("macros::AFTER_IMAGE_ON") {
            let prefix = if line.contains("macros::AFTER_IMAGE4_ON_arg29(") {
                "macros::AFTER_IMAGE4_ON_arg29("
            } else if line.contains("macros::AFTER_IMAGE4_ON(") {
                "macros::AFTER_IMAGE4_ON("
            } else {
                "macros::AFTER_IMAGE_ON("
            };
            if let Some(t) = try_extract(prefix) {
                // t[1] = tex1 (effect name), t[4] = bone
                let effect_name = extract_hash40_string(t.get(1).map(|s| s.as_str()).unwrap_or(""))
                    .unwrap_or_else(|| t.get(1).map(|s| s.trim().to_string()).unwrap_or_default());
                let bone_name = extract_hash40_string(t.get(4).map(|s| s.as_str()).unwrap_or(""))
                    .unwrap_or_else(|| t.get(4).map(|s| s.trim().to_string()).unwrap_or_default());
                if !effect_name.is_empty() {
                    macros.push(EffectMacro::AfterImage {
                        effect_name,
                        bone_name,
                        raw: line.to_string(),
                    });
                    continue;
                }
            }
        }

        // AFTER_IMAGE_OFF — turns off a sword trail.
        if line.contains("macros::AFTER_IMAGE_OFF(") {
            macros.push(EffectMacro::AfterImageOff);
            continue;
        }

        if line.contains("macros::LAST_EFFECT_SET_RATE(") {
            if let Some(t) = try_extract("macros::LAST_EFFECT_SET_RATE(") {
                if t.len() > 1 {
                    let rate = t[1].trim().parse::<f32>().unwrap_or(0.0);
                    macros.push(EffectMacro::LastEffectSetRate { rate });
                    continue;
                }
            }
        }

        // Tint and opacity for the last spawned effect. Both arities are uniform across the
        // whole corpus — 65 `(agent, r, g, b)` and 4 `(agent, a)` — so a call of the wrong
        // length is not a variant to interpret but a line this parser does not understand, and
        // falls through to `Raw` rather than being padded into shape. A component that will not
        // parse does the same: these arguments are the entire content of the call, so defaulting
        // one means recolouring an effect the script never recoloured.
        if line.contains("macros::LAST_EFFECT_SET_COLOR(") {
            if let Some(t) = try_extract("macros::LAST_EFFECT_SET_COLOR(") {
                if t.len() > 3 {
                    let component = |i: usize| t[i].trim().parse::<f32>().ok();
                    if let (Some(r), Some(g), Some(b)) = (component(1), component(2), component(3))
                    {
                        macros.push(EffectMacro::LastEffectSetColor { rgb: [r, g, b] });
                        continue;
                    }
                }
            }
        }

        if line.contains("macros::LAST_EFFECT_SET_ALPHA(") {
            if let Some(t) = try_extract("macros::LAST_EFFECT_SET_ALPHA(") {
                if t.len() > 1 {
                    if let Ok(alpha) = t[1].trim().parse::<f32>() {
                        macros.push(EffectMacro::LastEffectSetAlpha { alpha });
                        continue;
                    }
                }
            }
        }

        // FLASH / BURN_COLOR and friends: not spawns, but they belong to the effect timeline
        // and the export regenerates that timeline from scratch, so a line left unmodelled
        // here is a line the export drops.
        if let Some((command, has_transition, has_rgba)) = color_macro_layout(line) {
            if let Some(t) = try_extract(&format!("macros::{command}(")) {
                let (transition_slot, rgba_slots) = crate::data::color_slots(has_transition);
                let num = |i: usize| -> Option<f32> { t.get(i)?.trim().parse::<f32>().ok() };
                // A short call is left as a `Raw` line rather than defaulted into shape: these
                // arguments are the whole content of the command, so guessing one means
                // shipping a colour the script never asked for.
                let transition = transition_slot.map(num);
                let rgba = has_rgba.then(|| {
                    let mut out = [0.0; 4];
                    for (dst, slot) in out.iter_mut().zip(rgba_slots) {
                        *dst = num(slot)?;
                    }
                    Some(out)
                });
                if !matches!(transition, Some(None)) && !matches!(rgba, Some(None)) {
                    macros.push(EffectMacro::Color {
                        command: command.to_string(),
                        color: crate::data::ColorCall {
                            transition: transition.flatten(),
                            rgba: rgba.flatten(),
                        },
                    });
                    continue;
                }
            }
        }

        macros.push(EffectMacro::Raw(line.to_string()));
    }
    macros
}

/// The colour command this line calls, with its argument layout.
///
/// Matched on the trailing `(`, which is what makes the table's order irrelevant here:
/// `macros::BURN_COLOR_FRAME(` does not contain `macros::BURN_COLOR(`, so the longer name
/// cannot be read as the shorter one with a stray argument. `effect_spawn_macro_layout` orders
/// its table longest-first for the same hazard; this one does not have to.
fn color_macro_layout(line: &str) -> Option<(&'static str, bool, bool)> {
    crate::data::COLOR_COMMANDS
        .iter()
        .copied()
        .find(|(command, _, _)| line.contains(&format!("macros::{command}(")))
}

/// Whether this line opens a brace block it does not also close.
///
/// Counted rather than tested with `ends_with('{')` because the dumps write costume checks as
/// `if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 3)){` — no space before the brace, and a
/// closing paren in between. No effect line puts a brace inside a string literal, so counting
/// characters is safe here.
fn opens_block(line: &str) -> bool {
    line.chars().filter(|c| *c == '{').count() > line.chars().filter(|c| *c == '}').count()
}

/// Parse statements from an effect_ function body, producing `EffectStmt`.
///
/// **On `else`.** These files are decompiler output, and the decompiler mis-scopes `else`: in
/// [kirby/CapturePulledHi.txt]() the `else {` sits inside the `if macros::is_excute(agent) {`
/// block it should be a sibling of, and the function only balances by accident. So an `else`
/// header is captured as an opaque [`EffectStmt::Cond`] header exactly like an `if` — no attempt
/// is made to pair it with anything. Anything that tried would be reasoning about a structure
/// the input does not actually have, and would be wrong on real fighters.
fn parse_effect_stmts(lines: &[&str], mut pos: usize) -> (Vec<EffectStmt>, usize) {
    let mut stmts = Vec::new();

    while pos < lines.len() {
        let line = lines[pos].trim();

        if line.is_empty() || line == "}" {
            pos += 1;
            continue;
        }

        if let Some(count) = parse_for_loop_header(line) {
            let body_start = pos + 1;
            let (body_lines_end, _) = find_block_end(lines, pos);
            let body_slice = &lines[body_start..body_lines_end];
            let (body, _) = parse_effect_stmts(body_slice, 0);
            stmts.push(EffectStmt::Loop { count, body });
            pos = body_lines_end + 1;
            continue;
        }

        if line.contains("is_excute") {
            let body_start = pos + 1;
            let (body_end, _) = find_block_end(lines, pos);
            let effect_macros = parse_excute_block_effects(&lines[body_start..body_end]);
            stmts.push(EffectStmt::Excute(effect_macros));
            pos = body_end + 1;
            continue;
        }

        // Any other block-opening line: `if <costume check> {`, `if !WorkModule::is_flag(…) {`,
        // `else {`. Tested after `is_excute` and after the `for` header, both of which also open
        // a block and both of which have typed forms above.
        //
        // This used to fall through to `EffectStmt::Raw` below, which kept the header as a
        // dropped line and — worse — parsed the body as siblings of the conditional. Both arms
        // of an `if`/`else` then resolved as unconditional spawns at the same frame.
        if opens_block(line) {
            let (body_end, _) = find_block_end(lines, pos);
            let (body, _) = parse_effect_stmts(&lines[pos + 1..body_end], 0);
            stmts.push(EffectStmt::Cond {
                header: line.to_string(),
                body,
            });
            pos = body_end + 1;
            continue;
        }

        if line.contains("frame(") && !line.contains("is_excute") && !opens_block(line) {
            if let Some(f) = parse_frame_call(line) {
                stmts.push(EffectStmt::Frame(f));
                pos += 1;
                continue;
            }
        }

        if line.contains("wait(") {
            if let Some(w) = parse_wait_call(line) {
                stmts.push(EffectStmt::Wait(w));
                pos += 1;
                continue;
            }
        }

        // Check for bare EFFECT macro calls (outside is_excute blocks).
        // Some effect functions place EFFECT macros directly in the function body
        // without an is_excute wrapper — route them through parse_excute_block_effects.
        let is_effect_macro = effect_spawn_macro_layout(line).is_some()
            || line.contains("macros::EFFECT_OFF_KIND(")
            || line.contains("macros::AFTER_IMAGE4_ON")
            || line.contains("macros::AFTER_IMAGE_ON")
            || line.contains("macros::AFTER_IMAGE_OFF(")
            || line.contains("macros::LAST_EFFECT_SET_RATE(")
            || line.contains("macros::LAST_EFFECT_SET_COLOR(")
            || line.contains("macros::LAST_EFFECT_SET_ALPHA(")
            || color_macro_layout(line).is_some();
        if is_effect_macro {
            let effect_macros = parse_excute_block_effects(&[line]);
            if !effect_macros.is_empty() {
                stmts.push(EffectStmt::Excute(effect_macros));
            }
            pos += 1;
            continue;
        }

        if !line.is_empty() {
            stmts.push(EffectStmt::Raw(line.to_string()));
        }
        pos += 1;
    }

    (stmts, pos)
}

/// Dumped effect spawn macros that share the common graphic/joint/transform prefix.
/// Returns `(macro name, has second flipped graphic, follows bone)`.
fn effect_spawn_macro_layout(line: &str) -> Option<(&'static str, bool, bool)> {
    const LAYOUTS: &[(&str, bool, bool)] = &[
        ("EFFECT_FOLLOW_NO_STOP_FLIP", true, true),
        ("EFFECT_FOLLOW_FLIP_ALPHA", true, true),
        ("EFFECT_FOLLOW_FLIP_COLOR", true, true),
        ("EFFECT_FOLLOW_FLIP_RND", true, true),
        ("EFFECT_FOLLOW_FLIP", true, true),
        ("EFFECT_FOLLOW_NO_SCALE", false, true),
        ("EFFECT_FOLLOW_NO_STOP", false, true),
        ("EFFECT_FOLLOW_ALPHA", false, true),
        ("EFFECT_FOLLOW_COLOR", false, true),
        ("EFFECT_FLW_POS_UNSYNC_VIS", false, true),
        ("EFFECT_FLW_POS_NO_STOP", false, true),
        ("EFFECT_FLW_UNSYNC_VIS", false, true),
        ("EFFECT_FLW_POS", false, true),
        ("EFFECT_FOLLOW", false, true),
        ("LANDING_EFFECT_FLIP", true, false),
        ("FOOT_EFFECT_FLIP", true, false),
        ("EFFECT_FLIP_ALPHA", true, false),
        ("EFFECT_FLIP", true, false),
        ("LANDING_EFFECT", false, false),
        ("FOOT_EFFECT", false, false),
        ("DOWN_EFFECT", false, false),
        ("EFFECT_ALPHA", false, false),
        ("EFFECT_ATTR", false, false),
        ("EFFECT", false, false),
    ];
    LAYOUTS
        .iter()
        .copied()
        .find(|(name, _, _)| line.contains(&format!("macros::{name}(")))
}

/// Parse an effect_ script source into an `EffectScript` IR.
pub fn parse_effect_script(source: &str) -> crate::data::EffectScript {
    let effect_fn = extract_effect_function(source);
    let effect_fn = match effect_fn {
        Some(ref s) => s.as_str(),
        None => return EffectScript::default(),
    };
    let lines: Vec<&str> = effect_fn.lines().collect();
    let body_lines = if lines.len() >= 2 {
        &lines[1..lines.len() - 1]
    } else {
        &lines[..]
    };
    let (stmts, _) = parse_effect_stmts(body_lines, 0);
    EffectScript { stmts }
}

fn parse_for_loop_header(line: &str) -> Option<usize> {
    let line = line.trim();
    if !line.starts_with("for ") || !line.contains("in 0..") {
        return None;
    }
    let range_start = line.find("in 0..")? + 6;
    let rest = &line[range_start..];
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let count: usize = rest[..num_end].parse().ok()?;
    Some(count.min(20))
}

fn parse_frame_call(line: &str) -> Option<f32> {
    let mut search_start = 0;
    while let Some(pos) = line[search_start..].find("frame(") {
        let abs_pos = search_start + pos;
        let before = if abs_pos == 0 {
            ' '
        } else {
            line.as_bytes()
                .get(abs_pos - 1)
                .copied()
                .map(|b| b as char)
                .unwrap_or(' ')
        };
        if !before.is_alphanumeric() && before != '_' {
            let inner = &line[abs_pos + 6..];
            let end = inner.find(')')?;
            let args: Vec<&str> = inner[..end].split(',').collect();
            if let Some(val) = args.get(1).and_then(|s| s.trim().parse::<f32>().ok()) {
                return Some(val);
            }
        }
        search_start = abs_pos + 6;
    }
    None
}

fn parse_wait_call(line: &str) -> Option<f32> {
    let mut search_start = 0;
    while let Some(pos) = line[search_start..].find("wait(") {
        let abs_pos = search_start + pos;
        let before = if abs_pos == 0 {
            ' '
        } else {
            line.as_bytes()
                .get(abs_pos - 1)
                .copied()
                .map(|b| b as char)
                .unwrap_or(' ')
        };
        if !before.is_alphanumeric() && before != '_' {
            let inner = &line[abs_pos + 5..];
            let end = inner.find(')')?;
            let args: Vec<&str> = inner[..end].split(',').collect();
            if let Some(val) = args.get(1).and_then(|s| s.trim().parse::<f32>().ok()) {
                return Some(val);
            }
        }
        search_start = abs_pos + 5;
    }
    None
}

/// The `ATTACK`-family macros the editor models, longest name first.
///
/// They share one argument layout, which is why they share one parser, one slot table, and
/// one live wire shape. Nothing else in ACMD may be added here on the strength of a similar
/// name — `ATTACK_ABS` is 16 arguments, not 36, and belongs to its own family.
pub const ATTACK_FUNCS: &[&str] = &["ATTACK_IGNORE_THROW", "ATTACK"];

/// Whether an `ATTACK`-family call carries the optional capsule triple at slots 13-15.
///
/// Decided by shape, not by length: `x2`/`y2`/`z2` are `Option<f32>` in smash-script, so a
/// call that has them spells all three as `None` or `Some(..)`. Anything else at slot 13 —
/// a bare `1.0` hitlag multiplier, in the case this was written for — means the triple was
/// left out and the rest of the call sits three slots earlier.
fn capsule_slots_present(t: &[String]) -> bool {
    let optionish = |s: &String| {
        let s = s.trim();
        s == "None" || s.starts_with("Some(")
    };
    t.len() >= 16 && t[13..16].iter().all(optionish)
}

fn parse_attack_call(line: &str) -> Option<AttackCall> {
    // Longest name first: `macros::ATTACK(` cannot match `macros::ATTACK_IGNORE_THROW(`
    // because of the paren, but ordering it this way keeps that from being load-bearing.
    let (func, start) = ATTACK_FUNCS
        .iter()
        .find_map(|name| Some((*name, line.find(&format!("macros::{name}("))?)))?;
    let inner = &line[start + "macros::".len() + func.len() + 1..];
    let end = inner.rfind(')')?;
    let inner = &inner[..end];
    let t = tokenize_args(inner);
    if t.len() < 13 {
        return None;
    }

    // [0]=agent [1]=id [2]=part [3]=bone [4]=damage [5]=angle [6]=kb_scaling
    // [7]=fkb [8]=kb_base [9]=size [10]=ox [11]=oy [12]=oz
    // [13]=cx2 [14]=cy2 [15]=cz2
    // [16]=hitlag_mult [17]=sdi_mult [18]=setoff_kind [19]=lr_check
    // [20]=is_clang [21]=is_add_attack [22]=hitbox_attr [23]=ground_or_air
    // [24]=is_mtk [25]=is_shield_disable [26]=is_reflectable [27]=is_absorbable
    // [28]=is_landing_attack
    // [29]=situation_mask [30]=category_mask [31]=part_mask
    // [32]=no_finish_camera [33]=collision_attr [34]=sound_level
    // [35]=sound_attr [36]=attack_region
    //
    // Slots 13-15 are OPTIONAL IN THE SOURCE TEXT. The vanilla archive writes every
    // `ATTACK` with them (all 386 in the local corpus are 36 arguments) but writes
    // `ATTACK_IGNORE_THROW` without them (33 arguments), even though smash-script declares
    // both with the same 36 parameters. Reading the shorter form against the full table put
    // hitlag in the capsule, `*ATTACK_LR_CHECK_POS` in hitlag, and shifted every property
    // after it by three. So the capsule triple is detected from the ARGUMENTS rather than
    // from the macro name, and everything past the transform shifts when it is absent.
    let shift = if capsule_slots_present(&t) { 0 } else { 3 };

    let id: u32 = t[1].trim().parse().ok()?;
    let part: u32 = t[2].trim().parse().ok()?;
    let bone_name = extract_hash40_string(&t[3]).unwrap_or_else(|| t[3].trim().to_string());
    let damage: f32 = t[4].trim().parse().ok()?;
    let angle: i32 = t[5]
        .trim()
        .parse::<i32>()
        .or_else(|_| t[5].trim().parse::<f32>().map(|f| f as i32))
        .unwrap_or(0);
    let kb_scaling: i32 = t[6].trim().parse().ok()?;
    let fkb: i32 = t[7].trim().parse().ok()?;
    let kb_base: i32 = t[8].trim().parse().ok()?;
    let size: f32 = t[9].trim().parse().ok()?;
    let offset_x: f32 = t[10].trim().parse().ok()?;
    let offset_y: f32 = t[11].trim().parse().ok()?;
    let offset_z: f32 = t[12].trim().parse().ok()?;

    let capsule_end = if shift == 0 {
        match (
            parse_option_f32(t[13].trim()),
            parse_option_f32(t[14].trim()),
            parse_option_f32(t[15].trim()),
        ) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => None,
        }
    } else {
        None
    };

    // Slots up to the transform are fixed; past it, a call without the capsule triple has
    // everything three earlier.
    let get = |i: usize| {
        t.get(if i >= 16 { i - shift } else { i })
            .map(|s| s.trim())
            .unwrap_or("")
    };

    let hitlag_mult: f32 = get(16).parse().unwrap_or(1.0);
    let sdi_mult: f32 = get(17).parse().unwrap_or(1.0);
    // Scripts normally spell these as `*NAME`, but dumps of live-captured moves can carry the
    // bare number — decode both to the symbolic name so the property dropdowns match.
    use crate::param_labels as pl;
    let setoff_kind = pl::decode_const(pl::SETOFF_KIND, get(18));
    let lr_check = pl::decode_const(pl::LR_CHECK, get(19));
    let is_clang = get(20) == "true";
    let is_add_attack: i32 = get(21).parse().unwrap_or(0);
    let hitbox_attr: f32 = get(22).parse().unwrap_or(0.0);
    let ground_or_air: i32 = get(23).parse().unwrap_or(0);
    let is_mtk = get(24) == "true";
    let is_shield_disable = get(25) == "true";
    let is_reflectable = get(26) == "true";
    let is_absorbable = get(27) == "true";
    let is_landing_attack = get(28) == "true";
    let situation_mask = pl::decode_const(pl::SITUATION_MASK, get(29));
    let category_mask = pl::decode_const(pl::CATEGORY_MASK, get(30));
    let part_mask = pl::decode_const(pl::PART_MASK, get(31));
    let no_finish_camera = get(32) == "true";
    let collision_attr = extract_hash40_string(get(33)).unwrap_or_else(|| strip_deref(get(33)));
    let sound_level = pl::decode_const(pl::SOUND_LEVEL, get(34));
    let sound_attr = pl::decode_const(pl::SOUND_ATTR, get(35));
    let attack_region = pl::decode_const(pl::ATTACK_REGION, get(36));

    Some(AttackCall {
        func: func.to_string(),
        id,
        part,
        bone_name,
        damage,
        angle,
        kb_scaling,
        fkb,
        kb_base,
        size,
        offset_x,
        offset_y,
        offset_z,
        capsule_end,
        hitlag_mult,
        sdi_mult,
        setoff_kind,
        lr_check,
        is_clang,
        is_add_attack,
        hitbox_attr,
        ground_or_air,
        is_mtk,
        is_shield_disable,
        is_reflectable,
        is_absorbable,
        is_landing_attack,
        situation_mask,
        category_mask,
        part_mask,
        no_finish_camera,
        collision_attr,
        sound_level,
        sound_attr,
        attack_region,
    })
}

/// Parse `macros::CATCH(agent, id, bone, size, x, y, z, x2, y2, z2, status, situation)`.
///
/// `CATCH` is a fixed-arity function — the capsule endpoints are `Option<f32>`, so a
/// spherical grab spells them `None` rather than omitting them. (The vanilla script dumps
/// show a shorter form, but those are Lua; the smashline macro has one signature.)
fn parse_catch_call(line: &str) -> Option<crate::data::CatchCall> {
    let start = line.find("macros::CATCH(")?;
    let inner = &line[start + "macros::CATCH(".len()..];
    let end = inner.rfind(')')?;
    let t = tokenize_args(&inner[..end]);
    if t.len() < 7 {
        return None;
    }

    // [0]=agent [1]=id [2]=bone [3]=size [4]=x [5]=y [6]=z
    // [7]=x2 [8]=y2 [9]=z2 [10]=status [11]=situation — but the Lua-shaped dumps omit 7..=9
    // outright, which moves status and situation up to 7 and 8. See [`is_capsule_slot`].
    let num = |i: usize, default: f32| {
        t.get(i)
            .and_then(|value| value.trim().parse::<f32>().ok())
            .unwrap_or(default)
    };
    let has_capsule_slots = t.get(7).is_some_and(|v| is_capsule_slot(v));
    let capsule_end = match (
        t.get(7).and_then(|v| parse_option_f32(v.trim())),
        t.get(8).and_then(|v| parse_option_f32(v.trim())),
        t.get(9).and_then(|v| parse_option_f32(v.trim())),
    ) {
        (Some(x), Some(y), Some(z)) if has_capsule_slots => Some([x, y, z]),
        _ => None,
    };
    let konst = |i: usize, default: &str| {
        t.get(i)
            .map(|v| strip_deref(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let tail = if has_capsule_slots { 10 } else { 7 };
    Some(crate::data::CatchCall {
        id: t[1].trim().parse().ok()?,
        bone_name: extract_hash40_string(&t[2]).unwrap_or_else(|| t[2].trim().to_string()),
        size: num(3, 1.0),
        offset_x: num(4, 0.0),
        offset_y: num(5, 0.0),
        offset_z: num(6, 0.0),
        capsule_end,
        status: konst(tail, crate::data::CATCH_DEFAULT_STATUS),
        situation: konst(tail + 1, crate::data::CATCH_DEFAULT_SITUATION),
    })
}

fn parse_wind_call(line: &str) -> Option<crate::data::WindboxData> {
    for (command, arity) in crate::data::WIND_COMMANDS {
        let needle = format!("{command}(");
        let Some(start) = line.find(&needle) else {
            continue;
        };
        let inner = &line[start + needle.len()..];
        let end = inner.rfind(')')?;
        let tokens = tokenize_args(&inner[..end]);
        let values = if tokens.len() == arity + 1 {
            &tokens[1..]
        } else if tokens.len() == arity {
            &tokens[..]
        } else {
            continue;
        };
        let args: Option<Vec<f32>> = values
            .iter()
            .map(|value| value.trim().parse::<f32>().ok())
            .collect();
        return Some(crate::data::WindboxData {
            command: command.into(),
            args: args?,
        });
    }
    None
}

fn parse_erase_wind(line: &str) -> Option<u32> {
    let start = line.find("erase_wind(")? + "erase_wind(".len();
    let end = line[start..].rfind(')')? + start;
    tokenize_args(&line[start..end])
        .last()?
        .trim()
        .parse::<u32>()
        .ok()
}

/// Parse `macros::ATTACK_ABS(...)`, or `None` if the call is not the shape this family takes.
///
/// One arity across the whole corpus — 16 arguments in all 32 calls, matching the declaration
/// in `macros.rs` exactly — so, per this file's rule for families whose arguments carry no
/// distinguishing shape, a call of any other length is refused rather than reinterpreted.
///
/// Slot order, which is NOT `ATTACK`'s: kind, id, damage, angle, kbg, fkb, bkb, hitlag, unk,
/// lr_check, unk2, unk3, collision_attr, sound_level, sound_attr, attack_region.
fn parse_attack_abs_call(line: &str) -> Option<crate::data::AttackAbsCall> {
    const NEEDLE: &str = "macros::ATTACK_ABS(";
    let start = line.find(NEEDLE)? + NEEDLE.len();
    let end = line[start..].rfind(')')? + start;
    let args = tokenize_args(&line[start..end]);
    // Leading `agent`, then the sixteen.
    let a = args.get(1..)?;
    if a.len() != 16 {
        return None;
    }
    let int = |i: usize| a[i].trim().parse::<i32>().ok();
    Some(crate::data::AttackAbsCall {
        // Carried verbatim, minus the deref. The corpus has a fighter-specific kind
        // (`FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL`), so there is no closed set to map onto.
        kind: strip_deref(&a[0]),
        id: a[1].trim().parse::<u32>().ok()?,
        damage: a[2].trim().parse::<f32>().ok()?,
        angle: int(3)?,
        kb_scaling: int(4)?,
        fkb: int(5)?,
        kb_base: int(6)?,
        hitlag_mult: a[7].trim().parse::<f32>().ok()?,
        lr_check: strip_deref(&a[9]),
        collision_attr: extract_hash40_string(&a[12]).unwrap_or_else(|| a[12].trim().to_string()),
        sound_level: strip_deref(&a[13]),
        sound_attr: strip_deref(&a[14]),
        attack_region: strip_deref(&a[15]),
        unknowns: (
            a[8].trim().parse::<f32>().ok()?,
            a[10].trim().parse::<f32>().ok()?,
            a[11].trim().parse::<bool>().ok()?,
        ),
    })
}

/// Parse one hurtbox-state or colour-blend line, or `None` if this is not one.
///
/// Every member has exactly one arity in the vanilla archive — `HIT_NODE` 30 calls of
/// `(bone, status)`, `HIT_NO` 2 of `(group, status)`, `WHOLE_HIT` 6 of `(status)`, `COL_PRI` 2
/// of `(pri)`, and `COL_NORMAL` and `HIT_RESET_ALL` 8 and 3 of `()` — and each is declared with
/// that same arity in `smash-script`'s `macros.rs`. There is no shape to discriminate on, so per
/// this file's own rule the command name *is* the layout: a call whose argument count disagrees
/// with its name falls through to `Raw` rather than being reinterpreted.
fn parse_hurtbox_call(line: &str) -> Option<ExcuteStmt> {
    // `HIT_NODE` before `HIT_NO`: the shorter name is a prefix of the longer one, so testing
    // it first would read every `HIT_NODE` as a `HIT_NO` with an unparseable group.
    let args = |name: &str| -> Option<Vec<String>> {
        let needle = format!("macros::{name}(");
        let start = line.find(&needle)? + needle.len();
        let end = line[start..].rfind(')')? + start;
        let tokens = tokenize_args(&line[start..end]);
        // Drop the leading `agent`. A call written without one is not a form this parser has
        // ever seen and is left alone rather than guessed at.
        Some(tokens.get(1..)?.to_vec())
    };

    if line.contains("macros::HIT_NODE(") {
        let a = args("HIT_NODE")?;
        let [bone, status] = a.as_slice() else {
            return None;
        };
        return Some(ExcuteStmt::HitStatus {
            target: crate::data::HurtTarget::Bone(extract_hash40_string(bone)?),
            status: strip_deref(status),
        });
    }
    if line.contains("macros::HIT_NO(") {
        let a = args("HIT_NO")?;
        let [group, status] = a.as_slice() else {
            return None;
        };
        return Some(ExcuteStmt::HitStatus {
            target: crate::data::HurtTarget::Group(group.trim().parse::<i64>().ok()?),
            status: strip_deref(status),
        });
    }
    // `WHOLE_HIT` carries its status in the first slot, because its target is the macro name.
    // No prefix hazard: it shares no leading substring with the other four, and the `macros::`
    // prefix keeps it clear of `ATK_HIT_ABS` and the rest of B3's genuinely-hitbox family.
    if line.contains("macros::WHOLE_HIT(") {
        let a = args("WHOLE_HIT")?;
        let [status] = a.as_slice() else {
            return None;
        };
        return Some(ExcuteStmt::HitStatus {
            target: crate::data::HurtTarget::Whole,
            status: strip_deref(status),
        });
    }
    if line.contains("macros::COL_PRI(") {
        let a = args("COL_PRI")?;
        let [pri] = a.as_slice() else {
            return None;
        };
        return Some(ExcuteStmt::ColPri(pri.trim().parse::<i64>().ok()?));
    }
    // The two no-argument members. `args` returning an empty slice is the whole check: a
    // `COL_NORMAL` that somehow carries arguments is not this call.
    if line.contains("macros::COL_NORMAL(") && args("COL_NORMAL")?.is_empty() {
        return Some(ExcuteStmt::ColNormal);
    }
    if line.contains("macros::HIT_RESET_ALL(") && args("HIT_RESET_ALL")?.is_empty() {
        return Some(ExcuteStmt::HitResetAll);
    }
    None
}

/// Parse `ATK_POWER` / `ATK_SET_SHIELD_SETOFF_MUL`, or `None` if this is not one.
///
/// Both are `(agent, id: u64, value: ToF32)` in `smash-script`'s `macros.rs`, which is what
/// places them together: `lua_const` has no `MA_MSC_CMD_*` constant for either, and the corpus
/// alone could not have said which slot is which — all 9 `ATK_SET_SHIELD_SETOFF_MUL` calls are
/// the identical `(agent, 0, 7)`. `ATK_POWER`'s two calls then confirm it from the other side,
/// writing `(agent, 0, 10)` and `(agent, 1, 10)` for two hitboxes that share a value.
///
/// Kept out of [`parse_hurtbox_call`] because these are a different family that happens to be
/// parsed nearby, and a call written at the wrong arity falls through to `Raw` by the same rule.
fn parse_attack_mod_call(line: &str) -> Option<ExcuteStmt> {
    for kind in crate::data::AttackModKind::ALL {
        let needle = format!("macros::{}(", kind.macro_name());
        if !line.contains(&needle) {
            continue;
        }
        let start = line.find(&needle)? + needle.len();
        let end = line[start..].rfind(')')? + start;
        let tokens = tokenize_args(&line[start..end]);
        // Drop the leading `agent`, then require exactly the two arguments the signature has.
        let [id, value] = tokens.get(1..)? else {
            return None;
        };
        return Some(ExcuteStmt::AttackMod {
            kind,
            id: id.trim().parse::<i64>().ok()?,
            value: value.trim().parse::<f32>().ok()?,
        });
    }
    None
}

/// Render an `ATK_POWER` / `ATK_SET_SHIELD_SETOFF_MUL` value the way the vanilla scripts spell it.
///
/// Deliberately *not* [`num`], and the difference is not an oversight. `num` appends a `.0` to
/// whole numbers because most float arguments sit in slots declared `f32`, where a bare `6` is a
/// type error. These two slots are declared `ToF32` instead, which `smash-script` implements for
/// `i32`, so both spellings compile — and every one of the 11 vanilla calls writes the bare
/// integer (`7`, `10`). Emitting `7.0` over a `7` the user never touched would be diff noise in
/// an exported mod, so the integer form is kept when the value is whole.
pub(crate) fn attack_mod_num(value: f32) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        return format!("{}", value as i64);
    }
    num(value)
}

fn parse_attack_clear(line: &str) -> Option<u32> {
    let start = line.find("AttackModule::clear(")? + "AttackModule::clear(".len();
    let end = line[start..].find(')')? + start;
    tokenize_args(&line[start..end])
        .get(1)?
        .trim()
        .parse::<u32>()
        .ok()
}

/// Strip leading `*` dereference from constant names like `*ATTACK_SETOFF_KIND_ON`.
fn strip_deref(s: &str) -> String {
    s.trim_start_matches('*').to_string()
}

/// Write a hit status back the way the scripts write it.
///
/// A symbolic name needs the `*` deref it was parsed without; a bare number must not get one,
/// since `*2` does not compile. Deciding on the *text* rather than on whether the name is in
/// [`HIT_STATUS`](crate::param_labels::HIT_STATUS) is deliberate — a script using a constant
/// this build has never heard of still exports as the constant it wrote.
pub fn emit_status(status: &str) -> String {
    let s = status.trim();
    if s.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        s.to_string()
    } else {
        format!("*{s}")
    }
}

/// Parse `Some(3.0)` → `Some(3.0)`, `None` → `None`.
/// Does the token at the head of a collision's capsule slots actually hold a coordinate?
///
/// `CATCH` and `SEARCH` are both dumped in two shapes. The smashline signature is fixed-arity
/// and spells an absent capsule `None`, but the vanilla dumps this parser is calibrated against
/// come from Lua, where the three endpoint arguments are simply **not written at all** — so the
/// slot that holds `x2` in one call holds the next *constant* in another.
///
/// Reading it positionally is therefore wrong, and wrong in the quiet direction: the arguments
/// after the capsule get read one slot short, run off the end of the token list, and fall back
/// to defaults. That is how every short-form `CATCH` in the corpus lost its status kind.
///
/// A coordinate is a bare float, `Some(..)`, or `None`; anything else — `*COLLISION_KIND_MASK_*`,
/// `*FIGHTER_STATUS_KIND_*` — is the short form's next argument and means the capsule was
/// omitted. Deliberately not a token *count* test: a call with a trailing comment or a spliced
/// argument would change the count without changing which slot holds what.
pub(crate) fn is_capsule_slot(token: &str) -> bool {
    let token = token.trim();
    token == "None" || token.starts_with("Some(") || token.parse::<f32>().is_ok()
}

fn parse_option_f32(s: &str) -> Option<f32> {
    let s = s.trim();
    if s == "None" {
        return None;
    }
    let inner = s.strip_prefix("Some(")?.strip_suffix(')')?;
    inner.trim().parse().ok()
}

fn tokenize_args(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => {
                tokens.push(current.trim().to_string());
                current = String::new();
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.trim().is_empty() {
        tokens.push(current.trim().to_string());
    }
    tokens
}

fn extract_hash40_string(s: &str) -> Option<String> {
    let s = s.trim();
    let inner = s.strip_prefix("Hash40::new(\"")?.strip_suffix("\")")?;
    Some(inner.to_string())
}

// ── Source code export ────────────────────────────────────────────────────────

/// A generated file path + contents ready to write to disk.
pub struct GeneratedFile {
    /// Relative path within the project root, e.g. `"src/mario/acmd.rs"`
    pub rel_path: String,
    pub contents: String,
}

/// All files that make up one exported mod project.
pub struct ModProject {
    /// The root folder name, e.g. `"my_mod"`
    pub name: String,
    pub files: Vec<GeneratedFile>,
}

/// Emit a single `macros::ATTACK(...)` call as a source line.
/// A lua-const expression: named consts emit as `*NAME`, but live-captured hitboxes store
/// the RAW numeric value ("3") — emit those bare (a `*3` would not compile).
/// Spell a hitbox property the way an `ATTACK` argument spells it: a lua const gets its
/// leading `*`, a value that is already a number stays one. Shared with the source
/// write-back so both routes put the identical text in that slot.
/// A collision-attr name as the `Hash40` expression an export writes.
///
/// Live-captured hitboxes carry the attr as a raw hash rather than a name, because the game
/// only ever had the hash — so those emit as `Hash40::new_raw`, which is the one form that
/// reproduces the captured value exactly.
fn hash40_expr(attr: &str) -> String {
    match attr.strip_prefix("0x") {
        Some(hex) => format!("Hash40::new_raw(0x{hex})"),
        None => format!("Hash40::new(\"{attr}\")"),
    }
}

pub fn const_expr(s: &str) -> String {
    let t = s.trim();
    if t.parse::<i64>().is_ok() || t.parse::<f64>().is_ok() {
        t.to_string()
    } else {
        format!("*{t}")
    }
}

/// Render a float the way an ACMD argument has to be spelled.
///
/// Two requirements pull against each other here. The value must survive the round trip
/// exactly: the old `{:.1}` turned a vanilla `0.35` hitbox attribute into `0.3`, and a grab
/// box sitting at `-17.25` into `-17.2`, so exporting a move quietly changed it. And it must
/// stay a *float* literal, because several of these arguments are declared `f32` rather than
/// generic over `ToF32` — a bare `6` is a type error in a slot where `6.0` compiles.
///
/// So: the shortest text that reads back as the same `f32`, with a decimal point put back on
/// when the shortest form does not have one. A value that is not a number at all is spelled
/// as the constant rather than as `NaN`, which is not Rust; the verifier refuses the export
/// either way, and this keeps that one problem from also looking like a syntax error.
pub(crate) fn num(value: f32) -> String {
    if value.is_nan() {
        return "f32::NAN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_positive() {
            "f32::INFINITY"
        } else {
            "f32::NEG_INFINITY"
        }
        .to_string();
    }
    let text = value.to_string();
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

fn emit_attack(call: &AttackCall, indent: &str) -> String {
    // Skeleton files expose display-case names such as `FootR`, while ACMD hashes the
    // lowercase resource name (`footr`). Live injection follows the same contract.
    let bone = format!("Hash40::new(\"{}\")", call.bone_name.to_ascii_lowercase());
    let capsule = match call.capsule_end {
        Some([x, y, z]) => format!("Some({}), Some({}), Some({})", num(x), num(y), num(z)),
        None => "None, None, None".to_string(),
    };
    let collision_attr = hash40_expr(&call.collision_attr);
    // Always written with the capsule triple, even for a call parsed from the shorter
    // archive form: smash-script declares `x2`/`y2`/`z2` on every member of the family, so
    // the long form is the one that builds. Re-parsing it yields the same hitbox.
    format!(
        "{indent}macros::{func}(agent, {id}, {part}, {bone}, {dmg}, {angle}, {kbs}, {fkb}, {kbb}, \
{size}, {ox}, {oy}, {oz}, {capsule}, \
{hitlag}, {sdi}, {setoff}, {lr}, {clang}, {add_atk}, {hb_attr}, {goa}, \
{mtk}, {shield}, {reflect}, {absorb}, {landing}, \
{sit}, {cat}, {part_mask}, {no_cam}, {col_attr}, {snd_lvl}, {snd_attr}, {region});",
        indent = indent,
        func = call.func,
        id = call.id,
        part = call.part,
        bone = bone,
        dmg = num(call.damage),
        angle = call.angle,
        kbs = call.kb_scaling,
        fkb = call.fkb,
        kbb = call.kb_base,
        size = num(call.size),
        ox = num(call.offset_x),
        oy = num(call.offset_y),
        oz = num(call.offset_z),
        capsule = capsule,
        hitlag = num(call.hitlag_mult),
        sdi = num(call.sdi_mult),
        setoff = const_expr(&call.setoff_kind),
        lr = const_expr(&call.lr_check),
        clang = call.is_clang,
        add_atk = call.is_add_attack,
        hb_attr = num(call.hitbox_attr),
        goa = call.ground_or_air,
        mtk = call.is_mtk,
        shield = call.is_shield_disable,
        reflect = call.is_reflectable,
        absorb = call.is_absorbable,
        landing = call.is_landing_attack,
        sit = const_expr(&call.situation_mask),
        cat = const_expr(&call.category_mask),
        part_mask = const_expr(&call.part_mask),
        no_cam = call.no_finish_camera,
        col_attr = collision_attr,
        snd_lvl = const_expr(&call.sound_level),
        snd_attr = const_expr(&call.sound_attr),
        region = const_expr(&call.attack_region),
    )
}

/// Emit the `macros::CATCH` call for a grab box.
///
/// `CATCH` is a fixed-arity function whose capsule endpoints are `Option<f32>`, so a
/// spherical grab spells them `None` exactly the way `ATTACK` does. Grabs used to be
/// exported through [`emit_attack`], which wrote a zero-damage attack hitbox in place of the
/// user's grab box and stopped the move grabbing at all.
fn emit_catch(call: &crate::data::CatchCall, indent: &str) -> String {
    let bone = format!("Hash40::new(\"{}\")", call.bone_name.to_ascii_lowercase());
    let capsule = match call.capsule_end {
        Some([x, y, z]) => format!("Some({}), Some({}), Some({})", num(x), num(y), num(z)),
        None => "None, None, None".to_string(),
    };
    format!(
        "{indent}macros::CATCH(agent, {id}, {bone}, {size}, {x}, {y}, {z}, \
{capsule}, {status}, {situation});",
        id = call.id,
        size = num(call.size),
        x = num(call.offset_x),
        y = num(call.offset_y),
        z = num(call.offset_z),
        status = const_expr(&call.status),
        situation = const_expr(&call.situation),
    )
}

/// Emit `macros::ATTACK_ABS`, in its own slot order.
///
/// `damage` and `hitlag` go through `num` because the archive writes them with a decimal
/// point; the knockback triple and the angle are whole numbers there and are written bare.
/// Matching each family's own spelling is what keeps a re-exported vanilla script textually
/// identical to the one it came from.
fn emit_attack_abs(call: &crate::data::AttackAbsCall, indent: &str) -> String {
    let (unk, unk2, unk3) = call.unknowns;
    format!(
        "{indent}macros::ATTACK_ABS(agent, {kind}, {id}, {damage}, {angle}, {kbg}, {fkb}, \
{bkb}, {hitlag}, {unk}, {lr}, {unk2}, {unk3}, {attr}, {level}, {sound}, {region});",
        kind = const_expr(&call.kind),
        id = call.id,
        damage = num(call.damage),
        angle = call.angle,
        kbg = call.kb_scaling,
        fkb = call.fkb,
        bkb = call.kb_base,
        hitlag = num(call.hitlag_mult),
        unk = num(unk),
        lr = const_expr(&call.lr_check),
        unk2 = num(unk2),
        attr = hash40_expr(&call.collision_attr),
        level = const_expr(&call.sound_level),
        sound = const_expr(&call.sound_attr),
        region = const_expr(&call.attack_region),
    )
}

fn emit_excute_stmts(stmts: &[crate::data::ExcuteStmt], indent: &str) -> Vec<String> {
    stmts
        .iter()
        .map(|s| match s {
            crate::data::ExcuteStmt::Attack(call) => emit_attack(call, indent),
            crate::data::ExcuteStmt::Catch(call) => emit_catch(call, indent),
            crate::data::ExcuteStmt::AttackAbs(call) => emit_attack_abs(call, indent),
            crate::data::ExcuteStmt::GrabClearAll => {
                format!("{indent}GrabModule::clear_all(agent.module_accessor);")
            }
            crate::data::ExcuteStmt::Wind(wind) => format!(
                "{indent}macros::{}(agent, {});",
                wind.command,
                // Deliberately not `num`: a wind payload is kept verbatim rather than
                // recomposed from editable fields, and `f32::to_string` is already both exact
                // and shortest, so an untouched wind command replays byte for byte. Every
                // argument slot is generic over `ToF32`, so a whole number needs no `.0`.
                wind.args
                    .iter()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            crate::data::ExcuteStmt::EraseWind(id) => {
                format!("{indent}AreaModule::erase_wind(agent.module_accessor, {id});")
            }
            crate::data::ExcuteStmt::Clear(id) => {
                format!("{indent}AttackModule::clear(agent.module_accessor, {id}, false);")
            }
            crate::data::ExcuteStmt::ClearAll => {
                format!("{indent}AttackModule::clear_all(agent.module_accessor);")
            }
            // A status is re-emitted with the `*` the scripts write, because it is a lua const
            // deref rather than a Rust path. `emit_status` puts it back only where the parse
            // stripped one, so a call written with a bare number stays a bare number.
            crate::data::ExcuteStmt::HitStatus { target, status } => match target {
                crate::data::HurtTarget::Bone(bone) => format!(
                    "{indent}macros::HIT_NODE(agent, Hash40::new(\"{}\"), {});",
                    bone.to_ascii_lowercase(),
                    emit_status(status)
                ),
                crate::data::HurtTarget::Group(group) => format!(
                    "{indent}macros::HIT_NO(agent, {group}, {});",
                    emit_status(status)
                ),
                // The status moves into the first slot: this macro's target is its name.
                crate::data::HurtTarget::Whole => {
                    format!("{indent}macros::WHOLE_HIT(agent, {});", emit_status(status))
                }
            },
            crate::data::ExcuteStmt::HitResetAll => {
                format!("{indent}macros::HIT_RESET_ALL(agent);")
            }
            crate::data::ExcuteStmt::ColPri(pri) => {
                format!("{indent}macros::COL_PRI(agent, {pri});")
            }
            crate::data::ExcuteStmt::ColNormal => format!("{indent}macros::COL_NORMAL(agent);"),
            // Re-emitted as its own call at its own frame, never folded into the `ATTACK` it
            // retunes: the corpus writes it both in that call's own block and several frames
            // later, and only the separate line can express the second.
            crate::data::ExcuteStmt::AttackMod { kind, id, value } => format!(
                "{indent}macros::{}(agent, {id}, {});",
                kind.macro_name(),
                attack_mod_num(*value)
            ),
            crate::data::ExcuteStmt::Raw(line) => format!("{indent}{line}"),
        })
        .collect()
}

fn emit_stmts(stmts: &[crate::data::AcmdStmt], indent: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for stmt in stmts {
        match stmt {
            crate::data::AcmdStmt::Frame(f) => lines.push(format!(
                "{indent}frame(agent.lua_state_agent, {});",
                num(*f)
            )),
            crate::data::AcmdStmt::Wait(w) => {
                lines.push(format!("{indent}wait(agent.lua_state_agent, {});", num(*w)))
            }
            crate::data::AcmdStmt::WaitLoopClear => {
                lines.push(format!("{indent}wait_loop_clear(agent.lua_state_agent);"))
            }
            crate::data::AcmdStmt::Excute(inner) => {
                lines.push(format!("{indent}if macros::is_excute(agent) {{"));
                lines.extend(emit_excute_stmts(inner, &format!("{indent}    ")));
                lines.push(format!("{indent}}}"));
            }
            crate::data::AcmdStmt::Loop { count, body } => {
                lines.push(format!("{indent}for _ in 0..{count} {{"));
                lines.extend(emit_stmts(body, &format!("{indent}    ")));
                lines.push(format!("{indent}}}"));
            }
            crate::data::AcmdStmt::Raw(line) => lines.push(format!("{indent}{line}")),
        }
    }
    lines
}

/// The ACMD script name for a move: `attack_air_n` + `game` → `game_attackairn`.
///
/// This is the game's own naming, so it doubles as the key an existing smashline project
/// registers its scripts under — see `acmd_src`.
pub fn acmd_script_name(prefix: &str, move_name: &str) -> String {
    script_function_name(prefix, move_name)
}

/// Whether `name` is one of the effect spawn macros this module knows how to read.
pub fn is_effect_spawn_macro(name: &str) -> bool {
    effect_spawn_macro_layout(&format!("macros::{name}(")).is_some_and(|(m, _, _)| m == name)
}

/// Emit one `unsafe extern "C" fn` for a single move and return
/// `(fn_name, source_block)`.
fn script_function_name(prefix: &str, move_name: &str) -> String {
    let suffix: String = move_name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    format!(
        "{prefix}_{}",
        if suffix.is_empty() { "move" } else { &suffix }
    )
}

fn rust_module_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_');
    let mut out = if out.is_empty() {
        "fighter".to_string()
    } else {
        out.to_string()
    };
    const KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn",
    ];
    if out.as_bytes()[0].is_ascii_digit() || KEYWORDS.contains(&out.as_str()) {
        out.insert_str(0, "fighter_");
    }
    out
}

fn emit_move_fn(script: &crate::data::AcmdScript, move_name: &str) -> (String, String) {
    // Function name matches the ACMD script convention: game_{movename_no_underscores}
    let fn_name = script_function_name("game", move_name);
    let body = emit_stmts(&script.stmts, "    ");
    let mut out = String::new();
    out.push_str(&format!(
        "unsafe extern \"C\" fn {fn_name}(agent: &mut L2CAgentBase) {{\n"
    ));
    for line in &body {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("}\n");
    (fn_name, out)
}

/// hash40 of an effect name (or parse a hex "0x…" placeholder) — the id live tweaks are
/// keyed by. Mirrors app::effect_name_hash.
fn tweak_hash(name: &str) -> u64 {
    let n = name.trim();
    if let Some(hex) = n.strip_prefix("0x") {
        if let Ok(v) = u64::from_str_radix(hex, 16) {
            return v;
        }
    }
    hash40::hash40(&n.to_lowercase()).0
}

/// Render one graphic/joint argument.
///
/// A parsed name is usually the string inside a `Hash40::new("…")`, but the dumped scripts
/// also pass consts, locals, and raw hashes in these slots. Those come back out of the
/// parser as the expression text, and wrapping them in `Hash40::new("…")` again would emit
/// a graphic literally named `LOCAL_VARIABLE` — so pass anything expression-shaped through.
fn hash_arg(name: &str) -> String {
    let name = name.trim();
    if let Some(hex) = name.strip_prefix("0x") {
        if u64::from_str_radix(hex, 16).is_ok() {
            return format!("Hash40::new_raw({name})");
        }
    }
    if name.starts_with('*') || name.contains("::") || name.contains('(') || name.contains('"') {
        return name.to_string();
    }
    format!("Hash40::new(\"{name}\")")
}

/// Emit the spawn call for one effect, reproducing the macro the script actually used.
///
/// This used to collapse every spawn to `EFFECT` or `EFFECT_FOLLOW` based on `follows_bone`
/// alone, so exporting a move that used `EFFECT_FOLLOW_FLIP` (or any of the ~20 other
/// families) silently swapped it for a different macro with different behaviour and dropped
/// its second graphic.
///
/// `spawn_func` names the macro and `extra_args` holds its tail verbatim, so the original
/// comes back out intact. A call whose tail is unknown — one the user added from scratch, or
/// one loaded from a project saved before `extra_args` existed — cannot be reissued under its
/// own name without inventing arguments for a signature this code does not know, so it falls
/// back to the plain pair. That is the old, compilable behaviour. A tail that is known to be
/// *empty* is a different thing entirely and is reissued as-is.
fn emit_spawn_call(call: &crate::data::EffectCall, indent: &str) -> String {
    // A colour command is not a spawn at all: no graphic, no joint, no transform. Everything
    // below this point would write arguments it does not have.
    if let Some(color) = &call.color {
        return emit_color_call(&call.spawn_func, color, indent);
    }

    // Trail macros take textures and trail parameters, not a transform — there is nothing
    // to rebuild them from, so the source line rides along verbatim apart from the two
    // arguments the editor does expose.
    if let Some(raw) = &call.raw_line {
        return format!("{indent}{}\n", retarget_trail_line(raw, call));
    }

    // Skeleton files expose display-case joint names; ACMD hashes the lowercase one.
    let bone = hash_arg(&call.bone_name.to_ascii_lowercase());
    let graphic = hash_arg(&call.effect_name);
    let [x, y, z] = call.offset;
    // The macros take rotation as zr, yr, xr, not in [x, y, z] order.
    let [rx, ry, rz] = call.rotation;
    let transform = format!(
        "{}, {}, {}, {}, {}, {}, {}",
        num(x),
        num(y),
        num(z),
        num(rz),
        num(ry),
        num(rx),
        num(call.scale)
    );

    let tail = call
        .extra_args
        .as_ref()
        .filter(|_| !call.spawn_func.is_empty());
    let Some(tail) = tail else {
        // The fallback pair takes ONE graphic, so a flipped call's second one is dropped
        // here rather than shifting every following argument by a slot.
        return if call.follows_bone {
            format!("{indent}macros::EFFECT_FOLLOW(agent, {graphic}, {bone}, {transform}, true);\n")
        } else {
            // macros::EFFECT takes six extra random-range args (zeroed) before the flag.
            format!(
                "{indent}macros::EFFECT(agent, {graphic}, {bone}, {transform}, 0, 0, 0, 0, 0, 0, false);\n"
            )
        };
    };

    let graphics = match &call.effect_name_alt {
        Some(alt) => format!("{graphic}, {}", hash_arg(alt)),
        None => graphic,
    };
    let mut args = format!("agent, {graphics}, {bone}, {transform}");
    for extra in tail {
        args.push_str(", ");
        args.push_str(extra);
    }
    format!("{indent}macros::{}({args});\n", call.spawn_func)
}

/// Emit one `FLASH` / `BURN_COLOR` line.
///
/// The arguments the command does not take are not written, and the ones it does are written
/// with plain `to_string` rather than `num`: every slot in this family is generic over
/// `ToF32`, so `2` compiles and is what the archive and the source write-back both spell.
/// Using `num` here would put a decimal point on, and the two export paths would then disagree
/// about the text for the same value — the bug A1 fixed for wind and A3 for the effect rate.
///
/// A colour whose command takes none is dropped rather than appended, and a missing one for a
/// command that does take a colour is written as zeroes. Both cases mean the editor's state
/// and the command name have come apart, which `check_effect_values` reports; emitting the
/// wrong arity here would break the build instead of the check.
fn emit_color_call(command: &str, color: &crate::data::ColorCall, indent: &str) -> String {
    let (has_transition, has_rgba) = crate::data::color_command_layout(command)
        .unwrap_or((color.transition.is_some(), color.rgba.is_some()));
    let mut args = String::from("agent");
    if has_transition {
        args.push_str(&format!(", {}", color.transition.unwrap_or(0.0)));
    }
    if has_rgba {
        for component in color.rgba.unwrap_or([0.0; 4]) {
            args.push_str(&format!(", {component}"));
        }
    }
    format!("{indent}macros::{command}({args});\n")
}

/// Argument slots of an AFTER_IMAGE trail that the editor surfaces as editable fields: the
/// first texture stands in for the effect name, and argument 4 is the joint. Matches what
/// `parse_excute_block_effects` reads back out of the same call.
const TRAIL_GRAPHIC_SLOT: usize = 1;
const TRAIL_JOINT_SLOT: usize = 4;

/// Re-render the two trail arguments the panels let the user change.
///
/// The rest of a trail call is textures and per-frame trail parameters that no editor field
/// maps to, so it rides along untouched — but the graphic and joint ARE editable, and
/// replaying the line unconditionally dropped those edits with nothing to say so. Only slots
/// whose value actually differs are spliced, so an untouched trail comes back byte-identical
/// and a round trip through the emitter still reproduces the original line exactly.
fn retarget_trail_line(raw: &str, call: &crate::data::EffectCall) -> String {
    let Some(site) = crate::acmd_src::scan_macro_sites(raw, 0..raw.len())
        .into_iter()
        .find(|site| site.name.starts_with("AFTER_IMAGE"))
    else {
        return raw.to_string();
    };

    let mut out = raw.to_string();
    // Descending, so an earlier splice cannot shift a later slot's span out from under it.
    for (slot, wanted) in [
        (TRAIL_JOINT_SLOT, call.bone_name.to_ascii_lowercase()),
        (TRAIL_GRAPHIC_SLOT, call.effect_name.clone()),
    ] {
        let Some(span) = site.args.get(slot) else {
            continue;
        };
        let current = raw[span.clone()].trim();
        // Compare against what the PARSER would read from this slot, not the raw text, so
        // `Hash40::new("x")` and a bare `x` are not mistaken for a rename of each other.
        let parsed = extract_hash40_string(current).unwrap_or_else(|| current.to_string());
        if parsed.eq_ignore_ascii_case(&wanted) {
            continue;
        }
        out.replace_range(span.clone(), &hash_arg(&wanted));
    }
    out
}

/// Emit the call that ends `call`'s active window.
///
/// A trail is closed by `AFTER_IMAGE_OFF`, not by killing an effect kind — the name a trail
/// carries is a texture, and `EFFECT_OFF_KIND` on it terminates nothing.
fn emit_spawn_stop(call: &crate::data::EffectCall, indent: &str) -> String {
    if call.spawn_func == "AFTER_IMAGE_ON" {
        return format!("{indent}macros::AFTER_IMAGE_OFF(agent);\n");
    }
    format!(
        "{indent}macros::EFFECT_OFF_KIND(agent, {}, false, true);\n",
        hash_arg(&call.effect_name)
    )
}

/// Generate a smashline `effect_*` ACMD function that replays the (edited) effect-call
/// list: calls grouped by spawn frame, disabled calls omitted. `tweaks` (keyed by effect
/// hash) adds LAST_EFFECT_SET_COLOR / LAST_EFFECT_SET_RATE lines after matching spawns so
/// live color/speed multipliers ship with the mod.
fn emit_effect_move_fn(
    calls: &[crate::data::EffectCall],
    move_name: &str,
    tweaks: &std::collections::HashMap<u64, crate::mod_project::LiveTweak>,
) -> (String, String) {
    let fn_name = script_function_name("effect", move_name);
    // A follow effect's end frame is an ACMD event, not an intrinsic particle lifetime.
    // Keep starts and stops in one ordered timeline so a finite `active_end` actually emits
    // EFFECT_OFF_KIND. Stops for an older instance run before starts at the same frame; a
    // zero-duration call stops after its own start.
    let mut events: std::collections::BTreeMap<
        u32,
        (Vec<&crate::data::EffectCall>, Vec<&crate::data::EffectCall>),
    > = std::collections::BTreeMap::new();
    for call in calls.iter().filter(|call| !call.disabled) {
        events.entry(call.active_start).or_default().1.push(call);
        if call.follows_bone && call.active_end != 9999 {
            events
                .entry(call.active_end.max(call.active_start))
                .or_default()
                .0
                .push(call);
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "unsafe extern \"C\" fn {fn_name}(agent: &mut L2CAgentBase) {{\n"
    ));
    for (frame, (stops, starts)) in events {
        out.push_str(&format!("    frame(agent.lua_state_agent, {frame}.0);\n"));

        // Split the frame's spawns into the runs that can share one `is_excute` block. A run
        // ends where the guard changes or where a carried line has to come between two spawns,
        // because those lines arrive already wrapped in their own `if`/`is_excute` and cannot
        // be emitted inside somebody else's block.
        //
        // A frame with no guard and no carried lines produces exactly one run, which is the
        // single block this emitter has always written — the shape below is a superset of the
        // old one, not a replacement for it.
        let mut chunks: Vec<Chunk> = Vec::new();
        for call in &starts {
            if !call.leading.is_empty() {
                chunks.push(Chunk::Carried(&call.leading));
            }
            chunks.push(Chunk::Spawn(call));
            if !call.trailing.is_empty() {
                chunks.push(Chunk::Carried(&call.trailing));
            }
        }
        let mut runs: Vec<Run> = Vec::new();
        for chunk in chunks {
            match chunk {
                Chunk::Carried(lines) => runs.push(Run::Carried(lines)),
                Chunk::Spawn(call) => match runs.last_mut() {
                    Some(Run::Spawns { guard, calls }) if *guard == call.guard.as_deref() => {
                        calls.push(call)
                    }
                    _ => runs.push(Run::Spawns {
                        guard: call.guard.as_deref(),
                        calls: vec![call],
                    }),
                },
            }
        }
        // Stops still need a home when every spawn at this frame was disabled or retimed away.
        if !runs.iter().any(|r| matches!(r, Run::Spawns { .. })) {
            runs.push(Run::Spawns {
                guard: None,
                calls: Vec::new(),
            });
        }
        let first_spawn_run = runs
            .iter()
            .position(|r| matches!(r, Run::Spawns { .. }))
            .unwrap_or(0);
        let last_spawn_run = runs
            .iter()
            .rposition(|r| matches!(r, Run::Spawns { .. }))
            .unwrap_or(0);

        // Dedupe on the emitted line: `EFFECT_OFF_KIND` kills every live instance of a
        // kind at once, and `AFTER_IMAGE_OFF` takes no name to key on at all.
        let mut emitted_stops = std::collections::HashSet::new();

        for (index, run) in runs.iter().enumerate() {
            let (guard, run_calls) = match run {
                Run::Carried(lines) => {
                    push_carried(&mut out, lines, "    ");
                    continue;
                }
                Run::Spawns { guard, calls } => (*guard, calls),
            };
            // Deliberately not guarding the stops with it. `EFFECT_OFF_KIND` is
            // `EffectModule::kill_kind`, which is a no-op when nothing of that kind is live, so
            // an unguarded stop for a guarded spawn is harmless; a *guarded* stop would leave
            // the effect running forever on the branch that did not take the guard.
            let inner = if let Some(header) = guard {
                out.push_str(&format!("    {header}\n"));
                "        "
            } else {
                "    "
            };
            out.push_str(&format!("{inner}if macros::is_excute(agent) {{\n"));
            let body = format!("{inner}    ");

            if index == first_spawn_run {
                for call in stops
                    .iter()
                    .copied()
                    .filter(|call| call.active_start < frame)
                {
                    let stop = emit_spawn_stop(call, &body);
                    if emitted_stops.insert(stop.trim_start().to_string()) {
                        out.push_str(&stop);
                    }
                }
            }

            for call in run_calls.iter().copied() {
                out.push_str(&emit_spawn_call(call, &body));
                let tweak = tweaks.get(&tweak_hash(&call.effect_name));
                // One tint line, never two, on exactly the terms the rate below uses: a live colour
                // multiplier is a deliberate replacement of this kind's tint, so it wins over the
                // spawn's own `LAST_EFFECT_SET_COLOR`. Before this the script's line was not in
                // `EffectCall` at all, so there was nothing to reconcile with — the export wrote the
                // tweak's tint and silently deleted the script's, which is the loss C5 measured at
                // 33 occurrences and the reason this is the biggest single item in the family.
                //
                // The tweak's fourth component is deliberately ignored. It is the live form's alpha,
                // which no panel exposes and `live_tweak_from_override` does not test for identity,
                // so emitting it would ship an opacity the user never set.
                let tint = tweak
                    .and_then(|tw| tw.color)
                    .map(|[r, g, b, _a]| [r, g, b])
                    .or(call.tint);
                if let Some([r, g, b]) = tint {
                    // `num`, not the bare `to_string` the rate uses, because every one of the 65
                    // colour calls in the archive is written with a decimal point while its rates
                    // are whole numbers. Matching each macro's own spelling is what keeps a
                    // re-exported vanilla script textually identical to the one it came from.
                    out.push_str(&format!(
                        "{body}macros::LAST_EFFECT_SET_COLOR(agent, {}, {}, {});\n",
                        num(r),
                        num(g),
                        num(b)
                    ));
                }
                if let Some(alpha) = call.alpha {
                    // No tweak counterpart to reconcile with: the live override has no opacity
                    // control, so a spawn's alpha can only ever come from its own script line.
                    out.push_str(&format!(
                        "{body}macros::LAST_EFFECT_SET_ALPHA(agent, {});\n",
                        num(alpha)
                    ));
                }
                // One rate line, never two. A live speed tweak is a deliberate override of this
                // kind's playback rate, so it wins over the spawn's own — emitting both would
                // leave the second one winning anyway, and reading the pair back would attribute
                // the tweak's value to the script. Otherwise the spawn's rate is written, which
                // is what makes a vanilla `LAST_EFFECT_SET_RATE` survive an export at all: it is
                // read into the call and re-emitted here, rather than dropped on the floor.
                if let Some(rate) = tweak.and_then(|tw| tw.speed).or(call.rate) {
                    // Deliberately not `num`, which puts a decimal point back on: the rate slot is
                    // generic over `ToF32`, so `2` compiles and is what both the archive and the
                    // source write-back spell. Emitting `2.0` here would mean the two export paths
                    // wrote different text for the same value. `check_effect_values` refuses a
                    // non-finite rate, which is the one thing `to_string` cannot spell as Rust.
                    out.push_str(&format!(
                        "{body}macros::LAST_EFFECT_SET_RATE(agent, {rate});\n"
                    ));
                }
            }

            if index == last_spawn_run {
                for call in stops
                    .iter()
                    .copied()
                    .filter(|call| call.active_start >= frame)
                {
                    let stop = emit_spawn_stop(call, &body);
                    if emitted_stops.insert(stop.trim_start().to_string()) {
                        out.push_str(&stop);
                    }
                }
            }
            out.push_str(&format!("{inner}}}\n"));
            if guard.is_some() {
                out.push_str("    }\n");
            }
        }
    }
    out.push_str("}\n");
    (fn_name, out)
}

/// One piece of a frame block, before consecutive spawns are merged into shared `is_excute`s.
enum Chunk<'a> {
    Spawn(&'a crate::data::EffectCall),
    Carried(&'a [String]),
}

/// One emitted unit inside a frame block: either an `is_excute` block full of spawns that share
/// a guard, or a run of carried lines that arrive with their own wrapper already on them.
enum Run<'a> {
    Spawns {
        guard: Option<&'a str>,
        calls: Vec<&'a crate::data::EffectCall>,
    },
    Carried(&'a [String]),
}

/// Write carried lines, re-indenting them to sit under `base`.
///
/// The stored lines are unindented and brace-balanced as a group, so the nesting is recomputed
/// here rather than saved. Saving it would freeze one indentation into the project file and
/// leave a reloaded move mis-aligned the moment the surrounding block changed depth.
fn push_carried(out: &mut String, lines: &[String], base: &str) {
    let mut depth: i32 = 0;
    for line in lines {
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;
        // A line that closes more than it opens dedents itself before printing, so `}` lines
        // up with the header that opened it rather than with the body.
        let at = if closes > opens {
            depth - (closes - opens)
        } else {
            depth
        };
        out.push_str(base);
        for _ in 0..at.max(0) {
            out.push_str("    ");
        }
        out.push_str(line);
        out.push('\n');
        depth += opens - closes;
    }
}

// ── Export preview ────────────────────────────────────────────────────────────

/// The exact `game_*` function an export would write for this move.
///
/// Calls the same emitter [`build_mod_project_full`] does, so what the preview shows and
/// what lands in `acmd_source/` cannot drift apart.
pub fn preview_game_fn(script: &crate::data::AcmdScript, move_name: &str) -> String {
    emit_move_fn(script, move_name).1
}

/// The exact lines an export would write inside one `is_excute` block.
///
/// Only the verifier needs this: comparing what the emitter *produces* is the one way to tell
/// two statements apart that differ in the editor but collapse to the same call in the file.
pub fn preview_excute_stmts(stmts: &[crate::data::ExcuteStmt]) -> Vec<String> {
    emit_excute_stmts(stmts, "")
}

/// The exact `effect_*` function an export would write for this move.
pub fn preview_effect_fn(
    calls: &[crate::data::EffectCall],
    move_name: &str,
    live_tweaks: &[crate::mod_project::LiveTweak],
) -> String {
    let tweaks = live_tweaks
        .iter()
        .map(|t| (tweak_hash(&t.effect_name), t.clone()))
        .collect();
    emit_effect_move_fn(calls, move_name, &tweaks).1
}

/// Spawns that an export cannot reissue under the macro their script used.
///
/// Returns `(spawn function, effect name)` for each. This is the one way a generated script
/// still departs from the original, and it is a data problem rather than a code one: the
/// call's trailing arguments were never recorded, so there is no way to spell its signature.
/// Reloading the move from a script or a live capture fills them in.
pub fn export_spawn_downgrades(calls: &[crate::data::EffectCall]) -> Vec<(String, String)> {
    calls
        .iter()
        .filter(|call| {
            !call.disabled
                && call.raw_line.is_none()
                // A colour command has no tail to be missing — its arguments are its whole
                // content and are held as values, so it always re-emits under its own name.
                && call.color.is_none()
                && !call.spawn_func.is_empty()
                && call.extra_args.is_none()
                // The fallback emits exactly these two, so they are not a downgrade.
                && call.spawn_func != "EFFECT"
                && call.spawn_func != "EFFECT_FOLLOW"
        })
        .map(|call| (call.spawn_func.clone(), call.effect_name.clone()))
        .collect()
}

/// The lines of an effect script that an export would not write back.
///
/// [`emit_effect_move_fn`] rebuilds the whole function out of `EffectCall`s, so a line that
/// became no call and rode along on no call is not reproduced anywhere in the output. It is
/// deleted, and until C5 nothing anywhere said so.
///
/// C6 moved most of this list into the export rather than shortening the report artificially,
/// so what is named here is what is genuinely still lost. Three things reach it:
///
/// - **Statement-level `Raw`.** `wait_loop_sync_mot`, bare `EffectModule::` calls, script
///   plumbing. Deliberately never carried — see the `EffectStmt::Raw` arm of
///   `eval_effect_stmts` for why re-emitting a timing primitive would be worse than dropping it.
/// - **Residue with no call to ride on**, from `to_effect_calls_reporting_losses`. A carried
///   line attaches to a spawn in its own frame block; one whose frame has no spawn at all has
///   nowhere to go that would not also retime it.
/// - **A nested conditional's header.** One guard per spawn is modelled; an inner one would
///   have to overwrite the outer, so it is reported instead.
///
/// Lines carrying no letters or digits are left out. The emitter regenerates every brace it
/// needs, so a bare `}` is not a loss, and listing one per block would bury the lines that are.
pub fn unexportable_effect_lines(script: &crate::data::EffectScript) -> Vec<String> {
    fn keep(line: &str, out: &mut Vec<String>) {
        let line = line.trim();
        if line.chars().any(|c| c.is_alphanumeric()) {
            out.push(line.to_string());
        }
    }
    fn walk(stmts: &[EffectStmt], depth: usize, out: &mut Vec<String>) {
        for stmt in stmts {
            match stmt {
                EffectStmt::Raw(line) => keep(line, out),
                // Not reported from the tree. Whether an untyped macro survives depends on
                // whether a spawn shares its frame block, which only the resolving walk knows;
                // reporting from here would name lines the export does write.
                EffectStmt::Excute(_) => {}
                EffectStmt::Loop { body, .. } => walk(body, depth, out),
                EffectStmt::Cond { header, body } => {
                    if depth > 0 {
                        keep(header, out);
                    }
                    walk(body, depth + 1, out);
                }
                EffectStmt::Frame(_) | EffectStmt::Wait(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&script.stmts, 0, &mut out);
    out.extend(script.to_effect_calls_reporting_losses().1);
    out
}

/// Build a complete, compilable skyline-rs mod project for all the provided edits.
///
/// `edits` — list of `(fighter_name, move_name, script)` tuples (all fighters combined).
/// `plugin_name` — the Cargo package name, e.g. `"my_hitbox_mod"`.
pub fn build_mod_project(
    edits: &[(String, String, crate::data::AcmdScript)],
    plugin_name: &str,
) -> ModProject {
    build_mod_project_full(edits, &[], &[], plugin_name)
}

/// Like [`build_mod_project`] but also generates `effect_*` ACMD scripts.
///
/// `effect_edits` — `(fighter, move, full edited call list)`; the generated effect script
/// REPLACES the move's original effect script, so the list must be the complete set of
/// calls (pristine + user edits applied), not just the changed ones.
pub fn build_mod_project_full(
    edits: &[(String, String, crate::data::AcmdScript)],
    effect_edits: &[(String, String, Vec<crate::data::EffectCall>)],
    live_tweaks: &[crate::mod_project::LiveTweak],
    plugin_name: &str,
) -> ModProject {
    use std::collections::HashMap;

    let tweaks: HashMap<u64, crate::mod_project::LiveTweak> = live_tweaks
        .iter()
        .map(|t| (tweak_hash(&t.effect_name), t.clone()))
        .collect();

    // Group by fighter
    let mut by_fighter: HashMap<&str, Vec<(&str, &crate::data::AcmdScript)>> = HashMap::new();
    for (fighter, move_name, script) in edits {
        by_fighter
            .entry(fighter.as_str())
            .or_default()
            .push((move_name.as_str(), script));
    }
    let mut fx_by_fighter: HashMap<&str, Vec<(&str, &Vec<crate::data::EffectCall>)>> =
        HashMap::new();
    for (fighter, move_name, calls) in effect_edits {
        fx_by_fighter
            .entry(fighter.as_str())
            .or_default()
            .push((move_name.as_str(), calls));
        // Ensure the fighter appears even with no hitbox edits.
        by_fighter.entry(fighter.as_str()).or_default();
    }

    let mut files: Vec<GeneratedFile> = Vec::new();

    // ── rust-toolchain.toml ───────────────────────────────────────────────
    // cargo-skyline installs its own "skyline-v3" toolchain via update-std,
    // which bundles the correct stdlib and an older Cargo. Plain nightly is
    // correct here — cargo-skyline ignores this file and uses skyline-v3.
    files.push(GeneratedFile {
        rel_path: "rust-toolchain.toml".into(),
        contents: r#"[toolchain]
channel = "nightly"
"#
        .to_string(),
    });

    // ── Cargo.toml ────────────────────────────────────────────────────────
    files.push(GeneratedFile {
        rel_path: "Cargo.toml".into(),
        contents: format!(
r#"[package]
name = "{plugin_name}"
version = "0.1.0"
edition = "2018"

[package.metadata.skyline]
titleid = "01006A800016E000"

[lib]
crate-type = ["cdylib"]

[dependencies]
skyline = {{ git = "https://github.com/ultimate-research/skyline-rs" }}
skyline_smash = {{ git = "https://github.com/ultimate-research/skyline-smash", features = ["weak_l2cvalue"] }}
smash_script = {{ git = "https://github.com/WuBoytH/smash-script", rev = "24c5b69b79c7d7b041c3993ecd766ceccf950c2b" }}
smashline = {{ git = "https://github.com/hdr-development/smashline", rev = "df194a03b918116adb7bda1c2b4565dcd82a4756" }}

[profile.dev]
panic = "abort"

[profile.release]
opt-level = "z"
panic = "abort"
lto = true
codegen-units = 1
"#,
            plugin_name = plugin_name,
        ),
    });

    // ── src/lib.rs ────────────────────────────────────────────────────────
    let mut fighter_names: Vec<&str> = by_fighter.keys().copied().collect();
    fighter_names.sort();

    let mut used_modules: HashMap<String, usize> = HashMap::new();
    let fighter_modules: Vec<(&str, String)> = fighter_names
        .iter()
        .map(|fighter| {
            let base = rust_module_name(fighter);
            let count = used_modules.entry(base.clone()).or_default();
            *count += 1;
            let module = if *count == 1 {
                base
            } else {
                format!("{base}_{}", *count)
            };
            (*fighter, module)
        })
        .collect();
    let mod_decls: String = fighter_modules
        .iter()
        .map(|(_, module)| format!("mod {module};\n"))
        .collect();
    let installs: String = fighter_modules
        .iter()
        .map(|(_, module)| format!("    {module}::install();\n"))
        .collect();

    files.push(GeneratedFile {
        rel_path: "src/lib.rs".into(),
        contents: format!(
            r#"// Auto-generated by Visionary
#![feature(proc_macro_hygiene)]
#![allow(unused_macros, unused_imports)]

{mod_decls}
#[skyline::main(name = "{plugin_name}")]
pub fn main() {{
{installs}}}
"#,
            mod_decls = mod_decls,
            plugin_name = plugin_name,
            installs = installs,
        ),
    });

    // ── Per-fighter files ─────────────────────────────────────────────────
    for (fighter, module) in &fighter_modules {
        let moves = &by_fighter[fighter];

        // src/{fighter}/mod.rs
        files.push(GeneratedFile {
            rel_path: format!("src/{module}/mod.rs"),
            contents: format!(
                r#"mod acmd;

pub fn install() {{
    let agent = &mut smashline::Agent::new("{fighter}");
    acmd::install(agent);
    agent.install();
}}
"#,
                fighter = fighter,
            ),
        });

        // src/{fighter}/acmd.rs — all moves for this fighter in one file
        let mut acmd_src = String::new();
        acmd_src.push_str("use {\n");
        acmd_src.push_str("    smash::{\n");
        acmd_src.push_str("        lua2cpp::*,\n");
        acmd_src.push_str("        phx::*,\n");
        acmd_src.push_str("        app::{sv_animcmd::*, lua_bind::*},\n");
        acmd_src.push_str("        lib::lua_const::*\n");
        acmd_src.push_str("    },\n");
        acmd_src.push_str("    smashline::*,\n");
        acmd_src.push_str("    smash_script::*\n");
        acmd_src.push_str("};\n\n");

        let mut sorted_moves = moves.clone();
        sorted_moves.sort_by_key(|(m, _)| *m);

        // (fn_name, acmd_script_name) pairs for the install block
        let mut fn_entries: Vec<(String, String)> = Vec::new();

        for (move_name, script) in &sorted_moves {
            let (fn_name, fn_src) = emit_move_fn(script, move_name);
            // The acmd script name used in agent.acmd() is "game_{movename_no_underscores}"
            let acmd_name = script_function_name("game", move_name);
            acmd_src.push_str(&fn_src);
            acmd_src.push('\n');
            fn_entries.push((fn_name, acmd_name));
        }

        // Effect scripts (edited spawn lists) for this fighter
        if let Some(fx_moves) = fx_by_fighter.get(fighter) {
            let mut sorted_fx = fx_moves.clone();
            sorted_fx.sort_by_key(|(m, _)| *m);
            for (move_name, calls) in &sorted_fx {
                let (fn_name, fn_src) = emit_effect_move_fn(calls, move_name, &tweaks);
                acmd_src.push_str(&fn_src);
                acmd_src.push('\n');
                fn_entries.push((fn_name.clone(), fn_name));
            }
        }

        // install fn
        acmd_src.push_str("pub fn install(agent: &mut smashline::Agent) {\n");
        for (fn_name, acmd_name) in &fn_entries {
            acmd_src.push_str(&format!(
                "    agent.acmd(\"{acmd_name}\", {fn_name}, smashline::Priority::Default);\n"
            ));
        }
        acmd_src.push_str("}\n");

        files.push(GeneratedFile {
            rel_path: format!("src/{module}/acmd.rs"),
            contents: acmd_src,
        });
    }

    // ── README.md ─────────────────────────────────────────────────────────
    let mut script_list: Vec<String> = edits
        .iter()
        .map(|(fighter, move_name, _)| format!("- {fighter}: {move_name} (hitboxes)"))
        .chain(
            effect_edits
                .iter()
                .map(|(fighter, move_name, _)| format!("- {fighter}: {move_name} (effect spawns)")),
        )
        .collect();
    script_list.sort();
    let move_list = script_list.join("\n");

    files.push(GeneratedFile {
        rel_path: "README.md".into(),
        contents: format!(
r#"# {plugin_name}

Skyline ACMD mod for Super Smash Bros. Ultimate.

## Edited scripts

{move_list}

## Building

Run the included build script for your platform — it handles everything automatically.

Windows:

```bat
build.bat
```

Linux and macOS:

```sh
bash build.sh
```

The compiled plugin will be at:
```
target/aarch64-skyline-switch/release/lib{plugin_name}.nro
```

## Installing on your Switch

Rename the compiled `.nro` to `plugin.nro` and place it at the root of your
ARCropolis mod folder:
```
ultimate/mods/<mod folder>/plugin.nro
```

### Required plugins (if not already installed)
Make sure the base Skyline/ARCropolis installation already provides these
runtime components. They are not copied into this mod:
- [Skyline](https://github.com/skyline-dev/skyline/releases) — copy the `exefs/` folder to `atmosphere/contents/01006A800016E000/`
- [nro_hook](https://github.com/ultimate-research/nro-hook-plugin/releases) — `libnro_hook.nro`
- [Smashline](https://github.com/HDR-Development/smashline/releases) — `libsmashline_plugin.nro`
"#,
            plugin_name = plugin_name,
            move_list = move_list,
        ),
    });

    // ── info.toml (ARCropolis mod metadata) ───────────────────────────────
    files.push(GeneratedFile {
        rel_path: "info.toml".into(),
        contents: format!(
            r#"display_name = "{plugin_name}"
authors = "Visionary"
version = "1.0"
description = """
ACMD mod generated by Visionary.
Edited scripts:
{move_list}
"""
category = "Fighter"
"#,
            plugin_name = plugin_name,
            move_list = move_list,
        ),
    });

    // ── build.sh (Linux/macOS) ────────────────────────────────────────────
    files.push(GeneratedFile {
        rel_path: "build.sh".into(),
        contents: r#"#!/usr/bin/env bash
set -e

# ── 1. Install rustup if missing ─────────────────────────────────────────────
if ! command -v rustup &>/dev/null; then
    echo "Installing rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly
    source "$HOME/.cargo/env"
fi

# ── 2. Ensure nightly is installed ───────────────────────────────────────────
rustup toolchain install nightly

# ── 3. Install cargo-skyline if missing ──────────────────────────────────────
if ! cargo skyline --version &>/dev/null 2>&1; then
    echo "Installing cargo-skyline..."
    cargo install cargo-skyline
fi

# ── 4. Install the skyline-v3 toolchain + custom stdlib ──────────────────────
# This is required by cargo-skyline to cross-compile for the Switch.
# It only needs to run once; subsequent builds skip it automatically.
echo "Setting up skyline-v3 toolchain (this may take a few minutes on first run)..."
cargo skyline update-std

# ── 5. Build ─────────────────────────────────────────────────────────────────
echo "Building..."
cargo skyline build --release

echo ""
echo "Done! Your plugin is at:"
echo "  target/aarch64-skyline-switch/release/$(basename "$PWD" | tr '-' '_' | sed 's/^lib//')*.nro"
echo ""
echo "Rename it to plugin.nro and place it at:"
echo "  ultimate/mods/<mod folder>/plugin.nro"
"#.to_string(),
    });

    // ── build.bat (Windows) ───────────────────────────────────────────────
    files.push(GeneratedFile {
        rel_path: "build.bat".into(),
        contents: r#"@echo off
setlocal

:: ── 1. Check for rustup ──────────────────────────────────────────────────────
where rustup >nul 2>&1
if %errorlevel% neq 0 (
    echo rustup not found.
    echo Please install Rust from https://rustup.rs then re-run this script.
    pause
    exit /b 1
)

:: ── 2. Ensure nightly is installed ───────────────────────────────────────────
rustup toolchain install nightly

:: ── 3. Install cargo-skyline if missing ──────────────────────────────────────
cargo skyline --version >nul 2>&1
if %errorlevel% neq 0 (
    echo Installing cargo-skyline...
    cargo install cargo-skyline
)

:: ── 4. Install the skyline-v3 toolchain + custom stdlib ──────────────────────
echo Setting up skyline-v3 toolchain (first run may take a few minutes)...
cargo skyline update-std

:: ── 5. Build ─────────────────────────────────────────────────────────────────
echo Building...
cargo skyline build --release

echo.
echo Done! Your plugin is in target\aarch64-skyline-switch\release\
echo Rename the .nro to plugin.nro and place it at:
echo   ultimate\mods\^<mod folder^>\plugin.nro
pause
"#
        .to_string(),
    });

    ModProject {
        name: plugin_name.to_string(),
        files,
    }
}

/// Convenience: export a single move as a standalone project.
/// Returns the `src/{fighter}/acmd.rs` content only — use `build_mod_project` for a full project.
#[allow(dead_code)]
pub fn export_acmd_source(
    script: &crate::data::AcmdScript,
    fighter: &str,
    move_name: &str,
) -> String {
    let edits = vec![(fighter.to_string(), move_name.to_string(), script.clone())];
    let project = build_mod_project(&edits, &format!("{fighter}_{move_name}_mod"));
    // Return all files joined with separators for single-file save (legacy path)
    project
        .files
        .iter()
        .map(|f| format!("// === {} ===\n{}", f.rel_path, f.contents))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AttackModule::clear_all` used to end a hitbox the frame *before* it started when the
    /// script had no `frame()` call ahead of it, so the timeline and the viewport drew nothing
    /// at all for kirby's jab. The id-scoped clear already clamped; this one did not.
    #[test]
    fn a_collision_cleared_on_the_next_frame_is_out_for_that_frame() {
        // kirby/Attack100Sub: a hitbox with no `frame()` before it, cleared one frame later.
        let source = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 0.2, 361, 35, 0, 7, 5.5, 0.0, 5.5, 15.0, None, None, None, 0.5, 0.4, *ATTACK_SETOFF_KIND_OFF, *ATTACK_LR_CHECK_F, false, 0, 0.0, 0, false, false, false, false, true, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_rush"), *ATTACK_SOUND_LEVEL_S, *COLLISION_SOUND_ATTR_PUNCH, *ATTACK_REGION_PUNCH);
    }
    wait(agent.lua_state_agent, 1.0);
    if macros::is_excute(agent) {
        AttackModule::clear_all(agent.module_accessor);
    }
}
"#;
        let hitboxes = parse_acmd_script(source).to_hitboxes();
        assert_eq!(
            (hitboxes[0].active_start, hitboxes[0].active_end),
            (1, 1),
            "the jab hitbox is out on frame 1"
        );
    }

    /// kirby/ThrowF, verbatim — two `ATTACK_ABS` in one block sharing id 0 and differing only
    /// by kind, which is the case that makes id-based identity wrong for this family.
    const THROW_ABS: &str = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::ATTACK_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, 0, 5.0, 75, 125, 0, 40, 0.0, 1.0, *ATTACK_LR_CHECK_F, 0.0, true, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_S, *COLLISION_SOUND_ATTR_NONE, *ATTACK_REGION_THROW);
        macros::ATTACK_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH, 0, 3.0, 361, 100, 0, 60, 0.0, 1.0, *ATTACK_LR_CHECK_F, 0.0, true, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_S, *COLLISION_SOUND_ATTR_NONE, *ATTACK_REGION_THROW);
    }
}
"#;

    /// Two calls in one block with the same id are two separate rows, because the *kind* is
    /// what tells them apart. Keying on id — which every corpus call writes as 0 — would end
    /// the first the instant the second was read, and a throw would lose its throw half.
    #[test]
    fn two_absolute_hits_sharing_an_id_are_told_apart_by_their_kind() {
        let hitboxes = parse_acmd_script(THROW_ABS).to_hitboxes();
        let [throw, catch] = &hitboxes[..] else {
            panic!("expected two rows, got {}", hitboxes.len());
        };
        assert_eq!(throw.category, crate::data::CAT_ABS);
        assert_eq!(
            throw.abs.as_ref().unwrap().kind,
            "FIGHTER_ATTACK_ABSOLUTE_KIND_THROW"
        );
        assert_eq!(
            catch.abs.as_ref().unwrap().kind,
            "FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH"
        );
        // Neither ended the other.
        assert_eq!((throw.active_end, catch.active_end), (9999, 9999));
        // Slot order is not ATTACK's; these are the values that shift if it were read as one.
        assert_eq!((throw.damage, throw.angle), (5.0, 75));
        assert_eq!((throw.kb_scaling, throw.fkb, throw.kb_base), (125, 0, 40));
        assert_eq!(catch.damage, 3.0);
        // No volume anywhere in the call — the panel and viewport read these as "not
        // applicable", so they must not come back as a plausible-looking hitbox at the origin.
        assert_eq!((throw.size, throw.bone_name.as_str()), (0.0, ""));
    }

    #[test]
    fn the_absolute_family_round_trips_through_the_emitter() {
        let script = parse_acmd_script(THROW_ABS);
        let exported = export_acmd_source(&script, "kirby", "throw_f");
        for line in [
            "*FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, 0, 5.0, 75, 125, 0, 40, 0.0, 1.0, \
             *ATTACK_LR_CHECK_F, 0.0, true,",
            "*FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH, 0, 3.0, 361, 100, 0, 60, 0.0, 1.0,",
        ] {
            assert!(exported.contains(line), "missing `{line}` in\n{exported}");
        }
        assert_eq!(
            parse_acmd_script(&exported).to_hitboxes().len(),
            2,
            "the export must read back as the same two rows"
        );
    }

    /// The kind slot takes fighter-specific constants — Terry's final smash has its own — so a
    /// closed table would rewrite it into someone else's throw. Carried verbatim instead.
    #[test]
    fn a_fighter_specific_absolute_kind_survives_the_round_trip() {
        let source = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::ATTACK_ABS(agent, *FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL, 0, 12.0, 361, 100, 0, 30, 0.5, 1.0, *ATTACK_LR_CHECK_F, 0.0, true, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_L, *COLLISION_SOUND_ATTR_DOLLY_CRITICAL, *ATTACK_REGION_NONE);
    }
}
"#;
        let script = parse_acmd_script(source);
        assert_eq!(
            script.to_hitboxes()[0].abs.as_ref().unwrap().kind,
            "FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL"
        );
        assert!(
            export_acmd_source(&script, "dolly", "final_air_start")
                .contains("*FIGHTER_DOLLY_ATTACK_ABSOLUTE_KIND_FINAL"),
            "a fighter-specific kind must not be normalised away"
        );
    }

    /// dolly/SpecialAirHiCommand, verbatim and trimmed to the hurtbox lines — the knee and leg
    /// intangibility on Terry's up special, which is the canonical use of this family.
    const HURT_INTANGIBLE: &str = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);
        macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_XLU);
    }
    frame(agent.lua_state_agent, 20.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_NORMAL);
        macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_NORMAL);
    }
}
"#;

    /// A hurtbox state is two independent lines in the script and one span on screen. Getting
    /// the join wrong is the whole difference between "the knee is intangible frames 9-19" and
    /// a pair of rows a modder has to diff by eye.
    #[test]
    fn a_hurtbox_state_spans_from_its_call_to_the_one_that_takes_it_back() {
        let (states, _) = parse_acmd_script(HURT_INTANGIBLE).to_hurtboxes();
        let knee: Vec<_> = states
            .iter()
            .filter(|s| s.target == crate::data::HurtTarget::Bone("kneer".into()))
            .collect();
        let [xlu, normal] = &knee[..] else {
            panic!("expected two states for kneer, got {}", knee.len());
        };
        assert_eq!(xlu.status, "HIT_STATUS_XLU");
        assert_eq!(
            (xlu.active_start, xlu.active_end),
            (9, 19),
            "intangible from its own frame until the frame before it is taken back"
        );
        // The closing call is a span too, and runs to the end of the move: nothing takes it
        // back, and inventing an end for it would claim the bone stops being normal.
        assert_eq!(normal.status, "HIT_STATUS_NORMAL");
        assert_eq!((normal.active_start, normal.active_end), (20, 9999));
    }

    /// The round-trip the definition of done asks for: real vanilla text in, export, parse the
    /// export, and the same spans come back. `*HIT_STATUS_XLU` in particular has to survive as
    /// a symbol — writing the `2` it stands for compiles and stops matching the archive.
    #[test]
    fn the_hurtbox_family_round_trips_through_the_emitter() {
        let script = parse_acmd_script(HURT_INTANGIBLE);
        let exported = export_acmd_source(&script, "dolly", "special_air_hi_command");
        assert!(
            exported.contains(r#"macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);"#),
            "{exported}"
        );
        assert_eq!(
            parse_acmd_script(&exported).to_hurtboxes(),
            script.to_hurtboxes(),
            "the export must resolve to the same spans as the source"
        );
    }

    /// kirby/FinalStart and kirby/ThrownHi, trimmed to the hurtbox lines. Kirby's final smash
    /// makes the whole body translucent for the cinematic; `ThrownHi` puts it back.
    ///
    /// The two are separate functions in the corpus and are spliced here because no vanilla
    /// script contains both a set and a reset of the whole body — which is worth knowing, and is
    /// why [`HurtTarget::Whole`](crate::data::HurtTarget::Whole) does not model the reach of the
    /// call across the per-bone targets.
    const HURT_WHOLE_BODY: &str = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 1.0);
    if macros::is_excute(agent) {
        macros::WHOLE_HIT(agent, *HIT_STATUS_XLU);
    }
    frame(agent.lua_state_agent, 46.0);
    if macros::is_excute(agent) {
        macros::WHOLE_HIT(agent, *HIT_STATUS_NORMAL);
    }
}
"#;

    /// `WHOLE_HIT` is a hurtbox state, not a hitbox one, and it is typed rather than carried.
    ///
    /// The corpus oracles cannot tell these apart on their own: an unparsed line survives an
    /// export as `Raw`, byte for byte, so a `WHOLE_HIT` that this parser did not recognise would
    /// round-trip just as cleanly as one it did. The assertion that matters is therefore that
    /// spans come out at all — that is what puts the call on the timeline and in the panel.
    #[test]
    fn a_whole_body_hit_status_is_a_hurtbox_span_and_not_a_raw_line() {
        let script = parse_acmd_script(HURT_WHOLE_BODY);
        let (states, pris) = script.to_hurtboxes();
        assert!(pris.is_empty(), "nothing here is a colour blend");
        let [xlu, normal] = &states[..] else {
            panic!("expected two whole-body states, got {}", states.len());
        };
        assert_eq!(xlu.target, crate::data::HurtTarget::Whole);
        assert_eq!(xlu.status, "HIT_STATUS_XLU");
        // Ends where the reset begins, exactly like a per-bone span: `Whole` is a third target
        // and not a special case in the span walk.
        assert_eq!((xlu.active_start, xlu.active_end), (1, 45));
        assert_eq!(normal.target, crate::data::HurtTarget::Whole);
        assert_eq!(normal.status, "HIT_STATUS_NORMAL");
        assert_eq!((normal.active_start, normal.active_end), (46, 9999));
    }

    /// The status moves to the first slot because the target is the macro name. An emitter that
    /// kept the two-argument layout would write `WHOLE_HIT(agent, Hash40::new(…), status)`,
    /// which does not compile — but only against a fighter nobody had exported yet.
    #[test]
    fn a_whole_body_status_is_emitted_into_the_slot_the_target_vacated() {
        let script = parse_acmd_script(HURT_WHOLE_BODY);
        let exported = export_acmd_source(&script, "kirby", "final_start");
        assert!(
            exported.contains("macros::WHOLE_HIT(agent, *HIT_STATUS_XLU);"),
            "{exported}"
        );
        assert!(
            !exported.contains("WHOLE_HIT(agent, Hash40"),
            "the target slot must not come back: {exported}"
        );
        assert_eq!(
            parse_acmd_script(&exported).to_hurtboxes(),
            script.to_hurtboxes(),
            "the export must resolve to the same spans as the source"
        );
    }

    /// `WHOLE_HIT` must not be read as one of the targeted pair, whose status sits a slot later.
    ///
    /// Written the wrong way round, `HIT_NODE`'s parse would take `*HIT_STATUS_XLU` for a bone
    /// and find no status at all. The arity check is what separates them, so this pins that a
    /// call with the *other* family's argument count falls through to `Raw` rather than being
    /// reinterpreted — the rule this function's doc comment states.
    #[test]
    fn a_whole_hit_written_with_the_targeted_arity_is_left_alone() {
        let two_args =
            r#"        macros::WHOLE_HIT(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);"#;
        assert!(
            parse_hurtbox_call(two_args).is_none(),
            "a WHOLE_HIT with a target slot is not a form this parser has seen"
        );
        let one_arg = r#"        macros::WHOLE_HIT(agent, *HIT_STATUS_XLU);"#;
        assert!(matches!(
            parse_hurtbox_call(one_arg),
            Some(crate::data::ExcuteStmt::HitStatus {
                target: crate::data::HurtTarget::Whole,
                ..
            })
        ));
    }

    /// kirby/AttackLw4 and kirby/Attack100Sub, spliced: the two placements the corpus uses.
    ///
    /// `ATK_SET_SHIELD_SETOFF_MUL` sits in the same `is_excute` block as the `ATTACK` it
    /// retunes, and `ATK_POWER` lands five frames later on hitboxes that are already out. Both
    /// have to survive as their own calls at their own frames, which is the whole reason these
    /// are separate statements rather than fields folded into the parent `ATTACK`.
    const ATTACK_MODS: &str = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::ATK_SET_SHIELD_SETOFF_MUL(agent, 0, 7);
    }
    frame(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        macros::ATK_POWER(agent, 0, 10);
        macros::ATK_POWER(agent, 1, 10);
    }
}
"#;

    /// The modifiers are typed, at their own frames, against the ids they name.
    ///
    /// Same caveat as the `WHOLE_HIT` test above: an unmodelled line exports verbatim as `Raw`
    /// and round-trips perfectly, so the corpus oracles were green on these before the parse arm
    /// existed. What has teeth is that they resolve at all — that is what reaches the panel.
    #[test]
    fn attack_modifiers_resolve_against_the_hitbox_id_they_name() {
        use crate::data::AttackModKind;
        let mods = parse_acmd_script(ATTACK_MODS).to_attack_mods();
        let [setoff, power_0, power_1] = &mods[..] else {
            panic!("expected three modifiers, got {}", mods.len());
        };
        assert_eq!(setoff.kind, AttackModKind::ShieldSetoffMul);
        assert_eq!((setoff.id, setoff.value, setoff.frame), (0, 7.0, 1));
        // Five frames after the ATTACK they retune, and telling the two ids apart is the whole
        // point of reading slot 0 as the id: both calls carry the same value.
        assert_eq!(power_0.kind, AttackModKind::Power);
        assert_eq!((power_0.id, power_0.value, power_0.frame), (0, 10.0, 10));
        assert_eq!((power_1.id, power_1.value, power_1.frame), (1, 10.0, 10));
        // Own numbering space: these must not consume hurtbox sites, or every hurtbox edit in a
        // script that also tunes a hitbox would land on the wrong line.
        assert_eq!([setoff.site, power_0.site, power_1.site], [0, 1, 2]);
    }

    /// A modifier is not a collision, so it must not appear as one.
    #[test]
    fn an_attack_modifier_does_not_become_a_hitbox_of_its_own() {
        assert!(parse_acmd_script(ATTACK_MODS).to_hitboxes().is_empty());
    }

    /// The value keeps the spelling the vanilla scripts use.
    ///
    /// All 11 corpus calls write a bare integer. These slots are `ToF32`-generic, so `7` and
    /// `7.0` both compile — but exporting `7.0` over a `7` the user never touched is diff noise,
    /// and `num` would do exactly that. A fractional value still has to keep its point.
    #[test]
    fn a_whole_modifier_value_is_emitted_as_the_integer_the_corpus_writes() {
        let script = parse_acmd_script(ATTACK_MODS);
        let exported = export_acmd_source(&script, "kirby", "attack_lw4");
        assert!(
            exported.contains("macros::ATK_SET_SHIELD_SETOFF_MUL(agent, 0, 7);"),
            "{exported}"
        );
        assert!(
            exported.contains("macros::ATK_POWER(agent, 0, 10);"),
            "{exported}"
        );
        // Scoped to the modifier lines: the surrounding `frame(agent.lua_state_agent, 10.0);`
        // is a slot declared `f32`, where `num`'s decimal point is exactly right.
        let emitted: Vec<&str> = exported
            .lines()
            .filter(|l| l.contains("ATK_POWER(") || l.contains("ATK_SET_SHIELD_SETOFF_MUL("))
            .collect();
        assert!(
            emitted.iter().all(|l| !l.contains('.')),
            "a whole value must not gain a decimal point: {emitted:?}"
        );
        assert_eq!(attack_mod_num(2.5), "2.5", "a fraction keeps its point");
        assert_eq!(
            parse_acmd_script(&exported).to_attack_mods(),
            script.to_attack_mods(),
            "the export must resolve to the same modifiers as the source"
        );
    }

    /// The id slot is not the value slot, and a wrong-arity call is left alone.
    ///
    /// The corpus could not have settled the first: all 9 `ATK_SET_SHIELD_SETOFF_MUL` calls are
    /// the identical `(agent, 0, 7)`. `macros.rs` declares `id: u64, val: ToF32`, so an
    /// asymmetric call is the test that would fail if the slots were ever swapped.
    #[test]
    fn the_id_slot_is_read_as_the_id_and_a_wrong_arity_call_falls_through() {
        use crate::data::{AttackModKind, ExcuteStmt};
        let asymmetric = "        macros::ATK_POWER(agent, 3, 12);";
        assert!(matches!(
            parse_attack_mod_call(asymmetric),
            Some(ExcuteStmt::AttackMod {
                kind: AttackModKind::Power,
                id: 3,
                value,
            }) if value == 12.0
        ));
        // One argument short: the signature has two, so this is not a form the parser has seen
        // and it stays `Raw` rather than being reinterpreted.
        assert!(parse_attack_mod_call("        macros::ATK_POWER(agent, 3);").is_none());
        // `ATK_HIT_ABS` takes no id and passes local variables. It must not be dragged in here
        // by its shared prefix — see TODO.md B3 for why it is carried verbatim instead.
        let hit_abs = r#"        macros::ATK_HIT_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, Hash40::new("throw"), target, target_group, target_no);"#;
        assert!(parse_attack_mod_call(hit_abs).is_none());
    }

    /// The other three members, which carry no bone: two that reset and one that takes a
    /// number. `HIT_NO`'s group must not be read as a bone hash, and `COL_PRI` must not be
    /// ended by `HIT_RESET_ALL` — they are different resets of different things.
    #[test]
    fn the_argument_less_members_and_the_numbered_ones_keep_their_own_families() {
        let source = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::HIT_NO(agent, 8, *HIT_STATUS_OFF);
        macros::COL_PRI(agent, 200);
    }
    frame(agent.lua_state_agent, 12.0);
    if macros::is_excute(agent) {
        macros::HIT_RESET_ALL(agent);
    }
    frame(agent.lua_state_agent, 16.0);
    if macros::is_excute(agent) {
        macros::COL_NORMAL(agent);
    }
}
"#;
        let script = parse_acmd_script(source);
        let (states, pris) = script.to_hurtboxes();
        let [group] = &states[..] else {
            panic!("expected one hurtbox state, got {}", states.len());
        };
        assert_eq!(group.target, crate::data::HurtTarget::Group(8));
        assert_eq!(group.status, "HIT_STATUS_OFF");
        assert_eq!(
            (group.active_start, group.active_end),
            (4, 11),
            "HIT_RESET_ALL ends it"
        );

        let [pri] = &pris[..] else {
            panic!("expected one priority span, got {}", pris.len());
        };
        assert_eq!(pri.pri, 200);
        assert_eq!(
            (pri.active_start, pri.active_end),
            (4, 15),
            "COL_NORMAL ends the priority, and HIT_RESET_ALL at frame 12 does not"
        );

        let exported = export_acmd_source(&script, "kirby", "guard_off");
        for line in [
            "macros::HIT_NO(agent, 8, *HIT_STATUS_OFF);",
            "macros::COL_PRI(agent, 200);",
            "macros::HIT_RESET_ALL(agent);",
            "macros::COL_NORMAL(agent);",
        ] {
            assert!(exported.contains(line), "missing {line} in {exported}");
        }
    }

    /// A call whose argument count disagrees with its name is not a variant to interpret.
    /// Every member has exactly one arity in the corpus and one in `macros.rs`, so a mismatch
    /// means this parser is looking at something it does not understand — and `Raw` is how a
    /// line survives an export unread.
    #[test]
    fn a_hurtbox_call_of_the_wrong_arity_is_left_alone_rather_than_padded() {
        for line in [
            "macros::HIT_NODE(agent, Hash40::new(\"legr\"));",
            "macros::COL_PRI(agent);",
            "macros::HIT_RESET_ALL(agent, 3);",
        ] {
            let source = format!(
                "unsafe extern \"C\" fn game_test(agent: &mut L2CAgentBase) {{\n    \
                 if macros::is_excute(agent) {{\n        {line}\n    }}\n}}\n"
            );
            let script = parse_acmd_script(&source);
            let (states, pris) = script.to_hurtboxes();
            assert!(
                states.is_empty() && pris.is_empty(),
                "{line} should not have parsed into a span"
            );
            // …and it must still come back out, which is what `Raw` buys.
            assert!(
                export_acmd_source(&script, "kirby", "attack_11").contains(line),
                "{line} was dropped by the export"
            );
        }
    }

    /// kirby/ThrowHi, verbatim — the only `ATTACK_IGNORE_THROW` in the vanilla corpus, and
    /// the reason the capsule triple is detected rather than assumed.
    ///
    /// The archive writes this macro with 33 arguments where every `ATTACK` has 36: the
    /// `x2`/`y2`/`z2` options are simply absent. Read against `ATTACK`'s table it parses,
    /// and parses WRONG — hitlag lands in the capsule, `*ATTACK_LR_CHECK_POS` in hitlag,
    /// and every property after that is off by three. Nothing about it looks broken.
    const IGNORE_THROW: &str = r#"macros::ATTACK_IGNORE_THROW(agent, 0, 0, Hash40::new("top"), 7.0, 65, 95, 0, 85, 9.5, 0.0, 6.5, 2.0, 1.0, 1.0, *ATTACK_SETOFF_KIND_OFF, *ATTACK_LR_CHECK_POS, false, 0, 0.0, 0, false, false, false, false, true, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_M, *COLLISION_SOUND_ATTR_KICK, *ATTACK_REGION_BODY);"#;

    #[test]
    fn a_capsule_less_attack_call_reads_every_slot_after_the_transform() {
        let source = format!(
            "unsafe extern \"C\" fn game_test(agent: &mut L2CAgentBase) {{\n    \
             frame(agent.lua_state_agent, 12.0);\n    if macros::is_excute(agent) {{\n        \
             {IGNORE_THROW}\n    }}\n}}\n"
        );
        let hitboxes = parse_acmd_script(&source).to_hitboxes();
        let [hb] = &hitboxes[..] else {
            panic!("expected one hitbox, got {}", hitboxes.len());
        };

        assert_eq!(hb.func, "ATTACK_IGNORE_THROW");
        assert_eq!(hb.capsule_end, None, "this call has no capsule triple");
        // The transform is ahead of the optional arguments and cannot shift.
        assert_eq!((hb.damage, hb.angle, hb.size), (7.0, 65, 9.5));
        assert_eq!((hb.offset_x, hb.offset_y, hb.offset_z), (0.0, 6.5, 2.0));
        // Everything below here is what the shift used to corrupt.
        assert_eq!(hb.hitlag_mult, 1.0, "hitlag must not read the capsule slot");
        assert_eq!(hb.sdi_mult, 1.0);
        assert_eq!(hb.setoff_kind, "ATTACK_SETOFF_KIND_OFF");
        assert_eq!(hb.lr_check, "ATTACK_LR_CHECK_POS");
        assert!(!hb.is_clang);
        assert_eq!(hb.hitbox_attr, 0.0);
        assert!(hb.is_landing_attack);
        assert_eq!(hb.situation_mask, "COLLISION_SITUATION_MASK_GA");
        assert_eq!(hb.collision_attr, "collision_attr_normal");
        assert_eq!(hb.sound_level, "ATTACK_SOUND_LEVEL_M");
        assert_eq!(hb.sound_attr, "COLLISION_SOUND_ATTR_KICK");
        assert_eq!(hb.attack_region, "ATTACK_REGION_BODY");
    }

    /// An export must name the family member it read. Emitting one as the other builds fine
    /// and silently changes whether the hitbox reaches a fighter already being thrown.
    #[test]
    fn an_attack_family_call_is_exported_under_its_own_macro() {
        let source = format!(
            "unsafe extern \"C\" fn game_test(agent: &mut L2CAgentBase) {{\n    \
             frame(agent.lua_state_agent, 12.0);\n    if macros::is_excute(agent) {{\n        \
             {IGNORE_THROW}\n    }}\n}}\n"
        );
        let parsed = parse_acmd_script(&source);
        let emitted = preview_game_fn(&parsed, "throwhi");
        assert!(
            emitted.contains("macros::ATTACK_IGNORE_THROW(agent, 0, 0,"),
            "the export dropped the family member:\n{emitted}"
        );
        // Written back out in the long form, because that is the one smash-script declares
        // and therefore the only one that builds.
        assert!(
            emitted.contains("2.0, None, None, None, 1.0, 1.0,"),
            "the export must supply the capsule options the source omitted:\n{emitted}"
        );
        // The long form re-reads as the same hitbox: the shift is a source-text detail, not
        // a property of the move.
        assert_eq!(
            parse_acmd_script(&emitted).to_hitboxes(),
            parsed.to_hitboxes(),
            "a capsule-less call must survive a round trip through the emitter"
        );
    }

    #[test]
    fn wind_commands_round_trip_with_exact_shapes_and_independent_erases() {
        let source = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::AREA_WIND_2ND_arg10(agent, 3, 1, 80, 300, 0.8, 4, 12, 24, 16, 50);
    }
    frame(agent.lua_state_agent, 7.0);
    if macros::is_excute(agent) {
        macros::AREA_WIND_2ND_RAD(agent, 4, 0.5, 0.02, 1000, 1, -2, 6, 18);
    }
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        AttackModule::clear_all(agent.module_accessor);
        AreaModule::erase_wind(agent.module_accessor, 4);
    }
    frame(agent.lua_state_agent, 12.0);
    if macros::is_excute(agent) {
        AreaModule::erase_wind(agent.module_accessor, 3);
    }
}
"#;
        let script = parse_acmd_script(source);
        let hitboxes = script.to_hitboxes();
        assert_eq!(hitboxes.len(), 2);

        let rect = &hitboxes[0];
        let rect_wind = rect.wind.as_ref().unwrap();
        assert_eq!(rect.id, 3);
        assert_eq!(rect_wind.command, "AREA_WIND_2ND_arg10");
        assert_eq!(rect_wind.offset(), [4.0, 12.0]);
        assert_eq!(rect_wind.dimensions(), [24.0, 16.0]);
        assert_eq!((rect.active_start, rect.active_end), (5, 11));

        let radial = &hitboxes[1];
        let radial_wind = radial.wind.as_ref().unwrap();
        assert_eq!(radial.id, 4);
        assert_eq!(radial_wind.command, "AREA_WIND_2ND_RAD");
        assert_eq!(radial_wind.offset(), [-2.0, 6.0]);
        assert_eq!(radial_wind.radius(), 18.0);
        assert_eq!((radial.active_start, radial.active_end), (7, 8));

        let emitted = export_acmd_source(&script, "mario", "test");
        assert!(emitted.contains(
            "macros::AREA_WIND_2ND_arg10(agent, 3, 1, 80, 300, 0.8, 4, 12, 24, 16, 50);"
        ));
        assert!(
            emitted.contains("macros::AREA_WIND_2ND_RAD(agent, 4, 0.5, 0.02, 1000, 1, -2, 6, 18);")
        );
        assert!(emitted.contains("AreaModule::erase_wind(agent.module_accessor, 4);"));
    }

    /// `macros::CATCH` was never parsed, so a grab box in a script — including one Visionary
    /// had just exported — was invisible to the editor and could not be edited at all.
    #[test]
    fn catch_calls_round_trip_through_the_editor() {
        let src = r#"unsafe extern "C" fn game_catch(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::CATCH(agent, 0, Hash40::new("top"), 5.5, 0.0, 6.4, 10.2, Some(1.0), Some(2.0), Some(3.0), *FIGHTER_STATUS_KIND_SWALLOWED, *COLLISION_SITUATION_MASK_A);
    }
    frame(agent.lua_state_agent, 13.0);
    if macros::is_excute(agent) {
        GrabModule::clear_all(agent.module_accessor);
    }
}
"#;
        let script = parse_acmd_script(src);
        let boxes = script.to_hitboxes();
        assert_eq!(boxes.len(), 1, "{boxes:#?}");
        let grab = &boxes[0];
        assert_eq!(grab.category, 1, "a CATCH is a grab box, not an attack");
        assert_eq!((grab.active_start, grab.active_end), (8, 12));
        assert_eq!(grab.size, 5.5);
        assert_eq!(grab.capsule_end, Some([1.0, 2.0, 3.0]));
        // Grabs deal no damage — the attack-only fields must not inherit ATTACK's defaults.
        assert_eq!(grab.damage, 0.0);

        // The author's own status and situation survive, rather than being replaced by the
        // stand-ins used for a grab that never came from a script.
        let exported = preview_game_fn(&script, "catch");
        assert!(
            exported.contains("*FIGHTER_STATUS_KIND_SWALLOWED"),
            "{exported}"
        );
        assert!(
            exported.contains("*COLLISION_SITUATION_MASK_A"),
            "{exported}"
        );
        assert!(exported.contains("GrabModule::clear_all"), "{exported}");
        assert!(!exported.contains("AttackModule::clear_all"), "{exported}");

        // And the whole thing round-trips: re-parsing the export gives the same grab back.
        assert_eq!(
            parse_acmd_script(&exported).to_hitboxes(),
            boxes,
            "{exported}"
        );
    }

    /// A grab written without the capsule slots keeps its own status and situation.
    ///
    /// The vanilla dumps come from Lua, which omits the three capsule arguments rather than
    /// spelling them `None`, so `status` and `situation` sit at slots 7 and 8 instead of 10 and
    /// 11. Reading them positionally ran off the end of the token list and silently substituted
    /// the defaults — which turned all four of the corpus's short-form calls, every one of them
    /// Kirby's inhale, from a swallow into an ordinary grab.
    ///
    /// The round-trip oracle cannot catch this class of bug on its own: it compares the export
    /// against the *parsed* model, and a value lost on the way in agrees with itself on the way
    /// out. So this asserts against the original text, not against a re-parse.
    #[test]
    fn a_grab_written_without_capsule_slots_keeps_its_status() {
        // Verbatim from kirby/SpecialNStart.
        let src = r#"unsafe extern "C" fn game_catch(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::CATCH(agent, 0, Hash40::new("top"), 6.0, 0.0, 6.0, 5.0, *FIGHTER_STATUS_KIND_SWALLOWED, *COLLISION_SITUATION_MASK_GA);
    }
}
"#;
        let script = parse_acmd_script(src);
        let boxes = script.to_hitboxes();
        assert_eq!(boxes.len(), 1, "{boxes:#?}");
        let grab = &boxes[0];
        let extras = grab.catch.as_ref().expect("a CATCH carries its extras");
        assert_eq!(extras.status, "FIGHTER_STATUS_KIND_SWALLOWED");
        assert_eq!(extras.situation, "COLLISION_SITUATION_MASK_GA");

        // The geometry is still read from the slots it really occupies, and the absent capsule
        // is absent rather than being assembled out of the constants that follow it.
        assert_eq!(grab.size, 6.0);
        assert_eq!(
            (grab.offset_x, grab.offset_y, grab.offset_z),
            (0.0, 6.0, 5.0)
        );
        assert_eq!(grab.capsule_end, None);

        let exported = preview_game_fn(&script, "catch");
        assert!(
            exported.contains("*FIGHTER_STATUS_KIND_SWALLOWED"),
            "{exported}"
        );
    }

    /// An attack clear does not close a grab box, and a grab clear does not close an attack.
    #[test]
    fn attack_and_grab_clears_do_not_close_each_other() {
        let src = r#"unsafe extern "C" fn game_catch(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 10.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, 0, 1, false, 0, 0.0, 0, false, false, true, true, false, 0, 0, 0, false, Hash40::new("collision_attr_normal"), 0, 0, 0);
        macros::CATCH(agent, 0, Hash40::new("top"), 5.5, 0.0, 6.4, 10.2, None, None, None, 0, 0);
    }
    frame(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        AttackModule::clear_all(agent.module_accessor);
    }
    frame(agent.lua_state_agent, 20.0);
    if macros::is_excute(agent) {
        GrabModule::clear_all(agent.module_accessor);
    }
}
"#;
        let boxes = parse_acmd_script(src).to_hitboxes();
        let attack = boxes.iter().find(|h| h.category == 0).expect("an attack");
        let grab = boxes.iter().find(|h| h.category == 1).expect("a grab");
        assert_eq!(
            attack.active_end, 9,
            "AttackModule::clear_all ends the attack"
        );
        assert_eq!(
            grab.active_end, 19,
            "and leaves the grab running to its own clear"
        );
    }

    #[test]
    fn id_scoped_attack_clear_round_trips() {
        let source = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        AttackModule::clear(agent.module_accessor, 3, false);
    }
}
"#;
        let script = parse_acmd_script(source);
        assert!(matches!(
            script.stmts.as_slice(),
            [crate::data::AcmdStmt::Frame(frame), crate::data::AcmdStmt::Excute(inner)]
                if *frame == 9.0 && matches!(inner.as_slice(), [crate::data::ExcuteStmt::Clear(3)])
        ));
        let emitted = export_acmd_source(&script, "mario", "test");
        assert!(emitted.contains("AttackModule::clear(agent.module_accessor, 3, false);"));
    }

    // ═══ Generated-source compile golden ════════════════════════════════════

    /// The inputs `sample_project` is built from, so a test can check the preview against
    /// the very same edits the export consumed.
    type SampleEdits = (
        crate::data::AcmdScript,
        Vec<crate::data::EffectCall>,
        Vec<crate::mod_project::LiveTweak>,
    );

    fn sample_project() -> ModProject {
        let (script, fx, tweaks) = sample_edits();
        build_mod_project_full(
            &[("mario".into(), "attack_air_n".into(), script)],
            &[("mario".into(), "attack_air_n".into(), fx)],
            &tweaks,
            "sample_plugin",
        )
    }

    fn sample_edits() -> SampleEdits {
        let atk = crate::data::AttackCall {
            func: "ATTACK".into(),
            id: 0,
            part: 0,
            bone_name: "Top".into(),
            damage: 8.0,
            angle: 361,
            kb_scaling: 100,
            fkb: 0,
            kb_base: 40,
            size: 4.0,
            offset_x: 0.0,
            offset_y: 8.0,
            offset_z: 6.0,
            capsule_end: Some([0.0, 8.0, -2.0]),
            hitlag_mult: 1.0,
            sdi_mult: 1.0,
            setoff_kind: "ATTACK_SETOFF_KIND_ON".into(),
            lr_check: "ATTACK_LR_CHECK_POS".into(),
            is_clang: true,
            is_add_attack: 1,
            hitbox_attr: 0.0,
            ground_or_air: 0,
            is_mtk: false,
            is_shield_disable: false,
            is_reflectable: false,
            is_absorbable: false,
            is_landing_attack: false,
            situation_mask: "COLLISION_SITUATION_MASK_GA".into(),
            category_mask: "COLLISION_CATEGORY_MASK_ALL".into(),
            part_mask: "COLLISION_PART_MASK_ALL".into(),
            no_finish_camera: false,
            collision_attr: "collision_attr_normal".into(),
            sound_level: "ATTACK_SOUND_LEVEL_S".into(),
            sound_attr: "COLLISION_SOUND_ATTR_KICK".into(),
            attack_region: "ATTACK_REGION_KICK".into(),
        };
        let script = crate::data::AcmdScript {
            stmts: vec![
                crate::data::AcmdStmt::Frame(3.0),
                crate::data::AcmdStmt::Excute(vec![crate::data::ExcuteStmt::Attack(atk)]),
                crate::data::AcmdStmt::Wait(2.0),
                crate::data::AcmdStmt::Excute(vec![crate::data::ExcuteStmt::ClearAll]),
            ],
        };
        let fx = vec![
            crate::data::EffectCall {
                effect_name: "sys_attack_arc".into(),
                effect_name_alt: None,
                spawn_func: "EFFECT".into(),
                bone_name: "Top".into(),
                offset: [0.0, 8.0, 0.0],
                rotation: [0.0, 90.0, 0.0],
                scale: 1.2,
                follows_bone: false,
                active_start: 3,
                active_end: 3,
                disabled: false,
                extra_args: Some(
                    vec!["0".into(); 6]
                        .into_iter()
                        .chain(["false".into()])
                        .collect(),
                ),
                raw_line: None,
                rate: None,
                tint: None,
                alpha: None,
                color: None,
                guard: None,
                leading: Vec::new(),
                trailing: Vec::new(),
            },
            crate::data::EffectCall {
                effect_name: "sys_flash".into(),
                effect_name_alt: None,
                spawn_func: "EFFECT_FOLLOW".into(),
                bone_name: "HaveR".into(),
                offset: [0.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0],
                scale: 0.8,
                follows_bone: true,
                active_start: 5,
                active_end: 20,
                disabled: false,
                extra_args: Some(vec!["true".into()]),
                raw_line: None,
                rate: None,
                tint: None,
                alpha: None,
                color: None,
                guard: None,
                leading: Vec::new(),
                trailing: Vec::new(),
            },
        ];
        let tweaks = vec![crate::mod_project::LiveTweak {
            effect_name: "sys_attack_arc".into(),
            color: Some([0.2, 0.4, 2.0, 1.0]),
            speed: Some(1.5),
        }];
        (script, fx, tweaks)
    }

    /// Always materializes the generated project under the editor cache dir; with
    /// VISIONARY_COMPILE_GOLDEN=1 it additionally runs cargo-skyline on it (slow, network)
    /// to prove the codegen actually builds.
    #[test]
    fn generated_source_project_compiles() {
        let project = sample_project();
        assert!(project.files.iter().any(|f| f.rel_path == "Cargo.toml"));
        assert!(project.files.iter().any(|f| f.rel_path == "src/lib.rs"));
        assert!(project
            .files
            .iter()
            .any(|f| f.rel_path == "src/mario/acmd.rs"));
        let generated_acmd = project
            .files
            .iter()
            .find(|f| f.rel_path == "src/mario/acmd.rs")
            .map(|f| f.contents.as_str())
            .expect("generated fighter ACMD");
        assert!(
            generated_acmd.contains("Hash40::new(\"top\")")
                && generated_acmd.contains("Hash40::new(\"haver\")")
                && !generated_acmd.contains("Hash40::new(\"Top\")")
                && !generated_acmd.contains("Hash40::new(\"HaveR\")"),
            "display-case skeleton names must export as lowercase ACMD Hash40 names"
        );
        assert!(
            generated_acmd.contains("frame(agent.lua_state_agent, 20.0);")
                && generated_acmd.contains(
                    "macros::EFFECT_OFF_KIND(agent, Hash40::new(\"sys_flash\"), false, true);"
                ),
            "a finite follow-effect end frame must emit EFFECT_OFF_KIND"
        );
        assert!(
            !generated_acmd
                .contains("macros::EFFECT_OFF_KIND(agent, Hash40::new(\"sys_attack_arc\")"),
            "one-shot effects must retain their intrinsic lifetime"
        );

        let root = crate::scratch_dirs::app_storage_root().join("source-golden");
        let proj_root = root.join(&project.name);
        for f in &project.files {
            let dest = proj_root.join(&f.rel_path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(&dest, &f.contents).unwrap();
        }
        if std::env::var("VISIONARY_COMPILE_GOLDEN").is_err() {
            return;
        }
        let out = std::process::Command::new("cargo")
            .args(["skyline", "build", "--release"])
            .current_dir(&proj_root)
            .output()
            .expect("cargo-skyline not runnable");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "generated project failed to build in {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            proj_root.display(),
            &stdout[stdout.len().saturating_sub(2000)..],
            &stderr[stderr.len().saturating_sub(6000)..],
        );
    }

    // ── Helper: wrap a body line in a minimal effect function ─────────────────
    fn wrap_effect_fn(body: &str) -> String {
        format!("unsafe extern \"C\" fn effect_test(agent: &mut L2CAgentBase) {{\n    {body}\n}}\n")
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 1: Bug condition exploration tests
    // These tests MUST FAIL on unfixed code — failure confirms the bug exists.
    // They will PASS after the fix in task 3 is applied.
    // ═══════════════════════════════════════════════════════════════════════

    /// Property 1: Bug Condition — bare EFFECT macro produces EffectCall
    /// CRITICAL: MUST FAIL on unfixed code (bare EFFECT is treated as Raw and discarded).
    #[test]
    fn test_bug_condition_bare_effect_produces_effect_call() {
        let src = wrap_effect_fn(
            r#"macros::EFFECT(agent, Hash40::new("test_effect"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);"#,
        );
        let script = parse_effect_script(&src);
        let calls = script.to_effect_calls();
        assert!(
            !calls.is_empty(),
            "bare EFFECT should produce at least one EffectCall, got 0"
        );
        assert_eq!(
            calls[0].effect_name, "test_effect",
            "effect_name should be 'test_effect', got '{}'",
            calls[0].effect_name
        );
    }

    /// Property 1b: bare EFFECT_FOLLOW produces EffectCall with follows_bone=true
    #[test]
    fn test_bug_condition_bare_effect_follow_follows_bone() {
        let src = wrap_effect_fn(
            r#"macros::EFFECT_FOLLOW(agent, Hash40::new("follow_eff"), Hash40::new("hip"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);"#,
        );
        let script = parse_effect_script(&src);
        let calls = script.to_effect_calls();
        assert!(
            !calls.is_empty(),
            "bare EFFECT_FOLLOW should produce EffectCall"
        );
        assert!(
            calls[0].follows_bone,
            "EFFECT_FOLLOW should set follows_bone=true"
        );
        assert_eq!(calls[0].effect_name, "follow_eff");
    }

    #[test]
    fn dumped_effect_variants_and_kill_kind_are_fully_timed() {
        let src = wrap_effect_fn(
            r#"
frame(agent.lua_state_agent, 3.0);
if macros::is_excute(agent) {
    macros::EFFECT_FOLLOW_NO_STOP_FLIP(agent, Hash40::new("moon_explosion"), Hash40::new("moon_explosion"), Hash40::new("top"), 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 0.7, true, *EF_FLIP_YZ);
    macros::EFFECT_FOLLOW_ALPHA(agent, Hash40::new("moon_explosion"), Hash40::new("top"), 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 0.8, true, 0.5);
    macros::EFFECT_ATTR(agent, Hash40::new("trace"), Hash40::new("rot"), 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 1.2, 0, 0, 0, 0, 0, 0, true, *EFFECT_SUB_ATTRIBUTE_NO_JOINT_SCALE);
    macros::LANDING_EFFECT_FLIP(agent, Hash40::new("smoke_l"), Hash40::new("smoke_r"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0, 0, 0, 0, 0, 0, false, *EF_FLIP_NONE);
}
frame(agent.lua_state_agent, 18.0);
if macros::is_excute(agent) {
    macros::EFFECT_OFF_KIND(agent, Hash40::new("moon_explosion"), false, true);
}"#,
        );
        let calls = parse_effect_script(&src).to_effect_calls();
        assert_eq!(calls.len(), 4);
        let moon: Vec<_> = calls
            .iter()
            .filter(|call| call.effect_name == "moon_explosion")
            .collect();
        assert_eq!(moon.len(), 2);
        assert!(moon.iter().all(|call| call.active_end == 18));
        assert!(calls
            .iter()
            .any(|call| call.effect_name == "trace" && call.active_end == call.active_start));
        assert!(calls
            .iter()
            .any(|call| call.effect_name == "smoke_l" && call.active_end == call.active_start));
        let landing = calls
            .iter()
            .find(|call| call.spawn_func == "LANDING_EFFECT_FLIP")
            .unwrap();
        assert_eq!(landing.effect_name, "smoke_l");
        assert_eq!(landing.effect_name_alt.as_deref(), Some("smoke_r"));
    }

    /// Property 1c: bare AFTER_IMAGE4_ON_arg29 produces EffectCall (AfterImage)
    #[test]
    fn test_bug_condition_bare_after_image_produces_effect_call() {
        // AFTER_IMAGE4_ON_arg29: args[1]=tex1, args[4]=bone
        let src = wrap_effect_fn(
            r#"macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("sword_trail"), Hash40::new("tex2"), 4, Hash40::new("sword"), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);"#,
        );
        let script = parse_effect_script(&src);
        let calls = script.to_effect_calls();
        assert!(
            !calls.is_empty(),
            "bare AFTER_IMAGE4_ON_arg29 should produce EffectCall"
        );
        assert_eq!(calls[0].effect_name, "sword_trail");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 2: Preservation tests
    // These tests MUST PASS on unfixed code — they confirm baseline behavior.
    // ═══════════════════════════════════════════════════════════════════════

    /// Preservation: is_excute-wrapped EFFECT still produces EffectCall
    #[test]
    fn test_preservation_is_excute_wrapped_effect_unchanged() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("wrapped_eff"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);
    }
}
"#;
        let script = parse_effect_script(src);
        let calls = script.to_effect_calls();
        assert!(
            !calls.is_empty(),
            "is_excute-wrapped EFFECT should produce EffectCall"
        );
        assert_eq!(calls[0].effect_name, "wrapped_eff");
    }

    /// Preservation: frame(...) advances the frame counter
    #[test]
    fn test_preservation_frame_call_advances_counter() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("late_eff"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);
    }
}
"#;
        let script = parse_effect_script(src);
        let calls = script.to_effect_calls();
        assert!(
            !calls.is_empty(),
            "should produce EffectCall after frame(10)"
        );
        assert_eq!(
            calls[0].active_start, 10,
            "active_start should be 10 after frame(10)"
        );
    }

    /// Preservation: non-EFFECT bare lines stay as Raw (no EffectCall produced)
    #[test]
    fn test_preservation_non_effect_bare_line_stays_raw() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    WorkModule::on_flag(agent.module_accessor, *FIGHTER_STATUS_WORK_ID_FLAG_RESERVE_ATTACK);
}
"#;
        let script = parse_effect_script(src);
        let calls = script.to_effect_calls();
        assert!(
            calls.is_empty(),
            "non-EFFECT bare line should produce no EffectCall, got {}",
            calls.len()
        );
    }

    /// Preservation: for loop with is_excute still works
    #[test]
    fn test_preservation_for_loop_with_is_excute_unchanged() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    for _ in 0..2 {
        wait(agent.lua_state_agent, 4.0);
        if macros::is_excute(agent) {
            macros::EFFECT(agent, Hash40::new("loop_eff"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);
        }
    }
}
"#;
        let script = parse_effect_script(src);
        let calls = script.to_effect_calls();
        // Loop runs 2 times, each spawning 1 effect at frames 4 and 8
        assert_eq!(
            calls.len(),
            2,
            "for loop should produce 2 EffectCalls, got {}",
            calls.len()
        );
        assert_eq!(calls[0].active_start, 4);
        assert_eq!(calls[1].active_start, 8);
    }

    // ═══ Spawn-macro fidelity ═══════════════════════════════════════════════

    /// The whole point of issue #4: an export must reissue the macro the script used.
    #[test]
    fn spawn_macros_survive_a_source_round_trip() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("sys_hit_l"), Hash40::new("sys_hit_r"), Hash40::new("haver"), 1.0, 2.0, 3.0, 0.0, 90.0, 45.0, 1.5, true, *EF_FLIP_YZ);
    }
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_NO_STOP(agent, Hash40::new("sys_smoke"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].spawn_func, "EFFECT_FOLLOW_FLIP");
        assert_eq!(calls[0].effect_name_alt.as_deref(), Some("sys_hit_r"));
        assert_eq!(
            calls[0].extra_args.as_deref(),
            Some(&["true".to_string(), "*EF_FLIP_YZ".to_string()][..])
        );
        assert_eq!(calls[1].spawn_func, "EFFECT_FOLLOW_NO_STOP");

        let (_, emitted) = emit_effect_move_fn(&calls, "test", &Default::default());
        assert!(
            emitted.contains(
                "macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new(\"sys_hit_l\"), Hash40::new(\"sys_hit_r\"), Hash40::new(\"haver\"), 1.0, 2.0, 3.0, 0.0, 90.0, 45.0, 1.5, true, *EF_FLIP_YZ);"
            ),
            "flipped follow spawn must come back out whole:\n{emitted}"
        );
        assert!(
            emitted.contains("macros::EFFECT_FOLLOW_NO_STOP(agent, Hash40::new(\"sys_smoke\")"),
            "a NO_STOP follow must not be swapped for plain EFFECT_FOLLOW:\n{emitted}"
        );
    }

    /// `LAST_EFFECT_SET_RATE` was parsed into the IR and then dropped on the way to
    /// `EffectCall`, so the export — which is generated from the calls — wrote no rate line at
    /// all. Every vanilla effect that plays fast or slow shipped at normal speed.
    ///
    /// Kirby's down attack, verbatim: two spawns and two different rates in one block, which
    /// is what proves the rate binds to the spawn directly above it rather than to whichever
    /// spawn happens to be last.
    #[test]
    fn a_rate_binds_to_the_spawn_above_it_and_survives_an_export() {
        let src = r#"
unsafe extern "C" fn effect_downattackd(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 15.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 180, 0, 0.4, 0, 0, 0, 0, 0, 0, false);
        macros::LAST_EFFECT_SET_RATE(agent, 2);
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("sys_attack_line"), Hash40::new("sys_attack_line"), Hash40::new("top"), -8, 4, 2.5, 0, 160, 0, 1.1, true, *EF_FLIP_YZ);
        macros::LAST_EFFECT_SET_RATE(agent, 1.5);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].effect_name, "sys_atk_smoke");
        assert_eq!(calls[0].rate, Some(2.0));
        assert_eq!(calls[1].effect_name, "sys_attack_line");
        assert_eq!(calls[1].rate, Some(1.5));

        let (_, emitted) = emit_effect_move_fn(&calls, "downattackd", &Default::default());
        // `2`, not `2.0`: the macro is generic over `ToF32`, and this is the spelling the
        // archive uses and the source write-back produces.
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_RATE(agent, 2);"),
            "a whole rate must not sprout a decimal point:\n{emitted}"
        );
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_RATE(agent, 1.5);"),
            "the second spawn's own rate must be written too:\n{emitted}"
        );
        let round_tripped = parse_effect_script(&emitted).to_effect_calls();
        assert_eq!(
            round_tripped.iter().map(|c| c.rate).collect::<Vec<_>>(),
            vec![Some(2.0), Some(1.5)],
            "reading the export back must find the same rates on the same spawns"
        );
    }

    /// Kirby's up air, verbatim, and Kirby's jab, verbatim — the two vanilla shapes for the
    /// colour and opacity modifiers. Both were deleted by every export before C1: the emitter
    /// rebuilds the function from `EffectCall`s, and neither line was in one.
    ///
    /// The green and blue here are over one, which is why the panel's drag fields are not
    /// clamped to the colour picker's range.
    #[test]
    fn a_tint_and_an_opacity_bind_to_the_spawn_above_and_survive_an_export() {
        let src = r#"
unsafe extern "C" fn effect_attackairhi(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("kirby_attack_arc"), Hash40::new("kirby_attack_arc"), Hash40::new("top"), -3, 7, 0, 0, 90, 90, 1, true, *EF_FLIP_YZ);
        macros::LAST_EFFECT_SET_COLOR(agent, 0.25, 1.3, 2.5);
        macros::EFFECT(agent, Hash40::new("sys_attack_impact"), Hash40::new("top"), 13, 6, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 360, false);
        macros::LAST_EFFECT_SET_ALPHA(agent, 0.7);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tint, Some([0.25, 1.3, 2.5]));
        // Each modifier lands on its own spawn and nowhere else. Sharing one field, or reaching
        // past the second spawn, would put the arc's colour on the impact or vice versa.
        assert_eq!(calls[0].alpha, None);
        assert_eq!(calls[1].tint, None);
        assert_eq!(calls[1].alpha, Some(0.7));

        let (_, emitted) = emit_effect_move_fn(&calls, "attackairhi", &Default::default());
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_COLOR(agent, 0.25, 1.3, 2.5);"),
            "the script's own tint must be written back out:\n{emitted}"
        );
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_ALPHA(agent, 0.7);"),
            "the script's own opacity must be written back out:\n{emitted}"
        );
        let round_tripped = parse_effect_script(&emitted).to_effect_calls();
        assert_eq!(
            round_tripped.iter().map(|c| c.tint).collect::<Vec<_>>(),
            vec![Some([0.25, 1.3, 2.5]), None]
        );
        assert_eq!(
            round_tripped.iter().map(|c| c.alpha).collect::<Vec<_>>(),
            vec![None, Some(0.7)]
        );
    }

    /// Dolly's up special, verbatim: the shape 64 of the corpus's 65 colour calls are in, and
    /// the reason modelling this macro recovered almost none of them.
    ///
    /// The tint is real at runtime — the game's "last effect" survives the block boundary — but
    /// it only runs on one costume. Binding it would export a costume-specific colour as an
    /// unconditional one, so it still binds to nothing.
    ///
    /// C1 left it there and reported it as a loss. C6 carries it: the line rides on the spawn
    /// it followed, wrapped in the guard it was written in, and comes back out of the export
    /// meaning what it meant going in.
    #[test]
    fn a_tint_cut_off_from_its_spawn_by_a_branch_is_carried_with_its_costume_check() {
        let src = r#"
unsafe extern "C" fn effect_specialhicommand(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_ALPHA(agent, Hash40::new("dolly_roll_l_color1"), Hash40::new("throw"), 0, 2.5, 0, 0, 0, 0, 1, true, 0.8);
    }
    if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){
        if macros::is_excute(agent) {
            macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);
        }
    }
}
"#;
        let (calls, unbound) = parse_effect_script(src).to_effect_calls_reporting_losses();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].tint, None,
            "a costume-gated tint must not be exported onto every costume"
        );
        assert!(
            unbound.is_empty(),
            "the export keeps this line now, so it is no longer a loss to report: {unbound:?}"
        );
        assert_eq!(
            calls[0].trailing,
            vec![
                "if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){",
                "if macros::is_excute(agent) {",
                "macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);",
                "}",
                "}",
            ],
            "the line rides on the spawn it followed, keeping its costume check"
        );

        let (_, emitted) = emit_effect_move_fn(&calls, "specialhicommand", &Default::default());
        // Both halves matter and they pull against each other. The tint must come back — that
        // is C6 — but only inside the costume check it was written in. Emitting the bare macro
        // would recolour all eight costumes, which is exactly what C1 refused to do and the
        // reason the line went unbound to begin with.
        assert!(
            emitted.contains("if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){"),
            "the costume check must survive the export:\n{emitted}"
        );
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);"),
            "the tint must survive the export:\n{emitted}"
        );
        let spawn_at = emitted.find("EFFECT_FOLLOW_ALPHA").unwrap();
        let tint_at = emitted.find("LAST_EFFECT_SET_COLOR").unwrap();
        assert!(
            spawn_at < tint_at,
            "LAST_EFFECT_SET_COLOR recolours whatever spawned last, so carrying it above its \
             own spawn would land it on someone else's:\n{emitted}"
        );
    }

    /// Two spawns at one frame, each with its own costume tint — dolly's up special stripped to
    /// its shape. The real one has three spawns and 24 tints in a single block.
    ///
    /// This is the case that decides whether carried lines hang off a call or off a frame, and
    /// it is why they hang off a call. `LAST_EFFECT_SET_COLOR` recolours whatever spawned most
    /// recently, so an export that wrote both spawns and then both tints — the natural shape if
    /// residue were anchored to a frame — would leave `dolly_roll_l_color1` its original colour
    /// and paint `dolly_roll_l_color2` twice. Every line would be present and the move would
    /// still be wrong, which is the failure hardest to notice by reading the output.
    #[test]
    fn each_costume_tint_stays_with_the_spawn_it_recolours() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_ALPHA(agent, Hash40::new("dolly_roll_l_color1"), Hash40::new("throw"), 0, 2.5, 0, 0, 0, 0, 1, true, 0.8);
    }
    if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){
        if macros::is_excute(agent) {
            macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);
        }
    }
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_ALPHA(agent, Hash40::new("dolly_roll_l_color2"), Hash40::new("throw"), 0, 2.5, 0, 0, 0, 0, 1, true, 0.8);
    }
    if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){
        if macros::is_excute(agent) {
            macros::LAST_EFFECT_SET_COLOR(agent, 0.587, 0.126, 0.169);
        }
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 2);
        let (_, emitted) = emit_effect_move_fn(&calls, "test", &Default::default());

        let first_spawn = emitted.find("dolly_roll_l_color1").unwrap();
        let first_tint = emitted.find("0.146").unwrap();
        let second_spawn = emitted.find("dolly_roll_l_color2").unwrap();
        let second_tint = emitted.find("0.587").unwrap();
        assert!(
            first_spawn < first_tint && first_tint < second_spawn && second_spawn < second_tint,
            "each tint must sit between its own spawn and the next one:\n{emitted}"
        );
        // The two spawns cannot share one `is_excute` block, because a tint has to come between
        // them — so the emitter must have split the frame rather than merging the run.
        assert_eq!(
            emitted.matches("if macros::is_excute(agent) {").count(),
            4,
            "one block per spawn and one per carried tint:\n{emitted}"
        );
    }

    /// A live colour multiplier and a script tint both want the same line. The override wins,
    /// exactly one line is written, and the script's value is not silently added underneath it.
    ///
    /// Before C1 this could not be got wrong, because the script's tint was not in `EffectCall`
    /// at all — the export wrote the tweak's colour and deleted the script's without a word.
    #[test]
    fn a_live_colour_override_replaces_the_scripts_tint_rather_than_stacking_with_it() {
        let src = r#"
unsafe extern "C" fn effect_attackairhi(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("kirby_attack_arc"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
        macros::LAST_EFFECT_SET_COLOR(agent, 0.25, 1.3, 2.5);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls[0].tint, Some([0.25, 1.3, 2.5]));
        let tweaks = std::collections::HashMap::from([(
            tweak_hash("kirby_attack_arc"),
            crate::mod_project::LiveTweak {
                effect_name: "kirby_attack_arc".into(),
                color: Some([1.0, 0.0, 0.0, 1.0]),
                speed: None,
            },
        )]);
        let (_, emitted) = emit_effect_move_fn(&calls, "attackairhi", &tweaks);
        assert_eq!(
            emitted.matches("LAST_EFFECT_SET_COLOR").count(),
            1,
            "exactly one tint line, or the export says two different things:\n{emitted}"
        );
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_COLOR(agent, 1.0, 0.0, 0.0);"),
            "the live override is the one that must survive:\n{emitted}"
        );
    }

    /// The rate macro names no effect, so binding it to anything but the spawn directly above
    /// it is a guess. A line this parser does not model could itself be a spawn, and then the
    /// rate would be written onto an effect it was never meant to touch.
    ///
    /// C6 made the export faithful here rather than merely cautious. The unmodelled spawn and
    /// the rate are both carried, in order, below `sys_atk_smoke` — so the rate still lands on
    /// whatever `SOME_UNMODELLED_SPAWN` spawns, exactly as it did in the original. The refusal
    /// to *bind* is unchanged and still the thing under test; what changed is that refusing no
    /// longer means deleting.
    ///
    /// This is also the answer to C6's stated doubling trap. A line only becomes carried
    /// residue if it produced no `EffectCall`, so a preserved line and a regenerated call can
    /// never be the same spawn — asserted below.
    #[test]
    fn a_rate_with_no_spawn_directly_above_it_attaches_to_nothing() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
        macros::SOME_UNMODELLED_SPAWN(agent, Hash40::new("whatever"));
        macros::LAST_EFFECT_SET_RATE(agent, 2);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].rate, None,
            "the rate belongs to the unmodelled line above it, not to sys_atk_smoke"
        );
        let (_, emitted) = emit_effect_move_fn(&calls, "test", &Default::default());
        assert_eq!(
            emitted.matches("SOME_UNMODELLED_SPAWN").count(),
            1,
            "a carried line must not be emitted alongside a call regenerated from it:\n{emitted}"
        );
        let unmodelled_at = emitted.find("SOME_UNMODELLED_SPAWN").unwrap();
        let rate_at = emitted.find("LAST_EFFECT_SET_RATE").unwrap();
        let smoke_at = emitted.find("sys_atk_smoke").unwrap();
        assert!(
            smoke_at < unmodelled_at && unmodelled_at < rate_at,
            "the rate must stay below the unmodelled spawn it names, and both below \
             sys_atk_smoke:\n{emitted}"
        );
    }

    /// The effect export regenerates the whole function from the call list, so a macro that is
    /// not in that list is a macro the export deletes. `FLASH` and the `BURN_COLOR` family were
    /// parsed as `Raw` lines and therefore silently dropped from every exported move — 69
    /// occurrences in the local corpus, including the entire colour ramp below.
    ///
    /// Kirby's dash attack, verbatim: the snap-then-interpolate pairing that makes up almost
    /// every real use of the family, plus the argument-less reset and a spawn in the same
    /// function to prove the two kinds of entry share a list without disturbing each other.
    #[test]
    fn colour_commands_survive_an_export_instead_of_being_dropped() {
        let src = r#"
unsafe extern "C" fn effect_attackdash(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_NO_STOP(agent, Hash40::new("kirby_dash"), Hash40::new("top"), 0, 6, 5, -90, 0, 160, 0.7, true);
    }
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::BURN_COLOR(agent, 2, 0.059, 0.008, 0);
        macros::BURN_COLOR_FRAME(agent, 4, 2, 0.059, 0.008, 0.9);
    }
    frame(agent.lua_state_agent, 30.0);
    if macros::is_excute(agent) {
        macros::BURN_COLOR(agent, 2, 0.059, 0.008, 0.9);
        macros::BURN_COLOR_FRAME(agent, 12, 2, 0.059, 0.008, 0);
        macros::EFFECT_OFF_KIND(agent, Hash40::new("kirby_dash"), false, true);
    }
    frame(agent.lua_state_agent, 42.0);
    if macros::is_excute(agent) {
        macros::BURN_COLOR_NORMAL(agent);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        // One spawn plus five colour commands, in script order.
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[0].effect_name, "kirby_dash");
        assert!(calls[0].color.is_none());
        let colors: Vec<_> = calls[1..]
            .iter()
            .map(|call| (call.spawn_func.as_str(), call.color.clone().unwrap()))
            .collect();
        assert_eq!(
            colors,
            vec![
                (
                    "BURN_COLOR",
                    crate::data::ColorCall {
                        transition: None,
                        rgba: Some([2.0, 0.059, 0.008, 0.0])
                    }
                ),
                (
                    "BURN_COLOR_FRAME",
                    crate::data::ColorCall {
                        transition: Some(4.0),
                        rgba: Some([2.0, 0.059, 0.008, 0.9])
                    }
                ),
                (
                    "BURN_COLOR",
                    crate::data::ColorCall {
                        transition: None,
                        rgba: Some([2.0, 0.059, 0.008, 0.9])
                    }
                ),
                (
                    "BURN_COLOR_FRAME",
                    crate::data::ColorCall {
                        transition: Some(12.0),
                        rgba: Some([2.0, 0.059, 0.008, 0.0])
                    }
                ),
                (
                    "BURN_COLOR_NORMAL",
                    crate::data::ColorCall {
                        transition: None,
                        rgba: None
                    }
                ),
            ],
            "the interpolation length is the first argument, and the four after it are the colour"
        );
        // A colour command is one instant event: nothing closes it, so it must not be given a
        // window that an EFFECT_OFF_KIND would then be emitted to end.
        assert!(calls[1..]
            .iter()
            .all(|call| call.active_end == call.active_start));

        let (_, emitted) = emit_effect_move_fn(&calls, "attackdash", &Default::default());
        // Whole numbers stay whole: every slot in this family is generic over `ToF32`, so this
        // is the spelling the archive uses and the one the source write-back produces.
        for line in [
            "macros::BURN_COLOR(agent, 2, 0.059, 0.008, 0);",
            "macros::BURN_COLOR_FRAME(agent, 4, 2, 0.059, 0.008, 0.9);",
            "macros::BURN_COLOR(agent, 2, 0.059, 0.008, 0.9);",
            "macros::BURN_COLOR_FRAME(agent, 12, 2, 0.059, 0.008, 0);",
            "macros::BURN_COLOR_NORMAL(agent);",
        ] {
            assert!(emitted.contains(line), "missing {line} from:\n{emitted}");
        }
        assert_eq!(
            parse_effect_script(&emitted).to_effect_calls(),
            calls,
            "reading the export back must produce the very same calls"
        );
    }

    /// `COL_NORMAL` is a colour-blend reset — `lua_const` names it
    /// `MA_MSC_CMD_COLOR_BLEND_COL_NORMAL`, one of six such commands with `FLASH` — and not the
    /// body-collision command the `game_` hurtbox panel files it under. Before C6b the effect
    /// export had no form for it, so it deleted all eight of the corpus's occurrences: they sit
    /// in blocks with no spawn, so C6's residue had no call to ride on.
    ///
    /// The source is kirby/SpecialSStart's `effect_` function verbatim, which is the smallest
    /// real case and the one that shows the line is written *outside* an `is_excute` block.
    #[test]
    fn a_lone_col_normal_survives_the_effect_export() {
        let src = r#"
unsafe extern "C" fn effect_specialsstart(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_flash"), Hash40::new("haver"), -0.012, 11.999, 0.137, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, true);
    }
    macros::COL_NORMAL(agent);
    wait(agent.lua_state_agent, 2.0);
    if macros::is_excute(agent) {
        macros::FLASH(agent, 1, 1, 1, 0.7);
    }
    wait(agent.lua_state_agent, 2.0);
}
"#;
        let script = parse_effect_script(src);
        assert!(
            unexportable_effect_lines(&script).is_empty(),
            "nothing in this function should be reported lost: {:?}",
            unexportable_effect_lines(&script)
        );

        let calls = script.to_effect_calls();
        assert_eq!(calls.len(), 3, "the spawn, the reset, and the flash");
        assert_eq!(calls[1].spawn_func, "COL_NORMAL");
        assert_eq!(
            calls[1].color,
            Some(crate::data::ColorCall {
                transition: None,
                rgba: None
            }),
            "it takes no arguments, exactly like BURN_COLOR_NORMAL"
        );
        // It reads as its own entry rather than as residue on the spawn above it, which is what
        // makes it separately disableable — and it must not have been pulled onto that spawn's
        // frame either.
        assert!(calls[0].trailing.is_empty() && calls[1].leading.is_empty());
        assert_eq!(calls[1].active_start, calls[0].active_start);
        assert_eq!(calls[2].active_start, calls[1].active_start + 2);

        let (_, emitted) = emit_effect_move_fn(&calls, "specialsstart", &Default::default());
        assert!(
            emitted.contains("macros::COL_NORMAL(agent);"),
            "missing the reset from:\n{emitted}"
        );
        assert_eq!(
            parse_effect_script(&emitted).to_effect_calls(),
            calls,
            "reading the export back must produce the very same calls"
        );
    }

    /// The colour table is matched by substring, so a shorter name that is a tail of a longer
    /// one is the standing hazard in this file — `COL_NORMAL` is a tail of `BURN_COLOR_NORMAL`.
    /// The `macros::` prefix is what keeps them apart; this pins that it does, because reading a
    /// burn reset as a blend reset would export the wrong macro and silently stop the burn from
    /// clearing.
    #[test]
    fn a_burn_color_normal_is_not_read_as_a_col_normal() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::BURN_COLOR_NORMAL(agent);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].spawn_func, "BURN_COLOR_NORMAL");
    }

    /// Teaching the effect parser about `COL_NORMAL` must not reach the `game_` parser, which
    /// keeps its own [`crate::data::ExcuteStmt::ColNormal`] so that a `COL_PRI` span still
    /// closes. The two never meet — every one of the corpus's ten occurrences is in an
    /// `effect_` function — but nothing in the code says so, so it is asserted here.
    #[test]
    fn a_game_script_still_reads_col_normal_as_its_own_statement() {
        let src = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::COL_PRI(agent, 200);
    }
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::COL_NORMAL(agent);
    }
}
"#;
        let script = parse_acmd_script(src);
        let stmts: Vec<_> = script
            .stmts
            .iter()
            .filter_map(|stmt| match stmt {
                crate::data::AcmdStmt::Excute(inner) => Some(inner),
                _ => None,
            })
            .flatten()
            .map(|stmt| format!("{stmt:?}"))
            .collect();
        assert_eq!(stmts, vec!["ColPri(200)", "ColNormal"]);
    }

    /// A colour command produces an entry in the call list, so it consumes a write-back
    /// ordinal — and it is not a spawn, so it must not anchor a rate. Getting either half
    /// wrong writes one call's value into another's line.
    #[test]
    fn a_colour_command_takes_an_ordinal_but_never_anchors_a_rate() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
        macros::FLASH(agent, 0.314, 0.235, 0.157, 0.039);
        macros::LAST_EFFECT_SET_RATE(agent, 2);
    }
}
"#;
        let script = parse_effect_script(src);
        let calls = script.to_effect_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].rate, None,
            "the rate sits under the FLASH, not under the spawn — attaching it anyway would \
             retune an effect the script never asked to retune"
        );
        assert_eq!(
            script.call_macro_ordinals().len(),
            calls.len(),
            "every call needs an ordinal or write-back writes into the wrong line"
        );
    }

    /// A live speed override is a deliberate replacement of the kind's playback rate. Writing
    /// both it and the spawn's own rate would leave the second line winning anyway, and would
    /// read back as though the script had asked for the override's value.
    #[test]
    fn a_live_speed_override_replaces_the_spawns_own_rate_rather_than_joining_it() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
        macros::LAST_EFFECT_SET_RATE(agent, 2);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        let mut tweaks = std::collections::HashMap::new();
        tweaks.insert(
            tweak_hash("sys_atk_smoke"),
            crate::mod_project::LiveTweak {
                effect_name: "sys_atk_smoke".into(),
                color: None,
                speed: Some(0.5),
            },
        );
        let (_, emitted) = emit_effect_move_fn(&calls, "test", &tweaks);
        assert_eq!(
            emitted.matches("LAST_EFFECT_SET_RATE").count(),
            1,
            "exactly one rate line, or the export says two different things:\n{emitted}"
        );
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_RATE(agent, 0.5);"),
            "the override is the one that must survive:\n{emitted}"
        );
    }

    /// The macros take rotation as `zr, yr, xr`. Parsing them left to right into `[x, y, z]`
    /// swapped two of the three angles against every other path in the editor.
    #[test]
    fn rotation_arguments_keep_their_zyx_slot_order() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 1.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("g"), Hash40::new("top"), 0.0, 0.0, 0.0, 10.0, 20.0, 30.0, 1.0, 0, 0, 0, 0, 0, 0, false);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        // zr=10, yr=20, xr=30 → the editor's [x, y, z].
        assert_eq!(calls[0].rotation, [30.0, 20.0, 10.0]);

        let (_, emitted) = emit_effect_move_fn(&calls, "test", &Default::default());
        assert!(
            emitted.contains("0.0, 0.0, 0.0, 10.0, 20.0, 30.0, 1.0"),
            "the angles must land back in the slots they came from:\n{emitted}"
        );
    }

    /// A sword trail is a texture and a set of trail parameters, not a graphic with a
    /// transform. Rebuilding one as `EFFECT_FOLLOW` spawned a nonexistent effect and left
    /// the trail running.
    /// A trail's texture and joint are editable fields in the panels, but the whole call was
    /// replayed verbatim, so renaming one exported the ORIGINAL trail and said nothing.
    #[test]
    fn renaming_a_trail_reaches_the_exported_source() {
        let src = wrap_effect_fn(
            r#"        macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), Hash40::new("sword2"), 3, 8, 0.75);"#,
        );
        let calls = parse_effect_script(&src).to_effect_calls();
        assert_eq!(calls.len(), 1, "{calls:#?}");

        let mut edited = calls.clone();
        edited[0].effect_name = "my_trail".into();
        edited[0].bone_name = "HaveL".into();
        let out = preview_effect_fn(&edited, "attacks4", &[]);

        assert!(out.contains(r#"Hash40::new("my_trail")"#), "{out}");
        // Joints are hashed lowercase, as everywhere else.
        assert!(out.contains(r#"Hash40::new("havel")"#), "{out}");
        // Everything the editor has no field for is left exactly as the user wrote it.
        assert!(out.contains(r#"Hash40::new("tex2"), 4,"#), "{out}");
        assert!(
            out.contains(r#"Hash40::new("sword2"), 3, 8, 0.75)"#),
            "{out}"
        );
        assert!(out.contains("macros::AFTER_IMAGE4_ON_arg29("), "{out}");

        // And an untouched trail still comes back byte-identical.
        let untouched = preview_effect_fn(&calls, "attacks4", &[]);
        assert!(
            untouched.contains(
                r#"macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), Hash40::new("sword2"), 3, 8, 0.75);"#
            ),
            "{untouched}"
        );
    }

    #[test]
    fn after_image_trails_export_as_trails() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 3.0);
    if macros::is_excute(agent) {
        macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), Hash40::new("sword2"), 0, 0, 0, 3, 8, 0.75);
    }
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::AFTER_IMAGE_OFF(agent);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].spawn_func, "AFTER_IMAGE_ON");
        assert_eq!((calls[0].active_start, calls[0].active_end), (3, 9));

        let (_, emitted) = emit_effect_move_fn(&calls, "test", &Default::default());
        assert!(
            emitted.contains("macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new(\"tex1\")"),
            "the trail call must be replayed verbatim:\n{emitted}"
        );
        assert!(
            emitted.contains("macros::AFTER_IMAGE_OFF(agent);"),
            "a trail is closed by AFTER_IMAGE_OFF:\n{emitted}"
        );
        assert!(
            !emitted.contains("EFFECT_OFF_KIND") && !emitted.contains("EFFECT_FOLLOW"),
            "a trail is not an effect spawn:\n{emitted}"
        );
    }

    /// Projects saved before `extra_args` existed still have to export something that
    /// compiles — the macro name alone is not enough to rebuild an unknown signature.
    #[test]
    fn a_spawn_with_no_recorded_tail_falls_back_to_the_plain_pair() {
        let call = crate::data::EffectCall {
            effect_name: "sys_hit".into(),
            effect_name_alt: Some("sys_hit_alt".into()),
            spawn_func: "EFFECT_FOLLOW_FLIP".into(),
            bone_name: "HaveR".into(),
            offset: [0.0; 3],
            rotation: [0.0; 3],
            scale: 1.0,
            follows_bone: true,
            active_start: 2,
            active_end: 9999,
            disabled: false,
            extra_args: None,
            raw_line: None,
            rate: None,
            tint: None,
            alpha: None,
            color: None,
            guard: None,
            leading: Vec::new(),
            trailing: Vec::new(),
        };
        let (_, emitted) = emit_effect_move_fn(&[call], "test", &Default::default());
        assert!(
            emitted.contains(
                "macros::EFFECT_FOLLOW(agent, Hash40::new(\"sys_hit\"), Hash40::new(\"haver\"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);"
            ),
            "an unrecoverable tail must fall back to a single-graphic EFFECT_FOLLOW:\n{emitted}"
        );
    }

    /// Audit the emitter against every script in the local fetch cache: each spawn must come
    /// back out under the same macro name and with the same number of arguments it went in
    /// with. Skipped when nothing has been fetched yet, so a clean machine still passes.
    #[test]
    fn cached_scripts_round_trip_through_the_emitter() {
        let cache = crate::scratch_dirs::app_storage_root().join("script-cache");
        if !cache.is_dir() {
            return;
        }
        let mut bodies = Vec::new();
        for fighter in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
            for script in std::fs::read_dir(fighter.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Ok(body) = std::fs::read_to_string(script.path()) {
                    bodies.push(body);
                }
            }
        }
        if bodies.is_empty() {
            return;
        }

        let mut spawns = 0usize;
        let mut problems: Vec<String> = Vec::new();
        for body in &bodies {
            let calls = parse_effect_script(body).to_effect_calls();
            if calls.is_empty() {
                continue;
            }
            // Re-reading the generated function must find the same set of spawns: this is
            // the property an exported mod depends on, end to end.
            let (_, emitted) = emit_effect_move_fn(&calls, "audit", &Default::default());
            let mut before: Vec<(String, String)> = calls
                .iter()
                .filter(|c| !c.disabled)
                .map(|c| (c.spawn_func.clone(), c.effect_name.clone()))
                .collect();
            let mut after: Vec<(String, String)> = parse_effect_script(&emitted)
                .to_effect_calls()
                .iter()
                .map(|c| (c.spawn_func.clone(), c.effect_name.clone()))
                .collect();
            before.sort();
            after.sort();
            if before != after {
                problems.push(format!("re-parse mismatch:\n{before:?}\n{after:?}"));
            }

            // Every spawn macro the source used must appear in the output under its own
            // name, with the same arity. A miscount here is a call the game would reject.
            for call in calls.iter().filter(|c| !c.disabled) {
                spawns += 1;
                let Some(tail) = &call.extra_args else {
                    continue;
                };
                let expected = 1 // agent
                    + 1 // graphic
                    + usize::from(call.effect_name_alt.is_some())
                    + 1 // joint
                    + 7 // pos, rot, size
                    + tail.len();
                let line = emit_spawn_call(call, "");
                let Some(open) = line.find('(') else {
                    problems.push(format!("no call emitted for {}", call.spawn_func));
                    continue;
                };
                if !line.starts_with(&format!("macros::{}(", call.spawn_func)) {
                    problems.push(format!("{} was emitted as {line}", call.spawn_func));
                    continue;
                }
                let args = tokenize_args(line[open + 1..].rsplit_once(')').unwrap().0);
                if args.len() != expected {
                    problems.push(format!(
                        "{} emitted {} args, source had {expected}: {line}",
                        call.spawn_func,
                        args.len()
                    ));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "{} of {spawns} spawns did not round-trip:\n{}",
            problems.len(),
            problems
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        eprintln!("[audit] {spawns} spawns across {} scripts", bodies.len());
    }

    /// C6's own corpus oracle: how much of a real effect script an export still deletes.
    ///
    /// C5 measured this and could only report it — 32 of the 132 effect scripts that produce
    /// calls lost at least one line. This asserts the number rather than printing it, because
    /// the whole family of tasks is a ratchet: every macro modelled and every line carried
    /// should move it down, and nothing should move it back up. A change that regresses this
    /// is a change that started silently deleting user code again.
    ///
    /// Two things are checked past the count. Braces must balance, or the carried lines have
    /// produced a function that will not compile; and the output must re-parse to the same
    /// spawns, which is what stops "preserve the line" from turning into "spawn it twice".
    #[test]
    fn the_effect_export_still_loses_no_more_of_the_corpus_than_it_did() {
        let cache = crate::scratch_dirs::app_storage_root().join("script-cache");
        if !cache.is_dir() {
            return;
        }
        let mut lossy = 0usize;
        let mut with_calls = 0usize;
        let mut unbalanced: Vec<String> = Vec::new();
        for fighter in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
            for entry in std::fs::read_dir(fighter.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let Ok(body) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                let script = parse_effect_script(&body);
                let calls = script.to_effect_calls();
                if calls.is_empty() {
                    continue;
                }
                with_calls += 1;
                if !unexportable_effect_lines(&script).is_empty() {
                    lossy += 1;
                }
                let (_, emitted) = emit_effect_move_fn(&calls, "audit", &Default::default());
                let opens = emitted.matches('{').count();
                let closes = emitted.matches('}').count();
                if opens != closes {
                    unbalanced.push(format!("{:?}: {opens} vs {closes}", entry.path()));
                }
            }
        }
        if with_calls == 0 {
            return;
        }
        assert!(
            unbalanced.is_empty(),
            "carried lines left the output unbalanced:\n{}",
            unbalanced.join("\n")
        );
        eprintln!("[audit] {lossy} of {with_calls} effect scripts still lose a line");
        assert!(
            lossy <= 15,
            "the export deletes lines from {lossy} of {with_calls} effect scripts; C5 measured \
             28, C6 brought it to 19, and C6b's COL_NORMAL to 15. Something started dropping \
             user code again."
        );
    }

    // ═══ Export preview ═════════════════════════════════════════════════════

    /// The preview is only worth showing if it is what you actually get. Both functions have
    /// to appear verbatim in the project the export writes.
    #[test]
    fn the_preview_is_exactly_what_the_export_writes() {
        let project = sample_project();
        let acmd = project
            .files
            .iter()
            .find(|f| f.rel_path == "src/mario/acmd.rs")
            .map(|f| f.contents.as_str())
            .expect("generated fighter ACMD");

        let sample = sample_edits();
        let game = preview_game_fn(&sample.0, "attack_air_n");
        assert!(
            acmd.contains(&game),
            "the previewed game_* function is not what was exported:\n{game}\n---\n{acmd}"
        );
        let effect = preview_effect_fn(&sample.1, "attack_air_n", &sample.2);
        assert!(
            acmd.contains(&effect),
            "the previewed effect_* function is not what was exported:\n{effect}\n---\n{acmd}"
        );
        // And the preview really is showing the user's macro, not a substitute.
        assert!(effect.contains("macros::EFFECT_FOLLOW(agent, Hash40::new(\"sys_flash\")"));
    }

    /// A call with no recorded tail is the one case the export still cannot reproduce, so the
    /// window has to say so rather than showing a substitute macro with no explanation.
    #[test]
    fn spawns_the_export_cannot_reproduce_are_reported() {
        let mut calls = sample_edits().1;
        // Faithful: both carry their tails.
        assert!(export_spawn_downgrades(&calls).is_empty());

        // A project saved before tails were recorded, on a macro with no plain equivalent.
        calls[1].spawn_func = "EFFECT_FOLLOW_NO_STOP".into();
        calls[1].extra_args = None;
        assert_eq!(
            export_spawn_downgrades(&calls),
            vec![("EFFECT_FOLLOW_NO_STOP".to_string(), "sys_flash".to_string())]
        );

        // A disabled call is not exported at all, so it is not a downgrade.
        calls[1].disabled = true;
        assert!(export_spawn_downgrades(&calls).is_empty());

        // Neither is one whose macro IS what the fallback emits.
        calls[1].disabled = false;
        calls[1].spawn_func = "EFFECT_FOLLOW".into();
        assert!(export_spawn_downgrades(&calls).is_empty());
    }

    /// Graphic slots are not always string literals; consts and locals must pass through
    /// rather than being re-quoted into a graphic literally named after the expression.
    #[test]
    fn expression_valued_graphic_arguments_are_not_re_quoted() {
        assert_eq!(hash_arg("sys_hit"), "Hash40::new(\"sys_hit\")");
        assert_eq!(hash_arg("0x1234abcd"), "Hash40::new_raw(0x1234abcd)");
        assert_eq!(hash_arg("*EFFECT_KIND"), "*EFFECT_KIND");
        assert_eq!(hash_arg("Hash40::new(\"x\")"), "Hash40::new(\"x\")");
    }
}
