//! Feed browsing + selective upvoting. Mirrors the python heartbeat's
//! `browse_and_upvote()` — fetch /feed?sort=hot, filter to topics that match
//! our domain, upvote up to N. Skips our own posts.

use anyhow::Result;
use serde_json::Value;
use tracing::{debug, info, warn};

/// Topic keywords that flag a feed post as "in our domain" and worth upvoting.
const TOPIC_KEYWORDS: &[&str] = &[
    "security", "static analysis", "lint", "code quality",
    "browser", "automation", "rust", "bare metal", "mcp",
    "agent tool", "clawhu",
    "agent", "claude", "model", "llm", "context", "memory",
    "training", "embedding", "vector", "rag", "tool use",
    "deploy", "docker", "podman", "systemd", "container",
    "build", "runtime", "kernel", "syscall", "wasm",
    "candle", "tokio", "actix", "axum", "tauri", "ollama",
    "obsidian", "wiki", "knowledge",
];

pub async fn upvote_hot(mb: &crate::Mb, max: usize, sleep_secs: u64) -> Result<usize> {
    let v: Value = mb.get_value("/feed?sort=hot&limit=20").await?;
    let posts = v.get("posts")
        .or_else(|| v.get("results"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    debug!(returned = posts.len(), "feed");
    let mut upvoted = 0usize;
    for p in &posts {
        if upvoted >= max { break; }
        let author = p.pointer("/author/name").and_then(|x| x.as_str()).unwrap_or("");
        if author == crate::OUR_AGENT_NAME { continue; }
        let title = p.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let content = p.get("content").and_then(|x| x.as_str()).unwrap_or("");
        let blob = format!("{title} {content}").to_lowercase();
        if !TOPIC_KEYWORDS.iter().any(|t| blob.contains(t)) { continue; }
        let Some(pid) = p.get("id").and_then(|x| x.as_str()) else { continue; };
        match mb.post_json(&format!("/posts/{pid}/upvote"), serde_json::json!({})).await {
            Ok(_) => {
                upvoted += 1;
                info!(title = %&title[..title.len().min(60)], "upvoted");
            }
            Err(e) => warn!(pid, error = %e, "upvote failed"),
        }
        if sleep_secs > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        }
    }
    Ok(upvoted)
}
