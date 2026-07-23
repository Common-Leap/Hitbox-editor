//! Delimiter-based CSV with quoted fields — Jorge excommand tokenizer (no regex dep).

fn unescape_quoted(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split `input` on Jorge delimiter set `[,\n]`, honoring `"..."` quotes.
pub fn split_fields(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] == b',' || bytes[i] == b'\n') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'"' {
                    break;
                } else {
                    i += 1;
                }
            }
            let field = unescape_quoted(&input[start..i.min(bytes.len())]);
            if !field.is_empty() {
                out.push(field);
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b'\n' {
                i += 1;
            }
            let field = input[start..i].trim();
            if !field.is_empty() {
                out.push(field.to_string());
            }
        }
    }
    out
}

pub fn split_lines(input: &str) -> Vec<String> {
    input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

pub fn split_record(line: &str) -> Vec<String> {
    split_fields(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_comma() {
        let fields = split_record(r#""hello, world",2,3"#);
        assert_eq!(fields, vec!["hello, world", "2", "3"]);
    }

    #[test]
    fn escaped_quote() {
        let fields = split_record(r#""a\"b",1"#);
        assert_eq!(fields, vec![r#"a"b"#, "1"]);
    }

    #[test]
    fn newline_field() {
        let fields = split_fields("a\nb");
        assert_eq!(fields, vec!["a", "b"]);
    }
}
