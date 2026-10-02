//! Core data types shared across fetchers and storage.

use sha2::{Digest, Sha256};

/// A normalized job posting, independent of which board it came from.
///
/// Every fetcher (Greenhouse now; Lever/Ashby later) is responsible for
/// mapping its own API response into this shape, so the rest of the app
/// never has to care about board-specific JSON.
#[derive(Debug, Clone)]
pub struct Job {
    /// Stable dedup id: sha256 of company+title+url. Computed via [`Job::new`],
    /// so re-running the fetch produces the same id and we skip duplicates.
    pub id: String,
    pub company: String,
    pub title: String,
    pub location: String,
    /// The apply/detail URL.
    pub url: String,
    /// Which fetcher produced this (e.g. "greenhouse").
    pub source: String,
    /// Full job description text (may be HTML from the ATS).
    pub description: String,
    /// The ATS-provided posting date, if any (kept as the source's string form).
    pub posted_date: Option<String>,
    /// The original API payload for this job, as a JSON string. Enrichment
    /// slims it to the fields classification reads before it's stored, so
    /// jobs can be re-classified later without re-fetching.
    pub raw_json: String,
    /// Fuzzy identity key (normalized company+title+location). Two postings
    /// with the same `dedup_key` are the same logical job even across sources.
    /// See [`crate::text::dedup_key`].
    pub dedup_key: String,

    // --- Enrichment (filled by the scoring/classification pass, not fetchers) ---
    /// remote | hybrid | onsite | unknown. See [`crate::classify`].
    pub work_mode: String,
    /// us | canada | uk | emea | apac | latam | unknown.
    pub region: String,
    /// junior | mid | senior | staff | lead | unknown.
    pub seniority: String,
    /// Stage-1 keyword score. See [`crate::score`].
    pub keyword_score: i64,
    /// apply_now | strong | maybe | skip. Empty until scored.
    pub tier: String,
}

impl Job {
    /// Build a `Job`, computing the stable dedup id from the identifying fields.
    ///
    /// The id is deliberately derived from `company + title + url`: those three
    /// together identify a posting, and hashing them means a second run of the
    /// same board yields the same id — so the DB insert is idempotent.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        company: impl Into<String>,
        title: impl Into<String>,
        location: impl Into<String>,
        url: impl Into<String>,
        source: impl Into<String>,
        description: impl Into<String>,
        posted_date: Option<String>,
        raw_json: impl Into<String>,
    ) -> Self {
        let company = company.into();
        let title = title.into();
        let url = url.into();
        let location = location.into();
        let id = stable_id(&company, &title, &url);
        let dedup_key = crate::text::dedup_key(&company, &title, &location);
        Job {
            id,
            company,
            title,
            location,
            url,
            source: source.into(),
            description: description.into(),
            posted_date,
            raw_json: raw_json.into(),
            dedup_key,
            // Defaults; the enrichment pass overwrites these before storage.
            work_mode: "unknown".to_string(),
            region: "unknown".to_string(),
            seniority: "unknown".to_string(),
            keyword_score: 0,
            tier: String::new(),
        }
    }

    /// Show a different company name without changing the posting's identity:
    /// `id` and `dedup_key` stay derived from the name passed to [`Job::new`].
    /// Used by boards that only give us a token ("deepgram"), so ids from
    /// earlier scans still match.
    pub fn with_display_company(mut self, company: impl Into<String>) -> Self {
        self.company = company.into();
        self
    }
}

/// sha256 hex of the three identifying fields, joined with a separator that
/// won't appear in the values themselves.
fn stable_id(company: &str, title: &str, url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(company.as_bytes());
    hasher.update(b"\x1f"); // unit separator
    hasher.update(title.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(url.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_across_calls() {
        let a = stable_id("Splice", "Rust Engineer", "https://example.com/1");
        let b = stable_id("Splice", "Rust Engineer", "https://example.com/1");
        assert_eq!(a, b);
    }

    #[test]
    fn id_changes_with_inputs() {
        let a = stable_id("Splice", "Rust Engineer", "https://example.com/1");
        let b = stable_id("Splice", "Rust Engineer", "https://example.com/2");
        assert_ne!(a, b);
    }
}
