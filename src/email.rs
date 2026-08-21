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

/// Send a digest of the given jobs via Resend. Assumes `cfg.is_ready()`.
pub async fn send_digest(cfg: &EmailConfig, client: &reqwest::Client, jobs: &[DigestJob]) -> Result<()> {
    let (subject, html, text) = compose(jobs);
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
pub fn compose(jobs: &[DigestJob]) -> (String, String, String) {
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

    // --- HTML ---
    let mut html = String::from(
        "<div style=\"font-family:-apple-system,system-ui,sans-serif;max-width:640px;margin:0 auto;color:#1a1a1a\">",
    );
    html.push_str(&format!("<h2 style=\"margin:0 0 4px\">{}</h2>", esc(&subject)));
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
    html.push_str("<p style=\"color:#999;font-size:11px;margin-top:20px\">Sent by your local Job Hunter.</p></div>");

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
        let (subject, html, text) = compose(&jobs);
        assert!(subject.contains("2 new matches"));
        assert!(subject.contains("1 apply-now"));
        assert!(html.contains("Apply now (1)"));
        assert!(text.contains("Rust Engineer"));
    }
}
