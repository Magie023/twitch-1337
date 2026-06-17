use twitch_1337_core as twitch_1337;
mod common;

use std::time::Duration;

use chrono::Duration as ChronoDuration;
use common::{TestBot, TestBotBuilder, fake_clock::FakeClock, fake_transport};
use secrecy::SecretString;
use twitch_1337::aviation::AviationClient;
use twitch_1337::aviation::tracker::{
    FlightIdentifier, FlightPhase, FlightTrackerState, HexSource, TargetConfirmation,
    TrackedFlight, TrackerCommand, run_flight_tracker,
};
use twitch_1337::config::AviationstackBootstrap;
use twitch_1337::settings::overrides::SettingsOverrides;
use twitch_1337::twitch::ChatSender;
use twitch_irc::ClientConfig;
use twitch_irc::login::StaticLoginCredentials;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, ResponseTemplate};

fn enable_aviationstack(config: &mut twitch_1337::config::Configuration) {
    // Only the secret api_key lives in the bootstrap config; enabled/base_url/
    // timeout_secs are runtime settings that the test supplies via overrides.
    config.aviationstack = Some(AviationstackBootstrap {
        api_key: SecretString::new("test-key".into()),
    });
}

fn set_aviationstack_enabled(o: &mut SettingsOverrides) {
    o.aviationstack.enabled = Some(true);
}

async fn read_debug_journal(bot: &TestBot) -> Vec<serde_json::Value> {
    let dir = bot.data_dir.path().join("flight-tracker-debug");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
            let mut events = Vec::new();
            while let Some(entry) = entries.next_entry().await.unwrap() {
                if entry.path().extension().is_some_and(|ext| ext == "jsonl") {
                    let contents = tokio::fs::read_to_string(entry.path()).await.unwrap();
                    events.extend(
                        contents
                            .lines()
                            .map(|line| serde_json::from_str(line).unwrap()),
                    );
                }
            }
            if !events.is_empty() {
                return events;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("debug journal was not written at {}", dir.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn track_command_acknowledges_flight() {
    let bot = TestBotBuilder::new().spawn().await;

    // Stub every live ADS-B / adsbdb route the tracker might hit.
    // Both base URLs point to the same mock server in tests.
    Mock::given(method("GET"))
        .and(path_regex(r"^/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3c6589",
                "flight": "DLH1234",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 50.0,
                "lon": 8.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1234").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    // Expected ack: "Tracke DLH1234 Okayge" (plus route info if adsbdb responded).
    // Accept any ack that references the callsign or "track".
    assert!(
        ack.contains("DLH1234") || ack.to_lowercase().contains("track"),
        "expected track ack, got: {ack}"
    );
    let aviationstack_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/flights")
        .count();
    assert_eq!(aviationstack_requests, 0);

    bot.shutdown().await;
}

#[tokio::test]
async fn track_command_enriches_flight_from_aviationstack_once() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/3C6589"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6589",
                "flight": "DLH1234",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 50.0,
                "lon": 8.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1234"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": {
                    "iata": "LH1234",
                    "icao": "DLH1234",
                    "number": "1234"
                },
                "airline": {
                    "iata": "LH",
                    "icao": "DLH",
                    "name": "Lufthansa"
                },
                "departure": {
                    "iata": "FRA",
                    "icao": "EDDF",
                    "scheduled": "2026-04-18T09:45:00+00:00",
                    "actual": "2026-04-18T10:00:00+00:00",
                    "actual_runway": "2026-04-18T10:05:00+00:00"
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T11:00:00+00:00",
                    "actual": null
                },
                "aircraft": {
                    "icao24": "3c6589",
                    "icao": "A320"
                }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1234").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("FRA") && ack.contains("MUC"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let aviationstack_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/flights")
        .count();
    assert_eq!(aviationstack_requests, 1);

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.route, Some(("FRA".to_string(), "MUC".to_string())));
    assert_eq!(flight.hex.as_deref(), Some("3C6589"));
    assert_eq!(flight.hex_source, Some(HexSource::Adsb));
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::ConfirmedByCallsign
    );
    assert_eq!(
        flight.takeoff_at.map(|dt| dt.timestamp()),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-18T10:05:00+00:00")
                .unwrap()
                .timestamp()
        )
    );
    assert!(flight.aviationstack_checked);

    let events = read_debug_journal(&bot).await;
    assert!(events.iter().any(|event| {
        event["event"] == "aviationstack_metadata"
            && event["http"]["provider"] == "aviationstack"
            && event["http"]["outcome"] == "success"
            && event["metadata"]["departure_iata"] == "FRA"
            && event["metadata"]["arrival_iata"] == "MUC"
            && event["metadata"]["aircraft_icao24"] == "3c6589"
    }));
    assert!(events.iter().any(|event| {
        event["event"] == "track_started"
            && event["identifier"] == "DLH1234"
            && event["route"]["origin"] == "FRA"
            && event["route"]["destination"] == "MUC"
            && event["route"]["destination_coordinates_resolved"] == true
    }));
    let journal_text = events
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<String>();
    assert!(!journal_text.contains("test-key"));
    assert!(!journal_text.contains("access_key"));
    assert!(!journal_text.contains("raw_body"));

    bot.shutdown().await;
}

#[tokio::test]
async fn track_command_by_hex_promotes_observed_callsign_and_route() {
    let bot = TestBotBuilder::new().spawn().await;

    Mock::given(method("GET"))
        .and(path("/hex/3C6589"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6589",
                "flight": "DLH1234",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 50.0,
                "lon": 8.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/DLH1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "response": {
                "flightroute": {
                    "origin": { "iata_code": "FRA" },
                    "destination": { "iata_code": "MUC" }
                }
            }
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track 3C6589").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1234"), "got: {ack}");
    assert!(ack.contains("FRA") && ack.contains("MUC"), "got: {ack}");

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("DLH1234"));
    assert_eq!(flight.route, Some(("FRA".to_string(), "MUC".to_string())));
    assert_eq!(flight.hex.as_deref(), Some("3C6589"));
    assert_eq!(flight.hex_source, Some(HexSource::UserInput));
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::InferredByAssignedHex
    );

    let events = read_debug_journal(&bot).await;
    assert!(events.iter().any(|event| {
        event["event"] == "flight_route_lookup"
            && event["callsign"] == "DLH1234"
            && event["http"]["provider"] == "adsbdb"
            && event["http"]["outcome"] == "success"
            && event["origin_iata"] == "FRA"
            && event["destination_iata"] == "MUC"
            && event["destination_coordinates_resolved"] == true
    }));
    assert!(events.iter().any(|event| {
        event["event"] == "track_started"
            && event["hex"] == "3C6589"
            && event["callsign"] == "DLH1234"
    }));

    bot.shutdown().await;
}

#[tokio::test]
async fn aviationstack_resolved_future_flight_strict_chills_without_adsb_lookup() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_iata", "LH1929"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "LH1929", "icao": "DLH1929", "number": "1929" },
                "airline": { "iata": "LH", "icao": "DLH", "name": "Lufthansa" },
                "departure": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "scheduled": "2026-04-18T16:00:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T17:10:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": "3c6497", "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track LH1929").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1929"), "got: {ack}");
    assert!(ack.contains("BER") && ack.contains("MUC"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let requests = bot.adsb_mock.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/hex/3C6497")
            .count(),
        0,
        "strict chill must not query ADS-B more than 3h before departure"
    );

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("DLH1929"));
    assert_eq!(flight.hex.as_deref(), Some("3C6497"));
    assert_eq!(flight.hex_source, Some(HexSource::AviationStack));
    assert_eq!(flight.target_confirmation, TargetConfirmation::Pending);
    assert_eq!(flight.last_seen, None);
    assert_eq!(flight.last_adsb_poll_at, None);

    bot.shutdown().await;
}

#[tokio::test]
async fn info_reuses_aviationstack_cache_seeded_by_track() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_iata", "LH1929"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight_date": "2026-04-18",
                "flight_status": "scheduled",
                "flight": { "iata": "LH1929", "icao": "DLH1929", "number": "1929" },
                "airline": { "iata": "LH", "icao": "DLH", "name": "Lufthansa" },
                "departure": {
                    "airport": "Berlin Brandenburg Airport",
                    "iata": "BER",
                    "icao": "EDDB",
                    "terminal": "1",
                    "gate": "B10",
                    "scheduled": "2026-04-18T16:00:00+00:00",
                    "estimated": "2026-04-18T16:05:00+00:00",
                    "actual": null,
                    "actual_runway": null,
                    "delay": 5
                },
                "arrival": {
                    "airport": "Franz Josef Strauss",
                    "iata": "MUC",
                    "icao": "EDDM",
                    "terminal": "2",
                    "gate": "G28",
                    "baggage": "17",
                    "scheduled": "2026-04-18T17:10:00+00:00",
                    "estimated": "2026-04-18T17:15:00+00:00",
                    "actual": null
                },
                "aircraft": {
                    "registration": "D-AIDW",
                    "icao24": "3c6497",
                    "icao": "A320"
                }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track LH1929").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("Tracke DLH1929"), "got: {ack}");
    assert!(ack.contains("LH1929 Lufthansa"), "got: {ack}");
    assert!(
        ack.contains("BER T1 Gate B10 -> MUC T2 Gate G28"),
        "got: {ack}"
    );
    assert!(ack.contains("Dep: 16:05"), "got: {ack}");
    assert!(ack.contains("Arr: 17:15"), "got: {ack}");
    assert!(ack.contains("Baggage: 17"), "got: {ack}");
    assert!(ack.contains("Aircraft: A320 D-AIDW"), "got: {ack}");
    assert!(ack.contains("ICAO24: 3C6497"), "got: {ack}");

    bot.send("bob", "!info DLH1929").await;
    let info = bot.expect_say(Duration::from_secs(5)).await;
    assert!(info.contains("LH1929 Lufthansa"), "got: {info}");
    assert!(
        info.contains("BER T1 Gate B10 -> MUC T2 Gate G28"),
        "got: {info}"
    );
    assert!(info.contains("Baggage: 17"), "got: {info}");
    assert!(info.contains("Status: scheduled"), "got: {info}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let aviationstack_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/flights")
        .count();
    assert_eq!(
        aviationstack_requests, 1,
        "!info should reuse cached AviationStack data from !track"
    );

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    assert_eq!(state.flight_info_cache.len(), 1);
    assert!(state.flight_info_cache[0].metadata.is_some());
    assert!(
        state.flight_info_cache[0]
            .aliases
            .iter()
            .any(|alias| alias == "LH1929")
    );
    assert!(
        state.flight_info_cache[0]
            .aliases
            .iter()
            .any(|alias| alias == "DLH1929")
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn aviationstack_mismatched_adsb_callsign_observes_aircraft_without_confirming_target() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1929"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "LH1929", "icao": "DLH1929", "number": "1929" },
                "airline": { "iata": "LH", "icao": "DLH", "name": "Lufthansa" },
                "departure": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "scheduled": "2026-04-18T12:00:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T13:10:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": "3c6497", "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6497",
                "flight": "DLH9999",
                "alt_baro": 8000,
                "gs": 250.0,
                "baro_rate": 500,
                "lat": 52.3,
                "lon": 13.4,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1929").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1929"), "got: {ack}");
    bot.expect_silent(Duration::from_millis(200)).await;

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.last_seen, None);
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::AircraftVisible
    );
    assert_eq!(flight.observed_callsign.as_deref(), Some("DLH9999"));
    assert_eq!(flight.callsign.as_deref(), Some("DLH1929"));
    assert_eq!(flight.hex_source, Some(HexSource::AviationStack));

    bot.shutdown().await;
}

#[tokio::test]
async fn aviationstack_codeshare_confirms_operating_callsign_alias() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_iata", "AF1234"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "AF1234", "icao": "KLM1234", "number": "1234" },
                "airline": { "iata": "KL", "icao": "KLM", "name": "KLM" },
                "departure": {
                    "iata": "CDG",
                    "icao": "LFPG",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "AMS",
                    "icao": "EHAM",
                    "estimated": "2026-04-18T11:45:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": "3c1234", "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/3C1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C1234",
                "flight": "AFR1234",
                "alt_baro": 12000,
                "gs": 280.0,
                "baro_rate": 1200,
                "lat": 49.0,
                "lon": 3.0,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track AF1234").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("KLM1234"), "got: {ack}");

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("KLM1234"));
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::ConfirmedByCallsign
    );
    assert_eq!(flight.observed_callsign.as_deref(), Some("AFR1234"));
    assert!(flight.alias_callsigns.iter().any(|alias| alias == "AF1234"));
    assert!(
        flight
            .alias_callsigns
            .iter()
            .any(|alias| alias == "AFR1234")
    );
    assert!(
        flight
            .alias_callsigns
            .iter()
            .any(|alias| alias == "KLM1234")
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn duplicate_tracking_rejects_existing_operating_alias() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_iata", "AF1234"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "AF1234", "icao": "KLM1234", "number": "1234" },
                "airline": { "iata": "KL", "icao": "KLM", "name": "KLM" },
                "departure": {
                    "iata": "CDG",
                    "icao": "LFPG",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "AMS",
                    "icao": "EHAM",
                    "estimated": "2026-04-18T11:45:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": null, "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/KLM1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track AF1234").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("KLM1234"), "got: {ack}");

    bot.send("bob", "!track AFR1234").await;
    let duplicate = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        duplicate.contains("wird schon getrackt"),
        "got: {duplicate}"
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn confirmed_aviationstack_hex_with_later_mismatched_callsign_suppresses_target_messages() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1929"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "LH1929", "icao": "DLH1929", "number": "1929" },
                "airline": { "iata": "LH", "icao": "DLH", "name": "Lufthansa" },
                "departure": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "scheduled": "2026-04-18T12:00:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T13:10:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": "3c6497", "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6497",
                "flight": "DLH1929",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 52.3,
                "lon": 13.4,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .up_to_n_times(1)
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6497",
                "flight": "DLH9999",
                "alt_baro": 12000,
                "gs": 280.0,
                "baro_rate": -1800,
                "lat": 52.4,
                "lon": 13.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1929").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1929"), "got: {ack}");

    bot.clock.advance(ChronoDuration::seconds(30));
    bot.expect_silent(Duration::from_millis(300)).await;

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(&state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::ConfirmedByCallsign
    );
    assert_eq!(flight.observed_callsign.as_deref(), Some("DLH9999"));
    assert_eq!(flight.callsign.as_deref(), Some("DLH1929"));
    assert_eq!(flight.hex_source, Some(HexSource::Adsb));

    bot.clock.advance(ChronoDuration::minutes(31));
    bot.expect_silent(Duration::from_millis(300)).await;

    let persisted = tokio::fs::read_to_string(&state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state
        .flights
        .first()
        .expect("visible assigned hex should remain tracked");
    assert_eq!(flight.observed_callsign.as_deref(), Some("DLH9999"));
    assert_eq!(flight.callsign.as_deref(), Some("DLH1929"));
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::AircraftVisible
    );

    bot.shutdown().await;
}

/// A flight whose assigned hex stays visible (with a mismatched callsign, so it
/// sits in `AircraftVisible`) freezes `last_seen` because that anchor only moves
/// on direct target confirmation. A separate `last_visible_at` anchor must keep
/// the tracking-lost removal timer alive: a single transient empty ADS-B poll
/// must NOT instantly remove the flight, but a sustained absence past
/// `TRACKING_LOST_REMOVAL` still removes it and announces the loss.
#[tokio::test]
async fn visible_assigned_hex_then_one_empty_poll_keeps_flight_within_grace_window() {
    // Start well clear of 13:37 Berlin so the long clock advances below never
    // trip the 1337 reminder; 14:00Z is 16:00 Berlin in April (CEST).
    let bot = TestBotBuilder::new()
        .at("2026-04-18T14:00:00Z".parse().unwrap())
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1929"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "LH1929", "icao": "DLH1929", "number": "1929" },
                "airline": { "iata": "LH", "icao": "DLH", "name": "Lufthansa" },
                "departure": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "scheduled": "2026-04-18T12:00:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T13:10:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": "3c6497", "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    // Initial confirm: assigned hex with the matching callsign.
    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6497",
                "flight": "DLH1929",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 52.3,
                "lon": 13.4,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .up_to_n_times(1)
        .mount(&bot.adsb_mock)
        .await;

    // Long visible stretch: assigned hex still present but the callsign is now
    // mismatched, so the flight sits in AircraftVisible and `last_seen` freezes.
    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6497",
                "flight": "DLH9999",
                "alt_baro": 12000,
                "gs": 280.0,
                "baro_rate": -1800,
                "lat": 52.4,
                "lon": 13.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .up_to_n_times(5)
        .mount(&bot.adsb_mock)
        .await;

    // After the visible stretch, the aircraft disappears from ADS-B.
    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1929").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1929"), "got: {ack}");

    let state_path = bot.data_dir.path().join("flights.ron");

    // Drive five visible polls, advancing 7 minutes each so the total stretch
    // (~35 min) pushes `last_seen` far past TRACKING_LOST_REMOVAL (1800s). With
    // `last_visible_at` driving the removal timer, the flight stays tracked.
    for _ in 0..5 {
        bot.clock.advance(ChronoDuration::minutes(7));
        bot.expect_silent(Duration::from_millis(200)).await;
    }

    let persisted = tokio::fs::read_to_string(&state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state
        .flights
        .first()
        .expect("visible assigned hex should remain tracked");
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::AircraftVisible
    );

    // One transient empty poll. The flight must survive the grace window: it is
    // still tracked, still AircraftVisible, and NO "Signal verloren" was sent.
    bot.clock.advance(ChronoDuration::minutes(7));
    bot.expect_silent(Duration::from_millis(300)).await;

    let persisted = tokio::fs::read_to_string(&state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state
        .flights
        .first()
        .expect("one transient empty poll must not remove the flight");
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::AircraftVisible
    );

    // Keep returning empty past TRACKING_LOST_REMOVAL measured from the last
    // visible poll. The flight is removed and the loss is announced.
    bot.clock.advance(ChronoDuration::minutes(31));
    let lost = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        lost.contains("Signal verloren"),
        "expected tracking-lost message, got: {lost}"
    );

    // The removal message is sent just before the state is persisted; give the
    // atomic save a moment to flush before reading the file back.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let persisted = tokio::fs::read_to_string(&state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    assert!(
        state.flights.is_empty(),
        "flight should be removed after sustained ADS-B absence"
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn aviationstack_hex_without_adsb_callsign_is_inferred_in_departure_window() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1929"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "LH1929", "icao": "DLH1929", "number": "1929" },
                "airline": { "iata": "LH", "icao": "DLH", "name": "Lufthansa" },
                "departure": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "scheduled": "2026-04-18T11:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T12:40:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": "3c6497", "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6497",
                "alt_baro": "ground",
                "gs": 0.0,
                "lat": 52.3,
                "lon": 13.4,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1929").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1929"), "got: {ack}");

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::InferredByAssignedHex
    );
    assert_eq!(flight.observed_callsign, None);
    assert_eq!(flight.hex_source, Some(HexSource::Adsb));

    bot.shutdown().await;
}

#[tokio::test]
async fn aviationstack_hex_without_adsb_callsign_is_not_inferred_after_window() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1929"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "LH1929", "icao": "DLH1929", "number": "1929" },
                "airline": { "iata": "LH", "icao": "DLH", "name": "Lufthansa" },
                "departure": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "scheduled": "2026-04-18T07:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "MUC",
                    "icao": "EDDM",
                    "estimated": "2026-04-18T08:40:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": "3c6497", "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/3C6497"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3C6497",
                "alt_baro": 12000,
                "gs": 280.0,
                "baro_rate": 0,
                "lat": 52.3,
                "lon": 13.4,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1929").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1929"), "got: {ack}");

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::AircraftVisible
    );
    assert_eq!(flight.observed_callsign, None);
    assert_eq!(flight.last_seen, None);
    assert_eq!(flight.hex_source, Some(HexSource::AviationStack));

    bot.shutdown().await;
}

#[tokio::test]
async fn aviationstack_miss_falls_back_to_adsb_only_pending_tracking() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "DLH1234"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": []
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/DLH1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DLH1234").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("DLH1234"), "got: {ack}");
    assert!(ack.contains("ADS-B-only"), "got: {ack}");

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("DLH1234"));
    assert_eq!(flight.hex, None);
    assert_eq!(flight.target_confirmation, TargetConfirmation::Pending);
    assert_eq!(flight.last_seen, None);
    assert!(flight.last_adsb_poll_at.is_some());

    bot.shutdown().await;
}

#[tokio::test]
async fn track_command_accepts_aviationstack_flight_before_adsb_appears() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "EIN336"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": {
                    "iata": "EI336",
                    "icao": "EIN336",
                    "number": "336"
                },
                "airline": {
                    "iata": "EI",
                    "icao": "EIN",
                    "name": "Aer Lingus"
                },
                "departure": {
                    "iata": "DUB",
                    "icao": "EIDW",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "estimated": "2026-04-18T12:45:00+00:00",
                    "actual": null
                },
                "aircraft": {
                    "icao24": null,
                    "icao": "A320"
                }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track EIN336").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("EIN336"), "got: {ack}");
    assert!(ack.contains("DUB") && ack.contains("BER"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("EIN336"));
    assert_eq!(flight.route, Some(("DUB".to_string(), "BER".to_string())));
    assert_eq!(flight.aircraft_type.as_deref(), Some("A320"));
    assert_eq!(flight.last_seen, None);
    assert_eq!(
        flight.scheduled_departure_at.map(|dt| dt.timestamp()),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-18T10:30:00+00:00")
                .unwrap()
                .timestamp()
        )
    );
    assert_eq!(
        flight.last_adsb_poll_at.map(|dt| dt.timestamp()),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-18T11:00:00+00:00")
                .unwrap()
                .timestamp()
        )
    );
    assert_eq!(
        flight.phase,
        twitch_1337::aviation::tracker::FlightPhase::Unknown
    );
    assert!(flight.aviationstack_checked);

    let callsign_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/callsign/EIN336")
        .count();
    assert_eq!(
        callsign_requests, 1,
        "pending flight should not be repolled immediately"
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn track_command_keeps_pending_flight_with_stale_scheduled_departure() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/hex/48C2A2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_iata", "FR196"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": {
                    "iata": "FR196",
                    "icao": "RYR196",
                    "number": "196"
                },
                "airline": {
                    "iata": "FR",
                    "icao": "RYR",
                    "name": "Ryanair"
                },
                "departure": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "scheduled": "2026-04-17T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "BUD",
                    "icao": "LHBP",
                    "estimated": "2026-04-17T12:00:00+00:00",
                    "actual": null
                },
                "aircraft": {
                    "icao24": "48c2a2",
                    "icao": "B38M"
                }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track FR196").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("RYR196"), "got: {ack}");
    assert!(ack.contains("BER") && ack.contains("BUD"), "got: {ack}");

    bot.expect_silent(Duration::from_millis(200)).await;

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("RYR196"));
    assert_eq!(flight.hex.as_deref(), Some("48C2A2"));
    assert_eq!(flight.hex_source, Some(HexSource::AviationStack));
    assert_eq!(flight.route, Some(("BER".to_string(), "BUD".to_string())));
    assert_eq!(flight.aircraft_type.as_deref(), Some("B38M"));
    assert_eq!(flight.last_seen, None);
    assert_eq!(
        flight.scheduled_departure_at.map(|dt| dt.timestamp()),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-17T10:30:00+00:00")
                .unwrap()
                .timestamp()
        )
    );

    let hex_requests = bot
        .adsb_mock
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/hex/48C2A2")
        .count();
    assert_eq!(
        hex_requests, 1,
        "stale pending flight should not be repolled immediately"
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn pending_flight_polls_when_due_and_becomes_visible() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .up_to_n_times(1)
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "4ca123",
                "flight": "EIN336",
                "alt_baro": 12000,
                "gs": 280.0,
                "baro_rate": 1200,
                "lat": 52.3,
                "lon": 13.4,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "EIN336"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "EI336", "icao": "EIN336", "number": "336" },
                "airline": { "iata": "EI", "icao": "EIN", "name": "Aer Lingus" },
                "departure": {
                    "iata": "DUB",
                    "icao": "EIDW",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "estimated": "2026-04-18T12:45:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": null, "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track EIN336").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("EIN336"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(50)).await;
    bot.clock.advance(ChronoDuration::minutes(2));

    let visible = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        visible.contains("EIN336") && visible.contains("ADS-B sichtbar"),
        "got: {visible}"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    let requests = bot.adsb_mock.received_requests().await.unwrap();
    let callsign_requests = requests
        .iter()
        .filter(|request| request.url.path() == "/callsign/EIN336")
        .count();
    assert_eq!(callsign_requests, 2);

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert!(flight.last_seen.is_some());
    assert_eq!(flight.hex.as_deref(), Some("4CA123"));
    assert_eq!(flight.hex_source, Some(HexSource::Adsb));
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::ConfirmedByCallsign
    );

    bot.shutdown().await;
}

#[tokio::test]
async fn pending_flight_expires_without_extra_adsb_call() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    Mock::given(method("GET"))
        .and(path("/callsign/EIN336"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("access_key", "test-key"))
        .and(query_param("flight_icao", "EIN336"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "EI336", "icao": "EIN336", "number": "336" },
                "airline": { "iata": "EI", "icao": "EIN", "name": "Aer Lingus" },
                "departure": {
                    "iata": "DUB",
                    "icao": "EIDW",
                    "scheduled": "2026-04-18T10:30:00+00:00",
                    "actual": null,
                    "actual_runway": null
                },
                "arrival": {
                    "iata": "BER",
                    "icao": "EDDB",
                    "estimated": "2026-04-18T12:45:00+00:00",
                    "actual": null
                },
                "aircraft": { "icao24": null, "icao": "A320" }
            }]
        })))
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track EIN336").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(ack.contains("EIN336"), "got: {ack}");

    tokio::time::sleep(Duration::from_millis(50)).await;
    bot.clock.advance(ChronoDuration::hours(13));

    let expired = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        expired.contains("EIN336") && expired.contains("nicht im ADS-B aufgetaucht"),
        "got: {expired}"
    );

    let requests = bot.adsb_mock.received_requests().await.unwrap();
    let callsign_requests = requests
        .iter()
        .filter(|request| request.url.path() == "/callsign/EIN336")
        .count();
    assert_eq!(callsign_requests, 1);

    let state_path = bot.data_dir.path().join("flights.ron");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let persisted = tokio::fs::read_to_string(&state_path).await.unwrap();
        let state: twitch_1337::aviation::tracker::FlightTrackerState =
            ron::from_str(&persisted).unwrap();
        if state.flights.is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("flight tracker did not clear expired flight within 5s: {state:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let events = read_debug_journal(&bot).await;
    assert!(events.iter().any(|event| {
        event["event"] == "tracking_removal" && event["reason"] == "pending_expired"
    }));

    bot.shutdown().await;
}

#[tokio::test]
async fn tracker_answers_dashboard_commands_while_polling() {
    twitch_1337::install_crypto_provider();
    let now = chrono::DateTime::parse_from_rfc3339("2026-04-18T11:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let data_dir = tempfile::TempDir::new().unwrap();
    let state = FlightTrackerState {
        flights: vec![TrackedFlight {
            identifier: FlightIdentifier::Callsign("DLH1234".to_string()),
            callsign: Some("DLH1234".to_string()),
            alias_callsigns: vec!["DLH1234".to_string()],
            hex: None,
            hex_source: None,
            observed_callsign: None,
            target_confirmation: TargetConfirmation::ConfirmedByCallsign,
            phase: FlightPhase::Cruise,
            route: None,
            aircraft_type: None,
            altitude_ft: Some(35_000),
            vertical_rate_fpm: None,
            ground_speed_kts: Some(450.0),
            lat: None,
            lon: None,
            squawk: None,
            tracked_by: "alice".to_string(),
            tracked_at: now,
            last_seen: Some(now),
            last_visible_at: Some(now),
            last_phase_change: None,
            polls_since_change: 0,
            takeoff_at: None,
            aviationstack_checked: false,
            scheduled_departure_at: None,
            last_adsb_poll_at: None,
            divert_consecutive_polls: 0,
            dest_lat: None,
            dest_lon: None,
        }],
        flight_info_cache: Vec::new(),
    };
    tokio::fs::write(
        data_dir.path().join("flights.ron"),
        ron::ser::to_string(&state).unwrap(),
    )
    .await
    .unwrap();

    let adsb_mock = wiremock::MockServer::start().await;
    let nominatim_mock = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/callsign/DLH1234"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(1_500))
                .set_body_json(serde_json::json!({
                    "ac": [{
                        "hex": "3C6589",
                        "flight": "DLH1234",
                        "alt_baro": 35000,
                        "gs": 450.0,
                        "baro_rate": 0,
                        "lat": 50.0,
                        "lon": 8.5,
                        "squawk": "1000"
                    }],
                    "ctime": 0,
                    "now": 0,
                    "total": 1
                })),
        )
        .mount(&adsb_mock)
        .await;

    let _transport = fake_transport::install().await;
    let client_cfg = ClientConfig::new_simple(StaticLoginCredentials::new(
        "bot".to_owned(),
        Some("test-token".to_owned()),
    ));
    let (_incoming, client) = twitch_irc::TwitchIRCClient::<
        fake_transport::FakeTransport,
        StaticLoginCredentials,
    >::new(client_cfg);
    let client = std::sync::Arc::new(client);
    client.join("test_channel".to_string()).expect("join");
    let sender = ChatSender::new(client);
    let aviation = AviationClient::new_with_base_url(
        adsb_mock.uri(),
        adsb_mock.uri(),
        nominatim_mock.uri(),
        reqwest::Client::new(),
    );
    let clock = FakeClock::new(now);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let tracker_task = tokio::spawn(run_flight_tracker(
        rx,
        sender,
        "test_channel".to_string(),
        aviation,
        data_dir.path().to_path_buf(),
        clock,
    ));

    tokio::time::sleep(Duration::from_millis(100)).await;

    let (snapshot_tx, snapshot_rx) = tokio::sync::oneshot::channel();
    tx.send(TrackerCommand::Snapshot { reply: snapshot_tx })
        .await
        .unwrap();
    let snapshot = tokio::time::timeout(Duration::from_millis(500), snapshot_rx)
        .await
        .expect("snapshot should not wait for ADS-B poll")
        .unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].identifier, "DLH1234");

    let (delete_tx, delete_rx) = tokio::sync::oneshot::channel();
    tx.send(TrackerCommand::DeleteFromWeb {
        identifier: "DLH1234".to_string(),
        reply: delete_tx,
    })
    .await
    .unwrap();
    let removed = tokio::time::timeout(Duration::from_millis(500), delete_rx)
        .await
        .expect("delete should not wait for ADS-B poll")
        .unwrap();
    assert_eq!(removed.as_deref(), Some("DLH1234"));

    drop(tx);
    tokio::time::timeout(Duration::from_secs(3), tracker_task)
        .await
        .expect("tracker should shut down")
        .unwrap();
}

/// Repro: `!track DE1513` (IATA, Condor) must find the aircraft that broadcasts
/// the resolved ICAO callsign `CFG1513` on ADS-B, exactly like `!track CFG1513`.
#[tokio::test]
async fn track_iata_flight_number_resolves_to_icao_callsign_on_adsb() {
    let bot = TestBotBuilder::new().spawn().await;

    // Only the resolved ICAO callsign appears on ADS-B.
    Mock::given(method("GET"))
        .and(path("/callsign/CFG1513"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [{
                "hex": "3c6589",
                "flight": "CFG1513",
                "alt_baro": 35000,
                "gs": 450.0,
                "baro_rate": 0,
                "lat": 50.0,
                "lon": 8.5,
                "squawk": "1000"
            }],
            "ctime": 0,
            "now": 0,
            "total": 1
        })))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;

    // Everything else (incl. /callsign/DE1513 and /hex/DE1513) is empty.
    Mock::given(method("GET"))
        .and(path_regex(r"^/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ac": [],
            "ctime": 0,
            "now": 0,
            "total": 0
        })))
        .with_priority(10)
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DE1513").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        ack.contains("CFG1513") && !ack.to_lowercase().contains("nicht gefunden"),
        "expected DE1513 to resolve to CFG1513 and be found, got: {ack}"
    );

    let state_path = bot.data_dir.path().join("flights.ron");
    let persisted = tokio::fs::read_to_string(state_path).await.unwrap();
    let state: twitch_1337::aviation::tracker::FlightTrackerState =
        ron::from_str(&persisted).unwrap();
    let flight = state.flights.first().expect("persisted flight");
    assert_eq!(flight.callsign.as_deref(), Some("CFG1513"));
    assert_eq!(
        flight.target_confirmation,
        TargetConfirmation::ConfirmedByCallsign
    );

    bot.shutdown().await;
}

/// Repro: with a resolvable IATA flight number (`DE1513` -> `CFG1513`), the
/// aviationstack enrichment must query by the resolved ICAO callsign
/// (`flight_icao=CFG1513`) when the raw `flight_iata=DE1513` query returns
/// nothing — codeshare/marketing numbers often only resolve under the
/// operating ICAO identity.
#[tokio::test]
async fn track_iata_flight_number_enriches_via_resolved_icao_query() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    // aviationstack only knows the flight under its operating ICAO identity.
    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("flight_icao", "CFG1513"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "DE1513", "icao": "CFG1513", "number": "1513" },
                "airline": { "iata": "DE", "icao": "CFG", "name": "Condor" },
                "departure": { "iata": "FRA", "icao": "EDDF", "scheduled": null, "actual": null, "actual_runway": null },
                "arrival": { "iata": "PMI", "icao": "LEPA", "estimated": null, "actual": null },
                "aircraft": { "icao24": "3c6589", "icao": "A320" }
            }]
        })))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;

    // The plane is live on ADS-B under CFG1513 (same aircraft via callsign or hex).
    let cfg1513 = serde_json::json!({
        "ac": [{ "hex": "3c6589", "flight": "CFG1513", "alt_baro": 35000, "gs": 450.0, "baro_rate": 0, "lat": 50.0, "lon": 8.5, "squawk": "1000" }],
        "ctime": 0, "now": 0, "total": 1
    });
    Mock::given(method("GET"))
        .and(path("/callsign/CFG1513"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cfg1513.clone()))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/hex/3C6589"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cfg1513))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;

    // Anything else (incl. flight_iata=DE1513) is empty.
    Mock::given(method("GET"))
        .and(path_regex(r"^/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [], "ac": [], "ctime": 0, "now": 0, "total": 0
        })))
        .with_priority(10)
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DE1513").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        ack.contains("FRA") && ack.contains("PMI"),
        "DE1513 should enrich via flight_icao=CFG1513 and show the route; got: {ack}"
    );

    bot.shutdown().await;
}

/// A transient (5xx) failure on the primary IATA query must not abort the
/// lookup before the resolved ICAO fallback (`flight_icao=CFG1513`) is tried.
#[tokio::test]
async fn track_iata_query_5xx_still_tries_icao_fallback() {
    let bot = TestBotBuilder::new()
        .with_config(enable_aviationstack)
        .with_settings(set_aviationstack_enabled)
        .spawn()
        .await;

    // Primary IATA query fails transiently...
    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("flight_iata", "DE1513"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;
    // ...but the ICAO fallback resolves.
    Mock::given(method("GET"))
        .and(path("/flights"))
        .and(query_param("flight_icao", "CFG1513"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "flight": { "iata": "DE1513", "icao": "CFG1513", "number": "1513" },
                "airline": { "iata": "DE", "icao": "CFG", "name": "Condor" },
                "departure": { "iata": "FRA", "icao": "EDDF", "scheduled": null, "actual": null, "actual_runway": null },
                "arrival": { "iata": "PMI", "icao": "LEPA", "estimated": null, "actual": null },
                "aircraft": { "icao24": "3c6589", "icao": "A320" }
            }]
        })))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;

    let cfg1513 = serde_json::json!({
        "ac": [{ "hex": "3c6589", "flight": "CFG1513", "alt_baro": 35000, "gs": 450.0, "baro_rate": 0, "lat": 50.0, "lon": 8.5, "squawk": "1000" }],
        "ctime": 0, "now": 0, "total": 1
    });
    Mock::given(method("GET"))
        .and(path("/callsign/CFG1513"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cfg1513.clone()))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/hex/3C6589"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cfg1513))
        .with_priority(1)
        .mount(&bot.adsb_mock)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [], "ac": [], "ctime": 0, "now": 0, "total": 0
        })))
        .with_priority(10)
        .mount(&bot.adsb_mock)
        .await;

    let mut bot = bot;
    bot.send("alice", "!track DE1513").await;
    let ack = bot.expect_say(Duration::from_secs(5)).await;
    assert!(
        ack.contains("FRA") && ack.contains("PMI"),
        "a 5xx on flight_iata=DE1513 should fall through to flight_icao=CFG1513; got: {ack}"
    );

    bot.shutdown().await;
}
