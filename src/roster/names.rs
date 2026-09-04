//! Character display names.
//!
//! Names live in the game's compiled `msg_name.msbt`. Visionary does not parse or rewrite that
//! binary; it writes an **`.xmsbt` override** — a UTF-16 XML file that ARCropolis merges over
//! the compiled message table at load. That is the established route for name mods, and it is
//! also the honest one here: an override names exactly the entries it changes and leaves the
//! several thousand it does not alone, where rewriting the binary would mean reproducing every
//! untouched string from a format this tool cannot read back.
//!
//! The consequence is stated rather than hidden: **existing names cannot be read.** The editor
//! shows the fighter's directory-derived display name until the user sets one, and the panel
//! says so. Inventing a "current name" that came from nowhere would be worse than admitting
//! the gap.
//!
//! Format confirmed against real name-mod templates: UTF-16 little-endian with a BOM, and
//! entries labelled `nam_chr{0,1,2}_<slot>_<name_id>`. `chr0` and `chr1` carry the name in
//! mixed case and `chr2` carries it uppercase; all three are written together because a mod
//! that sets one and not the others shows two different names in different menus.

use std::path::PathBuf;

/// Where the override goes inside a mod folder.
pub const XMSBT_PATH: &str = "ui/message/msg_name.xmsbt";

/// One character's name, for one costume slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameOverride {
    /// The `ui_chara_db` `name_id` this name belongs to.
    pub name_id: String,
    /// Costume slot. Names are per-slot in this table, which is what lets a slot-backed
    /// character carry its own name while the donor keeps its.
    pub slot: u8,
    pub display: String,
}

impl NameOverride {
    /// The message labels this override writes, paired with the text each gets.
    ///
    /// `chr2` is uppercased because that is how the vanilla table stores it; leaving it in
    /// mixed case produces a character whose name changes case between menus.
    pub fn labels(&self) -> Vec<(String, String)> {
        let name_id = self.name_id.to_ascii_lowercase();
        let slot = format!("{:02}", self.slot);
        vec![
            (format!("nam_chr0_{slot}_{name_id}"), self.display.clone()),
            (format!("nam_chr1_{slot}_{name_id}"), self.display.clone()),
            (
                format!("nam_chr2_{slot}_{name_id}"),
                self.display.to_uppercase(),
            ),
        ]
    }
}

/// One character's per-label name overrides.
///
/// When the user edits "all the names", they may want `chr0`, `chr1`, and `chr2`
/// to differ (e.g. a short name for the stock icon vs a long one for the CSS
/// banner). This carries the three optionally, and falls back to `display` for
/// any `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailedNameOverride {
    pub name_id: String,
    pub slot: u8,
    pub chr0: Option<String>,
    pub chr1: Option<String>,
    pub chr2: Option<String>,
    /// Fallback display name when a variant is `None`. If `None`, that label is
    /// not emitted at all (the entry is left to the base file).
    pub fallback: Option<String>,
}

impl DetailedNameOverride {
    pub fn labels(&self) -> Vec<(String, String)> {
        let name_id = self.name_id.to_ascii_lowercase();
        let slot = format!("{:02}", self.slot);
        let mut out = Vec::new();
        if let Some(text) = self
            .chr0
            .clone()
            .or_else(|| self.fallback.clone())
        {
            out.push((format!("nam_chr0_{slot}_{name_id}"), text));
        }
        if let Some(text) = self
            .chr1
            .clone()
            .or_else(|| self.fallback.clone())
        {
            out.push((format!("nam_chr1_{slot}_{name_id}"), text));
        }
        if let Some(text) = self
            .chr2
            .clone()
            .or_else(|| {
                self.fallback
                    .clone()
                    .map(|s| s.to_uppercase())
            })
            .or_else(|| {
                self.chr0
                    .clone()
                    .or_else(|| self.chr1.clone())
                    .map(|s| s.to_uppercase())
            })
        {
            // If chr2 was explicitly set, keep as-is; otherwise upper-case the fallback.
            let text = if self.chr2.is_some() {
                text
            } else {
                text.to_uppercase()
            };
            out.push((format!("nam_chr2_{slot}_{name_id}"), text));
        }
        out
    }
}

/// Render a complete `.xmsbt` document from already-resolved labels.
///
/// One encoder behind every write path. There used to be three parallel
/// render/write pairs here (simple, detailed, labels) with identical bodies;
/// every export resolves to labels first (`export::resolve_all_names`), so the
/// other two only gave future edits three places to diverge.
///
/// Returns `None` when there is nothing to write. An empty override file is not harmless: it
/// is a file the mod ships, and a reader that meets one has to decide whether the mod meant to
/// blank every name.
pub fn render_xmsbt_from_labels(labels: &[(String, String)]) -> Option<Vec<u8>> {
    if labels.is_empty() {
        return None;
    }
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-16\"?>\n<xmsbt>\n");
    for (label, text) in labels {
        xml.push_str(&format!(
            "  <entry label=\"{}\">\n    <text>{}</text>\n  </entry>\n",
            escape(label),
            escape(text)
        ));
    }
    xml.push_str("</xmsbt>\n");
    Some(to_utf16le_with_bom(&xml))
}

/// Write the override into a mod folder, or remove a stale one when there is nothing to say.
pub fn write_xmsbt_labels(
    mod_root: &std::path::Path,
    labels: &[(String, String)],
) -> anyhow::Result<Option<PathBuf>> {
    let path = mod_root.join(XMSBT_PATH);
    match render_xmsbt_from_labels(labels) {
        Some(body) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, body)?;
            Ok(Some(path))
        }
        None => {
            // A previous export may have left one behind. Removing it is the difference
            // between "this mod no longer renames anything" and "this mod still renames
            // things, from a version of the project that no longer exists".
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            Ok(None)
        }
    }
}

/// UTF-16 little-endian with a byte order mark, which is what the format requires.
fn to_utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// XML-escape a value. Display names are user text and routinely contain `&`.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> String {
        assert_eq!(
            &bytes[..2],
            &[0xFF, 0xFE],
            "missing UTF-16 LE byte order mark"
        );
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    fn sample() -> NameOverride {
        NameOverride {
            name_id: "pickel".into(),
            slot: 0,
            display: "Steve".into(),
        }
    }

    /// The label shape is the whole contract with the game. It was confirmed against real
    /// name-mod templates, and a wrong one produces a file that loads and renames nothing.
    #[test]
    fn labels_match_the_games_naming_scheme() {
        let labels: Vec<String> = sample()
            .labels()
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "nam_chr0_00_pickel",
                "nam_chr1_00_pickel",
                "nam_chr2_00_pickel"
            ]
        );
    }

    /// Slots are two-digit here as they are everywhere else in the UI files.
    #[test]
    fn slot_numbers_are_two_digit() {
        let entry = NameOverride {
            slot: 8,
            ..sample()
        };
        assert!(entry.labels()[0].0.ends_with("_08_pickel"));
    }

    /// A mod that sets the mixed-case label and not the uppercase one shows two different
    /// names in different menus.
    #[test]
    fn the_uppercase_label_gets_the_uppercased_name() {
        let texts: Vec<String> = sample()
            .labels()
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(texts, vec!["Steve", "Steve", "STEVE"]);
    }

    #[test]
    fn the_document_is_utf16_with_a_bom_and_well_formed() {
        let bytes = render_xmsbt_from_labels(&sample().labels()).unwrap();
        let xml = decode(&bytes);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-16\"?>"));
        assert!(xml.contains("<entry label=\"nam_chr1_00_pickel\">"));
        assert!(xml.contains("<text>Steve</text>"));
        assert!(xml.trim_end().ends_with("</xmsbt>"));
    }

    /// Display names are user text. An unescaped `&` produces a file the game's parser
    /// rejects, taking every other name in the file down with it.
    #[test]
    fn names_with_xml_significant_characters_are_escaped() {
        let entry = NameOverride {
            display: "R.O.B. & <Friends>".into(),
            ..sample()
        };
        let xml = decode(&render_xmsbt_from_labels(&entry.labels()).unwrap());
        assert!(xml.contains("R.O.B. &amp; &lt;Friends&gt;"));
        assert!(!xml.contains("& <"));
    }

    /// An empty document is not harmless: a reader meeting one has to decide whether the mod
    /// meant to blank every name.
    #[test]
    fn nothing_to_say_writes_no_file_and_removes_a_stale_one() {
        assert!(render_xmsbt_from_labels(&[]).is_none());

        let dir = tempfile::tempdir().unwrap();
        let written = write_xmsbt_labels(dir.path(), &sample().labels())
            .unwrap()
            .unwrap();
        assert!(written.exists());

        assert!(write_xmsbt_labels(dir.path(), &[]).unwrap().is_none());
        assert!(
            !written.exists(),
            "a stale override from an earlier export survived"
        );
    }
}
