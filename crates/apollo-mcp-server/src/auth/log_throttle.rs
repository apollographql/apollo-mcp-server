use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Minimum time between emissions of attacker-influenced auth logs.
const AUTH_LOG_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct LogWindow {
    last_emission: Option<Instant>,
    suppressed: u64,
}

/// Emits immediately on the first event, then returns the number of events
/// suppressed since the preceding emission at most once per interval.
#[derive(Default)]
pub(super) struct LogThrottle {
    window: Mutex<LogWindow>,
}

impl LogThrottle {
    pub(super) fn record(&self, now: Instant) -> Option<u64> {
        let mut window = self.window.lock();
        let emission_due = window
            .last_emission
            .is_none_or(|last| now.saturating_duration_since(last) >= AUTH_LOG_INTERVAL);

        if emission_due {
            window.last_emission = Some(now);
            Some(std::mem::take(&mut window.suppressed))
        } else {
            window.suppressed = window.suppressed.saturating_add(1);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_first_event_then_reports_suppressed_count_once_per_interval() {
        let throttle = LogThrottle::default();
        let start = Instant::now();

        assert_eq!(throttle.record(start), Some(0));
        assert_eq!(throttle.record(start + Duration::from_secs(1)), None);
        assert_eq!(
            throttle.record(start + AUTH_LOG_INTERVAL - Duration::from_nanos(1)),
            None
        );
        assert_eq!(throttle.record(start + AUTH_LOG_INTERVAL), Some(2));
    }
}
