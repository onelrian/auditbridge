pub mod http;
pub mod loki;
pub mod wazuh;

use crate::models::Event;
use anyhow::Result;
use async_trait::async_trait;

pub use http::HttpSink;
pub use loki::LokiSink;
pub use wazuh::WazuhSink;

/// A destination events are shipped to. `name()` doubles as the per-sink
/// cursor key, so each sink advances independently: one down sink never
/// blocks or duplicates delivery to the others.
#[async_trait]
pub trait Sink: Send + Sync {
    fn name(&self) -> &str;
    async fn send(&self, events: &[Event]) -> Result<()>;
}
