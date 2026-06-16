use chrono::{DateTime, TimeDelta, Utc};
use tokio::time::Duration;

use super::{TargetConfirmation, TrackedFlight};

const PENDING_POLL_60_SEC: Duration = Duration::from_secs(60);
const PENDING_POLL_2_MIN: Duration = Duration::from_secs(120);
const PENDING_POLL_5_MIN: Duration = Duration::from_secs(300);
const PENDING_POLL_10_MIN: Duration = Duration::from_secs(600);
const PENDING_POLL_15_MIN: Duration = Duration::from_secs(900);
const PENDING_POLL_30_MIN: Duration = Duration::from_secs(1800);

const PENDING_SCHEDULED_WAKE_BEFORE: i64 = 3 * 60 * 60;
const PENDING_SCHEDULED_LOCK_IN_BEFORE: i64 = 60 * 60;
const PENDING_SCHEDULED_HUNT_BEFORE: i64 = 30 * 60;
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
        starts_at: DateTime<Utc>,
        refresh_at: Option<DateTime<Utc>>,
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
    flight.target_confirmation == TargetConfirmation::Pending
        || (flight.target_confirmation == TargetConfirmation::AircraftVisible
            && flight.last_visible_at.is_none())
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
        starts_at: flight.tracked_at,
        refresh_at: None,
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
        let wake_at = scheduled_departure_at - chrono_seconds(PENDING_SCHEDULED_WAKE_BEFORE);
        let lock_in_at = scheduled_departure_at - chrono_seconds(PENDING_SCHEDULED_LOCK_IN_BEFORE);
        let hunt_at = scheduled_departure_at - chrono_seconds(PENDING_SCHEDULED_HUNT_BEFORE);
        let near_after_ends_at =
            scheduled_departure_at + chrono_seconds(PENDING_SCHEDULED_NEAR_AFTER);
        let mid_after_ends_at =
            scheduled_departure_at + chrono_seconds(PENDING_SCHEDULED_MID_AFTER);
        let (interval, starts_at, refresh_at) =
            if until_departure > chrono_seconds(PENDING_SCHEDULED_WAKE_BEFORE) {
                (PENDING_POLL_15_MIN, wake_at, Some(wake_at))
            } else if until_departure > chrono_seconds(PENDING_SCHEDULED_LOCK_IN_BEFORE) {
                (PENDING_POLL_15_MIN, wake_at, Some(lock_in_at))
            } else if until_departure > chrono_seconds(PENDING_SCHEDULED_HUNT_BEFORE) {
                (PENDING_POLL_5_MIN, lock_in_at, Some(hunt_at))
            } else if now <= scheduled_departure_at {
                (PENDING_POLL_60_SEC, hunt_at, Some(scheduled_departure_at))
            } else if since_departure <= chrono_seconds(PENDING_SCHEDULED_NEAR_AFTER) {
                (
                    PENDING_POLL_2_MIN,
                    scheduled_departure_at,
                    Some(near_after_ends_at),
                )
            } else if since_departure <= chrono_seconds(PENDING_SCHEDULED_MID_AFTER) {
                (
                    PENDING_POLL_5_MIN,
                    near_after_ends_at,
                    Some(mid_after_ends_at),
                )
            } else {
                (PENDING_POLL_15_MIN, mid_after_ends_at, None)
            };

        return PendingPollSchedule::Active {
            interval,
            starts_at,
            refresh_at,
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
                starts_at,
                refresh_at,
                expires_at,
            } => {
                let due_at = next_due_after_last_poll(flight.last_adsb_poll_at, interval, now);
                let mut due_at = due_at.max(starts_at);
                if let Some(refresh_at) = refresh_at
                    && refresh_at > now
                    && refresh_at < due_at
                {
                    due_at = refresh_at;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aviation::tracker::test_support::{dt, tracked_flight};

    #[test]
    fn aircraft_visible_without_target_confirmation_uses_live_polling_after_pending_expiry() {
        let mut flight = tracked_flight();
        flight.target_confirmation = TargetConfirmation::AircraftVisible;
        flight.last_seen = None;
        flight.last_visible_at = Some(dt("2026-04-18T23:58:00Z"));
        flight.last_adsb_poll_at = Some(dt("2026-04-18T23:58:00Z"));

        assert_eq!(
            poll_readiness(&flight, dt("2026-04-19T00:01:00Z")),
            PollReadiness::Due
        );
    }

    #[test]
    fn aircraft_visible_without_visibility_anchor_can_still_expire_as_pending() {
        let mut flight = tracked_flight();
        flight.target_confirmation = TargetConfirmation::AircraftVisible;
        flight.last_seen = None;
        flight.last_visible_at = None;
        flight.last_adsb_poll_at = Some(dt("2026-04-18T23:58:00Z"));

        assert_eq!(
            poll_readiness(&flight, dt("2026-04-19T00:01:00Z")),
            PollReadiness::Expired
        );
    }
}
