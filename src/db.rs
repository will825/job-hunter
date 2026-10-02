//! SQLite storage layer.
//!
//! One `jobs` table, keyed on the stable per-posting `id` (PRIMARY KEY) with a
//! UNIQUE `dedup_key` so the same logical job never appears twice even when two
//! boards both list it. The scoring/status columns from the plan arrive in
//! later phases; what's here is correct storage + dedup.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::models::Job;
use crate::profile::Tiers;

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
        ("locations", "TEXT"),
        ("title_key", "TEXT"),
        ("llm_scored_at", "TEXT"),
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
    // Every row starts out listing just its own location.
    if !existing.contains("locations") {
        conn.execute("UPDATE jobs SET locations = json_array(location) WHERE locations IS NULL", [])?;
    }
    // title_key is computed in Rust (it uses the text normalizers), so backfill
    // row by row. Only rows missing it are touched, so this is a no-op after the
    // first run.
    backfill_title_keys(conn)?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_jobs_title_key ON jobs(title_key)", [])?;
    // Indexes for the Home filters/sorts. Created here (not in init_schema) so
    // they come after the columns they cover exist on upgraded databases.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
         CREATE INDEX IF NOT EXISTS idx_jobs_tier ON jobs(tier);
         CREATE INDEX IF NOT EXISTS idx_jobs_work_mode ON jobs(work_mode);
         CREATE INDEX IF NOT EXISTS idx_jobs_first_seen ON jobs(first_seen);
         CREATE INDEX IF NOT EXISTS idx_jobs_llm_score ON jobs(llm_score);",
    )?;
    Ok(())
}

fn backfill_title_keys(conn: &Connection) -> Result<()> {
    let rows: Vec<(String, String, String)> = conn
        .prepare("SELECT id, company, title FROM jobs WHERE title_key IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for (id, company, title) in rows {
        tx.execute(
            "UPDATE jobs SET title_key = ?2 WHERE id = ?1",
            rusqlite::params![id, crate::text::title_key(&company, &title)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn init_schema(conn: &Connection) -> Result<()> {
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
            status_at    TEXT,
            locations    TEXT,             -- JSON array; >1 entry when merged across cities
            title_key    TEXT,             -- hash of normalized company + title (no location)
            llm_scored_at TEXT             -- when the LLM last scored it (NULL = never/unknown)
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
/// Aggregator sources, which re-list one posting once per city. Only these are
/// merged across locations; ATS boards (greenhouse/lever/ashby) list genuinely
/// separate openings ("Engineer I" vs "II", regional roles) and stay as they are.
const AGGREGATORS: &[&str] = &["adzuna", "himalayas", "remoteok", "remotive", "jobicy"];
/// `AGGREGATORS` as a SQL list, for `source IN (...)`. Fixed text, not input.
const AGGREGATORS_SQL: &str = "('adzuna','himalayas','remoteok','remotive','jobicy')";
/// How recently an aggregator row must have been seen to absorb a new city.
const MERGE_WINDOW_DAYS: i64 = 30;

fn is_aggregator(source: &str) -> bool {
    AGGREGATORS.contains(&source)
}

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

    let title_key = crate::text::title_key(&job.company, &job.title);
    match existing {
        None => {
            // 3. An aggregator re-listing the same posting for another city?
            //    Fold it into the existing row as an extra location.
            if is_aggregator(&job.source) {
                if let Some(target) = aggregator_twin(conn, &title_key)? {
                    add_location(conn, &target, &job.location)?;
                    return Ok(Upsert::MergedDuplicate);
                }
            }
            conn.execute(
                r#"
                INSERT INTO jobs
                    (id, company, title, location, url, source, description,
                     posted_date, raw_json, dedup_key,
                     work_mode, region, seniority, keyword_score, tier, last_seen,
                     locations, title_key)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                        ?11, ?12, ?13, ?14, ?15, datetime('now'),
                        json_array(?4), ?16)
                "#,
                rusqlite::params![
                    job.id, job.company, job.title, job.location, job.url,
                    job.source, job.description, job.posted_date, job.raw_json, job.dedup_key,
                    job.work_mode, job.region, job.seniority, job.keyword_score, job.tier,
                    title_key,
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
                        keyword_score = ?14, tier = ?15,
                        locations = json_array(?5), title_key = ?16
                    WHERE dedup_key = ?1
                    "#,
                    rusqlite::params![
                        job.dedup_key, job.id, job.company, job.title, job.location,
                        job.url, job.source, job.description, job.posted_date, job.raw_json,
                        job.work_mode, job.region, job.seniority, job.keyword_score, job.tier,
                        title_key,
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

/// The aggregator row a new aggregator posting with this `title_key` should
/// merge into: seen in the last [`MERGE_WINDOW_DAYS`], preferring one you've
/// triaged (has a status), else the oldest.
fn aggregator_twin(conn: &Connection, title_key: &str) -> Result<Option<String>> {
    let sql = format!(
        "SELECT id FROM jobs
         WHERE title_key = ?1 AND source IN {AGGREGATORS_SQL}
           AND last_seen >= datetime('now', ?2)
         ORDER BY (status IS NULL), first_seen, id
         LIMIT 1"
    );
    let window = format!("-{MERGE_WINDOW_DAYS} days");
    Ok(conn
        .query_row(&sql, rusqlite::params![title_key, window], |r| r.get(0))
        .optional()?)
}

/// A row's locations as a list (a NULL/garbled column falls back to its
/// single `location`).
fn parse_locations(locations: Option<&str>, location: &str) -> Vec<String> {
    locations
        .and_then(|l| serde_json::from_str::<Vec<String>>(l).ok())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![location.to_string()])
}

/// Add `extra` to `list` unless an equivalent location is already there.
fn merge_location(list: &mut Vec<String>, extra: &str) {
    let key = crate::text::normalize_location(extra);
    if !extra.trim().is_empty() && !list.iter().any(|l| crate::text::normalize_location(l) == key) {
        list.push(extra.trim().to_string());
    }
}

/// Record another city for row `id` and mark it seen now.
fn add_location(conn: &Connection, id: &str, location: &str) -> Result<()> {
    let (locations, own): (Option<String>, String) = conn.query_row(
        "SELECT locations, location FROM jobs WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut list = parse_locations(locations.as_deref(), &own);
    merge_location(&mut list, location);
    conn.execute(
        "UPDATE jobs SET locations = ?2, last_seen = datetime('now') WHERE id = ?1",
        rusqlite::params![id, serde_json::to_string(&list)?],
    )?;
    Ok(())
}

/// Collapse aggregator rows already stored once per city (from before
/// upserts merged them). Groups aggregator rows by `title_key`; the keeper is
/// one you've triaged (has a status) if any, else the oldest. Untriaged
/// duplicates are folded into it — locations, latest last_seen, notified_at
/// and any AI verdict it lacks — then deleted. Rows with a status are never
/// deleted. Returns the number of rows merged away.
pub fn dedupe_aggregators(conn: &Connection) -> Result<usize> {
    let sql = format!(
        "SELECT id, title_key, status IS NOT NULL, location, locations FROM jobs
         WHERE source IN {AGGREGATORS_SQL} AND title_key IS NOT NULL
         ORDER BY title_key, (status IS NULL), first_seen, id"
    );
    let rows: Vec<(String, String, bool, String, Option<String>)> = conn
        .prepare(&sql)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let tx = conn.unchecked_transaction()?;
    let mut merged = 0;
    let mut i = 0;
    while i < rows.len() {
        let group_end = rows[i..].iter().position(|r| r.1 != rows[i].1).map_or(rows.len(), |n| i + n);
        let (keeper, _, _, keeper_loc, keeper_locs) = &rows[i];
        let mut list = parse_locations(keeper_locs.as_deref(), keeper_loc);
        for (dup, _, has_status, loc, locs) in &rows[i + 1..group_end] {
            if *has_status {
                continue; // triaged rows are yours — never delete them
            }
            for l in parse_locations(locs.as_deref(), loc) {
                merge_location(&mut list, &l);
            }
            tx.execute(
                "UPDATE jobs SET
                    last_seen = MAX(last_seen, (SELECT last_seen FROM jobs WHERE id = ?2)),
                    notified_at = COALESCE(notified_at, (SELECT notified_at FROM jobs WHERE id = ?2)),
                    llm_reasoning = CASE WHEN llm_score IS NULL
                        THEN (SELECT llm_reasoning FROM jobs WHERE id = ?2) ELSE llm_reasoning END,
                    llm_gaps = CASE WHEN llm_score IS NULL
                        THEN (SELECT llm_gaps FROM jobs WHERE id = ?2) ELSE llm_gaps END,
                    llm_score = COALESCE(llm_score, (SELECT llm_score FROM jobs WHERE id = ?2))
                 WHERE id = ?1",
                rusqlite::params![keeper, dup],
            )?;
            tx.execute("DELETE FROM jobs WHERE id = ?1", [dup])?;
            merged += 1;
        }
        tx.execute(
            "UPDATE jobs SET locations = ?2 WHERE id = ?1",
            rusqlite::params![keeper, serde_json::to_string(&list)?],
        )?;
        i = group_end;
    }
    tx.commit()?;
    Ok(merged)
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

/// Jobs the LLM scored in the last `days` days (twins that inherited a
/// verdict count too). Rows scored before `llm_scored_at` existed don't count.
pub fn count_llm_scored_since(conn: &Connection, days: i64) -> Result<i64> {
    let cutoff = format!("-{} days", days.max(1));
    let n = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE llm_scored_at >= datetime('now', ?1)",
        [cutoff],
        |r| r.get(0),
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

/// A job to send to the LLM for Stage-2 fit-scoring.
#[derive(Debug, Clone)]
pub struct RescoreCandidate {
    pub id: String,
    pub company: String,
    pub title: String,
    pub description: String,
    pub work_mode: String,
    pub location: String,
    pub seniority: String,
}

/// The candidates for Stage-2 fit-scoring: keyword survivors (tier above skip)
/// not yet LLM-scored, skipping jobs you've dismissed/applied to/been rejected
/// from and (with `hide_onsite`) onsite jobs. At most one job per normalized
/// company + title (`title_key`), so a role listed in ten cities costs one call.
/// Jobs first seen in the last 7 days come first, then best keyword score.
pub fn top_for_rescore(conn: &Connection, limit: i64, hide_onsite: bool) -> Result<Vec<RescoreCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, company, title, description, work_mode, location, seniority FROM (
             SELECT id, company, title, description, work_mode, location, seniority,
                    keyword_score,
                    first_seen >= datetime('now', '-7 days') AS recent,
                    ROW_NUMBER() OVER (
                        PARTITION BY COALESCE(title_key, id)
                        ORDER BY first_seen >= datetime('now', '-7 days') DESC,
                                 keyword_score DESC, id
                    ) AS rn
             FROM jobs
             WHERE tier != 'skip' AND llm_score IS NULL
               AND COALESCE(status, '') NOT IN ('dismissed', 'applied', 'rejected')
               AND NOT (?2 AND work_mode = 'onsite')
         )
         WHERE rn = 1
         ORDER BY recent DESC, keyword_score DESC, id
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit, hide_onsite], |r| {
            Ok(RescoreCandidate {
                id: r.get(0)?,
                company: r.get(1)?,
                title: r.get(2)?,
                description: r.get(3)?,
                work_mode: r.get(4)?,
                location: r.get(5)?,
                seniority: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Store an LLM verdict for a job (score + reasoning). The tier is NOT set here
/// — it's derived from the score by [`rederive_llm_tiers`], the single source of
/// truth, so a job's tier always matches its numeric fit.
pub fn set_llm_verdict(conn: &Connection, id: &str, fit_score: i64, reasoning: &str, gaps: &str) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET llm_score = ?2, llm_reasoning = ?3, llm_gaps = ?4,
                         llm_scored_at = datetime('now')
         WHERE id = ?1",
        rusqlite::params![id, fit_score, reasoning, gaps],
    )?;
    Ok(())
}

/// Copy job `id`'s LLM verdict to its unscored twins: other rows with the same
/// `title_key` (normalized company + title, e.g. one role listed per city) AND
/// the same `work_mode` — a remote and an onsite posting of one title can
/// deserve different scores. Returns how many rows were updated.
pub fn copy_llm_verdict_to_twins(conn: &Connection, id: &str) -> Result<usize> {
    let n = conn.execute(
        "UPDATE jobs SET (llm_score, llm_reasoning, llm_gaps, llm_scored_at) =
             (SELECT llm_score, llm_reasoning, llm_gaps, llm_scored_at FROM jobs WHERE id = ?1)
         WHERE llm_score IS NULL AND id != ?1
           AND (title_key, work_mode) =
               (SELECT title_key, work_mode FROM jobs
                WHERE id = ?1 AND llm_score IS NOT NULL AND title_key IS NOT NULL)",
        [id],
    )?;
    Ok(n)
}

/// Recompute tier from the LLM fit score for every AI-reviewed job. This is the
/// single place tiering happens once a job has been scored — so tiers always
/// reflect the real fit, not the LLM's inconsistent tier label. `tiers` are
/// the fit-score thresholds from profile `[llm]` (default 80/62/40, so only
/// genuinely strong matches reach apply_now/strong and the digest). With
/// `hide_onsite` (profile `onsite_mode = "hide"`), onsite jobs stay "skip"
/// whatever their fit score.
pub fn rederive_llm_tiers(conn: &Connection, hide_onsite: bool, tiers: Tiers) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET tier = CASE
            WHEN ?1 AND work_mode = 'onsite' THEN 'skip'
            WHEN llm_score >= ?2 THEN 'apply_now'
            WHEN llm_score >= ?3 THEN 'strong'
            WHEN llm_score >= ?4 THEN 'maybe'
            ELSE 'skip'
         END
         WHERE llm_score IS NOT NULL",
        rusqlite::params![hide_onsite, tiers.apply_now, tiers.strong, tiers.maybe],
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

/// Job count per tier across the whole DB (tier → count). Ordered so the
/// before/after summaries of a rescore line up.
pub fn tier_counts(conn: &Connection) -> Result<std::collections::BTreeMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT tier, COUNT(*) FROM jobs GROUP BY tier")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Every stored job rebuilt as a [`Job`] from its stored fields, for
/// re-running classification + keyword scoring without a re-fetch. The stored
/// `id` is kept (not recomputed), so updates hit the right row.
pub fn stored_jobs(conn: &Connection) -> Result<Vec<Job>> {
    let mut stmt = conn.prepare(
        "SELECT id, company, title, location, url, source, description, posted_date, raw_json FROM jobs",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let mut job = Job::new(
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
            );
            job.id = r.get(0)?;
            Ok(job)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Write a job's classification + keyword score + tier back to its row.
/// Touches nothing else (not `last_seen`, status, or the LLM verdict).
pub fn set_enrichment(conn: &Connection, job: &Job) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET work_mode = ?2, region = ?3, seniority = ?4, keyword_score = ?5, tier = ?6
         WHERE id = ?1",
        rusqlite::params![job.id, job.work_mode, job.region, job.seniority, job.keyword_score, job.tier],
    )?;
    Ok(())
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
    /// Every city this posting was listed in (always at least `location`).
    pub locations: Vec<String>,
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
                work_mode, region, seniority, llm_score, llm_reasoning, status, note, status_at,
                locations
         FROM jobs
         WHERE status IN ('saved','applied','interviewing','offer','rejected')
         ORDER BY COALESCE(llm_score, keyword_score) DESC",
    )?;
    let rows = stmt
        .query_map([], row_to_jobrow)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Shared row → JobRow mapper for the 17-column job select.
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
        locations: parse_locations(r.get::<_, Option<String>>(16)?.as_deref(), &r.get::<_, String>(3)?),
    })
}

/// One job with everything the card's details pane needs (`GET /api/jobs/:id`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct JobDetail {
    #[serde(flatten)]
    pub row: JobRow,
    pub description: String,
    /// The LLM's gaps, split back out of the stored "; "-joined string.
    pub llm_gaps: Vec<String>,
    /// As the source gave it: ISO date, or epoch seconds/millis.
    pub posted_date: Option<String>,
    pub first_seen: String,
    /// Whole days since `posted_date`, or since `first_seen` when the posted
    /// date is missing or unparseable (`age_from_posted` says which).
    pub age_days: Option<i64>,
    pub age_from_posted: bool,
}

/// Load one job for the details view; `None` if the id isn't stored.
pub fn job_detail(conn: &Connection, id: &str) -> Result<Option<JobDetail>> {
    // posted_date shapes seen across sources: ISO-8601 (with or without time
    // / zone), epoch seconds (Himalayas), epoch millis (Lever), sometimes with
    // stray JSON quotes. julianday() is NULL for anything it can't read.
    let row = conn
        .query_row(
            "SELECT id, company, title, location, source, url, tier, keyword_score,
                    work_mode, region, seniority, llm_score, llm_reasoning, status, note, status_at,
                    locations, description, llm_gaps, posted_date, first_seen,
                    CAST(julianday('now') - CASE
                        WHEN p IS NULL THEN NULL
                        WHEN p NOT GLOB '*[^0-9]*' THEN julianday(
                            CAST(p AS INTEGER) / (CASE WHEN CAST(p AS INTEGER) > 100000000000 THEN 1000 ELSE 1 END),
                            'unixepoch')
                        ELSE julianday(p) END AS INTEGER),
                    CAST(julianday('now') - julianday(first_seen) AS INTEGER)
             FROM (SELECT *, NULLIF(TRIM(posted_date, '\" '), '') AS p FROM jobs WHERE id = ?1)",
            [id],
            |r| {
                let gaps: Option<String> = r.get(18)?;
                let posted_age: Option<i64> = r.get(21)?;
                let seen_age: Option<i64> = r.get(22)?;
                Ok(JobDetail {
                    row: row_to_jobrow(r)?,
                    description: r.get(17)?,
                    llm_gaps: gaps
                        .unwrap_or_default()
                        .split(';')
                        .map(str::trim)
                        .filter(|g| !g.is_empty())
                        .map(String::from)
                        .collect(),
                    posted_date: r.get(19)?,
                    first_seen: r.get(20)?,
                    age_days: posted_age.or(seen_age).map(|d| d.max(0)),
                    age_from_posted: posted_age.is_some(),
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Filters for [`search_jobs`]. `tier`/`work_mode`/`region`/`seniority` are
/// exact matches when `Some`; `q` matches title or company (case-insensitive
/// substring). `hide_onsite` drops onsite jobs unless `work_mode` asks for them;
/// `ai_only` keeps only jobs the LLM has scored.
#[derive(Debug, Default)]
pub struct JobFilter<'a> {
    pub tier: Option<&'a str>,
    pub work_mode: Option<&'a str>,
    pub region: Option<&'a str>,
    pub seniority: Option<&'a str>,
    pub q: Option<&'a str>,
    pub only_status: Option<&'a str>,
    pub exclude_statuses: &'a [&'a str],
    pub hide_onsite: bool,
    pub ai_only: bool,
}

/// Search stored jobs with optional filters, one page at a time.
/// Returns the page plus the total number of matching jobs.
pub fn search_jobs(
    conn: &Connection,
    f: &JobFilter,
    sort: &str,
    limit: i64,
    offset: i64,
) -> Result<(Vec<JobRow>, i64)> {
    let JobFilter { tier, work_mode, region, seniority, q, only_status, exclude_statuses, hide_onsite, ai_only } = *f;
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

    if hide_onsite && work_mode.is_none() {
        clauses.push("work_mode != 'onsite'".to_string());
    }
    if ai_only {
        clauses.push("llm_score IS NOT NULL".to_string());
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
                work_mode, region, seniority, llm_score, llm_reasoning, status, note, status_at,
                locations
         FROM jobs {where_sql}
         ORDER BY {order_by}, id LIMIT ? OFFSET ?"
    );

    let mut param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM jobs {where_sql}"),
        param_refs.as_slice(),
        |r| r.get(0),
    )?;
    param_refs.push(&limit);
    param_refs.push(&offset);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(param_refs.as_slice(), row_to_jobrow)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total))
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

    /// A stored greenhouse job ready for LLM scoring (tier above skip).
    fn candidate(conn: &Connection, company: &str, title: &str, location: &str, url: &str, score: i64) -> Job {
        let mut j = Job::new(company, title, location, url, "greenhouse", "desc", None, "{}");
        j.work_mode = "remote".into();
        j.keyword_score = score;
        j.tier = "strong".into();
        upsert_job(conn, &j).unwrap();
        j
    }

    #[test]
    fn top_for_rescore_takes_one_job_per_company_and_title() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // The same role at one company, listed in three cities (ATS rows don't merge).
        for (i, city) in ["Louisville, KY", "Tampa, FL", "Remote, US"].iter().enumerate() {
            candidate(&conn, "Humana", "Data Engineer", city, &format!("h{i}"), 50 + i as i64);
        }
        candidate(&conn, "Humana, Inc.", "Data  Engineer", "Denver, CO", "h9", 10);
        candidate(&conn, "Splice", "Data Engineer", "Remote", "s1", 40);

        let got = top_for_rescore(&conn, 30, false).unwrap();
        let names: Vec<_> = got.iter().map(|c| (c.company.as_str(), c.location.as_str())).collect();
        assert_eq!(names, [("Humana", "Remote, US"), ("Splice", "Remote")], "best-scoring copy wins");
        assert_eq!(got[0].work_mode, "remote");
    }

    #[test]
    fn top_for_rescore_skips_acted_on_scored_and_hidden_onsite_jobs() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let dismissed = candidate(&conn, "A", "Rust Engineer", "Remote", "a", 90);
        set_job_status(&conn, &dismissed.id, Some("dismissed")).unwrap();
        let applied = candidate(&conn, "B", "Rust Engineer", "Remote", "b", 90);
        set_job_status(&conn, &applied.id, Some("applied")).unwrap();
        let scored = candidate(&conn, "C", "Rust Engineer", "Remote", "c", 90);
        set_llm_verdict(&conn, &scored.id, 70, "ok", "").unwrap();
        let saved = candidate(&conn, "D", "Rust Engineer", "Remote", "d", 20);
        set_job_status(&conn, &saved.id, Some("saved")).unwrap();
        let mut onsite = Job::new("E", "Rust Engineer", "Austin, TX", "e", "greenhouse", "desc", None, "{}");
        onsite.work_mode = "onsite".into();
        onsite.keyword_score = 30;
        onsite.tier = "strong".into();
        upsert_job(&conn, &onsite).unwrap();

        let ids = |hide| -> Vec<String> { top_for_rescore(&conn, 30, hide).unwrap().into_iter().map(|c| c.id).collect() };
        assert_eq!(ids(false), [onsite.id, saved.id.clone()]);
        assert_eq!(ids(true), [saved.id], "onsite hidden when onsite_mode = hide");
    }

    #[test]
    fn verdict_copies_to_same_title_and_work_mode_only() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let judged = candidate(&conn, "Humana", "Data Engineer", "Louisville, KY", "h1", 50);
        let twin = candidate(&conn, "Humana, Inc.", "Data Engineer", "Tampa, FL", "h2", 40);
        let mut onsite = Job::new("Humana", "Data Engineer", "Denver, CO", "h3", "greenhouse", "desc", None, "{}");
        onsite.work_mode = "onsite".into();
        onsite.tier = "strong".into();
        upsert_job(&conn, &onsite).unwrap();
        let other_title = candidate(&conn, "Humana", "Data Analyst", "Remote", "h4", 40);
        let already = candidate(&conn, "Humana", "Data Engineer", "Austin, TX", "h5", 40);
        set_llm_verdict(&conn, &already.id, 30, "own verdict", "").unwrap();

        set_llm_verdict(&conn, &judged.id, 85, "great fit", "k8s").unwrap();
        assert_eq!(copy_llm_verdict_to_twins(&conn, &judged.id).unwrap(), 1);

        let verdict = |id: &str| -> (Option<i64>, Option<String>, Option<String>) {
            conn.query_row("SELECT llm_score, llm_reasoning, llm_gaps FROM jobs WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
        };
        assert_eq!(verdict(&twin.id), (Some(85), Some("great fit".into()), Some("k8s".into())));
        assert_eq!(verdict(&onsite.id).0, None, "different work_mode keeps its own score");
        assert_eq!(verdict(&other_title.id).0, None);
        assert_eq!(verdict(&already.id).0, Some(30), "existing verdicts aren't overwritten");
    }

    #[test]
    fn top_for_rescore_prefers_recent_jobs() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let old = candidate(&conn, "Old", "Rust Engineer", "Remote", "o", 90);
        conn.execute("UPDATE jobs SET first_seen = datetime('now', '-10 days') WHERE id = ?1", [&old.id]).unwrap();
        let new = candidate(&conn, "New", "Rust Engineer", "Remote", "n", 40);
        let ids: Vec<_> = top_for_rescore(&conn, 30, false).unwrap().into_iter().map(|c| c.id).collect();
        assert_eq!(ids, [new.id, old.id]);
    }

    #[test]
    fn hidden_onsite_jobs_stay_skip_after_llm_rederive() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let mut onsite = job("greenhouse", "Rust Engineer", "u1");
        onsite.work_mode = "onsite".into();
        let mut remote = job("greenhouse", "Audio Engineer", "u2");
        remote.work_mode = "remote".into();
        upsert_job(&conn, &onsite).unwrap();
        upsert_job(&conn, &remote).unwrap();
        set_llm_verdict(&conn, &onsite.id, 95, "great", "").unwrap();
        set_llm_verdict(&conn, &remote.id, 95, "great", "").unwrap();
        let tier = |id: &str| -> String {
            conn.query_row("SELECT tier FROM jobs WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };

        let llm_tiers = crate::profile::Profile::default().compile().llm_tiers;
        rederive_llm_tiers(&conn, false, llm_tiers).unwrap();
        assert_eq!(tier(&onsite.id), "apply_now");
        rederive_llm_tiers(&conn, true, llm_tiers).unwrap();
        assert_eq!(tier(&onsite.id), "skip");
        assert_eq!(tier(&remote.id), "apply_now");
    }

    #[test]
    fn llm_tiers_follow_the_given_thresholds() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let j = job("greenhouse", "Rust Engineer", "u1");
        upsert_job(&conn, &j).unwrap();
        set_llm_verdict(&conn, &j.id, 70, "good", "").unwrap();
        let tier = || -> String {
            conn.query_row("SELECT tier FROM jobs WHERE id = ?1", [&j.id], |r| r.get(0)).unwrap()
        };

        rederive_llm_tiers(&conn, false, Tiers { apply_now: 80, strong: 62, maybe: 40 }).unwrap();
        assert_eq!(tier(), "strong");
        rederive_llm_tiers(&conn, false, Tiers { apply_now: 70, strong: 50, maybe: 30 }).unwrap();
        assert_eq!(tier(), "apply_now");
        rederive_llm_tiers(&conn, false, Tiers { apply_now: 95, strong: 85, maybe: 75 }).unwrap();
        assert_eq!(tier(), "skip");
    }

    fn located(company: &str, source: &str, title: &str, location: &str, url: &str) -> Job {
        Job::new(company, title, location, url, source, "desc", None, "{}")
    }

    fn locations_of(conn: &Connection, id: &str) -> Vec<String> {
        let (locs, loc): (Option<String>, String) = conn
            .query_row("SELECT locations, location FROM jobs WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        parse_locations(locs.as_deref(), &loc)
    }

    #[test]
    fn adzuna_city_copies_merge_into_one_row_with_both_locations() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let a = located("Saab", "adzuna", "Senior Software Engineer", "Syracuse, NY", "a1");
        let b = located("Saab, Inc.", "adzuna", "Sr. Software Engineer", "East Syracuse, NY", "a2");
        assert_eq!(upsert_job(&conn, &a).unwrap(), Upsert::Inserted);
        assert_eq!(upsert_job(&conn, &b).unwrap(), Upsert::MergedDuplicate);
        assert_eq!(count_jobs(&conn).unwrap(), 1);
        assert_eq!(locations_of(&conn, &a.id), vec!["Syracuse, NY", "East Syracuse, NY"]);
        // Seeing the same city again doesn't duplicate it.
        assert_eq!(upsert_job(&conn, &b).unwrap(), Upsert::MergedDuplicate);
        assert_eq!(locations_of(&conn, &a.id).len(), 2);
    }

    #[test]
    fn greenhouse_levels_stay_separate_rows() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let one = located("Co", "greenhouse", "Engineer I", "New York", "g1");
        let two = located("Co", "greenhouse", "Engineer II", "New York", "g2");
        assert_eq!(upsert_job(&conn, &one).unwrap(), Upsert::Inserted);
        assert_eq!(upsert_job(&conn, &two).unwrap(), Upsert::Inserted);
        assert_eq!(count_jobs(&conn).unwrap(), 2);
    }

    #[test]
    fn ats_city_variants_are_not_merged() {
        // Same title in two cities on an ATS board = two real openings.
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        upsert_job(&conn, &located("Co", "lever", "Engineer", "Austin", "l1")).unwrap();
        upsert_job(&conn, &located("Co", "lever", "Engineer", "Denver", "l2")).unwrap();
        assert_eq!(count_jobs(&conn).unwrap(), 2);
    }

    #[test]
    fn stale_aggregator_row_is_not_merged_into() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let old = located("Saab", "adzuna", "Engineer", "Syracuse, NY", "a1");
        upsert_job(&conn, &old).unwrap();
        conn.execute("UPDATE jobs SET last_seen = datetime('now', '-31 days')", []).unwrap();
        let new = located("Saab", "adzuna", "Engineer", "Orlando, FL", "a2");
        assert_eq!(upsert_job(&conn, &new).unwrap(), Upsert::Inserted);
    }

    #[test]
    fn dedupe_keeps_the_triaged_row_and_folds_in_the_rest() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // Simulate rows stored before merging existed: insert them directly.
        let cities = ["Syracuse, NY", "Orlando, FL", "Sterling, VA"];
        let jobs: Vec<Job> = cities
            .iter()
            .enumerate()
            .map(|(i, c)| located("Saab", "adzuna", "Senior Software Engineer", c, &format!("u{i}")))
            .collect();
        for (i, j) in jobs.iter().enumerate() {
            conn.execute(
                "INSERT INTO jobs (id, company, title, location, url, source, description, raw_json,
                                   dedup_key, first_seen, locations, title_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'd', '{}', ?7, datetime('now', ?8), json_array(?4), ?9)",
                rusqlite::params![
                    j.id, j.company, j.title, j.location, j.url, j.source, j.dedup_key,
                    format!("-{} days", 10 - i),
                    crate::text::title_key(&j.company, &j.title)
                ],
            )
            .unwrap();
        }
        set_job_status(&conn, &jobs[1].id, Some("saved")).unwrap();
        set_llm_verdict(&conn, &jobs[2].id, 88, "fits", "").unwrap();
        // An unrelated ATS row is left alone.
        upsert_job(&conn, &located("Saab", "greenhouse", "Senior Software Engineer", "Remote", "g")).unwrap();

        assert_eq!(dedupe_aggregators(&conn).unwrap(), 2);
        assert_eq!(count_jobs(&conn).unwrap(), 2);
        let mut locs = locations_of(&conn, &jobs[1].id);
        locs.sort();
        assert_eq!(locs, vec!["Orlando, FL", "Sterling, VA", "Syracuse, NY"]);
        let score: Option<i64> =
            conn.query_row("SELECT llm_score FROM jobs WHERE id = ?1", [&jobs[1].id], |r| r.get(0)).unwrap();
        assert_eq!(score, Some(88), "the keeper inherits an AI verdict it lacked");
        assert_eq!(dedupe_aggregators(&conn).unwrap(), 0, "idempotent");
    }

    #[test]
    fn migrate_backfills_locations_and_title_keys() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO jobs (id, company, title, location, url, source, description, raw_json, dedup_key)
             VALUES ('x', 'Saab', 'Engineer', 'Syracuse', 'u', 'adzuna', 'd', '{}', 'k')",
            [],
        )
        .unwrap();
        migrate(&conn).unwrap();
        let key: Option<String> = conn.query_row("SELECT title_key FROM jobs WHERE id = 'x'", [], |r| r.get(0)).unwrap();
        assert_eq!(key.as_deref(), Some(crate::text::title_key("Saab", "Engineer").as_str()));
        assert_eq!(locations_of(&conn, "x"), vec!["Syracuse"]);
        migrate(&conn).unwrap();
    }

    #[test]
    fn search_jobs_pages_and_filters() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();
        for i in 0..5 {
            let mut j = Job::new("Co", &format!("Engineer {i}"), "Remote", &format!("u{i}"), "greenhouse", "d", None, "{}");
            j.work_mode = if i == 0 { "onsite".into() } else { "remote".into() };
            j.keyword_score = i;
            upsert_job(&conn, &j).unwrap();
        }
        conn.execute("UPDATE jobs SET llm_score = 70 WHERE url IN ('u3', 'u4')", []).unwrap();

        let all = JobFilter::default();
        let (page1, total) = search_jobs(&conn, &all, "best", 2, 0).unwrap();
        let (page2, _) = search_jobs(&conn, &all, "best", 2, 2).unwrap();
        let (page3, _) = search_jobs(&conn, &all, "best", 2, 4).unwrap();
        assert_eq!(total, 5);
        assert_eq!((page1.len(), page2.len(), page3.len()), (2, 2, 1));
        let mut ids: Vec<_> = page1.iter().chain(&page2).chain(&page3).map(|r| r.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 5, "pages must not overlap");

        let no_onsite = JobFilter { hide_onsite: true, ..Default::default() };
        assert_eq!(search_jobs(&conn, &no_onsite, "best", 50, 0).unwrap().1, 4);
        // An explicit work_mode wins over hide_onsite.
        let onsite = JobFilter { hide_onsite: true, work_mode: Some("onsite"), ..Default::default() };
        assert_eq!(search_jobs(&conn, &onsite, "best", 50, 0).unwrap().1, 1);
        let ai = JobFilter { ai_only: true, ..Default::default() };
        let (rows, total) = search_jobs(&conn, &ai, "best", 50, 0).unwrap();
        assert_eq!(total, 2);
        assert!(rows.iter().all(|r| r.llm_score == Some(70)));
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

    #[test]
    fn job_detail_reads_description_gaps_and_age_from_any_date_shape() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let three_days = now - 3 * 86_400 - 60;
        let add = |title: &str, posted: Option<String>| {
            let j = Job::new("Splice", title, "Remote", title, "greenhouse", "Para one.\n\nPara two.", posted, "{}");
            upsert_job(&conn, &j).unwrap();
            j.id
        };
        let secs = add("Secs", Some(three_days.to_string()));
        let millis = add("Millis", Some((three_days * 1000).to_string()));
        let quoted = add("Quoted", Some(format!("\"{three_days}\"")));
        let none = add("None", None);
        let junk = add("Junk", Some("last Tuesday".into()));

        for id in [&secs, &millis, &quoted] {
            let d = job_detail(&conn, id).unwrap().unwrap();
            assert_eq!((d.age_days, d.age_from_posted), (Some(3), true), "{:?}", d.posted_date);
        }
        // No usable posted date: fall back to first_seen (just now).
        for id in [&none, &junk] {
            let d = job_detail(&conn, id).unwrap().unwrap();
            assert_eq!((d.age_days, d.age_from_posted), (Some(0), false));
        }

        conn.execute("UPDATE jobs SET posted_date = date('now', '-10 days') || 'T09:30:00Z' WHERE id = ?1", [&none])
            .unwrap();
        assert_eq!(job_detail(&conn, &none).unwrap().unwrap().age_days, Some(10));

        set_llm_verdict(&conn, &secs, 70, "Good fit", "5y Go; ; Kubernetes ").unwrap();
        let d = job_detail(&conn, &secs).unwrap().unwrap();
        assert_eq!(d.llm_gaps, ["5y Go", "Kubernetes"]);
        assert_eq!(d.description, "Para one.\n\nPara two.");
        assert_eq!(d.row.title, "Secs");

        assert!(job_detail(&conn, "nope").unwrap().is_none());
    }

    #[test]
    fn counts_recent_llm_scores_including_twins() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let a = job("greenhouse", "Rust Engineer", "u1");
        let old = job("greenhouse", "Go Engineer", "u2");
        upsert_job(&conn, &a).unwrap();
        upsert_job(&conn, &old).unwrap();
        assert_eq!(count_llm_scored_since(&conn, 7).unwrap(), 0);

        set_llm_verdict(&conn, &a.id, 70, "ok", "").unwrap();
        set_llm_verdict(&conn, &old.id, 70, "ok", "").unwrap();
        conn.execute("UPDATE jobs SET llm_scored_at = datetime('now', '-8 days') WHERE id = ?1", [&old.id])
            .unwrap();
        assert_eq!(count_llm_scored_since(&conn, 7).unwrap(), 1);
    }
}
