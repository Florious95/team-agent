use std::time::{Duration, Instant};

/// One monotonic clock domain per execution; fake clocks advance deterministically.
pub trait Clock {
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

pub struct RealClock {
    origin: Instant,
}

impl RealClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub fn remaining(clock: &dyn Clock, deadline: Duration) -> Option<Duration> {
    deadline
        .checked_sub(clock.now())
        .filter(|remaining| !remaining.is_zero())
}
