//! Telegram notification sender. Reads TELEGRAM_BOT_TOKEN + TELEGRAM_CHAT_ID from
//! env. Falls back to /j/baremetal claude/.claude/.env if not in env (Windows-side).
//! Returns Ok(()) silently if no credentials — telegram is best-effort, not required.

use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

fn load_creds() -> (Option<String>, Option<String>) {
    let token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    let chat = std::env::var("TELEGRAM_CHAT_ID").ok();
    if token.is_some() && chat.is_some() {
        return (token, chat);
    }
    // Windows-side fallback path matching the python heartbeat's behavior.
    let path = std::env::var("MOLTBOOK_TELEGRAM_ENV_PATH")
        .unwrap_or_else(|_| "/j/baremetal claude/.claude/.env".to_string());
    let Ok(txt) = std::fs::read_to_string(&path) else { return (token, chat); };
    let mut t = token;
    let mut c = chat;
    for line in txt.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let Some((k, v)) = line.split_once('=') else { continue; };
        let k = k.trim();
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        match k {
            "TELEGRAM_BOT_TOKEN" if t.is_none() => t = Some(v),
            "TELEGRAM_CHAT_ID" if c.is_none() => c = Some(v),
            _ => {}
        }
    }
    (t, c)
}

pub async fn send(message: &str) -> Result<()> {
    let (Some(token), Some(chat_id)) = load_creds() else {
        tracing::debug!("telegram: no creds, skipping");
        return Ok(());
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let resp = client.post(&url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": format!("[Moltbook] {message}"),
        }))
        .send().await;
    match resp {
        Ok(r) if r.status().is_success() => tracing::debug!("telegram sent"),
        Ok(r) => tracing::warn!(status = %r.status(), "telegram send rejected"),
        Err(e) => tracing::warn!(error = %e, "telegram send failed"),
    }
    Ok(())
}
