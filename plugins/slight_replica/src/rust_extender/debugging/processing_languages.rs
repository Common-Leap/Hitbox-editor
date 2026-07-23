//! C# float literal parser — processing_languages.rs @ 6094abf.

pub fn parse_three_floats(input: &str) -> Option<[f32; 3]> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_number = false;

    for ch in input.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' {
            current.push(ch);
            in_number = true;
        } else if in_number {
            if let Ok(v) = current.parse::<f32>() {
                values.push(v);
            }
            current.clear();
            in_number = false;
            if values.len() >= 3 {
                break;
            }
        }
    }
    if in_number {
        if let Ok(v) = current.parse::<f32>() {
            values.push(v);
        }
    }
    if values.len() >= 3 {
        Some([values[0], values[1], values[2]])
    } else {
        None
    }
}
