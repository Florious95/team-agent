//! leader 内部 helper —— provider/turn-state wire 串、JSON path 取值、rollout 读、
//! workspace 路径归一 / session 文件夹消毒、uuid 前缀、时间戳、sha1。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::provider::{Provider, RolloutPath, TurnState};

use super::LeaderError;

pub(crate) use crate::provider::wire::{parse_provider, provider_wire};

pub(crate) fn turn_state_wire(state: TurnState) -> &'static str {
    match state {
        TurnState::Idle => "idle",
        TurnState::Working => "working",
        TurnState::IdleInterrupted => "idle_interrupted",
        TurnState::BlockedOnHuman => "blocked_on_human",
        TurnState::Abnormal => "abnormal",
        TurnState::Unknown => "unknown",
    }
}

pub(crate) fn get_path_str(value: &Value, path: &[&str]) -> Option<String> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str().map(str::to_string)
}

pub(crate) fn get_path_u64(value: &Value, path: &[&str]) -> Option<u64> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_u64()
}

pub(crate) fn read_rollout_text(path: Option<&RolloutPath>) -> Result<String, LeaderError> {
    match path {
        Some(p) => match std::fs::read_to_string(p.as_path()) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(LeaderError::Io(e)),
        },
        None => Ok(String::new()),
    }
}

pub(crate) fn resolve_workspace_for_hash(workspace: &Path) -> PathBuf {
    match workspace.canonicalize() {
        Ok(path) => path,
        Err(_) if workspace.is_absolute() => workspace.to_path_buf(),
        Err(_) => match std::env::current_dir() {
            Ok(cwd) => cwd.join(workspace),
            Err(_) => workspace.to_path_buf(),
        },
    }
}

pub(crate) fn sanitize_session_folder(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    let trimmed = replaced.trim_matches(|c| matches!(c, '_' | '-' | '.'));
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn prefix(s: &str, chars: usize) -> &str {
    match s.char_indices().nth(chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

pub(crate) fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub(crate) fn sha1_hex_prefix(bytes: &[u8], len: usize) -> String {
    let digest = sha1_digest(bytes);
    let full: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    full.chars().take(len).collect()
}

pub(crate) fn sha1_digest(input: &[u8]) -> [u8; 20] {
    let mut msg = input.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut h0 = 0x6745_2301u32;
    let mut h1 = 0xEFCD_AB89u32;
    let mut h2 = 0x98BA_DCFEu32;
    let mut h3 = 0x1032_5476u32;
    let mut h4 = 0xC3D2_E1F0u32;

    for chunk in msg.chunks(64) {
        let mut w: Vec<u32> = Vec::with_capacity(80);
        for word in chunk.chunks(4).take(16) {
            let mut iter = word.iter().copied();
            let b0 = iter.next().unwrap_or(0);
            let b1 = iter.next().unwrap_or(0);
            let b2 = iter.next().unwrap_or(0);
            let b3 = iter.next().unwrap_or(0);
            w.push(u32::from_be_bytes([b0, b1, b2, b3]));
        }
        for i in 16..80 {
            let value = w.get(i - 3).copied().unwrap_or(0)
                ^ w.get(i - 8).copied().unwrap_or(0)
                ^ w.get(i - 14).copied().unwrap_or(0)
                ^ w.get(i - 16).copied().unwrap_or(0);
            w.push(value.rotate_left(1));
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (i, wi) in w.iter().copied().enumerate().take(80) {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    let mut pos = 0usize;
    for word in [h0, h1, h2, h3, h4] {
        for byte in word.to_be_bytes() {
            if let Some(slot) = out.get_mut(pos) {
                *slot = byte;
            }
            pos = pos.saturating_add(1);
        }
    }
    out
}
