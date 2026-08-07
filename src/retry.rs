use anyhow::Result;
use rand::Rng;
use std::future::Future;
use std::time::Duration;
use tracing::warn;

#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl RetryConfig {
    pub fn from_env() -> Self {
        Self {
            max_attempts: env_u32("RETRY_MAX_ATTEMPTS", 5),
            base_delay: Duration::from_millis(env_u64("RETRY_BASE_DELAY_MS", 500)),
            max_delay: Duration::from_millis(env_u64("RETRY_MAX_DELAY_MS", 30_000)),
        }
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Retries `f` with "full jitter" exponential backoff (AWS's recommended
/// strategy: sleep for a random duration in [0, min(cap, base * 2^attempt)]
/// rather than a fixed exponential delay), up to `max_attempts` bounded so a
/// sustained outage can't retry, or log, forever.
pub async fn with_retry<T, Fut>(
    op_name: &str,
    cfg: &RetryConfig,
    mut f: impl FnMut() -> Fut,
) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt >= cfg.max_attempts => return Err(e),
            Err(e) => {
                let delay = backoff_delay(cfg, attempt);
                warn!(
                    "{} failed (attempt {}/{}), retrying in {:?}: {}",
                    op_name, attempt, cfg.max_attempts, delay, e
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn backoff_delay(cfg: &RetryConfig, attempt: u32) -> Duration {
    // attempt=1 (first failure) -> base * 2^0, attempt=2 -> base * 2^1, ...
    let exponent = attempt.saturating_sub(1).min(20);
    let exp_ms = cfg
        .base_delay
        .as_millis()
        .saturating_mul(1u128 << exponent)
        .min(cfg.max_delay.as_millis());
    let jittered_ms = rand::thread_rng().gen_range(0..=exp_ms.max(1));
    Duration::from_millis(jittered_ms as u64)
}
