use chrono::{DateTime, TimeDelta, Utc};
use tokio::time::Duration;

use super::TrackedFlight;

const PENDING_POLL_2_MIN: Duration = Duration::from_secs(120);
const PENDING_POLL_5_MIN: Duration = Duration::from_secs(300);
const PENDING_POLL_10_MIN: Duration = Duration::from_secs(600);
const PENDING_POLL_15_MIN: Duration = Duration::from_secs(900);
const PENDING_POLL_30_MIN: Duration = Duration::from_secs(1800);

const PENDING_SCHEDULED_FAR_BEFORE: i64 = 6 * 60 * 60;
const PENDING_SCHEDULED_NEAR_BEFORE: i64 = 90 * 60;
const PENDING_SCHEDULED_NEAR_AFTER: i64 = 45 * 60;
const PENDING_SCHEDULED_MID_AFTER: i64 = 3 * 60 * 60;
const PENDING_SCHEDULED_EXPIRE_AFTER: i64 = 12 * 60 * 60;
const PENDING_UNKNOWN_FAST_AFTER_TRACK: i64 = 10 * 60;
const PENDING_UNKNOWN_NORMAL_AFTER_TRACK: i64 = 6 * 60 * 60;
const PENDING_UNKNOWN_EXPIRE_AFTER: i64 = 24 * 60 * 60;

use super::{FlightPhase, POLL_FAST, POLL_NORMAL, POLL_SLOW};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingPollSchedule {
    Active {
        interval: Duration,
        expires_at: DateTime<Utc>,
    },
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollReadiness {
    Due,
    NotDue(DateTime<Utc>),
    Expired,
}

fn chrono_seconds(seconds: i64) -> TimeDelta {
    TimeDelta::seconds(seconds)
}

fn duration_to_chrono(duration: Duration) -> TimeDelta {
    TimeDelta::from_std(duration).unwrap_or_else(|_| TimeDelta::zero())
}

fn add_duration(time: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    time + duration_to_chrono(duration)
}

pub(crate) fn is_pending_adsb(flight: &TrackedFlight) -> bool {
    flight.last_seen.is_none()
}

pub(crate) fn live_poll_interval(flight: &TrackedFlight) -> Duration {
    if flight.polls_since_change < 5
        || matches!(
            flight.phase,
            FlightPhase::Takeoff | FlightPhase::Approach | FlightPhase::Landing
        )
    {
        POLL_FAST
    } else if matches!(flight.phase, FlightPhase::Climb | FlightPhase::Descent) {
        POLL_NORMAL
    } else {
        POLL_SLOW
    }
}

fn pending_unknown_poll_schedule(
    flight: &TrackedFlight,
    now: DateTime<Utc>,
) -> PendingPollSchedule {
    let expires_at = flight.tracked_at + chrono_seconds(PENDING_UNKNOWN_EXPIRE_AFTER);
    if now >= expires_at {
        return PendingPollSchedule::Expired;
    }

    let age = now.signed_duration_since(flight.tracked_at);
    let interval = if age < chrono_seconds(PENDING_UNKNOWN_FAST_AFTER_TRACK) {
        PENDING_POLL_2_MIN
    } else if age < chrono_seconds(PENDING_UNKNOWN_NORMAL_AFTER_TRACK) {
        PENDING_POLL_10_MIN
    } else {
        PENDING_POLL_30_MIN
    };

    PendingPollSchedule::Active {
        interval,
        expires_at,
    }
}

pub(crate) fn pending_poll_schedule(
    flight: &TrackedFlight,
    now: DateTime<Utc>,
) -> PendingPollSchedule {
    if let Some(scheduled_departure_at) = flight.scheduled_departure_at {
        let expires_at = scheduled_departure_at + chrono_seconds(PENDING_SCHEDULED_EXPIRE_AFTER);
        if flight.tracked_at >= expires_at {
            return pending_unknown_poll_schedule(flight, now);
        }
        if now >= expires_at {
            return PendingPollSchedule::Expired;
        }

        let until_departure = scheduled_departure_at.signed_duration_since(now);
        let since_departure = now.signed_duration_since(scheduled_departure_at);
        let interval = if until_departure > chrono_seconds(PENDING_SCHEDULED_FAR_BEFORE) {
            PENDING_POLL_30_MIN
        } else if until_departure > chrono_seconds(PENDING_SCHEDULED_NEAR_BEFORE) {
            PENDING_POLL_15_MIN
        } else if since_departure <= chrono_seconds(PENDING_SCHEDULED_NEAR_AFTER) {
            PENDING_POLL_2_MIN
        } else if since_departure <= chrono_seconds(PENDING_SCHEDULED_MID_AFTER) {
            PENDING_POLL_5_MIN
        } else {
            PENDING_POLL_15_MIN
        };

        return PendingPollSchedule::Active {
            interval,
            expires_at,
        };
    }

    pending_unknown_poll_schedule(flight, now)
}

pub(crate) fn next_due_after_last_poll(
    last_adsb_poll_at: Option<DateTime<Utc>>,
    interval: Duration,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    last_adsb_poll_at
        .map(|last_poll| add_duration(last_poll, interval))
        .unwrap_or(now)
}

pub(crate) fn poll_readiness(flight: &TrackedFlight, now: DateTime<Utc>) -> PollReadiness {
    let next_due = if is_pending_adsb(flight) {
        match pending_poll_schedule(flight, now) {
            PendingPollSchedule::Expired => return PollReadiness::Expired,
            PendingPollSchedule::Active {
                interval,
                expires_at,
            } => {
                let due_at = next_due_after_last_poll(flight.last_adsb_poll_at, interval, now);
                if due_at <= expires_at {
                    due_at
                } else {
                    expires_at
                }
            }
        }
    } else {
        next_due_after_last_poll(flight.last_adsb_poll_at, live_poll_interval(flight), now)
    };

    if next_due <= now {
        PollReadiness::Due
    } else {
        PollReadiness::NotDue(next_due)
    }
}

pub(crate) fn next_poll_at(flights: &[TrackedFlight], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    flights
        .iter()
        .map(|flight| match poll_readiness(flight, now) {
            PollReadiness::Due | PollReadiness::Expired => now,
            PollReadiness::NotDue(next_at) => next_at,
        })
        .min()
}

#[allow(dead_code)]
pub(crate) fn compute_poll_interval(flights: &[TrackedFlight]) -> Duration {
    if flights.is_empty() || flights.iter().all(is_pending_adsb) {
        return POLL_SLOW;
    }

    let needs_fast = flights.iter().filter(|f| !is_pending_adsb(f)).any(|f| {
        f.polls_since_change < 5
            || matches!(
                f.phase,
                FlightPhase::Takeoff | FlightPhase::Approach | FlightPhase::Landing
            )
    });

    if needs_fast {
        return POLL_FAST;
    }

    let needs_normal = flights
        .iter()
        .filter(|f| !is_pending_adsb(f))
        .any(|f| matches!(f.phase, FlightPhase::Climb | FlightPhase::Descent));

    if needs_normal {
        return POLL_NORMAL;
    }

    POLL_SLOW
}
