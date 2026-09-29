//! Local web UI for managing watched companies and browsing matches.
//!
//! Runs a small axum server bound to localhost. Endpoints:
//!   GET    /                      the single-page UI
//!   GET    /api/companies         list watched sources
//!   POST   /api/companies {url}   detect + validate + add a source by link
//!   DELETE /api/companies/:id     remove a source
//!   POST   /api/scan              start a full scan in the background (202; 409 if one is running)
//!   GET    /api/scan/status       progress of the current/last web scan
//!   GET    /api/jobs?...          ranked jobs with filters
//!
//! Each handler opens its own SQLite connection (cheap with WAL) so we never
//! share a non-Sync `Connection` across async tasks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::scan_lock::ScanLock;
use crate::{db, detect, llm, pipeline, profile};

#[derive(Clone)]
struct AppState {
    db_path: Arc<String>,
    profile_path: Arc<String>,
    lock_path: Arc<String>,
    client: reqwest::Client,
    scan: Arc<Mutex<ScanStatus>>,
}

/// Progress of the current (or most recent) web scan, polled by the UI.
#[derive(Debug, Default, Clone, Serialize)]
struct ScanStatus {
    running: bool,
    /// Unix seconds.
    started_at: Option<u64>,
    /// "fetching" | "storing" | "ai scoring" | "done" | "failed"
    phase: Option<&'static str>,
    boards_done: usize,
    boards_total: usize,
    last_summary: Option<serde_json::Value>,
    error: Option<String>,
}

impl AppState {
    /// Mutate the scan status. The std mutex is only ever held for this
    /// synchronous closure, never across an await.
    fn update_scan(&self, f: impl FnOnce(&mut ScanStatus)) {
        f(&mut self.scan.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

/// Start the web UI and block serving it.
pub async fn serve(
    db_path: String,
    profile_path: String,
    lock_path: String,
    client: reqwest::Client,
    port: u16,
) -> anyhow::Result<()> {
    let state = AppState {
        db_path: Arc::new(db_path),
        profile_path: Arc::new(profile_path),
        lock_path: Arc::new(lock_path),
        client,
        scan: Arc::new(Mutex::new(ScanStatus::default())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/companies", get(list_companies).post(add_company))
        .route("/api/companies/:id", axum::routing::delete(delete_company))
        .route("/api/scan", post(scan))
        .route("/api/scan/status", get(scan_status))
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/status", post(set_job_status))
        .route("/api/jobs/note", post(set_job_note))
        .route("/api/tracker", get(tracker))
        .route("/api/analytics", get(analytics))
        .route("/api/profile", get(get_profile))
        .route("/api/profile/list", post(edit_profile_list))
        .route("/api/profile/resume", post(upload_resume))
        .route("/api/profile/email", post(set_email))
        .with_state(state);

    // Listen on all interfaces so the dashboard is reachable from other devices
    // on the same network / over Tailscale (not just this Mac). Keep it on your
    // private network — the app has no login.
    let addr = ("0.0.0.0", port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let url = format!("http://127.0.0.1:{port}");
    println!("\n  Job Hunter web UI running:");
    println!("    on this Mac:      {url}");
    println!("    on your network:  http://<this-mac-ip>:{port}");
    println!("  (press Ctrl-C to stop)\n");
    // Only pop a browser when launched interactively on the Mac — not as a
    // background service, and not on the Pi (no `open` command there).
    if cfg!(target_os = "macos") && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }

    axum::serve(listener, app).await?;
    Ok(())
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn list_companies(State(st): State<AppState>) -> Response {
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match db::list_companies(&conn) {
        Ok(list) => Json(json!({
            "companies": list.iter().map(|c| json!({
                "id": c.id, "ats": c.ats, "token": c.token,
                "label": c.label, "added_at": c.added_at,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct AddReq {
    url: String,
}

async fn add_company(State(st): State<AppState>, Json(req): Json<AddReq>) -> Response {
    // Detect + validate the pasted link. This is where an unsupported page
    // produces a clear, user-facing error.
    let source = match detect::detect(&req.url, &st.client).await {
        Ok(s) => s,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match db::add_company(&conn, source.ats(), source.token(), &source.label()) {
        Ok(inserted) => Json(json!({
            "ok": true,
            "inserted": inserted,
            "label": source.label(),
            "message": if inserted {
                format!("Added {}", source.label())
            } else {
                format!("{} is already on your list", source.label())
            }
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn delete_company(State(st): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match db::remove_company(&conn, id) {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "No such company"),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /api/scan`: start a full scan in the background and return 202 right
/// away. The UI polls `/api/scan/status` for progress. 409 if a scan (web or
/// CLI) is already running.
async fn scan(State(st): State<AppState>) -> Response {
    const BUSY: &str = "A scan is already running";
    // Claim the in-process slot first (cheap, and closes the double-click race),
    // then the cross-process lock shared with the CLI scan/digest.
    {
        let mut status = st.scan.lock().unwrap_or_else(|e| e.into_inner());
        if status.running {
            return err(StatusCode::CONFLICT, BUSY);
        }
        let lock = match ScanLock::try_acquire(st.lock_path.as_str()) {
            Ok(Some(lock)) => Arc::new(lock),
            Ok(None) => return err(StatusCode::CONFLICT, BUSY),
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        *status = ScanStatus {
            running: true,
            started_at: Some(unix_now()),
            phase: Some("fetching"),
            last_summary: status.last_summary.take(),
            ..Default::default()
        };
        drop(status);

        let st = st.clone();
        tokio::spawn(async move {
            // Run the scan in its own task so a panic is caught here and still
            // reported as "failed" instead of leaving `running` stuck on.
            let outcome = tokio::spawn(run_web_scan(st.clone(), lock.clone())).await;
            let (summary, error) = match outcome {
                Ok(Ok(summary)) => (Some(summary), None),
                Ok(Err(e)) => (None, Some(format!("{e:#}"))),
                Err(e) => (None, Some(format!("scan task crashed: {e}"))),
            };
            if let Some(e) = &error {
                eprintln!("web scan failed: {e}");
            }
            st.update_scan(|s| {
                s.running = false;
                s.phase = Some(if error.is_some() { "failed" } else { "done" });
                if summary.is_some() {
                    s.last_summary = summary;
                }
                s.error = error;
            });
            drop(lock);
        });
    }

    (StatusCode::ACCEPTED, Json(json!({ "ok": true }))).into_response()
}

/// `GET /api/scan/status`: the current/last web scan's progress.
async fn scan_status(State(st): State<AppState>) -> Response {
    let status = st.scan.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Json(status).into_response()
}

/// The web scan itself: fetch → store + prune → LLM re-score, updating
/// `st.scan` as it goes. Custom pages are left out (the CLI scan handles
/// those). Connections are short-lived and never held across an await.
async fn run_web_scan(st: AppState, lock: Arc<ScanLock>) -> anyhow::Result<serde_json::Value> {
    // Mirror the phase into the lock file so the CLI digest can see it.
    let phase = |p: &'static str| {
        st.update_scan(|s| s.phase = Some(p));
        lock.set_phase("web", p);
    };
    phase("fetching");

    // 1. Load profile + sources, then drop the connection before any await.
    let (model, sources) = {
        let conn = db::connect(&st.db_path)?;
        let profile = profile::load_or_create(&st.profile_path)?;
        let mut sources = pipeline::load_sources(&conn)?;
        sources.extend(pipeline::adzuna_sources(&profile));
        (profile.compile(), sources)
    };
    let sources: Vec<_> = sources.into_iter().filter(|s| !s.is_custom()).collect();
    st.update_scan(|s| s.boards_total = sources.len());

    // 2. Fetch (network only).
    let fetched = pipeline::fetch_all(&st.client, &sources, None, |_| {
        st.update_scan(|s| s.boards_done += 1);
    })
    .await;

    // 3. Store + prune with a fresh connection. Prune runs after store_all, so
    //    every job seen this scan just had its last_seen refreshed.
    phase("storing");
    let mut summary = {
        let conn = db::connect(&st.db_path)?;
        let mut summary = pipeline::store_all(&conn, &model, fetched)?;
        summary.pruned = db::prune_stale(&conn, pipeline::STALE_DAYS)?;
        summary
    };

    // 4. LLM fit-scoring (skips cleanly when unconfigured).
    phase("ai scoring");
    let cfg = profile::load_or_create(&st.profile_path).map(|p| llm::LlmConfig::from_profile(&p))?;
    let tally = match rescore_in_background(st.db_path.clone(), st.profile_path.clone(), st.client.clone()).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("background rescore failed: {e:#}");
            pipeline::LlmTally { first_error: Some(format!("{e:#}")), ..Default::default() }
        }
    };
    summary.set_llm(&cfg, &tally);

    let conn = db::connect(&st.db_path)?;
    if let Err(e) = pipeline::record_last_run(&conn, "web", &summary) {
        eprintln!("couldn't record last_run: {e:#}");
    }
    Ok(pipeline::summary_json("web", &summary))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// LLM re-scoring step of the web "Scan now". Opens its own short-lived
/// connections and never holds one across an await, so the future is `Send` and
/// can be spawned. Scores the top keyword survivors, writes verdicts, and
/// re-derives tiers from the fit scores (mirrors `pipeline::rescore_llm`,
/// including the first-error log and circuit breaker).
async fn rescore_in_background(
    db_path: Arc<String>,
    profile_path: Arc<String>,
    client: reqwest::Client,
) -> anyhow::Result<pipeline::LlmTally> {
    let mut tally = pipeline::LlmTally::default();
    let profile = profile::load_or_create(profile_path.as_str())?;
    let cfg = llm::LlmConfig::from_profile(&profile);
    if !cfg.is_ready() {
        return Ok(tally);
    }
    // 1. Read candidates, then drop the connection before any await.
    let candidates = {
        let conn = db::connect(db_path.as_str())?;
        db::top_for_rescore(&conn, cfg.max_jobs_per_run)?
    };
    if candidates.is_empty() {
        return Ok(tally);
    }
    // 2. Score and persist each verdict as we go. A short-lived connection per
    //    write means no connection is held across an await (keeps the future
    //    Send) and partial progress survives if the task is interrupted.
    for (i, (id, company, title, description)) in candidates.iter().enumerate() {
        if tally.should_stop() {
            eprintln!("{} — skipping the other {} job(s) this run.", tally.stop_reason(), candidates.len() - i);
            break;
        }
        match llm::score_fit(&cfg, &client, &profile, title, company, description).await {
            Ok(v) => {
                let conn = db::connect(db_path.as_str())?;
                db::set_llm_verdict(&conn, id, v.fit_score, &v.reasoning, &v.gaps.join("; "))?;
                tally.ok();
            }
            Err(e) => {
                if tally.fail(&e) {
                    eprintln!("LLM error (first this run) on {title}: {e:#}");
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
    tally.tokens_est = cfg.tokens_used();
    eprintln!(
        "LLM re-scored {} job(s) ({} failed), ~{} prompt tokens (estimate).",
        tally.scored, tally.failed, tally.tokens_est
    );
    // 3. Re-derive tiers from the fit scores once at the end.
    let conn = db::connect(db_path.as_str())?;
    db::rederive_llm_tiers(&conn, profile.compile().hide_onsite)?;
    Ok(tally)
}

async fn jobs(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Response {
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let get = |k: &str| q.get(k).filter(|s| !s.is_empty()).map(|s| s.as_str());
    let limit: i64 = get("limit").and_then(|s| s.parse().ok()).unwrap_or(200);
    // Home is a triage queue: it only shows jobs you HAVEN'T acted on yet, so
    // saving / applying / dismissing a card makes it leave Home ("Tinder for
    // jobs"). Saved/Applied views show only that status.
    let (only_status, exclude_statuses): (Option<&str>, &[&str]) = match get("view") {
        Some("saved") => (Some("saved"), &[]),
        Some("applied") => (Some("applied"), &[]),
        _ => (None, &["dismissed", "applied", "saved"]),
    };
    let sort = get("sort").unwrap_or("best");
    match db::search_jobs(
        &conn,
        get("tier"),
        get("work_mode"),
        get("region"),
        get("seniority"),
        get("q"),
        only_status,
        exclude_statuses,
        sort,
        limit,
    ) {
        Ok(rows) => Json(json!({ "jobs": rows })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct StatusReq {
    id: String,
    status: String, // "saved" | "dismissed" | "active" (clears)
}

async fn set_job_status(State(st): State<AppState>, Json(req): Json<StatusReq>) -> Response {
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let status = match req.status.as_str() {
        "active" | "" => None,
        s => Some(s),
    };
    match db::set_job_status(&conn, &req.id, status) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct NoteReq {
    id: String,
    #[serde(default)]
    note: String,
}

async fn set_job_note(State(st): State<AppState>, Json(req): Json<NoteReq>) -> Response {
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match db::set_job_note(&conn, &req.id, &req.note) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn tracker(State(st): State<AppState>) -> Response {
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match db::pipeline_jobs(&conn) {
        Ok(rows) => Json(json!({ "jobs": rows })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn analytics(State(st): State<AppState>) -> Response {
    let conn = match db::connect(&st.db_path) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let build = || -> anyhow::Result<serde_json::Value> {
        let status = db::status_counts(&conn)?;
        let get = |k: &str| status.get(k).copied().unwrap_or(0);
        let sources = db::source_stats(&conn, 12)?;
        Ok(json!({
            "total": db::count_jobs(&conn)?,
            "tiers": {
                "apply_now": db::count_tier(&conn, "apply_now")?,
                "strong": db::count_tier(&conn, "strong")?,
                "maybe": db::count_tier(&conn, "maybe")?,
                "skip": db::count_tier(&conn, "skip")?,
            },
            "pipeline": {
                "saved": get("saved"), "applied": get("applied"),
                "interviewing": get("interviewing"), "offer": get("offer"),
                "rejected": get("rejected"),
            },
            "sources": sources.iter().map(|(s, total, good)| json!({
                "source": s, "total": total, "good": good
            })).collect::<Vec<_>>(),
        }))
    };
    match build() {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn get_profile(State(st): State<AppState>) -> Response {
    match profile::load_or_create(&st.profile_path) {
        Ok(p) => Json(json!({
            "name": p.name,
            "target_roles": p.target_roles,
            "skills": p.skills.strong,
            "interests": p.interests,
            "email": {
                "enabled": p.email.enabled,
                "from": p.email.from,
                "to": p.email.to,
                // Whether the Resend API key is present (never the value itself).
                "key_set": std::env::var("RESEND_API_KEY").ok().is_some_and(|v| !v.trim().is_empty()),
            },
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct EmailReq {
    enabled: bool,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
}

async fn set_email(State(st): State<AppState>, Json(req): Json<EmailReq>) -> Response {
    match profile::set_email(&st.profile_path, req.enabled, &req.from, &req.to) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

#[derive(Deserialize)]
struct ListEdit {
    field: String, // roles | skills | interests
    value: String,
    #[serde(default)]
    remove: bool,
}

async fn edit_profile_list(State(st): State<AppState>, Json(req): Json<ListEdit>) -> Response {
    match profile::edit_list(&st.profile_path, &req.field, &req.value, req.remove) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

/// Upload a resume (PDF or .txt); extract text, have the LLM turn it into a
/// profile, and merge roles/skills/interests (and bio if empty) into profile.toml.
async fn upload_resume(State(st): State<AppState>, mut multipart: Multipart) -> Response {
    // Collect every uploaded file (supports multiple resumes at once).
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            let filename = field.file_name().unwrap_or("").to_lowercase();
            if let Ok(b) = field.bytes().await {
                if !b.is_empty() {
                    files.push((filename, b.to_vec()));
                }
            }
        }
    }
    if files.is_empty() {
        return err(StatusCode::BAD_REQUEST, "No file received.");
    }

    let profile = match profile::load_or_create(&st.profile_path) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let cfg = llm::LlmConfig::from_profile(&profile);
    if !cfg.is_ready() {
        return err(StatusCode::BAD_REQUEST, format!("Set up your Groq key first — {}", cfg.why_not_ready()));
    }

    let (mut roles, mut skills, mut interests, mut read) = (0usize, 0usize, 0usize, 0usize);
    let mut bio_set = !profile.bio.trim().is_empty();
    let mut failed: Vec<String> = Vec::new();

    for (filename, bytes) in &files {
        let text = match extract_resume_text(filename, bytes) {
            Ok(t) if t.trim().len() >= 30 => t,
            _ => {
                failed.push(filename.clone());
                continue;
            }
        };
        let extract = match llm::extract_profile(&cfg, &st.client, &text).await {
            Ok(e) => e,
            Err(_) => {
                failed.push(filename.clone());
                continue;
            }
        };
        read += 1;
        for r in &extract.roles {
            let _ = profile::edit_list(&st.profile_path, "roles", r, false);
        }
        for s in &extract.skills {
            let _ = profile::edit_list(&st.profile_path, "skills", s, false);
        }
        for i in &extract.interests {
            let _ = profile::edit_list(&st.profile_path, "interests", i, false);
        }
        roles += extract.roles.len();
        skills += extract.skills.len();
        interests += extract.interests.len();
        if !bio_set && !extract.bio.trim().is_empty() {
            let _ = profile::set_bio(&st.profile_path, &extract.bio);
            bio_set = true;
        }
    }

    if read == 0 {
        return err(
            StatusCode::BAD_REQUEST,
            "Couldn't read any of those files — try exporting your resume(s) as PDF.",
        );
    }
    let mut message = format!(
        "Read {} resume(s) — added {roles} role(s), {skills} skill(s), {interests} interest(s).",
        read
    );
    if !failed.is_empty() {
        message.push_str(&format!(" (Skipped {}: couldn't read.)", failed.len()));
    }
    Json(json!({ "ok": true, "message": message })).into_response()
}

/// Extract plain text from an uploaded resume. Handles PDF (default) and .txt.
fn extract_resume_text(filename: &str, bytes: &[u8]) -> anyhow::Result<String> {
    if filename.ends_with(".txt") {
        return Ok(String::from_utf8_lossy(bytes).to_string());
    }
    // PDF — guard against panics from malformed files.
    let buf = bytes.to_vec();
    match std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&buf)) {
        Ok(Ok(text)) => Ok(text),
        _ => Err(anyhow::anyhow!("Couldn't read that file. Please upload a PDF or .txt resume.")),
    }
}

const INDEX_HTML: &str = include_str!("web/index.html");
