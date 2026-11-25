//! JSON as orchard's Python writes it. A component's key is the SHA-256 of
//! its inputs written this way, and orchard's CI publishes keys computed in
//! Python, so the bytes have to be the same: keys sorted, `, ` and `: `
//! between items, and everything outside printable ASCII escaped.
use serde_json::Value;
use sha2::{Digest, Sha256};

/// `value` on one line, as Python's `json.dumps(value, sort_keys=True)`.
pub fn dumps(value: &Value) -> String {
    let mut out = String::new();
    write(value, None, 0, &mut out);
    out
}

/// `value` over several lines, as `json.dumps(value, indent=2, sort_keys=True)`.
pub fn pretty(value: &Value) -> String {
    let mut out = String::new();
    write(value, Some(2), 0, &mut out);
    out.push('\n');
    out
}

/// The key of a set of inputs: the SHA-256 of [`dumps`].
pub fn hash(value: &Value) -> String {
    sha256(dumps(value).as_bytes())
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn write(value: &Value, indent: Option<usize>, depth: usize, out: &mut String) {
    let line = |out: &mut String, depth: usize| {
        if let Some(width) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(width * depth));
        }
    };
    let between = if indent.is_some() { "," } else { ", " };
    match value {
        Value::String(text) => string(text, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(between);
                }
                line(out, depth + 1);
                write(item, indent, depth + 1, out);
            }
            line(out, depth);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            let mut names: Vec<&String> = map.keys().collect();
            names.sort();
            out.push('{');
            for (index, name) in names.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(between);
                }
                line(out, depth + 1);
                string(name, out);
                out.push_str(": ");
                write(&map[name], indent, depth + 1, out);
            }
            line(out, depth);
            out.push('}');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn one_line_is_pythons() {
        // python3 -c 'import json; print(json.dumps({...}, sort_keys=True))'
        let value = json!({"b": [1, "x", true, null], "a": {"z": "é\n\"", "y": 493, "e": {}, "l": []}, "😀": "\u{7f}"});
        let expected = [
            r#"{"a": {"e": {}, "l": [], "y": 493, "z": ""#,
            "\\u00e9\\n\\\"",
            r#""}, "b": [1, "x", true, null], ""#,
            "\\ud83d\\ude00",
            r#"": ""#,
            "\\u007f",
            r#""}"#,
        ];
        assert_eq!(dumps(&value), expected.concat());
    }

    #[test]
    fn several_lines_are_pythons() {
        let value = json!({"b": [1, 2], "a": {"c": "d"}, "e": []});
        assert_eq!(
            pretty(&value),
            "{\n  \"a\": {\n    \"c\": \"d\"\n  },\n  \"b\": [\n    1,\n    2\n  ],\n  \"e\": []\n}\n"
        );
    }

    #[test]
    fn a_key_is_the_hash_of_the_one_line_form() {
        // hashlib.sha256(json.dumps({"tree": "abc", "producer": 3}, sort_keys=True).encode()).hexdigest()
        assert_eq!(
            hash(&json!({"tree": "abc", "producer": 3})),
            sha256(br#"{"producer": 3, "tree": "abc"}"#)
        );
    }
}
