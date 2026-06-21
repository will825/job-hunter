# notifiers/discord.py — Job Hunter · Terra Echo Labs · v2.0
# Discord webhook — per-job embeds + run summary

import logging
import time
from datetime import datetime, timezone

import httpx

import config

logger = logging.getLogger(__name__)

RATE_LIMIT_DELAY = 0.5  # seconds between embeds


def _score_color(score: int) -> int:
    """Return Discord embed color based on score tier."""
    if score >= 20:
        return config.COLOR_EXCELLENT   # bright green
    if score >= 10:
        return config.COLOR_STRONG      # amber
    if score >= 5:
        return config.COLOR_GOOD        # blue
    return config.COLOR_TARGET          # grey


def _score_label(score: int) -> str:
    if score >= 20:
        return "Excellent Match"
    if score >= 10:
        return "Strong Match"
    if score >= 5:
        return "Good Match"
    return "Target Company"


def send_job_alert(job: dict, dry_run: bool = False) -> bool:
    """
    Send a single job embed to Discord.
    Returns True on success, False on failure.
    If dry_run=True, prints the embed data instead.
    """
    score = job["score"]
    keywords = job.get("keywords", "")
    description = (job.get("description") or "")[:300]
    if len(job.get("description") or "") > 300:
        description += "…"

    embed = {
        "title": job["title"],
        "url": job["url"],
        "color": _score_color(score),
        "fields": [
            {"name": "Company",          "value": job["company"],                     "inline": True},
            {"name": "Location",         "value": job.get("location") or "Not listed","inline": True},
            {"name": "Score",            "value": f"{score} — {_score_label(score)}", "inline": True},
            {"name": "Source",           "value": job.get("source", "unknown"),       "inline": True},
            {"name": "Keywords Matched", "value": keywords or "none",                 "inline": False},
            {"name": "Description",      "value": description or "No description",    "inline": False},
        ],
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "footer": {"text": "Job Hunter · Terra Echo Labs"},
    }

    payload = {"embeds": [embed]}

    if dry_run:
        print(f"[DRY RUN] Would send: {job['title']} @ {job['company']} (score={score})")
        return True

    if not config.DISCORD_WEBHOOK_URL:
        logger.warning("DISCORD_WEBHOOK_URL not set — skipping alert for: %s", job["title"])
        return False

    try:
        response = httpx.post(config.DISCORD_WEBHOOK_URL, json=payload, timeout=15)
        response.raise_for_status()
        logger.info("Discord alert sent: %s @ %s (score=%d)", job["title"], job["company"], score)
        return True
    except Exception as e:
        logger.error("Discord send error for '%s': %s", job["title"], e)
        return False


def send_summary(
    run_started: str,
    jobs_found: int,
    jobs_new: int,
    jobs_alerted: int,
    errors: list[str],
    dry_run: bool = False,
) -> bool:
    """Send a run summary embed to Discord."""
    color = 0x00FF00 if not errors else 0xFF6B35
    error_text = "\n".join(errors[:10]) if errors else "None"

    embed = {
        "title": "Job Hunter Run Complete",
        "color": color,
        "fields": [
            {"name": "Run Started",   "value": run_started,          "inline": True},
            {"name": "Jobs Found",    "value": str(jobs_found),      "inline": True},
            {"name": "New Jobs",      "value": str(jobs_new),        "inline": True},
            {"name": "Alerts Sent",   "value": str(jobs_alerted),    "inline": True},
            {"name": "Errors",        "value": error_text,           "inline": False},
        ],
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "footer": {"text": "Job Hunter · Terra Echo Labs"},
    }

    payload = {"embeds": [embed]}

    if dry_run:
        print(f"[DRY RUN] Summary: found={jobs_found} new={jobs_new} alerted={jobs_alerted}")
        return True

    if not config.DISCORD_WEBHOOK_URL:
        logger.warning("DISCORD_WEBHOOK_URL not set — skipping summary")
        return False

    try:
        response = httpx.post(config.DISCORD_WEBHOOK_URL, json=payload, timeout=15)
        response.raise_for_status()
        logger.info("Discord summary sent")
        return True
    except Exception as e:
        logger.error("Discord summary error: %s", e)
        return False


def send_alerts_batch(jobs: list[dict], dry_run: bool = False) -> int:
    """
    Send Discord alerts for a list of jobs.
    Jobs should be pre-sorted score ASC (lowest first, best lands last = top of channel).
    Returns count of alerts successfully sent.
    """
    sent = 0
    for job in jobs:
        success = send_job_alert(job, dry_run=dry_run)
        if success:
            sent += 1
        if not dry_run:
            time.sleep(RATE_LIMIT_DELAY)
    return sent
