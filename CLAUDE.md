# Job Hunter — Claude Handoff

## What This Is
Automated job-monitoring tool for Will (audio industry job seeker). Scrapes multiple job boards and company ATS pages, scores each listing against a weighted keyword list, deduplicates via SQLite, and fires color-coded Discord alerts for anything above the score threshold. Focus areas: audio software engineering, DSP, bioacoustics, mycology, and adjacent roles.

## Tech Stack
- **Python 3** — CLI script, no web framework
- **SQLite** (`data/jobs.db`) — append-only audit log, never delete rows
- **Playwright** — headless Chromium for LinkedIn scraping
- **httpx** — HTTP client for ATS API calls
- **BeautifulSoup4 + lxml** — HTML parsing
- **feedparser** — RSS ingestion (RemoteOK, WeWorkRemotely)
- **APScheduler** — scheduling support (can be combined with launchd/cron)
- **Discord webhook** — notification delivery

## Project Structure
```
job-hunter/
  run.py              # Orchestrator: scrape → score → save → notify
  config.py           # SINGLE SOURCE OF TRUTH — all keywords, companies, thresholds
  scorer.py           # Keyword matching + score calculation logic
  database.py         # SQLite: init_db, save_job (dedup), get_unnotified_jobs, mark_notified, log_run
  verify_tokens.py    # Utility: check which ATS tokens are still returning 200s
  scrapers/
    company_ats.py       # Greenhouse / Lever / Ashby ATS API scrapers
    linkedin_scraper.py  # Playwright-based LinkedIn job search
    avixa_scraper.py     # AVIXA Pro AV job board scraper
    remoteok_wwr.py      # RemoteOK + WeWorkRemotely RSS feeds
  notifiers/
    discord.py        # send_job_alert(), send_alerts_batch(), send_summary()
  data/
    jobs.db           # SQLite database (gitignored, created on first run)
```

## Pipeline (how a run works)
1. Each scraper returns a list of raw job dicts: `{title, company, location, url, source, description}`
2. `scorer.py` matches each job against `KEYWORD_WEIGHTS` in `config.py` (case-insensitive, title + description + company name)
3. Jobs with a title matching `TITLE_BLOCKLIST` are skipped before scoring
4. Jobs from `TARGET_COMPANIES` get a minimum score of 1 even if no keywords match
5. Each job is saved to SQLite — deduped via SHA-1 hash of `title+company+url`; duplicates are silently dropped
6. Unnotified jobs with `score >= SCORE_THRESHOLD` are fetched and sent to Discord (lowest score first so best job appears at top of Discord feed)
7. Alerted jobs are marked `notified=1` — never re-sent
8. A summary embed is always sent at the end of every run

## Configuration (`config.py`)
**Edit this file to tune the tool — not the scrapers.**

| Variable | Purpose |
|---|---|
| `DISCORD_WEBHOOK_URL` | Discord channel webhook URL |
| `SCORE_THRESHOLD` | Min score to alert (default: 5; raise to reduce noise) |
| `KEYWORD_WEIGHTS` | `{"keyword phrase": weight}` — matched against title + description + company |
| `TITLE_BLOCKLIST` | Title substrings that cause a job to be skipped entirely |
| `TARGET_COMPANIES` | `[{name, ats, token}]` — all jobs from these companies are saved regardless of score |
| `LINKEDIN_SEARCHES` | Search query strings fed to the LinkedIn scraper |

## CLI Usage
```bash
# Full run — scrape all sources, score, alert
python run.py

# Dry run — print alerts to stdout, send nothing to Discord
python run.py --dry-run

# ATS only — skip RemoteOK / WWR / AVIXA / LinkedIn
python run.py --companies

# Summary only — send the run summary embed, skip individual job alerts
python run.py --summary

# Verify ATS tokens
python verify_tokens.py
```

## Database Rules
- **Never delete rows** — `jobs` is an append-only audit log
- `notified=0` → saved but not yet sent to Discord
- `notified=1` → already alerted, will not be re-sent
- `score=0` → saved for record, never alerted
- `score=1` → target company floor (only alerted if `SCORE_THRESHOLD` is dropped to 1)

## Discord Alert Colors
| Score | Color | Label |
|---|---|---|
| >= 20 | Bright green | Excellent |
| >= 10 | Amber | Strong |
| >= 5 | Blue | Good |
| >= 1 | Grey | Target company floor |

## ATS API Endpoints
`company_ats.py` hits these APIs directly:
- **Greenhouse**: `https://boards-api.greenhouse.io/v1/boards/{token}/jobs`
- **Lever**: `https://api.lever.co/v0/postings/{token}`
- **Ashby**: `https://jobs.ashbyhq.com/api/non-user-facing/listing/job-board/{token}`

Many tokens in `config.py` are noted as potentially stale — 404s are logged gracefully. Tokens verified working as of 2026-03 are commented as such. Run `verify_tokens.py` to audit them.

## Common Tasks
- **Add a keyword** → add to `KEYWORD_WEIGHTS` in `config.py` with a weight (1–10)
- **Block a role type** → add a substring to `TITLE_BLOCKLIST` in `config.py`
- **Add a target company** → add `{name, ats, token}` to `TARGET_COMPANIES` in `config.py`
- **Reduce alert noise** → raise `SCORE_THRESHOLD` in `config.py`
- **Fix a broken ATS token** → run `python verify_tokens.py`, update the token in `config.py`
- **Add a new source** → create a scraper in `scrapers/`, import and call it in `run.py`

## Setup
```bash
pip install -r requirements.txt
playwright install chromium   # required for LinkedIn scraper
```
