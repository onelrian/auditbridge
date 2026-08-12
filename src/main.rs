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
use cursor::SinkCursor;
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

    let persisted: HashMap<String, SinkCursor> = config
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

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        info!("Shutdown signal received, stopping after the in-flight cycle completes");
        let _ = shutdown_tx.send(true);
    });

    info!("Started monitoring...");

    run(
        &nb_client,
        &sinks,
        &mut cursors,
        &config,
        &metrics,
        shutdown_rx,
    )
    .await;

    info!("Shutdown complete");
    Ok(())
}

// Waits until the process finishes the poll cycle it's currently in (its
// duration already bounded by RETRY_MAX_ATTEMPTS/RETRY_MAX_DELAY_MS on the
// fetch and each sink's send) before stopping, rather than aborting a batch
// mid-send. Only the idle wait between cycles gets interrupted immediately.
async fn run(
    nb_client: &NetbirdClient,
    sinks: &[Box<dyn Sink>],
    cursors: &mut HashMap<String, SinkCursor>,
    config: &Config,
    metrics: &Metrics,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *shutdown_rx.borrow() {
            return;
        }

        process_cycle(nb_client, sinks, cursors, &config.retry, metrics).await;

        if let Some(path) = &config.cursor_file {
            let to_save: HashMap<String, SinkCursor> = cursors
                .iter()
                .filter_map(|(name, c)| c.timestamp.map(|_| (name.clone(), c.clone())))
                .collect();
            if let Err(e) = cursor::save(path, &to_save) {
                error!("Failed to persist cursor file: {}", e);
            }
        }

        if *shutdown_rx.borrow() {
            return;
        }

        tokio::select! {
            _ = sleep(config.check_interval) => {}
            _ = shutdown_rx.changed() => {}
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install SIGINT handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

// Seeds each configured sink's cursor from whatever was persisted for it, so
// a restart with an intact cursor file resumes instead of replaying the
// account's full audit history into every sink again.
fn build_initial_cursors(
    sinks: &[Box<dyn Sink>],
    persisted: &HashMap<String, SinkCursor>,
) -> HashMap<String, SinkCursor> {
    sinks
        .iter()
        .map(|s| {
            (
                s.name().to_string(),
                persisted.get(s.name()).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

// Fetches once per cycle, then delivers to each sink against its own cursor
// so a down sink never blocks the others. Both retry with backoff first, so
// a transient blip recovers without waiting a full CHECK_INTERVAL.
async fn process_cycle(
    nb_client: &NetbirdClient,
    sinks: &[Box<dyn Sink>],
    cursors: &mut HashMap<String, SinkCursor>,
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
        let cursor = cursors.entry(sink.name().to_string()).or_default();

        let mut pending = events.clone();
        if let Some(last_ts) = cursor.timestamp {
            // Strictly newer events are pending; events exactly at the
            // watermark are pending only if they were not delivered before,
            // which tracks events NetBird surfaces later with a timestamp
            // equal to an already-delivered one.
            pending.retain(|e| {
                DateTime::parse_from_rfc3339(&e.timestamp)
                    .map(|ts| {
                        let ts = ts.with_timezone(&Utc);
                        ts > last_ts || (ts == last_ts && !cursor.delivered_ids.contains(&e.id))
                    })
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
                        let ts = ts.with_timezone(&Utc);
                        cursor.timestamp = Some(ts);
                        // Only IDs at the watermark matter; a later, higher
                        // watermark replaces this set wholesale.
                        cursor.delivered_ids = pending
                            .iter()
                            .filter(|e| {
                                DateTime::parse_from_rfc3339(&e.timestamp)
                                    .map(|t| t.with_timezone(&Utc) == ts)
                                    .unwrap_or(false)
                            })
                            .map(|e| e.id.clone())
                            .collect();
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
