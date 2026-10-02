//! The scan pipeline: fetch every watched source, classify + score each job,
//! and store it. Shared by the CLI (`cargo run`) and the web UI's "scan now".
//!
//! Split into an async **fetch** phase (network, no DB) and a sync **store**
//! phase (DB, no network). Keeping the SQLite connection out of any `.await`
//! is what lets the web handler stay `Send`; it's also just cleaner.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use futures::stream::{self, StreamExt};
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
    pub source: Source,
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
    /// Jobs that hit a DB error while storing — logged and skipped.
    pub store_failed: usize,
    pub total_in_db: i64,
    /// (label, error) for each board that failed — the scan continues past them.
    pub failures: Vec<(String, String)>,
    /// (label, count) per successful board, in scan order.
    pub per_board: Vec<(String, usize)>,
    pub boards_failed: usize,
    /// Stale postings removed by this run's prune step.
    pub pruned: usize,
    /// Postings closed by the store step: missing from their ATS board's
    /// listing, or aggregator jobs unseen/too old.
    pub closed: usize,
    /// `[llm] enabled` in the profile (independent of whether a key is set).
    pub llm_enabled: bool,
    pub llm_scored: usize,
    pub llm_failed: usize,
    /// Why the LLM step didn't fully work: the first call's error in full, or
    /// why it couldn't run at all (e.g. no key). `None` when it ran cleanly or
    /// is disabled.
    pub llm_error: Option<String>,
    /// Groq's daily quota ran out this run (the LLM step stopped early).
    pub llm_quota_exhausted: bool,
    /// The LLM circuit breaker opened (remaining candidates were skipped).
    pub llm_tripped: bool,
    /// Estimated prompt tokens the LLM calls used (chars / 4), to watch the
    /// daily cap.
    pub llm_tokens_est: u64,
}

impl ScanSummary {
    /// Fold the LLM step's outcome into the summary.
    pub fn set_llm(&mut self, cfg: &LlmConfig, tally: &LlmTally) {
        self.llm_enabled = cfg.enabled;
        self.llm_scored = tally.scored;
        self.llm_failed = tally.failed;
        self.llm_quota_exhausted = tally.quota_exhausted;
        self.llm_tripped = tally.tripped;
        self.llm_tokens_est = tally.tokens_est;
        self.llm_error = if cfg.enabled && !cfg.is_ready() {
            Some(cfg.why_not_ready().to_string())
        } else {
            tally.first_error.clone()
        };
    }

    /// The LLM is enabled but failed this run — produced no scores, or ran out
    /// of daily quota part-way — the digest's cue to fall back to keyword
    /// matches. (Zero scores with no error just means nothing new to score.)
    pub fn llm_failed_run(&self) -> bool {
        self.llm_enabled
            && (self.llm_quota_exhausted || (self.llm_scored == 0 && self.llm_error.is_some()))
    }
}

/// Success/failure count for one run's LLM calls, with a circuit breaker: if
/// the first [`LlmTally::BREAKER`] calls all fail, the rest of the run skips
/// the LLM (it's almost certainly a config/outage problem, not a bad job).
#[derive(Debug, Default, Clone)]
pub struct LlmTally {
    pub scored: usize,
    pub failed: usize,
    pub first_error: Option<String>,
    /// The breaker opened and remaining candidates were skipped.
    pub tripped: bool,
    /// Groq's daily quota ran out (trips the breaker immediately).
    pub quota_exhausted: bool,
    /// Estimated prompt tokens used by this run's calls.
    pub tokens_est: u64,
}

impl LlmTally {
    pub const BREAKER: usize = 3;
    /// `llm_error` when the daily quota runs out — shown in the digest banner.
    pub const QUOTA_MSG: &'static str = "Groq daily quota used up";

    pub fn ok(&mut self) {
        self.scored += 1;
    }

    /// Record a failed call. Returns `true` for the run's first failure (or
    /// the quota running out), which the caller should log in full. A spent
    /// daily quota trips the breaker at once — no point trying other jobs.
    pub fn fail(&mut self, e: &anyhow::Error) -> bool {
        self.failed += 1;
        if llm::is_quota_exhausted(e) && !self.quota_exhausted {
            self.quota_exhausted = true;
            self.tripped = true;
            self.first_error = Some(Self::QUOTA_MSG.to_string());
            return true;
        }
        if self.first_error.is_none() {
            self.first_error = Some(format!("{e:#}"));
            true
        } else {
            false
        }
    }

    /// Why the breaker opened, for the log line.
    pub fn stop_reason(&self) -> String {
        if self.quota_exhausted {
            format!("LLM stopped: {}", Self::QUOTA_MSG)
        } else {
            format!("LLM circuit breaker: first {} calls failed", self.failed)
        }
    }

    /// Whether to stop calling the LLM for the rest of this run.
    pub fn should_stop(&mut self) -> bool {
        if self.scored == 0 && self.failed >= Self::BREAKER {
            self.tripped = true;
        }
        self.tripped
    }
}

/// Save a run's summary as JSON in `meta` under `last_run`, for the web UI.
/// `trigger` is what started the run: "scan", "digest", or "web". The previous
/// run's failed board labels are kept as `prev_failed_boards`, so health can
/// tell a board that keeps failing from a one-off blip.
pub fn record_last_run(conn: &Connection, trigger: &str, s: &ScanSummary) -> Result<()> {
    let prev = db::meta_get(conn, "last_run")?
        .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok())
        .map(|r| failed_boards(&r))
        .unwrap_or_default();
    let mut run = summary_json(trigger, s);
    run["prev_failed_boards"] = serde_json::json!(prev);
    db::meta_set(conn, "last_run", &run.to_string())
}

/// The labels of the boards that failed in a stored run (its `board_errors`).
pub fn failed_boards(run: &serde_json::Value) -> Vec<String> {
    run["board_errors"]
        .as_array()
        .map(|errs| errs.iter().filter_map(|e| e["board"].as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Add one field to the stored `last_run` (e.g. the digest's first-run
/// baseline count). With no readable `last_run`, starts one for `trigger`.
pub fn note_last_run(conn: &Connection, trigger: &str, key: &str, value: serde_json::Value) -> Result<()> {
    let mut run = db::meta_get(conn, "last_run")?
        .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| {
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            serde_json::json!({ "at": at, "trigger": trigger })
        });
    run[key] = value;
    db::meta_set(conn, "last_run", &run.to_string())
}

/// A run's summary as JSON — the shape stored in `last_run` and returned by
/// the web scan status.
pub fn summary_json(trigger: &str, s: &ScanSummary) -> serde_json::Value {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let board_errors: Vec<_> = s
        .failures
        .iter()
        .map(|(label, error)| serde_json::json!({ "board": label, "error": error }))
        .collect();
    serde_json::json!({
        "at": at,
        "trigger": trigger,
        "boards_scanned": s.boards_scanned,
        "boards_failed": s.boards_failed,
        "total_fetched": s.total_fetched,
        "inserted": s.inserted,
        "already_seen": s.already_seen,
        "merged": s.merged,
        "store_failed": s.store_failed,
        "pruned": s.pruned,
        "closed": s.closed,
        "total_in_db": s.total_in_db,
        "llm_enabled": s.llm_enabled,
        "llm_scored": s.llm_scored,
        "llm_failed": s.llm_failed,
        "llm_error": s.llm_error,
        "llm_quota_exhausted": s.llm_quota_exhausted,
        "llm_tripped": s.llm_tripped,
        "llm_tokens_est": s.llm_tokens_est,
        "board_errors": board_errors,
    })
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
        out.push(BoardFetch { label: source.label(), source: source.clone(), result });
    }
    out
}

/// Phase 2 (sync, no network): enrich + store the fetched jobs.
///
/// All upserts go in one transaction, so a scan is a single commit rather than
/// one per job. A DB error on one job is logged and counted, never fatal.
pub fn store_all(conn: &Connection, model: &ScoringModel, fetched: Vec<BoardFetch>) -> Result<ScanSummary> {
    let mut s = ScanSummary { boards_scanned: fetched.len(), ..Default::default() };
    let tx = conn.unchecked_transaction()?;
    // Every job stored below gets a last_seen at or after this, so a board's
    // rows still older than it weren't in that board's listing.
    let since: String = tx.query_row("SELECT datetime('now')", [], |r| r.get(0))?;
    for board in fetched {
        match board.result {
            Ok(mut jobs) => {
                s.total_fetched += jobs.len();
                let mut store_failed = false;
                for job in &mut jobs {
                    job.board = board.label.clone();
                    enrich(job, model);
                    match db::upsert_job(&tx, job) {
                        Ok(Upsert::Inserted) => s.inserted += 1,
                        Ok(Upsert::AlreadySeen) => s.already_seen += 1,
                        Ok(Upsert::MergedDuplicate) => s.merged += 1,
                        Err(e) => {
                            s.store_failed += 1;
                            store_failed = true;
                            eprintln!("couldn't store {} — {}: {e:#}", job.company, job.title);
                        }
                    }
                }
                // A complete ATS listing: whatever it no longer lists has
                // closed. Skipped if a job failed to store (it'd look missing).
                if board.source.is_ats() && !store_failed {
                    let token = board.source.token();
                    let mut names: Vec<String> = jobs.iter().map(|j| j.company.clone()).collect();
                    names.push(token.to_string());
                    names.push(crate::text::company_from_token(token));
                    names.sort();
                    names.dedup();
                    s.closed += db::close_missing_from_board(&tx, board.source.ats(), &board.label, &since, &names)?;
                }
                s.per_board.push((board.label, jobs.len()));
            }
            Err(e) => s.failures.push((board.label, e.to_string())),
        }
    }
    s.closed += db::close_stale_aggregators(&tx, AGGREGATOR_UNSEEN_DAYS, AGGREGATOR_MAX_AGE_DAYS)?;
    tx.commit()?;
    s.boards_failed = s.failures.len();
    s.total_in_db = db::count_jobs(conn)?;
    Ok(s)
}

/// How many LLM fit-scoring calls run at once. Two keeps a run quick without
/// tripping Groq's per-minute limits (429s back off inside `llm::groq_json`).
const LLM_CONCURRENCY: usize = 2;

/// Stage-2 LLM re-ranking: for the top keyword survivors (see
/// [`db::top_for_rescore`]), ask the LLM to judge genuine fit and let its
/// verdict override the tier. Skips cleanly (empty tally) when the LLM isn't
/// ready. Logs the run's first LLM error in full and stops early if the first
/// calls all fail (see [`LlmTally`]).
///
/// Opens short-lived connections to `db_path` and never holds one across an
/// await, so the future is `Send` (when `progress` is) and the web server can
/// spawn it. Shared by the CLI scan, the digest, and the web scan.
pub async fn rescore_llm_owned(
    db_path: &str,
    profile: &Profile,
    client: &reqwest::Client,
    mut progress: impl FnMut(&str),
) -> Result<LlmTally> {
    let mut tally = LlmTally::default();
    let cfg = LlmConfig::from_profile(profile);
    if !cfg.is_ready() {
        return Ok(tally);
    }
    let model = profile.compile();
    let hide_onsite = model.hide_onsite;
    let candidates = {
        let conn = db::connect(db_path)?;
        db::top_for_rescore(&conn, cfg.max_jobs_per_run, hide_onsite)?
    };
    if candidates.is_empty() {
        return Ok(tally);
    }

    // Calls run LLM_CONCURRENCY at a time. Once the breaker opens, calls not yet
    // started see `stop` and are skipped instead of sent.
    let stop = AtomicBool::new(false);
    let mut skipped = 0usize;
    let mut results = stream::iter(candidates)
        .map(|job| {
            let (cfg, stop) = (&cfg, &stop);
            async move {
                if stop.load(Ordering::Relaxed) {
                    return (job, None);
                }
                let verdict = llm::score_fit(cfg, client, profile, &job).await;
                (job, Some(verdict))
            }
        })
        .buffer_unordered(LLM_CONCURRENCY);

    while let Some((job, result)) = results.next().await {
        match result {
            None => skipped += 1,
            Some(Ok(v)) => {
                // Persist each verdict as it's computed, so an interrupted run
                // (timeout, reboot, rate-limit abort) keeps the work already done.
                let conn = db::connect(db_path)?;
                db::set_llm_verdict(&conn, &job.id, v.fit_score, &v.reasoning, &v.gaps.join("; "))?;
                // The same role listed elsewhere with the same work mode shares it.
                db::copy_llm_verdict_to_twins(&conn, &job.id)?;
                tally.ok();
            }
            Some(Err(e)) => {
                if tally.fail(&e) {
                    progress(&format!("  LLM error (first this run) on {}: {e:#}", job.title));
                } else {
                    progress(&format!("  (llm skipped {}: {e})", job.title));
                }
            }
        }
        if tally.should_stop() {
            stop.store(true, Ordering::Relaxed);
        }
    }
    drop(results);
    if skipped > 0 {
        progress(&format!("  {} — skipped the other {skipped} job(s) this run.", tally.stop_reason()));
    }

    // Tiers are derived from the fit scores, not the LLM's tier label.
    let conn = db::connect(db_path)?;
    db::rederive_llm_tiers(&conn, hide_onsite, model.llm_tiers)?;
    tally.tokens_est = cfg.tokens_used();
    progress(&format!("  LLM used ~{} prompt tokens this run (estimate).", tally.tokens_est));
    Ok(tally)
}

/// Classify and keyword-score a job in place, before it's stored.
/// Classification runs first because scoring reads `work_mode`/`region`.
pub fn enrich(job: &mut Job, model: &ScoringModel) {
    let cls = classify::classify(job);
    // Classification is the only reader of raw_json: keep just what it uses.
    job.raw_json = classify::slim_raw_json(&job.raw_json);
    job.work_mode = cls.work_mode;
    job.region = cls.region;
    job.seniority = cls.seniority;
    job.keyword_score = score::keyword_score(job, model);
    job.tier = score::job_tier(job, job.keyword_score, model).as_str().to_string();
}

/// What a [`rescore_all`] changed: tier counts across the whole DB before and
/// after, and how many jobs were re-scored.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RescoreSummary {
    pub jobs: usize,
    pub before: BTreeMap<String, i64>,
    pub after: BTreeMap<String, i64>,
}

/// Re-run [`enrich`] on every stored job against `model` (the current
/// profile), using each job's stored title/description/location/raw_json — so
/// a profile change applies without waiting for boards to be re-fetched. One
/// transaction. Jobs with an LLM verdict then get their tier back from the fit
/// score via [`db::rederive_llm_tiers`].
pub fn rescore_all(conn: &Connection, model: &ScoringModel) -> Result<RescoreSummary> {
    let tx = conn.unchecked_transaction()?;
    let before = db::tier_counts(&tx)?;
    let mut jobs = db::stored_jobs(&tx)?;
    for job in &mut jobs {
        enrich(job, model);
        db::set_enrichment(&tx, job)?;
    }
    db::rederive_llm_tiers(&tx, model.hide_onsite, model.llm_tiers)?;
    let after = db::tier_counts(&tx)?;
    tx.commit()?;
    Ok(RescoreSummary { jobs: jobs.len(), before, after })
}

/// How long a posting can go unseen by scans before it's pruned as expired.
pub const STALE_DAYS: i64 = 14;
/// Aggregator jobs not seen by a scan for this long are closed.
pub const AGGREGATOR_UNSEEN_DAYS: i64 = 3;
/// Aggregator jobs posted longer ago than this are closed.
pub const AGGREGATOR_MAX_AGE_DAYS: i64 = 30;

/// Run a complete scan: load sources (+ Adzuna + custom pages if enabled),
/// fetch, store, LLM-rerank, then a liveness pass over Home's top jobs. One code path shared by `cargo run` and the
/// daily `digest`. Holds the DB connection across awaits (fine for the CLI).
/// With `llm_skip`, the LLM step doesn't run and that reason becomes the run's
/// `llm_error` (so a digest falls back to keyword matches with it in the banner).
pub async fn full_scan(
    conn: &Connection,
    db_path: &str,
    client: &reqwest::Client,
    profile: &Profile,
    llm_skip: Option<String>,
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
    let ctx = custom_enabled.then_some(CustomCtx { cfg: &cfg, profile });

    let fetched = fetch_all(client, &sources, ctx.as_ref(), &mut progress).await;
    let mut summary = store_all(conn, &model, fetched)?;

    // Drop postings the boards no longer list (expired/filled). Runs after
    // store_all so anything still live has just had its last_seen refreshed.
    let removed = db::prune_stale(conn, STALE_DAYS)?;
    summary.pruned = removed;
    if removed > 0 {
        progress(&format!("Pruned {removed} stale posting(s) not seen in {STALE_DAYS} days."));
    }

    let mut tally = LlmTally::default();
    if let Some(reason) = llm_skip.filter(|_| cfg.is_ready()) {
        progress(&format!("(LLM skipped: {reason} — keyword scoring only)"));
        tally.first_error = Some(reason);
    } else if cfg.is_ready() {
        progress(&format!("LLM fit-scoring top matches ({})…", cfg.model));
        tally = rescore_llm_owned(db_path, profile, client, &mut progress).await?;
        progress(&format!("Re-scored {} job(s) with the LLM ({} failed).", tally.scored, tally.failed));
    } else {
        progress(&format!("(LLM off: {} — keyword scoring only)", cfg.why_not_ready()));
    }
    summary.set_llm(&cfg, &tally);

    progress(&format!("Checking the top {} postings are still up…", crate::liveness::TOP_N));
    match crate::liveness::run_pass(db_path, client).await {
        Ok(p) => progress(&format!(
            "Checked {} posting(s): {} closed, {} inconclusive.",
            p.checked, p.closed, p.unknown
        )),
        Err(e) => progress(&format!("(Liveness check failed: {e:#})")),
    }
    Ok(summary)
}

/// Build Adzuna search sources from the profile's target roles — one broad
/// internet-wide search per role — when Adzuna is enabled and its credentials
/// are present. Returns empty otherwise (feature simply off). Country and page
/// size come from `[adzuna]`; a non-empty `ADZUNA_COUNTRY` env var overrides
/// the country.
pub fn adzuna_sources(profile: &Profile) -> Vec<Source> {
    if !profile.adzuna.enabled {
        return Vec::new();
    }
    let has_creds = std::env::var("ADZUNA_APP_ID").ok().is_some_and(|s| !s.trim().is_empty())
        && std::env::var("ADZUNA_APP_KEY").ok().is_some_and(|s| !s.trim().is_empty());
    if !has_creds {
        return Vec::new();
    }
    let country = std::env::var("ADZUNA_COUNTRY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| profile.adzuna.country.clone());
    let max = profile.adzuna.max_roles.max(0) as usize;
    profile
        .target_roles
        .iter()
        .filter(|r| !r.trim().is_empty())
        .take(max)
        .map(|r| Source::adzuna(r.clone(), &country, profile.adzuna.results_per_page))
        .collect()
}

/// Load the watchlist from the DB. Defaults are seeded once, on a brand-new
/// DB (`db::init`); an emptied watchlist stays empty.
pub fn load_sources(conn: &Connection) -> Result<Vec<Source>> {
    let companies = db::list_companies(conn)?;
    // Preserve a stable scan order (oldest first) for readable output.
    Ok(companies
        .iter()
        .rev()
        .filter_map(|c| Source::from_ats(&c.ats, &c.token))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, key: Option<&str>) -> LlmConfig {
        LlmConfig::for_test(enabled, key)
    }

    #[test]
    fn note_last_run_adds_a_field() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        note_last_run(&conn, "digest", "digest_baselined", 3.into()).unwrap();
        let run: serde_json::Value = serde_json::from_str(&db::meta_get(&conn, "last_run").unwrap().unwrap()).unwrap();
        assert_eq!((run["trigger"].as_str(), run["digest_baselined"].as_u64()), (Some("digest"), Some(3)));

        record_last_run(&conn, "scan", &ScanSummary { inserted: 5, ..Default::default() }).unwrap();
        note_last_run(&conn, "digest", "digest_baselined", 7.into()).unwrap();
        let run: serde_json::Value = serde_json::from_str(&db::meta_get(&conn, "last_run").unwrap().unwrap()).unwrap();
        assert_eq!((run["inserted"].as_u64(), run["digest_baselined"].as_u64()), (Some(5), Some(7)));
    }

    #[test]
    fn last_run_keeps_the_previous_runs_failed_boards() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let failing = |boards: &[&str]| ScanSummary {
            failures: boards.iter().map(|b| (b.to_string(), "HTTP 500".to_string())).collect(),
            ..Default::default()
        };
        let stored = || -> serde_json::Value {
            serde_json::from_str(&db::meta_get(&conn, "last_run").unwrap().unwrap()).unwrap()
        };
        record_last_run(&conn, "scan", &failing(&["lever:spotify", "ashby:suno"])).unwrap();
        assert_eq!(stored()["prev_failed_boards"], serde_json::json!([]));
        record_last_run(&conn, "web", &failing(&["ashby:suno"])).unwrap();
        assert_eq!(stored()["prev_failed_boards"], serde_json::json!(["lever:spotify", "ashby:suno"]));
        assert_eq!(failed_boards(&stored()), ["ashby:suno"]);
    }

    fn board(source: Source, result: Result<Vec<Job>>) -> BoardFetch {
        BoardFetch { label: source.label(), source, result }
    }

    fn ats_job(company: &str, title: &str, url: &str) -> Job {
        Job::new(company, title, "Remote", url, "lever", "d", None, "{}")
            .with_display_company(crate::text::company_from_token(company))
    }

    fn open_titles(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT title FROM jobs WHERE closed_at IS NULL ORDER BY title").unwrap()
            .query_map([], |r| r.get(0)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap()
    }

    /// Pretend every stored job was last seen by an earlier scan.
    fn age_last_seen(conn: &Connection) {
        conn.execute("UPDATE jobs SET last_seen = datetime('now', '-1 hour')", []).unwrap();
    }

    #[test]
    fn ats_jobs_missing_from_a_successful_fetch_are_closed() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let model = Profile::default().compile();
        let spotify = || Source::Lever("spotify".into());
        let (a, b) = (ats_job("spotify", "Audio Engineer", "u-a"), ats_job("spotify", "Data Engineer", "u-b"));
        let other = ats_job("deepgram", "ML Engineer", "u-c");
        store_all(&conn, &model, vec![
            board(spotify(), Ok(vec![a.clone(), b.clone()])),
            board(Source::Lever("deepgram".into()), Ok(vec![other])),
        ]).unwrap();
        // A row stored before `board` was recorded is matched by company.
        conn.execute("UPDATE jobs SET board = NULL WHERE title = 'Data Engineer'", []).unwrap();
        age_last_seen(&conn);

        // A failed fetch closes nothing.
        let s = store_all(&conn, &model, vec![board(spotify(), Err(anyhow::anyhow!("HTTP 500")))]).unwrap();
        assert_eq!(s.closed, 0);
        assert_eq!(open_titles(&conn).len(), 3);

        // A successful fetch without Data Engineer closes just that one.
        let s = store_all(&conn, &model, vec![board(spotify(), Ok(vec![a.clone()]))]).unwrap();
        assert_eq!(s.closed, 1);
        assert_eq!(open_titles(&conn), ["Audio Engineer", "ML Engineer"]);
        let reason: String = conn.query_row("SELECT closed_reason FROM jobs WHERE title = 'Data Engineer'", [], |r| r.get(0)).unwrap();
        assert_eq!(reason, "board");

        // Listed again → open again.
        age_last_seen(&conn);
        store_all(&conn, &model, vec![board(spotify(), Ok(vec![a, b]))]).unwrap();
        assert_eq!(open_titles(&conn).len(), 3);
    }

    #[test]
    fn liveness_closures_survive_a_board_still_listing_the_job() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let model = Profile::default().compile();
        let a = ats_job("spotify", "Audio Engineer", "u-a");
        store_all(&conn, &model, vec![board(Source::Lever("spotify".into()), Ok(vec![a.clone()]))]).unwrap();
        assert!(db::record_check(&conn, &a.id, Some("check: HTTP 404")).unwrap());
        store_all(&conn, &model, vec![board(Source::Lever("spotify".into()), Ok(vec![a]))]).unwrap();
        assert!(open_titles(&conn).is_empty());
    }

    #[test]
    fn aggregator_jobs_close_when_unseen_or_old() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let model = Profile::default().compile();
        let agg = |title: &str, url: &str, posted: Option<&str>| {
            Job::new("Acme", title, "Remote", url, "remotive", "d", posted.map(String::from), "{}")
        };
        let days_ago = |d: i64| {
            let t: String = conn.query_row(&format!("SELECT datetime('now', '-{d} days')"), [], |r| r.get(0)).unwrap();
            t.replace(' ', "T") + "Z"
        };
        let millis_40_days_ago = ((std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
            - 40 * 86_400) * 1000).to_string();
        let jobs = vec![
            agg("Fresh", "u1", Some(&days_ago(2))),
            agg("Old ISO", "u2", Some(&days_ago(31))),
            agg("Old millis", "u3", Some(&millis_40_days_ago)),
            agg("No date", "u4", None),
            agg("Junk date", "u5", Some("last week")),
        ];
        let remotive = || Source::Remotive("software-dev".into());
        let s = store_all(&conn, &model, vec![board(remotive(), Ok(jobs))]).unwrap();
        assert_eq!(s.closed, 2, "posted over 30 days ago");
        assert_eq!(open_titles(&conn), ["Fresh", "Junk date", "No date"]);

        // Not seen for 2 days: still open. For 4 days: closed.
        conn.execute("UPDATE jobs SET last_seen = datetime('now', '-2 days') WHERE title = 'Fresh'", []).unwrap();
        conn.execute("UPDATE jobs SET last_seen = datetime('now', '-4 days') WHERE title = 'No date'", []).unwrap();
        let s = store_all(&conn, &model, vec![board(remotive(), Err(anyhow::anyhow!("down")))]).unwrap();
        assert_eq!(s.closed, 1);
        assert_eq!(open_titles(&conn), ["Fresh", "Junk date"]);
    }

    #[test]
    fn closed_jobs_are_left_out_of_home_digest_and_ai_candidates() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let model = Profile::default().compile();
        let (a, b) = (ats_job("spotify", "Audio Engineer", "u-a"), ats_job("spotify", "Data Engineer", "u-b"));
        store_all(&conn, &model, vec![board(Source::Lever("spotify".into()), Ok(vec![a.clone(), b.clone()]))]).unwrap();
        conn.execute("UPDATE jobs SET tier = 'strong', llm_score = NULL", []).unwrap();
        db::set_job_status(&conn, &b.id, Some("saved")).unwrap();
        db::record_check(&conn, &a.id, Some("check: HTTP 410")).unwrap();
        db::record_check(&conn, &b.id, Some("check: HTTP 410")).unwrap();

        let home = db::JobFilter { exclude_statuses: db::HOME_HIDDEN_STATUSES, ..Default::default() };
        assert_eq!(db::search_jobs(&conn, &home, "best", 50, 0).unwrap().1, 0);
        assert!(db::new_keyword_digest_matches(&conn, 10).unwrap().is_empty());
        assert!(db::top_for_rescore(&conn, 10, false).unwrap().is_empty());
        // The tracker still shows the saved one, marked closed.
        let tracked = db::pipeline_jobs(&conn).unwrap();
        assert_eq!(tracked.len(), 1);
        assert!(tracked[0].closed_at.is_some());
    }

    #[test]
    fn emptied_watchlist_stays_empty() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        assert!(load_sources(&conn).unwrap().is_empty());
        db::add_company(&conn, "lever", "spotify", "lever:spotify").unwrap();
        assert_eq!(load_sources(&conn).unwrap(), [Source::Lever("spotify".into())]);
    }

    #[test]
    fn quota_exhausted_trips_breaker_at_once_and_forces_fallback() {
        let mut t = LlmTally::default();
        t.ok();
        let quota = anyhow::Error::new(llm::QuotaExhausted { detail: "429".into() });
        assert!(t.fail(&quota), "quota running out is logged");
        assert!(t.should_stop(), "one quota error is enough to stop");
        assert_eq!(t.first_error.as_deref(), Some(LlmTally::QUOTA_MSG));

        let mut s = ScanSummary::default();
        s.set_llm(&cfg(true, Some("k")), &t);
        assert_eq!(s.llm_error.as_deref(), Some("Groq daily quota used up"));
        assert!(s.llm_failed_run(), "quota exhaustion falls back even after some scores");
    }

    #[test]
    fn breaker_trips_after_three_straight_failures() {
        let mut t = LlmTally::default();
        let e = anyhow::anyhow!("Groq returned 404 Not Found: model decommissioned");
        assert!(t.fail(&e), "first failure is reported for full logging");
        assert!(!t.should_stop());
        assert!(!t.fail(&e));
        assert!(!t.should_stop());
        t.fail(&e);
        assert!(t.should_stop());
        assert!(t.tripped);
        assert_eq!(t.first_error.as_deref(), Some("Groq returned 404 Not Found: model decommissioned"));
    }

    #[test]
    fn breaker_stays_closed_once_a_call_succeeds() {
        let mut t = LlmTally::default();
        let e = anyhow::anyhow!("timeout");
        t.ok();
        for _ in 0..5 {
            t.fail(&e);
        }
        assert!(!t.should_stop());
    }

    #[test]
    fn failed_run_only_when_enabled_and_erroring() {
        let failed = LlmTally { failed: 3, first_error: Some("boom".into()), ..Default::default() };

        let mut s = ScanSummary::default();
        s.set_llm(&cfg(true, Some("k")), &failed);
        assert!(s.llm_failed_run());

        // Enabled but no key: can't run at all → still a failure worth flagging.
        let mut s = ScanSummary::default();
        s.set_llm(&cfg(true, None), &LlmTally::default());
        assert!(s.llm_failed_run());
        assert!(s.llm_error.unwrap().contains("GROQ_API_KEY"));

        // Nothing new to score: zero scores but no error → not a failure.
        let mut s = ScanSummary::default();
        s.set_llm(&cfg(true, Some("k")), &LlmTally::default());
        assert!(!s.llm_failed_run());

        // Disabled: never a failure, no error.
        let mut s = ScanSummary::default();
        s.set_llm(&cfg(false, None), &LlmTally::default());
        assert!(!s.llm_failed_run());
        assert!(s.llm_error.is_none());
    }

    #[test]
    fn rescore_all_applies_the_current_profile_and_keeps_llm_tiers() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let mut rust = Job::new("Co", "Rust Engineer", "Remote", "u1", "greenhouse", "rust", None, "{}");
        let mut judged = Job::new("Co", "Rust Developer", "Remote", "u2", "greenhouse", "rust", None, "{}");
        // Stored under a profile that didn't care about Rust.
        let old = Profile::default().compile();
        enrich(&mut rust, &old);
        enrich(&mut judged, &old);
        assert_eq!(rust.tier, "skip");
        db::upsert_job(&conn, &rust).unwrap();
        db::upsert_job(&conn, &judged).unwrap();
        db::set_llm_verdict(&conn, &judged.id, 50, "ok", "").unwrap();
        db::rederive_llm_tiers(&conn, false, old.llm_tiers).unwrap();

        let profile = Profile {
            target_roles: vec!["Rust Engineer".into(), "Rust Developer".into()],
            ..Default::default()
        };
        let s = rescore_all(&conn, &profile.compile()).unwrap();

        let tier = |id: &str| -> String {
            conn.query_row("SELECT tier FROM jobs WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!(s.jobs, 2);
        assert_eq!(s.before.get("skip"), Some(&1));
        assert_eq!(tier(&rust.id), "apply_now");
        // The LLM verdict (50 → maybe) wins over the new keyword tier.
        assert_eq!(tier(&judged.id), "maybe");
        assert_eq!(s.after.get("apply_now"), Some(&1));
        assert_eq!(s.after.get("maybe"), Some(&1));
    }
}
