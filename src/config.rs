use anyhow::{Context, Result};
use std::env;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkConfig {
    Loki(String),
    Wazuh(String),
    Http(String),
}

#[derive(Debug)]
pub struct Config {
    pub netbird_api_url: String,
    pub netbird_api_token: String,
    pub check_interval: Duration,
    pub sinks: Vec<SinkConfig>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let sinks = parse_sinks(&env::var("SINKS").unwrap_or_else(|_| "loki".to_string()))?;

        Ok(Self {
            netbird_api_url: env::var("NETBIRD_API_URL")
                .unwrap_or_else(|_| "https://api.netbird.io".to_string())
                .trim_end_matches('/')
                .to_string(),
            netbird_api_token: env::var("NETBIRD_API_TOKEN")
                .context("NETBIRD_API_TOKEN is required")?,
            check_interval: Duration::from_secs(
                env::var("CHECK_INTERVAL")
                    .unwrap_or_else(|_| "10".to_string())
                    .parse()
                    .unwrap_or(10),
            ),
            sinks,
        })
    }
}

// Split out for unit testing without the process-wide env dependency of `from_env`.
fn parse_sinks(sinks_var: &str) -> Result<Vec<SinkConfig>> {
    sinks_var
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|kind| match kind {
            "loki" => Ok(SinkConfig::Loki(
                env::var("LOKI_URL").unwrap_or_else(|_| "http://loki:3100".to_string()),
            )),
            "wazuh" => Ok(SinkConfig::Wazuh(
                env::var("WAZUH_ADDR")
                    .context("WAZUH_ADDR is required when SINKS includes wazuh")?,
            )),
            "http" => Ok(SinkConfig::Http(
                env::var("HTTP_SINK_URL")
                    .context("HTTP_SINK_URL is required when SINKS includes http")?,
            )),
            other => anyhow::bail!(
                "Unknown sink '{}' in SINKS (expected loki, wazuh, http)",
                other
            ),
        })
        .collect()
}
