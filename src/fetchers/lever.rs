//! Lever fetcher.
//!
//! Endpoint: `https://api.lever.co/v0/postings/{token}?mode=json`
//! Public, no auth. Returns a JSON array of postings. Lever already provides
//! plain-text description fields, so no HTML stripping is needed.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::Fetcher;
use crate::models::Job;
use crate::text::company_from_token;

/// Fetches jobs from one Lever board, identified by its token (e.g. `spotify`).
pub struct LeverFetcher {
    token: String,
}

impl LeverFetcher {
    pub fn new(token: impl Into<String>) -> Self {
        LeverFetcher { token: token.into() }
    }

    fn url(&self) -> String {
        format!("https://api.lever.co/v0/postings/{}?mode=json", self.token)
    }
}

#[derive(Debug, Deserialize)]
struct LeverPosting {
    text: String,
    #[serde(rename = "hostedUrl")]
    hosted_url: String,
    #[serde(default)]
    categories: Option<LeverCategories>,
    /// Epoch-millis creation time.
    #[serde(rename = "createdAt", default)]
    created_at: Option<i64>,
    // Lever splits the description across a few plain-text fields; we join the
    // ones present to get the full body for scoring.
    #[serde(rename = "descriptionPlain", default)]
    description_plain: String,
    #[serde(rename = "descriptionBodyPlain", default)]
    description_body_plain: String,
    #[serde(rename = "additionalPlain", default)]
    additional_plain: String,
}

#[derive(Debug, Deserialize)]
struct LeverCategories {
    #[serde(default)]
    location: Option<String>,
}

impl Fetcher for LeverFetcher {
    fn source_name(&self) -> &'static str {
        "lever"
    }

    async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<Job>> {
        let url = self.url();
        let body = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting Lever board {}", self.token))?
            .error_for_status()
            .with_context(|| format!("Lever board {} returned an error status", self.token))?
            .text()
            .await
            .with_context(|| format!("reading Lever response for {}", self.token))?;

        let postings: Vec<LeverPosting> = serde_json::from_str(&body)
            .with_context(|| format!("parsing Lever JSON for {}", self.token))?;
        let raw_arr: serde_json::Value = serde_json::from_str(&body)?;
        let raw_arr = raw_arr.as_array().cloned().unwrap_or_default();

        let jobs = postings
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let location = p
                    .categories
                    .and_then(|c| c.location)
                    .unwrap_or_else(|| "Unspecified".to_string());
                let description = [p.description_plain, p.description_body_plain, p.additional_plain]
                    .into_iter()
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let posted = p.created_at.map(|ms| ms.to_string());
                let raw = raw_arr.get(i).map(|v| v.to_string()).unwrap_or_else(|| "{}".into());

                Job::new(
                    &self.token, // Lever has no per-job company field
                    p.text,
                    location,
                    p.hosted_url,
                    self.source_name(),
                    description,
                    posted,
                    raw,
                )
                .with_display_company(company_from_token(&self.token))
            })
            .collect();

        Ok(jobs)
    }
}
