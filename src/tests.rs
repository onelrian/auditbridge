#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::cursor;
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
            .and(path("/api/events"))
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
            .and(path("/api/events"))
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
        let mut cursors: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();

        process_cycle(
            &nb_client,
            &sinks,
            &mut cursors,
            &no_retry(),
            &Metrics::default(),
        )
        .await;

        assert_eq!(
            cursors.get("flaky").copied().flatten(),
            None,
            "watermark must not advance when the sink write fails"
        );
    }

    #[tokio::test]
    async fn test_process_cycle_advances_watermark_on_send_success() {
        let nb_mock = MockServer::start().await;
        let sink_mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/events"))
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
        let mut cursors: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();

        process_cycle(
            &nb_client,
            &sinks,
            &mut cursors,
            &no_retry(),
            &Metrics::default(),
        )
        .await;

        assert!(
            cursors.get("generic").copied().flatten().is_some(),
            "watermark must advance once delivery is confirmed"
        );
    }

    #[tokio::test]
    async fn test_process_cycle_one_failing_sink_does_not_block_the_other() {
        let nb_mock = MockServer::start().await;
        let down_mock = MockServer::start().await;
        let up_mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/events"))
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
        let mut cursors: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();

        process_cycle(
            &nb_client,
            &sinks,
            &mut cursors,
            &no_retry(),
            &Metrics::default(),
        )
        .await;

        assert_eq!(cursors.get("down").copied().flatten(), None);
        assert!(cursors.get("up").copied().flatten().is_some());
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
        let mut cursors = HashMap::new();
        cursors.insert(
            "loki".to_string(),
            "2023-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
        );
        cursors.insert(
            "wazuh".to_string(),
            "2023-06-15T12:30:00Z".parse::<DateTime<Utc>>().unwrap(),
        );

        cursor::save(&path, &cursors).expect("save should succeed");
        let loaded = cursor::load(&path);

        assert_eq!(loaded, cursors);
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
        persisted.insert("loki".to_string(), loki_ts);
        // no entry for "wazuh": never persisted (e.g. first run for that sink)

        let cursors = build_initial_cursors(&sinks, &persisted);

        assert_eq!(cursors.get("loki").copied().flatten(), Some(loki_ts));
        assert_eq!(cursors.get("wazuh").copied().flatten(), None);
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
            .and(path("/api/events"))
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
            .and(path("/api/events"))
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
            .and(path("/api/events"))
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
            result.get("test").copied().flatten().is_some(),
            "the in-flight cycle must finish and its result apply even though shutdown fired mid-fetch"
        );
    }
}
