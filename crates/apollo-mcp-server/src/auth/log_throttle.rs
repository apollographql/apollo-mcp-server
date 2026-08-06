use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Baseline interval for attacker-influenced auth logs. The pre-merge JWT
/// benchmark may justify adjusting this value before release.
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
    fn first_event_emits_immediately() {
        let throttle = LogThrottle::default();

        assert_eq!(throttle.record(Instant::now()), Some(0));
    }

    #[test]
    fn events_within_interval_are_suppressed() {
        let throttle = LogThrottle::default();
        let start = Instant::now();
        assert_eq!(throttle.record(start), Some(0));

        assert_eq!(
            throttle.record(start + AUTH_LOG_INTERVAL - Duration::from_nanos(1)),
            None
        );
    }

    #[test]
    fn next_emission_reports_suppressed_events_since_preceding_emission() {
        let throttle = LogThrottle::default();
        let start = Instant::now();
        assert_eq!(throttle.record(start), Some(0));
        assert_eq!(throttle.record(start + Duration::from_secs(1)), None);
        assert_eq!(throttle.record(start + Duration::from_secs(2)), None);

        assert_eq!(throttle.record(start + AUTH_LOG_INTERVAL), Some(2));
    }
}
