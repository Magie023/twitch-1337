use std::path::Path;
use std::sync::Arc;

use tracing::{debug, error, info, warn};
use twitch_irc::{login::LoginCredentials, message::PrivmsgMessage, transport::Transport};

use crate::aviation::{AviationClient, NearbyAircraft, iata_to_coords};
use crate::twitch::ChatSender;
use crate::util::clock::Clock;

use super::{
    FlightIdentifier, FlightPhase, FlightTrackerState, TrackedFlight, TrackerCommand,
    build_flight_view,
    format::{
        msg_adsb_visible, msg_approach, msg_cruise, msg_descent, msg_flight_status,
        msg_flights_list, msg_landing, msg_pending_expired, msg_possible_divert,
        msg_squawk_emergency, msg_takeoff, msg_track_started, msg_tracking_lost,
    },
    metadata::{
        apply_aviationstack_metadata, fetch_aviationstack_metadata_for_tracking, metadata_callsign,
        set_route_from_iata,
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

    let already_tracked = state.flights.iter().any(|f| {
        f.identifier == identifier || identifier.matches(f.callsign.as_deref(), f.hex.as_deref())
    });
    if already_tracked {
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
    {
        let already_tracked = state.flights.iter().any(|f| {
            f.identifier.as_str().eq_ignore_ascii_case(resolved)
                || f.callsign
                    .as_ref()
                    .is_some_and(|cs| cs.eq_ignore_ascii_case(resolved))
        });
        if already_tracked {
            sender
                .reply(reply_to, format!("{identifier} wird schon getrackt FDM"))
                .await;
            return;
        }
    }

    let ac_result = match &identifier {
        FlightIdentifier::Hex(hex) => {
            tokio::time::timeout(POLL_TIMEOUT, aviation_client.get_aircraft_by_hex(hex)).await
        }
        FlightIdentifier::Callsign(_) => {
            let resolved = resolved_callsign
                .as_deref()
                .expect("callsign identifiers have a resolved callsign");
            tokio::time::timeout(
                POLL_TIMEOUT,
                aviation_client.get_aircraft_by_callsign(resolved),
            )
            .await
        }
    };

    let mut aviationstack_checked = false;
    let mut metadata = None;
    let ac = match ac_result {
        Ok(Ok(Some(ac))) => Some(ac),
        Ok(Ok(None)) => {
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

            if metadata.is_none() {
                sender
                    .reply(
                        reply_to,
                        format!("{identifier} nicht gefunden im ADS-B FDM"),
                    )
                    .await;
                return;
            }
            None
        }
        Ok(Err(e)) => {
            error!(error = ?e, identifier = %identifier, "ADS-B lookup failed");
            sender
                .reply(reply_to, "ADS-B Anfrage fehlgeschlagen FDM")
                .await;
            return;
        }
        Err(_) => {
            sender.reply(reply_to, "ADS-B Anfrage Timeout FDM").await;
            return;
        }
    };

    let callsign = ac
        .as_ref()
        .and_then(|ac| {
            ac.flight
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| metadata.as_ref().and_then(metadata_callsign))
        .or_else(|| resolved_callsign.clone());
    let hex = ac.as_ref().and_then(|ac| ac.hex.clone());
    let aircraft_type = ac.as_ref().and_then(|ac| ac.t.clone());
    let now = clock.now_utc();

    let mut flight = TrackedFlight {
        identifier: identifier.clone(),
        callsign: callsign.clone(),
        hex,
        phase: FlightPhase::Unknown,
        route: None,
        aircraft_type,
        altitude_ft: ac.as_ref().and_then(altitude_ft),
        vertical_rate_fpm: ac.as_ref().and_then(vertical_rate),
        ground_speed_kts: ac.as_ref().and_then(|ac| ac.gs),
        lat: ac.as_ref().and_then(|ac| ac.lat),
        lon: ac.as_ref().and_then(|ac| ac.lon),
        squawk: ac.as_ref().and_then(|ac| ac.squawk.clone()),
        tracked_by: requested_by.to_string(),
        tracked_at: now,
        last_seen: ac.as_ref().map(|_| now),
        last_phase_change: None,
        polls_since_change: 0,
        takeoff_at: None,
        aviationstack_checked,
        scheduled_departure_at: None,
        last_adsb_poll_at: Some(now),
        divert_consecutive_polls: 0,
        dest_lat: None,
        dest_lon: None,
    };

    if let Some(ac) = &ac {
        flight.phase = detect_phase(&flight, ac);
    }

    if aviation_client.aviationstack_enabled() && !aviationstack_checked {
        aviationstack_checked = true;
        metadata = fetch_aviationstack_metadata_for_tracking(
            aviation_client,
            &identifier,
            callsign.as_deref(),
        )
        .await;
    }

    flight.aviationstack_checked = aviationstack_checked;
    if let Some(metadata) = metadata {
        apply_aviationstack_metadata(&mut flight, metadata);
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

    let response = msg_track_started(&flight);
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
        let has_live_adsb_identity = flight.last_seen.is_some();
        join_set.spawn(async move {
            let (used_hex, result): PollAttempt = match &id {
                FlightIdentifier::Hex(h) => (
                    true,
                    tokio::time::timeout(POLL_TIMEOUT, ac.get_aircraft_by_hex(h)).await,
                ),
                FlightIdentifier::Callsign(cs) => {
                    if has_live_adsb_identity && let Some(h) = &hex {
                        (
                            true,
                            tokio::time::timeout(POLL_TIMEOUT, ac.get_aircraft_by_hex(h)).await,
                        )
                    } else {
                        let lookup_callsign = callsign.as_deref().unwrap_or(cs);
                        (
                            false,
                            tokio::time::timeout(
                                POLL_TIMEOUT,
                                ac.get_aircraft_by_callsign(lookup_callsign),
                            )
                            .await,
                        )
                    }
                }
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

        if !aircraft_matches_tracked_callsign(&ac, flight) {
            debug!(
                identifier = %flight.identifier,
                aircraft_callsign = aircraft_callsign(&ac).unwrap_or("<missing>"),
                "Ignoring ADS-B aircraft with mismatched callsign"
            );
            if used_hex && flight.hex.is_some() {
                flight.hex = None;
            }
            continue;
        }

        if was_pending {
            messages.push(msg_adsb_visible(flight));
        }

        flight.last_seen = Some(now);

        if flight.callsign.is_none()
            && let Some(cs) = ac
                .flight
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
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
                if let Some((lat, lon, _)) = iata_to_coords(&dest) {
                    flight.dest_lat = Some(lat);
                    flight.dest_lon = Some(lon);
                }
                flight.route = Some((origin, dest));
            }
        }
        if (flight.hex.is_none() || was_pending)
            && let Some(hex) = &ac.hex
        {
            debug!(identifier = %flight.identifier, hex = %hex, "Resolved hex");
            flight.hex = Some(hex.clone());
        }
        if flight.aircraft_type.is_none()
            && let Some(t) = &ac.t
        {
            flight.aircraft_type = Some(t.clone());
        }

        if let Some(new_squawk) = &ac.squawk {
            let squawk_changed = flight.squawk.as_ref() != Some(new_squawk);
            if squawk_changed && let Some(meaning) = emergency_squawk_meaning(new_squawk) {
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

            if !was_pending {
                match new_phase {
                    FlightPhase::Takeoff => messages.push(msg_takeoff(flight)),
                    FlightPhase::Cruise => messages.push(msg_cruise(flight)),
                    FlightPhase::Descent => messages.push(msg_descent(flight)),
                    FlightPhase::Approach => messages.push(msg_approach(flight)),
                    FlightPhase::Landing => messages.push(msg_landing(flight, now)),
                    _ => {}
                }
            }

            if new_phase == FlightPhase::Landing {
                flight.phase = FlightPhase::Ground;
                flight.takeoff_at = None;
            }
        } else {
            flight.polls_since_change += 1;
        }

        if matches!(flight.phase, FlightPhase::Descent | FlightPhase::Approach) {
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
