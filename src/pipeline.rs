//! The scan pipeline: fetch every watched source, classify + score each job,
//! and store it. Shared by the CLI (`cargo run`) and the web UI's "scan now".
//!
//! Split into an async **fetch** phase (network, no DB) and a sync **store**
//! phase (DB, no network). Keeping the SQLite connection out of any `.await`
//! is what lets the web handler stay `Send`; it's also just cleaner.

use std::time::Duration;

use anyhow::Result;
use rusqlite::Connection;

use crate::classify;
use crate::custom_page;
use crate::db::{self, Upsert};
use crate::llm::{self, LlmConfig};
use crate::models::Job;
use crate::profile::{Profile, ScoringModel};
use crate::score;
use crate::sources::Source;

/// Small pause between boards — polite to the APIs, negligible to the run.
const BETWEEN_BOARDS: Duration = Duration::from_millis(250);

/// Context needed to read custom (non-ATS) pages. Present only when the
/// custom-page feature is enabled and the LLM is ready.
pub struct CustomCtx<'a> {
    pub cfg: &'a LlmConfig,
    pub profile: &'a Profile,
}

/// The outcome of fetching one board.
pub struct BoardFetch {
    pub label: String,
    pub result: Result<Vec<Job>>,
}

/// Aggregate result of a scan.
#[derive(Debug, Default, Clone)]
pub struct ScanSummary {
    pub boards_scanned: usize,
    pub total_fetched: usize,
    pub inserted: usize,
    pub already_seen: usize,
    pub merged: usize,
    pub total_in_db: i64,
    /// (label, error) for each board that failed — the scan continues past them.
    pub failures: Vec<(String, String)>,
    /// (label, count) per successful board, in scan order.
    pub per_board: Vec<(String, usize)>,
}

/// Phase 1 (async, no DB): fetch every source, one at a time. Per-board errors
/// are captured, never fatal. `progress` is called after each board.
pub async fn fetch_all(
    client: &reqwest::Client,
    sources: &[Source],
    custom: Option<&CustomCtx<'_>>,
    mut progress: impl FnMut(&str),
) -> Vec<BoardFetch> {
    let mut out = Vec::with_capacity(sources.len());
    for (i, source) in sources.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(BETWEEN_BOARDS).await;
        }
        let result = if source.is_custom() {
            match custom {
                Some(ctx) => custom_page::fetch(source.token(), client, ctx.cfg, ctx.profile).await,
                // Feature off: report a friendly skip rather than a scary error.
                None => Err(anyhow::anyhow!("custom-page reading is off (enable [custom_pages] in profile.toml)")),
            }
        } else {
            crate::fetchers::fetch_source(source, client).await
        };
        match &result {
            Ok(jobs) => progress(&format!("✓ {:<24} {} job(s)", source.label(), jobs.len())),
            Err(e) => progress(&format!("✗ {:<24} {e}", source.label())),
        }
        out.push(BoardFetch { label: source.label(), result });
    }
    out
}

/// Phase 2 (sync, no network): enrich + store the fetched jobs.
pub fn store_all(conn: &Connection, model: &ScoringModel, fetched: Vec<BoardFetch>) -> Result<ScanSummary> {
    let mut s = ScanSummary { boards_scanned: fetched.len(), ..Default::default() };
    for board in fetched {
        match board.result {
            Ok(mut jobs) => {
                s.total_fetched += jobs.len();
                s.per_board.push((board.label, jobs.len()));
                for job in &mut jobs {
                    enrich(job, model);
                    match db::upsert_job(conn, job)? {
                        Upsert::Inserted => s.inserted += 1,
                        Upsert::AlreadySeen => s.already_seen += 1,
                        Upsert::MergedDuplicate => s.merged += 1,
                    }
                }
            }
            Err(e) => s.failures.push((board.label, e.to_string())),
        }
    }
    s.total_in_db = db::count_jobs(conn)?;
    Ok(s)
}

/// Stage-2 LLM re-ranking: for the top keyword survivors, ask the LLM to judge
/// genuine fit and let its verdict override the tier. Skips cleanly (returns 0)
/// when the LLM isn't ready. Keeps the DB connection out of any `.await`.
pub async fn rescore_llm(
    conn: &Connection,
    client: &reqwest::Client,
    cfg: &LlmConfig,
    profile: &Profile,
    mut progress: impl FnMut(&str),
) -> Result<usize> {
    if !cfg.is_ready() {
        return Ok(0);
    }
    // Read the candidates (sync), then release the borrow before any await.
    let candidates = db::top_for_rescore(conn, cfg.max_jobs_per_run)?;
    if candidates.is_empty() {
        return Ok(0);
    }

    let mut written = 0usize;
    for (id, company, title, description) in &candidates {
        match llm::score_fit(cfg, client, profile, title, company, description).await {
            Ok(v) => {
                // Persist each verdict as it's computed, so an interrupted run
                // (timeout, reboot, rate-limit abort) keeps the work already done
                // instead of discarding the whole batch.
                db::set_llm_verdict(conn, id, v.fit_score, &v.reasoning, &v.gaps.join("; "))?;
                written += 1;
            }
            Err(e) => progress(&format!("  (llm skipped {title}: {e})")),
        }
        tokio::time::sleep(Duration::from_millis(120)).await; // gentle on the API
    }

    // Tiers are derived from the fit scores, not the LLM's tier label.
    db::rederive_llm_tiers(conn)?;
    Ok(written)
}

/// Classify and keyword-score a job in place, before it's stored.
/// Classification runs first because scoring reads `work_mode`/`region`.
pub fn enrich(job: &mut Job, model: &ScoringModel) {
    let cls = classify::classify(job);
    job.work_mode = cls.work_mode;
    job.region = cls.region;
    job.seniority = cls.seniority;
    job.keyword_score = score::keyword_score(job, model);
    job.tier = score::tier_for(job.keyword_score, &model.tiers).as_str().to_string();
}

/// How long a posting can go unseen by scans before it's pruned as expired.
pub const STALE_DAYS: i64 = 14;

/// Run a complete scan: load sources (+ Adzuna + custom pages if enabled),
/// fetch, store, and LLM-rerank. One code path shared by `cargo run` and the
/// daily `digest`. Holds the DB connection across awaits (fine for the CLI).
pub async fn full_scan(
    conn: &Connection,
    client: &reqwest::Client,
    profile: &Profile,
    mut progress: impl FnMut(&str),
) -> Result<ScanSummary> {
    let model = profile.compile();
    let cfg = crate::llm::LlmConfig::from_profile(profile);

    let custom_enabled = profile.custom_pages.enabled && cfg.is_ready();
    let mut sources: Vec<Source> = load_sources(conn)?
        .into_iter()
        .filter(|s| !s.is_custom() || custom_enabled)
        .collect();
    sources.extend(adzuna_sources(profile));
    let ctx = custom_enabled.then(|| CustomCtx { cfg: &cfg, profile });

    let fetched = fetch_all(client, &sources, ctx.as_ref(), &mut progress).await;
    let summary = store_all(conn, &model, fetched)?;

    // Drop postings the boards no longer list (expired/filled). Runs after
    // store_all so anything still live has just had its last_seen refreshed.
    let removed = db::prune_stale(conn, STALE_DAYS)?;
    if removed > 0 {
        progress(&format!("Pruned {removed} stale posting(s) not seen in {STALE_DAYS} days."));
    }

    if cfg.is_ready() {
        progress(&format!("LLM fit-scoring top matches ({})…", cfg.model));
        let n = rescore_llm(conn, client, &cfg, profile, &mut progress).await?;
        progress(&format!("Re-scored {n} job(s) with the LLM."));
    } else {
        progress(&format!("(LLM off: {} — keyword scoring only)", cfg.why_not_ready()));
    }
    Ok(summary)
}

/// Build Adzuna search sources from the profile's target roles — one broad
/// internet-wide search per role — when Adzuna is enabled and its credentials
/// are present. Returns empty otherwise (feature simply off).
pub fn adzuna_sources(profile: &Profile) -> Vec<Source> {
    if !profile.adzuna.enabled {
        return Vec::new();
    }
    let has_creds = std::env::var("ADZUNA_APP_ID").ok().is_some_and(|s| !s.trim().is_empty())
        && std::env::var("ADZUNA_APP_KEY").ok().is_some_and(|s| !s.trim().is_empty());
    if !has_creds {
        return Vec::new();
    }
    let max = profile.adzuna.max_roles.max(0) as usize;
    profile
        .target_roles
        .iter()
        .filter(|r| !r.trim().is_empty())
        .take(max)
        .map(|r| Source::Adzuna(r.clone()))
        .collect()
}

/// Load the watchlist from the DB (seeding defaults on first run).
pub fn load_sources(conn: &Connection) -> Result<Vec<Source>> {
    let companies = db::list_companies(conn)?;
    // Preserve a stable scan order (oldest first) for readable output.
    let mut sources: Vec<Source> = companies
        .iter()
        .rev()
        .filter_map(|c| Source::from_ats(&c.ats, &c.token))
        .collect();
    if sources.is_empty() {
        sources = crate::sources::seed();
    }
    Ok(sources)
}
