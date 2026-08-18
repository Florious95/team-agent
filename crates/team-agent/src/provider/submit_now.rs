//! ---
//! purpose: grok/cursor 忙时默认把显式队列顶出去（再按回车）
//! contract:
//!   provides:
//!     - name: flush_explicit_queue
//!       what: 屏幕出现 grok 或 cursor 的 send-now 标记时只重按回车直到消失
//!   depends:
//!     - crate::transport::Transport
//! boundary:
//!   - 不重粘文本、不用 Escape、不用 Ctrl-C、不加自动重投
//!   - 不按忙闲决定发不发；只在已经注入之后确认队列态
//!   - 无标记时不改 claude/codex 提交行为
//! maturity: wired
//! ---

use std::time::Duration;

use crate::transport::{CaptureRange, Key, Target, Transport, TransportError};

/// grok 1.0.4 实测：页脚出现这串就是显式队列。
pub const GROK_SEND_NOW_MARK: &str = "Enter:send now";

/// cursor-agent 2026.08.11 实测页脚原文（大小写按屏上，与 grok 不同）。
pub const CURSOR_SEND_NOW_MARK: &str = "enter send now";

/// 与 grok_send.sh / cursor_send.sh 对齐：最多再按 8 次回车。
pub const GROK_SEND_NOW_MAX_ENTERS: u32 = 8;

pub struct FlushReport {
    pub extra_enters: u32,
    pub mark_cleared: bool,
}

/// After paste+first Enter, if the pane still shows the grok queue footer,
/// press Enter only until the mark is gone. No mark ⇒ no keys (claude/codex).
pub fn flush_explicit_queue(
    transport: &dyn Transport,
    target: &Target,
) -> Result<FlushReport, TransportError> {
    let mut extra_enters = 0;
    for _ in 0..GROK_SEND_NOW_MAX_ENTERS {
        let captured = transport.capture(target, CaptureRange::Tail(40))?;
        if !queue_mark_visible(&captured.text) {
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
        mark_cleared: !queue_mark_visible(&captured.text),
    })
}

pub fn keep_provider_queue_requested() -> bool {
    matches!(
        std::env::var("TEAM_AGENT_KEEP_PROVIDER_QUEUE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

pub fn queue_mark_visible(text: &str) -> bool {
    text.contains(GROK_SEND_NOW_MARK) || text.contains(CURSOR_SEND_NOW_MARK)
}

#[allow(dead_code)]
fn retry_pause() -> Duration {
    Duration::from_millis(50)
}
