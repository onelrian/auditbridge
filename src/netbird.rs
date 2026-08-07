use crate::models::Event;
use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;

pub struct NetbirdClient {
    client: Client,
    base_url: String,
    token: String,
}

impl NetbirdClient {
    pub fn new(base_url: String, token: String) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let base_url = if base_url.ends_with("/api") {
            base_url.trim_end_matches("/api").to_string()
        } else {
            base_url
        };

        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            base_url,
            token,
        }
    }

    // /api/events/audit has no pagination or since/cursor params (confirmed
    // against docs.netbird.io/api/resources/events), so every poll fetches
    // this account's full audit history. See the README's "Scaling Limits"
    // section for why CURSOR_FILE only partly mitigates this.
    pub async fn fetch_events(&self) -> Result<Vec<Event>> {
        let url = format!("{}/api/events/audit", self.base_url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Token {}", self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to send request to Netbird API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Netbird API Error: {} - {}", status, body);
        }

        let events = response
            .json::<Vec<Event>>()
            .await
            .context("Failed to parse Netbird API response")?;

        Ok(events)
    }
}
