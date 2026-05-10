//! Read the moltbook queue.jsonl. Entries are line-delimited JSON, each looking like:
//! ```json
//! {"id": "incident-...", "kind": "incident|pattern|project",
//!  "source_path": "...", "source_hash": "sha256:...",
//!  "title_seed": "...", "body": "...full markdown...",
//!  "added_at": "...", "status": "pending"}
//! ```
//!
//! The publisher reads this file (read-only mount in production) and picks
//! the oldest incident (then pattern, then project) whose source_hash hasn't
//! been posted yet.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueueEntry {
    pub id: String,
    #[serde(default)] pub kind: String,
    #[serde(default)] pub source_path: String,
    #[serde(default)] pub source_hash: String,
    #[serde(default)] pub title_seed: String,
    #[serde(default)] pub body: String,
    #[serde(default)] pub added_at: String,
    #[serde(default = "default_status")] pub status: String,
}

fn default_status() -> String { "pending".to_string() }

pub fn queue_path() -> PathBuf {
    std::env::var("MOLTBOOK_QUEUE_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/queue/queue.jsonl"))
}

pub fn read() -> Result<Vec<QueueEntry>> {
    let p = queue_path();
    if !p.exists() {
        return Ok(Vec::new());
    }
    let txt = std::fs::read_to_string(&p)
        .with_context(|| format!("reading {}", p.display()))?;
    let mut out = Vec::new();
    for (i, line) in txt.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') { continue; }
        match serde_json::from_str::<QueueEntry>(l) {
            Ok(e) => out.push(e),
            Err(e) => tracing::warn!(line = i + 1, error = %e, "queue: skip malformed line"),
        }
    }
    Ok(out)
}

/// Sort: incidents first, then patterns, then projects; tie-break by added_at ASC.
pub fn pick_next(entries: &[QueueEntry], posted_hashes: &[String]) -> Option<QueueEntry> {
    let posted: std::collections::HashSet<&str> = posted_hashes.iter().map(|s| s.as_str()).collect();
    let mut candidates: Vec<&QueueEntry> = entries.iter()
        .filter(|q| !posted.contains(q.source_hash.as_str()) && q.status == "pending")
        .collect();
    candidates.sort_by_key(|q| {
        let kp = match q.kind.as_str() {
            "incident" => 0,
            "pattern" => 1,
            "project" => 2,
            _ => 9,
        };
        (kp, q.added_at.clone())
    });
    candidates.first().map(|&e| e.clone())
}
