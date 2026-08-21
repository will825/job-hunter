//! Himalayas fetcher — a free, no-auth remote-jobs aggregator.
//!
//! Endpoint: `https://himalayas.app/jobs/api?limit={n}`
//! Scans recent remote jobs across many companies. Lower-priority aggregator.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::html_to_text;

/// Fetches recent remote jobs from Himalayas. The string is a soft limit hint
/// ("all" = default limit).
pub struct HimalayasFetcher {
    limit: i64,
}

impl HimalayasFetcher {
    pub fn new(_tag: impl Into<String>) -> Self {
        HimalayasFetcher { limit: 100 }
    }

    fn url(&self) -> String {
        format!("https://himalayas.app/jobs/api?limit={}", self.limit)
    }
}

#[derive(Debug, Deserialize)]
struct HimalayasResponse {
    #[serde(default)]
    jobs: Vec<HimalayasJob>,
}

#[derive(Debug, Deserialize)]
struct HimalayasJob {
    #[serde(default)]
    title: String,
    #[serde(rename = "companyName", default)]
    company_name: String,
    #[serde(default)]
    excerpt: String,
    #[serde(rename = "applicationLink", default)]
    application_link: Option<String>,
    #[serde(default)]
    guid: Option<String>,
    #[serde(rename = "locationRestrictions", default)]
    location_restrictions: Vec<String>,
    #[serde(rename = "pubDate", default)]
    pub_date: Option<serde_json::Value>,
}

impl Fetcher for HimalayasFetcher {
    fn source_name(&self) -> &'static str {
        "himalayas"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let url = self.url();
        let body = client
            .get(&url)
            .send()
            .await
            .context("requesting Himalayas")?
            .error_for_status()
            .context("Himalayas returned an error status")?
            .text()
            .await
            .context("reading Himalayas response")?;

        let parsed: HimalayasResponse = serde_json::from_str(&body).context("parsing Himalayas JSON")?;
        let raw_arr: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let raw_arr = raw_arr.get("jobs").and_then(|r| r.as_array()).cloned().unwrap_or_default();

        let jobs = parsed
            .jobs
            .into_iter()
            .enumerate()
            .filter(|(_, j)| !j.title.trim().is_empty())
            .filter_map(|(i, j)| {
                let url = j.application_link.or(j.guid)?;
                let location = if j.location_restrictions.is_empty() {
                    "Remote".to_string()
                } else {
                    j.location_restrictions.join(", ")
                };
                let posted = j.pub_date.map(|v| v.to_string());
                let raw = raw_arr.get(i).map(|v| v.to_string()).unwrap_or_else(|| "{}".into());
                Some(Job::new(
                    j.company_name,
                    j.title,
                    location,
                    url,
                    "himalayas",
                    html_to_text(&j.excerpt),
                    posted,
                    raw,
                ))
            })
            .collect();
        Ok(jobs)
    }
}
