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
//!    parser and the resulting hitboxes, hurtbox states, and effect calls are compared field by
//!    field with the ones the user edited. A single rounded decimal is a finding.
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
use crate::data::{AcmdScript, AcmdStmt, EffectCall, ExcuteStmt, Hitbox};
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

    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }

    /// One line per warning, for an export that succeeded but did not carry everything.
    ///
    /// Capped on the same reasoning as [`Self::blocker_summary`], and more sharply: a single
    /// unmodelled macro in a `for` body produces one finding per move that uses it, and an
    /// export summary is read at a glance rather than studied.
    pub fn warning_summary(&self) -> Vec<String> {
        const SHOWN: usize = 5;
        let all: Vec<String> = self.warnings().map(|f| f.to_string()).collect();
        let mut out: Vec<String> = all.iter().take(SHOWN).cloned().collect();
        if all.len() > SHOWN {
            out.push(format!("… and {} more", all.len() - SHOWN));
        }
        out
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
///
/// `dropped` is keyed `"fighter/move"` and holds what the effect export threw away when that
/// move was last read from a script. It is separate from `effect_edits` because it is the one
/// input here that no amount of looking at the generated project can recover: the export builds
/// the function out of the calls it understood, so a line that became no call left no trace in
/// either the edits or the output. A move missing from the map had no source in hand, which is
/// not the same as a move that had one and lost nothing — but both produce no findings, so the
/// distinction costs nothing to collapse and empty lists are not stored.
#[allow(dead_code)]
pub fn verify_export(
    project: &ModProject,
    acmd_edits: &[(String, String, AcmdScript)],
    effect_edits: &[crate::acmd::EffectExport],
    sound_edits: &[(String, String, AcmdScript)],
    tweaks: &[LiveTweak],
    dropped: &HashMap<String, Vec<String>>,
) -> Report {
    verify_export_with_expression(
        project,
        acmd_edits,
        effect_edits,
        sound_edits,
        &[],
        tweaks,
        dropped,
    )
}

/// [`verify_export`] with the measured `expression_` script family included.
pub fn verify_export_with_expression(
    project: &ModProject,
    acmd_edits: &[(String, String, AcmdScript)],
    effect_edits: &[crate::acmd::EffectExport],
    sound_edits: &[(String, String, AcmdScript)],
    expression_edits: &[(String, String, AcmdScript)],
    tweaks: &[LiveTweak],
    dropped: &HashMap<String, Vec<String>>,
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

    for (fighter, move_name, calls, residue) in effect_edits {
        let subject = format!("{fighter} / {move_name}");
        // Not `subject`: that is prose for the report and has spaces around the slash. The
        // dropped map is keyed the way the editor keys every other per-move table.
        let key = format!("{fighter}/{move_name}");
        let emitted = crate::acmd::preview_effect_fn(calls, move_name, tweaks, residue);
        // C5 left this call passing `None`, because a saved project remembered the resolved
        // calls and nothing about the lines that became none of them. C6c gave the project
        // somewhere to keep that list, so both halves of the loss report now reach an export
        // run from a reloaded mod, not just the generated-source pane.
        verify_effect_move(
            &subject,
            calls,
            &emitted,
            tweaks,
            dropped.get(&key).map(Vec::as_slice),
            residue,
            &mut report,
        );
        if !sources.iter().any(|text| text.contains(&emitted)) {
            report.blocker(
                &subject,
                "the generated effect script is missing from the exported project",
            );
        }
    }

    for (fighter, move_name, script) in sound_edits {
        let subject = format!("{fighter} / {move_name}");
        let emitted = crate::acmd::preview_sound_fn(script, move_name);
        verify_sound_move(&subject, script, &emitted, &mut report);
        if !sources.iter().any(|text| text.contains(&emitted)) {
            report.blocker(
                &subject,
                "the generated sound script is missing from the exported project",
            );
        }
    }

    for (fighter, move_name, script) in expression_edits {
        let subject = format!("{fighter} / {move_name}");
        let emitted = crate::acmd::preview_expression_fn(script, move_name);
        verify_expression_move(&subject, script, &emitted, &mut report);
        if !sources.iter().any(|text| text.contains(&emitted)) {
            report.blocker(
                &subject,
                "the generated expression script is missing from the exported project",
            );
        }
    }

    report
}

/// Verify the typed expression calls that the editor models. Unknown expression lines remain
/// raw and are checked by the generated Rust/source-presence checks around this function.
pub fn verify_expression_move(
    subject: &str,
    script: &AcmdScript,
    emitted: &str,
    report: &mut Report,
) {
    let expected = script.to_expression_events();
    let round_tripped = crate::acmd::parse_expression_script(emitted).to_expression_events();
    if expected == round_tripped {
        return;
    }
    report.blocker(
        subject,
        "the generated expression script does not reproduce the measured camera/rumble calls",
    );
}

/// Verify one move's sound script: read the emitted function back and compare what it plays.
///
/// The comparison is on the *resolved events* rather than on the statements, because that is
/// what the user was shown and what the game will do. An emitter that moved a call into a
/// different `frame` block, or dropped a member it no longer recognised, changes the events and
/// is caught; one that merely re-indented does not, and should not be.
pub fn verify_sound_move(subject: &str, script: &AcmdScript, emitted: &str, report: &mut Report) {
    let expected = script.to_sound_events();
    let round_tripped = crate::acmd::parse_sound_script(emitted).to_sound_events();
    if expected == round_tripped {
        return;
    }
    // Named rather than counted: "3 sounds became 2" does not say which move went quiet.
    let lost: Vec<String> = expected
        .iter()
        .filter(|event| !round_tripped.contains(event))
        .map(|event| format!("{} on frame {}", event.call.func, event.frame))
        .collect();
    report.blocker(
        subject,
        &if lost.is_empty() {
            "the generated sound script plays something the editor did not show".into()
        } else {
            format!(
                "the generated sound script does not play {}",
                lost.join(", ")
            )
        },
    );
}

/// Verify one move's hitbox script on its own, for the editor's generated-source preview.
pub fn verify_move(subject: &str, script: &AcmdScript, emitted: &str, report: &mut Report) {
    check_hitbox_fidelity(subject, script, emitted, report);
    check_hurtbox_fidelity(subject, script, emitted, report);
    check_speed_fidelity(subject, script, emitted, report);
    check_script_values(subject, script, report);
    check_script_shape(subject, script, report);
}

/// Verify one move's effect script on its own, for the editor's generated-source preview.
///
/// `tweaks` are needed, not incidental: a live speed override deliberately replaces a spawn's
/// own `LAST_EFFECT_SET_RATE` value in the emitted code, so without them a tweaked effect
/// looks exactly like an export that lost the script's rate.
/// `lost` is what [`crate::acmd::unexportable_effect_lines`] said about the script the calls were
/// parsed from. This function takes the finished list rather than the script for the reason C6c
/// exists: by export time the script is usually gone — `calls` has already lost those lines and
/// `emitted` never had them — but the list can be carried in a saved project, and the caller that
/// still holds a script can derive it in one line. Pass `None` when no source was ever in hand,
/// which is the case for a move reconstructed from live captures.
pub fn verify_effect_move(
    subject: &str,
    calls: &[EffectCall],
    emitted: &str,
    tweaks: &[LiveTweak],
    lost: Option<&[String]>,
    residue: &std::collections::BTreeMap<u32, Vec<String>>,
    report: &mut Report,
) {
    check_effect_fidelity(subject, calls, emitted, tweaks, report);
    check_effect_values(subject, calls, report);
    // The carried lines travel on the calls themselves, so a project saved and reloaded still
    // reports them. `residue` is the half that cannot: it belongs to a frame rather than a call,
    // and it is passed alongside for the same reason `lost` is. Both are saved with the project
    // — a user who opens a saved mod and exports it gets the same warnings as the one who parsed
    // the script this session.
    check_carried_lines(subject, calls, residue, report);
    if let Some(lost) = lost {
        check_dropped_lines(subject, lost, report);
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

/// Compare a spawn tail without mistaking a legacy plain macro for a changed call.
///
/// Newly added plain `EFFECT`/`EFFECT_FOLLOW` calls historically stored `extra_args = None`.
/// Their emitter has always had a deterministic compilable fallback, however, so read-back
/// parses that fallback as `Some(...)`.  Accept only that exact one-way normalization: explicit
/// source tails, empty tails, and every non-plain macro family remain strict comparisons.
fn effect_extra_args_match(want: &EffectCall, got: &EffectCall) -> bool {
    if want.extra_args == got.extra_args {
        return true;
    }
    if want.extra_args.is_some() || want.spawn_func != got.spawn_func {
        return false;
    }
    let Some(fallback) = crate::acmd::plain_spawn_fallback_tail(&want.spawn_func) else {
        return false;
    };
    got.extra_args.as_deref().is_some_and(|actual| {
        actual.len() == fallback.len()
            && actual
                .iter()
                .zip(fallback)
                .all(|(actual, expected)| actual == expected)
    })
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

/// Read the export back and check its hurtbox spans against the move on screen.
///
/// A blocker rather than a warning throughout, matching the collision check above and for the
/// same reason: intangibility decides whether a move trades or loses, so shipping a mod whose
/// knee is normal when the editor showed it intangible is worse than shipping no mod.
fn check_hurtbox_fidelity(subject: &str, script: &AcmdScript, emitted: &str, report: &mut Report) {
    let (want_states, want_pris) = script.to_hurtboxes();
    let (got_states, got_pris) = crate::acmd::parse_acmd_script(emitted).to_hurtboxes();

    if want_states.len() != got_states.len() {
        report.blocker(
            subject,
            format!(
                "the export has {} hurtbox state(s) but {} were specified",
                got_states.len(),
                want_states.len()
            ),
        );
    } else {
        for (want, got) in want_states.iter().zip(&got_states) {
            if want != got {
                report.blocker(
                    subject,
                    format!(
                        "the hurtbox state on {} at frame {} exported as {got:?}, not {want:?}",
                        want.target.label(),
                        want.active_start
                    ),
                );
            }
        }
    }

    if want_pris.len() != got_pris.len() {
        report.blocker(
            subject,
            format!(
                "the export has {} collision-priority span(s) but {} were specified",
                got_pris.len(),
                want_pris.len()
            ),
        );
    } else {
        for (want, got) in want_pris.iter().zip(&got_pris) {
            if want != got {
                report.blocker(
                    subject,
                    format!(
                        "the collision priority at frame {} exported as {got:?}, not {want:?}",
                        want.active_start
                    ),
                );
            }
        }
    }
}

fn check_speed_fidelity(subject: &str, script: &AcmdScript, emitted: &str, report: &mut Report) {
    let specified = script.to_speed_events();
    let exported = crate::acmd::parse_acmd_script(emitted).to_speed_events();
    if specified == exported {
        return;
    }
    report.blocker(
        subject,
        format!(
            "the generated SET_SPEED points do not reproduce the specified values: exported {exported:?}, specified {specified:?}"
        ),
    );
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
    // ATTACK_FP's modeled slots are rebuilt from Hitbox controls on export, so their source
    // spelling may legitimately normalize. The remaining slots have no editor representation
    // and must survive byte-for-byte as typed/raw IR.
    if let (Some(want), Some(got)) = (&want.fp, &got.fp) {
        const MODELED: &[usize] = &[
            0, 1, 3, 4, 5, 6, 7, 12, 14, 15, 16, 19, 20, 21, 23, 29, 30, 34,
        ];
        let unknown_want: Vec<_> = want
            .args
            .iter()
            .enumerate()
            .filter(|(index, _)| !MODELED.contains(index))
            .collect();
        let unknown_got: Vec<_> = got
            .args
            .iter()
            .enumerate()
            .filter(|(index, _)| !MODELED.contains(index))
            .collect();
        if unknown_want != unknown_got {
            out.push(format!(
                "ATTACK_FP preserved slots — specified {:?}, exported {:?}",
                unknown_want, unknown_got
            ));
        }
    } else if want.fp != got.fp {
        out.push(format!(
            "ATTACK_FP payload — specified {:?}, exported {:?}",
            want.fp, got.fp
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
    let mut specified: Vec<EffectCall> = calls
        .iter()
        .filter(|call| !call.disabled)
        .cloned()
        .map(EffectCall::normalized_timing)
        .collect();
    let exported = crate::acmd::parse_effect_script(emitted).to_effect_calls();
    let mut exported: Vec<EffectCall> = exported
        .into_iter()
        .map(EffectCall::normalized_timing)
        .collect();
    specified.sort_by_key(effect_order);
    exported.sort_by_key(effect_order);

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
    for (want, got) in specified.iter().zip(&exported) {
        if want.control.is_some() || got.control.is_some() {
            if want.control != got.control
                || want.spawn_func != got.spawn_func
                || want.active_start != got.active_start
            {
                report.blocker(
                    subject,
                    format!(
                        "control {} on frame {} did not round-trip",
                        want.spawn_func, want.active_start
                    ),
                );
            }
            continue;
        }
        // A spawn whose macro tail was never recorded is already reported by name as a
        // downgrade. The emitter necessarily falls back to the plain EFFECT/EFFECT_FOLLOW
        // spelling, so the macro name, alternate graphic, and tail cannot be compared. Keep
        // checking every independent field, however: a bad offset, frame, or modifier must not
        // disappear merely because the source macro's tail was unavailable.
        let downgraded = want.extra_args.is_none()
            && want.raw_line.is_none()
            && want.color.is_none()
            && !want.spawn_func.is_empty()
            && crate::acmd::plain_spawn_fallback_tail(&want.spawn_func).is_none();
        let mut out = Vec::new();
        diff_fields!(
            out,
            want,
            got,
            effect_name,
            bone_name,
            offset,
            rotation,
            scale,
            follows_bone,
            active_start,
            active_end,
            // Compared here rather than trusted, because the corpus oracle pairs calls on
            // `(spawn_func, effect_name)` alone and a colour command has no effect name at
            // all — every one of them would pair up and compare equal without this line.
            color,
            // Work IDs are source tokens. The generated helper restores their compile-time
            // constant form, so the token itself still has to round-trip.
            work_int,
            // Opacity has no live override to legitimately replace it, unlike the tint and
            // rate below, so any difference at all is the export losing or inventing a
            // `LAST_EFFECT_SET_ALPHA` line.
            camera_offset,
            alpha,
            // Particle tint is a separate last-particle primitive with no live multiplier
            // fallback. It must round-trip as its own field instead of being folded into tint.
            particle_tint,
        );
        if !downgraded {
            if want.effect_name_alt != got.effect_name_alt {
                out.push(format!(
                    "effect_name_alt — specified {:?}, exported {:?}",
                    want.effect_name_alt, got.effect_name_alt
                ));
            }
            if want.spawn_func != got.spawn_func {
                out.push(format!(
                    "spawn_func — specified {}, exported {}",
                    want.spawn_func, got.spawn_func
                ));
            }
            if !effect_extra_args_match(want, got) {
                out.push(format!(
                    "extra_args — specified {:?}, exported {:?}",
                    want.extra_args, got.extra_args
                ));
            }
        }
        for difference in out {
            report.blocker(
                subject,
                format!(
                    "spawn {} on frame {} exported a different {difference}",
                    want.effect_name, want.active_start
                ),
            );
        }

        // Tint is checked apart from the fields above for the same reason the rate below is:
        // a live colour multiplier is meant to replace the spawn's authored tint. Said out
        // loud either way — the user set a multiplier on an effect kind and it quietly took
        // over a colour the script had chosen, which is worth knowing before shipping.
        if got.tint != want.tint {
            let override_tint = tweaks
                .iter()
                .find(|tweak| tweak.effect_name.eq_ignore_ascii_case(&want.effect_name))
                .and_then(|tweak| tweak.color)
                .map(|[r, g, b, _a]| [r, g, b]);
            if override_tint == got.tint {
                report.warn(
                    subject,
                    format!(
                        "spawn {} on frame {} ships the live colour override instead of the \
                         tint its script sets ({})",
                        want.effect_name,
                        want.active_start,
                        want.tint
                            .map(|[r, g, b]| format!("{r}, {g}, {b}"))
                            .unwrap_or("none".into()),
                    ),
                );
            } else {
                report.blocker(
                    subject,
                    format!(
                        "spawn {} on frame {} exported a different tint",
                        want.effect_name, want.active_start
                    ),
                );
            }
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
///
/// `lost` comes from [`crate::acmd::unexportable_effect_lines`], which already folds together
/// both ways of losing a line — no typed variant, and a typed one that binds to no spawn. This
/// used to derive it here and append the second list separately, which reported every unbound
/// modifier twice.
///
/// **Do not filter `lost` against the emitted text.** C6c tried it, to stop a list carried in a
/// saved project from naming a line a newer Visionary had since learned to emit. Measured over
/// the corpus it silenced two real losses: kirby's `SpecialHi2` and `SpecialAirHi2` each call
/// `methodlib::L2CAgent::pop()` twice, one carried on a spawn and one not, so the surviving copy
/// made the dropped one look reproduced. Reporting a loss that no longer happens is a stale
/// warning; hiding one that does is the silence this whole check exists to end.
fn check_dropped_lines(subject: &str, lost: &[String], report: &mut Report) {
    // Grouped by text and kept in first-seen order: a line inside a `for` body is genuinely
    // lost once per iteration, but saying so five times reads as five different problems.
    let mut seen: Vec<(String, usize)> = Vec::new();
    for line in lost {
        match seen.iter_mut().find(|(text, _)| text == line) {
            Some((_, count)) => *count += 1,
            None => seen.push((line.clone(), 1)),
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

/// Name the lines the export reproduces verbatim without understanding them.
///
/// The counterpart to [`check_dropped_lines`], and it exists because C6 turned one kind of
/// surprise into another. A line the editor cannot model is no longer deleted — it is copied
/// through — and a copied line is not the same as a supported one. Two things follow that a
/// user should hear before they build:
///
/// - The editor does not know what it does, so editing the move around it will not update it.
///   A carried spawn keeps its original graphic no matter what the panel says.
/// - It is copied from the decompiled dump *as written*, and those dumps are not valid Rust.
///   Costume checks come through as `if(0x2508e0(…)){`, which will not compile until the user
///   spells the condition properly. Before C6 the same script exported cleanly and silently
///   wrong; this is the better failure, but it is still a failure and it should not arrive as
///   a mystery from `cargo build`.
///
/// A warning, on the same reasoning as the dropped-line check: refusing the export helps nobody
/// who can read the message and fix the line. A known `smash-script` wrapper gap is the one
/// exception: the generated source is not buildable in that case, so it is a blocker rather than
/// a warning. The decompiler's zero-argument `LAST_PARTICLE_SET_COLOR(agent)` spelling is the
/// remaining known non-buildable C7 shape;
/// `LAST_EFFECT_SET_WORK_INT` uses the generated local primitive helper emitted by the effect
/// exporter.
fn check_carried_lines(
    subject: &str,
    calls: &[EffectCall],
    residue: &std::collections::BTreeMap<u32, Vec<String>>,
    report: &mut Report,
) {
    let mut seen: Vec<String> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    for line in calls
        .iter()
        .filter(|call| !call.disabled)
        .flat_map(|call| call.leading.iter().chain(&call.trailing))
        // Frame-anchored residue is emitted on exactly the same terms as a carried line and
        // needs the same warning. E3 added the channel and this chain a moment later: the first
        // draft emitted those lines and said nothing about them, so a script whose only
        // unmodelled line owned a frame of its own exported verbatim, silently.
        .chain(residue.values().flatten())
    {
        // Braces and the regenerated `is_excute` header are this module's own scaffolding, not
        // the user's code, so naming them would bury the lines that are actually theirs.
        let text = line.trim();
        if text.contains("is_excute") || !text.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        if let Some(message) = carried_line_blocker(text) {
            if !blocked.iter().any(|s| s == text) {
                blocked.push(text.to_string());
                report.blocker(subject, message);
            }
        } else if !seen.iter().any(|s| s == text) {
            seen.push(text.to_string());
        }
    }
    for line in seen {
        report.warn(
            subject,
            format!(
                "the generated script copies this line through as written — editing the move \
                 will not update it, and it must be valid Rust on its own: {line}"
            ),
        );
    }
}

/// Return a blocker for an opaque effect line that the generated Rust cannot compile as written.
///
/// `sv_animcmd` exposes more primitives than `smash-script::macros`, so a dumped source line can
/// be a real game call while still naming no callable wrapper in an exported Skyline project.
/// The particle colour wrapper does exist, but the real dump's stack-form line has no explicit RGB
/// arguments; emitting it unchanged would call a three-argument wrapper with only `agent`. The
/// measured scale-W dump line is handled by the generated dynamic-arity helper. Do not broaden
/// this to all opaque lines: a decompiled condition or
/// a project-specific raw helper is a warning, not enough evidence for a new compile rule.
fn carried_line_blocker(line: &str) -> Option<String> {
    if line == "macros::LAST_PARTICLE_SET_COLOR(agent);" {
        return Some(
            "the generated script carries the dump's zero-argument `LAST_PARTICLE_SET_COLOR`, \
             but smash-script's wrapper requires three colour arguments; resolve its Lua-stack \
             inputs before exporting"
                .to_string(),
        );
    }
    None
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
        // `ATTACK_ABS` has no joint slot at all — it applies to an opponent already caught, so
        // there is nothing to attach to. Its empty bone name means "not applicable" rather than
        // "the author left it blank", and demanding one here would fail every Kirby throw.
        if hitbox.category != crate::data::CAT_ABS && hitbox.category != crate::data::CAT_ATTACK_FP
        {
            check_hash_name(
                subject,
                &format!("{label} joint"),
                &hitbox.bone_name,
                report,
            );
        }
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
        ExcuteStmt::AddSpeedNoLimit(call) => {
            check_finite(
                subject,
                "ADD_SPEED_NO_LIMIT x velocity",
                call.speed_x,
                report,
            );
            check_finite(
                subject,
                "ADD_SPEED_NO_LIMIT y velocity",
                call.speed_y,
                report,
            );
        }
        ExcuteStmt::SetSpeed(call) => {
            check_finite(subject, "SET_SPEED x velocity", call.speed_x, report);
            check_finite(subject, "SET_SPEED y velocity", call.speed_y, report);
        }
        ExcuteStmt::KineticAddSpeed(call) => {
            check_finite(
                subject,
                "KineticModule::add_speed x velocity",
                call.speed_x,
                report,
            );
            check_finite(
                subject,
                "KineticModule::add_speed y velocity",
                call.speed_y,
                report,
            );
        }
        ExcuteStmt::KineticEnergy(call) if call.kinetic_energy_id.trim().is_empty() => {
            report.blocker(
                subject,
                "KineticModule suspend/resume energy call has an empty energy ID",
            );
        }
        ExcuteStmt::Correct(call) if call.kind.trim().is_empty() => {
            report.blocker(subject, "CORRECT has an empty correction kind");
        }
        ExcuteStmt::FtCatchStop(call) => {
            check_finite(subject, "FT_CATCH_STOP argument 1", call.arg1, report);
            check_finite(subject, "FT_CATCH_STOP argument 2", call.arg2, report);
        }
        ExcuteStmt::FtStartAdjustMotionFrame(call) => {
            check_finite(
                subject,
                "FT_START_ADJUST_MOTION_FRAME_arg1 value",
                call.value,
                report,
            );
        }
        ExcuteStmt::ClrSpeed(call) if call.kinetic_kind.trim().is_empty() => {
            report.blocker(subject, "CLR_SPEED has an empty kinetic kind");
        }
        ExcuteStmt::ChangeKinetic(call) if call.kinetic_type.trim().is_empty() => {
            report.blocker(
                subject,
                "KineticModule::change_kinetic has an empty kinetic type",
            );
        }
        ExcuteStmt::WorkFlag(call) if call.flag.trim().is_empty() => {
            report.blocker(subject, "WorkModule flag call has an empty flag token");
        }
        ExcuteStmt::WorkTransitionTerm(call) if call.transition_term.trim().is_empty() => {
            report.blocker(
                subject,
                "WorkModule transition-term call has an empty transition term token",
            );
        }
        ExcuteStmt::WorkModuleSet(call)
            if call.value.trim().is_empty() || call.slot.trim().is_empty() =>
        {
            report.blocker(
                subject,
                "WorkModule value-set call has an empty value or slot token",
            );
        }
        _ => {}
    }
}

fn check_effect_values(subject: &str, calls: &[EffectCall], report: &mut Report) {
    for call in calls
        .iter()
        .filter(|call| !call.disabled)
        .cloned()
        .map(EffectCall::normalized_timing)
    {
        // A colour command has no graphic to name it, so it is labelled by its command.
        let label = match &call.color {
            Some(_) => format!("{} on frame {}", call.spawn_func, call.active_start),
            None if call.control.is_some() => {
                format!("{} on frame {}", call.spawn_func, call.active_start)
            }
            None => format!("spawn {} on frame {}", call.effect_name, call.active_start),
        };
        if let Some(color) = &call.color {
            check_color_values(subject, &label, &call.spawn_func, color, report);
            continue;
        }
        if let Some(control) = &call.control {
            match control {
                crate::data::EffectControl::DetachKind {
                    effect_name,
                    unk: _,
                } => {
                    check_hash_name(
                        subject,
                        &format!("{label} effect kind"),
                        effect_name,
                        report,
                    );
                }
                crate::data::EffectControl::DetachKindWork { work, unk: _ }
                | crate::data::EffectControl::EnableArea { kind: work }
                | crate::data::EffectControl::UnableArea { kind: work } => {
                    if work.trim().is_empty() {
                        report.blocker(subject, format!("{label} has an empty control value"));
                    }
                }
            }
            continue;
        }
        // Checked for a trail too. Its *transform* is genuinely never emitted — the line rides
        // through as written — but its graphic and joints are not: `retarget_trail_line` splices
        // an edited one back in through `hash_arg`, which re-quotes it. So a quote or backslash
        // here yields `Hash40::new("to"er")`, which is not Rust. The guard below used to cover
        // these two names as well, on the strength of a comment that said a trail was never
        // re-quoted; that stopped being true when the splice landed and the comment did not.
        check_hash_name(
            subject,
            &format!("{label} graphic"),
            &call.effect_name,
            report,
        );
        check_hash_name(subject, &format!("{label} joint"), &call.bone_name, report);
        if let Some(bone2) = &call.trail_bone2 {
            check_hash_name(subject, &format!("{label} second joint"), bone2, report);
        }
        if call.raw_line.is_none() {
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
        if let Some(offset) = call.camera_offset {
            check_finite(subject, &format!("{label} camera offset"), offset, report);
        }
        if let Some(work) = &call.work_int {
            if work.trim().is_empty() {
                report.blocker(subject, format!("{label} has an empty Work ID"));
            }
        }
        if let Some([r, g, b]) = call.particle_tint {
            for (axis, value) in ["r", "g", "b"].iter().zip([r, g, b]) {
                check_finite(
                    subject,
                    &format!("{label} particle tint {axis}"),
                    value,
                    report,
                );
            }
        }
        if let Some(values) = &call.scale_w {
            if !(1..=3).contains(&values.len()) {
                report.blocker(
                    subject,
                    format!(
                        "{label} has {} LAST_EFFECT_SET_SCALE_W values; the native primitive accepts one to three",
                        values.len()
                    ),
                );
            }
            for (index, value) in values.iter().copied().enumerate() {
                check_finite(
                    subject,
                    &format!("{label} scale W value {}", index + 1),
                    value,
                    report,
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
        // A branch is the strongest form of this: not only is the line unmodelled, whether the
        // lines under it run at all is decided at runtime.
        AcmdStmt::Raw(_) | AcmdStmt::RawBlock { .. } => true,
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
            // The clock counts *motion* frames, which is what every other statement here is
            // keyed to, and a rate call does not move it — it changes how many game frames
            // those motion frames take. So this arm checks the value and nothing else.
            AcmdStmt::MotionRate(rate) => {
                if *rate <= 0.0 {
                    report.warn(
                        subject,
                        format!(
                            "`FT_MOTION_RATE(agent, {rate})` stops the animation advancing, and \
                             nothing below it will run"
                        ),
                    );
                }
            }
            // A bare command is a single statement, so the duplicate-call check the `Excute`
            // arm runs has nothing to compare it against. `Bare` is only produced for `sound_`
            // scripts, which this pass does not verify at all.
            AcmdStmt::Bare(_) => {}
            // Unreachable: a branch makes `has_unmodelled_flow` true and this pass never runs.
            AcmdStmt::WaitLoopClear | AcmdStmt::Raw(_) | AcmdStmt::RawBlock { .. } => {}
        }
    }
}

/// Every statement in the tree, loop and branch bodies included, in source order.
///
/// Branches are descended into because the checks this feeds are about *values* — a NaN offset
/// or an unhashable name is just as broken inside an `if` as outside one, and whether the
/// branch is taken has no bearing on it.
fn flatten(stmts: &[AcmdStmt]) -> Vec<&AcmdStmt> {
    let mut out = Vec::new();
    for stmt in stmts {
        out.push(stmt);
        match stmt {
            AcmdStmt::Loop { body, .. } | AcmdStmt::RawBlock { body, .. } => {
                out.extend(flatten(body))
            }
            _ => {}
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

    #[test]
    fn a_retimed_one_shot_with_a_legacy_stale_end_verifies_cleanly() {
        let source = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_flash"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, true);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(source).to_effect_calls();
        let mut edited = pristine.clone();
        edited[0].active_start = 8;
        // This is the legacy shape that used to block verification after a start edit.
        edited[0].active_end = pristine[0].active_start;
        let emitted = crate::acmd::preview_effect_fn(
            &edited,
            "test",
            &[],
            &std::collections::BTreeMap::new(),
        );
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &edited,
            &emitted,
            &[],
            None,
            &std::collections::BTreeMap::new(),
            &mut report,
        );
        assert!(!report.has_blockers(), "{}", messages(&report));
        assert!(emitted.contains("frame(agent.lua_state_agent, 8.0);"));
    }

    #[test]
    fn legacy_plain_effect_tail_matches_its_deterministic_fallback_only() {
        let source = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("sys_flash"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, true);
    }
}
"#;
        let parsed = crate::acmd::parse_effect_script(source).to_effect_calls();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].extra_args.as_deref(),
            Some(&["true".to_string()][..])
        );

        // A newly added or legacy project call can have no recorded tail.  The plain fallback
        // emits exactly the same `true` tail and should not turn that compatibility detail into
        // an export blocker.
        let mut legacy = parsed.clone();
        legacy[0].extra_args = None;
        let emitted = crate::acmd::preview_effect_fn(
            &legacy,
            "test",
            &[],
            &std::collections::BTreeMap::new(),
        );
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &legacy,
            &emitted,
            &[],
            None,
            &std::collections::BTreeMap::new(),
            &mut report,
        );
        assert!(!report.has_blockers(), "{}", messages(&report));

        // An explicit tail remains an authorial claim.  Do not normalize a genuinely different
        // bool or relax the comparison for a source-authored `Some(...)` value.
        let mut wrong = parsed;
        wrong[0].extra_args = Some(vec!["false".into()]);
        let emitted =
            crate::acmd::preview_effect_fn(&wrong, "test", &[], &std::collections::BTreeMap::new());
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &wrong,
            &emitted,
            &[],
            None,
            &std::collections::BTreeMap::new(),
            &mut report,
        );
        assert!(
            !report.has_blockers(),
            "explicit tails should round-trip: {}",
            messages(&report)
        );

        // The read-back of the emitted `false` is faithful.  To exercise the strict mismatch,
        // compare a call that claims the plain fallback while its emitted source carries a
        // different explicit tail.
        let mut mismatched = wrong;
        mismatched[0].extra_args = None;
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &mismatched,
            &emitted,
            &[],
            None,
            &std::collections::BTreeMap::new(),
            &mut report,
        );
        assert!(
            report
                .blockers()
                .any(|finding| finding.message.contains("extra_args")),
            "a None tail must not accept a non-fallback explicit tail: {report:?}"
        );
    }

    #[test]
    fn missing_non_plain_tail_downgrade_still_checks_transform_fields() {
        let source = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("sys_hit_l"), Hash40::new("sys_hit_r"), Hash40::new("top"), 1.0, 2.0, 3.0, 0.0, 90.0, 45.0, 1.5, true, *EF_FLIP_YZ);
    }
}
"#;
        let parsed = crate::acmd::parse_effect_script(source).to_effect_calls();
        assert_eq!(parsed.len(), 1);

        // Without the recorded tail, emission intentionally falls back to plain
        // EFFECT_FOLLOW. That downgrade is a warning, not permission to ignore the rest of the
        // call's transform data.
        let mut downgraded = parsed;
        downgraded[0].extra_args = None;
        let emitted = crate::acmd::preview_effect_fn(
            &downgraded,
            "test",
            &[],
            &std::collections::BTreeMap::new(),
        );
        let mut clean = Report::default();
        verify_effect_move(
            "test",
            &downgraded,
            &emitted,
            &[],
            None,
            &std::collections::BTreeMap::new(),
            &mut clean,
        );
        assert!(
            !clean.has_blockers(),
            "downgrade itself blocked: {}",
            messages(&clean)
        );
        assert!(
            clean
                .findings
                .iter()
                .any(|finding| finding.message.contains("trailing arguments")),
            "the missing-tail downgrade should remain visible: {}",
            messages(&clean)
        );

        // The generated source still carries the original transform. A changed specified
        // offset must be reported even though the macro family was downgraded.
        downgraded[0].offset[0] = 99.0;
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &downgraded,
            &emitted,
            &[],
            None,
            &std::collections::BTreeMap::new(),
            &mut report,
        );
        assert!(
            report
                .blockers()
                .any(|finding| finding.message.contains("offset")),
            "a non-tail transform mismatch must remain a blocker: {}",
            messages(&report)
        );
    }

    /// The property the whole module exists for, checked against every script the app has ever
    /// fetched. The intentional exception is the corpus's malformed zero-argument
    /// `LAST_PARTICLE_SET_COLOR` line: it is carried faithfully but cannot compile against its
    /// three-argument wrapper, so the verifier must report that blocker rather than let this
    /// oracle hide it. Everything else
    /// still has to be a faithful inverse of the parser across a thousand real functions.
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
                let Some(whole) = crate::acmd::cached_script_body_at(&file.path()) else {
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
                    let (calls, residue) = effect_script.to_effect_calls_and_residue();
                    if !calls.is_empty() {
                        let emitted =
                            crate::acmd::preview_effect_fn(&calls, "audit", &[], &residue);
                        verify_effect_move(
                            &label,
                            &calls,
                            &emitted,
                            &[],
                            Some(&crate::acmd::unexportable_effect_lines(&effect_script)),
                            &residue,
                            &mut report,
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 100, "the cache held almost nothing: {checked}");
        let blockers: Vec<String> = report.blockers().map(|f| f.to_string()).collect();
        let unexpected: Vec<&String> = blockers
            .iter()
            .filter(|line| !line.contains("LAST_PARTICLE_SET_COLOR(agent);"))
            .collect();
        assert!(
            unexpected.is_empty(),
            "{} unexpected blockers among {checked} scripts; only the malformed particle wrapper line is intentional:\n{}",
            unexpected.len(),
            unexpected
                .iter()
                .map(|line| line.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn dynamic_scale_w_stack_form_is_exportable_with_its_authored_arity() {
        let source = r#"unsafe extern "C" fn effect_scale_w(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 23.0);
    if macros::is_excute(agent) {
        macros::LAST_EFFECT_SET_SCALE_W(agent, 1821741189);
    }
}
"#;
        let parsed = crate::acmd::parse_effect_script(source);
        let (calls, residue) = parsed.to_effect_calls_and_residue();
        assert!(
            calls.is_empty(),
            "the stack-form fixture has no effect spawn"
        );
        let emitted = crate::acmd::preview_effect_fn(&calls, "scale_w", &[], &residue);
        assert!(
            emitted.contains("visionary_last_effect_set_scale_w(agent, &[")
                && emitted.contains("]);"),
            "dynamic scale-W helper call missing:\n{emitted}"
        );

        let mut report = Report::default();
        verify_effect_move(
            "lucario / specialsthrow",
            &calls,
            &emitted,
            &[],
            Some(&crate::acmd::unexportable_effect_lines(&parsed)),
            &residue,
            &mut report,
        );
        assert!(
            !report
                .blockers()
                .any(|finding| finding.message.contains("LAST_EFFECT_SET_SCALE_W")),
            "the dynamic helper must remove the former wrapper blocker: {report:?}"
        );
        assert!(carried_line_blocker("macros::LAST_EFFECT_SET_SCALE_W(agent, 1, 2, 3);").is_none());
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
    fn an_empty_work_module_set_token_is_reported_as_a_blocker() {
        let script = AcmdScript {
            stmts: vec![AcmdStmt::Excute(vec![ExcuteStmt::WorkModuleSet(
                crate::data::WorkModuleSetCall {
                    kind: crate::data::WorkModuleSetKind::Int,
                    receiver: "agent.module_accessor".into(),
                    value: String::new(),
                    slot: String::new(),
                },
            )])],
        };
        let mut report = Report::default();
        check_script_values("test", &script, &mut report);
        assert!(report.has_blockers(), "{report:?}");
        assert!(
            messages(&report).contains("WorkModule value-set call has an empty value or slot"),
            "{report:?}"
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
        let report = verify_export(&project, &[], &[], &[], &[], &Default::default());
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
        let report = verify_export(&project, &edits, &[], &[], &[], &Default::default());
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
        let report = verify_export(&project, &edits, &[], &[], &[], &Default::default());
        assert!(
            messages(&report).contains("missing from the exported project"),
            "{}",
            messages(&report)
        );
    }

    /// The whole point of reading the export back rather than trusting the emitter. An
    /// intangible knee that ships as a normal one is a move that loses every trade it used to
    /// win, and nothing about the generated file would look wrong.
    #[test]
    fn an_export_that_changes_a_hurtbox_status_is_refused() {
        let parsed = script(
            "    frame(agent.lua_state_agent, 9.0);\n    if macros::is_excute(agent) {\n        \
             macros::HIT_NODE(agent, Hash40::new(\"kneer\"), *HIT_STATUS_XLU);\n    }",
        );
        assert!(verify(&parsed).is_clean(), "{}", messages(&verify(&parsed)));

        // Emit it, then corrupt exactly the status — the shape of a slot-table mistake.
        let emitted =
            preview_game_fn(&parsed, "test").replace("*HIT_STATUS_XLU", "*HIT_STATUS_NORMAL");
        let mut report = Report::default();
        verify_move("test", &parsed, &emitted, &mut report);
        assert!(
            messages(&report).contains("hurtbox state on kneer"),
            "{}",
            messages(&report)
        );
    }

    /// A dropped hurtbox line has to fail too, not just a changed one — that is the failure
    /// mode `rebuild_script_from_hitboxes` actually had.
    #[test]
    fn an_export_that_drops_a_hurtbox_line_is_refused() {
        let parsed = script(
            "    frame(agent.lua_state_agent, 9.0);\n    if macros::is_excute(agent) {\n        \
             macros::COL_PRI(agent, 200);\n    }",
        );
        let emitted = preview_game_fn(&parsed, "test").replace("macros::COL_PRI(agent, 200);", "");
        let mut report = Report::default();
        verify_move("test", &parsed, &emitted, &mut report);
        assert!(
            messages(&report).contains("collision-priority span"),
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

    /// A trail's names are re-quoted on export, so a quote in one must be refused there too.
    ///
    /// The skip in `check_effect_values` was written when a trail's line rode through verbatim
    /// and nothing about it was re-emitted — true then, and stale the moment C2 taught the
    /// export to splice the graphic and joint slots back in through `hash_arg`. Since then an
    /// edit putting a quote into any of the three has produced `Hash40::new("to"er")`, which is
    /// not Rust, with the verifier reporting nothing.
    ///
    /// All three names are checked in one test because they share the one splice path and the
    /// one skip: whichever of them is left out is left out silently.
    #[test]
    fn a_quote_in_a_trails_graphic_or_either_joint_is_refused() {
        for field in ["graphic", "joint", "second joint"] {
            let mut calls =
                crate::acmd::parse_effect_script(&crate::acmd::tests::corpus_trail_script())
                    .to_effect_calls();
            let trail = calls
                .iter_mut()
                .find(|call| call.spawn_func == "AFTER_IMAGE_ON")
                .expect("trail call");
            match field {
                "graphic" => trail.effect_name = "to\"er".into(),
                "joint" => trail.bone_name = "to\"er".into(),
                _ => trail.trail_bone2 = Some("to\"er".into()),
            }

            let mut report = Report::default();
            check_effect_values("t", &calls, &mut report);
            assert!(
                messages(&report).contains("quote or backslash"),
                "a trail's {field} reaches the emitted source and must be checked: {}",
                messages(&report)
            );
        }
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

    /// The whole point of C5: a line the export cannot reproduce must be named rather than
    /// silently deleted.
    ///
    /// This test has now been repointed twice, and the premise assertion below is what forced
    /// it both times. It was written against `LAST_EFFECT_SET_COLOR` until C1 modelled that
    /// macro, then against `FILL_SCREEN_MODEL_COLOR` until C6 began carrying untyped macros
    /// through. Move it again rather than deleting it — the check it guards outlives any one
    /// example of a loss.
    ///
    /// `wait_loop_sync_mot` is the current example and a more durable one, because it is not
    /// merely waiting to be modelled: it is dropped *by decision*. It advances the ACMD
    /// coroutine, while the regenerated function states every frame absolutely with its own
    /// `frame()` calls — so carrying it would shift every effect after it. Seven sit in the
    /// corpus and each is a real loss the user should hear about.
    #[test]
    fn a_line_the_export_cannot_reproduce_is_named_rather_than_silently_deleted() {
        let src = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, false);
    }
    wait_loop_sync_mot(agent.lua_state_agent, false, 3.0);
}
"#;
        let source = crate::acmd::parse_effect_script(src);
        let (calls, residue) = source.to_effect_calls_and_residue();
        let emitted = crate::acmd::preview_effect_fn(&calls, "test", &[], &residue);
        assert!(
            !emitted.contains("wait_loop_sync_mot"),
            "the premise no longer holds — the export now keeps this line, so this test is \
             checking nothing:\n{emitted}"
        );

        let mut report = Report::default();
        verify_effect_move(
            "test",
            &calls,
            &emitted,
            &[],
            Some(&crate::acmd::unexportable_effect_lines(&source)),
            &residue,
            &mut report,
        );
        assert!(
            messages(&report).contains("wait_loop_sync_mot(agent.lua_state_agent, false, 3.0);"),
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
        verify_effect_move(
            "test",
            &calls,
            &emitted,
            &[],
            None,
            &Default::default(),
            &mut blind,
        );
        assert!(
            !messages(&blind).contains("does not include this line"),
            "{}",
            messages(&blind)
        );
    }

    /// The report must track what the export actually does, in both directions.
    ///
    /// C1 nearly broke it one way: modelling `LAST_EFFECT_SET_COLOR` stopped the costume-gated
    /// tints from being *named* without making them survive, so a real loss went quiet. C6 can
    /// break it the other way — it makes those tints survive, and a report still calling them
    /// deleted would send a user hunting for a line that is sitting in the file.
    ///
    /// Both halves are asserted here on one script, because the failure mode is the report and
    /// the export disagreeing rather than either being wrong alone. Dolly's up special supplies
    /// the line carried by a spawn in its own block; the second frame supplies one with no spawn
    /// anywhere in its block, which C6 could not place and E3 now emits at a frame of its own.
    ///
    /// The second case used to assert the opposite — that the alpha was *deleted* and named as
    /// deleted. That was the truth until E3 and is written down here rather than edited away,
    /// because the two are one line apart in the report and swapping them silently is precisely
    /// what this test exists to catch.
    ///
    /// Note what the fixture has to do to be honest: it passes the residue the parse produced,
    /// not `Default::default()`. An empty map would have kept `!emitted.contains(...)` green
    /// while proving only that the test forgot to pass anything.
    #[test]
    fn the_loss_report_names_what_the_export_drops_and_only_that() {
        let src = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_ALPHA(agent, Hash40::new("dolly_roll_l_color1"), Hash40::new("throw"), 0, 2.5, 0, 0, 0, 0, 1, true, 0.8);
    }
    if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){
        if macros::is_excute(agent) {
            macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);
        }
    }
    frame(agent.lua_state_agent, 20.0);
    if macros::is_excute(agent) {
        macros::LAST_EFFECT_SET_ALPHA(agent, 0.25);
    }
}
"#;
        let source = crate::acmd::parse_effect_script(src);
        let (calls, residue) = source.to_effect_calls_and_residue();
        assert_eq!(
            calls[0].tint, None,
            "the premise: a costume-gated tint still binds to no spawn"
        );
        assert!(
            residue.contains_key(&20),
            "the premise: frame 20 owns its line, because no spawn in that block can: {residue:?}"
        );
        let emitted = crate::acmd::preview_effect_fn(&calls, "test", &[], &residue);

        let mut report = Report::default();
        verify_effect_move(
            "test",
            &calls,
            &emitted,
            &[],
            Some(&crate::acmd::unexportable_effect_lines(&source)),
            &residue,
            &mut report,
        );
        let said = messages(&report);
        // The two reports use different wording on purpose, and the difference is the whole
        // point of this test — "does not include" is a deletion, "copies this line through" is
        // a preservation. Matching on the line text alone would pass either way.
        let deleted = |line: &str| {
            said.lines()
                .any(|m| m.contains("does not include this line") && m.contains(line))
        };

        // Carried: in the file, so out of the deletion report.
        assert!(
            emitted.contains("macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);"),
            "the costume tint must reach the export:\n{emitted}"
        );
        assert!(
            !deleted("LAST_EFFECT_SET_COLOR"),
            "a line the export keeps must not be reported as deleted:\n{said}"
        );
        // ...but it must still be flagged as copied through rather than understood, or a user
        // will edit the move and wonder why the tint never changes.
        assert!(
            said.contains("copies this line through as written")
                && said.contains("macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);"),
            "a carried line must be named as carried:\n{said}"
        );

        // Frame 20 has no spawn for the alpha to ride on, and attaching it to frame 9's spawn
        // would retime it rather than preserve it. E3's answer is neither: the frame keeps it.
        assert!(
            emitted.contains("frame(agent.lua_state_agent, 20.0);")
                && emitted.contains("macros::LAST_EFFECT_SET_ALPHA(agent, 0.25);"),
            "a line whose frame holds no spawn must be emitted at that frame:\n{emitted}"
        );
        assert!(
            !deleted("LAST_EFFECT_SET_ALPHA"),
            "and must not still be reported as deleted:\n{said}"
        );
        // It is copied through, not understood — the same warning the carried tint gets, for the
        // same reason. Editing the alpha in the panel will not change this line.
        assert!(
            said.contains("copies this line through as written")
                && said.contains("macros::LAST_EFFECT_SET_ALPHA(agent, 0.25);"),
            "a line emitted verbatim must be named as verbatim:\n{said}"
        );
        assert!(!report.has_blockers(), "{said}");
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
        let (calls, residue) = source.to_effect_calls_and_residue();
        let emitted = crate::acmd::preview_effect_fn(&calls, "test", &[], &residue);
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &calls,
            &emitted,
            &[],
            Some(&crate::acmd::unexportable_effect_lines(&source)),
            &residue,
            &mut report,
        );
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

        let emitted = crate::acmd::preview_effect_fn(&calls, "test", &tweaks, &Default::default());
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &calls,
            &emitted,
            &tweaks,
            None,
            &Default::default(),
            &mut report,
        );
        assert!(!report.has_blockers(), "{}", messages(&report));
        assert!(
            messages(&report).contains("ships the live speed override"),
            "{}",
            messages(&report)
        );

        // Without the tweak to explain it, the same divergence is an export that lost a value.
        let mut report = Report::default();
        verify_effect_move(
            "test",
            &calls,
            &emitted,
            &[],
            None,
            &Default::default(),
            &mut report,
        );
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
