#!/usr/bin/env python3
# run.py — Job Hunter · Terra Echo Labs · v2.0
# Main orchestrator — scrape → score → save → notify

import argparse
import logging
import sys
from datetime import datetime, timezone

# Ensure logs go to stdout → /tmp/jobhunter.log when run via launchd
logging.root.handlers = []

import config
import database
import scorer
from notifiers import discord
from scrapers import company_ats, remoteok_wwr
from scrapers.avixa_scraper import scrape_avixa
from scrapers.linkedin_scraper import scrape_linkedin

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)-8s %(name)s — %(message)s",
    datefmt="%Y-%m-%dT%H:%M:%SZ",
    stream=sys.stdout,
)
logger = logging.getLogger("job_hunter")


def parse_args():
    parser = argparse.ArgumentParser(description="Job Hunter — Automated Audio Industry Job Monitor")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Scrape and score jobs, print results, but send zero Discord messages.",
    )
    parser.add_argument(
        "--summary",
        action="store_true",
        help="Send only the run summary embed (no individual job alerts).",
    )
    parser.add_argument(
        "--companies",
        action="store_true",
        help="Scrape only company ATS boards (skip RemoteOK/WWR).",
    )
    return parser.parse_args()


def main():
    args = parse_args()
    dry_run = args.dry_run

    if not config.DISCORD_WEBHOOK_URL and not dry_run:
        logger.warning(
            "DISCORD_WEBHOOK_URL is not set in config.py. "
            "Discord alerts will be skipped. Use --dry-run to suppress this warning."
        )

    run_started = datetime.now(timezone.utc).isoformat()
    logger.info("=== Job Hunter run starting at %s ===", run_started)

    # ── Initialize DB ────────────────────────────────────────────────
    database.init_db()

    # ── Collect all raw jobs ─────────────────────────────────────────
    all_raw: list[dict] = []
    errors: list[str] = []

    if not args.companies:
        try:
            remoteok_jobs = remoteok_wwr.scrape_remoteok()
            all_raw.extend(remoteok_jobs)
        except Exception as e:
            msg = f"RemoteOK scraper failed: {e}"
            logger.error(msg)
            errors.append(msg)

        try:
            wwr_jobs = remoteok_wwr.scrape_weworkremotely()
            all_raw.extend(wwr_jobs)
        except Exception as e:
            msg = f"WWR scraper failed: {e}"
            logger.error(msg)
            errors.append(msg)

        try:
            avixa_jobs = scrape_avixa()
            all_raw.extend(avixa_jobs)
        except Exception as e:
            msg = f"AVIXA scraper failed: {e}"
            logger.error(msg)
            errors.append(msg)

        try:
            linkedin_jobs = scrape_linkedin()
            all_raw.extend(linkedin_jobs)
            logger.info("LinkedIn: %d jobs", len(linkedin_jobs))
        except Exception as e:
            msg = f"LinkedIn scraper failed: {e}"
            logger.error(msg)
            errors.append(msg)

    try:
        ats_jobs = company_ats.scrape_all_companies()
        all_raw.extend(ats_jobs)
    except Exception as e:
        msg = f"ATS scraper failed: {e}"
        logger.error(msg)
        errors.append(msg)

    logger.info("Total raw jobs collected: %d", len(all_raw))

    # ── Score + save each job ────────────────────────────────────────
    target_names_lower = {c["name"].lower() for c in config.TARGET_COMPANIES}
    jobs_new = 0
    jobs_found = len(all_raw)

    for job in all_raw:
        title = job.get("title", "")
        company = job.get("company", "")
        location = job.get("location", "")
        url = job.get("url", "")
        source = job.get("source", "")
        description = job.get("description", "")

        # Score
        score, matched_keywords = scorer.score_job(title, description, company)

        # Target company floor: minimum score=1 regardless of keywords
        is_target = company.lower() in target_names_lower
        if is_target and score == 0:
            score = 1
            matched_keywords = [f"[{company} target]"]

        # Save (dedup via SHA-1 hash)
        saved = database.save_job(
            title=title,
            company=company,
            location=location,
            url=url,
            source=source,
            score=score,
            keywords=matched_keywords,
            description=description,
        )
        if saved:
            jobs_new += 1

    logger.info("Jobs new (not seen before): %d", jobs_new)

    # ── Fetch unnotified jobs above threshold and send alerts ────────
    if not args.summary:
        unnotified = database.get_unnotified_jobs(min_score=config.SCORE_THRESHOLD)
        # Convert Row objects to dicts for Discord notifier
        jobs_to_alert = [dict(row) for row in unnotified]

        logger.info("Unnotified jobs above threshold (score >= %d): %d", config.SCORE_THRESHOLD, len(jobs_to_alert))

        if dry_run:
            for job in jobs_to_alert:
                discord.send_job_alert(job, dry_run=True)
            jobs_alerted = len(jobs_to_alert)
        else:
            # Send lowest score first — best job lands last (top of Discord feed)
            jobs_to_alert_sorted = sorted(jobs_to_alert, key=lambda j: j["score"])
            jobs_alerted = discord.send_alerts_batch(jobs_to_alert_sorted, dry_run=False)

            # Mark sent jobs as notified
            for job in jobs_to_alert_sorted:
                database.mark_notified(job["id"])
    else:
        jobs_alerted = 0

    # ── Send summary ─────────────────────────────────────────────────
    discord.send_summary(
        run_started=run_started,
        jobs_found=jobs_found,
        jobs_new=jobs_new,
        jobs_alerted=jobs_alerted,
        errors=errors,
        dry_run=dry_run,
    )

    # ── Log run ──────────────────────────────────────────────────────
    finished_at = datetime.now(timezone.utc).isoformat()
    database.log_run(
        started_at=run_started,
        finished_at=finished_at,
        jobs_found=jobs_found,
        jobs_new=jobs_new,
        jobs_alerted=jobs_alerted,
        errors="; ".join(errors),
    )

    logger.info(
        "=== Run complete: found=%d new=%d alerted=%d errors=%d ===",
        jobs_found, jobs_new, jobs_alerted, len(errors),
    )


if __name__ == "__main__":
    main()
