# scrapers/avixa_scraper.py — Job Hunter · Terra Echo Labs · v2.0
# AVIXA Job Board scraper — TOP PRIORITY AV industry board
# AVIXA (jobs.avixa.org) runs on YM Careers / Multiview and exposes an RSS feed.
# Covers: Applications Engineer, AV Design, Technical Sales, AV Technician roles.

import logging
import re

import feedparser

logger = logging.getLogger(__name__)

AVIXA_RSS = "https://avixa.careerwebsite.com/jobs/rss"


def _strip_html(text: str) -> str:
    return re.sub(r"<[^>]+>", " ", text or "").strip()


def scrape_avixa() -> list[dict]:
    """Fetch jobs from the AVIXA Job Board RSS feed."""
    jobs = []
    try:
        feed = feedparser.parse(AVIXA_RSS)

        if feed.bozo and not feed.entries:
            logger.warning("AVIXA RSS parse issue: %s", feed.bozo_exception)
            return jobs

        for entry in feed.entries:
            title = entry.get("title", "").strip()
            url = entry.get("link", "").strip()
            description = _strip_html(entry.get("summary", entry.get("description", "")))

            # AVIXA RSS includes company in author or tags — fall back gracefully
            company = ""
            if hasattr(entry, "author") and entry.author:
                company = entry.author.strip()
            elif hasattr(entry, "tags") and entry.tags:
                company = entry.tags[0].get("term", "").strip()

            if not title or not url:
                continue

            jobs.append({
                "title": title,
                "company": company or "AVIXA Board",
                "location": entry.get("location", ""),
                "url": url,
                "source": "AVIXA",
                "description": description[:2000],
            })

        logger.info("AVIXA: fetched %d jobs", len(jobs))

    except Exception as e:
        logger.error("AVIXA scrape error: %s", e)

    return jobs
