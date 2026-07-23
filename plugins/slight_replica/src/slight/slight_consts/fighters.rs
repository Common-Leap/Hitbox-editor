//! Fighter kind names from `slight_consts` rodata (fighters.rs).

use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FighterKind {
    Mario,
    Luigi,
    Lucina,
    Lucas,
    Lucario,
    LittleMac,
    Kirby,
    KingKRool,
    KingDedede,
    Ken,
    Kazuya,
    Joker,
    Jigglypuff,
    Ivysaur,
    Inkling,
    Incineroar,
    Ike,
    IceClimber,
    Ganondorf,
    GameWatch,
    Fox,
    Falco,
    Element,
    Donkey,
    DoctorMario,
    DiddyKong,
    DarkSamus,
    Daisy,
    Corrin,
    Cloud,
    Chrom,
    Charizard,
    CaptainFalcon,
    Byleth,
    BowserJr,
    BowserGiga,
    Bowser,
    Bayonetta,
    Banjo,
    ZeroSuitSamus,
    Zelda,
    YoungLink,
    Yoshi,
    WiiFitTrainer,
    Wario,
    Terry,
    Steve,
    Sonic,
    Snake,
    Simon,
    Shulk,
    Sheik,
    Samus,
    Ryu,
    Roy,
    Robot,
    Robin,
    Ridley,
    Richter,
    PokemonTrainer,
    Pit,
    PiranhaPlant,
    Pikachu,
    Pichu,
    Peach,
    Pacman,
    Olimar,
    Mythra,
    MinMin,
    MiiSwordsman,
    MiiGunner,
    MiiFighter,
    MiiEnemyS,
    MiiEnemyG,
    MiiEnemyF,
    Mewtwo,
    MetaKnight,
    MegaMan,
    Marth,
    All,
    Unknown,
}

const NAMES: &[(&str, FighterKind)] = &[
    ("ALL", FighterKind::All),
    ("MARIO", FighterKind::Mario),
    ("LUIGI", FighterKind::Luigi),
    ("LUCINA", FighterKind::Lucina),
    ("LUCAS", FighterKind::Lucas),
    ("LUCARIO", FighterKind::Lucario),
    ("LITTLEMAC", FighterKind::LittleMac),
    ("KIRBY", FighterKind::Kirby),
    ("KING_K_ROOL", FighterKind::KingKRool),
    ("KING_DEDEDE", FighterKind::KingDedede),
    ("KEN", FighterKind::Ken),
    ("KAZUYA", FighterKind::Kazuya),
    ("JOKER", FighterKind::Joker),
    ("JIGGLYPUFF", FighterKind::Jigglypuff),
    ("IVYSAUR", FighterKind::Ivysaur),
    ("INKLING", FighterKind::Inkling),
    ("INCINEROAR", FighterKind::Incineroar),
    ("IKE", FighterKind::Ike),
    ("ICECLIMBER", FighterKind::IceClimber),
    ("GANONDORF", FighterKind::Ganondorf),
    ("GAMEWATCH", FighterKind::GameWatch),
    ("FOX", FighterKind::Fox),
    ("FALCO", FighterKind::Falco),
    ("ELEMENT", FighterKind::Element),
    ("DONKEY", FighterKind::Donkey),
    ("DOCTOR_MARIO", FighterKind::DoctorMario),
    ("DIDDY_KONG", FighterKind::DiddyKong),
    ("DARK_SAMUS", FighterKind::DarkSamus),
    ("DAISY", FighterKind::Daisy),
    ("CORRIN", FighterKind::Corrin),
    ("CLOUD", FighterKind::Cloud),
    ("CHROM", FighterKind::Chrom),
    ("CHARIZARD", FighterKind::Charizard),
    ("CAPTAIN_FALCON", FighterKind::CaptainFalcon),
    ("BYLETH", FighterKind::Byleth),
    ("BOWSER_JR", FighterKind::BowserJr),
    ("BOWSER_GIGA", FighterKind::BowserGiga),
    ("BOWSER", FighterKind::Bowser),
    ("BAYONETTA", FighterKind::Bayonetta),
    ("BANJO", FighterKind::Banjo),
    ("ZERO_SUIT_SAMUS", FighterKind::ZeroSuitSamus),
    ("ZELDA", FighterKind::Zelda),
    ("YOUNGLINK", FighterKind::YoungLink),
    ("YOSHI", FighterKind::Yoshi),
    ("WIIFIT_TRAINER", FighterKind::WiiFitTrainer),
    ("WARIO", FighterKind::Wario),
    ("TERRY", FighterKind::Terry),
    ("STEVE", FighterKind::Steve),
    ("SONIC", FighterKind::Sonic),
    ("SNAKE", FighterKind::Snake),
    ("SIMON", FighterKind::Simon),
    ("SHULK", FighterKind::Shulk),
    ("SHEIK", FighterKind::Sheik),
    ("SAMUS", FighterKind::Samus),
    ("RYU", FighterKind::Ryu),
    ("ROY", FighterKind::Roy),
    ("ROBOT", FighterKind::Robot),
    ("ROBIN", FighterKind::Robin),
    ("RIDLEY", FighterKind::Ridley),
    ("RICHTER", FighterKind::Richter),
    ("POKEMON_TRAINER", FighterKind::PokemonTrainer),
    ("PIT", FighterKind::Pit),
    ("PIRANHA_PLANT", FighterKind::PiranhaPlant),
    ("PIKACHU", FighterKind::Pikachu),
    ("PICHU", FighterKind::Pichu),
    ("PEACH", FighterKind::Peach),
    ("PACMAN", FighterKind::Pacman),
    ("OLIMAR", FighterKind::Olimar),
    ("MYTHRA", FighterKind::Mythra),
    ("MINMIN", FighterKind::MinMin),
    ("MIISWORDSMAN", FighterKind::MiiSwordsman),
    ("MIIGUNNER", FighterKind::MiiGunner),
    ("MIIFIGHTER", FighterKind::MiiFighter),
    ("MIIENEMYS", FighterKind::MiiEnemyS),
    ("MIIENEMYG", FighterKind::MiiEnemyG),
    ("MIIENEMYF", FighterKind::MiiEnemyF),
    ("MEWTWO", FighterKind::Mewtwo),
    ("METAKNIGHT", FighterKind::MetaKnight),
    ("MEGAMAN", FighterKind::MegaMan),
    ("MARTH", FighterKind::Marth),
];

impl FromStr for FighterKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_uppercase().replace(' ', "_");
        for (name, kind) in NAMES {
            if *name == key {
                return Ok(*kind);
            }
        }
        Err(())
    }
}

pub fn parse_fighter(s: &str) -> Option<FighterKind> {
    s.parse().ok()
}

pub fn name(kind: FighterKind) -> &'static str {
    for (n, k) in NAMES {
        if *k == kind {
            return n;
        }
    }
    "UNKNOWN"
}

/// Number of fighter-kind entries in the game's lowercase-name table.
const FIGHTER_NAME_COUNT: usize = 118;
/// `.text`-relative offset of the game's lowercase fighter-name table (array of `*const u8`
/// C-string pointers). Same table/offset the loaded smashline 2 plugin reads
/// (`LOWERCASE_FIGHTER_NAMES`), valid for the current SSBU version.
const FIGHTER_NAMES_OFFSET: usize = 0x4f80e20;

/// Resolve a game fighter-kind id (`utility::get_kind`) to its lowercase name (e.g. "mario",
/// "donkey") by reading the game's own name table — the authoritative, version-correct source.
pub fn game_kind_name(kind: i32) -> Option<&'static str> {
    if kind < 0 || kind as usize >= FIGHTER_NAME_COUNT {
        return None;
    }
    unsafe {
        let text = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as usize;
        if text == 0 {
            return None;
        }
        let ptr = *((text + FIGHTER_NAMES_OFFSET + 8 * kind as usize) as *const *const u8);
        if ptr.is_null() {
            return None;
        }
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        Some(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
            ptr, len,
        )))
    }
}
