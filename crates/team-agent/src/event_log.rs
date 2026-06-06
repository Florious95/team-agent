//! step 4 · event_log — `events.jsonl` 唯一审计流(真相源 `events.py`)。
//!
//! 每行一个事件:`{"ts": iso8601_utc, "event": type, ...fields}`,经
//! `json.dumps(sort_keys=True, ensure_ascii=False)` 序列化。**字节对拍要点(对抗检查)**:
//! - Python `json.dumps` 默认分隔符是 `", "` / `": "`(**带空格**)→ 自定义 [`PythonFormatter`]
//!   (serde_json 默认无空格)。
//! - `sort_keys=True` 是**递归**排序 → [`sort_value`](preserve_order 下按 sorted 序重建;
//!   envelope/state 仍需插入序,故不全局改 serde_json)。
//! - `ensure_ascii=False` → serde_json 默认即不转义非 ASCII(UTF-8 字面)。
//! - 轮转:5 MiB × 保留 5 archives(`events.jsonl.1..5`)。
//!
//! §10:无 unwrap/expect/panic;rotation 的 rename 失败best-effort 忽略(对应 Python
//! `except OSError: pass`,bug-084 的 os.replace 崩溃教训)。
//!
//! **有意分歧/已知边界(对抗检查确认,事件流里不会触发或不复刻 Python bug)**:
//! - `tail` 用 `\n` 行分割(JSONL 正解),**不复刻** Python `splitlines()` 在 U+2028/U+2029/
//!   NEL 上误切 JSON 行的潜在 bug(§11 不重蹈)。
//! - 事件字段**可含 float**(如 `waited_sec: 0.0`,见 bug_082 golden):正常小数 serde_json 与
//!   Python 字节一致(`0.0`/`0.5`…),由真 fixture round-trip 锁死。**已知边界**:`|v|<1e-4` 的
//!   指数浮点(`1e-07` vs serde `1e-7`、`1e-05` vs `0.00001`)与 NaN/Infinity、超 i64 整数会漂移 ——
//!   实测事件流不出现这些值,故未启 serde_json `arbitrary_precision`(全局影响 Number/preserve_order)
//!   或自定义 CPython float_repr;若将来引入,需重审。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize as _;
use serde_json::Value;
use thiserror::Error;

use crate::model::paths::logs_dir;

/// `events.py:17`:5 MiB。
pub const EVENT_LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// `events.py:18`:保留 5 个 archive。
pub const EVENT_LOG_ARCHIVE_KEEP: u32 = 5;

#[derive(Debug, Error)]
pub enum EventLogError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// serde_json `Formatter`,复刻 Python `json.dumps` 默认分隔符 `", "` / `": "`。
struct PythonFormatter;

impl serde_json::ser::Formatter for PythonFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }
    fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }
    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }
}

/// 递归把 object 键排序(`sort_keys=True`)。preserve_order 下 `serde_json::Map` 是 IndexMap,
/// 按 sorted 序插入 → 序列化按插入序 = sorted。
fn sort_value(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_unstable();
            let mut out = serde_json::Map::new();
            for k in keys {
                if let Some(val) = m.get(k) {
                    out.insert(k.clone(), sort_value(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

/// `json.dumps(value, sort_keys=True, ensure_ascii=False)` 的字节等价(传入前先 [`sort_value`])。
fn to_python_json(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PythonFormatter);
    if value.serialize(&mut ser).is_err() {
        return String::new();
    }
    String::from_utf8(buf).unwrap_or_default() // serde_json 必产合法 UTF-8
}

/// `datetime.now(utc).isoformat()` 字节等价:micros==0 省略小数秒(Python isoformat),否则 6 位微秒。
fn format_ts(dt: chrono::DateTime<chrono::Utc>) -> String {
    if dt.timestamp_subsec_micros() == 0 {
        dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    } else {
        dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
    }
}

/// `events.py:EventLog`。
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    /// `EventLog(workspace)`:路径 = `<workspace>/.team/logs/events.jsonl`。
    pub fn new(workspace: &Path) -> Self {
        Self { path: logs_dir(workspace).join("events.jsonl") }
    }

    /// 直接指定 events.jsonl 路径(测试 / 非标准布局)。
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// `EventLog.write`:追加一行 `{ts, event, **fields}`(sort_keys + Python 分隔符)。
    /// 返回合并后的事件 Value。`fields` 应是 object;非 object 视作无字段。
    pub fn write(&self, event_type: &str, fields: Value) -> Result<Value, EventLogError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let ts = format_ts(chrono::Utc::now());
        let mut obj = serde_json::Map::new();
        obj.insert("ts".to_string(), Value::String(ts));
        obj.insert("event".to_string(), Value::String(event_type.to_string()));
        if let Value::Object(f) = fields {
            for (k, v) in f {
                obj.insert(k, v);
            }
        }
        let event = Value::Object(obj);
        self.maybe_rotate()?;
        // 单次 write_all(line+"\n"):POSIX O_APPEND 对 <PIPE_BUF 写原子,避免并发写者交错(对抗 P1)。
        let mut bytes = to_python_json(&sort_value(&event)).into_bytes();
        bytes.push(b'\n');
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        file.write_all(&bytes)?;
        Ok(event)
    }

    /// `EventLog.tail`:末尾 `limit` 行,逐行 JSON parse;失败行 → `{"raw": line}`。
    pub fn tail(&self, limit: usize) -> Result<Vec<Value>, EventLogError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&self.path)?;
        let lines: Vec<&str> = text.lines().collect();
        // Python lines[-limit:]:limit==0 → lines[0:] = 全部(负零切片怪癖);limit>len → 全部。
        let start = if limit == 0 { 0 } else { lines.len().saturating_sub(limit) };
        let mut out = Vec::new();
        for line in &lines[start..] {
            match serde_json::from_str::<Value>(line) {
                Ok(v) => out.push(v),
                Err(_) => {
                    let mut m = serde_json::Map::new();
                    m.insert("raw".to_string(), Value::String((*line).to_string()));
                    out.push(Value::Object(m));
                }
            }
        }
        Ok(out)
    }

    /// `events.py:_maybe_rotate`:size >= 5 MiB → 丢最旧 + 移位 + current→.1。rename 失败忽略。
    fn maybe_rotate(&self) -> Result<(), EventLogError> {
        let size = match std::fs::metadata(&self.path) {
            Ok(m) => m.len(),
            Err(_) => return Ok(()), // 文件不存在 → 不轮转(对应 Python FileNotFoundError)
        };
        if size < EVENT_LOG_ROTATE_BYTES {
            return Ok(());
        }
        let oldest = self.archive_path(EVENT_LOG_ARCHIVE_KEEP);
        if oldest.exists() {
            let _ = std::fs::remove_file(&oldest);
        }
        for idx in (1..EVENT_LOG_ARCHIVE_KEEP).rev() {
            let src = self.archive_path(idx);
            if src.exists() {
                let _ = std::fs::rename(&src, self.archive_path(idx + 1));
            }
        }
        let _ = std::fs::rename(&self.path, self.archive_path(1));
        Ok(())
    }

    fn archive_path(&self, index: u32) -> PathBuf {
        let name = self.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.path.with_file_name(format!("{name}.{index}"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);
    fn temp_ws() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let ws = std::env::temp_dir().join(format!("ta_rs_ev_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }

    // 确定性字节对拍:to_python_json(sort_value(..)) == Python json.dumps(sort_keys,ensure_ascii=False)。
    #[test]
    fn python_json_format_byte_parity() {
        let v = json!({"event":"u","msg":"héllo🦀\n世界","nested":{"b":2,"a":[1,2]}});
        assert_eq!(
            to_python_json(&sort_value(&v)),
            r#"{"event": "u", "msg": "héllo🦀\n世界", "nested": {"a": [1, 2], "b": 2}}"#
        );
        // 空 fields。
        assert_eq!(to_python_json(&sort_value(&json!({"event":"empty"}))), r#"{"event": "empty"}"#);
        // 类型:bool/null/int 与 Python 一致。
        assert_eq!(
            to_python_json(&sort_value(&json!({"missing":false,"x":null,"n":2}))),
            r#"{"missing": false, "n": 2, "x": null}"#
        );
    }

    #[test]
    fn write_sorts_keys_and_has_ts_event() {
        let ws = temp_ws();
        let log = EventLog::new(&ws);
        log.write("schema.layout_rebuild", json!({"table":"messages","row_count_before":2,"row_count_after":2,"missing":false})).unwrap();
        let line = std::fs::read_to_string(ws.join(".team/logs/events.jsonl")).unwrap();
        let line = line.trim_end();
        // 键全排序:event < missing < row_count_after < row_count_before < table < ts。
        assert!(line.starts_with(r#"{"event": "schema.layout_rebuild", "missing": false, "row_count_after": 2, "row_count_before": 2, "table": "messages", "ts": "#));
        // ts 是合法 rfc3339 UTC。
        let v: Value = serde_json::from_str(line).unwrap();
        let ts = v["ts"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(ts).is_ok(), "ts 非合法 rfc3339: {ts}");
        assert!(ts.ends_with("+00:00"));
    }

    #[test]
    fn tail_returns_last_n_and_raw_on_bad_line() {
        let ws = temp_ws();
        let log = EventLog::new(&ws);
        for i in 0..5 {
            log.write("e", json!({"i":i})).unwrap();
        }
        // 追加一行坏 JSON。
        let p = ws.join(".team/logs/events.jsonl");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"not json\n").unwrap();
        drop(f);
        let t = log.tail(2).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["i"], json!(4));
        assert_eq!(t[1]["raw"], json!("not json"));
        // 不存在 → 空。
        assert!(EventLog::new(&temp_ws()).tail(10).unwrap().is_empty());
    }

    #[test]
    fn rotation_at_threshold_shifts_archives_and_drops_oldest() {
        let ws = temp_ws();
        let p = logs_dir(&ws).join("events.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let log = EventLog::at(p.clone());
        // 预置 current >= 5MiB + 已有 .1..=.5 archive(各带标记内容)。
        std::fs::write(&p, vec![b'x'; EVENT_LOG_ROTATE_BYTES as usize]).unwrap();
        for i in 1..=EVENT_LOG_ARCHIVE_KEEP {
            std::fs::write(p.with_file_name(format!("events.jsonl.{i}")), format!("arc{i}")).unwrap();
        }
        // 下一次 write 触发轮转:.5 丢弃,.4→.5 ... .1→.2,current→.1。
        log.write("after.rotate", json!({})).unwrap();
        // current 现在是轮转后的新文件,只含刚写的一条。
        let new_current = std::fs::read_to_string(&p).unwrap();
        assert_eq!(new_current.lines().count(), 1);
        assert!(new_current.contains("after.rotate"));
        // .1 = 旧 current(5MiB 的 x)。
        assert_eq!(std::fs::metadata(p.with_file_name("events.jsonl.1")).unwrap().len(), EVENT_LOG_ROTATE_BYTES);
        // .2 = 旧 .1(内容 "arc1");.5 = 旧 .4("arc4");旧 .5("arc5")被丢弃。
        assert_eq!(std::fs::read_to_string(p.with_file_name("events.jsonl.2")).unwrap(), "arc1");
        assert_eq!(std::fs::read_to_string(p.with_file_name("events.jsonl.5")).unwrap(), "arc4");
    }

    #[test]
    fn no_rotation_below_threshold() {
        let ws = temp_ws();
        let log = EventLog::new(&ws);
        log.write("small", json!({})).unwrap();
        log.write("small2", json!({})).unwrap();
        // 未超阈值 → 无 .1 archive。
        assert!(!ws.join(".team/logs/events.jsonl.1").exists());
        assert_eq!(log.tail(10).unwrap().len(), 2);
    }

    // 对抗 P4:ts micros==0 省略小数秒(Python isoformat);否则 6 位微秒。golden 自 Python。
    #[test]
    fn format_ts_matches_python_isoformat() {
        let m0 = chrono::DateTime::from_timestamp(1_780_000_000, 0).unwrap();
        assert_eq!(format_ts(m0), "2026-05-28T20:26:40+00:00");
        let m = chrono::DateTime::from_timestamp(1_780_000_000, 123_456_000).unwrap();
        assert_eq!(format_ts(m), "2026-05-28T20:26:40.123456+00:00");
    }

    // 对抗 P7:tail(0) 复刻 Python lines[-0:] = 全部(负零切片怪癖)。
    #[test]
    fn tail_zero_returns_all_limit_clamps() {
        let ws = temp_ws();
        let log = EventLog::new(&ws);
        for i in 0..3 {
            log.write("e", json!({ "i": i })).unwrap();
        }
        assert_eq!(log.tail(0).unwrap().len(), 3, "tail(0) == 全部(Python [-0:])");
        assert_eq!(log.tail(2).unwrap().len(), 2);
        assert_eq!(log.tail(99).unwrap().len(), 3);
    }

    // 对抗 P1:多写者并发 append 不交错(单次 write_all 原子)。每行须是完整合法 JSON。
    #[test]
    fn concurrent_appends_do_not_interleave() {
        let ws = temp_ws();
        let path = logs_dir(&ws).join("events.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let p = path.clone();
                std::thread::spawn(move || {
                    let log = EventLog::at(p);
                    for i in 0..50 {
                        log.write("concurrent", json!({ "t": t, "i": i })).unwrap();
                    }
                })
            })
            .collect();
        for h in threads {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 8 * 50, "无半行/丢行");
        for line in &lines {
            // 每行完整合法 JSON 且含 event 键 → 未交错。
            let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("交错坏行: {line:?} ({e})"));
            assert_eq!(v["event"], json!("concurrent"));
        }
    }

    // 对抗 P5:真 events.jsonl golden(60 行 Python 序列化)逐行 round-trip 字节对拍。
    #[test]
    fn real_events_jsonl_fixture_round_trips_byte_identical() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../snapshot/fixtures/bug_082_codex_trust/macmini_0210_own_events_after_corrected_send.jsonl"
        ));
        let mut n = 0;
        for line in fixture.lines() {
            if line.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(to_python_json(&sort_value(&v)), line, "第 {n} 行 round-trip 不字节一致");
            n += 1;
        }
        assert_eq!(n, 60, "fixture 应 60 行");
    }
}
