//! Adzuna fetcher — a broad aggregator across thousands of job sites.
//!
//! Endpoint: `https://api.adzuna.com/v1/api/jobs/{country}/search/1`
//! Needs free credentials from the `ADZUNA_APP_ID` / `ADZUNA_APP_KEY`
//! environment variables (and optional `ADZUNA_COUNTRY`, default "us").
//! Searched by one of your target roles per source, so it surfaces matching
//! jobs at companies not on your watchlist. Lower-priority aggregator, so
//! direct ATS links win on dedup.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::html_to_text;

/// Fetches Adzuna results for one search query (a target role).
pub struct AdzunaFetcher {
    query: String,
    results_per_page: i64,
}

impl AdzunaFetcher {
    pub fn new(query: impl Into<String>) -> Self {
        AdzunaFetcher { query: query.into(), results_per_page: 50 }
    }
}

#[derive(Debug, Deserialize)]
struct AdzunaResponse {
    #[serde(default)]
    results: Vec<AdzunaJob>,
}

#[derive(Debug, Deserialize)]
struct AdzunaJob {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    redirect_url: String,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    company: Option<AdzunaNamed>,
    #[serde(default)]
    location: Option<AdzunaNamed>,
}

#[derive(Debug, Deserialize)]
struct AdzunaNamed {
    #[serde(default)]
    display_name: Option<String>,
}

impl Fetcher for AdzunaFetcher {
    fn source_name(&self) -> &'static str {
        "adzuna"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let app_id = std::env::var("ADZUNA_APP_ID").ok().filter(|s| !s.trim().is_empty());
        let app_key = std::env::var("ADZUNA_APP_KEY").ok().filter(|s| !s.trim().is_empty());
        let (app_id, app_key) = match (app_id, app_key) {
            (Some(i), Some(k)) => (i, k),
            _ => return Err(anyhow!("Adzuna needs ADZUNA_APP_ID and ADZUNA_APP_KEY env vars")),
        };
        let country = std::env::var("ADZUNA_COUNTRY").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "us".into());

        let url = format!("https://api.adzuna.com/v1/api/jobs/{country}/search/1");
        let rpp = self.results_per_page.to_string();

        // Retry on rate-limit (429) / transient 5xx — Adzuna's free tier is
        // easy to trip when searching several roles in a row.
        let mut attempt = 0u32;
        let resp = loop {
            let sent = client
                .get(&url)
                .query(&[
                    ("app_id", app_id.as_str()),
                    ("app_key", app_key.as_str()),
                    ("results_per_page", rpp.as_str()),
                    ("what", self.query.as_str()),
                    ("content-type", "application/json"),
                ])
                .send()
                .await
                .with_context(|| format!("requesting Adzuna for '{}'", self.query))?;
            let status = sent.status();
            if (status.as_u16() == 429 || status.is_server_error()) && attempt < 3 {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                continue;
            }
            break sent
                .error_for_status()
                .with_context(|| format!("Adzuna returned an error status for '{}'", self.query))?;
        };

        let body = resp.text().await.context("reading Adzuna response")?;
        let parsed: AdzunaResponse =
            serde_json::from_str(&body).with_context(|| format!("parsing Adzuna JSON for '{}'", self.query))?;
        let raw_arr: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let raw_arr = raw_arr.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default();

        let jobs = parsed
            .results
            .into_iter()
            .enumerate()
            .filter(|(_, j)| !j.title.trim().is_empty() && !j.redirect_url.trim().is_empty())
            .map(|(i, j)| {
                let company = j.company.and_then(|c| c.display_name).unwrap_or_else(|| "Unknown".into());
                let location = j.location.and_then(|l| l.display_name).unwrap_or_else(|| "Unspecified".into());
                let raw = raw_arr.get(i).map(|v| v.to_string()).unwrap_or_else(|| "{}".into());
                Job::new(
                    company,
                    j.title,
                    location,
                    j.redirect_url,
                    self.source_name(),
                    html_to_text(&j.description),
                    j.created,
                    raw,
                )
            })
            .collect();
        Ok(jobs)
    }
}
