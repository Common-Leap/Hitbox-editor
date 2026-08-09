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
            // A trail written in the raw `effect(*MA_MSC_CMD_…, …)` form is a rewritable call
            // too, and it has to be scanned here rather than anywhere else: `call_macro_ordinals`
            // counts the `EffectCall` the parser makes from it, so a site missing here would
            // renumber every later call and land the next edit on the wrong line. See
            // `data::RAW_TRAIL_COMMANDS`.
            if let Some(site) = scan_raw_trail_site(text, i, range.end) {
                i = site.span.end;
                sites.push(site);
                continue;
            }
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

/// Every direct call to `name(...)` in `text[range]`, in document order.
///
/// This covers the small number of generated helpers that deliberately do not live under the
/// `macros::` namespace. It shares the same literal/comment and balanced-argument handling as
/// [`scan_macro_sites`], while excluding a function declaration whose name happens to match.
fn scan_named_sites(text: &str, name: &str, range: Range<usize>) -> Vec<MacroSite> {
    let bytes = text.as_bytes();
    let mut sites = Vec::new();
    let mut i = range.start;
    while i < range.end {
        if let Some(next) = skip_trivia(text, i) {
            i = next;
            continue;
        }
        if !text.is_char_boundary(i) || !text[i..range.end].starts_with(name) {
            i += 1;
            while i < range.end && !text.is_char_boundary(i) {
                i += 1;
            }
            continue;
        }

        // Do not match a qualified or longer identifier that merely ends with `name`.
        if text[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.')
        {
            i += name.len();
            continue;
        }

        let name_end = i + name.len();
        let mut open = name_end;
        while open < range.end && bytes[open].is_ascii_whitespace() {
            open += 1;
        }
        if open >= range.end || bytes[open] != b'(' {
            i = name_end.max(i + 1);
            continue;
        }

        // The generated helper is nested in the exported ACMD function. Its declaration has the
        // same token sequence as a call after the name, but it is not an editable event site.
        if text[..i].trim_end().ends_with("fn") {
            i = open + 1;
            continue;
        }

        let Some((args, close)) = split_call_args(text, open + 1, range.end) else {
            i = open + 1;
            continue;
        };
        sites.push(MacroSite {
            name: name.to_string(),
            span: i..close + 1,
            args,
        });
        i = close + 1;
    }
    sites
}

/// A raw `effect(*MA_MSC_CMD_…, …)` trail call starting at `i`, as a site the rewriters can use.
///
/// Only the commands in [`crate::data::RAW_TRAIL_COMMANDS`] match, and the returned `args`
/// include the command id at index 0 — the slot `agent` fills in a `macros::` call — so a trail
/// written either way is addressed by the same slot numbers.
fn scan_raw_trail_site(text: &str, i: usize, limit: usize) -> Option<MacroSite> {
    const HEAD: &str = "effect";
    let rest = text.get(i..limit)?;
    let after = rest.strip_prefix(HEAD)?;
    // `effect` has to be the whole identifier. Without this, `my_effect(`, `sub_effect(` and a
    // qualified `foo::effect(` all match and produce a site in the middle of another call.
    if text[..i]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
    {
        return None;
    }
    let open = i + HEAD.len() + (after.len() - after.trim_start().len());
    if text.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let (args, close) = split_call_args(text, open + 1, limit)?;
    let id = args.first().map(|span| &text[span.clone()])?;
    let name = crate::data::raw_trail_command(id)?;
    Some(MacroSite {
        name: name.to_string(),
        span: i..close + 1,
        args,
    })
}

/// The raw trail call on `line`, if it holds one, as the site the rewriters would find.
///
/// The effect parser uses this to decide whether to type a line, so it types exactly the lines
/// `scan_macro_sites` can locate a site in. A looser test on the parser side (`contains`, say)
/// would accept `sub_effect(*MA_MSC_CMD_…)` that the scanner rejects, produce an `EffectCall`
/// with no site behind it, and shift every later call ordinal onto the wrong line.
pub fn raw_trail_line(line: &str) -> Option<MacroSite> {
    line.match_indices("effect(")
        .find_map(|(i, _)| scan_raw_trail_site(line, i, line.len()))
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

/// What a linked project has to say about one move, and what it leaves to the mirror.
#[derive(Debug, Clone)]
pub struct ProjectScript {
    /// The project's own functions, concatenated, verbatim.
    pub body: String,
    /// The [`crate::acmd::SCRIPT_PREFIXES`] entries `body` covers.
    pub covers: Vec<&'static str>,
}

impl ProjectScript {
    /// Whether a category the editor displays is missing, and so worth fetching the mirror for.
    ///
    /// Only the displayed categories count. Waiting on the network to fill in a `sound_`
    /// nothing reads yet would be a straight regression for anyone working offline; see
    /// [`crate::acmd::DISPLAYED_PREFIXES`].
    pub fn needs_mirror(&self) -> bool {
        crate::acmd::DISPLAYED_PREFIXES
            .iter()
            .any(|prefix| !self.covers.contains(prefix))
    }
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

    /// The functions this project defines for a move, concatenated into one body the existing
    /// `acmd::parse_*` functions can read — the same shape the dumped scripts have — together
    /// with the list of categories that body actually covers.
    ///
    /// `None` when the project defines nothing at all for the move, so the caller uses the
    /// mirror alone. Any single category is enough for `Some`: what the project does not
    /// define is filled from the mirror by [`crate::acmd::merge_project_over_mirror`], which
    /// is why the coverage list has to come back with the text.
    pub fn script_source(&self, fighter: &str, move_name: &str) -> Option<ProjectScript> {
        let mut body = String::new();
        let mut covers: Vec<&'static str> = Vec::new();
        for prefix in crate::acmd::SCRIPT_PREFIXES {
            let name = crate::acmd::acmd_script_name(prefix.trim_end_matches('_'), move_name);
            if let Some(site) = self.script(fighter, &name) {
                if let Ok(text) = std::fs::read_to_string(&site.file) {
                    // The span came from this file's text; a concurrent edit can shrink it.
                    if let Some(source) = text.get(site.span.clone()) {
                        body.push_str(source);
                        body.push_str("\n\n");
                        covers.push(prefix);
                    }
                }
            }
        }
        (!covers.is_empty()).then_some(ProjectScript { body, covers })
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
    let (span, quoted) = attr_value_span(line, key)?;
    quoted.then(|| line[span].to_string())
}

/// The span of `key`'s value inside an attribute line, and whether it was quoted.
///
/// A quoted span covers the text *between* the quotes, so splicing over it leaves them in
/// place. A bare one covers a path-shaped token — `category = ACMD_GAME`, and the `::` so a
/// fully-qualified spelling comes back whole rather than truncated at its first colon.
fn attr_value_span(line: &str, key: &str) -> Option<(Range<usize>, bool)> {
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
        let after_key = line[search..].trim_start();
        let Some(rest) = after_key.strip_prefix('=') else {
            continue;
        };
        let value_at = line.len() - rest.trim_start().len();
        let rest = rest.trim_start();
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"')?;
            return Some((value_at + 1..value_at + 1 + end, true));
        }
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(rest.len());
        if end == 0 {
            continue;
        }
        return Some((value_at..value_at + end, false));
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

// ── Creating a script the project does not have ───────────────────────────────

/// A script written into the user's project, and how it was registered.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedScript {
    pub file: PathBuf,
    /// One sentence for the sync report: what was written, beside what, and installed how.
    /// Registration is the half that can silently do nothing, so it is always named.
    pub note: String,
}

/// How a project binds one Rust function to one ACMD script name.
#[derive(Debug, Clone, PartialEq)]
enum Binding {
    /// An `#[acmd_script(...)]` attribute line, verbatim.
    Attribute(String),
    /// An `agent.acmd("…", fn, …);` statement, verbatim, with the script-name and
    /// function-name spans *relative to that text* and the file offset just past it.
    Call {
        text: String,
        script: Range<usize>,
        function: Range<usize>,
        end: usize,
    },
    /// Nothing explicit — the function is reached by its conventional name alone.
    Convention,
}

/// A sibling script to copy a registration from, and where to put the new function.
#[derive(Debug, Clone)]
struct Anchor {
    file: PathBuf,
    /// Offset just past the sibling function, where the new one is inserted.
    after: usize,
    /// The sibling's ACMD script name. Its Rust function name is not kept: it is needed only to
    /// *find* the registration, and the new function's own name comes from the text being written.
    script: String,
    binding: Binding,
}

/// Write `source` into the user's project as `script_name`, registered the way its siblings are.
///
/// `source` is a whole `unsafe extern "C" fn …` block. It is expected to be the *mirror's* text
/// for this category rather than something regenerated from the IR: the effect emitter drops
/// lines it could not type, and creating a function that way would write a lossy copy of vanilla
/// into the user's project under their name. Copying the text and letting the ordinary value
/// sync edit it afterwards keeps creation under the same rule as every other write.
///
/// The caller must rebuild its [`SourceIndex`] afterwards. Every span in the anchor's file past
/// the insertion point has moved, and patching them by arithmetic across two insertions is the
/// kind of thing that works until it does not.
pub fn create_script(
    index: &SourceIndex,
    fighter: &str,
    script_name: &str,
    source: &str,
) -> Result<CreatedScript> {
    if index.script(fighter, script_name).is_some() {
        bail!("{fighter}: the project already has a {script_name}");
    }
    let Some(anchor) = find_anchor(index, fighter, script_name) else {
        bail!(
            "{fighter}: the project has no script to put {script_name} beside — \
             it needs at least one ACMD function for this fighter first"
        );
    };
    let Some(function) = function_name_of(source) else {
        bail!("{script_name}: no `fn` to write — the source for it is not a function");
    };
    let text = std::fs::read_to_string(&anchor.file)
        .with_context(|| format!("reading {}", anchor.file.display()))?;
    if anchor.after > text.len() {
        bail!(
            "{} changed on disk since it was indexed — rescan and try again",
            anchor.file.display()
        );
    }
    let where_at = anchor
        .file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    // Highest offset first, so an earlier insertion cannot move a later one's index.
    let mut inserts: Vec<(usize, String)> = Vec::new();
    let body = source.trim_end();
    let note = match &anchor.binding {
        Binding::Attribute(line) => {
            let attribute = rebuild_attribute(line, &anchor.script, script_name)?;
            inserts.push((anchor.after, format!("\n\n{attribute}\n{body}\n")));
            format!(
                "created {script_name} in {where_at}, after {}, with its own \
                 #[acmd_script] attribute",
                anchor.script
            )
        }
        Binding::Call {
            text: statement,
            script,
            function: function_span,
            end,
        } => {
            let mut registration = statement.clone();
            // Later span first, for the same reason the insertions are ordered.
            let (first, second) = if script.start < function_span.start {
                (function_span, script)
            } else {
                (script, function_span)
            };
            registration.replace_range(first.clone(), &function);
            registration.replace_range(second.clone(), script_name);
            inserts.push((*end, format!("\n{registration}")));
            inserts.push((anchor.after, format!("\n\n{body}\n")));
            format!(
                "created {script_name} in {where_at}, and registered it beside {}",
                anchor.script
            )
        }
        Binding::Convention => {
            inserts.push((anchor.after, format!("\n\n{body}\n")));
            format!(
                "created {script_name} in {where_at}, named the same conventional way as {} — \
                 which this project registers nowhere Visionary can see, so check it installs",
                anchor.script
            )
        }
    };

    inserts.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    let mut updated = text;
    for (at, insert) in inserts {
        if at > updated.len() || !updated.is_char_boundary(at) {
            bail!(
                "{} changed on disk since it was indexed — rescan and try again",
                anchor.file.display()
            );
        }
        updated.insert_str(at, &insert);
    }
    std::fs::write(&anchor.file, &updated)
        .with_context(|| format!("writing {}", anchor.file.display()))?;
    Ok(CreatedScript {
        file: anchor.file,
        note,
    })
}

/// The sibling to write next to: this move's other categories first, then anything else.
///
/// The same move is preferred because its function is where a reader expects the new one to
/// appear. The fallback matters more than it looks: a project that defines the move in no
/// category at all still has *somewhere* for this fighter, and that is enough to know the file,
/// the fighter attribution and the registration style.
fn find_anchor(index: &SourceIndex, fighter: &str, script_name: &str) -> Option<Anchor> {
    let scripts = &index.fighters.get(&normalize_fighter(fighter))?.scripts;
    let mut order: Vec<String> = Vec::new();
    if let Some((_, suffix)) = script_name.split_once('_') {
        for prefix in crate::acmd::SCRIPT_PREFIXES {
            order.push(format!("{prefix}{suffix}"));
        }
    }
    let mut rest: Vec<String> = scripts.keys().cloned().collect();
    rest.sort();
    order.extend(rest);

    for name in order {
        if name == script_name {
            continue;
        }
        let Some(site) = scripts.get(&name) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&site.file) else {
            continue;
        };
        let Some(function) = text.get(site.span.clone()).and_then(function_name_of) else {
            continue;
        };
        let binding = if let Some(line) = attribute_line(&text, site.span.start) {
            Binding::Attribute(line)
        } else if let Some(call) = registration_statement(&text, &name, &function) {
            call
        } else {
            Binding::Convention
        };
        return Some(Anchor {
            file: site.file.clone(),
            after: site.span.end,
            script: name,
            binding,
        });
    }
    None
}

/// The name declared by the first `fn` in a function block.
fn function_name_of(source: &str) -> Option<String> {
    let at = source.find("fn ")? + 3;
    let end = at + source[at..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    (end > at).then(|| source[at..end].to_string())
}

/// The `#[acmd_script(...)]` line above a function declaration, verbatim.
fn attribute_line(text: &str, line_start: usize) -> Option<String> {
    let mut cursor = line_start;
    for _ in 0..8 {
        let prev_start = text[..cursor]
            .trim_end_matches('\n')
            .rfind('\n')
            .map_or(0, |n| n + 1);
        let line = text[prev_start..cursor].trim();
        if line.starts_with("#[") {
            if line.contains("acmd_script") {
                return Some(line.to_string());
            }
        } else if !line.is_empty() {
            return None;
        }
        if prev_start == 0 {
            return None;
        }
        cursor = prev_start;
    }
    None
}

/// The same attribute, respelled for `target` instead of `sibling`.
///
/// `script = "…"` is substituted when present; when it is absent the attribute names only the
/// agent and the function's own conventional name carries the binding, which is already true of
/// the text being written.
fn rebuild_attribute(line: &str, sibling: &str, target: &str) -> Result<String> {
    let mut out = line.to_string();
    if let Some((span, quoted)) = attr_value_span(line, "category") {
        if !quoted {
            let Some(token) = derive_category(sibling, &line[span.clone()], target) else {
                bail!(
                    "cannot tell what `category` a {target} should carry: {sibling} is \
                     declared `{}`, which is not the name its own category implies, so there \
                     is nothing here to derive the other one from",
                    &line[span]
                );
            };
            out.replace_range(span, &token);
        }
    }
    let script = attr_value_span(&out, "script").filter(|(_, quoted)| *quoted);
    if let Some((span, _)) = script {
        out.replace_range(span, target);
    }
    Ok(out)
}

/// The `category = …` token a `target` script should carry, read off the sibling's own.
///
/// Derives only when the sibling's token is exactly the `ACMD_<CATEGORY>` its *own* prefix
/// implies. There is no copy of the `#[acmd_script]` macro on this machine to check a spelling
/// against, so the project's file is the only oracle available — and it is an oracle only while
/// it agrees with itself. Anything else is refused rather than guessed, because a function
/// installed under the wrong category compiles and then replaces the wrong script.
fn derive_category(sibling_script: &str, token: &str, target_script: &str) -> Option<String> {
    let category_of = |script: &str| {
        crate::acmd::SCRIPT_PREFIXES
            .iter()
            .find(|prefix| script.starts_with(*prefix))
            .map(|prefix| prefix.trim_end_matches('_').to_ascii_uppercase())
    };
    let sibling = category_of(sibling_script)?;
    let target = category_of(target_script)?;
    (token.trim() == format!("ACMD_{sibling}")).then(|| format!("ACMD_{target}"))
}

/// The `agent.acmd("…", fn, …);` statement that installs `script`, ready to be respelled.
///
/// Only the general `.acmd(` form is matched, deliberately. `.game_acmd(` and `.effect_acmd(`
/// name the category in the *method*, so mirroring one for a different category would spell a
/// method this project may not have — and there is no smashline on this machine to ask. A
/// project registering that way falls through to [`Binding::Convention`], which writes the
/// function and says plainly that it could not be registered.
fn registration_statement(text: &str, script: &str, function: &str) -> Option<Binding> {
    const MARKER: &str = ".acmd(";
    let mut search = 0;
    while let Some(rel) = text[search..].find(MARKER) {
        let open = search + rel + MARKER.len() - 1;
        search = open + 1;
        let Some((args, close)) = split_call_args(text, open + 1, text.len()) else {
            continue;
        };
        if args.len() < 2 {
            continue;
        }
        // A literal, not a constant: the span arithmetic below steps over the quotes, and a
        // `const NAME` in that slot has none to step over.
        let named = &text[args[0].clone()];
        if !(named.starts_with('"') && named.ends_with('"') && named.len() >= 2) {
            continue;
        }
        if named.trim_matches('"') != script || text[args[1].clone()] != *function {
            continue;
        }
        let start = text[..open].rfind('\n').map_or(0, |n| n + 1);
        let mut end = close + 1;
        if text[end..].starts_with(';') {
            end += 1;
        }
        // Inside the quotes, so respelling the script leaves them in place.
        let quoted = args[0].start + 1..args[0].end - 1;
        return Some(Binding::Call {
            text: text[start..end].to_string(),
            script: quoted.start - start..quoted.end - start,
            function: args[1].start - start..args[1].end - start,
            end,
        });
    }
    None
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

fn bool_edit(text: &str, span: &Range<usize>, new: bool) -> Option<Replacement> {
    let current = text[span.clone()].trim();
    let old = match current {
        "false" => false,
        "true" => true,
        _ => return None,
    };
    let replacement = new.to_string();
    if old == new || replacement == current {
        return None;
    }
    Some(Replacement {
        span: span.clone(),
        value: replacement,
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
    let (sites, modifier_sites) = spawn_and_modifier_sites(text);

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
        if let Some(control) = &target.control {
            let Some(was) = pristine[differs[0]].control.as_ref() else {
                report.skipped.push(format!(
                    "{label}: `{}` changed into a control command — source syncing does not add or replace calls",
                    macro_site.name
                ));
                continue;
            };
            control_edits(
                text,
                label,
                macro_site,
                was,
                control,
                &mut edits,
                &mut report,
            );
            continue;
        }
        // A colour command has no transform to write and no rate line beneath it — its
        // arguments are a length and four components, and nothing else.
        if let Some(color) = &target.color {
            edits.extend(color_edits(text, macro_site, color));
            continue;
        }
        edits.extend(transform_edits(text, macro_site, target));

        let was = &pristine[differs[0]];
        let sites = modifier_sites.get(ordinal);
        modifier_edits(
            text,
            label,
            "rate",
            "LAST_EFFECT_SET_RATE",
            macro_site,
            was.rate.map(|v| vec![v]),
            target.rate.map(|v| vec![v]),
            sites.and_then(|s| s.rate.as_ref()),
            &mut edits,
            &mut report,
        );
        work_int_edits(
            text,
            label,
            macro_site,
            was.work_int.as_deref(),
            target.work_int.as_deref(),
            sites.and_then(|s| s.work_int.as_ref()),
            &mut edits,
            &mut report,
        );
        modifier_edits(
            text,
            label,
            "camera offset",
            "LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT",
            macro_site,
            was.camera_offset.map(|v| vec![v]),
            target.camera_offset.map(|v| vec![v]),
            sites.and_then(|s| s.camera_offset.as_ref()),
            &mut edits,
            &mut report,
        );
        modifier_edits(
            text,
            label,
            "tint",
            "LAST_EFFECT_SET_COLOR",
            macro_site,
            was.tint.map(|v| v.to_vec()),
            target.tint.map(|v| v.to_vec()),
            sites.and_then(|s| s.tint.as_ref()),
            &mut edits,
            &mut report,
        );
        modifier_edits(
            text,
            label,
            "particle tint",
            "LAST_PARTICLE_SET_COLOR",
            macro_site,
            was.particle_tint.map(|v| v.to_vec()),
            target.particle_tint.map(|v| v.to_vec()),
            sites.and_then(|s| s.particle_tint.as_ref()),
            &mut edits,
            &mut report,
        );
        modifier_edits(
            text,
            label,
            "opacity",
            "LAST_EFFECT_SET_ALPHA",
            macro_site,
            was.alpha.map(|v| vec![v]),
            target.alpha.map(|v| vec![v]),
            sites.and_then(|s| s.alpha.as_ref()),
            &mut edits,
            &mut report,
        );
        modifier_edits(
            text,
            label,
            "scale W",
            "LAST_EFFECT_SET_SCALE_W",
            macro_site,
            was.scale_w.clone(),
            target.scale_w.clone(),
            sites.and_then(|s| s.scale_w.as_ref()),
            &mut edits,
            &mut report,
        );
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

/// Write one `LAST_EFFECT_SET_*` modifier back, or say why it could not be.
///
/// Each of these lives on its own line, so turning one on or off is a call added or removed —
/// structural, and reported. Only retuning an existing one is a value edit. The modifiers differ
/// only in which property they carry — rate, camera offset, opacity, effect tint, or
/// particle tint —
/// and every rule around them is the same. `noun` names the property in the user's terms and
/// `command` names the line to look for.
#[allow(clippy::too_many_arguments)]
fn modifier_edits(
    text: &str,
    label: &str,
    noun: &str,
    command: &str,
    macro_site: &MacroSite,
    was: Option<Vec<f32>>,
    now: Option<Vec<f32>>,
    site: Option<&MacroSite>,
    edits: &mut Vec<Replacement>,
    report: &mut SyncReport,
) {
    if was == now {
        return;
    }
    if command == "LAST_EFFECT_SET_SCALE_W"
        && site.is_some_and(|site| site.name == "visionary_last_effect_set_scale_w")
    {
        match (was.as_ref(), now.as_ref(), site) {
            (Some(_), Some(values), Some(site)) => {
                if let Some(span) = site.args.get(1) {
                    let current = text[span.clone()].trim();
                    let current_values = current
                        .strip_prefix("&[")
                        .and_then(|value| value.strip_suffix(']'))
                        .map(|value| {
                            value
                                .split(',')
                                .filter_map(|value| value.trim().parse::<f32>().ok())
                                .collect::<Vec<_>>()
                        });
                    if current_values.as_ref() != Some(values) {
                        edits.push(Replacement {
                            span: span.clone(),
                            value: format!(
                                "&[{}]",
                                values
                                    .iter()
                                    .map(|value| format_float(*value))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        });
                    }
                }
            }
            (None, Some(_), _) => report.skipped.push(format!(
                "{label}: `{}` gained a {noun} — that is a new {command} line, which source \
                 syncing does not add",
                macro_site.name
            )),
            (Some(_), None, _) => report.skipped.push(format!(
                "{label}: `{}` lost its {noun} — source syncing does not delete the {command} \
                 line that sets it",
                macro_site.name
            )),
            (Some(_), Some(_), None) => report.skipped.push(format!(
                "{label}: `{}` has a {noun} in the editor but no {command} directly beneath it \
                 in the source — reload the move from source",
                macro_site.name
            )),
            (None, None, _) => {}
        }
        return;
    }
    match (was, now, site) {
        (Some(_), Some(values), Some(site)) => {
            // Argument 0 is `agent`, so the components start at 1 and are in source order —
            // the one layout the whole family shares.
            for (offset, value) in values.iter().enumerate() {
                if let Some(span) = site.args.get(offset + 1) {
                    edits.extend(to_f32_edit(text, span, *value));
                }
            }
        }
        (None, Some(_), _) => report.skipped.push(format!(
            "{label}: `{}` gained a {noun} — that is a new {command} line, which source \
             syncing does not add",
            macro_site.name
        )),
        (Some(_), None, _) => report.skipped.push(format!(
            "{label}: `{}` lost its {noun} — source syncing does not delete the {command} \
             line that sets it",
            macro_site.name
        )),
        (Some(_), Some(_), None) => report.skipped.push(format!(
            "{label}: `{}` has a {noun} in the editor but no {command} directly beneath it \
             in the source — reload the move from source",
            macro_site.name
        )),
        (None, None, _) => {}
    }
}

/// Rewrite the authored WorkModule slot of an existing `LAST_EFFECT_SET_WORK_INT` line.
///
/// Unlike the numeric last-effect modifiers, this value is a source token rather than a float:
/// a named Work ID is meaningful only with its authored constant spelling. Adding or removing
/// the line remains structural, so source syncing only changes an existing argument.
#[allow(clippy::too_many_arguments)]
fn work_int_edits(
    text: &str,
    label: &str,
    macro_site: &MacroSite,
    was: Option<&str>,
    now: Option<&str>,
    site: Option<&MacroSite>,
    edits: &mut Vec<Replacement>,
    report: &mut SyncReport,
) {
    if was == now {
        return;
    }
    match (was, now, site) {
        (Some(_), Some(value), Some(site)) => {
            if let Some(edit) = site.args.get(1).and_then(|span| {
                let replacement = crate::acmd::const_expr(value);
                (text[span.clone()].trim() != replacement).then_some(Replacement {
                    span: span.clone(),
                    value: replacement,
                })
            }) {
                edits.push(edit);
            }
        }
        (None, Some(_), _) => report.skipped.push(format!(
            "{label}: `{}` gained a Work ID — that is a new LAST_EFFECT_SET_WORK_INT line, which source syncing does not add",
            macro_site.name
        )),
        (Some(_), None, _) => report.skipped.push(format!(
            "{label}: `{}` lost its Work ID — source syncing does not delete the LAST_EFFECT_SET_WORK_INT line that sets it",
            macro_site.name
        )),
        (Some(_), Some(_), None) => report.skipped.push(format!(
            "{label}: `{}` has a Work ID in the editor but no LAST_EFFECT_SET_WORK_INT line directly beneath it in the source — reload the move from source",
            macro_site.name
        )),
        (None, None, _) => {}
    }
}

/// The `LAST_EFFECT_SET_*` / `LAST_PARTICLE_SET_COLOR` lines a spawn can carry, in the spelling
/// `scan_macro_sites` reports.
///
/// Only these seven break nothing when they sit between a spawn and a later modifier of its
/// own; every other macro ends the run. Adding a member of the family to
/// [`crate::data::EffectCall`] means adding it here too, or its line will end the run and the
/// modifier after it will be reported as unfindable.
const MODIFIER_COMMANDS: &[&str] = &[
    "LAST_EFFECT_SET_RATE",
    "LAST_EFFECT_SET_WORK_INT",
    "LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT",
    "LAST_EFFECT_SET_COLOR",
    "LAST_PARTICLE_SET_COLOR",
    "LAST_EFFECT_SET_ALPHA",
    "LAST_EFFECT_SET_SCALE_W",
    "visionary_last_effect_set_scale_w",
];

/// The last-target modifier lines belonging to one spawn, each present only if the source has it.
#[derive(Default)]
struct ModifierSites {
    rate: Option<MacroSite>,
    work_int: Option<MacroSite>,
    camera_offset: Option<MacroSite>,
    tint: Option<MacroSite>,
    particle_tint: Option<MacroSite>,
    alpha: Option<MacroSite>,
    scale_w: Option<MacroSite>,
}

/// The spawn calls in `text`, in ordinal order, each paired with its `LAST_EFFECT_SET_*` lines.
///
/// These macros name no effect, so the only thing tying one to a spawn is that it comes
/// directly after it — the same rule `eval_effect_stmts` uses to fill `EffectCall::rate`,
/// `camera_offset`, `tint`, `particle_tint`, and `alpha`, and the two must agree or a value would
/// be read off one call and
/// written into another. Anything else between them, including a macro this scanner does not
/// recognise, breaks the pairing rather than reaching further back for a spawn to claim.
///
/// A modifier does *not* break the run for the modifiers after it: a script that writes a tint
/// and then a rate has both of them naming the spawn above the pair, and `eval_effect_stmts`
/// reads them that way too.
/// The spawn sites of `text`, in order, by name — the list `rewrite_effect_calls` indexes by
/// call ordinal. Exposed so a corpus oracle can check it against what the parser produces.
#[cfg(test)]
pub fn spawn_site_names(text: &str) -> Vec<String> {
    spawn_and_modifier_sites(text)
        .0
        .into_iter()
        .map(|site| site.name)
        .collect()
}

fn spawn_and_modifier_sites(text: &str) -> (Vec<MacroSite>, Vec<ModifierSites>) {
    let mut spawns: Vec<MacroSite> = Vec::new();
    let mut modifiers: Vec<ModifierSites> = Vec::new();
    let mut adjacent = false;
    let mut sites = scan_macro_sites(text, 0..text.len());
    sites.extend(scan_named_sites(
        text,
        "visionary_last_effect_set_scale_w",
        0..text.len(),
    ));
    sites.sort_by_key(|site| site.span.start);
    for site in sites {
        if is_spawn_macro(&site.name) {
            // A trail is a spawn for ordinal purposes but never anchors a modifier, matching
            // the parser — see the `AfterImage` arm of `eval_effect_stmts`. A colour command is
            // counted for the same reason and anchors nothing for the same reason: both
            // produce an `EffectCall`, so both consume an ordinal, and neither is what
            // `LAST_EFFECT_SET_*` would find at runtime.
            adjacent = crate::acmd::is_effect_spawn_macro(&site.name);
            spawns.push(site);
            modifiers.push(ModifierSites::default());
            continue;
        }
        if !MODIFIER_COMMANDS.contains(&site.name.as_str()) {
            adjacent = false;
            continue;
        }
        if !adjacent {
            continue;
        }
        let Some(entry) = modifiers.last_mut() else {
            continue;
        };
        let valid_arity = match site.name.as_str() {
            "LAST_EFFECT_SET_COLOR" | "LAST_PARTICLE_SET_COLOR" => site.args.len() == 4,
            "LAST_EFFECT_SET_SCALE_W" => (2..=4).contains(&site.args.len()),
            "visionary_last_effect_set_scale_w" => site
                .args
                .get(1)
                .and_then(|span| text.get(span.clone()))
                .and_then(|value| value.trim().strip_prefix("&["))
                .and_then(|value| value.strip_suffix(']'))
                .map(|value| {
                    let values = value
                        .split(',')
                        .filter_map(|value| value.trim().parse::<f32>().ok())
                        .count();
                    (1..=3).contains(&values)
                })
                .unwrap_or(false),
            _ => site.args.len() == 2,
        };
        if !valid_arity {
            adjacent = false;
            continue;
        }
        // A second line of the same kind overwrites the first, because in game the later call
        // wins and that is the value the parser will have read.
        let slot = match site.name.as_str() {
            "LAST_EFFECT_SET_WORK_INT" => &mut entry.work_int,
            "LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT" => &mut entry.camera_offset,
            "LAST_EFFECT_SET_COLOR" => &mut entry.tint,
            "LAST_PARTICLE_SET_COLOR" => &mut entry.particle_tint,
            "LAST_EFFECT_SET_ALPHA" => &mut entry.alpha,
            "LAST_EFFECT_SET_SCALE_W" | "visionary_last_effect_set_scale_w" => &mut entry.scale_w,
            _ => &mut entry.rate,
        };
        *slot = Some(site);
    }
    (spawns, modifiers)
}

/// Whether a scanned macro name is one `call_macro_ordinals` counts.
///
/// "Spawn" is a slight lie now that colour commands are in the list, but the property that
/// matters is unchanged: these are exactly the macros that produce an `EffectCall`, so this
/// predicate and `call_macro_ordinals` must accept the same set or every ordinal past the
/// first disagreement points at the wrong line of source.
fn is_spawn_macro(name: &str) -> bool {
    crate::acmd::is_effect_spawn_macro(name)
        || is_trail_macro(name)
        || crate::data::is_color_command(name)
        || is_effect_control_macro(name)
}

fn is_effect_control_macro(name: &str) -> bool {
    matches!(
        name,
        "EFFECT_DETACH_KIND" | "EFFECT_DETACH_KIND_WORK" | "ENABLE_AREA" | "UNABLE_AREA"
    )
}

/// Whether a scanned macro name starts an AFTER_IMAGE trail.
///
/// Trails produce an `EffectCall` like any other spawn, so they have to be counted here to
/// keep call ordinals aligned with the source — but their arguments share none of the spawn
/// layout, so nothing may be written into them positionally.
fn is_trail_macro(name: &str) -> bool {
    name.starts_with("AFTER_IMAGE4_ON")
        || name == "AFTER_IMAGE_ON"
        // The raw-command trails, under the name `scan_raw_trail_site` gives them. These are the
        // only trail-ON calls that actually occur in the corpus.
        || crate::data::RAW_TRAIL_COMMANDS
            .iter()
            .any(|(_, site)| *site == name)
}

/// Everything a value rewrite CAN change about a call — the test for whether every iteration
/// of a loop body agrees, since they all come off one line of source.
///
/// The `LAST_EFFECT_SET_*` modifiers count: each is written back from its own line, but that
/// line is inside the loop body too, so a per-iteration rate, camera offset, effect tint, particle
/// tint, or opacity is no more expressible than a per-iteration position.
fn transform_matches(a: &crate::data::EffectCall, b: &crate::data::EffectCall) -> bool {
    a.offset == b.offset
        && a.rotation == b.rotation
        && a.scale == b.scale
        && a.rate == b.rate
        && a.work_int == b.work_int
        && a.camera_offset == b.camera_offset
        && a.tint == b.tint
        && a.particle_tint == b.particle_tint
        && a.alpha == b.alpha
        && a.scale_w == b.scale_w
        && a.color == b.color
        && a.control == b.control
}

/// Everything a value rewrite CANNOT change about a call.
///
/// `active_end` belongs here with `active_start`: a follow effect's end frame is the
/// `EFFECT_OFF_KIND` that closes it, so moving it means moving a different call in a
/// different frame block, not retuning an argument.
fn identity_matches(a: &crate::data::EffectCall, b: &crate::data::EffectCall) -> bool {
    let same_control_kind = match (&a.control, &b.control) {
        (Some(left), Some(right)) => left.command_name() == right.command_name(),
        (None, None) => true,
        _ => false,
    };
    same_control_kind
        && a.effect_name.eq_ignore_ascii_case(&b.effect_name)
        && a.effect_name_alt == b.effect_name_alt
        && a.bone_name.eq_ignore_ascii_case(&b.bone_name)
        // A trail's second edge is a joint like the first, and no transform rewrite reaches it.
        && a.trail_bone2 == b.trail_bone2
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
    let mut edits: Vec<Replacement> = Vec::new();
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

/// Replacements for one colour command's arguments.
///
/// The layout comes from the command found in the *source*, not from what the editor holds:
/// if the two have come apart, the source is what the spans belong to, and writing a
/// transition length into a `FLASH` that has no such slot would put it in the red channel.
fn color_edits(text: &str, site: &MacroSite, color: &crate::data::ColorCall) -> Vec<Replacement> {
    let Some((has_transition, has_rgba)) = crate::data::color_command_layout(&site.name) else {
        return Vec::new();
    };
    let (transition_slot, rgba_slots) = crate::data::color_slots(has_transition);
    let mut slots: Vec<(usize, f32)> = Vec::new();
    if let (Some(slot), Some(value)) = (transition_slot, color.transition) {
        slots.push((slot, value));
    }
    if let (true, Some(rgba)) = (has_rgba, color.rgba) {
        slots.extend(rgba_slots.into_iter().zip(rgba));
    }
    slots
        .into_iter()
        .filter_map(|(slot, value)| to_f32_edit(text, site.args.get(slot)?, value))
        .collect()
}

/// Replacements for an existing C4 point command. The command itself is structural and is
/// therefore required to stay the same; its authored values are safe to retune in place.
fn control_edits(
    text: &str,
    label: &str,
    site: &MacroSite,
    was: &crate::data::EffectControl,
    now: &crate::data::EffectControl,
    edits: &mut Vec<Replacement>,
    report: &mut SyncReport,
) {
    if was.command_name() != now.command_name() {
        report.skipped.push(format!(
            "{label}: `{}` changed control command — source syncing only retunes existing values",
            site.name
        ));
        return;
    }
    match (was, now) {
        (
            crate::data::EffectControl::DetachKind { .. },
            crate::data::EffectControl::DetachKind { effect_name, unk },
        ) => {
            if let Some(edit) = control_const_edit(text, site, 1, effect_name, true) {
                edits.push(edit);
            }
            control_unk_edit(text, label, site, 2, *unk, edits, report);
        }
        (
            crate::data::EffectControl::DetachKindWork { .. },
            crate::data::EffectControl::DetachKindWork { work, unk },
        ) => {
            if let Some(edit) = control_const_edit(text, site, 1, work, false) {
                edits.push(edit);
            }
            control_unk_edit(text, label, site, 2, *unk, edits, report);
        }
        (
            crate::data::EffectControl::EnableArea { .. },
            crate::data::EffectControl::EnableArea { kind },
        )
        | (
            crate::data::EffectControl::UnableArea { .. },
            crate::data::EffectControl::UnableArea { kind },
        ) => {
            if let Some(edit) = control_const_edit(text, site, 1, kind, false) {
                edits.push(edit);
            }
        }
        _ => report.skipped.push(format!(
            "{label}: `{}` has a control payload that does not match its source command",
            site.name
        )),
    }
}

fn control_const_edit(
    text: &str,
    site: &MacroSite,
    slot: usize,
    value: &str,
    hash: bool,
) -> Option<Replacement> {
    let span = site.args.get(slot)?.clone();
    let replacement = if hash {
        crate::acmd::hash_arg(value)
    } else {
        crate::acmd::const_expr(value)
    };
    (text[span.clone()].trim() != replacement).then_some(Replacement {
        span,
        value: replacement,
    })
}

fn control_unk_edit(
    text: &str,
    label: &str,
    site: &MacroSite,
    slot: usize,
    value: i64,
    edits: &mut Vec<Replacement>,
    report: &mut SyncReport,
) {
    let Some(span) = site.args.get(slot) else {
        report.skipped.push(format!(
            "{label}: `{}` has no `unk` argument slot",
            site.name
        ));
        return;
    };
    let Some(edit) = int_edit(text, span, value) else {
        if text[span.clone()].trim().parse::<i64>().is_err() {
            report.skipped.push(format!(
                "{label}: `{}` has a non-literal `unk` argument, so source syncing left it unchanged",
                site.name
            ));
        }
        return;
    };
    edits.push(edit);
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
        .filter(|s| crate::acmd::ATTACK_FUNCS.contains(&s.name.as_str()) || s.name == "ATTACK_FP")
        .collect();
    let catches: Vec<&MacroSite> = sites.iter().filter(|s| s.name == "CATCH").collect();
    let abs: Vec<&MacroSite> = sites.iter().filter(|s| s.name == "ATTACK_ABS").collect();
    // Name equality, not a prefix: `SET_SEARCH_SIZE_EXIST` is a two-argument modifier, and
    // retuning it through the 17-slot box layout is exactly the cross-family write this
    // per-family split exists to prevent.
    let searches: Vec<&MacroSite> = sites.iter().filter(|s| s.name == "SEARCH").collect();
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
        // `ATTACK_ABS` shares the id space with nothing and is matched on its absolute kind:
        // every corpus call writes id 0, and kirby/ThrowF has two in one block that differ
        // only by kind. Matching on id here would be matching on a constant.
        if hitbox.category == crate::data::CAT_ABS || before.category == crate::data::CAT_ABS {
            let want = before.abs.as_ref().map(|a| a.kind.as_str()).unwrap_or("");
            let matching: Vec<&MacroSite> = abs
                .iter()
                .copied()
                .filter(|site| {
                    site.arg(text, 1).map(|a| a.trim().trim_start_matches('*')) == Some(want)
                })
                .collect();
            let [macro_site] = matching[..] else {
                report.skipped.push(format!(
                    "{label}: the {want} throw damage matches {} `ATTACK_ABS` calls in the \
                     source — cannot tell which one to retune",
                    matching.len()
                ));
                continue;
            };
            let (call_edits, missing) = attack_abs_edits(text, macro_site, before, hitbox);
            if !missing.is_empty() {
                report.skipped.push(format!(
                    "{label}: the {want} throw damage changed {}, but its `ATTACK_ABS` call in \
                     the source is too short to have those arguments",
                    missing.join(", ")
                ));
            }
            edits.extend(call_edits);
            continue;
        }
        // `ATTACK_FP` is a collision, but its 41-slot payload is not the ordinary ATTACK
        // layout. Its source path has a separate table just as its parser and live hook do.
        if hitbox.category == crate::data::CAT_ATTACK_FP
            || before.category == crate::data::CAT_ATTACK_FP
        {
            if before.func != hitbox.func {
                report.skipped.push(format!(
                    "{label}: hitbox {} changed from `{}` to `{}` — source syncing rewrites \
                     argument values, not the macro being called",
                    before.id, before.func, hitbox.func
                ));
                continue;
            }
            if before.id != hitbox.id || before.part != hitbox.part {
                report.skipped.push(format!(
                    "{label}: ATTACK_FP hitbox {} was renumbered — source syncing only rewrites \
                     argument values",
                    before.id
                ));
                continue;
            }
            let matching: Vec<&MacroSite> = sites
                .iter()
                .filter(|site| {
                    site.name == "ATTACK_FP"
                        && site.arg(text, 1).and_then(|a| a.trim().parse::<u32>().ok())
                            == Some(before.id)
                        && site.arg(text, 2).and_then(|a| a.trim().parse::<u32>().ok())
                            == Some(before.part)
                })
                .collect();
            let [macro_site] = matching[..] else {
                report.skipped.push(format!(
                    "{label}: ATTACK_FP hitbox {} matches {} calls in the source — cannot tell \
                     which one to retune",
                    before.id,
                    matching.len()
                ));
                continue;
            };
            if before.active_start != hitbox.active_start || before.active_end != hitbox.active_end
            {
                report.skipped.push(format!(
                    "{label}: ATTACK_FP hitbox {} was retimed — its frames are the block it sits \
                     in, not arguments, so source syncing cannot move it",
                    before.id
                ));
            }
            let (call_edits, missing) = attack_fp_edits(text, macro_site, before, hitbox);
            if !missing.is_empty() {
                report.skipped.push(format!(
                    "{label}: ATTACK_FP hitbox {} changed {}, but its call in the source is too \
                     short to have those arguments",
                    before.id,
                    missing.join(", ")
                ));
            }
            edits.extend(call_edits);
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
        // `CATCH` takes no `part`, so a grab box is keyed on its id alone. `SEARCH` takes both,
        // and is keyed like an `ATTACK` — but against its own calls, since kirby/SpecialNStart
        // opens a `CATCH`, a `SEARCH` and an `ATTACK_ABS` all carrying id 0 in one block.
        let is_grab = hitbox.category == 1;
        let is_search = hitbox.category == crate::data::CAT_SEARCH;
        let matching: Vec<&MacroSite> = if is_grab {
            catches
                .iter()
                .copied()
                .filter(|site| {
                    site.arg(text, 1).and_then(|a| a.trim().parse::<u32>().ok()) == Some(before.id)
                })
                .collect()
        } else if is_search {
            searches
                .iter()
                .copied()
                .filter(|site| {
                    site.arg(text, 1).and_then(|a| a.trim().parse::<u32>().ok()) == Some(before.id)
                        && site.arg(text, 2).and_then(|a| a.trim().parse::<u32>().ok())
                            == Some(before.part)
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
        } else if is_search {
            search_edits(text, macro_site, before, hitbox)
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

/// Replacements for the established editor fields in one `ATTACK_FP` call.
///
/// The site indices include `agent`; the FP slots after it are independently documented in the
/// `smash-script` wrapper. Geometry and the remaining unknown fields are intentionally absent:
/// source syncing must not turn an ordinary ATTACK interpretation into a guess about FP.
fn attack_fp_edits(
    text: &str,
    site: &MacroSite,
    before: &crate::data::Hitbox,
    after: &crate::data::Hitbox,
) -> (Vec<Replacement>, Vec<&'static str>) {
    let attr = match after.collision_attr.strip_prefix("0x") {
        Some(hex) => format!("Hash40::new_raw(0x{hex})"),
        None => format!("Hash40::new(\"{}\")", after.collision_attr),
    };
    let konst = crate::acmd::const_expr;
    let slots: [(usize, &'static str, bool, ArgValue); 12] = [
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
            13,
            "collision attribute",
            before.collision_attr != after.collision_attr,
            ArgValue::Text(attr),
        ),
        (
            15,
            "hitlag multiplier",
            before.hitlag_mult != after.hitlag_mult,
            ArgValue::Float(after.hitlag_mult),
        ),
        (
            16,
            "SDI multiplier",
            before.sdi_mult != after.sdi_mult,
            ArgValue::Float(after.sdi_mult),
        ),
        (
            17,
            "clang",
            before.is_clang != after.is_clang,
            ArgValue::Text(after.is_clang.to_string()),
        ),
        (
            22,
            "ground/air",
            before.ground_or_air != after.ground_or_air,
            ArgValue::Int(after.ground_or_air as i64),
        ),
        (
            30,
            "reflectable",
            before.is_reflectable != after.is_reflectable,
            ArgValue::Text(after.is_reflectable.to_string()),
        ),
        (
            31,
            "absorbable",
            before.is_absorbable != after.is_absorbable,
            ArgValue::Text(after.is_absorbable.to_string()),
        ),
    ];
    let (mut edits, mut missing) = apply_slots(text, site, &slots);
    let tail: [(usize, &'static str, bool, ArgValue); 4] = [
        (
            20,
            "sound level",
            before.sound_level != after.sound_level,
            ArgValue::Text(konst(&after.sound_level)),
        ),
        (
            21,
            "sound attribute",
            before.sound_attr != after.sound_attr,
            ArgValue::Text(konst(&after.sound_attr)),
        ),
        (
            24,
            "attack region",
            before.attack_region != after.attack_region,
            ArgValue::Text(konst(&after.attack_region)),
        ),
        (
            35,
            "facing check",
            before.lr_check != after.lr_check,
            ArgValue::Text(konst(&after.lr_check)),
        ),
    ];
    let (tail_edits, tail_missing) = apply_slots(text, site, &tail);
    edits.extend(tail_edits);
    missing.extend(tail_missing);
    missing.sort_unstable();
    missing.dedup();
    (edits, missing)
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

/// Replacements for the arguments of one `SEARCH` call that the user actually changed.
///
/// A detection box's editable properties are its geometry, what it looks for, which hurtbox
/// states it counts, and its three trailing masks. The two undocumented slots have no control
/// and are never written; see [`crate::data::SearchExtras`].
///
/// The tail moves. `SEARCH` is dumped both with and without its three capsule arguments, and 4
/// of the corpus's 7 calls are the short form, so every argument after the geometry sits three
/// slots earlier in one shape than the other. Locating them by a fixed number would write the
/// situation mask over the hit status — a call that still compiles and detects the wrong thing,
/// which is the failure mode this file exists to refuse.
fn search_edits(
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
    let has_capsule_slots = site.arg(text, 8).is_some_and(crate::acmd::is_capsule_slot);
    let capsule_changed = before.capsule_end != after.capsule_end;
    let capsule_writable = capsule_changed && has_capsule_slots;
    let tail = if has_capsule_slots { 11 } else { 8 };
    let konst = |slot: usize, field: &'static str, was: &str, now: &str| {
        (
            slot,
            field,
            was != now,
            ArgValue::Text(crate::acmd::const_expr(now)),
        )
    };
    // A box with no extras at all can only come from a capture; fall back to the same stand-ins
    // the export uses, so "unchanged" means the same thing on both paths.
    let extras = |hb: &crate::data::Hitbox| {
        hb.search
            .clone()
            .unwrap_or_else(|| crate::data::Hitbox::default().to_search_call().extras)
    };
    let (was, now) = (extras(before), extras(after));
    let slots: [(usize, &'static str, bool, ArgValue); 14] = [
        (
            3,
            "bone",
            before.bone_name != after.bone_name,
            ArgValue::Text(bone),
        ),
        (
            4,
            "size",
            before.size != after.size,
            ArgValue::Float(after.size),
        ),
        (
            5,
            "x offset",
            before.offset_x != after.offset_x,
            ArgValue::Float(after.offset_x),
        ),
        (
            6,
            "y offset",
            before.offset_y != after.offset_y,
            ArgValue::Float(after.offset_y),
        ),
        (
            7,
            "z offset",
            before.offset_z != after.offset_z,
            ArgValue::Float(after.offset_z),
        ),
        (
            8,
            "capsule end",
            capsule_writable,
            ArgValue::Text(capsule(0)),
        ),
        (
            9,
            "capsule end",
            capsule_writable,
            ArgValue::Text(capsule(1)),
        ),
        (
            10,
            "capsule end",
            capsule_writable,
            ArgValue::Text(capsule(2)),
        ),
        konst(
            tail,
            "collision kind",
            &was.collision_kind,
            &now.collision_kind,
        ),
        konst(tail + 1, "hit status", &was.hit_status, &now.hit_status),
        konst(
            tail + 3,
            "situation mask",
            &before.situation_mask,
            &after.situation_mask,
        ),
        konst(
            tail + 4,
            "category mask",
            &before.category_mask,
            &after.category_mask,
        ),
        konst(tail + 5, "part mask", &before.part_mask, &after.part_mask),
        (
            1,
            "id",
            before.id != after.id,
            ArgValue::Int(after.id as i64),
        ),
    ];
    let (edits, mut missing) = apply_slots(text, site, &slots);
    // Same refusal `catch_edits` makes: adding a capsule to a call written without the slots
    // needs three arguments inserted, which a slot rewrite cannot do. Writing them anyway
    // would put `Some(1.0)` over `*COLLISION_KIND_MASK_ATTACK`.
    if capsule_changed && !has_capsule_slots && !missing.contains(&"capsule end") {
        missing.push("capsule end");
    }
    (edits, missing)
}

/// Replacements for the arguments of one `ATTACK_ABS` call that the user actually changed.
///
/// Its own table, and the slot numbers are *not* `ATTACK`'s. The two families name many of the
/// same properties — damage, angle, the knockback triple, hitlag, `lr_check`, the sound pair,
/// `attack_region` — in a different order and a different count, which is exactly the shape of
/// mistake that corrupts a call while still compiling.
///
/// Full layout (slot 0 is `agent`): 1 kind, 2 id, 3 damage, 4 angle, 5 kbg, 6 fkb, 7 bkb,
/// 8 hitlag, 9 unk, 10 lr_check, 11 unk2, 12 unk3, 13 collision_attr, 14 sound level, 15 sound
/// attr, 16 attack region. Slots 9, 11 and 12 are never written: they are invariant across the
/// corpus, undocumented, and the editor exposes no control for them.
fn attack_abs_edits(
    text: &str,
    site: &MacroSite,
    before: &crate::data::Hitbox,
    after: &crate::data::Hitbox,
) -> (Vec<Replacement>, Vec<&'static str>) {
    let konst = |slot: usize, field: &'static str, was: &str, now: &str| {
        (
            slot,
            field,
            was != now,
            ArgValue::Text(crate::acmd::const_expr(now)),
        )
    };
    let kind =
        |hb: &crate::data::Hitbox| hb.abs.as_ref().map(|a| a.kind.clone()).unwrap_or_default();
    let slots: [(usize, &'static str, bool, ArgValue); 13] = [
        konst(1, "absolute kind", &kind(before), &kind(after)),
        (
            2,
            "id",
            before.id != after.id,
            ArgValue::Int(after.id as i64),
        ),
        (
            3,
            "damage",
            before.damage != after.damage,
            ArgValue::Float(after.damage),
        ),
        (
            4,
            "angle",
            before.angle != after.angle,
            ArgValue::Int(after.angle as i64),
        ),
        (
            5,
            "knockback scaling",
            before.kb_scaling != after.kb_scaling,
            ArgValue::Int(after.kb_scaling as i64),
        ),
        (
            6,
            "fixed knockback",
            before.fkb != after.fkb,
            ArgValue::Int(after.fkb as i64),
        ),
        (
            7,
            "base knockback",
            before.kb_base != after.kb_base,
            ArgValue::Int(after.kb_base as i64),
        ),
        (
            8,
            "hitlag multiplier",
            before.hitlag_mult != after.hitlag_mult,
            ArgValue::Float(after.hitlag_mult),
        ),
        konst(10, "LR check", &before.lr_check, &after.lr_check),
        (
            13,
            "collision attribute",
            before.collision_attr != after.collision_attr,
            ArgValue::Text(format!("Hash40::new(\"{}\")", after.collision_attr)),
        ),
        konst(14, "sound level", &before.sound_level, &after.sound_level),
        konst(15, "sound attribute", &before.sound_attr, &after.sound_attr),
        konst(
            16,
            "attack region",
            &before.attack_region,
            &after.attack_region,
        ),
    ];
    apply_slots(text, site, &slots)
}

/// Replacements for the arguments of one `CATCH` call that the user actually changed.
///
/// `CATCH` numbers its arguments differently from `ATTACK` and has no `part`, no damage, and
/// no knockback — a grab box's editable properties are its joint, its size, its offsets, and
/// its capsule endpoint. Status and situation are not editable, so they are never written.
///
/// Slots 0..=6 mean the same thing in both of `CATCH`'s written shapes, so only the capsule
/// needs the form test — see [`crate::acmd::is_capsule_slot`]. In a call written without the
/// capsule arguments, slots 7 and 8 hold the status kind and the situation mask, and writing
/// `Some(1.0)` into them would both destroy the grab's behaviour and produce a file that does
/// not compile. Adding a capsule there needs three arguments *inserted*, which a slot rewrite
/// cannot do, so the change is reported instead — the same choice retimes get.
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
    let has_capsule_slots = site.arg(text, 7).is_some_and(crate::acmd::is_capsule_slot);
    let capsule_changed = before.capsule_end != after.capsule_end;
    let capsule_writable = capsule_changed && has_capsule_slots;
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
            capsule_writable,
            ArgValue::Text(capsule(0)),
        ),
        (
            8,
            "capsule end",
            capsule_writable,
            ArgValue::Text(capsule(1)),
        ),
        (
            9,
            "capsule end",
            capsule_writable,
            ArgValue::Text(capsule(2)),
        ),
    ];
    let (edits, mut missing) = apply_slots(text, site, &slots);
    if capsule_changed && !has_capsule_slots && !missing.contains(&"capsule end") {
        missing.push("capsule end");
    }
    (edits, missing)
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

// ── Hurtbox state write-back ─────────────────────────────────────────────────

/// The hurtbox commands, with the argument count each one has after `agent`.
///
/// The arity is carried here because it is part of *identifying* the call, not just of reading
/// it: the parser refuses a member written with the wrong number of arguments and leaves it as
/// a raw line, so a scanner that counted it anyway would number every site after it one too
/// high and retune the wrong call.
const HURT_COMMANDS: &[(&str, usize)] = &[
    ("HIT_NODE", 2),
    ("HIT_NO", 2),
    // One argument, not two: `WHOLE_HIT`'s target is the macro name. Listing it at 2 here would
    // exclude every real call from `hurt_sites` and shift every later site's ordinal.
    ("WHOLE_HIT", 1),
    ("HIT_RESET_ALL", 0),
    ("COL_PRI", 1),
    ("COL_NORMAL", 0),
];

/// The hurtbox calls in `text`, in document order — index `n` is site `n`.
///
/// This is the write-back's half of the correspondence [`crate::data::HurtSite`] defines. It
/// holds because a pre-order walk of the parsed script visits `is_excute` blocks, and the
/// statements inside them, in the order they appear in the file.
fn hurt_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| {
            HURT_COMMANDS
                .iter()
                .any(|(name, arity)| site.name == *name && site.args.len() == arity + 1)
        })
        .collect()
}

/// Rewrite edited hurtbox state and collision priority into the user's own source.
///
/// Value edits only, as everywhere on this path. The frame a state starts on is the block it
/// sits in rather than an argument, so a retime is reported; so is a change of target, since
/// `HIT_NODE` and `HIT_NO` are different macros taking different argument types and swapping
/// them is structure rather than value.
pub fn rewrite_hurtboxes(
    text: &str,
    label: &str,
    pristine: &(
        Vec<crate::data::HurtboxState>,
        Vec<crate::data::ColPriState>,
    ),
    edited: &(
        Vec<crate::data::HurtboxState>,
        Vec<crate::data::ColPriState>,
    ),
) -> Result<(String, SyncReport)> {
    use crate::data::HurtTarget;

    let sites = hurt_sites(text);
    let mut report = SyncReport::default();
    let mut edits = Vec::new();

    // Look the site up rather than pairing by position: a span list can be longer than the
    // statement list, because one looped call produces one span per iteration.
    let site_for = |site: usize, report: &mut SyncReport| -> Option<&MacroSite> {
        let found = sites.get(site);
        if found.is_none() {
            report.skipped.push(format!(
                "{label}: a hurtbox state has no matching call in the source — it was added in \
                 the editor, and source syncing only retunes existing calls"
            ));
        }
        found
    };

    for (before, now) in pristine.0.iter().zip(edited.0.iter()) {
        if before == now {
            continue;
        }
        let Some(site) = site_for(now.site, &mut report) else {
            continue;
        };
        if before.active_start != now.active_start {
            report.skipped.push(format!(
                "{label}: the `{}` on {} was retimed — its frame is the block it sits in, not an \
                 argument, so source syncing cannot move it",
                site.name,
                before.target.label()
            ));
        }
        // Retuning across a target change would write a bone hash into a group slot, which is
        // the cross-family corruption this codebase keeps finding the hard way.
        if std::mem::discriminant(&before.target) != std::mem::discriminant(&now.target) {
            report.skipped.push(format!(
                "{label}: {} became {} — that is a change from `{}` to `{}`, not a change of \
                 argument value",
                before.target.label(),
                now.target.label(),
                before.target.macro_name(),
                now.target.macro_name()
            ));
            continue;
        }
        match (&before.target, &now.target) {
            (HurtTarget::Bone(was), HurtTarget::Bone(is)) if was != is => {
                if let Some(span) = site.args.get(1) {
                    edits.extend(text_edit(
                        text,
                        span,
                        &format!("Hash40::new(\"{}\")", is.to_ascii_lowercase()),
                    ));
                }
            }
            (HurtTarget::Group(was), HurtTarget::Group(is)) if was != is => {
                if let Some(span) = site.args.get(1) {
                    edits.extend(int_edit(text, span, *is));
                }
            }
            _ => {}
        }
        if before.status != now.status {
            // Slot 1 is `agent`, so the status is slot 2 for the two targeted macros and slot 1
            // for `WHOLE_HIT`, whose target is its name. Writing 2 unconditionally would put the
            // new status past the end of a `WHOLE_HIT`'s arguments and silently drop the edit.
            let slot = if now.target.takes_target_argument() {
                2
            } else {
                1
            };
            if let Some(span) = site.args.get(slot) {
                edits.extend(text_edit(
                    text,
                    span,
                    &crate::acmd::emit_status(&now.status),
                ));
            }
        }
    }

    for (before, now) in pristine.1.iter().zip(edited.1.iter()) {
        if before == now {
            continue;
        }
        let Some(site) = site_for(now.site, &mut report) else {
            continue;
        };
        if before.active_start != now.active_start {
            report.skipped.push(format!(
                "{label}: a `COL_PRI` was retimed — its frame is the block it sits in, not an \
                 argument, so source syncing cannot move it"
            ));
        }
        if before.pri != now.pri {
            if let Some(span) = site.args.get(1) {
                edits.extend(int_edit(text, span, now.pri));
            }
        }
    }

    if pristine.0.len() != edited.0.len() || pristine.1.len() != edited.1.len() {
        report.skipped.push(format!(
            "{label}: hurtbox states were added or removed — source syncing only retunes \
             existing calls, so use an export to land that"
        ));
    }

    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

// ── Attack modifier write-back ───────────────────────────────────────────────

/// The post-hoc tuning commands, with the argument count each one has after `agent`.
///
/// Both take `(id, value)`, so unlike [`HURT_COMMANDS`] there is no per-macro asymmetry to get
/// wrong here. The arity is still part of identifying the call, for the same reason: the parser
/// leaves a wrong-arity call as `Raw`, and counting it anyway would number every later site one
/// too high and retune a different call.
///
/// `ATK_HIT_ABS` and `ATK_LERP_RATIO` are deliberately absent — they take no id, so they are not
/// this family and have no site here. See `TODO.md` B3.
const ATTACK_MOD_COMMANDS: &[(&str, usize)] = &[("ATK_POWER", 2), ("ATK_SET_SHIELD_SETOFF_MUL", 2)];

/// The attack-modifier calls in `text`, in document order — index `n` is site `n`.
///
/// Its own scan and its own numbering, not a share of [`hurt_sites`]: the two families are
/// counted separately in the parsed script too, so that adding an `ATK_POWER` to a move cannot
/// shift the site of a `HIT_NODE` that comes after it.
fn attack_mod_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| {
            ATTACK_MOD_COMMANDS
                .iter()
                .any(|(name, arity)| site.name == *name && site.args.len() == arity + 1)
        })
        .collect()
}

/// Rewrite edited post-hoc hitbox modifiers into the user's own source.
///
/// Value edits only, as everywhere on this path. A retime is reported rather than written, since
/// the frame is the block the call sits in and not an argument; so is a change of *kind*, which
/// is a different macro rather than a different value.
pub fn rewrite_attack_mods(
    text: &str,
    label: &str,
    pristine: &[crate::data::AttackModState],
    edited: &[crate::data::AttackModState],
) -> Result<(String, SyncReport)> {
    let sites = attack_mod_sites(text);
    let mut report = SyncReport::default();
    let mut edits = Vec::new();

    for (before, now) in pristine.iter().zip(edited.iter()) {
        if before == now {
            continue;
        }
        let Some(site) = sites.get(now.site) else {
            report.skipped.push(format!(
                "{label}: a `{}` has no matching call in the source — it was added in the \
                 editor, and source syncing only retunes existing calls",
                now.kind.macro_name()
            ));
            continue;
        };
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: the `{}` on hitbox {} was retimed — its frame is the block it sits in, \
                 not an argument, so source syncing cannot move it",
                before.kind.macro_name(),
                before.id
            ));
        }
        if before.kind != now.kind {
            report.skipped.push(format!(
                "{label}: a `{}` became a `{}` — that is a different macro, not a change of \
                 argument value",
                before.kind.macro_name(),
                now.kind.macro_name()
            ));
            continue;
        }
        // Slot 0 is `agent`, so the id is 1 and the value is 2 — the order `macros.rs` declares
        // and the order `ATK_POWER`'s two vanilla calls confirm by varying the id alone.
        if before.id != now.id {
            if let Some(span) = site.args.get(1) {
                edits.extend(int_edit(text, span, now.id));
            }
        }
        if before.value != now.value {
            if let Some(span) = site.args.get(2) {
                // `to_f32_edit`, not `float_edit`: these slots are `ToF32`-generic and every
                // vanilla call writes a bare integer, so a value of 7 must stay `7` and not
                // become `7.0` — the same rule `crate::acmd::attack_mod_num` follows on export.
                edits.extend(to_f32_edit(text, span, now.value));
            }
        }
    }

    if pristine.len() != edited.len() {
        report.skipped.push(format!(
            "{label}: hitbox modifiers were added or removed — source syncing only retunes \
             existing calls, so use an export to land that"
        ));
    }

    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited post-hoc hitbox modifiers for one move back into the project source on disk.
pub fn sync_attack_mods(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::AttackModState],
    edited: &[crate::data::AttackModState],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_attack_mods(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

// ── Playback rate write-back ─────────────────────────────────────────────────

/// The `FT_MOTION_RATE` calls in `text`, in document order — index `n` is site `n`.
///
/// **Name equality, not a prefix.** `FT_MOTION_RATE_RANGE` starts with this macro's entire name
/// and takes three arguments after `agent`; counting it here would number every later site one
/// too high and write a rate into an unrelated call. The arity filter says the same thing twice
/// on purpose, because both of these have gone wrong before in this codebase.
fn motion_rate_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| site.name == "FT_MOTION_RATE" && site.args.len() == 2)
        .collect()
}

/// Rewrite edited playback rates into the user's own source.
///
/// `edited` is the script's rate calls in script order, as
/// [`AcmdScript::motion_rate_sites`](crate::data::AcmdScript::motion_rate_sites) gives them.
/// **The source is its own pristine copy** — the value in the file is what the game plays — so
/// unlike the other families here this takes no separate baseline. That is possible because a
/// rate edit changes the argument and nothing else: it cannot renumber, retime, or change which
/// macro is called.
///
/// **The counts must agree, and this refuses rather than guessing when they do not.** The parser
/// only models a rate call at the function's top level, so one written inside a runtime branch is
/// kept as a raw line and never reaches `edited` — while `motion_rate_sites` above would still
/// count it. That mismatch is exactly the site-ordinal drift that silently retargets a later
/// edit, so it is reported instead.
pub fn rewrite_motion_rates(
    text: &str,
    label: &str,
    edited: &[(usize, f32, f32)],
) -> Result<(String, SyncReport)> {
    let sites = motion_rate_sites(text);
    let mut report = SyncReport::default();

    if sites.len() != edited.len() {
        report.skipped.push(format!(
            "{label}: the source has {} `FT_MOTION_RATE` call(s) and the editor models {} — one \
             of them is inside a branch, which this cannot address by position",
            sites.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (site, (_, _, rate)) in sites.iter().zip(edited.iter()) {
        // Slot 0 is `agent`, so the rate is slot 1 — the order `macros.rs` declares.
        if let Some(span) = site.args.get(1) {
            // `float_edit`, not `to_f32_edit`: every corpus call writes a decimal (`0.5`, `1.0`),
            // and the argument is `ToF32`-generic, so keeping the decimal form matches both the
            // source and what `acmd::num` emits.
            edits.extend(float_edit(text, span, *rate));
        }
    }

    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited playback rates for one move back into the project source on disk.
pub fn sync_motion_rates(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    edited: &[(usize, f32, f32)],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_motion_rates(body, &format!("{fighter}/{move_name}"), edited)
    })
}

// ── Kinetic speed write-back ────────────────────────────────────────────────

/// The buildable `SET_SPEED_EX` calls in source order.
pub(crate) fn speed_ex_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| site.name == "SET_SPEED_EX" && site.args.len() == 4)
        .collect()
}

/// Rewrite only the x/y values of existing `SET_SPEED_EX` calls.
///
/// The kinetic kind and frame are structural/source-owned values. A named kinetic constant is
/// deliberately not decoded to a guessed integer, and moving a call belongs to the source
/// editor/export path rather than this value-only synchronizer. Exact arity and site checks keep
/// malformed dump artifacts and loop-unrolled events from retargeting a later call.
pub fn rewrite_speed_ex(
    text: &str,
    label: &str,
    pristine: &[crate::data::SetSpeedExEvent],
    edited: &[crate::data::SetSpeedExEvent],
) -> Result<(String, SyncReport)> {
    let sites = speed_ex_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `SET_SPEED_EX` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `SET_SPEED_EX` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `SET_SPEED_EX` site {} was retimed from frame {} to {} — source syncing only retunes velocity values",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        if before.call.kinetic_kind != now.call.kinetic_kind {
            report.skipped.push(format!(
                "{label}: `SET_SPEED_EX` site {} changed kinetic kind — source syncing preserves that structural token",
                before.site
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::SetSpeedExCall::FUNC || site.args.len() != 4 {
            report.skipped.push(format!(
                "{label}: `SET_SPEED_EX` site {} no longer has its verified four-argument shape",
                before.site
            ));
            continue;
        }
        // Slot 0 is `agent`; x and y are the two following `ToF32` arguments.
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = to_f32_edit(text, span, now.call.speed_x) {
                edits.push(edit);
            }
        }
        if let Some(span) = site.args.get(2) {
            if let Some(edit) = to_f32_edit(text, span, now.call.speed_y) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited `SET_SPEED_EX` values into the project's `game_` function.
pub fn sync_speed_ex(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::SetSpeedExEvent],
    edited: &[crate::data::SetSpeedExEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_speed_ex(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The buildable direct `SET_SPEED` calls and generated helper calls in source order.
pub(crate) fn speed_sites(text: &str) -> Vec<MacroSite> {
    let mut sites = scan_macro_sites(text, 0..text.len());
    sites.extend(scan_named_sites(text, "visionary_set_speed", 0..text.len()));
    sites.sort_by_key(|site| site.span.start);
    sites
        .into_iter()
        .filter(|site| {
            matches!(site.name.as_str(), "SET_SPEED" | "visionary_set_speed")
                && site.args.len() == 3
        })
        .collect()
}

/// Rewrite only the x/y values of existing `SET_SPEED` calls.
pub fn rewrite_speed(
    text: &str,
    label: &str,
    pristine: &[crate::data::SetSpeedEvent],
    edited: &[crate::data::SetSpeedEvent],
) -> Result<(String, SyncReport)> {
    let sites = speed_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `SET_SPEED` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `SET_SPEED` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `SET_SPEED` site {} was retimed from frame {} to {} — source syncing only retunes velocity values",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if !matches!(
            site.name.as_str(),
            crate::data::SetSpeedCall::FUNC | "visionary_set_speed"
        ) || site.args.len() != 3
        {
            report.skipped.push(format!(
                "{label}: `SET_SPEED` site {} no longer has its verified three-argument shape",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = to_f32_edit(text, span, now.call.speed_x) {
                edits.push(edit);
            }
        }
        if let Some(span) = site.args.get(2) {
            if let Some(edit) = to_f32_edit(text, span, now.call.speed_y) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited `SET_SPEED` values into the project's `game_` function.
pub fn sync_speed(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::SetSpeedEvent],
    edited: &[crate::data::SetSpeedEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_speed(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The buildable `ADD_SPEED_NO_LIMIT` calls in source order.
pub(crate) fn add_speed_no_limit_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| site.name == "ADD_SPEED_NO_LIMIT" && site.args.len() == 3)
        .collect()
}

/// Rewrite only the x/y values of existing `ADD_SPEED_NO_LIMIT` calls.
pub fn rewrite_add_speed_no_limit(
    text: &str,
    label: &str,
    pristine: &[crate::data::AddSpeedNoLimitEvent],
    edited: &[crate::data::AddSpeedNoLimitEvent],
) -> Result<(String, SyncReport)> {
    let sites = add_speed_no_limit_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `ADD_SPEED_NO_LIMIT` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `ADD_SPEED_NO_LIMIT` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `ADD_SPEED_NO_LIMIT` site {} was retimed from frame {} to {} — source syncing only retunes velocity values",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::AddSpeedNoLimitCall::FUNC || site.args.len() != 3 {
            report.skipped.push(format!(
                "{label}: `ADD_SPEED_NO_LIMIT` site {} no longer has its verified three-argument shape",
                before.site
            ));
            continue;
        }
        // Slot 0 is `agent`; x and y are the following `ToF32` arguments.
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = to_f32_edit(text, span, now.call.speed_x) {
                edits.push(edit);
            }
        }
        if let Some(span) = site.args.get(2) {
            if let Some(edit) = to_f32_edit(text, span, now.call.speed_y) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited `ADD_SPEED_NO_LIMIT` values into the project's `game_` function.
pub fn sync_add_speed_no_limit(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::AddSpeedNoLimitEvent],
    edited: &[crate::data::AddSpeedNoLimitEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_add_speed_no_limit(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The buildable `CORRECT` calls in source order.
pub(crate) fn correct_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| site.name == "CORRECT" && site.args.len() == 2)
        .collect()
}

/// Rewrite the authored correction-kind token of existing `CORRECT` calls.
pub fn rewrite_correct(
    text: &str,
    label: &str,
    pristine: &[crate::data::CorrectEvent],
    edited: &[crate::data::CorrectEvent],
) -> Result<(String, SyncReport)> {
    let sites = correct_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `CORRECT` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `CORRECT` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `CORRECT` site {} was retimed from frame {} to {} — source syncing only retunes its authored kind token",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::CorrectCall::FUNC || site.args.len() != 2 {
            report.skipped.push(format!(
                "{label}: `CORRECT` site {} no longer has its verified two-argument shape",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.kind.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited `CORRECT` kind tokens into the project's `game_` function.
pub fn sync_correct(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::CorrectEvent],
    edited: &[crate::data::CorrectEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_correct(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The buildable `FT_CATCH_STOP` calls in source order.
pub(crate) fn ft_catch_stop_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| site.name == "FT_CATCH_STOP" && site.args.len() == 3)
        .collect()
}

/// Rewrite only the two numeric arguments of existing `FT_CATCH_STOP` calls.
pub fn rewrite_ft_catch_stop(
    text: &str,
    label: &str,
    pristine: &[crate::data::FtCatchStopEvent],
    edited: &[crate::data::FtCatchStopEvent],
) -> Result<(String, SyncReport)> {
    let sites = ft_catch_stop_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `FT_CATCH_STOP` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `FT_CATCH_STOP` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `FT_CATCH_STOP` site {} was retimed from frame {} to {} — source syncing only retunes its two numeric arguments",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::FtCatchStopCall::FUNC || site.args.len() != 3 {
            report.skipped.push(format!(
                "{label}: `FT_CATCH_STOP` site {} no longer has its verified three-argument shape",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = to_f32_edit(text, span, now.call.arg1) {
                edits.push(edit);
            }
        }
        if let Some(span) = site.args.get(2) {
            if let Some(edit) = to_f32_edit(text, span, now.call.arg2) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited `FT_CATCH_STOP` values into the project's `game_` function.
pub fn sync_ft_catch_stop(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::FtCatchStopEvent],
    edited: &[crate::data::FtCatchStopEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_ft_catch_stop(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The buildable `FT_START_ADJUST_MOTION_FRAME_arg1` calls in source order.
pub(crate) fn ft_start_adjust_motion_frame_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| site.name == "FT_START_ADJUST_MOTION_FRAME_arg1" && site.args.len() == 2)
        .collect()
}

/// Rewrite only the numeric payload of existing motion-frame adjustment calls.
pub fn rewrite_ft_start_adjust_motion_frame(
    text: &str,
    label: &str,
    pristine: &[crate::data::FtStartAdjustMotionFrameEvent],
    edited: &[crate::data::FtStartAdjustMotionFrameEvent],
) -> Result<(String, SyncReport)> {
    let sites = ft_start_adjust_motion_frame_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `FT_START_ADJUST_MOTION_FRAME_arg1` call(s), while the editor has {} pristine and {} edited point(s) — malformed or revised calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `FT_START_ADJUST_MOTION_FRAME_arg1` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `FT_START_ADJUST_MOTION_FRAME_arg1` site {} was retimed from frame {} to {} — source syncing only retunes its numeric value",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::FtStartAdjustMotionFrameCall::FUNC || site.args.len() != 2 {
            report.skipped.push(format!(
                "{label}: `FT_START_ADJUST_MOTION_FRAME_arg1` site {} no longer has its verified two-argument shape",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = to_f32_edit(text, span, now.call.value) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited motion-frame adjustment values into the project's `game_` function.
pub fn sync_ft_start_adjust_motion_frame(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::FtStartAdjustMotionFrameEvent],
    edited: &[crate::data::FtStartAdjustMotionFrameEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_ft_start_adjust_motion_frame(
            body,
            &format!("{fighter}/{move_name}"),
            pristine,
            edited,
        )
    })
}

/// The verified direct `MotionModule::set_rate` calls in source order.
pub(crate) fn motion_module_set_rate_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(
        text,
        crate::data::MotionModuleSetRateCall::FUNC,
        0..text.len(),
    )
    .into_iter()
    .filter(|site| {
        site.args.len() == 2
            && site
                .arg(text, 0)
                .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
            && site
                .arg(text, 1)
                .and_then(|value| value.trim().parse::<f32>().ok())
                .is_some_and(|value| value.is_finite() && value >= 0.0)
    })
    .collect()
}

/// Rewrite only the numeric payload of existing direct `MotionModule::set_rate` calls.
pub fn rewrite_motion_module_set_rate(
    text: &str,
    label: &str,
    pristine: &[crate::data::MotionModuleSetRateEvent],
    edited: &[crate::data::MotionModuleSetRateEvent],
) -> Result<(String, SyncReport)> {
    let sites = motion_module_set_rate_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `MotionModule::set_rate` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate` site {} was retimed from frame {} to {} — source syncing only retunes its numeric rate",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::MotionModuleSetRateCall::FUNC
            || site.args.len() != 2
            || site
                .arg(text, 0)
                .is_none_or(|value| !matches!(value.trim(), "agent.module_accessor" | "boma"))
        {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate` site {} no longer has its verified receiver/arity shape",
                before.site
            ));
            continue;
        }
        if !(now.call.rate.is_finite() && now.call.rate >= 0.0) {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate` site {} has a non-finite or negative rate — source syncing leaves the call intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = to_f32_edit(text, span, now.call.rate) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited direct `MotionModule::set_rate` values into the project's `game_` function.
pub fn sync_motion_module_set_rate(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::MotionModuleSetRateEvent],
    edited: &[crate::data::MotionModuleSetRateEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_motion_module_set_rate(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The verified direct `MotionModule::set_helper_calculation` calls in source order.
pub(crate) fn motion_module_set_helper_calculation_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(
        text,
        crate::data::MotionModuleSetHelperCalculationCall::FUNC,
        0..text.len(),
    )
    .into_iter()
    .filter(|site| {
        site.args.len() == 2
            && site
                .arg(text, 0)
                .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
            && site
                .arg(text, 1)
                .is_some_and(|value| matches!(value.trim(), "false" | "true"))
    })
    .collect()
}

/// Rewrite only the boolean payload of existing direct helper-calculation calls.
pub fn rewrite_motion_module_set_helper_calculation(
    text: &str,
    label: &str,
    pristine: &[crate::data::MotionModuleSetHelperCalculationEvent],
    edited: &[crate::data::MotionModuleSetHelperCalculationEvent],
) -> Result<(String, SyncReport)> {
    let sites = motion_module_set_helper_calculation_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `MotionModule::set_helper_calculation` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_helper_calculation` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_helper_calculation` site {} was retimed from frame {} to {} — source syncing only retunes its boolean value",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::MotionModuleSetHelperCalculationCall::FUNC
            || site.args.len() != 2
            || site
                .arg(text, 0)
                .is_none_or(|value| !matches!(value.trim(), "agent.module_accessor" | "boma"))
            || site
                .arg(text, 1)
                .is_none_or(|value| !matches!(value.trim(), "false" | "true"))
        {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_helper_calculation` site {} no longer has its verified receiver/arity/boolean shape",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = bool_edit(text, span, now.call.enabled) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited helper-calculation booleans into the project's `game_` function.
pub fn sync_motion_module_set_helper_calculation(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::MotionModuleSetHelperCalculationEvent],
    edited: &[crate::data::MotionModuleSetHelperCalculationEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_motion_module_set_helper_calculation(
            body,
            &format!("{fighter}/{move_name}"),
            pristine,
            edited,
        )
    })
}

fn valid_motion_module_part_kind_token(value: &str) -> bool {
    let value = value.trim();
    value.parse::<i64>().is_ok()
        || value.strip_prefix('*').is_some_and(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
}

/// The verified direct `MotionModule::set_rate_partial` calls in source order.
pub(crate) fn motion_module_set_rate_partial_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(
        text,
        crate::data::MotionModuleSetRatePartialCall::FUNC,
        0..text.len(),
    )
    .into_iter()
    .filter(|site| {
        site.args.len() == 3
            && site
                .arg(text, 0)
                .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
            && site
                .arg(text, 1)
                .is_some_and(valid_motion_module_part_kind_token)
            && site
                .arg(text, 2)
                .and_then(|value| value.trim().parse::<f32>().ok())
                .is_some_and(|value| value.is_finite() && value >= 0.0)
    })
    .collect()
}

/// Rewrite only the numeric rate of existing direct `MotionModule::set_rate_partial` calls.
pub fn rewrite_motion_module_set_rate_partial(
    text: &str,
    label: &str,
    pristine: &[crate::data::MotionModuleSetRatePartialEvent],
    edited: &[crate::data::MotionModuleSetRatePartialEvent],
) -> Result<(String, SyncReport)> {
    let sites = motion_module_set_rate_partial_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `MotionModule::set_rate_partial` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate_partial` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate_partial` site {} was retimed from frame {} to {} — source syncing only retunes its numeric rate",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        if before.call.part_kind != now.call.part_kind {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate_partial` site {} changed part kind — source syncing only retunes its numeric rate",
                before.site
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::MotionModuleSetRatePartialCall::FUNC
            || site.args.len() != 3
            || site
                .arg(text, 0)
                .is_none_or(|value| !matches!(value.trim(), "agent.module_accessor" | "boma"))
            || site
                .arg(text, 1)
                .is_none_or(|value| value.trim() != before.call.part_kind.trim())
        {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate_partial` site {} no longer has its verified receiver/part-kind/arity shape",
                before.site
            ));
            continue;
        }
        if !(now.call.rate.is_finite() && now.call.rate >= 0.0) {
            report.skipped.push(format!(
                "{label}: `MotionModule::set_rate_partial` site {} has a non-finite or negative rate — source syncing leaves the call intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(2) {
            if let Some(edit) = to_f32_edit(text, span, now.call.rate) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited direct `MotionModule::set_rate_partial` values into the project's `game_` function.
pub fn sync_motion_module_set_rate_partial(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::MotionModuleSetRatePartialEvent],
    edited: &[crate::data::MotionModuleSetRatePartialEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_motion_module_set_rate_partial(
            body,
            &format!("{fighter}/{move_name}"),
            pristine,
            edited,
        )
    })
}

/// Sync edited direct `MotionModule::set_rate_partial` values into the project's `expression_`
/// function. The rewrite is shared with the `game_` path; only the category anchor differs.
pub fn sync_expression_motion_module_set_rate_partial(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::MotionModuleSetRatePartialEvent],
    edited: &[crate::data::MotionModuleSetRatePartialEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("expression", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_motion_module_set_rate_partial(
            body,
            &format!("{fighter}/{move_name}"),
            pristine,
            edited,
        )
    })
}

// ── Kinetic point write-back ────────────────────────────────────────────────

/// The buildable `CLR_SPEED` calls and generated helper calls in source order.
pub(crate) fn clr_speed_sites(text: &str) -> Vec<MacroSite> {
    let mut sites = scan_macro_sites(text, 0..text.len());
    sites.extend(scan_named_sites(text, "visionary_clr_speed", 0..text.len()));
    sites.sort_by_key(|site| site.span.start);
    sites
        .into_iter()
        .filter(|site| {
            matches!(site.name.as_str(), "CLR_SPEED" | "visionary_clr_speed")
                && site.args.len() == 2
        })
        .collect()
}

/// Rewrite the authored kinetic-ID token of existing `CLR_SPEED` calls.
pub fn rewrite_clr_speed(
    text: &str,
    label: &str,
    pristine: &[crate::data::ClrSpeedEvent],
    edited: &[crate::data::ClrSpeedEvent],
) -> Result<(String, SyncReport)> {
    let sites = clr_speed_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `CLR_SPEED` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `CLR_SPEED` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `CLR_SPEED` site {} was retimed from frame {} to {} — source syncing only retunes its authored kinetic token",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if !matches!(
            site.name.as_str(),
            crate::data::ClrSpeedCall::FUNC | "visionary_clr_speed"
        ) || site.args.len() != 2
        {
            report.skipped.push(format!(
                "{label}: `CLR_SPEED` site {} no longer has its verified two-argument shape",
                before.site
            ));
            continue;
        }
        if now.call.kinetic_kind.trim().is_empty() {
            report.skipped.push(format!(
                "{label}: `CLR_SPEED` site {} has an empty kinetic kind — source syncing leaves the authored token intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.kinetic_kind.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited `CLR_SPEED` kinetic tokens into the project's `game_` function.
pub fn sync_clr_speed(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::ClrSpeedEvent],
    edited: &[crate::data::ClrSpeedEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_clr_speed(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// Whether a direct `KineticModule::change_kinetic` receiver is one of the measured source forms.
/// The HDR dump binds `boma` to `agent.boma()` at the top of the function; generated source uses
/// `agent.module_accessor`, so this receiver is normalized only for parsing and source syncing.
fn is_change_kinetic_receiver(value: &str) -> bool {
    matches!(value.trim(), "agent.module_accessor" | "boma")
}

/// The verified direct `KineticModule::change_kinetic` calls in source order.
pub(crate) fn change_kinetic_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(text, crate::data::ChangeKineticCall::FUNC, 0..text.len())
        .into_iter()
        .filter(|site| {
            site.args.len() == 2
                && site.arg(text, 0).is_some_and(is_change_kinetic_receiver)
                && site
                    .arg(text, 1)
                    .is_some_and(|value| !value.trim().is_empty())
        })
        .collect()
}

/// Rewrite only the authored kinetic-type token of an existing direct call.
pub fn rewrite_change_kinetic(
    text: &str,
    label: &str,
    pristine: &[crate::data::ChangeKineticEvent],
    edited: &[crate::data::ChangeKineticEvent],
) -> Result<(String, SyncReport)> {
    let sites = change_kinetic_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `KineticModule::change_kinetic` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `KineticModule::change_kinetic` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `KineticModule::change_kinetic` site {} was retimed from frame {} to {} — source syncing only retunes its authored kinetic token",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::ChangeKineticCall::FUNC || site.args.len() != 2 {
            report.skipped.push(format!(
                "{label}: `KineticModule::change_kinetic` site {} no longer has its verified two-argument shape",
                before.site
            ));
            continue;
        }
        if site
            .arg(text, 0)
            .is_none_or(|value| !is_change_kinetic_receiver(value))
        {
            report.skipped.push(format!(
                "{label}: `KineticModule::change_kinetic` site {} no longer targets a measured module receiver",
                before.site
            ));
            continue;
        }
        if now.call.kinetic_type.trim().is_empty() {
            report.skipped.push(format!(
                "{label}: `KineticModule::change_kinetic` site {} has an empty kinetic type — source syncing leaves the authored token intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.kinetic_type.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited kinetic-type tokens into the project's `game_` function.
pub fn sync_change_kinetic(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::ChangeKineticEvent],
    edited: &[crate::data::ChangeKineticEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_change_kinetic(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// Whether a direct kinetic-energy receiver is one of the measured source forms.
fn is_kinetic_energy_receiver(value: &str) -> bool {
    matches!(value.trim(), "agent.module_accessor" | "boma")
}

/// The verified direct kinetic-energy toggle calls in source order.
pub(crate) fn kinetic_energy_sites(text: &str) -> Vec<MacroSite> {
    let mut sites = Vec::new();
    for action in [
        crate::data::KineticEnergyAction::Suspend,
        crate::data::KineticEnergyAction::Resume,
        crate::data::KineticEnergyAction::Enable,
        crate::data::KineticEnergyAction::Unable,
    ] {
        sites.extend(scan_named_sites(text, action.func(), 0..text.len()));
    }
    sites.sort_by_key(|site| site.span.start);
    sites
        .into_iter()
        .filter(|site| {
            site.args.len() == 2
                && site.arg(text, 0).is_some_and(is_kinetic_energy_receiver)
                && site
                    .arg(text, 1)
                    .is_some_and(|value| !value.trim().is_empty())
        })
        .collect()
}

/// Rewrite only the authored energy-ID token of existing direct kinetic-energy calls. The source
/// receiver and operation remain structural; HDR's `boma` spelling is retained.
pub fn rewrite_kinetic_energy(
    text: &str,
    label: &str,
    pristine: &[crate::data::KineticEnergyEvent],
    edited: &[crate::data::KineticEnergyEvent],
) -> Result<(String, SyncReport)> {
    let sites = kinetic_energy_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable direct kinetic-energy call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: direct kinetic-energy site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: direct kinetic-energy site {} was retimed from frame {} to {} — source syncing only retunes its authored ID token",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        if before.call.action != now.call.action {
            report.skipped.push(format!(
                "{label}: direct kinetic-energy site {} changed between suspend and resume — source syncing preserves the authored operation",
                before.site
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != before.call.func()
            || site.args.len() != 2
            || site
                .arg(text, 0)
                .is_none_or(|value| !is_kinetic_energy_receiver(value))
            || site
                .arg(text, 1)
                .is_none_or(|value| value.trim().is_empty())
        {
            report.skipped.push(format!(
                "{label}: direct kinetic-energy site {} no longer has its verified receiver/operation/ID shape",
                before.site
            ));
            continue;
        }
        if now.call.kinetic_energy_id.trim().is_empty() {
            report.skipped.push(format!(
                "{label}: direct kinetic-energy site {} has an empty energy ID — source syncing leaves the authored token intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.kinetic_energy_id.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited direct kinetic-energy IDs into the project's `game_` function.
pub fn sync_kinetic_energy(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::KineticEnergyEvent],
    edited: &[crate::data::KineticEnergyEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_kinetic_energy(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

fn valid_work_flag_token(value: &str) -> bool {
    let value = value.trim();
    value.parse::<i64>().is_ok()
        || value.strip_prefix('*').is_some_and(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
}

fn valid_work_module_identifier(value: &str) -> bool {
    let mut chars = value.trim().chars();
    chars.next().is_some_and(|character| {
        (character.is_ascii_alphabetic() || character == '_')
            && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
    })
}

fn valid_work_module_slot_token(value: &str) -> bool {
    let value = value.trim();
    valid_work_flag_token(value)
        || value
            .strip_prefix('*')
            .is_some_and(valid_work_module_identifier)
        || valid_work_module_identifier(value)
}

fn valid_work_module_int64_value(value: &str) -> bool {
    let value = value.trim();
    valid_work_module_slot_token(value)
        || value
            .strip_prefix("hash40(\"")
            .and_then(|value| value.strip_suffix("\") as i64"))
            .is_some_and(|value| !value.is_empty() && !value.contains('"'))
}

/// The verified direct `WorkModule::on_flag` / `off_flag` calls in source order.
pub(crate) fn work_flag_sites(text: &str) -> Vec<MacroSite> {
    let mut sites = Vec::new();
    for action in [
        crate::data::WorkFlagAction::On,
        crate::data::WorkFlagAction::Off,
    ] {
        sites.extend(scan_named_sites(text, action.func(), 0..text.len()));
    }
    sites.sort_by_key(|site| site.span.start);
    sites
        .into_iter()
        .filter(|site| {
            site.args.len() == 2
                && site
                    .arg(text, 0)
                    .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
                && site.arg(text, 1).is_some_and(valid_work_flag_token)
        })
        .collect()
}

/// Rewrite only the authored flag token of existing direct WorkModule flag calls.
pub fn rewrite_work_flags(
    text: &str,
    label: &str,
    pristine: &[crate::data::WorkFlagEvent],
    edited: &[crate::data::WorkFlagEvent],
) -> Result<(String, SyncReport)> {
    let sites = work_flag_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable direct WorkModule flag call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: WorkModule flag site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: WorkModule flag site {} was retimed from frame {} to {} — source syncing only retunes its authored flag token",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        if before.call.action != now.call.action {
            report.skipped.push(format!(
                "{label}: WorkModule flag site {} changed between on_flag and off_flag — source syncing preserves the authored operation",
                before.site
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != before.call.func()
            || site.args.len() != 2
            || site
                .arg(text, 0)
                .is_none_or(|value| !matches!(value.trim(), "agent.module_accessor" | "boma"))
            || site
                .arg(text, 1)
                .is_none_or(|value| value.trim() != before.call.flag.trim())
        {
            report.skipped.push(format!(
                "{label}: WorkModule flag site {} no longer has its verified receiver/operation/flag shape",
                before.site
            ));
            continue;
        }
        if !valid_work_flag_token(&now.call.flag) {
            report.skipped.push(format!(
                "{label}: WorkModule flag site {} has an unsupported or empty flag token — source syncing leaves the authored token intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.flag.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited WorkModule flag tokens into the project's `game_` function.
pub fn sync_work_flags(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::WorkFlagEvent],
    edited: &[crate::data::WorkFlagEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_work_flags(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// Return whether a direct transition-term operation uses a measured receiver spelling.
fn valid_work_transition_term_receiver(function: &str, receiver: &str) -> bool {
    match function {
        "WorkModule::enable_transition_term_group"
        | "WorkModule::unable_transition_term_group_ex" => receiver == "agent.module_accessor",
        "WorkModule::enable_transition_term" | "WorkModule::unable_transition_term" => {
            matches!(receiver, "agent.module_accessor" | "boma")
        }
        _ => false,
    }
}

/// The verified direct WorkModule transition-term and transition-term-group calls in source
/// order.
pub(crate) fn work_transition_term_sites(text: &str) -> Vec<MacroSite> {
    let mut sites = Vec::new();
    for action in [
        crate::data::WorkTransitionTermAction::Enable,
        crate::data::WorkTransitionTermAction::Unable,
        crate::data::WorkTransitionTermAction::EnableGroup,
        crate::data::WorkTransitionTermAction::UnableGroupEx,
    ] {
        sites.extend(scan_named_sites(text, action.func(), 0..text.len()));
    }
    sites.sort_by_key(|site| site.span.start);
    sites
        .into_iter()
        .filter(|site| {
            site.args.len() == 2
                && site.arg(text, 0).is_some_and(|value| {
                    valid_work_transition_term_receiver(&site.name, value.trim())
                })
                && site.arg(text, 1).is_some_and(valid_work_flag_token)
        })
        .collect()
}

/// Rewrite only the authored transition-term or group token of existing direct WorkModule calls.
pub fn rewrite_work_transition_terms(
    text: &str,
    label: &str,
    pristine: &[crate::data::WorkTransitionTermEvent],
    edited: &[crate::data::WorkTransitionTermEvent],
) -> Result<(String, SyncReport)> {
    let sites = work_transition_term_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable direct WorkModule transition-term call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: WorkModule transition-term site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: WorkModule transition-term site {} was retimed from frame {} to {} — source syncing only retunes its authored transition term",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        if before.call.action != now.call.action {
            report.skipped.push(format!(
                "{label}: WorkModule transition-term site {} changed between enable and unable — source syncing preserves the authored operation",
                before.site
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != before.call.func()
            || site.args.len() != 2
            || site
                .arg(text, 0)
                .is_none_or(|value| !valid_work_transition_term_receiver(&site.name, value.trim()))
            || site
                .arg(text, 1)
                .is_none_or(|value| value.trim() != before.call.transition_term.trim())
        {
            report.skipped.push(format!(
                "{label}: WorkModule transition-term site {} no longer has its verified receiver/operation/term shape",
                before.site
            ));
            continue;
        }
        if !valid_work_flag_token(&now.call.transition_term) {
            report.skipped.push(format!(
                "{label}: WorkModule transition-term site {} has an unsupported or empty transition term — source syncing leaves the authored token intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.transition_term.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited WorkModule transition-term tokens into the project's `game_` function.
pub fn sync_work_transition_terms(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::WorkTransitionTermEvent],
    edited: &[crate::data::WorkTransitionTermEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_work_transition_terms(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The verified direct `WorkModule::inc_int` calls in source order. Standard scripts use
/// `agent.module_accessor`; HDR scripts use `boma`.
pub(crate) fn work_module_inc_int_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(text, crate::data::WorkModuleIncIntCall::FUNC, 0..text.len())
        .into_iter()
        .filter(|site| {
            site.args.len() == 2
                && site
                    .arg(text, 0)
                    .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
                && site.arg(text, 1).is_some_and(valid_work_flag_token)
        })
        .collect()
}

/// Rewrite only the authored slot token of existing direct `WorkModule::inc_int` calls.
pub fn rewrite_work_module_inc_int(
    text: &str,
    label: &str,
    pristine: &[crate::data::WorkModuleIncIntEvent],
    edited: &[crate::data::WorkModuleIncIntEvent],
) -> Result<(String, SyncReport)> {
    let sites = work_module_inc_int_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable direct WorkModule::inc_int call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: WorkModule::inc_int site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: WorkModule::inc_int site {} was retimed from frame {} to {} — source syncing only retunes its authored slot",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::WorkModuleIncIntCall::FUNC
            || site.args.len() != 2
            || site
                .arg(text, 0)
                .is_none_or(|value| !matches!(value.trim(), "agent.module_accessor" | "boma"))
            || site
                .arg(text, 1)
                .is_none_or(|value| value.trim() != before.call.slot.trim())
        {
            report.skipped.push(format!(
                "{label}: WorkModule::inc_int site {} no longer has its verified receiver/arity/slot shape",
                before.site
            ));
            continue;
        }
        if !valid_work_flag_token(&now.call.slot) {
            report.skipped.push(format!(
                "{label}: WorkModule::inc_int site {} has an unsupported or empty slot token — source syncing leaves the authored call intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.slot.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited direct `WorkModule::inc_int` slot tokens into the project's `game_` function.
pub fn sync_work_module_inc_int(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::WorkModuleIncIntEvent],
    edited: &[crate::data::WorkModuleIncIntEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_work_module_inc_int(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The verified direct `WorkModule::set_int` / `set_float` / `set_int64` calls in source order.
/// The set_int64 corpus is measured in both standard `agent.module_accessor` and HDR `boma`
/// receiver forms; preserving the receiver keeps export and source sync lossless.
pub(crate) fn work_module_set_sites(text: &str) -> Vec<MacroSite> {
    let mut sites = Vec::new();
    for kind in [
        crate::data::WorkModuleSetKind::Int,
        crate::data::WorkModuleSetKind::Float,
        crate::data::WorkModuleSetKind::Int64,
    ] {
        sites.extend(scan_named_sites(text, kind.func(), 0..text.len()));
    }
    sites.sort_by_key(|site| site.span.start);
    sites
        .into_iter()
        .filter(|site| {
            let kind = match site.name.as_str() {
                "WorkModule::set_int" => crate::data::WorkModuleSetKind::Int,
                "WorkModule::set_float" => crate::data::WorkModuleSetKind::Float,
                "WorkModule::set_int64" => crate::data::WorkModuleSetKind::Int64,
                _ => return false,
            };
            site.args.len() == 3
                && site
                    .arg(text, 0)
                    .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
                && site
                    .arg(text, 1)
                    .is_some_and(|value| valid_work_module_set_value(value, kind))
                && site.arg(text, 2).is_some_and(|value| {
                    if kind.is_int64() {
                        valid_work_module_slot_token(value)
                    } else {
                        valid_work_flag_token(value)
                    }
                })
        })
        .collect()
}

fn valid_work_module_set_value(value: &str, kind: crate::data::WorkModuleSetKind) -> bool {
    let value = value.trim();
    if kind.is_float() {
        value.parse::<f32>().is_ok_and(f32::is_finite)
    } else if kind.is_int64() {
        valid_work_module_int64_value(value)
    } else {
        valid_work_flag_token(value)
    }
}

/// Rewrite only authored value and slot tokens of existing direct WorkModule setters.
pub fn rewrite_work_module_sets(
    text: &str,
    label: &str,
    pristine: &[crate::data::WorkModuleSetEvent],
    edited: &[crate::data::WorkModuleSetEvent],
) -> Result<(String, SyncReport)> {
    let sites = work_module_set_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable direct WorkModule set call(s), while the editor has {} pristine and {} edited point(s) — malformed or unmeasured calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: WorkModule set site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: WorkModule set site {} was retimed from frame {} to {} — source syncing only retunes authored value/slot tokens",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        if before.call.kind != now.call.kind {
            report.skipped.push(format!(
                "{label}: WorkModule set site {} changed setter operation — source syncing preserves the authored operation",
                before.site
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != before.call.func()
            || site.args.len() != 3
            || site
                .arg(text, 0)
                .is_none_or(|value| value.trim() != before.call.receiver.trim())
            || site
                .arg(text, 1)
                .is_none_or(|value| value.trim() != before.call.value.trim())
            || site
                .arg(text, 2)
                .is_none_or(|value| value.trim() != before.call.slot.trim())
        {
            report.skipped.push(format!(
                "{label}: WorkModule set site {} no longer has its verified receiver/operation/value/slot shape",
                before.site
            ));
            continue;
        }
        if !matches!(now.call.receiver.trim(), "agent.module_accessor" | "boma")
            || !valid_work_module_set_value(&now.call.value, now.call.kind)
            || if now.call.kind.is_int64() {
                !valid_work_module_slot_token(&now.call.slot)
            } else {
                !valid_work_flag_token(&now.call.slot)
            }
        {
            report.skipped.push(format!(
                "{label}: WorkModule set site {} has an unsupported or empty value/slot token — source syncing leaves the authored call intact",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            if let Some(edit) = text_edit(text, span, now.call.value.trim()) {
                edits.push(edit);
            }
        }
        if let Some(span) = site.args.get(2) {
            if let Some(edit) = text_edit(text, span, now.call.slot.trim()) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited WorkModule value setters into the project's `game_` function.
pub fn sync_work_module_sets(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::WorkModuleSetEvent],
    edited: &[crate::data::WorkModuleSetEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_work_module_sets(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// Whether a direct `KineticModule::add_speed` receiver is one of the measured source forms.
fn is_kinetic_add_speed_receiver(value: &str) -> bool {
    matches!(value.trim(), "agent.module_accessor" | "boma")
}

fn parse_source_zero_z_vector(value: &str) -> Option<(f32, f32)> {
    let value = value.trim();
    let value = value.strip_prefix('&')?.trim_start();
    let value = value.strip_prefix("Vector3f")?.trim_start();
    let value = value.strip_prefix('{')?.strip_suffix('}')?;
    let fields = split_source_fields(value);
    let [x, y, z] = fields.as_slice() else {
        return None;
    };
    let (x_name, x_value) = x.split_once(':')?;
    let (y_name, y_value) = y.split_once(':')?;
    let (z_name, z_value) = z.split_once(':')?;
    if x_name.trim() != "x" || y_name.trim() != "y" || z_name.trim() != "z" {
        return None;
    }
    let speed_x = x_value.trim().parse::<f32>().ok()?;
    let speed_y = y_value.trim().parse::<f32>().ok()?;
    let speed_z = z_value.trim().parse::<f32>().ok()?;
    (speed_z.is_finite() && speed_z == 0.0).then_some((speed_x, speed_y))
}

fn split_source_fields(text: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                fields.push(text[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    if !text[start..].trim().is_empty() {
        fields.push(text[start..].trim());
    }
    fields
}

/// The verified direct `KineticModule::add_speed` calls in source order.
pub(crate) fn kinetic_add_speed_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(text, crate::data::KineticAddSpeedCall::FUNC, 0..text.len())
        .into_iter()
        .filter(|site| {
            site.args.len() == 2
                && site.arg(text, 0).is_some_and(is_kinetic_add_speed_receiver)
                && site
                    .arg(text, 1)
                    .and_then(parse_source_zero_z_vector)
                    .is_some()
        })
        .collect()
}

/// Rewrite the measured x/y components of existing direct kinetic-vector additions. The source
/// receiver is retained (`agent.module_accessor` or HDR's `boma`); only the verified vector
/// argument is replaced, with the measured zero z component made explicit.
pub fn rewrite_kinetic_add_speed(
    text: &str,
    label: &str,
    pristine: &[crate::data::KineticAddSpeedEvent],
    edited: &[crate::data::KineticAddSpeedEvent],
) -> Result<(String, SyncReport)> {
    let sites = kinetic_add_speed_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != sites.len() || edited.len() != pristine.len() {
        report.skipped.push(format!(
            "{label}: source has {} buildable `KineticModule::add_speed` call(s), while the editor has {} pristine and {} edited point(s) — malformed or looped calls are source-only",
            sites.len(),
            pristine.len(),
            edited.len()
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits = Vec::new();
    for (index, (before, now)) in pristine.iter().zip(edited).enumerate() {
        if before == now {
            continue;
        }
        if before.site != index || now.site != index {
            report.skipped.push(format!(
                "{label}: `KineticModule::add_speed` site {} does not match the flat source order — source syncing refuses positional guessing",
                before.site
            ));
            continue;
        }
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `KineticModule::add_speed` site {} was retimed from frame {} to {} — source syncing only retunes its authored vector",
                before.site, before.frame, now.frame
            ));
            continue;
        }
        let Some(site) = sites.get(index) else {
            continue;
        };
        if site.name != crate::data::KineticAddSpeedCall::FUNC
            || site.args.len() != 2
            || site
                .arg(text, 0)
                .is_none_or(|value| !is_kinetic_add_speed_receiver(value))
            || site
                .arg(text, 1)
                .and_then(parse_source_zero_z_vector)
                .is_none()
        {
            report.skipped.push(format!(
                "{label}: `KineticModule::add_speed` site {} no longer has its verified receiver/vector shape",
                before.site
            ));
            continue;
        }
        if let Some(span) = site.args.get(1) {
            let value = format!(
                "&Vector3f{{x: {}, y: {}, z: 0.0}}",
                crate::acmd::num(now.call.speed_x),
                crate::acmd::num(now.call.speed_y)
            );
            if let Some(edit) = text_edit(text, span, &value) {
                edits.push(edit);
            }
        }
    }
    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Sync edited direct kinetic-vector additions into the project's `game_` function.
pub fn sync_kinetic_add_speed(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::KineticAddSpeedEvent],
    edited: &[crate::data::KineticAddSpeedEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_kinetic_add_speed(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// The measured direct `KineticModule::set_consider_ground_friction` calls in source order.
pub(crate) fn kinetic_set_consider_ground_friction_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(
        text,
        crate::data::KineticSetConsiderGroundFrictionCall::FUNC,
        0..text.len(),
    )
    .into_iter()
    .filter(|site| {
        site.args.len() == 3
            && site
                .arg(text, 0)
                .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
            && site
                .arg(text, 1)
                .is_some_and(|value| matches!(value.trim(), "true" | "false"))
            && site
                .arg(text, 2)
                .is_some_and(|value| !value.trim().is_empty())
    })
    .collect()
}

fn remove_kinetic_set_consider_ground_friction_line(text: &str, site: &MacroSite) -> Replacement {
    let ranges = source_line_ranges(text);
    let Some(line) = ranges
        .iter()
        .find(|range| range.start <= site.span.start && site.span.start < range.end)
    else {
        return Replacement {
            span: site.span.clone(),
            value: String::new(),
        };
    };
    let trimmed = text[line.clone()].trim();
    if trimmed == "KineticModule::set_consider_ground_friction(agent.module_accessor, true, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);"
        || trimmed == "KineticModule::set_consider_ground_friction(agent.module_accessor, false, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);"
        || trimmed == "KineticModule::set_consider_ground_friction(boma, true, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);"
        || trimmed == "KineticModule::set_consider_ground_friction(boma, false, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);"
    {
        Replacement {
            span: line.clone(),
            value: String::new(),
        }
    } else {
        let mut end = site.span.end;
        if text[end..].starts_with(';') {
            end += 1;
        }
        Replacement {
            span: site.span.start..end,
            value: String::new(),
        }
    }
}

fn insert_kinetic_set_consider_ground_friction_line(
    text: &mut String,
    frame: u32,
    call: &crate::data::KineticSetConsiderGroundFrictionCall,
) -> bool {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let receiver = if text.contains("let boma = agent.boma()") {
        "boma"
    } else {
        "agent.module_accessor"
    };
    let call = format!(
        "KineticModule::set_consider_ground_friction({receiver}, {}, {});",
        call.consider_ground_friction, call.kinetic_energy_attribute
    );
    let ranges = source_line_ranges(text);
    let frame_index = ranges.iter().position(|range| {
        frame_literal(&text[range.clone()])
            .is_some_and(|value| value.round().max(1.0) as u32 == frame)
    });

    if let Some(frame_index) = frame_index {
        let frame_range = ranges[frame_index].clone();
        for range in ranges.iter().skip(frame_index + 1) {
            if frame_literal(&text[range.clone()]).is_some() {
                break;
            }
            let line = &text[range.clone()];
            if line.contains("if macros::is_excute") && line.contains('{') {
                let open = range.start + line.find('{').unwrap();
                if let Some(close) = matching_brace(text, open) {
                    let body_indent = ranges
                        .iter()
                        .skip(frame_index + 1)
                        .find(|body| {
                            body.start > open && body.start < close && {
                                let body_text = &text[body.start..body.end];
                                !body_text.trim().is_empty() && !body_text.trim().starts_with('}')
                            }
                        })
                        .map(|body| line_indent(text, body).to_string())
                        .unwrap_or_else(|| format!("{}    ", line_indent(text, range)));
                    text.insert_str(close, &format!("{body_indent}{call}{newline}"));
                    return true;
                }
            }
        }

        let indent = line_indent(text, &frame_range);
        let block = format!(
            "{indent}if macros::is_excute(agent) {{{newline}{indent}    {call}{newline}{indent}}}{newline}"
        );
        text.insert_str(frame_range.end, &block);
        return true;
    }

    let insert_at = ranges
        .iter()
        .find(|range| {
            frame_literal(&text[range.start..range.end]).is_some_and(|value| value > frame as f32)
        })
        .map(|range| range.start)
        .or_else(|| text.rfind('}'))
        .unwrap_or(text.len());
    let indent = ranges
        .iter()
        .find(|range| frame_literal(&text[range.start..range.end]).is_some())
        .map(|range| line_indent(text, range).to_string())
        .unwrap_or_else(|| "    ".into());
    let block = format!(
        "{indent}frame(agent.lua_state_agent, {frame}.0);{newline}{indent}if macros::is_excute(agent) {{{newline}{indent}    {call}{newline}{indent}}}{newline}"
    );
    text.insert_str(insert_at, &block);
    true
}

/// Rewrite direct ground-friction points in a flat `game_` function. Existing values are edited
/// in place; flat additions/removals/retimes use the same frame-ordered structural policy as
/// the argument-less direct kinetic point. Branches, loops, and source-site mismatches are
/// reported instead of guessed into an execution context.
pub fn rewrite_kinetic_set_consider_ground_friction(
    text: &str,
    label: &str,
    pristine: &[crate::data::KineticSetConsiderGroundFrictionEvent],
    edited: &[crate::data::KineticSetConsiderGroundFrictionEvent],
) -> Result<(String, SyncReport)> {
    let sites = kinetic_set_consider_ground_friction_sites(text);
    let parsed = crate::acmd::parse_acmd_script(text);
    fn contains_loop(stmts: &[crate::data::AcmdStmt]) -> bool {
        stmts.iter().any(|stmt| match stmt {
            crate::data::AcmdStmt::Loop { .. } => true,
            crate::data::AcmdStmt::RawBlock { body, .. } => contains_loop(body),
            _ => false,
        })
    }
    let mut report = SyncReport::default();
    let flat_sites = pristine.len() == sites.len()
        && pristine
            .iter()
            .enumerate()
            .all(|(index, event)| event.site == index);
    let flat_edited = edited
        .iter()
        .enumerate()
        .all(|(index, event)| event.site == index);
    if parsed.branch_count() > 0 || contains_loop(&parsed.stmts) || !flat_sites || !flat_edited {
        if pristine != edited {
            report.skipped.push(format!(
                "{label}: set_consider_ground_friction placement changed inside a loop/branch or after a source-site mismatch — source syncing only edits flat point calls"
            ));
        }
        return Ok((text.to_string(), report));
    }

    let old: Vec<u32> = pristine.iter().map(|event| event.frame).collect();
    let new: Vec<u32> = edited.iter().map(|event| event.frame).collect();
    let n = old.len();
    let m = new.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut matched_old = vec![false; n];
    let mut matched_new = vec![false; m];
    let mut matched_pairs = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            matched_old[i] = true;
            matched_new[j] = true;
            matched_pairs.push((i, j));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let mut edits = Vec::new();
    for (old_index, new_index) in &matched_pairs {
        let before = &pristine[*old_index];
        let now = &edited[*new_index];
        if before == now {
            continue;
        }
        let Some(site) = sites.get(*old_index) else {
            continue;
        };
        if site.name != crate::data::KineticSetConsiderGroundFrictionCall::FUNC
            || site.args.len() != 3
            || site
                .arg(text, 0)
                .is_none_or(|value| !matches!(value.trim(), "agent.module_accessor" | "boma"))
        {
            report.skipped.push(format!(
                "{label}: set_consider_ground_friction site {} no longer has its verified receiver/arity shape",
                before.site
            ));
            continue;
        }
        if before.call.consider_ground_friction != now.call.consider_ground_friction {
            if let Some(span) = site.args.get(1) {
                if let Some(edit) = text_edit(
                    text,
                    span,
                    if now.call.consider_ground_friction {
                        "true"
                    } else {
                        "false"
                    },
                ) {
                    edits.push(edit);
                }
            }
        }
        if before.call.kinetic_energy_attribute != now.call.kinetic_energy_attribute {
            if now.call.kinetic_energy_attribute.trim().is_empty() {
                report.skipped.push(format!(
                    "{label}: set_consider_ground_friction site {} has an empty reserve attribute — source syncing leaves the authored token intact",
                    before.site
                ));
            } else if let Some(span) = site.args.get(2) {
                if let Some(edit) = text_edit(text, span, now.call.kinetic_energy_attribute.trim())
                {
                    edits.push(edit);
                }
            }
        }
    }
    if matched_old.iter().all(|matched| *matched) && matched_new.iter().all(|matched| *matched) {
        report.changed = edits.len();
        return Ok((apply(text, edits), report));
    }

    let removals: Vec<Replacement> = sites
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_old[*index])
        .map(|(_, site)| remove_kinetic_set_consider_ground_friction_line(text, site))
        .collect();
    edits.extend(removals);
    let mut updated = apply(text, edits);
    report.changed += matched_old.iter().filter(|matched| !**matched).count();
    report.changed += matched_pairs
        .iter()
        .filter(|(old_index, new_index)| pristine[*old_index] != edited[*new_index])
        .count();
    for (index, event) in edited
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_new[*index])
    {
        if insert_kinetic_set_consider_ground_friction_line(&mut updated, event.frame, &event.call)
        {
            report.changed += 1;
        } else {
            report.skipped.push(format!(
                "{label}: could not insert set_consider_ground_friction point {} at frame {}",
                index, event.frame
            ));
        }
    }
    Ok((updated, report))
}

/// Sync edited direct ground-friction points into the project's `game_` function.
pub fn sync_kinetic_set_consider_ground_friction(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::KineticSetConsiderGroundFrictionEvent],
    edited: &[crate::data::KineticSetConsiderGroundFrictionEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_kinetic_set_consider_ground_friction(
            body,
            &format!("{fighter}/{move_name}"),
            pristine,
            edited,
        )
    })
}

/// The buildable argument-less `SET_AIR` calls and generated helper calls in source order.
pub(crate) fn set_air_sites(text: &str) -> Vec<MacroSite> {
    let mut sites = scan_macro_sites(text, 0..text.len());
    sites.extend(scan_named_sites(text, "visionary_set_air", 0..text.len()));
    sites.sort_by_key(|site| site.span.start);
    sites
        .into_iter()
        .filter(|site| {
            matches!(site.name.as_str(), "SET_AIR" | "visionary_set_air") && site.args.len() == 1
        })
        .collect()
}

/// The buildable direct `KineticModule::clear_speed_all` calls in source order.
pub(crate) fn kinetic_clear_speed_all_sites(text: &str) -> Vec<MacroSite> {
    scan_named_sites(
        text,
        crate::data::KineticClearSpeedAllCall::FUNC,
        0..text.len(),
    )
    .into_iter()
    .filter(|site| {
        site.args.len() == 1
            && site
                .arg(text, 0)
                .is_some_and(|value| matches!(value.trim(), "agent.module_accessor" | "boma"))
    })
    .collect()
}

fn remove_set_air_line(text: &str, site: &MacroSite) -> Replacement {
    let ranges = source_line_ranges(text);
    let Some(line) = ranges
        .iter()
        .find(|range| range.start <= site.span.start && site.span.start < range.end)
    else {
        return Replacement {
            span: site.span.clone(),
            value: String::new(),
        };
    };
    let trimmed = text[line.clone()].trim();
    if trimmed == "macros::SET_AIR(agent);" || trimmed == "visionary_set_air(agent);" {
        Replacement {
            span: line.clone(),
            value: String::new(),
        }
    } else {
        let mut end = site.span.end;
        if text[end..].starts_with(';') {
            end += 1;
        }
        Replacement {
            span: site.span.start..end,
            value: String::new(),
        }
    }
}

fn insert_set_air_line(text: &mut String, frame: u32) -> bool {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let ranges = source_line_ranges(text);
    let frame_index = ranges.iter().position(|range| {
        frame_literal(&text[range.clone()])
            .is_some_and(|value| value.round().max(1.0) as u32 == frame)
    });

    if let Some(frame_index) = frame_index {
        let frame_range = ranges[frame_index].clone();
        for range in ranges.iter().skip(frame_index + 1) {
            if frame_literal(&text[range.clone()]).is_some() {
                break;
            }
            let line = &text[range.clone()];
            if line.contains("if macros::is_excute") && line.contains('{') {
                let open = range.start + line.find('{').unwrap();
                if let Some(close) = matching_brace(text, open) {
                    let body_indent = ranges
                        .iter()
                        .skip(frame_index + 1)
                        .find(|body| {
                            body.start > open && body.start < close && {
                                let body_text = &text[body.start..body.end];
                                !body_text.trim().is_empty() && !body_text.trim().starts_with('}')
                            }
                        })
                        .map(|body| line_indent(text, body).to_string())
                        .unwrap_or_else(|| format!("{}    ", line_indent(text, range)));
                    text.insert_str(
                        close,
                        &format!("{body_indent}macros::SET_AIR(agent);{newline}"),
                    );
                    return true;
                }
            }
        }

        let indent = line_indent(text, &frame_range);
        let block = format!(
            "{indent}if macros::is_excute(agent) {{{newline}{indent}    macros::SET_AIR(agent);{newline}{indent}}}{newline}"
        );
        text.insert_str(frame_range.end, &block);
        return true;
    }

    let insert_at = ranges
        .iter()
        .find(|range| {
            frame_literal(&text[range.start..range.end]).is_some_and(|value| value > frame as f32)
        })
        .map(|range| range.start)
        .or_else(|| text.rfind('}'))
        .unwrap_or(text.len());
    let indent = ranges
        .iter()
        .find(|range| frame_literal(&text[range.start..range.end]).is_some())
        .map(|range| line_indent(text, range).to_string())
        .unwrap_or_else(|| "    ".into());
    let block = format!(
        "{indent}frame(agent.lua_state_agent, {frame}.0);{newline}{indent}if macros::is_excute(agent) {{{newline}{indent}    macros::SET_AIR(agent);{newline}{indent}}}{newline}"
    );
    text.insert_str(insert_at, &block);
    true
}

fn remove_kinetic_clear_speed_all_line(text: &str, site: &MacroSite) -> Replacement {
    let ranges = source_line_ranges(text);
    let Some(line) = ranges
        .iter()
        .find(|range| range.start <= site.span.start && site.span.start < range.end)
    else {
        return Replacement {
            span: site.span.clone(),
            value: String::new(),
        };
    };
    let trimmed = text[line.clone()].trim();
    if matches!(
        trimmed,
        "KineticModule::clear_speed_all(agent.module_accessor);"
            | "KineticModule::clear_speed_all(boma);"
    ) {
        Replacement {
            span: line.clone(),
            value: String::new(),
        }
    } else {
        let mut end = site.span.end;
        if text[end..].starts_with(';') {
            end += 1;
        }
        Replacement {
            span: site.span.start..end,
            value: String::new(),
        }
    }
}

fn insert_kinetic_clear_speed_all_line(text: &mut String, frame: u32) -> bool {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let call = if text.contains("let boma = agent.boma()") {
        "KineticModule::clear_speed_all(boma);"
    } else {
        "KineticModule::clear_speed_all(agent.module_accessor);"
    };
    let ranges = source_line_ranges(text);
    let frame_index = ranges.iter().position(|range| {
        frame_literal(&text[range.clone()])
            .is_some_and(|value| value.round().max(1.0) as u32 == frame)
    });

    if let Some(frame_index) = frame_index {
        let frame_range = ranges[frame_index].clone();
        for range in ranges.iter().skip(frame_index + 1) {
            if frame_literal(&text[range.clone()]).is_some() {
                break;
            }
            let line = &text[range.clone()];
            if line.contains("if macros::is_excute") && line.contains('{') {
                let open = range.start + line.find('{').unwrap();
                if let Some(close) = matching_brace(text, open) {
                    let body_indent = ranges
                        .iter()
                        .skip(frame_index + 1)
                        .find(|body| {
                            body.start > open && body.start < close && {
                                let body_text = &text[body.start..body.end];
                                !body_text.trim().is_empty() && !body_text.trim().starts_with('}')
                            }
                        })
                        .map(|body| line_indent(text, body).to_string())
                        .unwrap_or_else(|| format!("{}    ", line_indent(text, range)));
                    text.insert_str(close, &format!("{body_indent}{call}{newline}"));
                    return true;
                }
            }
        }

        let indent = line_indent(text, &frame_range);
        let block = format!(
            "{indent}if macros::is_excute(agent) {{{newline}{indent}    {call}{newline}{indent}}}{newline}"
        );
        text.insert_str(frame_range.end, &block);
        return true;
    }

    let insert_at = ranges
        .iter()
        .find(|range| {
            frame_literal(&text[range.start..range.end]).is_some_and(|value| value > frame as f32)
        })
        .map(|range| range.start)
        .or_else(|| text.rfind('}'))
        .unwrap_or(text.len());
    let indent = ranges
        .iter()
        .find(|range| frame_literal(&text[range.start..range.end]).is_some())
        .map(|range| line_indent(text, range).to_string())
        .unwrap_or_else(|| "    ".into());
    let block = format!(
        "{indent}frame(agent.lua_state_agent, {frame}.0);{newline}{indent}if macros::is_excute(agent) {{{newline}{indent}    {call}{newline}{indent}}}{newline}"
    );
    text.insert_str(insert_at, &block);
    true
}

/// Rewrite direct `KineticModule::clear_speed_all` presence and frame placement in a flat
/// `game_` function. Structural changes in branches, loops, or mismatched source ordinals are
/// reported rather than placed into an execution context the editor cannot prove.
pub fn rewrite_kinetic_clear_speed_all(
    text: &str,
    label: &str,
    pristine: &[crate::data::KineticClearSpeedAllEvent],
    edited: &[crate::data::KineticClearSpeedAllEvent],
) -> Result<(String, SyncReport)> {
    let sites = kinetic_clear_speed_all_sites(text);
    let parsed = crate::acmd::parse_acmd_script(text);
    fn contains_loop(stmts: &[crate::data::AcmdStmt]) -> bool {
        stmts.iter().any(|stmt| match stmt {
            crate::data::AcmdStmt::Loop { .. } => true,
            crate::data::AcmdStmt::RawBlock { body, .. } => contains_loop(body),
            _ => false,
        })
    }
    let mut report = SyncReport::default();
    let flat_sites = pristine.len() == sites.len()
        && pristine
            .iter()
            .enumerate()
            .all(|(index, event)| event.site == index);
    let flat_edited = edited
        .iter()
        .enumerate()
        .all(|(index, event)| event.site == index);
    let old: Vec<u32> = pristine.iter().map(|event| event.frame).collect();
    let new: Vec<u32> = edited.iter().map(|event| event.frame).collect();
    let has_unsupported_structure =
        parsed.branch_count() > 0 || contains_loop(&parsed.stmts) || !flat_sites || !flat_edited;
    if has_unsupported_structure {
        if pristine != edited {
            report.skipped.push(format!(
                "{label}: KineticModule::clear_speed_all placement changed inside a loop/branch or after a source-site mismatch — source syncing only edits flat point calls"
            ));
        }
        return Ok((text.to_string(), report));
    }

    let n = old.len();
    let m = new.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut matched_old = vec![false; n];
    let mut matched_new = vec![false; m];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            matched_old[i] = true;
            matched_new[j] = true;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if matched_old.iter().all(|matched| *matched) && matched_new.iter().all(|matched| *matched) {
        return Ok((text.to_string(), report));
    }

    let removals: Vec<Replacement> = sites
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_old[*index])
        .map(|(_, site)| remove_kinetic_clear_speed_all_line(text, site))
        .collect();
    let mut updated = apply(text, removals);
    for (_, event) in edited
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_new[*index])
    {
        if insert_kinetic_clear_speed_all_line(&mut updated, event.frame) {
            report.changed += 1;
        }
    }
    report.changed += matched_old.iter().filter(|matched| !**matched).count();
    Ok((updated, report))
}

/// Sync edited direct `KineticModule::clear_speed_all` points into the project's `game_` function.
pub fn sync_kinetic_clear_speed_all(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::KineticClearSpeedAllEvent],
    edited: &[crate::data::KineticClearSpeedAllEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_kinetic_clear_speed_all(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// Rewrite `SET_AIR` presence and frame placement in a flat `game_` function.
pub fn rewrite_set_air(
    text: &str,
    label: &str,
    pristine: &[crate::data::SetAirEvent],
    edited: &[crate::data::SetAirEvent],
) -> Result<(String, SyncReport)> {
    let sites = set_air_sites(text);
    let parsed = crate::acmd::parse_acmd_script(text);
    fn contains_loop(stmts: &[crate::data::AcmdStmt]) -> bool {
        stmts.iter().any(|stmt| match stmt {
            crate::data::AcmdStmt::Loop { .. } => true,
            crate::data::AcmdStmt::RawBlock { body, .. } => contains_loop(body),
            _ => false,
        })
    }
    let mut report = SyncReport::default();
    let flat_sites = pristine.len() == sites.len()
        && pristine
            .iter()
            .enumerate()
            .all(|(index, event)| event.site == index);
    let flat_edited = edited
        .iter()
        .enumerate()
        .all(|(index, event)| event.site == index);
    let old: Vec<u32> = pristine.iter().map(|event| event.frame).collect();
    let new: Vec<u32> = edited.iter().map(|event| event.frame).collect();
    let has_unsupported_structure =
        parsed.branch_count() > 0 || contains_loop(&parsed.stmts) || !flat_sites || !flat_edited;
    if has_unsupported_structure {
        if pristine != edited {
            report.skipped.push(format!(
                "{label}: SET_AIR placement changed inside a loop/branch or after a source-site mismatch — source syncing only edits flat point calls"
            ));
        }
        return Ok((text.to_string(), report));
    }

    let n = old.len();
    let m = new.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut matched_old = vec![false; n];
    let mut matched_new = vec![false; m];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            matched_old[i] = true;
            matched_new[j] = true;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if matched_old.iter().all(|matched| *matched) && matched_new.iter().all(|matched| *matched) {
        return Ok((text.to_string(), report));
    }

    let removals: Vec<Replacement> = sites
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_old[*index])
        .map(|(_, site)| remove_set_air_line(text, site))
        .collect();
    let mut updated = apply(text, removals);
    for (_, event) in edited
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_new[*index])
    {
        if insert_set_air_line(&mut updated, event.frame) {
            report.changed += 1;
        }
    }
    report.changed += matched_old.iter().filter(|matched| !**matched).count();
    Ok((updated, report))
}

/// Sync edited `SET_AIR` points into the project's `game_` function.
pub fn sync_set_air(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::SetAirEvent],
    edited: &[crate::data::SetAirEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_set_air(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

// ── Facing-direction point write-back ───────────────────────────────────────

/// The exact argument-less `REVERSE_LR` calls in a source function.
pub(crate) fn reverse_lr_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| site.name == "REVERSE_LR" && site.args.len() == 1)
        .collect()
}

fn source_line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for chunk in text.split_inclusive('\n') {
        ranges.push(start..start + chunk.len());
        start += chunk.len();
    }
    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
}

fn line_indent<'a>(text: &'a str, range: &Range<usize>) -> &'a str {
    let line = &text[range.clone()];
    &line[..line.len() - line.trim_start().len()]
}

fn frame_literal(line: &str) -> Option<f32> {
    let start = line.find("frame(")? + "frame(".len();
    let comma = line[start..].find(',')? + start;
    let end = line[comma + 1..].find(')')? + comma + 1;
    line[comma + 1..end].trim().parse().ok()
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in text.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn remove_reverse_line(text: &str, site: &MacroSite) -> Replacement {
    let ranges = source_line_ranges(text);
    let Some(line) = ranges
        .iter()
        .find(|range| range.start <= site.span.start && site.span.start < range.end)
    else {
        return Replacement {
            span: site.span.clone(),
            value: String::new(),
        };
    };
    let trimmed = text[line.clone()].trim();
    if trimmed == "macros::REVERSE_LR(agent);" {
        Replacement {
            span: line.clone(),
            value: String::new(),
        }
    } else {
        let mut end = site.span.end;
        if text[end..].starts_with(';') {
            end += 1;
        }
        Replacement {
            span: site.span.start..end,
            value: String::new(),
        }
    }
}

fn insert_reverse_line(text: &mut String, frame: u32) -> bool {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let ranges = source_line_ranges(text);
    let frame_index = ranges.iter().position(|range| {
        frame_literal(&text[range.clone()])
            .is_some_and(|value| value.round().max(1.0) as u32 == frame)
    });

    // Prefer the existing frame's execute block. This keeps the source's frame ordering and
    // indentation and adds only the one call the user requested.
    if let Some(frame_index) = frame_index {
        let frame_range = ranges[frame_index].clone();
        for range in ranges.iter().skip(frame_index + 1) {
            if frame_literal(&text[range.clone()]).is_some() {
                break;
            }
            let line = &text[range.clone()];
            if line.contains("if macros::is_excute") && line.contains('{') {
                let open = range.start + line.find('{').unwrap();
                if let Some(close) = matching_brace(text, open) {
                    let body_indent = ranges
                        .iter()
                        .skip(frame_index + 1)
                        .find(|body| {
                            body.start > open && body.start < close && {
                                let body_text = &text[body.start..body.end];
                                !body_text.trim().is_empty() && !body_text.trim().starts_with('}')
                            }
                        })
                        .map(|body| line_indent(text, body).to_string())
                        .unwrap_or_else(|| format!("{}    ", line_indent(text, range)));
                    text.insert_str(
                        close,
                        &format!("{body_indent}macros::REVERSE_LR(agent);{newline}"),
                    );
                    return true;
                }
            }
        }

        // The frame exists but has no execute block. Put a new one directly after the frame
        // line, before another top-level statement or the next frame.
        let indent = line_indent(text, &frame_range);
        let block = format!(
            "{indent}if macros::is_excute(agent) {{{newline}{indent}    macros::REVERSE_LR(agent);{newline}{indent}}}{newline}"
        );
        text.insert_str(frame_range.end, &block);
        return true;
    }

    // No exact frame exists. Insert a new frame/block before the first later frame, or just
    // before the function's final brace when this is the last point in the move.
    let insert_at = ranges
        .iter()
        .find(|range| {
            frame_literal(&text[range.start..range.end]).is_some_and(|value| value > frame as f32)
        })
        .map(|range| range.start)
        .or_else(|| text.rfind('}'))
        .unwrap_or(text.len());
    let indent = ranges
        .iter()
        .find(|range| frame_literal(&text[range.start..range.end]).is_some())
        .map(|range| line_indent(text, range).to_string())
        .unwrap_or_else(|| "    ".into());
    let block = format!(
        "{indent}frame(agent.lua_state_agent, {frame}.0);{newline}{indent}if macros::is_excute(agent) {{{newline}{indent}    macros::REVERSE_LR(agent);{newline}{indent}}}{newline}"
    );
    text.insert_str(insert_at, &block);
    true
}

/// Rewrite `REVERSE_LR` presence and frame placement in a user's own `game_` source.
///
/// This is the one structural source rewrite in the first E1 slice. It is deliberately limited
/// to flat, one-call-per-source-site functions: inserting into a runtime branch or a loop would
/// require choosing an execution context the editor does not know. Such a change is reported
/// without touching the user's text. An LCS over the one-based frame sequence makes a retime a
/// safe remove-plus-insert and keeps later calls from being shifted by an earlier removal.
pub fn rewrite_reverse_lr(
    text: &str,
    label: &str,
    pristine: &[crate::data::ReverseLrEvent],
    edited: &[crate::data::ReverseLrEvent],
) -> Result<(String, SyncReport)> {
    let sites = reverse_lr_sites(text);
    let parsed = crate::acmd::parse_acmd_script(text);
    fn contains_loop(stmts: &[crate::data::AcmdStmt]) -> bool {
        stmts.iter().any(|stmt| match stmt {
            crate::data::AcmdStmt::Loop { .. } => true,
            crate::data::AcmdStmt::RawBlock { body, .. } => contains_loop(body),
            _ => false,
        })
    }
    let mut report = SyncReport::default();
    let flat_sites = pristine.len() == sites.len()
        && pristine
            .iter()
            .enumerate()
            .all(|(index, event)| event.site == index);
    let flat_edited = edited
        .iter()
        .enumerate()
        .all(|(index, event)| event.site == index);
    let old: Vec<u32> = pristine.iter().map(|event| event.frame).collect();
    let new: Vec<u32> = edited.iter().map(|event| event.frame).collect();
    let has_unsupported_structure =
        parsed.branch_count() > 0 || contains_loop(&parsed.stmts) || !flat_sites || !flat_edited;
    if has_unsupported_structure {
        if pristine != edited {
            report.skipped.push(format!(
                "{label}: REVERSE_LR placement changed inside a loop/branch or after a source-site mismatch — source syncing only edits flat point calls"
            ));
        }
        return Ok((text.to_string(), report));
    }

    let n = old.len();
    let m = new.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut matched_old = vec![false; n];
    let mut matched_new = vec![false; m];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            matched_old[i] = true;
            matched_new[j] = true;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if matched_old.iter().all(|matched| *matched) && matched_new.iter().all(|matched| *matched) {
        return Ok((text.to_string(), report));
    }

    let removals: Vec<Replacement> = sites
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_old[*index])
        .map(|(_, site)| remove_reverse_line(text, site))
        .collect();
    let mut updated = apply(text, removals);
    for (_, event) in edited
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched_new[*index])
    {
        if insert_reverse_line(&mut updated, event.frame) {
            report.changed += 1;
        }
    }
    report.changed += matched_old.iter().filter(|matched| !**matched).count();
    Ok((updated, report))
}

/// Sync edited `REVERSE_LR` points into the project's `game_` function.
pub fn sync_reverse_lr(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::ReverseLrEvent],
    edited: &[crate::data::ReverseLrEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_reverse_lr(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

// ── Expression write-back ───────────────────────────────────────────────────

/// The measured expression calls in source order, with the leading `agent` included in the
/// arity just as [`MacroSite::args`] stores it.
pub(crate) fn expression_sites(text: &str) -> Vec<MacroSite> {
    const CALLS: &[(&str, usize)] = &[
        ("RUMBLE_HIT", 3),
        ("QUAKE", 2),
        ("FT_ATTACK_ABS_CAMERA_QUAKE", 3),
    ];
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| {
            CALLS
                .iter()
                .any(|(name, arity)| site.name == *name && site.args.len() == *arity)
        })
        .collect()
}

fn expression_tokens(call: &crate::data::ExpressionCall) -> Vec<&str> {
    match call {
        crate::data::ExpressionCall::RumbleHit { kind, unk } => vec![kind, unk],
        crate::data::ExpressionCall::Quake { kind } => vec![kind],
        crate::data::ExpressionCall::FtAttackAbsCameraQuake {
            attack_abs_kind,
            quake_kind,
        } => vec![attack_abs_kind, quake_kind],
    }
}

/// Rewrite the argument tokens of the measured expression calls in the user's own source.
///
/// This is intentionally token-based rather than numeric. Camera/rumble constants are not one
/// collision namespace: a mod can use a named constant, a raw capture value, or a project-local
/// expression, and replacing any of those with a guessed `Hash40`/integer would change the
/// caller's intent. Frames, macro names, and call count are structural and are reported rather
/// than moved or invented. A looped call is one source line, so all unrolled events from that
/// site must agree on the replacement.
pub fn rewrite_expressions(
    text: &str,
    label: &str,
    pristine: &[crate::data::ExpressionEvent],
    edited: &[crate::data::ExpressionEvent],
) -> Result<(String, SyncReport)> {
    let sites = expression_sites(text);
    let mut report = SyncReport::default();
    if pristine.len() != edited.len() {
        report.skipped.push(format!(
            "{label}: {} expression call(s) were added or removed — source syncing only retunes existing calls",
            edited.len().abs_diff(pristine.len())
        ));
        return Ok((text.to_string(), report));
    }

    let mut edits: Vec<Replacement> = Vec::new();
    for (before, now) in pristine.iter().zip(edited.iter()) {
        if before == now {
            continue;
        }
        let Some(site) = sites.get(before.site) else {
            report.skipped.push(format!(
                "{label}: expression call #{} has no matching source call",
                before.site
            ));
            continue;
        };
        if before.site != now.site || before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: `{}` on frame {} was moved or reordered — source syncing cannot change expression structure",
                before.call.func(),
                before.frame
            ));
            continue;
        }
        if before.call.func() != now.call.func() || site.name != before.call.func() {
            report.skipped.push(format!(
                "{label}: expression call #{} changed macro — source syncing only rewrites its argument values",
                before.site
            ));
            continue;
        }
        let old_tokens = expression_tokens(&before.call);
        let new_tokens = expression_tokens(&now.call);
        if old_tokens.len() != new_tokens.len() || site.args.len() != old_tokens.len() + 1 {
            report.skipped.push(format!(
                "{label}: `{}` on frame {} has an unsupported argument shape",
                before.call.func(),
                before.frame
            ));
            continue;
        }
        for (index, (old, new)) in old_tokens.iter().zip(new_tokens).enumerate() {
            if *old == new {
                continue;
            }
            let Some(span) = site.args.get(index + 1) else {
                continue;
            };
            let Some(edit) = text_edit(text, span, new) else {
                continue;
            };
            match edits.iter().find(|existing| existing.span == edit.span) {
                Some(existing) if existing.value == edit.value => {}
                Some(_) => report.skipped.push(format!(
                    "{label}: looped `{}` on frame {} was given conflicting values in one source line",
                    before.call.func(),
                    before.frame
                )),
                None => edits.push(edit),
            }
        }
    }

    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Write measured expression arguments back into the project's own `expression_` function.
pub fn sync_expressions(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::ExpressionEvent],
    edited: &[crate::data::ExpressionEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("expression", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_expressions(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

// ── Sound write-back ─────────────────────────────────────────────────────────

/// The sound calls in `text`, in document order — index `n` is site `n`.
///
/// The arity filter is load-bearing for the same reason it is in [`hurt_sites`]: the parser
/// refuses a family member written with the wrong number of arguments and leaves it `Raw`, so
/// counting it here would number every later site one too high and retune the wrong call.
///
/// `SOUND_FUNCS` carries `(name, hash args, has tail)`, and the total after `agent` is the sum
/// of the last two.
pub(crate) fn sound_sites(text: &str) -> Vec<MacroSite> {
    scan_macro_sites(text, 0..text.len())
        .into_iter()
        .filter(|site| {
            crate::acmd::SOUND_FUNCS.iter().any(|(name, hashes, tail)| {
                site.name == *name && site.args.len() == hashes + usize::from(*tail) + 1
            })
        })
        .collect()
}

/// Rewrite edited sound names into the user's own source.
///
/// Value edits only, as everywhere on this path — and for sound the value is the `Hash40` naming
/// which sound plays. A retime is reported rather than performed, for the reason
/// [`rewrite_hurtboxes`] gives: the frame is the block the call sits in, not an argument. So is
/// a change of macro, which is a different call taking different arguments rather than a
/// different value in the same one.
pub fn rewrite_sounds(
    text: &str,
    label: &str,
    pristine: &[crate::data::SoundEvent],
    edited: &[crate::data::SoundEvent],
) -> Result<(String, SyncReport)> {
    let sites = sound_sites(text);
    let mut report = SyncReport::default();
    let mut edits: Vec<Replacement> = Vec::new();

    for (before, now) in pristine.iter().zip(edited.iter()) {
        if before == now {
            continue;
        }
        let Some(site) = sites.get(now.site) else {
            report.skipped.push(format!(
                "{label}: a sound has no matching call in the source — it was added in the \
                 editor, and source syncing only retunes existing calls"
            ));
            continue;
        };
        if before.frame != now.frame {
            report.skipped.push(format!(
                "{label}: the `{}` on frame {} was retimed — its frame is the block it sits in, \
                 not an argument, so source syncing cannot move it",
                site.name, before.frame
            ));
            continue;
        }
        if before.call.func != now.call.func || before.call.sounds.len() != now.call.sounds.len() {
            report.skipped.push(format!(
                "{label}: the `{}` on frame {} became a `{}` — a different macro takes different \
                 arguments, so that is structure rather than a value",
                before.call.func, before.frame, now.call.func
            ));
            continue;
        }
        if before.call.tail != now.call.tail {
            report.skipped.push(format!(
                "{label}: the suppression window on the `{}` at frame {} changed — only the \
                 sound name is editable here",
                before.call.func, before.frame
            ));
        }
        for (index, (was, is)) in before
            .call
            .sounds
            .iter()
            .zip(now.call.sounds.iter())
            .enumerate()
        {
            if was == is {
                continue;
            }
            // `+ 1` to step over `agent`, which every one of these macros takes first.
            let Some(span) = site.args.get(index + 1) else {
                continue;
            };
            let Some(edit) = text_edit(text, span, &format!("Hash40::new(\"{is}\")")) else {
                continue;
            };
            // One looped `PLAY_SE` yields one event per iteration, all carrying the same site,
            // so the same span can be reached more than once. Writing it twice would hand
            // `apply` two overlapping edits; a *disagreement* between the two is a real conflict
            // and is reported rather than resolved by whichever iteration happened to be last.
            match edits.iter().find(|e| e.span == edit.span) {
                Some(existing) if existing.value == edit.value => {}
                Some(_) => report.skipped.push(format!(
                    "{label}: the looped `{}` at frame {} was given two different sounds in the \
                     same line — every iteration is the one call in the file",
                    before.call.func, before.frame
                )),
                None => edits.push(edit),
            }
        }
    }

    report.changed = edits.len();
    Ok((apply(text, edits), report))
}

/// Write edited sound names back into the project's own `sound_` function.
pub fn sync_sounds(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &[crate::data::SoundEvent],
    edited: &[crate::data::SoundEvent],
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("sound", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_sounds(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

/// Sync edited hurtbox state for one move back into the project source on disk.
pub fn sync_hurtboxes(
    index: &SourceIndex,
    fighter: &str,
    move_name: &str,
    pristine: &(
        Vec<crate::data::HurtboxState>,
        Vec<crate::data::ColPriState>,
    ),
    edited: &(
        Vec<crate::data::HurtboxState>,
        Vec<crate::data::ColPriState>,
    ),
) -> Result<SyncReport> {
    let script_name = crate::acmd::acmd_script_name("game", move_name);
    sync_script(index, fighter, &script_name, |body| {
        rewrite_hurtboxes(body, &format!("{fighter}/{move_name}"), pristine, edited)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kirby's down smash, verbatim from the corpus — two rate calls around a `frame()`.
    const DOWN_SMASH: &str = "unsafe extern \"C\" fn game_attacklw4(agent: &mut L2CAgentBase) {\n\
        \x20   macros::FT_MOTION_RATE(agent, 0.25);\n\
        \x20   frame(agent.lua_state_agent, 4.0);\n\
        \x20   macros::FT_MOTION_RATE(agent, 1.0);\n\
        }\n";

    /// A rate edit rewrites **only** that argument's span, leaving the rest byte for byte.
    ///
    /// The paired positive matters as much as the change: a rewriter that returned the whole
    /// file re-rendered from the model would pass "the new value is present" while quietly
    /// reformatting the user's own source, which is the thing this path exists to avoid.
    #[test]
    fn a_rate_edit_rewrites_only_that_argument() {
        let edited = crate::acmd::parse_acmd_script(DOWN_SMASH);
        let mut sites = edited.motion_rate_sites();
        assert_eq!(sites.len(), 2, "the fixture has two rate calls");
        sites[0].2 = 0.5;

        let (out, report) = rewrite_motion_rates(DOWN_SMASH, "kirby/attack_lw4", &sites).unwrap();
        assert_eq!(report.changed, 1, "one argument changed");
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(
            out,
            DOWN_SMASH.replace("agent, 0.25", "agent, 0.5"),
            "only the first rate's argument may differ"
        );
    }

    /// The site-ordinal guard.
    ///
    /// The parser models a rate call only at the function's top level, so one written inside a
    /// runtime branch never reaches the editor — while the source scan still counts it. Writing
    /// by position across that mismatch lands the edit on the *wrong call*, silently, which is
    /// the failure this codebase has already paid for once with per-family site counters.
    ///
    /// Paired with a positive: the same source with the branch removed must still write, or this
    /// test would pass against a rewriter that refuses everything.
    #[test]
    fn a_rate_inside_a_branch_is_refused_rather_than_written_by_position() {
        let branched = "unsafe extern \"C\" fn game_x(agent: &mut L2CAgentBase) {\n\
            \x20   if WorkModule::is_flag(agent.module_accessor, 0) {\n\
            \x20       macros::FT_MOTION_RATE(agent, 0.25);\n\
            \x20   }\n\
            \x20   macros::FT_MOTION_RATE(agent, 1.0);\n\
            }\n";
        let script = crate::acmd::parse_acmd_script(branched);
        let sites = script.motion_rate_sites();
        assert_eq!(sites.len(), 1, "only the top-level call is modelled");

        let (out, report) = rewrite_motion_rates(branched, "kirby/x", &sites).unwrap();
        assert_eq!(
            out, branched,
            "nothing may be written when the counts disagree"
        );
        assert!(
            report.skipped.iter().any(|s| s.contains("branch")),
            "the refusal must say why: {:?}",
            report.skipped
        );

        // The positive half, through the same function.
        let flat = crate::acmd::parse_acmd_script(DOWN_SMASH);
        let (_, ok) =
            rewrite_motion_rates(DOWN_SMASH, "kirby/x", &flat.motion_rate_sites()).unwrap();
        assert!(
            ok.skipped.is_empty(),
            "a flat script must not be refused: {:?}",
            ok.skipped
        );
    }

    #[test]
    fn an_expression_token_edit_rewrites_only_that_argument() {
        let source = r#"unsafe extern "C" fn expression_throwhi(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        // Keep this comment and the unknown expression line byte-for-byte.
        slope!(agent, *MA_MSC_CMD_SLOPE_SLOPE, *SLOPE_STATUS_LR);
        macros::RUMBLE_HIT(agent, Hash40::new("rbkind_attackm"), 0);
        macros::QUAKE(agent, *CAMERA_QUAKE_KIND_M);
    }
}
"#;
        let script = crate::acmd::parse_expression_script(source);
        let pristine = script.to_expression_events();
        let mut edited_script = script.clone();
        let call = edited_script.expression_stmt_mut(0).unwrap();
        *call = crate::data::ExpressionCall::RumbleHit {
            kind: "Hash40::new(\"rbkind_attackl\")".into(),
            unk: "0".into(),
        };
        let edited = edited_script.to_expression_events();

        let (out, report) =
            rewrite_expressions(source, "kirby/throw_hi", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert!(out.contains("Hash40::new(\"rbkind_attackl\")"));
        assert!(out.contains("slope!(agent, *MA_MSC_CMD_SLOPE_SLOPE, *SLOPE_STATUS_LR);"));
        assert!(out.contains("// Keep this comment"));
        assert_eq!(
            crate::acmd::parse_expression_script(&out).to_expression_events(),
            edited
        );
    }

    #[test]
    fn an_expression_retime_is_reported_without_touching_source() {
        let source = r#"unsafe extern "C" fn expression_x(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::QUAKE(agent, *CAMERA_QUAKE_KIND_S);
    }
}
"#;
        let script = crate::acmd::parse_expression_script(source);
        let pristine = script.to_expression_events();
        let mut edited = pristine.clone();
        edited[0].frame = 5;
        let (out, report) = rewrite_expressions(source, "kirby/x", &pristine, &edited).unwrap();
        assert_eq!(out, source);
        assert_eq!(report.changed, 0);
        assert!(report.skipped.iter().any(|note| note.contains("moved")));
    }

    const REVERSE_FLAT: &str = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 3.0);
    if macros::is_excute(agent) {
        macros::REVERSE_LR(agent);
    }
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::REVERSE_LR(agent);
    }
}
pub fn install(agent: &mut smashline::Agent) {
    agent.acmd("game_attackairn", game_attackairn, smashline::Priority::Default);
}
"#;

    #[test]
    fn reverse_lr_source_write_back_retimes_without_reformatting_the_other_point() {
        let script = crate::acmd::parse_acmd_script(REVERSE_FLAT);
        let pristine = script.to_reverse_lr_events();
        let mut edited = pristine.clone();
        edited[0].frame = 4;

        let (out, report) =
            rewrite_reverse_lr(REVERSE_FLAT, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(
            report.changed, 2,
            "one source call was removed and one added"
        );
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(
            crate::acmd::parse_acmd_script(&out).to_reverse_lr_events(),
            edited,
            "the source rewriter and parser must agree on the edited points"
        );
        assert_eq!(
            out.matches("macros::REVERSE_LR(agent);").count(),
            2,
            "retiming must not duplicate the later point"
        );
        assert!(
            out.contains("frame(agent.lua_state_agent, 8.0);"),
            "the untouched frame must remain byte-for-byte present"
        );
    }

    #[test]
    fn reverse_lr_source_write_back_refuses_a_runtime_branch() {
        let branched = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if WorkModule::is_flag(agent.module_accessor, 0) {
        frame(agent.lua_state_agent, 3.0);
        if macros::is_excute(agent) {
            macros::REVERSE_LR(agent);
        }
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(branched).to_reverse_lr_events();
        let mut edited = pristine.clone();
        edited[0].frame = 4;
        let (out, report) =
            rewrite_reverse_lr(branched, "mario/branch", &pristine, &edited).unwrap();
        assert_eq!(out, branched, "a branch placement must not be guessed at");
        assert!(
            report.skipped.iter().any(|note| note.contains("branch")),
            "the refusal must name the structural blocker: {:?}",
            report.skipped
        );
    }

    #[test]
    fn reverse_lr_source_site_mismatch_is_safe_when_another_family_changes() {
        let mismatch = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 3.0);
    macros::REVERSE_LR(agent);
}
"#;
        // The parser intentionally models only the measured execute-block form, while the
        // source scanner still sees this line. A hitbox-only edit can therefore invoke this
        // pass with equal empty baselines; it must be a no-op, not an ordinal-index panic.
        let events = Vec::new();
        let (out, report) = rewrite_reverse_lr(mismatch, "mario/mismatch", &events, &events)
            .expect("a source-site mismatch is reportable, not fatal");
        assert_eq!(out, mismatch);
        assert!(report.changed == 0 && report.skipped.is_empty());
    }

    #[test]
    fn syncing_reverse_lr_reindexes_the_projected_source_shape() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/mario/acmd.rs", REVERSE_FLAT);
        write(
            tmp.path(),
            "src/mario/mod.rs",
            "pub fn install() { let agent = &mut smashline::Agent::new(\"mario\"); }",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();
        let source = index.script_source("mario", "attack_air_n").unwrap().body;
        let pristine = crate::acmd::parse_acmd_script(&source).to_reverse_lr_events();
        let mut edited = pristine.clone();
        edited[0].frame = 4;
        let report = sync_reverse_lr(&index, "mario", "attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2);
        let after = SourceIndex::build(tmp.path()).unwrap();
        let body = after.script_source("mario", "attack_air_n").unwrap().body;
        assert_eq!(
            crate::acmd::parse_acmd_script(&body).to_reverse_lr_events(),
            edited
        );
    }

    /// `FT_MOTION_RATE_RANGE` must not be counted as a rate site.
    ///
    /// It begins with `FT_MOTION_RATE`'s entire name, so a prefix match would count it, shift
    /// every later site by one, and write a playback rate into its first frame argument.
    #[test]
    fn the_range_macro_is_not_counted_as_a_rate_site() {
        let text = "unsafe extern \"C\" fn game_x(agent: &mut L2CAgentBase) {\n\
            \x20   macros::FT_MOTION_RATE_RANGE(agent, 5.0, 10.0, 0.5);\n\
            \x20   macros::FT_MOTION_RATE(agent, 0.6);\n\
            }\n";
        let sites = motion_rate_sites(text);
        assert_eq!(sites.len(), 1, "only the plain form is a rate site");
        assert_eq!(sites[0].arg(text, 1).map(str::trim), Some("0.6"));
    }

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

        let body = index.script_source("mario", "attack_air_n").unwrap().body;
        assert!(body.contains("fn game_attackairn") && body.contains("fn effect_attackairn"));
        // The whole point: the parsers see the user's macro, not vanilla's.
        let calls = crate::acmd::parse_effect_script(&body).to_effect_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].spawn_func, "EFFECT_FOLLOW_FLIP");
        assert_eq!(calls[0].effect_name_alt.as_deref(), Some("sys_hit_r"));
    }

    /// A project says which categories it speaks for, and only asks for the mirror when one
    /// the editor displays is missing.
    ///
    /// The sound-only case is the one D1a wrote down and could not fix: `script_body` returned
    /// `None` for it, so the project's own sounds were the single thing that did not survive
    /// loading them.
    #[test]
    fn a_project_reports_the_categories_it_covers_and_asks_for_the_rest() {
        let (_tmp, index) = mario_project();
        let both = index.script_source("mario", "attack_air_n").unwrap();
        assert_eq!(both.covers, ["game_", "effect_"]);
        // **Changed at D1d, and it is the change rather than a regression.** This project has
        // hitboxes and effects but writes no `sound_`, and until sounds were editable that was
        // "everything shown" and no reason to touch the network. Now the sound section would sit
        // empty for it, so the mirror is worth asking for.
        assert!(
            both.needs_mirror(),
            "a project with no sound script of its own needs the mirror's to show one"
        );

        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/kirby/acmd.rs",
            "unsafe extern \"C\" fn sound_attackairn(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n",
        );
        write(
            tmp.path(),
            "src/kirby/mod.rs",
            "pub fn install() { let agent = &mut smashline::Agent::new(\"kirby\"); }",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();
        let sound_only = index.script_source("kirby", "attack_air_n").unwrap();
        assert_eq!(sound_only.covers, ["sound_"]);
        assert!(sound_only.needs_mirror());

        assert!(
            index.script_source("kirby", "attack_air_f").is_none(),
            "a move the project says nothing about is the mirror's alone"
        );
    }

    /// The merge makes a mirror-sourced effect editable in a project that has no `effect_` of
    /// its own, so write-back has to say so rather than guess where to put it.
    ///
    /// **Still true after D1e, and deliberately so.** Creating the missing function is a
    /// separate step ([`create_script`]) that the caller runs *first*; the value sync's own job
    /// never became "write wherever looks plausible". If this test ever starts passing by
    /// writing something, a sync has grown the power to invent a destination.
    #[test]
    fn syncing_a_category_the_project_does_not_define_refuses_instead_of_guessing() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/kirby/acmd.rs",
            "unsafe extern \"C\" fn game_attackairn(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n",
        );
        write(
            tmp.path(),
            "src/kirby/mod.rs",
            "pub fn install() { let agent = &mut smashline::Agent::new(\"kirby\"); }",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();

        let error = sync_effect_calls(&index, "kirby", "attack_air_n", &[], &[])
            .expect_err("a project with no effect_ must not be written into")
            .to_string();
        assert!(
            error.contains("effect_attackairn"),
            "the message has to name what is missing: {error}"
        );
    }

    /// A sound the project does not define is created beside the scripts it does, registered the
    /// same way, and comes back under exactly the name it was asked for.
    ///
    /// **The re-index is the assertion that matters**, not the file's text. A function installed
    /// under the wrong script name is still valid Rust that compiles and runs — it simply
    /// replaces a move nobody edited. Only asking the indexer what it now sees catches that.
    #[test]
    fn a_missing_sound_script_is_created_beside_its_siblings_and_registered_with_them() {
        let (tmp, index) = mario_project();
        let source = SOUNDS.replace("sound_turndash", "sound_attackairn");

        let created = create_script(&index, "mario", "sound_attackairn", &source).unwrap();
        assert!(
            created.note.contains("registered it beside"),
            "{}",
            created.note
        );

        let after = SourceIndex::build(tmp.path()).unwrap();
        let site = after
            .script("mario", "sound_attackairn")
            .expect("the created script has to index under the name it was created as");
        let text = std::fs::read_to_string(&created.file).unwrap();
        assert_eq!(
            &text[site.span.clone()],
            source.trim_end(),
            "the function is the mirror's text verbatim — creation writes no edit of its own"
        );
        assert!(
            text.contains(
                "agent.acmd(\"sound_attackairn\", sound_attackairn, smashline::Priority::Default);"
            ),
            "the registration copies the sibling's tail as well as its shape:\n{text}"
        );
        // The siblings still resolve: an insertion moves spans, and a rescan is what makes that
        // safe. This is the assertion that fails if creation ever patches spans by hand instead.
        assert!(after.script("mario", "game_attackairn").is_some());
        assert!(after.script("mario", "effect_attackairn").is_some());
        assert_eq!(after.script_count(), 3);
    }

    /// With no sibling for this move, any script of the same fighter is anchor enough.
    #[test]
    fn a_move_the_project_says_nothing_about_still_has_somewhere_to_be_created() {
        let (tmp, index) = mario_project();
        create_script(&index, "mario", "sound_turndash", SOUNDS).unwrap();

        let after = SourceIndex::build(tmp.path()).unwrap();
        assert!(after.script("mario", "sound_turndash").is_some());
        assert!(
            after.script_source("mario", "turn_dash").is_some(),
            "and the move now reads back out of the project"
        );
    }

    /// An attribute-registered project gets an attribute, with the category derived from how it
    /// spells its own.
    #[test]
    fn creating_into_an_attribute_project_respells_the_attribute_for_the_new_category() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "#[acmd_script(agent = \"fighter_lucina\", script = \"game_turndash\", \
             category = ACMD_GAME)]\nunsafe extern \"C\" fn my_custom_name(agent: &mut \
             L2CAgentBase) {\n    frame(agent.lua_state_agent, 1.0);\n}\n",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();

        let created = create_script(&index, "lucina", "sound_turndash", SOUNDS).unwrap();
        let text = std::fs::read_to_string(&created.file).unwrap();
        assert!(
            text.contains(
                "#[acmd_script(agent = \"fighter_lucina\", script = \"sound_turndash\", \
                 category = ACMD_SOUND)]"
            ),
            "the agent is carried, the script renamed, the category derived:\n{text}"
        );
        let after = SourceIndex::build(tmp.path()).unwrap();
        assert!(after.script("lucina", "sound_turndash").is_some());
        assert!(after.script("lucina", "game_turndash").is_some());
    }

    /// The category is only derivable while the project agrees with itself about its own.
    ///
    /// There is no copy of the `#[acmd_script]` macro on this machine, so `ACMD_SOUND` is a
    /// name read off the project rather than one looked up. A project that spells its `game_`
    /// script something else has told us the rule does not hold there, and inventing the token
    /// anyway would install the function under the wrong category — which compiles.
    #[test]
    fn an_unfamiliar_category_token_is_refused_rather_than_guessed_at() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "#[acmd_script(agent = \"fighter_lucina\", script = \"game_turndash\", \
             category = Acmd::Game)]\nunsafe extern \"C\" fn my_custom_name(agent: &mut \
             L2CAgentBase) {\n    frame(agent.lua_state_agent, 1.0);\n}\n",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();
        let before = std::fs::read_to_string(tmp.path().join("src/lib.rs")).unwrap();

        let error = create_script(&index, "lucina", "sound_turndash", SOUNDS)
            .expect_err("an unrecognised category token must not be extrapolated")
            .to_string();
        assert!(error.contains("Acmd::Game"), "say what it saw: {error}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("src/lib.rs")).unwrap(),
            before,
            "and a refusal writes nothing at all"
        );
    }

    /// A project that registers nothing Visionary can see gets a function named the same
    /// conventional way — and is told so, because that is the case where creating one could
    /// silently do nothing.
    #[test]
    fn creating_beside_a_conventionally_named_script_says_it_could_not_register_it() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/kirby/acmd.rs",
            "unsafe extern \"C\" fn game_turndash(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n",
        );
        write(
            tmp.path(),
            "src/kirby/mod.rs",
            "pub fn install() { let agent = &mut smashline::Agent::new(\"kirby\"); }",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();

        let created = create_script(&index, "kirby", "sound_turndash", SOUNDS).unwrap();
        assert!(
            created.note.contains("check it installs"),
            "a registration that could not be copied has to be said out loud: {}",
            created.note
        );
        let after = SourceIndex::build(tmp.path()).unwrap();
        assert!(after.script("kirby", "sound_turndash").is_some());
    }

    /// Creating a script that already exists is refused, because the failure is not a wasted
    /// write — it is two functions with one name, and a project that stops compiling.
    #[test]
    fn creating_a_script_the_project_already_has_is_refused() {
        let (tmp, index) = mario_project();
        let before = std::fs::read_to_string(tmp.path().join("src/mario/acmd.rs")).unwrap();
        let error = create_script(&index, "mario", "game_attackairn", SOUNDS)
            .expect_err("a second copy of a function is a duplicate definition, not an edit")
            .to_string();
        assert!(error.contains("game_attackairn"), "{error}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("src/mario/acmd.rs")).unwrap(),
            before
        );
    }

    /// The new function goes beside the same *move*, not beside whatever sorts first.
    ///
    /// Both files here would produce a script that works, so nothing downstream can tell them
    /// apart — which is exactly why the choice needs its own assertion. A project keeps its
    /// moves in separate files for its own reasons, and dropping the aerial's sound into the
    /// specials file is a change the user did not ask for and has to go and undo.
    #[test]
    fn a_created_script_lands_in_the_file_that_holds_the_rest_of_its_move() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/mario/aerials.rs",
            "unsafe extern \"C\" fn game_attackairn(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n",
        );
        // Sorts before `game_attackairn`, so an anchor chosen by name alone picks this one.
        write(
            tmp.path(),
            "src/mario/specials.rs",
            "unsafe extern \"C\" fn effect_specialn(agent: &mut L2CAgentBase) {\n    \
             frame(agent.lua_state_agent, 3.0);\n}\n",
        );
        write(
            tmp.path(),
            "src/mario/mod.rs",
            "pub fn install() { let agent = &mut smashline::Agent::new(\"mario\"); }",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();

        let created = create_script(&index, "mario", "sound_attackairn", SOUNDS).unwrap();
        assert_eq!(created.file.file_name().unwrap(), "aerials.rs");
    }

    /// An attribute that is not `#[acmd_script]` is not a registration.
    ///
    /// Mistaking one for a registration is the quiet failure this whole path exists to avoid:
    /// the function is written with an `#[allow]` copied above it, the real registration is
    /// never looked for, and the result compiles, installs nothing, and plays vanilla.
    #[test]
    fn an_unrelated_attribute_above_a_sibling_is_not_read_as_its_registration() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/mario/acmd.rs",
            "#[allow(unused_variables)]\nunsafe extern \"C\" fn game_turndash(agent: &mut \
             L2CAgentBase) {\n    frame(agent.lua_state_agent, 3.0);\n}\n\npub fn \
             install(agent: &mut smashline::Agent) {\n    agent.acmd(\"game_turndash\", \
             game_turndash, smashline::Priority::Default);\n}\n",
        );
        write(
            tmp.path(),
            "src/mario/mod.rs",
            "pub fn install() { let agent = &mut smashline::Agent::new(\"mario\"); }",
        );
        let index = SourceIndex::build(tmp.path()).unwrap();

        let created = create_script(&index, "mario", "sound_turndash", SOUNDS).unwrap();
        let text = std::fs::read_to_string(&created.file).unwrap();
        assert!(
            !text.contains("#[allow(unused_variables)]\nunsafe extern \"C\" fn sound_turndash"),
            "an #[allow] is not a registration:\n{text}"
        );
        assert!(
            text.contains("agent.acmd(\"sound_turndash\", sound_turndash,"),
            "the real registration is the one below it:\n{text}"
        );
    }

    /// No script for this fighter at all is still a refusal: there is no file to write into, no
    /// fighter attribution to inherit, and no registration style to copy.
    #[test]
    fn creating_for_a_fighter_the_project_never_mentions_is_refused() {
        let (_tmp, index) = mario_project();
        let error = create_script(&index, "kirby", "sound_turndash", SOUNDS)
            .expect_err("a fighter the project says nothing about is out of scope")
            .to_string();
        assert!(error.contains("sound_turndash"), "{error}");
    }

    /// Creation writes vanilla, the ordinary sync writes the edit, and the two together leave a
    /// function that differs from the mirror's on exactly one line.
    ///
    /// This is the whole point of splitting them: creation never learns what an edit is, and the
    /// value write never learns the function is new. A regenerating creator would have rewritten
    /// every line here and passed a round-trip while doing it.
    #[test]
    fn a_created_script_then_synced_differs_from_the_mirror_only_where_it_was_edited() {
        let (tmp, index) = mario_project();
        let source = SOUNDS.replace("sound_turndash", "sound_attackairn");
        create_script(&index, "mario", "sound_attackairn", &source).unwrap();
        let index = SourceIndex::build(tmp.path()).unwrap();

        let pristine = sounds_of(&source);
        let mut edited = pristine.clone();
        edited[0].call.sounds[0] = "se_mario_dash_start".into();
        let report = sync_sounds(&index, "mario", "attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);

        let site = index.script("mario", "sound_attackairn").unwrap();
        let written = std::fs::read_to_string(&site.file).unwrap();
        let written = &written[site.span.clone()];
        let differing: Vec<_> = written
            .lines()
            .zip(source.lines())
            .filter(|(now, was)| now != was)
            .collect();
        assert_eq!(differing.len(), 1, "{differing:?}");
        assert!(differing[0].0.contains("se_mario_dash_start"));
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

    #[test]
    fn generated_helper_scanner_only_accepts_direct_calls() {
        let text = r#"unsafe fn visionary_set_speed(agent: &mut L2CAgentBase, x: f32, y: f32) {}
// visionary_set_speed(agent, 1, 2);
let string = "visionary_set_speed(agent, 3, 4)";
foo.visionary_set_speed(agent, 5, 6);
macros::visionary_set_speed(agent, 7, 8);
visionary_set_speed(agent, 9, 10);
"#;
        let sites = scan_named_sites(text, "visionary_set_speed", 0..text.len());
        assert_eq!(sites.len(), 1, "found {sites:?}");
        assert_eq!(sites[0].arg(text, 1), Some("9"));
        assert_eq!(sites[0].arg(text, 2), Some("10"));
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
        let body = index.script_source("mario", "attack_air_n").unwrap().body;
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

    /// Retuning the spawn beside a Work ID line must leave that authored token untouched. The
    /// Work ID is a separate source value, not part of the spawn transform.
    #[test]
    fn syncing_a_spawn_does_not_delete_the_work_int_line_beside_it() {
        let source = r#"unsafe extern "C" fn effect_tornadostart(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("metaknight_tornado"), Hash40::new("trans"), 0, 0, 0, 0, 0, 0, 1, false);
        macros::LAST_EFFECT_SET_WORK_INT(agent, *FIGHTER_METAKNIGHT_STATUS_SPECIAL_N_SPIN_WORK_INT_EFFECT_HANDLE);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(source).to_effect_calls();
        let mut edited = pristine.clone();
        edited[0].offset[0] = 2.5;

        let (after, report) =
            rewrite_effect_calls(source, "kirby/tornadostart", &pristine, &edited).unwrap();
        assert_eq!(
            report.changed, 1,
            "only the spawn transform changed: {report:?}"
        );
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains("macros::LAST_EFFECT_SET_WORK_INT(agent, *FIGHTER_METAKNIGHT_STATUS_SPECIAL_N_SPIN_WORK_INT_EFFECT_HANDLE);"),
            "source write-back must not delete the typed C7 line:\n{after}"
        );
        assert_eq!(
            after
                .matches("macros::LAST_EFFECT_SET_WORK_INT(agent,")
                .count(),
            1,
            "the unmodeled line must remain exactly once:\n{after}"
        );
        assert!(
            after.contains(", 2.5, 0, 0, 0, 0, 0, 1, false);"),
            "{after}"
        );
    }

    #[test]
    fn work_int_write_back_changes_only_the_existing_work_id_token() {
        let source = r#"unsafe extern "C" fn effect_work(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("sys_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, true);
        macros::LAST_EFFECT_SET_WORK_INT(agent, *WORK_INT);
        macros::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT(agent, 0.25);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(source).to_effect_calls();
        assert_eq!(pristine[0].work_int.as_deref(), Some("WORK_INT"));
        let mut edited = pristine.clone();
        edited[0].work_int = Some("WORK_INT_ALT".into());

        let (after, report) =
            rewrite_effect_calls(source, "test/work_int", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("LAST_EFFECT_SET_WORK_INT(agent, *WORK_INT_ALT);"));
        assert!(after.contains("LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT(agent, 0.25);"));
        assert_eq!(
            crate::acmd::parse_effect_script(&after).to_effect_calls()[0]
                .work_int
                .as_deref(),
            Some("WORK_INT_ALT")
        );
    }

    /// C4's opaque lifetime controls are not source-editable fields yet. Retuning the spawn they
    /// follow must still leave each command in place, so a source sync cannot turn a preserved
    /// action into a deleted one.
    #[test]
    fn syncing_a_spawn_does_not_delete_opaque_c4_lifetime_lines() {
        let source = r#"unsafe extern "C" fn effect_c4(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("sys_smoke"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, true);
        macros::EFFECT_DETACH_KIND(agent, Hash40::new("sys_smoke"), 0);
        macros::EFFECT_DETACH_KIND_WORK(agent, *WORK_INT, 0);
        macros::ENABLE_AREA(agent, 2);
        macros::UNABLE_AREA(agent, 2);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(source).to_effect_calls();
        let mut edited = pristine.clone();
        edited[0].scale = 1.5;

        let (after, report) = rewrite_effect_calls(source, "test/c4", &pristine, &edited).unwrap();
        assert_eq!(
            report.changed, 1,
            "only the spawn scale changed: {report:?}"
        );
        assert!(report.skipped.is_empty(), "{report:?}");
        for line in [
            "macros::EFFECT_DETACH_KIND(agent, Hash40::new(\"sys_smoke\"), 0);",
            "macros::EFFECT_DETACH_KIND_WORK(agent, *WORK_INT, 0);",
            "macros::ENABLE_AREA(agent, 2);",
            "macros::UNABLE_AREA(agent, 2);",
        ] {
            assert_eq!(
                after.matches(line).count(),
                1,
                "source syncing must retain {line}:\n{after}"
            );
        }
        assert!(after.contains(", 1.5, true);"), "{after}");
    }

    #[test]
    fn c4_control_values_write_back_without_reordering_the_effect_script() {
        let source = r#"unsafe extern "C" fn effect_c4(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::EFFECT_DETACH_KIND(agent, Hash40::new("sys_smoke"), 0);
        macros::EFFECT_DETACH_KIND_WORK(agent, *WORK_INT, 0);
        macros::ENABLE_AREA(agent, 2);
        macros::UNABLE_AREA(agent, 2);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(source).to_effect_calls();
        assert_eq!(pristine.len(), 4);
        let mut edited = pristine.clone();
        edited[0].control = Some(crate::data::EffectControl::DetachKind {
            effect_name: "sys_smoke_alt".into(),
            unk: -1,
        });
        edited[1].control = Some(crate::data::EffectControl::DetachKindWork {
            work: "WORK_INT_ALT".into(),
            unk: -2,
        });
        edited[2].control = Some(crate::data::EffectControl::EnableArea { kind: "3".into() });
        edited[3].control = Some(crate::data::EffectControl::UnableArea { kind: "4".into() });

        let (after, report) = rewrite_effect_calls(source, "test/c4", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(
            report.changed, 6,
            "each edited C4 value is one source replacement"
        );
        assert!(after.contains("EFFECT_DETACH_KIND(agent, Hash40::new(\"sys_smoke_alt\"), -1);"));
        assert!(after.contains("EFFECT_DETACH_KIND_WORK(agent, *WORK_INT_ALT, -2);"));
        assert!(after.contains("ENABLE_AREA(agent, 3);"));
        assert!(after.contains("UNABLE_AREA(agent, 4);"));
        assert_eq!(
            crate::acmd::parse_effect_script(&after).to_effect_calls(),
            edited,
            "source write-back must parse back to the edited control values"
        );
    }

    /// The source editor and the editor panels drive each other, so a value written into the
    /// text has to parse back to exactly what was written. Any drift — a rounding difference,
    /// a reformat — reads as a fresh edit on the next frame and the two ping-pong forever.
    #[test]
    fn a_panel_edit_written_to_source_parses_back_unchanged() {
        let (_tmp, index) = mario_project();
        let body = index.script_source("mario", "attack_air_n").unwrap().body;
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
        let body = index.script_source("mario", "attack_air_n").unwrap().body;
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
        let body = index.script_source("mario", "attack_air_n").unwrap().body;
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
        let body = index.script_source("test_fighter", "test").unwrap().body;
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
        let body = index.script_source("mario", "attack_air_n").unwrap().body;
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

    /// A wrapper-form trail, in the shape `smash-script` actually declares.
    ///
    /// Hand-written, because no corpus script calls this spelling — the four vanilla trails are
    /// the raw `effect(*MA_MSC_CMD_EFFECT_AFTER_IMAGE3_ON, …)` form. So its *layout* is taken
    /// from the declaration rather than invented: 29 arguments, `trail_bone1` at 4 with
    /// `trail_x1/y1/z1` at 5..=7, `trail_bone2` at 8. The version this replaced put `sword2` at
    /// slot 5 and `0.75` at slot 8 — a `Hash40` where a coordinate goes and a float where the
    /// second joint goes — which was harmless only for as long as nothing read slot 8. It now
    /// does, and a fixture whose shape is made up would have had the editor offering `0.75` as
    /// an editable joint.
    const TRAIL: &str = r#"unsafe extern "C" fn effect_attacks4(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::AFTER_IMAGE4_ON_arg29(agent, Hash40::new("tex1"), Hash40::new("tex2"), 4, Hash40::new("sword1"), 0, 3, 0.25, Hash40::new("sword2"), 0, 26, 0.5, true, Hash40::new("null"), Hash40::new("haver"), 0, 0, 0, 0, 0, 0, 1, 0, *EFFECT_AXIS_X, 0, *TRAIL_BLEND_BLEND_SRC_ONE, 1, 0, 0.0, 0.0);
    }
}
"#;

    /// A trail's arguments are textures and trail parameters, and slots 5..=7 are its first
    /// edge's offset, NOT the spawn transform. Writing the spawn layout into them replaced the
    /// trail's own parameters with position values and reported success.
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

    /// The declared wrapper form yields both joints, from the slots the declaration names.
    ///
    /// This is the fixture's own guard: it reads the two joints out of positions holding
    /// *different* values, so a parser taking `trail_bone2` from slot 4, or `trail_bone1` from
    /// slot 8, fails here. On any vanilla call the two agree and no such test is possible.
    #[test]
    fn the_declared_wrapper_trail_yields_both_of_its_joints() {
        let calls = crate::acmd::parse_effect_script(TRAIL).to_effect_calls();
        assert_eq!(calls[0].bone_name, "sword1");
        assert_eq!(calls[0].trail_bone2.as_deref(), Some("sword2"));
    }

    /// Moving only the second joint is reported as the edit it is.
    ///
    /// **This pins the wording, and that is the whole of what it pins.** A trail meets two
    /// guards in sequence — `identity_matches` first, the "no transform to write" check second —
    /// and either alone leaves the source untouched, so nothing about *safety* distinguishes
    /// them. What differs is the sentence the user gets after editing `Bone 2` and syncing:
    /// "you changed a joint, and syncing only rewrites transform values", or "this call has no
    /// transform", which is true of the call and says nothing about their edit.
    ///
    /// Asserting on the substring `joint` does not separate those — both messages contain it,
    /// and dropping `trail_bone2` from `identity_matches` survived a version of this test that
    /// did. Whether the edit is *seen* at all is covered elsewhere: `differs` is a plain `!=`
    /// over the whole call and catches it either way.
    #[test]
    fn moving_only_the_second_joint_is_reported_as_a_change_source_syncing_will_not_make() {
        let pristine = crate::acmd::parse_effect_script(TRAIL).to_effect_calls();
        let mut edited = pristine.clone();
        edited[0].trail_bone2 = Some("blade".into());

        let (after, report) = rewrite_effect_calls(TRAIL, "t", &pristine, &edited).unwrap();
        assert_eq!(after, TRAIL, "a trail call must come back untouched");
        assert_eq!(report.changed, 0);
        assert_eq!(report.skipped.len(), 1, "{report:?}");
        assert!(
            report.skipped[0].contains("changed graphic, joint, timing, or enablement"),
            "the report must name what the user changed, not the transform this call never \
             had: {report:?}"
        );
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
        assert_eq!(
            after, text,
            "nothing may be inserted into the user's script"
        );
        assert!(
            report.skipped.iter().any(|s| s.contains("gained a rate")),
            "{report:?}"
        );
    }

    const CAMERA_OFFSETS: &str = r#"unsafe extern "C" fn effect_camera(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("sys_hit"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, true);
        macros::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT(agent, -5);
        macros::EFFECT_FOLLOW(agent, Hash40::new("sys_hit_2"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, true);
        macros::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT(agent, 0.4);
    }
}
"#;

    #[test]
    fn a_camera_flat_offset_edit_rewrites_only_its_own_modifier_line() {
        let pristine = crate::acmd::parse_effect_script(CAMERA_OFFSETS).to_effect_calls();
        assert_eq!(
            pristine.iter().map(|c| c.camera_offset).collect::<Vec<_>>(),
            vec![Some(-5.0), Some(0.4)]
        );
        let mut edited = pristine.clone();
        edited[1].camera_offset = Some(0.75);

        let (after, report) = rewrite_effect_calls(CAMERA_OFFSETS, "t", &pristine, &edited)
            .expect("camera-flat offset source rewrite");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(
            after.contains("macros::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT(agent, -5);"),
            "the first spawn's modifier must be untouched:\n{after}"
        );
        assert!(
            after.contains("macros::LAST_EFFECT_SET_OFFSET_TO_CAMERA_FLAT(agent, 0.75);"),
            "the second spawn's modifier must be retuned:\n{after}"
        );
    }

    #[test]
    fn a_particle_tint_edit_rewrites_only_its_particle_modifier_line() {
        const PARTICLE: &str = r#"unsafe extern "C" fn effect_particle(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 7.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_hit"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, true);
        macros::LAST_PARTICLE_SET_COLOR(agent, 0.1, 1.2, 0.3);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(PARTICLE).to_effect_calls();
        assert_eq!(pristine[0].particle_tint, Some([0.1, 1.2, 0.3]));
        let mut edited = pristine.clone();
        edited[0].particle_tint = Some([0.1, 0.5, 0.3]);
        let (after, report) = rewrite_effect_calls(PARTICLE, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(after.contains("macros::LAST_PARTICLE_SET_COLOR(agent, 0.1, 0.5, 0.3);"));
        assert!(!after.contains("LAST_EFFECT_SET_COLOR"));
    }

    #[test]
    fn a_dynamic_scale_w_edit_rewrites_only_its_native_value_list() {
        const SCALE_W: &str = r#"unsafe extern "C" fn effect_scale_w(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 7.0);
    if macros::is_excute(agent) {
        macros::EFFECT(agent, Hash40::new("sys_hit"), Hash40::new("top"), 0, 0, 0, 0, 0, 0, 1, true);
        macros::LAST_EFFECT_SET_SCALE_W(agent, 1, 2);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(SCALE_W).to_effect_calls();
        assert_eq!(pristine[0].scale_w, Some(vec![1.0, 2.0]));
        let mut edited = pristine.clone();
        edited[0].scale_w = Some(vec![1.0, 3.5]);

        let (after, report) = rewrite_effect_calls(SCALE_W, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(after.contains("macros::LAST_EFFECT_SET_SCALE_W(agent, 1, 3.5);"));
    }

    /// Two spawns, each with a different mix of the three modifiers, and a rate written *after*
    /// a tint on the same spawn — the case that decides whether a modifier line ends the run for
    /// the modifiers after it. It must not: both lines name the spawn above the pair.
    const MODIFIERS: &str = r#"unsafe extern "C" fn effect_attackairhi(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_FLIP(agent, Hash40::new("kirby_attack_arc"), Hash40::new("kirby_attack_arc"), Hash40::new("top"), -3, 7, 0, 0, 90, 90, 1, true, *EF_FLIP_YZ);
        macros::LAST_EFFECT_SET_COLOR(agent, 0.25, 1.3, 2.5);
        macros::LAST_EFFECT_SET_RATE(agent, 2);
        macros::EFFECT(agent, Hash40::new("sys_attack_impact"), Hash40::new("top"), 13, 6, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 360, false);
        macros::LAST_EFFECT_SET_ALPHA(agent, 0.7);
    }
}
"#;

    /// dolly/SpecialAirHiCommand's hurtbox lines, plus the `COL_PRI` pair, in one function.
    /// Two bones set and taken back means four `HIT_NODE` calls sharing two argument shapes —
    /// which is what makes writing to the wrong site visible instead of coincidentally right.
    const HURTBOXES: &str = r#"unsafe extern "C" fn game_specialairhicommand(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);
        macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_XLU);
        macros::COL_PRI(agent, 200);
    }
    frame(agent.lua_state_agent, 20.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_NORMAL);
        macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_NORMAL);
        macros::COL_NORMAL(agent);
    }
}
"#;

    fn hurt_of(
        text: &str,
    ) -> (
        Vec<crate::data::HurtboxState>,
        Vec<crate::data::ColPriState>,
    ) {
        crate::acmd::parse_acmd_script(text).to_hurtboxes()
    }

    /// kirby/ThrowF verbatim — two `ATTACK_ABS` sharing id 0, told apart only by kind.
    const THROW_ABS: &str = r#"unsafe extern "C" fn game_throwf(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::ATTACK_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, 0, 5.0, 75, 125, 0, 40, 0.0, 1.0, *ATTACK_LR_CHECK_F, 0.0, true, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_S, *COLLISION_SOUND_ATTR_NONE, *ATTACK_REGION_THROW);
        macros::ATTACK_ABS(agent, *FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH, 0, 3.0, 361, 100, 0, 60, 0.0, 1.0, *ATTACK_LR_CHECK_F, 0.0, true, Hash40::new("collision_attr_normal"), *ATTACK_SOUND_LEVEL_S, *COLLISION_SOUND_ATTR_NONE, *ATTACK_REGION_THROW);
    }
}
"#;

    /// Both calls carry id 0, so the sync has to match on kind. Getting that wrong writes the
    /// throw's damage into the catch line, which compiles and is wrong.
    #[test]
    fn an_absolute_hit_is_retuned_by_kind_rather_than_by_its_shared_id() {
        let pristine = crate::acmd::parse_acmd_script(THROW_ABS).to_hitboxes();
        assert_eq!(pristine.len(), 2);
        let mut edited = pristine.clone();
        edited[1].damage = 4.5;

        let (after, report) = rewrite_hitboxes(THROW_ABS, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1, "one argument span");
        assert!(
            after.contains("*FIGHTER_ATTACK_ABSOLUTE_KIND_CATCH, 0, 4.5, 361,"),
            "the catch line takes the edit:\n{after}"
        );
        assert!(
            after.contains("*FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, 0, 5.0, 75,"),
            "the throw line beside it must be untouched:\n{after}"
        );
    }

    /// The slot order is not `ATTACK`'s. Writing through that table would land each value one
    /// or more places off, so every editable slot is checked against its own position.
    #[test]
    fn every_editable_absolute_slot_lands_in_its_own_position() {
        let pristine = crate::acmd::parse_acmd_script(THROW_ABS).to_hitboxes();
        let mut edited = pristine.clone();
        let hb = &mut edited[0];
        hb.damage = 6.5;
        hb.angle = 45;
        hb.kb_scaling = 130;
        hb.fkb = 7;
        hb.kb_base = 55;
        hb.hitlag_mult = 0.5;
        hb.sound_level = "ATTACK_SOUND_LEVEL_L".into();
        hb.attack_region = "ATTACK_REGION_NONE".into();

        let (after, report) = rewrite_hitboxes(THROW_ABS, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains(
                "*FIGHTER_ATTACK_ABSOLUTE_KIND_THROW, 0, 6.5, 45, 130, 7, 55, 0.5, 1.0, \
                 *ATTACK_LR_CHECK_F, 0.0, true, Hash40::new(\"collision_attr_normal\"), \
                 *ATTACK_SOUND_LEVEL_L, *COLLISION_SOUND_ATTR_NONE, *ATTACK_REGION_NONE"
            ),
            "every value in its own slot, and the three unknowns untouched:\n{after}"
        );
        // Re-reading the file must produce exactly what the editor had.
        let reparsed = crate::acmd::parse_acmd_script(&after).to_hitboxes();
        assert_eq!(reparsed, edited);
    }

    #[test]
    fn a_status_edit_rewrites_only_that_ones_argument() {
        let pristine = hurt_of(HURTBOXES);
        assert_eq!(pristine.0.len(), 4, "two bones, set and taken back");

        // Make the knee fully invincible rather than intangible, on its opening call only.
        let mut edited = pristine.clone();
        edited.0[0].status = "HIT_STATUS_INVINCIBLE".into();
        let (after, report) = rewrite_hurtboxes(HURTBOXES, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1, "one argument span, not one line");
        assert!(
            after.contains(
                r#"macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_INVINCIBLE);"#
            ),
            "{after}"
        );
        // The three calls that did not change must be byte-identical, including the one that
        // shares this bone and the one that shares this status.
        for line in [
            r#"macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_XLU);"#,
            r#"macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_NORMAL);"#,
            r#"macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_NORMAL);"#,
        ] {
            assert!(after.contains(line), "{line} was disturbed:\n{after}");
        }
    }

    /// A `WHOLE_HIT` between two `HIT_NODE`s, so a status edit has to pick the right slot *and*
    /// the site ordinals either side of it have to stay put.
    const HURTBOXES_WHOLE: &str = r#"unsafe extern "C" fn game_finalstart(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 1.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);
        macros::WHOLE_HIT(agent, *HIT_STATUS_XLU);
        macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_XLU);
    }
}
"#;

    /// The status of a `WHOLE_HIT` lives one slot earlier than the other two members', and this
    /// path writes by slot index into the user's own text.
    ///
    /// Writing slot 2 unconditionally would land past the end of this call's arguments and drop
    /// the edit silently — the user changes a value, the panel shows the change, and their file
    /// keeps the old status. Both neighbours share the edited status, so a write to the wrong
    /// site shows up here rather than being masked.
    #[test]
    fn a_whole_body_status_edit_lands_in_the_call_and_not_past_its_arguments() {
        let pristine = hurt_of(HURTBOXES_WHOLE);
        assert_eq!(pristine.0.len(), 3, "two bones and the whole body");
        assert_eq!(pristine.0[1].target, crate::data::HurtTarget::Whole);

        let mut edited = pristine.clone();
        edited.0[1].status = "HIT_STATUS_INVINCIBLE".into();
        let (after, report) = rewrite_hurtboxes(HURTBOXES_WHOLE, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(
            after.contains("macros::WHOLE_HIT(agent, *HIT_STATUS_INVINCIBLE);"),
            "{after}"
        );
        for line in [
            r#"macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);"#,
            r#"macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_XLU);"#,
        ] {
            assert!(after.contains(line), "{line} was disturbed:\n{after}");
        }
    }

    /// A modifier between two hurtbox calls, so a family that miscounted sites is visible.
    const ATTACK_MODS_SRC: &str = r#"unsafe extern "C" fn game_attacklw4(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 10.0);
    if macros::is_excute(agent) {
        macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);
        macros::ATK_SET_SHIELD_SETOFF_MUL(agent, 0, 7);
        macros::ATK_POWER(agent, 1, 10);
        macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_XLU);
    }
}
"#;

    fn mods_of(text: &str) -> Vec<crate::data::AttackModState> {
        crate::acmd::parse_acmd_script(text).to_attack_mods()
    }

    /// A value edit rewrites that argument and nothing else, keeping the bare-integer spelling.
    ///
    /// The two calls sit next to each other with different macros, so writing to the wrong site
    /// shows up rather than being masked. `10` must not become `10.0`: these slots are
    /// `ToF32`-generic, so both compile, and churning an untouched spelling is the diff noise
    /// `to_f32_edit` exists to avoid.
    #[test]
    fn a_modifier_value_edit_rewrites_only_that_argument() {
        let pristine = mods_of(ATTACK_MODS_SRC);
        assert_eq!(pristine.len(), 2);
        let mut edited = pristine.clone();
        edited[1].value = 14.0;
        let (after, report) =
            rewrite_attack_mods(ATTACK_MODS_SRC, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(
            after.contains("macros::ATK_POWER(agent, 1, 14);"),
            "{after}"
        );
        assert!(
            after.contains("macros::ATK_SET_SHIELD_SETOFF_MUL(agent, 0, 7);"),
            "the neighbouring modifier was disturbed:\n{after}"
        );
    }

    /// The id is slot 1 and the value slot 2 — the one thing the corpus could not have proved.
    ///
    /// Every vanilla `ATK_SET_SHIELD_SETOFF_MUL` is the identical `(agent, 0, 7)`, so only
    /// `macros.rs` says which slot is which. Editing the id alone would silently rewrite the
    /// value if the two were ever transposed.
    #[test]
    fn a_modifier_id_edit_writes_the_id_slot_and_leaves_the_value() {
        let pristine = mods_of(ATTACK_MODS_SRC);
        let mut edited = pristine.clone();
        edited[0].id = 3;
        let (after, report) =
            rewrite_attack_mods(ATTACK_MODS_SRC, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains("macros::ATK_SET_SHIELD_SETOFF_MUL(agent, 3, 7);"),
            "{after}"
        );
    }

    /// The two families must not share a numbering space.
    ///
    /// Both `HIT_NODE`s here sit either side of two modifiers. If the modifiers consumed hurtbox
    /// sites — or the hurtbox calls consumed modifier sites — an edit to the *second* `HIT_NODE`
    /// would land on whatever the shifted ordinal pointed at. This is the failure `HURT_COMMANDS`
    /// already carries a warning about, checked across the family boundary.
    #[test]
    fn the_two_families_number_their_sites_independently() {
        let hurt_pristine = hurt_of(ATTACK_MODS_SRC);
        let mut hurt_edited = hurt_pristine.clone();
        hurt_edited.0[1].status = "HIT_STATUS_INVINCIBLE".into();
        let (after, report) =
            rewrite_hurtboxes(ATTACK_MODS_SRC, "t", &hurt_pristine, &hurt_edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains(
                r#"macros::HIT_NODE(agent, Hash40::new("legl"), *HIT_STATUS_INVINCIBLE);"#
            ),
            "the second HIT_NODE should have been edited:\n{after}"
        );
        // And nothing in the modifier lines moved.
        assert!(
            after.contains("macros::ATK_POWER(agent, 1, 10);"),
            "{after}"
        );
    }

    /// Changing which macro a modifier is, is structure rather than value.
    #[test]
    fn changing_a_modifier_to_the_other_macro_is_reported_not_written() {
        let pristine = mods_of(ATTACK_MODS_SRC);
        let mut edited = pristine.clone();
        edited[0].kind = crate::data::AttackModKind::Power;
        let (after, report) =
            rewrite_attack_mods(ATTACK_MODS_SRC, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 0);
        assert!(
            report.skipped.iter().any(|s| s.contains("ATK_POWER")),
            "{report:?}"
        );
        assert_eq!(after, ATTACK_MODS_SRC, "a reported edit changes nothing");
    }

    /// Turning a whole-body state into a per-bone one is a different macro, so it is reported
    /// rather than written — the same rule `HIT_NODE` ↔ `HIT_NO` already follows, and the reason
    /// is sharper here: the target it would need has no slot to go in.
    #[test]
    fn retargeting_a_whole_body_state_to_a_bone_is_reported_not_written() {
        let pristine = hurt_of(HURTBOXES_WHOLE);
        let mut edited = pristine.clone();
        edited.0[1].target = crate::data::HurtTarget::Bone("kneer".into());
        let (after, report) = rewrite_hurtboxes(HURTBOXES_WHOLE, "t", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 0);
        assert!(
            report.skipped.iter().any(|s| s.contains("WHOLE_HIT")),
            "{report:?}"
        );
        assert_eq!(after, HURTBOXES_WHOLE, "a reported edit changes nothing");
    }

    #[test]
    fn a_bone_rename_and_a_priority_edit_land_in_their_own_calls() {
        let pristine = hurt_of(HURTBOXES);
        let mut edited = pristine.clone();
        edited.0[1].target = crate::data::HurtTarget::Bone("legr".into());
        edited.1[0].pri = 150;
        let (after, report) = rewrite_hurtboxes(HURTBOXES, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 2);
        assert!(
            after.contains(r#"macros::HIT_NODE(agent, Hash40::new("legr"), *HIT_STATUS_XLU);"#),
            "{after}"
        );
        assert!(after.contains("macros::COL_PRI(agent, 150);"), "{after}");
        // `COL_PRI` is the third hurtbox call in the file but the first priority span. Pairing
        // those two lists by position rather than by site would have written 150 into a
        // `HIT_NODE`.
        assert!(
            after.contains(r#"macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_XLU);"#),
            "the priority edit must not have landed in a bone call:\n{after}"
        );
    }

    /// `HIT_NODE` and `HIT_NO` take a hash and an integer in the same slot. Retuning across
    /// that boundary would write a `Hash40::new(…)` where the macro wants a number — a call
    /// that does not compile — so it is reported instead.
    #[test]
    fn changing_a_bone_into_a_numbered_group_is_reported_rather_than_written() {
        let pristine = hurt_of(HURTBOXES);
        let mut edited = pristine.clone();
        edited.0[0].target = crate::data::HurtTarget::Group(8);
        let (after, report) = rewrite_hurtboxes(HURTBOXES, "t", &pristine, &edited).unwrap();
        assert_eq!(after, HURTBOXES, "nothing may be written");
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("HIT_NODE") && s.contains("HIT_NO")),
            "the report must name both macros: {report:?}"
        );
    }

    /// A state's frame is the `frame(...)` block it sits in, not an argument of the call, so
    /// moving one has to be reported the way a retimed hitbox is.
    #[test]
    fn retiming_a_hurtbox_state_is_reported_and_its_other_edits_still_land() {
        let pristine = hurt_of(HURTBOXES);
        let mut edited = pristine.clone();
        edited.0[0].active_start = 4;
        edited.0[0].status = "HIT_STATUS_OFF".into();
        let (after, report) = rewrite_hurtboxes(HURTBOXES, "t", &pristine, &edited).unwrap();
        assert!(
            report.skipped.iter().any(|s| s.contains("retimed")),
            "{report:?}"
        );
        assert!(
            after.contains(r#"macros::HIT_NODE(agent, Hash40::new("kneer"), *HIT_STATUS_OFF);"#),
            "the value edit is still worth writing:\n{after}"
        );
    }

    /// `kirby/TurnDash` verbatim — the sound write-back's fixture.
    ///
    /// Chosen because it holds all three shapes in one function: a one-hash call, the one
    /// member with a trailing non-hash argument, and two `PLAY_STEP_FLIPPABLE`s that name the
    /// same two sounds in opposite order. That last pair is what a write-back keyed on the
    /// *sound name* rather than on the site would get wrong.
    const SOUNDS: &str = r#"unsafe extern "C" fn sound_turndash(agent: &mut L2CAgentBase) {
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

    fn sounds_of(text: &str) -> Vec<crate::data::SoundEvent> {
        crate::acmd::parse_sound_script(text).to_sound_events()
    }

    /// Renaming a sound rewrites that one argument and leaves the rest of the file alone.
    ///
    /// The second footstep is the interesting one: it names the same two sounds as the first,
    /// swapped. Editing only its left channel has to leave three other spans holding those very
    /// strings untouched, which is what says the edit was placed by site.
    #[test]
    fn renaming_one_sound_rewrites_only_that_argument() {
        let pristine = sounds_of(SOUNDS);
        let mut edited = pristine.clone();
        edited[3].call.sounds[0] = "se_common_step_left_m".into();
        let (after, report) = rewrite_sounds(SOUNDS, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(
            after.contains(
                r#"macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_common_step_left_m"), Hash40::new("se_kirby_step_left_m"));"#
            ),
            "{after}"
        );
        // The first footstep names the same two sounds the other way round and must be intact.
        assert!(
            after.contains(
                r#"macros::PLAY_STEP_FLIPPABLE(agent, Hash40::new("se_kirby_step_left_m"), Hash40::new("se_kirby_step_right_m"));"#
            ),
            "the edit landed in the wrong footstep:\n{after}"
        );
        // And exactly one line differs — an argument rewrite, not a re-emission of the function.
        let changed_lines = SOUNDS
            .lines()
            .zip(after.lines())
            .filter(|(before, now)| before != now)
            .count();
        assert_eq!(changed_lines, 1, "the write-back reformatted the file");
    }

    /// A sound in one macro moved to another is structure, not a value, and is reported.
    ///
    /// `PLAY_SE` and `PLAY_STEP_FLIPPABLE` do not even take the same number of arguments, so
    /// writing one over the other produces a call that does not compile. The report names the
    /// macro because that is the part the user has to undo.
    #[test]
    fn changing_which_sound_macro_is_called_is_reported_rather_than_written() {
        let pristine = sounds_of(SOUNDS);
        let mut edited = pristine.clone();
        edited[0].call.func = "PLAY_STEP_FLIPPABLE".into();
        let (after, report) = rewrite_sounds(SOUNDS, "t", &pristine, &edited).unwrap();
        assert_eq!(after, SOUNDS, "nothing may be written");
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("PLAY_SE") && s.contains("PLAY_STEP_FLIPPABLE")),
            "the report must name both macros: {report:?}"
        );
    }

    /// Moving a sound to another frame is reported, and a rename in the same pass still lands.
    #[test]
    fn retiming_a_sound_is_reported_and_other_edits_still_land() {
        let pristine = sounds_of(SOUNDS);
        let mut edited = pristine.clone();
        edited[0].frame = 9;
        edited[0].call.sounds[0] = "se_common_dash_start".into();
        edited[1].call.sounds[0] = "se_common_dash_start".into();
        let (after, report) = rewrite_sounds(SOUNDS, "t", &pristine, &edited).unwrap();
        assert!(
            report.skipped.iter().any(|s| s.contains("retimed")),
            "{report:?}"
        );
        assert!(
            !after.contains(r#"macros::PLAY_SE(agent, Hash40::new("se_common_dash_start"));"#),
            "a retimed call must not also be rewritten:\n{after}"
        );
        assert!(
            after.contains(
                r#"macros::SET_PLAY_INHIVIT(agent, Hash40::new("se_common_dash_start"), 20);"#
            ),
            "the untouched call's own rename is still worth writing:\n{after}"
        );
    }

    /// A sound call written with the wrong number of arguments is not a site.
    ///
    /// The parser refuses one and leaves the line `Raw`, so the IR never numbers it. A scan
    /// matching on the macro name alone would number it anyway, and every site after it would
    /// resolve one line too early — here, renaming the footstep would rewrite the broken call
    /// above it and leave the footstep untouched.
    ///
    /// Nothing in the 301-script corpus is written this way, so the corpus oracle cannot reach
    /// it, and a mutation removing the arity filter passed every other test in this file. Hand
    /// authoring is exactly where it would come from: it is a mistake a person makes and the
    /// game's own scripts do not.
    #[test]
    fn a_malformed_sound_call_does_not_take_a_site() {
        const MALFORMED: &str = r#"unsafe extern "C" fn sound_typo(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::PLAY_SE(agent);
        macros::PLAY_SE(agent, Hash40::new("se_kirby_dash_start"));
    }
}
"#;
        assert_eq!(
            sound_sites(MALFORMED).len(),
            1,
            "the argument-less call must not be scanned as a site"
        );

        let pristine = sounds_of(MALFORMED);
        assert_eq!(pristine.len(), 1, "only the well-formed call is an event");
        let mut edited = pristine.clone();
        edited[0].call.sounds[0] = "se_common_dash_start".into();
        let (after, report) = rewrite_sounds(MALFORMED, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains(r#"macros::PLAY_SE(agent, Hash40::new("se_common_dash_start"));"#),
            "the rename did not land on the call it named:\n{after}"
        );
        assert!(
            after.contains("macros::PLAY_SE(agent);"),
            "the broken call was rewritten instead:\n{after}"
        );
    }

    /// A looped call is one line and several events, so the same span is reached more than once.
    ///
    /// Writing it twice would hand `apply` two edits over the same bytes. Agreeing edits collapse
    /// to one; disagreeing ones are a genuine conflict the user has to resolve, and are reported
    /// rather than decided by whichever iteration happened to be last in the list.
    #[test]
    fn a_looped_sound_edited_once_is_written_once() {
        const LOOPED: &str = r#"unsafe extern "C" fn sound_looped(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    for _ in 0..3 {
    wait(agent.lua_state_agent, 2.0);
    if macros::is_excute(agent) {
        macros::PLAY_SE(agent, Hash40::new("se_kirby_dash_start"));
    }
    }
}
"#;
        let pristine = sounds_of(LOOPED);
        assert_eq!(pristine.len(), 3, "the loop should unroll to three events");

        let mut edited = pristine.clone();
        for event in &mut edited {
            event.call.sounds[0] = "se_common_dash_start".into();
        }
        let (after, report) = rewrite_sounds(LOOPED, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1, "one line, one edit");
        assert_eq!(
            after.matches("se_common_dash_start").count(),
            1,
            "the loop body was written three times:\n{after}"
        );

        // Two iterations disagreeing is not something the panel can produce today — it edits the
        // statement, so every event moves together. It is asserted anyway because the *next*
        // thing to touch this, a per-event editor, would produce it silently.
        let mut split = pristine.clone();
        split[0].call.sounds[0] = "se_common_dash_start".into();
        split[1].call.sounds[0] = "se_common_dash_stop".into();
        let (_, report) = rewrite_sounds(LOOPED, "t", &pristine, &split).unwrap();
        assert!(
            report.skipped.iter().any(|s| s.contains("two different")),
            "a conflict must be reported, not resolved: {report:?}"
        );
    }

    /// The write-back matches a call to its line by counting spawn macros in source order, so
    /// C6 teaching the parser to nest conditionals instead of flattening them could have shifted
    /// every ordinal in a guarded script. It does not — the ordinal walk descends into a `Cond`
    /// body in place — and this pins that on the shape that would break first.
    ///
    /// The costume tint matters for a second reason. It is carried through the *export*
    /// verbatim, but the write-back edits the user's own file, which already contains it. So it
    /// has to come out of this untouched: a sync that also wrote the carried copy would be
    /// stacking the export's duplicate on top of the original.
    #[test]
    fn a_costume_guard_does_not_shift_the_write_backs_ordinals() {
        const GUARDED: &str = r#"unsafe extern "C" fn effect_test(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 9.0);
    if get_value_float(agent.lua_state_agent, *SO_VAR_FLOAT_LR) < 0.0 {
        if macros::is_excute(agent) {
            macros::EFFECT_FOLLOW(agent, Hash40::new("dolly_roll_l"), Hash40::new("throw"), 0, 2.5, 0, 0, 0, 0, 1, true);
        }
    }
    if(0x2508e0(*FIGHTER_INSTANCE_WORK_ID_INT_COLOR, 0)){
        if macros::is_excute(agent) {
            macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);
        }
    }
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW(agent, Hash40::new("dolly_roll_r"), Hash40::new("throw"), 0, 2.5, 0, 0, 0, 0, 1, true);
    }
}
"#;
        let pristine = crate::acmd::parse_effect_script(GUARDED).to_effect_calls();
        assert_eq!(pristine.len(), 2, "both spawns are still resolved");
        assert_eq!(
            pristine[0].guard.as_deref(),
            Some("if get_value_float(agent.lua_state_agent, *SO_VAR_FLOAT_LR) < 0.0 {"),
            "the first spawn is the guarded one"
        );

        // Edit the SECOND spawn — the one past both the guard and the carried tint. Had the
        // conditional shifted the ordinals, this would land on the first spawn's line instead.
        let mut edited = pristine.clone();
        edited[1].offset = [1.5, 2.5, 0.0];
        let (after, report) = rewrite_effect_calls(GUARDED, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains(r#"Hash40::new("dolly_roll_r"), Hash40::new("throw"), 1.5, 2.5, 0"#),
            "the edit must land on the spawn past the guard:\n{after}"
        );
        assert!(
            after.contains(r#"Hash40::new("dolly_roll_l"), Hash40::new("throw"), 0, 2.5, 0"#),
            "the guarded spawn must be untouched:\n{after}"
        );
        assert_eq!(
            after.matches("LAST_EFFECT_SET_COLOR").count(),
            1,
            "the tint is already in the user's file; carrying it through the export must not \
             also insert a second copy here:\n{after}"
        );
        assert!(
            after.contains("macros::LAST_EFFECT_SET_COLOR(agent, 0.146, 0.205, 0.333);"),
            "and its value must not move:\n{after}"
        );
    }

    #[test]
    fn a_tint_edit_rewrites_only_its_own_spawns_colour_line() {
        let pristine = crate::acmd::parse_effect_script(MODIFIERS).to_effect_calls();
        assert_eq!(pristine.len(), 2);
        // The rate below the tint still found its spawn, which is the whole point of the
        // fixture: a modifier does not break the run for the modifier after it.
        assert_eq!(pristine[0].tint, Some([0.25, 1.3, 2.5]));
        assert_eq!(pristine[0].rate, Some(2.0));
        assert_eq!(pristine[1].alpha, Some(0.7));

        let mut edited = pristine.clone();
        edited[0].tint = Some([0.25, 0.5, 2.5]);
        let (after, report) = rewrite_effect_calls(MODIFIERS, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        // One component moved, so one argument span is rewritten — not the whole line, and not
        // the two components that did not change.
        assert_eq!(report.changed, 1);
        assert!(
            after.contains("macros::LAST_EFFECT_SET_COLOR(agent, 0.25, 0.5, 2.5);"),
            "only the green component may move:\n{after}"
        );
        assert!(
            after.contains("macros::LAST_EFFECT_SET_RATE(agent, 2);")
                && after.contains("macros::LAST_EFFECT_SET_ALPHA(agent, 0.7);"),
            "recolouring one spawn must not disturb any other modifier:\n{after}"
        );
    }

    #[test]
    fn an_opacity_edit_rewrites_only_its_own_spawns_alpha_line() {
        let pristine = crate::acmd::parse_effect_script(MODIFIERS).to_effect_calls();
        let mut edited = pristine.clone();
        edited[1].alpha = Some(0.3);
        let (after, report) = rewrite_effect_calls(MODIFIERS, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 1);
        assert!(
            after.contains("macros::LAST_EFFECT_SET_ALPHA(agent, 0.3);"),
            "{after}"
        );
        assert!(
            after.contains("macros::LAST_EFFECT_SET_COLOR(agent, 0.25, 1.3, 2.5);"),
            "the other spawn's tint must be untouched:\n{after}"
        );
    }

    /// Each modifier lives on a line of its own, so switching one on or off adds or deletes a
    /// call. Structural, and named rather than guessed at — the same rule the rate follows, and
    /// the reason all three share `modifier_edits`.
    #[test]
    fn turning_a_tint_or_opacity_on_or_off_is_reported_rather_than_written() {
        let pristine = crate::acmd::parse_effect_script(MODIFIERS).to_effect_calls();

        let mut removed = pristine.clone();
        removed[0].tint = None;
        let (after, report) = rewrite_effect_calls(MODIFIERS, "t", &pristine, &removed).unwrap();
        assert_eq!(after, MODIFIERS, "the user's line must still be there");
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("lost its tint") && s.contains("LAST_EFFECT_SET_COLOR")),
            "{report:?}"
        );

        // The second spawn has no colour line at all, so there is nowhere for one to be written.
        let mut added = pristine.clone();
        added[1].tint = Some([1.0, 0.0, 0.0]);
        let (after, report) = rewrite_effect_calls(MODIFIERS, "t", &pristine, &added).unwrap();
        assert_eq!(after, MODIFIERS, "nothing may be inserted into the script");
        assert!(
            report.skipped.iter().any(|s| s.contains("gained a tint")),
            "{report:?}"
        );

        let mut faded = pristine.clone();
        faded[1].alpha = None;
        let (after, report) = rewrite_effect_calls(MODIFIERS, "t", &pristine, &faded).unwrap();
        assert_eq!(after, MODIFIERS);
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("lost its opacity") && s.contains("LAST_EFFECT_SET_ALPHA")),
            "{report:?}"
        );
    }

    /// Kirby's dash attack, verbatim: a spawn and four colour commands, which is what makes
    /// this a test of the ordinals as much as of the values. A colour command produces a call,
    /// so it consumes an ordinal; if it did not, every edit after the first `BURN_COLOR` would
    /// be written into the line belonging to a different call.
    const COLORS: &str = r#"unsafe extern "C" fn effect_attackdash(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::EFFECT_FOLLOW_NO_STOP(agent, Hash40::new("kirby_dash"), Hash40::new("top"), 0, 6, 5, -90, 0, 160, 0.7, true);
    }
    frame(agent.lua_state_agent, 9.0);
    if macros::is_excute(agent) {
        macros::BURN_COLOR(agent, 2, 0.059, 0.008, 0);
        macros::BURN_COLOR_FRAME(agent, 4, 2, 0.059, 0.008, 0.9);
    }
    frame(agent.lua_state_agent, 42.0);
    if macros::is_excute(agent) {
        macros::BURN_COLOR_NORMAL(agent);
    }
}
"#;

    #[test]
    fn a_colour_edit_rewrites_only_its_own_commands_arguments() {
        let pristine = crate::acmd::parse_effect_script(COLORS).to_effect_calls();
        assert_eq!(pristine.len(), 4);

        let mut edited = pristine.clone();
        // The interpolating half of the pair only: the snap above it keeps its own values, so
        // a wrong ordinal or a wrong slot table shows up as the wrong line changing.
        edited[2].color = Some(crate::data::ColorCall {
            transition: Some(6.0),
            rgba: Some([2.0, 0.5, 0.25, 0.9]),
        });

        let (after, report) = rewrite_effect_calls(COLORS, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains("macros::BURN_COLOR(agent, 2, 0.059, 0.008, 0);"),
            "the snap above must be untouched:\n{after}"
        );
        assert!(
            after.contains("macros::BURN_COLOR_FRAME(agent, 6, 2, 0.5, 0.25, 0.9);"),
            "the length is slot 1 and the colour follows it — and 6, not 6.0:\n{after}"
        );
        assert!(
            after.contains("macros::BURN_COLOR_NORMAL(agent);"),
            "an argument-less command has nothing to write and must survive:\n{after}"
        );
        // Everything else in the file, comments and formatting included, byte for byte.
        assert_eq!(
            after.replace("macros::BURN_COLOR_FRAME(agent, 6, 2, 0.5, 0.25, 0.9);", ""),
            COLORS.replace(
                "macros::BURN_COLOR_FRAME(agent, 4, 2, 0.059, 0.008, 0.9);",
                ""
            ),
        );
    }

    /// Swapping `BURN_COLOR` for `BURN_COLOR_FRAME` adds an argument the existing call does not
    /// have. That is a change of command, not of value, so it lands in an export and is
    /// reported here — writing the length into slot 1 would put it in the red channel.
    #[test]
    fn changing_which_colour_command_a_call_is_gets_reported() {
        let pristine = crate::acmd::parse_effect_script(COLORS).to_effect_calls();
        let mut edited = pristine.clone();
        edited[1].spawn_func = "BURN_COLOR_FRAME".into();
        edited[1].color = Some(crate::data::ColorCall {
            transition: Some(3.0),
            rgba: Some([2.0, 0.059, 0.008, 0.0]),
        });

        let (after, report) = rewrite_effect_calls(COLORS, "t", &pristine, &edited).unwrap();
        assert_eq!(
            after, COLORS,
            "the user's call must be left exactly as written"
        );
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.contains("changed graphic, joint, timing, or enablement")),
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

    /// A detection box is retuned through its own slots, in whichever shape it was written.
    ///
    /// The two calls here are the same box in the two forms the corpus uses, and the masks sit
    /// three slots apart between them. Editing the same property in both is what proves the
    /// tail is being located rather than assumed — a fixed slot number passes for one form and
    /// silently writes the situation mask over the hit status in the other.
    #[test]
    fn a_search_box_is_retuned_through_the_shape_it_was_written_in() {
        let text = r#"unsafe extern "C" fn game_search(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::SEARCH(agent, 0, 0, Hash40::new("top"), 4.0, 0.0, 7.0, 8.0, *COLLISION_KIND_MASK_ATTACK, *HIT_STATUS_MASK_ALL, 0, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_FIGHTER, *COLLISION_PART_MASK_ALL, false);
        macros::SEARCH(agent, 1, 0, Hash40::new("top"), 7.5, 0.0, 7.0, 4.0, 0.0, 7.0, 13.0, *COLLISION_KIND_MASK_ATTACK, *HIT_STATUS_MASK_NORMAL, 60, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_ALL, *COLLISION_PART_MASK_ALL, false);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 2, "{pristine:#?}");

        let mut edited = pristine.clone();
        // The short-form box: edit a slot on each side of the absent capsule.
        edited[0].size = 5.25;
        edited[0].situation_mask = "COLLISION_SITUATION_MASK_G".into();
        edited[0].search.as_mut().unwrap().collision_kind = "COLLISION_KIND_MASK_GRAB".into();
        // The long-form box: the same two properties, three slots further along.
        edited[1].offset_z = 6.5;
        edited[1].category_mask = "COLLISION_CATEGORY_MASK_FIGHTER".into();
        edited[1].search.as_mut().unwrap().hit_status = "HIT_STATUS_MASK_XLU".into();

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_hitboxes(),
            edited,
            "\n{after}"
        );

        // The two arguments no panel exposes are untouched in both shapes, and a property left
        // alone on one box keeps what it had even though the other box changed it.
        assert!(after.contains("*HIT_STATUS_MASK_ALL"), "{after}");
        assert!(after.contains("*COLLISION_KIND_MASK_ATTACK"), "{after}");
        assert!(
            after.contains(", 60, "),
            "the undocumented slot survives\n{after}"
        );
        assert!(after.contains("false);"), "{after}");
    }

    /// Giving a capsule to a search box with no capsule slots is reported, never written.
    ///
    /// Slot 8 holds `*COLLISION_KIND_MASK_ATTACK` in the short form. This is the same refusal
    /// `CATCH` makes, and it is here as its own test because the slot number differs — a fix
    /// applied to one family says nothing about the other.
    #[test]
    fn adding_a_capsule_to_a_short_form_search_is_refused() {
        let text = r#"unsafe extern "C" fn game_search(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::SEARCH(agent, 0, 0, Hash40::new("top"), 4.0, 0.0, 7.0, 8.0, *COLLISION_KIND_MASK_ATTACK, *HIT_STATUS_MASK_ALL, 0, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_FIGHTER, *COLLISION_PART_MASK_ALL, false);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        let mut edited = pristine.clone();
        edited[0].capsule_end = Some([1.0, 2.0, 3.0]);
        edited[0].size = 9.0;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(
            after.contains("*COLLISION_KIND_MASK_ATTACK"),
            "what the box looks for must survive a refused capsule edit\n{after}"
        );
        assert!(!after.contains("Some(1.0)"), "{after}");
        assert!(
            report.skipped.iter().any(|s| s.contains("capsule end")),
            "{report:?}"
        );
        assert!(after.contains("Hash40::new(\"top\"), 9.0,"), "{after}");
    }

    /// A search box never retunes the size modifier that shares its name.
    ///
    /// `SET_SEARCH_SIZE_EXIST(agent, 0, 7)` names the same id. Matching sites by prefix, or
    /// pooling every collision into one candidate list, would write a bone hash into its size.
    #[test]
    fn a_search_edit_does_not_reach_the_search_size_modifier() {
        let text = r#"unsafe extern "C" fn game_search(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::SEARCH(agent, 0, 0, Hash40::new("top"), 4.0, 0.0, 7.0, 8.0, *COLLISION_KIND_MASK_ATTACK, *HIT_STATUS_MASK_ALL, 0, *COLLISION_SITUATION_MASK_GA, *COLLISION_CATEGORY_MASK_FIGHTER, *COLLISION_PART_MASK_ALL, false);
        macros::SET_SEARCH_SIZE_EXIST(agent, 0, 7);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 1, "only the box is a box\n{pristine:#?}");
        let mut edited = pristine.clone();
        edited[0].size = 5.5;
        edited[0].bone_name = "ArmL".into();

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains("macros::SET_SEARCH_SIZE_EXIST(agent, 0, 7);"),
            "the modifier is left exactly as it was\n{after}"
        );
        assert!(after.contains("Hash40::new(\"arml\"), 5.5,"), "{after}");
    }

    /// Giving a capsule to a grab that has no capsule slots is reported, never written.
    ///
    /// In the Lua-shaped form the three endpoint arguments are absent, so slots 7 and 8 hold
    /// the status kind and the situation mask. Writing `Some(1.0)` over them would silently
    /// turn Kirby's inhale into a grab that neither swallows nor compiles. Adding the capsule
    /// needs three arguments *inserted*, which a slot rewrite cannot do — so it is refused.
    ///
    /// The edits that do fit the call still land: refusing one property must not cost the rest.
    #[test]
    fn adding_a_capsule_to_a_grab_written_without_the_slots_is_refused() {
        // Verbatim from kirby/SpecialNStart.
        let text = r#"unsafe extern "C" fn game_catch(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::CATCH(agent, 0, Hash40::new("top"), 6.0, 0.0, 6.0, 5.0, *FIGHTER_STATUS_KIND_SWALLOWED, *COLLISION_SITUATION_MASK_GA);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 1, "{pristine:#?}");
        assert_eq!(pristine[0].capsule_end, None);

        let mut edited = pristine.clone();
        edited[0].capsule_end = Some([1.0, 2.0, 3.0]);
        edited[0].size = 7.25;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(
            after.contains("*FIGHTER_STATUS_KIND_SWALLOWED")
                && after.contains("*COLLISION_SITUATION_MASK_GA"),
            "the grab's own behaviour must survive a refused capsule edit\n{after}"
        );
        assert!(!after.contains("Some(1.0)"), "{after}");
        assert!(
            report.skipped.iter().any(|s| s.contains("capsule end")),
            "the refusal has to be visible, not silent: {report:?}"
        );
        // The size still lands.
        assert!(after.contains("Hash40::new(\"top\"), 7.25,"), "{after}");
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

    #[test]
    fn an_attack_fp_edit_uses_its_own_source_slots() {
        let text = r#"unsafe extern "C" fn game_fp(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        macros::ATTACK_FP(agent, 2, 3, Hash40::new("top"), 9.5, 45, 100, 2, 30, 4.0, 1.0, 2.0, 3.0, Hash40::new("collision_attr_fire"), 0.25, 0.5, 0.75, true, false, 0, 3, 4, 0, true, 7, 8, false, 9, false, true, false, false, 10, true, false, *ATTACK_LR_CHECK_POS, false, true, false, false, false, 12);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_hitboxes();
        assert_eq!(pristine.len(), 1, "{pristine:#?}");
        assert_eq!(pristine[0].category, crate::data::CAT_ATTACK_FP);
        let mut edited = pristine.clone();
        edited[0].damage = 14.25;
        edited[0].lr_check = "ATTACK_LR_CHECK_F".into();
        edited[0].collision_attr = "collision_attr_elec".into();
        edited[0].is_reflectable = true;

        let (after, report) = rewrite_hitboxes(text, "t", &pristine, &edited).unwrap();
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.changed, 4, "{report:?}");
        assert!(
            after.contains(
                "Hash40::new(\"top\"), 14.25, 45, 100, 2, 30, 4.0, 1.0, 2.0, 3.0, Hash40::new(\"collision_attr_elec\")"
            ),
            "{after}"
        );
        assert!(after.contains("*ATTACK_LR_CHECK_F, false, true"), "{after}");
        assert!(
            crate::acmd::parse_acmd_script(&after).to_hitboxes()[0].is_reflectable,
            "{after}"
        );
    }

    #[test]
    fn set_speed_ex_write_back_changes_only_velocity_arguments() {
        let text = r#"unsafe extern "C" fn game_speed(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::SET_SPEED_EX(agent, 0, -3.8, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_speed_ex_events();
        let mut edited = pristine.clone();
        edited[0].call.speed_x = 1.25;
        edited[0].call.speed_y = 2.0;
        let (after, report) = rewrite_speed_ex(text, "mario/speed", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "macros::SET_SPEED_EX(agent, 1.25, 2, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);"
        ));
        assert!(after.contains("frame(agent.lua_state_agent, 4.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_speed_ex_events(),
            edited
        );
    }

    #[test]
    fn set_speed_write_back_changes_only_direct_velocity_arguments() {
        let text = r#"unsafe extern "C" fn game_speed(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::SET_SPEED(agent, 0, -3.8);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_speed_events();
        let mut edited = pristine.clone();
        edited[0].call.speed_x = 1.25;
        edited[0].call.speed_y = 2.0;
        let (after, report) = rewrite_speed(text, "mario/speed", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("macros::SET_SPEED(agent, 1.25, 2);"));
        assert!(after.contains("frame(agent.lua_state_agent, 4.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_speed_events(),
            edited
        );
    }

    #[test]
    fn generated_set_speed_helper_write_back_round_trips_without_touching_definition() {
        let source = r#"unsafe extern "C" fn game_speed(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::SET_SPEED(agent, 0, -3.8);
    }
}
"#;
        let script = crate::acmd::parse_acmd_script(source);
        let generated = crate::acmd::preview_game_fn(&script, "speed");
        let pristine = crate::acmd::parse_acmd_script(&generated).to_speed_events();
        assert_eq!(speed_sites(&generated).len(), 1, "{generated}");
        assert!(
            generated.contains("unsafe fn visionary_set_speed(")
                && generated.contains("visionary_set_speed(agent, 0.0, -3.8);"),
            "{generated}"
        );

        let mut edited = pristine.clone();
        edited[0].call.speed_x = 1.25;
        edited[0].call.speed_y = 2.0;
        let (after, report) = rewrite_speed(&generated, "mario/speed", &pristine, &edited).unwrap();

        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "unsafe fn visionary_set_speed(agent: &mut L2CAgentBase, speed_x: f32, speed_y: f32)"
        ));
        assert!(after.contains("visionary_set_speed(agent, 1.25, 2);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_speed_events(),
            edited
        );
    }

    #[test]
    fn malformed_set_speed_shapes_block_positional_source_sync() {
        let text = r#"unsafe extern "C" fn game_speed(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::SET_SPEED(agent, 0);
        macros::SET_SPEED(agent, 0, 1, 2);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_speed_events();
        assert!(pristine.is_empty());
        let (after, report) = rewrite_speed(text, "mario/speed", &pristine, &pristine).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.is_empty());
        assert_eq!(speed_sites(text).len(), 0);
    }

    #[test]
    fn speed_addition_and_correction_write_back_change_only_their_values() {
        let text = r#"unsafe extern "C" fn game_speed(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::ADD_SPEED_NO_LIMIT(agent, 0, -3.8);
        macros::CORRECT(agent, *GROUND_CORRECT_KIND_GROUND);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text);
        let speed_before = pristine.to_add_speed_no_limit_events();
        let correct_before = pristine.to_correct_events();
        let mut speed_after = speed_before.clone();
        speed_after[0].call.speed_x = 1.25;
        speed_after[0].call.speed_y = 2.0;
        let mut correct_after = correct_before.clone();
        correct_after[0].call.kind = "2".into();

        let (after_speed, speed_report) =
            rewrite_add_speed_no_limit(text, "mario/speed", &speed_before, &speed_after).unwrap();
        assert_eq!(speed_report.changed, 2, "{speed_report:?}");
        assert!(speed_report.skipped.is_empty(), "{speed_report:?}");
        let (after, correct_report) =
            rewrite_correct(&after_speed, "mario/speed", &correct_before, &correct_after).unwrap();
        assert_eq!(correct_report.changed, 1, "{correct_report:?}");
        assert!(correct_report.skipped.is_empty(), "{correct_report:?}");
        assert!(after.contains("macros::ADD_SPEED_NO_LIMIT(agent, 1.25, 2);"));
        assert!(after.contains("macros::CORRECT(agent, 2);"));
        assert!(after.contains("frame(agent.lua_state_agent, 4.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_add_speed_no_limit_events(),
            speed_after
        );
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_correct_events(),
            correct_after
        );
    }

    #[test]
    fn ft_catch_stop_write_back_changes_only_numeric_values() {
        let text = r#"unsafe extern "C" fn game_throwf(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        macros::FT_CATCH_STOP(agent, 6, 1);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_ft_catch_stop_events();
        let mut edited = pristine.clone();
        edited[0].call.arg1 = 7.5;
        edited[0].call.arg2 = 0.25;

        let (after, report) =
            rewrite_ft_catch_stop(text, "mario/throw_f", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("macros::FT_CATCH_STOP(agent, 7.5, 0.25);"));
        assert!(after.contains("frame(agent.lua_state_agent, 6.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_ft_catch_stop_events(),
            edited
        );
    }

    #[test]
    fn malformed_ft_catch_stop_shapes_block_positional_source_sync() {
        let text = r#"unsafe extern "C" fn game_throwf(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::FT_CATCH_STOP(agent, 6);
        macros::FT_CATCH_STOP(agent, 6, 1, 2);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_ft_catch_stop_events();
        assert!(pristine.is_empty());
        assert!(ft_catch_stop_sites(text).is_empty());
        let (after, report) = rewrite_ft_catch_stop(text, "mario/throw_f", &[], &[]).unwrap();
        assert_eq!(after, text);
        assert!(
            report.skipped.is_empty(),
            "unchanged opaque calls need no warning"
        );
    }

    #[test]
    fn ft_start_adjust_motion_frame_write_back_changes_only_numeric_value() {
        let text = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 17.0);
    macros::FT_START_ADJUST_MOTION_FRAME_arg1(agent, 0.85);
}
"#;
        let pristine =
            crate::acmd::parse_acmd_script(text).to_ft_start_adjust_motion_frame_events();
        let mut edited = pristine.clone();
        edited[0].call.value = 1.25;

        let (after, report) = rewrite_ft_start_adjust_motion_frame(
            text,
            "bayonetta/attack_air_n",
            &pristine,
            &edited,
        )
        .unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("macros::FT_START_ADJUST_MOTION_FRAME_arg1(agent, 1.25);"));
        assert!(after.contains("frame(agent.lua_state_agent, 17.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_ft_start_adjust_motion_frame_events(),
            edited
        );
    }

    #[test]
    fn revised_ft_start_adjust_motion_frame_shapes_block_positional_source_sync() {
        let text = r#"unsafe extern "C" fn game_specialn(agent: &mut L2CAgentBase) {
    FT_START_ADJUST_MOTION_FRAME_REVISED_arg1(1.0);
}
"#;
        let pristine =
            crate::acmd::parse_acmd_script(text).to_ft_start_adjust_motion_frame_events();
        assert!(pristine.is_empty());
        assert!(ft_start_adjust_motion_frame_sites(text).is_empty());
        let (after, report) =
            rewrite_ft_start_adjust_motion_frame(text, "trail/special_n", &[], &[]).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.is_empty(), "{report:?}");
    }

    #[test]
    fn clr_speed_source_sync_rewrites_only_the_authored_kinetic_token() {
        let text = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::CLR_SPEED(agent, *FIGHTER_KINETIC_ENERGY_ID_GRAVITY);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_clr_speed_events();
        let mut edited = pristine.clone();
        edited[0].call.kinetic_kind = "*FIGHTER_KINETIC_ENERGY_ID_MOTION".into();
        let (after, report) =
            rewrite_clr_speed(text, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("macros::CLR_SPEED(agent, *FIGHTER_KINETIC_ENERGY_ID_MOTION);"));
        assert!(after.contains("frame(agent.lua_state_agent, 4.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_clr_speed_events(),
            edited
        );
    }

    #[test]
    fn change_kinetic_source_sync_rewrites_only_the_authored_type_token() {
        let text = r#"unsafe extern "C" fn game_escapeair(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 19.0);
    if macros::is_excute(agent) {
        KineticModule::change_kinetic(agent.module_accessor, *FIGHTER_KINETIC_TYPE_FALL);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_change_kinetic_events();
        let mut edited = pristine.clone();
        edited[0].call.kinetic_type = "*FIGHTER_KINETIC_TYPE_GROUND".into();
        let (after, report) =
            rewrite_change_kinetic(text, "kirby/escape_air", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "KineticModule::change_kinetic(agent.module_accessor, *FIGHTER_KINETIC_TYPE_GROUND);"
        ));
        assert!(after.contains("frame(agent.lua_state_agent, 19.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_change_kinetic_events(),
            edited
        );
    }

    #[test]
    fn change_kinetic_source_sync_rewrites_the_measured_boma_shape() {
        let text = r#"unsafe extern "C" fn game_passiveceil(agent: &mut L2CAgentBase) {
    let boma = agent.boma();
    frame(agent.lua_state_agent, 18.0);
    if is_excute(agent) {
        KineticModule::change_kinetic(boma, *FIGHTER_KINETIC_TYPE_PASSIVE_CEIL);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_change_kinetic_events();
        let mut edited = pristine.clone();
        edited[0].call.kinetic_type = "*FIGHTER_KINETIC_TYPE_FALL".into();
        let (after, report) =
            rewrite_change_kinetic(text, "bayonetta/passive_ceil", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("KineticModule::change_kinetic(boma, *FIGHTER_KINETIC_TYPE_FALL);"));
        assert!(after.contains("let boma = agent.boma();"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_change_kinetic_events(),
            edited
        );
    }

    #[test]
    fn kinetic_add_speed_source_sync_rewrites_only_the_vector_and_keeps_receiver() {
        let text = r#"unsafe extern "C" fn game_escapeair(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 1.0);
    if macros::is_excute(agent) {
        KineticModule::add_speed(agent.module_accessor, &Vector3f{x: 0.72, y: -1.5, z: 0.0});
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_kinetic_add_speed_events();
        let mut edited = pristine.clone();
        edited[0].call.speed_x = 1.25;
        edited[0].call.speed_y = 2.0;
        let (after, report) =
            rewrite_kinetic_add_speed(text, "mario/escape_air", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "KineticModule::add_speed(agent.module_accessor, &Vector3f{x: 1.25, y: 2.0, z: 0.0});"
        ));
        assert!(after.contains("frame(agent.lua_state_agent, 1.0);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_kinetic_add_speed_events(),
            edited
        );
    }

    #[test]
    fn kinetic_add_speed_source_sync_rewrites_the_measured_boma_shape() {
        let text = r#"unsafe extern "C" fn game_escapeair(agent: &mut L2CAgentBase) {
    let boma = agent.boma();
    frame(agent.lua_state_agent, 1.0);
    if is_excute(agent) {
        KineticModule::add_speed(boma, &Vector3f{x: 0.0, y: 0.8, z: 0.0});
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_kinetic_add_speed_events();
        let mut edited = pristine.clone();
        edited[0].call.speed_y = 1.33;
        let (after, report) =
            rewrite_kinetic_add_speed(text, "mario/escape_air", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains("KineticModule::add_speed(boma, &Vector3f{x: 0.0, y: 1.33, z: 0.0});")
        );
        assert!(after.contains("let boma = agent.boma();"));
    }

    #[test]
    fn malformed_kinetic_add_speed_vectors_are_source_only() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        KineticModule::add_speed(agent.module_accessor, &Vector3f{x: 1.0, y: 2.0, z: 1.0});
    }
}
"#;
        assert!(crate::acmd::parse_acmd_script(text)
            .to_kinetic_add_speed_events()
            .is_empty());
        assert!(kinetic_add_speed_sites(text).is_empty());
        let (after, report) = rewrite_kinetic_add_speed(text, "mario/x", &[], &[]).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.is_empty(), "{report:?}");
    }

    #[test]
    fn work_flag_source_sync_rewrites_only_authored_tokens_and_keeps_receivers() {
        let text = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    let boma = agent.boma();
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        WorkModule::on_flag(agent.module_accessor, *FIGHTER_STATUS_ATTACK_FLAG_START_SMASH_HOLD);
        WorkModule::off_flag(boma, 17);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_work_flag_events();
        let mut edited = pristine.clone();
        edited[0].call.flag = "23".into();
        edited[1].call.flag = "*FIGHTER_STATUS_ATTACK_FLAG_ENABLE_COMBO".into();

        let (after, report) =
            rewrite_work_flags(text, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("WorkModule::on_flag(agent.module_accessor, 23);"));
        assert!(
            after.contains("WorkModule::off_flag(boma, *FIGHTER_STATUS_ATTACK_FLAG_ENABLE_COMBO);")
        );
        assert!(after.contains("let boma = agent.boma();"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_work_flag_events(),
            edited
        );
    }

    #[test]
    fn work_flag_source_sync_refuses_retiming_operation_changes_and_malformed_tokens() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        WorkModule::on_flag(agent.module_accessor, 7);
        WorkModule::off_flag(agent.module_accessor, 8);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_work_flag_events();
        let mut edited = pristine.clone();
        edited[0].frame = 2;
        edited[1].call.action = crate::data::WorkFlagAction::On;
        edited[1].call.flag = "make_flag()".into();
        let (after, report) = rewrite_work_flags(text, "mario/x", &pristine, &edited).unwrap();
        assert_eq!(after, text);
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(report.skipped.len(), 2, "{report:?}");
    }

    #[test]
    fn work_transition_term_source_sync_rewrites_only_authored_tokens_and_keeps_receivers() {
        let text = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    let boma = agent.boma();
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        WorkModule::enable_transition_term(agent.module_accessor, *FIGHTER_STATUS_TRANSITION_TERM_ID_DASH_TO_RUN);
        WorkModule::unable_transition_term(boma, 17);
        WorkModule::enable_transition_term_group(agent.module_accessor, *FIGHTER_STATUS_TRANSITION_GROUP_CHK_AIR_LANDING);
        WorkModule::unable_transition_term_group_ex(agent.module_accessor, 18);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_work_transition_term_events();
        let mut edited = pristine.clone();
        edited[0].call.transition_term = "23".into();
        edited[1].call.transition_term = "*FIGHTER_STATUS_TRANSITION_TERM_ID_ENABLE_COMBO".into();
        edited[2].call.transition_term = "19".into();
        edited[3].call.transition_term =
            "*FIGHTER_STATUS_TRANSITION_TERM_ID_CONT_SPECIAL_HI".into();

        let (after, report) =
            rewrite_work_transition_terms(text, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 4, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("WorkModule::enable_transition_term(agent.module_accessor, 23);"));
        assert!(after.contains(
            "WorkModule::unable_transition_term(boma, *FIGHTER_STATUS_TRANSITION_TERM_ID_ENABLE_COMBO);"
        ));
        assert!(
            after.contains("WorkModule::enable_transition_term_group(agent.module_accessor, 19);")
        );
        assert!(after.contains(
            "WorkModule::unable_transition_term_group_ex(agent.module_accessor, *FIGHTER_STATUS_TRANSITION_TERM_ID_CONT_SPECIAL_HI);"
        ));
        assert!(after.contains("let boma = agent.boma();"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_work_transition_term_events(),
            edited
        );
    }

    #[test]
    fn work_transition_term_source_sync_refuses_retiming_operation_changes_and_malformed_tokens() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        WorkModule::enable_transition_term(agent.module_accessor, 7);
        WorkModule::unable_transition_term(agent.module_accessor, 8);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_work_transition_term_events();
        let mut edited = pristine.clone();
        edited[0].frame = 2;
        edited[1].call.action = crate::data::WorkTransitionTermAction::Enable;
        edited[1].call.transition_term = "make_term()".into();
        let (after, report) =
            rewrite_work_transition_terms(text, "mario/x", &pristine, &edited).unwrap();
        assert_eq!(after, text);
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(report.skipped.len(), 2, "{report:?}");
    }

    #[test]
    fn work_module_set_source_sync_rewrites_value_and_slot_tokens() {
        let text = r#"unsafe extern "C" fn game_itemgrasspull(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        WorkModule::set_int(agent.module_accessor, *FIGHTER_ITEM_GRASS_PULL_STEP_PICKUP, *FIGHTER_STATUS_ITEM_GRASS_PULL_WORK_INT_NEXT_STEP);
    }
    frame(agent.lua_state_agent, 47.0);
    if macros::is_excute(agent) {
        WorkModule::set_float(agent.module_accessor, 5.0, *FIGHTER_INSTANCE_WORK_ID_FLOAT_FINISH_CAMERA_THROW_RAY_LENGTH);
    }
    frame(agent.lua_state_agent, 70.0);
    if macros::is_excute(agent) {
        WorkModule::set_int64(boma, hash40("fall_damage") as i64, FIGHTER_STATUS_FINAL_WORK_INT_REQUEST_LOOP_DAMAGE_MOTION);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_work_module_set_events();
        let mut edited = pristine.clone();
        edited[0].call.value = "4".into();
        edited[0].call.slot = "9".into();
        edited[1].call.value = "6.25".into();
        edited[1].call.slot = "12".into();
        edited[2].call.value = "1099511627777".into();
        edited[2].call.slot = "14".into();

        let (after, report) =
            rewrite_work_module_sets(text, "kirby/item_grass_pull", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 6, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("WorkModule::set_int(agent.module_accessor, 4, 9);"));
        assert!(after.contains("WorkModule::set_float(agent.module_accessor, 6.25, 12);"));
        assert!(after.contains("WorkModule::set_int64(boma, 1099511627777, 14);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_work_module_set_events(),
            edited
        );
    }

    #[test]
    fn work_module_set_source_sync_refuses_retiming_kind_and_malformed_edits() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        WorkModule::set_int(agent.module_accessor, 1, 7);
        WorkModule::set_float(agent.module_accessor, 2.0, 8);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_work_module_set_events();
        let mut edited = pristine.clone();
        edited[0].frame = 2;
        edited[1].call.kind = crate::data::WorkModuleSetKind::Int;
        edited[1].call.value = "make_value()".into();
        let (after, report) =
            rewrite_work_module_sets(text, "mario/x", &pristine, &edited).unwrap();
        assert_eq!(after, text);
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(report.skipped.len(), 2, "{report:?}");
    }

    #[test]
    fn work_module_inc_int_source_sync_rewrites_authored_slots_for_standard_and_hdr() {
        let standard = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 5.0);
    if macros::is_excute(agent) {
        WorkModule::inc_int(agent.module_accessor, *FIGHTER_STATUS_WORK_INT_NEXT_STEP);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(standard).to_work_module_inc_int_events();
        let mut edited = pristine.clone();
        edited[0].call.slot = "9".into();
        let (after, report) =
            rewrite_work_module_inc_int(standard, "mario/x", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("WorkModule::inc_int(agent.module_accessor, 9);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_work_module_inc_int_events(),
            edited
        );

        let hdr = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    let boma = agent.boma();
    frame(agent.lua_state_agent, 5.0);
    if is_excute(agent) {
        WorkModule::inc_int(boma, *FIGHTER_STATUS_WORK_INT_NEXT_STEP);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(hdr).to_work_module_inc_int_events();
        let mut edited = pristine.clone();
        edited[0].call.slot = "*FIGHTER_STATUS_WORK_INT_OTHER".into();
        let (after, report) =
            rewrite_work_module_inc_int(hdr, "mario/x_hdr", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("WorkModule::inc_int(boma, *FIGHTER_STATUS_WORK_INT_OTHER);"));
        assert!(after.contains("let boma = agent.boma();"));
    }

    #[test]
    fn work_module_inc_int_source_sync_refuses_retiming_and_malformed_edits() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        WorkModule::inc_int(agent.module_accessor, 7);
        WorkModule::inc_int(agent.module_accessor, 8);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_work_module_inc_int_events();
        let mut edited = pristine.clone();
        edited[0].frame = 2;
        edited[1].call.slot = "make_slot()".into();
        let (after, report) =
            rewrite_work_module_inc_int(text, "mario/x", &pristine, &edited).unwrap();
        assert_eq!(after, text);
        assert_eq!(report.changed, 0, "{report:?}");
        assert_eq!(report.skipped.len(), 2, "{report:?}");
    }

    #[test]
    fn kinetic_clear_speed_all_source_sync_retimes_flat_standard_and_hdr_calls() {
        let standard = r#"unsafe extern "C" fn game_escapeair(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        KineticModule::clear_speed_all(agent.module_accessor);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(standard).to_kinetic_clear_speed_all_events();
        let mut edited = pristine.clone();
        edited[0].frame = 6;
        let (after, report) =
            rewrite_kinetic_clear_speed_all(standard, "mario/escape_air", &pristine, &edited)
                .unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("frame(agent.lua_state_agent, 6.0);"));
        assert!(after.contains("KineticModule::clear_speed_all(agent.module_accessor);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_kinetic_clear_speed_all_events(),
            edited
        );

        let hdr = r#"unsafe extern "C" fn game_escapeair(agent: &mut L2CAgentBase) {
    let boma = agent.boma();
    frame(agent.lua_state_agent, 4.0);
    if is_excute(agent) {
        KineticModule::clear_speed_all(boma);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(hdr).to_kinetic_clear_speed_all_events();
        let mut edited = pristine.clone();
        edited[0].frame = 9;
        let (after, report) =
            rewrite_kinetic_clear_speed_all(hdr, "mario/escape_air_hdr", &pristine, &edited)
                .unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(after.contains("let boma = agent.boma();"));
        assert!(after.contains("KineticModule::clear_speed_all(boma);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_kinetic_clear_speed_all_events(),
            edited
        );
    }

    #[test]
    fn malformed_kinetic_clear_speed_all_shapes_are_source_only() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        KineticModule::clear_speed_all(agent.module_accessor, 1);
        KineticModule::clear_speed_all(other.module_accessor);
    }
}
"#;
        assert!(crate::acmd::parse_acmd_script(text)
            .to_kinetic_clear_speed_all_events()
            .is_empty());
        assert!(kinetic_clear_speed_all_sites(text).is_empty());
        let (after, report) = rewrite_kinetic_clear_speed_all(text, "mario/x", &[], &[]).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.is_empty(), "{report:?}");
    }

    #[test]
    fn kinetic_set_consider_ground_friction_source_sync_retimes_standard_and_hdr_points() {
        let standard = r#"unsafe extern "C" fn game_escapeair(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        KineticModule::set_consider_ground_friction(agent.module_accessor, false, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(standard)
            .to_kinetic_set_consider_ground_friction_events();
        let mut edited = pristine.clone();
        edited[0].frame = 6;
        edited[0].call.consider_ground_friction = true;
        edited[0].call.kinetic_energy_attribute = "7".into();
        let (after, report) = rewrite_kinetic_set_consider_ground_friction(
            standard,
            "mario/escape_air",
            &pristine,
            &edited,
        )
        .unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("frame(agent.lua_state_agent, 6.0);"));
        assert!(after.contains(
            "KineticModule::set_consider_ground_friction(agent.module_accessor, true, 7);"
        ));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_kinetic_set_consider_ground_friction_events(),
            edited
        );

        let hdr = standard.replace("agent.module_accessor", "boma");
        let pristine =
            crate::acmd::parse_acmd_script(&hdr).to_kinetic_set_consider_ground_friction_events();
        let mut edited = pristine.clone();
        edited[0].call.consider_ground_friction = true;
        let (after, report) = rewrite_kinetic_set_consider_ground_friction(
            &hdr,
            "mario/escape_air_hdr",
            &pristine,
            &edited,
        )
        .unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(after.contains("KineticModule::set_consider_ground_friction(boma, true"));
        assert!(after.contains("let boma = agent.boma();") || !after.contains("agent.boma"));

        let mixed = r#"unsafe extern "C" fn game_escapeair(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        KineticModule::set_consider_ground_friction(agent.module_accessor, false, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);
    }
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        KineticModule::set_consider_ground_friction(agent.module_accessor, true, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);
    }
}
"#;
        let pristine =
            crate::acmd::parse_acmd_script(mixed).to_kinetic_set_consider_ground_friction_events();
        let mut edited = vec![pristine[0].clone(), pristine[1].clone()];
        edited[1].call.kinetic_energy_attribute = "9".into();
        edited.insert(
            1,
            crate::data::KineticSetConsiderGroundFrictionEvent {
                frame: 6,
                call: crate::data::KineticSetConsiderGroundFrictionCall {
                    consider_ground_friction: false,
                    kinetic_energy_attribute: "5".into(),
                },
                site: 1,
            },
        );
        edited[2].site = 2;
        let (after, report) = rewrite_kinetic_set_consider_ground_friction(
            mixed,
            "mario/escape_air",
            &pristine,
            &edited,
        )
        .unwrap();
        assert_eq!(report.changed, 2, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "KineticModule::set_consider_ground_friction(agent.module_accessor, false, 5);"
        ));
        assert!(after.contains(
            "KineticModule::set_consider_ground_friction(agent.module_accessor, true, 9);"
        ));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_kinetic_set_consider_ground_friction_events(),
            edited
        );
    }

    #[test]
    fn malformed_kinetic_set_consider_ground_friction_source_shapes_are_source_only() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        KineticModule::set_consider_ground_friction(agent.module_accessor, 1, 0);
        KineticModule::set_consider_ground_friction(other, false, 0);
    }
}
"#;
        assert!(crate::acmd::parse_acmd_script(text)
            .to_kinetic_set_consider_ground_friction_events()
            .is_empty());
        assert!(kinetic_set_consider_ground_friction_sites(text).is_empty());
        let (after, report) =
            rewrite_kinetic_set_consider_ground_friction(text, "mario/x", &[], &[]).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.is_empty(), "{report:?}");
    }

    #[test]
    fn kinetic_energy_source_sync_rewrites_ids_and_keeps_operation_and_receiver() {
        let text = r#"unsafe extern "C" fn game_attackairlw(agent: &mut L2CAgentBase) {
    let boma = agent.boma();
    frame(agent.lua_state_agent, 1.0);
    if is_excute(agent) {
        KineticModule::suspend_energy(boma, *FIGHTER_KINETIC_ENERGY_ID_CONTROL);
        KineticModule::resume_energy(boma, *FIGHTER_KINETIC_ENERGY_ID_CONTROL);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_kinetic_energy_events();
        let mut edited = pristine.clone();
        edited[0].call.kinetic_energy_id = "*FIGHTER_KINETIC_ENERGY_ID_GRAVITY".into();
        let (after, report) =
            rewrite_kinetic_energy(text, "mario/attack_air_lw", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after
            .contains("KineticModule::suspend_energy(boma, *FIGHTER_KINETIC_ENERGY_ID_GRAVITY);"));
        assert!(after
            .contains("KineticModule::resume_energy(boma, *FIGHTER_KINETIC_ENERGY_ID_CONTROL);"));
        assert!(after.contains("let boma = agent.boma();"));
    }

    #[test]
    fn malformed_kinetic_energy_source_shapes_are_source_only() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        KineticModule::suspend_energy(agent.module_accessor);
    }
}
"#;
        assert!(crate::acmd::parse_acmd_script(text)
            .to_kinetic_energy_events()
            .is_empty());
        assert!(kinetic_energy_sites(text).is_empty());
        let (after, report) = rewrite_kinetic_energy(text, "mario/x", &[], &[]).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.is_empty(), "{report:?}");
    }

    #[test]
    fn kinetic_energy_source_sync_accepts_enable_and_unable_operations() {
        let text = r#"unsafe extern "C" fn game_specialhi(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        KineticModule::enable_energy(agent.module_accessor, *FIGHTER_KINETIC_ENERGY_ID_GRAVITY);
        KineticModule::unable_energy(agent.module_accessor, *FIGHTER_KINETIC_ENERGY_ID_CONTROL);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_kinetic_energy_events();
        let mut edited = pristine.clone();
        edited[1].call.kinetic_energy_id = "*FIGHTER_KINETIC_ENERGY_ID_GRAVITY".into();
        let (after, report) =
            rewrite_kinetic_energy(text, "mario/special_hi", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "KineticModule::enable_energy(agent.module_accessor, *FIGHTER_KINETIC_ENERGY_ID_GRAVITY);"
        ));
        assert!(after.contains(
            "KineticModule::unable_energy(agent.module_accessor, *FIGHTER_KINETIC_ENERGY_ID_GRAVITY);"
        ));
    }

    #[test]
    fn set_air_source_sync_handles_flat_remove_retime_and_insert() {
        let text = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 4.0);
    if macros::is_excute(agent) {
        macros::SET_AIR(agent);
    }
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        macros::SET_AIR(agent);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_set_air_events();
        let edited = vec![crate::data::SetAirEvent { frame: 6, site: 0 }];
        let (after, report) =
            rewrite_set_air(text, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 3, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        let round_trip = crate::acmd::parse_acmd_script(&after).to_set_air_events();
        assert_eq!(round_trip, edited, "{after}");
        assert!(
            after.contains("frame(agent.lua_state_agent, 6.0);"),
            "{after}"
        );
    }

    #[test]
    fn set_air_source_sync_refuses_branch_restructuring() {
        let text = r#"unsafe extern "C" fn game_specialn(agent: &mut L2CAgentBase) {
    if WorkModule::is_flag(agent.module_accessor, 1) {
        if macros::is_excute(agent) {
            macros::SET_AIR(agent);
        }
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_set_air_events();
        let edited = Vec::new();
        let (after, report) = rewrite_set_air(text, "mario/special_n", &pristine, &edited).unwrap();
        assert_eq!(after, text);
        assert!(report
            .skipped
            .iter()
            .any(|note| note.contains("loop/branch")));
    }

    #[test]
    fn malformed_set_speed_ex_shapes_block_positional_source_sync() {
        let text = r#"unsafe extern "C" fn game_speed(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::SET_SPEED_EX(agent, 0, 1);
        macros::SET_SPEED_EX(agent, 0, 1, 0, *KINETIC_ENERGY_RESERVE_ATTRIBUTE_MAIN);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_speed_ex_events();
        assert!(pristine.is_empty());
        let (after, report) = rewrite_speed_ex(text, "mario/speed", &pristine, &pristine).unwrap();
        assert_eq!(after, text);
        assert!(
            report.skipped.is_empty(),
            "unchanged opaque calls need no warning"
        );
        assert_eq!(speed_ex_sites(text).len(), 0);
    }

    #[test]
    fn malformed_speed_addition_and_correction_shapes_block_positional_sync() {
        let text = r#"unsafe extern "C" fn game_speed(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        macros::ADD_SPEED_NO_LIMIT(agent, 0);
        macros::ADD_SPEED_NO_LIMIT(agent, 0, 1, 2);
        macros::CORRECT(agent);
        macros::CORRECT(agent, 1, 2);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text);
        assert!(pristine.to_add_speed_no_limit_events().is_empty());
        assert!(pristine.to_correct_events().is_empty());
        assert_eq!(add_speed_no_limit_sites(text).len(), 0);
        assert_eq!(correct_sites(text).len(), 0);
    }

    #[test]
    fn motion_module_set_rate_source_sync_changes_only_numeric_values() {
        let text = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        MotionModule::set_rate(agent.module_accessor, 0.8);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_motion_module_set_rate_events();
        let mut edited = pristine.clone();
        edited[0].call.rate = 1.25;
        let (after, report) =
            rewrite_motion_module_set_rate(text, "mario/attack_air_n", &pristine, &edited).unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains("MotionModule::set_rate(agent.module_accessor, 1.25);"));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_motion_module_set_rate_events(),
            edited
        );
    }

    #[test]
    fn motion_module_set_rate_source_sync_refuses_retime_and_malformed_sites() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        MotionModule::set_rate(agent.module_accessor, 0.8);
        MotionModule::set_rate(agent.module_accessor, -378992935);
    }
}
"#;
        let pristine = crate::acmd::parse_acmd_script(text).to_motion_module_set_rate_events();
        assert_eq!(pristine.len(), 1);
        let mut edited = pristine.clone();
        edited[0].frame = 4;
        let (after, report) =
            rewrite_motion_module_set_rate(text, "mario/x", &pristine, &edited).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.iter().any(|note| note.contains("retimed")));
        assert_eq!(motion_module_set_rate_sites(text).len(), 1);
    }

    #[test]
    fn motion_module_set_rate_partial_source_sync_changes_only_numeric_rate() {
        let text = r#"unsafe extern "C" fn game_attackairn(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 6.0);
    if macros::is_excute(agent) {
        MotionModule::set_rate_partial(agent.module_accessor, *FIGHTER_MOTION_PART_SET_KIND_UPPER_BODY, 0.8);
    }
}
"#;
        let pristine =
            crate::acmd::parse_acmd_script(text).to_motion_module_set_rate_partial_events();
        let mut edited = pristine.clone();
        edited[0].call.rate = 1.25;
        let (after, report) =
            rewrite_motion_module_set_rate_partial(text, "mario/attack_air_n", &pristine, &edited)
                .unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "MotionModule::set_rate_partial(agent.module_accessor, *FIGHTER_MOTION_PART_SET_KIND_UPPER_BODY, 1.25);"
        ));
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_motion_module_set_rate_partial_events(),
            edited
        );
    }

    #[test]
    fn expression_motion_module_set_rate_partial_source_sync_uses_expression_category() {
        let text = r#"unsafe extern "C" fn expression_appeallwl(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 8.0);
    if macros::is_excute(agent) {
        MotionModule::set_rate_partial(agent.module_accessor, *FIGHTER_PACMAN_MOTION_PART_SET_KIND_MATERIAL, 0.5);
    }
}
"#;
        let script = crate::acmd::parse_expression_script(text);
        let pristine = script.to_motion_module_set_rate_partial_events();
        let mut edited = pristine.clone();
        edited[0].call.rate = 1.25;
        let (after, report) =
            rewrite_motion_module_set_rate_partial(text, "pacman/appeal_lw_l", &pristine, &edited)
                .unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(after.contains(
            "MotionModule::set_rate_partial(agent.module_accessor, *FIGHTER_PACMAN_MOTION_PART_SET_KIND_MATERIAL, 1.25);"
        ));
        assert_eq!(
            crate::acmd::parse_expression_script(&after).to_motion_module_set_rate_partial_events(),
            edited
        );
    }

    #[test]
    fn motion_module_set_rate_partial_source_sync_refuses_structure_part_changes_and_malformed_sites(
    ) {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        MotionModule::set_rate_partial(boma, *FIGHTER_MOTION_PART_SET_KIND_UPPER_BODY, 0.8);
        MotionModule::set_rate_partial(boma, *FIGHTER_MOTION_PART_SET_KIND_UPPER_BODY);
    }
}
"#;
        let pristine =
            crate::acmd::parse_acmd_script(text).to_motion_module_set_rate_partial_events();
        assert_eq!(pristine.len(), 1);
        let mut edited = pristine.clone();
        edited[0].call.part_kind = "*FIGHTER_PACMAN_MOTION_PART_SET_KIND_MATERIAL".into();
        let (after, report) =
            rewrite_motion_module_set_rate_partial(text, "mario/x", &pristine, &edited).unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.iter().any(|note| note.contains("part kind")));
        assert_eq!(motion_module_set_rate_partial_sites(text).len(), 1);
    }

    #[test]
    fn motion_module_set_helper_calculation_source_sync_changes_only_boolean_values() {
        let text = r#"unsafe extern "C" fn game_specialairhi(agent: &mut L2CAgentBase) {
    frame(agent.lua_state_agent, 3.0);
    if macros::is_excute(agent) {
        MotionModule::set_helper_calculation(agent.module_accessor, false);
    }
}
"#;
        let pristine =
            crate::acmd::parse_acmd_script(text).to_motion_module_set_helper_calculation_events();
        let mut edited = pristine.clone();
        edited[0].call.enabled = true;
        let (after, report) = rewrite_motion_module_set_helper_calculation(
            text,
            "pichu/special_air_hi",
            &pristine,
            &edited,
        )
        .unwrap();
        assert_eq!(report.changed, 1, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        assert!(
            after.contains("MotionModule::set_helper_calculation(agent.module_accessor, true);")
        );
        assert_eq!(
            crate::acmd::parse_acmd_script(&after).to_motion_module_set_helper_calculation_events(),
            edited
        );
    }

    #[test]
    fn motion_module_set_helper_calculation_source_sync_refuses_retime_and_malformed_sites() {
        let text = r#"unsafe extern "C" fn game_x(agent: &mut L2CAgentBase) {
    if macros::is_excute(agent) {
        MotionModule::set_helper_calculation(boma, false);
        MotionModule::set_helper_calculation(boma, enabled);
    }
}
"#;
        let pristine =
            crate::acmd::parse_acmd_script(text).to_motion_module_set_helper_calculation_events();
        assert_eq!(pristine.len(), 1);
        let mut edited = pristine.clone();
        edited[0].frame = 4;
        let (after, report) =
            rewrite_motion_module_set_helper_calculation(text, "pichu/x", &pristine, &edited)
                .unwrap();
        assert_eq!(after, text);
        assert!(report.skipped.iter().any(|note| note.contains("retimed")));
        assert_eq!(motion_module_set_helper_calculation_sites(text).len(), 1);
    }
}
