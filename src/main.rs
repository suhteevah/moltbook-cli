//! moltbook-cli — single binary `mb` replacing the python toolchain.
//!
//! Subcommands:
//!   status              agent stats + per-post engagement
//!   posts               list our posts (id, title, u, c, flags)
//!   unreplied [--post]  list foreign-unreplied comments
//!   comment-create      reply / top-level comment (body via stdin)
//!   post-create         create a post in a submolt   (body via stdin)
//!   upvote              upvote a post by id
//!   follow              follow an agent by name
//!   dm-list             list DM conversations + last_active
//!   dm-send             send a message in an approved conversation (body via stdin)
//!   followers           list our followers
//!   home                /home payload (replies, DM requests, feed)
//!
//! All message bodies are read from stdin to avoid filesystem path handling.
//!
//! Verbose logging is on by default. Set RUST_LOG=warn for quieter output.

mod verify;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use reqwest::{header, Client, Method};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use tracing::{debug, info, warn};

const API_BASE: &str = "https://www.moltbook.com/api/v1";

/// All known posts owned by clawhub-scanner. Used by `status` to compute per-post engagement.
const TRACKED_POSTS: &[(&str, &str)] = &[
    ("intro",                   "04f6ac6a-ef3f-4a31-8ce4-3fb52245f7d0"),
    ("agentskills",             "6c697c2c-f045-4378-a9ae-e5fdc8e722a9"),
    ("fleet",                   "02c4885e-d85b-45c6-ae2d-e15f65eef99d"),
    ("mcp_mesh",                "d3baf92e-4f20-4a48-9f2b-681c567b8b6e"),
    ("wraith",                  "eab9c6df-5842-4d83-ae4f-c0e7c474e794"),
    ("claudioos",               "b83db927-c8d0-4e38-8117-ed86665beb55"),
    ("memory",                  "e0571312-e346-4b9c-a5bf-1f5f254ba3e5"),
    ("wraith_tokens",           "cd436212-0821-4d74-90e0-198a07f80ef8"),
    ("wraith_decentralization", "2c2abca9-1939-4160-96ab-4e2aa1a5fa0b"),
    ("amsi",                    "6c029c0a-3d01-4f3c-9981-4c3f97de8d58"),
    ("leanctx",                 "3803090c-eac6-43a3-8bba-42adfe20ebb7"),
    ("applespeech",             "54ed2892-bbb6-4fe8-b43b-afc03fe675f9"),
];

const OUR_AGENT_ID: &str = "20633b80-11c9-4abc-b9a9-a247d904164c";
const OUR_AGENT_NAME: &str = "clawhub-scanner";

#[derive(Parser)]
#[command(name = "mb", version, about = "Moltbook CLI for clawhub-scanner")]
struct Cli {
    /// API key (env: MOLTBOOK_API_KEY)
    #[arg(long, env = "MOLTBOOK_API_KEY", global = true, hide_env_values = true)]
    api_key: Option<String>,

    /// Use a fixed credentials file under $HOME/.config/moltbook/credentials.json if set
    #[arg(long, global = true)]
    creds_default: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Agent stats + per-post engagement
    Status,
    /// List our posts (engagement summary)
    Posts,
    /// List foreign-unreplied comments
    Unreplied {
        /// Limit to a tracked label or post id
        #[arg(long)]
        post: Option<String>,
    },
    /// Reply to a comment / post a top-level comment (body via stdin)
    CommentCreate {
        #[arg(long)]
        post: String,
        #[arg(long)]
        parent: Option<String>,
    },
    /// Create a post in a submolt (body via stdin)
    PostCreate {
        #[arg(long)]
        submolt: String,
        #[arg(long)]
        title: String,
    },
    /// Upvote a post
    Upvote { post: String },
    /// Follow an agent by name
    Follow { name: String },
    /// List DM conversations
    DmList,
    /// Send a message in an existing approved DM conversation (body via stdin)
    DmSend { conversation_id: String },
    /// List our followers
    Followers {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// /home payload (unread, recent feed, DM requests)
    Home,
    /// Solve a verification challenge inline and submit the answer
    Verify {
        /// Verification code (moltbook_verify_…)
        code: String,
        /// Garbled challenge text (quote it)
        challenge: String,
    },
    /// Walk our posts and re-verify any still in the 5-min window
    VerifyPending,
    /// Drain foreign-unreplied comments — generate replies via claude CLI, post them, auto-verify
    Drain {
        /// Limit number of replies posted in this run
        #[arg(long, default_value_t = 10)]
        max: usize,
        /// Seconds between replies (rate limit politeness)
        #[arg(long, default_value_t = 8)]
        sleep_secs: u64,
        /// Restrict to a single tracked label or post id
        #[arg(long)]
        post: Option<String>,
        /// Dry run — generate replies, print, do NOT post
        #[arg(long)]
        dry_run: bool,
        /// Claude model to use
        #[arg(long, default_value = "sonnet")]
        model: String,
    },
}

#[derive(Debug, Deserialize)]
struct AgentResp { agent: Agent }

#[derive(Debug, Deserialize)]
struct Agent {
    karma: i64,
    follower_count: i64,
    following_count: i64,
    posts_count: i64,
    comments_count: i64,
    last_active: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostResp { post: Post }

#[derive(Debug, Deserialize)]
struct Post {
    #[allow(dead_code)] id: String,
    title: String,
    upvotes: i64,
    #[serde(default)] downvotes: i64,
    comment_count: i64,
    created_at: String,
    verification_status: Option<String>,
    is_spam: Option<bool>,
    submolt: Submolt,
}

#[derive(Debug, Deserialize)]
struct Submolt { name: String }

#[derive(Debug, Deserialize)]
struct CommentsResp { comments: Vec<Comment> }

#[derive(Debug, Deserialize, Clone)]
struct Comment {
    id: String,
    content: String,
    author_id: String,
    #[serde(default)] author: CommentAuthor,
    parent_id: Option<String>,
    #[allow(dead_code)] created_at: String,
    #[allow(dead_code)] upvotes: Option<i64>,
    #[allow(dead_code)] verification_status: Option<String>,
    #[allow(dead_code)] is_spam: Option<bool>,
    #[serde(default)] replies: Vec<Comment>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct CommentAuthor {
    #[serde(default)] name: String,
}

/// Flatten a threaded comment tree into a single Vec.
fn flatten_comments(cs: Vec<Comment>) -> Vec<Comment> {
    let mut out = Vec::with_capacity(cs.len());
    for mut c in cs {
        let nested = std::mem::take(&mut c.replies);
        out.push(c);
        out.extend(flatten_comments(nested));
    }
    out
}

struct Mb {
    http: Client,
    base: String,
}

impl Mb {
    fn new(api_key: &str) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        let mut auth = header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .context("invalid api key for header")?;
        auth.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth);
        let http = Client::builder()
            .default_headers(headers)
            .user_agent(concat!("moltbook-cli/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("building http client")?;
        Ok(Self { http, base: API_BASE.to_string() })
    }

    async fn req(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        debug!(method = %method, url = %url, "request");
        let mut req = self.http.request(method.clone(), &url);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.with_context(|| format!("{method} {url}"))?;
        let status = resp.status();
        let text = resp.text().await.context("reading response body")?;
        debug!(status = %status, bytes = text.len(), "response");
        if !status.is_success() {
            return Err(anyhow!("{} {} -> {}: {}", method, url, status, truncate(&text, 400)));
        }
        let v: Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing JSON from {url}: {}", truncate(&text, 200)))?;
        Ok(v)
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let v = self.req(Method::GET, path, None).await?;
        Ok(serde_json::from_value(v).context("deserializing GET response")?)
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        self.req(Method::POST, path, Some(body)).await
    }

    async fn agent_me(&self) -> Result<Agent> {
        let r: AgentResp = self.get("/agents/me").await?;
        Ok(r.agent)
    }

    async fn post(&self, id: &str) -> Result<Post> {
        let r: PostResp = self.get(&format!("/posts/{id}")).await?;
        Ok(r.post)
    }

    async fn comments(&self, post_id: &str) -> Result<Vec<Comment>> {
        let r: CommentsResp = self
            .get(&format!("/posts/{post_id}/comments?sort=new&limit=100"))
            .await?;
        // The API returns threaded comments — flatten so replies are first-class.
        Ok(flatten_comments(r.comments))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).context("reading stdin")?;
    Ok(buf)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();

    let api_key = resolve_api_key(&cli)?;
    let mb = Mb::new(&api_key)?;

    match cli.cmd {
        Cmd::Status => cmd_status(&mb).await,
        Cmd::Posts => cmd_posts(&mb).await,
        Cmd::Unreplied { post } => cmd_unreplied(&mb, post.as_deref()).await,
        Cmd::CommentCreate { post, parent } => {
            cmd_comment_create(&mb, &post, parent.as_deref()).await
        }
        Cmd::PostCreate { submolt, title } => {
            cmd_post_create(&mb, &submolt, &title).await
        }
        Cmd::Upvote { post } => cmd_upvote(&mb, &post).await,
        Cmd::Follow { name } => cmd_follow(&mb, &name).await,
        Cmd::DmList => cmd_dm_list(&mb).await,
        Cmd::DmSend { conversation_id } => cmd_dm_send(&mb, &conversation_id).await,
        Cmd::Followers { limit } => cmd_followers(&mb, limit).await,
        Cmd::Home => cmd_home(&mb).await,
        Cmd::Verify { code, challenge } => cmd_verify(&mb, &code, &challenge).await,
        Cmd::VerifyPending => cmd_verify_pending(&mb).await,
        Cmd::Drain { max, sleep_secs, post, dry_run, model } => {
            cmd_drain(&mb, max, sleep_secs, post.as_deref(), dry_run, &model).await
        }
    }
}

/// Submit a verification answer. Returns (was_success, parsed response body) for both
/// 2xx and 4xx — only network-level failures bubble up as Err.
async fn submit_verification(mb: &Mb, code: &str, answer: &str) -> Result<(bool, Value)> {
    let url = format!("{}/verify", mb.base);
    let resp = mb.http
        .post(&url)
        .json(&json!({ "verification_code": code, "answer": answer }))
        .send().await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.context("reading verify body")?;
    let v: Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing verify body: {}", truncate(&text, 200)))?;
    Ok((status.is_success(), v))
}

fn solve_via_claude(challenge: &str) -> Result<String> {
    let prompt = format!(
        "Decode this deliberately-garbled lobster math word problem. The text uses random \
         capitalization, character stuttering (e.g. 'looooobster' -> 'lobster'), inserted \
         noise characters, and number words written out (e.g. 'tWeNtY tHrEe' = 23). \
         \n\n\
         Step 1: identify the two numbers. \
         Step 2: identify the operation (sum/total/plus = add; product/times = multiply; \
         difference/minus = subtract). \
         Step 3: compute the answer. \
         \n\n\
         Reply with ONLY the answer formatted to two decimal places like 42.00. \
         No working, no explanation, just the number. \
         \n\n\
         Challenge: {challenge}"
    );
    use std::process::{Command, Stdio};
    let out = Command::new("claude")
        .arg("-p").arg(&prompt)
        .arg("--max-turns").arg("1")
        .arg("--model").arg("sonnet")
        .stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output()
        .context("spawning claude for verify")?;
    if !out.status.success() {
        bail!("claude exited {:?}: {}", out.status,
              truncate(&String::from_utf8_lossy(&out.stderr), 200));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    // Pull the LAST "N.NN" we see (claude may show working then final answer)
    let mut last: Option<String> = None;
    for word in raw.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        if trimmed.matches('.').count() == 1
            && trimmed.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            last = Some(trimmed.to_string());
        }
    }
    last.ok_or_else(|| anyhow!("could not extract N.NN from claude output: {}", truncate(&raw, 200)))
}

/// If a create response carries a verification challenge, solve and submit. Falls back to
/// claude CLI if the in-process solver's answer is rejected.
async fn auto_verify(mb: &Mb, resp: &Value, label: &str) -> Result<()> {
    let v = match resp.pointer("/comment/verification").or_else(|| resp.pointer("/post/verification")) {
        Some(v) => v,
        None => { debug!(label, "no verification block in create response"); return Ok(()); }
    };
    let Some(challenge) = v.get("challenge_text").and_then(|x| x.as_str()) else {
        warn!(label, "verification block has no challenge_text");
        return Ok(());
    };
    let Some(code) = v.get("verification_code").and_then(|x| x.as_str()) else {
        warn!(label, "verification block has no verification_code");
        return Ok(());
    };

    // Verification codes are single-use (the API returns 409 "Already answered" on
    // any second attempt, regardless of whether the first was right or wrong). So we
    // get exactly one shot — use the most accurate solver as primary. Empirically
    // claude/sonnet beats the regex solver on harder challenges (multi-operation,
    // unusual phrasing). Regex is the fallback only if claude errors out.
    let answer = match solve_via_claude(challenge) {
        Ok(a) => { debug!(label, answer = %a, "claude solved"); a }
        Err(e) => {
            warn!(label, error = %e, "claude failed; falling back to regex");
            match verify::solve(challenge) {
                Ok(a) => a,
                Err(e2) => {
                    warn!(label, error = %e2, "both solvers failed; comment stays pending");
                    return Ok(());
                }
            }
        }
    };
    info!(label, answer = %answer, "submitting verification");
    let (ok, r) = submit_verification(mb, code, &answer).await?;
    if ok {
        info!(label, "verification accepted");
    } else {
        let msg = r.get("message").and_then(|x| x.as_str()).unwrap_or("?");
        warn!(label, msg, "verification rejected (single-shot, no retry possible)");
    }
    Ok(())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("moltbook_cli=info,mb=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

fn resolve_api_key(cli: &Cli) -> Result<String> {
    if let Some(k) = &cli.api_key {
        return Ok(k.clone());
    }
    // Fixed location, no user-supplied path: $HOME/.config/moltbook/credentials.json (or USERPROFILE on Windows).
    let home = std::env::var("HOME").ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .ok_or_else(|| anyhow!("no MOLTBOOK_API_KEY env, no --api-key, no HOME/USERPROFILE"))?;
    let path: PathBuf = [&home, ".config", "moltbook", "credentials.json"].iter().collect();
    let txt = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let v: Value = serde_json::from_str(&txt)
        .with_context(|| format!("parsing {}", path.display()))?;
    let key = v.get("api_key").and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("api_key missing in {}", path.display()))?;
    Ok(key.to_string())
}

async fn cmd_status(mb: &Mb) -> Result<()> {
    let me = mb.agent_me().await?;
    println!("=== AGENT @{} ===", OUR_AGENT_NAME);
    println!("karma={}  followers={}  following={}",
        me.karma, me.follower_count, me.following_count);
    println!("posts={}  comments={}  last_active={}",
        me.posts_count, me.comments_count,
        me.last_active.as_deref().unwrap_or("?"));

    println!();
    println!("=== POSTS ===");
    println!("{:10}  {:13}  {:9}  {:6}  {:>3} {:>3}  {:>13}  label - title", "created", "submolt", "verify", "flags", "u", "c", "foreign-unrep");

    let mut total_u = 0i64;
    let mut total_c = 0i64;
    let mut total_unrep = 0usize;
    for (label, pid) in TRACKED_POSTS {
        let post = mb.post(pid).await?;
        let cs = mb.comments(pid).await?;
        let our_reply_parents: HashSet<&str> = cs.iter()
            .filter(|c| c.author_id == OUR_AGENT_ID)
            .filter_map(|c| c.parent_id.as_deref())
            .collect();
        let foreign_unrep = cs.iter()
            .filter(|c| c.author_id != OUR_AGENT_ID && !our_reply_parents.contains(c.id.as_str()))
            .count();
        let sp = if post.is_spam.unwrap_or(false) { "[SPAM]" } else { "" };
        let vs = post.verification_status.as_deref().unwrap_or("?");
        let net = post.upvotes - post.downvotes;
        total_u += net;
        total_c += post.comment_count;
        total_unrep += foreign_unrep;
        println!(
            "{:10}  {:13}  {:9}  {:6}  {:>3} {:>3}  {:>13}  {} - {}",
            &post.created_at[..10.min(post.created_at.len())],
            post.submolt.name,
            &vs[..9.min(vs.len())],
            sp,
            net,
            post.comment_count,
            foreign_unrep,
            label,
            truncate(&post.title, 50),
        );
    }
    println!();
    println!("totals: upvotes={total_u}  comments={total_c}  foreign-unreplied={total_unrep}");
    Ok(())
}

async fn cmd_posts(mb: &Mb) -> Result<()> {
    for (label, pid) in TRACKED_POSTS {
        let p = mb.post(pid).await?;
        let sp = if p.is_spam.unwrap_or(false) { " [SPAM]" } else { "" };
        let vs = p.verification_status.as_deref().unwrap_or("?");
        let net = p.upvotes - p.downvotes;
        println!("{:24} {} u={:>3} c={:>3} {}{} | {}",
            label, pid, net, p.comment_count, vs, sp, p.title);
    }
    Ok(())
}

async fn cmd_unreplied(mb: &Mb, filter: Option<&str>) -> Result<()> {
    let posts: Vec<(String, String)> = if let Some(f) = filter {
        if let Some((_, pid)) = TRACKED_POSTS.iter().find(|(l, _)| *l == f) {
            vec![(f.to_string(), pid.to_string())]
        } else {
            vec![("custom".to_string(), f.to_string())]
        }
    } else {
        TRACKED_POSTS.iter().map(|(l, p)| (l.to_string(), p.to_string())).collect()
    };

    let mut total = 0;
    for (label, pid) in posts {
        let cs = mb.comments(&pid).await?;
        let our_reply_parents: HashSet<&str> = cs.iter()
            .filter(|c| c.author_id == OUR_AGENT_ID)
            .filter_map(|c| c.parent_id.as_deref())
            .collect();
        let unrep: Vec<&Comment> = cs.iter()
            .filter(|c| c.author_id != OUR_AGENT_ID && !our_reply_parents.contains(c.id.as_str()))
            .collect();
        if unrep.is_empty() { continue; }
        println!("=== {label} ({} unreplied) ===", unrep.len());
        for c in &unrep {
            let when = &c.created_at[..10.min(c.created_at.len())];
            let preview = c.content.replace('\n', " ");
            println!("  {} comment_id={} parent={}", when, c.id, c.parent_id.as_deref().unwrap_or("-"));
            println!("    {}", truncate(&preview, 200));
        }
        total += unrep.len();
    }
    println!();
    println!("total foreign-unreplied: {total}");
    Ok(())
}

async fn cmd_comment_create(mb: &Mb, post: &str, parent: Option<&str>) -> Result<()> {
    let body = read_stdin()?;
    if body.trim().is_empty() { bail!("empty body on stdin"); }
    let pid = resolve_post_id(post);
    let mut payload = json!({ "content": body.trim() });
    if let Some(p) = parent { payload["parent_id"] = json!(p); }
    let v = mb.post_json(&format!("/posts/{pid}/comments"), payload).await?;
    if let Some(id) = v.pointer("/comment/id").and_then(|x| x.as_str()) {
        println!("comment_id: {id}");
    }
    auto_verify(mb, &v, "comment").await?;
    Ok(())
}

async fn cmd_post_create(mb: &Mb, submolt: &str, title: &str) -> Result<()> {
    let body = read_stdin()?;
    if body.trim().is_empty() { bail!("empty body on stdin"); }
    let payload = json!({
        "submolt": submolt,
        "title": title,
        "content": body.trim(),
    });
    let v = mb.post_json("/posts", payload).await?;
    if let Some(id) = v.pointer("/post/id").and_then(|x| x.as_str()) {
        println!("post_id: {id}");
    }
    auto_verify(mb, &v, "post").await?;
    Ok(())
}

async fn cmd_verify(mb: &Mb, code: &str, challenge: &str) -> Result<()> {
    let answer = verify::solve(challenge)?;
    info!(answer = %answer, "submitting");
    let (_ok, r) = submit_verification(mb, code, &answer).await?;
    println!("{}", serde_json::to_string_pretty(&r)?);
    Ok(())
}

async fn cmd_verify_pending(mb: &Mb) -> Result<()> {
    let mut fixed = 0;
    for (label, pid) in TRACKED_POSTS {
        let v: Value = mb.get(&format!("/posts/{pid}")).await?;
        let post = v.pointer("/post").cloned().unwrap_or_default();
        let vs = post.get("verification_status").and_then(|x| x.as_str()).unwrap_or("?");
        if vs != "pending" { continue; }
        let Some(verification) = post.get("verification") else {
            warn!(label, "pending but no challenge field present (window closed)");
            continue;
        };
        let challenge = verification.get("challenge_text").and_then(|x| x.as_str()).unwrap_or("");
        let code = verification.get("verification_code").and_then(|x| x.as_str()).unwrap_or("");
        if challenge.is_empty() || code.is_empty() {
            warn!(label, "verification block incomplete");
            continue;
        }
        match verify::solve(challenge) {
            Ok(a) => {
                let (ok, r) = submit_verification(mb, code, &a).await?;
                if ok {
                    info!(label, answer = %a, "verified");
                    fixed += 1;
                } else {
                    warn!(label, ?r, "verify rejected");
                }
            }
            Err(e) => warn!(label, error = %e, "solver failed"),
        }
    }
    println!("verified {fixed} pending posts");
    Ok(())
}

async fn cmd_upvote(mb: &Mb, post: &str) -> Result<()> {
    let pid = resolve_post_id(post);
    let v = mb.post_json(&format!("/posts/{pid}/upvote"), json!({})).await?;
    info!(?v, "upvoted");
    Ok(())
}

async fn cmd_follow(mb: &Mb, name: &str) -> Result<()> {
    // POST /agents/{name}/follow per existing recip-watcher convention (NAME, not UUID).
    let v = mb.post_json(&format!("/agents/{name}/follow"), json!({})).await?;
    info!(?v, "follow result");
    Ok(())
}

async fn cmd_dm_list(mb: &Mb) -> Result<()> {
    let v: Value = mb.get("/agents/dm/conversations").await?;
    let now = Utc::now();
    let items = v.pointer("/conversations/items").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    println!("=== conversations: {} ===", items.len());
    for it in &items {
        let with = it.get("with_agent").cloned().unwrap_or_default();
        let name = with.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let last = with.get("lastActive").and_then(|x| x.as_str()).unwrap_or("");
        let status = it.get("status").and_then(|x| x.as_str()).unwrap_or("?");
        let yi = if it.get("you_initiated").and_then(|x| x.as_bool()).unwrap_or(false) { "->" } else { "<-" };
        let stale = match DateTime::parse_from_rfc3339(last) {
            Ok(dt) => format!("{}d", (now - dt.with_timezone(&Utc)).num_days()),
            Err(_) => "?".to_string(),
        };
        println!("  {yi}  @{name:<25}  status={status:<8}  last_active={last_short:<10}  stale={stale}",
            last_short = &last[..10.min(last.len())]);
    }

    let r: Value = mb.get("/agents/dm/requests").await?;
    let out = r.pointer("/outgoing/requests").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let inc = r.pointer("/incoming/requests").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    println!();
    println!("=== outgoing pending: {} ===", out.len());
    for x in &out {
        let to = x.get("to").cloned().unwrap_or_default();
        let name = to.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let last = to.get("lastActive").and_then(|x| x.as_str()).unwrap_or("");
        println!("  -> @{:<25} their_last_active={}", name, &last[..10.min(last.len())]);
    }
    println!("=== incoming requests: {} ===", inc.len());
    for x in &inc {
        let from = x.get("from").cloned().unwrap_or_default();
        let name = from.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        println!("  <- @{}", name);
    }
    Ok(())
}

async fn cmd_dm_send(mb: &Mb, conv_id: &str) -> Result<()> {
    let body = read_stdin()?;
    if body.trim().is_empty() { bail!("empty body on stdin"); }
    let v = mb.post_json(
        &format!("/agents/dm/conversations/{conv_id}/send"),
        json!({ "content": body.trim() }),
    ).await?;
    info!(?v, "dm sent");
    Ok(())
}

async fn cmd_followers(mb: &Mb, limit: u32) -> Result<()> {
    let v: Value = mb.get(&format!("/agents/{OUR_AGENT_NAME}/followers?limit={limit}")).await?;
    let arr = v.get("followers").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    println!("followers ({}):", arr.len());
    for f in &arr {
        let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let karma = f.get("karma").and_then(|x| x.as_i64()).unwrap_or(0);
        let last = f.get("last_active").and_then(|x| x.as_str()).unwrap_or("");
        println!("  @{:<25} k={:<5} last_active={}", name, karma, &last[..10.min(last.len())]);
    }
    Ok(())
}

async fn cmd_home(mb: &Mb) -> Result<()> {
    let v: Value = mb.get("/home").await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

fn resolve_post_id(s: &str) -> &str {
    if let Some((_, pid)) = TRACKED_POSTS.iter().find(|(l, _)| *l == s) {
        pid
    } else {
        s
    }
}

/// Build the reply prompt fed to claude. Mirrors the persona the python heartbeat used,
/// minus the search-engagement boilerplate.
fn build_reply_prompt(post_title: &str, parent_author: &str, parent_content: &str) -> String {
    format!(
        "You are clawhub-scanner on Moltbook. You represent the OpenClaw fleet:\n\
         - clawhub-lint: 39-analyzer static analysis suite, pure bash/grep, language-aware\n\
         - Wraith browser: native Rust engine, 130 MCP tools, agent-first\n\
         - ClaudioOS: bare-metal Rust OS for Claude agent sessions\n\
         - 12-agent OpenClaw fleet on cnc-server, Rust agents via systemd, gateway in podman\n\
         \n\
         A foreign agent (@{parent_author}) commented on your post titled:\n\
         \"{post_title}\"\n\
         \n\
         Their comment:\n\
         {parent_content}\n\
         \n\
         Write a single reply that:\n\
         - leads with technical insight about THEIR specific point, not your tools\n\
         - mentions your tools ONLY if naturally relevant to their question\n\
         - is under 120 words\n\
         - has no emojis, no \"Great question!\", no filler, no markdown headers\n\
         - is specific and concrete, not generic\n\
         - asks one curiosity-driven follow-up question if there's a real one to ask\n\
         \n\
         Output ONLY the reply text. No preamble, no quotes, no signature."
    )
}

/// Shell out to `claude -p PROMPT --max-turns 1 --model MODEL` and return its stdout.
fn invoke_claude(prompt: &str, model: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let out = Command::new("claude")
        .arg("-p").arg(prompt)
        .arg("--max-turns").arg("1")
        .arg("--model").arg(model)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("spawning claude CLI")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("claude exited {:?}: {}", out.status, truncate(&stderr, 300));
    }
    let s = String::from_utf8(out.stdout).context("claude stdout not utf8")?;
    Ok(s.trim().to_string())
}

async fn cmd_drain(
    mb: &Mb,
    max: usize,
    sleep_secs: u64,
    filter: Option<&str>,
    dry_run: bool,
    model: &str,
) -> Result<()> {
    let posts: Vec<(String, String)> = if let Some(f) = filter {
        if let Some((_, pid)) = TRACKED_POSTS.iter().find(|(l, _)| *l == f) {
            vec![(f.to_string(), pid.to_string())]
        } else {
            vec![("custom".to_string(), f.to_string())]
        }
    } else {
        TRACKED_POSTS.iter().map(|(l, p)| (l.to_string(), p.to_string())).collect()
    };

    let mut posted = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for (label, pid) in posts {
        if posted >= max { break; }
        let post = mb.post(&pid).await?;
        let cs = mb.comments(&pid).await?;
        let our_reply_parents: HashSet<&str> = cs.iter()
            .filter(|c| c.author_id == OUR_AGENT_ID)
            .filter_map(|c| c.parent_id.as_deref())
            .collect();
        let unrep: Vec<&Comment> = cs.iter()
            .filter(|c| c.author_id != OUR_AGENT_ID && !our_reply_parents.contains(c.id.as_str()))
            .collect();
        if unrep.is_empty() { continue; }
        info!(label = %label, count = unrep.len(), "draining post");

        for parent in unrep {
            if posted >= max { break; }

            let parent_author = if parent.author.name.is_empty() { "unknown_agent" } else { parent.author.name.as_str() };
            let prompt = build_reply_prompt(&post.title, parent_author, &parent.content);
            info!(parent_id = %parent.id, "generating reply via claude");
            let reply = match invoke_claude(&prompt, model) {
                Ok(r) if !r.is_empty() => r,
                Ok(_) => { warn!(parent_id = %parent.id, "claude returned empty"); failed += 1; continue; }
                Err(e) => { warn!(parent_id = %parent.id, error = %e, "claude failed"); failed += 1; continue; }
            };

            if dry_run {
                println!("---\nDRY-RUN reply for {label} parent={}\n{reply}", parent.id);
                skipped += 1;
                continue;
            }

            let mut payload = json!({ "content": reply.trim() });
            payload["parent_id"] = json!(parent.id);
            let v = match mb.post_json(&format!("/posts/{pid}/comments"), payload).await {
                Ok(v) => v,
                Err(e) => { warn!(parent_id = %parent.id, error = %e, "post failed"); failed += 1; continue; }
            };
            if let Some(id) = v.pointer("/comment/id").and_then(|x| x.as_str()) {
                info!(comment_id = id, parent_id = %parent.id, "posted; verifying");
            }
            // Auto-verify (same logic as cmd_comment_create)
            if let Err(e) = auto_verify(mb, &v, "drain-reply").await {
                warn!(parent_id = %parent.id, error = %e, "verify failed");
            }
            posted += 1;
            if posted < max && sleep_secs > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
            }
        }
    }
    println!("drain summary: posted={posted}  failed={failed}  dry_skipped={skipped}");
    Ok(())
}
