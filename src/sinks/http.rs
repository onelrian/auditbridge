use super::encoding::{self, Encoding};
use super::Sink;
use crate::models::Event;
use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, Method};
use std::time::Duration;
use tracing::info;

/// Generic HTTP transport: any URL, any method, any headers. The wire
/// format is entirely delegated to `encoding` (see `encoding.rs`), so most
/// new HTTP-based backends (a webhook, Datadog, Splunk HEC, ...) are pure
/// configuration on this one type, not a new sink implementation.
pub struct HttpSink {
    name: String,
    client: Client,
    url: String,
    method: Method,
    headers: Vec<(String, String)>,
    encoding: Encoding,
}

impl HttpSink {
    pub fn new(
        name: String,
        url: String,
        method: Method,
        headers: Vec<(String, String)>,
        encoding: Encoding,
    ) -> Self {
        Self {
            name,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            url,
            method,
            headers,
            encoding,
        }
    }
}

#[async_trait]
impl Sink for HttpSink {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let (body, content_type) = encoding::encode_http_body(self.encoding, events)?;

        let mut request = self
            .client
            .request(self.method.clone(), &self.url)
            .header("Content-Type", content_type)
            .timeout(Duration::from_secs(10))
            .body(body);

        for (key, value) in &self.headers {
            request = request.header(key, value);
        }

        let response = request.send().await?;

        if response.status().is_success() {
            info!(
                "Sent {} events to sink '{}' ({})",
                events.len(),
                self.name,
                self.url
            );
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Sink '{}' rejected the batch: {} - {}",
                self.name,
                status,
                body
            )
        }
    }
}
