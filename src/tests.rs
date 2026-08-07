#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::models::Event;
    use crate::netbird::NetbirdClient;
    use crate::process_cycle;
    use crate::sinks::encoding::Encoding;
    use crate::sinks::http::HttpSink;
    use crate::sinks::syslog::{SyslogProtocol, SyslogSink};
    use crate::sinks::Sink;
    use chrono::{DateTime, Utc};
    use reqwest::Method;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

        process_cycle(&nb_client, &sinks, &mut cursors).await;

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

        process_cycle(&nb_client, &sinks, &mut cursors).await;

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

        process_cycle(&nb_client, &sinks, &mut cursors).await;

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
}
