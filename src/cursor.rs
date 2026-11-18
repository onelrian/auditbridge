use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use std::collections::{BTreeSet, HashMap};
use tracing::warn;

use crate::models::Event;
use crate::sinks::encoding::parse_event_timestamp;

/// A sink's delivery position: a watermark timestamp plus the IDs of the
/// events already delivered at exactly that timestamp.
///
/// A timestamp alone loses events. NetBird can return several events sharing
/// one timestamp in a poll, the watermark advances to that timestamp, and any
/// further event with the *same* timestamp that arrives in a later poll is
/// then excluded forever by a strict `ts > watermark` filter. Remembering the
/// boundary IDs makes delivery at-least-once instead: an unseen ID at the
/// watermark is still sent, while an ID already delivered there is not.
///
/// `boundary_ids` is bounded by the number of events sharing the newest
/// timestamp, not by history length: it is replaced whenever the watermark
/// strictly advances.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SinkCursor {
    /// Events strictly newer than this are always new.
    pub watermark: Option<DateTime<Utc>>,
    /// IDs already delivered at exactly `watermark`. `BTreeSet`, not
    /// `HashSet`, so the persisted JSON is deterministic and diffable.
    pub boundary_ids: BTreeSet<String>,
}

impl SinkCursor {
    /// Records a fully delivered batch: the watermark moves to the newest
    /// timestamp in it, and the boundary IDs become exactly the events sharing
    /// that timestamp.
    ///
    /// The watermark never decreases. A batch older than the current position
    /// leaves the cursor untouched rather than re-delivering everything
    /// between it and the watermark, and a batch at the same position only
    /// extends the boundary IDs instead of replacing them.
    ///
    /// Events with an unparseable timestamp are skipped: they cannot be
    /// ordered, so they are never recorded and are re-sent next cycle (see
    /// `select_pending` in `main.rs`).
    pub fn record_delivered(&mut self, delivered: &[Event]) {
        let newest = delivered
            .iter()
            .filter_map(|event| parse_event_timestamp(&event.timestamp))
            .max();

        let Some(newest) = newest else {
            return;
        };

        let ids: BTreeSet<String> = delivered
            .iter()
            .filter(|event| parse_event_timestamp(&event.timestamp) == Some(newest))
            .map(|event| event.id.clone())
            .collect();

        match self.watermark {
            Some(current) if newest < current => {}
            Some(current) if newest == current => self.boundary_ids.extend(ids),
            _ => {
                self.watermark = Some(newest);
                self.boundary_ids = ids;
            }
        }
    }
}

/// On-disk shape. Kept separate from `SinkCursor` so serialization stays an
/// explicit, documented contract rather than a derived struct layout.
#[derive(Serialize)]
struct StoredCursor<'a> {
    watermark: Option<DateTime<Utc>>,
    boundary_ids: &'a BTreeSet<String>,
}

impl Serialize for SinkCursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        StoredCursor {
            watermark: self.watermark,
            boundary_ids: &self.boundary_ids,
        }
        .serialize(serializer)
    }
}

/// Reads a cursor file written by this version, or by the version that stored
/// a bare timestamp per sink. The legacy shape must keep loading: discarding
/// it would replay the account's full audit history into every sink on the
/// first poll after an upgrade. A legacy entry loads with no boundary IDs, so
/// the event at its watermark is re-sent once, which is the at-least-once
/// trade this format exists to make.
pub fn load(path: &str) -> HashMap<String, SinkCursor> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            warn!(
                "Failed to read cursor file '{}' ({}), starting empty",
                path, e
            );
            return HashMap::new();
        }
    };

    match serde_json::from_str::<HashMap<String, StoredEntry>>(&contents) {
        Ok(entries) => entries
            .into_iter()
            .map(|(name, entry)| (name, entry.into_cursor()))
            .collect(),
        Err(e) => {
            warn!("Cursor file '{}' is corrupt ({}), starting empty", path, e);
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

/// One sink's entry, in either on-disk shape. Unambiguous because the two
/// shapes have different JSON types: an object is the current format, a
/// string is the legacy bare timestamp.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum StoredEntry {
    Current(SinkCursor),
    LegacyTimestamp(DateTime<Utc>),
}

impl StoredEntry {
    fn into_cursor(self) -> SinkCursor {
        match self {
            StoredEntry::Current(cursor) => cursor,
            StoredEntry::LegacyTimestamp(watermark) => SinkCursor {
                watermark: Some(watermark),
                boundary_ids: BTreeSet::new(),
            },
        }
    }
}

/// `SinkCursor` is deserialized through this mirror so the stored `watermark`
/// key is required to be nullable but `boundary_ids` may be absent.
impl<'de> serde::Deserialize<'de> for SinkCursor {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            watermark: Option<DateTime<Utc>>,
            #[serde(default)]
            boundary_ids: BTreeSet<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        Ok(SinkCursor {
            watermark: raw.watermark,
            boundary_ids: raw.boundary_ids,
        })
    }
}
