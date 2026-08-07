use crate::models::Event;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;

/// How an event batch becomes bytes on the wire. Transports (http, syslog)
/// are generic; encodings are what actually make a specific backend's wire
/// format correct. Most new backends only need an existing encoding plus
/// config, not a new encoding variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Flat JSON array of events. Works for most generic HTTP intake APIs.
    Json,
    /// Newline-delimited JSON, one event per line.
    Ndjson,
    /// Grafana Loki's push API shape: events grouped into per-label-set
    /// streams with nanosecond timestamps. Loki rejects out-of-order
    /// timestamps within a stream and needs this grouping to accept the
    /// batch at all, so this can't be reduced to flat JSON + a template.
    Loki,
    /// RFC 3164 syslog framing with the event JSON as the MSG part. The
    /// safer default for syslog destinations: RFC 5424 support varies by
    /// consumer (Wazuh's own support for it is inconsistently documented).
    Syslog3164,
    /// RFC 5424 syslog framing, for consumers confirmed to support it.
    Syslog5424,
}

impl Encoding {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "json" => Ok(Encoding::Json),
            "ndjson" => Ok(Encoding::Ndjson),
            "loki" => Ok(Encoding::Loki),
            "syslog3164" => Ok(Encoding::Syslog3164),
            "syslog5424" => Ok(Encoding::Syslog5424),
            other => bail!(
                "Unknown encoding '{}' (expected json, ndjson, loki, syslog3164, syslog5424)",
                other
            ),
        }
    }
}

/// Encodes a batch for the `http` transport. Returns the body bytes and the
/// Content-Type header to send with them.
pub fn encode_http_body(encoding: Encoding, events: &[Event]) -> Result<(Vec<u8>, &'static str)> {
    match encoding {
        Encoding::Json => Ok((serde_json::to_vec(events)?, "application/json")),
        Encoding::Ndjson => {
            let mut body = Vec::new();
            for event in events {
                serde_json::to_writer(&mut body, event)?;
                body.push(b'\n');
            }
            Ok((body, "application/x-ndjson"))
        }
        Encoding::Loki => Ok((
            serde_json::to_vec(&build_loki_push_request(events))?,
            "application/json",
        )),
        Encoding::Syslog3164 | Encoding::Syslog5424 => {
            bail!("syslog encodings are only valid on the syslog transport")
        }
    }
}

/// Encodes a batch for the `syslog` transport as newline-terminated frames.
pub fn encode_syslog_frames(encoding: Encoding, events: &[Event]) -> Result<Vec<String>> {
    match encoding {
        Encoding::Syslog3164 => Ok(events.iter().map(format_rfc3164).collect()),
        Encoding::Syslog5424 => Ok(events.iter().map(format_rfc5424).collect()),
        Encoding::Json | Encoding::Ndjson | Encoding::Loki => {
            bail!("json/ndjson/loki encodings are only valid on the http transport")
        }
    }
}

fn event_json(event: &Event) -> serde_json::Value {
    serde_json::json!({
        "event_id": event.id,
        "timestamp": event.timestamp,
        "activity": event.activity,
        "activity_code": event.activity_code,
        "initiator_id": event.initiator_id,
        "initiator_email": event.initiator_email,
        "initiator_name": event.initiator_name,
        "target_id": event.target_id,
        "account_id": event.account_id,
        "meta": event.meta,
    })
}

// local0.info: facility 16 * 8 + severity 6, the RFC 5424 example PRI most
// syslog collectors expect for application-level logs.
const SYSLOG_PRI: u32 = 134;

fn format_rfc3164(event: &Event) -> String {
    // <PRI>TIMESTAMP HOSTNAME TAG: MSG. RFC 3164 timestamps are notionally
    // "Mmm dd hh:mm:ss" local time, but most collectors (including syslog-ng
    // and rsyslog defaults) accept ISO 8601 in the field without complaint,
    // and it keeps this encoder timezone-independent.
    format!(
        "<{}>{} auditbridge netbird-audit: {}\n",
        SYSLOG_PRI,
        event.timestamp,
        event_json(event)
    )
}

fn format_rfc5424(event: &Event) -> String {
    // <PRI>VERSION TIMESTAMP HOSTNAME APP-NAME PROCID MSGID STRUCTURED-DATA MSG
    format!(
        "<{}>1 {} auditbridge netbird-audit - AUDIT - {}\n",
        SYSLOG_PRI,
        event.timestamp,
        event_json(event)
    )
}

#[derive(Debug, Serialize)]
struct LokiStream {
    stream: HashMap<String, String>,
    values: Vec<(String, String)>,
}

#[derive(Debug, Serialize)]
struct LokiPushRequest {
    streams: Vec<LokiStream>,
}

fn build_loki_push_request(events: &[Event]) -> LokiPushRequest {
    let mut streams: HashMap<String, LokiStream> = HashMap::new();

    for event in events {
        let mut labels = HashMap::new();
        labels.insert("job".to_string(), "netbird-events".to_string());
        labels.insert(
            "account_id".to_string(),
            event
                .account_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        );
        labels.insert("activity".to_string(), event.activity.clone());
        labels.insert("activity_code".to_string(), event.activity_code.clone());

        let label_key = format!(
            "{{{}}}",
            labels
                .iter()
                .map(|(k, v)| format!("{}=\"{}\"", k, v))
                .collect::<Vec<_>>()
                .join(",")
        );

        // initiator_email/name stay in the JSON body, not labels: they're
        // higher cardinality than the rest of the label set and would
        // fragment Loki streams per-user instead of per-activity.
        let ts_ns = timestamp_to_nanoseconds(&event.timestamp);
        let log_line = event_json(event).to_string();

        streams
            .entry(label_key)
            .or_insert_with(|| LokiStream {
                stream: labels.clone(),
                values: Vec::new(),
            })
            .values
            .push((ts_ns, log_line));
    }

    LokiPushRequest {
        streams: streams.into_values().collect(),
    }
}

fn timestamp_to_nanoseconds(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .or_else(|_| {
            let ts = timestamp.trim_end_matches('Z');
            DateTime::parse_from_rfc3339(&format!("{}+00:00", ts))
        })
        .map(|dt| (dt.timestamp_nanos_opt().unwrap_or(0)).to_string())
        .unwrap_or_else(|_| Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string())
}
