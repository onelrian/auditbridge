#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::models::Event;
    use crate::netbird::NetbirdClient;
    use crate::process_cycle;
    use crate::sinks::{HttpSink, LokiSink, Sink, WazuhSink};
    use chrono::{DateTime, Utc};
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
    async fn test_loki_sink_send_events() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/loki/api/v1/push"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&mock_server)
            .await;

        let sink = LokiSink::new(mock_server.uri());
        sink.send(&[sample_event("1")])
            .await
            .expect("Failed to send events");
    }

    #[tokio::test]
    async fn test_http_sink_send_events() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ingest"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let sink = HttpSink::new(format!("{}/ingest", mock_server.uri()));
        sink.send(&[sample_event("1")])
            .await
            .expect("Failed to send events");
    }

    #[tokio::test]
    async fn test_wazuh_sink_send_events() {
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

        let sink = WazuhSink::new(addr.to_string());
        sink.send(&[sample_event("1")])
            .await
            .expect("Failed to send events");
        drop(sink);

        let received = tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .expect("wazuh mock server timed out")
            .expect("wazuh mock server task panicked");
        let text = String::from_utf8(received).unwrap();

        assert!(
            text.starts_with("<134>1 "),
            "expected RFC5424 framing, got: {}",
            text
        );
        assert!(text.contains("test.activity"));
    }

    #[tokio::test]
    async fn test_process_cycle_does_not_advance_watermark_on_send_failure() {
        let nb_mock = MockServer::start().await;
        let loki_mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![sample_event("1")]))
            .mount(&nb_mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/loki/api/v1/push"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&loki_mock)
            .await;

        let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
        let sinks: Vec<Box<dyn Sink>> = vec![Box::new(LokiSink::new(loki_mock.uri()))];
        let mut cursors: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();

        process_cycle(&nb_client, &sinks, &mut cursors).await;

        assert_eq!(
            cursors.get("loki").copied().flatten(),
            None,
            "watermark must not advance when the sink write fails"
        );
    }

    #[tokio::test]
    async fn test_process_cycle_advances_watermark_on_send_success() {
        let nb_mock = MockServer::start().await;
        let loki_mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![sample_event("1")]))
            .mount(&nb_mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/loki/api/v1/push"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&loki_mock)
            .await;

        let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
        let sinks: Vec<Box<dyn Sink>> = vec![Box::new(LokiSink::new(loki_mock.uri()))];
        let mut cursors: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();

        process_cycle(&nb_client, &sinks, &mut cursors).await;

        assert!(
            cursors.get("loki").copied().flatten().is_some(),
            "watermark must advance once delivery is confirmed"
        );
    }

    #[tokio::test]
    async fn test_process_cycle_one_failing_sink_does_not_block_the_other() {
        let nb_mock = MockServer::start().await;
        let loki_mock = MockServer::start().await;
        let http_mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![sample_event("1")]))
            .mount(&nb_mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/loki/api/v1/push"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&loki_mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/ingest"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&http_mock)
            .await;

        let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
        let sinks: Vec<Box<dyn Sink>> = vec![
            Box::new(LokiSink::new(loki_mock.uri())),
            Box::new(HttpSink::new(format!("{}/ingest", http_mock.uri()))),
        ];
        let mut cursors: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();

        process_cycle(&nb_client, &sinks, &mut cursors).await;

        assert_eq!(cursors.get("loki").copied().flatten(), None);
        assert!(cursors.get("http").copied().flatten().is_some());
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
            },
        );
    }

    #[test]
    fn test_config_wazuh_requires_addr() {
        temp_env::with_vars(
            [
                ("NETBIRD_API_TOKEN", Some("test_token")),
                ("SINKS", Some("wazuh")),
                ("WAZUH_ADDR", None),
            ],
            || {
                let result = Config::from_env();
                assert!(
                    result.is_err(),
                    "expected an error when WAZUH_ADDR is missing"
                );
            },
        );
    }

    #[test]
    fn test_config_multi_sink_fanout() {
        temp_env::with_vars(
            [
                ("NETBIRD_API_TOKEN", Some("test_token")),
                ("SINKS", Some("loki,wazuh,http")),
                ("WAZUH_ADDR", Some("127.0.0.1:1514")),
                ("HTTP_SINK_URL", Some("http://example.invalid/ingest")),
            ],
            || {
                let config = Config::from_env().unwrap();
                assert_eq!(config.sinks.len(), 3);
            },
        );
    }
}
