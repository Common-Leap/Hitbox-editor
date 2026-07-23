//! Jorge `common_status` axis labels for excommand direction args.

pub const X: &str = "x";
pub const Y: &str = "y";
pub const Z: &str = "z";

pub fn parse_axis(s: &str) -> Option<char> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "x" => Some('x'),
        "y" => Some('y'),
        "z" => Some('z'),
        _ => None,
    }
}
