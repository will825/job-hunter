//! Ashby fetcher.
//!
//! Endpoint: `https://api.ashbyhq.com/posting-api/job-board/{token}`
//! Public, no auth. Returns `{ "jobs": [ ... ] }`. Ashby provides both
//! `descriptionPlain` and `descriptionHtml`; we take the plain one.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::html_to_text;

/// Fetches jobs from one Ashby board, identified by its token (e.g. `elevenlabs`).
pub struct AshbyFetcher {
    token: String,
}

impl AshbyFetcher {
    pub fn new(token: impl Into<String>) -> Self {
        AshbyFetcher { token: token.into() }
    }

    fn url(&self) -> String {
        format!("https://api.ashbyhq.com/posting-api/job-board/{}", self.token)
    }
}

#[derive(Debug, Deserialize)]
struct AshbyResponse {
    #[serde(default)]
    jobs: Vec<AshbyJob>,
}

#[derive(Debug, Deserialize)]
struct AshbyJob {
    title: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(rename = "jobUrl", default)]
    job_url: Option<String>,
    #[serde(rename = "applyUrl", default)]
    apply_url: Option<String>,
    #[serde(rename = "descriptionPlain", default)]
    description_plain: Option<String>,
    #[serde(rename = "descriptionHtml", default)]
    description_html: Option<String>,
    #[serde(rename = "publishedAt", default)]
    published_at: Option<String>,
}

impl Fetcher for AshbyFetcher {
    fn source_name(&self) -> &'static str {
        "ashby"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let url = self.url();
        let body = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting Ashby board {}", self.token))?
            .error_for_status()
            .with_context(|| format!("Ashby board {} returned an error status", self.token))?
            .text()
            .await
            .with_context(|| format!("reading Ashby response for {}", self.token))?;

        let parsed: AshbyResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing Ashby JSON for {}", self.token))?;
        let raw_arr: serde_json::Value = serde_json::from_str(&body)?;
        let raw_arr = raw_arr
            .get("jobs")
            .and_then(|j| j.as_array())
            .cloned()
            .unwrap_or_default();

        let jobs = parsed
            .jobs
            .into_iter()
            .enumerate()
            .map(|(i, j)| {
                let url = j.job_url.or(j.apply_url).unwrap_or_default();
                let location = j.location.unwrap_or_else(|| "Unspecified".to_string());
                // Prefer plain text; fall back to stripping the HTML variant.
                let description = match j.description_plain {
                    Some(p) if !p.trim().is_empty() => p,
                    _ => html_to_text(j.description_html.as_deref().unwrap_or_default()),
                };
                let raw = raw_arr.get(i).map(|v| v.to_string()).unwrap_or_else(|| "{}".into());

                Job::new(
                    &self.token,
                    j.title,
                    location,
                    url,
                    self.source_name(),
                    description,
                    j.published_at,
                    raw,
                )
            })
            .collect();

        Ok(jobs)
    }
}
