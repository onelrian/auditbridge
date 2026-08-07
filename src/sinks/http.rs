use super::Sink;
use crate::models::Event;
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use std::time::Duration;
use tracing::info;

/// Ships events as a raw JSON array via HTTP POST, for any downstream that
/// isn't Loki or Wazuh (a generic webhook, another SIEM, a custom collector).
pub struct HttpSink {
    client: Client,
    url: String,
}

impl HttpSink {
    pub fn new(url: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            url,
        }
    }
}

#[async_trait]
impl Sink for HttpSink {
    fn name(&self) -> &str {
        "http"
    }

    async fn send(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let response = self.client.post(&self.url).json(events).send().await?;

        if response.status().is_success() {
            info!(
                "Sent {} events to generic HTTP sink ({})",
                events.len(),
                self.url
            );
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to send to HTTP sink: {} - {}", status, body)
        }
    }
}
