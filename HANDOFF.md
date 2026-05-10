# moltbook-cli HANDOFF

## Last Updated
2026-05-10

## Project Status
🟢 Working — autonomous Rust agent live on Moltbook, posting + replying + monitoring every 30 min via cnc systemd timer

## What Was Done This Session

### Full python-heartbeat → Rust port (the big one)
The python `moltbook-heartbeat` container was the long-running engine for our Moltbook agent. Today it was retired entirely; a single Rust binary `mb` now drives the same workflows from a 30-min systemd timer.

**Modules added** (all under `src/`):
- `verify.rs` — lobster-math challenge solver (regex). Already existed.
- `state.rs` — `post_state.json` read/write (wire-compatible with python).
- `queue.rs` — `queue.jsonl` reader, incident→pattern→project pick order.
- `publish.rs` — full publish pipeline: queue→draft (claude/sonnet)→safety check→approval-or-auto→submit→auto-verify→state update.
- `feed.rs` — `upvote-hot` (topic-keyword filter, skip own posts).
- `dms.rs` — DM request approval + auto-reply via claude.
- `notify.rs` — Telegram sender (env or `.env` fallback).
- `cult_vocab.rs` — two-tier blocklist for AI-cult vocabulary (HARD = single hit rejects, SOFT = 3+ cluster rejects). 5 unit tests passing.
- `feedwatch.rs` — passive `/feed` cult-cluster scanner. Dedup state at `~/.config/moltbook/feedwatch_state.json` (24h alert suppress).
- `reciprocity.rs` — follow-back agents who follow us (karma ≥3, not stale, not cult-coded). State at `~/.config/moltbook/reciprocity_state.json`.

**Subcommands shipped** (in addition to the existing status / posts / unreplied / drain / etc):
- `mb cycle` — full orchestrator: drain → upvote-hot → dms-tend → feed-watch (new+hot) → reciprocity → publish-next → karma snapshot
- `mb publish-next [--dry-run]` — single publish attempt
- `mb upvote-hot --max N`
- `mb dms-tend --model <m>`
- `mb notify <message>`
- `mb queue-list` — show queue + posted/pending state
- `mb feed-watch --sort {new|hot|top} --limit N --threshold N [--telegram]`
- `mb reciprocate --max N [--telegram]`

### Critical bug discoveries + fixes earlier in the session
- **Comment verification was the karma-stuck root cause.** Python heartbeat verified posts but not comments; ALL 152 prior comments stuck at `verification_status: pending` permanently because the 5-min challenge window expires. Rust auto_verify on every comment-create fixes this going forward. Drain run today posted 30 replies, 28 verified (then 5/5 after switching verify path to claude-first per the next finding).
- **Verification codes are single-shot.** API returns `409 "Already answered"` on any second `/verify` POST. So we MUST use the most accurate solver primary, not retry. Switched to claude/sonnet primary for the verify step; regex solver only as a fallback when claude errors.
- **Comment threading.** `/posts/{id}/comments` returns top-level comments with nested `replies`; flatten the tree before computing "did we reply?". Pre-flatten the bare-metal `mb` was double-counting threaded children, reporting backlog of 86 when the real number was 27.
- **Glibc mismatch.** cnc host has glibc 2.39; container image (Bookworm) has glibc 2.36. Build a separate `mb-bookworm` binary inside `rust:1-bookworm` for in-container use. Bind-mount it into the heartbeat image at `/usr/local/bin/mb` so it inherits OAuth from the `moltbook-data` volume.
- **Container OAuth.** The existing `moltbook-heartbeat:latest` image already had claude CLI v2.1.126 baked in and OAuth credentials in the `moltbook-data` volume. So our Rust binary, run with `--entrypoint /usr/local/bin/mb` inside that container, gets BOTH moltbook API creds and claude OAuth for free. No new auth flow needed.
- **Dead-comment zombies.** Comments with `is_deleted=true` were leaking into the unrep list. Filter applied in drain + status.

### Operational deliverables on cnc
- `moltbook-heartbeat.service` — stopped + disabled (do not re-enable; will leak unverified comments)
- `moltbook-cycle.timer` + `.service` — active, every 30 min, `OnUnitActiveSec=30min`
- `moltbook-drain.timer` + `.service` — deleted (interim, superseded by cycle)
- `MOLTBOOK_PUBLISH_COOLDOWN_HOURS=3` (was bumped from 24 mid-session)
- `MOLTBOOK_APPROVAL_MODE=0` (auto-publish enabled mid-session after Matt verified post quality)
- `mb` host-bare-metal binary at `/usr/local/bin/mb`
- `mb-bookworm` container-compatible binary at `/usr/local/bin/mb-bookworm`

### Live results from today
- karma 58 → 60 (+2 from natural traction)
- One auto-published verified post: `df4a73c9` "Reciting facts and composing strategy are not the same capability" (submolt agentskills, 1 comment within minutes)
- 30 drain replies posted, 28 verified (93% before claude-first verify; 5/5 = 100% after the switch)
- 8 reciprocity follow-backs (Undercurrent 3073, forgecascade 1245, agiotagebot 87, maven_thematrix 958, lisaclawbww 874, storjagent 1295, +2 in cycle)
- Feed-watch detected 2 distinct clustered posts on /feed?sort=hot, telegrammed once (24h dedup suppressed the rest)

### Cult-formation safety scaffolding
Mid-session Matt flagged two YouTube videos (`ddAmdYh32Q4`, `k8BOpvNHClU`) covering the AI-cult / "spiral" / AI-aided-psychosis phenomenon and warned about hallucination/obsession risk when LLMs read this content. Output:
- Raw transcripts quarantined at `J:/claudeai/scratch/poison-quarantine/` (NOT in llm-wiki)
- Concept-level note at `J:/llm-wiki/concepts/ai-mediated-cult-formation.md` — describes the mechanism without quoting cult vocabulary
- Two-tier blocklist in `src/cult_vocab.rs` (vocabulary + clusters)
- Forbidden-vocab paragraph spliced into all three draft prompt builders (post / comment / DM)
- `mb feed-watch` for inbound observation (no engagement)
- Telegram dedup (24h suppress per post_id) so we don't spam alerts on persistent clusters
- The llm-wiki concept page references `cult_vocab.rs` as the source of truth for the actual word list (which evolves), keeping the wiki at the *why* level

### Wiki restructure
Independently of the moltbook work, the LLM-wiki concept doc `Thoughtful Youtubes we should analyze together.md` had grown to 27,044 lines / 3 MB and Obsidian was choking. Restructured: each transcript moved to `J:/llm-wiki/transcripts/yt-{video_id}.md`, main concept doc slimmed to ~60-line index with Processed table + workflow + paste-zone. New source notes added for Dave's Garage Morse-code prompt-injection video (`UQ4pSVS_mN0`).

## Current State

### What's working
- `mb cycle` end-to-end every 30 min
- Auto-publish from queue (3h cooldown, no approval gate)
- Drain (5/cycle, claude-first verify)
- Upvote-hot (5/cycle, topic-keyword filter)
- DMs auto-tend (claude-drafted replies)
- Feed-watch (new + hot, telegram dedup)
- Reciprocity follow-back (5/cycle, multi-gate filter)
- Cult-vocab safety check on all draft outputs
- Auto-verify on every post and comment created via `mb`

### What's stubbed / unbuilt
- `mb compose` smoke-test pipeline (fact-sheet-grounded + audit pass) — the fact-sheet at `J:/moltbook-cli/fact-sheets/clawhub-scanner-tools.md` is drafted but the subcommand is not yet wired
- Existing-post audit against cult-vocab blocklist — script not yet written
- Karma analytics + daily Telegram digest — not yet built
- Engagement reciprocity (visit-their-recent-posts after they engage on ours) — Phase 2; parked because per-author posts API endpoint is unverified
- `moltbook-recip-watcher.py` (still python on cnc, watches for follow-back of specific @-targets)
- `moltbook-curator.py` (still python on cnc, daily llm-wiki scan that appends to queue.jsonl)
- "Hot post for Matt" telegram pings — folded into feed-watch but the relevance-keyword logic from the python heartbeat isn't ported yet

### What's broken
- Nothing currently. API throws intermittent 502/504s; cycle fails-soft and skips per-step on those.
- `cargo build` warns about `Cluster has Clone+Debug but unused` — cosmetic, not blocking.

## Blocking Issues
- Network instability at Matt's home (3-6 second cutouts pre-mesh-fleet shutdown). cnc cycle keeps running because cnc has direct WAN; only Matt's interactive sessions to cnc are affected. **Not blocking the agent**, only blocking interactive operator work.

## What's Next
Priority order based on this session's discussion:
1. `mb compose` — the fact-sheet-grounded smoke-test pipeline. Fact-sheet exists, audit-pass design discussed (claude self-critique against fact sheet, second pass strips untraced claims), no code yet.
2. Audit existing posts against cult-vocab blocklist — should be a quick `mb audit` subcommand: iterate all our authored posts/comments, run `cult_vocab::scan` on each, list any hits. Mostly diagnostic.
3. Karma analytics + daily Telegram digest — track karma history in state file, push 24h delta to telegram once a day.
4. Port `moltbook-recip-watcher.py` and `moltbook-curator.py` to Rust subcommands.
5. Engagement reciprocity (Phase 2) — needs API probing for "posts by author X" first.

## Notes for Next Session
- Matt's network is unstable (mesh fleet pulled, running TP-Link bridge + node-05 only). **SSH to cnc may be flaky.** If you need to do heavy cnc work, expect retries.
- The `mb-bookworm` binary build pattern: `podman run --rm -v /opt/moltbook-cli:/src -w /src -e CARGO_TARGET_DIR=/src/target-bookworm -e RUSTC_WRAPPER= rust:1-bookworm cargo build --release` then `cp target-bookworm/release/mb /usr/local/bin/mb-bookworm`. The host `mb` will NOT run inside the container because of glibc version mismatch.
- Verification codes are SINGLE-USE. The cycle uses claude/sonnet PRIMARY for verify (not regex) because we get exactly one shot per challenge.
- Comments come threaded from the API. Flatten before computing unrep counts.
- Cult-vocab is in `src/cult_vocab.rs` — that file is the source of truth for the word list. `J:/llm-wiki/concepts/ai-mediated-cult-formation.md` explains *why* but does not list the words.
- Post quality on the existing 3 drain replies (sampled) was technically solid but had two persona-drift problems: signing as "— Matt G" and referring to Matt G in the third person. The new prompt explicitly forbids this; if it recurs, tighten the system prompt further.
- `moltbook-heartbeat.service` is **disabled, do not re-enable**. It would re-introduce the unverified-comment leak.
- `moltbook-data` named podman volume holds: claude OAuth credentials, moltbook API key, post_state.json, observations.jsonl, feedwatch_state.json, reciprocity_state.json. Back this up if cnc disk gets dicey.
- The `moltbook-drain` interim units have been deleted; only `moltbook-cycle.{service,timer}` should be present in `/etc/systemd/system/`.
