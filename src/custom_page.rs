//! The optional "watch any careers page" reader.
//!
//! Turns an arbitrary careers page into jobs by reading its text with the LLM.
//! This is the isolated, removable home of the custom-page feature — gated by
//! `profile.toml [custom_pages] enabled` and dependent on the LLM being set up.
//!
//! Today it fetches the page's *static* HTML. That's enough for simple pages,
//! but many big sites (Warner Bros, Shure/iCIMS, The Audio Programmer) render
//! their jobs with JavaScript, so a static fetch sees little. The next step
//! swaps [`page_text`] for a headless-browser render — everything else here
//! (LLM extraction → Job) stays the same. To remove the feature entirely,
//! delete this file and its call in `pipeline.rs`.

use anyhow::{Context, Result};

use crate::llm::{self, LlmConfig};
use crate::models::Job;
use crate::profile::Profile;
use crate::text::html_to_text;

/// Read a custom careers page into jobs. Returns an empty vec (not an error)
/// when the page genuinely has no readable listings.
pub async fn fetch(
    url: &str,
    client: &reqwest::Client,
    cfg: &LlmConfig,
    _profile: &Profile,
) -> Result<Vec<Job>> {
    let text = page_text(url, client).await?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }

    let extracted = llm::extract_jobs(cfg, client, url, &text)
        .await
        .with_context(|| format!("extracting jobs from {url}"))?;

    let company = host_of(url);
    let jobs = extracted
        .into_iter()
        .filter(|j| !j.title.trim().is_empty())
        .map(|j| {
            let job_url = if j.url.trim().is_empty() { url.to_string() } else { absolutize(&j.url, url) };
            let location = if j.location.trim().is_empty() { "Unspecified".into() } else { j.location };
            let raw = serde_json::to_string(&serde_json::json!({
                "title": j.title, "location": location, "url": job_url, "via": url,
            })).unwrap_or_else(|_| "{}".into());
            Job::new(&company, j.title, location, job_url, "custom", j.description, None, raw)
        })
        .collect();
    Ok(jobs)
}

/// Fetch the page's visible text.
///
/// NOTE: static fetch only, for now. Swap this for a headless render to support
/// JavaScript-heavy pages — the rest of the pipeline is unchanged.
async fn page_text(url: &str, client: &reqwest::Client) -> Result<String> {
    let html = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("{url} returned an error status"))?
        .text()
        .await
        .with_context(|| format!("reading {url}"))?;
    Ok(html_to_text(&html))
}

fn host_of(url: &str) -> String {
    url.split("://")
        .last()
        .and_then(|s| s.split(['/', '?', '#']).next())
        .unwrap_or(url)
        .trim_start_matches("www.")
        .to_string()
}

/// Resolve a possibly-relative job URL against the page URL.
fn absolutize(link: &str, base: &str) -> String {
    if link.starts_with("http://") || link.starts_with("https://") {
        return link.to_string();
    }
    let scheme_host = {
        let scheme = if base.starts_with("http://") { "http://" } else { "https://" };
        let host = host_of(base);
        format!("{scheme}{host}")
    };
    if link.starts_with('/') {
        format!("{scheme_host}{link}")
    } else {
        format!("{scheme_host}/{link}")
    }
}
