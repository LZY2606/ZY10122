//! Small deterministic fingerprint helpers (SHA-256 hex).

use sha2::{Digest, Sha256};

pub fn sha256_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Stable JSON fingerprint: serialize with a fixed, object-sorted serializer.
pub fn canonical_json(value: &serde_json::Value) -> String {
    fn emit(value: &serde_json::Value, out: &mut String) {
        use serde_json::Value;
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => {
                out.push('"');
                for ch in s.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    emit(item, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    emit(&serde_json::Value::String((*key).clone()), out);
                    out.push(':');
                    emit(&map[*key], out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    emit(value, &mut out);
    out
}

pub fn json_fingerprint(parts: &[&str], value: &serde_json::Value) -> String {
    let mut preamble = String::new();
    for part in parts {
        preamble.push_str(part);
        preamble.push('|');
    }
    sha256_hex(&[&preamble, &canonical_json(value)])
}
