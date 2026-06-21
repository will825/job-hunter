# scrapers/remoteok_wwr.py — Job Hunter · Terra Echo Labs · v2.0
# RemoteOK JSON API + We Work Remotely RSS scrapers

import logging
import re

import feedparser
import httpx

logger = logging.getLogger(__name__)

REMOTEOK_API = "https://remoteok.com/api"

WWR_FEEDS = [
    "https://weworkremotely.com/categories/remote-programming-jobs.rss",
    "https://weworkremotely.com/categories/remote-design-jobs.rss",
    "https://weworkremotely.com/categories/remote-full-stack-programming-jobs.rss",
]

HEADERS = {
    "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    "Accept": "application/json",
}


def _strip_html(text: str) -> str:
    """Remove HTML tags from a string."""
    return re.sub(r"<[^>]+>", " ", text or "").strip()


def scrape_remoteok() -> list[dict]:
    """Fetch all jobs from RemoteOK JSON API."""
    jobs = []
    try:
        response = httpx.get(REMOTEOK_API, headers=HEADERS, timeout=30, follow_redirects=True)
        response.raise_for_status()
        data = response.json()

        # First element is metadata, skip it
        for item in data[1:]:
            if not isinstance(item, dict):
                continue
            title = item.get("position", "")
            company = item.get("company", "")
            url = item.get("url", "")
            location = item.get("location", "Remote")
            description = _strip_html(item.get("description", ""))

            if not title or not url:
                continue

            jobs.append({
                "title": title,
                "company": company,
                "location": location or "Remote",
                "url": url,
                "source": "RemoteOK",
                "description": description,
            })

        logger.info("RemoteOK: fetched %d jobs", len(jobs))
    except Exception as e:
        logger.error("RemoteOK scrape error: %s", e)

    return jobs


def scrape_weworkremotely() -> list[dict]:
    """Fetch jobs from We Work Remotely RSS feeds."""
    jobs = []
    for feed_url in WWR_FEEDS:
        try:
            feed = feedparser.parse(feed_url)
            for entry in feed.entries:
                title = entry.get("title", "")
                url = entry.get("link", "")
                description = _strip_html(entry.get("summary", ""))

                # WWR title format: "Company Name: Job Title"
                if ": " in title:
                    company, job_title = title.split(": ", 1)
                else:
                    company = "Unknown"
                    job_title = title

                if not job_title or not url:
                    continue

                jobs.append({
                    "title": job_title,
                    "company": company.strip(),
                    "location": "Remote",
                    "url": url,
                    "source": "WeWorkRemotely",
                    "description": description,
                })

            logger.info("WWR feed %s: fetched %d entries", feed_url, len(feed.entries))
        except Exception as e:
            logger.error("WWR feed error (%s): %s", feed_url, e)

    logger.info("WeWorkRemotely total: %d jobs", len(jobs))
    return jobs
