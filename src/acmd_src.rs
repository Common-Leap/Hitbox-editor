//! Read ACMD scripts from the user's OWN Rust source instead of the dumped-script mirror,
//! and write edited values back into it.
//!
//! The GitHub mirror holds vanilla scripts. Anyone who has already modded a move is looking
//! at code that no longer matches their game: their move may use `EFFECT_FOLLOW` where
//! vanilla used `EFFECT`, may spawn a graphic vanilla never had, or may not exist upstream
//! at all. Pointing Visionary at the smashline project that actually builds their plugin
//! makes the editor read what they wrote.
//!
//! The write-back direction is deliberately narrow. It rewrites argument VALUES in place and
//! nothing else: the macro a line calls, the order of statements, the comments, and the
//! whitespace all survive untouched, because the point is to retune numbers that were
//! already dialled in by hand — not to regenerate someone's code from the editor's model of
//! it. Anything that would need the code restructured is refused and reported, not guessed
//! at. For "throw the original away and emit a fresh project", `mod_export` already exists.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Files bigger than this are not plausible hand-written ACMD and are skipped during the
/// scan, so pointing at a directory that also holds generated data or vendored code does not
/// turn indexing into a multi-second stall.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

/// Directories never worth descending into when looking for ACMD source.
const SKIPPED_DIRS: &[&str] = &["target", ".git", "node_modules", ".cargo"];

// ── Macro call scanning ───────────────────────────────────────────────────────

/// One `macros::NAME(...)` call located in a source file, with a byte span per argument.
///
/// Spans are into the file text and cover the argument's own characters, trimmed of the
/// surrounding whitespace — replacing one leaves the commas and layout exactly as written.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroSite {
    pub name: String,
    /// Span of the whole `macros::NAME(...)` expression.
    pub span: Range<usize>,
    pub args: Vec<Range<usize>>,
}

impl MacroSite {
    pub fn arg<'a>(&self, text: &'a str, index: usize) -> Option<&'a str> {
        self.args.get(index).map(|span| &text[span.clone()])
    }
}

/// Every `macros::…` call in `text[range]`, in document order.
///
/// This is a scanner, not a Rust parser: it tracks string literals, character literals, and
/// both comment forms well enough not to be fooled by a macro name inside one, which is all
/// the accuracy an argument-value rewrite needs.
pub fn scan_macro_sites(text: &str, range: Range<usize>) -> Vec<MacroSite> {
    const PREFIX: &str = "macros::";
    let bytes = text.as_bytes();
    let mut sites = Vec::new();
    let mut i = range.start;
    while i < range.end {
        // Skip over anything that is not live code before testing for the prefix.
        if let Some(next) = skip_trivia(text, i) {
            i = next;
            continue;
        }
        if !text[i..range.end].starts_with(PREFIX) {
            i += 1;
            while i < range.end && !text.is_char_boundary(i) {
                i += 1;
            }
            continue;
        }
        let name_start = i + PREFIX.len();
        let mut name_end = name_start;
        while name_end < range.end
            && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_')
        {
            name_end += 1;
        }
        if name_end == name_start || name_end >= range.end || bytes[name_end] != b'(' {
            i = name_end.max(i + 1);
            continue;
        }
        let Some((args, close)) = split_call_args(text, name_end + 1, range.end) else {
            i = name_end + 1;
            continue;
        };
        sites.push(MacroSite {
            name: text[name_start..name_end].to_string(),
            span: i..close + 1,
            args,
        });
        i = close + 1;
    }
    sites
}

/// If `i` starts a comment, string, or char literal, the offset just past it.
///
/// The scanners walk byte by byte, so `i` can land inside a multi-byte character — a
/// non-ASCII identifier in the user's code is enough. A continuation byte can never start
/// any of these, and returning early keeps the `&text[i..]` below from panicking on it.
fn skip_trivia(text: &str, i: usize) -> Option<usize> {
    if !text.is_char_boundary(i) {
        return None;
    }
    let rest = &text[i..];
    if let Some(body) = rest.strip_prefix("//") {
        return Some(i + 2 + body.find('\n').map_or(body.len(), |n| n + 1));
    }
    if let Some(body) = rest.strip_prefix("/*") {
        return Some(match body.find("*/") {
            Some(end) => i + 4 + end,
            None => text.len(),
        });
    }
    if rest.starts_with('"') {
        return Some(i + string_literal_len(rest));
    }
    // A lifetime (`'a`) also starts with a quote, so only treat this as a char literal when
    // it actually closes like one.
    if rest.starts_with('\'') {
        let len = char_literal_len(rest)?;
        return Some(i + len);
    }
    None
}

fn string_literal_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut j = 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

fn char_literal_len(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut j = 1;
    while j < bytes.len() && j <= 4 {
        match bytes[j] {
            b'\\' => j += 2,
            b'\'' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

/// Split a call's arguments starting just after its `(`. Returns the argument spans and the
/// offset of the matching `)`.
fn split_call_args(text: &str, open: usize, limit: usize) -> Option<(Vec<Range<usize>>, usize)> {
    let bytes = text.as_bytes();
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = open;
    let mut i = open;
    while i < limit {
        if let Some(next) = skip_trivia(text, i) {
            i = next;
            continue;
        }
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' if depth == 0 => {
                push_arg(text, start..i, &mut args);
                return Some((args, i));
            }
            // Saturating, because a truncated or malformed call would otherwise underflow
            // and panic rather than simply failing to match.
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                push_arg(text, start..i, &mut args);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Record `span` with its surrounding whitespace trimmed off.
///
/// An empty span is either a call with no arguments or the slot after a trailing comma;
/// neither is an argument, and numbering them would shift every following slot.
fn push_arg(text: &str, span: Range<usize>, args: &mut Vec<Range<usize>>) {
    let raw = &text[span.clone()];
    if raw.trim().is_empty() {
        return;
    }
    let lead = raw.len() - raw.trim_start().len();
    let trail = raw.len() - raw.trim_end().len();
    args.push(span.start + lead..span.end - trail);
}

// ── Project indexing ──────────────────────────────────────────────────────────

/// One ACMD function located in the user's source.
#[derive(Debug, Clone)]
pub struct ScriptSite {
    pub file: PathBuf,
    /// The `unsafe extern "C" fn …` item, from `fn` through its closing brace.
    pub span: Range<usize>,
}

/// Where a fighter's scripts live in the user's project.
#[derive(Debug, Default, Clone)]
pub struct FighterScripts {
    /// ACMD script name (`game_attackairn`, `effect_attackairn`) → its function.
    pub scripts: HashMap<String, ScriptSite>,
}

/// An indexed smashline/smash-script project.
#[derive(Debug, Default, Clone)]
pub struct SourceIndex {
    pub root: PathBuf,
    /// Fighter name as Visionary knows it (`mario`) → its scripts.
    pub fighters: HashMap<String, FighterScripts>,
    /// Files that were read during the scan, for change detection.
    pub files: Vec<PathBuf>,
}

impl SourceIndex {
    /// Walk `root` and index every ACMD function it can attribute to a fighter.
    pub fn build(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            bail!("not a directory: {}", root.display());
        }
        let mut index = SourceIndex {
            root: root.to_path_buf(),
            ..Default::default()
        };
        let mut files = Vec::new();
        collect_rust_files(root, &mut files, 0)?;
        files.sort();

        // Two passes, because the conventional smashline layout splits the two halves of the
        // answer across files: `src/mario/mod.rs` says which fighter this is, and
        // `src/mario/acmd.rs` holds its scripts. Neither file can be indexed alone.
        let sources: Vec<(PathBuf, String)> = files
            .iter()
            .filter_map(|file| Some((file.clone(), std::fs::read_to_string(file).ok()?)))
            .collect();
        let mut by_dir: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let mut project_wide: Vec<String> = Vec::new();
        for (file, text) in &sources {
            let names = scan_fighter_names(text);
            for name in &names {
                if !project_wide.contains(name) {
                    project_wide.push(name.clone());
                }
            }
            if let Some(dir) = file.parent() {
                let entry = by_dir.entry(dir.to_path_buf()).or_default();
                for name in names {
                    if !entry.contains(&name) {
                        entry.push(name);
                    }
                }
            }
        }
        for (file, text) in &sources {
            let neighbours = file
                .parent()
                .and_then(|dir| by_dir.get(dir))
                .map(Vec::as_slice)
                .unwrap_or_default();
            index.index_file(file, text, neighbours, &project_wide);
        }
        index.files = files;
        Ok(index)
    }

    /// Total number of indexed scripts, across every fighter.
    pub fn script_count(&self) -> usize {
        self.fighters.values().map(|f| f.scripts.len()).sum()
    }

    /// The site for one fighter + ACMD script name.
    pub fn script(&self, fighter: &str, script_name: &str) -> Option<&ScriptSite> {
        self.fighters
            .get(&normalize_fighter(fighter))?
            .scripts
            .get(script_name)
    }

    /// Whether this project has anything at all for `fighter`.
    pub fn has_fighter(&self, fighter: &str) -> bool {
        self.fighters.contains_key(&normalize_fighter(fighter))
    }

    /// The `game_*` and `effect_*` functions for a move, concatenated into one body the
    /// existing `acmd::parse_*` functions can read — the same shape the dumped scripts have.
    ///
    /// `None` when the project defines neither, so callers can fall back to the mirror.
    pub fn script_body(&self, fighter: &str, move_name: &str) -> Option<String> {
        let mut body = String::new();
        for prefix in ["game", "effect"] {
            let name = crate::acmd::acmd_script_name(prefix, move_name);
            if let Some(site) = self.script(fighter, &name) {
                if let Ok(text) = std::fs::read_to_string(&site.file) {
                    // The span came from this file's text; a concurrent edit can shrink it.
                    if let Some(source) = text.get(site.span.clone()) {
                        body.push_str(source);
                        body.push_str("\n\n");
                    }
                }
            }
        }
        (!body.is_empty()).then_some(body)
    }

    fn index_file(
        &mut self,
        file: &Path,
        text: &str,
        neighbours: &[String],
        project_wide: &[String],
    ) {
        let functions = scan_acmd_functions(text);
        if functions.is_empty() {
            return;
        }
        // `agent.acmd("game_attackairn", game_attackairn, …)` names a script explicitly, as
        // does a `#[acmd_script(script = "…")]` attribute; a plain `fn game_attackairn` is
        // its own name by convention.
        let mut named: HashMap<&str, &str> = HashMap::new();
        for (script, function) in scan_acmd_registrations(text) {
            named.insert(function, script);
        }

        for function in functions {
            let script_name = function
                .script
                .clone()
                .or_else(|| named.get(function.name.as_str()).map(|s| s.to_string()))
                .or_else(|| {
                    ["game_", "effect_", "sound_", "expression_"]
                        .iter()
                        .any(|prefix| function.name.starts_with(prefix))
                        .then(|| function.name.clone())
                });
            let Some(script_name) = script_name else {
                continue;
            };
            // Narrowest scope that names exactly ONE fighter wins: the function's own
            // attribute, then the directory (`src/mario/mod.rs` + `src/mario/acmd.rs`), then
            // the whole project for a single-character mod. A scope naming several fighters
            // is ambiguous, and guessing would silently attach a move to the wrong one.
            let sole = |names: &[String]| (names.len() == 1).then(|| names[0].clone());
            let Some(fighter) = function
                .fighter
                .clone()
                .or_else(|| sole(neighbours))
                .or_else(|| sole(project_wide))
            else {
                continue;
            };
            self.fighters
                .entry(normalize_fighter(&fighter))
                .or_default()
                .scripts
                .insert(
                    script_name,
                    ScriptSite {
                        file: file.to_path_buf(),
                        span: function.span,
                    },
                );
        }
    }
}

/// Visionary names fighters after the dump folder (`mario`); ACMD attributes and
/// `Agent::new` calls often spell the same character `fighter_mario`.
fn normalize_fighter(name: &str) -> String {
    let name = name.trim().to_ascii_lowercase();
    name.strip_prefix("fighter_").unwrap_or(&name).to_string()
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    // Deep enough for any real project layout, shallow enough that a symlink loop or an
    // accidentally-selected home directory cannot run away.
    if depth > 12 {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_rust_files(&path, out, depth + 1)?;
        } else if kind.is_file()
            && path.extension().is_some_and(|e| e == "rs")
            && entry.metadata().is_ok_and(|m| m.len() <= MAX_SOURCE_BYTES)
        {
            out.push(path);
        }
    }
    Ok(())
}

/// One ACMD function found by [`scan_acmd_functions`].
#[derive(Debug, Clone)]
struct FoundFunction {
    name: String,
    /// Fighter named by a `#[acmd_script(agent = "…")]` attribute on this function, if any.
    fighter: Option<String>,
    /// Script named by that attribute's `script = "…"`. Older smash-script projects give the
    /// function whatever name they like and declare the script here.
    script: Option<String>,
    span: Range<usize>,
}

/// Every `unsafe extern "C" fn NAME(...) { ... }` in `text`, with its body span.
fn scan_acmd_functions(text: &str) -> Vec<FoundFunction> {
    let mut found = Vec::new();
    let mut search = 0;
    while let Some(rel) = text[search..].find("fn ") {
        let fn_kw = search + rel;
        search = fn_kw + 3;
        // Only take free functions declared the way ACMD scripts are; `fn` also appears in
        // trait bounds, closures' types, and ordinary helpers.
        let line_start = text[..fn_kw].rfind('\n').map_or(0, |n| n + 1);
        let prefix = text[line_start..fn_kw].trim();
        if !prefix.contains("extern") {
            continue;
        }
        let name_start = fn_kw + 3;
        let name_end = name_start
            + text[name_start..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(0);
        if name_end == name_start {
            continue;
        }
        let Some(body_start) = text[name_end..].find('{').map(|n| name_end + n) else {
            continue;
        };
        let Some(body_end) = match_brace(text, body_start) else {
            continue;
        };
        let (fighter, script) = attribute_values(text, line_start);
        found.push(FoundFunction {
            name: text[name_start..name_end].to_string(),
            fighter,
            script,
            span: line_start..body_end + 1,
        });
        search = body_end + 1;
    }
    found
}

/// Walk from an opening `{` to its match, ignoring braces inside strings and comments.
fn match_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < text.len() {
        if let Some(next) = skip_trivia(text, i) {
            i = next;
            continue;
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `(agent, script)` from an `#[acmd_script(...)]` attribute above `line_start`.
fn attribute_values(text: &str, line_start: usize) -> (Option<String>, Option<String>) {
    // Attributes sit on the lines immediately above the declaration; scan back over them.
    let mut cursor = line_start;
    for _ in 0..8 {
        let prev_start = text[..cursor]
            .trim_end_matches('\n')
            .rfind('\n')
            .map_or(0, |n| n + 1);
        let line = text[prev_start..cursor].trim();
        if line.starts_with("#[") {
            let agent = attr_string_value(line, "agent");
            let script = attr_string_value(line, "script");
            if agent.is_some() || script.is_some() {
                return (agent, script);
            }
        } else if !line.is_empty() {
            return (None, None);
        }
        if prev_start == 0 {
            return (None, None);
        }
        cursor = prev_start;
    }
    (None, None)
}

/// `key = "value"` out of an attribute body.
///
/// The key has to be a whole word followed by `=`: `#[acmd_script(agent = "…")]` contains
/// the substring `script` inside its own name, and matching that read the agent as the
/// script name.
fn attr_string_value(line: &str, key: &str) -> Option<String> {
    let mut search = 0;
    while let Some(rel) = line[search..].find(key) {
        let at = search + rel;
        search = at + key.len();
        let preceded_by_word = line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if preceded_by_word {
            continue;
        }
        let Some(rest) = line[search..].trim_start().strip_prefix('=') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Fighter names a file declares, via `Agent::new("…")` or an `agent = "…"` attribute.
fn scan_fighter_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: String| {
        let name = normalize_fighter(&name);
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    };
    for marker in ["Agent::new(", "Agent::new_with_weapon("] {
        let mut search = 0;
        while let Some(rel) = text[search..].find(marker) {
            let at = search + rel + marker.len();
            search = at;
            let rest = text[at..].trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    push(rest[..end].to_string());
                }
            }
        }
    }
    let mut search = 0;
    while let Some(rel) = text[search..].find("agent") {
        let at = search + rel;
        search = at + 5;
        let line_end = text[at..].find('\n').map_or(text.len(), |n| at + n);
        if let Some(value) = attr_string_value(&text[at..line_end], "agent") {
            push(value);
        }
    }
    names
}

/// `(script_name, function_name)` for every `agent.acmd("…", fn, …)`-style registration, and
/// for `#[acmd_script(script = "…")]` attributes.
fn scan_acmd_registrations(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    for marker in [
        ".acmd(",
        ".game_acmd(",
        ".effect_acmd(",
        "install_acmd_script!(",
    ] {
        let mut search = 0;
        while let Some(rel) = text[search..].find(marker) {
            let open = search + rel + marker.len() - 1;
            search = open + 1;
            let Some((args, _)) = split_call_args(text, open + 1, text.len()) else {
                continue;
            };
            // (script, function) for `.acmd(…)`; the bang form leads with the agent.
            let (script, function) = match (marker, args.len()) {
                ("install_acmd_script!(", 3..) => (&args[1], &args[2]),
                (_, 2..) => (&args[0], &args[1]),
                _ => continue,
            };
            let script = text[script.clone()].trim().trim_matches('"');
            let function = text[function.clone()].trim();
            if !script.is_empty() && is_identifier(function) {
                out.push((script, function));
            }
        }
    }
    out
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !value.starts_with(|c: char| c.is_ascii_digit())
}

// ── Writing edits back ────────────────────────────────────────────────────────

/// What a sync did, and what it declined to do.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SyncReport {
    /// Argument values rewritten.
    pub changed: usize,
    /// Files whose contents actually differ afterwards.
    pub files: Vec<PathBuf>,
    /// Edits that could not be expressed as a value change, each with the reason.
    pub skipped: Vec<String>,
}

/// A pending value replacement: span in the file, and the text to put there.
#[derive(Debug, Clone)]
struct Replacement {
    span: Range<usize>,
    value: String,
}

/// Rewrite `text` with every replacement applied. Spans must not overlap.
fn apply(text: &str, mut edits: Vec<Replacement>) -> String {
    edits.sort_by_key(|e| e.span.start);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for edit in edits {
        if edit.span.start < cursor {
            continue; // overlapping — the earlier edit wins
        }
        out.push_str(&text[cursor..edit.span.start]);
        out.push_str(&edit.value);
        cursor = edit.span.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Format a float the way ACMD source spells one, and only if it actually differs.
///
/// Rewriting `1.0` to `1.0000` on every sync would churn the whole file and bury the real
/// change in the diff, so a value that round-trips to the same number is left alone.
fn float_edit(text: &str, span: &Range<usize>, new: f32) -> Option<Replacement> {
    let current = text[span.clone()].trim();
    // A const or expression, not a literal — replacing it would discard the author's intent,
    // which is exactly what this path promises not to do.
    let old = current.parse::<f32>().ok()?;
    let replacement = format_float(new);
    // Two ways there is nothing to write, and both matter. The value being unchanged is the
    // obvious one. The second is that a written value only carries four decimals, so a float
    // finer than that — a third, a dragged slider — never compares equal to what was written
    // for it. Asking "would writing this change the text?" instead of "are these floats
    // equal?" is what lets the source editor and the panels settle against each other rather
    // than each seeing the other's value as a fresh edit, forever.
    if old == new || replacement == current {
        return None;
    }
    Some(Replacement {
        span: span.clone(),
        value: replacement,
    })
}

/// Format a number for a `ToF32` slot exactly as the emitter would, and only if it differs.
///
/// `f32::to_string` is both exact and shortest, so a wind argument written `16` stays `16` and
/// only a value that genuinely needs decimals grows them. The same two-part guard as
/// [`float_edit`]: an unchanged value writes nothing, and neither does one whose spelling the
/// author already chose.
fn to_f32_edit(text: &str, span: &Range<usize>, new: f32) -> Option<Replacement> {
    let current = text[span.clone()].trim();
    let old = current.parse::<f32>().ok()?;
    let replacement = new.to_string();
    if old == new || replacement == current {
        return None;
    }
    Some(Replacement {
        span: span.clone(),
        value: replacement,
    })
}

fn int_edit(text: &str, span: &Range<usize>, new: i64) -> Option<Replacement> {
    let current = text[span.clone()].trim();
    let old = current.parse::<i64>().ok()?;
    (old != new).then(|| Replacement {
        span: span.clone(),
        value: new.to_string(),
    })
}

/// `2.0`, `0.75`, `-1.5` — always with a decimal point, since these slots are floats.
fn format_float(value: f32) -> String {
    let mut text = format!("{value:.4}");
    while text.ends_with('0') && !text.ends_with(".0") {
        text.pop();
    }
    text
}

/// Rewrite one `effect_*` function's source with the editor's edited transform values.
///
/// Pure: `text` is the function on its own, and the result is that function rewritten. This
/// is the shared core of both write-back routes — the explicit sync, which splices the result
/// back into the project file, and the source editor window, which is already holding exactly
/// this text and simply swaps it.
///
/// Only position, rotation, and scale move. A call that was added, removed, renamed, retimed,
/// or pointed at a different joint cannot be expressed as a value change to an existing line,
/// so it lands in `skipped` rather than being approximated.
pub fn rewrite_effect_calls(
    text: &str,
    label: &str,
    pristine: &[crate::data::EffectCall],
    edited: &[crate::data::EffectCall],
) -> Result<(String, SyncReport)> {
    let ordinals = crate::acmd::parse_effect_script(text).call_macro_ordinals();
    let (sites, rate_sites) = spawn_and_rate_sites(text);

    let mut report = SyncReport::default();
    if ordinals.len() != pristine.len() {
        bail!(
            "{label}: the editor holds {} spawns but the source parses to {} — reload the \
             move from source before syncing",
            pristine.len(),
            ordinals.len()
        );
    }

    // A macro inside a `for` produces one call per iteration off ONE line of text. Only sync
    // it when every iteration agrees, so an edit to a single iteration is refused instead of
    // silently rewriting all of them.
    let mut per_site: HashMap<usize, Vec<usize>> = HashMap::new();
    for (call_index, ordinal) in ordinals.iter().enumerate() {
        per_site.entry(*ordinal).or_default().push(call_index);
    }

    let mut edits: Vec<Replacement> = Vec::new();
    if edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: {} spawn(s) added or removed — source syncing only retunes existing calls",
            edited.len().abs_diff(pristine.len())
        ));
    }

    for (ordinal, call_indices) in per_site {
        let Some(macro_site) = sites.get(ordinal) else {
            report.skipped.push(format!(
                "{label}: spawn #{ordinal} has no matching line in the source"
            ));
            continue;
        };
        // Any difference at all, not just a transform one. Selecting on the transform alone
        // meant an edit that changed only the graphic, the joint, the frame, or the disabled
        // flag was neither written nor reported: the user renamed an effect, pressed Save,
        // and the source silently kept the old name with the report claiming success.
        let differs: Vec<usize> = call_indices
            .iter()
            .copied()
            .filter(|i| edited.get(*i).is_some_and(|e| &pristine[*i] != e))
            .collect();
        if differs.is_empty() {
            continue;
        }
        if differs
            .iter()
            .any(|i| !identity_matches(&pristine[*i], &edited[*i]))
        {
            report.skipped.push(format!(
                "{label}: `{}` changed graphic, joint, timing, or enablement — source syncing \
                 only rewrites transform values",
                macro_site.name
            ));
            continue;
        }
        // A trail carries textures and per-frame trail parameters, and no transform at all —
        // its joints are arguments 4 onward. Applying the spawn layout here overwrote the
        // user's own trail settings with position values and reported it as a clean success.
        if is_trail_macro(&macro_site.name) {
            report.skipped.push(format!(
                "{label}: `{}` has no position, rotation, or scale arguments to write — a \
                 trail is placed by the joints it names, not by a transform",
                macro_site.name
            ));
            continue;
        }
        let target = &edited[differs[0]];
        if differs.len() != call_indices.len()
            || differs
                .iter()
                .any(|i| !transform_matches(&edited[*i], target))
        {
            report.skipped.push(format!(
                "{label}: `{}` on one loop iteration differs from the others — a loop body is \
                 a single line of source and cannot hold per-iteration values",
                macro_site.name
            ));
            continue;
        }
        edits.extend(transform_edits(text, macro_site, target));

        // The rate lives on its own line, so turning one on or off is a call added or
        // removed — structural, and reported. Only retuning an existing one is a value edit.
        let was = pristine[differs[0]].rate;
        if was != target.rate {
            match (was, target.rate, rate_sites.get(ordinal).and_then(Option::as_ref)) {
                (Some(_), Some(now), Some(rate_site)) => {
                    if let Some(span) = rate_site.args.get(1) {
                        edits.extend(to_f32_edit(text, span, now));
                    }
                }
                (None, Some(_), _) => report.skipped.push(format!(
                    "{label}: `{}` gained a rate — that is a new LAST_EFFECT_SET_RATE line, \
                     which source syncing does not add",
                    macro_site.name
                )),
                (Some(_), None, _) => report.skipped.push(format!(
                    "{label}: `{}` lost its rate — source syncing does not delete the \
                     LAST_EFFECT_SET_RATE line that sets it",
                    macro_site.name
                )),
                (Some(_), Some(_), None) => report.skipped.push(format!(
                    "{label}: `{}` has a rate in the editor but no LAST_EFFECT_SET_RATE \
                     directly beneath it in the source — reload the move from source",
                    macro_site.name
                )),
                (None, None, _) => {}
            }
        }
    }

    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited effect calls for one move back into the project source on disk.
pub fn sync_effect_calls(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::EffectCall],
    edited: &[crate::data::EffectCall],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("effect", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_effect_calls(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// Read one indexed script, hand its text to `rewrite`, and splice the result back in.
///
/// The rewrite only ever sees the function it is meant to change, so nothing else in the
/// file — other moves, imports, the install block — can be disturbed by it.
fn sync_script(
    index: &SourceIndex,
    fighter: &str,
    script_name: &str,
    rewrite: impl FnOnce(&str) -> Result<(String, SyncReport)>,
) -> Result<SyncReport> {
    let Some(site) = index.script(fighter, script_name) else {
        bail!("{fighter}: the project has no {script_name} to sync into");
    };
    let text = std::fs::read_to_string(&site.file)
        .with_context(|| format!("reading {}", site.file.display()))?;
    let Some(body) = text.get(site.span.clone()) else {
        bail!(
            "{} changed on disk since it was indexed — rescan and try again",
            site.file.display()
        );
    };
    let (updated_body, mut report) = rewrite(body)?;
    if updated_body == body {
        return Ok(report);
    }
    let mut updated = String::with_capacity(text.len());
    updated.push_str(&text[..site.span.start]);
    updated.push_str(&updated_body);
    updated.push_str(&text[site.span.end..]);
    std::fs::write(&site.file, &updated)
        .with_context(|| format!("writing {}", site.file.display()))?;
    report.files.push(site.file.clone());
    Ok(report)
}

/// The spawn calls in `text`, in ordinal order, each paired with its `LAST_EFFECT_SET_RATE`
/// line if it has one.
///
/// The rate macro names no effect, so the only thing tying it to a spawn is that it comes
/// directly after one — the same rule `eval_effect_stmts` uses to fill `EffectCall::rate`,
/// and the two must agree or a value would be read off one call and written into another.
/// Anything at all between them, including a macro this scanner does not recognise, breaks
/// the pairing rather than reaching further back for a spawn to claim.
fn spawn_and_rate_sites(text: &str) -> (Vec<MacroSite>, Vec<Option<MacroSite>>) {
    let mut spawns: Vec<MacroSite> = Vec::new();
    let mut rates: Vec<Option<MacroSite>> = Vec::new();
    let mut adjacent = false;
    for site in scan_macro_sites(text, 0..text.len()) {
        if is_spawn_macro(&site.name) {
            // A trail is a spawn for ordinal purposes but never anchors a rate, matching the
            // parser — see the `AfterImage` arm of `eval_effect_stmts`.
            adjacent = !is_trail_macro(&site.name);
            spawns.push(site);
            rates.push(None);
            continue;
        }
        if site.name == "LAST_EFFECT_SET_RATE" {
            if adjacent {
                if let Some(slot) = rates.last_mut() {
                    // A second rate line overwrites the first, because in game the later call
                    // wins and that is the value the parser will have read.
                    *slot = Some(site);
                }
            }
            continue;
        }
        adjacent = false;
    }
    (spawns, rates)
}

/// Whether a scanned macro name is one of the spawn families `call_macro_ordinals` counts.
fn is_spawn_macro(name: &str) -> bool {
    crate::acmd::is_effect_spawn_macro(name) || is_trail_macro(name)
}

/// Whether a scanned macro name starts an AFTER_IMAGE trail.
///
/// Trails produce an `EffectCall` like any other spawn, so they have to be counted here to
/// keep call ordinals aligned with the source — but their arguments share none of the spawn
/// layout, so nothing may be written into them positionally.
fn is_trail_macro(name: &str) -> bool {
    name.starts_with("AFTER_IMAGE4_ON") || name == "AFTER_IMAGE_ON"
}

/// Everything a value rewrite CAN change about a call — the test for whether every iteration
/// of a loop body agrees, since they all come off one line of source.
///
/// `rate` counts: it is written back from its own line, but that line is inside the loop body
/// too, so per-iteration rates are no more expressible than per-iteration positions.
fn transform_matches(a: &crate::data::EffectCall, b: &crate::data::EffectCall) -> bool {
    a.offset == b.offset && a.rotation == b.rotation && a.scale == b.scale && a.rate == b.rate
}

/// Everything a value rewrite CANNOT change about a call.
///
/// `active_end` belongs here with `active_start`: a follow effect's end frame is the
/// `EFFECT_OFF_KIND` that closes it, so moving it means moving a different call in a
/// different frame block, not retuning an argument.
fn identity_matches(a: &crate::data::EffectCall, b: &crate::data::EffectCall) -> bool {
    a.effect_name.eq_ignore_ascii_case(&b.effect_name)
        && a.effect_name_alt == b.effect_name_alt
        && a.bone_name.eq_ignore_ascii_case(&b.bone_name)
        && a.spawn_func == b.spawn_func
        && a.active_start == b.active_start
        && a.active_end == b.active_end
        && a.disabled == b.disabled
}

/// Replacements for one spawn's position, rotation, and scale arguments.
fn transform_edits(
    text: &str,
    site: &MacroSite,
    call: &crate::data::EffectCall,
) -> Vec<Replacement> {
    // agent, graphic[, flipped graphic], joint, x, y, z, zr, yr, xr, size — the same layout
    // the parser reads, including the reversed rotation slots.
    let off = usize::from(call.effect_name_alt.is_some());
    let mut edits = Vec::new();
    let slots: [(usize, f32); 7] = [
        (3 + off, call.offset[0]),
        (4 + off, call.offset[1]),
        (5 + off, call.offset[2]),
        (6 + off, call.rotation[2]),
        (7 + off, call.rotation[1]),
        (8 + off, call.rotation[0]),
        (9 + off, call.scale),
    ];
    for (slot, value) in slots {
        if let Some(span) = site.args.get(slot) {
            edits.extend(float_edit(text, span, value));
        }
    }
    edits
}

/// Rewrite one `game_*` function's source with the editor's edited hitbox values.
///
/// Pure, and the hitbox counterpart of [`rewrite_effect_calls`]. Same contract: existing
/// `ATTACK` arguments are retuned, and anything structural is reported instead.
pub fn rewrite_hitboxes(
    text: &str,
    label: &str,
    pristine: &[crate::data::Hitbox],
    edited: &[crate::data::Hitbox],
) -> Result<(String, SyncReport)> {
    // Each collision family is matched only against its own calls. They share the id space
    // but not the argument layout — slot 4 is damage in `ATTACK` and size in `CATCH` — so a
    // single candidate set meant a hitbox matched by id alone and had another family's values
    // written straight into it.
    let sites = scan_macro_sites(text, 0..text.len());
    let attacks: Vec<&MacroSite> = sites
        .iter()
        .filter(|s| crate::acmd::ATTACK_FUNCS.contains(&s.name.as_str()))
        .collect();
    let catches: Vec<&MacroSite> = sites.iter().filter(|s| s.name == "CATCH").collect();
    let winds: Vec<&MacroSite> = sites
        .iter()
        .filter(|s| crate::data::is_wind_command(&s.name))
        .collect();

    let mut report = SyncReport::default();
    let mut edits = Vec::new();
    // Hitboxes are keyed by id + part, which is what the game uses to tell them apart and
    // what stays stable across a retime. Two calls sharing a key in one script would be
    // ambiguous, so those are reported rather than guessed at.
    for (position, hitbox) in edited.iter().enumerate() {
        let Some(before) = pristine.get(position) else {
            report.skipped.push(format!(
                "{label}: hitbox {} was added — source syncing only retunes existing calls",
                hitbox.id
            ));
            continue;
        };
        if before == hitbox {
            continue;
        }
        // A wind area shares nothing with `ATTACK` but the id space, so it is matched against
        // its own calls and retuned through its own slot table.
        if hitbox.category == 2 || hitbox.wind.is_some() || before.wind.is_some() {
            edits.extend(wind_box_edits(
                text,
                label,
                &winds,
                before,
                hitbox,
                &mut report,
            ));
            continue;
        }
        if before.id != hitbox.id || before.part != hitbox.part {
            report.skipped.push(format!(
                "{label}: hitbox {} was renumbered — source syncing only rewrites argument \
                 values",
                before.id
            ));
            continue;
        }
        // Swapping the family member is a different macro, not a different value. Rewriting
        // the call name in place would also have to add or drop the capsule triple, which is
        // structure — so it is reported, and the export path is where that swap can land.
        if before.func != hitbox.func {
            report.skipped.push(format!(
                "{label}: hitbox {} changed from `{}` to `{}` — source syncing rewrites \
                 argument values, not the macro being called",
                before.id, before.func, hitbox.func
            ));
            continue;
        }
        // `CATCH` takes no `part`, so a grab box is keyed on its id alone.
        let is_grab = hitbox.category == 1;
        let matching: Vec<&MacroSite> = if is_grab {
            catches
                .iter()
                .copied()
                .filter(|site| {
                    site.arg(text, 1).and_then(|a| a.trim().parse::<u32>().ok()) == Some(before.id)
                })
                .collect()
        } else {
            attacks
                .iter()
                .copied()
                .filter(|site| {
                    // Family members share the id space but are not the same call, so an
                    // `ATTACK` hitbox never retunes an `ATTACK_IGNORE_THROW` beside it.
                    site.name == before.func
                        && site.arg(text, 1).and_then(|a| a.trim().parse::<u32>().ok())
                            == Some(before.id)
                        && site.arg(text, 2).and_then(|a| a.trim().parse::<u32>().ok())
                            == Some(before.part)
                })
                .collect()
        };
        let [macro_site] = matching[..] else {
            report.skipped.push(format!(
                "{label}: hitbox {} matches {} calls in the source — cannot tell which one to \
                 retune",
                before.id,
                matching.len()
            ));
            continue;
        };
        // A hitbox's frames are the `frame(...)` block it sits in and the `ATTACK_CLEAR` that
        // ends it, not arguments of the call — so a retime is reported. The values it also
        // changed are still worth writing, so this does not skip the call.
        if before.active_start != hitbox.active_start || before.active_end != hitbox.active_end {
            report.skipped.push(format!(
                "{label}: hitbox {} was retimed — its frames are the block it sits in, not \
                 arguments, so source syncing cannot move it",
                before.id
            ));
        }
        let (call_edits, missing) = if is_grab {
            catch_edits(text, macro_site, before, hitbox)
        } else {
            attack_edits(text, macro_site, before, hitbox)
        };
        if !missing.is_empty() {
            report.skipped.push(format!(
                "{label}: hitbox {} changed {}, but its `{}` call in the source is too short to \
                 have those arguments",
                before.id,
                missing.join(", "),
                macro_site.name
            ));
        }
        edits.extend(call_edits);
    }

    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited hitboxes for one move back into the project source on disk.
pub fn sync_hitboxes(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::Hitbox],
    edited: &[crate::data::Hitbox],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_hitboxes(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The new text for one changed `ATTACK` argument, by the kind of slot it is.
enum ArgValue {
    Float(f32),
    Int(i64),
    /// A number for a slot that is generic over `ToF32` — every `AREA_WIND_2ND*` argument is.
    /// Written the way the emitter writes one, `20` rather than `20.0`, so the two export paths
    /// put the same text in the file and a whole number does not sprout a decimal point.
    ToF32(f32),
    /// A const, a bool, a hash, or a capsule endpoint — compared and written as text.
    Text(String),
}

/// Replacements for the arguments of one `ATTACK` call that the user actually changed.
///
/// Returns the edits plus the names of any changed field whose slot the call does not have,
/// so a shortened `ATTACK` cannot swallow an edit silently.
///
/// Only *differing* fields are written. A blanket rewrite of every slot would churn calls
/// nobody touched, because the editor holds these values decoded: the parser turns a bare
/// `1` into `*ATTACK_LR_CHECK_F`, and writing the editor's spelling back unconditionally
/// would restyle every hitbox in the file on the first sync.
fn attack_edits(
    text: &str,
    site: &MacroSite,
    before: &crate::data::Hitbox,
    after: &crate::data::Hitbox,
) -> (Vec<Replacement>, Vec<&'static str>) {
    let bone = format!("Hash40::new(\"{}\")", after.bone_name.to_ascii_lowercase());
    // Live-captured hitboxes carry the collision attr as a raw hash — same rule as emit_attack.
    let collision_attr = match after.collision_attr.strip_prefix("0x") {
        Some(hex) => format!("Hash40::new_raw(0x{hex})"),
        None => format!("Hash40::new(\"{}\")", after.collision_attr),
    };
    let capsule = |axis: usize| match after.capsule_end {
        Some(end) => format!("Some({:.1})", end[axis]),
        None => "None".to_string(),
    };
    let konst = crate::acmd::const_expr;

    // Slots follow macros::ATTACK, the same numbering `parse_attack_call` documents and
    // `emit_attack` writes. Every argument the hitbox panels expose appears here, so an edit
    // either lands in the source or is named in the report — it can no longer just vanish.
    // Slots 1 and 2 (id, part) are absent on purpose: they identify which call this is, and
    // `rewrite_hitboxes` reports a renumber rather than matching by them and then changing
    // them out from under itself.
    let slots: [(usize, &'static str, bool, ArgValue); 34] = [
        (
            3,
            "bone",
            before.bone_name != after.bone_name,
            ArgValue::Text(bone),
        ),
        (
            4,
            "damage",
            before.damage != after.damage,
            ArgValue::Float(after.damage),
        ),
        (
            5,
            "angle",
            before.angle != after.angle,
            ArgValue::Int(after.angle as i64),
        ),
        (
            6,
            "knockback scaling",
            before.kb_scaling != after.kb_scaling,
            ArgValue::Int(after.kb_scaling as i64),
        ),
        (
            7,
            "fixed knockback",
            before.fkb != after.fkb,
            ArgValue::Int(after.fkb as i64),
        ),
        (
            8,
            "base knockback",
            before.kb_base != after.kb_base,
            ArgValue::Int(after.kb_base as i64),
        ),
        (
            9,
            "size",
            before.size != after.size,
            ArgValue::Float(after.size),
        ),
        (
            10,
            "x offset",
            before.offset_x != after.offset_x,
            ArgValue::Float(after.offset_x),
        ),
        (
            11,
            "y offset",
            before.offset_y != after.offset_y,
            ArgValue::Float(after.offset_y),
        ),
        (
            12,
            "z offset",
            before.offset_z != after.offset_z,
            ArgValue::Float(after.offset_z),
        ),
        (
            13,
            "capsule end",
            before.capsule_end != after.capsule_end,
            ArgValue::Text(capsule(0)),
        ),
        (
            14,
            "capsule end",
            before.capsule_end != after.capsule_end,
            ArgValue::Text(capsule(1)),
        ),
        (
            15,
            "capsule end",
            before.capsule_end != after.capsule_end,
            ArgValue::Text(capsule(2)),
        ),
        (
            16,
            "hitlag multiplier",
            before.hitlag_mult != after.hitlag_mult,
            ArgValue::Float(after.hitlag_mult),
        ),
        (
            17,
            "SDI multiplier",
            before.sdi_mult != after.sdi_mult,
            ArgValue::Float(after.sdi_mult),
        ),
        (
            18,
            "set-off kind",
            before.setoff_kind != after.setoff_kind,
            ArgValue::Text(konst(&after.setoff_kind)),
        ),
        (
            19,
            "facing check",
            before.lr_check != after.lr_check,
            ArgValue::Text(konst(&after.lr_check)),
        ),
        (
            20,
            "clang",
            before.is_clang != after.is_clang,
            ArgValue::Text(after.is_clang.to_string()),
        ),
        (
            21,
            "add attack",
            before.is_add_attack != after.is_add_attack,
            ArgValue::Int(after.is_add_attack as i64),
        ),
        (
            22,
            "hitbox attribute",
            before.hitbox_attr != after.hitbox_attr,
            ArgValue::Float(after.hitbox_attr),
        ),
        (
            23,
            "ground/air",
            before.ground_or_air != after.ground_or_air,
            ArgValue::Int(after.ground_or_air as i64),
        ),
        (
            24,
            "meteor",
            before.is_mtk != after.is_mtk,
            ArgValue::Text(after.is_mtk.to_string()),
        ),
        (
            25,
            "shield disable",
            before.is_shield_disable != after.is_shield_disable,
            ArgValue::Text(after.is_shield_disable.to_string()),
        ),
        (
            26,
            "reflectable",
            before.is_reflectable != after.is_reflectable,
            ArgValue::Text(after.is_reflectable.to_string()),
        ),
        (
            27,
            "absorbable",
            before.is_absorbable != after.is_absorbable,
            ArgValue::Text(after.is_absorbable.to_string()),
        ),
        (
            28,
            "landing attack",
            before.is_landing_attack != after.is_landing_attack,
            ArgValue::Text(after.is_landing_attack.to_string()),
        ),
        (
            29,
            "situation mask",
            before.situation_mask != after.situation_mask,
            ArgValue::Text(konst(&after.situation_mask)),
        ),
        (
            30,
            "category mask",
            before.category_mask != after.category_mask,
            ArgValue::Text(konst(&after.category_mask)),
        ),
        (
            31,
            "part mask",
            before.part_mask != after.part_mask,
            ArgValue::Text(konst(&after.part_mask)),
        ),
        (
            32,
            "finish camera",
            before.no_finish_camera != after.no_finish_camera,
            ArgValue::Text(after.no_finish_camera.to_string()),
        ),
        (
            33,
            "collision attribute",
            before.collision_attr != after.collision_attr,
            ArgValue::Text(collision_attr),
        ),
        (
            34,
            "sound level",
            before.sound_level != after.sound_level,
            ArgValue::Text(konst(&after.sound_level)),
        ),
        (
            35,
            "sound attribute",
            before.sound_attr != after.sound_attr,
            ArgValue::Text(konst(&after.sound_attr)),
        ),
        (
            36,
            "attack region",
            before.attack_region != after.attack_region,
            ArgValue::Text(konst(&after.attack_region)),
        ),
    ];

    apply_slots(text, site, &shift_past_absent_capsule(text, site, slots))
}

/// Re-aim an `ATTACK` slot table at a call written without the optional capsule triple.
///
/// The archive writes `ATTACK_IGNORE_THROW` with 33 arguments where `ATTACK` has 36; every
/// slot past the transform then sits three earlier. Without this, retuning such a call wrote
/// the hitlag multiplier into `z`, the setoff kind into hitlag, and so on down the line —
/// well-formed source, silently the wrong move.
///
/// The three capsule slots are aimed past the end of the call instead of being dropped, so a
/// capsule edit on a call that has nowhere to put one is *reported* by `apply_slots` rather
/// than discarded.
fn shift_past_absent_capsule<const N: usize>(
    text: &str,
    site: &MacroSite,
    slots: [(usize, &'static str, bool, ArgValue); N],
) -> Vec<(usize, &'static str, bool, ArgValue)> {
    let optionish = |index: usize| {
        site.arg(text, index)
            .map(|a| a.trim())
            .is_some_and(|a| a == "None" || a.starts_with("Some("))
    };
    if site.args.len() >= 16 && (13..16).all(optionish) {
        return slots.into_iter().collect();
    }
    slots
        .into_iter()
        .map(|(slot, field, changed, value)| match slot {
            13..=15 => (usize::MAX, field, changed, value),
            16.. => (slot - 3, field, changed, value),
            _ => (slot, field, changed, value),
        })
        .collect()
}

/// Write each changed slot, and name any whose argument the call does not have.
///
/// A call shorter than its slot table is not an error — scripts do use the shorter macro
/// forms — but an edit that lands nowhere has to be reported rather than dropped.
fn apply_slots(
    text: &str,
    site: &MacroSite,
    slots: &[(usize, &'static str, bool, ArgValue)],
) -> (Vec<Replacement>, Vec<&'static str>) {
    let mut edits = Vec::new();
    let mut missing = Vec::new();
    for (slot, field, changed, value) in slots {
        if !changed {
            continue;
        }
        let Some(span) = site.args.get(*slot) else {
            if !missing.contains(field) {
                missing.push(*field);
            }
            continue;
        };
        let edit = match value {
            ArgValue::Float(v) => float_edit(text, span, *v),
            ArgValue::Int(v) => int_edit(text, span, *v),
            ArgValue::ToF32(v) => to_f32_edit(text, span, *v),
            ArgValue::Text(v) => text_edit(text, span, v),
        };
        edits.extend(edit);
    }
    (edits, missing)
}

/// Replacements for the arguments of one `CATCH` call that the user actually changed.
///
/// `CATCH` numbers its arguments differently from `ATTACK` and has no `part`, no damage, and
/// no knockback — a grab box's editable properties are its joint, its size, its offsets, and
/// its capsule endpoint. Status and situation are not editable, so they are never written.
fn catch_edits(
    text: &str,
    site: &MacroSite,
    before: &crate::data::Hitbox,
    after: &crate::data::Hitbox,
) -> (Vec<Replacement>, Vec<&'static str>) {
    let bone = format!("Hash40::new(\"{}\")", after.bone_name.to_ascii_lowercase());
    let capsule = |axis: usize| match after.capsule_end {
        Some(end) => format!("Some({:.1})", end[axis]),
        None => "None".to_string(),
    };
    // agent, id, bone, size, x, y, z, x2, y2, z2, status, situation.
    let slots: [(usize, &'static str, bool, ArgValue); 8] = [
        (
            2,
            "bone",
            before.bone_name != after.bone_name,
            ArgValue::Text(bone),
        ),
        (
            3,
            "size",
            before.size != after.size,
            ArgValue::Float(after.size),
        ),
        (
            4,
            "x offset",
            before.offset_x != after.offset_x,
            ArgValue::Float(after.offset_x),
        ),
        (
            5,
            "y offset",
            before.offset_y != after.offset_y,
            ArgValue::Float(after.offset_y),
        ),
        (
            6,
            "z offset",
            before.offset_z != after.offset_z,
            ArgValue::Float(after.offset_z),
        ),
        (
            7,
            "capsule end",
            before.capsule_end != after.capsule_end,
            ArgValue::Text(capsule(0)),
        ),
        (
            8,
            "capsule end",
            before.capsule_end != after.capsule_end,
            ArgValue::Text(capsule(1)),
        ),
        (
            9,
            "capsule end",
            before.capsule_end != after.capsule_end,
            ArgValue::Text(capsule(2)),
        ),
    ];
    apply_slots(text, site, &slots)
}

/// Match one wind area to its `AREA_WIND_2ND*` call and retune the arguments that changed.
///
/// Wind is a family in the strongest sense. Every argument is a bare float, so unlike `ATTACK`
/// there is no argument *shape* to recognise a layout from — the command name is the layout.
/// The four commands share slots 0..=7 and nothing else, so a rectangular call is only ever
/// matched against a rectangular one, and a width is never written where a radius goes.
///
/// Anything that is not a value change to an existing argument — a different command, a
/// different argument count, a renumber — is pushed to `report` and nothing is written.
fn wind_box_edits(
    text: &str,
    label: &str,
    winds: &[&MacroSite],
    before: &crate::data::Hitbox,
    after: &crate::data::Hitbox,
    report: &mut SyncReport,
) -> Vec<Replacement> {
    let (Some(was), Some(now)) = (before.wind.as_ref(), after.wind.as_ref()) else {
        report.skipped.push(format!(
            "{label}: wind box {} carries no `AREA_WIND` payload to retune — fetch the move again",
            before.id
        ));
        return Vec::new();
    };
    // The command is the layout, so swapping it is a different call, not a different value.
    if was.command != now.command || was.args.len() != now.args.len() {
        report.skipped.push(format!(
            "{label}: wind box {} changed from `{}` to `{}` — source syncing rewrites argument \
             values, not the macro being called",
            before.id, was.command, now.command
        ));
        return Vec::new();
    }
    if before.id != after.id || was.id() != now.id() {
        report.skipped.push(format!(
            "{label}: wind box {} was renumbered — source syncing only rewrites argument values",
            before.id
        ));
        return Vec::new();
    }
    let matching: Vec<&MacroSite> = winds
        .iter()
        .copied()
        .filter(|site| {
            site.name == was.command
                && site
                    .arg(text, 1)
                    .and_then(|a| a.trim().parse::<f32>().ok())
                    .map(|id| id.max(0.0) as u32)
                    == Some(was.id())
        })
        .collect();
    let [site] = matching[..] else {
        report.skipped.push(format!(
            "{label}: wind box {} matches {} `{}` calls in the source — cannot tell which one to \
             retune",
            before.id,
            matching.len(),
            was.command
        ));
        return Vec::new();
    };
    // `agent` occupies argument 0, so the call must be exactly one longer than the payload.
    // A shorter or longer one is a call this editor does not understand; retuning it by
    // position would write into whatever the author actually put there.
    if site.args.len() != was.args.len() + 1 {
        report.skipped.push(format!(
            "{label}: the `{}` call for wind box {} has {} arguments, not the {} that command \
             takes — source syncing will not retune it",
            was.command,
            before.id,
            site.args.len().saturating_sub(1),
            was.args.len()
        ));
        return Vec::new();
    }
    // A wind area's start frame is the block it sits in, and is never an argument. Its end
    // frame is the lifetime argument when the command has that slot — which this does rewrite,
    // so an end the payload now accounts for is a value edit and not a retime. The shorter
    // forms have no such slot and are ended by an `AreaModule::erase_wind` on another line, so
    // there the only end that can be written is the one already there.
    let end_moved = if now.has_lifetime() {
        after.active_end != now.end_frame(after.active_start)
    } else {
        after.active_end != before.active_end
    };
    if before.active_start != after.active_start || end_moved {
        report.skipped.push(format!(
            "{label}: wind box {} was retimed — its start is the block it sits in and its end is \
             an `erase_wind`, so source syncing cannot move it",
            before.id
        ));
    }
    let names: &[&'static str] = if now.is_radial() {
        &[
            "id",
            "strength",
            "radial falloff",
            "speed limit",
            "acceleration",
            "x offset",
            "y offset",
            "radius",
            "lifetime",
        ]
    } else {
        &[
            "id",
            "strength",
            "direction",
            "speed limit",
            "acceleration",
            "x offset",
            "y offset",
            "width",
            "height",
            "lifetime",
        ]
    };
    let slots: Vec<(usize, &'static str, bool, ArgValue)> = now
        .args
        .iter()
        .enumerate()
        .map(|(index, value)| {
            (
                index + 1,
                names.get(index).copied().unwrap_or("argument"),
                was.args.get(index) != Some(value),
                ArgValue::ToF32(*value),
            )
        })
        .collect();
    let (edits, missing) = apply_slots(text, site, &slots);
    if !missing.is_empty() {
        report.skipped.push(format!(
            "{label}: wind box {} changed {}, but its `{}` call in the source is too short to \
             have those arguments",
            before.id,
            missing.join(", "),
            was.command
        ));
    }
    edits
}

/// Replace a non-numeric argument — a const, a bool, a hash, a capsule endpoint — when the
/// text the editor would write differs from what is already there.
fn text_edit(text: &str, span: &Range<usize>, new: &str) -> Option<Replacement> {
    (text[span.clone()].trim() != new).then(|| Replacement {
        span: span.clone(),
        value: new.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) -> PathBuf {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    const MARIO: &str = r#"
use smash::{lua2cpp::*, phx::*};

unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        // A comment mentioning macros::EFFECT that must not be scanned.
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 8.0, 361, 100, 0, 40, 4.5, 0.0, 8.0, 6.0, None, None, None, 1.0, 1.0, *ATTACK_SETOFF_KIND_ON, *ATTACK_LR_CHECK_POS, false, 0, 0.0, 0, false, false, false, false, false, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_M, *COLLISION_SOUND_ATTR_PUNCH, *ATTACK_REGION_PUNCH);
    }
}

unsafe extern "C" fn effect_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("sys_hit_l"), Hash40::new("sys_hit_r"), Hash40::new("haver"), 1.0, 2.0, 3.0, 0.0, 90.0, 45.0, 1.5, true, *EF_FLIP_YZ);
    }
}

pub fn install(agent: &mut smashline::Agent) {
    agent.acmd("game_attackairn", game_attackairn, smashline::Priority::Default);
    agent.acmd("effect_attackairn", effect_attackairn, smashline::Priority::Default);
}
"#;

    fn mario_project() -> (tempfile::TempDir, SourceIndex) {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/mario/acmd.rs", MARIO);
        write(
            tmp.path(),
            "src/mario/mod.rs",
            "pub fn install() { let agent = &mut smashline::Agent::new(\"mario\"); }",
        );
        // Must be ignored: build output, and a file naming no fighter.
        write(tmp.path(), "target/debug/build.rs", MARIO);
        let index = SourceIndex::build(tmp.path()).unwrap();
        (tmp, index)
    }

    #[test]
    fn a_smashline_project_indexes_its_scripts_by_fighter_and_move() {
        let (_tmp, index) = mario_project();
        assert!(index.has_fighter("mario"));
        assert!(index.script("mario", "game_attackairn").is_some());
        assert!(index.script("mario", "effect_attackairn").is_some());
        assert_eq!(index.script_count(), 2, "target/ must not be indexed");

        let body = index.script_body("mario", "attack_air_n").unwrap();
        assert!(body.contains("fn game_attackairn") && body.contains("fn effect_attackairn"));
        // The whole point: the parsers see the user's macro, not vanilla's.
        let calls = crate::acmd::parse_effect_script(&body).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].spawn_func, "EFFECT_FOLLOW_FLIP");
        assert_eq!(calls[0].effect_name_alt.as_deref(), Some("sys_hit_r"));
    }

    #[test]
    fn a_fighter_named_only_by_an_attribute_still_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            r#"
#[acmd_script(agent = "fighter_lucina", script = "game_attacks4", category = ACMD_GAME)]
unsafe extern "C" fn my_custom_name(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 1.0);
}
"#,
        );
        let index = SourceIndex::build(tmp.path()).unwrap();
        // `fighter_lucina` and `lucina` are the same character.
        assert!(index.script("lucina", "game_attacks4").is_some());
    }

    #[test]
    fn scanning_ignores_macro_names_inside_comments_and_strings() {
        let text = r#"fn f() {
    // macros::EFFECT(agent, 1);
    let s = "macros::ATTACK(agent, 2);";
    /* macros::EFFECT_FOLLOW(agent, 3); */
    macros::EFFECT(agent, 4, 5);
}"#;
        let sites = scan_macro_sites(text, 0..text.len());
        assert_eq!(sites.len(), 1, "found {sites:?}");
        assert_eq!(sites[0].name, "EFFECT");
        assert_eq!(sites[0].arg(text, 1), Some("4"));
    }

    /// The scanners walk bytes, so anything multi-byte in the user's file used to be a
    /// chance to slice mid-character and panic the whole editor.
    #[test]
    fn non_ascii_source_does_not_panic_the_scanner() {
        let text = "fn f() {\n    // café — naïve ☃\n    let σ = \"日本語\";\n    \
                    macros::EFFECT(agent, 1, 2);\n}";
        let sites = scan_macro_sites(text, 0..text.len());
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].arg(text, 1), Some("1"));
        // And the same for the function scanner, which brace-matches over the same text.
        assert_eq!(scan_acmd_functions(text).len(), 0);
    }

    #[test]
    fn nested_calls_do_not_split_an_argument() {
        let text = r#"macros::EFFECT(agent, Hash40::new("a"), foo(1, 2), 3.0);"#;
        let sites = scan_macro_sites(text, 0..text.len());
        assert_eq!(sites[0].args.len(), 4);
        assert_eq!(sites[0].arg(text, 1), Some(r#"Hash40::new("a")"#));
        assert_eq!(sites[0].arg(text, 2), Some("foo(1, 2)"));
    }

    #[test]
    fn syncing_a_spawn_rewrites_only_the_values_it_changed() {
        let (tmp, index) = mario_project();
        let body = index.script_body("mario", "attack_air_n").unwrap();
        let pristine = crate::acmd::parse_effect_script(&body).to_effect_calls();

        let mut edited = pristine.clone();
        edited[0].offset[1] = 9.25;
        edited[0].scale = 2.0;

        let report =
            sync_effect_calls(&index, "mario", "attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");

        let after = std::fs::read_to_string(tmp.path().join("src/mario/acmd.rs")).unwrap();
        assert!(
            after.contains(
                r#"macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("sys_hit_l"), Hash40::new("sys_hit_r"), Hash40::new("haver"), 1.0, 9.25, 3.0, 0.0, 90.0, 45.0, 2.0, true, *EF_FLIP_YZ);"#
            ),
            "the macro, its graphics, and its tail must be untouched:\n{after}"
        );
        // Untouched values keep their original spelling rather than being reformatted.
        assert!(after.contains("1.0, 9.25, 3.0"), "{after}");
        assert!(
            after.contains("// A comment mentioning"),
            "comments survive"
        );
    }

    /// The source editor and the editor panels drive each other, so a value written into the
    /// text has to parse back to exactly what was written. Any drift — a rounding difference,
    /// a reformat — reads as a fresh edit on the next frame and the two ping-pong forever.
    #[test]
    fn a_panel_edit_written_to_source_parses_back_unchanged() {
        let (_tmp, index) = mario_project();
        let body = index.script_body("mario", "attack_air_n").unwrap();
        let effect_fn = body[body.find("unsafe extern \"C\" fn effect_").unwrap()..].to_string();
        let pristine = crate::acmd::parse_effect_script(&effect_fn).to_effect_calls();

        let mut edited = pristine.clone();
        edited[0].offset = [1.0 / 3.0, -0.125, 12.5];
        edited[0].rotation = [90.0, 0.5, -45.0];
        edited[0].scale = 0.7;

        let (updated, report) =
            rewrite_effect_calls(&effect_fn, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");

        let reparsed = crate::acmd::parse_effect_script(&updated).to_effect_calls();
        assert_eq!(reparsed.len(), 1);
        for (axis, (got, want)) in reparsed[0]
            .offset
            .iter()
            .chain(&reparsed[0].rotation)
            .zip(edited[0].offset.iter().chain(&edited[0].rotation))
            .enumerate()
        {
            assert!(
                (got - want).abs() < 1e-4,
                "axis {axis}: wrote {want}, read back {got}"
            );
        }
        assert!((reparsed[0].scale - 0.7).abs() < 1e-4);

        // And the second pass is a no-op: this is what actually breaks the feedback loop.
        let (again, report) =
            rewrite_effect_calls(&updated, "mario/attack_air_n", &reparsed, &edited).unwrap();
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(again, updated, "a settled buffer must be byte-identical");
    }

    /// Same convergence property for the hitbox side.
    #[test]
    fn a_hitbox_edit_written_to_source_parses_back_unchanged() {
        let (_tmp, index) = mario_project();
        let body = index.script_body("mario", "attack_air_n").unwrap();
        let game_fn = body[..body.find("unsafe extern \"C\" fn effect_").unwrap()].to_string();
        let pristine = crate::acmd::parse_acmd_script(&game_fn).to_hitboxes();

        let mut edited = pristine.clone();
        edited[0].damage = 13.7;
        edited[0].angle = 270;
        edited[0].kb_base = 62;
        edited[0].offset_y = -3.25;
        edited[0].hitlag_mult = 1.5;

        let (updated, report) =
            rewrite_hitboxes(&game_fn, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");

        let reparsed = crate::acmd::parse_acmd_script(&updated).to_hitboxes();
        assert_eq!(reparsed[0].damage, 13.7);
        assert_eq!(reparsed[0].angle, 270);
        assert_eq!(reparsed[0].kb_base, 62);
        assert_eq!(reparsed[0].offset_y, -3.25);
        assert_eq!(reparsed[0].hitlag_mult, 1.5);

        let (again, report) =
            rewrite_hitboxes(&updated, "mario/attack_air_n", &reparsed, &edited).unwrap();
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(again, updated);
    }

    #[test]
    fn syncing_refuses_to_invent_structure() {
        let (tmp, index) = mario_project();
        let before = std::fs::read_to_string(tmp.path().join("src/mario/acmd.rs")).unwrap();
        let body = index.script_body("mario", "attack_air_n").unwrap();
        let pristine = crate::acmd::parse_effect_script(&body).to_effect_calls();

        // Renaming the graphic is not a value change to this line.
        let mut edited = pristine.clone();
        edited[0].effect_name = "sys_something_else".into();
        edited[0].scale = 3.0;
        let report =
            sync_effect_calls(&index, "mario", "attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.skipped.len(), 1, "{report:?}");

        // Adding a call is reported, and does not silently drop the report either.
        let mut edited = pristine.clone();
        edited.push(pristine[0].clone());
        let report =
            sync_effect_calls(&index, "mario", "attack_air_n", &pristine, &edited).unwrap();
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("added or removed")),
            "{report:?}"
        );

        // Nothing above changed a value, so the file is byte-identical.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("src/mario/acmd.rs")).unwrap(),
            before
        );
    }

    #[test]
    fn a_loop_body_only_syncs_when_every_iteration_agrees() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            r#"
unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    for _ in 0..3 {
        wait(agent.lua_state_agent, 2.0);
        if macros::is_excute(agent) {
            macros::EFFECT(agent, Hash40::new("g"), Hash40::new("top"), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0, 0, 0, 0, 0, 0, false);
        }
    }
}
pub fn install() { let agent = &mut smashline::Agent::new("test_fighter"); }
"#,
        );
        let index = SourceIndex::build(tmp.path()).unwrap();
        let body = index.script_body("test_fighter", "test").unwrap();
        let pristine = crate::acmd::parse_effect_script(&body).to_effect_calls();
        assert_eq!(pristine.len(), 3, "one line, three iterations");

        // One iteration alone cannot be expressed in a single line of source.
        let mut edited = pristine.clone();
        edited[1].scale = 2.0;
        let report = sync_effect_calls(&index, "test_fighter", "test", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 0);
        assert_eq!(report.skipped.len(), 1, "{report:?}");

        // All three together are just a value change to that line.
        let mut edited = pristine.clone();
        for call in &mut edited {
            call.scale = 2.0;
        }
        let report = sync_effect_calls(&index, "test_fighter", "test", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(std::fs::read_to_string(tmp.path().join("src/lib.rs"))
            .unwrap()
            .contains("0.0, 0.0, 0.0, 2.0, 0"));
    }

    #[test]
    fn syncing_a_hitbox_retunes_its_attack_arguments() {
        let (tmp, index) = mario_project();
        let body = index.script_body("mario", "attack_air_n").unwrap();
        let pristine = crate::acmd::parse_acmd_script(&body).to_hitboxes();
        assert_eq!(pristine.len(), 1);

        let mut edited = pristine.clone();
        edited[0].damage = 12.5;
        edited[0].angle = 45;
        edited[0].size = 5.0;

        let report = sync_hitboxes(&index, "mario", "attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 3, "{report:?}");

        let after = std::fs::read_to_string(tmp.path().join("src/mario/acmd.rs")).unwrap();
        assert!(
            after.contains(
                r#"macros::ATTACK(agent, 0, 0, Hash40::new("top"), 12.5, 45, 100, 0, 40, 5.0, 0.0, 8.0, 6.0, None,"#
            ),
            "{after}"
        );
        // The const-valued arguments after the numbers are untouched.
        assert!(after.contains("*ATTACK_SETOFF_KIND_ON"), "{after}");
    }

    const TRAIL: &str = r#"unsafe extern "C" fn effect_attacks4(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), Hash40::new("sword2"), 3, 8, 0.75, 1, 2, 3);
    }
}
"#;

    /// A trail's arguments are textures and trail parameters, and slots 3..9 are NOT the
    /// spawn transform. Writing the spawn layout into them replaced the trail's own count
    /// and parameters (`4, ... 3, 8, 0.75`) with position values and reported success.
    #[test]
    fn a_trail_is_never_written_to_through_the_spawn_transform_layout() {
        let pristine = crate::acmd::parse_effect_script(TRAIL).to_effect_calls();
        assert_eq!(pristine.len(), 1, "{pristine:#?}");
        let mut edited = pristine.clone();
        edited[0].offset = [2.5, 0.0, 0.0];
        edited[0].scale = 3.0;

        let (after, report) = rewrite_effect_calls(TRAIL, "t", &pristine, &edited).unwrap();
        assert_eq!(after, TRAIL, "a trail call must come back untouched");
        assert_eq!(report.changed, 0);
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert!(report.skipped[0].contains("no position"), "{report:?}");
    }

    /// Kirby's down attack, verbatim: two spawns, each with its own rate, in one block. The
    /// rate macro names no effect, so the only thing saying which spawn a rate belongs to is
    /// that it sits directly beneath it — and getting that wrong writes one spawn's value
    /// into the other's line.
    const RATES: &str = r#"unsafe extern "C" fn effect_downattackd(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 15.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_atk_smoke"), Hash40::new("top"), 0, 0, 0, 0, 180, 0, 0.4, 0, 0, 0, 0, 0, 0, false);
        macros::LAST_EFFECT_SET_RATE(agent, 2);
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("sys_attack_line"), Hash40::new("sys_attack_line"), Hash40::new("top"), -8, 4, 2.5, 0, 160, 0, 1.1, true, *EF_FLIP_YZ);
        macros::LAST_EFFECT_SET_RATE(agent, 1.5);
    }
}
"#;

    #[test]
    fn a_rate_edit_rewrites_only_its_own_spawns_rate_line() {
        let pristine = crate::acmd::parse_effect_script(RATES).to_effect_calls();
        assert_eq!(
            pristine.iter().map(|c| c.rate).collect::<Vec<_>>(),
            vec![Some(2.0), Some(1.5)]
        );

        let mut edited = pristine.clone();
        edited[1].rate = Some(0.75);
        let (after, report) = rewrite_effect_calls(RATES, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(
            after.contains("macros::LAST_EFFECT_SET_RATE(agent, 2);"),
            "the first spawn's rate must be untouched:\n{after}"
        );
        assert!(
            after.contains("macros::LAST_EFFECT_SET_RATE(agent, 0.75);"),
            "the second spawn's rate is the one that changed:\n{after}"
        );
        // A whole number stays whole — the slot is generic over `ToF32`, and the emitter
        // spells it the same way, so the two export paths agree on the text.
        let mut whole = pristine.clone();
        whole[0].rate = Some(3.0);
        let (after, _) = rewrite_effect_calls(RATES, "t", &pristine, &whole).unwrap();
        assert!(
            after.contains("macros::LAST_EFFECT_SET_RATE(agent, 3);"),
            "a whole rate must not sprout a decimal point:\n{after}"
        );
    }

    /// The rate lives on a line of its own, so switching it on or off adds or deletes a call.
    /// That is structural, and the house rule is to name it rather than guess where the line
    /// should go — or, worse, delete one of the user's.
    #[test]
    fn turning_a_rate_off_is_reported_rather_than_deleting_the_line() {
        let pristine = crate::acmd::parse_effect_script(RATES).to_effect_calls();
        let mut edited = pristine.clone();
        edited[0].rate = None;

        let (after, report) = rewrite_effect_calls(RATES, "t", &pristine, &edited).unwrap();
        assert_eq!(after, RATES, "the user's line must still be there");
        assert!(
            report.skipped.iter().any(|s| s.contains("lost its rate")),
            "{report:?}"
        );
    }

    #[test]
    fn turning_a_rate_on_is_reported_rather_than_inventing_a_line() {
        // One spawn with no rate at all, so there is nowhere for a value to be written.
        let text = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("sys_attack"), Hash40::new("top"), 0, 1, 0, 0, 0, 0, 1, true);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(text).to_effect_calls();
        assert_eq!(pristine[0].rate, None);
        let mut edited = pristine.clone();
        edited[0].rate = Some(1.25);

        let (after, report) = rewrite_effect_calls(text, "t", &pristine, &edited).unwrap();
        assert_eq!(after, text, "nothing may be inserted into the user's script");
        assert!(
            report.skipped.iter().any(|s| s.contains("gained a rate")),
            "{report:?}"
        );
    }

    /// Selecting calls to sync by transform difference alone meant an edit that changed only
    /// the graphic was neither written nor reported: Save looked like it worked and the
    /// source still said the old name.
    #[test]
    fn an_edit_that_changes_no_transform_value_is_still_reported() {
        let text = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("sys_attack"), Hash40::new("top"), 0, 1, 0, 0, 0, 0, 1, true);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(text).to_effect_calls();
        assert_eq!(pristine.len(), 1);

        for mutate in [
            (|c: &mut crate::data::EffectCall| c.effect_name = "sys_renamed".into())
                as fn(&mut crate::data::EffectCall),
            |c| c.bone_name = "havel".into(),
            |c| c.active_start += 3,
            |c| c.active_end = 20,
            |c| c.disabled = true,
        ] {
            let mut edited = pristine.clone();
            mutate(&mut edited[0]);
            let (after, report) = rewrite_effect_calls(text, "t", &pristine, &edited).unwrap();
            assert_eq!(after, text, "nothing structural may be guessed at");
            assert_eq!(
                report.skipped.len(),
                1,
                "an unwritable edit must be named, not dropped: {report:?}"
            );
        }
    }

    /// The write-back covered eleven numeric slots; every other property the hitbox panels
    /// expose — the joint, the masks, the sound and collision attributes, the flags — was
    /// silently discarded on Save.
    #[test]
    fn every_hitbox_property_the_panels_expose_reaches_the_source() {
        let text = r#"unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 10.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, *ATTACK_SETOFF_KIND_OFF, *ATTACK_LR_CHECK_F, false, 0, 0.0, 0, false, false, true, true, false, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_M, *COLLISION_SOUND_ATTR_PUNCH, *ATTACK_REGION_PUNCH);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 1, "{pristine:#?}");

        let mut edited = pristine.clone();
        let h = &mut edited[0];
        h.bone_name = "ArmL".into();
        h.capsule_end = Some([1.0, 2.0, 3.0]);
        h.setoff_kind = "ATTACK_SETOFF_KIND_ON".into();
        h.lr_check = "ATTACK_LR_CHECK_POS".into();
        h.is_clang = true;
        h.is_add_attack = 1;
        h.hitbox_attr = 2.0;
        h.ground_or_air = 1;
        h.is_mtk = true;
        h.is_shield_disable = true;
        h.is_reflectable = false;
        h.is_absorbable = false;
        h.is_landing_attack = true;
        h.situation_mask = "COLLISION_SITUATION_MASK_G".into();
        h.category_mask = "COLLISION_CATEGORY_MASK_CAT1".into();
        h.part_mask = "COLLISION_PART_MASK_HEAD".into();
        h.no_finish_camera = true;
        h.collision_attr = "collision_attr_fire".into();
        h.sound_level = "ATTACK_SOUND_LEVEL_L".into();
        h.sound_attr = "COLLISION_SOUND_ATTR_KICK".into();
        h.attack_region = "ATTACK_REGION_KICK".into();

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");

        // The written source must parse back to exactly what the editor holds — the real
        // test, since it covers spelling as well as slot numbering. The one intended
        // difference is the joint: skeletons expose `ArmL`, ACMD hashes `arml`, and both
        // `emit_attack` and live injection write the lowercase name.
        let round_tripped = crate::acmd::parse_acmd_script(&after).to_hitboxes();
        edited[0].bone_name = "arml".into();
        assert_eq!(round_tripped, edited, "\n{after}");
        assert!(after.contains(r#"Hash40::new("arml")"#), "{after}");
        assert!(after.contains("Some(1.0), Some(2.0), Some(3.0)"), "{after}");
    }

    /// The flip side: syncing a hitbox nobody edited must not restyle the call. The editor
    /// holds these values decoded, so writing every slot back would respell the whole file.
    #[test]
    fn an_unedited_hitbox_is_left_byte_identical() {
        let text = r#"unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 10.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, 0, 1, false, 0, 0.0, 0, false, false, true, true, false, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_M, *COLLISION_SOUND_ATTR_PUNCH, *ATTACK_REGION_PUNCH);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        let mut edited = pristine.clone();
        edited[0].damage = 12.5;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1, "only damage moved: {after}");
        // The bare `0` and `1` the parser decoded into const names stay bare.
        assert!(after.contains("1.0, 1.0, 0, 1, false"), "{after}");
        assert_eq!(after, text.replace("10.0, 361", "12.5, 361"));
    }

    /// A hitbox's frames are the block it sits in, not arguments of its call.
    #[test]
    fn retiming_a_hitbox_is_reported_rather_than_written() {
        let text = r#"unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 10.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, 0, 1, false, 0, 0.0, 0, false, false, true, true, false, 0, 0, 0, false, Hash40::new("collision_attr_normal"), 0, 0, 0);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        let mut edited = pristine.clone();
        edited[0].active_start += 4;
        edited[0].damage = 12.0;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert!(report.skipped[0].contains("retimed"), "{report:?}");
        // The retime is refused, but the damage it was bundled with still lands.
        assert!(after.contains("12.0, 361"), "{after}");
    }

    /// kirby/ThrowHi's `ATTACK_IGNORE_THROW`, which the archive writes without the capsule
    /// options every `ATTACK` carries. The slot table has to follow the call it is aimed at.
    const CAPSULE_LESS: &str = r#"unsafe extern "C" fn game_throwhi(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 12.0);
    if macros::is_excute(agent) {
        macros::ATTACK_IGNORE_THROW(agent, 0, 0, Hash40::new("top"), 7.0, 65, 95, 0, 85, 9.5, 0.0, 6.5, 2.0, 1.0, 1.0, *ATTACK_SETOFF_KIND_OFF, *ATTACK_LR_CHECK_POS, false, 0, 0.0, 0, false, false, false, false, true, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_M, *COLLISION_SOUND_ATTR_KICK, *ATTACK_REGION_BODY);
    }
}
"#;

    /// Retuning a call written without the capsule triple must count slots from the call,
    /// not from the macro's full signature. Off by three, the hitlag multiplier is written
    /// into the z offset and the setoff kind into hitlag — well-formed source, wrong move.
    #[test]
    fn a_capsule_less_call_is_retuned_through_its_own_shifted_slots() {
        let pristine = crate::acmd::parse_acmd_script(CAPSULE_LESS).to_hitboxes();
        assert_eq!(pristine.len(), 1);
        let mut edited = pristine.clone();
        edited[0].damage = 9.0;
        edited[0].hitlag_mult = 0.5;

        let (after, report) = rewrite_hitboxes(CAPSULE_LESS, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        // Damage is ahead of the optional arguments and cannot shift.
        assert!(after.contains(r#"Hash40::new("top"), 9.0, 65,"#), "{after}");
        // Hitlag is the argument straight after the z offset here. The z offset keeps its
        // own value, which is what the unshifted table used to overwrite.
        assert!(
            after.contains("6.5, 2.0, 0.5, 1.0, *ATTACK_SETOFF"),
            "{after}"
        );
        // And the edit survives a read-back, which is the property that actually matters.
        assert_eq!(crate::acmd::parse_acmd_script(&after).to_hitboxes(), edited);
    }

    /// The capsule arguments are not there to write to. An edit that lands nowhere has to be
    /// named, not dropped — and must not fall through onto whatever argument sits at 13.
    #[test]
    fn a_capsule_edit_on_a_call_without_one_is_reported() {
        let pristine = crate::acmd::parse_acmd_script(CAPSULE_LESS).to_hitboxes();
        let mut edited = pristine.clone();
        edited[0].capsule_end = Some([1.0, 2.0, 3.0]);

        let (after, report) = rewrite_hitboxes(CAPSULE_LESS, "t", &pristine, &edited).unwrap();
        assert_eq!(after, CAPSULE_LESS, "nothing may be written");
        assert_eq!(report.changed, 0);
        assert!(
            report.skipped.iter().any(|s| s.contains("too short")),
            "{report:?}"
        );
    }

    /// The two `ATTACK`-family macros share the id space. Matching on id and part alone made
    /// a hitbox ambiguous between them — or worse, retuned the wrong one.
    #[test]
    fn an_attack_edit_never_lands_in_the_ignore_throw_call_beside_it() {
        let text = r#"unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 10.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, 0, 1, false, 0, 0.0, 0, false, false, true, true, false, 0, 0, 0, false, Hash40::new("collision_attr_normal"), 0, 0, 0);
    }
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::ATTACK_IGNORE_THROW(agent, 0, 0, Hash40::new("top"), 7.0, 65, 95, 0, 85, 9.5, 0.0, 6.5, 2.0, 1.0, 1.0, *ATTACK_SETOFF_KIND_OFF, *ATTACK_LR_CHECK_POS, false, 0, 0.0, 0, false, false, false, false, true, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_M, *COLLISION_SOUND_ATTR_KICK, *ATTACK_REGION_BODY);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 2);
        assert_eq!(
            (pristine[0].id, pristine[1].id),
            (0, 0),
            "the ids really do collide"
        );
        assert_eq!(pristine[0].func, "ATTACK");
        assert_eq!(pristine[1].func, "ATTACK_IGNORE_THROW");

        let mut edited = pristine.clone();
        edited[0].damage = 15.0;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains(r#"Hash40::new("top"), 15.0, 361,"#),
            "{after}"
        );
        assert!(
            after.contains(r#"Hash40::new("top"), 7.0, 65,"#),
            "the throw-piercing hitbox must be untouched:\n{after}"
        );
    }

    /// Swapping which family member a hitbox is is a different macro, not a different
    /// value — the call would also have to gain or lose the capsule triple.
    #[test]
    fn changing_the_attack_macro_is_reported_rather_than_written() {
        let pristine = crate::acmd::parse_acmd_script(CAPSULE_LESS).to_hitboxes();
        let mut edited = pristine.clone();
        edited[0].func = "ATTACK".into();

        let (after, report) = rewrite_hitboxes(CAPSULE_LESS, "t", &pristine, &edited).unwrap();
        assert_eq!(after, CAPSULE_LESS);
        assert_eq!(report.changed, 0);
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("macro being called")),
            "{report:?}"
        );
    }

    /// A wind box and an attack hitbox can carry the same id, and matching on id alone sent
    /// a wind edit into whichever `ATTACK` shared its number.
    #[test]
    fn a_wind_box_edit_never_lands_in_an_attack_call() {
        let text = r#"unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 10.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, 0, 1, false, 0, 0.0, 0, false, false, true, true, false, 0, 0, 0, false, Hash40::new("collision_attr_normal"), 0, 0, 0);
        macros::AREA_WIND_2ND_arg10(agent, 0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        let wind = pristine
            .iter()
            .position(|h| h.category == 2)
            .expect("a wind box");
        assert_eq!(
            pristine[wind].id, pristine[0].id,
            "the ids really do collide"
        );

        let mut edited = pristine.clone();
        let payload = edited[wind].wind.as_mut().expect("a wind payload");
        // Slot 5 is the wind area's X offset. Slot 5 of the `ATTACK` beside it is the angle.
        payload.args[5] = 77.0;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(
            after.contains(
                "macros::ATTACK(agent, 0, 0, Hash40::new(\"top\"), 10.0, 361, 100, 0, 30, 4.0, \
                 0.0, 8.0, 0.0, None"
            ),
            "the ATTACK call must not move:\n{after}"
        );
        assert!(
            after.contains(
                "macros::AREA_WIND_2ND_arg10(agent, 0, 1.0, 2.0, 3.0, 4.0, 77, 6.0, 7.0, 8.0, \
                 9.0);"
            ),
            "{after}"
        );
    }

    const WIND: &str = r#"unsafe extern "C" fn game_specialn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::AREA_WIND_2ND_arg10(agent, 0, 1, 80, 300, 0.8, 4, 12, 24, 16, 50);
        macros::AREA_WIND_2ND_RAD(agent, 1, 0.5, 0.02, 1000, 1, -2, 6, 18);
    }
}
"#;

    /// The wind family's whole point: four commands that share slots 0..=7 and nothing else.
    /// Slot 8 is the rectangle's height and the radial call's lifetime, so retuning one
    /// through the other's table would change how long the area lives.
    #[test]
    fn a_wind_value_rewrites_only_its_own_argument() {
        let pristine = crate::acmd::parse_acmd_script(WIND).to_hitboxes();
        let mut edited = pristine.clone();
        let rect = edited
            .iter_mut()
            .find(|h| h.wind.as_ref().is_some_and(|w| !w.is_radial()))
            .expect("the rectangular wind");
        let payload = rect.wind.as_mut().unwrap();
        payload.args[1] = 2.5; // strength
        payload.args[8] = 20.0; // height — the radial call has a lifetime here

        let (after, report) = rewrite_hitboxes(WIND, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains(
                "macros::AREA_WIND_2ND_arg10(agent, 0, 2.5, 80, 300, 0.8, 4, 12, 24, 20, 50);"
            ),
            "{after}"
        );
        assert!(
            after.contains("macros::AREA_WIND_2ND_RAD(agent, 1, 0.5, 0.02, 1000, 1, -2, 6, 18);"),
            "the radial call must be untouched:\n{after}"
        );
    }

    /// Rectangular and radial are different calls, not different values — the argument they
    /// disagree about is the shape itself. Writing one over the other is exactly the
    /// cross-family corruption the slot tables exist to prevent.
    #[test]
    fn changing_a_wind_from_rectangular_to_radial_is_reported_rather_than_written() {
        let pristine = crate::acmd::parse_acmd_script(WIND).to_hitboxes();
        let mut edited = pristine.clone();
        let rect = edited
            .iter_mut()
            .find(|h| h.wind.as_ref().is_some_and(|w| !w.is_radial()))
            .expect("the rectangular wind");
        let payload = rect.wind.as_mut().unwrap();
        payload.command = "AREA_WIND_2ND_RAD_arg9".into();
        payload.args.remove(8);

        let (after, report) = rewrite_hitboxes(WIND, "t", &pristine, &edited).unwrap();
        assert_eq!(after, WIND);
        assert_eq!(report.changed, 0);
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("AREA_WIND_2ND_arg10") && s.contains("AREA_WIND_2ND_RAD_arg9")),
            "{report:?}"
        );
    }

    /// The arity is part of the command name and every argument is a bare float, so there is
    /// no shape to fall back on: a call whose length disagrees with its name is refused rather
    /// than retuned by position into whatever the author actually wrote there.
    #[test]
    fn a_wind_call_of_the_wrong_length_is_refused() {
        let short = WIND.replace(
            "macros::AREA_WIND_2ND_arg10(agent, 0, 1, 80, 300, 0.8, 4, 12, 24, 16, 50);",
            "macros::AREA_WIND_2ND_arg10(agent, 0, 1, 80, 300, 0.8, 4, 12, 24, 16);",
        );
        let pristine = crate::acmd::parse_acmd_script(WIND).to_hitboxes();
        let mut edited = pristine.clone();
        edited[0].wind.as_mut().unwrap().args[1] = 2.5;

        let (after, report) = rewrite_hitboxes(&short, "t", &pristine, &edited).unwrap();
        assert_eq!(after, short);
        assert_eq!(report.changed, 0);
        assert!(
            report.skipped.iter().any(|s| s.contains("not the 10 that")),
            "{report:?}"
        );
    }

    /// A wind area's end frame is its lifetime argument, so retiming it through the panel is a
    /// value edit and must land — while moving the bar on the timeline, which the payload
    /// cannot account for, must be reported.
    #[test]
    fn a_wind_lifetime_edit_is_written_but_a_timeline_retime_is_reported() {
        let pristine = crate::acmd::parse_acmd_script(WIND).to_hitboxes();

        let mut edited = pristine.clone();
        let start = edited[0].active_start;
        let payload = edited[0].wind.as_mut().unwrap();
        payload.args[9] = 30.0;
        edited[0].active_end = edited[0].wind.as_ref().unwrap().end_frame(start);
        let (after, report) = rewrite_hitboxes(WIND, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains(
                "macros::AREA_WIND_2ND_arg10(agent, 0, 1, 80, 300, 0.8, 4, 12, 24, 16, 30);"
            ),
            "{after}"
        );

        let mut dragged = pristine.clone();
        dragged[0].active_end += 7;
        let (after, report) = rewrite_hitboxes(WIND, "t", &pristine, &dragged).unwrap();
        assert_eq!(after, WIND);
        assert_eq!(report.changed, 0);
        assert!(
            report.skipped.iter().any(|s| s.contains("was retimed")),
            "{report:?}"
        );
    }

    /// The shorter commands have no lifetime slot: the area runs until an
    /// `AreaModule::erase_wind` on a later line, so its end frame is not this call's to
    /// explain. Measuring it against the lifetime anyway reported a retime on every single
    /// edit, because a command with no lifetime "ends" at [`u32::MAX`].
    #[test]
    fn a_wind_ended_by_erase_wind_is_still_retunable() {
        let text = r#"unsafe extern "C" fn game_specialn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::AREA_WIND_2ND_RAD(agent, 1, 0.5, 0.02, 1000, 1, -2, 6, 18);
    }
    wait(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        AreaModule::erase_wind(agent.module_accessor, 1);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 1, "{pristine:#?}");
        assert_ne!(
            pristine[0].active_end,
            u32::MAX,
            "the erase_wind is what ends it"
        );

        let mut edited = pristine.clone();
        edited[0].wind.as_mut().unwrap().args[7] = 22.0; // radius

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains("macros::AREA_WIND_2ND_RAD(agent, 1, 0.5, 0.02, 1000, 1, -2, 6, 22);"),
            "{after}"
        );
    }

    /// `CATCH` numbers its arguments differently from `ATTACK` — slot 4 is the grab box's
    /// size, not damage — so a grab box is retuned through its own layout, never the attack
    /// one.
    #[test]
    fn a_grab_box_is_retuned_through_the_catch_layout() {
        let text = r#"unsafe extern "C" fn game_catch(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::CATCH(agent, 0, Hash40::new("top"), 5.5, 0.0, 6.4, 10.2, None, None, None, *FIGHTER_STATUS_KIND_SWALLOWED, *COLLISION_SITUATION_MASK_A);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 1, "{pristine:#?}");
        assert_eq!(pristine[0].category, 1);

        let mut edited = pristine.clone();
        edited[0].size = 7.25;
        edited[0].offset_y = 9.0;
        edited[0].bone_name = "ArmL".into();
        edited[0].capsule_end = Some([1.0, 2.0, 3.0]);

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");

        let round_tripped = crate::acmd::parse_acmd_script(&after).to_hitboxes();
        edited[0].bone_name = "arml".into();
        assert_eq!(round_tripped, edited, "\n{after}");
        // The two arguments no panel exposes are never touched.
        assert!(after.contains("*FIGHTER_STATUS_KIND_SWALLOWED"), "{after}");
        assert!(after.contains("*COLLISION_SITUATION_MASK_A"), "{after}");
    }

    /// A grab box carries attack-only fields that `CATCH` has no argument for. Editing one
    /// must land nowhere rather than in whichever slot happens to share its number.
    #[test]
    fn a_damage_edit_on_a_grab_box_writes_nothing() {
        let text = r#"unsafe extern "C" fn game_catch(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::CATCH(agent, 0, Hash40::new("top"), 5.5, 0.0, 6.4, 10.2, None, None, None, 0, 0);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        let mut edited = pristine.clone();
        edited[0].damage = 42.0;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert_eq!(after, text, "a CATCH has no damage argument to write into");
        assert_eq!(report.changed, 0, "{report:?}");
    }

    /// The old failure: a grab box matched an `ATTACK` by id alone and had the attack
    /// layout written into it.
    #[test]
    fn a_grab_box_edit_never_lands_in_an_attack_call() {
        let text = r#"unsafe extern "C" fn game_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK(agent, 0, 0, Hash40::new("top"), 10.0, 361, 100, 0, 30, 4.0, 0.0, 8.0, 0.0, None, None, None, 1.0, 1.0, 0, 1, false, 0, 0.0, 0, false, false, true, true, false, 0, 0, 0, false, Hash40::new("collision_attr_normal"), 0, 0, 0);
    }
}
"#;
        // A grab box sharing the attack's id, with no CATCH anywhere in the script.
        let pristine = vec![crate::data::Hitbox {
            id: 0,
            part: 0,
            category: 1,
            ..Default::default()
        }];
        let mut edited = pristine.clone();
        edited[0].size = 9.0;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert_eq!(after, text, "the ATTACK call must not move");
        assert_eq!(report.changed, 0);
        assert_eq!(report.skipped.len(), 1, "{report:?}");
    }
}
