//! A PID is an address, not a seat identity. These observations never authorize
//! signalling: callers must still prove pane/workspace ownership separately.
use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CarrierState {
    Possible,
    Dead,
    Reused,
    Unknown,
}

impl CarrierState {
    pub(crate) fn gone(self) -> bool {
        matches!(self, Self::Dead | Self::Reused)
    }
}

pub(crate) struct CarrierObservation {
    pub(crate) state: CarrierState,
    pub(crate) birth: Option<String>,
    pub(crate) detail: String,
}

pub(crate) fn pane_pid(row: &Value) -> Option<u32> {
    row.get("pane_pid")
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .and_then(|pid| u32::try_from(pid).ok())
}

pub(crate) fn birth_identity(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        team_agent_contract::host::process::process_birth_identity(pid).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// Update the pair in the same row before the existing atomic state save.
/// Null birth means capture failed, not permission to use the legacy fallback.
pub(crate) fn record(row: &mut Map<String, Value>, pid: Option<u32>) {
    if let Some(pid) = pid {
        row.insert("pane_pid".into(), Value::from(pid));
        row.insert(
            "pane_birth_identity".into(),
            serde_json::json!(birth_identity(pid)),
        );
    } else {
        row.remove("pane_pid");
        row.remove("pane_birth_identity");
    }
}

pub(crate) fn observe(row: &Value, pid: u32) -> CarrierObservation {
    let birth = (pid > 1).then(|| birth_identity(pid)).flatten();
    let boot = boot_time();
    let started = birth.as_deref().and_then(|birth| birth_time(birth, boot));
    let spawned = row
        .get("spawned_at")
        .and_then(Value::as_str)
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.with_timezone(&Utc));
    let (state, reason) = if pid <= 1 {
        (CarrierState::Unknown, "invalid carrier PID")
    } else if let Some(actual) = birth.as_deref() {
        // Other historical PID aliases do not share pane_pid's exact birth.
        if let Some(expected) = row
            .get("pane_birth_identity")
            .filter(|_| pane_pid(row) == Some(pid))
        {
            match expected.as_str() {
                Some(expected) if expected == actual => {
                    (CarrierState::Possible, "recorded birth matches")
                }
                Some(_) => (CarrierState::Reused, "recorded birth differs"),
                None => (CarrierState::Unknown, "recorded birth is unreadable"),
            }
        } else if let Some(spawned) = spawned {
            if boot.is_some_and(|boot| boot > spawned) {
                (CarrierState::Reused, "host booted after the recorded spawn")
            } else if started.is_some_and(|started| started > spawned + Duration::seconds(120)) {
                (
                    CarrierState::Reused,
                    "process birth is after the recorded spawn window",
                )
            } else if started.is_some_and(|started| started < spawned - Duration::seconds(5)) {
                (
                    CarrierState::Unknown,
                    "legacy process predates the recorded spawn window",
                )
            } else if boot.is_some() && started.is_some() {
                (
                    CarrierState::Possible,
                    "legacy birth is inside the spawn window",
                )
            } else {
                (
                    CarrierState::Unknown,
                    "process start or host boot time is unreadable",
                )
            }
        } else {
            (CarrierState::Unknown, "recorded spawn time is unreadable")
        }
    } else if matches!(
        crate::platform::process::pid_liveness(pid),
        Ok(crate::platform::process::ProcessLiveness::Dead)
    ) {
        (CarrierState::Dead, "recorded PID is dead")
    } else {
        (CarrierState::Unknown, "live process birth is unreadable")
    };
    CarrierObservation {
        state,
        detail: format!("pid={pid}; recorded_birth={}; current_birth={}; process_started_at={}; spawned_at={}; boot_time={}; {reason}",
            row.get("pane_birth_identity").map(Value::to_string).unwrap_or_else(|| "missing".into()),
            birth.as_deref().unwrap_or("unreadable"),
            started.map(|time| time.to_rfc3339()).unwrap_or_else(|| "unreadable".into()),
            row.get("spawned_at").and_then(Value::as_str).unwrap_or("missing"),
            boot.map(|time| time.to_rfc3339()).unwrap_or_else(|| "unreadable".into())),
        birth,
    }
}

/// Denials apply across projections and PID aliases, not just selected roots.
pub(crate) fn protected_roots(state: &Value) -> std::collections::BTreeMap<u32, Observation> {
    let mut roots = std::collections::BTreeMap::new();
    for scope in std::iter::once(state).chain(
        state
            .get("teams")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|teams| teams.values()),
    ) {
        for row in scope
            .get("agents")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|agents| agents.values())
        {
            for key in ["provider_pid", "process_id", "pid", "child_pid", "pane_pid"] {
                let Some(pid) = row
                    .get(key)
                    .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
                    .and_then(|pid| u32::try_from(pid).ok())
                    .filter(|pid| *pid > 1)
                else {
                    continue;
                };
                let observation = observe(row, pid);
                if (matches!(
                    observation.state,
                    CarrierState::Reused | CarrierState::Unknown
                ) || (observation.state == CarrierState::Possible
                    && row["status"] == "stopped"))
                    && roots
                        .get(&pid)
                        .is_none_or(|old: &Observation| old.state == CarrierState::Reused)
                {
                    roots.insert(pid, observation);
                }
            }
        }
    }
    roots
}

/// Closing a containing pane also threatens a protected child. Conversely, a
/// descendant cannot regain kill eligibility through another discovery path.
pub(crate) fn protected_target(
    pid: u32,
    roots: &std::collections::BTreeMap<u32, Observation>,
) -> Option<&str> {
    fn ancestor(ancestor: u32, mut child: u32) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        while child > 1 && seen.insert(child) {
            if child == ancestor {
                return true;
            }
            let Some(parent) = crate::platform::argv::parent_pid(child) else {
                return !matches!(
                    crate::platform::process::pid_liveness(child),
                    Ok(crate::platform::process::ProcessLiveness::Dead)
                );
            };
            child = parent;
        }
        child > 1 // A changing/cyclic ancestry is not proof of separation.
    }
    roots.iter().find_map(|(root, observation)| {
        (ancestor(*root, pid) || ancestor(pid, *root)).then_some(observation.detail.as_str())
    })
}

fn birth_time(birth: &str, boot: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    let parts: Vec<_> = birth.split(':').collect();
    match parts.as_slice() {
        ["macos", seconds, micros] => {
            let micros: u32 = micros.parse().ok()?;
            if micros >= 1_000_000 {
                return None;
            }
            DateTime::from_timestamp(seconds.parse().ok()?, micros * 1000)
        }
        #[cfg(target_os = "linux")]
        ["linux", _, ticks] => {
            let ticks: u64 = ticks.parse().ok()?;
            // SAFETY: sysconf only reads the kernel clock-tick frequency.
            let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
                .ok()
                .filter(|hz| *hz > 0)?;
            let elapsed =
                std::time::Duration::new(ticks / hz, ((ticks % hz) * 1_000_000_000 / hz) as u32);
            boot?.checked_add_signed(Duration::from_std(elapsed).ok()?)
        }
        _ => {
            let _ = boot;
            None
        }
    }
}

fn boot_time() -> Option<DateTime<Utc>> {
    #[cfg(target_os = "macos")]
    {
        let mut boot = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of_val(&boot);
        // SAFETY: the output pointer and length describe a writable timeval;
        // this read-only sysctl supplies no new value and cannot signal a PID.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut boot as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || size != std::mem::size_of_val(&boot) {
            return None;
        }
        let micros = u32::try_from(boot.tv_usec)
            .ok()
            .filter(|value| *value < 1_000_000)?;
        DateTime::from_timestamp(boot.tv_sec, micros * 1000)
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let seconds = stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        DateTime::from_timestamp(seconds, 0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}
