use std::borrow::Borrow;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt as _;
use tracing::warn;

use crate::aviation::{AviationstackFlightMetadata, NearbyAircraft};

use super::{FlightIdentifier, TrackedFlight};

/// Daily journal files retained on disk; older ones are pruned. ~one month.
pub(crate) const DEBUG_JOURNAL_KEEP_FILES: usize = 30;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DebugHttpOutcome {
    pub provider: String,
    pub endpoint_kind: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "client_response/forbidden_parked/rate_limited are exercised by tests, pending ADS-B poll-path wiring of provider-refusal detail"
    )
)]
impl DebugHttpOutcome {
    pub(crate) fn client_response(provider: &str, endpoint_kind: &str, status: u16) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint_kind: endpoint_kind.to_string(),
            outcome: "client_response".to_string(),
            status: Some(status),
        }
    }
    pub(crate) fn forbidden_parked(provider: &str, endpoint_kind: &str, status: u16) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint_kind: endpoint_kind.to_string(),
            outcome: "forbidden_parked".to_string(),
            status: Some(status),
        }
    }
    pub(crate) fn rate_limited(provider: &str, endpoint_kind: &str) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint_kind: endpoint_kind.to_string(),
            outcome: "rate_limited".to_string(),
            status: Some(429),
        }
    }
    pub(crate) fn success(provider: &str, endpoint_kind: &str) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint_kind: endpoint_kind.to_string(),
            outcome: "success".to_string(),
            status: None,
        }
    }
    pub(crate) fn miss(provider: &str, endpoint_kind: &str) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint_kind: endpoint_kind.to_string(),
            outcome: "miss".to_string(),
            status: None,
        }
    }
    pub(crate) fn error(provider: &str, endpoint_kind: &str) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint_kind: endpoint_kind.to_string(),
            outcome: "error".to_string(),
            status: None,
        }
    }
    pub(crate) fn timeout(provider: &str, endpoint_kind: &str) -> Self {
        Self {
            provider: provider.to_string(),
            endpoint_kind: endpoint_kind.to_string(),
            outcome: "timeout".to_string(),
            status: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FlightTrackerDebugEvent {
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ts: Option<String>,
    identifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_confirmation: Option<String>,
    #[serde(flatten)]
    payload: Map<String, Value>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "non-_opt callsign/hex builder setters are used only by this module's tests"
    )
)]
impl FlightTrackerDebugEvent {
    pub(crate) fn new(event: &str, identifier: impl Into<String>) -> Self {
        Self {
            event: event.to_string(),
            ts: None,
            identifier: identifier.into(),
            callsign: None,
            hex: None,
            phase: None,
            target_confirmation: None,
            payload: Map::new(),
        }
    }
    pub(crate) fn for_flight(event: &str, flight: &TrackedFlight) -> Self {
        Self::new(event, flight.identifier.as_str())
            .callsign_opt(flight.callsign.as_deref())
            .hex_opt(flight.hex.as_deref())
            .phase(format!("{:?}", flight.phase))
            .target_confirmation(format!("{:?}", flight.target_confirmation))
            .payload(json!({
                "tracked_by": flight.tracked_by,
                "route": route_summary(flight),
                "scheduled_departure_at": flight.scheduled_departure_at.map(format_ts),
                "aircraft_type": flight.aircraft_type,
            }))
    }
    pub(crate) fn track_started(flight: &TrackedFlight, original_identifier: &str) -> Self {
        // `for_flight` already emits route/scheduled_departure_at/aircraft_type.
        Self::for_flight("track_started", flight).payload(json!({
            "original_identifier": original_identifier,
            "normalized_identifier": flight.identifier.as_str(),
            "user": flight.tracked_by,
            "aviationstack_checked": flight.aviationstack_checked,
        }))
    }
    pub(crate) fn aviationstack_metadata(
        identifier: &FlightIdentifier,
        callsign: Option<&str>,
        outcome: DebugHttpOutcome,
        metadata: Option<&AviationstackFlightMetadata>,
        cache_hit: bool,
    ) -> Self {
        Self::new("aviationstack_metadata", identifier.as_str())
            .callsign_opt(callsign)
            .payload(json!({
                "query": { "identifier": identifier.as_str(), "callsign": callsign },
                "http": outcome,
                "cache_hit": cache_hit,
                "metadata": metadata.map(metadata_summary),
            }))
    }
    pub(crate) fn flight_route_lookup(
        identifier: &str,
        callsign: Option<&str>,
        outcome: DebugHttpOutcome,
        origin: Option<&str>,
        destination: Option<&str>,
        destination_coordinates_resolved: bool,
    ) -> Self {
        Self::new("flight_route_lookup", identifier)
            .callsign_opt(callsign)
            .payload(json!({
                "http": outcome,
                "origin_iata": origin,
                "destination_iata": destination,
                "destination_coordinates_resolved": destination_coordinates_resolved,
            }))
    }
    pub(crate) fn adsb_poll_result(flight: &TrackedFlight, input: AdsbPollDebugInput<'_>) -> Self {
        Self::for_flight("adsb_poll_result", flight).payload(json!({
            "lookup_mode": input.lookup_mode,
            "aliases": input.aliases,
            "outcome": input.outcome,
            "selected_aircraft": input.aircraft.map(aircraft_summary),
            "direct_target_confirmed": input.direct_target_confirmed,
            "sticky_confirmation": input.sticky_confirmation,
            "phase_sample_eligible": input.phase_sample_eligible,
            "last_seen_age_secs": input.last_seen_age_secs,
        }))
    }
    pub(crate) fn phase_transition(
        flight: &TrackedFlight,
        old_phase: &str,
        new_phase: &str,
        takeoff_reset: bool,
    ) -> Self {
        Self::for_flight("phase_transition", flight).payload(json!({
            "old_phase": old_phase,
            "new_phase": new_phase,
            "altitude_ft": flight.altitude_ft,
            "vertical_rate_fpm": flight.vertical_rate_fpm,
            "ground_speed_kts": flight.ground_speed_kts,
            "takeoff_reset": takeoff_reset,
        }))
    }
    pub(crate) fn diversion_decision(flight: &TrackedFlight, input: DiversionDebugInput) -> Self {
        Self::for_flight("diversion_decision", flight).payload(json!({
            "previous_position": {"lat": input.previous_lat, "lon": input.previous_lon},
            "current_position": {"lat": input.current_lat, "lon": input.current_lon},
            "destination_position": {"lat": input.destination_lat, "lon": input.destination_lon},
            "ground_track": input.ground_track,
            "bearing_to_dest": input.bearing_to_dest,
            "diff": input.diff,
            "threshold": input.threshold,
            "anomalous": input.anomalous,
            "counter_before": input.counter_before,
            "counter_after": input.counter_after,
            "alert_emitted": input.alert_emitted,
        }))
    }
    pub(crate) fn target_confirmation_transition(
        flight: &TrackedFlight,
        old_confirmation: &str,
        new_confirmation: &str,
    ) -> Self {
        Self::for_flight("target_confirmation_transition", flight).payload(json!({
            "old_target_confirmation": old_confirmation,
            "new_target_confirmation": new_confirmation,
        }))
    }
    pub(crate) fn hex_assignment(
        flight: &TrackedFlight,
        old_hex: Option<&str>,
        new_hex: Option<&str>,
        old_source: Option<&str>,
        new_source: Option<&str>,
    ) -> Self {
        Self::for_flight("hex_assignment", flight).payload(json!({
            "old_hex": old_hex,
            "new_hex": new_hex,
            "old_hex_source": old_source,
            "new_hex_source": new_source,
        }))
    }
    pub(crate) fn web_command(command: &str, identifier: Option<&str>) -> Self {
        Self::new("web_command", identifier.unwrap_or("<snapshot>"))
            .payload(json!({ "command": command, "identifier": identifier }))
    }
    pub(crate) fn tracking_removal(
        flight: &TrackedFlight,
        reason: &str,
        last_seen_age_secs: Option<i64>,
    ) -> Self {
        Self::for_flight("tracking_removal", flight).payload(json!({
            "reason": reason,
            "age_secs": last_seen_age_secs,
            "last_seen_age_secs": last_seen_age_secs,
            "route": route_summary(flight)
        }))
    }
    pub(crate) fn callsign(mut self, callsign: impl Into<String>) -> Self {
        self.callsign = Some(callsign.into());
        self
    }
    pub(crate) fn callsign_opt(mut self, callsign: Option<&str>) -> Self {
        self.callsign = callsign.map(str::to_string);
        self
    }
    pub(crate) fn hex(mut self, hex: impl Into<String>) -> Self {
        self.hex = Some(hex.into());
        self
    }
    pub(crate) fn hex_opt(mut self, hex: Option<&str>) -> Self {
        self.hex = hex.map(str::to_string);
        self
    }
    pub(crate) fn phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = Some(phase.into());
        self
    }
    pub(crate) fn target_confirmation(mut self, target_confirmation: impl Into<String>) -> Self {
        self.target_confirmation = Some(target_confirmation.into());
        self
    }
    pub(crate) fn payload(mut self, payload: Value) -> Self {
        if let Value::Object(map) = sanitize_value(payload) {
            self.payload.extend(map);
        }
        self
    }
    fn with_timestamp(&self, now: DateTime<Utc>) -> Self {
        // `payload()` already sanitized every inserted value, so only the
        // timestamp is left to stamp here.
        let mut event = self.clone();
        event.ts = Some(format_ts(now));
        event
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AdsbPollDebugInput<'a> {
    pub lookup_mode: &'a str,
    pub aliases: &'a [String],
    pub outcome: &'a str,
    pub aircraft: Option<&'a NearbyAircraft>,
    pub direct_target_confirmed: Option<bool>,
    pub sticky_confirmation: Option<bool>,
    pub phase_sample_eligible: Option<bool>,
    pub last_seen_age_secs: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DiversionDebugInput {
    pub previous_lat: f64,
    pub previous_lon: f64,
    pub current_lat: f64,
    pub current_lon: f64,
    pub destination_lat: f64,
    pub destination_lon: f64,
    pub ground_track: f64,
    pub bearing_to_dest: f64,
    pub diff: f64,
    pub threshold: f64,
    pub anomalous: bool,
    pub counter_before: u32,
    pub counter_after: u32,
    pub alert_emitted: bool,
}

pub(crate) fn debug_journal_path(data_dir: &Path, now: DateTime<Utc>) -> PathBuf {
    data_dir
        .join("flight-tracker-debug")
        .join(format!("{}.jsonl", now.format("%Y-%m-%d")))
}

/// Delete all but the newest `keep` daily journal files (best-effort).
///
/// File names are `YYYY-MM-DD.jsonl`, so a lexicographic sort is chronological.
/// Errors are logged and swallowed, matching [`append_debug_event`].
pub(crate) async fn prune_debug_journals(data_dir: &Path, keep: usize) {
    let dir = data_dir.join("flight-tracker-debug");
    let mut read_dir = match tokio::fs::read_dir(&dir).await {
        Ok(read_dir) => read_dir,
        // A missing directory just means nothing has been written yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warn!(error = ?error, path = %dir.display(), "Failed to read flight tracker debug journal directory");
            return;
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    loop {
        match read_dir.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "jsonl") {
                    files.push(path);
                }
            }
            Ok(None) => break,
            Err(error) => {
                warn!(error = ?error, path = %dir.display(), "Failed to enumerate flight tracker debug journals");
                return;
            }
        }
    }
    if files.len() <= keep {
        return;
    }
    files.sort();
    let remove_count = files.len() - keep;
    for path in files.into_iter().take(remove_count) {
        if let Err(error) = tokio::fs::remove_file(&path).await {
            warn!(error = ?error, path = %path.display(), "Failed to prune flight tracker debug journal");
        }
    }
}

pub(crate) async fn append_debug_event<E>(data_dir: &Path, now: DateTime<Utc>, event: E)
where
    E: Borrow<FlightTrackerDebugEvent>,
{
    let path = debug_journal_path(data_dir, now);
    if let Some(parent) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        warn!(error = ?error, path = %parent.display(), "Failed to create flight tracker debug journal directory");
        return;
    }
    let event = event.borrow().with_timestamp(now);
    let mut line = match serde_json::to_vec(&event) {
        Ok(line) => line,
        Err(error) => {
            warn!(error = ?error, "Failed to serialize flight tracker debug event");
            return;
        }
    };
    line.push(b'\n');
    let mut file = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            warn!(error = ?error, path = %path.display(), "Failed to open flight tracker debug journal");
            return;
        }
    };
    if let Err(error) = file.write_all(&line).await {
        warn!(error = ?error, path = %path.display(), "Failed to append flight tracker debug event");
        return;
    }
    if let Err(error) = file.flush().await {
        warn!(error = ?error, path = %path.display(), "Failed to flush flight tracker debug journal");
    }
}

fn format_ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn route_summary(flight: &TrackedFlight) -> Value {
    match &flight.route {
        Some((origin, destination)) => {
            json!({ "origin": origin, "destination": destination, "destination_coordinates_resolved": flight.dest_lat.is_some() && flight.dest_lon.is_some() })
        }
        None => Value::Null,
    }
}

fn metadata_summary(metadata: &AviationstackFlightMetadata) -> Value {
    json!({
        "flight_iata": metadata.flight_iata,
        "flight_icao": metadata.flight_icao,
        "departure_iata": metadata.departure_iata,
        "arrival_iata": metadata.arrival_iata,
        "departure_scheduled": metadata.departure_scheduled.map(format_ts),
        "departure_actual": metadata.departure_actual.map(format_ts),
        "arrival_actual": metadata.arrival_actual.map(format_ts),
        "aircraft_icao24": metadata.aircraft_icao24,
        "aircraft_icao": metadata.aircraft_icao,
    })
}

fn aircraft_summary(aircraft: &NearbyAircraft) -> Value {
    json!({
        "hex": aircraft.hex,
        "flight": aircraft.flight.as_deref().map(str::trim).filter(|value| !value.is_empty()),
        "aircraft_type": aircraft.t,
        "altitude_baro": match &aircraft.alt_baro {
            Some(crate::aviation::AltBaro::Feet(value)) => json!({"feet": value}),
            Some(crate::aviation::AltBaro::Ground) => json!("ground"),
            None => Value::Null,
        },
        "lat": aircraft.lat,
        "lon": aircraft.lon,
        "ground_speed_kts": aircraft.gs,
        "baro_rate": aircraft.baro_rate,
        "seen_pos": aircraft.seen_pos,
    })
}

fn sanitize_map(map: Map<String, Value>) -> Map<String, Value> {
    map.into_iter()
        .filter_map(|(key, value)| {
            if is_sensitive_key(&key) {
                None
            } else {
                Some((key, sanitize_value(value)))
            }
        })
        .collect()
}

fn sanitize_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(sanitize_map(map)),
        Value::Array(values) => Value::Array(values.into_iter().map(sanitize_value).collect()),
        Value::String(value) => Value::String(sanitize_string(&value)),
        other => other,
    }
}

fn sanitize_string(value: &str) -> String {
    let mut sanitized = value.to_string();
    for key in ["access_key", "api_key", "token", "authorization"] {
        sanitized = redact_query_value(&sanitized, key);
    }
    sanitized
}

fn redact_query_value(value: &str, key: &str) -> String {
    // `key` is lowercase; match the parameter name case-insensitively (e.g.
    // `Access_Key=`) by searching a lowercased copy. ASCII-lowercasing keeps
    // byte offsets identical, so the indices still slice the original `value`
    // and preserve its casing in the output.
    let needle = format!("{key}=");
    let haystack = value.to_ascii_lowercase();
    let mut out = String::new();
    let mut pos = 0;
    while let Some(rel) = haystack[pos..].find(&needle) {
        let key_end = pos + rel + needle.len();
        out.push_str(&value[pos..key_end]);
        out.push_str("<redacted>");
        // Drop the value up to the next '&' (kept) or the end of the string.
        match value[key_end..].find('&') {
            Some(amp) => pos = key_end + amp,
            None => return out,
        }
    }
    out.push_str(&value[pos..]);
    out
}

fn is_sensitive_key(key: &str) -> bool {
    [
        "access_key",
        "api_key",
        "authorization",
        "token",
        "raw_body",
        "body",
    ]
    .iter()
    .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
}

#[cfg(test)]
mod tests {
    use super::{FlightTrackerDebugEvent, append_debug_event};
    use crate::aviation::tracker::test_support::dt;
    use serde_json::json;

    #[tokio::test]
    async fn prune_debug_journals_keeps_newest_n_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("flight-tracker-debug");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        for date in [
            "2026-01-01",
            "2026-01-02",
            "2026-01-03",
            "2026-01-04",
            "2026-01-05",
        ] {
            tokio::fs::write(dir.join(format!("{date}.jsonl")), b"{}\n")
                .await
                .unwrap();
        }
        // A non-journal file must be left untouched.
        tokio::fs::write(dir.join("notes.txt"), b"keep me")
            .await
            .unwrap();

        super::prune_debug_journals(temp.path(), 2).await;

        let mut names = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        assert_eq!(names, ["2026-01-04.jsonl", "2026-01-05.jsonl", "notes.txt"]);
    }

    #[tokio::test]
    async fn append_debug_event_writes_jsonl_partitioned_by_date() {
        let temp = tempfile::tempdir().unwrap();
        let now = dt("2026-04-18T10:00:00Z");
        let event = FlightTrackerDebugEvent::new("track_started", "DLH1234")
            .callsign("DLH1234")
            .hex("3C6589")
            .phase("Unknown")
            .target_confirmation("Pending")
            .payload(json!({"route": {"origin": "FRA", "destination": "MUC"}}));
        append_debug_event(temp.path(), now, event).await;
        let path = temp.path().join("flight-tracker-debug/2026-04-18.jsonl");
        let contents = tokio::fs::read_to_string(path).await.unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(value["event"], "track_started");
        assert_eq!(value["ts"], "2026-04-18T10:00:00Z");
        assert_eq!(value["identifier"], "DLH1234");
        assert_eq!(value["callsign"], "DLH1234");
        assert_eq!(value["hex"], "3C6589");
        assert_eq!(value["phase"], "Unknown");
        assert_eq!(value["target_confirmation"], "Pending");
        assert_eq!(value["route"]["destination"], "MUC");
    }

    #[tokio::test]
    async fn append_debug_event_never_persists_secrets_or_raw_bodies() {
        let temp = tempfile::tempdir().unwrap();
        let now = dt("2026-04-18T10:00:00Z");
        let event =
            FlightTrackerDebugEvent::new("aviationstack_metadata", "DLH1234").payload(json!({
                "provider": "aviationstack",
                "url": "https://api.example/flights?access_key=secret-value&flight_icao=DLH1234",
                "access_key": "secret-value",
                "raw_body": "{\"secret\":\"secret-value\"}",
                "body": "do not store this",
                "status": 403
            }));
        append_debug_event(temp.path(), now, event).await;
        let contents =
            tokio::fs::read_to_string(temp.path().join("flight-tracker-debug/2026-04-18.jsonl"))
                .await
                .unwrap();
        assert!(!contents.contains("secret-value"), "{contents}");
        assert!(!contents.contains("raw_body"), "{contents}");
        assert!(!contents.contains("do not store this"), "{contents}");
        assert!(contents.contains("access_key=<redacted>"), "{contents}");
    }
}
