//! Job sources: which boards/aggregators to scan.
//!
//! Sources are now **runtime data** — stored in the database and managed via
//! the web UI / CLI (add a company by pasting its careers link). [`seed`]
//! provides the initial default list used to populate an empty database on
//! first run; after that, the DB is the source of truth.

/// A job source, tagged by platform. Tokens are owned so sources can be built
/// at runtime from the database or a pasted URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Greenhouse(String),
    Lever(String),
    Ashby(String),
    /// A free aggregator; the string is the category (e.g. "software-dev").
    Remotive(String),
    /// A free aggregator; the string is a tag/search (e.g. "dev").
    RemoteOk(String),
    /// Broad aggregator across thousands of sites (needs a free key). `query`
    /// is one of your target roles; `country` and `results_per_page` come from
    /// the profile's `[adzuna]` section.
    Adzuna { query: String, country: String, results_per_page: i64 },
    /// A free aggregator; the string is an (unused) hint.
    Himalayas(String),
    /// A free aggregator; the string is a tag (e.g. "engineering").
    Jobicy(String),
    /// Any other company careers page (not on a known ATS). The string is the
    /// full URL. Read by the optional custom-page reader (headless + LLM),
    /// which can be turned off entirely — see `profile.toml [custom_pages]`.
    CustomPage(String),
}

impl Source {
    /// The board token / category / URL.
    pub fn token(&self) -> &str {
        match self {
            Source::Greenhouse(t)
            | Source::Lever(t)
            | Source::Ashby(t)
            | Source::Remotive(t)
            | Source::RemoteOk(t)
            | Source::Adzuna { query: t, .. }
            | Source::Himalayas(t)
            | Source::Jobicy(t)
            | Source::CustomPage(t) => t,
        }
    }

    /// The platform name (matches the `source` stored on each job).
    pub fn ats(&self) -> &'static str {
        match self {
            Source::Greenhouse(_) => "greenhouse",
            Source::Lever(_) => "lever",
            Source::Ashby(_) => "ashby",
            Source::Remotive(_) => "remotive",
            Source::RemoteOk(_) => "remoteok",
            Source::Adzuna { .. } => "adzuna",
            Source::Himalayas(_) => "himalayas",
            Source::Jobicy(_) => "jobicy",
            Source::CustomPage(_) => "custom",
        }
    }

    /// An Adzuna search. The country must be a plain country code (it goes in
    /// the URL path); anything else falls back to "us". Results per page are
    /// clamped to Adzuna's 1–50.
    pub fn adzuna(query: impl Into<String>, country: &str, results_per_page: i64) -> Source {
        let country = country.trim().to_lowercase();
        let country = if !country.is_empty() && country.len() <= 3 && country.bytes().all(|b| b.is_ascii_lowercase()) {
            country
        } else {
            "us".to_string()
        };
        Source::Adzuna { query: query.into(), country, results_per_page: results_per_page.clamp(1, 50) }
    }

    /// Whether this source needs the optional custom-page reader.
    pub fn is_custom(&self) -> bool {
        matches!(self, Source::CustomPage(_))
    }

    /// Human label for run output. Custom pages show their host, not the full URL.
    pub fn label(&self) -> String {
        match self {
            Source::CustomPage(url) => format!("custom:{}", host_of(url)),
            _ => format!("{}:{}", self.ats(), self.token()),
        }
    }

    /// Build a source from a platform name + token (e.g. from a DB row).
    /// Returns `None` for an unknown platform.
    pub fn from_ats(ats: &str, token: &str) -> Option<Source> {
        let token = token.to_string();
        match ats.to_lowercase().as_str() {
            "greenhouse" => Some(Source::Greenhouse(token)),
            "lever" => Some(Source::Lever(token)),
            "ashby" => Some(Source::Ashby(token)),
            "remotive" => Some(Source::Remotive(token)),
            "remoteok" => Some(Source::RemoteOk(token)),
            "adzuna" => Some(Source::adzuna(token, "us", 50)),
            "himalayas" => Some(Source::Himalayas(token)),
            "jobicy" => Some(Source::Jobicy(token)),
            "custom" => Some(Source::CustomPage(token)),
            _ => None,
        }
    }
}

/// Extract a display host from a URL for labels.
fn host_of(url: &str) -> String {
    url.split("://")
        .last()
        .and_then(|s| s.split(['/', '?', '#']).next())
        .unwrap_or(url)
        .to_string()
}

/// The default seed list, used only to populate an empty database on first run.
/// Every token here was verified to resolve to a live board. Grouped by lane.
pub fn seed() -> Vec<Source> {
    let s = |t: &str| Source::Greenhouse(t.to_string());
    let l = |t: &str| Source::Lever(t.to_string());
    let a = |t: &str| Source::Ashby(t.to_string());
    vec![
        // --- Music / audio tech ---
        s("splice"),
        l("spotify"),
        a("output"),
        a("hookmusic"),
        s("genius"),
        // --- Audio AI / speech (your sharpest lane) ---
        a("elevenlabs"),
        a("cartesia"),
        a("deepgram"),
        a("gladia"),
        s("assemblyai"),
        s("speechmatics"),
        // --- Generative music/video AI ---
        a("suno"),
        s("udio"),
        a("pika"),
        s("descript"),
        // --- AI labs / platforms ---
        s("anthropic"),
        a("openai"),
        a("cohere"),
        a("perplexity"),
        a("character"),
        a("modal"),
        s("togetherai"),
        s("stabilityai"),
        s("scaleai"),
        // --- Creator / product / dev tools ---
        s("figma"),
        a("notion"),
        a("linear"),
        s("vercel"),
        // --- Broad aggregator scans (companies beyond the curated list) ---
        Source::Remotive("software-dev".to_string()),
        Source::RemoteOk("dev".to_string()),
        Source::Himalayas("all".to_string()),
        Source::Jobicy("engineering".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adzuna_source_cleans_country_and_page_size() {
        let s = Source::adzuna("Rust Engineer", " GB ", 200);
        assert_eq!(s, Source::Adzuna { query: "Rust Engineer".into(), country: "gb".into(), results_per_page: 50 });
        let s = Source::adzuna("x", "us/../v2", 0);
        assert_eq!(s, Source::Adzuna { query: "x".into(), country: "us".into(), results_per_page: 1 });
        assert_eq!(s.token(), "x");
        assert_eq!(s.label(), "adzuna:x");
    }
}
