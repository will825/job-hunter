//! SQLite storage layer.
//!
//! One `jobs` table, keyed on the stable per-posting `id` (PRIMARY KEY) with a
//! UNIQUE `dedup_key` so the same logical job never appears twice even when two
//! boards both list it. The scoring/status columns from the plan arrive in
//! later phases; what's here is correct storage + dedup.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::models::Job;

/// One-time setup for the database at `path`: create the schema, apply
/// additive migrations, and seed the default watchlist. Run once at startup
/// (idempotent); everything else uses [`connect`].
pub fn init(path: &str) -> Result<()> {
    let conn = connect(path)?;
    init_schema(&conn)?;
    migrate(&conn)?;
    seed_defaults(&conn)?;
    Ok(())
}

/// Open a connection to an already-initialized database. Cheap and write-free:
/// just sets per-connection pragmas, so it's safe to call per request.
pub fn connect(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL + NORMAL sync: fast, safe enough for a local single-writer app, and
    // easy on disk I/O. busy_timeout rides out a scan's write lock rather than
    // failing with "database is locked".
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(conn)
}

/// The current default-watchlist version. Bump when shipping new default
/// sources, and list the newly-added ones in [`seed_defaults`].
const SEED_VERSION: i64 = 2;

/// Populate the watchlist with default companies, tracked via `PRAGMA
/// user_version` so it's smart about upgrades:
/// - Brand-new DB (v0): add the whole seed list.
/// - Upgrade (v < current): add only the defaults introduced since, so a user's
///   deletions of older defaults are preserved.
/// Uses INSERT OR IGNORE, so nothing is ever duplicated.
fn seed_defaults(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= SEED_VERSION {
        return Ok(());
    }

    let to_add: Vec<crate::sources::Source> = if version == 0 {
        crate::sources::seed() // fresh DB: everything
    } else {
        // Defaults introduced in v2 (added after the first release).
        vec![
            crate::sources::Source::Himalayas("all".to_string()),
            crate::sources::Source::Jobicy("engineering".to_string()),
        ]
    };
    for src in to_add {
        add_company(conn, src.ats(), src.token(), &src.label())?;
    }
    conn.pragma_update(None, "user_version", SEED_VERSION)?;
    Ok(())
}

/// Additively bring an existing database up to the current schema without
/// dropping data. Each column is added only if missing, so upgrading an old
/// `jobs.db` never loses the jobs already stored.
fn migrate(conn: &Connection) -> Result<()> {
    let existing: std::collections::HashSet<String> = conn
        .prepare("PRAGMA table_info(jobs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;

    let wanted: &[(&str, &str)] = &[
        ("last_seen", "TEXT"),
        ("work_mode", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("region", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("seniority", "TEXT NOT NULL DEFAULT 'unknown'"),
        ("keyword_score", "INTEGER NOT NULL DEFAULT 0"),
        ("tier", "TEXT NOT NULL DEFAULT ''"),
        ("llm_score", "INTEGER"),
        ("llm_reasoning", "TEXT"),
        ("llm_gaps", "TEXT"),
        ("notified_at", "TEXT"),
        ("status", "TEXT"),
        ("note", "TEXT"),
        ("status_at", "TEXT"),
    ];
    for (col, decl) in wanted {
        if !existing.contains(*col) {
            conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {col} {decl}"), [])?;
        }
    }
    // Backfill last_seen for rows that predate the column: assume they were last
    // seen when first stored. The next scan refreshes it for anything still live.
    // Only needed the one time the column is added.
    if !existing.contains("last_seen") {
        conn.execute(
            "UPDATE jobs SET last_seen = first_seen WHERE last_seen IS NULL",
            [],
        )?;
    }
    Ok(())
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS jobs (
            id           TEXT PRIMARY KEY,
            company      TEXT NOT NULL,
            title        TEXT NOT NULL,
            location     TEXT NOT NULL,
            url          TEXT NOT NULL,
            source       TEXT NOT NULL,
            description  TEXT NOT NULL,
            posted_date  TEXT,
            first_seen   TEXT NOT NULL DEFAULT (datetime('now')),
            last_seen    TEXT NOT NULL DEFAULT (datetime('now')),
            raw_json     TEXT NOT NULL,
            dedup_key    TEXT NOT NULL,
            work_mode    TEXT NOT NULL DEFAULT 'unknown',
            region       TEXT NOT NULL DEFAULT 'unknown',
            seniority    TEXT NOT NULL DEFAULT 'unknown',
            keyword_score INTEGER NOT NULL DEFAULT 0,
            tier         TEXT NOT NULL DEFAULT '',
            llm_score    INTEGER,
            llm_reasoning TEXT,
            llm_gaps     TEXT,
            notified_at  TEXT,
            status       TEXT,
            note         TEXT,
            status_at    TEXT
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_dedup_key ON jobs(dedup_key);

        -- Small key/value store for app state (e.g. digest baseline).
        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        -- The watchlist: which boards/aggregators to scan. Managed at runtime
        -- (add a company by pasting its careers link; delete from the UI).
        CREATE TABLE IF NOT EXISTS companies (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            ats      TEXT NOT NULL,             -- greenhouse | lever | ashby | remotive | remoteok
            token    TEXT NOT NULL,             -- board token / category
            label    TEXT NOT NULL,             -- display name, e.g. "greenhouse:splice"
            added_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(ats, token)                  -- can't watch the same board twice
        );
        "#,
    )?;
    Ok(())
}

/// One watched source row.
#[derive(Debug, Clone)]
pub struct Company {
    pub id: i64,
    pub ats: String,
    pub token: String,
    pub label: String,
    pub added_at: String,
}

/// All watched companies, newest first.
pub fn list_companies(conn: &Connection) -> Result<Vec<Company>> {
    let mut stmt = conn.prepare(
        "SELECT id, ats, token, label, added_at FROM companies ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Company {
                id: r.get(0)?,
                ats: r.get(1)?,
                token: r.get(2)?,
                label: r.get(3)?,
                added_at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Add a company to the watchlist. Idempotent on (ats, token).
/// Returns true if a new row was inserted, false if it already existed.
pub fn add_company(conn: &Connection, ats: &str, token: &str, label: &str) -> Result<bool> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO companies (ats, token, label) VALUES (?1, ?2, ?3)",
        rusqlite::params![ats, token, label],
    )?;
    Ok(changed > 0)
}

/// Remove a company from the watchlist by id. Returns true if a row was removed.
pub fn remove_company(conn: &Connection, id: i64) -> Result<bool> {
    let changed = conn.execute("DELETE FROM companies WHERE id = ?1", [id])?;
    Ok(changed > 0)
}

/// Outcome of upserting one job, so the caller can report meaningful counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    /// A logically new job (its `dedup_key` was not present).
    Inserted,
    /// The exact same posting we'd already stored (same id) — fields refreshed.
    AlreadySeen,
    /// The same logical job as an existing row from another source/variant;
    /// collapsed into one row (kept whichever source ranks higher).
    MergedDuplicate,
}

/// Source ranking for dedup collisions: prefer a direct ATS apply link over a
/// broad aggregator repost, so the stored URL always goes to the real form.
fn source_priority(source: &str) -> i32 {
    match source {
        "greenhouse" | "lever" | "ashby" => 10, // direct ATS
        _ => 5,                                   // aggregators (added later)
    }
}

/// Insert, refresh, or merge a job, enforcing dedup on `dedup_key`.
///
/// - New `dedup_key`            → INSERT ([`Upsert::Inserted`]).
/// - Same posting (same `id`)   → refresh mutable fields ([`Upsert::AlreadySeen`]).
/// - Same logical job, diff row → keep the higher-priority source, collapse to
///   one row ([`Upsert::MergedDuplicate`]). `first_seen` is always preserved.
pub fn upsert_job(conn: &Connection, job: &Job) -> Result<Upsert> {
    // 1. Same posting already stored (by primary id)? Refresh its mutable
    //    fields. We check `id` FIRST — a posting's normalized `dedup_key` can
    //    drift between runs (e.g. a tweaked location), and if we only looked up
    //    by dedup_key we'd miss the existing row and try to INSERT a duplicate
    //    primary key. We intentionally do NOT change dedup_key here, to avoid a
    //    UNIQUE(dedup_key) collision with some other row.
    let id_exists = conn
        .query_row("SELECT 1 FROM jobs WHERE id = ?1", [&job.id], |_| Ok(()))
        .optional()?
        .is_some();
    if id_exists {
        conn.execute(
            r#"
            UPDATE jobs SET
                location = ?2, url = ?3, description = ?4,
                posted_date = ?5, raw_json = ?6,
                work_mode = ?7, region = ?8, seniority = ?9,
                keyword_score = ?10, tier = ?11,
                last_seen = datetime('now')
            WHERE id = ?1
            "#,
            rusqlite::params![
                job.id, job.location, job.url, job.description,
                job.posted_date, job.raw_json,
                job.work_mode, job.region, job.seniority, job.keyword_score, job.tier,
            ],
        )?;
        return Ok(Upsert::AlreadySeen);
    }

    // 2. Same logical job under a DIFFERENT id (cross-source or near-variant)?
    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT id, source FROM jobs WHERE dedup_key = ?1",
            [&job.dedup_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;

    match existing {
        None => {
            conn.execute(
                r#"
                INSERT INTO jobs
                    (id, company, title, location, url, source, description,
                     posted_date, raw_json, dedup_key,
                     work_mode, region, seniority, keyword_score, tier, last_seen)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                        ?11, ?12, ?13, ?14, ?15, datetime('now'))
                "#,
                rusqlite::params![
                    job.id, job.company, job.title, job.location, job.url,
                    job.source, job.description, job.posted_date, job.raw_json, job.dedup_key,
                    job.work_mode, job.region, job.seniority, job.keyword_score, job.tier,
                ],
            )?;
            Ok(Upsert::Inserted)
        }
        Some((_existing_id, existing_source)) => {
            // Same logical job, different posting/source. Keep the better source.
            if source_priority(&job.source) > source_priority(&existing_source) {
                // Replace the existing row's identity + content with this one,
                // matched by the shared dedup_key. first_seen stays as-is.
                conn.execute(
                    r#"
                    UPDATE jobs SET
                        id = ?2, company = ?3, title = ?4, location = ?5,
                        url = ?6, source = ?7, description = ?8,
                        posted_date = ?9, raw_json = ?10,
                        work_mode = ?11, region = ?12, seniority = ?13,
                        keyword_score = ?14, tier = ?15
                    WHERE dedup_key = ?1
                    "#,
                    rusqlite::params![
                        job.dedup_key, job.id, job.company, job.title, job.location,
                        job.url, job.source, job.description, job.posted_date, job.raw_json,
                        job.work_mode, job.region, job.seniority, job.keyword_score, job.tier,
                    ],
                )?;
            }
            // Whether or not we replaced it, this posting appeared in this scan,
            // so it's still live — refresh last_seen (keeps it from being pruned).
            conn.execute(
                "UPDATE jobs SET last_seen = datetime('now') WHERE dedup_key = ?1",
                [&job.dedup_key],
            )?;
            // Either way, the caller sees this as a collapsed duplicate.
            Ok(Upsert::MergedDuplicate)
        }
    }
}

/// Total number of jobs currently stored (used for the run summary).
pub fn count_jobs(conn: &Connection) -> Result<i64> {
    let n = conn.query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))?;
    Ok(n)
}

/// Delete stale/expired postings: jobs the scan hasn't seen in `max_age_days`
/// (their board stopped listing them — filled or closed). Only untriaged jobs
/// are removed; anything you've saved/applied/dismissed (`status` set) is kept,
/// so your tracker never loses a job even after the posting comes down.
/// Returns how many rows were pruned.
pub fn prune_stale(conn: &Connection, max_age_days: i64) -> Result<usize> {
    let cutoff = format!("-{} days", max_age_days.max(1));
    let n = conn.execute(
        "DELETE FROM jobs
         WHERE status IS NULL
           AND last_seen IS NOT NULL
           AND last_seen < datetime('now', ?1)",
        [cutoff],
    )?;
    Ok(n)
}

// --- Meta key/value (app state) ---

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get::<_, String>(0))
        .optional()?;
    Ok(v)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

// --- Digest (daily email) ---

/// One job for the digest email.
#[derive(Debug, Clone)]
pub struct DigestJob {
    pub id: String,
    pub company: String,
    pub title: String,
    pub url: String,
    pub tier: String,
    pub work_mode: String,
    pub location: String,
    pub score: i64, // llm_score if present, else keyword_score
    pub reasoning: Option<String>,
}

/// New apply-now/strong matches that haven't been emailed yet, best first.
pub fn new_digest_matches(conn: &Connection, limit: i64) -> Result<Vec<DigestJob>> {
    let mut stmt = conn.prepare(
        "SELECT id, company, title, url, tier, work_mode, location,
                COALESCE(llm_score, keyword_score) AS score, llm_reasoning
         FROM jobs
         WHERE tier IN ('apply_now','strong') AND llm_score IS NOT NULL AND notified_at IS NULL
         ORDER BY (tier = 'apply_now') DESC, score DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], row_to_digest)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Fallback for a run where the LLM failed: new apply-now/strong matches by
/// keyword tier alone (no `llm_score` required), excluding onsite roles, best
/// keyword score first — so the digest still goes out when AI scoring is down.
pub fn new_keyword_digest_matches(conn: &Connection, limit: i64) -> Result<Vec<DigestJob>> {
    let mut stmt = conn.prepare(
        "SELECT id, company, title, url, tier, work_mode, location,
                keyword_score AS score, llm_reasoning
         FROM jobs
         WHERE tier IN ('apply_now','strong') AND notified_at IS NULL AND work_mode != 'onsite'
         ORDER BY keyword_score DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], row_to_digest)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn row_to_digest(r: &rusqlite::Row) -> rusqlite::Result<DigestJob> {
    Ok(DigestJob {
        id: r.get(0)?,
        company: r.get(1)?,
        title: r.get(2)?,
        url: r.get(3)?,
        tier: r.get(4)?,
        work_mode: r.get(5)?,
        location: r.get(6)?,
        score: r.get(7)?,
        reasoning: r.get(8)?,
    })
}

/// Mark the given job ids as emailed (so they're never sent again).
pub fn mark_notified(conn: &Connection, ids: &[String]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute("UPDATE jobs SET notified_at = datetime('now') WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    Ok(())
}

/// Mark ALL current apply-now/strong matches as already-notified, without
/// sending anything. Used once to establish a baseline so the first real digest
/// only contains jobs that appear *after* setup. Returns how many were marked.
pub fn baseline_notified(conn: &Connection) -> Result<usize> {
    let n = conn.execute(
        "UPDATE jobs SET notified_at = datetime('now')
         WHERE tier IN ('apply_now','strong') AND notified_at IS NULL",
        [],
    )?;
    Ok(n)
}

/// The top keyword-surviving jobs (tier above skip) that haven't been LLM-scored
/// yet, best keyword score first — the candidates for Stage-2 fit-scoring.
pub fn top_for_rescore(conn: &Connection, limit: i64) -> Result<Vec<(String, String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, company, title, description FROM jobs
         WHERE tier != 'skip' AND llm_score IS NULL
         ORDER BY keyword_score DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Store an LLM verdict for a job (score + reasoning). The tier is NOT set here
/// — it's derived from the score by [`rederive_llm_tiers`], the single source of
/// truth, so a job's tier always matches its numeric fit.
pub fn set_llm_verdict(conn: &Connection, id: &str, fit_score: i64, reasoning: &str, gaps: &str) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET llm_score = ?2, llm_reasoning = ?3, llm_gaps = ?4 WHERE id = ?1",
        rusqlite::params![id, fit_score, reasoning, gaps],
    )?;
    Ok(())
}

/// Recompute tier from the LLM fit score for every AI-reviewed job. This is the
/// single place tiering happens once a job has been scored — so tiers always
/// reflect the real fit, not the LLM's inconsistent tier label. Thresholds are
/// tuned so only genuinely strong matches reach apply_now/strong (and the
/// digest); adjust them here to taste.
pub fn rederive_llm_tiers(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET tier = CASE
            WHEN llm_score >= 80 THEN 'apply_now'
            WHEN llm_score >= 62 THEN 'strong'
            WHEN llm_score >= 40 THEN 'maybe'
            ELSE 'skip'
         END
         WHERE llm_score IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// Counts of jobs by pipeline status (only non-null statuses).
pub fn status_counts(conn: &Connection) -> Result<std::collections::HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM jobs WHERE status IS NOT NULL GROUP BY status")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut m = std::collections::HashMap::new();
    for row in rows {
        let (k, v) = row?;
        m.insert(k, v);
    }
    Ok(m)
}

/// Per-source stats: (source, total jobs, count of apply_now/strong "good" matches),
/// best sources first.
pub fn source_stats(conn: &Connection, limit: i64) -> Result<Vec<(String, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT source, COUNT(*),
                SUM(CASE WHEN tier IN ('apply_now','strong') THEN 1 ELSE 0 END)
         FROM jobs GROUP BY source ORDER BY 3 DESC, 2 DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count of jobs in a given tier across the whole DB.
pub fn count_tier(conn: &Connection, tier: &str) -> Result<i64> {
    let n = conn.query_row("SELECT COUNT(*) FROM jobs WHERE tier = ?1", [tier], |r| r.get(0))?;
    Ok(n)
}

/// A job row for display in the web UI / results view.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JobRow {
    pub id: String,
    pub company: String,
    pub title: String,
    pub location: String,
    pub source: String,
    pub url: String,
    pub tier: String,
    pub keyword_score: i64,
    pub work_mode: String,
    pub region: String,
    pub seniority: String,
    pub llm_score: Option<i64>,
    pub llm_reasoning: Option<String>,
    pub status: Option<String>,
    pub note: Option<String>,
    pub status_at: Option<String>,
}

/// Set (or clear, with `None`) a job's status, recording when it changed.
pub fn set_job_status(conn: &Connection, id: &str, status: Option<&str>) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE jobs SET status = ?2,
             status_at = CASE WHEN ?2 IS NULL THEN NULL ELSE datetime('now') END
         WHERE id = ?1",
        rusqlite::params![id, status],
    )?;
    Ok(changed > 0)
}

/// Set (or clear, with empty) a per-job note.
pub fn set_job_note(conn: &Connection, id: &str, note: &str) -> Result<bool> {
    let note: Option<&str> = if note.trim().is_empty() { None } else { Some(note) };
    let changed = conn.execute(
        "UPDATE jobs SET note = ?2 WHERE id = ?1",
        rusqlite::params![id, note],
    )?;
    Ok(changed > 0)
}

/// All jobs currently in the application pipeline (any tracked stage), best
/// score first — for the Tracker board.
pub fn pipeline_jobs(conn: &Connection) -> Result<Vec<JobRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, company, title, location, source, url, tier, keyword_score,
                work_mode, region, seniority, llm_score, llm_reasoning, status, note, status_at
         FROM jobs
         WHERE status IN ('saved','applied','interviewing','offer','rejected')
         ORDER BY COALESCE(llm_score, keyword_score) DESC",
    )?;
    let rows = stmt
        .query_map([], row_to_jobrow)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Shared row → JobRow mapper for the 16-column job select.
fn row_to_jobrow(r: &rusqlite::Row) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        company: r.get(1)?,
        title: r.get(2)?,
        location: r.get(3)?,
        source: r.get(4)?,
        url: r.get(5)?,
        tier: r.get(6)?,
        keyword_score: r.get(7)?,
        work_mode: r.get(8)?,
        region: r.get(9)?,
        seniority: r.get(10)?,
        llm_score: r.get(11)?,
        llm_reasoning: r.get(12)?,
        status: r.get(13)?,
        note: r.get(14)?,
        status_at: r.get(15)?,
    })
}

/// Search stored jobs with optional filters, best score first.
/// `tier`/`work_mode`/`region`/`seniority` are exact matches when `Some`;
/// `q` matches title or company (case-insensitive substring).
#[allow(clippy::too_many_arguments)]
pub fn search_jobs(
    conn: &Connection,
    tier: Option<&str>,
    work_mode: Option<&str>,
    region: Option<&str>,
    seniority: Option<&str>,
    q: Option<&str>,
    only_status: Option<&str>,
    exclude_statuses: &[&str],
    sort: &str,
    limit: i64,
) -> Result<Vec<JobRow>> {
    // Build the WHERE clause with bound parameters (never string-interpolate q).
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let push = |cond: &str, val: String, clauses: &mut Vec<String>, params: &mut Vec<Box<dyn rusqlite::ToSql>>| {
        clauses.push(cond.to_string());
        params.push(Box::new(val));
    };
    if let Some(v) = tier { push("tier = ?", v.to_string(), &mut clauses, &mut params); }
    if let Some(v) = work_mode { push("work_mode = ?", v.to_string(), &mut clauses, &mut params); }
    if let Some(v) = region { push("region = ?", v.to_string(), &mut clauses, &mut params); }
    if let Some(v) = seniority { push("seniority = ?", v.to_string(), &mut clauses, &mut params); }
    if let Some(v) = q {
        clauses.push("(LOWER(title) LIKE ? OR LOWER(company) LIKE ?)".to_string());
        let like = format!("%{}%", v.to_lowercase());
        params.push(Box::new(like.clone()));
        params.push(Box::new(like));
    }
    if let Some(v) = only_status {
        push("status = ?", v.to_string(), &mut clauses, &mut params);
    }
    if !exclude_statuses.is_empty() {
        let placeholders = exclude_statuses.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        clauses.push(format!("(status IS NULL OR status NOT IN ({placeholders}))"));
        for s in exclude_statuses {
            params.push(Box::new(s.to_string()));
        }
    }

    let where_sql = if clauses.is_empty() { String::new() } else { format!("WHERE {}", clauses.join(" AND ")) };
    // `sort` is a fixed whitelist — never interpolate user text into SQL.
    let order_by = match sort {
        "newest" => "first_seen DESC, COALESCE(llm_score, -1) DESC",
        "company" => "LOWER(company) ASC, COALESCE(llm_score, -1) DESC",
        // default "best": AI-scored jobs first (by fit), then keyword-only.
        _ => "COALESCE(llm_score, -1) DESC, keyword_score DESC",
    };
    let sql = format!(
        "SELECT id, company, title, location, source, url, tier, keyword_score,
                work_mode, region, seniority, llm_score, llm_reasoning, status, note, status_at
         FROM jobs {where_sql}
         ORDER BY {order_by} LIMIT ?"
    );
    params.push(Box::new(limit));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), row_to_jobrow)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The top jobs in a tier, best keyword score first — for the summary preview.
pub fn top_in_tier(conn: &Connection, tier: &str, limit: i64) -> Result<Vec<(String, String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT company, title, work_mode, keyword_score FROM jobs
         WHERE tier = ?1 ORDER BY keyword_score DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![tier, limit], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(source: &str, title: &str, url: &str) -> Job {
        Job::new("Splice", title, "Remote", url, source, "desc", None, "{}")
    }

    #[test]
    fn init_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("job_hunter_init_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jobs.db");
        let path = path.to_str().unwrap();
        init(path).unwrap();
        let conn = connect(path).unwrap();
        let companies = list_companies(&conn).unwrap().len();
        assert!(companies > 0, "first init should seed the watchlist");
        assert_eq!(upsert_job(&conn, &job("greenhouse", "Rust Engineer", "u1")).unwrap(), Upsert::Inserted);
        drop(conn);

        init(path).unwrap();
        let conn = connect(path).unwrap();
        assert_eq!(list_companies(&conn).unwrap().len(), companies, "second init must not re-seed");
        assert_eq!(count_jobs(&conn).unwrap(), 1, "second init must keep stored jobs");
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_repost_is_already_seen() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(upsert_job(&conn, &job("greenhouse", "Rust Engineer", "u1")).unwrap(), Upsert::Inserted);
        assert_eq!(upsert_job(&conn, &job("greenhouse", "Rust Engineer", "u1")).unwrap(), Upsert::AlreadySeen);
        assert_eq!(count_jobs(&conn).unwrap(), 1);
    }

    #[test]
    fn same_title_across_sources_merges_to_one_row() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // Same logical job found on two different boards → one row.
        assert_eq!(upsert_job(&conn, &job("lever", "Rust Engineer", "u1")).unwrap(), Upsert::Inserted);
        assert_eq!(upsert_job(&conn, &job("greenhouse", "Rust Engineer", "u2")).unwrap(), Upsert::MergedDuplicate);
        assert_eq!(count_jobs(&conn).unwrap(), 1);
    }

    #[test]
    fn same_id_different_location_refreshes_without_crashing() {
        // Regression: same company+title+url (→ same id) but a different
        // location (→ different dedup_key) must refresh the existing row, not
        // try to INSERT a duplicate primary key.
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let j1 = Job::new("Co", "Rust Engineer", "New York", "http://x", "greenhouse", "d", None, "{}");
        let j2 = Job::new("Co", "Rust Engineer", "Remote - US", "http://x", "greenhouse", "d", None, "{}");
        assert_eq!(j1.id, j2.id, "same company+title+url should share an id");
        assert_ne!(j1.dedup_key, j2.dedup_key, "different location should differ the dedup_key");
        assert_eq!(upsert_job(&conn, &j1).unwrap(), Upsert::Inserted);
        assert_eq!(upsert_job(&conn, &j2).unwrap(), Upsert::AlreadySeen);
        assert_eq!(count_jobs(&conn).unwrap(), 1);
    }

    #[test]
    fn distinct_levels_stay_separate_rows() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // Conservative dedup: "I" and "II" are different postings, both kept.
        assert_eq!(upsert_job(&conn, &job("greenhouse", "Rust Engineer I", "u1")).unwrap(), Upsert::Inserted);
        assert_eq!(upsert_job(&conn, &job("greenhouse", "Rust Engineer II", "u2")).unwrap(), Upsert::Inserted);
        assert_eq!(count_jobs(&conn).unwrap(), 2);
    }

    #[test]
    fn higher_priority_source_wins_the_url() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // Pretend an aggregator (low priority) found it first.
        assert_eq!(upsert_job(&conn, &job("adzuna", "Rust Engineer", "agg-url")).unwrap(), Upsert::Inserted);
        // Then the direct ATS finds the same job.
        assert_eq!(upsert_job(&conn, &job("greenhouse", "Rust Engineer", "ats-url")).unwrap(), Upsert::MergedDuplicate);
        assert_eq!(count_jobs(&conn).unwrap(), 1);
        let url: String = conn.query_row("SELECT url FROM jobs", [], |r| r.get(0)).unwrap();
        assert_eq!(url, "ats-url", "direct ATS link should replace the aggregator link");
    }

    #[test]
    fn keyword_fallback_picks_unscored_new_non_onsite_best_first() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let add = |title: &str, tier: &str, mode: &str, score: i64| {
            let mut j = job("greenhouse", title, title);
            j.tier = tier.into();
            j.work_mode = mode.into();
            j.keyword_score = score;
            upsert_job(&conn, &j).unwrap();
        };
        add("Rust Engineer", "strong", "remote", 20);
        add("Platform Engineer", "apply_now", "hybrid", 35);
        add("Staff Engineer", "apply_now", "onsite", 50); // onsite: excluded
        add("Data Engineer", "maybe", "remote", 10); // below strong: excluded
        add("ML Engineer", "strong", "remote", 25);
        mark_notified(&conn, &[job("greenhouse", "ML Engineer", "ML Engineer").id]).unwrap(); // already sent

        // The normal digest query needs an LLM score, so it finds nothing...
        assert!(new_digest_matches(&conn, 10).unwrap().is_empty());
        // ...while the fallback returns keyword matches, best score first.
        let titles: Vec<String> =
            new_keyword_digest_matches(&conn, 10).unwrap().into_iter().map(|j| j.title).collect();
        assert_eq!(titles, ["Platform Engineer", "Rust Engineer"]);
        assert_eq!(new_keyword_digest_matches(&conn, 1).unwrap().len(), 1);
    }
}
