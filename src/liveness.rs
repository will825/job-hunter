//! Liveness check: is a stored posting still up?
//!
//! Boards and aggregators don't always drop a filled job, so after each scan
//! the top open jobs Home would show have their URL fetched and are closed
//! (`closed_at`, reason `check: …`) when the page is gone (404/410), redirects
//! to a generic careers/search page, or says the job is closed. The digest
//! runs the same check on each job right before emailing it.
//!
//! Anything inconclusive (timeout, 403, 5xx) leaves the job open. Connections
//! are short-lived and never held across an await, so this runs from the web
//! server's spawned tasks as well as the CLI.

use std::time::Duration;

use anyhow::Result;
use futures::stream::{self, StreamExt};
use reqwest::Url;

use crate::db;

/// Per-request timeout for a liveness fetch.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// How many URLs are checked at once.
const CONCURRENCY: usize = 3;
/// How many of Home's top open jobs a pass looks at.
pub const TOP_N: i64 = 150;
/// Jobs checked more recently than this are skipped by a pass.
const RECHECK_HOURS: i64 = 24;
/// Only this much of a page is read.
const MAX_BODY: usize = 2 * 1024 * 1024;

/// What a page says closed postings look like. Matched against the page's
/// visible text, lowercased with whitespace and apostrophes normalized.
const CLOSED_PHRASES: &[&str] = &[
    "no longer available",
    "position has been filled",
    "job not found",
    "no longer accepting applications",
    "this job has expired",
    "job posting has expired",
    "this job is no longer open",
    "this position is no longer open",
    "this job has been closed",
    "this position has been closed",
];

/// The last path segment of a page a closed job's URL commonly redirects to.
const GENERIC_PAGES: &[&str] = &[
    "careers", "career", "jobs", "job", "search", "job-search", "jobs-search", "search-results",
    "openings", "positions", "opportunities", "vacancies", "join", "join-us", "work-with-us",
];

/// The result of checking one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Open,
    /// Closed, with the reason stored in `closed_reason` (starts "check:").
    Closed(String),
    /// Couldn't tell (network error, 403, 5xx…) — left open.
    Unknown(String),
}

/// Outcome of a liveness pass.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PassSummary {
    pub checked: usize,
    pub closed: usize,
    pub unknown: usize,
}

/// The first closed-posting phrase in a page's visible text, if any.
pub fn closed_phrase(text: &str) -> Option<&'static str> {
    let norm = text.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'");
    let norm = norm.split_whitespace().collect::<Vec<_>>().join(" ");
    CLOSED_PHRASES.iter().copied().find(|p| norm.contains(p))
}

/// Whether a request for `orig` that ended up at `fin` was sent to a generic
/// page instead of the posting: Greenhouse's `?error=true`, a site root, a
/// page named like a careers/search page, a parent of the posting's path
/// (e.g. the board listing), or the same page minus the query that named the
/// job (`/careers?gh_jid=123` → `/careers`).
pub fn generic_redirect(orig: &Url, fin: &Url) -> bool {
    if fin.query_pairs().any(|(k, _)| k == "error") {
        return true;
    }
    let segs = |u: &Url| -> Vec<String> {
        u.path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).map(|p| p.to_lowercase()).collect())
            .unwrap_or_default()
    };
    let (o, f) = (segs(orig), segs(fin));
    if o == f && orig.host_str() == fin.host_str() {
        return orig.query().is_some_and(|q| !q.is_empty()) && fin.query().is_none_or(str::is_empty);
    }
    match f.last() {
        None => true,
        Some(last) if GENERIC_PAGES.contains(&last.as_str()) => true,
        Some(_) => orig.host_str() == fin.host_str() && f.len() < o.len() && o.starts_with(&f),
    }
}

/// Fetch a posting's URL and decide whether it's still open.
pub async fn check_url(client: &reqwest::Client, url: &str) -> Verdict {
    let Ok(orig) = Url::parse(url) else {
        return Verdict::Unknown("not a valid URL".into());
    };
    let mut resp = match client.get(orig.clone()).timeout(CHECK_TIMEOUT).send().await {
        Ok(r) => r,
        Err(e) => return Verdict::Unknown(if e.is_timeout() { "timed out".into() } else { "request failed".into() }),
    };
    let status = resp.status();
    if matches!(status.as_u16(), 404 | 410) {
        return Verdict::Closed(format!("check: HTTP {}", status.as_u16()));
    }
    if generic_redirect(&orig, resp.url()) {
        return Verdict::Closed(format!("check: redirected to {}", resp.url().path()));
    }
    if !status.is_success() {
        return Verdict::Unknown(format!("HTTP {}", status.as_u16()));
    }
    let mut body = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                body.extend_from_slice(&chunk);
                if body.len() >= MAX_BODY {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => return Verdict::Unknown("couldn't read the page".into()),
        }
    }
    let text = crate::text::page_text(&String::from_utf8_lossy(&body));
    match closed_phrase(&text) {
        Some(p) => Verdict::Closed(format!("check: page says \"{p}\"")),
        None => Verdict::Open,
    }
}

/// Check each `(id, url)`, 3 at a time, recording `last_checked_at` and
/// closing the ones found closed. Returns the tally and the closed ids.
pub async fn check_jobs(
    db_path: &str,
    client: &reqwest::Client,
    jobs: Vec<(String, String)>,
) -> Result<(PassSummary, Vec<String>)> {
    let mut summary = PassSummary::default();
    let mut closed_ids = Vec::new();
    let mut results = stream::iter(jobs)
        .map(|(id, url)| async move {
            let verdict = check_url(client, &url).await;
            (id, verdict)
        })
        .buffer_unordered(CONCURRENCY);
    while let Some((id, verdict)) = results.next().await {
        summary.checked += 1;
        let reason = match &verdict {
            Verdict::Closed(r) => Some(r.as_str()),
            Verdict::Unknown(_) => {
                summary.unknown += 1;
                None
            }
            Verdict::Open => None,
        };
        let conn = db::connect(db_path)?;
        if db::record_check(&conn, &id, reason)? {
            summary.closed += 1;
            closed_ids.push(id);
        }
    }
    Ok((summary, closed_ids))
}

/// One pass over the top [`TOP_N`] open jobs Home would show (skipping any
/// checked in the last day). The tally is saved in meta `last_liveness`.
pub async fn run_pass(db_path: &str, client: &reqwest::Client) -> Result<PassSummary> {
    let jobs = {
        let conn = db::connect(db_path)?;
        db::liveness_candidates(&conn, TOP_N, RECHECK_HOURS)?
    };
    let (summary, _) = check_jobs(db_path, client, jobs).await?;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut record = serde_json::to_value(&summary)?;
    record["at"] = at.into();
    db::meta_set(&db::connect(db_path)?, "last_liveness", &record.to_string())?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_phrases_are_found_in_page_text() {
        assert_eq!(closed_phrase("Sorry, this job is  No Longer\nAvailable."), Some("no longer available"));
        assert_eq!(closed_phrase("The position has been filled — thanks!"), Some("position has been filled"));
        assert_eq!(closed_phrase("404: Job not found"), Some("job not found"));
        assert_eq!(
            closed_phrase("We\u{2019}re no longer accepting applications for this role."),
            Some("no longer accepting applications")
        );
        assert_eq!(closed_phrase("Senior Rust Engineer. Apply now! Remote (US)."), None);
        // Only the visible text counts, not bundled UI strings.
        let html = "<script>msg='This job is no longer available'</script><h1>Rust Engineer</h1>";
        assert_eq!(closed_phrase(&crate::text::page_text(html)), None);
    }

    #[test]
    fn redirects_to_generic_pages_are_closed() {
        let u = |s: &str| Url::parse(s).unwrap();
        let gh = u("https://boards.greenhouse.io/acme/jobs/123");
        assert!(generic_redirect(&gh, &u("https://boards.greenhouse.io/acme?error=true")));
        assert!(generic_redirect(&u("https://jobs.lever.co/acme/abc-123"), &u("https://jobs.lever.co/acme")));
        assert!(generic_redirect(&u("https://acme.com/careers/eng-42"), &u("https://acme.com/careers/")));
        assert!(generic_redirect(&u("https://acme.com/job/42"), &u("https://acme.com/")));
        assert!(generic_redirect(&u("https://x.com/a/1"), &u("https://jobs.x.com/search?q=")));
        assert!(generic_redirect(&u("https://acme.com/careers?gh_jid=42"), &u("https://acme.com/careers")));
        // Not generic: no redirect, http→https / trailing slash, or a move to another posting page.
        assert!(!generic_redirect(&gh, &gh));
        assert!(!generic_redirect(&u("http://acme.com/careers?gh_jid=42"), &u("https://acme.com/careers/?gh_jid=42")));
        assert!(!generic_redirect(&u("https://www.adzuna.com/land/ad/99"), &u("https://acme.com/jobs/rust-engineer-99")));
    }
}
