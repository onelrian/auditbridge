use crate::sinks::encoding::Encoding;
use crate::sinks::syslog::SyslogProtocol;
use anyhow::{Context, Result};
use reqwest::Method;
use std::env;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    Syslog,
}

impl Transport {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "http" => Ok(Transport::Http),
            "syslog" => Ok(Transport::Syslog),
            other => anyhow::bail!("Unknown transport '{}' (expected http or syslog)", other),
        }
    }
}

/// Fully resolved config for one sink: a name (also the cursor key), a
/// transport (how bytes are delivered), and an encoding (what the bytes
/// look like). Adding a new backend that speaks plain HTTP or syslog is
/// just a new entry here via env vars, never a new Rust type.
#[derive(Debug, Clone)]
pub struct SinkSpec {
    pub name: String,
    pub transport: Transport,
    pub encoding: Encoding,
    // http transport
    pub url: Option<String>,
    pub method: Method,
    pub headers: Vec<(String, String)>,
    // syslog transport
    pub addr: Option<String>,
    pub protocol: Option<SyslogProtocol>,
}

#[derive(Debug)]
pub struct Config {
    pub netbird_api_url: String,
    pub netbird_api_token: String,
    pub check_interval: Duration,
    pub sinks: Vec<SinkSpec>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let sinks = parse_sinks(&env::var("SINKS").unwrap_or_else(|_| "loki".to_string()))?;

        Ok(Self {
            netbird_api_url: env::var("NETBIRD_API_URL")
                .unwrap_or_else(|_| "https://api.netbird.io".to_string())
                .trim_end_matches('/')
                .to_string(),
            netbird_api_token: env::var("NETBIRD_API_TOKEN")
                .context("NETBIRD_API_TOKEN is required")?,
            check_interval: Duration::from_secs(
                env::var("CHECK_INTERVAL")
                    .unwrap_or_else(|_| "10".to_string())
                    .parse()
                    .unwrap_or(10),
            ),
            sinks,
        })
    }
}

// Split out for unit testing without the process-wide env dependency of `from_env`.
fn parse_sinks(sinks_var: &str) -> Result<Vec<SinkSpec>> {
    sinks_var
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_sink_spec)
        .collect()
}

fn parse_sink_spec(name: &str) -> Result<SinkSpec> {
    let prefix = format!("SINK_{}_", name.to_uppercase());
    let var = |suffix: &str| env::var(format!("{}{}", prefix, suffix)).ok();

    // "loki" and "wazuh" get zero-config defaults for the two integrations
    // this project documents out of the box. Any other name is a fully
    // generic sink: the caller supplies transport/encoding/destination.
    let (default_transport, default_encoding, default_url, default_addr, default_protocol) =
        match name {
            "loki" => {
                // LOKI_URL (legacy) is a base URL with the push path appended here for
                // backward compatibility. SINK_LOKI_URL, like every other sink's URL,
                // is used verbatim if the caller sets it instead.
                let base = env::var("LOKI_URL").unwrap_or_else(|_| "http://loki:3100".to_string());
                let base = base.trim_end_matches('/');
                (
                    Some(Transport::Http),
                    Some(Encoding::Loki),
                    Some(format!("{}/loki/api/v1/push", base)),
                    None,
                    None,
                )
            }
            "wazuh" => (
                Some(Transport::Syslog),
                Some(Encoding::Syslog3164),
                None,
                env::var("WAZUH_ADDR").ok(),
                Some(SyslogProtocol::Tcp),
            ),
            _ => (None, None, None, None, None),
        };

    let transport = match var("TRANSPORT").as_deref() {
        Some(t) => Transport::parse(t)?,
        None => default_transport
            .with_context(|| format!("{}TRANSPORT is required for sink '{}'", prefix, name))?,
    };

    let encoding = match var("ENCODING").as_deref() {
        Some(e) => Encoding::parse(e)?,
        None => default_encoding
            .with_context(|| format!("{}ENCODING is required for sink '{}'", prefix, name))?,
    };

    let method = match var("METHOD").as_deref() {
        Some(m) => Method::from_bytes(m.as_bytes())
            .with_context(|| format!("Invalid {}METHOD '{}'", prefix, m))?,
        None => Method::POST,
    };

    let headers = var("HEADERS")
        .map(|raw| parse_headers(&raw))
        .transpose()?
        .unwrap_or_default();

    let url = var("URL").or(default_url);
    let addr = var("ADDR").or(default_addr);
    let protocol = match var("PROTOCOL").as_deref() {
        Some(p) => Some(SyslogProtocol::parse(p)?),
        None => default_protocol,
    };

    if transport == Transport::Http && url.is_none() {
        anyhow::bail!(
            "{}URL is required for sink '{}' (transport=http)",
            prefix,
            name
        );
    }
    if transport == Transport::Syslog && addr.is_none() {
        anyhow::bail!(
            "{}ADDR is required for sink '{}' (transport=syslog)",
            prefix,
            name
        );
    }

    Ok(SinkSpec {
        name: name.to_string(),
        transport,
        encoding,
        url,
        method,
        headers,
        addr,
        protocol,
    })
}

// "Key1:Value1,Key2:Value2" -> [(Key1, Value1), (Key2, Value2)]
fn parse_headers(raw: &str) -> Result<Vec<(String, String)>> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair
                .split_once(':')
                .with_context(|| format!("Invalid header entry '{}', expected Key:Value", pair))?;
            Ok((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}
