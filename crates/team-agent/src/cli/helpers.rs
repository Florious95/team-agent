//!
//! cli · helpers — 跨子模块共用的小工具:tmux 冲突解析、Python 真值/falsy-or 语义、
//! 字符级截断、相对 age 文本、稳定 key-sorted JSON。

use super::*;

pub(crate) fn tmux_conflict_session(error: &str) -> Option<String> {
    let prefix = "tmux session already exists: ";
    let start = error.find(prefix)? + prefix.len();
    let rest = &error[start..];
    let session = rest.split('.').next().unwrap_or(rest).trim();
    if session.is_empty() {
        None
    } else {
        Some(session.to_string())
    }
}



pub(crate) fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n
            .as_i64()
            .map(|v| v != 0)
            .or_else(|| n.as_u64().map(|v| v != 0))
            .or_else(|| n.as_f64().map(|v| v != 0.0))
            .unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}




pub(crate) fn sort_json(value: &Value) -> Value {
    match value {
        Value::Object(obj) => {
            let mut sorted = Map::new();
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for key in keys {
                if let Some(v) = obj.get(key) {
                    sorted.insert(key.clone(), sort_json(v));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_json).collect()),
        other => other.clone(),
    }
}
