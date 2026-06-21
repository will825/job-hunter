# scrapers/company_ats.py — Job Hunter · Terra Echo Labs · v2.0
# Greenhouse + Lever + Ashby ATS scrapers for all target companies

import logging
import re

import httpx

import config

logger = logging.getLogger(__name__)

GREENHOUSE_URL = "https://boards-api.greenhouse.io/v1/boards/{token}/jobs?content=true"
LEVER_URL      = "https://api.lever.co/v0/postings/{token}?mode=json"
ASHBY_GRAPHQL  = "https://jobs.ashbyhq.com/api/non-user-graphql?op=ApiJobBoardWithTeams"

# GraphQL query used by Ashby's public job board
_ASHBY_QUERY = """
query ApiJobBoardWithTeams($organizationHostedJobsPageName: String!) {
  jobBoard: jobBoardWithTeams(
    organizationHostedJobsPageName: $organizationHostedJobsPageName
  ) {
    jobPostings {
      id
      title
      locationName
      workplaceType
      employmentType
      secondaryLocations {
        locationName
      }
    }
  }
}
"""

HEADERS = {
    "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
}

ASHBY_HEADERS = {
    "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    "Content-Type": "application/json",
    "apollographql-client-name": "frontend_non_user",
    "apollographql-client-version": "0.1.0",
}


def _strip_html(text: str) -> str:
    """Remove HTML tags."""
    return re.sub(r"<[^>]+>", " ", text or "").strip()


def _scrape_greenhouse(company_name: str, token: str) -> list[dict]:
    """Fetch all jobs for one Greenhouse company board."""
    url = GREENHOUSE_URL.format(token=token)
    try:
        response = httpx.get(url, headers=HEADERS, timeout=30, follow_redirects=True)
        if response.status_code == 404:
            logger.warning("Greenhouse 404 — company: %s | token: %s", company_name, token)
            return []
        response.raise_for_status()
        data = response.json()

        jobs = []
        for job in data.get("jobs", []):
            title = job.get("title", "")
            job_url = job.get("absolute_url", "")
            location = job.get("location", {}).get("name", "")
            description = _strip_html(job.get("content", ""))

            if not title or not job_url:
                continue

            jobs.append({
                "title": title,
                "company": company_name,
                "location": location,
                "url": job_url,
                "source": f"Greenhouse/{token}",
                "description": description,
            })

        logger.debug("Greenhouse %s (%s): %d jobs", company_name, token, len(jobs))
        return jobs

    except httpx.HTTPStatusError as e:
        if e.response.status_code == 404:
            logger.warning("Greenhouse 404 — company: %s | token: %s", company_name, token)
            return []
        logger.error("Greenhouse error %s (%s): %s", company_name, token, e)
        return []
    except Exception as e:
        logger.error("Greenhouse error %s (%s): %s", company_name, token, e)
        return []


def _scrape_lever(company_name: str, token: str) -> list[dict]:
    """Fetch all jobs for one Lever company board."""
    url = LEVER_URL.format(token=token)
    try:
        response = httpx.get(url, headers=HEADERS, timeout=30, follow_redirects=True)
        if response.status_code == 404:
            logger.warning("Lever 404 — company: %s | token: %s", company_name, token)
            return []
        response.raise_for_status()
        data = response.json()

        jobs = []
        for posting in data:
            title = posting.get("text", "")
            job_url = posting.get("hostedUrl", "")
            location = posting.get("categories", {}).get("location", "")
            description = posting.get("descriptionPlain", "")

            # Also pull content from lists (requirements, responsibilities)
            lists = posting.get("lists", [])
            for lst in lists:
                description += " " + _strip_html(lst.get("content", ""))

            if not title or not job_url:
                continue

            jobs.append({
                "title": title,
                "company": company_name,
                "location": location,
                "url": job_url,
                "source": f"Lever/{token}",
                "description": description.strip(),
            })

        logger.debug("Lever %s (%s): %d jobs", company_name, token, len(jobs))
        return jobs

    except httpx.HTTPStatusError as e:
        if e.response.status_code == 404:
            logger.warning("Lever 404 — company: %s | token: %s", company_name, token)
            return []
        logger.error("Lever error %s (%s): %s", company_name, token, e)
        return []
    except Exception as e:
        logger.error("Lever error %s (%s): %s", company_name, token, e)
        return []


def _scrape_ashby(company_name: str, token: str) -> list[dict]:
    """Fetch all jobs for one Ashby company board via GraphQL."""
    try:
        payload = {
            "operationName": "ApiJobBoardWithTeams",
            "variables": {"organizationHostedJobsPageName": token},
            "query": _ASHBY_QUERY,
        }
        headers = {**ASHBY_HEADERS, "Referer": f"https://jobs.ashbyhq.com/{token}"}
        response = httpx.post(ASHBY_GRAPHQL, json=payload, headers=headers, timeout=30)

        if response.status_code in (401, 403, 404):
            logger.warning("Ashby %d — company: %s | token: %s", response.status_code, company_name, token)
            return []
        response.raise_for_status()
        data = response.json()

        board = data.get("data", {}).get("jobBoard") or {}
        if not board:
            logger.debug("Ashby %s (%s): org not found on Ashby (jobBoard=null)", company_name, token)
            return []
        postings = board.get("jobPostings", []) or []

        jobs = []
        for posting in postings:
            job_id = posting.get("id", "")
            title = posting.get("title", "")
            location = posting.get("locationName", "")
            workplace = posting.get("workplaceType", "")

            if workplace == "Remote" and not location:
                location = "Remote"

            job_url = f"https://jobs.ashbyhq.com/{token}/{job_id}"

            if not title or not job_id:
                continue

            jobs.append({
                "title": title,
                "company": company_name,
                "location": location,
                "url": job_url,
                "source": f"Ashby/{token}",
                "description": "",  # Ashby public board API doesn't include description text
            })

        logger.debug("Ashby %s (%s): %d jobs", company_name, token, len(jobs))
        return jobs

    except httpx.HTTPStatusError as e:
        logger.warning("Ashby error %s (%s): HTTP %s", company_name, token, e.response.status_code)
        return []
    except Exception as e:
        logger.error("Ashby error %s (%s): %s", company_name, token, e)
        return []


def scrape_all_companies() -> list[dict]:
    """
    Dispatch scraper for all TARGET_COMPANIES.
    - 404s are logged and skipped — never raises.
    - Duplicate tokens within the same ATS are skipped (e.g. Novation + Focusrite
      both use token 'focusrite' on Greenhouse — only hit once).
    - Unknown / unsupported ATS types (e.g. 'ashby') are logged and skipped.
    Returns flat list of all job dicts.
    """
    all_jobs = []
    not_found = []

    # Track seen (ats, token) pairs to avoid double-hitting the same endpoint
    seen_tokens: set[tuple[str, str]] = set()

    for company in config.TARGET_COMPANIES:
        name  = company["name"]
        ats   = company["ats"]
        token = company["token"]

        key = (ats, token)
        if key in seen_tokens:
            logger.debug("Skipping duplicate ATS endpoint: %s/%s (already scraped for another company)", ats, token)
            continue
        seen_tokens.add(key)

        if ats == "greenhouse":
            jobs = _scrape_greenhouse(name, token)
        elif ats == "lever":
            jobs = _scrape_lever(name, token)
        elif ats == "ashby":
            jobs = _scrape_ashby(name, token)
        else:
            logger.warning("Unsupported ATS type '%s' for %s — skipping", ats, name)
            jobs = []

        if not jobs:
            not_found.append(f"{name} ({ats}/{token})")

        all_jobs.extend(jobs)

    if not_found:
        logger.warning(
            "Companies returning 0 jobs (404 or empty board) — verify these tokens:\n  %s",
            "\n  ".join(not_found),
        )

    logger.info("scrape_all_companies: %d total jobs from %d companies", len(all_jobs), len(config.TARGET_COMPANIES))
    return all_jobs
