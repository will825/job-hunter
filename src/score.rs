//! Stage-1 keyword scoring (cheap, runs on every job).
//!
//! A transparent weighted model over the job's title + description, driven by
//! your [`crate::profile::Profile`] (target roles, skills, interests,
//! dealbreakers, and work-mode/region preferences). It produces an integer
//! `keyword_score` and a coarse `tier`. This is the cheap pre-filter from the
//! plan: it sorts the flood so the expensive Stage-2 LLM (Phase 3) only ever
//! looks at the survivors.
//!
//! A term matched in the **title** counts for more than the same term in the
//! description, because titles are denser signal — and positive description
//! matches are capped so company boilerplate can't inflate an off-target role.

use crate::models::Job;
use crate::profile::{ScoringModel, Tiers};

/// A scored tier. Ordered worst→best so it derives `Ord` usefully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Skip,
    Maybe,
    Strong,
    ApplyNow,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Skip => "skip",
            Tier::Maybe => "maybe",
            Tier::Strong => "strong",
            Tier::ApplyNow => "apply_now",
        }
    }
}

/// Title matches are worth this multiple of a description match.
const TITLE_MULTIPLIER: i32 = 3;
/// Positive description matches can add at most this much. This is the key
/// precision guard: it stops company boilerplate (e.g. "audio"/"music"/"ai" in
/// every posting at an audio company) from lifting an off-target role — the
/// role's real fit has to show up in the *title* to reach the top tiers.
const DESC_POSITIVE_CAP: i32 = 12;

/// Compute the keyword score for a job against a compiled profile model.
///
/// Reads `job.work_mode` / `job.region`, so classification must run first.
pub fn keyword_score(job: &Job, model: &ScoringModel) -> i64 {
    let title = job.title.to_lowercase();
    let desc = job.description.to_lowercase();
    let title_tokens = tokenize(&title);
    let desc_tokens = tokenize(&desc);

    let mut title_score = 0i32;
    let mut desc_positive = 0i32;
    let mut desc_negative = 0i32; // penalties always apply in full, uncapped

    for (term, weight) in &model.weights {
        if term_present(term, &title, &title_tokens) {
            title_score += weight * TITLE_MULTIPLIER;
        }
        if term_present(term, &desc, &desc_tokens) {
            if *weight >= 0 {
                desc_positive += weight;
            } else {
                desc_negative += weight;
            }
        }
    }

    let mut score = title_score + desc_positive.min(DESC_POSITIVE_CAP) + desc_negative;

    // Preference down-ranking. Only applies when the job is *definitely* off a
    // stated preference — an "unknown" classification is never penalized, so a
    // job is never silently buried on a bad guess.
    if !model.preferred_work_modes.is_empty()
        && job.work_mode != "unknown"
        && !model.preferred_work_modes.contains(&job.work_mode)
    {
        score += model.offmode_penalty;
    }
    if !model.preferred_regions.is_empty()
        && job.region != "unknown"
        && !model.preferred_regions.contains(&job.region)
    {
        score += model.offregion_penalty;
    }

    score as i64
}

/// Map a keyword score to a tier using the profile's thresholds.
pub fn tier_for(score: i64, tiers: &Tiers) -> Tier {
    if score >= tiers.apply_now {
        Tier::ApplyNow
    } else if score >= tiers.strong {
        Tier::Strong
    } else if score >= tiers.maybe {
        Tier::Maybe
    } else {
        Tier::Skip
    }
}

/// Whether a keyword is present: multi-word terms match as substrings;
/// single-word terms must match a whole token.
fn term_present(term: &str, text: &str, tokens: &[String]) -> bool {
    if term.contains(' ') || term.contains('-') {
        text.contains(term)
    } else {
        tokens.iter().any(|t| t == term)
    }
}

/// Split text into lowercase alphanumeric tokens.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;

    fn model() -> ScoringModel {
        // Compile the built-in default profile for tests.
        let p: Profile = toml::from_str(
            r#"
            target_roles = ["product engineer", "software engineer"]
            interests = ["music", "audio"]
            [skills]
            strong = ["rust", "dsp"]
            medium = ["react", "ai"]
            [preferences]
            work_modes = ["remote", "hybrid"]
            regions = ["us"]
            dealbreakers = ["security clearance"]
            "#,
        )
        .unwrap();
        p.compile()
    }

    fn job(title: &str, desc: &str) -> Job {
        Job::new("Co", title, "Remote", "http://x", "greenhouse", desc, None, "{}")
    }

    #[test]
    fn strong_match_scores_high_and_tiers_up() {
        let m = model();
        let mut j = job("Product Engineer, Audio", "Build Rust DSP tools for music creators");
        j.work_mode = "remote".into();
        j.region = "us".into();
        let s = keyword_score(&j, &m);
        assert!(s >= 30, "expected apply_now-level score, got {s}");
        assert_eq!(tier_for(s, &m.tiers), Tier::ApplyNow);
    }

    #[test]
    fn off_lane_role_scores_low() {
        let m = model();
        let j = job("Warehouse Associate", "Lift boxes and manage inventory logistics");
        assert_eq!(tier_for(keyword_score(&j, &m), &m.tiers), Tier::Skip);
    }

    #[test]
    fn ai_does_not_match_inside_email() {
        let m = model();
        let j = job("Customer Support", "Answer email tickets");
        assert_eq!(tier_for(keyword_score(&j, &m), &m.tiers), Tier::Skip);
    }

    #[test]
    fn dealbreaker_penalizes() {
        let m = model();
        let with = job("Software Engineer", "Active security clearance required");
        let without = job("Software Engineer", "Great team");
        assert!(keyword_score(&with, &m) < keyword_score(&without, &m));
    }

    #[test]
    fn offmode_job_is_downranked() {
        let m = model();
        let mut remote = job("Software Engineer", "desc");
        remote.work_mode = "remote".into();
        let mut onsite = job("Software Engineer", "desc");
        onsite.work_mode = "onsite".into();
        assert!(keyword_score(&onsite, &m) < keyword_score(&remote, &m));
    }
}
