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
use config::Config;
use cursor::SinkCursor;
use metrics::Metrics;
use models::Event;
use netbird::NetbirdClient;
use retry::{with_retry, RetryConfig};
use sinks::encoding::parse_event_timestamp;
use sinks::Sink;
use std::collections::{HashMap, HashSet};
use std::env;
use tokio::time::sleep;
use tracing::{error, info, warn};

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

        process_cycle(
            nb_client,
            sinks,
            cursors,
            &config.retry,
            config.batch_size,
            metrics,
        )
        .await;

        if let Some(path) = &config.cursor_file {
            let to_save: HashMap<String, SinkCursor> = cursors
                .iter()
                .filter(|(_, cursor)| cursor.watermark.is_some())
                .map(|(name, cursor)| (name.clone(), cursor.clone()))
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
            let name = s.name().to_string();
            let cursor = persisted.get(&name).cloned().unwrap_or_default();
            (name, cursor)
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
    batch_size: usize,
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
        let cursor = cursors.entry(sink.name().to_string()).or_default().clone();

        let pending = select_pending(&events, &cursor, sink.name());

        if pending.is_empty() {
            continue;
        }

        let count = pending.len();
        let op_name = format!("sink '{}' send", sink.name());
        // A full-history replay (fresh install, lost cursor) can produce a
        // very large batch in one poll; most intake endpoints cap payload
        // size, so split into chunks that each get the usual retry/backoff.
        // 0 = no chunking. The watermark advances only when every chunk
        // delivers, so a partial failure retries the whole set next cycle.
        let chunk_size = if batch_size == 0 {
            count.max(1)
        } else {
            batch_size
        };

        let mut all_delivered = true;
        for chunk in pending.chunks(chunk_size) {
            match with_retry(&op_name, retry_cfg, || sink.send(chunk)).await {
                Ok(_) => {}
                Err(e) => {
                    all_delivered = false;
                    metrics.record_sink_error(sink.name());
                    error!(
                        "Failed to send {} of {} events to {}: {}",
                        chunk.len(),
                        count,
                        sink.name(),
                        e
                    );
                    break;
                }
            }
        }

        if all_delivered {
            let mut advanced = cursor;
            advanced.record_delivered(&pending);
            cursors.insert(sink.name().to_string(), advanced);
            metrics.record_sink_success(sink.name(), count);
            info!("Delivered {} events to {}", count, sink.name());
        }
    }
}

// Picks the events this sink has not been confirmed to have received yet.
//
// A strict `ts > watermark` filter loses events: when several events share
// the newest timestamp, the watermark lands on that timestamp and any further
// event with the same timestamp in a later poll is excluded forever. So an
// event is also pending when it sits exactly at the watermark and its ID is
// not one of the IDs already delivered there.
//
// Duplicates are collapsed by (timestamp, id), keeping the first occurrence:
// at-least-once is a retry guarantee, not a licence to send the same event
// twice inside a single request.
fn select_pending(events: &[Event], cursor: &SinkCursor, sink_name: &str) -> Vec<Event> {
    let mut pending: Vec<Event> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut warned_unparseable = false;

    for event in events {
        match parse_event_timestamp(&event.timestamp) {
            Some(ts) => {
                let is_new = match cursor.watermark {
                    None => true,
                    Some(watermark) => {
                        ts > watermark
                            || (ts == watermark && !cursor.boundary_ids.contains(&event.id))
                    }
                };

                if is_new && seen.insert((event.timestamp.clone(), event.id.clone())) {
                    pending.push(event.clone());
                }
            }
            // An unparseable timestamp cannot be ordered, so it can never be
            // recorded as delivered and is re-sent on every cycle while it
            // stays in NetBird's history. That is the deliberate trade: the
            // alternative is dropping it silently, which is the event loss
            // this ticket is about. Kept loud so it can be fixed upstream.
            None => {
                if !warned_unparseable {
                    warn!(
                        "Event '{}' has an unparseable timestamp {:?}; delivering it without \
                         advancing the watermark for sink '{}'",
                        event.id, event.timestamp, sink_name
                    );
                    warned_unparseable = true;
                }
                if seen.insert((event.timestamp.clone(), event.id.clone())) {
                    pending.push(event.clone());
                }
            }
        }
    }

    pending
}
