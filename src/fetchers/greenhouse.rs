//! Greenhouse fetcher.
//!
//! Endpoint: `https://boards-api.greenhouse.io/v1/boards/{token}/jobs?content=true`
//! Public, no auth. `content=true` includes the full (HTML) job description
//! inline so we don't have to make a second request per job.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::{company_from_token, html_to_text};

/// Fetches jobs from one Greenhouse board, identified by its token
/// (e.g. `splice`).
pub struct GreenhouseFetcher {
    token: String,
}

impl GreenhouseFetcher {
    pub fn new(token: impl Into<String>) -> Self {
        GreenhouseFetcher { token: token.into() }
    }

    fn url(&self) -> String {
        format!(
            "https://boards-api.greenhouse.io/v1/boards/{}/jobs?content=true",
            self.token
        )
    }
}

// --- Wire types: exactly the slice of Greenhouse's JSON we consume. ---
// Only fields we use are declared; serde ignores the rest.

#[derive(Debug, Deserialize)]
struct GreenhouseResponse {
    jobs: Vec<GreenhouseJob>,
}

#[derive(Debug, Deserialize)]
struct GreenhouseJob {
    title: String,
    /// Public apply/detail URL.
    absolute_url: String,
    /// HTML job description (present because we pass `content=true`).
    #[serde(default)]
    content: String,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    location: Option<GreenhouseLocation>,
    /// Some boards include the company name per job; if absent we fall back
    /// to the title-cased board token.
    #[serde(default)]
    company_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GreenhouseLocation {
    #[serde(default)]
    name: Option<String>,
}

impl Fetcher for GreenhouseFetcher {
    fn source_name(&self) -> &'static str {
        "greenhouse"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let url = self.url();
        let resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting Greenhouse board {}", self.token))?
            .error_for_status()
            .with_context(|| format!("Greenhouse board {} returned an error status", self.token))?;

        // Grab the raw body first so we can both parse it and stash each job's
        // original payload for re-scoring later.
        let body = resp
            .text()
            .await
            .with_context(|| format!("reading Greenhouse response for {}", self.token))?;

        let parsed: GreenhouseResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing Greenhouse JSON for {}", self.token))?;

        // Re-parse into generic Values purely to capture each job's raw JSON.
        // (Cheap, and keeps the typed struct above clean.)
        let raw_jobs: serde_json::Value = serde_json::from_str(&body)?;
        let raw_arr = raw_jobs
            .get("jobs")
            .and_then(|j| j.as_array())
            .cloned()
            .unwrap_or_default();

        let company_fallback = &self.token;
        let jobs = parsed
            .jobs
            .into_iter()
            .enumerate()
            .map(|(i, j)| {
                // Some boards give no company name: identify the job by the
                // token (as before, so ids stay stable) but show it title-cased.
                let company = j.company_name.filter(|c| !c.trim().is_empty());
                let location = j
                    .location
                    .and_then(|l| l.name)
                    .unwrap_or_else(|| "Unspecified".to_string());
                let raw = raw_arr
                    .get(i)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());

                let job = Job::new(
                    company.as_deref().unwrap_or(company_fallback),
                    j.title,
                    location,
                    j.absolute_url,
                    self.source_name(),
                    html_to_text(&j.content), // Greenhouse content is HTML
                    j.updated_at,
                    raw,
                );
                match company {
                    Some(_) => job,
                    None => job.with_display_company(company_from_token(company_fallback)),
                }
            })
            .collect();

        Ok(jobs)
    }
}
