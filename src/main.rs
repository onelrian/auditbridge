mod config;
mod cursor;
mod metrics;
mod models;
mod netbird;
mod retry;
mod sinks;

#[cfg(test)]
mod tests;

use anyhow::Result;
use chrono::{DateTime, Utc};
use config::Config;
use metrics::Metrics;
use netbird::NetbirdClient;
use retry::{with_retry, RetryConfig};
use sinks::Sink;
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

    let sinks: Vec<Box<dyn Sink>> = config.sinks.iter().map(sinks::build_sink).collect();
    if sinks.is_empty() {
        error!("No sinks configured, nothing to do");
        std::process::exit(1);
    }

    let nb_client = NetbirdClient::new(
        config.netbird_api_url.clone(),
        config.netbird_api_token.clone(),
    );

    let persisted = config
        .cursor_file
        .as_deref()
        .map(cursor::load)
        .unwrap_or_default();
    if let Some(path) = &config.cursor_file {
        info!(
            "Cursor persistence: {} ({} sink(s) resumed)",
            path,
            persisted.len()
        );
    }

    // Each sink advances its own watermark, so one down sink never blocks or
    // duplicates delivery to the others.
    let mut cursors = build_initial_cursors(&sinks, &persisted);

    let metrics = Metrics::new();
    let metrics_for_server = metrics.clone();
    let metrics_port = config.metrics_port;
    tokio::spawn(async move {
        if let Err(e) = metrics::serve(metrics_port, metrics_for_server).await {
            error!("Metrics/health server failed: {}", e);
        }
    });

    info!("Started monitoring...");

    loop {
        process_cycle(&nb_client, &sinks, &mut cursors, &config.retry, &metrics).await;

        if let Some(path) = &config.cursor_file {
            let to_save: HashMap<String, DateTime<Utc>> = cursors
                .iter()
                .filter_map(|(name, ts)| ts.map(|ts| (name.clone(), ts)))
                .collect();
            if let Err(e) = cursor::save(path, &to_save) {
                error!("Failed to persist cursor file: {}", e);
            }
        }

        sleep(config.check_interval).await;
    }
}

// Seeds each configured sink's cursor from whatever was persisted for it, so
// a restart with an intact cursor file resumes instead of replaying the
// account's full audit history into every sink again.
fn build_initial_cursors(
    sinks: &[Box<dyn Sink>],
    persisted: &HashMap<String, DateTime<Utc>>,
) -> HashMap<String, Option<DateTime<Utc>>> {
    sinks
        .iter()
        .map(|s| (s.name().to_string(), persisted.get(s.name()).copied()))
        .collect()
}

// Fetches once per cycle, then delivers to each sink against its own cursor
// so a down sink never blocks the others. Both retry with backoff first, so
// a transient blip recovers without waiting a full CHECK_INTERVAL.
async fn process_cycle(
    nb_client: &NetbirdClient,
    sinks: &[Box<dyn Sink>],
    cursors: &mut HashMap<String, Option<DateTime<Utc>>>,
    retry_cfg: &RetryConfig,
    metrics: &Metrics,
) {
    let mut events = match with_retry("netbird fetch", retry_cfg, || nb_client.fetch_events()).await
    {
        Ok(events) => {
            metrics.record_fetch_success(events.len());
            events
        }
        Err(e) => {
            metrics.record_fetch_error();
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
        let op_name = format!("sink '{}' send", sink.name());
        match with_retry(&op_name, retry_cfg, || sink.send(&pending)).await {
            Ok(_) => {
                if let Some(last_event) = pending.last() {
                    if let Ok(ts) = DateTime::parse_from_rfc3339(&last_event.timestamp) {
                        *cursor = Some(ts.with_timezone(&Utc));
                    }
                }
                metrics.record_sink_success(sink.name(), count);
                info!("Delivered {} events to {}", count, sink.name());
            }
            Err(e) => {
                metrics.record_sink_error(sink.name());
                error!("Failed to send {} events to {}: {}", count, sink.name(), e);
            }
        }
    }
}
