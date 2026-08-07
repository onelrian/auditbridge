use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use tracing::warn;

/// Loads per-sink cursors from disk. A missing or corrupt file is not fatal:
/// this is a durability optimization, not a source of truth, so falling
/// back to an empty map (full history replay on this restart only) is
/// always safe, just wasteful.
pub fn load(path: &str) -> HashMap<String, DateTime<Utc>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|e| {
            warn!("Cursor file '{}' is corrupt ({}), starting empty", path, e);
            HashMap::new()
        }),
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
pub fn save(path: &str, cursors: &HashMap<String, DateTime<Utc>>) -> Result<()> {
    let tmp_path = format!("{}.tmp", path);
    let contents = serde_json::to_string_pretty(cursors)?;
    std::fs::write(&tmp_path, contents)
        .with_context(|| format!("Failed to write temp cursor file '{}'", tmp_path))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("Failed to move cursor file into place at '{}'", path))?;
    Ok(())
}
