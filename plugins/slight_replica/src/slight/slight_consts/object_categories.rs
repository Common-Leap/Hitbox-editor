//! Object categories from `slight_consts::object_categories`.

use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectCategory {
    Fighter,
    Weapon,
    Enemy,
    Gimmick,
    Invalid,
}

const NAMES: &[(&str, ObjectCategory)] = &[
    ("FIGHTER", ObjectCategory::Fighter),
    ("WEAPON", ObjectCategory::Weapon),
    ("ENEMY", ObjectCategory::Enemy),
    ("GIMMICK", ObjectCategory::Gimmick),
    ("INVALID", ObjectCategory::Invalid),
];

impl FromStr for ObjectCategory {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_uppercase();
        for (name, cat) in NAMES {
            if *name == key {
                return Ok(*cat);
            }
        }
        Err(())
    }
}

pub fn parse_category(s: &str) -> Option<ObjectCategory> {
    s.parse().ok()
}

impl ObjectCategory {
    pub fn as_i32(self) -> i32 {
        match self {
            ObjectCategory::Fighter => 0,
            ObjectCategory::Weapon => 1,
            ObjectCategory::Enemy => 2,
            ObjectCategory::Gimmick => 3,
            ObjectCategory::Invalid => -1,
        }
    }

    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => ObjectCategory::Fighter,
            1 => ObjectCategory::Weapon,
            2 => ObjectCategory::Enemy,
            3 => ObjectCategory::Gimmick,
            _ => ObjectCategory::Invalid,
        }
    }
}
