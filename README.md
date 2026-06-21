# Job Hunter
**Multi-source audio industry job aggregator · Terra Echo Labs · v2.0**

Polls 80+ company ATS pages (Greenhouse, Lever, Ashby), RSS feeds (RemoteOK, WeWorkRemotely), and the AVIXA Pro AV job board on a schedule. Each listing is scored against a weighted keyword list tuned for audio software, DSP, bioacoustics, and adjacent roles. New matches above the score threshold fire rich Discord embeds with relevance scores and matched keywords.

---

## How It Works

```
scrape → score → deduplicate → alert
```

1. Each scraper returns raw job dicts: `{title, company, location, url, source, description}`
2. `scorer.py` matches against `KEYWORD_WEIGHTS` in `config.py` (case-insensitive, title + description + company)
3. Jobs with titles matching `TITLE_BLOCKLIST` are skipped before scoring
4. Jobs from `TARGET_COMPANIES` get a score floor of 1 regardless of keyword matches
5. Each job is saved to SQLite — deduped via SHA-1 hash of `title+company+url`
6. Unnotified jobs above `SCORE_THRESHOLD` are batched and sent to Discord (lowest score first so the best match lands at the top of the channel)
7. Alerted jobs are marked `notified=1` and never re-sent
8. A summary embed is always sent at the end of each run

---

## Setup

### 1. Install dependencies
```bash
pip install -r requirements.txt
playwright install chromium
```

### 2. Configure credentials
```bash
cp .env.example .env
```
Edit `.env` and paste your Discord webhook URL:
```
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/YOUR_ID/YOUR_TOKEN
```
To get a webhook: **Discord Server Settings → Integrations → Webhooks → New Webhook → Copy URL**

### 3. Test run
```bash
# Dry run — scrape + score + print results, zero Discord messages
python run.py --dry-run

# Live run
python run.py
```

### 4. Schedule (macOS launchd)
Point a launchd plist at `run.py` with whatever interval you want (4-hour works well). Stdout/stderr can be piped to `/tmp/jobhunter.log` and `/tmp/jobhunter.err`.

---

## CLI Flags

| Flag | Effect |
|------|--------|
| `--dry-run` | Scrape + score + print, no Discord messages sent |
| `--summary` | Send only the run summary embed, skip individual alerts |
| `--companies` | Hit ATS boards only — skip RSS feeds |

---

## Configuration (`config.py`)

All tunable knobs live here. Edit this file, not the scrapers.

| Variable | Purpose |
|---|---|
| `DISCORD_WEBHOOK_URL` | Loaded from `.env` — do not hardcode |
| `SCORE_THRESHOLD` | Minimum score to trigger a Discord alert (default: `5`) |
| `KEYWORD_WEIGHTS` | `{"keyword phrase": weight}` — matched against title + description + company name |
| `TITLE_BLOCKLIST` | Title substrings that cause a job to be skipped entirely |
| `TARGET_COMPANIES` | `[{name, ats, token}]` — all jobs from these companies are saved, even at score 0 |
| `LINKEDIN_SEARCHES` | Search strings used by the auxiliary search scraper |

### Score tiers (Discord embed colors)

| Score | Color | Label |
|-------|-------|---------|
| ≥ 20 | Green | Excellent Match |
| ≥ 10 | Amber | Strong Match |
| ≥ 5  | Blue | Good Match |
| ≥ 1  | Grey | Target Company |

---

## Project Structure

```
job-hunter/
  run.py              # Orchestrator: scrape → score → save → notify
  config.py           # All keywords, companies, and thresholds
  scorer.py           # Keyword matching + score calculation
  database.py         # SQLite: init, save (dedup), query, mark notified
  verify_tokens.py    # Utility: audit which ATS tokens are still live
  scrapers/
    company_ats.py       # Greenhouse / Lever / Ashby ATS API scrapers
    linkedin_scraper.py  # Auxiliary search scraper (Playwright)
    avixa_scraper.py     # AVIXA Pro AV job board
    remoteok_wwr.py      # RemoteOK + WeWorkRemotely RSS feeds
  notifiers/
    discord.py        # send_job_alert(), send_alerts_batch(), send_summary()
  data/               # SQLite DB lives here (gitignored, created on first run)
  .env                # Your secrets (gitignored)
  .env.example        # Template — copy to .env
```

---

## Common Tasks

- **Add a keyword** → add to `KEYWORD_WEIGHTS` in `config.py` with a weight 1–10
- **Block a role type** → add a substring to `TITLE_BLOCKLIST` in `config.py`
- **Add a target company** → add `{name, ats, token}` to `TARGET_COMPANIES` in `config.py`
- **Reduce noise** → raise `SCORE_THRESHOLD` in `config.py`
- **Fix a broken ATS token** → run `python verify_tokens.py`, update the token in `config.py`
- **Add a new source** → create a scraper in `scrapers/`, import and call it in `run.py`

### Checking ATS tokens
Many ATS tokens go stale when companies change systems. 404s are logged gracefully.
```bash
python verify_tokens.py
```
Manual verification:
- Greenhouse: `https://boards.greenhouse.io/TOKEN`
- Lever: `https://jobs.lever.co/TOKEN`
