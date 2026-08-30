//! ---
//! purpose: grok 忙时默认把显式队列顶出去（再按回车）
//! contract:
//!   provides:
//!     - name: flush_explicit_queue
//!       what: 屏幕出现 grok `Enter:send now` 时只重按回车直到消失；默认不认 cursor 页脚
//!   depends:
//!     - crate::transport::Transport
//! boundary:
//!   - 不重粘文本、不用 Escape、不用 Ctrl-C、不加自动重投
//!   - 不按忙闲决定发不发；只在已经注入之后确认队列态
//!   - 无标记时不改 claude/codex 提交行为
//!   - cursor 第二下 Enter 会打断进行中回合；生产 flush 不带 cursor 标记
//! maturity: wired
//! ---

use std::time::Duration;

use crate::transport::{CaptureRange, Key, Target, Transport, TransportError};

/// grok 1.0.4 实测：页脚出现这串就是显式队列。
pub const GROK_SEND_NOW_MARK: &str = "Enter:send now";

/// 2026-08-18 真机，cursor-agent 2026.08.11-e8db854。
/// 忙时 follow-ups 盒页脚原文（大小写按屏上）：
/// `enter send now · ↑ select/edit · esc cancel`
/// 出处：`.team/scripts/cursor_send.sh` 的 `QUEUE_MARK='enter send now'`
/// （脚本头 2026-08-18 真机段）。与 grok 的 `Enter:send now`（大写 E、冒号）
/// 不是同一串。
pub const CURSOR_SEND_NOW_MARK: &str = "enter send now";

/// 与 grok_send.sh / cursor_send.sh 对齐：最多再按 8 次回车。
pub const GROK_SEND_NOW_MAX_ENTERS: u32 = 8;

pub struct FlushReport {
    pub extra_enters: u32,
    pub mark_cleared: bool,
}

/// After paste+first Enter, if the pane still shows a queue footer,
/// press Enter only until the mark is gone. No mark ⇒ no keys (claude/codex).
pub fn flush_explicit_queue(
    transport: &dyn Transport,
    target: &Target,
) -> Result<FlushReport, TransportError> {
    flush_explicit_queue_for(transport, target, &[GROK_SEND_NOW_MARK])
}

/// One detector path at a time. Production flush is grok-only.
/// Tests enable a single mark so a false-positive cannot hide behind the other path.
pub fn flush_explicit_queue_for(
    transport: &dyn Transport,
    target: &Target,
    marks: &[&str],
) -> Result<FlushReport, TransportError> {
    let mut extra_enters = 0;
    for _ in 0..GROK_SEND_NOW_MAX_ENTERS {
        let captured = transport.capture(target, CaptureRange::Tail(40))?;
        if !queue_mark_visible_for(&captured.text, marks) {
            return Ok(FlushReport {
                extra_enters,
                mark_cleared: true,
            });
        }
        transport.send_keys(target, &[Key::Enter])?;
        extra_enters = extra_enters.saturating_add(1);
        std::thread::sleep(retry_pause());
    }
    let captured = transport.capture(target, CaptureRange::Tail(40))?;
    Ok(FlushReport {
        extra_enters,
        mark_cleared: !queue_mark_visible_for(&captured.text, marks),
    })
}

pub fn keep_provider_queue_requested() -> bool {
    matches!(
        std::env::var("TEAM_AGENT_KEEP_PROVIDER_QUEUE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

pub fn queue_mark_visible(text: &str) -> bool {
    queue_mark_visible_for(text, &[GROK_SEND_NOW_MARK])
}

pub fn queue_mark_visible_for(text: &str, marks: &[&str]) -> bool {
    marks.iter().any(|mark| text.contains(mark))
}

#[allow(dead_code)]
fn retry_pause() -> Duration {
    Duration::from_millis(50)
}
