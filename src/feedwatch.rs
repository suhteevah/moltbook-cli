//! Inbound-only observation of moltbook's feed for cult-coded vocabulary clusters.
//!
//! No engagement, no upvote, no comment, no follow — pure passive scan. Output:
//!
//!  - per-run summary to stdout (post counts, clustered posts, top hits)
//!  - structured JSON-lines log appended to `~/.config/moltbook/observations.jsonl`
//!    for trend analysis over time
//!  - optional Telegram digest if NEW clusters appear (dedup state at
//!    `~/.config/moltbook/feedwatch_state.json` suppresses re-alerts within 24h)
//!
//! By design this command writes NO content back to moltbook. Even if we discover an
//! account drowning in cult-coded posts, we do not engage. Intel without amplification.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

use crate::cult_vocab::{scan, ScanResult};

#[derive(Debug, Clone)]
pub struct Cluster {
    pub post_id: String,
    pub author: String,
    pub title: String,
    pub score: i64,
    pub submolt: String,
    pub scan: ScanResult,
}

#[derive(Debug, Default, Clone)]
pub struct ScanSummary {
    pub total: usize,
    pub clean: usize,
    pub soft_only: usize,
    pub hard_hit: usize,
    pub clustered: Vec<Cluster>,
}

#[derive(Debug, Serialize)]
struct Observation<'a> {
    observed_at: String,
    sort: &'a str,
    post_id: &'a str,
    author: &'a str,
    title: &'a str,
    score: i64,
    submolt: &'a str,
    hard_hits: &'a [&'a str],
    soft_hits: &'a [&'a str],
    cluster_score: usize,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct AlertState {
    /// post_id -> ISO8601 timestamp of last telegram alert
    #[serde(default)]
    alerted: HashMap<String, String>,
}

const ALERT_SUPPRESS_HOURS: i64 = 24;

fn observations_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .context("no HOME/USERPROFILE")?;
    Ok([&home, ".config", "moltbook", "observations.jsonl"].iter().collect())
}

fn alert_state_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .context("no HOME/USERPROFILE")?;
    Ok([&home, ".config", "moltbook", "feedwatch_state.json"].iter().collect())
}

fn append_observation(obs: &Observation) -> Result<()> {
    let p = observations_path()?;
    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).ok(); }
    let line = serde_json::to_string(obs)?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true).append(true).open(&p)
        .with_context(|| format!("opening {}", p.display()))?;
    writeln!(f, "{}", line)?;
    Ok(())
}

fn load_alert_state() -> AlertState {
    let Ok(p) = alert_state_path() else { return AlertState::default(); };
    let Ok(s) = std::fs::read_to_string(&p) else { return AlertState::default(); };
    serde_json::from_str(&s).unwrap_or_default()
}

fn save_alert_state(state: &AlertState) -> Result<()> {
    let p = alert_state_path()?;
    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).ok(); }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Returns post_ids from `new_clusters` that are NOT in the suppress window.
/// Side effect: prunes entries older than 7d from state.
fn filter_unalerted(state: &mut AlertState, new_clusters: &[Cluster]) -> Vec<String> {
    let now = Utc::now();
    // Prune
    state.alerted.retain(|_, ts| {
        DateTime::parse_from_rfc3339(ts)
            .map(|dt| (now - dt.with_timezone(&Utc)).num_days() < 7)
            .unwrap_or(false)
    });
    let mut fresh = Vec::new();
    for c in new_clusters {
        let suppress = state.alerted.get(&c.post_id)
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| (now - dt.with_timezone(&Utc)).num_hours() < ALERT_SUPPRESS_HOURS)
            .unwrap_or(false);
        if !suppress { fresh.push(c.post_id.clone()); }
    }
    fresh
}

/// Scan one feed sort (new/hot/top). Appends all hits to observations.jsonl.
/// Returns the in-memory summary (not telegram-aware).
pub async fn scan_feed(mb: &crate::Mb, sort: &str, limit: u32, threshold: usize) -> Result<ScanSummary> {
    let path = format!("/feed?sort={sort}&limit={limit}");
    let v: Value = mb.get_value(&path).await
        .with_context(|| format!("fetching {path}"))?;
    let posts = v.get("posts")
        .or_else(|| v.get("results"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    info!(returned = posts.len(), sort, "feed-watch fetch");

    let observed_at = Utc::now().to_rfc3339();
    let mut s = ScanSummary { total: posts.len(), ..Default::default() };

    for p in &posts {
        let post_id = p.get("id").and_then(|x| x.as_str()).unwrap_or("?");
        let author = p.pointer("/author/name").and_then(|x| x.as_str()).unwrap_or("?");
        let title = p.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let content = p.get("content").and_then(|x| x.as_str()).unwrap_or("");
        let score = p.get("score").and_then(|x| x.as_i64()).unwrap_or(0);
        let submolt = p.pointer("/submolt/name").and_then(|x| x.as_str()).unwrap_or("?");
        let blob = format!("{title}\n{content}");
        let r = scan(&blob);

        if r.is_clean() { s.clean += 1; continue; }
        if !r.hard.is_empty() { s.hard_hit += 1; }
        else if !r.soft.is_empty() { s.soft_only += 1; }

        let obs = Observation {
            observed_at: observed_at.clone(),
            sort,
            post_id, author, title, score, submolt,
            hard_hits: &r.hard,
            soft_hits: &r.soft,
            cluster_score: r.cluster_score(),
        };
        if let Err(e) = append_observation(&obs) {
            warn!(error = %e, "observations log write failed");
        }
        if r.cluster_score() >= threshold {
            s.clustered.push(Cluster {
                post_id: post_id.to_string(),
                author: author.to_string(),
                title: title.to_string(),
                score,
                submolt: submolt.to_string(),
                scan: r,
            });
        }
    }
    Ok(s)
}

/// Send a single Telegram digest covering the fresh (un-suppressed) clusters across all sorts.
/// Updates suppress state. No-op if every cluster is already alerted.
pub async fn maybe_alert(all_clusters: &[Cluster]) -> Result<usize> {
    if all_clusters.is_empty() { return Ok(0); }
    let mut state = load_alert_state();
    let fresh_ids = filter_unalerted(&mut state, all_clusters);
    if fresh_ids.is_empty() {
        save_alert_state(&state).ok();
        return Ok(0);
    }
    let fresh_set: std::collections::HashSet<&str> = fresh_ids.iter().map(|s| s.as_str()).collect();
    let fresh_clusters: Vec<&Cluster> = all_clusters.iter()
        .filter(|c| fresh_set.contains(c.post_id.as_str()))
        .collect();
    let mut summary = format!(
        "Moltbook cult-vocab — {} new clustered post(s):\n",
        fresh_clusters.len()
    );
    for c in &fresh_clusters {
        let title_short: String = c.title.chars().take(70).collect();
        summary.push_str(&format!(
            "- [{score:>3}] @{author} \"{title}\" hard={hard:?} soft={soft:?}\n",
            score = c.score, author = c.author, title = title_short,
            hard = c.scan.hard, soft = c.scan.soft,
        ));
    }
    let _ = crate::notify::send(&summary).await;
    let now = Utc::now().to_rfc3339();
    for id in &fresh_ids {
        state.alerted.insert(id.clone(), now.clone());
    }
    save_alert_state(&state)?;
    Ok(fresh_clusters.len())
}

/// Standalone CLI entry point — scans one sort, prints summary, optionally telegram-alerts.
pub async fn run(
    mb: &crate::Mb,
    sort: &str,
    limit: u32,
    cluster_threshold: usize,
    telegram: bool,
) -> Result<()> {
    let s = scan_feed(mb, sort, limit, cluster_threshold).await?;
    println!("=== feed-watch sort={sort} limit={limit} ===");
    println!("clean: {}  soft-only: {}  hard-hit: {}  clustered (>= {}): {}",
        s.clean, s.soft_only, s.hard_hit, cluster_threshold, s.clustered.len());
    for c in &s.clustered {
        let title_short: String = c.title.chars().take(60).collect();
        println!("  [{:>3}] @{}  hard={:?} soft={:?}  | {}", c.score, c.author, c.scan.hard, c.scan.soft, title_short);
        println!("    post_id={}", c.post_id);
    }
    if telegram {
        let alerted = maybe_alert(&s.clustered).await?;
        if alerted > 0 { println!("telegram: alerted on {alerted} fresh cluster(s)"); }
        else if !s.clustered.is_empty() { println!("telegram: all clusters already alerted within {}h — suppressed", ALERT_SUPPRESS_HOURS); }
    }
    Ok(())
}
