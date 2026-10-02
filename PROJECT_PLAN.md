# Job Hunter v2 — Project Plan & Status

A self-hosted job search engine that scans company job boards, ranks postings
against your profile, and emails you a daily digest of new matches. Rust core,
SQLite storage, embedded web UI, optional LLM re-ranking (Groq). Runs 24/7 on a
Raspberry Pi.

_Last updated: 2026-10-01. Sections below the status table are partly historical._

---

## Status at a glance

| Phase | What it delivers | Status |
|---|---|---|
| 0 | Prove the pipeline (1 Greenhouse board → SQLite → print) | ✅ Done |
| 1 | Real sources (Greenhouse/Lever/Ashby) + dedup | ✅ Done |
| 2 | Keyword scoring + tiers + region/mode/seniority classification | ✅ Done |
| 2.5 | Editable `profile.toml` drives the scoring | ✅ Done |
| — | Coverage expansion (seed boards) + aggregators (Remotive, RemoteOK, Himalayas, Jobicy; Adzuna needs a key) | ✅ Done |
| — | Company management: add-by-link, list, delete (CLI + web UI) | ✅ Done |
| 3 | LLM fit-scoring (Groq) re-ranks top matches | ✅ Done — fit-score tiers, quota/breaker handling |
| 3.5 | Custom-page reader (watch any careers page) | 🟡 Partial — works for static pages; JS pages need the headless step |
| 4 | Daily digest email (Resend) + scheduling (launchd on macOS, systemd + cron on the Pi) | ✅ Done — running daily at 7 AM |
| 5 | Full Tauri desktop UI (your Figma design) | 🟡 Web UI done (Home triage, Swipe, Tracker, Analytics, Profile, Companies, Digest preview, Settings/Health); Tauri wrapper not started |
| 6 | Package, sign, README, screenshots — portfolio-ready | 🟡 README + screenshots done (public repo); signed build not started |
| — | Ops: `doctor`, `rescore`, `dedupe`, `compact`, scan lock, Health card | ✅ Done |

---

## What's completed

### The core pipeline (Phases 0–2)
- **Fetchers** for Greenhouse, Lever, Ashby (public JSON APIs, no auth), plus
  **Remotive** and **RemoteOK** aggregators — all behind one `Fetcher` trait.
- **SQLite storage** with robust dedup: a stable `id` per posting and a
  conservative fuzzy `dedup_key` that collapses the same job across sources
  **without ever losing a genuinely-different job**.
- **Classification** at ingest: `work_mode` (remote/hybrid/onsite),
  `region` (us/uk/emea/apac/…), `seniority` (junior/mid/senior/staff/lead).
- **Keyword scoring → tiers** (apply_now / strong / maybe / skip), driven by
  your profile, with title-weighting and a description cap so company
  boilerplate can't inflate off-target roles.

### Your profile (`profile.toml`)
- Human-editable file: target roles, skills, interests, preferences
  (remote/region), dealbreakers, and tunable weights/thresholds.
- Preferences **down-rank** rather than hide, so you never lose a job to a
  filter. Edit the file, re-run, everything re-ranks.

### Company management (the "add any board by link" feature)
- **Add by link**: paste a careers URL. If it's Greenhouse/Lever/Ashby (or a
  page that embeds one), it's detected, validated, and added. If it's any other
  page, it's added as a **custom page** to watch — never rejected.
- **See / delete** your watched companies.
- Available both in the **CLI** (`add`/`list`/`remove`) and the **web UI**.
- Watchlist is stored in the database; the 30 defaults seed once on first run.

### Local web UI (`serve`)
- Add a company by link (with clear success/error messages).
- See and delete watched companies.
- "Scan now" button.
- Browse matches ranked to your profile, filtered by tier / mode / region /
  seniority. This is an early, functional slice of the eventual Phase-5 app.

### LLM fit-scoring (Phase 3) — built, needs your key
- After keyword scoring, the top matches are sent to **Groq** to judge genuine
  fit and return `{ fit_score, tier, reasoning, gaps }`, overriding the tier.
- **Provider-abstracted** and fully optional: set `[llm] enabled = false` and
  it's off; runs on keyword scoring alone with no errors.
- Needs a free Groq API key in the `GROQ_API_KEY` environment variable.

---

## What's in progress / remaining

### 3.5 — Custom-page reader (watch ANY careers page)
**Goal:** paste any company's careers page (Warner Bros, Shure, The Audio
Programmer, Yamaha…) and get notified when they post a matching new job.

**Done:** you can already add/see/delete these pages. The reader extracts jobs
from a page's text using the LLM, gated by `[custom_pages] enabled` and easily
removable.

**Remaining — the headless step:** the big enterprise/custom sites load their
jobs with JavaScript, so a plain fetch sees nothing. The fix is to render each
custom page in a **headless browser** before the LLM reads it. That's the next
build. It's isolated to `custom_page.rs`, so it can be toggled off or removed
without touching anything else.

- Trade-off: modest, brief extra load on your Mac during scans (seconds per
  page); more brittle than ATS APIs (a site redesign can need a fix).
- Removability: `[custom_pages] enabled = false`, delete the custom pages from
  your list, or delete `custom_page.rs` + its one call site.

### 4 — Daily digest email + scheduling ✅
- `cargo run -- digest` = scan + email the NEW apply-now/strong matches since
  last time. Email via Resend's HTTP API, key from `RESEND_API_KEY` in `.env`,
  config in `[email]`.
- New-job diffing via a per-job `notified_at` column (each job emailed once); a
  one-time baseline on first run so you're not blasted with everything.
- If email isn't set up, the digest prints instead (nothing marked sent).
- Scheduling: `scheduling/` has a launchd plist + wrapper + `SCHEDULING.md`
  for macOS, and `scheduling/linux/` has the systemd unit + Pi guide. It now
  runs on a Raspberry Pi: `serve` as a systemd service, `digest` on a 7 AM cron.

### 5 — Full Tauri desktop UI
- Your Figma design: pipeline board (New → To Apply → Applied → Interviewing →
  Closed), rich filters, job cards, profile editor, settings.
- The current web UI becomes the basis for this.

### 6 — Package & polish
- Signed build (like Fissure/Plumb), README, screenshots — portfolio-ready.

---

## How to use it today

```bash
# scan every watched board, rank against your profile, print top matches
cargo run

# open the web UI (manage companies, scan, browse matches)
cargo run -- serve

# watch a new company by pasting its careers link
cargo run -- add "https://boards.greenhouse.io/anthropic"
cargo run -- add "https://www.theaudioprogrammer.com/jobs"   # custom page

# see / remove watched companies
cargo run -- list
cargo run -- remove 7

# maintenance
cargo run -- doctor     # check keys, profile, DB, Groq model
cargo run -- rescore    # re-score stored jobs after a profile change
cargo run -- dedupe     # merge per-city aggregator duplicates
cargo run -- compact    # trim stored raw_json + VACUUM
```

To turn on LLM ranking: get a free key at console.groq.com, put
`GROQ_API_KEY=...` in `.env`, and run a scan.

To turn on Adzuna (whole-internet search by your target roles): get free
credentials at developer.adzuna.com, put `ADZUNA_APP_ID` / `ADZUNA_APP_KEY` in
`.env`, and run a scan (country and page size are in `[adzuna]`). Himalayas, Jobicy,
Remotive, and RemoteOK aggregators need no key and are on by default.

To turn on the custom-page reader: set `[custom_pages] enabled = true` in
`profile.toml` (and have the Groq key set).

---

## Design principles (why it's built this way)
- **Never lose a job.** Dedup and preference-filtering favor an occasional
  duplicate over ever hiding a real match.
- **Never crash the run.** One bad board is logged and skipped; the scan
  continues.
- **Light on the machine.** ATS fetches are instant; the heavy optional bits
  (LLM, headless browser) run on Groq's servers / only for custom pages.
- **Everything optional is removable.** LLM and custom-pages are isolated
  modules behind config toggles.

## Tech stack
Rust · tokio + reqwest (async HTTP) · rusqlite (SQLite) · serde · axum (web UI)
· toml (profile) · Groq (LLM) · Resend (email) · headless browser + Tauri (next).
