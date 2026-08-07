use super::Sink;
use crate::models::Event;
use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::info;

// local0.info: facility 16 * 8 + severity 6, matches the RFC 5424 examples
// most syslog collectors (including Wazuh's Logcollector) expect for app logs.
const SYSLOG_PRI: &str = "134";

/// Ships events to a Wazuh manager as RFC 5424 syslog over TCP, the
/// documented agentless-friendly ingestion path for an external service
/// with no local agent or shared filesystem
/// (see documentation.wazuh.com/current/user-manual/capabilities/log-data-collection).
pub struct WazuhSink {
    addr: String,
}

impl WazuhSink {
    pub fn new(addr: String) -> Self {
        Self { addr }
    }

    fn format_event(event: &Event) -> String {
        let ts = &event.timestamp;
        let msg = serde_json::json!({
            "event_id": event.id,
            "activity": event.activity,
            "activity_code": event.activity_code,
            "initiator_id": event.initiator_id,
            "initiator_email": event.initiator_email,
            "initiator_name": event.initiator_name,
            "target_id": event.target_id,
            "account_id": event.account_id,
            "meta": event.meta,
        });

        // <PRI>VERSION TIMESTAMP HOSTNAME APP-NAME PROCID MSGID STRUCTURED-DATA MSG
        format!(
            "<{}>1 {} auditbridge netbird-audit - AUDIT - {}\n",
            SYSLOG_PRI, ts, msg
        )
    }
}

#[async_trait]
impl Sink for WazuhSink {
    fn name(&self) -> &str {
        "wazuh"
    }

    async fn send(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let mut stream = timeout(Duration::from_secs(10), TcpStream::connect(&self.addr))
            .await
            .context("Timed out connecting to Wazuh syslog endpoint")?
            .with_context(|| format!("Failed to connect to Wazuh syslog endpoint {}", self.addr))?;

        for event in events {
            let line = Self::format_event(event);
            stream
                .write_all(line.as_bytes())
                .await
                .context("Failed to write event to Wazuh syslog endpoint")?;
        }
        stream
            .flush()
            .await
            .context("Failed to flush Wazuh syslog connection")?;

        info!("Sent {} events to Wazuh ({})", events.len(), self.addr);
        Ok(())
    }
}
