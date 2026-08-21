//! Remotive fetcher — a free, no-auth *aggregator* (the "broad scan" layer).
//!
//! Endpoint: `https://remotive.com/api/remote-jobs?category={category}`
//! Unlike the ATS fetchers (which track specific companies), this surfaces
//! remote jobs from companies *not* on your curated list. It's tagged as a
//! lower-priority source, so if the same job also appears on a company's own
//! ATS, the dedup keeps the direct ATS apply link.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::html_to_text;

/// Fetches remote jobs from Remotive for one category (e.g. "software-dev").
pub struct RemotiveFetcher {
    category: String,
}

impl RemotiveFetcher {
    pub fn new(category: impl Into<String>) -> Self {
        RemotiveFetcher { category: category.into() }
    }

    fn url(&self) -> String {
        format!("https://remotive.com/api/remote-jobs?category={}", self.category)
    }
}

#[derive(Debug, Deserialize)]
struct RemotiveResponse {
    #[serde(default)]
    jobs: Vec<RemotiveJob>,
}

#[derive(Debug, Deserialize)]
struct RemotiveJob {
    title: String,
    company_name: String,
    url: String,
    #[serde(default)]
    candidate_required_location: String,
    #[serde(default)]
    publication_date: Option<String>,
    #[serde(default)]
    description: String, // HTML
}

impl Fetcher for RemotiveFetcher {
    fn source_name(&self) -> &'static str {
        "remotive"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let url = self.url();
        let body = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting Remotive category {}", self.category))?
            .error_for_status()
            .with_context(|| format!("Remotive category {} returned an error status", self.category))?
            .text()
            .await
            .with_context(|| format!("reading Remotive response for {}", self.category))?;

        let parsed: RemotiveResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing Remotive JSON for {}", self.category))?;
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
                let location = if j.candidate_required_location.trim().is_empty() {
                    "Remote".to_string()
                } else {
                    j.candidate_required_location
                };
                let raw = raw_arr.get(i).map(|v| v.to_string()).unwrap_or_else(|| "{}".into());
                Job::new(
                    j.company_name,
                    j.title,
                    location,
                    j.url,
                    self.source_name(),
                    html_to_text(&j.description),
                    j.publication_date,
                    raw,
                )
            })
            .collect();

        Ok(jobs)
    }
}
