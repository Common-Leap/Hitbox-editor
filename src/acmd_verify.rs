//! Verification of the Rust that Visionary generates.
//!
//! Every export runs through here before anything is written. The point is not to re-state
//! what the emitter meant to do — that would only agree with itself — but to *read the code
//! back* the same way the editor reads a user's own script, and check that what comes out is
//! what the user specified. Everything in this module is derived from the emitted text, not
//! from the emitter's intentions.
//!
//! Four properties are checked, in the order a failure matters:
//!
//! 1. **It is Rust.** Every generated `.rs` file is parsed with `syn`. Raw script lines and
//!    recorded macro tails are spliced into the output verbatim, so this is not a formality.
//! 2. **It says what the user said.** Each emitted function is re-parsed by the editor's own
//!    parser and the resulting hitboxes and effect calls are compared field by field with the
//!    ones the user edited. A single rounded decimal is a finding.
//! 3. **It will compile and run.** Values that produce a well-formed but broken call — a
//!    non-finite float, a graphic name with a quote in it, a wind command with the wrong
//!    number of arguments — are caught here rather than by the user's toolchain, or worse, by
//!    the game.
//! 4. **It is not wasteful.** Empty blocks, dead statements, and calls the game will never
//!    reach are reported so the generated file stays worth reading.
//! 5. **It does not lose anything.** The effect export rebuilds the function out of the calls
//!    it recognises, so a line it has no variant for is deleted rather than kept. Each one is
//!    named against the script it came from.
//!
//! Anything in class 1–3 is a [`Severity::Blocker`] and fails the export outright: a mod that
//! does not compile, or that quietly ships different numbers from the ones on screen, is worse
//! than no mod. Classes 4 and 5 are a [`Severity::Warning`] and only inform — see
//! [`check_dropped_lines`] for why a real loss is not allowed to fail an export.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::acmd::ModProject;
use crate::data::{AcmdScript, AcmdStmt, EffectCall, EffectScript, ExcuteStmt, Hitbox};
use crate::mod_project::LiveTweak;

/// How much a finding matters. Ordered so `max()` picks the worse one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The export is still correct; the generated code could just be tidier.
    Warning,
    /// The export is wrong. It will not compile, or it does not match what the user specified.
    Blocker,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    /// What the finding is about — `"mario / attack_air_n"`, or a generated file path.
    pub subject: String,
    pub message: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.subject, self.message)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    fn push(&mut self, severity: Severity, subject: impl Into<String>, message: impl Into<String>) {
        self.findings.push(Finding {
            severity,
            subject: subject.into(),
            message: message.into(),
        });
    }

    fn blocker(&mut self, subject: impl Into<String>, message: impl Into<String>) {
        self.push(Severity::Blocker, subject, message);
    }

    fn warn(&mut self, subject: impl Into<String>, message: impl Into<String>) {
        self.push(Severity::Warning, subject, message);
    }

    pub fn blockers(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Blocker)
    }

    pub fn has_blockers(&self) -> bool {
        self.blockers().next().is_some()
    }

    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// One line per blocker, for the error an export fails with.
    ///
    /// Capped, because a single structural mistake can produce one finding per hitbox and an
    /// error dialog listing four hundred of them tells the user less than five does.
    pub fn blocker_summary(&self) -> String {
        const SHOWN: usize = 8;
        let all: Vec<String> = self.blockers().map(|f| f.to_string()).collect();
        let mut out = all
            .iter()
            .take(SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if all.len() > SHOWN {
            out.push_str(&format!("\n… and {} more", all.len() - SHOWN));
        }
        out
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Verify a generated project against the edits it was built from.
///
/// `acmd_edits` and `effect_edits` are the same `(fighter, move, …)` lists handed to
/// [`crate::acmd::build_mod_project_full`], so this checks the code that is actually about to
/// be written rather than a re-derivation of it.
pub fn verify_export(
    project: &ModProject,
    acmd_edits: &[(String, String, AcmdScript)],
    effect_edits: &[(String, String, Vec<EffectCall>)],
    tweaks: &[LiveTweak],
) -> Report {
    let mut report = Report::default();
    check_files(project, &mut report);

    // Every emitted function must both survive a read-back and actually appear in a shipped
    // file. Verifying the text alone would pass a project that dropped the move on the floor.
    let sources: Vec<&str> = project
        .files
        .iter()
        .filter(|file| file.rel_path.ends_with(".rs"))
        .map(|file| file.contents.as_str())
        .collect();

    for (fighter, move_name, script) in acmd_edits {
        let subject = format!("{fighter} / {move_name}");
        let emitted = crate::acmd::preview_game_fn(script, move_name);
        verify_move(&subject, script, &emitted, &mut report);
        if !sources.iter().any(|text| text.contains(&emitted)) {
            report.blocker(
                &subject,
                "the generated hitbox script is missing from the exported project",
            );
        }
    }

    for (fighter, move_name, calls) in effect_edits {
        let subject = format!("{fighter} / {move_name}");
        let emitted = crate::acmd::preview_effect_fn(calls, move_name, tweaks);
        // No source to hand over: a saved project stores the resolved call list and nothing
        // else, so by the time an export runs, the lines that were dropped are already gone
        // from the data. Naming them here needs the project to carry them, which is the same
        // plumbing that would let the export keep them — so it lands with that half, not this
        // one. The generated-source pane in the editor does have the script, and does check.
        verify_effect_move(&subject, calls, &emitted, tweaks, None, &mut report);
        if !sources.iter().any(|text| text.contains(&emitted)) {
            report.blocker(
                &subject,
                "the generated effect script is missing from the exported project",
            );
        }
    }

    report
}

/// Verify one move's hitbox script on its own, for the editor's generated-source preview.
pub fn verify_move(subject: &str, script: &AcmdScript, emitted: &str, report: &mut Report) {
    check_hitbox_fidelity(subject, script, emitted, report);
    check_script_values(subject, script, report);
    check_script_shape(subject, script, report);
}

/// Verify one move's effect script on its own, for the editor's generated-source preview.
///
/// `tweaks` are needed, not incidental: a live speed override deliberately replaces a spawn's
/// own `LAST_EFFECT_SET_RATE` value in the emitted code, so without them a tweaked effect
/// looks exactly like an export that lost the script's rate.
/// `source` is the script the calls were parsed from, when there is one. It is the only place
/// the lines an export deletes still exist — `calls` has already lost them and `emitted` never
/// had them — so without it that check cannot run. Pass `None` for a move captured live, which
/// has no source anywhere.
pub fn verify_effect_move(
    subject: &str,
    calls: &[EffectCall],
    emitted: &str,
    tweaks: &[LiveTweak],
    source: Option<&EffectScript>,
    report: &mut Report,
) {
    check_effect_fidelity(subject, calls, emitted, tweaks, report);
    check_effect_values(subject, calls, report);
    if let Some(source) = source {
        check_dropped_lines(subject, source, report);
    }
    for (spawn_func, effect_name) in crate::acmd::export_spawn_downgrades(calls) {
        report.warn(
            subject,
            format!(
                "{effect_name} exports as plain {} instead of {spawn_func} — its trailing \
                 arguments were never recorded. Reload this move from source, or perform it in \
                 game, to capture them.",
                if spawn_func.contains("FOLLOW") || spawn_func.contains("FLW") {
                    "EFFECT_FOLLOW"
                } else {
                    "EFFECT"
                }
            ),
        );
    }
}

// ── 1. It is Rust ─────────────────────────────────────────────────────────────

fn check_files(project: &ModProject, report: &mut Report) {
    let mut seen_paths: HashSet<&str> = HashSet::new();
    for file in &project.files {
        let path = file.rel_path.as_str();
        if !seen_paths.insert(path) {
            report.blocker(path, "two generated files claim the same path");
        }
        if file.contents.trim().is_empty() {
            report.blocker(path, "generated file is empty");
        }
        // A stray control character survives a `syn` parse inside a string literal and then
        // corrupts the TOML files, which are not parsed at all.
        if let Some(bad) = file
            .contents
            .chars()
            .find(|ch| ch.is_control() && *ch != '\n' && *ch != '\t')
        {
            report.blocker(
                path,
                format!("generated file contains the control character {bad:?}"),
            );
        }
        if path.ends_with(".rs") {
            if let Err(error) = syn::parse_file(&file.contents) {
                let span = error.span().start();
                report.blocker(
                    path,
                    format!(
                        "generated Rust does not parse at line {}, column {}: {error}",
                        span.line, span.column
                    ),
                );
            }
        }
        if path.ends_with(".toml") && !toml_strings_are_closed(&file.contents) {
            // The TOML files are the one generated kind nothing else parses. Every value in
            // them is either slugged or a name taken from the dump, so a stray quote means one
            // of those was not — and it would end the string it sits in and take the rest of
            // the file with it. The mod list is written as a `"""` block, hence the state.
            report.blocker(path, "a generated TOML value contains a stray quote");
        }
    }
    check_module_wiring(project, report);
}

/// Every `mod` declared is present and installed, and no two moves collapsed onto one name.
///
/// `script_function_name` strips every non-alphanumeric character, so `attack_air_n` and
/// `attack_airn` both become `game_attackairn`. Two such moves on one fighter would emit the
/// same function twice — valid Rust to parse, a duplicate-definition error to compile.
fn check_module_wiring(project: &ModProject, report: &mut Report) {
    let files: HashMap<&str, &str> = project
        .files
        .iter()
        .map(|file| (file.rel_path.as_str(), file.contents.as_str()))
        .collect();

    if let Some(lib) = files.get("src/lib.rs") {
        for module in declared_modules(lib) {
            let path = format!("src/{module}/mod.rs");
            if !files.contains_key(path.as_str()) {
                report.blocker("src/lib.rs", format!("`mod {module};` has no {path}"));
            }
            if !lib.contains(&format!("{module}::install();")) {
                report.blocker(
                    "src/lib.rs",
                    format!("module `{module}` is declared but never installed"),
                );
            }
        }
    }

    for (path, contents) in &files {
        if !path.ends_with("/acmd.rs") {
            continue;
        }
        let Ok(parsed) = syn::parse_file(contents) else {
            continue; // already reported as a parse failure
        };
        let mut seen: HashSet<String> = HashSet::new();
        for item in &parsed.items {
            let syn::Item::Fn(function) = item else {
                continue;
            };
            let name = function.sig.ident.to_string();
            if name == "install" {
                continue;
            }
            if !seen.insert(name.clone()) {
                report.blocker(
                    *path,
                    format!(
                        "two moves both generate `{name}` — their names differ only by \
                         punctuation. Rename one of them"
                    ),
                );
            }
            if !contents.contains(&format!(", {name}, smashline::Priority")) {
                report.blocker(
                    *path,
                    format!("`{name}` is generated but never registered, so it would not run"),
                );
            }
        }
    }
}

/// Whether every string in a generated TOML file is closed on the line that opens it.
///
/// A `"""` on its own line opens or closes a multi-line block; inside one, any quote at all is
/// a value that came through unescaped. Outside, a line with an odd number of quotes has one
/// dangling.
fn toml_strings_are_closed(contents: &str) -> bool {
    let mut in_block = false;
    for line in contents.lines() {
        let line = line.trim_end();
        if in_block {
            if line.trim() == "\"\"\"" {
                in_block = false;
            } else if line.contains('"') {
                return false;
            }
        } else if let Some(head) = line.strip_suffix("\"\"\"") {
            if head.contains('"') {
                return false;
            }
            in_block = true;
        } else if line.matches('"').count() % 2 != 0 {
            return false;
        }
    }
    !in_block
}

fn declared_modules(lib: &str) -> Vec<String> {
    lib.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
                .map(str::to_string)
        })
        .collect()
}

// ── 2. It says what the user said ─────────────────────────────────────────────

/// Report every field of `$a` that differs from `$b`, by name.
///
/// Listing the fields explicitly rather than comparing whole structs is the whole value here:
/// "damage: specified 12.34, exported 12.3" is actionable, "the hitboxes differ" is not.
macro_rules! diff_fields {
    ($out:ident, $a:expr, $b:expr, $($field:ident),+ $(,)?) => {
        $(
            if $a.$field != $b.$field {
                $out.push(format!(
                    "{} — specified {:?}, exported {:?}",
                    stringify!($field), $a.$field, $b.$field
                ));
            }
        )+
    };
}

fn check_hitbox_fidelity(subject: &str, script: &AcmdScript, emitted: &str, report: &mut Report) {
    let specified = script.to_hitboxes();
    let exported = crate::acmd::parse_acmd_script(emitted).to_hitboxes();

    if specified.len() != exported.len() {
        report.blocker(
            subject,
            format!(
                "the export has {} collision box(es) but {} were specified",
                exported.len(),
                specified.len()
            ),
        );
        return;
    }
    for (index, (want, got)) in specified.iter().zip(&exported).enumerate() {
        for difference in hitbox_differences(want, got) {
            report.blocker(
                subject,
                format!(
                    "collision {} (id {}, frame {}) exported a different {difference}",
                    index + 1,
                    want.id,
                    want.active_start
                ),
            );
        }
    }
}

fn hitbox_differences(want: &Hitbox, got: &Hitbox) -> Vec<String> {
    let mut out = Vec::new();
    diff_fields!(
        out,
        want,
        got,
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
        active_start,
        active_end,
        hitbox_type,
        category,
    );
    // `wind` and `catch` are not `PartialEq`-compared by the macro because a mismatch there is
    // better described by the payload than by a derived debug dump.
    match (&want.wind, &got.wind) {
        (Some(a), Some(b)) if a.command != b.command => out.push(format!(
            "wind command — specified {}, exported {}",
            a.command, b.command
        )),
        (Some(a), Some(b)) if a.args != b.args => out.push(format!(
            "wind arguments — specified {:?}, exported {:?}",
            a.args, b.args
        )),
        (Some(a), None) => out.push(format!("wind payload — {} was dropped", a.command)),
        (None, Some(b)) => out.push(format!("wind payload — {} was invented", b.command)),
        _ => {}
    }
    if want.catch != got.catch {
        out.push(format!(
            "grab status/situation — specified {:?}, exported {:?}",
            want.catch, got.catch
        ));
    }
    out
}

fn check_effect_fidelity(
    subject: &str,
    calls: &[EffectCall],
    emitted: &str,
    tweaks: &[LiveTweak],
    report: &mut Report,
) {
    // The emitter groups spawns by frame, so the read-back order is the timeline order rather
    // than the editor's list order. Sort both the same way before pairing them up.
    let mut specified: Vec<&EffectCall> = calls.iter().filter(|call| !call.disabled).collect();
    let exported = crate::acmd::parse_effect_script(emitted).to_effect_calls();
    let mut exported: Vec<&EffectCall> = exported.iter().collect();
    specified.sort_by_key(|call| effect_order(call));
    exported.sort_by_key(|call| effect_order(call));

    if specified.len() != exported.len() {
        report.blocker(
            subject,
            format!(
                "the export has {} effect spawn(s) but {} were specified",
                exported.len(),
                specified.len()
            ),
        );
        return;
    }
    let downgraded: HashSet<(String, String)> = crate::acmd::export_spawn_downgrades(calls)
        .into_iter()
        .collect();
    for (want, got) in specified.iter().zip(&exported) {
        // A spawn whose macro tail was never recorded is already reported by name as a
        // downgrade; re-listing each of its shifted arguments would bury that.
        if downgraded.contains(&(want.spawn_func.clone(), want.effect_name.clone())) {
            continue;
        }
        let mut out = Vec::new();
        diff_fields!(
            out,
            want,
            got,
            effect_name,
            effect_name_alt,
            spawn_func,
            bone_name,
            offset,
            rotation,
            scale,
            follows_bone,
            active_start,
            active_end,
            extra_args,
            // Compared here rather than trusted, because the corpus oracle pairs calls on
            // `(spawn_func, effect_name)` alone and a colour command has no effect name at
            // all — every one of them would pair up and compare equal without this line.
            color,
        );
        for difference in out {
            report.blocker(
                subject,
                format!(
                    "spawn {} on frame {} exported a different {difference}",
                    want.effect_name, want.active_start
                ),
            );
        }

        // Rate is checked apart from the fields above because one difference is legitimate:
        // a live speed override is meant to replace the spawn's authored rate. That still
        // gets said out loud — the user set a multiplier on an effect kind and it quietly
        // took over a value the script had chosen, which is worth knowing before shipping.
        if got.rate != want.rate {
            let override_rate = tweaks
                .iter()
                .find(|tweak| tweak.effect_name.eq_ignore_ascii_case(&want.effect_name))
                .and_then(|tweak| tweak.speed);
            if override_rate == got.rate {
                report.warn(
                    subject,
                    format!(
                        "spawn {} on frame {} ships the live speed override ({}) instead of \
                         the rate its script sets ({})",
                        want.effect_name,
                        want.active_start,
                        got.rate.unwrap_or(1.0),
                        want.rate.map(|r| r.to_string()).unwrap_or("none".into()),
                    ),
                );
            } else {
                report.blocker(
                    subject,
                    format!(
                        "spawn {} on frame {} exported a different rate",
                        want.effect_name, want.active_start
                    ),
                );
            }
        }
    }
}

/// Name every line of the user's effect script that the export throws away.
///
/// The effect export regenerates the function from the calls it recognises, so a line it never
/// parsed into one does not survive. That has always been true and was always silent: C3 found
/// 69 `FLASH` / `BURN_COLOR` calls being deleted out of exported moves, and only found them by
/// reading a diff. Modelling a family removes it from this list; until then the loss is at
/// least said out loud.
///
/// A warning rather than a blocker, deliberately. Most vanilla effect scripts carry at least
/// one line this parser has no variant for, so refusing them would turn a lossy export into no
/// export at all — worse for every user who does not care about the dropped line, and no better
/// for the ones who do, since the message is the same either way.
fn check_dropped_lines(subject: &str, source: &EffectScript, report: &mut Report) {
    // Grouped by text and kept in first-seen order: a line inside a `for` body is genuinely
    // lost once per iteration, but saying so five times reads as five different problems.
    let mut seen: Vec<(String, usize)> = Vec::new();
    for line in crate::acmd::unexportable_effect_lines(source) {
        match seen.iter_mut().find(|(text, _)| *text == line) {
            Some((_, count)) => *count += 1,
            None => seen.push((line, 1)),
        }
    }
    for (line, count) in seen {
        let times = if count > 1 {
            format!(" ({count} times)")
        } else {
            String::new()
        };
        report.warn(
            subject,
            format!(
                "the generated script does not include this line{times}, and nothing in it \
                 does the same job: {line}"
            ),
        );
    }
}

fn effect_order(call: &EffectCall) -> (u32, String, String) {
    (
        call.active_start,
        call.effect_name.clone(),
        call.spawn_func.clone(),
    )
}

// ── 3. It will compile and run ────────────────────────────────────────────────

/// A name that is about to be written inside `Hash40::new("…")`.
///
/// An empty one hashes to a graphic that does not exist; a quote or a backslash ends the
/// string literal early and takes the rest of the call with it.
fn check_hash_name(subject: &str, what: &str, name: &str, report: &mut Report) {
    if name.trim().is_empty() {
        report.blocker(subject, format!("{what} has no name"));
    } else if name.contains('"') || name.contains('\\') {
        report.blocker(
            subject,
            format!("{what} name {name:?} contains a quote or backslash"),
        );
    }
}

fn check_finite(subject: &str, what: &str, value: f32, report: &mut Report) {
    if !value.is_finite() {
        report.blocker(subject, format!("{what} is {value}, which is not a number"));
    }
}

fn check_script_values(subject: &str, script: &AcmdScript, report: &mut Report) {
    for stmt in flatten(&script.stmts) {
        match stmt {
            AcmdStmt::Frame(f) | AcmdStmt::Wait(f) => {
                check_finite(subject, "a frame timing", *f, report)
            }
            AcmdStmt::Excute(inner) => {
                for excute in inner {
                    check_excute_values(subject, excute, report);
                }
            }
            _ => {}
        }
    }
    for (index, hitbox) in script.to_hitboxes().iter().enumerate() {
        let label = format!("collision {} (id {})", index + 1, hitbox.id);
        check_hash_name(
            subject,
            &format!("{label} joint"),
            &hitbox.bone_name,
            report,
        );
        for (what, value) in [
            ("damage", hitbox.damage),
            ("size", hitbox.size),
            ("x offset", hitbox.offset_x),
            ("y offset", hitbox.offset_y),
            ("z offset", hitbox.offset_z),
            ("hitlag multiplier", hitbox.hitlag_mult),
            ("SDI multiplier", hitbox.sdi_mult),
            ("hitbox attribute", hitbox.hitbox_attr),
        ] {
            check_finite(subject, &format!("{label} {what}"), value, report);
        }
        if let Some(end) = hitbox.capsule_end {
            for (axis, value) in ["x", "y", "z"].iter().zip(end) {
                check_finite(
                    subject,
                    &format!("{label} capsule {axis} endpoint"),
                    value,
                    report,
                );
            }
        }
    }
}

fn check_excute_values(subject: &str, stmt: &ExcuteStmt, report: &mut Report) {
    match stmt {
        ExcuteStmt::Wind(wind) => {
            if !wind.is_valid() {
                report.blocker(
                    subject,
                    format!(
                        "wind command {} was given {} arguments but takes {}",
                        wind.command,
                        wind.args.len(),
                        wind.expected_arity()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "an unknown number of".to_string()),
                    ),
                );
            }
            // `sv_animcmd` has all four wind commands and the plugin hooks all four, so one can
            // reach the editor from a live capture or an older project — but smash-script never
            // wrapped the plain rectangular `AREA_WIND_2ND`, and the export writes `macros::`
            // calls. Emitting it names a function that does not exist.
            if !wind.has_macro_wrapper() && wind.expected_arity().is_some() {
                report.blocker(
                    subject,
                    format!(
                        "`macros::{}` does not exist — smash-script has no wrapper for that \
                         command. Use the rectangular form with a lifetime, \
                         `AREA_WIND_2ND_arg10`, instead.",
                        wind.command
                    ),
                );
            }
            for (index, value) in wind.args.iter().enumerate() {
                check_finite(
                    subject,
                    &format!("wind {} argument {}", wind.command, index + 1),
                    *value,
                    report,
                );
            }
        }
        ExcuteStmt::Catch(call) => {
            check_hash_name(subject, "grab box joint", &call.bone_name, report);
        }
        _ => {}
    }
}

fn check_effect_values(subject: &str, calls: &[EffectCall], report: &mut Report) {
    for call in calls.iter().filter(|call| !call.disabled) {
        // A colour command has no graphic to name it, so it is labelled by its command.
        let label = match &call.color {
            Some(_) => format!("{} on frame {}", call.spawn_func, call.active_start),
            None => format!("spawn {} on frame {}", call.effect_name, call.active_start),
        };
        if let Some(color) = &call.color {
            check_color_values(subject, &label, &call.spawn_func, color, report);
            continue;
        }
        // A trail rides along as its own source line, so its names are never re-quoted and its
        // transform is not emitted at all.
        if call.raw_line.is_none() {
            check_hash_name(
                subject,
                &format!("{label} graphic"),
                &call.effect_name,
                report,
            );
            check_hash_name(subject, &format!("{label} joint"), &call.bone_name, report);
            if let Some(alt) = &call.effect_name_alt {
                check_hash_name(subject, &format!("{label} second graphic"), alt, report);
            }
            for (axis, value) in ["x", "y", "z"].iter().zip(call.offset) {
                check_finite(subject, &format!("{label} {axis} offset"), value, report);
            }
            for (axis, value) in ["x", "y", "z"].iter().zip(call.rotation) {
                check_finite(subject, &format!("{label} {axis} rotation"), value, report);
            }
            check_finite(subject, &format!("{label} scale"), call.scale, report);
        }
        // Checked for every spawn, trail included: the rate is its own line rather than an
        // argument of the spawn, so a trail with a rate still emits one. It is written with
        // plain `to_string`, which spells a non-finite value `NaN` or `inf` — neither of
        // which is Rust, so nothing downstream would build.
        if let Some(rate) = call.rate {
            check_finite(subject, &format!("{label} rate"), rate, report);
            if rate < 0.0 {
                report.warn(
                    subject,
                    format!("{label} has a negative rate ({rate}), which will not play backwards"),
                );
            }
        }
        if call.active_end < call.active_start {
            report.warn(
                subject,
                format!(
                    "{label} ends on frame {} before it starts, so it never appears",
                    call.active_end
                ),
            );
        }
    }
}

/// Everything that can go wrong with a `FLASH` / `BURN_COLOR` line before it reaches a
/// compiler.
///
/// The arity check is the one that matters: these commands are emitted from a table keyed by
/// the command name, so an editor state holding a transition for a command that has no such
/// slot — or no colour for one that needs four — writes a call the signature does not accept.
/// That is a mod that does not build, which is a blocker by the same rule as a wind command an
/// argument short.
fn check_color_values(
    subject: &str,
    label: &str,
    command: &str,
    color: &crate::data::ColorCall,
    report: &mut Report,
) {
    let Some((has_transition, has_rgba)) = crate::data::color_command_layout(command) else {
        report.blocker(
            subject,
            format!("{label} is not a colour command smash-script wraps, so it cannot be written"),
        );
        return;
    };
    if has_transition != color.transition.is_some() {
        report.blocker(
            subject,
            format!(
                "{label} takes {} transition length",
                if has_transition { "a" } else { "no" }
            ),
        );
    }
    if has_rgba != color.rgba.is_some() {
        report.blocker(
            subject,
            format!("{label} takes {} colour", if has_rgba { "a" } else { "no" }),
        );
    }
    // Every slot is written with plain `to_string`, which spells a non-finite value `NaN` or
    // `inf`. Neither is Rust, so nothing downstream would build.
    if let Some(frames) = color.transition {
        check_finite(subject, &format!("{label} transition"), frames, report);
        if frames < 0.0 {
            report.warn(
                subject,
                format!("{label} interpolates over {frames} frames, which never completes"),
            );
        }
    }
    for (channel, value) in ["red", "green", "blue", "blend"]
        .iter()
        .zip(color.rgba.unwrap_or([0.0; 4]))
    {
        check_finite(subject, &format!("{label} {channel}"), value, report);
    }
}

// ── 4. It is not wasteful ─────────────────────────────────────────────────────

/// Report statements the game will never act on.
///
/// Every check below reasons about *when* something runs, which means it is only sound for a
/// script the editor fully models. A real script routinely contains lines the parser keeps
/// verbatim — `if(WorkModule::is_flag(…)){`, `FT_MOTION_RATE(…)`, `wait_loop_sync_mot()` —
/// and each of those breaks the assumption that statements run once, in order, at the frame
/// they name. Calibrating against the vanilla corpus, this pass fired on 5% of scripts before
/// the guard and every one of those was a branch the model could not see, not a real problem.
/// So it runs only where it can be right: a move with no unmodelled lines at all, which is
/// exactly the live-captured and editor-built case where the user laid out the timeline.
fn check_script_shape(subject: &str, script: &AcmdScript, report: &mut Report) {
    if has_unmodelled_flow(&script.stmts) {
        return;
    }
    check_shape(subject, &script.stmts, &mut 0.0, report);
    for (index, hitbox) in script.to_hitboxes().iter().enumerate() {
        if hitbox.active_end < hitbox.active_start {
            report.warn(
                subject,
                format!(
                    "collision {} (id {}) is cleared on frame {} before it starts on frame {}, \
                     so it never comes out",
                    index + 1,
                    hitbox.id,
                    hitbox.active_end,
                    hitbox.active_start
                ),
            );
        }
    }
}

fn has_unmodelled_flow(stmts: &[AcmdStmt]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        AcmdStmt::Raw(_) => true,
        AcmdStmt::Loop { body, .. } => has_unmodelled_flow(body),
        _ => false,
    })
}

/// `clock` tracks the frame the game would actually be on, which is not the same as the frame
/// the script names: `frame()` waits *until* a frame and returns immediately if that frame has
/// already passed, so a timeline that goes backwards silently collapses.
fn check_shape(subject: &str, stmts: &[AcmdStmt], clock: &mut f32, report: &mut Report) {
    for stmt in stmts {
        match stmt {
            AcmdStmt::Frame(target) => {
                if *target < *clock {
                    report.warn(
                        subject,
                        format!(
                            "frame {target} comes after frame {clock}, and the game does not \
                             rewind — everything below it runs on frame {clock} instead"
                        ),
                    );
                } else {
                    *clock = *target;
                }
            }
            AcmdStmt::Wait(amount) => {
                if *amount <= 0.0 {
                    report.warn(subject, format!("`wait({amount})` does not wait"));
                }
                *clock += amount.max(0.0);
            }
            AcmdStmt::Excute(inner) => {
                if inner.is_empty() {
                    report.warn(
                        subject,
                        format!("the block on frame {clock} is empty and can be removed"),
                    );
                }
                // Only the statements the editor understands. Two identical collisions in one
                // block are one collision; two identical raw calls may well be two articles.
                let modelled: Vec<&ExcuteStmt> = inner
                    .iter()
                    .filter(|stmt| !matches!(stmt, ExcuteStmt::Raw(_)))
                    .collect();
                let mut seen: BTreeMap<String, usize> = BTreeMap::new();
                for stmt in modelled {
                    for line in crate::acmd::preview_excute_stmts(std::slice::from_ref(stmt)) {
                        *seen.entry(line).or_default() += 1;
                    }
                }
                for (line, count) in seen.into_iter().filter(|(_, count)| *count > 1) {
                    report.warn(
                        subject,
                        format!(
                            "frame {clock} issues the same call {count} times: {}",
                            line.trim()
                        ),
                    );
                }
            }
            AcmdStmt::Loop { count, body } => {
                if *count <= 1 {
                    report.warn(
                        subject,
                        format!("a loop that runs {count} time(s) does not need to be a loop"),
                    );
                }
                // Only the first iteration is walked: the frames inside repeat, and reporting
                // the same statement once per iteration would say nothing new.
                check_shape(subject, body, clock, report);
            }
            AcmdStmt::WaitLoopClear | AcmdStmt::Raw(_) => {}
        }
    }
}

/// Every statement in the tree, loop bodies included, in source order.
fn flatten(stmts: &[AcmdStmt]) -> Vec<&AcmdStmt> {
    let mut out = Vec::new();
    for stmt in stmts {
        out.push(stmt);
        if let AcmdStmt::Loop { body, .. } = stmt {
            out.extend(flatten(body));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acmd::{build_mod_project, parse_acmd_script, preview_game_fn, GeneratedFile};
    use crate::data::WindboxData;

    const ATTACK: &str = r#"macros::ATTACK(agent, 0, 0, Hash40::new("toer"), 6.0, 361, 43, 0, 30, 3.7, 4.3, 0.0, 0.0, None, None, None, 1.0, 1.0, *ATTACK_SETOFF_KIND_ON, *ATTACK_LR_CHECK_POS, false, 0, 0.35, 0, false, false, false, false, true, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_S, *COLLISION_SOUND_ATTR_KICK, *ATTACK_REGION_KICK);"#;

    fn script(body: &str) -> AcmdScript {
        parse_acmd_script(&format!(
            "unsafe extern \"C\" fn game_test(agent: &mut L2CAgentBase) {{\n{body}\n}}\n"
        ))
    }

    fn verify(script: &AcmdScript) -> Report {
        let mut report = Report::default();
        let emitted = preview_game_fn(script, "test");
        verify_move("test", script, &emitted, &mut report);
        report
    }

    fn messages(report: &Report) -> String {
        report
            .findings
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The property the whole module exists for, checked against every script the app has ever
    /// fetched. A clean run here means the emitter is a faithful inverse of the parser across
    /// a thousand real functions, not just across the cases someone thought to write down.
    #[test]
    fn every_cached_script_survives_its_own_export() {
        let cache = crate::scratch_dirs::app_storage_root().join("script-cache");
        if !cache.is_dir() {
            return;
        }
        let mut report = Report::default();
        let mut checked = 0;
        for fighter in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
            for file in std::fs::read_dir(fighter.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let Ok(whole) = std::fs::read_to_string(file.path()) else {
                    continue;
                };
                // One cached file holds every ACMD function for a motion; an export writes one.
                for body in whole.split_inclusive("\n}\n") {
                    let label = format!(
                        "{}/{}",
                        fighter.file_name().to_string_lossy(),
                        file.file_name().to_string_lossy()
                    );
                    let parsed = crate::acmd::parse_acmd_script(body);
                    if !parsed.stmts.is_empty() {
                        let emitted = preview_game_fn(&parsed, "audit");
                        verify_move(&label, &parsed, &emitted, &mut report);
                        checked += 1;
                    }
                    let effect_script = crate::acmd::parse_effect_script(body);
                    let calls = effect_script.to_effect_calls();
                    if !calls.is_empty() {
                        let emitted = crate::acmd::preview_effect_fn(&calls, "audit", &[]);
                        verify_effect_move(
                            &label,
                            &calls,
                            &emitted,
                            &[],
                            Some(&effect_script),
                            &mut report,
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 100, "the cache held almost nothing: {checked}");
        let blockers: Vec<String> = report.blockers().map(|f| f.to_string()).collect();
        assert!(
            blockers.is_empty(),
            "{} of {checked} scripts do not export faithfully:\n{}",
            blockers.len(),
            blockers.join("\n")
        );
    }

    /// The bug this module was written to find, kept as a value rather than as a format string:
    /// a vanilla hitbox attribute of `0.35` used to export as `0.3`, and a grab box at `-17.25`
    /// as `-17.2`. Nothing said so.
    #[test]
    fn a_value_finer_than_one_decimal_survives_the_export() {
        let grab = r#"macros::CATCH(agent, 0, Hash40::new("top"), 5.5, 12.25, -17.25, 0.0, None, None, None, *FIGHTER_STATUS_KIND_CAPTURE_PULLED, *COLLISION_SITUATION_MASK_GA);"#;
        let parsed = script(&format!(
            "    frame(agent.lua_state_agent, 5.0);\n    if macros::is_excute(agent) {{\n        {ATTACK}\n        {grab}\n    }}"
        ));
        let boxes = parsed.to_hitboxes();
        assert_eq!(boxes[0].hitbox_attr, 0.35);
        assert_eq!(boxes[1].offset_y, -17.25);

        let report = verify(&parsed);
        assert!(report.is_clean(), "{}", messages(&report));
    }

    /// The verifier has to *catch* a rounded value, not merely coexist with one — otherwise the
    /// test above would keep passing after the check was accidentally disabled.
    #[test]
    fn a_rounded_value_is_reported_as_a_blocker() {
        let parsed = script(&format!(
            "    frame(agent.lua_state_agent, 5.0);\n    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}"
        ));
        let rounded = preview_game_fn(&parsed, "test").replace("0.35", "0.3");
        let mut report = Report::default();
        check_hitbox_fidelity("test", &parsed, &rounded, &mut report);
        let text = messages(&report);
        assert!(report.has_blockers(), "a rounded value went unreported");
        assert!(
            text.contains("hitbox_attr") && text.contains("0.35") && text.contains("0.3"),
            "the report must name the field and both values:\n{text}"
        );
    }

    #[test]
    fn generated_rust_that_does_not_parse_is_refused() {
        let project = ModProject {
            name: "broken".into(),
            files: vec![GeneratedFile {
                rel_path: "src/lib.rs".into(),
                contents: "pub fn main() { let x = ;\n".into(),
            }],
        };
        let report = verify_export(&project, &[], &[], &[]);
        assert!(report.has_blockers(), "{}", messages(&report));
        assert!(
            messages(&report).contains("does not parse at line"),
            "the report must say where:\n{}",
            messages(&report)
        );
    }

    /// Two move names that differ only by punctuation collapse onto one function name, which
    /// parses fine and then fails to compile. Catching it needs a name check, not a syntax one.
    #[test]
    fn two_moves_that_generate_the_same_function_are_refused() {
        let body = format!("    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}");
        let edits = vec![
            ("mario".into(), "attack_air_n".into(), script(&body)),
            ("mario".into(), "attackairn".into(), script(&body)),
        ];
        let project = build_mod_project(&edits, "collide_plugin");
        let report = verify_export(&project, &edits, &[], &[]);
        assert!(
            messages(&report).contains("both generate `game_attackairn`"),
            "{}",
            messages(&report)
        );
    }

    /// A project that emits a move and then does not ship it is the failure the containment
    /// check exists for. Verifying the emitted text alone would call this export clean.
    #[test]
    fn a_move_the_project_does_not_actually_contain_is_refused() {
        let body = format!("    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}");
        let edits = vec![("mario".into(), "attack_air_n".into(), script(&body))];
        let mut project = build_mod_project(&edits, "dropped_plugin");
        project
            .files
            .retain(|file| !file.rel_path.ends_with("/acmd.rs"));
        let report = verify_export(&project, &edits, &[], &[]);
        assert!(
            messages(&report).contains("missing from the exported project"),
            "{}",
            messages(&report)
        );
    }

    #[test]
    fn a_value_that_is_not_a_number_is_refused() {
        let parsed = script(&format!(
            "    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}"
        ));
        let mut hitboxes = parsed.to_hitboxes();
        hitboxes[0].size = f32::NAN;
        let rebuilt = crate::app::synthesize_script_from_hitboxes(&hitboxes);
        let report = verify(&rebuilt);
        assert!(
            messages(&report).contains("not a number"),
            "{}",
            messages(&report)
        );
        // …and it is still Rust, so the parse failure does not mask the real cause.
        assert!(preview_game_fn(&rebuilt, "test").contains("f32::NAN"));
        assert!(syn::parse_file(&preview_game_fn(&rebuilt, "test")).is_ok());
    }

    #[test]
    fn a_graphic_name_with_a_quote_in_it_is_refused() {
        let mut hitboxes = script(&format!(
            "    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}"
        ))
        .to_hitboxes();
        hitboxes[0].bone_name = "to\"er".into();
        let report = verify(&crate::app::synthesize_script_from_hitboxes(&hitboxes));
        assert!(
            messages(&report).contains("quote or backslash"),
            "{}",
            messages(&report)
        );
    }

    /// A wind command's arity is part of its name. One argument short is a call that does not
    /// compile, and there is no signature to pad it out from.
    #[test]
    fn a_wind_command_with_the_wrong_number_of_arguments_is_refused() {
        let mut parsed = script("    if macros::is_excute(agent) {\n        macros::AREA_WIND_2ND_RAD(agent, 4, 0.5, 0.02, 1000, 1, -2, 6, 18);\n    }");
        if let AcmdStmt::Excute(inner) = &mut parsed.stmts[0] {
            if let ExcuteStmt::Wind(WindboxData { args, .. }) = &mut inner[0] {
                args.pop();
            }
        }
        let report = verify(&parsed);
        assert!(
            messages(&report).contains("takes 8"),
            "{}",
            messages(&report)
        );
    }

    /// `sv_animcmd` has `AREA_WIND_2ND` and the plugin hooks it, so one can reach the editor
    /// from a live capture or a project saved before this check existed — but smash-script
    /// never wrapped it, and the export writes `macros::` calls. A well-formed file naming a
    /// function that does not exist is exactly what this check is for.
    #[test]
    fn a_wind_command_smash_script_never_wrapped_cannot_be_exported() {
        let parsed = script(
            "    if macros::is_excute(agent) {\n        macros::AREA_WIND_2ND(agent, 0, 1, 80, \
             300, 0.8, 4, 12, 24, 16);\n    }",
        );
        let report = verify(&parsed);
        assert!(report.has_blockers(), "{}", messages(&report));
        assert!(
            messages(&report).contains("does not exist"),
            "{}",
            messages(&report)
        );

        // The three commands that do have wrappers must stay exportable.
        let fine = script(
            "    if macros::is_excute(agent) {\n        macros::AREA_WIND_2ND_arg10(agent, 0, 1, \
             80, 300, 0.8, 4, 12, 24, 16, 50);\n    }",
        );
        assert!(
            !verify(&fine).has_blockers(),
            "{}",
            messages(&verify(&fine))
        );
    }

    /// The whole point of C5: an effect script's unmodelled lines do not survive an export, and
    /// before this nothing said which ones. `LAST_EFFECT_SET_COLOR` is the case worth pinning —
    /// 33 occurrences in the cache, the single most-deleted line in the corpus, and one the
    /// *emitter* writes for live colour tweaks while the parser has no variant for it, so a
    /// script's own colour modifier is dropped on the way through.
    #[test]
    fn a_line_the_export_cannot_reproduce_is_named_rather_than_silently_deleted() {
        let src = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
        macros::LAST_EFFECT_SET_COLOR(agent, 1, 0.5, 0);
    }
}
"#;
        let source = crate::acmd::parse_effect_script(src);
        let calls = source.to_effect_calls();
        let emitted = crate::acmd::preview_effect_fn(&calls, "test", &[]);
        assert!(
            !emitted.contains("LAST_EFFECT_SET_COLOR"),
            "the premise no longer holds — the export now keeps this line, so this test is \
             checking nothing:\n{emitted}"
        );

        let mut report = Report::default();
        verify_effect_move("test", &calls, &emitted, &[], Some(&source), &mut report);
        assert!(
            messages(&report).contains("macros::LAST_EFFECT_SET_COLOR(agent, 1, 0.5, 0);"),
            "the dropped line was not named:\n{}",
            messages(&report)
        );
        // A warning, not a blocker. Roughly a quarter of the cached vanilla effect scripts lose
        // a line this way, so refusing them would leave a user who does not care about the
        // dropped line with no export at all.
        assert!(!report.has_blockers(), "{}", messages(&report));

        // Without the source there is nothing to compare against, and the check must stay quiet
        // rather than guess: a move captured live has no script and has lost nothing.
        let mut blind = Report::default();
        verify_effect_move("test", &calls, &emitted, &[], None, &mut blind);
        assert!(
            !messages(&blind).contains("does not include this line"),
            "{}",
            messages(&blind)
        );
    }

    /// The emitter writes its own braces and `is_excute` headers, so reporting the ones it threw
    /// away would be one finding per block of noise burying the lines that are a real loss.
    #[test]
    fn punctuation_the_emitter_regenerates_is_not_reported_as_a_loss() {
        let src = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
    }
}
"#;
        let source = crate::acmd::parse_effect_script(src);
        let calls = source.to_effect_calls();
        let emitted = crate::acmd::preview_effect_fn(&calls, "test", &[]);
        let mut report = Report::default();
        verify_effect_move("test", &calls, &emitted, &[], Some(&source), &mut report);
        assert!(
            report.is_clean(),
            "a script the export reproduces exactly still reported something:\n{}",
            messages(&report)
        );
    }

    /// A live speed override deliberately replaces the spawn's own rate in the export. That is
    /// the one rate difference that is not a bug — but it is still a value the user set on an
    /// effect *kind* quietly taking over one the script chose for a single spawn, so it is
    /// said out loud rather than suppressed. Any other difference means the export lost a rate.
    #[test]
    fn a_speed_override_replacing_a_scripts_rate_is_said_out_loud_but_not_refused() {
        let src = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
        macros::LAST_EFFECT_SET_RATE(agent, 2);
    }
}
"#;
        let calls = crate::acmd::parse_effect_script(src).to_effect_calls();
        assert_eq!(calls[0].rate, Some(2.0));
        let tweaks = vec![LiveTweak {
            effect_name: "sys_atk_smoke".into(),
            color: None,
            speed: Some(0.5),
        }];

        let emitted = crate::acmd::preview_effect_fn(&calls, "test", &tweaks);
        let mut report = Report::default();
        verify_effect_move("test", &calls, &emitted, &tweaks, None, &mut report);
        assert!(!report.has_blockers(), "{}", messages(&report));
        assert!(
            messages(&report).contains("ships the live speed override"),
            "{}",
            messages(&report)
        );

        // Without the tweak to explain it, the same divergence is an export that lost a value.
        let mut report = Report::default();
        verify_effect_move("test", &calls, &emitted, &[], None, &mut report);
        assert!(report.has_blockers(), "{}", messages(&report));
        assert!(
            messages(&report).contains("different rate"),
            "{}",
            messages(&report)
        );
    }

    #[test]
    fn wasteful_code_is_a_warning_and_not_a_refusal() {
        let parsed = script(&format!(
            "    frame(agent.lua_state_agent, 5.0);\n    if macros::is_excute(agent) {{\n        {ATTACK}\n        {ATTACK}\n    }}\n    wait(agent.lua_state_agent, 0.0);\n    if macros::is_excute(agent) {{\n    }}"
        ));
        let report = verify(&parsed);
        assert!(!report.has_blockers(), "{}", messages(&report));
        let text = messages(&report);
        for expected in ["the same call 2 times", "does not wait", "is empty"] {
            assert!(text.contains(expected), "missing {expected}:\n{text}");
        }
    }

    /// The timing checks reason about statements running once, in order. A script with its own
    /// `if(…)` branches breaks that, and guessing anyway produced a wrong warning on 5% of the
    /// vanilla corpus — every one of them a branch the editor cannot see.
    #[test]
    fn a_script_with_branches_of_its_own_is_not_second_guessed() {
        let parsed = script(&format!(
            "    frame(agent.lua_state_agent, 12.0);\n    if(WorkModule::is_flag(agent.module_accessor, 1)){{\n    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}\n    }}\n    else {{\n    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}\n    }}\n    frame(agent.lua_state_agent, 3.0);"
        ));
        assert!(
            has_unmodelled_flow(&parsed.stmts),
            "the branch must be visible as an unmodelled line"
        );
        let report = verify(&parsed);
        assert!(
            report.findings.is_empty(),
            "no timing claim can be made about a branch:\n{}",
            messages(&report)
        );
    }

    /// Same inputs, same bytes. An export that shuffles between runs is one nobody can diff.
    #[test]
    fn building_the_same_edits_twice_gives_the_same_bytes() {
        let body = format!("    if macros::is_excute(agent) {{\n        {ATTACK}\n    }}");
        let edits = vec![
            ("mario".into(), "attack_air_n".into(), script(&body)),
            ("kirby".into(), "attack_lw_3".into(), script(&body)),
            ("donkey".into(), "attack_hi_4".into(), script(&body)),
        ];
        let first = build_mod_project(&edits, "stable_plugin");
        let second = build_mod_project(&edits, "stable_plugin");
        let paths: Vec<&String> = first.files.iter().map(|f| &f.rel_path).collect();
        assert_eq!(
            paths,
            second.files.iter().map(|f| &f.rel_path).collect::<Vec<_>>()
        );
        for (a, b) in first.files.iter().zip(&second.files) {
            assert_eq!(
                a.contents, b.contents,
                "{} differs between runs",
                a.rel_path
            );
        }
    }

    #[test]
    fn a_multi_line_toml_block_is_not_mistaken_for_a_stray_quote() {
        assert!(toml_strings_are_closed(
            "display_name = \"a\"\ndescription = \"\"\"\n- mario: attack_air_n\n\"\"\"\n"
        ));
        assert!(!toml_strings_are_closed(
            "display_name = \"a\"\ndescription = \"\"\"\n- mario: att\"ack\n\"\"\"\n"
        ));
        assert!(!toml_strings_are_closed("display_name = \"a\n"));
    }
}

