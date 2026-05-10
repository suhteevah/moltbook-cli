//! DM tending — approve incoming requests, reply to unread messages via claude.
//!
//! Two passes:
//!  1. /agents/dm/requests → for each incoming, POST .../approve, then read +
//!     reply with claude-drafted opener.
//!  2. /agents/dm/conversations → walk approved convos; if last message is theirs
//!     (not ours), draft + send a reply.

use anyhow::Result;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

fn build_dm_prompt(sender: &str, content: &str) -> String {
    format!(
        "You are @clawhub-scanner, an autonomous agent on Moltbook representing the OpenClaw \
fleet (clawhub-lint static analysis, Wraith browser, ClaudioOS bare-metal Rust OS, plus 12 \
Rust agents on systemd).\n\
\n\
@{sender} just sent you this DM:\n\
\"{content}\"\n\
\n\
Write a reply that:\n\
- is 60-120 words\n\
- agent voice: \"we\"/\"the fleet\", never first-person human\n\
- references one specific thing they said\n\
- offers one concrete piece of info from your fleet (a tool, a config, a number) when relevant\n\
- ends with at most ONE follow-up question if there's a real one\n\
- no emojis, no signoffs (\"— Matt\", etc.), no \"Hey @sender\" (it's a reply)\n\
- never references the names \"Matt\", \"Matt Gates\", \"Ridge Cell\", \"kokonoe\", \"cnc-server\", \"pixie\", or any private hostname\n\
\n\
{}\n\
\n\
Output ONLY the reply text, no preamble.",
        crate::cult_vocab::PROMPT_FORBIDDEN_VOCAB
    )
}

fn invoke_claude(prompt: &str, model: &str) -> anyhow::Result<String> {
    use std::process::{Command, Stdio};
    let out = Command::new("claude")
        .arg("-p").arg(prompt)
        .arg("--max-turns").arg("1")
        .arg("--model").arg(model)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        anyhow::bail!("claude exit {:?}: {}", out.status,
            String::from_utf8_lossy(&out.stderr).chars().take(200).collect::<String>());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Process incoming DM requests + walk approved conversations to reply.
/// Returns (approved_count, replied_count).
pub async fn tend(mb: &crate::Mb, model: &str, sleep_secs: u64) -> Result<(usize, usize)> {
    let mut approved = 0;
    let mut replied = 0;

    // Pass 1: incoming requests
    let reqs: Value = mb.get_value("/agents/dm/requests").await.unwrap_or(json!({}));
    let incoming = reqs.pointer("/incoming/requests")
        .and_then(|x| x.as_array()).cloned().unwrap_or_default();
    if !incoming.is_empty() {
        info!(count = incoming.len(), "DM requests pending");
    }
    for req in &incoming {
        let Some(conv_id) = req.get("conversation_id").or_else(|| req.get("id"))
            .and_then(|x| x.as_str()) else { continue; };
        let sender = req.pointer("/from/name").and_then(|x| x.as_str()).unwrap_or("?");
        match mb.post_json(&format!("/agents/dm/requests/{conv_id}/approve"), json!({})).await {
            Ok(_) => { info!(sender, "DM request approved"); approved += 1; }
            Err(e) => { warn!(sender, conv_id, error = %e, "DM approve failed"); continue; }
        }
        // Read what they sent and draft a reply.
        let conv: Value = match mb.get_value(&format!("/agents/dm/conversations/{conv_id}")).await {
            Ok(v) => v,
            Err(e) => { warn!(sender, error = %e, "DM read after approve failed"); continue; }
        };
        let msgs = conv.get("messages").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let their_msgs: Vec<&Value> = msgs.iter()
            .filter(|m| m.get("senderAgentId").and_then(|x| x.as_str()) != Some(crate::OUR_AGENT_ID))
            .collect();
        let body = if let Some(last) = their_msgs.last() {
            let content = last.get("content").or_else(|| last.get("message"))
                .and_then(|x| x.as_str()).unwrap_or("");
            if content.trim().is_empty() {
                format!("Thanks for reaching out, @{sender}. What are you building?")
            } else {
                let prompt = build_dm_prompt(sender, content);
                match invoke_claude(&prompt, model) {
                    Ok(s) if !s.is_empty() => s,
                    _ => format!("Thanks for reaching out, @{sender}. What are you building?"),
                }
            }
        } else {
            format!("Thanks for reaching out, @{sender}. What are you building?")
        };
        if sleep_secs > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        }
        match mb.post_json(&format!("/agents/dm/conversations/{conv_id}/send"),
                           json!({"content": body})).await {
            Ok(_) => {
                info!(sender, "DM reply sent");
                let _ = crate::notify::send(&format!("DM exchange opened with @{sender}")).await;
            }
            Err(e) => warn!(sender, error = %e, "DM send failed"),
        }
    }

    // Pass 2: walk approved conversations
    let convs: Value = mb.get_value("/agents/dm/conversations").await.unwrap_or(json!({}));
    let items = convs.pointer("/conversations/items")
        .and_then(|x| x.as_array()).cloned().unwrap_or_default();
    for cv in &items {
        let Some(conv_id) = cv.get("conversation_id").or_else(|| cv.get("id"))
            .and_then(|x| x.as_str()) else { continue; };
        let status = cv.get("status").and_then(|x| x.as_str()).unwrap_or("?");
        if status == "pending" { continue; }
        let sender = cv.pointer("/with_agent/name").and_then(|x| x.as_str()).unwrap_or("?");
        let full: Value = match mb.get_value(&format!("/agents/dm/conversations/{conv_id}")).await {
            Ok(v) => v,
            Err(e) => { debug!(sender, error = %e, "DM walk fetch failed"); continue; }
        };
        let msgs = full.get("messages").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let Some(last) = msgs.last() else { continue; };
        let last_is_ours = last.get("senderAgentId").and_then(|x| x.as_str())
            == Some(crate::OUR_AGENT_ID)
            || last.get("is_mine").and_then(|x| x.as_bool()) == Some(true);
        if last_is_ours { continue; }
        let their_msg = last.get("content").or_else(|| last.get("message"))
            .and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        if their_msg.is_empty() { continue; }
        debug!(sender, msg_preview = %&their_msg[..their_msg.len().min(120)], "unread DM");
        let prompt = build_dm_prompt(sender, &their_msg);
        let reply = match invoke_claude(&prompt, model) {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => continue,
            Err(e) => { warn!(sender, error = %e, "claude DM gen failed"); continue; }
        };
        if sleep_secs > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        }
        match mb.post_json(&format!("/agents/dm/conversations/{conv_id}/send"),
                           json!({"content": reply})).await {
            Ok(_) => {
                info!(sender, "DM reply sent");
                let _ = crate::notify::send(&format!("Replied DM @{sender}")).await;
                replied += 1;
            }
            Err(e) => warn!(sender, error = %e, "DM send failed"),
        }
    }

    Ok((approved, replied))
}
