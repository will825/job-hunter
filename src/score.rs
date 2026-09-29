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
//! description, because titles are denser signal. Positives are capped three
//! ways so generous matching can't flood the top tier: only the best
//! [`MAX_TITLE_MATCHES`] title terms count, description positives are capped
//! (`desc_positive_cap`) so company boilerplate can't inflate an off-target
//! role, and the total positive score is capped at [`POSITIVE_CAP`] before
//! penalties. Penalties (dealbreakers, off-mode/region) always apply in full.

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

/// Only this many positive title matches count (the highest-weighted ones), so
/// a title that lists skills ("C#, Python, Rust, SQL Lead Software Engineer")
/// can't stack them.
const MAX_TITLE_MATCHES: usize = 2;
/// Cap on the total positive score (title + description) before penalties, so
/// an off-mode/off-region penalty is meaningful next to it.
const POSITIVE_CAP: i32 = 60;

/// Compute the keyword score for a job against a compiled profile model.
///
/// Reads `job.work_mode` / `job.region`, so classification must run first.
pub fn keyword_score(job: &Job, model: &ScoringModel) -> i64 {
    let title = job.title.to_lowercase();
    let desc = job.description.to_lowercase();
    let title_tokens = tokenize(&title);
    let desc_tokens = tokenize(&desc);

    let mut title_positive: Vec<i32> = Vec::new();
    let mut desc_positive = 0i32;
    let mut negative = 0i32; // penalties always apply in full, uncapped

    for (term, weight) in &model.weights {
        if term_present(term, &title, &title_tokens) {
            if *weight >= 0 {
                title_positive.push(*weight);
            } else {
                negative += weight * model.title_multiplier;
            }
        }
        if term_present(term, &desc, &desc_tokens) {
            if *weight >= 0 {
                desc_positive += weight;
            } else {
                negative += weight;
            }
        }
    }

    // Only the best few title matches count.
    title_positive.sort_unstable_by(|a, b| b.cmp(a));
    let title_score: i32 =
        title_positive.iter().take(MAX_TITLE_MATCHES).sum::<i32>() * model.title_multiplier;
    let positive = (title_score + desc_positive.min(model.desc_positive_cap)).min(POSITIVE_CAP);
    let mut score = positive + negative;

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

/// The tier for a scored job: [`tier_for`] on its score, except that with
/// `onsite_mode = "hide"` an onsite job is always `Skip`.
pub fn job_tier(job: &Job, score: i64, model: &ScoringModel) -> Tier {
    if model.hide_onsite && job.work_mode == "onsite" {
        Tier::Skip
    } else {
        tier_for(score, &model.tiers)
    }
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

    const PROFILE: &str = r#"
        target_roles = ["product engineer", "software engineer"]
        interests = ["music", "audio"]
        [skills]
        strong = ["rust", "dsp", "python"]
        medium = ["react", "ai"]
        common = ["sql", "java", "git"]
        [preferences]
        work_modes = ["remote", "hybrid"]
        regions = ["us"]
        dealbreakers = ["security clearance"]
        "#;

    fn model() -> ScoringModel {
        let p: Profile = toml::from_str(PROFILE).unwrap();
        p.compile()
    }

    fn model_with(extra_prefs: &str) -> ScoringModel {
        let text = PROFILE.replace("[preferences]", &format!("[preferences]\n{extra_prefs}"));
        let p: Profile = toml::from_str(&text).unwrap();
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
    fn title_listing_four_skills_scores_no_higher_than_two() {
        let m = model();
        let four = job("C#, Python, Rust, SQL Lead Software Engineer", "desc");
        let two = job("Rust Software Engineer", "desc");
        assert!(
            keyword_score(&four, &m) <= keyword_score(&two, &m),
            "four = {}, two = {}",
            keyword_score(&four, &m),
            keyword_score(&two, &m)
        );
    }

    #[test]
    fn positive_score_is_capped_before_penalties() {
        let m = model();
        let desc = "Rust DSP Python React AI music audio SQL git product engineer";
        let mut remote = job("Rust Product Engineer", desc);
        remote.work_mode = "remote".into();
        let mut onsite = job("Rust Product Engineer", desc);
        onsite.work_mode = "onsite".into();
        assert_eq!(keyword_score(&remote, &m), POSITIVE_CAP as i64);
        assert_eq!(keyword_score(&onsite, &m), (POSITIVE_CAP - 25) as i64, "penalty applies after the cap");
    }

    #[test]
    fn common_skills_count_less_than_strong() {
        let m = model();
        let common = job("Engineer", "sql");
        let strong = job("Engineer", "rust");
        assert!(keyword_score(&common, &m) < keyword_score(&strong, &m));
        assert!(keyword_score(&common, &m) > 0);
    }

    #[test]
    fn onsite_job_is_skipped_when_hidden() {
        let hide = model_with(r#"onsite_mode = "hide""#);
        let mut onsite = job("Rust Product Engineer", "Build Rust DSP tools for music creators");
        onsite.work_mode = "onsite".into();
        let s = keyword_score(&onsite, &hide);
        assert!(tier_for(s, &hide.tiers) != Tier::Skip, "score alone would not skip it");
        assert_eq!(job_tier(&onsite, s, &hide), Tier::Skip);

        // Default ("penalize"): the same job keeps its score-based tier.
        let penalize = model();
        let s = keyword_score(&onsite, &penalize);
        assert_eq!(job_tier(&onsite, s, &penalize), tier_for(s, &penalize.tiers));
        // Hybrid/remote/unknown jobs are never hidden.
        let mut hybrid = onsite.clone();
        hybrid.work_mode = "hybrid".into();
        let s = keyword_score(&hybrid, &hide);
        assert_eq!(job_tier(&hybrid, s, &hide), tier_for(s, &hide.tiers));
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
