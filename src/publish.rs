//! Queue → draft → safety → approval → post + verify pipeline.
//!
//! Mirrors the python heartbeat's `maybe_publish_post()` semantics so we can
//! cleanly retire it. Approval gating is via file drop at `/queue/decisions/<id>.{approve,reject}`.
//! State persists at `~/.config/moltbook/post_state.json` (wire-compatible with python).

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::queue::{self, QueueEntry};
use crate::state::{self, Draft, PendingApproval, PostHistory, PostState};

/// Strings that MUST NOT appear in any public post — private hostnames, identities, paths.
pub const SAFETY_BLOCKLIST: &[&str] = &[
    "kokonoe", "cnc-server", "cnc.server", "pixie-stl", "pixiedust",
    "Matt Gates", "Ridge Cell", "Chico, CA", "suhteevah", "swoop",
    "192.168.168.", "192.168.1.1", "10.0.0.", "100.77.", "100.102.",
    "207.244.232.227", "j:\\", "J:\\", "/home/heartbeat",
    "/opt/moltbook", "moltbook-heartbeat",
    "the-right-wire", "kalshi", "polymarket", "job-hunter",
    "mmichels88", "@gmail.com",
];

pub const TONE_BLOCKLIST: &[&str] = &[
    "great post", "thanks for sharing", "let me share",
    "in conclusion", "in summary", "🦞", "🚀", "🔥", "✨",
];

pub const GENERIC_TITLE_PREFIXES: &[&str] = &[
    "how i ", "a tale of", "the definitive guide", "the ultimate",
];

pub fn approval_mode() -> bool {
    std::env::var("MOLTBOOK_APPROVAL_MODE").as_deref() != Ok("0")
}

pub fn cooldown_hours() -> i64 {
    std::env::var("MOLTBOOK_PUBLISH_COOLDOWN_HOURS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24)
}

pub fn approval_timeout_hours() -> i64 {
    std::env::var("MOLTBOOK_APPROVAL_TIMEOUT_HOURS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12)
}

pub fn default_submolt() -> String {
    std::env::var("MOLTBOOK_SUBMOLT").unwrap_or_else(|_| "agents".to_string())
}

pub fn decisions_dir() -> PathBuf {
    PathBuf::from(std::env::var("MOLTBOOK_DECISIONS_DIR")
        .unwrap_or_else(|_| "/queue/decisions".to_string()))
}

fn cooldown_active(state: &PostState) -> bool {
    let Some(ref last) = state.last_post_at else { return false; };
    let Ok(dt) = DateTime::parse_from_rfc3339(last) else { return false; };
    let elapsed = (Utc::now() - dt.with_timezone(&Utc)).num_seconds();
    elapsed < cooldown_hours() * 3600
}

/// Read /queue/decisions/<id>.{approve,reject} and return the verdict if present.
fn check_decision_files(queue_id: &str) -> Option<&'static str> {
    let approve = decisions_dir().join(format!("{queue_id}.approve"));
    let reject = decisions_dir().join(format!("{queue_id}.reject"));
    if approve.exists() {
        // Try to unlink; ignore failure (read-only mount).
        let _ = std::fs::remove_file(&approve);
        return Some("APPROVE");
    }
    if reject.exists() {
        let _ = std::fs::remove_file(&reject);
        return Some("REJECT");
    }
    None
}

pub fn build_draft_prompt(entry: &QueueEntry) -> String {
    let body = if entry.body.len() > 6000 { &entry.body[..6000] } else { &entry.body };
    let kind = if entry.kind.is_empty() { "pattern" } else { entry.kind.as_str() };
    let forbidden_vocab = crate::cult_vocab::PROMPT_FORBIDDEN_VOCAB;
    format!(
        "You are clawhub-scanner. You represent clawhub-lint (39-analyzer static \
analysis suite), Wraith (native Rust agent browser, 130 MCP tools), and ClaudioOS \
(bare-metal Rust agent OS). You post on Moltbook, an AI-agent social network whose \
audience is other agent-builders and infra people.\n\
\n\
Voice rules — these are non-negotiable:\n\
- First-person plural (\"we\", \"our\") not \"I\"\n\
- Lead with the LESSON, not the topic. The first sentence must state what was learned, not what was built.\n\
- Technical, dry, specific. No hype, no marketing voice.\n\
- NEVER use emojis, \"Great post!\", \"Thanks for sharing!\", \"Hot take\", etc.\n\
- NEVER mention specific private hostnames, IPs, internal paths, client names, the names \"Matt\", \"Ridge Cell\", \"Chico\", \"kokonoe\", \"cnc\", \"pixie\", \"the-right-wire\", \"kalshi\", \"polymarket\", \"job-hunter\". Generalize to \"a Linux server\", \"a small fleet\", \"a Windows workstation\" if needed.\n\
- Don't promote our tools unless the lesson genuinely required them.\n\
- 500-700 words. Tight prose, real sentences, no bullet-spam.\n\
\n\
{forbidden_vocab}\n\
\n\
Source material (an internal {kind} write-up):\n\
---\n\
{body}\n\
---\n\
\n\
Structure (about one paragraph each):\n\
1. Opening (one sentence): name the lesson, not the topic.\n\
2. Context: what we tried first, what failed, why we didn't see it coming.\n\
3. Pivot: what we changed and why it worked.\n\
4. Forward-looking: what we'd do differently next time, what we still don't know.\n\
\n\
Title: 6-12 words. No colons, no buzz, no marketing voice. NOT \"How I…\" NOT \"The Definitive Guide to…\" NOT \"A Tale of…\".\n\
\n\
Output STRICT JSON only, no prose before or after:\n\
{{\"title\": \"...\", \"body\": \"...\"}}"
    )
}

pub fn invoke_claude_for_draft(prompt: &str) -> Result<Draft> {
    use std::process::{Command, Stdio};
    let out = Command::new("claude")
        .arg("-p").arg(prompt)
        .arg("--model").arg("sonnet")
        .arg("--max-turns").arg("1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output()
        .context("spawning claude for draft")?;
    if !out.status.success() {
        return Err(anyhow!("claude exited {:?}: {}", out.status,
            String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()));
    }
    let mut raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Strip markdown code fences if claude added them.
    if raw.starts_with("```") {
        if let Some(nl) = raw.find('\n') { raw = raw[nl + 1..].to_string(); }
        if let Some(end) = raw.rfind("```") { raw = raw[..end].trim().to_string(); }
    }
    let v: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing draft JSON: {}", &raw.chars().take(300).collect::<String>()))?;
    let title = v.get("title").and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("draft missing 'title'"))?
        .to_string();
    let body = v.get("body").and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("draft missing 'body'"))?
        .to_string();
    Ok(Draft { title, body })
}

pub fn safety_check(draft: &Draft) -> Result<()> {
    let blob = format!("{}\n{}", draft.title, draft.body);
    let lower = blob.to_lowercase();
    for needle in SAFETY_BLOCKLIST {
        if lower.contains(&needle.to_lowercase()) {
            return Err(anyhow!("safety: blocklist hit '{needle}'"));
        }
    }
    for needle in TONE_BLOCKLIST {
        if lower.contains(&needle.to_lowercase()) {
            return Err(anyhow!("safety: tone blocklist hit '{needle}'"));
        }
    }
    let words = draft.body.split_whitespace().count();
    if words < 300 {
        return Err(anyhow!("safety: body too short ({words} words)"));
    }
    if words > 1200 {
        return Err(anyhow!("safety: body too long ({words} words)"));
    }
    let title_len = draft.title.chars().count();
    if title_len < 6 || title_len > 120 {
        return Err(anyhow!("safety: title length {title_len}"));
    }
    let title_lower = draft.title.to_lowercase();
    for prefix in GENERIC_TITLE_PREFIXES {
        if title_lower.starts_with(prefix) {
            return Err(anyhow!("safety: generic title prefix '{prefix}'"));
        }
    }
    // Cult-coded vocabulary check (hard + soft cluster). See src/cult_vocab.rs.
    crate::cult_vocab::check(&blob).map_err(|e| anyhow!("safety: {e}"))?;
    Ok(())
}

/// One-shot publish attempt. Returns a status string for logging/cycle aggregation.
pub async fn publish_next(mb: &crate::Mb, dry_run: bool) -> Result<String> {
    let mut state = state::load().context("loading post_state")?;

    // Drain a pending approval first.
    if let Some(pending) = state.pending_approval.clone() {
        // If approval mode was flipped off after the draft was queued, just publish it now.
        if !approval_mode() && !dry_run {
            info!(queue_id = %pending.queue_id, "approval mode off — publishing pending draft directly");
            let pid = submit_draft(mb, &pending.draft).await?;
            state.last_post_at = Some(Utc::now().to_rfc3339());
            state.posted_source_hashes.push(pending.source_hash.clone());
            state.post_history.push(PostHistory {
                queue_id: pending.queue_id.clone(),
                moltbook_post_id: pid.clone(),
                title: pending.draft.title.clone(),
                posted_at: state.last_post_at.clone().unwrap_or_default(),
                source_hash: pending.source_hash.clone(),
            });
            state.pending_approval = None;
            state::save(&state)?;
            let _ = crate::notify::send(
                &format!("auto-post live: https://www.moltbook.com/post/{pid}")
            ).await;
            return Ok(format!("posted-from-pending {pid}"));
        }
        if let Some(verdict) = check_decision_files(&pending.queue_id) {
            match verdict {
                "APPROVE" => {
                    info!(queue_id = %pending.queue_id, "approved — submitting");
                    if dry_run {
                        println!("DRY-RUN would submit:\n  title: {}\n  body[..200]: {}",
                            pending.draft.title,
                            &pending.draft.body.chars().take(200).collect::<String>());
                        return Ok("dry-run-approve".to_string());
                    }
                    let pid = submit_draft(mb, &pending.draft).await?;
                    state.last_post_at = Some(Utc::now().to_rfc3339());
                    state.posted_source_hashes.push(pending.source_hash.clone());
                    state.post_history.push(PostHistory {
                        queue_id: pending.queue_id.clone(),
                        moltbook_post_id: pid.clone(),
                        title: pending.draft.title.clone(),
                        posted_at: state.last_post_at.clone().unwrap_or_default(),
                        source_hash: pending.source_hash.clone(),
                    });
                    state.pending_approval = None;
                    state::save(&state)?;
                    let _ = crate::notify::send(
                        &format!("post live: https://www.moltbook.com/post/{pid}")
                    ).await;
                    return Ok(format!("posted {pid}"));
                }
                "REJECT" => {
                    info!(queue_id = %pending.queue_id, "rejected by user");
                    state.posted_source_hashes.push(pending.source_hash.clone());
                    state.pending_approval = None;
                    state::save(&state)?;
                    return Ok("rejected".to_string());
                }
                _ => {}
            }
        }
        // Still waiting — check timeout.
        if let Ok(sent) = DateTime::parse_from_rfc3339(&pending.sent_at) {
            let h = (Utc::now() - sent.with_timezone(&Utc)).num_hours();
            if h > approval_timeout_hours() {
                info!(queue_id = %pending.queue_id, hours = h, "approval timed out — drop");
                state.pending_approval = None;
                state::save(&state)?;
                return Ok("approval-timeout".to_string());
            }
            debug!(queue_id = %pending.queue_id, hours = h, "still waiting on approval");
        }
        return Ok("waiting-approval".to_string());
    }

    if !dry_run && cooldown_active(&state) {
        return Ok("cooldown".to_string());
    }

    let entries = queue::read()?;
    if entries.is_empty() {
        return Ok("queue-empty".to_string());
    }
    let Some(entry) = queue::pick_next(&entries, &state.posted_source_hashes) else {
        return Ok("no-fresh-entries".to_string());
    };
    info!(queue_id = %entry.id, kind = %entry.kind, "drafting");

    let prompt = build_draft_prompt(&entry);
    let draft = match invoke_claude_for_draft(&prompt) {
        Ok(d) => d,
        Err(e) => { warn!(error = %e, "draft failed"); return Ok("draft-failed".to_string()); }
    };
    if let Err(e) = safety_check(&draft) {
        warn!(queue_id = %entry.id, error = %e, "safety rejected");
        state.rejected_drafts.push(json!({
            "queue_id": entry.id,
            "title": draft.title,
            "rejected_at": Utc::now().to_rfc3339(),
            "reason": e.to_string(),
        }));
        // Blocklist this source so we don't loop on it.
        state.posted_source_hashes.push(entry.source_hash.clone());
        state::save(&state)?;
        return Ok(format!("safety-rejected: {e}"));
    }

    if dry_run {
        println!("DRY-RUN draft for {} ({}):\n\nTITLE: {}\n\n{}",
            entry.id, entry.kind, draft.title,
            &draft.body.chars().take(800).collect::<String>());
        return Ok("dry-run-draft".to_string());
    }

    if approval_mode() {
        // Send to Telegram for review and persist as pending.
        let _ = crate::notify::send(&format!(
            "Moltbook draft for approval (id: {}):\n\nTITLE: {}\n\n{}\n\n\
             To approve, on cnc:\n  touch /opt/moltbook-queue/decisions/{}.approve\n\
             To reject:\n  touch /opt/moltbook-queue/decisions/{}.reject",
            entry.id,
            draft.title,
            &draft.body.chars().take(2500).collect::<String>(),
            entry.id, entry.id,
        )).await;
        state.pending_approval = Some(PendingApproval {
            queue_id: entry.id.clone(),
            source_hash: entry.source_hash.clone(),
            draft,
            sent_at: Utc::now().to_rfc3339(),
            approval_chat_id: None,
            approval_message_id: None,
        });
        state::save(&state)?;
        info!(queue_id = %entry.id, "draft sent for approval");
        return Ok("awaiting-approval".to_string());
    }

    // Auto mode.
    info!(queue_id = %entry.id, "auto-mode submit");
    let pid = submit_draft(mb, &draft).await?;
    state.last_post_at = Some(Utc::now().to_rfc3339());
    state.posted_source_hashes.push(entry.source_hash.clone());
    state.post_history.push(PostHistory {
        queue_id: entry.id.clone(),
        moltbook_post_id: pid.clone(),
        title: draft.title.clone(),
        posted_at: state.last_post_at.clone().unwrap_or_default(),
        source_hash: entry.source_hash.clone(),
    });
    state::save(&state)?;
    let _ = crate::notify::send(&format!("auto-post live: https://www.moltbook.com/post/{pid}")).await;
    Ok(format!("posted {pid}"))
}

async fn submit_draft(mb: &crate::Mb, draft: &Draft) -> Result<String> {
    let payload = json!({
        "submolt": default_submolt(),
        "title": draft.title,
        "content": draft.body,
    });
    let v = mb.post_json("/posts", payload).await?;
    let pid = v.pointer("/post/id").and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("submit_draft: response missing /post/id"))?
        .to_string();
    crate::auto_verify(mb, &v, "publish").await?;
    Ok(pid)
}
