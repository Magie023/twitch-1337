use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use twitch_irc::{login::LoginCredentials, message::PrivmsgMessage, transport::Transport};

use crate::aviation::{AviationClient, AviationstackFlightMetadata, NearbyAircraft};
use crate::twitch::ChatSender;
use crate::util::clock::Clock;

use super::{
    CachedFlightInfo, FlightIdentifier, FlightPhase, FlightTrackerState, HexSource,
    TargetConfirmation, TrackedFlight, TrackerCommand, build_flight_view,
    debug_journal::{DebugHttpOutcome, FlightTrackerDebugEvent, append_debug_event},
    format::{msg_aviationstack_info, msg_flight_status, msg_flights_list, msg_track_started},
    metadata::{
        aircraft_callsign, apply_aviationstack_metadata, metadata_callsign, normalize_flight_code,
        seed_flight_aliases, set_route_from_iata,
    },
    phase::detect_phase,
    schedule::{PollReadiness, poll_readiness},
    state::save_tracker_state,
};

use super::{MAX_FLIGHTS_PER_USER, MAX_TRACKED_FLIGHTS, POLL_TIMEOUT, ROUTE_FETCH_TIMEOUT};

use super::advance::{
    Emit, Followup, Observation, PollOutcome, RemovalReason, advance_flight,
    apply_observed_aircraft, apply_route, candidate_callsign_matches_flight, format_emit,
    identifier_callsign, last_seen_age_secs, target_confirmation_for_aircraft,
};

fn find_index_by_identifier(
    flights: &[TrackedFlight],
    identifier: &FlightIdentifier,
) -> Option<usize> {
    flights
        .iter()
        .position(|flight| &flight.identifier == identifier)
}

fn callsign_poll_candidates(flight: &TrackedFlight) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut push = |value: &str| {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        let value = value.to_uppercase();
        if candidates
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
        {
            return;
        }
        candidates.push(value);
    };

    if let Some(callsign) = flight.callsign.as_deref() {
        push(callsign);
    }
    if let FlightIdentifier::Callsign(identifier) = &flight.identifier {
        push(identifier);
    }
    for alias in &flight.alias_callsigns {
        push(alias);
    }
    candidates
}

async fn poll_aircraft_by_callsign_aliases(
    aviation_client: &AviationClient,
    candidates: &[String],
) -> Result<Result<Option<NearbyAircraft>, eyre::Report>, tokio::time::error::Elapsed> {
    let mut last = Ok(Ok(None));
    for callsign in candidates {
        let result = tokio::time::timeout(
            POLL_TIMEOUT,
            aviation_client.get_aircraft_by_callsign(callsign),
        )
        .await;
        if matches!(&result, Ok(Ok(Some(_)))) {
            return result;
        }
        last = result;
    }
    last
}

pub(crate) fn find_flight_index(flights: &[TrackedFlight], query: &str) -> Option<usize> {
    let upper = query.to_uppercase();
    flights.iter().position(|f| {
        f.identifier.as_str().eq_ignore_ascii_case(&upper)
            || f.callsign
                .as_ref()
                .is_some_and(|cs| cs.eq_ignore_ascii_case(&upper))
            || f.alias_callsigns
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&upper))
            || f.hex
                .as_ref()
                .is_some_and(|h| h.eq_ignore_ascii_case(&upper))
    })
}

#[cfg(test)]
pub(crate) fn aircraft_matches_tracked_callsign(
    ac: &NearbyAircraft,
    flight: &TrackedFlight,
) -> bool {
    use super::advance::flight_matches_callsign;

    matches!(&flight.identifier, FlightIdentifier::Hex(_))
        || aircraft_callsign(ac).is_some_and(|actual| flight_matches_callsign(flight, actual))
}

fn should_poll_by_hex(flight: &TrackedFlight) -> bool {
    matches!(&flight.identifier, FlightIdentifier::Hex(_))
        || (flight.hex.is_some() && (flight.hex_source.is_some() || flight.last_seen.is_some()))
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
            || identifier_callsign(identifier)
                .is_some_and(|candidate| candidate_callsign_matches_flight(f, candidate))
            || callsign.is_some_and(|callsign| candidate_callsign_matches_flight(f, callsign))
            || hex.is_some_and(|hex| {
                f.hex
                    .as_deref()
                    .is_some_and(|existing| existing.eq_ignore_ascii_case(hex))
            })
    })
}

fn track_started_response(
    flight: &TrackedFlight,
    aviationstack_info: Option<&str>,
    aviationstack_fallback: bool,
) -> String {
    let mut response = msg_track_started(flight);
    if let Some(info) = aviationstack_info {
        response.push_str(" | ");
        response.push_str(info);
    }
    if aviationstack_fallback {
        response
            .push_str(" | Provider-Hinweis: AviationStack down/keine Daten, Status: ADS-B-only");
    }
    response
}

struct AviationstackLookup {
    checked: bool,
    metadata: Option<AviationstackFlightMetadata>,
    cache_changed: bool,
    failed: bool,
}

fn push_cache_alias(aliases: &mut Vec<String>, value: Option<&str>) {
    let Some(alias) = value.and_then(normalize_flight_code) else {
        return;
    };
    if !aliases
        .iter()
        .any(|known| known.eq_ignore_ascii_case(&alias))
    {
        aliases.push(alias);
    }
}

fn query_cache_aliases(identifier: &FlightIdentifier, callsign: Option<&str>) -> Vec<String> {
    let mut aliases = Vec::new();
    if matches!(identifier, FlightIdentifier::Callsign(_)) {
        push_cache_alias(&mut aliases, Some(identifier.as_str()));
    }
    push_cache_alias(&mut aliases, callsign);
    aliases
}

fn metadata_cache_aliases(metadata: &AviationstackFlightMetadata) -> Vec<String> {
    let mut aliases = Vec::new();
    push_cache_alias(&mut aliases, metadata.flight_iata.as_deref());
    push_cache_alias(&mut aliases, metadata.flight_icao.as_deref());
    push_cache_alias(&mut aliases, metadata_callsign(metadata).as_deref());
    aliases
}

fn metadata_flight_date(metadata: &AviationstackFlightMetadata) -> Option<String> {
    metadata
        .flight_date
        .as_deref()
        .map(str::trim)
        .filter(|date| !date.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            metadata
                .departure_scheduled
                .as_ref()
                .or(metadata.arrival_scheduled.as_ref())
                .or(metadata.arrival_estimated.as_ref())
                .map(|dt| dt.date_naive().to_string())
        })
}

fn flight_info_cache_expiry(
    metadata: Option<&AviationstackFlightMetadata>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let Some(metadata) = metadata else {
        return now + TimeDelta::minutes(10);
    };

    if metadata
        .flight_status
        .as_deref()
        .is_some_and(|status| status.eq_ignore_ascii_case("landed"))
    {
        return now + TimeDelta::hours(24);
    }

    let arrival = metadata
        .arrival_actual
        .as_ref()
        .cloned()
        .or_else(|| metadata.arrival_estimated.as_ref().cloned())
        .or_else(|| metadata.arrival_scheduled.as_ref().cloned());
    let expires_at = arrival
        .map(|arrival| arrival + TimeDelta::hours(4))
        .unwrap_or_else(|| now + TimeDelta::hours(4));
    if expires_at > now {
        expires_at
    } else {
        now + TimeDelta::hours(4)
    }
}

fn prune_flight_info_cache(state: &mut FlightTrackerState, now: DateTime<Utc>) -> bool {
    let before = state.flight_info_cache.len();
    state
        .flight_info_cache
        .retain(|entry| !entry.is_expired(now));
    before != state.flight_info_cache.len()
}

fn cached_flight_info(
    state: &FlightTrackerState,
    aliases: &[String],
    now: DateTime<Utc>,
) -> Option<Option<AviationstackFlightMetadata>> {
    state
        .flight_info_cache
        .iter()
        .filter(|entry| !entry.is_expired(now) && entry.matches_any_alias(aliases))
        .max_by_key(|entry| entry.cached_at)
        .map(|entry| entry.metadata.clone())
}

fn upsert_flight_info_cache(
    state: &mut FlightTrackerState,
    query_aliases: &[String],
    metadata: Option<AviationstackFlightMetadata>,
    now: DateTime<Utc>,
) {
    let mut aliases = query_aliases.to_vec();
    let flight_date = metadata.as_ref().and_then(metadata_flight_date);
    if let Some(metadata) = metadata.as_ref() {
        for alias in metadata_cache_aliases(metadata) {
            push_cache_alias(&mut aliases, Some(&alias));
        }
    }
    if aliases.is_empty() {
        return;
    }

    let expires_at = flight_info_cache_expiry(metadata.as_ref(), now);
    if let Some(entry) = state.flight_info_cache.iter_mut().find(|entry| {
        entry.matches_any_alias(&aliases)
            && (entry.flight_date == flight_date
                || entry.flight_date.is_none()
                || flight_date.is_none())
    }) {
        for alias in aliases {
            push_cache_alias(&mut entry.aliases, Some(&alias));
        }
        entry.flight_date = flight_date;
        entry.metadata = metadata;
        entry.cached_at = now;
        entry.expires_at = expires_at;
        return;
    }

    state.flight_info_cache.push(CachedFlightInfo {
        aliases,
        flight_date,
        metadata,
        cached_at: now,
        expires_at,
    });
}

async fn lookup_aviationstack_metadata(
    state: &mut FlightTrackerState,
    aviation_client: &AviationClient,
    identifier: &FlightIdentifier,
    callsign: Option<&str>,
    now: DateTime<Utc>,
) -> AviationstackLookup {
    let cache_changed = prune_flight_info_cache(state, now);
    let query_aliases = query_cache_aliases(identifier, callsign);
    if query_aliases.is_empty() {
        return AviationstackLookup {
            checked: false,
            metadata: None,
            cache_changed,
            failed: false,
        };
    }

    if let Some(metadata) = cached_flight_info(state, &query_aliases, now) {
        return AviationstackLookup {
            checked: true,
            metadata,
            cache_changed,
            failed: false,
        };
    }

    if !aviation_client.aviationstack_enabled() {
        return AviationstackLookup {
            checked: false,
            metadata: None,
            cache_changed,
            failed: false,
        };
    }

    match aviation_client
        .get_aviationstack_flight_metadata(identifier, callsign)
        .await
    {
        Ok(metadata) => {
            upsert_flight_info_cache(state, &query_aliases, metadata.clone(), now);
            AviationstackLookup {
                checked: true,
                metadata,
                cache_changed: true,
                failed: false,
            }
        }
        Err(e) => {
            warn!(error = ?e, identifier = %identifier, "Aviationstack metadata lookup failed");
            AviationstackLookup {
                checked: true,
                metadata: None,
                cache_changed,
                failed: true,
            }
        }
    }
}

pub(crate) async fn remove_flight_at(
    state: &mut FlightTrackerState,
    query: &str,
    data_dir: &Path,
    source: &str,
    clock: &dyn Clock,
) -> Option<String> {
    let idx = find_flight_index(&state.flights, query)?;
    let now = clock.now_utc();
    let flight = &state.flights[idx];
    let label = flight
        .callsign
        .as_deref()
        .unwrap_or(flight.identifier.as_str())
        .to_owned();
    append_debug_event(
        data_dir,
        now,
        FlightTrackerDebugEvent::tracking_removal(flight, source, last_seen_age_secs(flight, now)),
    )
    .await;
    state.flights.remove(idx);
    save_tracker_state(data_dir, state).await;
    info!(identifier = %query, source = %source, "Flight untracked");
    Some(label)
}

async fn process_latency_sensitive_command(
    cmd: TrackerCommand,
    state: &mut FlightTrackerState,
    data_dir: &Path,
    clock: &dyn Clock,
) -> Option<TrackerCommand> {
    match cmd {
        TrackerCommand::Snapshot { reply } => {
            let now = clock.now_utc();
            let _ = reply.send(build_flight_view(state, now));
            None
        }
        TrackerCommand::DeleteFromWeb { identifier, reply } => {
            let removed = remove_flight_at(state, &identifier, data_dir, "web", clock).await;
            let _ = reply.send(removed);
            None
        }
        other => Some(other),
    }
}

async fn drain_latency_sensitive_commands(
    cmd_rx: &mut Option<&mut mpsc::Receiver<TrackerCommand>>,
    state: &mut FlightTrackerState,
    data_dir: &Path,
    clock: &dyn Clock,
    deferred_commands: &mut Vec<TrackerCommand>,
) {
    let Some(cmd_rx) = cmd_rx.as_mut() else {
        return;
    };
    while let Ok(cmd) = cmd_rx.try_recv() {
        if let Some(deferred) = process_latency_sensitive_command(cmd, state, data_dir, clock).await
        {
            deferred_commands.push(deferred);
        }
    }
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
                clock,
            )
            .await;
        }
        TrackerCommand::Status {
            identifier,
            reply_to,
        } => {
            handle_status(identifier.as_deref(), &reply_to, state, sender, clock).await;
        }
        TrackerCommand::Info {
            identifier,
            reply_to,
        } => {
            handle_info(
                identifier,
                &reply_to,
                state,
                sender,
                aviation_client,
                data_dir,
                clock,
            )
            .await;
        }
        cmd @ (TrackerCommand::Snapshot { .. } | TrackerCommand::DeleteFromWeb { .. }) => {
            let _ = process_latency_sensitive_command(cmd, state, data_dir, clock).await;
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
                format!("Track-Limit erreicht: maximal {MAX_TRACKED_FLIGHTS} aktive Flüge gleichzeitig FDM"),
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
                format!(
                    "Track-Limit erreicht: du trackst bereits {MAX_FLIGHTS_PER_USER} Flüge FDM"
                ),
            )
            .await;
        return;
    }

    if duplicate_tracking_exists(state, &identifier, None, None) {
        sender
            .reply(
                reply_to,
                format!("{identifier} wird bereits getrackt | Status: aktiv FDM"),
            )
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
            .reply(
                reply_to,
                format!("{identifier} wird bereits getrackt | Status: aktiv FDM"),
            )
            .await;
        return;
    }

    let now = clock.now_utc();
    let aviationstack_lookup = if matches!(&identifier, FlightIdentifier::Callsign(_)) {
        lookup_aviationstack_metadata(
            state,
            aviation_client,
            &identifier,
            resolved_callsign.as_deref(),
            now,
        )
        .await
    } else {
        AviationstackLookup {
            checked: false,
            metadata: None,
            cache_changed: false,
            failed: false,
        }
    };
    let aviationstack_cache_changed = aviationstack_lookup.cache_changed;
    let metadata = aviationstack_lookup.metadata;
    let aviationstack_checked = aviationstack_lookup.checked;
    let aviationstack_info = metadata.as_ref().map(msg_aviationstack_info);
    if matches!(&identifier, FlightIdentifier::Callsign(_)) {
        let outcome = if metadata.is_some() {
            DebugHttpOutcome::success("aviationstack", "flight_metadata")
        } else {
            DebugHttpOutcome::miss("aviationstack", "flight_metadata")
        };
        append_debug_event(
            data_dir,
            now,
            FlightTrackerDebugEvent::aviationstack_metadata(
                &identifier,
                resolved_callsign.as_deref(),
                outcome,
                metadata.as_ref(),
            ),
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
        alias_callsigns: Vec::new(),
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
        last_visible_at: None,
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
    seed_flight_aliases(&mut flight);

    if let Some(metadata) = metadata.clone() {
        apply_aviationstack_metadata(&mut flight, metadata);
    }
    if duplicate_tracking_exists(
        state,
        &identifier,
        flight.callsign.as_deref(),
        flight.hex.as_deref(),
    ) {
        if aviationstack_cache_changed {
            save_tracker_state(data_dir, state).await;
        }
        sender
            .reply(
                reply_to,
                format!("{identifier} wird bereits getrackt | Status: aktiv FDM"),
            )
            .await;
        return;
    }

    let keep_pending_on_adsb_absence = matches!(&identifier, FlightIdentifier::Callsign(_))
        && aviation_client.aviationstack_enabled();
    if !should_skip_adsb_for_strict_chill(&flight, now) {
        type PollResult =
            Result<Result<Option<NearbyAircraft>, eyre::Report>, tokio::time::error::Elapsed>;

        let mut used_hex = should_poll_by_hex(&flight);
        let mut ac_result: Option<PollResult> = if used_hex {
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
            let candidates = callsign_poll_candidates(&flight);
            if candidates.is_empty() {
                None
            } else {
                Some(poll_aircraft_by_callsign_aliases(aviation_client, &candidates).await)
            }
        };

        if let Some(ref result) = ac_result
            && matches!(result, Ok(Ok(None)))
            && !used_hex
            && let FlightIdentifier::Callsign(input) = &identifier
            && FlightIdentifier::is_valid_icao24_hex(input)
        {
            used_hex = true;
            ac_result = Some(
                tokio::time::timeout(POLL_TIMEOUT, aviation_client.get_aircraft_by_hex(input))
                    .await,
            );
        }

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
                                    format!("{identifier} nicht gefunden | Provider: ADS-B | Grund: kein passendes Signal FDM"),
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
                            append_debug_event(
                                data_dir,
                                now,
                                FlightTrackerDebugEvent::flight_route_lookup(
                                    flight.identifier.as_str(),
                                    Some(cs),
                                    DebugHttpOutcome::success("adsbdb", "flight_route"),
                                    Some(&origin),
                                    Some(&dest),
                                    flight.dest_lat.is_some() && flight.dest_lon.is_some(),
                                ),
                            )
                            .await;
                        }
                        let response = track_started_response(
                            &flight,
                            aviationstack_info.as_deref(),
                            aviationstack_fallback,
                        );
                        append_debug_event(
                            data_dir,
                            now,
                            FlightTrackerDebugEvent::track_started(&flight),
                        )
                        .await;
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
                    // Seed identity + telemetry through the shared core, the one
                    // owner of this mutation (also driven per-cycle by
                    // `advance_flight`). The initial-track ack does not announce,
                    // so the newly-resolved callsign it returns is unused here.
                    let _ =
                        apply_observed_aircraft(&mut flight, &ac, confirmation, target_confirmed);
                    if target_confirmed {
                        flight.last_seen = Some(now);
                        flight.last_visible_at = Some(now);
                        flight.phase = detect_phase(&flight, &ac);
                    }
                }
                Ok(Ok(None)) => {
                    if !keep_pending_on_adsb_absence {
                        sender
                            .reply(
                                reply_to,
                                format!("{identifier} nicht gefunden | Provider: ADS-B | Grund: kein passendes Signal FDM"),
                            )
                            .await;
                        return;
                    }
                }
                Ok(Err(e)) => {
                    error!(error = ?e, identifier = %identifier, "ADS-B lookup failed");
                    if !keep_pending_on_adsb_absence {
                        sender
                            .reply(
                                reply_to,
                                "Provider-Problem: ADS-B down | Grund: Anfrage fehlgeschlagen FDM",
                            )
                            .await;
                        return;
                    }
                }
                Err(_) => {
                    if !keep_pending_on_adsb_absence {
                        sender.reply(reply_to, "Provider-Problem: Timeout | Provider: ADS-B | Grund: Anfrage dauerte zu lange FDM").await;
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
        let lookup = lookup_aviationstack_metadata(
            state,
            aviation_client,
            &identifier,
            flight.callsign.as_deref(),
            now,
        )
        .await;
        let outcome = if lookup.metadata.is_some() {
            DebugHttpOutcome::success("aviationstack", "flight_metadata")
        } else {
            DebugHttpOutcome::miss("aviationstack", "flight_metadata")
        };
        append_debug_event(
            data_dir,
            now,
            FlightTrackerDebugEvent::aviationstack_metadata(
                &identifier,
                flight.callsign.as_deref(),
                outcome,
                lookup.metadata.as_ref(),
            ),
        )
        .await;
        if let Some(metadata) = lookup.metadata {
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
            .reply(
                reply_to,
                format!("{identifier} wird bereits getrackt | Status: aktiv FDM"),
            )
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
                append_debug_event(
                    data_dir,
                    now,
                    FlightTrackerDebugEvent::flight_route_lookup(
                        flight.identifier.as_str(),
                        Some(cs),
                        DebugHttpOutcome::success("adsbdb", "flight_route"),
                        Some(&origin),
                        Some(&dest),
                        flight.dest_lat.is_some() && flight.dest_lon.is_some(),
                    ),
                )
                .await;
            }
            Ok(Ok(None)) => {
                debug!(callsign = %cs, "No route found for flight");
                append_debug_event(
                    data_dir,
                    now,
                    FlightTrackerDebugEvent::flight_route_lookup(
                        flight.identifier.as_str(),
                        Some(cs),
                        DebugHttpOutcome::miss("adsbdb", "flight_route"),
                        None,
                        None,
                        false,
                    ),
                )
                .await;
            }
            Ok(Err(e)) => {
                warn!(error = ?e, callsign = %cs, "Failed to fetch route");
                append_debug_event(
                    data_dir,
                    now,
                    FlightTrackerDebugEvent::flight_route_lookup(
                        flight.identifier.as_str(),
                        Some(cs),
                        DebugHttpOutcome::error("adsbdb", "flight_route"),
                        None,
                        None,
                        false,
                    ),
                )
                .await;
            }
            Err(_) => {
                warn!(callsign = %cs, "Route fetch timed out");
                append_debug_event(
                    data_dir,
                    now,
                    FlightTrackerDebugEvent::flight_route_lookup(
                        flight.identifier.as_str(),
                        Some(cs),
                        DebugHttpOutcome::timeout("adsbdb", "flight_route"),
                        None,
                        None,
                        false,
                    ),
                )
                .await;
            }
        }
    }

    let response = track_started_response(
        &flight,
        aviationstack_info.as_deref(),
        aviationstack_fallback,
    );
    append_debug_event(
        data_dir,
        now,
        FlightTrackerDebugEvent::track_started(&flight),
    )
    .await;
    state.flights.push(flight);
    save_tracker_state(data_dir, state).await;

    info!(identifier = %identifier, requested_by = %requested_by, "Flight tracking started");
    sender.reply(reply_to, response).await;
}

#[allow(
    clippy::too_many_arguments,
    reason = "command handler threads shared tracker deps + injected clock"
)]
async fn handle_untrack<T, L>(
    identifier: &str,
    requested_by: &str,
    is_mod: bool,
    reply_to: &PrivmsgMessage,
    state: &mut FlightTrackerState,
    sender: &Arc<ChatSender<T, L>>,
    data_dir: &Path,
    clock: &dyn Clock,
) where
    T: Transport,
    L: LoginCredentials,
{
    let Some(idx) = find_flight_index(&state.flights, identifier) else {
        sender
            .reply(
                reply_to,
                format!("{identifier} nicht gefunden | Status: kein aktiver Track FDM"),
            )
            .await;
        return;
    };

    let flight = &state.flights[idx];
    if flight.tracked_by != requested_by && !is_mod {
        sender
            .reply(
                reply_to,
                "Keine Berechtigung: nur Tracker oder Mods können diesen Flug entfernen FDM",
            )
            .await;
        return;
    }

    let name = remove_flight_at(state, identifier, data_dir, requested_by, clock)
        .await
        .unwrap_or_else(|| identifier.to_owned());

    sender
        .reply(
            reply_to,
            format!("Untrack entfernt: {name} | Status: nicht mehr getrackt Okayge"),
        )
        .await;
}

async fn handle_info<T, L>(
    identifier: FlightIdentifier,
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
    let tracked_callsign = find_flight_index(state.flights.as_slice(), identifier.as_str())
        .and_then(|idx| state.flights[idx].callsign.clone());
    let resolved_callsign = match (&identifier, tracked_callsign.as_deref()) {
        (_, Some(callsign)) => Some(callsign.to_string()),
        (FlightIdentifier::Callsign(callsign), None) => {
            Some(aviation_client.resolve_callsign(callsign).await)
        }
        (FlightIdentifier::Hex(_), None) => None,
    };

    let now = clock.now_utc();
    let lookup = lookup_aviationstack_metadata(
        state,
        aviation_client,
        &identifier,
        resolved_callsign.as_deref(),
        now,
    )
    .await;
    if lookup.cache_changed {
        save_tracker_state(data_dir, state).await;
    }

    let response = if let Some(metadata) = lookup.metadata {
        msg_aviationstack_info(&metadata)
    } else if lookup.failed {
        "Provider-Problem: AviationStack down | Grund: Anfrage fehlgeschlagen FDM".to_string()
    } else if lookup.checked || aviation_client.aviationstack_enabled() {
        format!("{identifier} nicht bei AviationStack gefunden | Grund: keine Provider-Daten FDM")
    } else {
        "Provider-Problem: AviationStack down | Grund: nicht konfiguriert FDM".to_string()
    };

    sender.reply(reply_to, response).await;
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
            None => format!("{id} nicht gefunden | Status: kein aktiver Track FDM"),
        },
    };

    sender.reply(reply_to, response).await;
}

pub(crate) async fn poll_all_flights_with_commands<T, L>(
    state: &mut FlightTrackerState,
    sender: &Arc<ChatSender<T, L>>,
    channel: &str,
    aviation_client: &AviationClient,
    data_dir: &Path,
    clock: &dyn Clock,
    mut cmd_rx: Option<&mut mpsc::Receiver<TrackerCommand>>,
) -> Vec<TrackerCommand>
where
    T: Transport,
    L: LoginCredentials,
{
    let now = clock.now_utc();
    let mut changed = false;
    let mut removals: Vec<FlightIdentifier> = Vec::new();
    let mut messages: Vec<String> = Vec::new();
    let mut deferred_commands = Vec::new();

    type PollResult =
        Result<Result<Option<NearbyAircraft>, eyre::Report>, tokio::time::error::Elapsed>;
    type PollAttempt = (FlightIdentifier, bool, PollResult);

    let mut join_set = tokio::task::JoinSet::new();
    let mut fetch_results: Vec<PollAttempt> = Vec::new();

    for flight in state.flights.iter() {
        match poll_readiness(flight, now) {
            PollReadiness::Due => {}
            PollReadiness::NotDue(_) => continue,
            PollReadiness::Expired => {
                info!(
                    identifier = %flight.identifier,
                    "Removing pending flight: ADS-B never appeared"
                );
                append_debug_event(
                    data_dir,
                    now,
                    FlightTrackerDebugEvent::tracking_removal(flight, "pending_expired", None),
                )
                .await;
                messages.push(format_emit(flight, &Emit::PendingExpired, now));
                removals.push(flight.identifier.clone());
                changed = true;
                continue;
            }
        }

        let ac = aviation_client.clone();
        let id = flight.identifier.clone();
        let hex = flight.hex.clone();
        let callsign_candidates = callsign_poll_candidates(flight);
        let poll_by_hex = should_poll_by_hex(flight);
        join_set.spawn(async move {
            let result: PollAttempt = if poll_by_hex {
                let lookup_hex = hex.as_deref().unwrap_or_else(|| id.as_str());
                (
                    id.clone(),
                    true,
                    tokio::time::timeout(POLL_TIMEOUT, ac.get_aircraft_by_hex(lookup_hex)).await,
                )
            } else if callsign_candidates.is_empty() {
                (id.clone(), false, Ok(Ok(None)))
            } else {
                (
                    id.clone(),
                    false,
                    poll_aircraft_by_callsign_aliases(&ac, &callsign_candidates).await,
                )
            };
            result
        });
    }

    if let Some(cmd_rx) = cmd_rx.as_mut() {
        let mut command_channel_open = true;
        while !join_set.is_empty() {
            tokio::select! {
                res = join_set.join_next() => {
                    if let Some(Ok((identifier, used_hex, poll_result))) = res {
                        fetch_results.push((identifier, used_hex, poll_result));
                    }
                }
                cmd = cmd_rx.recv(), if command_channel_open => {
                    match cmd {
                        Some(cmd) => {
                            if let Some(deferred) = process_latency_sensitive_command(
                                cmd,
                                state,
                                data_dir,
                                clock,
                            )
                            .await
                            {
                                deferred_commands.push(deferred);
                            }
                        }
                        None => {
                            command_channel_open = false;
                        }
                    }
                }
            }
        }
    } else {
        while let Some(res) = join_set.join_next().await {
            if let Ok((identifier, used_hex, poll_result)) = res {
                fetch_results.push((identifier, used_hex, poll_result));
            }
        }
    }

    for (identifier, used_hex, ac_result) in fetch_results {
        drain_latency_sensitive_commands(
            &mut cmd_rx,
            state,
            data_dir,
            clock,
            &mut deferred_commands,
        )
        .await;

        let Some(idx) = find_index_by_identifier(&state.flights, &identifier) else {
            continue;
        };

        let outcome = match ac_result {
            Ok(Ok(Some(ac))) => PollOutcome::Hit(Box::new(ac)),
            Ok(Ok(None)) => PollOutcome::Miss,
            Ok(Err(e)) => {
                warn!(error = ?e, identifier = %identifier, "ADS-B poll failed");
                PollOutcome::Error
            }
            Err(_) => {
                warn!(identifier = %identifier, "ADS-B poll timed out");
                PollOutcome::Timeout
            }
        };
        let obs = Observation { used_hex, outcome };

        let update = {
            let flight = &mut state.flights[idx];
            advance_flight(flight, &obs, now)
        };
        changed = true;

        for event in update.debug {
            append_debug_event(data_dir, now, event).await;
        }

        for followup in update.followups {
            let Followup::FetchRoute { callsign } = followup;
            match tokio::time::timeout(
                ROUTE_FETCH_TIMEOUT,
                aviation_client.get_flight_route(&callsign),
            )
            .await
            {
                Ok(Ok(Some(route))) => {
                    let origin = route.origin.iata_code.clone();
                    let dest = route.destination.iata_code.clone();
                    let flight = &mut state.flights[idx];
                    apply_route(flight, &origin, &dest);
                    append_debug_event(
                        data_dir,
                        now,
                        FlightTrackerDebugEvent::flight_route_lookup(
                            state.flights[idx].identifier.as_str(),
                            Some(&callsign),
                            DebugHttpOutcome::success("adsbdb", "flight_route"),
                            Some(&origin),
                            Some(&dest),
                            state.flights[idx].dest_lat.is_some()
                                && state.flights[idx].dest_lon.is_some(),
                        ),
                    )
                    .await;
                }
                Ok(Ok(None)) => {
                    append_debug_event(
                        data_dir,
                        now,
                        FlightTrackerDebugEvent::flight_route_lookup(
                            state.flights[idx].identifier.as_str(),
                            Some(&callsign),
                            DebugHttpOutcome::miss("adsbdb", "flight_route"),
                            None,
                            None,
                            false,
                        ),
                    )
                    .await;
                }
                Ok(Err(_)) => {
                    append_debug_event(
                        data_dir,
                        now,
                        FlightTrackerDebugEvent::flight_route_lookup(
                            state.flights[idx].identifier.as_str(),
                            Some(&callsign),
                            DebugHttpOutcome::error("adsbdb", "flight_route"),
                            None,
                            None,
                            false,
                        ),
                    )
                    .await;
                }
                Err(_) => {
                    append_debug_event(
                        data_dir,
                        now,
                        FlightTrackerDebugEvent::flight_route_lookup(
                            state.flights[idx].identifier.as_str(),
                            Some(&callsign),
                            DebugHttpOutcome::timeout("adsbdb", "flight_route"),
                            None,
                            None,
                            false,
                        ),
                    )
                    .await;
                }
            }
        }

        for emit in &update.emits {
            let msg = format_emit(&state.flights[idx], emit, now);
            messages.push(msg);
        }

        if let Some(reason) = update.removal {
            match reason {
                RemovalReason::TrackingLost { secs } => {
                    info!(identifier = %identifier, "Removing flight: tracking lost for {secs}s");
                }
            }
            removals.push(identifier.clone());
        }
    }

    for identifier in removals {
        if let Some(idx) = find_index_by_identifier(&state.flights, &identifier) {
            state.flights.remove(idx);
            changed = true;
        }
    }

    drain_latency_sensitive_commands(&mut cmd_rx, state, data_dir, clock, &mut deferred_commands)
        .await;

    for msg in messages {
        sender.say(channel.to_string(), msg).await;
    }

    if changed {
        save_tracker_state(data_dir, state).await;
    }

    deferred_commands
}
