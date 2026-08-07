pub mod encoding;
pub mod http;
pub mod syslog;

use crate::config::{SinkSpec, Transport};
use crate::models::Event;
use anyhow::Result;
use async_trait::async_trait;
use http::HttpSink;
use syslog::SyslogSink;

/// A destination events are shipped to. `name()` doubles as the per-sink
/// cursor key, so each sink advances independently: one down sink never
/// blocks or duplicates delivery to the others.
#[async_trait]
pub trait Sink: Send + Sync {
    fn name(&self) -> &str;
    async fn send(&self, events: &[Event]) -> Result<()>;
}

/// Builds a concrete `Sink` from a config-driven spec. Adding a new backend
/// that speaks plain HTTP or syslog never needs a new match arm here, only
/// a new `SINKS` entry and its `SINK_<NAME>_*` env vars.
pub fn build_sink(spec: &SinkSpec) -> Box<dyn Sink> {
    match spec.transport {
        Transport::Http => Box::new(HttpSink::new(
            spec.name.clone(),
            spec.url.clone().expect("http sink must have a url"),
            spec.method.clone(),
            spec.headers.clone(),
            spec.encoding,
        )),
        Transport::Syslog => Box::new(SyslogSink::new(
            spec.name.clone(),
            spec.addr.clone().expect("syslog sink must have an addr"),
            spec.protocol.expect("syslog sink must have a protocol"),
            spec.encoding,
        )),
    }
}
