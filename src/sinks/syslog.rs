use super::encoding::{self, Encoding};
use super::Sink;
use crate::models::Event;
use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{timeout, Duration};
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyslogProtocol {
    Tcp,
    Udp,
}

impl SyslogProtocol {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "tcp" => Ok(SyslogProtocol::Tcp),
            "udp" => Ok(SyslogProtocol::Udp),
            other => anyhow::bail!("Unknown syslog protocol '{}' (expected tcp or udp)", other),
        }
    }
}

/// Generic syslog transport: any address, TCP or UDP, RFC 3164 or RFC 5424
/// framing (see `encoding.rs`). This is the agentless-friendly path most
/// SIEMs (Wazuh included) accept from a service with no local agent or
/// shared filesystem.
pub struct SyslogSink {
    name: String,
    addr: String,
    protocol: SyslogProtocol,
    encoding: Encoding,
}

impl SyslogSink {
    pub fn new(name: String, addr: String, protocol: SyslogProtocol, encoding: Encoding) -> Self {
        Self {
            name,
            addr,
            protocol,
            encoding,
        }
    }

    async fn send_tcp(&self, frames: &[String]) -> Result<()> {
        let mut stream = timeout(Duration::from_secs(10), TcpStream::connect(&self.addr))
            .await
            .with_context(|| {
                format!(
                    "Timed out connecting to syslog sink '{}' at {}",
                    self.name, self.addr
                )
            })?
            .with_context(|| {
                format!(
                    "Failed to connect to syslog sink '{}' at {}",
                    self.name, self.addr
                )
            })?;

        for frame in frames {
            stream
                .write_all(frame.as_bytes())
                .await
                .with_context(|| format!("Failed to write to syslog sink '{}'", self.name))?;
        }
        stream
            .flush()
            .await
            .with_context(|| format!("Failed to flush syslog sink '{}'", self.name))
    }

    async fn send_udp(&self, frames: &[String]) -> Result<()> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .with_context(|| format!("Failed to bind local UDP socket for sink '{}'", self.name))?;
        socket.connect(&self.addr).await.with_context(|| {
            format!(
                "Failed to resolve syslog sink '{}' at {}",
                self.name, self.addr
            )
        })?;

        for frame in frames {
            socket.send(frame.as_bytes()).await.with_context(|| {
                format!("Failed to send datagram to syslog sink '{}'", self.name)
            })?;
        }
        Ok(())
    }
}

#[async_trait]
impl Sink for SyslogSink {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let frames = encoding::encode_syslog_frames(self.encoding, events)?;

        match self.protocol {
            SyslogProtocol::Tcp => self.send_tcp(&frames).await?,
            SyslogProtocol::Udp => self.send_udp(&frames).await?,
        }

        info!(
            "Sent {} events to sink '{}' ({})",
            events.len(),
            self.name,
            self.addr
        );
        Ok(())
    }
}
