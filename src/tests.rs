#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::loki::LokiClient;
    use crate::models::Event;
    use crate::netbird::NetbirdClient;
    use crate::process_cycle;
    use chrono::{DateTime, Utc};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_netbird_fetch_events() {
        let mock_server = MockServer::start().await;

        let mock_events = vec![
            Event {
                id: "1".to_string(),
                timestamp: "2023-01-01T00:00:00Z".to_string(),
                activity: "user_joined".to_string(),
                activity_code: "user.join".to_string(),
                initiator_id: Some("init1".to_string()),
                initiator_email: None,
                initiator_name: None,
                target_id: Some("target1".to_string()),
                account_id: Some("acc1".to_string()),
                meta: None,
            }
        ];

        Mock::given(method("GET"))
            .and(path("/api/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_events))
            .mount(&mock_server)
            .await;

        let client = NetbirdClient::new(mock_server.uri(), "fake_token".to_string());
        let events = client.fetch_events().await.expect("Failed to fetch events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "1");
        assert_eq!(events[0].activity, "user_joined");
    }

    #[tokio::test]
    async fn test_loki_send_events() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/loki/api/v1/push"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&mock_server)
            .await;

        let client = LokiClient::new(mock_server.uri());
        
        let events = vec![
            Event {
                id: "1".to_string(),
                timestamp: "2023-01-01T00:00:00Z".to_string(),
                activity: "test_activity".to_string(),
                activity_code: "test.activity".to_string(),
                initiator_id: None,
                initiator_email: None,
                initiator_name: None,
                target_id: None,
                account_id: Some("acc1".to_string()),
                meta: None,
            }
        ];

        client.send_events(&events).await.expect("Failed to send events");
    }

    #[tokio::test]
    async fn test_process_cycle_does_not_advance_watermark_on_send_failure() {
        let nb_mock = MockServer::start().await;
        let loki_mock = MockServer::start().await;

        let event = Event {
            id: "1".to_string(),
            timestamp: "2023-01-01T00:00:00Z".to_string(),
            activity: "test_activity".to_string(),
            activity_code: "test.activity".to_string(),
            initiator_id: None,
            initiator_email: None,
            initiator_name: None,
            target_id: None,
            account_id: Some("acc1".to_string()),
            meta: None,
        };

        Mock::given(method("GET"))
            .and(path("/api/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![event]))
            .mount(&nb_mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/loki/api/v1/push"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&loki_mock)
            .await;

        let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
        let loki_client = LokiClient::new(loki_mock.uri());
        let mut cursor: Option<DateTime<Utc>> = None;

        process_cycle(&nb_client, &loki_client, &mut cursor).await;

        assert_eq!(cursor, None, "watermark must not advance when the sink write fails");
    }

    #[tokio::test]
    async fn test_process_cycle_advances_watermark_on_send_success() {
        let nb_mock = MockServer::start().await;
        let loki_mock = MockServer::start().await;

        let event = Event {
            id: "1".to_string(),
            timestamp: "2023-01-01T00:00:00Z".to_string(),
            activity: "test_activity".to_string(),
            activity_code: "test.activity".to_string(),
            initiator_id: None,
            initiator_email: None,
            initiator_name: None,
            target_id: None,
            account_id: Some("acc1".to_string()),
            meta: None,
        };

        Mock::given(method("GET"))
            .and(path("/api/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![event]))
            .mount(&nb_mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/loki/api/v1/push"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&loki_mock)
            .await;

        let nb_client = NetbirdClient::new(nb_mock.uri(), "fake_token".to_string());
        let loki_client = LokiClient::new(loki_mock.uri());
        let mut cursor: Option<DateTime<Utc>> = None;

        process_cycle(&nb_client, &loki_client, &mut cursor).await;

        assert!(cursor.is_some(), "watermark must advance once delivery is confirmed");
    }

    #[test]
    fn test_config_defaults() {
        temp_env::with_vars(
            [
                ("NETBIRD_API_TOKEN", Some("test_token")),
                ("LOKI_URL", None), // Should use default
            ],
            || {
                let config = Config::from_env().unwrap();
                assert_eq!(config.loki_url, "http://loki:3100");
                assert_eq!(config.netbird_api_token, "test_token");
            },
        );
    }
}
