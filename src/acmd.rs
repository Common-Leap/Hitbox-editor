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

/// A cached body that is really an HTTP error page rather than a script.
///
/// Until 2026-08-06 the fetch below wrote whatever came back to the cache, and a 404 from
/// `raw.githubusercontent.com` is a *successful* request whose body is the eight-word error
/// page. Those files reached the parser as script source: the move opened as a one-line script
/// reading `404: Not Found`, and because that line is kept as [`AcmdStmt::Raw`] the export wrote
/// it verbatim into a generated `.rs` file.
///
/// The fetch now checks the status, so no new file can look like this. This exists for the ones
/// already on disk — normalising them rather than deleting them, so the fix takes effect without
/// anyone clearing their cache.
fn is_cached_error_page(body: &str) -> bool {
    body.trim().starts_with("404: Not Found")
}

/// The script source a body represents: the body itself, or **empty** if it is a miss.
///
/// A miss is empty rather than [`None`] on purpose. To the fighter-wide scan `None` means "not
/// cached", so reporting a known-missing move that way would send it back to the network on
/// every scan and undo the caching this whole path exists for. Upstream never serves an empty
/// script, so an empty body is an unambiguous "this move has none".
///
/// **Every body reaching a caller goes through here, fresh off the wire or off the disk.** The
/// status check in [`fetch_script_body`] cannot be reached by a test without a network, so it is
/// deliberately not the only guard: if it regressed, this would still catch the page before it
/// reached a parser.
fn script_source_from_body(raw: &str) -> String {
    if is_cached_error_page(raw) {
        String::new()
    } else {
        raw.to_string()
    }
}

/// Cached script body, if this move was fetched before. Never touches the network — lets a
/// scan resolve every already-known move up front and spend threads only on the rest.
///
/// [`Some`] means "this move has been fetched", **not** "this move has a script": a cached miss
/// comes back as `Some("")`. See [`script_source_from_body`].
pub fn cached_script_body(fighter: &str, move_name: &str) -> Option<String> {
    cached_script_body_at(&script_cache_path(fighter, move_name))
}

/// [`cached_script_body`] against an explicit path, so a test can exercise the real read and
/// normalise without redirecting the process-wide cache directory out from under the corpus
/// tests (which would make them silently skip rather than fail).
fn cached_script_body_at(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|raw| script_source_from_body(&raw))
}

/// Fetch the raw script body text for a fighter+move from GitHub.
///
/// A move with no upstream script is an ordinary outcome, not an error, and is reported as an
/// empty body. **`send()?` does not fail on a 404** — the request succeeded, and it is the
/// status that says the file is missing, so this has to be checked explicitly.
pub fn fetch_script_body(fighter: &str, move_name: &str) -> anyhow::Result<String> {
    let pascal = move_name_to_pascal(move_name);
    let url = format!(
        "https://raw.githubusercontent.com/WuBoytH/SSBU-Dumped-Scripts/main/smashline/lua2cpp_{fighter}/{fighter}/{pascal}.txt"
    );
    let response = HTTP.get(&url).send()?;
    if !response.status().is_success() {
        return Ok(String::new());
    }
    Ok(response.text()?)
}

/// Disk-cached [`fetch_script_body`]: bodies (**including misses**, stored as an empty file) are
/// kept under `{app_storage_root}/script-cache/{fighter}/`, so fighter-wide scans (the
/// transplant studio's full-use discovery) only hit the network once per move ever.
pub fn fetch_script_body_cached(fighter: &str, move_name: &str) -> anyhow::Result<String> {
    if let Some(body) = cached_script_body(fighter, move_name) {
        return Ok(body);
    }
    // Normalised before it is stored *and* before it is returned, so the cold path and the warm
    // path cannot disagree: what goes on disk is exactly what a later read gives back.
    let body = script_source_from_body(&fetch_script_body(fighter, move_name)?);
    let path = script_cache_path(fighter, move_name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, &body);
    Ok(body)
}

pub fn parse_acmd_script(source: &str) -> AcmdScript {
    let game_fn = extract_function(source, "game_");
    // A body with no function header at all is a live capture or a paste, and is read whole —
    // that is the only reason this falls back. A source that *does* hold ACMD functions but no
    // `game_` is a different thing entirely: reading it whole pulls another category's lines in
    // as `Raw`, and `emit_stmts` writes those straight back out into the generated `game_`
    // function. A sound-only or effect-only move would export its sounds twice.
    let source = match game_fn {
        Some(ref extracted) => extracted.as_str(),
        None if holds_acmd_function(source) => return AcmdScript::default(),
        None => source,
    };
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

        // macros::FT_MOTION_RATE(agent, r) — the animation playback rate from here on. Read
        // before the block and `Raw` fallthroughs below, which is where it used to end up.
        if let Some(rate) = parse_motion_rate_call(line) {
            stmts.push(AcmdStmt::MotionRate(rate));
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

        // Any other line that opens a block — a runtime branch, its `else`, a `for` whose
        // count could not be read. The body is walked, so a `frame()` or a hitbox inside a
        // branch is still seen; the brace is kept, so the function still closes.
        if line.ends_with('{') {
            let body_start = pos + 1;
            let (body_end, _) = find_block_end(lines, pos);
            let (body, _) = parse_stmts(&lines[body_start..body_end], 0);
            stmts.push(AcmdStmt::RawBlock {
                header: line.to_string(),
                body,
            });
            pos = body_end + 1;
            continue;
        }

        // A sound played outside every `is_excute` block. Fifteen corpus `sound_` scripts end
        // this way, and the D1c oracle found them: they parsed as `Raw`, so a move whose last
        // footstep is written bare showed one fewer sound than it plays. Only this family is
        // routed here — a bare collision would need the timeline to decide what an unwrapped
        // hitbox means, which nothing has measured.
        if let Some(sound) = parse_sound_call(line) {
            stmts.push(AcmdStmt::Bare(Box::new(sound)));
            pos += 1;
            continue;
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
        // `ATTACK_FP` starts with `ATTACK` but has a different fixed 41-slot layout. Keep its
        // complete-name dispatch ahead of the ordinary family so a future addition to
        // `ATTACK_FUNCS` cannot route it through the 36-slot parser.
        if line.contains("macros::ATTACK_FP(") {
            if let Some(call) = parse_attack_fp_call(line) {
                stmts.push(ExcuteStmt::AttackFp(call));
                continue;
            }
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
        // The `macros::NAME(` form with the paren is doing real work here too:
        // `macros::SET_SEARCH_SIZE_EXIST(` does not contain `macros::SEARCH(`, so the
        // size-modifier macro cannot be read through the 17-slot box layout.
        if line.contains("macros::SEARCH(") {
            if let Some(call) = parse_search_call(line) {
                stmts.push(ExcuteStmt::Search(call));
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
        if let Some(stmt) = parse_reverse_lr_call(line) {
            stmts.push(stmt);
            continue;
        }
        if let Some(stmt) = parse_expression_call(line) {
            stmts.push(stmt);
            continue;
        }
        if let Some(stmt) = parse_sound_call(line) {
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

/// The four ACMD categories, in the order a dumped script file writes them.
///
/// Used as the set a function header is matched *against*, not just for: a header names
/// exactly one of these, so the other three appearing in the same line is what rules it out.
pub(crate) const SCRIPT_PREFIXES: [&str; 4] = ["game_", "effect_", "sound_", "expression_"];

/// The categories the editor actually puts on screen, and so the only ones worth *waiting* on
/// the mirror for.
///
/// [`merge_project_over_mirror`] fills all four when a mirror body is already in hand — that is
/// free. This shorter list decides the other question: whether a partial project override is
/// worth a network round trip. Pulling vanilla's `sound_` for text nothing displays would make
/// an offline user with a perfectly good `game_`+`effect_` project sit out the HTTP timeout
/// before seeing their own move. D2 gives `expression_` the same lane now that its measured
/// macro slice is being modelled.
///
/// `sound_` joined it at D1d, which is where sounds became editable rather than merely drawn.
/// The cost the paragraph above describes is now the right trade: a project that overrides only
/// `game_` needs the mirror's `sound_` to show what the move plays, and without it the sound
/// section is empty for exactly the projects most likely to want it.
pub(crate) const DISPLAYED_PREFIXES: [&str; 4] = ["game_", "effect_", "sound_", "expression_"];

/// Fill the categories a project does not define with the mirror's, and return one body.
///
/// `project` is carried verbatim instead of being re-extracted category by category, and that
/// asymmetry is deliberate. A project's function can be called anything and bound to a script
/// by attribute — `#[acmd_script(script = "game_attacks4")] fn my_custom_name` — so pulling it
/// apart by prefix would silently drop it on the floor. Only the mirror is safe to take apart
/// that way, because it is always dumped vanilla and always plainly named.
pub fn merge_project_over_mirror(project: &str, covered: &[&str], mirror: &str) -> String {
    let mut body = project.to_string();
    for prefix in SCRIPT_PREFIXES {
        if covered.contains(&prefix) {
            continue;
        }
        if let Some(function) = extract_function(mirror, prefix) {
            body.push_str(&function);
            body.push_str("\n\n");
        }
    }
    body
}

/// Whether `trimmed` opens the function for `prefix` and no other category.
fn is_function_header(trimmed: &str, prefix: &str) -> bool {
    SCRIPT_PREFIXES
        .iter()
        .all(|other| (*other == prefix) == trimmed.contains(other))
        && (trimmed.contains(&format!("fn {prefix}")) || trimmed.starts_with("unsafe extern"))
}

/// Whether `source` holds any ACMD function at all, of any category.
///
/// The question [`parse_acmd_script`] asks to tell a headerless body — a live capture, or text
/// the user pasted — apart from a real file that simply has no `game_` in it.
fn holds_acmd_function(source: &str) -> bool {
    source.lines().any(|line| {
        let trimmed = line.trim();
        SCRIPT_PREFIXES
            .iter()
            .any(|prefix| is_function_header(trimmed, prefix))
    })
}

/// Extract one category's function body from a source that may hold all four.
pub(crate) fn extract_function(source: &str, prefix: &str) -> Option<String> {
    let mut result = String::new();
    let mut in_fn = false;
    let mut depth: i32 = 0;
    let mut found = false;

    for line in source.lines() {
        if !in_fn && is_function_header(line.trim(), prefix) {
            in_fn = true;
            found = true;
        }
        if in_fn {
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
                let read = |i: usize| {
                    extract_hash40_string(t.get(i).map(|s| s.as_str()).unwrap_or(""))
                        .unwrap_or_else(|| {
                            t.get(i).map(|s| s.trim().to_string()).unwrap_or_default()
                        })
                };
                let effect_name = read(TRAIL_GRAPHIC_SLOT);
                let bone_name = read(TRAIL_JOINT_SLOT);
                // The second joint is read only from `_arg29`, the one spelling here that
                // `smash-script` actually declares. `AFTER_IMAGE4_ON` and `AFTER_IMAGE_ON` are
                // names no wrapper defines and no corpus script calls, so there is no signature
                // saying slot 8 is a joint in them — this branch parses them at all only so a
                // source file containing one survives a round trip. Claiming a joint there would
                // put an editable field on a call whose layout nothing establishes.
                let bone_name2 = if prefix.contains("_arg29") {
                    let name = read(TRAIL_JOINT2_SLOT);
                    (!name.is_empty()).then_some(name)
                } else {
                    None
                };
                if !effect_name.is_empty() {
                    macros.push(EffectMacro::AfterImage {
                        effect_name,
                        bone_name,
                        bone_name2,
                        raw: line.to_string(),
                    });
                    continue;
                }
            }
        }

        // The same trail, written as a raw command instead of a wrapper: there is no
        // `macros::AFTER_IMAGE3_ON` for the caller to have used. All four trail-ON calls in the
        // corpus take this form, so this branch — not the one above it — is the one that fires
        // on real vanilla code. The command id occupies slot 0, where `agent` sits in a wrapper
        // call, so the graphic and joint are read from slots 1 and 4 exactly as above.
        // Read the slots off the site the rewriter would find, not off a second, looser scan of
        // the same text — that is what keeps "the parser made a call here" and "the rewriter can
        // edit a call here" the same statement.
        if let Some(site) = crate::acmd_src::raw_trail_line(line) {
            let slot = |i: usize| {
                site.arg(line, i)
                    .map(|value| {
                        extract_hash40_string(value).unwrap_or_else(|| value.trim().to_string())
                    })
                    .unwrap_or_default()
            };
            let effect_name = slot(TRAIL_GRAPHIC_SLOT);
            if !effect_name.is_empty() {
                let bone_name2 = slot(TRAIL_JOINT2_SLOT);
                macros.push(EffectMacro::AfterImage {
                    effect_name,
                    bone_name: slot(TRAIL_JOINT_SLOT),
                    // Absent rather than empty when the call is too short to reach slot 8, so
                    // the panel offers a second joint only where there is one to rewrite.
                    bone_name2: (!bone_name2.is_empty()).then_some(bone_name2),
                    raw: line.to_string(),
                });
                continue;
            }
        }

        // AFTER_IMAGE_OFF — turns off a sword trail. Its one argument is undocumented but
        // required by the macro, so it is read and carried rather than discarded.
        if line.contains("macros::AFTER_IMAGE_OFF(") {
            let arg = try_extract("macros::AFTER_IMAGE_OFF(")
                .and_then(|t| t.get(1).and_then(|v| v.trim().parse::<f32>().ok()))
                .unwrap_or(crate::data::TRAIL_OFF_DEFAULT);
            macros.push(EffectMacro::AfterImageOff { arg });
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
            || crate::acmd_src::raw_trail_line(line).is_some()
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
    let effect_fn = extract_function(source, "effect_");
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

/// Parse the `sound_` function's timeline, with every command left as `Raw`.
///
/// The structure — `frame`, `wait`, `is_excute` blocks, branches — is read by the same walker
/// the `game_` script uses, because it is the same shape; only the calls inside the blocks
/// differ, and none of them is typed yet. That is deliberate: this pass exists to prove the
/// round trip is byte-exact before anything starts rewriting sound lines. See the `sound_`
/// entry in `TODO.md` for the staging.
///
/// `AcmdScript::to_hitboxes` and `to_hurtboxes` are meaningless on the result — a sound script
/// has no collisions — and nothing calls them on it.
///
/// Called only by its own corpus gate for now, which is the whole point of landing it on its
/// own: the round trip has to be proven before a panel or an export is built on top of it. The
/// generated-source pane is not that consumer — it promises what an export would write, and an
/// export does not write sound scripts yet.
#[allow(dead_code)]
pub fn parse_sound_script(source: &str) -> AcmdScript {
    let Some(sound_fn) = extract_function(source, "sound_") else {
        return AcmdScript::default();
    };
    let lines: Vec<&str> = sound_fn.lines().collect();
    let body_lines = if lines.len() >= 2 {
        &lines[1..lines.len() - 1]
    } else {
        &lines[..]
    };
    let (stmts, _) = parse_stmts(body_lines, 0);
    AcmdScript { stmts }
}

/// Parse the `expression_` function's measured camera/rumble calls.
pub fn parse_expression_script(source: &str) -> AcmdScript {
    let Some(expression_fn) = extract_function(source, "expression_") else {
        return AcmdScript::default();
    };
    let lines: Vec<&str> = expression_fn.lines().collect();
    let body_lines = if lines.len() >= 2 {
        &lines[1..lines.len() - 1]
    } else {
        &lines[..]
    };
    let (stmts, _) = parse_stmts(body_lines, 0);
    AcmdScript { stmts }
}

/// Re-emit a `sound_` function body at the corpus's own indentation.
///
/// The inverse of [`parse_sound_script`] up to the function header and closing brace, which the
/// caller owns. Fidelity over the corpus is asserted by
/// `every_sound_script_in_the_corpus_survives_a_round_trip`.
#[allow(dead_code)]
pub fn emit_sound_body(script: &AcmdScript) -> String {
    emit_stmts(&script.stmts, "    ")
        .into_iter()
        .map(|line| format!("{line}\n"))
        .collect()
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

/// `macros::FT_MOTION_RATE(agent, 0.6);` → `0.6`.
///
/// **Matched with its opening parenthesis, never as a prefix.** `FT_MOTION_RATE_RANGE` begins
/// with this macro's entire name and takes a different argument list, so a
/// `contains("FT_MOTION_RATE")` would read one as the other and write back a call the game does
/// not have. That is the family-prefix collision this codebase has already paid for once with
/// `ATTACK`/`ATTACK_ABS`.
///
/// `FT_MOTION_RATE_RANGE` and `FT_DESIRED_RATE` have **zero** calls between them in the corpus,
/// so neither is modelled and both stay [`AcmdStmt::Raw`] — deliberately, because there is no
/// sample to check a parse of them against.
fn parse_motion_rate_call(line: &str) -> Option<f32> {
    let (_, rest) = line.split_once("FT_MOTION_RATE(")?;
    let (inner, _) = rest.split_once(')')?;
    let (agent, rate) = inner.split_once(',')?;
    // The first argument is the agent in every corpus call. Anything else is a shape this has
    // not seen, and guessing at it is how a wrong value reaches the export.
    (agent.trim() == "agent").then(|| rate.trim().parse::<f32>().ok())?
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
        t.get(7).and_then(|v| parse_capsule_coord(v.trim())),
        t.get(8).and_then(|v| parse_capsule_coord(v.trim())),
        t.get(9).and_then(|v| parse_capsule_coord(v.trim())),
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

/// Parse `macros::SEARCH(agent, id, part, bone, size, x, y, z, x2, y2, z2, collision_kind,
/// hit_status, unk, ground_air, collision_category, collision_parts, unk2)`.
///
/// Dumped in the same two shapes as `CATCH` — the Lua-derived scripts omit the three capsule
/// arguments rather than writing `None` — so the tail is located with [`is_capsule_slot`]
/// rather than by slot number. 4 of the corpus's 7 calls are the short form.
fn parse_search_call(line: &str) -> Option<crate::data::SearchCall> {
    let start = line.find("macros::SEARCH(")?;
    let inner = &line[start + "macros::SEARCH(".len()..];
    let end = inner.rfind(')')?;
    let t = tokenize_args(&inner[..end]);
    if t.len() < 8 {
        return None;
    }

    // [0]=agent [1]=id [2]=part [3]=bone [4]=size [5]=x [6]=y [7]=z, then either
    // [8..=10]=capsule and the tail at 11, or the tail straight away at 8.
    let num = |i: usize, default: f32| {
        t.get(i)
            .and_then(|value| value.trim().parse::<f32>().ok())
            .unwrap_or(default)
    };
    let has_capsule_slots = t.get(8).is_some_and(|v| is_capsule_slot(v));
    let capsule_end = match (
        t.get(8).and_then(|v| parse_capsule_coord(v.trim())),
        t.get(9).and_then(|v| parse_capsule_coord(v.trim())),
        t.get(10).and_then(|v| parse_capsule_coord(v.trim())),
    ) {
        (Some(x), Some(y), Some(z)) if has_capsule_slots => Some([x, y, z]),
        _ => None,
    };
    let tail = if has_capsule_slots { 11 } else { 8 };
    let konst = |i: usize, default: &str| {
        t.get(i)
            .map(|v| strip_deref(v.trim()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    Some(crate::data::SearchCall {
        id: t[1].trim().parse().ok()?,
        part: t[2].trim().parse().ok()?,
        bone_name: extract_hash40_string(&t[3]).unwrap_or_else(|| t[3].trim().to_string()),
        size: num(4, 1.0),
        offset_x: num(5, 0.0),
        offset_y: num(6, 0.0),
        offset_z: num(7, 0.0),
        capsule_end,
        situation_mask: konst(tail + 3, "COLLISION_SITUATION_MASK_GA"),
        category_mask: konst(tail + 4, "COLLISION_CATEGORY_MASK_ALL"),
        part_mask: konst(tail + 5, "COLLISION_PART_MASK_ALL"),
        extras: crate::data::SearchExtras {
            collision_kind: konst(tail, crate::data::SEARCH_DEFAULT_COLLISION_KIND),
            hit_status: konst(tail + 1, crate::data::SEARCH_DEFAULT_HIT_STATUS),
            unk: t
                .get(tail + 2)
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(0),
            unk2: t
                .get(tail + 6)
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
        },
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

/// Parse `macros::ATTACK_FP(...)`, whose 41 arguments after `agent` are not the ordinary
/// `ATTACK` layout. The family has no corpus oracle, so the slot count is enforced directly
/// against the `smash-script` declaration and every token is retained for lossless export.
fn parse_attack_fp_call(line: &str) -> Option<crate::data::AttackFpCall> {
    const NEEDLE: &str = "macros::ATTACK_FP(";
    let start = line.find(NEEDLE)? + NEEDLE.len();
    let end = line[start..].rfind(')')? + start;
    let args = tokenize_args(&line[start..end]);
    let args = args.get(1..)?;
    if args.len() != crate::data::ATTACK_FP_ARGC {
        return None;
    }
    Some(crate::data::AttackFpCall {
        args: args
            .iter()
            .map(|arg| crate::data::AttackFpArg::Source(arg.clone()))
            .collect(),
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
/// The `PLAY_SE` family: macro name, how many `Hash40` arguments it takes, and whether one
/// more argument follows them.
///
/// Arities are read off `smash-script`'s `macros.rs`, not inferred from the call sites — a
/// member whose corpus calls all happen to be the same length would otherwise pin the wrong
/// signature. `SET_PLAY_INHIVIT` is here rather than with the effect-lifetime commands for
/// that reason: its second argument is a `ToF32` suppression window, and all ten of its corpus
/// calls are in `sound_` functions.
///
/// Matched with the trailing paren, never on the bare name. `PLAY_SE` is a prefix of
/// `PLAY_SE_NO_3D` and `PLAY_SE_REMAIN`, and `PLAY_STEP` of `PLAY_STEP_FLIPPABLE`; a bare
/// `contains` would read a two-hash call through the one-hash layout and drop its second
/// sound. This is the same collision `ATTACK` and `ATTACK_ABS` have, and the paren is the
/// same fix.
pub(crate) const SOUND_FUNCS: &[(&str, usize, bool)] = &[
    ("PLAY_SE", 1, false),
    ("PLAY_SE_NO_3D", 1, false),
    ("PLAY_SE_REMAIN", 1, false),
    ("STOP_SE", 1, false),
    ("PLAY_STEP", 1, false),
    ("PLAY_STEP_FLIPPABLE", 2, false),
    ("PLAY_SEQUENCE", 1, false),
    ("PLAY_STATUS", 1, false),
    ("PLAY_LANDING_SE", 1, false),
    ("PLAY_DOWN_SE", 1, false),
    ("PLAY_FLY_VOICE", 2, false),
    ("SET_PLAY_INHIVIT", 1, true),
];

/// Read one `PLAY_SE`-family call, or `None` to leave the line as `Raw`.
///
/// Refuses anything it could not write back byte for byte: a hash argument that is not a
/// literal `Hash40::new("…")`, or an argument count the signature does not have. Every one of
/// the corpus's 610 calls passes a literal, but a hand-written project is free to pass a
/// variable, and a typed call is *regenerated* on export rather than copied — so a form this
/// emitter cannot spell has to stay verbatim text instead.
fn parse_sound_call(line: &str) -> Option<ExcuteStmt> {
    let (func, hashes, has_tail) = SOUND_FUNCS
        .iter()
        .copied()
        .find(|(name, _, _)| line.contains(&format!("macros::{name}(")))?;
    let needle = format!("macros::{func}(");
    let start = line.find(&needle)? + needle.len();
    let end = line[start..].rfind(')')? + start;
    let tokens = tokenize_args(&line[start..end]);
    // Drop the leading `agent`, then require exactly the signature's arguments — no more, so
    // an argument this parser has no field for cannot vanish on the way back out.
    let args = tokens.get(1..)?;
    if args.len() != hashes + usize::from(has_tail) {
        return None;
    }
    let sounds: Vec<String> = args[..hashes]
        .iter()
        .map(|arg| extract_hash40_string(arg))
        .collect::<Option<_>>()?;
    Some(ExcuteStmt::Sound(crate::data::SoundCall {
        func: func.to_string(),
        sounds,
        tail: has_tail.then(|| args[hashes].clone()),
    }))
}

/// Read one of the three measured expression calls, or leave the line as `Raw`.
///
/// The expression family has several similarly named raw module calls beside these macros, so
/// the exact `macros::NAME(` spelling and exact arity are both required. Arguments stay as
/// tokens: source constants and live numeric captures must each survive an export.
fn parse_expression_call(line: &str) -> Option<ExcuteStmt> {
    let args = |name: &str| -> Option<Vec<String>> {
        let needle = format!("macros::{name}(");
        let start = line.find(&needle)? + needle.len();
        let end = line[start..].rfind(')')? + start;
        let tokens = tokenize_args(&line[start..end]);
        tokens.get(1..).map(ToOwned::to_owned)
    };

    if line.contains("macros::RUMBLE_HIT(") {
        let values = args("RUMBLE_HIT")?;
        let [kind, unk] = values.as_slice() else {
            return None;
        };
        return Some(ExcuteStmt::Expression(
            crate::data::ExpressionCall::RumbleHit {
                kind: kind.clone(),
                unk: unk.clone(),
            },
        ));
    }
    if line.contains("macros::QUAKE(") {
        let values = args("QUAKE")?;
        let [kind] = values.as_slice() else {
            return None;
        };
        return Some(ExcuteStmt::Expression(crate::data::ExpressionCall::Quake {
            kind: kind.clone(),
        }));
    }
    if line.contains("macros::FT_ATTACK_ABS_CAMERA_QUAKE(") {
        let values = args("FT_ATTACK_ABS_CAMERA_QUAKE")?;
        let [attack_abs_kind, quake_kind] = values.as_slice() else {
            return None;
        };
        return Some(ExcuteStmt::Expression(
            crate::data::ExpressionCall::FtAttackAbsCameraQuake {
                attack_abs_kind: attack_abs_kind.clone(),
                quake_kind: quake_kind.clone(),
            },
        ));
    }
    None
}

/// Parse the argument-less `macros::REVERSE_LR(agent)` facing-direction command.
fn parse_reverse_lr_call(line: &str) -> Option<ExcuteStmt> {
    let needle = "macros::REVERSE_LR(";
    let start = line.find(needle)? + needle.len();
    let end = line[start..].rfind(')')? + start;
    let tokens = tokenize_args(&line[start..end]);
    (tokens.as_slice() == ["agent"]).then_some(ExcuteStmt::ReverseLr)
}

fn emit_sound(call: &crate::data::SoundCall, indent: &str) -> String {
    let args = call
        .sounds
        .iter()
        .map(|name| format!("Hash40::new(\"{name}\")"))
        .chain(call.tail.clone())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{indent}macros::{}(agent, {args});", call.func)
}

fn emit_expression(call: &crate::data::ExpressionCall, indent: &str) -> String {
    match call {
        crate::data::ExpressionCall::RumbleHit { kind, unk } => {
            format!("{indent}macros::{}(agent, {kind}, {unk});", call.func())
        }
        crate::data::ExpressionCall::Quake { kind } => {
            format!("{indent}macros::{}(agent, {kind});", call.func())
        }
        crate::data::ExpressionCall::FtAttackAbsCameraQuake {
            attack_abs_kind,
            quake_kind,
        } => format!(
            "{indent}macros::{}(agent, {attack_abs_kind}, {quake_kind});",
            call.func()
        ),
    }
}

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

/// One capsule endpoint coordinate, in any of the three spellings that occur.
///
/// `Some(1.0)` and `None` are the smashline signature's. The Lua-derived dumps write the
/// coordinate **bare** — `macros::SEARCH(.., 0.0, 7.0, 13.0, *COLLISION_KIND_MASK_ATTACK, ..)` —
/// so a reader that only understood `Some(..)` would see a stretched box as spherical and
/// quietly shorten it to a point.
///
/// Only ever called on a slot [`is_capsule_slot`] has already claimed, which is what keeps a
/// bare number here from swallowing the argument that follows an omitted capsule.
fn parse_capsule_coord(s: &str) -> Option<f32> {
    let s = s.trim();
    if s == "None" {
        return None;
    }
    match s.strip_prefix("Some(").and_then(|v| v.strip_suffix(')')) {
        Some(inner) => inner.trim().parse().ok(),
        None => s.parse().ok(),
    }
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

/// Hash40 renderer shared by the FP IR conversion and this module's emitters.
pub(crate) fn hash40_expr_for_data(attr: &str) -> String {
    hash40_expr(attr)
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

/// Emit `macros::ATTACK_FP` from its own complete slot table. Unsupported or unknown slots are
/// emitted from the preserved token/type rather than reconstructed from ordinary ATTACK fields.
fn emit_attack_fp(call: &crate::data::AttackFpCall, indent: &str) -> String {
    let args = call
        .args
        .iter()
        .map(crate::data::AttackFpArg::source)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{indent}macros::ATTACK_FP(agent, {args});")
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

/// Emit `macros::SEARCH`, in its own slot order.
///
/// Always the full form, with the capsule spelled `Some(..)`/`None`, even when the call was
/// read from the shorter Lua-shaped dump. That shorter form is not valid Rust — `macros::SEARCH`
/// is fixed-arity — so a mod exported in it would not build. Same choice `emit_catch` makes.
fn emit_search(call: &crate::data::SearchCall, indent: &str) -> String {
    let bone = format!("Hash40::new(\"{}\")", call.bone_name.to_ascii_lowercase());
    let capsule = match call.capsule_end {
        Some([x, y, z]) => format!("Some({}), Some({}), Some({})", num(x), num(y), num(z)),
        None => "None, None, None".to_string(),
    };
    format!(
        "{indent}macros::SEARCH(agent, {id}, {part}, {bone}, {size}, {x}, {y}, {z}, \
{capsule}, {kind}, {status}, {unk}, {situation}, {category}, {parts}, {unk2});",
        id = call.id,
        part = call.part,
        size = num(call.size),
        x = num(call.offset_x),
        y = num(call.offset_y),
        z = num(call.offset_z),
        kind = const_expr(&call.extras.collision_kind),
        status = const_expr(&call.extras.hit_status),
        unk = call.extras.unk,
        situation = const_expr(&call.situation_mask),
        category = const_expr(&call.category_mask),
        parts = const_expr(&call.part_mask),
        unk2 = call.extras.unk2,
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
            crate::data::ExcuteStmt::AttackFp(call) => emit_attack_fp(call, indent),
            crate::data::ExcuteStmt::Search(call) => emit_search(call, indent),
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
            crate::data::ExcuteStmt::Sound(call) => emit_sound(call, indent),
            crate::data::ExcuteStmt::Expression(call) => emit_expression(call, indent),
            crate::data::ExcuteStmt::ReverseLr => {
                format!("{indent}macros::REVERSE_LR(agent);")
            }
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
            crate::data::AcmdStmt::MotionRate(rate) => lines.push(format!(
                "{indent}macros::FT_MOTION_RATE(agent, {});",
                num(*rate)
            )),
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
            // The header is carried verbatim — it can be any Rust the dumper wrote, including
            // a raw address call — and the brace it opens is closed here.
            crate::data::AcmdStmt::RawBlock { header, body } => {
                lines.push(format!("{indent}{header}"));
                lines.extend(emit_stmts(body, &format!("{indent}    ")));
                lines.push(format!("{indent}}}"));
            }
            // At the caller's own indent and with no wrapper: the source wrote this command
            // outside every `is_excute` block, and adding one back would change when it runs.
            crate::data::AcmdStmt::Bare(inner) => lines.extend(emit_excute_stmts(
                std::slice::from_ref(inner.as_ref()),
                indent,
            )),
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

/// Emit one `sound_` function: its name and its full source.
///
/// Shares [`emit_stmts`] with [`emit_move_fn`] rather than wrapping [`emit_sound_body`], so the
/// two categories cannot drift in how they spell a statement. The only difference between them
/// is the prefix on the function name — which is the whole of what the game uses to decide
/// which of a fighter's four scripts this one replaces.
fn emit_sound_move_fn(script: &crate::data::AcmdScript, move_name: &str) -> (String, String) {
    let fn_name = script_function_name("sound", move_name);
    let mut out = format!("unsafe extern \"C\" fn {fn_name}(agent: &mut L2CAgentBase) {{\n");
    for line in emit_stmts(&script.stmts, "    ") {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("}\n");
    (fn_name, out)
}

/// Emit one `expression_` function: its name and full source.
fn emit_expression_move_fn(script: &crate::data::AcmdScript, move_name: &str) -> (String, String) {
    let fn_name = script_function_name("expression", move_name);
    let mut out = format!("unsafe extern \"C\" fn {fn_name}(agent: &mut L2CAgentBase) {{\n");
    for line in emit_stmts(&script.stmts, "    ") {
        out.push_str(&line);
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
/// first texture stands in for the effect name, and arguments 4 and 8 are the two joints the
/// ribbon is stretched between. Matches what `parse_excute_block_effects` reads back out of
/// the same call.
///
/// Slots 4 and 8 are `trail_bone1` and `trail_bone2` on the strength of the `smash-script`
/// declaration, not of the corpus — every vanilla call writes the same joint into both (and
/// into `flare_bone` at 14), so no real call can tell them apart. Anything asserting which of
/// these is which must vary the values itself; a corpus-shaped fixture agrees with a swap.
const TRAIL_GRAPHIC_SLOT: usize = 1;
const TRAIL_JOINT_SLOT: usize = 4;
const TRAIL_JOINT2_SLOT: usize = 8;

/// Re-render the trail arguments the panels let the user change.
///
/// The rest of a trail call is textures and per-frame trail parameters that no editor field
/// maps to, so it rides along untouched — but the graphic and the two joints ARE editable, and
/// replaying the line unconditionally dropped those edits with nothing to say so. Only slots
/// whose value actually differs are spliced, so an untouched trail comes back byte-identical
/// and a round trip through the emitter still reproduces the original line exactly.
///
/// The second joint is spliced only when the call carried one. A trail whose layout the parser
/// would not vouch for stores `None`, and writing slot 8 of such a call would edit whatever
/// argument happens to sit there.
fn retarget_trail_line(raw: &str, call: &crate::data::EffectCall) -> String {
    let Some(site) = crate::acmd_src::scan_macro_sites(raw, 0..raw.len())
        .into_iter()
        .find(|site| site.name.starts_with("AFTER_IMAGE"))
    else {
        return raw.to_string();
    };

    let mut out = raw.to_string();
    // Descending, so an earlier splice cannot shift a later slot's span out from under it.
    let slots = [
        call.trail_bone2
            .as_ref()
            .map(|bone| (TRAIL_JOINT2_SLOT, bone.to_ascii_lowercase())),
        Some((TRAIL_JOINT_SLOT, call.bone_name.to_ascii_lowercase())),
        Some((TRAIL_GRAPHIC_SLOT, call.effect_name.clone())),
    ];
    for (slot, wanted) in slots.into_iter().flatten() {
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
        // The argument is not optional. `macros::AFTER_IMAGE_OFF` is declared
        // `<F: ToF32>(agent, unk: F)`, so the bare call this used to emit does not compile —
        // every exported move with a sword trail was an unbuildable project.
        //
        // `attack_mod_num`, not `num`: the slot is `ToF32`-generic, so both `0` and `0.0`
        // compile, and all four vanilla calls write the bare integer. See B3.
        let arg = call.trail_off.unwrap_or(crate::data::TRAIL_OFF_DEFAULT);
        return format!(
            "{indent}macros::AFTER_IMAGE_OFF(agent, {});\n",
            attack_mod_num(arg)
        );
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
/// `residue` is the second half of
/// [`EffectScript::to_effect_calls_and_residue`](crate::data::EffectScript::to_effect_calls_and_residue):
/// wrapped lines that belong to a frame rather than to any one call, because their frame block
/// held no spawn for them to ride on. There is no defaulted overload on purpose — passing an
/// empty map is a claim, and a caller that made it by accident would delete those lines exactly
/// as the export did before E3.
fn emit_effect_move_fn(
    calls: &[crate::data::EffectCall],
    move_name: &str,
    tweaks: &std::collections::HashMap<u64, crate::mod_project::LiveTweak>,
    residue: &std::collections::BTreeMap<u32, Vec<String>>,
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
    // A frame can exist because of residue alone — the source wrote a block there and this
    // editor modelled nothing in it. Give it an entry so the loop below writes its `frame()`.
    for frame in residue.keys() {
        events.entry(*frame).or_default();
    }

    let mut out = String::new();
    out.push_str(&format!(
        "unsafe extern \"C\" fn {fn_name}(agent: &mut L2CAgentBase) {{\n"
    ));
    for (frame, (stops, starts)) in events {
        out.push_str(&format!("    frame(agent.lua_state_agent, {frame}.0);\n"));

        // Frame-anchored residue comes first: it was written before anything this editor
        // modelled, because the only reason it is here rather than on a call is that the walk
        // reached the end of the frame block without finding a spawn to hand it to. It arrives
        // with its own `is_excute` wrapper and, where the source had one, its own guard.
        if let Some(lines) = residue.get(&frame) {
            push_carried(&mut out, lines, "    ");
        }

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
        // Gated on there being a stop to place: a frame that exists only because of residue has
        // neither, and an unconditional fallback would open an empty `is_excute` block after it.
        if !stops.is_empty() && !runs.iter().any(|r| matches!(r, Run::Spawns { .. })) {
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

/// The exact `sound_*` function an export would write for this move.
pub fn preview_sound_fn(script: &crate::data::AcmdScript, move_name: &str) -> String {
    emit_sound_move_fn(script, move_name).1
}

/// The exact `expression_*` function an export would write for this move.
pub fn preview_expression_fn(script: &crate::data::AcmdScript, move_name: &str) -> String {
    emit_expression_move_fn(script, move_name).1
}

/// The exact `effect_*` function an export would write for this move.
pub fn preview_effect_fn(
    calls: &[crate::data::EffectCall],
    move_name: &str,
    live_tweaks: &[crate::mod_project::LiveTweak],
    residue: &std::collections::BTreeMap<u32, Vec<String>>,
) -> String {
    let tweaks = live_tweaks
        .iter()
        .map(|t| (tweak_hash(&t.effect_name), t.clone()))
        .collect();
    emit_effect_move_fn(calls, move_name, &tweaks, residue).1
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
/// - **A nested conditional's header.** One guard per spawn is modelled; an inner one would
///   have to overwrite the outer, so it is reported instead.
///
/// A third source used to feed this list and no longer does: residue whose frame block held no
/// spawn to ride on. E3 gave those lines a frame of their own in the output instead of a report
/// — see [`crate::data::EffectScript::to_effect_calls_and_residue`]. This function is therefore
/// decided entirely from the statement tree now, which is why it does not consult the walk.
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
    build_mod_project_full(edits, &[], &[], &[], plugin_name)
}

/// One move's effect script as an export needs it: `(fighter, move, full edited call list,
/// frame-anchored residue)`.
///
/// The fourth element is the second half of
/// [`EffectScript::to_effect_calls_and_residue`](crate::data::EffectScript::to_effect_calls_and_residue),
/// carried this far because nothing downstream can recover it — the emitter regenerates the
/// whole function from the call list, and these lines are not in it. It is a tuple rather than a
/// struct to stay the shape `sound_edits` and `edits` already are.
pub type EffectExport = (
    String,
    String,
    Vec<crate::data::EffectCall>,
    std::collections::BTreeMap<u32, Vec<String>>,
);

/// Like [`build_mod_project`] but also generates `effect_*` and `sound_*` ACMD scripts.
///
/// `effect_edits` — see [`EffectExport`]; the generated effect script
/// REPLACES the move's original effect script, so the list must be the complete set of
/// calls (pristine + user edits applied), not just the changed ones.
///
/// `sound_edits` — `(fighter, move, whole sound script)`, on the same terms and for the same
/// reason: a `sound_` function the plugin installs replaces the fighter's own, so a partial one
/// would delete every call it left out. Passing only *changed* moves is correct and passing
/// only *changed calls* is not.
pub fn build_mod_project_full(
    edits: &[(String, String, crate::data::AcmdScript)],
    effect_edits: &[EffectExport],
    sound_edits: &[(String, String, crate::data::AcmdScript)],
    live_tweaks: &[crate::mod_project::LiveTweak],
    plugin_name: &str,
) -> ModProject {
    build_mod_project_full_with_expression(
        edits,
        effect_edits,
        sound_edits,
        &[],
        live_tweaks,
        plugin_name,
    )
}

/// Like [`build_mod_project_full`] with editable `expression_` scripts included.
pub fn build_mod_project_full_with_expression(
    edits: &[(String, String, crate::data::AcmdScript)],
    effect_edits: &[EffectExport],
    sound_edits: &[(String, String, crate::data::AcmdScript)],
    expression_edits: &[(String, String, crate::data::AcmdScript)],
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
    #[allow(clippy::type_complexity)]
    let mut fx_by_fighter: HashMap<
        &str,
        Vec<(
            &str,
            &Vec<crate::data::EffectCall>,
            &std::collections::BTreeMap<u32, Vec<String>>,
        )>,
    > = HashMap::new();
    for (fighter, move_name, calls, residue) in effect_edits {
        fx_by_fighter.entry(fighter.as_str()).or_default().push((
            move_name.as_str(),
            calls,
            residue,
        ));
        // Ensure the fighter appears even with no hitbox edits.
        by_fighter.entry(fighter.as_str()).or_default();
    }
    let mut sfx_by_fighter: HashMap<&str, Vec<(&str, &crate::data::AcmdScript)>> = HashMap::new();
    for (fighter, move_name, script) in sound_edits {
        sfx_by_fighter
            .entry(fighter.as_str())
            .or_default()
            .push((move_name.as_str(), script));
        by_fighter.entry(fighter.as_str()).or_default();
    }
    let mut expr_by_fighter: HashMap<&str, Vec<(&str, &crate::data::AcmdScript)>> = HashMap::new();
    for (fighter, move_name, script) in expression_edits {
        expr_by_fighter
            .entry(fighter.as_str())
            .or_default()
            .push((move_name.as_str(), script));
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
            sorted_fx.sort_by_key(|(m, _, _)| *m);
            for (move_name, calls, residue) in &sorted_fx {
                let (fn_name, fn_src) = emit_effect_move_fn(calls, move_name, &tweaks, residue);
                acmd_src.push_str(&fn_src);
                acmd_src.push('\n');
                fn_entries.push((fn_name.clone(), fn_name));
            }
        }

        // Sound scripts for this fighter.
        if let Some(sfx_moves) = sfx_by_fighter.get(fighter) {
            let mut sorted_sfx = sfx_moves.clone();
            sorted_sfx.sort_by_key(|(m, _)| *m);
            for (move_name, script) in &sorted_sfx {
                let (fn_name, fn_src) = emit_sound_move_fn(script, move_name);
                acmd_src.push_str(&fn_src);
                acmd_src.push('\n');
                fn_entries.push((fn_name.clone(), fn_name));
            }
        }

        // Expression scripts for this fighter.
        if let Some(expression_moves) = expr_by_fighter.get(fighter) {
            let mut sorted_expression = expression_moves.clone();
            sorted_expression.sort_by_key(|(m, _)| *m);
            for (move_name, script) in &sorted_expression {
                let (fn_name, fn_src) = emit_expression_move_fn(script, move_name);
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
    let mut script_list: Vec<String> =
        edits
            .iter()
            .map(|(fighter, move_name, _)| format!("- {fighter}: {move_name} (hitboxes)"))
            .chain(effect_edits.iter().map(|(fighter, move_name, _, _)| {
                format!("- {fighter}: {move_name} (effect spawns)")
            }))
            .chain(
                sound_edits
                    .iter()
                    .map(|(fighter, move_name, _)| format!("- {fighter}: {move_name} (sounds)")),
            )
            .chain(
                expression_edits.iter().map(|(fighter, move_name, _)| {
                    format!("- {fighter}: {move_name} (expression)")
                }),
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
pub(crate) mod tests {
    use super::*;

    /// The test with teeth for a newly modelled macro: **the typed thing comes out at all.**
    ///
    /// A round-trip oracle cannot show this. An unmodelled line is kept as `Raw` and emitted
    /// verbatim, so it round-trips *perfectly* — every export test was green on
    /// `FT_MOTION_RATE` both before and after it was given a parse arm. Green means "nothing
    /// broke", never "the new family works".
    #[test]
    fn a_motion_rate_call_parses_to_a_typed_statement_and_not_to_raw() {
        let script = parse_acmd_script(
            "unsafe extern \"C\" fn game_attackhi4(agent: &mut L2CAgentBase) {\n\
                 frame(agent.lua_state_agent, 9.0);\n\
                 macros::FT_MOTION_RATE(agent, 0.6);\n\
             }\n",
        );
        assert!(
            matches!(
                script.stmts.as_slice(),
                [
                    crate::data::AcmdStmt::Frame(f),
                    crate::data::AcmdStmt::MotionRate(r),
                ] if *f == 9.0 && *r == 0.6
            ),
            "expected a typed rate statement, got {:?}",
            script.stmts
        );
    }

    /// `REVERSE_LR` is a point statement: it has no editable payload, but it must still be
    /// typed so the frame walk, live rule path, and source writer can all see the same event.
    #[test]
    fn a_reverse_lr_call_parses_to_a_typed_point_and_round_trips() {
        let source = r#"unsafe extern "C" fn game_throwf(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::REVERSE_LR(agent);
    }
}
"#;
        let script = parse_acmd_script(source);
        assert_eq!(
            script.to_reverse_lr_events(),
            vec![crate::data::ReverseLrEvent { frame: 4, site: 0 }]
        );
        assert!(
            script.stmts.iter().any(|stmt| matches!(
                stmt,
                crate::data::AcmdStmt::Excute(inner)
                    if matches!(inner.as_slice(), [crate::data::ExcuteStmt::ReverseLr])
            )),
            "the call must be typed inside its execute block: {:?}",
            script.stmts
        );

        assert!(
            parse_reverse_lr_call("macros::REVERSE_LR(agent, true);").is_none(),
            "a different arity must remain raw until its own measured signature exists"
        );
        let emitted = preview_game_fn(&script, "throw_f");
        assert!(emitted.contains("macros::REVERSE_LR(agent);"));
        assert_eq!(
            parse_acmd_script(&emitted).to_reverse_lr_events(),
            script.to_reverse_lr_events()
        );
    }

    /// `FT_MOTION_RATE_RANGE` begins with `FT_MOTION_RATE`'s entire name and takes a different
    /// argument list, so a prefix or `contains` match reads one as the other and writes back a
    /// call the game does not have. This codebase has paid for that shape once already with
    /// `ATTACK` / `ATTACK_ABS`.
    ///
    /// Both of these have **zero** corpus calls, so they stay `Raw` on purpose — there is no
    /// sample to check a parse of them against. If one is ever modelled, this test should
    /// change rather than be deleted.
    ///
    /// **The third line is synthetic and is the one with teeth.** Mutation showed that the two
    /// real calls are rejected by their *argument count*, not by their name — replacing the
    /// parenthesised match with a prefix match still passes on them, because `5.0, 10.0, 0.5`
    /// fails to parse as one `f32`. That makes the real guard accidental. A two-argument macro
    /// whose name merely starts with `FT_MOTION_RATE` isolates the name match itself, which is
    /// what the parser actually relies on.
    #[test]
    fn the_longer_rate_macros_are_not_read_as_a_motion_rate() {
        for line in [
            "    macros::FT_MOTION_RATE_RANGE(agent, 5.0, 10.0, 0.5);",
            "    macros::FT_DESIRED_RATE(agent, 12.0);",
            "    macros::FT_MOTION_RATE_SYNTHETIC(agent, 0.5);",
        ] {
            assert_eq!(
                parse_motion_rate_call(line),
                None,
                "{line} must not be read as FT_MOTION_RATE"
            );
            let script = parse_acmd_script(&format!(
                "unsafe extern \"C\" fn game_x(agent: &mut L2CAgentBase) {{\n{line}\n}}\n"
            ));
            assert!(
                matches!(script.stmts.as_slice(), [crate::data::AcmdStmt::Raw(_)]),
                "{line} should still be kept verbatim, got {:?}",
                script.stmts
            );
        }
    }

    /// Every `FT_MOTION_RATE` in the corpus, parsed and written back.
    ///
    /// This is the round-trip half, and it is worth having *given* the test above: together
    /// they say the call is now typed **and** that typing it did not change what is exported.
    #[test]
    fn every_corpus_motion_rate_call_survives_being_typed() {
        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }
        let mut seen = 0usize;
        for (path, body) in &bodies {
            for func in body.split_inclusive("\n}\n") {
                let script = parse_acmd_script(func);
                let rates: Vec<f32> = script
                    .stmts
                    .iter()
                    .filter_map(|s| match s {
                        crate::data::AcmdStmt::MotionRate(r) => Some(*r),
                        _ => None,
                    })
                    .collect();
                if rates.is_empty() {
                    continue;
                }
                seen += rates.len();
                let emitted = preview_game_fn(&script, "audit");
                let reparsed: Vec<f32> = parse_acmd_script(&emitted)
                    .stmts
                    .iter()
                    .filter_map(|s| match s {
                        crate::data::AcmdStmt::MotionRate(r) => Some(*r),
                        _ => None,
                    })
                    .collect();
                assert_eq!(rates, reparsed, "{path} lost or changed a rate on export");
            }
        }
        // Guard the oracle with what it claims to test: if the corpus stops containing rate
        // calls, or the parse arm regresses, this test would otherwise pass having checked
        // nothing at all.
        assert!(
            seen >= 17,
            "expected at least the 17 known corpus rate calls, saw {seen}"
        );
    }

    /// The seven measured corpus calls are all the exact zero-argument macro form. This keeps a
    /// family-prefix or wrong-arity parser from appearing to work on a synthetic fixture while
    /// silently leaving a real call as `Raw`.
    #[test]
    fn every_corpus_reverse_lr_call_is_typed_and_exported() {
        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }
        let mut written = 0usize;
        let mut typed = 0usize;
        for (path, body) in &bodies {
            if extract_function(body, "game_").is_none() {
                continue;
            }
            let all = body.matches("macros::REVERSE_LR(").count();
            let exact = body.matches("macros::REVERSE_LR(agent);").count();
            assert_eq!(
                all, exact,
                "{path} contains a REVERSE_LR shape outside the measured signature"
            );
            if exact == 0 {
                continue;
            }
            written += exact;
            let script = parse_acmd_script(body);
            let events = script.to_reverse_lr_events();
            typed += events.len();
            assert_eq!(events.len(), exact, "{path} lost a typed REVERSE_LR event");
            let emitted = preview_game_fn(&script, "reverse_lr_audit");
            assert_eq!(
                parse_acmd_script(&emitted).to_reverse_lr_events(),
                events,
                "{path} changed its REVERSE_LR points on export"
            );
        }
        assert_eq!(written, 7, "the measured E1 corpus count changed");
        assert_eq!(typed, written, "every measured call must be typed");
    }

    /// Three files in a long-lived script cache are the literal bytes `404: Not Found`, because
    /// the fetch used to store whatever a request returned and a 404 is a *successful* request.
    ///
    /// Both halves matter and the negative one is worthless alone: "the error page is not read
    /// as a script" passes just as well against a function that reads nothing at all, so the
    /// same call is made with a real script body to show the path still carries one through.
    #[test]
    fn a_cached_error_page_is_not_read_as_a_script_but_a_real_one_still_is() {
        let dir = tempfile::tempdir().unwrap();

        let miss = dir.path().join("SpecialBStart.txt");
        std::fs::write(&miss, "404: Not Found").unwrap();
        assert_eq!(
            cached_script_body_at(&miss).as_deref(),
            Some(""),
            "an HTTP error page must not reach the parser as script source"
        );

        let real = dir.path().join("AttackHi4.txt");
        let body = "unsafe extern \"C\" fn game_attackhi4(agent: &mut L2CAgentBase) {\n\
                        frame(agent.lua_state_agent, 4.0);\n\
                    }\n";
        std::fs::write(&real, body).unwrap();
        assert_eq!(
            cached_script_body_at(&real).as_deref(),
            Some(body),
            "a real cached script must come back byte for byte"
        );
    }

    /// The regression this fix could easily have introduced, and the reason a miss normalises to
    /// an empty body instead of to `None`.
    ///
    /// The fighter-wide scan reads `None` as "not cached yet" and queues the move for a network
    /// fetch. If a known-missing move answered `None`, every scan would re-request all of them
    /// forever — the exact cost the cache was built to avoid — and nothing would have failed.
    #[test]
    fn a_cached_miss_still_counts_as_fetched_so_a_scan_does_not_re_request_it() {
        let dir = tempfile::tempdir().unwrap();
        let miss = dir.path().join("SpecialBAttack.txt");
        std::fs::write(&miss, "404: Not Found").unwrap();

        assert!(
            cached_script_body_at(&miss).is_some(),
            "a cached miss is still cached; returning None sends it back to the network"
        );
        assert!(
            cached_script_body_at(&dir.path().join("NeverFetched.txt")).is_none(),
            "a move that was never fetched must stay distinguishable from one that missed"
        );
    }

    /// What the bug actually cost, asserted at the surface where it did damage.
    ///
    /// `404: Not Found` is not a macro call, so the parser keeps it as [`AcmdStmt::Raw`] and
    /// `emit_stmts` writes `Raw` lines straight back out. Exporting one of those three dolly
    /// moves therefore emitted the words `404: Not Found` into a generated `.rs` file, which no
    /// toolchain will build. This is the verbatim-escape-hatch trap: a `Raw` line is trusted to
    /// be Rust, and here it was an error page.
    #[test]
    fn exporting_a_move_with_no_upstream_script_does_not_emit_the_error_page() {
        // Unnormalised, to show the export path really would have written it.
        let raw = parse_acmd_script("404: Not Found");
        assert!(
            preview_game_fn(&raw, "special_b_start").contains("404: Not Found"),
            "if this stops holding the export no longer round-trips Raw lines, and the \
             normalisation below is guarding nothing — re-check the fix, do not delete this"
        );

        let normalised = parse_acmd_script(&script_source_from_body("404: Not Found"));
        let emitted = preview_game_fn(&normalised, "special_b_start");
        assert!(
            !emitted.contains("404"),
            "a move with no upstream script must export as an empty function, got:\n{emitted}"
        );
    }

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

    /// A runtime `if` around a hurtbox call, with a second call after it.
    ///
    /// Composed rather than lifted, and that is defensible here in a way it would not be for a
    /// macro signature: **both halves are corpus shapes, only their nesting is not.** The header
    /// is one of the twenty-five distinct `RawBlock` openers the cache contains verbatim, and
    /// `HIT_NODE` appears in it hundreds of times — what is being constructed is a composition,
    /// not a guess about what the game accepts. Nothing here claims a layout the corpus could
    /// have refuted.
    ///
    /// The measurement that makes this fixture necessary: **zero** hurtbox and **zero**
    /// attack-modifier statements sit inside a `RawBlock` anywhere in the 460-file cache, against
    /// 26 sounds in 16 files and 107 effect calls in 12. So the corpus oracles cannot reach this
    /// path for these two families and never could, however long they run.
    const HURT_IN_RAW_BLOCK: &str = r#"unsafe extern "C" fn game_rawblockhurt(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 2.0);
    if WorkModule::is_flag(agent.module_accessor, *FIGHTER_STATUS_SQUAT_FLAG_REQUEST_SQUAT_SE) {
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("shoulderl"), *HIT_STATUS_XLU);
    }
    }
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_NORMAL);
    }
}
"#;

    /// The walk and the resolver have to agree about a hurtbox inside a runtime branch.
    ///
    /// [`eval_stmts`](crate::data) descends into an [`AcmdStmt::RawBlock`] deliberately — a
    /// hitbox inside an `if` has always been shown unconditionally — so a `HIT_NODE` in there
    /// takes a site. `hurt_stmt_mut` used not to descend, so it resolved sites against a
    /// *shorter* sequence than the one that handed them out: the branch's own state resolved to
    /// the call after the branch, and the last state resolved to nothing at all.
    ///
    /// Asserted on the resolved statement's **bone**, not on `is_some()`. Both sites resolve to
    /// a real `HIT_NODE` with the bug present; what is wrong is *which one*, and a liveness
    /// check cannot see that. Every state in the script is checked rather than just the
    /// interesting one, because the failure is a shift — pinning only the branch's own call
    /// would pass with the walk and the resolver both wrong by the same amount.
    #[test]
    fn a_hurtbox_inside_a_runtime_branch_resolves_to_its_own_call() {
        let mut script = parse_acmd_script(HURT_IN_RAW_BLOCK);
        let (states, _) = script.to_hurtboxes();
        let expected: Vec<(usize, String)> = states
            .iter()
            .map(|s| {
                let crate::data::HurtTarget::Bone(bone) = &s.target else {
                    panic!("fixture uses HIT_NODE, which targets a bone");
                };
                (s.site, bone.clone())
            })
            .collect();
        assert_eq!(
            expected.len(),
            2,
            "the walk should see the branch's call and the one after it"
        );
        for (site, bone) in expected {
            let stmt = script
                .hurt_stmt_mut(site)
                .unwrap_or_else(|| panic!("site {site} ({bone}) resolved to nothing"));
            let crate::data::ExcuteStmt::HitStatus { target, .. } = stmt else {
                panic!("site {site} resolved to {stmt:?}, not a hurtbox statement");
            };
            assert_eq!(
                target,
                &crate::data::HurtTarget::Bone(bone.clone()),
                "site {site} should be the `{bone}` call the walk took it from"
            );
        }
    }

    /// A zero-iteration `for` wrapping a runtime branch still steps both cursors over it.
    ///
    /// The counters are a *second* place the same disagreement lives, reachable only through a
    /// zero-count loop — at one iteration or more the cursor arrives correctly on its own, which
    /// is why the two tests above pass with the counter arms reverted. Verified by mutation, not
    /// assumed: reverting either arm left all 397 other tests green.
    ///
    /// Both families in one test here, unlike the resolver pair above, because the assertion is
    /// about a single shared property — the site the *trailing* call receives — and neither
    /// family's number is meaningful without the other's cursor having advanced too.
    #[test]
    fn a_branch_inside_a_loop_that_never_runs_still_advances_both_cursors() {
        const EMPTY_LOOP_BRANCH: &str = r#"unsafe extern "C" fn game_emptyloopbranch(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 2.0);
    for _ in 0..0 {
    if WorkModule::is_flag(agent.module_accessor, *FIGHTER_STATUS_SQUAT_FLAG_REQUEST_SQUAT_SE) {
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("shoulderl"), *HIT_STATUS_XLU);
        macros::ATK_POWER(agent, 0, 12.0);
    }
    }
    }
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_NORMAL);
        macros::ATK_POWER(agent, 1, 3.5);
    }
}
"#;
        let script = parse_acmd_script(EMPTY_LOOP_BRANCH);
        let (states, _) = script.to_hurtboxes();
        let mods = script.to_attack_mods();
        assert_eq!(
            states.len(),
            1,
            "a loop with no iterations set a hurtbox anyway"
        );
        assert_eq!(
            mods.len(),
            1,
            "a loop with no iterations ran a modifier anyway"
        );
        // Site 1, not 0: the skipped branch's own call is site 0 and keeps it. Without the
        // `RawBlock` arm in the counters the cursor never steps over that call, and the
        // surviving statement claims site 0 — the line inside the loop that never ran.
        assert_eq!(
            states[0].site, 1,
            "the surviving `kneer` call took the site of the `shoulderl` call inside the skipped loop"
        );
        assert_eq!(
            mods[0].site, 1,
            "the surviving id-1 modifier took the site of the id-0 call inside the skipped loop"
        );
    }

    /// The attack-modifier resolver, same defect and same fixture shape.
    ///
    /// Its own test rather than a second assertion in the one above, because the two families
    /// keep independent numbering spaces on purpose — a shared counter would make every hurtbox
    /// site shift the moment a script gained an `ATK_POWER` — so a fix to one is no evidence
    /// about the other. They were in fact broken and fixed together, which is exactly the
    /// coincidence that makes a shared test worthless later.
    #[test]
    fn an_attack_modifier_inside_a_runtime_branch_resolves_to_its_own_call() {
        const MOD_IN_RAW_BLOCK: &str = r#"unsafe extern "C" fn game_rawblockmod(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if WorkModule::is_flag(agent.module_accessor, *FIGHTER_STATUS_ATTACK_FLAG_SMASH_SMASH_HOLD_TO_ATTACK) {
    if macros::is_excute(agent) {
        macros::ATK_POWER(agent, 0, 12.0);
    }
    }
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::ATK_POWER(agent, 1, 3.5);
    }
}
"#;
        let mut script = parse_acmd_script(MOD_IN_RAW_BLOCK);
        let expected: Vec<(usize, i64)> = script
            .to_attack_mods()
            .iter()
            .map(|m| (m.site, m.id))
            .collect();
        assert_eq!(expected.len(), 2, "the walk should see both modifiers");
        for (site, id) in expected {
            let stmt = script
                .attack_mod_stmt_mut(site)
                .unwrap_or_else(|| panic!("site {site} (id {id}) resolved to nothing"));
            let crate::data::ExcuteStmt::AttackMod { id: got, .. } = stmt else {
                panic!("site {site} resolved to {stmt:?}, not an attack modifier");
            };
            assert_eq!(
                *got, id,
                "site {site} should be the id-{id} call the walk took it from"
            );
        }
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

    /// A `SEARCH` is a detection box of its own family, in both of the shapes it is written in.
    ///
    /// Both fixtures are verbatim corpus lines: the short one from kirby/SpecialNStart, the long
    /// one from kirby/BatSwing4. They differ in arity by exactly the three capsule slots, which
    /// is what makes them worth asserting together — every argument after the capsule moves.
    #[test]
    fn search_boxes_parse_in_both_of_the_shapes_the_corpus_writes_them() {
        let src = r#"unsafe extern "C" fn game_search(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::SEARCH(agent, 0, 0, Hash40::new("top"), 4.0, 0.0, 7.0, 8.0, *COLLISION_KIND_MASK_ATTACK, *HIT_STATUS_MASK_ALL, 0, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_FIGHTER, *COLLISION_PART_MASK_ALL, false);
    }
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::SEARCH(agent, 1, 0, Hash40::new("top"), 7.5, 0.0, 7.0, 4.0, 0.0, 7.0, 13.0, *COLLISION_KIND_MASK_ATTACK, *HIT_STATUS_MASK_NORMAL, 60, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false);
    }
}
"#;
        let script = parse_acmd_script(src);
        let boxes = script.to_hitboxes();
        assert_eq!(boxes.len(), 2, "{boxes:#?}");

        let short = &boxes[0];
        assert_eq!(short.category, crate::data::CAT_SEARCH);
        assert_eq!(short.size, 4.0);
        assert_eq!(
            (short.offset_x, short.offset_y, short.offset_z),
            (0.0, 7.0, 8.0)
        );
        assert_eq!(short.capsule_end, None);
        // The three masks land on the fields that already existed, not in the extras.
        assert_eq!(short.situation_mask, "COLLISION_SITUATION_MASK_GA");
        assert_eq!(short.category_mask, "COLLISION_CATEGORY_MASK_FIGHTER");
        assert_eq!(short.part_mask, "COLLISION_PART_MASK_ALL");
        let extras = short.search.as_ref().expect("a SEARCH carries its extras");
        assert_eq!(extras.collision_kind, "COLLISION_KIND_MASK_ATTACK");
        assert_eq!(extras.hit_status, "HIT_STATUS_MASK_ALL");
        assert_eq!(extras.unk, 0);
        assert!(!extras.unk2);
        // A detection box deals nothing — the attack defaults must not leak in.
        assert_eq!(short.damage, 0.0);

        let long = &boxes[1];
        assert_eq!(long.capsule_end, Some([0.0, 7.0, 13.0]));
        assert_eq!(long.size, 7.5);
        assert_eq!(long.category_mask, "COLLISION_CATEGORY_MASK_ALL");
        let extras = long.search.as_ref().expect("a SEARCH carries its extras");
        assert_eq!(extras.hit_status, "HIT_STATUS_MASK_NORMAL");
        // Not invariant across the corpus, so it has to survive rather than be defaulted.
        assert_eq!(extras.unk, 60);

        // Nothing closes a search volume, so both run to the end of the move.
        assert_eq!(short.active_end, 9999);
        assert_eq!(long.active_end, 9999);

        // And the whole thing round-trips.
        let exported = preview_game_fn(&script, "search");
        assert_eq!(
            parse_acmd_script(&exported).to_hitboxes(),
            boxes,
            "{exported}"
        );
    }

    /// Three families open a box with id 0 in one block, and all three must survive.
    ///
    /// This is kirby/SpecialNStart, near enough verbatim: a `CATCH`, a `SEARCH` and an
    /// `ATTACK_ABS` in a single `is_excute`, every one of them id 0. Matching on id alone
    /// anywhere in the walk would close two of the three the moment the next one was read.
    #[test]
    fn a_search_a_grab_and_a_throw_hit_sharing_id_zero_do_not_close_each_other() {
        let src = r#"unsafe extern "C" fn game_specialnstart(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 18.0);
    if macros::is_excute(agent) {
        macros::CATCH(agent, 0, Hash40::new("top"), 6.0, 0.0, 6.0, 5.0, *FIGHTER_STATUS_KIND_SWALLOWED, *COLLISION_SITUATION_MASK_GA);
        macros::SEARCH(agent, 0, 0, Hash40::new("top"), 4.0, 0.0, 7.0, 8.0, *COLLISION_KIND_MASK_ATTACK, *HIT_STATUS_MASK_ALL, 0, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_FIGHTER, *COLLISION_PART_MASK_ALL, false);
        macros::ATTACK_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH, 0, 5.0, 361, 100, 0, 0, 0.0, 1.0, *ATTACK_LR_CHECK_F, 0.0, true, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_S, *COLLISION_SOUND_ATTR_NONE, *ATTACK_REGION_NONE);
    }
}
"#;
        let script = parse_acmd_script(src);
        let boxes = script.to_hitboxes();
        assert_eq!(boxes.len(), 3, "all three families stay open\n{boxes:#?}");

        let mut categories: Vec<u8> = boxes.iter().map(|b| b.category).collect();
        categories.sort_unstable();
        assert_eq!(
            categories,
            vec![1, crate::data::CAT_ABS, crate::data::CAT_SEARCH]
        );
        assert!(
            boxes.iter().all(|b| b.id == 0 && b.active_start == 18),
            "{boxes:#?}"
        );
        // None of them was ended by another's arrival.
        assert!(boxes.iter().all(|b| b.active_end == 9999), "{boxes:#?}");

        // The inhale's own arguments all survive the export, each through its own layout.
        let exported = preview_game_fn(&script, "specialnstart");
        assert!(
            exported.contains("*FIGHTER_STATUS_KIND_SWALLOWED"),
            "{exported}"
        );
        assert!(
            exported.contains("*COLLISION_CATEGORY_MASK_FIGHTER"),
            "{exported}"
        );
        assert!(
            exported.contains("*FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH"),
            "{exported}"
        );
    }

    /// `SET_SEARCH_SIZE_EXIST` is not a `SEARCH`, and must not be read through its layout.
    ///
    /// It re-sizes a box already out and takes two arguments; `SEARCH` takes seventeen. The
    /// `macros::NAME(` form with the paren is what keeps them apart — the same guard that
    /// stops `ATTACK_ABS` being read as an `ATTACK`. Reading one as the other is the silent
    /// cross-family corruption this parser is built to refuse.
    #[test]
    fn the_search_size_modifier_is_not_parsed_as_a_search_box() {
        let src = r#"unsafe extern "C" fn game_search(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::SET_SEARCH_SIZE_EXIST(agent, 0, 7);
    }
}
"#;
        let script = parse_acmd_script(src);
        assert!(
            script.to_hitboxes().is_empty(),
            "a size modifier is not a box of its own"
        );
        // Unmodelled, so it rides through verbatim rather than being dropped.
        let exported = preview_game_fn(&script, "search");
        assert!(
            exported.contains("macros::SET_SEARCH_SIZE_EXIST(agent, 0, 7);"),
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
    fn attack_fp_uses_its_own_slot_table_and_preserves_unknowns() {
        let src = r#"unsafe extern "C" fn game_fp(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK_FP(agent, 2, 3, Hash40::new("top"), 9.5, 45, 100, 2, 30, 4.0, 1.0, 2.0, 3.0, Hash40::new_raw(0x123456), 0.25, 0.5, 0.75, true, false, 0, 3, 4, 0, true, 7, 8, false, 9, false, true, false, false, 10, true, false, *ATTACK_LR_CHECK_POS, false, true, false, false, false, 12);
    }
}
"#;
        let script = parse_acmd_script(src);
        let boxes = script.to_hitboxes();
        assert_eq!(boxes.len(), 1, "{boxes:#?}");
        let fp = &boxes[0];
        assert_eq!(fp.category, crate::data::CAT_ATTACK_FP);
        assert_eq!(fp.func, "ATTACK_FP");
        assert_eq!((fp.id, fp.part, fp.damage, fp.angle), (2, 3, 9.5, 45));
        assert_eq!(fp.collision_attr, "0x123456");
        assert_eq!(fp.lr_check, "ATTACK_LR_CHECK_POS");
        assert_eq!(
            fp.fp.as_ref().unwrap().args.len(),
            crate::data::ATTACK_FP_ARGC
        );
        // Slot 31 is undocumented `rehit`; it must not disappear just because the editor only
        // exposes the established shared fields.
        assert_eq!(fp.fp.as_ref().unwrap().args[31].source(), "10");

        let emitted = preview_game_fn(&script, "fp");
        assert!(
            emitted.contains("macros::ATTACK_FP(agent, 2, 3"),
            "{emitted}"
        );
        let round_trip = parse_acmd_script(&emitted).to_hitboxes();
        assert_eq!(round_trip.len(), 1);
        assert_eq!(round_trip[0].category, crate::data::CAT_ATTACK_FP);
        assert_eq!(round_trip[0].damage, fp.damage);
        assert_eq!(round_trip[0].collision_attr, "0x123456");
        for slot in [
            2usize, 8, 9, 10, 11, 13, 17, 18, 22, 24, 25, 26, 27, 28, 31, 32, 33, 35, 36, 37, 38,
            39, 40,
        ] {
            assert_eq!(
                round_trip[0].fp.as_ref().unwrap().args[slot],
                fp.fp.as_ref().unwrap().args[slot],
                "unmodeled ATTACK_FP slot {slot}"
            );
        }
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

    #[test]
    fn measured_expression_calls_round_trip_with_raw_neighbors() {
        let source = r#"
unsafe extern "C" fn expression_throwhi(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        slope!(agent, *MA_MSC_CMD_SLOPE_SLOPE, *SLOPE_STATUS_LR);
        macros::FT_ATTACK_ABS_CAMERA_QUAKE(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, *CAMERA_QUAKE_KIND_NONE);
    }
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::RUMBLE_HIT(agent, Hash40::new("rbkind_attackm"), 0);
        macros::QUAKE(agent, *CAMERA_QUAKE_KIND_M);
    }
}
"#;
        let script = parse_expression_script(source);
        let events = script.to_expression_events();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].frame, 1);
        assert_eq!(events[1].frame, 6);
        assert_eq!(events[0].call.func(), "FT_ATTACK_ABS_CAMERA_QUAKE");
        assert_eq!(events[1].call.func(), "RUMBLE_HIT");
        assert_eq!(events[2].call.func(), "QUAKE");

        let emitted = preview_expression_fn(&script, "throw_hi");
        assert!(emitted.contains("slope!(agent, *MA_MSC_CMD_SLOPE_SLOPE, *SLOPE_STATUS_LR);"));
        assert!(emitted.contains(
            "macros::FT_ATTACK_ABS_CAMERA_QUAKE(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, *CAMERA_QUAKE_KIND_NONE);"
        ));
        assert_eq!(
            parse_expression_script(&emitted).to_expression_events(),
            events
        );
    }

    /// Every measured expression call in the local script cache is typed and survives the same
    /// parse/emit/read-back path as the fixture above. The cache is the evidence boundary for
    /// D2: unknown expression lines remain raw, while these three known macros must never
    /// silently fall through to raw text or disappear from an export.
    #[test]
    fn every_measured_expression_call_in_the_corpus_is_typed() {
        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }

        let mut expression_scripts = 0usize;
        let mut written = [0usize; 3];
        let mut typed = [0usize; 3];
        let mut problems = Vec::new();
        for (path, body) in &bodies {
            let Some(interior) = function_interior(body, "expression_") else {
                continue;
            };
            expression_scripts += 1;
            let names = ["RUMBLE_HIT", "QUAKE", "FT_ATTACK_ABS_CAMERA_QUAKE"];
            for (index, name) in names.iter().enumerate() {
                written[index] += interior.matches(&format!("macros::{name}(")).count();
            }

            let script = parse_expression_script(body);
            let events = script.to_expression_events();
            for event in &events {
                let Some(index) = names.iter().position(|name| *name == event.call.func()) else {
                    continue;
                };
                typed[index] += 1;
            }
            let emitted = preview_expression_fn(&script, "corpus_audit");
            if parse_expression_script(&emitted).to_expression_events() != events {
                problems.push(format!("{path}: expression events changed on export"));
            }
        }

        assert!(
            problems.is_empty(),
            "{} expression scripts changed on export:\n{}",
            problems.len(),
            problems.join("\n")
        );
        assert_eq!(
            typed, written,
            "the measured expression calls no longer all have typed events"
        );
        assert_eq!(
            expression_scripts, 335,
            "the local expression corpus changed; update the measured D2 scope deliberately"
        );
        assert_eq!(
            written,
            [65, 51, 2],
            "the measured expression macro counts changed; update D2 before expanding scope"
        );
    }

    // ═══ Generated-source compile golden ════════════════════════════════════

    /// The inputs `sample_project` is built from, so a test can check the preview against
    /// the very same edits the export consumed.
    type SampleEdits = (
        crate::data::AcmdScript,
        Vec<crate::data::EffectCall>,
        crate::data::AcmdScript,
        Vec<crate::mod_project::LiveTweak>,
    );

    fn sample_project() -> ModProject {
        let (script, fx, sfx, tweaks) = sample_edits();
        build_mod_project_full(
            &[("mario".into(), "attack_air_n".into(), script)],
            &[(
                "mario".into(),
                "attack_air_n".into(),
                fx,
                Default::default(),
            )],
            &[("mario".into(), "attack_air_n".into(), sfx)],
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
                trail_off: None,
                trail_bone2: None,
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
                trail_off: None,
                trail_bone2: None,
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
        // `kirby/TurnDash` verbatim, parsed rather than hand-built, so the sound half of the
        // compile golden is a script the game actually ships. It carries a wrapped call, a
        // two-hash member, and the one member with a trailing non-hash argument — the three
        // shapes the emitter can get wrong.
        let sfx = parse_sound_script(
            r#"unsafe extern "C" fn sound_turndash(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::PLAY_SE(agent, Hash40::new("se_kirby_dash_start"));
        macros::SET_PLAY_INHIVIT(agent, Hash40::new("se_kirby_dash_start"), 20);
    }
    wait(agent.lua_state_agent, 13.0);
    if macros::is_excute(agent) {
        macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_left_m"), Hash40::new("se_kirby_step_right_m"));
    }
}
"#,
        );
        (script, fx, sfx, tweaks)
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

        let (_, emitted) =
            emit_effect_move_fn(&calls, "test", &Default::default(), &Default::default());
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

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "downattackd",
            &Default::default(),
            &Default::default(),
        );
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

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "attackairhi",
            &Default::default(),
            &Default::default(),
        );
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
        let (calls, residue) = parse_effect_script(src).to_effect_calls_and_residue();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].tint, None,
            "a costume-gated tint must not be exported onto every costume"
        );
        assert!(
            residue.is_empty(),
            "the line rides on the spawn above it, so no frame owns it: {residue:?}"
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

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "specialhicommand",
            &Default::default(),
            &Default::default(),
        );
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

    /// `dolly/FinalAirEnd`, trimmed to the two frames that matter and otherwise verbatim.
    ///
    /// This is E3's whole measured defect. Frame 40 of that script is two `CANCEL_FILL_SCREEN`
    /// calls and nothing else — no spawn in the block for C6's carry to attach them to — so both
    /// lines were deleted by every export of that move. It is the only such frame in the corpus,
    /// which is why the number moved by two.
    ///
    /// The macro is deliberately **not** modelled. Nothing here claims to know what
    /// `CANCEL_FILL_SCREEN(agent, 1, 30)`'s two arguments mean, and on two corpus calls nothing
    /// should: the line is reproduced as written, exactly as a carried line is. What changed is
    /// that a frame can now own lines, not that the editor understands these ones.
    #[test]
    fn a_frame_whose_block_holds_no_spawn_still_reaches_the_export() {
        let src = r#"unsafe extern "C" fn effect_finalairend(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("dolly_buster_ground"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, true);
    }
    frame(agent.lua_state_agent, 40.0);
    if macros::is_excute(agent) {
        macros::CANCEL_FILL_SCREEN(agent, 1, 30);
        macros::CANCEL_FILL_SCREEN(agent, 0, 30);
    }
}
"#;
        let (calls, residue) = parse_effect_script(src).to_effect_calls_and_residue();
        assert_eq!(calls.len(), 1, "only the EFFECT is a call");

        // Keyed by frame 40, not by frame 1. `end_frame` is handed the cursor it is *leaving*,
        // and handing it the one being entered would put these lines on the following block —
        // still exported, still well-formed, and playing at the wrong time. There is no later
        // frame in this fixture, so the wrong answer here is the script's first frame.
        assert_eq!(
            residue.keys().copied().collect::<Vec<_>>(),
            vec![40],
            "{residue:?}"
        );
        assert!(
            residue[&40]
                .iter()
                .any(|l| l.contains("CANCEL_FILL_SCREEN(agent, 1, 30)"))
                && residue[&40]
                    .iter()
                    .any(|l| l.contains("CANCEL_FILL_SCREEN(agent, 0, 30)")),
            "both calls, in their own is_excute wrapper: {:?}",
            residue[&40]
        );

        let (_, emitted) =
            emit_effect_move_fn(&calls, "finalairend", &Default::default(), &residue);
        let frame_at = emitted
            .find("frame(agent.lua_state_agent, 40.0);")
            .unwrap_or_else(|| panic!("frame 40 must exist in its own right:\n{emitted}"));
        let first = emitted
            .find("CANCEL_FILL_SCREEN(agent, 1, 30)")
            .unwrap_or_else(|| panic!("the line the export used to delete:\n{emitted}"));
        assert!(
            frame_at < first,
            "the lines must land after their frame, not before it:\n{emitted}"
        );
        assert!(
            emitted.contains("CANCEL_FILL_SCREEN(agent, 0, 30)"),
            "both of them, in source order:\n{emitted}"
        );
        assert_eq!(
            emitted.matches("CANCEL_FILL_SCREEN").count(),
            2,
            "and each exactly once — a residue block emitted per call would duplicate them on a \
             frame that had two:\n{emitted}"
        );
        assert_eq!(
            emitted.matches('{').count(),
            emitted.matches('}').count(),
            "the carried block brings its own braces:\n{emitted}"
        );
        assert!(
            !emitted.contains("if macros::is_excute(agent) {\n    }"),
            "a frame that exists only for residue must not also open an empty block:\n{emitted}"
        );

        // The export no longer deletes them, so the report must no longer say it does.
        assert!(
            !unexportable_effect_lines(&parse_effect_script(src))
                .iter()
                .any(|l| l.contains("CANCEL_FILL_SCREEN")),
            "a line the export writes must not be reported as dropped"
        );
    }

    /// The frame a residue line is keyed to is the one it was written under, not the next one.
    ///
    /// Composed on purpose, and the composition is the point: the corpus's only spawn-less block
    /// is the *last* frame of `dolly/FinalAirEnd`, so it is flushed by the final `end_frame` and
    /// says nothing about the two inside the walk. Putting a frame after it is what distinguishes
    /// "close the block being left" from "close the block being entered" — and getting that
    /// backwards produces an export that is well-formed, compiles, balances its braces, and plays
    /// the line 20 frames late. Every other assertion in this file would have stayed green.
    #[test]
    fn residue_is_keyed_to_the_frame_it_was_written_at_not_the_next_one() {
        let src = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 40.0);
    if macros::is_excute(agent) {
        macros::CANCEL_FILL_SCREEN(agent, 1, 30);
    }
    frame(agent.lua_state_agent, 60.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
    }
}
"#;
        let (calls, residue) = parse_effect_script(src).to_effect_calls_and_residue();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].active_start, 60, "the spawn is the later frame's");
        assert_eq!(
            residue.keys().copied().collect::<Vec<_>>(),
            vec![40],
            "the line was written under frame 40 and belongs to it: {residue:?}"
        );
        assert!(
            calls[0].leading.is_empty(),
            "and it must not have been handed to the next frame's spawn either, which would \
             retime it just as surely: {:?}",
            calls[0].leading
        );

        let (_, emitted) = emit_effect_move_fn(&calls, "test", &Default::default(), &residue);
        let at_40 = emitted.find("frame(agent.lua_state_agent, 40.0);").unwrap();
        let at_60 = emitted.find("frame(agent.lua_state_agent, 60.0);").unwrap();
        let line = emitted.find("CANCEL_FILL_SCREEN").unwrap();
        assert!(
            at_40 < line && line < at_60,
            "the line must sit between its own frame and the next:\n{emitted}"
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
        let (_, emitted) =
            emit_effect_move_fn(&calls, "test", &Default::default(), &Default::default());

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
        let (_, emitted) = emit_effect_move_fn(&calls, "attackairhi", &tweaks, &Default::default());
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
        let (_, emitted) =
            emit_effect_move_fn(&calls, "test", &Default::default(), &Default::default());
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

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "attackdash",
            &Default::default(),
            &Default::default(),
        );
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

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "specialsstart",
            &Default::default(),
            &Default::default(),
        );
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
        let (_, emitted) = emit_effect_move_fn(&calls, "test", &tweaks, &Default::default());
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

        let (_, emitted) =
            emit_effect_move_fn(&calls, "test", &Default::default(), &Default::default());
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
        let out = preview_effect_fn(&edited, "attacks4", &[], &Default::default());

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
        let untouched = preview_effect_fn(&calls, "attacks4", &[], &Default::default());
        assert!(
            untouched.contains(
                r#"macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), Hash40::new("sword2"), 3, 8, 0.75);"#
            ),
            "{untouched}"
        );
    }

    /// Both `AFTER_IMAGE_OFF` values the corpus writes survive a round trip, as integers.
    ///
    /// The four vanilla calls split 2/2 between `0` and `3`, so neither is "the" value and
    /// normalising to either would rewrite half the archive. The slot is `ToF32`-generic, so
    /// `0` and `0.0` both compile and no semantic check would flag the difference — the same
    /// formatting trap B3 found, in a second family.
    #[test]
    fn a_trails_closing_argument_round_trips_as_the_integer_the_corpus_writes() {
        for value in ["0", "3"] {
            let src = format!(
                r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {{
    frame(agent.lua_state_agent, 3.0);
    if macros::is_excute(agent) {{
        macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), 0, 0, 0, Hash40::new("sword2"), 0, 0, 0);
    }}
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {{
        macros::AFTER_IMAGE_OFF(agent, {value});
    }}
}}
"#
            );
            let calls = parse_effect_script(&src).to_effect_calls();
            assert_eq!(calls.len(), 1, "{value}");
            assert_eq!(calls[0].trail_off, Some(value.parse::<f32>().unwrap()));

            let (_, emitted) =
                emit_effect_move_fn(&calls, "test", &Default::default(), &Default::default());
            assert!(
                emitted.contains(&format!("macros::AFTER_IMAGE_OFF(agent, {value});")),
                "{value} must come back exactly as written:\n{emitted}"
            );
        }
    }

    // ── C2: trails written as a raw command ──────────────────────────────────
    //
    // Every fixture below is the real kirby `SpecialHi2` text, not a shape invented here. The
    // entry's earlier `_arg29` fixture was fabricated, put a `Hash40` where a coordinate goes,
    // and would have pinned joint work to a call the game never makes.

    /// The first trail-ON call of kirby `SpecialHi2`, verbatim.
    ///
    /// 26 arguments after the command id — C2's own note said 27, having counted the id itself.
    /// Slot 1 is the texture and slot 4 the joint, the same numbering as a `macros::` call,
    /// because the command id sits where `agent` does.
    pub(crate) const CORPUS_TRAIL: &str = r#"        effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, Hash40::new("tex_kirby_cutter"), Hash40::new("tex_kirby_cutter"), 12, Hash40::new("haver"), 0, 3, 0.25, Hash40::new("haver"), 0, 26, 0.5, true, Hash40::new("null"), Hash40::new("haver"), 0, 0, 0, 0, 0, 0, 1, 0, *EFFECT_AXIS_X, 0, *TRAIL_BLEND_BLEND_SRC_ONE, 1);"#;

    pub(crate) fn corpus_trail_script() -> String {
        format!(
            r#"
unsafe extern "C" fn effect_specialhi2(agent: &mut L2CAgentBase) {{
    frame(agent.lua_state_agent, 1.0);
    if macros::is_excute(agent) {{
{CORPUS_TRAIL}
        macros::EFFECT_FOLLOW(agent, Hash40::new("kirby_fcut_rise"), Hash40::new("haver"), 0, 3, 0.3, 0, 0, 0, 1, true);
    }}
    frame(agent.lua_state_agent, 7.0);
    if macros::is_excute(agent) {{
        macros::AFTER_IMAGE_OFF(agent, 3);
    }}
}}
"#
        )
    }

    /// The trail form the corpus actually uses parses, and comes back out byte-identical.
    ///
    /// All four vanilla trail-ON calls are this raw `effect(*MA_MSC_CMD_…, …)` command; the three
    /// `macros::` names C2 was written around have zero. Before this the line was untyped, so it
    /// was carried only when a spawn happened to share its frame block and deleted otherwise.
    #[test]
    fn the_raw_command_trail_the_corpus_writes_round_trips_byte_identically() {
        let calls = parse_effect_script(&corpus_trail_script()).to_effect_calls();
        let trail = calls
            .iter()
            .find(|call| call.spawn_func == "AFTER_IMAGE_ON")
            .expect("the raw command form must produce a trail call");
        // Slot 1 and slot 4, read through the same constants the wrapper form uses.
        assert_eq!(trail.effect_name, "tex_kirby_cutter");
        assert_eq!(trail.bone_name, "haver");

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "specialhi2",
            &Default::default(),
            &Default::default(),
        );
        assert!(
            emitted.contains(CORPUS_TRAIL.trim()),
            "the trail must be re-emitted exactly as written — there is no \
             `macros::AFTER_IMAGE3_ON` to fall back to:\n{emitted}"
        );
    }

    /// The graphic and joints come from slots 1, 4 and 8 specifically, not from their twins.
    ///
    /// **The corpus cannot prove this, and tests built only from it silently did not.** Every
    /// vanilla call writes `Hash40::new("tex_kirby_cutter")` at both slot 1 (`trail1`) and slot 2
    /// (`trail2`), and `Hash40::new("haver")` at slots 4 (`trail_bone1`), 8 (`trail_bone2`) and
    /// 14 (`flare_bone`) — so reading the wrong member of either group returns the same string.
    /// Mutations moving the graphic read to slot 2 and the joint read to slot 8 both passed the
    /// entire suite.
    ///
    /// The layout below is still the corpus's, 26 arguments in vanilla order; only the twin
    /// slots' *values* are varied, which is what a modder pointing the two trail edges at
    /// different bones would write. The independent evidence for which slot is which is the
    /// declaration: `AFTER_IMAGE4_ON_arg29(agent, trail1, trail2, trail_length, trail_bone1,
    /// trail_x1, trail_y1, trail_z1, trail_bone2, …)`.
    #[test]
    fn a_raw_trails_graphic_and_joints_are_read_from_the_slots_the_declaration_names() {
        // Slot 2 is the second texture; slot 8 the trail's other edge.
        let line = CORPUS_TRAIL
            .trim()
            .replacen(
                r#"tex_kirby_cutter"), Hash40::new("tex_kirby_cutter")"#,
                r#"tex_kirby_cutter"), Hash40::new("tex_other")"#,
                1,
            )
            .replacen(
                r#"0.25, Hash40::new("haver")"#,
                r#"0.25, Hash40::new("sword")"#,
                1,
            );
        assert!(
            line.contains(r#"Hash40::new("tex_other")"#)
                && line.contains(r#"Hash40::new("sword")"#),
            "the fixture must actually differ at slots 2 and 8, or this proves nothing:\n{line}"
        );
        let src = format!(
            "unsafe extern \"C\" fn effect_t(agent: &mut L2CAgentBase) {{\n    \
             if macros::is_excute(agent) {{\n{line}\n    }}\n}}\n"
        );
        let calls = parse_effect_script(&src).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].effect_name, "tex_kirby_cutter",
            "slot 1 is `trail1`; slot 2 is the second texture"
        );
        assert_eq!(
            calls[0].bone_name, "haver",
            "slot 4 is `trail_bone1`; slot 8 is the trail's other edge and slot 14 the flare's"
        );
        // The same fixture, read the other way round. Asserting both directions is what makes
        // the pair a statement about *which* slot each field comes from: either assertion alone
        // still passes with the two reads swapped, because on any vanilla call they agree.
        assert_eq!(
            calls[0].trail_bone2.as_deref(),
            Some("sword"),
            "slot 8 is `trail_bone2`; slot 14 is the flare's bone and is not a trail edge"
        );
    }

    /// Retargeting the joint rewrites slot 4 and leaves the other 25 arguments alone.
    ///
    /// The paired half matters: an emitter that dropped the whole line would also "not contain
    /// the old joint", so the assertion that the edit landed is checked together with the
    /// assertion that its neighbours — including the *other* two `haver` hashes at slots 8 and
    /// 14 — did not move.
    #[test]
    fn editing_a_raw_trails_joint_rewrites_only_that_argument() {
        let mut calls = parse_effect_script(&corpus_trail_script()).to_effect_calls();
        let trail = calls
            .iter_mut()
            .find(|call| call.spawn_func == "AFTER_IMAGE_ON")
            .expect("trail call");
        trail.bone_name = "top".into();

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "specialhi2",
            &Default::default(),
            &Default::default(),
        );
        let line = emitted
            .lines()
            .find(|line| line.contains("AFTER_IMAGE3_ON"))
            .expect("the trail line must still be emitted");

        let expected = CORPUS_TRAIL.trim().replacen(
            r#"12, Hash40::new("haver")"#,
            r#"12, Hash40::new("top")"#,
            1,
        );
        assert_eq!(
            line.trim(),
            expected,
            "only the joint slot may change; slots 8 and 14 are also `haver` and must not"
        );
    }

    /// Retargeting the *second* joint rewrites slot 8 and nothing else.
    ///
    /// The mirror of the test above, and it has to be its own case rather than a second edit in
    /// that one: applied together, an emitter that wrote both edits into a single slot would
    /// still leave a line containing `top` and `blade`, and asserting on the whole line is only
    /// decisive when exactly one slot is expected to move. Slot 8 sits between two other
    /// `haver` hashes, so a splice that got the span even slightly wrong lands on a neighbour.
    #[test]
    fn editing_a_raw_trails_second_joint_rewrites_only_that_argument() {
        let mut calls = parse_effect_script(&corpus_trail_script()).to_effect_calls();
        let trail = calls
            .iter_mut()
            .find(|call| call.spawn_func == "AFTER_IMAGE_ON")
            .expect("trail call");
        assert_eq!(
            trail.trail_bone2.as_deref(),
            Some("haver"),
            "the corpus call must carry a second joint, or this edits nothing"
        );
        trail.trail_bone2 = Some("blade".into());

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "specialhi2",
            &Default::default(),
            &Default::default(),
        );
        let line = emitted
            .lines()
            .find(|line| line.contains("AFTER_IMAGE3_ON"))
            .expect("the trail line must still be emitted");

        // Anchored on the preceding `trail_z1` value, which is what distinguishes slot 8 from
        // the identical hashes at 4 and 14.
        let expected = CORPUS_TRAIL.trim().replacen(
            r#"0.25, Hash40::new("haver")"#,
            r#"0.25, Hash40::new("blade")"#,
            1,
        );
        assert_eq!(
            line.trim(),
            expected,
            "only `trail_bone2` may change; `trail_bone1` and `flare_bone` hold the same hash"
        );
    }

    /// A trail call with no slot 8 gets no second joint, rather than an empty one.
    ///
    /// The distinction is `Some(String::new())` versus `None`, and only the second is right: a
    /// `Some` puts a `Bone 2` row in the panel holding nothing, which invites the user to fill
    /// in a joint that has no argument to be written to. The export would then find no slot 8 to
    /// splice and drop the edit without a word.
    ///
    /// Truncated calls are not hypothetical here — the editor loads whatever a modder's source
    /// contains, and a hand-written trail is exactly where a short one comes from.
    #[test]
    fn a_trail_too_short_to_reach_slot_eight_offers_no_second_joint() {
        let short = r#"        effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, Hash40::new("tex_kirby_cutter"), Hash40::new("tex_kirby_cutter"), 12, Hash40::new("haver"));"#;
        let src = format!(
            "unsafe extern \"C\" fn effect_t(agent: &mut L2CAgentBase) {{\n    \
             if macros::is_excute(agent) {{\n{short}\n    }}\n}}\n"
        );
        let calls = parse_effect_script(&src).to_effect_calls();
        assert_eq!(calls.len(), 1, "the call must still parse as a trail");
        assert_eq!(calls[0].bone_name, "haver", "slot 4 is present and is read");
        assert_eq!(
            calls[0].trail_bone2, None,
            "there is no slot 8 in this call, so there is no second joint to offer"
        );

        let (_, emitted) =
            emit_effect_move_fn(&calls, "t", &Default::default(), &Default::default());
        assert!(
            emitted.contains(short.trim()),
            "a short trail must still ride through verbatim:\n{emitted}"
        );
    }

    /// The two trail spellings nothing declares get no second joint to edit.
    ///
    /// `macros::AFTER_IMAGE4_ON` and `macros::AFTER_IMAGE_ON` are not in `smash-script` and not
    /// in the corpus, so no signature says what slot 8 of such a call holds. They are parsed so
    /// that a source file containing one survives a round trip — offering a `Bone 2` field on
    /// them would invite an edit to an argument the editor has guessed the meaning of.
    #[test]
    fn an_undeclared_trail_spelling_offers_no_second_joint() {
        let undeclared = r#"        macros::AFTER_IMAGE_ON(agent, Hash40::new("tex_a"), Hash40::new("tex_b"), 12, Hash40::new("haver"), 0, 3, 0.25, Hash40::new("sword"), 0, 26, 0.5);"#;
        let src = format!(
            "unsafe extern \"C\" fn effect_t(agent: &mut L2CAgentBase) {{\n    \
             if macros::is_excute(agent) {{\n{undeclared}\n    }}\n}}\n"
        );
        let calls = parse_effect_script(&src).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].bone_name, "haver",
            "the first joint is read from these spellings, as it always was"
        );
        assert_eq!(
            calls[0].trail_bone2, None,
            "slot 8 holds `sword` here, but nothing declares this call's layout"
        );

        // And the line still round-trips, which is the only reason the branch exists.
        let (_, emitted) =
            emit_effect_move_fn(&calls, "t", &Default::default(), &Default::default());
        assert!(
            emitted.contains(undeclared.trim()),
            "an undeclared spelling must ride through verbatim:\n{emitted}"
        );
    }

    /// `AFTER_IMAGE_OFF` ends the trail, not whatever spawn happens to still be open.
    ///
    /// Kirby `SpecialHi2` starts a trail and an `EFFECT_FOLLOW` in one `is_excute`, then closes
    /// the trail four frames later. Resolving that close against the most recent open *call*
    /// picks the follow, which loses the `AFTER_IMAGE_OFF` — the trail then runs forever — and
    /// gives the follow an end frame the script never wrote, so the export invents an
    /// `EFFECT_OFF_KIND` that kills an effect early. Both halves are asserted, because either
    /// alone passes with the other still broken.
    #[test]
    fn a_trail_off_closes_the_trail_and_not_a_spawn_still_running() {
        let calls = parse_effect_script(&corpus_trail_script()).to_effect_calls();
        let trail = calls
            .iter()
            .find(|call| call.spawn_func == "AFTER_IMAGE_ON")
            .expect("trail call");
        assert_eq!(trail.active_end, 7, "the trail is what frame 7 closes");
        assert_eq!(
            trail.trail_off,
            Some(3.0),
            "and it keeps the written argument"
        );

        let follow = calls
            .iter()
            .find(|call| call.effect_name == "kirby_fcut_rise")
            .expect("follow call");
        assert_eq!(
            follow.active_end, 9999,
            "the follow is never closed by this script and must stay open"
        );

        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "specialhi2",
            &Default::default(),
            &Default::default(),
        );
        assert!(
            emitted.contains("macros::AFTER_IMAGE_OFF(agent, 3);"),
            "the close must survive to the export:\n{emitted}"
        );
        assert!(
            !emitted.contains("EFFECT_OFF_KIND"),
            "no end was written for the follow, so none may be invented:\n{emitted}"
        );
    }

    /// A trail this script did not start is carried, not dropped.
    ///
    /// Kirby splits the move: `SpecialHi2` turns the trail on, `SpecialHi4` turns it off. The
    /// off-script has no trail call to close and cannot be given one, and deleting the line
    /// would leave the trail running for the rest of the match.
    #[test]
    fn a_trail_off_with_no_trail_open_is_carried_rather_than_dropped() {
        let src = r#"
unsafe extern "C" fn effect_specialhi4(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 1.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("kirby_fcut_rise"), Hash40::new("haver"), 0, 3, 0.3, 0, 0, 0, 1, true);
        macros::AFTER_IMAGE_OFF(agent, 0);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        let (_, emitted) = emit_effect_move_fn(
            &calls,
            "specialhi4",
            &Default::default(),
            &Default::default(),
        );
        assert!(
            emitted.contains("macros::AFTER_IMAGE_OFF(agent, 0);"),
            "a close with nothing local to close still has to reach the export:\n{emitted}"
        );
        assert!(
            !emitted.contains("EFFECT_OFF_KIND"),
            "and it must not be resolved against the follow instead:\n{emitted}"
        );
    }

    /// Only a whole `effect` identifier starts a raw trail call.
    ///
    /// The parser and `scan_macro_sites` have to accept exactly the same lines: the parser's
    /// call consumes an ordinal and the scanner's site is what a write-back edits, so a line one
    /// accepts and the other refuses shifts every later call onto the wrong piece of source. The
    /// positive control is the point — without it this passes for a parser that recognises
    /// nothing at all.
    #[test]
    fn only_a_whole_effect_identifier_starts_a_raw_trail_call() {
        let real = CORPUS_TRAIL.trim();
        let embedded = real.replacen("effect(", "sub_effect(", 1);

        assert!(
            crate::acmd_src::raw_trail_line(real).is_some(),
            "the corpus line must be recognised, or the negative below proves nothing"
        );
        assert!(
            crate::acmd_src::raw_trail_line(&embedded).is_none(),
            "`sub_effect(` is a different function and must not be read as a trail"
        );

        // And the parser agrees with the scanner on both, which is the invariant that matters.
        let wrap = |line: &str| {
            format!(
                "unsafe extern \"C\" fn effect_t(agent: &mut L2CAgentBase) {{\n    \
                 if macros::is_excute(agent) {{\n{line}\n    }}\n}}\n"
            )
        };
        assert_eq!(
            parse_effect_script(&wrap(real)).to_effect_calls().len(),
            1,
            "the real line must produce exactly one call"
        );
        assert_eq!(
            parse_effect_script(&wrap(&embedded))
                .to_effect_calls()
                .len(),
            0,
            "and the embedded one none, matching the scanner"
        );
    }

    /// A trail the editor ends itself still exports a call that compiles.
    ///
    /// There is no closing line to take the argument from — the trail was retimed or added in
    /// the editor — so the export supplies one. What it must not do is emit the bare call,
    /// which is what happens if the argument is treated as optional because it is unknown.
    #[test]
    fn a_trail_with_no_closing_line_still_exports_the_required_argument() {
        let src = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 3.0);
    if macros::is_excute(agent) {
        macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), 0, 0, 0, Hash40::new("sword2"), 0, 0, 0);
    }
}
"#;
        let mut calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].trail_off, None, "nothing closed it");
        // The user drags the trail's end onto frame 12.
        calls[0].active_end = 12;

        let (_, emitted) =
            emit_effect_move_fn(&calls, "test", &Default::default(), &Default::default());
        assert!(
            emitted.contains(&format!(
                "macros::AFTER_IMAGE_OFF(agent, {});",
                crate::data::TRAIL_OFF_DEFAULT as i64
            )),
            "{emitted}"
        );
        assert!(
            !emitted.contains("AFTER_IMAGE_OFF(agent);"),
            "the arity-less form does not compile:\n{emitted}"
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
        macros::AFTER_IMAGE_OFF(agent, 3);
    }
}
"#;
        let calls = parse_effect_script(src).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].spawn_func, "AFTER_IMAGE_ON");
        assert_eq!((calls[0].active_start, calls[0].active_end), (3, 9));
        // The closing call's argument is kept: the corpus does not agree on it, so replacing
        // it with a house value would change what the author wrote.
        assert_eq!(calls[0].trail_off, Some(3.0));

        let (_, emitted) =
            emit_effect_move_fn(&calls, "test", &Default::default(), &Default::default());
        assert!(
            emitted.contains("macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new(\"tex1\")"),
            "the trail call must be replayed verbatim:\n{emitted}"
        );
        // With its argument, and as the bare integer the corpus writes. `macros::AFTER_IMAGE_OFF`
        // is declared `<F: ToF32>(agent, unk: F)` — the bare call this used to emit is a compile
        // error, so every exported move with a trail was an unbuildable project.
        assert!(
            emitted.contains("macros::AFTER_IMAGE_OFF(agent, 3);"),
            "a trail is closed by AFTER_IMAGE_OFF, which takes an argument:\n{emitted}"
        );
        assert!(
            !emitted.contains("AFTER_IMAGE_OFF(agent);"),
            "the arity-less form does not compile:\n{emitted}"
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
            trail_off: None,
            trail_bone2: None,
            rate: None,
            tint: None,
            alpha: None,
            color: None,
            guard: None,
            leading: Vec::new(),
            trailing: Vec::new(),
        };
        let (_, emitted) =
            emit_effect_move_fn(&[call], "test", &Default::default(), &Default::default());
        assert!(
            emitted.contains(
                "macros::EFFECT_FOLLOW(agent, Hash40::new(\"sys_hit\"), Hash40::new(\"haver\"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, true);"
            ),
            "an unrecoverable tail must fall back to a single-graphic EFFECT_FOLLOW:\n{emitted}"
        );
    }

    /// Every `.txt` in the local fetch cache, as `(path, body)`. Empty on a clean machine,
    /// which is why every caller checks for that rather than trusting the corpus to be there.
    fn corpus_bodies() -> Vec<(String, String)> {
        let cache = crate::scratch_dirs::app_storage_root().join("script-cache");
        let mut bodies = Vec::new();
        for fighter in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
            for script in std::fs::read_dir(fighter.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Ok(body) = std::fs::read_to_string(script.path()) {
                    bodies.push((script.path().display().to_string(), body));
                }
            }
        }
        bodies
    }

    /// The body between a function's header line and its closing brace, newline-terminated —
    /// the exact text `emit_*_body` is the inverse of.
    fn function_interior(source: &str, prefix: &str) -> Option<String> {
        let extracted = extract_function(source, prefix)?;
        let lines: Vec<&str> = extracted.lines().collect();
        (lines.len() >= 2).then(|| {
            lines[1..lines.len() - 1]
                .iter()
                .map(|line| format!("{line}\n"))
                .collect()
        })
    }

    /// The gate on loading `sound_` at all.
    ///
    /// Until now no `sound_` function was ever read, so no file holding one could be rewritten
    /// by mistake. Reading them gives that up, and the defence is that re-emitting what was
    /// read reproduces the script that went in. A spot check would not do: the corpus is where
    /// the shapes the parser has never seen live, and it is the whole population this feature
    /// will meet.
    ///
    /// Three separate properties, because "byte-identical" turns out to be the wrong bar on its
    /// own. Six corpus scripts are mis-indented at source — the dumper does not re-indent after
    /// an `else`, so the arm's body sits at the wrong depth — and the emitter corrects them.
    /// That is a diff, but it is not a loss, and demanding byte-equality would either fail on a
    /// dumper bug or force the emitter to reproduce it.
    ///
    /// 1. **Nothing is lost.** The trimmed lines, in order, are the same. This is the property
    ///    that matters: no line dropped, added, reordered or rewritten.
    /// 2. **Rewriting settles.** Emitting the emitted text again is a fixed point, so a file
    ///    rewritten once never drifts on later saves.
    /// 3. **Formatting does not quietly rot.** Almost all of them *are* byte-exact, and the
    ///    count is asserted so a change that reformats the whole corpus has to be deliberate.
    #[test]
    fn every_sound_script_in_the_corpus_survives_a_round_trip() {
        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }

        let trimmed = |text: &str| -> Vec<String> {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        };

        let mut checked = 0usize;
        let mut byte_exact = 0usize;
        let mut problems: Vec<String> = Vec::new();
        for (path, body) in &bodies {
            let Some(original) = function_interior(body, "sound_") else {
                continue;
            };
            checked += 1;
            let emitted = emit_sound_body(&parse_sound_script(body));

            if trimmed(&emitted) != trimmed(&original) {
                problems.push(format!(
                    "{path} lost or changed a line:\n--- was ---\n{original}--- now ---\n{emitted}"
                ));
                continue;
            }
            // Re-parsing the emitted text has to reach the same place, or a second save would
            // move the file again.
            let again = emit_sound_body(&parse_sound_script(&format!(
                "unsafe extern \"C\" fn sound_test(agent: &mut L2CAgentBase) {{\n{emitted}}}\n"
            )));
            if again != emitted {
                problems.push(format!(
                    "{path} does not settle:\n--- once ---\n{emitted}--- twice ---\n{again}"
                ));
                continue;
            }
            byte_exact += usize::from(emitted == original);
        }

        assert!(
            problems.is_empty(),
            "{} of {checked} sound scripts do not round-trip:\n\n{}",
            problems.len(),
            problems.join("\n")
        );
        assert!(
            checked > 100,
            "only {checked} sound scripts found — the corpus is too thin to be a gate"
        );
        assert_eq!(
            checked - byte_exact,
            6,
            "{} of {checked} sound scripts came back with different whitespace; 6 are known \
             mis-indented at source, so any other number is the emitter's formatting changing",
            checked - byte_exact
        );
    }

    /// One real vanilla sound script, read end to end.
    ///
    /// `kirby/TurnDash` because it is the only shape in the corpus that carries all three of
    /// the family's argument layouts at once — one hash, two hashes, and a hash with a trailing
    /// number — and it spaces them with a `frame` and two `wait`s, so a walk that ignored the
    /// timing would still have to produce four events but would put them in the wrong places.
    #[test]
    fn a_vanilla_sound_script_reads_as_typed_calls_at_the_frames_it_writes() {
        const TURN_DASH: &str = r#"unsafe extern "C" fn sound_turndash(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::PLAY_SE(agent, Hash40::new("se_kirby_dash_start"));
        macros::SET_PLAY_INHIVIT(agent, Hash40::new("se_kirby_dash_start"), 20);
    }
    wait(agent.lua_state_agent, 13.0);
    if macros::is_excute(agent) {
        macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_left_m"), Hash40::new("se_kirby_step_right_m"));
    }
    wait(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_right_m"), Hash40::new("se_kirby_step_left_m"));
    }
}
"#;
        let script = parse_sound_script(TURN_DASH);
        let events = script.to_sound_events();
        let seen: Vec<(u32, &str, Vec<&str>, Option<&str>)> = events
            .iter()
            .map(|e| {
                (
                    e.frame,
                    e.call.func.as_str(),
                    e.call.sounds.iter().map(String::as_str).collect(),
                    e.call.tail.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                (6, "PLAY_SE", vec!["se_kirby_dash_start"], None),
                (
                    6,
                    "SET_PLAY_INHIVIT",
                    vec!["se_kirby_dash_start"],
                    Some("20")
                ),
                (
                    19,
                    "PLAY_STEP_FLIPPABLE",
                    vec!["se_kirby_step_left_m", "se_kirby_step_right_m"],
                    None
                ),
                (
                    23,
                    "PLAY_STEP_FLIPPABLE",
                    vec!["se_kirby_step_right_m", "se_kirby_step_left_m"],
                    None
                ),
            ]
        );

        // Parse → emit → parse reaches the same events, which is the property that lets a
        // typed call be regenerated instead of copied.
        let again = parse_sound_script(&format!(
            "unsafe extern \"C\" fn sound_turndash(agent: &mut L2CAgentBase) {{\n{}}}\n",
            emit_sound_body(&script)
        ));
        assert_eq!(again.to_sound_events(), events);
    }

    /// A sound written outside every `is_excute` block, which 15 corpus scripts do.
    ///
    /// `kirby/WalkMiddle` verbatim, because it writes one footstep each way: the first is
    /// wrapped and the second is bare. Before D1c the bare one parsed as `Raw` and the move
    /// showed one footstep instead of two — and the corpus oracle counts *statements*, so it
    /// would go on passing if a bare call were typed but never walked. This is the assertion
    /// that says the event comes out the other end.
    #[test]
    fn a_sound_written_outside_an_excute_block_still_fires() {
        const WALK_MIDDLE: &str = r#"unsafe extern "C" fn sound_walkmiddle(agent: &mut L2CAgentBase) {
    wait_loop_sync_mot();
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_left_m"), Hash40::new("se_kirby_step_right_m"));
    }
    frame(agent.lua_state_agent, 30.0);
    macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_right_m"), Hash40::new("se_kirby_step_left_m"));
}
"#;
        let script = parse_sound_script(WALK_MIDDLE);
        let frames: Vec<u32> = script.to_sound_events().iter().map(|e| e.frame).collect();
        assert_eq!(
            frames,
            vec![8, 30],
            "the bare footstep on frame 30 was lost"
        );

        // And it is written back bare. Emitting it inside an `if macros::is_excute(agent) {`
        // it never had would be a behaviour change, not a formatting one: the wrapper decides
        // whether the line runs on this pass at all.
        let emitted = emit_sound_body(&script);
        assert!(
            emitted.contains(
                "\n    macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new(\"se_kirby_step_right_m\")"
            ),
            "the bare call did not come back at the function's own indent:\n{emitted}"
        );

        // And it is *editable*. Resolving a site has to walk the same shapes the site counter
        // counts, `Bare` included — drop it from either one and this move's second footstep is
        // either silently uneditable or, worse, edited by writing to the first.
        let mut edited = script;
        let events = edited.to_sound_events();
        assert_eq!(events[1].site, 1, "the bare call is the second site");
        let mut call = events[1].call.clone();
        call.sounds[0] = "se_common_step_right_m".into();
        *edited
            .sound_stmt_mut(events[1].site)
            .expect("the bare call must resolve to a statement") =
            crate::data::ExcuteStmt::Sound(call);

        let after = emit_sound_body(&edited);
        assert!(
            after.contains(
                "macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new(\"se_common_step_right_m\"), \
                 Hash40::new(\"se_kirby_step_left_m\"));"
            ),
            "the edit did not reach the bare call:\n{after}"
        );
        assert!(
            after.contains(
                "macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new(\"se_kirby_step_left_m\"), \
                 Hash40::new(\"se_kirby_step_right_m\"));"
            ),
            "the edit landed in the wrapped call instead:\n{after}"
        );
    }

    /// Every iteration of a looped sound comes back as the *same* site, and the call after the
    /// loop gets the next one.
    ///
    /// **Composed rather than lifted, and that needs saying.** Not one of the 301 corpus sound
    /// scripts contains a `for`, which the corpus oracle records as measured fact — so there is
    /// no vanilla script to lift this from. What is here is two corpus-verified shapes joined:
    /// the `for _ in 0..3 {` header is written verbatim by real `effect_` scripts, and the calls
    /// inside it are `kirby/TurnDash`'s own lines. Neither half is invented, which is the part
    /// [[hand-written-fixtures-are-evidence-of-nothing]] is actually about — a *signature* nobody
    /// checked, not a control-flow shape the parser already handles.
    ///
    /// The property under test is the one that makes an edit land on the right line: three
    /// iterations of one `PLAY_SE` are three events, all of site 0, because all three are the
    /// one line in the file. Get this wrong and the site cursor runs ahead, so retuning the
    /// footstep after the loop rewrites something else — and the result still parses, still
    /// compiles, and still round-trips.
    #[test]
    fn a_looped_sound_reports_the_same_site_every_time_round() {
        const LOOPED: &str = r#"unsafe extern "C" fn sound_looped(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    for _ in 0..3 {
    wait(agent.lua_state_agent, 2.0);
    if macros::is_excute(agent) {
        macros::PLAY_SE(agent, Hash40::new("se_kirby_dash_start"));
    }
    }
    wait(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_left_m"), Hash40::new("se_kirby_step_right_m"));
    }
}
"#;
        let events = parse_sound_script(LOOPED).to_sound_events();
        let seen: Vec<(u32, usize, &str)> = events
            .iter()
            .map(|e| (e.frame, e.site, e.call.func.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (8, 0, "PLAY_SE"),
                (10, 0, "PLAY_SE"),
                (12, 0, "PLAY_SE"),
                (16, 1, "PLAY_STEP_FLIPPABLE"),
            ],
            "the site cursor did not rewind for each iteration"
        );

        // And the other half of the join: those ordinals index the textual scan write-back uses.
        // Checking only that they are `0, 0, 0, 1` would pass just as well if the scan happened
        // to list the two calls the other way round.
        let sites = crate::acmd_src::sound_sites(LOOPED);
        for event in &events {
            assert_eq!(
                sites[event.site].name, event.call.func,
                "site {} is not the `{}` it was resolved for",
                event.site, event.call.func
            );
        }
    }

    /// A loop that runs no iterations still steps the site cursor over its body.
    ///
    /// This is the one thing `count_sound_stmts` is for, and it is reachable only through a
    /// zero-count `for`: at one iteration or more the cursor arrives at the right place on its
    /// own. So the function is untestable through any realistic script, and a mutation deleting
    /// its `Bare` arm passed every other test in this file — including the corpus oracle, which
    /// cannot help, because no vanilla sound script loops anything.
    ///
    /// Both statement shapes go inside the loop deliberately. A count that saw only the wrapped
    /// call would leave the trailing footstep resolving to site 1, which is the *bare* call
    /// inside the loop that never ran — an edit written to a line the game skipped.
    #[test]
    fn a_loop_that_never_runs_still_advances_the_site_cursor() {
        const EMPTY_LOOP: &str = r#"unsafe extern "C" fn sound_emptyloop(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    for _ in 0..0 {
    if macros::is_excute(agent) {
        macros::PLAY_SE(agent, Hash40::new("se_kirby_dash_start"));
    }
    macros::PLAY_SE(agent, Hash40::new("se_kirby_dash_stop"));
    }
    wait(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_left_m"), Hash40::new("se_kirby_step_right_m"));
    }
}
"#;
        let events = parse_sound_script(EMPTY_LOOP).to_sound_events();
        assert_eq!(
            events.len(),
            1,
            "a loop with no iterations played a sound anyway"
        );
        let sites = crate::acmd_src::sound_sites(EMPTY_LOOP);
        assert_eq!(
            sites.len(),
            3,
            "the scan should still see all three calls — the text has them either way"
        );
        assert_eq!(
            events[0].site, 2,
            "the surviving footstep resolved to `{}`, a call inside the loop that never ran",
            sites[events[0].site].name
        );
    }

    /// The sound family's own `RawBlock` arms, which had the code but never the test.
    ///
    /// `count_sound_stmts` and `sound_stmt_mut` have descended into a raw block since D1c, on
    /// evidence — 26 corpus sounds sit inside one, against zero hurtbox and zero attack-modifier
    /// statements, which is why the sound family got the arm first and the other two waited for
    /// B6. But *having* the arm was never pinned: deleting `RawBlock` from either left all 398
    /// other tests green, verified by mutation.
    ///
    /// The corpus oracle beside this one cannot close the gap however many scripts it reads. It
    /// compares the walk against `acmd_src::sound_sites`, a *textual* scan — a different function
    /// from `sound_stmt_mut`, which resolves against the IR. Two implementations of "which call
    /// is site N", and the oracle only ever exercises one of them.
    #[test]
    fn a_sound_inside_a_runtime_branch_is_counted_and_resolvable() {
        const SOUND_IN_RAW_BLOCK: &str = r#"unsafe extern "C" fn sound_rawblocksound(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 2.0);
    for _ in 0..0 {
    if WorkModule::is_flag(agent.module_accessor, *FIGHTER_STATUS_SQUAT_FLAG_REQUEST_SQUAT_SE) {
    if macros::is_excute(agent) {
        macros::PLAY_SE(agent, Hash40::new("se_common_guardoff"));
    }
    }
    }
    frame(agent.lua_state_agent, 6.0);
    if WorkModule::is_flag(agent.module_accessor, *FIGHTER_STATUS_SQUAT_FLAG_REQUEST_SQUAT_SE) {
    if macros::is_excute(agent) {
        macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_left_m"), Hash40::new("se_kirby_step_right_m"));
    }
    }
}
"#;
        let mut script = parse_sound_script(SOUND_IN_RAW_BLOCK);
        let events = script.to_sound_events();
        assert_eq!(events.len(), 1, "the skipped loop played a sound anyway");
        // Site 1 exercises the counter: without its `RawBlock` arm the cursor never steps over
        // the call inside the skipped branch, and this event claims site 0.
        assert_eq!(
            events[0].site, 1,
            "the surviving call took the site of the one inside the skipped loop"
        );
        // And resolving it exercises the walker, which is the other half and the other function.
        let stmt = script
            .sound_stmt_mut(events[0].site)
            .expect("a sound inside a branch must resolve to a statement");
        let crate::data::ExcuteStmt::Sound(call) = stmt else {
            panic!("site {} resolved to {stmt:?}, not a sound", events[0].site);
        };
        assert_eq!(
            call.func, "PLAY_STEP_FLIPPABLE",
            "resolved to the wrong call — an edit here would rewrite a different line"
        );
    }

    /// Every sound call the corpus writes is *typed*, not left as `Raw`.
    ///
    /// The round-trip gate above cannot see this and never could: an unrecognised line
    /// round-trips perfectly precisely *because* it is copied verbatim. So it stayed green
    /// through the whole of D1a, when nothing in the family was typed at all, and it would stay
    /// green if a member silently stopped being recognised tomorrow. This is the assertion that
    /// says the new thing works rather than that nothing broke.
    ///
    /// Counted per script against the source text rather than in total, so a file that loses
    /// its calls cannot be hidden by another that gains some.
    #[test]
    fn every_sound_call_in_the_corpus_is_typed_rather_than_left_raw() {
        fn typed(stmts: &[crate::data::AcmdStmt]) -> usize {
            stmts
                .iter()
                .map(|stmt| match stmt {
                    crate::data::AcmdStmt::Excute(inner) => inner
                        .iter()
                        .filter(|s| matches!(s, ExcuteStmt::Sound(_)))
                        .count(),
                    crate::data::AcmdStmt::Bare(s) => {
                        usize::from(matches!(s.as_ref(), ExcuteStmt::Sound(_)))
                    }
                    crate::data::AcmdStmt::Loop { body, .. }
                    | crate::data::AcmdStmt::RawBlock { body, .. } => typed(body),
                    _ => 0,
                })
                .sum()
        }

        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }

        let mut total = 0usize;
        let mut problems: Vec<String> = Vec::new();
        for (path, body) in &bodies {
            let Some(interior) = function_interior(body, "sound_") else {
                continue;
            };
            let mut written = 0usize;
            for line in interior.lines() {
                let matched = SOUND_FUNCS
                    .iter()
                    .filter(|(name, _, _)| line.contains(&format!("macros::{name}(")))
                    .count();
                // Two needles on one line means a family name is a prefix of another and the
                // paren is not separating them after all — the `ATTACK`/`ATTACK_ABS` trap. The
                // wrong layout would then read `PLAY_STEP_FLIPPABLE`'s second footstep away.
                assert!(matched <= 1, "{path}: two sound macros matched `{line}`");
                written += matched;
            }
            let found = typed(&parse_sound_script(body).stmts);
            if found != written {
                problems.push(format!("{path}: {written} calls written, {found} typed"));
            }
            total += written;
        }

        assert!(
            problems.is_empty(),
            "{} corpus sound scripts lost a call to `Raw`:\n{}",
            problems.len(),
            problems.join("\n")
        );
        assert!(
            total > 500,
            "only {total} sound calls found — the corpus is too thin to be a gate"
        );
    }

    /// Every sound event's site resolves to a call of its own macro, in every corpus script.
    ///
    /// This is the assertion no round trip can make. A site is the join between the walked IR
    /// and a textual scan of the source, and the two are counted by different code: the walk
    /// unrolls `for` bodies and steps over empty ones, while the scan just reads the file. When
    /// they disagree, an edit is written to a *different call* — and the result is a perfectly
    /// well-formed script, so parsing it back proves nothing. Comparing the macro name at the
    /// resolved site is what turns a silent retarget into a failure.
    ///
    /// The corpus is the right place for it because the shapes that break a site counter are
    /// the ones nobody writes by hand: a sound inside a `for`, a sound after a `for`, and a
    /// sound outside every `is_excute` block.
    #[test]
    fn every_corpus_sound_site_lands_on_a_call_of_its_own_macro() {
        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }

        let mut checked = 0usize;
        let mut problems: Vec<String> = Vec::new();
        for (path, body) in &bodies {
            let Some(interior) = function_interior(body, "sound_") else {
                continue;
            };
            let sites = crate::acmd_src::sound_sites(&interior);
            let events = parse_sound_script(body).to_sound_events();
            // Every call in the text is reached by at least one event. A call the walk never
            // visits is invisible in the panel and unreachable by an edit, and the typed-count
            // oracle beside this one cannot see that: it counts *parsed* statements, so one
            // that is parsed and then skipped by `eval_stmts` passes it.
            let mut reached: Vec<usize> = events.iter().map(|e| e.site).collect();
            reached.sort_unstable();
            reached.dedup();
            if reached.len() != sites.len() {
                problems.push(format!(
                    "{path}: {} calls in the text, {} reached by the walk",
                    sites.len(),
                    reached.len()
                ));
            }
            for event in &events {
                match sites.get(event.site) {
                    Some(site) if site.name == event.call.func => {}
                    Some(site) => problems.push(format!(
                        "{path}: the `{}` on frame {} resolved to site {}, which is a `{}`",
                        event.call.func, event.frame, event.site, site.name
                    )),
                    None => problems.push(format!(
                        "{path}: the `{}` on frame {} has site {}, past the {} calls in the text",
                        event.call.func,
                        event.frame,
                        event.site,
                        sites.len()
                    )),
                }
            }
            checked += events.len();
        }

        assert!(
            problems.is_empty(),
            "{} sound events resolved to the wrong call:\n{}",
            problems.len(),
            problems.join("\n")
        );
        assert!(
            checked > 500,
            "only {checked} sound events resolved — the corpus is too thin to be a gate"
        );
        // **Measured, not assumed: not one of the 301 corpus sound scripts contains a `for`.**
        // A guard asserting otherwise was written here first and failed, which is how this is
        // known. So the corpus cannot exercise the loop rewind at all, and the case is covered
        // by `a_looped_sound_reports_the_same_site_every_time_round` instead — do not read a
        // green run here as saying anything about a looped call.
    }

    /// The same brace property over every `game_` script in the corpus, which is where the bug
    /// actually was: 35 of them open a runtime branch, and each one exported a function with
    /// more `{` than `}`. Nothing downstream compiled, and no test said so — the effect and
    /// hitbox round-trips both re-parse the output with the same lenient parser that wrote it,
    /// so an unbalanced function read back exactly as it went in.
    #[test]
    fn no_game_script_in_the_corpus_exports_an_unbalanced_function() {
        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }

        let mut checked = 0usize;
        let mut branching = 0usize;
        let mut problems: Vec<String> = Vec::new();
        for (path, body) in &bodies {
            if function_interior(body, "game_").is_none() {
                continue;
            }
            checked += 1;
            let script = parse_acmd_script(body);
            if script
                .stmts
                .iter()
                .any(|stmt| matches!(stmt, crate::data::AcmdStmt::RawBlock { .. }))
            {
                branching += 1;
            }
            let emitted = emit_stmts(&script.stmts, "    ").join("\n");
            let opens = emitted.matches('{').count();
            let closes = emitted.matches('}').count();
            if opens != closes {
                problems.push(format!("{path}: {opens} `{{` against {closes} `}}`"));
            }
        }

        assert!(
            problems.is_empty(),
            "{} of {checked} game scripts export a function that does not close:\n{}",
            problems.len(),
            problems.join("\n")
        );
        assert!(
            branching >= 30,
            "only {branching} of {checked} game scripts have a top-level branch — this test \
             passes vacuously if branches stop being recognised"
        );
    }

    /// The bug D1b fixes, stated over real vanilla text.
    ///
    /// Only the project half is hand-written, and deliberately so: it is a stand-in for
    /// arbitrary user source, and what is under test is which *category* survives the merge,
    /// not what the user wrote. The mirror is the corpus, because the thing that must not
    /// disappear — a move's real effects, a move's real hitboxes — only exists there.
    #[test]
    fn a_partial_project_override_keeps_every_category_it_does_not_define() {
        let bodies = corpus_bodies();
        if bodies.is_empty() {
            return;
        }

        const GAME_ONLY: &str = "unsafe extern \"C\" fn game_x(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n\n";
        const EFFECT_ONLY: &str =
            "unsafe extern \"C\" fn effect_x(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n\n";
        const SOUND_ONLY: &str = "unsafe extern \"C\" fn sound_x(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n\n";

        // A category must arrive from exactly one side. Two `game_` functions in one body
        // parse fine — the first one wins — so nothing above would notice the merge appending
        // vanilla's copy underneath the project's, right up until an export wrote both out.
        let single_headers = |path: &str, merged: &str| {
            for prefix in SCRIPT_PREFIXES {
                let headers = merged
                    .lines()
                    .filter(|line| is_function_header(line.trim(), prefix))
                    .count();
                assert!(
                    headers <= 1,
                    "{path}: {headers} `{prefix}` functions in the merge"
                );
            }
        };

        // Counted separately, because a move only proves a category can be lost if it has one
        // to lose. Requiring both at once would have run this over 52 of the 460 corpus files.
        let (mut with_effects, mut with_hitboxes) = (0usize, 0usize);
        for (path, mirror) in &bodies {
            let hitboxes = parse_acmd_script(mirror).to_hitboxes().len();
            let effects = parse_effect_script(mirror).to_effect_calls().len();

            if effects > 0 {
                with_effects += 1;
                // Overriding the hitboxes must not take the effects with it — the shape of
                // most hitbox mods, and what used to display as a move with no effects at all.
                let merged = merge_project_over_mirror(GAME_ONLY, &["game_"], mirror);
                assert_eq!(
                    parse_effect_script(&merged).to_effect_calls().len(),
                    effects,
                    "{path}: a game-only override lost the mirror's effects"
                );
                // …and the override still wins where it was made: vanilla's `game_` must not
                // come back alongside it, or the move shows hitboxes the project deleted.
                assert!(
                    parse_acmd_script(&merged).to_hitboxes().is_empty(),
                    "{path}: the mirror's hitboxes outlived the project's own game_"
                );
                single_headers(path, &merged);
            }

            if hitboxes > 0 {
                with_hitboxes += 1;
                let merged = merge_project_over_mirror(EFFECT_ONLY, &["effect_"], mirror);
                assert_eq!(
                    parse_acmd_script(&merged).to_hitboxes().len(),
                    hitboxes,
                    "{path}: an effect-only override lost the mirror's hitboxes"
                );
                assert!(
                    parse_effect_script(&merged).to_effect_calls().is_empty(),
                    "{path}: the mirror's effects outlived the project's own effect_"
                );
                single_headers(path, &merged);
            }

            // The case D1a named. A sound-only project used to fall back to the mirror whole,
            // so its own sounds were the one thing that did not survive being loaded.
            let merged = merge_project_over_mirror(SOUND_ONLY, &["sound_"], mirror);
            assert_eq!(
                parse_acmd_script(&merged).to_hitboxes().len(),
                hitboxes,
                "{path}: a sound-only override lost the mirror's hitboxes"
            );
            assert_eq!(
                parse_effect_script(&merged).to_effect_calls().len(),
                effects,
                "{path}: a sound-only override lost the mirror's effects"
            );
            assert_eq!(
                parse_sound_script(&merged).stmts.len(),
                1,
                "{path}: the mirror's sounds outlived the project's own sound_"
            );
            single_headers(path, &merged);
        }

        assert!(
            with_effects > 100 && with_hitboxes > 60,
            "only {with_effects} corpus moves have effects and {with_hitboxes} have hitboxes — \
             this test passes vacuously below that"
        );
    }

    /// Kirby's Final Smash start, verbatim: a `frame()` and an `is_excute` inside a runtime
    /// branch, with an `else` arm.
    ///
    /// Before this was a block, the branch's opening line was kept as `Raw` and its closing
    /// brace was thrown away, so the exported function had two fewer `}` than `{` and did not
    /// close — the next function in the generated file was swallowed by it. Both arms were also
    /// promoted to unconditional, so `FT_START_CUTIN` was issued twice.
    const RAW_BRANCH: &str = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 1.0);
    if !WorkModule::is_flag(agent.module_accessor, *FIGHTER_INSTANCE_WORK_ID_FLAG_DISABLE_FINAL_START_CAMERA) {
        frame(agent.lua_state_agent, 10.0);
        if macros::is_excute(agent) {
            macros::FT_START_CUTIN(agent);
        }
    }
    else {
        if macros::is_excute(agent) {
            macros::FT_START_CUTIN(agent);
        }
    }
    frame(agent.lua_state_agent, 17.0);
}
"#;

    #[test]
    fn a_branch_keeps_the_brace_that_closes_it() {
        let script = parse_acmd_script(RAW_BRANCH);
        let emitted = emit_stmts(&script.stmts, "    ").join("\n");
        let opens = emitted.matches('{').count();
        let closes = emitted.matches('}').count();
        assert_eq!(
            (opens, closes),
            (4, 4),
            "the branch, its else and the two excute blocks must all close:\n{emitted}"
        );
        // The `else` has to stay attached to something, or it is not Rust at all.
        assert!(
            emitted.contains("    }\n    else {"),
            "the else arm lost the brace it hangs off:\n{emitted}"
        );
        // One issue per arm, not two in a row at the top level.
        assert_eq!(
            emitted.matches("FT_START_CUTIN").count(),
            2,
            "both arms are still there:\n{emitted}"
        );
    }

    /// The frame walk has always read a conditional hitbox as an unconditional one, because the
    /// branch used to be flattened. Blocks must not change that on their own — eighteen `game_`
    /// scripts in the corpus put an `ATTACK` inside an `if`, and they would all lose their
    /// hitbox from the editor the moment a branch stopped being walked.
    #[test]
    fn a_hitbox_inside_a_branch_is_still_seen_by_the_frame_walk() {
        let source = r#"
unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if WorkModule::get_int(agent.module_accessor, *FIGHTER_STATUS_WORK_ID_INT_STRENGTH) == 0 {
        if macros::is_excute(agent) {
            macros::ATTACK(agent, 0, 0, Hash40::new("top"), 12.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, *ATTACK_SETOFF_KIND_ON, *ATTACK_LR_CHECK_F, false, 0, 0.0, 0, false, false, false, false, true, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_M, *COLLISION_SOUND_ATTR_PUNCH, *ATTACK_REGION_PUNCH);
        }
    }
}
"#;
        let hitboxes = parse_acmd_script(source).to_hitboxes();
        assert_eq!(hitboxes.len(), 1, "the conditional hitbox is still read");
        assert_eq!((hitboxes[0].active_start, hitboxes[0].damage), (4, 12.0));
    }

    /// A body holding an `effect_` function but no `game_` used to be read *whole* by the game
    /// parser, so every effect line came back as `Raw` — and `emit_stmts` writes `Raw` straight
    /// into the generated `game_` function. The move's effects would spawn twice: once from the
    /// generated effect script, once from the game one.
    #[test]
    fn a_body_with_no_game_function_contributes_no_game_statements() {
        let source = r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 3.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_attack_impact"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, true);
    }
}
"#;
        assert!(
            parse_acmd_script(source).stmts.is_empty(),
            "an effect-only body has no game script in it"
        );
        // The headerless case is what the fallback exists for, and still works.
        assert!(
            !parse_acmd_script("    frame(agent.lua_state_agent, 3.0);\n")
                .stmts
                .is_empty(),
            "a bare pasted body is still read whole"
        );
    }

    /// Across the whole corpus, the calls the parser makes and the sites the rewriter can edit
    /// are the same list.
    ///
    /// `rewrite_effect_calls` looks a call up by its ordinal among the scanned sites, so the two
    /// have to agree not just in count but position-for-position. When they do not, write-back
    /// does not fail — it splices the user's edit into a *different* call, which is why this is
    /// asserted over every script rather than left to the four trail lines that motivated it.
    ///
    /// The guard on the trail count is the part that keeps this honest: C2 made the raw command
    /// form produce a call, and if a later change stops it doing so, every other script here
    /// still agrees trivially and this test would go on passing while covering nothing.
    #[test]
    fn every_corpus_script_scans_to_exactly_the_calls_it_parses_to() {
        let cache = crate::scratch_dirs::app_storage_root().join("script-cache");
        if !cache.is_dir() {
            return;
        }
        let mut checked = 0usize;
        let mut trails = 0usize;
        let mut mismatched = Vec::new();
        for fighter in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
            for script in std::fs::read_dir(fighter.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let Ok(body) = std::fs::read_to_string(script.path()) else {
                    continue;
                };
                let ordinals = parse_effect_script(&body).call_macro_ordinals();
                let sites = crate::acmd_src::spawn_site_names(&body);
                trails += sites
                    .iter()
                    .filter(|name| name.starts_with("AFTER_IMAGE3"))
                    .count();
                if ordinals.iter().copied().max().map(|n| n + 1).unwrap_or(0) > sites.len() {
                    mismatched.push(format!(
                        "{:?}: {} calls but only {} sites",
                        script.path(),
                        ordinals.len(),
                        sites.len()
                    ));
                }
                checked += 1;
            }
        }
        if checked == 0 {
            return;
        }
        assert!(
            mismatched.is_empty(),
            "a call with no site behind it renumbers every later one:\n{}",
            mismatched.join("\n")
        );
        assert_eq!(
            trails, 4,
            "the corpus holds four raw-command trails; if they stopped scanning as sites this \
             test would agree with itself and cover nothing"
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
            let (_, emitted) =
                emit_effect_move_fn(&calls, "audit", &Default::default(), &Default::default());
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
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        // The distinct residue lines the corpus produces, and any that failed to reach an
        // export. Both are asserted below — a count that only ever shrinks would be satisfied by
        // a change that stopped producing residue at all.
        let mut emitted_residue: Vec<String> = Vec::new();
        let mut missing_residue: Vec<String> = Vec::new();
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
                let (calls, residue) = script.to_effect_calls_and_residue();
                if calls.is_empty() {
                    continue;
                }
                with_calls += 1;
                let lost = unexportable_effect_lines(&script);
                if !lost.is_empty() {
                    lossy += 1;
                }
                for line in &lost {
                    *kinds.entry(loss_kind(line)).or_default() += 1;
                }
                // Every residue line must come back out. Asserted here rather than trusted,
                // because the emitter takes residue as a separate argument and a caller that
                // passes an empty map deletes exactly what this test claims is now kept.
                for line in residue.values().flatten() {
                    let text = line.trim();
                    if text.contains("is_excute") || !text.chars().any(|c| c.is_alphanumeric()) {
                        continue;
                    }
                    if !emitted_residue.iter().any(|s| s == text) {
                        emitted_residue.push(text.to_string());
                    }
                }
                let (_, emitted) =
                    emit_effect_move_fn(&calls, "audit", &Default::default(), &residue);
                for text in residue.values().flatten() {
                    let text = text.trim();
                    if text.chars().any(|c| c.is_alphanumeric()) && !emitted.contains(text) {
                        missing_residue.push(format!("{:?}: {text}", entry.path()));
                    }
                }
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
            lossy <= 12,
            "the export deletes lines from {lossy} of {with_calls} effect scripts; C5 measured \
             28, C6 brought it to 19, C6b's COL_NORMAL to 15, C2's raw-command trail to 13, and \
             E3's frame-anchored residue to 12. Something started dropping user code again."
        );
        // The other half of the ratchet, and the half E3 needed. `lossy` counts what the report
        // *names*; these two count what the export *writes*. A change that stopped producing
        // residue — or that quietly passed an empty map to the emitter — would leave `lossy` at
        // 12 and be caught only here.
        assert!(
            emitted_residue.len() >= 3,
            "the corpus stopped producing frame-anchored residue, so the assertion below is \
             measuring nothing: {emitted_residue:?}"
        );
        assert!(
            missing_residue.is_empty(),
            "residue that reached the emitter and did not reach its output:\n{}",
            missing_residue.join("\n")
        );

        // The count alone was not enough. C6b wrote its remainder down as a table of which
        // lines these are, and that table listed 18 of the 20 — the two raw
        // `effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, …)` calls were simply missing from it,
        // and nothing could notice because only the script count was asserted. An entry that
        // says "here is exactly what is left" has to be pinned to what is left, not to how much
        // of it there is.
        //
        // C2 removed four entries at once, which is more than it set out to. Typing the two raw
        // trail calls was the point; the two `methodlib::L2CAgent::pop()` lines went with them
        // because each sits in the same `is_excute` block as a trail and had no spawn to ride on
        // until the trail became one. C2's own estimate — 20 lines to 18, 15 scripts staying 15 —
        // assumed `pop()` would keep those two files lossy, and it was wrong in the useful
        // direction: kirby `SpecialHi2` and `SpecialAirHi2` are now clean.
        let expected: std::collections::BTreeMap<String, usize> =
            [("wait_loop_sync_mot", 7), ("else {", 5)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
        assert_eq!(
            kinds, expected,
            "the remaining losses are not the ones C6b measured — update that entry's table"
        );
    }

    /// Collapse a dropped line to what it *is*, so the audit above can assert composition
    /// without pinning argument values. `macros::X(…)` → `macros::X`; the raw `effect(*CMD, …)`
    /// form → `effect(*CMD)`, since every such call would otherwise collapse to `effect`.
    fn loss_kind(line: &str) -> String {
        let line = line.trim();
        let Some(open) = line.find('(') else {
            return line.to_string();
        };
        let head = &line[..open];
        if head != "effect" {
            return head.to_string();
        }
        let rest = &line[open + 1..];
        let end = rest.find(',').unwrap_or(rest.len());
        format!("effect({})", rest[..end].trim())
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
        let effect = preview_effect_fn(&sample.1, "attack_air_n", &sample.3, &Default::default());
        assert!(
            acmd.contains(&effect),
            "the previewed effect_* function is not what was exported:\n{effect}\n---\n{acmd}"
        );
        // And the preview really is showing the user's macro, not a substitute.
        assert!(effect.contains("macros::EFFECT_FOLLOW(agent, Hash40::new(\"sys_flash\")"));

        let sound = preview_sound_fn(&sample.2, "attack_air_n");
        assert!(
            acmd.contains(&sound),
            "the previewed sound_* function is not what was exported:\n{sound}\n---\n{acmd}"
        );
        // The function is named for the category, not for the move alone. Get this wrong and
        // the plugin installs the move's sounds over its `game_` script, which does compile.
        assert!(sound.starts_with("unsafe extern \"C\" fn sound_attackairn("));
        assert!(
            sound.contains(
                "macros::SET_PLAY_INHIVIT(agent, Hash40::new(\"se_kirby_dash_start\"), 20);"
            ),
            "the one member with a non-hash tail lost it:\n{sound}"
        );
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
