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

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::profile::Profile;

/// Resolved LLM configuration (profile settings + env-provided key).
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub max_jobs_per_run: i64,
    pub api_key: Option<String>,
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
        }
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

/// Score how well one job fits the profile. Constrained to structured JSON.
pub async fn score_fit(
    cfg: &LlmConfig,
    client: &reqwest::Client,
    profile: &Profile,
    title: &str,
    company: &str,
    description: &str,
) -> Result<FitVerdict> {
    let system = "You are a precise job-fit evaluator. Given a candidate profile \
        and a job posting, judge how well the job fits THIS candidate. Return ONLY \
        JSON: {\"fit_score\": 0-100, \"tier\": \"apply_now|strong|maybe|skip\", \
        \"reasoning\": \"one sentence\", \"gaps\": [\"missing requirement\", ...]}. \
        Base the score on genuine fit to the candidate's target roles, skills, and \
        preferences — not on how prestigious the company is.";

    let user = format!(
        "CANDIDATE PROFILE:\n{}\n\nTarget roles: {}\nStrong skills: {}\nPrefers: {}\n\n\
         JOB:\nTitle: {title}\nCompany: {company}\nDescription:\n{}",
        profile.bio.trim(),
        profile.target_roles.join(", "),
        profile.skills.strong.join(", "),
        profile.preferences.work_modes.join(", "),
        truncate(description, 6000),
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
    let key = cfg.api_key.as_ref().ok_or_else(|| anyhow!("no GROQ_API_KEY set"))?;
    let body = json!({
        "model": cfg.model,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ]
    });

    // Retry on rate-limit (429) / transient 5xx with exponential backoff, so
    // free-tier limits don't cause skipped jobs.
    let mut attempt = 0u32;
    let resp = loop {
        let sent = client
            .post("https://api.groq.com/openai/v1/chat/completions")
            .bearer_auth(key)
            .json(&body)
            .send()
            .await;
        match sent {
            Ok(r) => {
                let status = r.status();
                if (status.as_u16() == 429 || status.is_server_error()) && attempt < 4 {
                    let wait = backoff_secs(&r, attempt);
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_secs_f64(wait)).await;
                    continue;
                }
                if !status.is_success() {
                    // Surface the status + body: Groq explains the failure there
                    // (decommissioned model, bad key, quota), and a bare
                    // "error status" left failures undiagnosable for weeks.
                    let detail = r.text().await.unwrap_or_default();
                    return Err(anyhow!("Groq returned {status}: {}", snippet(&detail, 300)));
                }
                break r;
            }
            Err(e) if attempt < 4 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                let _ = e;
            }
            Err(e) => return Err(anyhow::Error::new(e).context("calling Groq")),
        }
    };

    let v: serde_json::Value = resp.json().await.context("reading Groq response")?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("Groq response had no message content"))?;
    parse_json_content(content)
}

/// Backoff for a rate-limited response: honor `Retry-After` if present, else
/// exponential (1, 2, 4, 8s).
fn backoff_secs(resp: &reqwest::Response, attempt: u32) -> f64 {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|s| s.min(30.0))
        .unwrap_or_else(|| (1u64 << attempt) as f64)
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

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a char boundary at/under max.
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
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
    fn parses_extracted_jobs() {
        let v = parse_json_content(r#"{"jobs":[{"title":"Rust Engineer","location":"Remote"}]}"#).unwrap();
        let jobs: Vec<ExtractedJob> = serde_json::from_value(v.get("jobs").cloned().unwrap()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].title, "Rust Engineer");
    }
}
