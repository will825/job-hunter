//! Daily-digest email (Phase 4) via Resend — isolated and removable.
//!
//! Resend is a free email API: you send with an API key (no email password, no
//! SMTP). Create a key at resend.com and put it in `.env` as `RESEND_API_KEY`.
//! On the free tier you can email yourself from the default `onboarding@resend.dev`
//! sender with no domain setup. Config lives in `profile.toml [email]`; set
//! `enabled = false` to turn it off. To remove the feature entirely, delete this
//! file and its calls in `main.rs`.

use anyhow::{Context, Result};
use serde_json::json;

use crate::db::DigestJob;
use crate::profile::Profile;

/// Resolved email configuration (profile + env-provided API key).
pub struct EmailConfig {
    pub enabled: bool,
    pub from: String,
    pub to: String,
    pub max_jobs: i64,
    pub api_key: Option<String>,
}

impl EmailConfig {
    pub fn from_profile(p: &Profile) -> Self {
        EmailConfig {
            enabled: p.email.enabled,
            from: p.email.from.clone(),
            to: p.email.to.clone(),
            max_jobs: p.email.max_jobs,
            api_key: std::env::var("RESEND_API_KEY").ok().filter(|k| !k.trim().is_empty()),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.enabled
            && !self.from.trim().is_empty()
            && !self.to.trim().is_empty()
            && self.api_key.is_some()
    }

    pub fn why_not_ready(&self) -> &'static str {
        if !self.enabled {
            "email disabled in profile.toml ([email] enabled = false)"
        } else if self.to.trim().is_empty() {
            "no [email] to-address set in profile.toml"
        } else if self.api_key.is_none() {
            "no RESEND_API_KEY set in the environment"
        } else {
            "ready"
        }
    }
}

/// How the scan behind a digest went, shown in the email's footer.
#[derive(Debug, Clone, Default)]
pub struct RunHealth {
    pub boards_scanned: usize,
    pub boards_failed: usize,
    pub llm_enabled: bool,
    pub llm_scored: usize,
    pub llm_failed: usize,
    /// The digest skipped its own scan (a web scan was mid AI-scoring) and was
    /// built from the jobs already stored.
    pub from_db_only: bool,
}

impl RunHealth {
    fn line(&self) -> String {
        if self.from_db_only {
            return "Run health: sent from stored jobs — a web scan was still AI-scoring, \
                    so this digest skipped its own fetch."
                .to_string();
        }
        let ai = if self.llm_enabled {
            format!("AI scored {}, failed {}", self.llm_scored, self.llm_failed)
        } else {
            "AI scoring off".to_string()
        };
        format!(
            "Run health: {} boards scanned, {} failed · {ai}",
            self.boards_scanned, self.boards_failed
        )
    }
}

/// Run context rendered around the job list.
#[derive(Debug, Clone, Default)]
pub struct DigestNotes {
    /// Set when AI scoring failed and the digest fell back to keyword matches;
    /// the error is shown in a banner at the top.
    pub ai_failure: Option<String>,
    pub health: RunHealth,
}

impl DigestNotes {
    fn banner(&self) -> Option<String> {
        self.ai_failure
            .as_ref()
            .map(|e| format!("AI scoring failed this run: {e}. These are keyword matches only."))
    }
}

/// Send a digest of the given jobs via Resend. Assumes `cfg.is_ready()`.
pub async fn send_digest(
    cfg: &EmailConfig,
    client: &reqwest::Client,
    jobs: &[DigestJob],
    notes: &DigestNotes,
) -> Result<()> {
    let (subject, html, text) = compose(jobs, notes);
    let key = cfg.api_key.as_ref().context("no RESEND_API_KEY")?;

    let body = json!({
        "from": cfg.from,
        "to": [cfg.to],
        "subject": subject,
        "html": html,
        "text": text,
    });

    let resp = client
        .post("https://api.resend.com/emails")
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .context("calling Resend")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        anyhow::bail!("Resend returned {status}: {}", detail.chars().take(300).collect::<String>());
    }
    Ok(())
}

/// Build (subject, html, text) for a digest of `jobs`.
pub fn compose(jobs: &[DigestJob], notes: &DigestNotes) -> (String, String, String) {
    let apply: Vec<&DigestJob> = jobs.iter().filter(|j| j.tier == "apply_now").collect();
    let strong: Vec<&DigestJob> = jobs.iter().filter(|j| j.tier == "strong").collect();

    let subject = format!(
        "Job Hunter: {} new match{} ({} apply-now)",
        jobs.len(),
        if jobs.len() == 1 { "" } else { "es" },
        apply.len()
    );

    // --- Plain text ---
    let mut text = format!("{}\n\n", subject);
    if let Some(b) = notes.banner() {
        text.push_str(&format!("⚠ {b}\n\n"));
    }
    for (label, group) in [("APPLY NOW", &apply), ("STRONG", &strong)] {
        if group.is_empty() {
            continue;
        }
        text.push_str(&format!("{label}\n"));
        for j in group.iter() {
            text.push_str(&format!("  [{}] {} — {} ({})\n", j.score, j.company, j.title, j.work_mode));
            if let Some(r) = &j.reasoning {
                text.push_str(&format!("      {}\n", r));
            }
            text.push_str(&format!("      {}\n", j.url));
        }
        text.push('\n');
    }
    text.push_str(&format!("—\n{}\n", notes.health.line()));

    // --- HTML ---
    let mut html = String::from(
        "<div style=\"font-family:-apple-system,system-ui,sans-serif;max-width:640px;margin:0 auto;color:#1a1a1a\">",
    );
    html.push_str(&format!("<h2 style=\"margin:0 0 4px\">{}</h2>", esc(&subject)));
    if let Some(b) = notes.banner() {
        html.push_str(&format!(
            "<div style=\"background:#fff4e5;border:1px solid #f0b35a;color:#7a4b00;border-radius:6px;padding:10px 12px;margin:8px 0 12px;font-size:13px\"><b>⚠</b> {}</div>",
            esc(&b)
        ));
    }
    html.push_str("<p style=\"color:#666;font-size:13px;margin:0 0 16px\">New matches since your last digest, ranked to your profile.</p>");
    for (label, color, group) in [("Apply now", "#128a4f", &apply), ("Strong", "#1f6feb", &strong)] {
        if group.is_empty() {
            continue;
        }
        html.push_str(&format!(
            "<h3 style=\"color:{color};border-bottom:1px solid #eee;padding-bottom:4px\">{label} ({})</h3>",
            group.len()
        ));
        for j in group.iter() {
            html.push_str("<div style=\"margin:0 0 14px\">");
            html.push_str(&format!(
                "<div><a href=\"{}\" style=\"color:#1f6feb;text-decoration:none;font-weight:600;font-size:15px\">{}</a></div>",
                esc(&j.url),
                esc(&j.title)
            ));
            html.push_str(&format!(
                "<div style=\"color:#555;font-size:13px\"><b>{}</b> · {} · {} · fit {}</div>",
                esc(&j.company),
                esc(&j.work_mode),
                esc(&j.location),
                j.score
            ));
            if let Some(r) = &j.reasoning {
                html.push_str(&format!("<div style=\"color:#777;font-size:12px;margin-top:2px\">{}</div>", esc(r)));
            }
            html.push_str("</div>");
        }
    }
    html.push_str(&format!(
        "<p style=\"color:#999;font-size:11px;margin-top:20px\">{}<br>Sent by your local Job Hunter.</p></div>",
        esc(&notes.health.line())
    ));

    (subject, html, text)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dj(tier: &str, company: &str, title: &str) -> DigestJob {
        DigestJob {
            id: "x".into(),
            company: company.into(),
            title: title.into(),
            url: "http://x".into(),
            tier: tier.into(),
            work_mode: "remote".into(),
            location: "US".into(),
            score: 90,
            reasoning: Some("great fit".into()),
        }
    }

    #[test]
    fn compose_groups_and_counts() {
        let jobs = vec![dj("apply_now", "A", "Rust Engineer"), dj("strong", "B", "PM")];
        let (subject, html, text) = compose(&jobs, &DigestNotes::default());
        assert!(subject.contains("2 new matches"));
        assert!(subject.contains("1 apply-now"));
        assert!(html.contains("Apply now (1)"));
        assert!(text.contains("Rust Engineer"));
    }

    #[test]
    fn banner_shows_when_ai_failed() {
        let notes = DigestNotes {
            ai_failure: Some("Groq returned 404 Not Found: <model> decommissioned".into()),
            health: RunHealth { boards_scanned: 33, boards_failed: 2, llm_enabled: true, llm_scored: 0, llm_failed: 3, from_db_only: false },
        };
        let (_subject, html, text) = compose(&[dj("strong", "A", "Rust Engineer")], &notes);
        let banner = "AI scoring failed this run: Groq returned 404 Not Found: <model> decommissioned. \
                      These are keyword matches only.";
        // Banner comes before the job list in the text body...
        let at = text.find(banner).expect("banner in text");
        assert!(at < text.find("Rust Engineer").unwrap());
        // ...and is HTML-escaped in the html body.
        assert!(html.contains("AI scoring failed this run: Groq returned 404 Not Found: &lt;model&gt; decommissioned."));
        assert!(text.contains("Run health: 33 boards scanned, 2 failed · AI scored 0, failed 3"));
    }

    #[test]
    fn no_banner_when_llm_disabled() {
        let notes = DigestNotes {
            ai_failure: None,
            health: RunHealth { boards_scanned: 5, boards_failed: 0, llm_enabled: false, ..Default::default() },
        };
        let (_s, html, text) = compose(&[dj("strong", "A", "Rust Engineer")], &notes);
        assert!(!text.contains("AI scoring failed"));
        assert!(!html.contains("AI scoring failed"));
        assert!(text.contains("Run health: 5 boards scanned, 0 failed · AI scoring off"));
        assert!(html.contains("AI scoring off"));
    }

    #[test]
    fn db_only_digest_says_it_skipped_the_fetch() {
        let health = RunHealth { from_db_only: true, llm_enabled: true, ..Default::default() };
        assert!(health.line().contains("skipped its own fetch"));
    }
}
