//! Extras command names from `slight_consts::commands`.

use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandName {
    Rot,
    Stick,
    Top,
    Forward,
    All,
    SetMultiplier,
    ClearMultipliers,
    DebugLog,
    Unknown,
}

const NAMES: &[(&str, CommandName)] = &[
    ("ROT", CommandName::Rot),
    ("STICK", CommandName::Stick),
    ("TOP", CommandName::Top),
    ("FORWARD", CommandName::Forward),
    ("ALL", CommandName::All),
    ("SET_MULTIPLIER", CommandName::SetMultiplier),
    ("MULTIPLIER", CommandName::SetMultiplier),
    ("CLEAR_MULTIPLIERS", CommandName::ClearMultipliers),
    ("DEBUG", CommandName::DebugLog),
    ("DEBUG_LOG", CommandName::DebugLog),
];

impl FromStr for CommandName {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_uppercase().replace(' ', "_");
        for (name, cmd) in NAMES {
            if *name == key {
                return Ok(*cmd);
            }
        }
        Err(())
    }
}

pub fn parse_command(s: &str) -> CommandName {
    s.parse().unwrap_or(CommandName::Unknown)
}
