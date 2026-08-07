mod config;
mod loki;
mod models;
mod netbird;

#[cfg(test)]
mod tests;

use anyhow::Result;
use chrono::{DateTime, Utc};
use config::Config;
use loki::LokiClient;
use netbird::NetbirdClient;
use std::env;
use tokio::time::sleep;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        )
        .init();
    
    // Load config
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            error!("Configuration error: {}", e);
            std::process::exit(1);
        }
    };
    
    info!("========================================");
    info!("Signal Exporter (NetBird -> Loki)");
    info!("========================================");
    info!("Loki URL: {}", config.loki_url);
    info!("Netbird API: {}", config.netbird_api_url);
    info!("Check interval: {:?}", config.check_interval);
    info!("========================================");
    
    let loki_client = LokiClient::new(config.loki_url.clone());
    
    // Wait for Loki usually, but we can make it non-blocking or just warn if down.
    if let Err(e) = loki_client.wait_for_ready().await {
        warn!("Loki check failed: {}. Continuing anyway...", e);
    }

    let nb_client = NetbirdClient::new(config.netbird_api_url.clone(), config.netbird_api_token.clone());

    // State tracking
    let mut last_processed_timestamp: Option<DateTime<Utc>> = None;

    info!("Started monitoring...");

    loop {
        process_cycle(&nb_client, &loki_client, &mut last_processed_timestamp).await;
        sleep(config.check_interval).await;
    }
}

// Watermark only advances once `loki_client.send_events` confirms delivery, so a failed
// send leaves the cursor where it was and the same events are retried next cycle.
async fn process_cycle(
    nb_client: &NetbirdClient,
    loki_client: &LokiClient,
    last_processed_timestamp: &mut Option<DateTime<Utc>>,
) {
    match nb_client.fetch_events().await {
        Ok(mut events) => {
            // Sort by timestamp asc
            events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

            // Filter out already processed events
            if let Some(last_ts) = *last_processed_timestamp {
                events.retain(|e| {
                    if let Ok(ts) = DateTime::parse_from_rfc3339(&e.timestamp) {
                        ts.with_timezone(&Utc) > last_ts
                    } else {
                        false
                    }
                });
            }

            if !events.is_empty() {
                let count = events.len();
                info!("Found {} new events", count);

                match loki_client.send_events(&events).await {
                    Ok(_) => {
                        // Only advance the watermark once delivery is confirmed.
                        if let Some(last_event) = events.last() {
                            if let Ok(ts) = DateTime::parse_from_rfc3339(&last_event.timestamp) {
                                *last_processed_timestamp = Some(ts.with_timezone(&Utc));
                            }
                        }
                    }
                    Err(e) => error!("Failed to send to Loki: {}", e),
                }
            }
        }
        Err(e) => {
            error!("Failed to fetch events from Netbird: {}", e);
        }
    }
}
