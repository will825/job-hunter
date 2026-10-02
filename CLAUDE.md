# CLAUDE.md — Job Hunter

Self-hosted job search engine in Rust (tokio + axum + rusqlite `bundled` + reqwest `rustls`).
Single binary: embedded SQLite, embedded web UI, no build step for the frontend.
Runs 24/7 on a Raspberry Pi (systemd service + 7 AM digest cron); see `scheduling/`.

## Pipeline

1. **Fetch** (async, no DB) — `pipeline::fetch_all` pulls every watched source, one at a
   time with a 250 ms pause. Per-board errors are captured, never fatal.
2. **Store** (sync, no network) — `pipeline::store_all` runs `enrich` on each job
   (classify work_mode/region/seniority → slim `raw_json` to `{isRemote, workplaceType, country}` →
   Stage-1 keyword score → tier), then
   `db::upsert_job` (dedup by stable `id`, then fuzzy `dedup_key`; direct ATS beats aggregators).
   Aggregator postings re-listed per city (same `title_key` = normalized company + title, seen in
   the last 30 days) merge into one row, appending to `locations` (JSON array). ATS rows never
   merge across locations. `job_hunter dedupe` collapses older per-city duplicates.
   `job_hunter rescore` / `POST /api/rescore` (`pipeline::rescore_all`) re-run enrich on every
   stored job with the current profile in one transaction, then `rederive_llm_tiers`. Profile edits
   in the web UI (roles/skills/interests, resume upload) return at once and re-score in the
   background under the scan lock (phase "re-scoring" in `/api/scan/status`); skipped if a scan
   is running, re-run once more if another edit lands mid-rescore.
3. **Prune** — `db::prune_stale` deletes untriaged jobs (`status IS NULL`) not seen in
   `STALE_DAYS` (14). Must run *after* store so live jobs have a fresh `last_seen`.
4. **LLM re-rank** — `pipeline::rescore_llm_owned` (the one Send-safe path for CLI, digest, and
   web) has Groq score `db::top_for_rescore` candidates (one per `title_key`, no dismissed/applied/
   rejected, no onsite when hidden, recent first), 2 calls at a time (`llm_score`, reasoning, gaps);
   `db::rederive_llm_tiers` sets tiers from fit scores (thresholds: profile `[llm] tier_apply_now`/
   `tier_strong`/`tier_maybe`, default 80/62/40). Skipped cleanly with no `GROQ_API_KEY`.
   Retries are short (≤20s per wait, ≤60s per call). A 429 for the daily quota (Retry-After
   over 60s, or "per day" in the body) is `llm::QuotaExhausted`: no retries, breaker trips at
   once, `llm_error` = "Groq daily quota used up". Estimated prompt tokens go in `last_run`.
5. **Digest** — `job_hunter digest` first runs `llm::check_model` (one tiny Groq request); if it
   fails (after one retry 2 min later if Groq was unreachable), the LLM step is skipped and the digest falls back to keyword matches with the reason in
   the banner. Emails new apply_now/strong matches via Resend, marks `notified_at`. First run only baselines (meta key `digest_baselined`).

CLI `scan`/`digest` use `pipeline::full_scan`. The web "Scan now" (`POST /api/scan`) returns 202
and runs `server::run_web_scan` in a spawned task — the same steps with short-lived connections,
skipping custom pages — reporting progress in `AppState.scan` (`GET /api/scan/status`, polled by the UI).

Only one scan runs at a time: every scan (CLI scan/digest and web) holds `scan_lock::ScanLock`
on `./jobhunter.scan.lock`. The web returns 409 if it's held; the CLI waits up to 10 min —
except `digest`, which skips its fetch and sends from the DB if a web scan is in "ai scoring"
(the web scan writes its phase into the lock file). A lock
whose pid is dead is taken over (or, if the pid can't be checked, once it's older than 30 min).

Web UI is **one file**, `src/web/index.html` (inline CSS + vanilla JS), embedded via
`include_str!` in `server.rs`. Views: Home (triage), Swipe, Tracker, Analytics, Profile,
Watched companies, Digest email, Settings. It talks to the `/api/*` JSON routes in `server.rs`.

## Module map

- `src/main.rs` — CLI entry, `.env` loader, subcommands: (none)=scan, `serve`, `add <url>`, `list`, `remove <id>`, `digest`, `dedupe`, `rescore`, `compact` (slim raw_json + VACUUM), `doctor` (setup checks; runs before `db::init`, never creates files).
- `src/server.rs` — axum router + JSON API handlers; binds `0.0.0.0:8787`; optional `JOBHUNTER_TOKEN` gates `/api/*` (X-Token header or `jh_token` cookie). `GET /` sends CSP + nosniff; the UI escapes all API data with `esc()`/`safeUrl()` and uses delegated listeners (no inline handlers with data).
- `src/scan_lock.rs` — cross-process scan lock file (create_new; takeover when holder pid is dead).
- `src/pipeline.rs` — fetch/store/prune/rescore orchestration, `full_scan`, Adzuna source expansion.
- `src/db.rs` — SQLite schema (`jobs`, `meta`, `companies`), `migrate`, seeding, upsert/dedup, queries.
- `src/models.rs` — `Job` struct; stable sha256 id from company+title+url.
- `src/sources.rs` — `Source` enum (one variant per platform) + default seed watchlist.
- `src/classify.rs` — heuristic work_mode / region / seniority; returns `unknown` rather than guess.
- `src/score.rs` — Stage-1 transparent keyword score + `Tier` thresholds.
- `src/profile.rs` — `profile.toml` load/create, `compile()` to `ScoringModel`, comment-preserving edits (toml_edit).
- `src/llm.rs` — Groq client: `score_fit` (re-rank) and `extract_jobs` (custom pages). Isolated/removable.
- `src/custom_page.rs` — optional LLM reader for arbitrary careers pages (`[custom_pages] enabled`).
- `src/detect.rs` — pasted careers URL → validated `Source` (sniffs embedded ATS boards).
- `src/email.rs` — Resend digest compose + send. Isolated/removable.
- `src/text.rs` — HTML→text stripper, normalization, fuzzy `dedup_key`.
- `src/fetchers/mod.rs` — `Fetcher` trait + `fetch_source` dispatch (the one Source→fetcher map).
- `src/fetchers/greenhouse.rs` — Greenhouse board API (HTML descriptions, double-escaped).
- `src/fetchers/lever.rs` — Lever postings API.
- `src/fetchers/ashby.rs` — Ashby posting API.
- `src/fetchers/adzuna.rs` — Adzuna search (needs `ADZUNA_APP_ID`/`ADZUNA_APP_KEY`), one query per target role.
- `src/fetchers/remotive.rs` — Remotive aggregator by category.
- `src/fetchers/remoteok.rs` — RemoteOK aggregator (first array element is metadata; skip it).
- `src/fetchers/himalayas.rs` — Himalayas remote-jobs aggregator.
- `src/fetchers/jobicy.rs` — Jobicy remote-jobs aggregator by tag.

Adding a source: new file in `src/fetchers/`, a `Source` variant + `ats()`/`from_ats()` arms in
`sources.rs`, and an arm in `fetch_source`. Aggregators get lower `db::source_priority`.

## Hard rules

- **Never hold a rusqlite `Connection` across an `.await`** (it is not `Send`). Open, use,
  drop, then await. In handlers, scope the connection in a block; reopen after the await.
  (`full_scan` holds one only because the CLI never needs `Send` — don't copy that into `server.rs`.)
- **Never string-interpolate user input into SQL.** Use bound params (`?1`, `params![]`).
  Sort/order values come from a fixed whitelist (see `db::search_jobs`).
- **Never read, print, or commit `.env`, `profile.toml`, or `jobs.db`** (or `jobs.db-wal`/`-shm`,
  resumes, logs). They are private and gitignored. Use `.env.example` / `profile.example.toml`.
- **Fetchers fail soft.** One bad board returns `Err`, gets logged as a failure, and the scan
  continues. Never `unwrap`/panic on network or JSON shape in a fetcher.
- **Schema changes go through `db::migrate`** as additive, idempotent steps (add column only if
  missing, backfill with idempotent `UPDATE`). Also update `init_schema` for fresh DBs.
  Never drop or rename columns/tables holding user data.
- Optional features (LLM, custom pages, email, Adzuna) must degrade to "off" with a friendly
  message when unconfigured — never error the run.

## Verify

```bash
cargo test
cargo build --release
./target/release/job_hunter serve   # then open http://127.0.0.1:8787
```

Tests live inline (`#[cfg(test)]`) in db, models, text, classify, score, etc.
`serve` runs against the real local `jobs.db`/`profile.toml` — don't dump their contents.

## Docs

- `README.md` — public portfolio overview (repo: will825/job-hunter).
- `PROJECT_PLAN.md` — phase history; partly stale (e.g. says Gmail/lettre, email is now Resend).
- `scheduling/` — macOS launchd plists + `run-digest.sh`; `scheduling/linux/` systemd unit + Pi guide.

## Commits

One commit per task, authored as Will. Do **not** add `Co-Authored-By` trailers,
"Generated with Claude Code" lines, or `Claude-Session` lines to commit messages.
