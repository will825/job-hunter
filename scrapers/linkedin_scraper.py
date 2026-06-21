# scrapers/linkedin_scraper.py — Job Hunter · Terra Echo Labs · v2.0
# LinkedIn job scraper via Playwright headless browser
# Anti-detection: human-like delays, real UA, no AutomationControlled flag

import asyncio
import logging
import random
import re

from playwright.async_api import async_playwright, TimeoutError as PlaywrightTimeout

import config

logger = logging.getLogger(__name__)

LINKEDIN_BASE = "https://www.linkedin.com/jobs/search/"

USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/124.0.0.0 Safari/537.36"
)


def _strip_html(text: str) -> str:
    return re.sub(r"<[^>]+>", " ", text or "").strip()


async def _scrape_query(page, query: str) -> list[dict]:
    """Scrape LinkedIn jobs for a single search query."""
    jobs = []

    params = {
        "keywords": query,
        "f_WT": "2",      # Remote filter
        "sortBy": "DD",   # Most recent first
    }
    query_string = "&".join(f"{k}={v.replace(' ', '%20')}" for k, v in params.items())
    url = f"{LINKEDIN_BASE}?{query_string}"

    try:
        logger.info("LinkedIn: searching '%s'", query)
        await page.goto(url, wait_until="domcontentloaded", timeout=30000)

        # Human-like delay after page load
        await asyncio.sleep(random.uniform(2, 4))

        # Check for CAPTCHA / login wall
        page_text = await page.inner_text("body")
        if "sign in" in page_text.lower() or "captcha" in page_text.lower() or "join now" in page_text.lower():
            logger.warning("LinkedIn: CAPTCHA or login wall detected for query '%s' — skipping", query)
            return []

        # Scroll 3x to trigger lazy-loaded job cards
        for _ in range(3):
            await page.evaluate("window.scrollBy(0, window.innerHeight)")
            await asyncio.sleep(random.uniform(1, 2))

        # Extract job cards
        cards = await page.query_selector_all(".jobs-search__results-list li, .base-card")
        logger.info("LinkedIn: found %d cards for '%s'", len(cards), query)

        for card in cards:
            try:
                title_el = await card.query_selector("h3, .base-search-card__title")
                company_el = await card.query_selector("h4, .base-search-card__subtitle")
                location_el = await card.query_selector(".job-search-card__location, .base-search-card__metadata")
                link_el = await card.query_selector("a")

                title = (await title_el.inner_text()).strip() if title_el else ""
                company = (await company_el.inner_text()).strip() if company_el else ""
                location = (await location_el.inner_text()).strip() if location_el else ""
                href = await link_el.get_attribute("href") if link_el else ""

                if not title or not href:
                    continue

                # Normalize URL — strip tracking params
                job_url = href.split("?")[0] if href else ""

                jobs.append({
                    "title": title,
                    "company": company,
                    "location": location,
                    "url": job_url,
                    "source": "LinkedIn",
                    "description": f"LinkedIn search: {query}",
                })
            except Exception as e:
                logger.debug("LinkedIn card parse error: %s", e)
                continue

    except PlaywrightTimeout:
        logger.warning("LinkedIn: timeout for query '%s' — skipping", query)
    except Exception as e:
        logger.warning("LinkedIn: error for query '%s': %s — skipping", query, e)

    return jobs


async def _run_async() -> list[dict]:
    all_jobs = []

    async with async_playwright() as p:
        browser = await p.chromium.launch(
            headless=True,
            args=[
                "--disable-blink-features=AutomationControlled",
                "--no-sandbox",
                "--disable-setuid-sandbox",
            ],
        )
        context = await browser.new_context(user_agent=USER_AGENT)
        page = await context.new_page()

        for query in config.LINKEDIN_SEARCHES:
            try:
                jobs = await _scrape_query(page, query)
                all_jobs.extend(jobs)
            except Exception as e:
                logger.warning("LinkedIn: unhandled error for '%s': %s — continuing", query, e)

            # Human-like delay between searches
            await asyncio.sleep(random.uniform(3, 6))

        await browser.close()

    logger.info("LinkedIn total: %d jobs across %d searches", len(all_jobs), len(config.LINKEDIN_SEARCHES))
    return all_jobs


def scrape_linkedin() -> list[dict]:
    """
    Synchronous wrapper around the async LinkedIn scraper.
    On any error (CAPTCHA, timeout, blocked), logs and returns [].
    LinkedIn is supplemental — never blocks the run.
    """
    try:
        return asyncio.run(_run_async())
    except Exception as e:
        logger.warning("LinkedIn scraper failed entirely: %s — returning []", e)
        return []
