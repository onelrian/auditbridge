mod config;
mod models;
mod netbird;
mod sinks;

#[cfg(test)]
mod tests;

use anyhow::Result;
use chrono::{DateTime, Utc};
use config::{Config, SinkConfig};
use netbird::NetbirdClient;
use sinks::{HttpSink, LokiSink, Sink, WazuhSink};
use std::collections::HashMap;
use std::env;
use tokio::time::sleep;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            error!("Configuration error: {}", e);
            std::process::exit(1);
        }
    };

    info!("========================================");
    info!("auditbridge (NetBird -> {} sink(s))", config.sinks.len());
    info!("========================================");
    info!("Netbird API: {}", config.netbird_api_url);
    info!("Check interval: {:?}", config.check_interval);
    info!("========================================");

    let sinks = build_sinks(&config.sinks).await;
    if sinks.is_empty() {
        error!("No sinks configured, nothing to do");
        std::process::exit(1);
    }

    let nb_client = NetbirdClient::new(
        config.netbird_api_url.clone(),
        config.netbird_api_token.clone(),
    );

    // Each sink advances its own watermark, so one down sink never blocks or
    // duplicates delivery to the others.
    let mut cursors: HashMap<String, Option<DateTime<Utc>>> =
        sinks.iter().map(|s| (s.name().to_string(), None)).collect();

    info!("Started monitoring...");

    loop {
        process_cycle(&nb_client, &sinks, &mut cursors).await;
        sleep(config.check_interval).await;
    }
}

async fn build_sinks(configs: &[SinkConfig]) -> Vec<Box<dyn Sink>> {
    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    for cfg in configs {
        match cfg {
            SinkConfig::Loki(url) => {
                let sink = LokiSink::new(url.clone());
                if let Err(e) = sink.wait_for_ready().await {
                    tracing::warn!("Loki check failed: {}. Continuing anyway...", e);
                }
                sinks.push(Box::new(sink));
            }
            SinkConfig::Wazuh(addr) => sinks.push(Box::new(WazuhSink::new(addr.clone()))),
            SinkConfig::Http(url) => sinks.push(Box::new(HttpSink::new(url.clone()))),
        }
    }
    sinks
}

// Fetches once per cycle (the NetBird audit endpoint has no server-side
// filtering), then delivers to each sink against its own cursor so a failed
// send only holds back that one sink's watermark, never the others'.
async fn process_cycle(
    nb_client: &NetbirdClient,
    sinks: &[Box<dyn Sink>],
    cursors: &mut HashMap<String, Option<DateTime<Utc>>>,
) {
    let mut events = match nb_client.fetch_events().await {
        Ok(events) => events,
        Err(e) => {
            error!("Failed to fetch events from Netbird: {}", e);
            return;
        }
    };

    events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    for sink in sinks {
        let cursor = cursors.entry(sink.name().to_string()).or_insert(None);

        let mut pending = events.clone();
        if let Some(last_ts) = *cursor {
            pending.retain(|e| {
                DateTime::parse_from_rfc3339(&e.timestamp)
                    .map(|ts| ts.with_timezone(&Utc) > last_ts)
                    .unwrap_or(false)
            });
        }

        if pending.is_empty() {
            continue;
        }

        let count = pending.len();
        match sink.send(&pending).await {
            Ok(_) => {
                if let Some(last_event) = pending.last() {
                    if let Ok(ts) = DateTime::parse_from_rfc3339(&last_event.timestamp) {
                        *cursor = Some(ts.with_timezone(&Utc));
                    }
                }
                info!("Delivered {} events to {}", count, sink.name());
            }
            Err(e) => error!("Failed to send {} events to {}: {}", count, sink.name(), e),
        }
    }
}
