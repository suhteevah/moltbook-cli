//! Reciprocity — follow back agents who follow us, with cult-vocab gating.
//!
//! Strategy is deliberately conservative for now:
//!
//!  1. Pull our followers list via `/agents/{name}/followers?limit=50`.
//!  2. For each follower NOT already in our state file:
//!     - Skip if karma below MIN_FOLLOWER_KARMA (filters brand-new bot accounts).
//!     - Scan their `description` for cult-coded vocabulary; HARD hit or soft
//!       cluster ≥ SOFT_THRESHOLD → skip (do not amplify).
//!     - Skip if their `last_active` is older than STALE_DAYS (dead accounts).
//!     - POST `/agents/{name}/follow`.
//!     - Record in state with timestamp.
//!  3. Optionally send a single Telegram summary at the end.
//!
//! Phase 2 (visit-their-recent-posts and upvote/comment one) is parked until we
//! verify a reliable "posts by author X" API path. The current heuristic
//! `/posts?author_id=X` returned a global feed in earlier probes, not filtered.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn, debug};

const MIN_FOLLOWER_KARMA: i64 = 3;
const STALE_DAYS: i64 = 60;

#[derive(Debug, Default, Serialize, Deserialize)]
struct ReciprocityState {
    /// agent_name -> ISO8601 timestamp of when we followed them back
    #[serde(default)]
    followed_back: HashMap<String, String>,
    /// agents we attempted but failed to follow (transient or skipped); skipped reason
    #[serde(default)]
    last_skipped: HashMap<String, String>,
}

fn state_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .context("no HOME/USERPROFILE")?;
    Ok([&home, ".config", "moltbook", "reciprocity_state.json"].iter().collect())
}

fn load_state() -> ReciprocityState {
    let Ok(p) = state_path() else { return ReciprocityState::default(); };
    let Ok(s) = std::fs::read_to_string(&p) else { return ReciprocityState::default(); };
    serde_json::from_str(&s).unwrap_or_default()
}

fn save_state(s: &ReciprocityState) -> Result<()> {
    let p = state_path()?;
    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).ok(); }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

fn is_stale(last_active: &str) -> bool {
    let Ok(dt) = DateTime::parse_from_rfc3339(last_active) else { return false; };
    (Utc::now() - dt.with_timezone(&Utc)).num_days() > STALE_DAYS
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub followed: Vec<String>,
    pub skipped_cult: Vec<String>,
    pub skipped_low_karma: Vec<String>,
    pub skipped_stale: Vec<String>,
    pub skipped_already: usize,
    pub api_errors: Vec<String>,
}

pub async fn tend(mb: &crate::Mb, max_follows: usize, send_telegram: bool) -> Result<Outcome> {
    let mut state = load_state();
    let mut out = Outcome::default();

    let path = format!("/agents/{}/followers?limit=50", crate::OUR_AGENT_NAME);
    let resp: Value = match mb.get_value(&path).await {
        Ok(v) => v,
        Err(e) => { warn!(error = %e, "fetch followers failed"); return Ok(out); }
    };
    let followers = resp.get("followers")
        .and_then(|x| x.as_array()).cloned().unwrap_or_default();
    info!(count = followers.len(), "reciprocity: fetched followers");

    for f in &followers {
        if out.followed.len() >= max_follows { break; }
        let name = match f.get("name").and_then(|x| x.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if state.followed_back.contains_key(&name) {
            out.skipped_already += 1;
            continue;
        }
        let karma = f.get("karma").and_then(|x| x.as_i64()).unwrap_or(0);
        let description = f.get("description").and_then(|x| x.as_str()).unwrap_or("");
        let last_active = f.get("last_active").and_then(|x| x.as_str()).unwrap_or("");

        if karma < MIN_FOLLOWER_KARMA {
            debug!(name, karma, "reciprocity: skip — low karma");
            out.skipped_low_karma.push(name.clone());
            state.last_skipped.insert(name, format!("low_karma:{karma}"));
            continue;
        }
        if is_stale(last_active) {
            debug!(name, last_active, "reciprocity: skip — stale");
            out.skipped_stale.push(name.clone());
            state.last_skipped.insert(name, format!("stale:{last_active}"));
            continue;
        }
        let scan_target = format!("{name}\n{description}");
        let r = crate::cult_vocab::scan(&scan_target);
        if !r.hard.is_empty() || r.soft.len() >= 3 {
            warn!(name, hard = ?r.hard, soft = ?r.soft, "reciprocity: skip — cult vocab");
            out.skipped_cult.push(name.clone());
            state.last_skipped.insert(name, format!("cult:{:?}/{:?}", r.hard, r.soft));
            continue;
        }

        // Follow them.
        match mb.post_json(&format!("/agents/{name}/follow"), json!({})).await {
            Ok(_) => {
                info!(name, karma, "reciprocity: followed back");
                state.followed_back.insert(name.clone(), Utc::now().to_rfc3339());
                state.last_skipped.remove(&name);
                out.followed.push(name);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => {
                let err = format!("{e}");
                let trunc = err.chars().take(150).collect::<String>();
                warn!(name, error = %trunc, "reciprocity: follow failed");
                out.api_errors.push(format!("{name}: {trunc}"));
            }
        }
    }

    if let Err(e) = save_state(&state) {
        warn!(error = %e, "reciprocity: state save failed");
    }

    if send_telegram && !out.followed.is_empty() {
        let msg = format!(
            "Reciprocity: followed back {} agent(s) — {}. (skipped: cult={} low-karma={} stale={} already={})",
            out.followed.len(),
            out.followed.join(", "),
            out.skipped_cult.len(),
            out.skipped_low_karma.len(),
            out.skipped_stale.len(),
            out.skipped_already,
        );
        let _ = crate::notify::send(&msg).await;
    }

    Ok(out)
}
