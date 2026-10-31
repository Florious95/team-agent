//! Lossless readers and the C3 frame codec. Unknown data is observable, not executable.
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum NoEffectReason {
    ComposerContended,
    PaneGone,
    IdentityMismatch,
    Unknown(String),
}
impl From<String> for NoEffectReason {
    fn from(s: String) -> Self {
        match s.as_str() {
            "composer_contended" | "composer_busy" => Self::ComposerContended,
            "pane_gone" => Self::PaneGone,
            "identity_mismatch" => Self::IdentityMismatch,
            _ => Self::Unknown(s),
        }
    }
}
impl From<NoEffectReason> for String {
    fn from(s: NoEffectReason) -> Self {
        match s {
            NoEffectReason::ComposerContended => "composer_contended".into(),
            NoEffectReason::PaneGone => "pane_gone".into(),
            NoEffectReason::IdentityMismatch => "identity_mismatch".into(),
            NoEffectReason::Unknown(s) => s,
        }
    }
}
#[derive(Debug, PartialEq)]
pub enum ObservedOutcome {
    NoEffect(NoEffectReason),
    Submitted,
    Uncertain(Value),
}
pub fn outcome(value: &Value) -> ObservedOutcome {
    match value["state"].as_str() {
        Some("no_effect") => ObservedOutcome::NoEffect(NoEffectReason::from(
            value["reason"].as_str().unwrap_or("unknown").to_owned(),
        )),
        Some("submitted") => ObservedOutcome::Submitted,
        _ => ObservedOutcome::Uncertain(value.clone()),
    }
}

/// Duplicate keys are rejected recursively before interpretation, including keys
/// with equivalent JSON escapes. serde_json's ordinary Value parser is last-wins.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate_key: {key}")));
                    }
                    let Unique(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}
pub fn read(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice::<Unique>(bytes)
        .map(|value| value.0)
        .map_err(|e| e.to_string())
}

pub fn authorize_mutation(value: &Value, supported: &[&str]) -> Result<(), String> {
    if value["v"].as_u64() != Some(1) {
        return Err("unsupported_version".into());
    }
    if let Some(requires) = value.get("requires") {
        for capability in requires.as_array().ok_or("invalid requires")? {
            let capability = capability.as_str().ok_or("invalid capability")?;
            if !supported.contains(&capability) {
                return Err(format!("unsupported_capability: {capability}"));
            }
        }
    }
    Ok(())
}

/// Replace an owned scalar leaf by splicing only its raw token. Every other byte,
/// including unknown nested keys, whitespace and escape spelling, is unchanged.
pub fn patch_scalar(
    original: &str,
    path: &[&str],
    replacement: &Value,
    supported: &[&str],
) -> Result<String, String> {
    let value = read(original.as_bytes())?;
    authorize_mutation(&value, supported)?;
    let mut token: &RawValue = serde_json::from_str(original).map_err(|e| e.to_string())?;
    for key in path {
        let fields: BTreeMap<String, &RawValue> =
            serde_json::from_str(token.get()).map_err(|e| e.to_string())?;
        token = *fields.get(*key).ok_or("unknown mutation path")?;
    }
    let old: Value = serde_json::from_str(token.get()).map_err(|e| e.to_string())?;
    if old.is_object() || old.is_array() || replacement.is_object() || replacement.is_array() {
        return Err(
            "unsupported_mutation: composite replacement would discard unknown fields".into(),
        );
    }
    let begin = token.get().as_ptr() as usize - original.as_ptr() as usize;
    let end = begin + token.get().len();
    let replacement = serde_json::to_string(replacement).map_err(|e| e.to_string())?;
    Ok(format!(
        "{}{}{}",
        &original[..begin],
        replacement,
        &original[end..]
    ))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Frame {
    pub v: u64,
    pub kind: String,
    pub requires: Vec<String>,
    pub seq: u64,
    pub data: Box<RawValue>,
    pub checksum: String,
}
/// Exact C3 domain/length-prefix/raw-token algorithm, shared with the frozen corpus.
pub fn frame_checksum(
    seq: u64,
    kind: &str,
    requires: &[String],
    data: &[u8],
) -> Result<String, String> {
    let mut hash = Sha256::new();
    hash.update(b"team-agent/inbox/v1\0");
    hash.update(seq.to_be_bytes());
    for field in std::iter::once(kind).chain(requires.iter().map(String::as_str)) {
        let length = u16::try_from(field.len()).map_err(|_| "frame tag is too long")?;
        if length == 0 {
            return Err("empty frame tag".into());
        }
        hash.update(length.to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.update([0, 0]);
    hash.update((data.len() as u64).to_be_bytes());
    hash.update(data);
    Ok(format!("{:x}", hash.finalize()))
}

pub fn encode(record: &Value, seq: u64) -> Result<Vec<u8>, String> {
    if seq == 0 || seq > 9_007_199_254_740_991 {
        return Err("invalid sequence".into());
    }
    let mut data = record.clone();
    let fields = data.as_object_mut().ok_or("record must be an object")?;
    let kind = fields
        .remove("kind")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "message".into());
    for key in [
        "v",
        "seq",
        "checksum",
        "requires",
        "dedupe_key",
        "record_ref",
    ] {
        fields.remove(key);
    }
    let data = serde_json::value::to_raw_value(&data).map_err(|e| e.to_string())?;
    let requires = vec!["station.inbox-commit.v1".to_string()];
    let checksum = frame_checksum(seq, &kind, &requires, data.get().as_bytes())?;
    let mut line = serde_json::to_vec(&Frame {
        v: 1,
        kind,
        requires,
        seq,
        data,
        checksum,
    })
    .map_err(|e| e.to_string())?;
    line.push(b'\n');
    Ok(line)
}

/// Unknown versions remain opaque. Known frames verify their raw token before
/// projecting the legacy business payload for existing inbox consumers.
pub fn decode(line: &[u8]) -> Result<Value, String> {
    if !line.ends_with(b"\n") {
        return Err("torn_record".into());
    }
    let value = read(line)?;
    if !value.is_object() {
        return Err("record must be an object".into());
    }
    if value
        .get("v")
        .and_then(Value::as_u64)
        .is_some_and(|v| v != 1)
    {
        return Ok(
            serde_json::json!({"v":value["v"],"kind":"opaque","opaque":true,"seq":value["seq"],"record":value}),
        );
    }
    if value.get("data").is_none() {
        return Ok(value);
    } // legacy flat records
    let frame: Frame = serde_json::from_slice(line).map_err(|e| e.to_string())?;
    if frame.seq == 0 || frame.seq > 9_007_199_254_740_991 {
        return Err("invalid sequence".into());
    }
    if frame_checksum(
        frame.seq,
        &frame.kind,
        &frame.requires,
        frame.data.get().as_bytes(),
    )? != frame.checksum
    {
        return Err("checksum_mismatch".into());
    }
    let mut data = read(frame.data.get().as_bytes())?;
    if !data.is_object() {
        return Err("frame data must be an object".into());
    }
    data["v"] = frame.v.into();
    data["kind"] = frame.kind.into();
    data["requires"] = serde_json::json!(frame.requires);
    data["seq"] = frame.seq.into();
    data["checksum"] = frame.checksum.into();
    Ok(data)
}
