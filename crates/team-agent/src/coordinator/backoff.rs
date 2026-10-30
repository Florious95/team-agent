//! Retired daemon tests pending the dedicated test-excision PR.

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_ws(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ta-rs-coord-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn coordinator_tick_panic_writes_durable_marker() {
        let workspace = tmp_ws("tick-panic");
        let event_log = EventLog::new(&workspace);

        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = run_tick_with_panic_marker(&event_log, || -> Result<TickReport, TickError> {
            panic!("synthetic tick panic")
        });
        std::panic::set_hook(old_hook);

        assert!(
            matches!(result, Err(TickError::Panic(message)) if message == "synthetic tick panic")
        );
        let events = event_log.tail(20).unwrap();
        let panic_event = events
            .iter()
            .find(|event| {
                event.get("event").and_then(serde_json::Value::as_str)
                    == Some("coordinator.tick_panic")
            })
            .expect("coordinator.tick_panic event");
        assert_eq!(
            panic_event.get("panic").and_then(serde_json::Value::as_str),
            Some("synthetic tick panic")
        );
        assert!(
            panic_event
                .get("backtrace")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|backtrace| !backtrace.is_empty()),
            "panic marker must carry a backtrace; event={panic_event}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn signal_stop_request_interrupts_daemon_sleep_without_consuming_signal() {
        SIGNAL_STOP_NUMBER.store(libc::SIGTERM, std::sync::atomic::Ordering::SeqCst);
        SIGNAL_STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);

        let started = std::time::Instant::now();
        sleep_seconds(1.0);
        let elapsed = started.elapsed();
        let signal = take_signal_stop_request();

        assert_eq!(signal, Some("SIGTERM"));
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "daemon sleep must wake promptly after SIGTERM stop flag; elapsed={elapsed:?}"
        );
    }
}
