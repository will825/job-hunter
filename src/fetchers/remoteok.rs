//! RemoteOK fetcher — a second free, no-auth aggregator.
//!
//! Endpoint: `https://remoteok.com/api?tag={tag}`
//! Returns a JSON array whose FIRST element is a legal/metadata object (no
//! `position`), followed by job objects. Like Remotive, it's a lower-priority
//! aggregator so direct ATS apply links win on dedup.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::html_to_text;

/// Fetches remote jobs from RemoteOK for one tag (e.g. "dev").
pub struct RemoteOkFetcher {
    tag: String,
}

impl RemoteOkFetcher {
    pub fn new(tag: impl Into<String>) -> Self {
        RemoteOkFetcher { tag: tag.into() }
    }

    fn url(&self) -> String {
        format!("https://remoteok.com/api?tag={}", self.tag)
    }
}

#[derive(Debug, Deserialize)]
struct RemoteOkJob {
    // The metadata element lacks these, so they're optional and we skip rows
    // missing the essentials.
    #[serde(default)]
    position: Option<String>,
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    apply_url: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    description: Option<String>, // HTML
}

impl Fetcher for RemoteOkFetcher {
    fn source_name(&self) -> &'static str {
        "remoteok"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let url = self.url();
        let body = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting RemoteOK tag {}", self.tag))?
            .error_for_status()
            .with_context(|| format!("RemoteOK tag {} returned an error status", self.tag))?
            .text()
            .await
            .with_context(|| format!("reading RemoteOK response for {}", self.tag))?;

        let items: Vec<RemoteOkJob> = serde_json::from_str(&body)
            .with_context(|| format!("parsing RemoteOK JSON for {}", self.tag))?;
        let raw_arr: serde_json::Value = serde_json::from_str(&body)?;
        let raw_arr = raw_arr.as_array().cloned().unwrap_or_default();

        let jobs = items
            .into_iter()
            .enumerate()
            .filter_map(|(i, j)| {
                // Skip the metadata element and any row missing essentials.
                let title = j.position?;
                let company = j.company?;
                let url = j.apply_url.or(j.url)?;
                if title.trim().is_empty() || company.trim().is_empty() {
                    return None;
                }
                let location = j.location.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Remote".into());
                let raw = raw_arr.get(i).map(|v| v.to_string()).unwrap_or_else(|| "{}".into());
                Some(Job::new(
                    company,
                    title,
                    location,
                    url,
                    self.source_name(),
                    html_to_text(j.description.as_deref().unwrap_or_default()),
                    j.date,
                    raw,
                ))
            })
            .collect();

        Ok(jobs)
    }
}
