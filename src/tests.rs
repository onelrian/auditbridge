use crate::config::Config;
use crate::cursor::{self, SinkCursor};
use crate::metrics::{router, Metrics};
use crate::models::Event;
use crate::netbird::NetbirdClient;
use crate::retry::{with_retry, RetryConfig};
use crate::sinks::encoding::Encoding;
use crate::sinks::http::HttpSink;
use crate::sinks::syslog::{SyslogProtocol, SyslogSink};
use crate::sinks::Sink;
use crate::{build_initial_cursors, process_cycle, run};
use chrono::{DateTime, Utc};
use reqwest::Method;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// No retries: keeps process_cycle tests focused on watermark behavior,
// not on how many times a deliberately-failing mock gets hit.
fn no_retry() -> RetryConfig {
    RetryConfig {
        max_attempts: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    }
}

fn test_config() -> Config {
    Config {
        netbird_api_url: "http://localhost".to_string(),
        netbird_api_token: "token".to_string(),
        check_interval: Duration::from_millis(10),
        sinks: vec![],
        cursor_file: None,
        retry: no_retry(),
        batch_size: 500,
        metrics_port: 0,
    }
}

fn sample_event(id: &str) -> Event {
    Event {
        id: id.to_string(),
        timestamp: "2023-01-01T00:00:00Z".to_string(),
        activity: "test_activity".to_string(),
        activity_code: "test.activity".to_string(),
        initiator_id: Some("init1".to_string()),
        initiator_email: Some("admin@example.com".to_string()),
        initiator_name: Some("Admin".to_string()),
        target_id: Some("target1".to_string()),
        account_id: Some("acc1".to_string()),
        meta: None,
    }
}

#[tokio::test]
async fn test_netbird_fetch_events() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![sample_event("1")]))
        .mount(&mock_server)
        .await;

    let client = NetbirdClient::new(mock_server.uri(), "fake_token".to_string());
    let events = client.fetch_events().await.expect("Failed to fetch events");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, "1");
    assert_eq!(events[0].activity, "test_activity");
}

#[tokio::test]
async fn test_http_sink_loki_encoding() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/loki/api/v1/push"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock_server)
        .await;

    let sink = HttpSink::new(
        "loki".to_string(),
        format!("{}/loki/api/v1/push", mock_server.uri()),
        Method::POST,
        vec![],
        Encoding::Loki,
    );
    sink.send(&[sample_event("1")])
        .await
        .expect("Failed to send events");
}

#[tokio::test]
async fn test_http_sink_json_encoding_with_custom_headers() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    let sink = HttpSink::new(
        "generic-webhook".to_string(),
        format!("{}/ingest", mock_server.uri()),
        Method::POST,
        vec![("X-Api-Key".to_string(), "secret".to_string())],
        Encoding::Json,
    );
    sink.send(&[sample_event("1")])
        .await
        .expect("Failed to send events");
}

#[tokio::test]
async fn test_syslog_sink_rfc3164_over_tcp() {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        socket.read_to_end(&mut buf).await.ok();
        buf
    });

    let sink = SyslogSink::new(
        "wazuh".to_string(),
        addr.to_string(),
        SyslogProtocol::Tcp,
        Encoding::Syslog3164,
    );
    sink.send(&[sample_event("1")])
        .await
        .expect("Failed to send events");
    drop(sink);

    let received = tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("syslog mock server timed out")
        .expect("syslog mock server task panicked");
    let text = String::from_utf8(received).unwrap();

    assert!(
        text.starts_with("<134>"),
        "expected syslog PRI framing, got: {}",
        text
    );
    assert!(text.contains("test.activity"));
}

#[tokio::test]
async fn test_process_cycle_does_not_advance_watermark_on_send_failure() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![sample_event("1")]))
        .mount(&nb_mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![Box::new(HttpSink::new(
        "flaky".to_string(),
        format!("{}/ingest", sink_mock.uri()),
        Method::POST,
        vec![],
        Encoding::Json,
    ))];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    assert_eq!(
        cursors.get("flaky").and_then(|c| c.watermark),
        None,
        "watermark must not advance when the sink write fails"
    );
}

#[tokio::test]
async fn test_process_cycle_advances_watermark_on_send_success() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![sample_event("1")]))
        .mount(&nb_mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![Box::new(HttpSink::new(
        "generic".to_string(),
        format!("{}/ingest", sink_mock.uri()),
        Method::POST,
        vec![],
        Encoding::Json,
    ))];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    assert!(
        cursors.get("generic").and_then(|c| c.watermark).is_some(),
        "watermark must advance once delivery is confirmed"
    );
}

#[tokio::test]
async fn test_process_cycle_one_failing_sink_does_not_block_the_other() {
    let nb_mock = MockServer::start().await;
    let down_mock = MockServer::start().await;
    let up_mock = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![sample_event("1")]))
        .mount(&nb_mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&down_mock)
        .await;

    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&up_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![
        Box::new(HttpSink::new(
            "down".to_string(),
            format!("{}/ingest", down_mock.uri()),
            Method::POST,
            vec![],
            Encoding::Json,
        )),
        Box::new(HttpSink::new(
            "up".to_string(),
            format!("{}/ingest", up_mock.uri()),
            Method::POST,
            vec![],
            Encoding::Json,
        )),
    ];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    assert_eq!(cursors.get("down").and_then(|c| c.watermark), None);
    assert!(cursors.get("up").and_then(|c| c.watermark).is_some());
}

#[test]
fn test_config_defaults_to_loki_sink() {
    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("SINKS", None),
            ("LOKI_URL", None),
        ],
        || {
            let config = Config::from_env().unwrap();
            assert_eq!(config.netbird_api_token, "test_token");
            assert_eq!(config.sinks.len(), 1);
            assert_eq!(config.sinks[0].name, "loki");
        },
    );
}

#[test]
fn test_config_wazuh_requires_addr() {
    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("SINKS", Some("wazuh")),
            ("SINK_WAZUH_ADDR", None),
            ("WAZUH_ADDR", None),
        ],
        || {
            let result = Config::from_env();
            assert!(
                result.is_err(),
                "expected an error when no Wazuh address is set"
            );
        },
    );
}

#[test]
fn test_config_generic_sink_requires_explicit_transport_and_encoding() {
    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("SINKS", Some("datadog")),
            (
                "SINK_DATADOG_URL",
                Some("https://http-intake.example/v1/logs"),
            ),
            ("SINK_DATADOG_ENCODING", Some("json")),
            // no SINK_DATADOG_TRANSPORT on purpose: unknown sink names get no defaults.
        ],
        || {
            let result = Config::from_env();
            assert!(
                result.is_err(),
                "a sink name with no built-in preset must require an explicit transport"
            );
        },
    );
}

#[test]
fn test_config_generic_http_sink_via_env_only() {
    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("SINKS", Some("loki,datadog")),
            ("SINK_DATADOG_TRANSPORT", Some("http")),
            (
                "SINK_DATADOG_URL",
                Some("https://http-intake.example/v1/logs"),
            ),
            ("SINK_DATADOG_ENCODING", Some("json")),
            ("SINK_DATADOG_HEADERS", Some("DD-API-KEY:secret,X-Extra:1")),
        ],
        || {
            let config = Config::from_env().unwrap();
            assert_eq!(config.sinks.len(), 2);
            let datadog = config.sinks.iter().find(|s| s.name == "datadog").unwrap();
            assert_eq!(datadog.headers.len(), 2);
        },
    );
}

#[test]
fn test_config_wazuh_syslog_encoding_defaults_to_rfc3164() {
    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("SINKS", Some("wazuh")),
            ("SINK_WAZUH_ADDR", Some("127.0.0.1:1514")),
        ],
        || {
            let config = Config::from_env().unwrap();
            assert_eq!(config.sinks[0].encoding, Encoding::Syslog3164);
        },
    );
}

fn temp_secret_file(name: &str, contents: &str) -> String {
    let path = std::env::temp_dir()
        .join(format!(
            "auditbridge-test-secret-{}-{}",
            name,
            std::process::id()
        ))
        .to_string_lossy()
        .to_string();
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn test_config_netbird_token_file_reads_and_trims_file_contents() {
    let path = temp_secret_file("token", "nbp_from_file\n");

    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", None),
            ("NETBIRD_API_TOKEN_FILE", Some(path.as_str())),
        ],
        || {
            let config = Config::from_env().unwrap();
            assert_eq!(config.netbird_api_token, "nbp_from_file");
        },
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn test_config_netbird_token_both_direct_and_file_is_an_error() {
    let path = temp_secret_file("token-conflict", "nbp_from_file");

    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("nbp_direct")),
            ("NETBIRD_API_TOKEN_FILE", Some(path.as_str())),
        ],
        || {
            let result = Config::from_env();
            assert!(
                result.is_err(),
                "setting both the direct var and _FILE is ambiguous"
            );
        },
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn test_config_sink_headers_file_reads_credentials_from_file() {
    let path = temp_secret_file("headers", "Authorization:Bearer secret-token\n");

    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("SINKS", Some("datadog")),
            (
                "SINK_DATADOG_URL",
                Some("https://http-intake.example/v1/logs"),
            ),
            ("SINK_DATADOG_ENCODING", Some("json")),
            ("SINK_DATADOG_TRANSPORT", Some("http")),
            ("SINK_DATADOG_HEADERS_FILE", Some(path.as_str())),
        ],
        || {
            let config = Config::from_env().unwrap();
            assert_eq!(
                config.sinks[0].headers,
                vec![(
                    "Authorization".to_string(),
                    "Bearer secret-token".to_string()
                )]
            );
        },
    );

    std::fs::remove_file(&path).ok();
}

fn temp_cursor_path(name: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "auditbridge-test-{}-{}.json",
            name,
            std::process::id()
        ))
        .to_string_lossy()
        .to_string()
}

#[test]
fn test_cursor_load_missing_file_returns_empty() {
    let path = temp_cursor_path("missing");
    let loaded = cursor::load(&path);
    assert!(loaded.is_empty());
}

#[test]
fn test_cursor_load_corrupt_file_returns_empty() {
    let path = temp_cursor_path("corrupt");
    std::fs::write(&path, "not valid json").unwrap();

    let loaded = cursor::load(&path);

    assert!(
        loaded.is_empty(),
        "a corrupt cursor file must not crash the load"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_cursor_save_and_load_round_trip() {
    let path = temp_cursor_path("roundtrip");
    let mut loki_ids = std::collections::BTreeSet::new();
    loki_ids.insert("evt-boundary-1".to_string());
    loki_ids.insert("evt-boundary-2".to_string());

    let mut cursors = HashMap::new();
    cursors.insert(
        "loki".to_string(),
        SinkCursor {
            watermark: Some("2023-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()),
            boundary_ids: loki_ids,
        },
    );
    cursors.insert(
        "wazuh".to_string(),
        SinkCursor {
            watermark: Some("2023-06-15T12:30:00Z".parse::<DateTime<Utc>>().unwrap()),
            boundary_ids: Default::default(),
        },
    );

    cursor::save(&path, &cursors).expect("save should succeed");
    let loaded = cursor::load(&path);

    assert_eq!(
        loaded, cursors,
        "watermarks and boundary IDs must survive a save/load round trip"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_cursor_load_legacy_timestamp_only_format() {
    let path = temp_cursor_path("legacy");
    // What the previous release wrote: a bare RFC 3339 timestamp per sink.
    std::fs::write(&path, r#"{"loki":"2023-01-01T00:00:00Z"}"#).unwrap();

    let loaded = cursor::load(&path);

    let loki = loaded
        .get("loki")
        .expect("legacy entry must not be dropped");
    assert_eq!(
        loki.watermark,
        Some("2023-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()),
        "a legacy cursor file must keep its watermark, otherwise the upgrade \
         replays the account's full history into every sink"
    );
    assert!(
        loki.boundary_ids.is_empty(),
        "legacy entries carry no boundary IDs"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_build_initial_cursors_resumes_from_persisted_state() {
    let sinks: Vec<Box<dyn Sink>> = vec![
        Box::new(HttpSink::new(
            "loki".to_string(),
            "http://loki/loki/api/v1/push".to_string(),
            Method::POST,
            vec![],
            Encoding::Loki,
        )),
        Box::new(SyslogSink::new(
            "wazuh".to_string(),
            "127.0.0.1:1514".to_string(),
            SyslogProtocol::Tcp,
            Encoding::Syslog3164,
        )),
    ];

    let mut persisted = HashMap::new();
    let loki_ts: DateTime<Utc> = "2023-01-01T00:00:00Z".parse().unwrap();
    persisted.insert(
        "loki".to_string(),
        SinkCursor {
            watermark: Some(loki_ts),
            boundary_ids: Default::default(),
        },
    );
    // no entry for "wazuh": never persisted (e.g. first run for that sink)

    let cursors = build_initial_cursors(&sinks, &persisted);

    assert_eq!(cursors.get("loki").and_then(|c| c.watermark), Some(loki_ts));
    assert_eq!(cursors.get("wazuh").and_then(|c| c.watermark), None);
}

fn fast_retry(max_attempts: u32) -> RetryConfig {
    RetryConfig {
        max_attempts,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    }
}

#[tokio::test]
async fn test_with_retry_succeeds_after_transient_failures() {
    let attempts = AtomicU32::new(0);
    let cfg = fast_retry(5);

    let result = with_retry("test op", &cfg, || {
        let n = attempts.fetch_add(1, Ordering::SeqCst);
        async move {
            if n < 2 {
                Err(anyhow::anyhow!("transient failure"))
            } else {
                Ok(42)
            }
        }
    })
    .await;

    assert_eq!(result.unwrap(), 42);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_with_retry_gives_up_after_max_attempts() {
    let attempts = AtomicU32::new(0);
    let cfg = fast_retry(3);

    let result: anyhow::Result<()> = with_retry("test op", &cfg, || {
        attempts.fetch_add(1, Ordering::SeqCst);
        async { Err(anyhow::anyhow!("permanent failure")) }
    })
    .await;

    assert!(result.is_err());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "must stop at max_attempts, not retry forever"
    );
}

#[test]
fn test_metrics_not_ready_until_fetch_and_a_sink_both_succeed() {
    let metrics = Metrics::default();
    assert!(
        !metrics.is_ready(),
        "must not be ready before the first cycle"
    );

    metrics.record_fetch_success(3);
    assert!(
        !metrics.is_ready(),
        "fetch alone isn't enough, no sink has delivered yet"
    );

    metrics.record_sink_success("loki", 3);
    assert!(metrics.is_ready());
}

#[test]
fn test_metrics_becomes_unready_again_after_fetch_failure() {
    let metrics = Metrics::default();
    metrics.record_fetch_success(1);
    metrics.record_sink_success("loki", 1);
    assert!(metrics.is_ready());

    metrics.record_fetch_error();
    assert!(
        !metrics.is_ready(),
        "a failed fetch must flip readiness back off"
    );
}

#[test]
fn test_metrics_render_prometheus_includes_recorded_values() {
    let metrics = Metrics::default();
    metrics.record_fetch_success(5);
    metrics.record_sink_success("loki", 5);
    metrics.record_sink_error("wazuh");

    let output = metrics.render_prometheus();

    assert!(output.contains("auditbridge_events_fetched_total 5"));
    assert!(output.contains("auditbridge_events_delivered_total{sink=\"loki\"} 5"));
    assert!(output.contains("auditbridge_delivery_errors_total{sink=\"wazuh\"} 1"));
}

#[tokio::test]
async fn test_metrics_server_exposes_healthz_readyz_metrics() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let metrics = Metrics::new();
    let server_metrics = metrics.clone();

    tokio::spawn(async move {
        axum::serve(listener, router(server_metrics)).await.unwrap();
    });

    let client = reqwest::Client::new();

    let healthz = client
        .get(format!("http://{}/healthz", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(healthz.status(), reqwest::StatusCode::OK);

    let readyz_before = client
        .get(format!("http://{}/readyz", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(
        readyz_before.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );

    metrics.record_fetch_success(2);
    metrics.record_sink_success("loki", 2);

    let readyz_after = client
        .get(format!("http://{}/readyz", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(readyz_after.status(), reqwest::StatusCode::OK);

    let metrics_resp = client
        .get(format!("http://{}/metrics", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(metrics_resp.status(), reqwest::StatusCode::OK);
    let body = metrics_resp.text().await.unwrap();
    assert!(body.contains("auditbridge_events_fetched_total 2"));
}

#[tokio::test]
async fn test_run_exits_immediately_if_shutdown_already_signaled() {
    let nb_mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<Event>::new()))
        .expect(0)
        .mount(&nb_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![];
    let mut cursors = HashMap::new();
    let config = test_config();
    let metrics = Metrics::default();
    let (_tx, rx) = tokio::sync::watch::channel(true);

    let start = std::time::Instant::now();
    run(&nb_client, &sinks, &mut cursors, &config, &metrics, rx).await;

    assert!(start.elapsed() < Duration::from_millis(500));
    nb_mock.verify().await;
}

#[tokio::test]
async fn test_run_exits_promptly_when_shutdown_fires_during_idle_wait() {
    let nb_mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<Event>::new()))
        .mount(&nb_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![];
    let cursors = HashMap::new();
    let mut config = test_config();
    config.check_interval = Duration::from_secs(60);
    let metrics = Metrics::default();
    let (tx, rx) = tokio::sync::watch::channel(false);

    let handle = tokio::spawn(async move {
        let mut cursors = cursors;
        run(&nb_client, &sinks, &mut cursors, &config, &metrics, rx).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(true).unwrap();

    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        result.is_ok(),
        "run() must exit promptly once shutdown fires during the idle wait, not wait out check_interval"
    );
}

#[tokio::test]
async fn test_run_lets_in_flight_cycle_finish_before_exiting() {
    let nb_mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(vec![sample_event("1")])
                .set_delay(Duration::from_millis(150)),
        )
        .mount(&nb_mock)
        .await;

    let sink_mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![Box::new(HttpSink::new(
        "test".to_string(),
        format!("{}/ingest", sink_mock.uri()),
        Method::POST,
        vec![],
        Encoding::Json,
    ))];
    let cursors = HashMap::new();
    let mut config = test_config();
    config.check_interval = Duration::from_secs(60);
    let metrics = Metrics::default();
    let (tx, rx) = tokio::sync::watch::channel(false);

    let handle = tokio::spawn(async move {
        let mut cursors = cursors;
        run(&nb_client, &sinks, &mut cursors, &config, &metrics, rx).await;
        cursors
    });

    // Fires while the 150ms-delayed fetch is still in flight.
    tokio::time::sleep(Duration::from_millis(30)).await;
    tx.send(true).unwrap();

    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("run() did not exit within the timeout")
        .expect("run() task panicked");

    assert!(
        result.get("test").and_then(|c| c.watermark).is_some(),
        "the in-flight cycle must finish and its result apply even though shutdown fired mid-fetch"
    );
}

fn sink_for(mock: &MockServer) -> Box<dyn Sink> {
    Box::new(HttpSink::new(
        "sink".to_string(),
        format!("{}/ingest", mock.uri()),
        Method::POST,
        vec![],
        Encoding::Json,
    ))
}

async fn mount_events(mock: &MockServer, events: Vec<Event>) {
    Mock::given(method("GET"))
        .and(path("/api/events/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(events))
        .mount(mock)
        .await;
}

/// Every event body the sink received, one `Vec<Event>` per request. Boundary
/// and dedupe assertions need the event *identities*, not just batch sizes.
async fn received_events(mock: &MockServer) -> Vec<Vec<Event>> {
    mock.received_requests()
        .await
        .expect("no requests received")
        .iter()
        .map(|req| serde_json::from_slice::<Vec<Event>>(&req.body).unwrap())
        .collect()
}

async fn received_batch_sizes(mock: &MockServer) -> Vec<usize> {
    received_events(mock)
        .await
        .iter()
        .map(|events| events.len())
        .collect()
}

fn event_at(id: &str, timestamp: &str) -> Event {
    let mut event = sample_event(id);
    event.timestamp = timestamp.to_string();
    event
}

fn ids(events: &[Event]) -> Vec<String> {
    events.iter().map(|e| e.id.clone()).collect()
}

fn ids_and_timestamps(batches: &[Vec<Event>]) -> Vec<Vec<(String, String)>> {
    batches
        .iter()
        .map(|events| {
            events
                .iter()
                .map(|e| (e.id.clone(), e.timestamp.clone()))
                .collect()
        })
        .collect()
}

fn expected_batches(batches: &[&[(&str, &str)]]) -> Vec<Vec<(String, String)>> {
    batches
        .iter()
        .map(|events| {
            events
                .iter()
                .map(|(id, ts)| (id.to_string(), ts.to_string()))
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn test_process_cycle_splits_large_batches_into_chunks() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;
    mount_events(
        &nb_mock,
        vec![sample_event("1"), sample_event("2"), sample_event("3")],
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![sink_for(&sink_mock)];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        2,
        &Metrics::default(),
    )
    .await;

    assert_eq!(
        received_batch_sizes(&sink_mock).await,
        vec![2, 1],
        "3 events with batch_size 2 must produce two requests of 2 and 1"
    );
    assert!(
        cursors.get("sink").and_then(|c| c.watermark).is_some(),
        "watermark must advance once every chunk delivered"
    );
}

#[tokio::test]
async fn test_process_cycle_batch_size_zero_disables_chunking() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;
    mount_events(
        &nb_mock,
        vec![sample_event("1"), sample_event("2"), sample_event("3")],
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![sink_for(&sink_mock)];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        0,
        &Metrics::default(),
    )
    .await;

    assert_eq!(received_batch_sizes(&sink_mock).await, vec![3]);
}

#[tokio::test]
async fn test_process_cycle_does_not_advance_watermark_when_a_chunk_fails() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;
    mount_events(&nb_mock, vec![sample_event("1"), sample_event("2")]).await;

    // First chunk succeeds, second chunk is rejected: mocks match in mount
    // order, and the 200 response is exhausted after one use.
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .up_to_n_times(1)
        .mount(&sink_mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![sink_for(&sink_mock)];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        1,
        &Metrics::default(),
    )
    .await;

    assert_eq!(
        cursors.get("sink").and_then(|c| c.watermark),
        None,
        "watermark must not advance when any chunk fails, so nothing is skipped"
    );
}

#[test]
fn test_config_batch_size_defaults_to_500_and_parses_env() {
    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("BATCH_SIZE", None),
        ],
        || assert_eq!(Config::from_env().unwrap().batch_size, 500),
    );

    temp_env::with_vars(
        [
            ("NETBIRD_API_TOKEN", Some("test_token")),
            ("BATCH_SIZE", Some("7")),
        ],
        || assert_eq!(Config::from_env().unwrap().batch_size, 7),
    );
}

const T0: &str = "2023-01-01T00:00:00Z";
const T5: &str = "2023-01-01T00:00:05Z";

// Loki needs nanosecond timestamps. A valid RFC 3339 value must encode to its
// own instant, and the `Z`-less form must still parse as UTC.
#[test]
fn test_loki_timestamp_encodes_parsed_instant() {
    let body = serde_json::to_string(
        &serde_json::from_slice::<serde_json::Value>(
            &crate::sinks::encoding::encode_http_body(
                Encoding::Loki,
                &[event_at("A", T0), event_at("B", "2023-01-01T00:00:00")],
            )
            .unwrap()
            .0,
        )
        .unwrap(),
    )
    .unwrap();

    // 2023-01-01T00:00:00Z in nanoseconds, however the offset was written.
    let expected = "1672531200000000000";
    let occurrences = body.matches(expected).count();
    assert_eq!(
        occurrences, 2,
        "both the RFC 3339 and the offset-less form must encode the same instant, got: {}",
        body
    );
}

// An unparseable timestamp must NOT be rewritten to "now": that made the
// audit time differ on every poll. It is deterministic (the epoch) instead.
#[test]
fn test_loki_unparseable_timestamp_falls_back_to_epoch_deterministically() {
    let encode = || {
        let (body, _) = crate::sinks::encoding::encode_http_body(
            Encoding::Loki,
            &[event_at("X", "not-a-timestamp")],
        )
        .unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()
    };

    let first = encode();
    std::thread::sleep(Duration::from_millis(20));
    let second = encode();

    // Compare the payload rather than the raw bytes: the stream label map is a
    // HashMap, so its key order is not stable between encodings (nor between
    // processes) and says nothing about the timestamp.
    assert_eq!(
        first, second,
        "encoding an unparseable timestamp twice must produce identical output"
    );
    assert_eq!(
        first["streams"][0]["values"][0][0],
        serde_json::json!("0"),
        "an unparseable timestamp must encode at the Unix epoch, got: {}",
        first
    );
}

// The ticket's scenario: two events share a timestamp and are delivered; a
// third event with the *same* timestamp then shows up in a later poll. A
// strict `ts > watermark` filter drops it forever. Boundary IDs must let it
// through while still suppressing the two already delivered at that time.
//
// The second request is the discriminating assertion: a plain `ts >= watermark`
// filter would resend A and B too, so asserting exactly [C] pins the boundary
// set rather than merely the watermark.
#[tokio::test]
async fn test_same_timestamp_event_arriving_later_is_not_dropped() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![sink_for(&sink_mock)];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    // Poll 1: A and B, both at T0.
    mount_events(&nb_mock, vec![event_at("A", T0), event_at("B", T0)]).await;
    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    let after_first = cursors.get("sink").cloned().expect("cursor must exist");
    assert_eq!(after_first.watermark, Some(T0.parse().unwrap()));
    assert_eq!(
        after_first.boundary_ids,
        ["A".to_string(), "B".to_string()].into_iter().collect(),
        "both events at the watermark must be recorded as delivered"
    );

    // Poll 2: A, B and a new C, all at T0.
    nb_mock.reset().await;
    mount_events(
        &nb_mock,
        vec![event_at("A", T0), event_at("B", T0), event_at("C", T0)],
    )
    .await;
    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    let batches = received_events(&sink_mock).await;
    assert_eq!(batches.len(), 2, "one request per poll that had work to do");
    assert_eq!(ids(&batches[0]), vec!["A", "B"]);
    assert_eq!(
        ids(&batches[1]),
        vec!["C"],
        "only the never-delivered event at the watermark may be resent"
    );

    let after_second = cursors.get("sink").cloned().unwrap();
    assert_eq!(after_second.watermark, Some(T0.parse().unwrap()));
    assert_eq!(
        after_second.boundary_ids,
        ["A".to_string(), "B".to_string(), "C".to_string()]
            .into_iter()
            .collect(),
        "the boundary set must grow with the newly delivered event"
    );

    // Poll 3: nothing new.
    sink_mock.reset().await;
    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;
    assert!(
        sink_mock.received_requests().await.unwrap().is_empty(),
        "a poll with nothing new must not reach the sink"
    );
}

// At-least-once is a retry guarantee, not permission to send the same event
// twice in one request: a duplicate in a single fetch is collapsed.
#[tokio::test]
async fn test_duplicate_event_in_one_poll_is_delivered_once() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;
    mount_events(
        &nb_mock,
        vec![event_at("A", T0), event_at("A", T0), event_at("B", T5)],
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![sink_for(&sink_mock)];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    assert_eq!(
        ids_and_timestamps(&received_events(&sink_mock).await),
        expected_batches(&[&[("A", T0), ("B", T5)]]),
        "a repeated (timestamp, id) must be sent once, in timestamp order"
    );
}

// An event whose timestamp cannot be parsed cannot be ordered, so it is
// delivered (never silently dropped) and never recorded in the cursor. It is
// therefore re-sent on every cycle while it stays in NetBird's history.
#[tokio::test]
async fn test_unparseable_timestamp_is_delivered_but_never_advances_watermark() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;
    mount_events(
        &nb_mock,
        vec![event_at("A", T0), event_at("X", "not-a-timestamp")],
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![sink_for(&sink_mock)];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();
    cursors.insert(
        "sink".to_string(),
        SinkCursor {
            watermark: Some(T0.parse().unwrap()),
            boundary_ids: ["A".to_string()].into_iter().collect(),
        },
    );

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    assert_eq!(
        ids_and_timestamps(&received_events(&sink_mock).await),
        expected_batches(&[&[("X", "not-a-timestamp")]]),
        "the unparseable event is delivered; the already-delivered boundary event is not"
    );

    let cursor = cursors.get("sink").cloned().unwrap();
    assert_eq!(cursor.watermark, Some(T0.parse().unwrap()));
    assert_eq!(
        cursor.boundary_ids,
        ["A".to_string()].into_iter().collect(),
        "an unparseable timestamp must not be recorded as a delivered boundary event"
    );

    // Second cycle: the unparseable event is still unattributable, so it is
    // re-sent rather than silently lost.
    sink_mock.reset().await;
    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;
    assert_eq!(
        ids_and_timestamps(&received_events(&sink_mock).await),
        expected_batches(&[&[("X", "not-a-timestamp")]]),
        "an unorderable event is retried every cycle instead of being lost"
    );
}

// A batch older than the current position must not move the watermark
// backwards, which would re-deliver everything between it and the watermark.
#[tokio::test]
async fn test_watermark_never_decreases() {
    let nb_mock = MockServer::start().await;
    let sink_mock = MockServer::start().await;
    mount_events(&nb_mock, vec![event_at("old", T0)]).await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink_mock)
        .await;

    let nb_client = NetbirdClient::new(nb_mock.uri(), "token".to_string());
    let sinks: Vec<Box<dyn Sink>> = vec![sink_for(&sink_mock)];
    let mut cursors: HashMap<String, SinkCursor> = HashMap::new();
    cursors.insert(
        "sink".to_string(),
        SinkCursor {
            watermark: Some(T5.parse().unwrap()),
            boundary_ids: ["new".to_string()].into_iter().collect(),
        },
    );

    process_cycle(
        &nb_client,
        &sinks,
        &mut cursors,
        &no_retry(),
        500,
        &Metrics::default(),
    )
    .await;

    let cursor = cursors.get("sink").cloned().unwrap();
    assert_eq!(
        cursor.watermark,
        Some(T5.parse().unwrap()),
        "the watermark must never move backwards"
    );
    assert_eq!(
        cursor.boundary_ids,
        ["new".to_string()].into_iter().collect()
    );
}
