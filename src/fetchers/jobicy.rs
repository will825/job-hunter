//! Jobicy fetcher — a free, no-auth remote-jobs aggregator.
//!
//! Endpoint: `https://jobicy.com/api/v2/remote-jobs?count={n}&tag={tag}`
//! Scans remote jobs across many companies, optionally filtered by tag.
//! Lower-priority aggregator.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::html_to_text;

/// Fetches remote jobs from Jobicy for one tag (e.g. "engineering").
pub struct JobicyFetcher {
    tag: String,
}

impl JobicyFetcher {
    pub fn new(tag: impl Into<String>) -> Self {
        JobicyFetcher { tag: tag.into() }
    }

    fn url(&self) -> String {
        format!("https://jobicy.com/api/v2/remote-jobs?count=50&tag={}", self.tag)
    }
}

#[derive(Debug, Deserialize)]
struct JobicyResponse {
    #[serde(default)]
    jobs: Vec<JobicyJob>,
}

#[derive(Debug, Deserialize)]
struct JobicyJob {
    #[serde(rename = "jobTitle", default)]
    job_title: String,
    #[serde(rename = "companyName", default)]
    company_name: String,
    #[serde(rename = "jobGeo", default)]
    job_geo: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "jobDescription", default)]
    job_description: String,
    #[serde(rename = "jobExcerpt", default)]
    job_excerpt: String,
    #[serde(rename = "pubDate", default)]
    pub_date: Option<String>,
}

impl Fetcher for JobicyFetcher {
    fn source_name(&self) -> &'static str {
        "jobicy"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let url = self.url();
        let body = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting Jobicy tag {}", self.tag))?
            .error_for_status()
            .with_context(|| format!("Jobicy tag {} returned an error status", self.tag))?
            .text()
            .await
            .with_context(|| format!("reading Jobicy response for {}", self.tag))?;

        let parsed: JobicyResponse =
            serde_json::from_str(&body).with_context(|| format!("parsing Jobicy JSON for {}", self.tag))?;
        let raw_arr: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let raw_arr = raw_arr.get("jobs").and_then(|r| r.as_array()).cloned().unwrap_or_default();

        let jobs = parsed
            .jobs
            .into_iter()
            .enumerate()
            .filter(|(_, j)| !j.job_title.trim().is_empty() && !j.url.trim().is_empty())
            .map(|(i, j)| {
                let desc = if j.job_description.trim().is_empty() { j.job_excerpt } else { j.job_description };
                let location = if j.job_geo.trim().is_empty() { "Remote".into() } else { j.job_geo };
                let raw = raw_arr.get(i).map(|v| v.to_string()).unwrap_or_else(|| "{}".into());
                Job::new(
                    j.company_name,
                    j.job_title,
                    location,
                    j.url,
                    "jobicy",
                    html_to_text(&desc),
                    j.pub_date,
                    raw,
                )
            })
            .collect();
        Ok(jobs)
    }
}
