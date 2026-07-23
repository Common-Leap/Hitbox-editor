//! Button names from `slight_consts::buttons`.

use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    A,
    B,
    X,
    Y,
    L,
    R,
    Zl,
    Zr,
    Plus,
    Minus,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Unknown,
}

const NAMES: &[(&str, Button)] = &[
    ("A", Button::A),
    ("B", Button::B),
    ("X", Button::X),
    ("Y", Button::Y),
    ("L", Button::L),
    ("R", Button::R),
    ("ZL", Button::Zl),
    ("ZR", Button::Zr),
    ("PLUS", Button::Plus),
    ("MINUS", Button::Minus),
    ("DPAD_UP", Button::DpadUp),
    ("DPAD_DOWN", Button::DpadDown),
    ("DPAD_LEFT", Button::DpadLeft),
    ("DPAD_RIGHT", Button::DpadRight),
];

impl FromStr for Button {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_uppercase().replace(' ', "_");
        for (name, btn) in NAMES {
            if *name == key {
                return Ok(*btn);
            }
        }
        Err(())
    }
}

pub fn parse_button(s: &str) -> Button {
    s.parse().unwrap_or(Button::Unknown)
}
