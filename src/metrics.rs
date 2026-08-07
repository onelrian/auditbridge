use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Default)]
struct Inner {
    events_fetched_total: u64,
    netbird_fetch_errors_total: u64,
    events_delivered_total: HashMap<String, u64>,
    delivery_errors_total: HashMap<String, u64>,
    last_successful_poll_unix: Option<i64>,
    // Drives /readyz: the process is only "ready" once the NetBird API has
    // been reachable at least once and at least one sink has confirmed
    // delivery, not just because the HTTP server itself is up.
    last_fetch_ok: bool,
    sink_ok: HashMap<String, bool>,
}

#[derive(Debug, Default)]
pub struct Metrics {
    inner: RwLock<Inner>,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_fetch_success(&self, count: usize) {
        let mut inner = self.inner.write().unwrap();
        inner.events_fetched_total += count as u64;
        inner.last_successful_poll_unix = Some(Utc::now().timestamp());
        inner.last_fetch_ok = true;
    }

    pub fn record_fetch_error(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.netbird_fetch_errors_total += 1;
        inner.last_fetch_ok = false;
    }

    pub fn record_sink_success(&self, sink: &str, count: usize) {
        let mut inner = self.inner.write().unwrap();
        *inner
            .events_delivered_total
            .entry(sink.to_string())
            .or_insert(0) += count as u64;
        inner.sink_ok.insert(sink.to_string(), true);
    }

    pub fn record_sink_error(&self, sink: &str) {
        let mut inner = self.inner.write().unwrap();
        *inner
            .delivery_errors_total
            .entry(sink.to_string())
            .or_insert(0) += 1;
        inner.sink_ok.insert(sink.to_string(), false);
    }

    pub fn is_ready(&self) -> bool {
        let inner = self.inner.read().unwrap();
        inner.last_fetch_ok && inner.sink_ok.values().any(|&ok| ok)
    }

    pub fn render_prometheus(&self) -> String {
        let inner = self.inner.read().unwrap();
        let mut out = String::new();

        out.push_str(
            "# HELP auditbridge_events_fetched_total Total events fetched from the NetBird API\n",
        );
        out.push_str("# TYPE auditbridge_events_fetched_total counter\n");
        out.push_str(&format!(
            "auditbridge_events_fetched_total {}\n\n",
            inner.events_fetched_total
        ));

        out.push_str(
            "# HELP auditbridge_netbird_fetch_errors_total Total NetBird API fetch errors\n",
        );
        out.push_str("# TYPE auditbridge_netbird_fetch_errors_total counter\n");
        out.push_str(&format!(
            "auditbridge_netbird_fetch_errors_total {}\n\n",
            inner.netbird_fetch_errors_total
        ));

        out.push_str(
            "# HELP auditbridge_events_delivered_total Total events delivered, per sink\n",
        );
        out.push_str("# TYPE auditbridge_events_delivered_total counter\n");
        for (sink, count) in sorted(&inner.events_delivered_total) {
            out.push_str(&format!(
                "auditbridge_events_delivered_total{{sink=\"{}\"}} {}\n",
                sink, count
            ));
        }
        out.push('\n');

        out.push_str("# HELP auditbridge_delivery_errors_total Total delivery errors, per sink\n");
        out.push_str("# TYPE auditbridge_delivery_errors_total counter\n");
        for (sink, count) in sorted(&inner.delivery_errors_total) {
            out.push_str(&format!(
                "auditbridge_delivery_errors_total{{sink=\"{}\"}} {}\n",
                sink, count
            ));
        }
        out.push('\n');

        out.push_str(
            "# HELP auditbridge_last_successful_poll_timestamp_seconds Unix timestamp of the last successful NetBird fetch\n",
        );
        out.push_str("# TYPE auditbridge_last_successful_poll_timestamp_seconds gauge\n");
        out.push_str(&format!(
            "auditbridge_last_successful_poll_timestamp_seconds {}\n",
            inner.last_successful_poll_unix.unwrap_or(0)
        ));

        out
    }
}

fn sorted(map: &HashMap<String, u64>) -> Vec<(&String, &u64)> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by_key(|(k, _)| k.as_str());
    entries
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(metrics): State<Arc<Metrics>>) -> impl IntoResponse {
    if metrics.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

async fn metrics_handler(State(metrics): State<Arc<Metrics>>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        metrics.render_prometheus(),
    )
}

pub fn router(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
        .with_state(metrics)
}

pub async fn serve(port: u16, metrics: Arc<Metrics>) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Metrics/health server listening on {}", addr);
    axum::serve(listener, router(metrics)).await?;
    Ok(())
}
