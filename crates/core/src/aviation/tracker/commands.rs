use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use tracing::{debug, error, info, warn};
use twitch_irc::{login::LoginCredentials, message::PrivmsgMessage, transport::Transport};

use crate::aviation::{AviationClient, NearbyAircraft};
use crate::twitch::ChatSender;
use crate::util::clock::Clock;

use super::{
    FlightIdentifier, FlightPhase, FlightTrackerState, HexSource, TargetConfirmation,
    TrackedFlight, TrackerCommand, build_flight_view,
    format::{
        msg_adsb_visible, msg_approach, msg_cruise, msg_descent, msg_flight_status,
        msg_flights_list, msg_landing, msg_pending_expired, msg_possible_divert,
        msg_squawk_emergency, msg_takeoff, msg_track_started, msg_tracking_lost,
    },
    metadata::{
        apply_aviationstack_metadata, fetch_aviationstack_metadata_for_tracking,
        set_hex_if_consistent, set_route_from_iata,
    },
    phase::{
        altitude_ft, detect_phase, emergency_squawk_meaning, is_airborne_phase,
        update_divert_counter, vertical_rate,
    },
    schedule::{PollReadiness, is_pending_adsb, poll_readiness},
    state::save_tracker_state,
};

use super::{
    DIVERT_BEARING_THRESHOLD, MAX_FLIGHTS_PER_USER, MAX_TRACKED_FLIGHTS, POLL_TIMEOUT,
    ROUTE_FETCH_TIMEOUT, TRACKING_LOST_REMOVAL, TRACKING_LOST_THRESHOLD,
};

pub(crate) fn find_flight_index(flights: &[TrackedFlight], query: &str) -> Option<usize> {
    let upper = query.to_uppercase();
    flights.iter().position(|f| {
        f.identifier.as_str().eq_ignore_ascii_case(&upper)
            || f.callsign
                .as_ref()
                .is_some_and(|cs| cs.eq_ignore_ascii_case(&upper))
            || f.hex
                .as_ref()
                .is_some_and(|h| h.eq_ignore_ascii_case(&upper))
    })
}

pub(crate) fn aircraft_callsign(ac: &NearbyAircraft) -> Option<&str> {
    ac.flight
        .as_deref()
        .map(str::trim)
        .filter(|callsign| !callsign.is_empty())
}

#[cfg(test)]
pub(crate) fn aircraft_matches_tracked_callsign(
    ac: &NearbyAircraft,
    flight: &TrackedFlight,
) -> bool {
    let expected = match &flight.identifier {
        FlightIdentifier::Callsign(identifier_callsign) => flight
            .callsign
            .as_deref()
            .unwrap_or(identifier_callsign.as_str()),
        FlightIdentifier::Hex(_) => return true,
    };

    aircraft_callsign(ac).is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
}

fn expected_target_callsign(flight: &TrackedFlight) -> Option<&str> {
    match &flight.identifier {
        FlightIdentifier::Callsign(identifier_callsign) => flight
            .callsign
            .as_deref()
            .or(Some(identifier_callsign.as_str())),
        FlightIdentifier::Hex(_) => flight.callsign.as_deref(),
    }
}

fn should_poll_by_hex(flight: &TrackedFlight) -> bool {
    matches!(&flight.identifier, FlightIdentifier::Hex(_))
        || (flight.hex.is_some() && (flight.hex_source.is_some() || flight.last_seen.is_some()))
}

fn inferred_by_hex_window(flight: &TrackedFlight, now: DateTime<Utc>) -> bool {
    if matches!(&flight.identifier, FlightIdentifier::Hex(_)) {
        return true;
    }

    let Some(scheduled_departure_at) = flight.scheduled_departure_at else {
        return false;
    };
    now >= scheduled_departure_at - TimeDelta::minutes(30)
        && now <= scheduled_departure_at + TimeDelta::hours(3)
}

fn tracking_lost_threshold_delta() -> TimeDelta {
    TimeDelta::from_std(TRACKING_LOST_THRESHOLD).unwrap_or_else(|_| TimeDelta::zero())
}

fn should_keep_prior_confirmation(
    flight: &TrackedFlight,
    confirmation: TargetConfirmation,
    used_hex: bool,
    now: DateTime<Utc>,
) -> bool {
    used_hex
        && flight.target_confirmation.is_target_confirmed()
        && confirmation == TargetConfirmation::AircraftVisible
        && flight.last_seen.is_some_and(|last_seen| {
            now.signed_duration_since(last_seen) <= tracking_lost_threshold_delta()
        })
}

fn target_confirmation_for_aircraft(
    flight: &TrackedFlight,
    ac: &NearbyAircraft,
    used_hex: bool,
    now: DateTime<Utc>,
) -> Option<TargetConfirmation> {
    if let Some(expected) = expected_target_callsign(flight)
        && aircraft_callsign(ac).is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
    {
        return Some(TargetConfirmation::ConfirmedByCallsign);
    }

    if !used_hex {
        return None;
    }

    if matches!(&flight.identifier, FlightIdentifier::Hex(_)) || aircraft_callsign(ac).is_none() {
        if inferred_by_hex_window(flight, now) {
            Some(TargetConfirmation::InferredByAssignedHex)
        } else {
            Some(TargetConfirmation::AircraftVisible)
        }
    } else {
        Some(TargetConfirmation::AircraftVisible)
    }
}

fn should_skip_adsb_for_strict_chill(flight: &TrackedFlight, now: DateTime<Utc>) -> bool {
    let Some(scheduled_departure_at) = flight.scheduled_departure_at else {
        return false;
    };
    scheduled_departure_at.signed_duration_since(now) > TimeDelta::hours(3)
}

fn duplicate_tracking_exists(
    state: &FlightTrackerState,
    identifier: &FlightIdentifier,
    callsign: Option<&str>,
    hex: Option<&str>,
) -> bool {
    state.flights.iter().any(|f| {
        &f.identifier == identifier
            || identifier.matches(f.callsign.as_deref(), f.hex.as_deref())
            || callsign.is_some_and(|callsign| {
                f.callsign
                    .as_deref()
                    .is_some_and(|existing| existing.eq_ignore_ascii_case(callsign))
            })
            || hex.is_some_and(|hex| {
                f.hex
                    .as_deref()
                    .is_some_and(|existing| existing.eq_ignore_ascii_case(hex))
            })
    })
}

pub(crate) async fn remove_flight_at(
    state: &mut FlightTrackerState,
    query: &str,
    data_dir: &Path,
    source: &str,
) -> Option<String> {
    let idx = find_flight_index(&state.flights, query)?;
    let flight = &state.flights[idx];
    let label = flight
        .callsign
        .as_deref()
        .unwrap_or(flight.identifier.as_str())
        .to_owned();
    state.flights.remove(idx);
    save_tracker_state(data_dir, state).await;
    info!(identifier = %query, source = %source, "Flight untracked");
    Some(label)
}

pub(crate) async fn process_command<T, L>(
    cmd: TrackerCommand,
    state: &mut FlightTrackerState,
    sender: &Arc<ChatSender<T, L>>,
    aviation_client: &AviationClient,
    data_dir: &Path,
    clock: &dyn Clock,
) where
    T: Transport,
    L: LoginCredentials,
{
    match cmd {
        TrackerCommand::Track {
            identifier,
            requested_by,
            reply_to,
        } => {
            handle_track(
                identifier,
                &requested_by,
                &reply_to,
                state,
                sender,
                aviation_client,
                data_dir,
                clock,
            )
            .await;
        }
        TrackerCommand::Untrack {
            identifier,
            requested_by,
            is_mod,
            reply_to,
        } => {
            handle_untrack(
                &identifier,
                &requested_by,
                is_mod,
                &reply_to,
                state,
                sender,
                data_dir,
            )
            .await;
        }
        TrackerCommand::Status {
            identifier,
            reply_to,
        } => {
            handle_status(identifier.as_deref(), &reply_to, state, sender, clock).await;
        }
        TrackerCommand::Snapshot { reply } => {
            let now = clock.now_utc();
            let _ = reply.send(build_flight_view(state, now));
        }
        TrackerCommand::DeleteFromWeb { identifier, reply } => {
            let removed = remove_flight_at(state, &identifier, data_dir, "web").await;
            let _ = reply.send(removed);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_track<T, L>(
    identifier: FlightIdentifier,
    requested_by: &str,
    reply_to: &PrivmsgMessage,
    state: &mut FlightTrackerState,
    sender: &Arc<ChatSender<T, L>>,
    aviation_client: &AviationClient,
    data_dir: &Path,
    clock: &dyn Clock,
) where
    T: Transport,
    L: LoginCredentials,
{
    if state.flights.len() >= MAX_TRACKED_FLIGHTS {
        sender
            .reply(
                reply_to,
                format!("Maximal {MAX_TRACKED_FLIGHTS} Flüge gleichzeitig FDM"),
            )
            .await;
        return;
    }

    let user_count = state
        .flights
        .iter()
        .filter(|f| f.tracked_by == requested_by)
        .count();
    if user_count >= MAX_FLIGHTS_PER_USER {
        sender
            .reply(
                reply_to,
                format!("Du trackst schon {MAX_FLIGHTS_PER_USER} Flüge FDM"),
            )
            .await;
        return;
    }

    if duplicate_tracking_exists(state, &identifier, None, None) {
        sender
            .reply(reply_to, format!("{identifier} wird schon getrackt FDM"))
            .await;
        return;
    }

    let resolved_callsign = match &identifier {
        FlightIdentifier::Callsign(cs) => Some(aviation_client.resolve_callsign(cs).await),
        FlightIdentifier::Hex(_) => None,
    };

    if let Some(resolved) = &resolved_callsign
        && !resolved.eq_ignore_ascii_case(identifier.as_str())
        && duplicate_tracking_exists(state, &identifier, Some(resolved), None)
    {
        sender
            .reply(reply_to, format!("{identifier} wird schon getrackt FDM"))
            .await;
        return;
    }

    let mut aviationstack_checked = false;
    let mut metadata = None;
    let now = clock.now_utc();
    if matches!(&identifier, FlightIdentifier::Callsign(_))
        && aviation_client.aviationstack_enabled()
    {
        aviationstack_checked = true;
        metadata = fetch_aviationstack_metadata_for_tracking(
            aviation_client,
            &identifier,
            resolved_callsign.as_deref(),
        )
        .await;
    }
    let aviationstack_fallback = aviationstack_checked
        && metadata.is_none()
        && matches!(&identifier, FlightIdentifier::Callsign(_));
    if aviationstack_fallback {
        warn!(identifier = %identifier, "AviationStack did not resolve flight; falling back to ADS-B-only tracking");
    }

    let initial_hex = match &identifier {
        FlightIdentifier::Hex(hex) => Some(hex.clone()),
        FlightIdentifier::Callsign(_) => None,
    };
    let mut flight = TrackedFlight {
        identifier: identifier.clone(),
        callsign: resolved_callsign.clone(),
        hex: initial_hex,
        hex_source: matches!(&identifier, FlightIdentifier::Hex(_)).then_some(HexSource::UserInput),
        observed_callsign: None,
        target_confirmation: TargetConfirmation::Pending,
        phase: FlightPhase::Unknown,
        route: None,
        aircraft_type: None,
        altitude_ft: None,
        vertical_rate_fpm: None,
        ground_speed_kts: None,
        lat: None,
        lon: None,
        squawk: None,
        tracked_by: requested_by.to_string(),
        tracked_at: now,
        last_seen: None,
        last_phase_change: None,
        polls_since_change: 0,
        takeoff_at: None,
        aviationstack_checked,
        scheduled_departure_at: None,
        last_adsb_poll_at: None,
        divert_consecutive_polls: 0,
        dest_lat: None,
        dest_lon: None,
    };

    if let Some(metadata) = metadata {
        apply_aviationstack_metadata(&mut flight, metadata);
    }
    if duplicate_tracking_exists(
        state,
        &identifier,
        flight.callsign.as_deref(),
        flight.hex.as_deref(),
    ) {
        sender
            .reply(reply_to, format!("{identifier} wird schon getrackt FDM"))
            .await;
        return;
    }

    let keep_pending_on_adsb_absence = matches!(&identifier, FlightIdentifier::Callsign(_))
        && aviation_client.aviationstack_enabled();
    if !should_skip_adsb_for_strict_chill(&flight, now) {
        type PollResult =
            Result<Result<Option<NearbyAircraft>, eyre::Report>, tokio::time::error::Elapsed>;

        let mut used_hex = should_poll_by_hex(&flight);
        let ac_result: Option<PollResult> = if used_hex {
            if let Some(hex) = flight.hex.clone() {
                Some(
                    tokio::time::timeout(POLL_TIMEOUT, aviation_client.get_aircraft_by_hex(&hex))
                        .await,
                )
            } else {
                used_hex = false;
                None
            }
        } else {
            let lookup_callsign = flight.callsign.as_deref().or(resolved_callsign.as_deref());
            if let Some(callsign) = lookup_callsign {
                Some(
                    tokio::time::timeout(
                        POLL_TIMEOUT,
                        aviation_client.get_aircraft_by_callsign(callsign),
                    )
                    .await,
                )
            } else {
                None
            }
        };

        if let Some(ac_result) = ac_result {
            flight.last_adsb_poll_at = Some(now);
            match ac_result {
                Ok(Ok(Some(ac))) => {
                    if used_hex && flight.hex.is_none() {
                        used_hex = false;
                    }

                    let Some(confirmation) =
                        target_confirmation_for_aircraft(&flight, &ac, used_hex, now)
                    else {
                        if !keep_pending_on_adsb_absence {
                            sender
                                .reply(
                                    reply_to,
                                    format!("{identifier} nicht gefunden im ADS-B FDM"),
                                )
                                .await;
                            return;
                        }
                        debug!(
                            identifier = %identifier,
                            aircraft_callsign = aircraft_callsign(&ac).unwrap_or("<missing>"),
                            "Ignoring ADS-B aircraft that does not confirm target"
                        );
                        flight.polls_since_change = flight.polls_since_change.saturating_add(1);
                        // Keep the pending track below.
                        let route_callsign = flight.callsign.clone();
                        if flight.route.is_none()
                            && let Some(cs) = route_callsign.as_deref()
                            && let Ok(Ok(Some(route))) = tokio::time::timeout(
                                ROUTE_FETCH_TIMEOUT,
                                aviation_client.get_flight_route(cs),
                            )
                            .await
                        {
                            let origin = route.origin.iata_code.clone();
                            let dest = route.destination.iata_code.clone();
                            set_route_from_iata(&mut flight, &origin, &dest);
                        }
                        let mut response = msg_track_started(&flight);
                        if aviationstack_fallback {
                            response.push_str(" | AviationStack nix, ADS-B-only");
                        }
                        state.flights.push(flight);
                        save_tracker_state(data_dir, state).await;
                        info!(
                            identifier = %identifier,
                            requested_by = %requested_by,
                            "Flight tracking started"
                        );
                        sender.reply(reply_to, response).await;
                        return;
                    };

                    let target_confirmed = confirmation.is_target_confirmed();
                    flight.target_confirmation = confirmation;
                    flight.observed_callsign =
                        aircraft_callsign(&ac).map(std::string::ToString::to_string);
                    if (confirmation == TargetConfirmation::ConfirmedByCallsign
                        || matches!(&flight.identifier, FlightIdentifier::Hex(_)))
                        && flight.callsign.is_none()
                        && flight.observed_callsign.is_some()
                    {
                        flight.callsign.clone_from(&flight.observed_callsign);
                    }
                    if let Some(hex) = ac.hex.as_deref() {
                        set_hex_if_consistent(&mut flight, hex, HexSource::Adsb, target_confirmed);
                    }
                    flight.aircraft_type = ac.t.clone();
                    flight.altitude_ft = altitude_ft(&ac);
                    flight.vertical_rate_fpm = vertical_rate(&ac);
                    flight.ground_speed_kts = ac.gs;
                    flight.lat = ac.lat;
                    flight.lon = ac.lon;
                    flight.squawk = ac.squawk.clone();
                    if target_confirmed {
                        flight.last_seen = Some(now);
                        flight.phase = detect_phase(&flight, &ac);
                    }
                }
                Ok(Ok(None)) => {
                    if !keep_pending_on_adsb_absence {
                        sender
                            .reply(
                                reply_to,
                                format!("{identifier} nicht gefunden im ADS-B FDM"),
                            )
                            .await;
                        return;
                    }
                }
                Ok(Err(e)) => {
                    error!(error = ?e, identifier = %identifier, "ADS-B lookup failed");
                    if !keep_pending_on_adsb_absence {
                        sender
                            .reply(reply_to, "ADS-B Anfrage fehlgeschlagen FDM")
                            .await;
                        return;
                    }
                }
                Err(_) => {
                    if !keep_pending_on_adsb_absence {
                        sender.reply(reply_to, "ADS-B Anfrage Timeout FDM").await;
                        return;
                    }
                    warn!(
                        identifier = %identifier,
                        "Initial ADS-B lookup timed out; keeping pending track"
                    );
                }
            }
        }
    }

    if aviation_client.aviationstack_enabled()
        && !flight.aviationstack_checked
        && flight.callsign.is_some()
    {
        flight.aviationstack_checked = true;
        if let Some(metadata) = fetch_aviationstack_metadata_for_tracking(
            aviation_client,
            &identifier,
            flight.callsign.as_deref(),
        )
        .await
        {
            apply_aviationstack_metadata(&mut flight, metadata);
        }
    }

    if duplicate_tracking_exists(
        state,
        &identifier,
        flight.callsign.as_deref(),
        flight.hex.as_deref(),
    ) {
        sender
            .reply(reply_to, format!("{identifier} wird schon getrackt FDM"))
            .await;
        return;
    }

    let route_callsign = flight.callsign.clone();
    if flight.route.is_none()
        && let Some(cs) = route_callsign.as_deref()
    {
        match tokio::time::timeout(ROUTE_FETCH_TIMEOUT, aviation_client.get_flight_route(cs)).await
        {
            Ok(Ok(Some(route))) => {
                let origin = route.origin.iata_code.clone();
                let dest = route.destination.iata_code.clone();
                set_route_from_iata(&mut flight, &origin, &dest);
            }
            Ok(Ok(None)) => {
                debug!(callsign = %cs, "No route found for flight");
            }
            Ok(Err(e)) => {
                warn!(error = ?e, callsign = %cs, "Failed to fetch route");
            }
            Err(_) => {
                warn!(callsign = %cs, "Route fetch timed out");
            }
        }
    }

    let mut response = msg_track_started(&flight);
    if aviationstack_fallback {
        response.push_str(" | AviationStack nix, ADS-B-only");
    }
    state.flights.push(flight);
    save_tracker_state(data_dir, state).await;

    info!(identifier = %identifier, requested_by = %requested_by, "Flight tracking started");
    sender.reply(reply_to, response).await;
}

async fn handle_untrack<T, L>(
    identifier: &str,
    requested_by: &str,
    is_mod: bool,
    reply_to: &PrivmsgMessage,
    state: &mut FlightTrackerState,
    sender: &Arc<ChatSender<T, L>>,
    data_dir: &Path,
) where
    T: Transport,
    L: LoginCredentials,
{
    let Some(idx) = find_flight_index(&state.flights, identifier) else {
        sender
            .reply(reply_to, format!("{identifier} nicht gefunden FDM"))
            .await;
        return;
    };

    let flight = &state.flights[idx];
    if flight.tracked_by != requested_by && !is_mod {
        sender
            .reply(
                reply_to,
                "Nur der Tracker oder Mods können das untracking machen FDM",
            )
            .await;
        return;
    }

    let name = remove_flight_at(state, identifier, data_dir, requested_by)
        .await
        .unwrap_or_else(|| identifier.to_owned());

    sender
        .reply(reply_to, format!("{name} wird nicht mehr getrackt Okayge"))
        .await;
}

async fn handle_status<T, L>(
    identifier: Option<&str>,
    reply_to: &PrivmsgMessage,
    state: &FlightTrackerState,
    sender: &Arc<ChatSender<T, L>>,
    clock: &dyn Clock,
) where
    T: Transport,
    L: LoginCredentials,
{
    let response = match identifier {
        None => msg_flights_list(&state.flights),
        Some(id) => match find_flight_index(&state.flights, id) {
            Some(idx) => msg_flight_status(&state.flights[idx], clock.now_utc()),
            None => format!("{id} nicht gefunden FDM"),
        },
    };

    sender.reply(reply_to, response).await;
}

pub(crate) async fn poll_all_flights<T, L>(
    state: &mut FlightTrackerState,
    sender: &Arc<ChatSender<T, L>>,
    channel: &str,
    aviation_client: &AviationClient,
    data_dir: &Path,
    clock: &dyn Clock,
) where
    T: Transport,
    L: LoginCredentials,
{
    let now = clock.now_utc();
    let mut changed = false;
    let mut removals: Vec<usize> = Vec::new();
    let mut messages: Vec<String> = Vec::new();

    type PollResult =
        Result<Result<Option<NearbyAircraft>, eyre::Report>, tokio::time::error::Elapsed>;
    type PollAttempt = (bool, PollResult);

    let mut join_set = tokio::task::JoinSet::new();
    let mut fetch_results: Vec<Option<PollAttempt>> =
        (0..state.flights.len()).map(|_| None).collect();

    for (idx, flight) in state.flights.iter().enumerate() {
        match poll_readiness(flight, now) {
            PollReadiness::Due => {}
            PollReadiness::NotDue(_) => continue,
            PollReadiness::Expired => {
                info!(
                    identifier = %flight.identifier,
                    "Removing pending flight: ADS-B never appeared"
                );
                messages.push(msg_pending_expired(flight));
                removals.push(idx);
                changed = true;
                continue;
            }
        }

        let ac = aviation_client.clone();
        let id = flight.identifier.clone();
        let hex = flight.hex.clone();
        let callsign = flight.callsign.clone();
        let poll_by_hex = should_poll_by_hex(flight);
        join_set.spawn(async move {
            let (used_hex, result): PollAttempt = if poll_by_hex {
                let lookup_hex = hex.as_deref().unwrap_or_else(|| id.as_str());
                (
                    true,
                    tokio::time::timeout(POLL_TIMEOUT, ac.get_aircraft_by_hex(lookup_hex)).await,
                )
            } else {
                let lookup_callsign = callsign.as_deref().unwrap_or_else(|| id.as_str());
                (
                    false,
                    tokio::time::timeout(
                        POLL_TIMEOUT,
                        ac.get_aircraft_by_callsign(lookup_callsign),
                    )
                    .await,
                )
            };
            (idx, used_hex, result)
        });
    }

    while let Some(res) = join_set.join_next().await {
        if let Ok((idx, used_hex, poll_result)) = res {
            fetch_results[idx] = Some((used_hex, poll_result));
        }
    }

    let removal_threshold =
        chrono::TimeDelta::from_std(TRACKING_LOST_REMOVAL).unwrap_or(chrono::TimeDelta::zero());
    let lost_threshold =
        chrono::TimeDelta::from_std(TRACKING_LOST_THRESHOLD).unwrap_or(chrono::TimeDelta::zero());

    #[allow(clippy::needless_range_loop)]
    for idx in 0..state.flights.len() {
        let Some((used_hex, ac_result)) = fetch_results[idx].take() else {
            continue;
        };
        let flight = &mut state.flights[idx];
        let was_pending = is_pending_adsb(flight);
        let was_target_confirmed = flight.target_confirmation.is_target_confirmed();
        flight.last_adsb_poll_at = Some(now);
        changed = true;

        let ac = match ac_result {
            Ok(Ok(Some(ac))) => ac,
            Ok(Ok(None)) => {
                if flight.last_seen.is_none() {
                    flight.polls_since_change = flight.polls_since_change.saturating_add(1);
                } else if let Some(last_seen) = flight.last_seen {
                    let lost_duration = now.signed_duration_since(last_seen);
                    if lost_duration >= removal_threshold {
                        info!(
                            identifier = %flight.identifier,
                            "Removing flight: tracking lost for {}s",
                            lost_duration.num_seconds()
                        );
                        messages.push(msg_tracking_lost(flight));
                        removals.push(idx);
                    } else if lost_duration >= lost_threshold {
                        debug!(
                            identifier = %flight.identifier,
                            last_seen_secs_ago = lost_duration.num_seconds(),
                            "Flight not visible via ADS-B"
                        );
                    }
                }
                continue;
            }
            Ok(Err(e)) => {
                warn!(error = ?e, identifier = %flight.identifier, "ADS-B poll failed");
                continue;
            }
            Err(_) => {
                warn!(identifier = %flight.identifier, "ADS-B poll timed out");
                continue;
            }
        };

        let Some(raw_confirmation) = target_confirmation_for_aircraft(flight, &ac, used_hex, now)
        else {
            debug!(
                identifier = %flight.identifier,
                aircraft_callsign = aircraft_callsign(&ac).unwrap_or("<missing>"),
                "Ignoring ADS-B aircraft that does not confirm target"
            );
            continue;
        };

        let direct_target_confirmed = raw_confirmation.is_target_confirmed();
        let sticky_confirmation =
            should_keep_prior_confirmation(flight, raw_confirmation, used_hex, now);
        let confirmation = if sticky_confirmation {
            flight.target_confirmation
        } else {
            raw_confirmation
        };
        let phase_sample_confirmed =
            direct_target_confirmed || (sticky_confirmation && aircraft_callsign(&ac).is_none());
        let became_target_confirmed = !was_target_confirmed && direct_target_confirmed;

        if !direct_target_confirmed && let Some(last_seen) = flight.last_seen {
            let lost_duration = now.signed_duration_since(last_seen);
            if lost_duration >= removal_threshold {
                info!(
                    identifier = %flight.identifier,
                    "Removing flight: tracking lost for {}s",
                    lost_duration.num_seconds()
                );
                messages.push(msg_tracking_lost(flight));
                removals.push(idx);
                continue;
            } else if lost_duration >= lost_threshold {
                debug!(
                    identifier = %flight.identifier,
                    last_seen_secs_ago = lost_duration.num_seconds(),
                    "Flight not confirmed via ADS-B"
                );
            }
        }

        if direct_target_confirmed {
            flight.last_seen = Some(now);
        }
        flight.target_confirmation = confirmation;
        flight.observed_callsign = aircraft_callsign(&ac).map(std::string::ToString::to_string);

        if (confirmation == TargetConfirmation::ConfirmedByCallsign
            || matches!(&flight.identifier, FlightIdentifier::Hex(_)))
            && flight.callsign.is_none()
            && let Some(cs) = flight.observed_callsign.clone()
        {
            debug!(identifier = %flight.identifier, callsign = %cs, "Resolved callsign");
            flight.callsign = Some(cs.clone());
            changed = true;

            if flight.route.is_none()
                && let Ok(Ok(Some(route))) =
                    tokio::time::timeout(ROUTE_FETCH_TIMEOUT, aviation_client.get_flight_route(&cs))
                        .await
            {
                let origin = route.origin.iata_code.clone();
                let dest = route.destination.iata_code.clone();
                set_route_from_iata(flight, &origin, &dest);
            }
        }
        if let Some(hex) = ac.hex.as_deref()
            && set_hex_if_consistent(flight, hex, HexSource::Adsb, direct_target_confirmed)
        {
            debug!(identifier = %flight.identifier, hex = %hex, "Resolved hex");
        }
        if flight.aircraft_type.is_none()
            && let Some(t) = &ac.t
        {
            flight.aircraft_type = Some(t.clone());
        }

        if let Some(new_squawk) = &ac.squawk {
            let squawk_changed = flight.squawk.as_ref() != Some(new_squawk);
            if phase_sample_confirmed
                && squawk_changed
                && let Some(meaning) = emergency_squawk_meaning(new_squawk)
            {
                messages.push(msg_squawk_emergency(flight, new_squawk, meaning));
            }
        }

        let prev_lat = flight.lat;
        let prev_lon = flight.lon;

        flight.altitude_ft = altitude_ft(&ac);
        flight.vertical_rate_fpm = vertical_rate(&ac);
        flight.ground_speed_kts = ac.gs;
        flight.lat = ac.lat;
        flight.lon = ac.lon;
        flight.squawk = ac.squawk.clone();

        if became_target_confirmed {
            messages.push(msg_adsb_visible(flight));
            if was_pending {
                flight.phase = FlightPhase::Unknown;
                flight.last_phase_change = None;
                flight.polls_since_change = 0;
            }
        }

        if phase_sample_confirmed {
            let new_phase = detect_phase(flight, &ac);
            let old_phase = flight.phase;

            if new_phase != old_phase {
                flight.phase = new_phase;
                flight.last_phase_change = Some(now);
                flight.polls_since_change = 0;
                changed = true;

                if old_phase == FlightPhase::Ground
                    && is_airborne_phase(new_phase)
                    && flight.takeoff_at.is_none()
                {
                    flight.takeoff_at = Some(now);
                }

                match new_phase {
                    FlightPhase::Takeoff => messages.push(msg_takeoff(flight)),
                    FlightPhase::Cruise => messages.push(msg_cruise(flight)),
                    FlightPhase::Descent => messages.push(msg_descent(flight)),
                    FlightPhase::Approach => messages.push(msg_approach(flight)),
                    FlightPhase::Landing => messages.push(msg_landing(flight, now)),
                    _ => {}
                }

                if new_phase == FlightPhase::Landing {
                    flight.phase = FlightPhase::Ground;
                    flight.takeoff_at = None;
                }
            } else {
                flight.polls_since_change += 1;
            }
        }

        if phase_sample_confirmed
            && matches!(flight.phase, FlightPhase::Descent | FlightPhase::Approach)
        {
            if let (
                Some(dest_lat),
                Some(dest_lon),
                Some(cur_lat),
                Some(cur_lon),
                Some(p_lat),
                Some(p_lon),
            ) = (
                flight.dest_lat,
                flight.dest_lon,
                flight.lat,
                flight.lon,
                prev_lat,
                prev_lon,
            ) {
                let ground_track =
                    random_flight::geo::initial_bearing(p_lat, p_lon, cur_lat, cur_lon);
                let bearing_to_dest =
                    random_flight::geo::initial_bearing(cur_lat, cur_lon, dest_lat, dest_lon);

                let mut diff = (ground_track - bearing_to_dest).abs();
                if diff > 180.0 {
                    diff = 360.0 - diff;
                }

                if update_divert_counter(
                    &mut flight.divert_consecutive_polls,
                    diff > DIVERT_BEARING_THRESHOLD,
                ) {
                    messages.push(msg_possible_divert(flight));
                }
            }
        } else {
            flight.divert_consecutive_polls = 0;
        }
    }

    for idx in removals.into_iter().rev() {
        state.flights.remove(idx);
        changed = true;
    }

    for msg in messages {
        sender.say(channel.to_string(), msg).await;
    }

    if changed {
        save_tracker_state(data_dir, state).await;
    }
}
