//! LLM layer (Phase 3) — deliberately isolated and removable.
//!
//! Two jobs:
//!   1. `score_fit` — read a job description against your profile and return a
//!      genuine fit score + tier + reasoning (the Stage-2 re-ranker).
//!   2. `extract_jobs` — read a custom careers page's text into structured job
//!      listings (used by the optional custom-page reader).
//!
//! Provider-abstracted (Groq today; Claude/Ollama slot in behind the same
//! `provider` switch). The API key is read from the environment, never stored.
//! If the LLM is disabled or unconfigured, callers simply skip it — nothing
//! errors, and the tool runs fine on keyword scoring alone. To remove the whole
//! feature: delete this file and the two call sites in `pipeline.rs`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::db::RescoreCandidate;
use crate::profile::Profile;

/// Groq's OpenAI-compatible chat-completions endpoint.
const GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";

/// Resolved LLM configuration (profile settings + env-provided key). Built once
/// per run, so it also carries that run's estimated token usage.
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub max_jobs_per_run: i64,
    pub api_key: Option<String>,
    /// Chat-completions endpoint (Groq; overridden in tests).
    pub api_url: String,
    /// Estimated prompt tokens sent by successful calls this run.
    tokens_est: AtomicU64,
}

/// Groq refused because the account's daily quota (tokens or requests per
/// day) is used up. Retrying won't help until it resets, so the caller should
/// stop calling the LLM for the rest of the run.
#[derive(Debug)]
pub struct QuotaExhausted {
    pub detail: String,
}

impl std::fmt::Display for QuotaExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Groq daily quota used up: {}", self.detail)
    }
}

impl std::error::Error for QuotaExhausted {}

/// Whether `e` (anywhere in its chain) is a [`QuotaExhausted`].
pub fn is_quota_exhausted(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<QuotaExhausted>())
}

impl LlmConfig {
    /// Build from the profile, reading the API key from the environment.
    pub fn from_profile(p: &Profile) -> Self {
        LlmConfig {
            enabled: p.llm.enabled,
            provider: p.llm.provider.to_lowercase(),
            model: p.llm.model.clone(),
            max_jobs_per_run: p.llm.max_jobs_per_run,
            api_key: std::env::var("GROQ_API_KEY").ok().filter(|k| !k.trim().is_empty()),
            api_url: GROQ_URL.to_string(),
            tokens_est: AtomicU64::new(0),
        }
    }

    /// A ready-to-use config for tests.
    #[cfg(test)]
    pub fn for_test(enabled: bool, api_key: Option<&str>) -> Self {
        LlmConfig {
            enabled,
            provider: "groq".into(),
            model: "test".into(),
            max_jobs_per_run: 30,
            api_key: api_key.map(String::from),
            api_url: GROQ_URL.to_string(),
            tokens_est: AtomicU64::new(0),
        }
    }

    /// Estimated prompt tokens this run's successful calls have used.
    pub fn tokens_used(&self) -> u64 {
        self.tokens_est.load(Ordering::Relaxed)
    }

    /// Whether the LLM is actually usable right now (enabled + provider + key).
    pub fn is_ready(&self) -> bool {
        self.enabled && self.provider == "groq" && self.api_key.is_some()
    }

    /// A one-line reason the LLM is off, for user-facing messages.
    pub fn why_not_ready(&self) -> &'static str {
        if !self.enabled {
            "LLM disabled in profile.toml ([llm] enabled = false)"
        } else if self.provider != "groq" {
            "LLM provider is not 'groq'"
        } else if self.api_key.is_none() {
            "no GROQ_API_KEY set in the environment"
        } else {
            "ready"
        }
    }
}

/// The model's verdict on how well a job fits you. (The model also returns a
/// `tier`, but we ignore it — tiering is derived from `fit_score` for
/// consistency. serde simply skips the unused JSON field.)
#[derive(Debug, Clone, Deserialize)]
pub struct FitVerdict {
    pub fit_score: i64,
    pub reasoning: String,
    #[serde(default)]
    pub gaps: Vec<String>,
}

/// A job listing extracted from a custom careers page.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedJob {
    pub title: String,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: String,
}

/// A profile extracted from a resume.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileExtract {
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub interests: Vec<String>,
    #[serde(default)]
    pub bio: String,
}

/// Read a resume's text into a job-search profile (one-time call on upload).
pub async fn extract_profile(cfg: &LlmConfig, client: &reqwest::Client, resume_text: &str) -> Result<ProfileExtract> {
    let system = "You build a job-search profile from a resume. Return ONLY JSON: \
        {\"roles\": [job titles this person should search for and apply to — 4 to 8], \
        \"skills\": [their concrete technical skills — 6 to 12], \
        \"interests\": [industries or domains they care about — 3 to 6], \
        \"bio\": \"a 2-sentence summary of who they are and what they're looking for\"}. \
        Keep roles/skills/interests short (1-4 words each), lowercase where natural. \
        Base everything on the resume; do not invent skills they don't show.";
    let user = format!("RESUME:\n{}", truncate(resume_text, 12000));
    let value = groq_json(cfg, client, system, &user).await?;
    let extract: ProfileExtract =
        serde_json::from_value(value).context("resume extract response was not the expected JSON")?;
    Ok(extract)
}

/// Most of a job description sent for fit-scoring, in chars — keeps each call
/// well under Groq free-tier per-minute token limits.
const FIT_DESCRIPTION_CHARS: usize = 4000;

/// Score how well one job fits the profile. Constrained to structured JSON.
/// The prompt carries the job's work mode, location, and seniority plus the
/// candidate's preferred work modes and regions, so the model can judge
/// remote/location fit rather than just skills.
pub async fn score_fit(
    cfg: &LlmConfig,
    client: &reqwest::Client,
    profile: &Profile,
    job: &RescoreCandidate,
) -> Result<FitVerdict> {
    let system = "You are a precise job-fit evaluator. Given a candidate profile \
        and a job posting, judge how well the job fits THIS candidate. Return ONLY \
        JSON: {\"fit_score\": 0-100, \"tier\": \"apply_now|strong|maybe|skip\", \
        \"reasoning\": \"one sentence\", \"gaps\": [\"missing requirement\", ...]}. \
        Base the score on genuine fit to the candidate's target roles, skills, and \
        preferences — not on how prestigious the company is. Location matters: if \
        the job is onsite-only (not remote or hybrid) and that is outside the \
        candidate's preferred work modes or regions, fit_score must be at most 40. \
        If the work mode is \"unknown\", judge it from the location and description.";

    let or_any = |v: &[String]| if v.is_empty() { "any".to_string() } else { v.join(", ") };
    let user = format!(
        "CANDIDATE PROFILE:\n{}\n\nTarget roles: {}\nStrong skills: {}\n\
         Preferred work modes: {}\nPreferred regions: {}\n\n\
         JOB:\nTitle: {}\nCompany: {}\nWork mode: {}\nLocation: {}\nSeniority: {}\n\
         Description:\n{}",
        profile.bio.trim(),
        profile.target_roles.join(", "),
        profile.skills.strong.join(", "),
        or_any(&profile.preferences.work_modes),
        or_any(&profile.preferences.regions),
        job.title,
        job.company,
        job.work_mode,
        job.location,
        job.seniority,
        truncate(&job.description, FIT_DESCRIPTION_CHARS),
    );

    let value = groq_json(cfg, client, system, &user).await?;
    let verdict: FitVerdict = serde_json::from_value(value)
        .context("LLM fit response was not in the expected JSON shape")?;
    Ok(verdict)
}

/// Extract structured job listings from a careers page's visible text.
pub async fn extract_jobs(
    cfg: &LlmConfig,
    client: &reqwest::Client,
    page_url: &str,
    page_text: &str,
) -> Result<Vec<ExtractedJob>> {
    let system = "You extract job listings from careers-page text. Return ONLY JSON: \
        {\"jobs\": [{\"title\": \"...\", \"location\": \"...\", \"url\": \"...\", \
        \"description\": \"...\"}, ...]}. Include every distinct job you can find. \
        If the text contains no job listings, return {\"jobs\": []}.";
    let user = format!(
        "Careers page: {page_url}\n\nPage text:\n{}",
        truncate(page_text, 12000)
    );

    let value = groq_json(cfg, client, system, &user).await?;
    let jobs = value
        .get("jobs")
        .cloned()
        .ok_or_else(|| anyhow!("LLM extract response missing 'jobs'"))?;
    let jobs: Vec<ExtractedJob> =
        serde_json::from_value(jobs).context("LLM extract 'jobs' had the wrong shape")?;
    Ok(jobs)
}

/// POST a chat-completion to Groq's OpenAI-compatible endpoint and parse the
/// assistant's JSON content into a `serde_json::Value`.
async fn groq_json(
    cfg: &LlmConfig,
    client: &reqwest::Client,
    system: &str,
    user: &str,
) -> Result<serde_json::Value> {
    let body = json!({
        "model": cfg.model,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ]
    });
    let resp = groq_send(cfg, client, &body).await?;

    let tokens = estimate_tokens(system, user);
    let total = cfg.tokens_est.fetch_add(tokens, Ordering::Relaxed) + tokens;
    eprintln!("LLM call ~{tokens} prompt tokens (run total ~{total})");

    let v: serde_json::Value = resp.json().await.context("reading Groq response")?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("Groq response had no message content"))?;
    parse_json_content(content)
}

/// Groq answered with a non-success status (after any retries).
#[derive(Debug)]
pub struct GroqStatus {
    pub status: u16,
    message: String,
}

impl std::fmt::Display for GroqStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GroqStatus {}

/// Send one chat-completion request to Groq, returning the successful response.
async fn groq_send(cfg: &LlmConfig, client: &reqwest::Client, body: &serde_json::Value) -> Result<reqwest::Response> {
    let key = cfg.api_key.as_ref().ok_or_else(|| anyhow!("no GROQ_API_KEY set"))?;

    // Retry transient rate limits (429) / 5xx with short backoff, so brief
    // free-tier throttling doesn't skip jobs. The daily quota fails fast with
    // `QuotaExhausted` instead: sleeping through a ~9 min Retry-After would hold
    // the scan lock for hours, while failing lets the breaker trip at once.
    let started = Instant::now();
    let mut attempt = 0u32;
    loop {
        let sent = client
            .post(&cfg.api_url)
            .bearer_auth(key)
            .json(body)
            .send()
            .await;
        match sent {
            Ok(r) => {
                let status = r.status();
                if status.is_success() {
                    return Ok(r);
                }
                let retry_after = r
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after);
                // Surface the status + body: Groq explains the failure there
                // (decommissioned model, bad key, quota), and a bare
                // "error status" left failures undiagnosable for weeks.
                let detail = r.text().await.unwrap_or_default();
                let after = retry_after
                    .map(|s| format!(" (Retry-After {s:.0}s)"))
                    .unwrap_or_default();
                if status.as_u16() == 429 && is_daily_quota(retry_after, &detail) {
                    return Err(anyhow::Error::new(QuotaExhausted {
                        detail: format!("{status}{after}: {}", snippet(&detail, 300)),
                    }));
                }
                match retry_wait(status.as_u16(), retry_after, attempt, started.elapsed()) {
                    Some(wait) => {
                        attempt += 1;
                        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                    }
                    None => {
                        return Err(anyhow::Error::new(GroqStatus {
                            status: status.as_u16(),
                            message: format!("Groq returned {status}{after}: {}", snippet(&detail, 300)),
                        }));
                    }
                }
            }
            Err(e) => match retry_wait(503, None, attempt, started.elapsed()) {
                // Network errors back off like a transient 5xx.
                Some(wait) => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                }
                None => return Err(anyhow::Error::new(e).context("calling Groq")),
            },
        }
    }
}

/// The message shown when Groq doesn't recognise the configured model.
pub fn model_unavailable(model: &str) -> String {
    format!(
        "model {model} is not available — pick a current one from \
         https://console.groq.com/docs/models and set [llm] model in profile.toml"
    )
}

/// Why [`check_model`] failed: a one-line, user-facing `message`, and whether
/// Groq was never reached (`network`: connect error or timeout, no HTTP
/// response), which is worth retrying later — unlike an answer from Groq.
#[derive(Debug, PartialEq)]
pub struct ModelCheckError {
    pub message: String,
    pub network: bool,
}

impl std::fmt::Display for ModelCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Make one tiny Groq request with the configured model, to catch a retired
/// model or bad key before a run depends on it. Assumes `cfg.is_ready()`.
pub async fn check_model(cfg: &LlmConfig, client: &reqwest::Client) -> std::result::Result<(), ModelCheckError> {
    let body = json!({
        "model": cfg.model,
        "max_tokens": 16,
        "messages": [{ "role": "user", "content": "Reply with OK." }]
    });
    match groq_send(cfg, client, &body).await {
        Ok(_) => Ok(()),
        Err(e) => Err(model_check_error(&cfg.model, &e)),
    }
}

fn model_check_error(model: &str, e: &anyhow::Error) -> ModelCheckError {
    let message = match e.downcast_ref::<GroqStatus>() {
        // Groq answers 404 (model_not_found) for a retired model, 400 for an
        // id it doesn't accept.
        Some(s) if s.status == 404 || s.status == 400 => model_unavailable(model),
        Some(s) if s.status == 401 => "Groq rejected GROQ_API_KEY (401 Unauthorized)".to_string(),
        _ => format!("{e:#}"),
    };
    // Only a transport failure carries a reqwest::Error; any HTTP answer from
    // Groq becomes a GroqStatus / QuotaExhausted instead.
    let network = e.chain().any(|c| c.is::<reqwest::Error>());
    ModelCheckError { message, network }
}

/// Retries allowed after the first Groq request.
const MAX_RETRIES: u32 = 4;
/// Longest single wait between retries; a longer Retry-After is clamped.
const MAX_RETRY_WAIT_SECS: f64 = 20.0;
/// Total time one call may spend retrying before it gives up.
const MAX_RETRY_TOTAL: Duration = Duration::from_secs(60);
/// A 429 asking us to wait longer than this is a quota, not a blip.
const QUOTA_RETRY_AFTER_SECS: f64 = 60.0;

/// Whether a 429 means the daily quota is spent: a long Retry-After, or Groq
/// saying so in the body ("tokens per day (TPD)", "requests per day (RPD)").
fn is_daily_quota(retry_after: Option<f64>, body: &str) -> bool {
    let body = body.to_lowercase();
    retry_after.is_some_and(|s| s > QUOTA_RETRY_AFTER_SECS)
        || body.contains("per day")
        || body.contains("(tpd)")
        || body.contains("(rpd)")
}

/// How long to wait before retrying a failed response, or `None` to give up.
/// Only 429 / 5xx are retried: `Retry-After` is honored (clamped to
/// `MAX_RETRY_WAIT_SECS`), else exponential (1, 2, 4, 8s). Gives up after
/// `MAX_RETRIES` or when the wait would push the call past `MAX_RETRY_TOTAL`.
fn retry_wait(status: u16, retry_after: Option<f64>, attempt: u32, elapsed: Duration) -> Option<f64> {
    let retryable = status == 429 || (500..600).contains(&status);
    if !retryable || attempt >= MAX_RETRIES {
        return None;
    }
    let wait = retry_after
        .unwrap_or_else(|| (1u64 << attempt) as f64)
        .min(MAX_RETRY_WAIT_SECS);
    (elapsed + Duration::from_secs_f64(wait) <= MAX_RETRY_TOTAL).then_some(wait)
}

/// Rough prompt size in tokens (~4 chars per token), for tracking daily usage.
fn estimate_tokens(system: &str, user: &str) -> u64 {
    ((system.chars().count() + user.chars().count()) / 4) as u64
}

/// `Retry-After` as delay-seconds (the form Groq sends); HTTP dates are ignored.
fn parse_retry_after(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0)
}

/// Parse the model's content string into JSON, tolerating stray code fences.
fn parse_json_content(content: &str) -> Result<serde_json::Value> {
    let trimmed = content.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    serde_json::from_str(trimmed).context("LLM did not return valid JSON")
}

/// The first `max` characters of `s` (by char, so it never splits UTF-8), trimmed.
pub fn snippet(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

/// The first `max` characters of `s` (by char, so it never splits UTF-8).
fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((end, _)) => &s[..end],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fit_json_with_optional_fences() {
        let content = "```json\n{\"fit_score\": 82, \"tier\": \"strong\", \"reasoning\": \"Rust + audio match\", \"gaps\": [\"5y exp\"]}\n```";
        let v = parse_json_content(content).unwrap();
        let verdict: FitVerdict = serde_json::from_value(v).unwrap();
        assert_eq!(verdict.fit_score, 82);
        assert_eq!(verdict.gaps.len(), 1);
    }

    #[test]
    fn snippet_caps_by_chars() {
        assert_eq!(snippet("  héllo world ", 5), "héllo");
        assert_eq!(snippet("short", 300), "short");
    }

    #[test]
    fn truncate_caps_by_chars() {
        assert_eq!(truncate("héllo", 2), "hé");
        assert_eq!(truncate("héllo", 5), "héllo");
        assert_eq!(truncate(&"é".repeat(5000), FIT_DESCRIPTION_CHARS).chars().count(), 4000);
    }

    #[test]
    fn retry_wait_backs_off_briefly_within_caps() {
        let t0 = Duration::ZERO;
        // Transient 429 / 5xx without Retry-After: exponential backoff.
        assert_eq!(retry_wait(429, None, 0, t0), Some(1.0));
        assert_eq!(retry_wait(503, None, 3, t0), Some(8.0));
        // Short Retry-After is honored; longer ones are clamped to 20s.
        assert_eq!(retry_wait(429, Some(2.5), 0, t0), Some(2.5));
        assert_eq!(retry_wait(429, Some(45.0), 1, t0), Some(20.0));
        // A wait that would push the call past 60s total gives up.
        assert_eq!(retry_wait(429, Some(20.0), 1, Duration::from_secs(40)), Some(20.0));
        assert_eq!(retry_wait(429, Some(20.0), 1, Duration::from_secs(41)), None);
        // Out of attempts, or non-retryable client errors: stop.
        assert_eq!(retry_wait(429, None, MAX_RETRIES, t0), None);
        assert_eq!(retry_wait(401, Some(1.0), 0, t0), None);
    }

    #[test]
    fn daily_quota_is_recognised() {
        assert!(is_daily_quota(Some(540.0), ""));
        assert!(is_daily_quota(None, "Rate limit reached ... on tokens per day (TPD): Limit 200000"));
        assert!(is_daily_quota(None, "on requests per day (RPD)"));
        assert!(!is_daily_quota(Some(2.0), "Rate limit reached ... on tokens per minute (TPM)"));
        assert!(!is_daily_quota(None, ""));
    }

    #[test]
    fn estimates_tokens_from_chars() {
        assert_eq!(estimate_tokens("abcd", "efghijkl"), 3);
    }

    /// Serve one canned HTTP response on a local port; returns its URL.
    async fn one_shot_server(response: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16 * 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
        format!("http://{addr}/v1/chat/completions")
    }

    #[tokio::test]
    async fn long_retry_after_429_is_quota_exhausted_without_sleeping() {
        let body = r#"{"error":{"message":"Rate limit reached"}}"#;
        let resp: &'static str = Box::leak(
            format!(
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 540\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let mut cfg = LlmConfig::for_test(true, Some("test-key"));
        cfg.api_url = one_shot_server(resp).await;
        let started = Instant::now();
        let err = groq_json(&cfg, &reqwest::Client::new(), "sys", "user").await.unwrap_err();
        assert!(is_quota_exhausted(&err), "expected QuotaExhausted, got: {err:#}");
        assert!(started.elapsed() < Duration::from_secs(2), "must not sleep on a daily quota");
        assert_eq!(cfg.tokens_used(), 0, "a refused call uses no tokens");
    }

    #[tokio::test]
    async fn retired_model_fails_the_check_with_a_fix() {
        let body = r#"{"error":{"message":"The model `old-model` does not exist","code":"model_not_found"}}"#;
        let resp: &'static str = Box::leak(
            format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let mut cfg = LlmConfig::for_test(true, Some("test-key"));
        cfg.model = "old-model".into();
        cfg.api_url = one_shot_server(resp).await;
        let err = check_model(&cfg, &reqwest::Client::new()).await.unwrap_err();
        assert!(!err.network, "Groq answered, so it's not a network failure");
        assert_eq!(
            err.message,
            "model old-model is not available — pick a current one from \
             https://console.groq.com/docs/models and set [llm] model in profile.toml"
        );
    }

    #[tokio::test]
    async fn unreachable_groq_is_a_network_failure() {
        // Port 1 on loopback refuses the connection at once.
        let e = reqwest::Client::new().get("http://127.0.0.1:1/").send().await.unwrap_err();
        let err = model_check_error("m", &anyhow::Error::new(e).context("calling Groq"));
        assert!(err.network);
        let status = anyhow::Error::new(GroqStatus { status: 503, message: "Groq returned 503".into() });
        assert!(!model_check_error("m", &status).network);
    }

    #[tokio::test]
    async fn model_check_passes_on_success() {
        let body = r#"{"choices":[{"message":{"content":"OK"}}]}"#;
        let resp: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let mut cfg = LlmConfig::for_test(true, Some("test-key"));
        cfg.api_url = one_shot_server(resp).await;
        assert_eq!(check_model(&cfg, &reqwest::Client::new()).await, Ok(()));
    }

    #[test]
    fn parses_retry_after_seconds() {
        assert_eq!(parse_retry_after(" 537 "), Some(537.0));
        assert_eq!(parse_retry_after("1.25"), Some(1.25));
        assert_eq!(parse_retry_after("-3"), None);
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    #[test]
    fn parses_extracted_jobs() {
        let v = parse_json_content(r#"{"jobs":[{"title":"Rust Engineer","location":"Remote"}]}"#).unwrap();
        let jobs: Vec<ExtractedJob> = serde_json::from_value(v.get("jobs").cloned().unwrap()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].title, "Rust Engineer");
    }
}
