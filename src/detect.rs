//! Turn a pasted careers-page URL into a watchable [`Source`].
//!
//! Handles the three supported ATSs directly from the URL, and for a custom
//! company page (e.g. `acme.com/careers`) it fetches the HTML and sniffs for an
//! embedded ATS board — since most "custom" pages actually embed Greenhouse,
//! Lever, or Ashby. If nothing supported is found, it returns a clear error the
//! UI can show ("that page isn't a supported job board").

use anyhow::{anyhow, Result};

use crate::sources::Source;

/// Detect a source from a URL, validating that it actually returns jobs.
///
/// Steps: parse the URL for a known ATS pattern → if none, fetch the page and
/// look for an embedded ATS → validate by fetching the board → return the
/// source. Every failure path yields a human-readable error.
pub async fn detect(url: &str, client: &reqwest::Client) -> Result<Source> {
    let url = url.trim();
    if url.is_empty() {
        return Err(anyhow!("Please paste a job-board or careers-page URL."));
    }

    // 1. Try to read a source straight out of the URL.
    if let Some(src) = from_url(url) {
        return validate(src, client).await;
    }

    // 2. Fetch the page. If it embeds a known ATS, prefer that (clean data).
    //    Otherwise, accept it as a custom page to watch — no rejection.
    let resp = client
        .get(normalize_url(url))
        .send()
        .await
        .map_err(|_| anyhow!("Couldn't reach that page. Check the URL and try again."))?;
    if !resp.status().is_success() {
        return Err(anyhow!("That page returned {} — check the URL.", resp.status().as_u16()));
    }
    let html = resp.text().await.unwrap_or_default();

    if let Some(src) = sniff_html(&html) {
        return validate(src, client).await;
    }

    // A reachable page with no known ATS → watch it as a custom page. Actually
    // reading its jobs is the custom-page reader's job (headless + LLM), which
    // can be toggled on/off; adding it here never fails.
    Ok(Source::CustomPage(normalize_url(url)))
}

/// Parse a known ATS URL directly. Returns None if it's not an obvious ATS URL.
fn from_url(url: &str) -> Option<Source> {
    let host = host_of(url)?;
    let segs = path_segments(url);

    if host.contains("greenhouse.io") {
        // boards-api.greenhouse.io/v1/boards/{token}/...
        if let Some(i) = segs.iter().position(|s| s == "boards") {
            if let Some(tok) = segs.get(i + 1) {
                return Some(Source::Greenhouse(tok.clone()));
            }
        }
        // boards.greenhouse.io/{token} or job-boards.greenhouse.io/{token}
        if let Some(tok) = segs.first() {
            return Some(Source::Greenhouse(tok.clone()));
        }
    }
    if host.contains("lever.co") {
        // api.lever.co/v0/postings/{token} or jobs.lever.co/{token}
        if let Some(i) = segs.iter().position(|s| s == "postings") {
            if let Some(tok) = segs.get(i + 1) {
                return Some(Source::Lever(tok.clone()));
            }
        }
        if let Some(tok) = segs.first() {
            return Some(Source::Lever(tok.clone()));
        }
    }
    if host.contains("ashbyhq.com") {
        // api.ashbyhq.com/posting-api/job-board/{token} or jobs.ashbyhq.com/{token}
        if let Some(i) = segs.iter().position(|s| s == "job-board") {
            if let Some(tok) = segs.get(i + 1) {
                return Some(Source::Ashby(tok.clone()));
            }
        }
        if let Some(tok) = segs.first() {
            return Some(Source::Ashby(tok.clone()));
        }
    }
    None
}

/// Sniff a page's HTML for an embedded ATS board URL.
fn sniff_html(html: &str) -> Option<Source> {
    // Look for the first occurrence of each ATS host, then read the token that
    // follows it. Order doesn't matter much; we return the first found.
    for (needle, make) in [
        ("boards.greenhouse.io/", 0u8),
        ("job-boards.greenhouse.io/", 0),
        ("boards-api.greenhouse.io/v1/boards/", 1),
        ("jobs.lever.co/", 2),
        ("jobs.ashbyhq.com/", 3),
    ] {
        if let Some(pos) = html.find(needle) {
            let after = &html[pos + needle.len()..];
            let tok = read_token(after);
            if !tok.is_empty() {
                return Some(match make {
                    0 | 1 => Source::Greenhouse(tok),
                    2 => Source::Lever(tok),
                    _ => Source::Ashby(tok),
                });
            }
        }
    }
    None
}

/// Read a URL-token (up to the next `/`, `"`, `'`, `?`, `#`, or whitespace).
fn read_token(s: &str) -> String {
    s.chars()
        .take_while(|c| !matches!(c, '/' | '"' | '\'' | '?' | '#' | '<' | '>' | ' ' | '\\'))
        .collect()
}

/// Validate a detected source by fetching its board; enrich the error if empty.
async fn validate(src: Source, client: &reqwest::Client) -> Result<Source> {
    let jobs = crate::fetchers::fetch_source(&src, client)
        .await
        .map_err(|e| anyhow!("Detected {} but couldn't load it: {e}", src.label()))?;
    if jobs.is_empty() {
        return Err(anyhow!(
            "Detected {} but it currently lists no jobs. Added anyway is not done — \
             double-check the URL.",
            src.label()
        ));
    }
    Ok(src)
}

// --- Tiny URL helpers (dependency-free; `url` crate would be overkill here) ---

fn normalize_url(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

fn host_of(url: &str) -> Option<String> {
    let no_scheme = url.split("://").last()?;
    let host = no_scheme.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

fn path_segments(url: &str) -> Vec<String> {
    let no_scheme = url.splitn(2, "://").last().unwrap_or(url);
    let after_host = no_scheme.split_once('/').map_or("", |(_, rest)| rest);
    let path = after_host.split(['?', '#']).next().unwrap_or("");
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_greenhouse_board_url() {
        assert_eq!(
            from_url("https://boards.greenhouse.io/splice"),
            Some(Source::Greenhouse("splice".into()))
        );
        assert_eq!(
            from_url("https://job-boards.greenhouse.io/splice/jobs/123"),
            Some(Source::Greenhouse("splice".into()))
        );
        assert_eq!(
            from_url("https://boards-api.greenhouse.io/v1/boards/splice/jobs"),
            Some(Source::Greenhouse("splice".into()))
        );
    }

    #[test]
    fn parses_lever_and_ashby_urls() {
        assert_eq!(from_url("https://jobs.lever.co/spotify"), Some(Source::Lever("spotify".into())));
        assert_eq!(
            from_url("https://jobs.ashbyhq.com/elevenlabs/some-id"),
            Some(Source::Ashby("elevenlabs".into()))
        );
    }

    #[test]
    fn non_ats_url_returns_none() {
        assert_eq!(from_url("https://acme.com/careers"), None);
    }

    #[test]
    fn sniffs_embedded_board_from_html() {
        let html = r#"<iframe src="https://boards.greenhouse.io/acmeco?for=acmeco"></iframe>"#;
        assert_eq!(sniff_html(html), Some(Source::Greenhouse("acmeco".into())));
    }
}
