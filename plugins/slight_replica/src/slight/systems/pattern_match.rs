//! Pattern rules — Jorge multiplier context match (regex-automata, no regex crate).

use regex_automata::meta::Regex;
use std::sync::LazyLock;

pub struct PatternRule {
    engine: Regex,
}

impl PatternRule {
    pub fn compile(pattern: &str) -> Option<Self> {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return None;
        }
        Regex::new(trimmed).ok().map(|engine| Self { engine })
    }

    pub fn is_match(&self, haystack: &[u8]) -> bool {
        self.engine.is_match(haystack)
    }
}

static EFFECT_KEY: LazyLock<PatternRule> =
    LazyLock::new(|| PatternRule::compile("Effect data").expect("effect data literal"));

pub fn effect_data_key_matches(haystack: &[u8]) -> bool {
    EFFECT_KEY.is_match(haystack)
}
