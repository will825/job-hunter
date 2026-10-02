//! Structured classification of a job into filterable fields:
//! `work_mode` (remote/hybrid/onsite), `region`, and `seniority`.
//!
//! These are derived **once at ingest** so the future UI and the email digest
//! can filter on clean columns instead of re-parsing messy location strings.
//! Where an ATS gives a reliable structured signal (Ashby's `isRemote`,
//! Ashby/Lever `workplaceType`) we prefer it and fall back to text otherwise.
//!
//! Everything here is a best-effort heuristic — when the signal is genuinely
//! ambiguous we return `"unknown"` rather than guess, so a filter never
//! silently hides a job on a bad guess.

use serde_json::Value;

use crate::models::Job;

/// The three derived fields for one job.
pub struct Classification {
    pub work_mode: String, // remote | hybrid | onsite | unknown
    pub region: String,    // us | canada | uk | emea | apac | latam | unknown
    pub seniority: String, // junior | mid | senior | staff | lead | unknown
}

/// Classify a fetched job. Reads its `raw_json` for source-specific signals.
pub fn classify(job: &Job) -> Classification {
    let raw: Value = serde_json::from_str(&job.raw_json).unwrap_or(Value::Null);
    Classification {
        work_mode: work_mode(job, &raw),
        region: region(&job.location, &raw),
        seniority: seniority(&job.title),
    }
}

/// The only `raw_json` fields [`classify`] reads. Everything else in a
/// board's payload is dropped before storage (see [`slim_raw_json`]).
const RAW_KEYS: &[&str] = &["isRemote", "workplaceType", "country"];

/// Cut a raw board payload down to the fields [`classify`] reads, so stored
/// jobs don't carry the whole API response. Classifying the slim form gives
/// the same result as the full one. Unparseable or non-object input → `{}`.
pub fn slim_raw_json(raw: &str) -> String {
    let Ok(Value::Object(full)) = serde_json::from_str::<Value>(raw) else {
        return "{}".to_string();
    };
    let slim: serde_json::Map<String, Value> = RAW_KEYS
        .iter()
        .filter_map(|k| full.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    Value::Object(slim).to_string()
}

fn work_mode(job: &Job, raw: &Value) -> String {
    let loc = job.location.to_lowercase();
    let title = job.title.to_lowercase();
    // Structured signals first.
    let is_remote = raw.get("isRemote").and_then(Value::as_bool);
    let workplace = raw
        .get("workplaceType")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();

    let text_has = |needle: &str| loc.contains(needle) || title.contains(needle) || workplace.contains(needle);

    // Hybrid is checked first: a "hybrid remote" posting is hybrid, not remote.
    if text_has("hybrid") {
        "hybrid".to_string()
    } else if is_remote == Some(true) || text_has("remote") || workplace == "remote" {
        "remote".to_string()
    } else if workplace.contains("on") || is_remote == Some(false) || looks_like_place(&loc) {
        "onsite".to_string()
    } else {
        "unknown".to_string()
    }
}

/// A location that names a concrete place (has letters, isn't just "remote"/
/// "unspecified") is treated as an on-site signal.
fn looks_like_place(loc: &str) -> bool {
    let l = loc.trim();
    !l.is_empty()
        && l != "unspecified"
        && l.chars().any(|c| c.is_alphabetic())
        && !l.contains("remote")
}

fn region(location: &str, raw: &Value) -> String {
    // Combine the location field with any explicit country the ATS provides.
    let mut hay = location.to_lowercase();
    if let Some(country) = raw.get("country").and_then(Value::as_str) {
        hay.push(' ');
        hay.push_str(&country.to_lowercase());
    }

    // Ordered most-specific → least, first match wins.
    const TABLE: &[(&str, &[&str])] = &[
        ("uk", &["united kingdom", "london", "england", "scotland", "wales", "manchester", " uk"]),
        ("canada", &["canada", "toronto", "vancouver", "montreal", "ontario"]),
        (
            "us",
            &[
                "united states", "u.s", "usa", "new york", "san francisco", "california",
                "texas", "seattle", "boston", "los angeles", "chicago", "austin", "denver",
                "remote - us", "remote us", " us", "atlanta", "washington",
            ],
        ),
        (
            "emea",
            &[
                "germany", "france", "spain", "netherlands", "ireland", "sweden", "poland",
                "italy", "belgium", "europe", "berlin", "paris", "amsterdam", "stockholm",
                "dublin", "madrid", "emea", "switzerland", "portugal", "denmark", "norway",
            ],
        ),
        (
            "apac",
            &[
                "australia", "japan", "singapore", "india", "tokyo", "sydney", "apac", "korea",
                "china", "hong kong", "taiwan", "new zealand", "bangalore", "melbourne",
            ],
        ),
        ("latam", &["brazil", "mexico", "argentina", "latam", "chile", "colombia", "sao paulo"]),
    ];

    for (region, needles) in TABLE {
        if needles.iter().any(|n| hay.contains(n)) {
            return region.to_string();
        }
    }
    "unknown".to_string()
}

fn seniority(title: &str) -> String {
    let t = format!(" {} ", title.to_lowercase());
    let has = |w: &str| t.contains(&format!(" {w} ")) || t.contains(&format!(" {w},")) || t.contains(&format!(" {w}-"));

    // Order matters: check the most senior markers first.
    if has("principal") || has("staff") || t.contains("distinguished") {
        "staff".to_string()
    } else if has("lead") || has("director") || has("head") || t.contains("vp") || t.contains("president") {
        "lead".to_string()
    } else if has("senior") || has("sr") || has("sr.") || t.contains("senior") {
        "senior".to_string()
    } else if has("junior") || has("jr") || has("intern") || has("entry") || has("graduate") || has("associate") || has("apprentice") {
        "junior".to_string()
    } else if t.contains("engineer") || t.contains("manager") || t.contains("designer") || t.contains("developer") || t.contains("analyst") || t.contains("scientist") {
        // A normal IC/role title with no seniority marker → mid-level.
        "mid".to_string()
    } else {
        "unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_with(location: &str, title: &str, raw: &str) -> Job {
        Job::new("Co", title, location, "http://x", "ashby", "desc", None, raw)
    }

    #[test]
    fn ashby_is_remote_flag_wins_over_place_location() {
        // The elevenlabs case: location "India" but isRemote true → remote.
        let j = job_with("India", "Account Manager", r#"{"isRemote":true}"#);
        assert_eq!(classify(&j).work_mode, "remote");
    }

    #[test]
    fn slim_raw_json_classifies_the_same() {
        let full = r#"{"workplaceType":"OnSite","country":"United Kingdom","extra":{"big":[1,2,3]}}"#;
        let slim = slim_raw_json(full);
        assert_eq!(slim, r#"{"country":"United Kingdom","workplaceType":"OnSite"}"#);
        let (a, b) = (classify(&job_with("Anywhere", "Eng", full)), classify(&job_with("Anywhere", "Eng", &slim)));
        assert_eq!((a.work_mode, a.region), (b.work_mode, b.region));
        assert_eq!(slim_raw_json("[1]"), "{}");
        assert_eq!(slim_raw_json("not json"), "{}");
    }

    #[test]
    fn hybrid_beats_remote() {
        let j = job_with("London (Hybrid, some remote)", "Engineer", "{}");
        assert_eq!(classify(&j).work_mode, "hybrid");
    }

    #[test]
    fn concrete_place_is_onsite() {
        let j = job_with("Stockholm", "Engineer", r#"{"isRemote":false}"#);
        assert_eq!(classify(&j).work_mode, "onsite");
    }

    #[test]
    fn regions_map() {
        assert_eq!(region("Remote - U.S.", &Value::Null), "us");
        assert_eq!(region("London", &Value::Null), "uk");
        assert_eq!(region("Stockholm", &Value::Null), "emea");
        assert_eq!(region("Tokyo", &Value::Null), "apac");
        assert_eq!(region("Remote", &Value::Null), "unknown");
    }

    #[test]
    fn seniority_levels() {
        assert_eq!(seniority("Senior Product Manager"), "senior");
        assert_eq!(seniority("Staff Software Engineer"), "staff");
        assert_eq!(seniority("Engineering Lead"), "lead");
        assert_eq!(seniority("Junior Analyst"), "junior");
        assert_eq!(seniority("Software Engineer"), "mid");
        assert_eq!(seniority("Account Executive"), "unknown");
    }
}
