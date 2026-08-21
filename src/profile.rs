//! Your profile — the thing that makes recommendations fit *you*.
//!
//! Loaded from a human-editable `profile.toml` in the project folder. It drives
//! the Stage-1 keyword weights (so scoring reflects your real target roles,
//! skills, interests, and dealbreakers instead of hardcoded guesses) and later
//! feeds the Stage-2 LLM the `bio` + `target_roles` so it judges *"does this fit
//! Will?"* rather than *"is this a tech job?"*.
//!
//! If `profile.toml` doesn't exist yet, a sensible default is written on first
//! run for you to edit.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// The on-disk profile. Every field has a default, so a partial TOML is fine.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Short paragraph the Stage-2 LLM reads to judge fit.
    pub bio: String,
    /// The roles you're actively searching for — strongest title signal.
    pub target_roles: Vec<String>,
    /// Domains/topics you care about.
    pub interests: Vec<String>,
    pub skills: Skills,
    pub preferences: Preferences,
    pub weights: Weights,
    pub tiers: Tiers,
    pub llm: Llm,
    pub custom_pages: CustomPages,
    pub adzuna: Adzuna,
    pub email: Email,
}

/// Daily-digest email config (via Resend). The API key comes from the
/// `RESEND_API_KEY` environment variable, never this file.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Email {
    pub enabled: bool,
    /// Sender address. On Resend's free tier, `onboarding@resend.dev` works to
    /// email yourself with no domain setup.
    pub from: String,
    /// Where the digest is sent (your email).
    pub to: String,
    /// Max jobs to include in one digest.
    pub max_jobs: i64,
}

impl Default for Email {
    fn default() -> Self {
        Email {
            enabled: false,
            from: "onboarding@resend.dev".to_string(),
            to: String::new(),
            max_jobs: 15,
        }
    }
}

/// Adzuna aggregator config. Adzuna scans thousands of job sites, searched by
/// your target roles — so it surfaces matching jobs at companies you never
/// listed. Needs free credentials in `ADZUNA_APP_ID` / `ADZUNA_APP_KEY`
/// (and optionally `ADZUNA_COUNTRY`, default "us").
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Adzuna {
    pub enabled: bool,
    pub country: String,
    pub results_per_page: i64,
    /// How many of your target roles to search each scan (one query per role).
    pub max_roles: i64,
}

impl Default for Adzuna {
    fn default() -> Self {
        Adzuna { enabled: true, country: "us".to_string(), results_per_page: 50, max_roles: 8 }
    }
}

/// LLM fit-scoring config (Phase 3). The API key is read from the environment
/// (`GROQ_API_KEY`), never stored in this file. Set `enabled = false` to turn
/// the whole LLM layer off — the tool works fine on keyword scoring alone.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Llm {
    pub enabled: bool,
    pub provider: String, // "groq" | "none"
    pub model: String,
    /// Cap on LLM calls per scan, so cost/time stays bounded.
    pub max_jobs_per_run: i64,
}

impl Default for Llm {
    fn default() -> Self {
        Llm {
            enabled: true,
            provider: "groq".to_string(),
            model: "llama-3.3-70b-versatile".to_string(),
            max_jobs_per_run: 30,
        }
    }
}

/// The optional "watch any careers page" feature (headless + LLM reader).
/// Off by default — flip to `true` once you want it, delete the section or set
/// `false` to disable it entirely.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct CustomPages {
    pub enabled: bool,
}

impl Default for CustomPages {
    fn default() -> Self {
        CustomPages { enabled: false }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Skills {
    pub strong: Vec<String>,
    pub medium: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Preferences {
    /// Preferred work modes (e.g. remote, hybrid). Others are down-ranked, not
    /// hidden — so you never silently lose a job you might still consider.
    pub work_modes: Vec<String>,
    /// Preferred regions (e.g. us). Others down-ranked. Empty = no preference.
    pub regions: Vec<String>,
    /// Phrases that strongly disqualify a role (e.g. "security clearance").
    pub dealbreakers: Vec<String>,
}

/// Point values the compiled scoring model uses.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Weights {
    pub target_role: i32,
    pub strong_skill: i32,
    pub medium_skill: i32,
    pub interest: i32,
    pub dealbreaker: i32,
    /// Penalty when a job's work_mode isn't one you prefer.
    pub offmode_penalty: i32,
    /// Penalty when a job's region isn't one you prefer.
    pub offregion_penalty: i32,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            target_role: 12,
            strong_skill: 10,
            medium_skill: 4,
            interest: 6,
            dealbreaker: -14,
            offmode_penalty: -6,
            offregion_penalty: -4,
        }
    }
}

/// Score thresholds that map a keyword score to a tier.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(default)]
pub struct Tiers {
    pub apply_now: i64,
    pub strong: i64,
    pub maybe: i64,
}

impl Default for Tiers {
    fn default() -> Self {
        Tiers { apply_now: 30, strong: 16, maybe: 6 }
    }
}

/// Load the profile from `path`, creating a commented default there if absent.
pub fn load_or_create(path: &str) -> Result<Profile> {
    if !Path::new(path).exists() {
        std::fs::write(path, DEFAULT_PROFILE_TOML)
            .with_context(|| format!("writing default profile to {path}"))?;
        println!("Created a starter profile at {path} — edit it to tune your matches.\n");
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let profile: Profile =
        toml::from_str(&text).with_context(|| format!("parsing {path} (check the TOML syntax)"))?;
    Ok(profile)
}

/// Which editable list on the profile a UI edit targets.
pub fn list_location(field: &str) -> Option<(Option<&'static str>, &'static str)> {
    match field {
        "roles" => Some((None, "target_roles")),
        "interests" => Some((None, "interests")),
        "skills" => Some((Some("skills"), "strong")),
        _ => None,
    }
}

/// Add or remove a value in one of the profile's editable lists (`roles`,
/// `skills`, `interests`), preserving the file's comments and formatting.
/// Adding is case-insensitively de-duplicated; the change takes effect on the
/// next scan (a new role, for instance, becomes a new Adzuna search).
pub fn edit_list(path: &str, field: &str, value: &str, remove: bool) -> Result<()> {
    let (section, key) = list_location(field)
        .ok_or_else(|| anyhow::anyhow!("unknown profile list '{field}' (use roles|skills|interests)"))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow::anyhow!("value is empty"));
    }

    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing {path}"))?;

    // Resolve (creating if needed) the target array.
    let empty_arr = || toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()));
    let item = match section {
        None => doc.as_table_mut().entry(key).or_insert_with(empty_arr),
        Some(sec) => {
            let tbl = doc
                .as_table_mut()
                .entry(sec)
                .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
            let tbl = tbl
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("[{sec}] is not a table"))?;
            tbl.entry(key).or_insert_with(empty_arr)
        }
    };
    if item.as_array().is_none() {
        *item = empty_arr();
    }
    let arr = item.as_array_mut().unwrap();

    if remove {
        arr.retain(|e| e.as_str().map(|s| s.trim()) != Some(value));
    } else if !arr.iter().any(|e| e.as_str().map(|s| s.trim().eq_ignore_ascii_case(value)).unwrap_or(false)) {
        arr.push(value);
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {path}"))?;
    Ok(())
}

/// Set the top-level `bio` string, preserving comments/formatting.
pub fn set_bio(path: &str, bio: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing {path}"))?;
    doc.as_table_mut().insert("bio", toml_edit::value(bio.trim()));
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {path}"))?;
    Ok(())
}

/// Save the `[email]` settings (enabled, from, recipient) to the profile,
/// preserving comments/formatting. The Resend API key is NOT stored here — it
/// stays in the `.env` file as a secret.
pub fn set_email(path: &str, enabled: bool, from: &str, to: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing {path}"))?;

    let tbl = doc
        .as_table_mut()
        .entry("email")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[email] is not a table"))?;
    tbl.insert("enabled", toml_edit::value(enabled));
    let from = from.trim();
    tbl.insert("from", toml_edit::value(if from.is_empty() { "onboarding@resend.dev" } else { from }));
    tbl.insert("to", toml_edit::value(to.trim()));

    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {path}"))?;
    Ok(())
}

/// The compiled model the scorer actually uses: a flat term→weight map plus the
/// preference penalties and tier thresholds. Built once from the profile.
pub struct ScoringModel {
    pub weights: Vec<(String, i32)>,
    pub preferred_work_modes: Vec<String>,
    pub preferred_regions: Vec<String>,
    pub offmode_penalty: i32,
    pub offregion_penalty: i32,
    pub tiers: Tiers,
}

impl Profile {
    /// Compile this profile into a scoring model.
    pub fn compile(&self) -> ScoringModel {
        // Collect term→weight, keeping the largest-magnitude weight on conflict
        // (so a dealbreaker always beats an incidental positive on the same word).
        let mut map: HashMap<String, i32> = HashMap::new();
        let mut add = |term: &str, weight: i32| {
            let term = term.trim().to_lowercase();
            if term.is_empty() {
                return;
            }
            map.entry(term)
                .and_modify(|w| {
                    if weight.abs() > w.abs() {
                        *w = weight;
                    }
                })
                .or_insert(weight);
        };

        for r in &self.target_roles {
            add(r, self.weights.target_role);
        }
        for s in &self.skills.strong {
            add(s, self.weights.strong_skill);
        }
        for s in &self.skills.medium {
            add(s, self.weights.medium_skill);
        }
        for i in &self.interests {
            add(i, self.weights.interest);
        }
        for d in &self.preferences.dealbreakers {
            add(d, self.weights.dealbreaker);
        }

        ScoringModel {
            weights: map.into_iter().collect(),
            preferred_work_modes: lower(&self.preferences.work_modes),
            preferred_regions: lower(&self.preferences.regions),
            offmode_penalty: self.weights.offmode_penalty,
            offregion_penalty: self.weights.offregion_penalty,
            tiers: self.tiers,
        }
    }
}

fn lower(v: &[String]) -> Vec<String> {
    v.iter().map(|s| s.to_lowercase()).collect()
}

/// The starter profile written on first run. Seeded from Will's target lane;
/// meant to be edited.
const DEFAULT_PROFILE_TOML: &str = r#"# Your Job Hunter profile. Edit freely — changes take effect next run.
# This drives how jobs are scored and (later) what the LLM compares against.

name = "Will Hall"

# The paragraph the LLM reads to judge fit. Make it about who you are and what
# you want. A couple of honest sentences beat a keyword dump.
bio = """
Product-minded engineer with a deep audio background (live sound, post-production)
now building software: Rust + Tauri desktop apps, on-device AI/ML, and creator
tools. Looking for product engineer / technical product manager / Rust roles at
music-tech, audio, or AI companies. Prefer remote or hybrid.
"""

# The jobs you're actively searching for. Strongest signal — matched in titles.
target_roles = [
    "product engineer",
    "technical product manager",
    "product manager",
    "rust engineer",
    "software engineer",
    "audio software engineer",
]

# Domains you care about (a moderate positive signal).
interests = ["music-tech", "creator tools", "ai audio", "music", "audio"]

[skills]
# Your strongest, most differentiating skills (high weight).
strong = ["rust", "tauri", "dsp", "on-device", "onnx", "audio plugin", "plugin"]
# Supporting skills (moderate weight).
medium = ["react", "typescript", "full-stack", "ai", "machine learning", "api", "native"]

[preferences]
# Preferred work modes; anything else is down-ranked (not hidden).
work_modes = ["remote", "hybrid"]
# Preferred regions (us, uk, emea, apac, canada, latam). Empty = no preference.
regions = ["us"]
# Phrases that strongly disqualify a role.
dealbreakers = ["security clearance", "clearance"]

# Optional: tune the point values. Defaults shown.
[weights]
target_role = 12
strong_skill = 10
medium_skill = 4
interest = 6
dealbreaker = -14
offmode_penalty = -6
offregion_penalty = -4

# Optional: tune the score thresholds for each tier.
[tiers]
apply_now = 30
strong = 16
maybe = 6

# LLM fit-scoring (Phase 3). Reads the top keyword matches and re-ranks them by
# genuine fit. Set enabled = false to turn it off completely (keyword scoring
# still works). The API key comes from the GROQ_API_KEY environment variable,
# never this file.
[llm]
enabled = true
provider = "groq"                 # groq | none
model = "llama-3.3-70b-versatile"
max_jobs_per_run = 30             # cap LLM calls per scan

# Optional "watch any careers page" feature (reads pages that aren't on a known
# ATS, like Warner Bros / Shure / The Audio Programmer). Off by default; set
# enabled = true to turn it on, false (or delete this section) to remove it.
[custom_pages]
enabled = false

# Adzuna aggregator: scans thousands of job sites, searched by your target roles
# above — surfaces matching jobs at companies you never listed. Needs free
# credentials in the ADZUNA_APP_ID and ADZUNA_APP_KEY environment variables
# (sign up at developer.adzuna.com). Set enabled = false to turn it off.
[adzuna]
enabled = true
country = "us"          # us | gb | ca | au | ...
results_per_page = 50
max_roles = 8           # searches this many of your target_roles per scan

# Daily digest email via Resend (free, uses an API key — no email password).
# Create a key at resend.com and put it in .env as RESEND_API_KEY. On the free
# tier you can email yourself using the default "from" below with no domain setup.
# The digest emails only NEW apply-now/strong matches since the last run.
[email]
enabled = false
from = "onboarding@resend.dev"   # Resend's test sender (fine for emailing yourself)
to = ""                          # your email
max_jobs = 40
"#;
