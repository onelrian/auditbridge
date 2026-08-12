use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;

/// A sink's delivery watermark plus the event IDs delivered exactly at that
/// watermark. NetBird can return multiple events sharing one timestamp, and a
/// later poll can surface a *new* event with a timestamp equal to the
/// watermark; tracking boundary IDs lets `process_cycle` deliver that event
/// without either dropping it (strict `>` filter) or re-sending the ones
/// already delivered (would need `>=`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkCursor {
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered_ids: Vec<String>,
}

/// Loads per-sink cursors from disk. A missing or corrupt file is not fatal:
/// this is a durability optimization, not a source of truth, so falling
/// back to an empty map (full history replay on this restart only) is
/// always safe, just wasteful. Files written by older versions (a bare
/// timestamp per sink, no ID tracking) are migrated in place.
pub fn load(path: &str) -> HashMap<String, SinkCursor> {
    match std::fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str::<HashMap<String, SinkCursor>>(&contents) {
            Ok(cursors) => cursors,
            Err(_) => match serde_json::from_str::<HashMap<String, DateTime<Utc>>>(&contents) {
                Ok(legacy) => legacy
                    .into_iter()
                    .map(|(name, ts)| {
                        (
                            name,
                            SinkCursor {
                                timestamp: Some(ts),
                                delivered_ids: Vec::new(),
                            },
                        )
                    })
                    .collect(),
                Err(e) => {
                    warn!("Cursor file '{}' is corrupt ({}), starting empty", path, e);
                    HashMap::new()
                }
            },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            warn!(
                "Failed to read cursor file '{}' ({}), starting empty",
                path, e
            );
            HashMap::new()
        }
    }
}

/// Writes cursors to disk via a temp-file-then-rename, so a crash mid-write
/// never leaves a partially-written, unparseable cursor file behind.
pub fn save(path: &str, cursors: &HashMap<String, SinkCursor>) -> Result<()> {
    let tmp_path = format!("{}.tmp", path);
    let contents = serde_json::to_string_pretty(cursors)?;
    std::fs::write(&tmp_path, contents)
        .with_context(|| format!("Failed to write temp cursor file '{}'", tmp_path))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("Failed to move cursor file into place at '{}'", path))?;
    Ok(())
}
