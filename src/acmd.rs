/// Parse ACMD scripts into a structured IR that preserves loops and can be re-exported.

use crate::data::{AcmdScript, AcmdStmt, AttackCall, EffectMacro, EffectScript, EffectStmt, ExcuteStmt};

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
pub fn fetch_acmd_script(fighter: &str, move_name: &str) -> anyhow::Result<AcmdScript> {
    let body = fetch_script_body(fighter, move_name)?;
    Ok(parse_acmd_script(&body))
}

/// Fetch the raw script body text for a fighter+move from GitHub.
pub fn fetch_script_body(fighter: &str, move_name: &str) -> anyhow::Result<String> {
    let pascal = move_name_to_pascal(move_name);
    let url = format!(
        "https://raw.githubusercontent.com/WuBoytH/SSBU-Dumped-Scripts/main/smashline/lua2cpp_{fighter}/{fighter}/{pascal}.txt"
    );
    Ok(reqwest::blocking::get(&url)?.text()?)
}

pub fn parse_acmd_script(source: &str) -> AcmdScript {
    let game_fn = extract_game_function(source);
    let source = game_fn.as_deref().unwrap_or(source);
    let lines: Vec<&str> = source.lines().collect();
    // Skip the function signature line and closing brace
    let body_lines = if lines.len() >= 2 { &lines[1..lines.len()-1] } else { &lines[..] };
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
        if line.is_empty() { continue; }
        if line.contains("macros::ATTACK(") {
            if let Some(call) = parse_attack_call(line) {
                stmts.push(ExcuteStmt::Attack(call));
                continue;
            }
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
            match ch { '{' => depth += 1, '}' => depth -= 1, _ => {} }
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
        if !in_game_fn {
            if trimmed.contains("game_")
                && !trimmed.contains("effect_")
                && !trimmed.contains("sound_")
                && !trimmed.contains("expression_")
                && (trimmed.contains("fn game_") || trimmed.starts_with("unsafe extern"))
            {
                in_game_fn = true;
                found = true;
            }
        }
        if in_game_fn {
            result.push_str(line);
            result.push('\n');
            for ch in line.chars() {
                match ch { '{' => depth += 1, '}' => { depth -= 1; } _ => {} }
            }
            if depth == 0 { break; }
        }
    }
    if found { Some(result) } else { None }
}

/// Extract only the `effect_` function body (mirrors `extract_game_function`).
fn extract_effect_function(source: &str) -> Option<String> {
    let mut result = String::new();
    let mut in_effect_fn = false;
    let mut depth: i32 = 0;
    let mut found = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if !in_effect_fn {
            if trimmed.contains("effect_")
                && !trimmed.contains("game_")
                && !trimmed.contains("sound_")
                && !trimmed.contains("expression_")
                && (trimmed.contains("fn effect_") || trimmed.starts_with("unsafe extern"))
            {
                in_effect_fn = true;
                found = true;
            }
        }
        if in_effect_fn {
            result.push_str(line);
            result.push('\n');
            for ch in line.chars() {
                match ch { '{' => depth += 1, '}' => { depth -= 1; } _ => {} }
            }
            if depth == 0 { break; }
        }
    }
    if found { Some(result) } else { None }
}

/// Parse the contents of an is_excute block from an effect_ script.
fn parse_excute_block_effects(lines: &[&str]) -> Vec<EffectMacro> {
    let mut macros = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() { continue; }

        // Helper: extract args string from a macro call like `macros::FOO(...)`
        let try_extract = |prefix: &str| -> Option<Vec<String>> {
            let start = line.find(prefix)?;
            let after = &line[start + prefix.len()..];
            // find matching closing paren
            let end = after.rfind(')')?;
            Some(tokenize_args(&after[..end]))
        };

        if line.contains("macros::EFFECT_FOLLOW_FLIP(") {
            if let Some(t) = try_extract("macros::EFFECT_FOLLOW_FLIP(") {
                // args[1]=effect_hash, args[2]=effect_hash2 (ignore), args[3]=bone_hash
                // args[4]=x, args[5]=y, args[6]=z, args[7]=rot_x, args[8]=rot_y, args[9]=rot_z, args[10]=scale
                if t.len() > 10 {
                    let effect_name = extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    let bone_name   = extract_hash40_string(&t[3]).unwrap_or_else(|| t[3].trim().to_string());
                    let x     = t[4].trim().parse::<f32>().unwrap_or(0.0);
                    let y     = t[5].trim().parse::<f32>().unwrap_or(0.0);
                    let z     = t[6].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_x = t[7].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_y = t[8].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_z = t[9].trim().parse::<f32>().unwrap_or(0.0);
                    let scale = t[10].trim().parse::<f32>().unwrap_or(1.0);
                    macros.push(EffectMacro::Effect {
                        effect_name, bone_name,
                        offset: [x, y, z],
                        rotation: [rot_x, rot_y, rot_z],
                        scale,
                        follows_bone: true,
                    });
                    continue;
                }
            }
        }

        if line.contains("macros::EFFECT_FLIP(") {
            if let Some(t) = try_extract("macros::EFFECT_FLIP(") {
                if t.len() > 10 {
                    let effect_name = extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    let bone_name   = extract_hash40_string(&t[3]).unwrap_or_else(|| t[3].trim().to_string());
                    let x     = t[4].trim().parse::<f32>().unwrap_or(0.0);
                    let y     = t[5].trim().parse::<f32>().unwrap_or(0.0);
                    let z     = t[6].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_x = t[7].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_y = t[8].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_z = t[9].trim().parse::<f32>().unwrap_or(0.0);
                    let scale = t[10].trim().parse::<f32>().unwrap_or(1.0);
                    macros.push(EffectMacro::Effect {
                        effect_name, bone_name,
                        offset: [x, y, z],
                        rotation: [rot_x, rot_y, rot_z],
                        scale,
                        follows_bone: false,
                    });
                    continue;
                }
            }
        }

        if line.contains("macros::EFFECT_FOLLOW(") {
            if let Some(t) = try_extract("macros::EFFECT_FOLLOW(") {
                // args[1]=effect_hash, args[2]=bone_hash, args[3]=x, args[4]=y, args[5]=z
                // args[6]=rot_x, args[7]=rot_y, args[8]=rot_z, args[9]=scale
                if t.len() > 9 {
                    let effect_name = extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    let bone_name   = extract_hash40_string(&t[2]).unwrap_or_else(|| t[2].trim().to_string());
                    let x     = t[3].trim().parse::<f32>().unwrap_or(0.0);
                    let y     = t[4].trim().parse::<f32>().unwrap_or(0.0);
                    let z     = t[5].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_x = t[6].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_y = t[7].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_z = t[8].trim().parse::<f32>().unwrap_or(0.0);
                    let scale = t[9].trim().parse::<f32>().unwrap_or(1.0);
                    macros.push(EffectMacro::Effect {
                        effect_name, bone_name,
                        offset: [x, y, z],
                        rotation: [rot_x, rot_y, rot_z],
                        scale,
                        follows_bone: true,
                    });
                    continue;
                }
            }
        }

        if line.contains("macros::EFFECT(") {
            if let Some(t) = try_extract("macros::EFFECT(") {
                if t.len() > 9 {
                    let effect_name = extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    let bone_name   = extract_hash40_string(&t[2]).unwrap_or_else(|| t[2].trim().to_string());
                    let x     = t[3].trim().parse::<f32>().unwrap_or(0.0);
                    let y     = t[4].trim().parse::<f32>().unwrap_or(0.0);
                    let z     = t[5].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_x = t[6].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_y = t[7].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_z = t[8].trim().parse::<f32>().unwrap_or(0.0);
                    let scale = t[9].trim().parse::<f32>().unwrap_or(1.0);
                    macros.push(EffectMacro::Effect {
                        effect_name, bone_name,
                        offset: [x, y, z],
                        rotation: [rot_x, rot_y, rot_z],
                        scale,
                        follows_bone: false,
                    });
                    continue;
                }
            }
        }

        if line.contains("macros::FOOT_EFFECT(") {
            if let Some(t) = try_extract("macros::FOOT_EFFECT(") {
                if t.len() > 9 {
                    let effect_name = extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    let bone_name   = extract_hash40_string(&t[2]).unwrap_or_else(|| t[2].trim().to_string());
                    let x     = t[3].trim().parse::<f32>().unwrap_or(0.0);
                    let y     = t[4].trim().parse::<f32>().unwrap_or(0.0);
                    let z     = t[5].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_x = t[6].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_y = t[7].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_z = t[8].trim().parse::<f32>().unwrap_or(0.0);
                    let scale = t[9].trim().parse::<f32>().unwrap_or(1.0);
                    macros.push(EffectMacro::Effect {
                        effect_name, bone_name,
                        offset: [x, y, z],
                        rotation: [rot_x, rot_y, rot_z],
                        scale,
                        follows_bone: false,
                    });
                    continue;
                }
            }
        }

        if line.contains("macros::LANDING_EFFECT(") {
            if let Some(t) = try_extract("macros::LANDING_EFFECT(") {
                if t.len() > 9 {
                    let effect_name = extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
                    let bone_name   = extract_hash40_string(&t[2]).unwrap_or_else(|| t[2].trim().to_string());
                    let x     = t[3].trim().parse::<f32>().unwrap_or(0.0);
                    let y     = t[4].trim().parse::<f32>().unwrap_or(0.0);
                    let z     = t[5].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_x = t[6].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_y = t[7].trim().parse::<f32>().unwrap_or(0.0);
                    let rot_z = t[8].trim().parse::<f32>().unwrap_or(0.0);
                    let scale = t[9].trim().parse::<f32>().unwrap_or(1.0);
                    macros.push(EffectMacro::Effect {
                        effect_name, bone_name,
                        offset: [x, y, z],
                        rotation: [rot_x, rot_y, rot_z],
                        scale,
                        follows_bone: false,
                    });
                    continue;
                }
            }
        }

        if line.contains("macros::EFFECT_OFF_KIND(") {
            if let Some(t) = try_extract("macros::EFFECT_OFF_KIND(") {
                if t.len() > 1 {
                    let effect_name = extract_hash40_string(&t[1]).unwrap_or_else(|| t[1].trim().to_string());
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
                    macros.push(EffectMacro::AfterImage { effect_name, bone_name });
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

        macros.push(EffectMacro::Raw(line.to_string()));
    }
    macros
}

/// Parse statements from an effect_ function body, producing `EffectStmt`.
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

        if line.contains("frame(") && !line.contains("is_excute") {
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
        let is_effect_macro = line.contains("macros::EFFECT(")
            || line.contains("macros::EFFECT_FOLLOW(")
            || line.contains("macros::EFFECT_FLIP(")
            || line.contains("macros::EFFECT_FOLLOW_FLIP(")
            || line.contains("macros::FOOT_EFFECT(")
            || line.contains("macros::LANDING_EFFECT(")
            || line.contains("macros::EFFECT_OFF_KIND(")
            || line.contains("macros::AFTER_IMAGE4_ON")
            || line.contains("macros::AFTER_IMAGE_ON")
            || line.contains("macros::AFTER_IMAGE_OFF(")
            || line.contains("macros::LAST_EFFECT_SET_RATE(");
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

/// Parse an effect_ script source into an `EffectScript` IR.
pub fn parse_effect_script(source: &str) -> crate::data::EffectScript {
    let effect_fn = extract_effect_function(source);
    let effect_fn = match effect_fn {
        Some(ref s) => s.as_str(),
        None => return EffectScript::default(),
    };
    let lines: Vec<&str> = effect_fn.lines().collect();
    let body_lines = if lines.len() >= 2 { &lines[1..lines.len()-1] } else { &lines[..] };
    let (stmts, _) = parse_effect_stmts(body_lines, 0);
    EffectScript { stmts }
}

fn parse_for_loop_header(line: &str) -> Option<usize> {
    let line = line.trim();
    if !line.starts_with("for ") || !line.contains("in 0..") { return None; }
    let range_start = line.find("in 0..")? + 6;
    let rest = &line[range_start..];
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    let num_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let count: usize = rest[..num_end].parse().ok()?;
    Some(count.min(20))
}

fn parse_frame_call(line: &str) -> Option<f32> {
    let mut search_start = 0;
    while let Some(pos) = line[search_start..].find("frame(") {
        let abs_pos = search_start + pos;
        let before = if abs_pos == 0 { ' ' } else {
            line.as_bytes().get(abs_pos - 1).copied().map(|b| b as char).unwrap_or(' ')
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
        let before = if abs_pos == 0 { ' ' } else {
            line.as_bytes().get(abs_pos - 1).copied().map(|b| b as char).unwrap_or(' ')
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

fn parse_attack_call(line: &str) -> Option<AttackCall> {
    let start = line.find("macros::ATTACK(")?;
    let inner = &line[start + "macros::ATTACK(".len()..];
    let end = inner.rfind(')')?;
    let inner = &inner[..end];
    let t = tokenize_args(inner);
    if t.len() < 13 { return None; }

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

    let id: u32       = t[1].trim().parse().ok()?;
    let part: u32     = t[2].trim().parse().ok()?;
    let bone_name     = extract_hash40_string(&t[3]).unwrap_or_else(|| t[3].trim().to_string());
    let damage: f32   = t[4].trim().parse().ok()?;
    let angle: i32    = t[5].trim().parse::<i32>()
        .or_else(|_| t[5].trim().parse::<f32>().map(|f| f as i32))
        .unwrap_or(0);
    let kb_scaling: i32 = t[6].trim().parse().ok()?;
    let fkb: i32      = t[7].trim().parse().ok()?;
    let kb_base: i32  = t[8].trim().parse().ok()?;
    let size: f32     = t[9].trim().parse().ok()?;
    let offset_x: f32 = t[10].trim().parse().ok()?;
    let offset_y: f32 = t[11].trim().parse().ok()?;
    let offset_z: f32 = t[12].trim().parse().ok()?;

    let capsule_end = if t.len() >= 16 {
        match (parse_option_f32(t[13].trim()), parse_option_f32(t[14].trim()), parse_option_f32(t[15].trim())) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => None,
        }
    } else { None };

    let get = |i: usize| t.get(i).map(|s| s.trim()).unwrap_or("");

    let hitlag_mult: f32   = get(16).parse().unwrap_or(1.0);
    let sdi_mult: f32      = get(17).parse().unwrap_or(1.0);
    let setoff_kind        = strip_deref(get(18));
    let lr_check           = strip_deref(get(19));
    let is_clang           = get(20) == "true";
    let is_add_attack: i32 = get(21).parse().unwrap_or(0);
    let hitbox_attr: f32   = get(22).parse().unwrap_or(0.0);
    let ground_or_air: i32 = get(23).parse().unwrap_or(0);
    let is_mtk             = get(24) == "true";
    let is_shield_disable  = get(25) == "true";
    let is_reflectable     = get(26) == "true";
    let is_absorbable      = get(27) == "true";
    let is_landing_attack  = get(28) == "true";
    let situation_mask     = strip_deref(get(29));
    let category_mask      = strip_deref(get(30));
    let part_mask          = strip_deref(get(31));
    let no_finish_camera   = get(32) == "true";
    let collision_attr     = extract_hash40_string(get(33)).unwrap_or_else(|| strip_deref(get(33)));
    let sound_level        = strip_deref(get(34));
    let sound_attr         = strip_deref(get(35));
    let attack_region      = strip_deref(get(36));

    Some(AttackCall {
        id, part, bone_name, damage, angle, kb_scaling, fkb, kb_base,
        size, offset_x, offset_y, offset_z, capsule_end,
        hitlag_mult, sdi_mult, setoff_kind, lr_check,
        is_clang, is_add_attack, hitbox_attr, ground_or_air,
        is_mtk, is_shield_disable, is_reflectable, is_absorbable, is_landing_attack,
        situation_mask, category_mask, part_mask, no_finish_camera,
        collision_attr, sound_level, sound_attr, attack_region,
    })
}

/// Strip leading `*` dereference from constant names like `*ATTACK_SETOFF_KIND_ON`.
fn strip_deref(s: &str) -> String {
    s.trim_start_matches('*').to_string()
}

/// Parse `Some(3.0)` → `Some(3.0)`, `None` → `None`.
fn parse_option_f32(s: &str) -> Option<f32> {
    let s = s.trim();
    if s == "None" { return None; }
    let inner = s.strip_prefix("Some(")?.strip_suffix(')')?;
    inner.trim().parse().ok()
}

fn tokenize_args(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in s.chars() {
        match ch {
            '(' => { depth += 1; current.push(ch); }
            ')' => { if depth > 0 { depth -= 1; } current.push(ch); }
            ',' if depth == 0 => {
                tokens.push(current.trim().to_string());
                current = String::new();
            }
            _ => { current.push(ch); }
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
fn emit_attack(call: &AttackCall, indent: &str) -> String {
    let bone = format!("Hash40::new(\"{}\")", call.bone_name);
    let capsule = match call.capsule_end {
        Some([x, y, z]) => format!("Some({:.1}), Some({:.1}), Some({:.1})", x, y, z),
        None => "None, None, None".to_string(),
    };
    let collision_attr = format!("Hash40::new(\"{}\")", call.collision_attr);
    format!(
        "{indent}macros::ATTACK(agent, {id}, {part}, {bone}, {dmg:.1}, {angle}, {kbs}, {fkb}, {kbb}, \
{size:.1}, {ox:.1}, {oy:.1}, {oz:.1}, {capsule}, \
{hitlag:.1}, {sdi:.1}, *{setoff}, *{lr}, {clang}, {add_atk}, {hb_attr:.1}, {goa}, \
{mtk}, {shield}, {reflect}, {absorb}, {landing}, \
*{sit}, *{cat}, *{part_mask}, {no_cam}, {col_attr}, *{snd_lvl}, *{snd_attr}, *{region});",
        indent = indent,
        id = call.id,
        part = call.part,
        bone = bone,
        dmg = call.damage,
        angle = call.angle,
        kbs = call.kb_scaling,
        fkb = call.fkb,
        kbb = call.kb_base,
        size = call.size,
        ox = call.offset_x,
        oy = call.offset_y,
        oz = call.offset_z,
        capsule = capsule,
        hitlag = call.hitlag_mult,
        sdi = call.sdi_mult,
        setoff = call.setoff_kind,
        lr = call.lr_check,
        clang = call.is_clang,
        add_atk = call.is_add_attack,
        hb_attr = call.hitbox_attr,
        goa = call.ground_or_air,
        mtk = call.is_mtk,
        shield = call.is_shield_disable,
        reflect = call.is_reflectable,
        absorb = call.is_absorbable,
        landing = call.is_landing_attack,
        sit = call.situation_mask,
        cat = call.category_mask,
        part_mask = call.part_mask,
        no_cam = call.no_finish_camera,
        col_attr = collision_attr,
        snd_lvl = call.sound_level,
        snd_attr = call.sound_attr,
        region = call.attack_region,
    )
}

fn emit_excute_stmts(stmts: &[crate::data::ExcuteStmt], indent: &str) -> Vec<String> {
    stmts.iter().map(|s| match s {
        crate::data::ExcuteStmt::Attack(call) => emit_attack(call, indent),
        crate::data::ExcuteStmt::ClearAll =>
            format!("{indent}AttackModule::clear_all(agent.module_accessor);"),
        crate::data::ExcuteStmt::Raw(line) => format!("{indent}{line}"),
    }).collect()
}

fn emit_stmts(stmts: &[crate::data::AcmdStmt], indent: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for stmt in stmts {
        match stmt {
            crate::data::AcmdStmt::Frame(f) =>
                lines.push(format!("{indent}frame(agent.lua_state_agent, {f:.1});")),
            crate::data::AcmdStmt::Wait(w) =>
                lines.push(format!("{indent}wait(agent.lua_state_agent, {w:.1});")),
            crate::data::AcmdStmt::WaitLoopClear =>
                lines.push(format!("{indent}wait_loop_clear(agent.lua_state_agent);")),
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
            crate::data::AcmdStmt::Raw(line) =>
                lines.push(format!("{indent}{line}")),
        }
    }
    lines
}

/// Emit one `unsafe extern "C" fn` for a single move and return
/// `(fn_name, source_block)`.
fn emit_move_fn(script: &crate::data::AcmdScript, move_name: &str) -> (String, String) {
    // Function name matches the ACMD script convention: game_{movename_no_underscores}
    let fn_name = format!("game_{}", move_name.to_lowercase().replace('_', "").replace(' ', ""));
    let body = emit_stmts(&script.stmts, "    ");
    let mut out = String::new();
    out.push_str(&format!("unsafe extern \"C\" fn {fn_name}(agent: &mut L2CAgentBase) {{\n"));
    for line in &body {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("}\n");
    (fn_name, out)
}

/// Build a complete, compilable skyline-rs mod project for all the provided edits.
///
/// `edits` — list of `(fighter_name, move_name, script)` tuples (all fighters combined).
/// `plugin_name` — the Cargo package name, e.g. `"my_hitbox_mod"`.
pub fn build_mod_project(
    edits: &[(String, String, crate::data::AcmdScript)],
    plugin_name: &str,
) -> ModProject {
    use std::collections::HashMap;

    // Group by fighter
    let mut by_fighter: HashMap<&str, Vec<(&str, &crate::data::AcmdScript)>> = HashMap::new();
    for (fighter, move_name, script) in edits {
        by_fighter.entry(fighter.as_str())
            .or_default()
            .push((move_name.as_str(), script));
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
"#.to_string(),
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
skyline_smash = {{ git = "https://github.com/ultimate-research/skyline-smash.git", features = ["weak_l2cvalue"] }}
smash_script = {{ git = "https://github.com/WuBoytH/smash-script.git", branch = "development" }}
smashline = {{ git = "https://github.com/hdr-development/smashline.git" }}

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

    let mod_decls: String = fighter_names.iter()
        .map(|f| format!("mod {f};\n"))
        .collect();
    let installs: String = fighter_names.iter()
        .map(|f| format!("    {f}::install();\n"))
        .collect();

    files.push(GeneratedFile {
        rel_path: "src/lib.rs".into(),
        contents: format!(
r#"// Auto-generated by SSBU Hitbox Editor
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
    for fighter in &fighter_names {
        let moves = &by_fighter[fighter];

        // src/{fighter}/mod.rs
        files.push(GeneratedFile {
            rel_path: format!("src/{fighter}/mod.rs"),
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
            let acmd_name = format!("game_{}", move_name.to_lowercase().replace('_', "").replace(' ', ""));
            acmd_src.push_str(&fn_src);
            acmd_src.push('\n');
            fn_entries.push((fn_name, acmd_name));
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
            rel_path: format!("src/{fighter}/acmd.rs"),
            contents: acmd_src,
        });
    }

    // ── README.md ─────────────────────────────────────────────────────────
    let move_list: String = edits.iter()
        .map(|(f, m, _)| format!("- {f}: {m}"))
        .collect::<Vec<_>>()
        .join("\n");

    files.push(GeneratedFile {
        rel_path: "README.md".into(),
        contents: format!(
r#"# {plugin_name}

Auto-generated hitbox mod for Super Smash Bros. Ultimate.

## Edited moves

{move_list}

## Building

Run the included build script — it handles everything automatically:

```sh
bash build.sh
```

The compiled plugin will be at:
```
target/aarch64-skyline-switch/release/lib{plugin_name}.nro
```

## Installing on your Switch

Copy the `.nro` to your SD card:
```
atmosphere/contents/01006A800016E000/romfs/skyline/plugins/lib{plugin_name}.nro
```

### Required plugins (if not already installed)
Download and place these in the same `plugins/` folder:
- [Skyline](https://github.com/skyline-dev/skyline/releases) — copy the `exefs/` folder to `atmosphere/contents/01006A800016E000/`
- [nro_hook](https://github.com/ultimate-research/nro-hook-plugin/releases) — `libnro_hook.nro`
- [smashline_hook](https://github.com/blu-dev/smashline_hook/releases) — `libsmashline_hook.nro`
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
authors = "SSBU Hitbox Editor"
version = "1.0"
description = """
Hitbox mod generated by SSBU Hitbox Editor.
Edited moves:
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
rustup default nightly

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
echo "Copy it to your SD card:"
echo "  atmosphere/contents/01006A800016E000/romfs/skyline/plugins/"
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
rustup default nightly

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
echo Copy the .nro file to:
echo   atmosphere\contents\01006A800016E000\romfs\skyline\plugins\
pause
"#.to_string(),
    });

    ModProject {
        name: plugin_name.to_string(),
        files,
    }
}

/// Convenience: export a single move as a standalone project.
/// Returns the `src/{fighter}/acmd.rs` content only — use `build_mod_project` for a full project.
pub fn export_acmd_source(
    script: &crate::data::AcmdScript,
    fighter: &str,
    move_name: &str,
) -> String {
    let edits = vec![(fighter.to_string(), move_name.to_string(), script.clone())];
    let project = build_mod_project(&edits, &format!("{fighter}_{move_name}_mod"));
    // Return all files joined with separators for single-file save (legacy path)
    project.files.iter()
        .map(|f| format!("// === {} ===\n{}", f.rel_path, f.contents))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::EffectScript;

    // ── Helper: wrap a body line in a minimal effect function ─────────────────
    fn wrap_effect_fn(body: &str) -> String {
        format!(
            "unsafe extern \"C\" fn effect_test(agent: &mut L2CAgentBase) {{\n    {body}\n}}\n"
        )
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
        assert!(!calls.is_empty(), "bare EFFECT should produce at least one EffectCall, got 0");
        assert_eq!(calls[0].effect_name, "test_effect",
            "effect_name should be 'test_effect', got '{}'", calls[0].effect_name);
    }

    /// Property 1b: bare EFFECT_FOLLOW produces EffectCall with follows_bone=true
    #[test]
    fn test_bug_condition_bare_effect_follow_follows_bone() {
        let src = wrap_effect_fn(
            r#"macros::EFFECT_FOLLOW(agent, Hash40::new("follow_eff"), Hash40::new("hip"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);"#,
        );
        let script = parse_effect_script(&src);
        let calls = script.to_effect_calls();
        assert!(!calls.is_empty(), "bare EFFECT_FOLLOW should produce EffectCall");
        assert!(calls[0].follows_bone, "EFFECT_FOLLOW should set follows_bone=true");
        assert_eq!(calls[0].effect_name, "follow_eff");
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
        assert!(!calls.is_empty(), "bare AFTER_IMAGE4_ON_arg29 should produce EffectCall");
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
        assert!(!calls.is_empty(), "is_excute-wrapped EFFECT should produce EffectCall");
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
        assert!(!calls.is_empty(), "should produce EffectCall after frame(10)");
        assert_eq!(calls[0].active_start, 10, "active_start should be 10 after frame(10)");
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
        assert!(calls.is_empty(), "non-EFFECT bare line should produce no EffectCall, got {}", calls.len());
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
        assert_eq!(calls.len(), 2, "for loop should produce 2 EffectCalls, got {}", calls.len());
        assert_eq!(calls[0].active_start, 4);
        assert_eq!(calls[1].active_start, 8);
    }
}
