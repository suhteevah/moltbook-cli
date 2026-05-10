//! Persistent state for the publish/cycle pipeline. Wire-compatible with the python
//! heartbeat's `post_state.json` so we can swap back-and-forth during the migration.
//!
//! Schema:
//! ```json
//! {
//!   "last_post_at": "2026-05-09T...",
//!   "posted_source_hashes": ["sha256:...", ...],
//!   "post_history": [{queue_id, moltbook_post_id, title, posted_at, source_hash}],
//!   "pending_approval": {queue_id, source_hash, draft, sent_at, approval_chat_id, approval_message_id} | null,
//!   "rejected_drafts": [...],
//!   "telegram_offset": 0
//! }
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostState {
    #[serde(default)] pub last_post_at: Option<String>,
    #[serde(default)] pub posted_source_hashes: Vec<String>,
    #[serde(default)] pub post_history: Vec<PostHistory>,
    #[serde(default)] pub pending_approval: Option<PendingApproval>,
    #[serde(default)] pub rejected_drafts: Vec<Value>,
    #[serde(default)] pub telegram_offset: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostHistory {
    pub queue_id: String,
    pub moltbook_post_id: String,
    pub title: String,
    pub posted_at: String,
    pub source_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub queue_id: String,
    pub source_hash: String,
    pub draft: Draft,
    pub sent_at: String,
    #[serde(default)] pub approval_chat_id: Option<String>,
    #[serde(default)] pub approval_message_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub title: String,
    pub body: String,
}

pub fn state_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("MOLTBOOK_POST_STATE") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .context("no HOME/USERPROFILE")?;
    Ok([&home, ".config", "moltbook", "post_state.json"].iter().collect())
}

pub fn load() -> Result<PostState> {
    let p = state_path()?;
    if !p.exists() {
        return Ok(PostState::default());
    }
    let txt = std::fs::read_to_string(&p)
        .with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_str(&txt)
        .with_context(|| format!("parsing {}", p.display()))
}

pub fn save(state: &PostState) -> Result<()> {
    let p = state_path()?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = p.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(state)?;
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("renaming to {}", p.display()))?;
    Ok(())
}
