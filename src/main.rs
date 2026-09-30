//! Job Hunter v2.
//!
//! Fetches jobs from watched company boards (Greenhouse, Lever, Ashby) and free
//! aggregators (Remotive, RemoteOK), dedups, classifies, and scores each job
//! against your profile, storing everything in local SQLite.
//!
//! Commands:
//!   cargo run                      scan all watched boards + print a summary
//!   cargo run -- serve             launch the web UI (manage companies, browse)
//!   cargo run -- add <url>         watch a company board by pasting its link
//!   cargo run -- list              list watched companies
//!   cargo run -- remove <id>       stop watching a company (id from `list`)
//!   cargo run -- rescore           re-score every stored job against the current profile
//!
//! Robustness is deliberate: one board failing (bad token, network blip, API
//! drift) logs a warning and the run continues — it never aborts the whole scan.

mod classify;
mod custom_page;
mod db;
mod detect;
mod email;
mod fetchers;
mod llm;
mod models;
mod pipeline;
mod profile;
mod scan_lock;
mod score;
mod server;
mod sources;
mod text;

use std::time::Duration;

use anyhow::{anyhow, Result};

const DB_PATH: &str = "jobs.db";
const PROFILE_PATH: &str = "profile.toml";
/// Held by any running scan (CLI scan/digest or web), so two never overlap.
const SCAN_LOCK_PATH: &str = "jobhunter.scan.lock";
/// How long the CLI waits for another scan to finish before giving up.
const SCAN_LOCK_WAIT: Duration = Duration::from_secs(10 * 60);
const WEB_PORT: u16 = 8787;

const ENV_FILE: &str = ".env";

/// Subcommands `main` dispatches; anything else is rejected before the DB is touched.
const KNOWN_COMMANDS: &[&str] = &["serve", "add", "list", "remove", "digest", "dedupe", "rescore"];

#[tokio::main]
async fn main() -> Result<()> {
    // Load API keys from the private `.env` file so the user has one obvious
    // place to paste keys (no shell knowledge needed).
    load_env_file(ENV_FILE);

    let client = reqwest::Client::builder()
        .user_agent("job-hunter/0.1 (personal job aggregator)")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str);
    if let Some(other) = command.filter(|c| !KNOWN_COMMANDS.contains(c)) {
        return Err(anyhow!(
            "unknown command '{other}'. Use: (no args) | serve | add <url> | list | remove <id> | digest | dedupe | rescore"
        ));
    }

    // Schema, migrations and seeding happen once here; every later connection
    // (CLI or per web request) is a plain `db::connect`.
    db::init(DB_PATH)?;

    match command {
        Some("serve") => {
            server::serve(
                DB_PATH.to_string(),
                PROFILE_PATH.to_string(),
                SCAN_LOCK_PATH.to_string(),
                client,
                WEB_PORT,
            )
            .await
        }
        Some("add") => cmd_add(&client, args.get(1)).await,
        Some("list") => cmd_list(),
        Some("remove") => cmd_remove(args.get(1)),
        Some("digest") => cmd_digest(&client).await,
        Some("dedupe") => cmd_dedupe().await,
        Some("rescore") => cmd_rescore().await,
        Some(other) => unreachable!("unknown command '{other}' rejected above"),
        None => cmd_scan(&client).await,
    }
}

/// Load `KEY=value` lines from a `.env` file into the environment. Lines that
/// are blank, commented (`#`), or already set in the real environment are
/// skipped. Values may be optionally quoted. Missing file = no-op.
fn load_env_file(path: &str) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'').trim();
            if !key.is_empty() && !value.is_empty() && std::env::var_os(key).is_none() {
                std::env::set_var(key, value);
            }
        }
    }
}

/// Default command: scan every watched board and print a summary.
async fn cmd_scan(client: &reqwest::Client) -> Result<()> {
    let _lock = scan_lock::ScanLock::acquire_waiting(SCAN_LOCK_PATH, SCAN_LOCK_WAIT).await?;
    let conn = db::connect(DB_PATH)?;
    let profile = profile::load_or_create(PROFILE_PATH)?;
    if !profile.name.is_empty() {
        println!("Profile: {} — {} target role(s)\n", profile.name, profile.target_roles.len());
    }

    println!("Scanning...\n");
    let summary = pipeline::full_scan(&conn, client, &profile, |line| println!("  {line}")).await?;
    record_last_run(&conn, "scan", &summary);

    println!("\nSummary");
    println!("  Boards scanned:   {}", summary.boards_scanned);
    println!("  Boards failed:    {}", summary.failures.len());
    println!("  Jobs fetched:     {}", summary.total_fetched);
    println!("  New this run:     {}", summary.inserted);
    println!("  Already seen:     {}", summary.already_seen);
    println!("  Duplicates merged:{}", summary.merged);
    if summary.store_failed > 0 {
        println!("  Failed to store:  {}", summary.store_failed);
    }
    println!("  Total in DB:      {}", summary.total_in_db);
    if summary.llm_enabled {
        println!("  AI scored/failed: {}/{}", summary.llm_scored, summary.llm_failed);
    }
    if summary.llm_enabled {
        println!("  AI tokens (est.): ~{}", summary.llm_tokens_est);
    }
    if let Some(e) = &summary.llm_error {
        println!("  AI error:         {e}");
    }

    if !summary.failures.is_empty() {
        println!("\nFailures (run continued past these):");
        for (label, e) in &summary.failures {
            println!("  • {label}: {e}");
        }
    }

    println!("\nTiers (whole DB):");
    for tier in ["apply_now", "strong", "maybe", "skip"] {
        println!("  {:<10} {}", tier, db::count_tier(&conn, tier)?);
    }
    for (label, tier) in [("APPLY NOW", "apply_now"), ("STRONG", "strong")] {
        let rows = db::top_in_tier(&conn, tier, 10)?;
        if !rows.is_empty() {
            println!("\n{label}:");
            for (company, title, work_mode, score) in rows {
                println!("  • [{score:>3}] {company} — {title}  ({work_mode})");
            }
        }
    }
    Ok(())
}

/// `digest`: the daily run — scan, then email only the NEW apply-now/strong
/// matches since last time (marking them so they're never re-sent).
async fn cmd_digest(client: &reqwest::Client) -> Result<()> {
    // If a web scan is already down to AI scoring, its fetch is done and the DB
    // is fresh — don't wait on the LLM (which can be slow or out of quota); send
    // from what's stored so the email still goes out.
    let lock = scan_lock::ScanLock::acquire_waiting_unless(
        SCAN_LOCK_PATH,
        SCAN_LOCK_WAIT,
        scan_lock::HolderInfo::is_web_ai_scoring,
    )
    .await?;
    let conn = db::connect(DB_PATH)?;
    let profile = profile::load_or_create(PROFILE_PATH)?;
    let ecfg = email::EmailConfig::from_profile(&profile);

    let (summary, from_db_only) = match lock {
        scan_lock::Acquired::Lock(_lock) => {
            println!("Daily digest: scanning…");
            let summary = pipeline::full_scan(&conn, client, &profile, |line| println!("  {line}")).await?;
            record_last_run(&conn, "digest", &summary);
            (summary, false)
        }
        scan_lock::Acquired::Skipped => {
            println!("Daily digest: a web scan is AI-scoring — skipping the fetch and sending from stored jobs.");
            let cfg = llm::LlmConfig::from_profile(&profile);
            let summary = pipeline::ScanSummary { llm_enabled: cfg.enabled, ..Default::default() };
            (summary, true)
        }
    };

    // First-ever run: baseline existing matches so the first email isn't a blast
    // of everything already in the DB — only jobs appearing *after* setup count.
    if db::meta_get(&conn, "digest_baselined")?.as_deref() != Some("1") {
        let n = db::baseline_notified(&conn)?;
        db::meta_set(&conn, "digest_baselined", "1")?;
        println!("\nFirst digest run — baselined {n} existing match(es).");
        println!("From now on, digests email only jobs that appear after this point.");
        return Ok(());
    }

    // Normal digests need an AI score. If the LLM is on but failed this run,
    // fall back to keyword-tier matches (with a banner) rather than going silent.
    let fallback = summary.llm_failed_run();
    let limit = ecfg.max_jobs.max(1);
    let jobs = if fallback {
        println!("\nAI scoring failed ({}) — falling back to keyword matches.",
            summary.llm_error.as_deref().unwrap_or("unknown error"));
        db::new_keyword_digest_matches(&conn, limit)?
    } else {
        db::new_digest_matches(&conn, limit)?
    };
    let notes = email::DigestNotes {
        ai_failure: if fallback { summary.llm_error.clone() } else { None },
        health: email::RunHealth {
            boards_scanned: summary.boards_scanned,
            boards_failed: summary.boards_failed,
            llm_enabled: summary.llm_enabled,
            llm_scored: summary.llm_scored,
            llm_failed: summary.llm_failed,
            from_db_only,
        },
    };
    if jobs.is_empty() {
        println!("\nNo new apply-now/strong matches since your last digest. Nothing to send.");
        return Ok(());
    }

    if ecfg.is_ready() {
        email::send_digest(&ecfg, client, &jobs, &notes).await?;
        let ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
        db::mark_notified(&conn, &ids)?;
        println!("\nEmailed {} new match(es) to {}.", jobs.len(), ecfg.to);
    } else {
        // Email not set up yet: show what *would* be sent, don't mark notified.
        let (subject, _html, text) = email::compose(&jobs, &notes);
        println!("\n(Email off: {} — showing the digest instead)\n", ecfg.why_not_ready());
        println!("Subject: {subject}\n{text}");
        println!("(Set up [email] + EMAIL_APP_PASSWORD to have this emailed to you.)");
    }
    Ok(())
}

/// Save the run summary for the web UI. A failure here is logged, never fatal —
/// it mustn't stop a digest from going out.
fn record_last_run(conn: &rusqlite::Connection, trigger: &str, summary: &pipeline::ScanSummary) {
    if let Err(e) = pipeline::record_last_run(conn, trigger, summary) {
        eprintln!("warning: couldn't record last_run: {e:#}");
    }
}

/// `add <url>`: detect + validate a pasted board link and watch it.
async fn cmd_add(client: &reqwest::Client, url: Option<&String>) -> Result<()> {
    let url = url.ok_or_else(|| anyhow!("usage: job_hunter add <careers-page-or-board-url>"))?;
    println!("Detecting board at {url} …");
    let source = detect::detect(url, client).await?;
    let conn = db::connect(DB_PATH)?;
    let inserted = db::add_company(&conn, source.ats(), source.token(), &source.label())?;
    if inserted {
        println!("✓ Added {}", source.label());
    } else {
        println!("• {} was already on your list", source.label());
    }
    if source.is_custom() {
        println!(
            "  (This is a custom page. It's read by the optional page reader —\n\
             \x20  turn it on with [custom_pages] enabled = true in profile.toml, and set up Groq.)"
        );
    }
    Ok(())
}

/// `dedupe`: collapse aggregator postings stored once per city into one row
/// each (newer scans merge them on the way in). Holds the scan lock so it
/// can't race a scan's writes.
async fn cmd_dedupe() -> Result<()> {
    let _lock = scan_lock::ScanLock::acquire_waiting(SCAN_LOCK_PATH, SCAN_LOCK_WAIT).await?;
    let conn = db::connect(DB_PATH)?;
    let before = db::count_jobs(&conn)?;
    let merged = db::dedupe_aggregators(&conn)?;
    println!("Merged {merged} duplicate aggregator row(s) ({before} → {} jobs).", db::count_jobs(&conn)?);
    Ok(())
}

/// `rescore`: re-run classification + keyword scoring on every stored job with
/// the current profile, so scoring/profile changes apply without a re-fetch.
/// Holds the scan lock so a scan can't overwrite it with its older model.
async fn cmd_rescore() -> Result<()> {
    let _lock = scan_lock::ScanLock::acquire_waiting(SCAN_LOCK_PATH, SCAN_LOCK_WAIT).await?;
    let conn = db::connect(DB_PATH)?;
    let model = profile::load_or_create(PROFILE_PATH)?.compile();
    let s = pipeline::rescore_all(&conn, &model)?;
    println!("Re-scored {} job(s) with the current profile.\n", s.jobs);
    println!("  {:<10} {:>7} {:>7}", "tier", "before", "after");
    for tier in ["apply_now", "strong", "maybe", "skip"] {
        let count = |m: &std::collections::BTreeMap<String, i64>| m.get(tier).copied().unwrap_or(0);
        println!("  {:<10} {:>7} {:>7}", tier, count(&s.before), count(&s.after));
    }
    Ok(())
}

/// `list`: show watched companies.
fn cmd_list() -> Result<()> {
    let conn = db::connect(DB_PATH)?;
    let companies = db::list_companies(&conn)?;
    println!("Watching {} board(s):\n", companies.len());
    for c in companies {
        println!("  [{:>3}] {:<11} {}", c.id, c.ats, c.token);
    }
    Ok(())
}

/// `remove <id>`: stop watching a company.
fn cmd_remove(id: Option<&String>) -> Result<()> {
    let id: i64 = id
        .ok_or_else(|| anyhow!("usage: job_hunter remove <id>  (see `list`)"))?
        .parse()
        .map_err(|_| anyhow!("id must be a number (see `list`)"))?;
    let conn = db::connect(DB_PATH)?;
    if db::remove_company(&conn, id)? {
        println!("✓ Removed company #{id}");
    } else {
        println!("• No company with id #{id}");
    }
    Ok(())
}
