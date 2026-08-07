use super::Sink;
use crate::models::Event;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Serialize)]
struct LokiStream {
    stream: HashMap<String, String>,
    values: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
struct LokiPushRequest {
    streams: Vec<LokiStream>,
}

pub struct LokiSink {
    client: Client,
    loki_url: String,
}

impl LokiSink {
    pub fn new(loki_url: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            loki_url,
        }
    }

    pub async fn wait_for_ready(&self) -> Result<()> {
        info!("Waiting for Loki to be ready...");
        let ready_url = format!("{}/ready", self.loki_url);

        for attempt in 1..=60 {
            match self
                .client
                .get(&ready_url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    info!("Loki is ready");
                    return Ok(());
                }
                _ => {
                    if attempt % 10 == 0 {
                        info!("Still waiting for Loki... (attempt {}/60)", attempt);
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }

        anyhow::bail!("Failed to connect to Loki after 60 attempts")
    }
}

#[async_trait]
impl Sink for LokiSink {
    fn name(&self) -> &str {
        "loki"
    }

    async fn send(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let mut streams: HashMap<String, LokiStream> = HashMap::new();

        for event in events {
            let activity_name = &event.activity;

            let mut labels = HashMap::new();
            labels.insert("job".to_string(), "netbird-events".to_string());
            labels.insert(
                "account_id".to_string(),
                event
                    .account_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            );
            labels.insert("activity".to_string(), activity_name.clone());
            labels.insert("activity_code".to_string(), event.activity_code.clone());

            let label_key = format!(
                "{{{}}}",
                labels
                    .iter()
                    .map(|(k, v)| format!("{}=\"{}\"", k, v))
                    .collect::<Vec<_>>()
                    .join(",")
            );

            // initiator_email/name stay in the JSON body, not labels: they're
            // higher cardinality than the rest of the label set and would
            // fragment Loki streams per-user instead of per-activity.
            let log_data = serde_json::json!({
                "event_id": event.id,
                "timestamp": event.timestamp,
                "activity": activity_name,
                "activity_code": event.activity_code,
                "initiator_id": event.initiator_id.clone().unwrap_or_default(),
                "initiator_email": event.initiator_email.clone().unwrap_or_default(),
                "initiator_name": event.initiator_name.clone().unwrap_or_default(),
                "target_id": event.target_id.clone().unwrap_or_default(),
                "account_id": event.account_id.clone().unwrap_or_default(),
                "meta": event.meta,
            });

            let ts_ns = timestamp_to_nanoseconds(&event.timestamp);
            let log_line = serde_json::to_string(&log_data)?;

            streams
                .entry(label_key)
                .or_insert_with(|| LokiStream {
                    stream: labels.clone(),
                    values: Vec::new(),
                })
                .values
                .push((ts_ns, log_line));
        }

        let request = LokiPushRequest {
            streams: streams.into_values().collect(),
        };

        let push_url = format!("{}/loki/api/v1/push", self.loki_url);
        let response = self
            .client
            .post(&push_url)
            .json(&request)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;

        if response.status().is_success() {
            info!("Sent {} events to Loki", events.len());
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to send to Loki: {} - {}", status, body)
        }
    }
}

fn timestamp_to_nanoseconds(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .or_else(|_| {
            let ts = timestamp.trim_end_matches('Z');
            DateTime::parse_from_rfc3339(&format!("{}+00:00", ts))
        })
        .map(|dt| (dt.timestamp_nanos_opt().unwrap_or(0)).to_string())
        .unwrap_or_else(|_| {
            warn!(
                "Failed to parse timestamp: {}, using current time",
                timestamp
            );
            Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string()
        })
}
