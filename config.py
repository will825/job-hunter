# config.py — Job Hunter · Terra Echo Labs · v2.0
#
# Public template — sensible defaults that work out of the box for audio industry jobs.
#
# To customize for your own search without touching this file, create a gitignored
# config_local.py alongside this one and override any variable:
#
#   # config_local.py
#   TARGET_COMPANIES = [{"name": "My Company", "ats": "greenhouse", "token": "mycompany"}]
#   KEYWORD_WEIGHTS  = {"audio engineer": 8, "dsp engineer": 10, ...}
#   TITLE_BLOCKLIST  = ["recruiter", "sales representative", ...]
#
# Any variable defined in config_local.py replaces the default below.

import os
from dotenv import load_dotenv

load_dotenv()

# ─── Settings ────────────────────────────────────────────────────────────────

DISCORD_WEBHOOK_URL = os.getenv("DISCORD_WEBHOOK_URL", "")
SCORE_THRESHOLD = 5       # Minimum score to trigger a Discord alert
DB_PATH = "data/jobs.db"

# ─── Title Blocklist ─────────────────────────────────────────────────────────
# Jobs whose titles match ANY of these patterns (case-insensitive) are skipped
# entirely — not saved, not alerted.

TITLE_BLOCKLIST = [
    "cloud infrastructure",
    "infrastructure engineer",
    "devops",
    "dev ops",
    "site reliability",
    "sre engineer",
    "data scientist",
    "data engineer",
    "mobile engineer",
    "android engineer",
    "ios engineer",
    "security engineer",
    "cybersecurity",
    "network engineer",
    "network administrator",
    "database administrator",
    "dba ",
    "blockchain",
    "crypto engineer",
    "web developer",
    "ui engineer",
    "ux engineer",
    "growth engineer",
    "platform engineer",
    "solutions architect",
    "cloud architect",
    "senior cloud",
    "vp of engineering",
    "director of engineering",
    "chief technology",
    "cto",
    "hr coordinator",
    "recruiter",
    "talent acquisition",
    "financial analyst",
    "accountant",
    "marketing manager",
    "content creator",
    "social media",
    "sales representative",
    "account executive",
    "business development",
    "legal counsel",
    "attorney",
    "paralegal",
]

# ─── Keywords & Weights ──────────────────────────────────────────────────────
# Format: "keyword phrase": weight (1–10)
# Matched case-insensitively against title + description + company name.
# Each keyword scores at most once per job.

KEYWORD_WEIGHTS = {

    # === Audio Engineering ===
    "audio engineer": 8,
    "audio software engineer": 10,
    "audio software": 10,
    "audio plugin": 10,
    "plugin developer": 9,
    "sound designer": 8,
    "spatial audio": 9,
    "immersive audio": 8,
    "audio middleware": 8,

    # === DSP / Real-Time Audio ===
    "audio dsp": 10,
    "dsp engineer": 10,
    "real-time audio": 9,
    "audio signal processing": 9,
    "signal processing engineer": 9,
    "juce": 9,
    "embedded audio": 8,

    # === Audio AI / ML ===
    "audio machine learning": 9,
    "audio ai": 8,
    "ml audio": 9,

    # === Audio QA / Testing ===
    "audio qa": 9,
    "audio quality engineer": 9,
    "audio test engineer": 8,
    "perceptual evaluation": 9,

    # === Bioacoustics / Field Recording ===
    "bioacoustics": 10,
    "passive acoustic monitoring": 10,
    "acoustic monitoring": 8,
    "soundscape ecology": 10,
    "wildlife acoustics": 9,
    "field recording": 7,

    # === Mycology ===
    "mycology": 10,
    "mycologist": 10,
    "fungal biology": 9,
}

# ─── Target Companies ────────────────────────────────────────────────────────
# All jobs from these companies are saved with a minimum score of 1, regardless
# of keyword matches — useful for companies you always want to monitor.
# Format: {"name": str, "ats": "greenhouse" | "lever" | "ashby", "token": str}
# Find ATS tokens at: boards.greenhouse.io/TOKEN  |  jobs.lever.co/TOKEN  |  jobs.ashbyhq.com/TOKEN

TARGET_COMPANIES = [
    # Greenhouse examples
    {"name": "iZotope",            "ats": "greenhouse", "token": "izotope"},
    {"name": "Avid Technology",    "ats": "greenhouse", "token": "avid"},
    {"name": "Dolby",              "ats": "greenhouse", "token": "dolby"},
    {"name": "Native Instruments", "ats": "greenhouse", "token": "nativeinstruments"},

    # Lever examples
    {"name": "Ableton",            "ats": "lever",      "token": "ableton"},
    {"name": "Shure",              "ats": "lever",      "token": "shure"},

    # Ashby examples
    {"name": "ElevenLabs",         "ats": "ashby",      "token": "elevenlabs"},
    {"name": "Deepgram",           "ats": "ashby",      "token": "deepgram"},
]

# ─── Score Tier Colors (Discord embed hex) ───────────────────────────────────
COLOR_EXCELLENT = 0x00FF00   # >= 20 — bright green
COLOR_STRONG    = 0xFFA500   # >= 10 — amber
COLOR_GOOD      = 0x3498DB   # >= 5  — blue
COLOR_TARGET    = 0x95A5A6   # >= 1  — grey (target company floor)

# ─── Job Board Reference (manual bookmarks) ──────────────────────────────────
JOB_BOARDS = [
    {"name": "AVIXA Job Board",     "url": "https://jobs.avixa.org",                          "priority": "TOP"},
    {"name": "ProAV Jobs",          "url": "https://proavjobs.com",                            "priority": "HIGH"},
    {"name": "Ziprecruiter Audio",  "url": "https://www.ziprecruiter.com/Jobs/Audio-Engineer", "priority": "HIGH"},
    {"name": "Science Careers",     "url": "https://jobs.sciencecareers.org/",                 "priority": "MEDIUM"},
    {"name": "Conservation Jobs",   "url": "https://www.conservationjobboard.com",             "priority": "MEDIUM"},
    {"name": "Environmental Career","url": "https://www.environmentalcareer.com",              "priority": "MEDIUM"},
]

# ─── Local overrides ─────────────────────────────────────────────────────────
# config_local.py (gitignored) can override any variable above.
# KEYWORDS and TARGET_COMPANY_NAMES are derived AFTER this import so they
# always reflect whichever values are active.
try:
    from config_local import *  # noqa: F401,F403
except ImportError:
    pass

# ─── Derived values (always computed last) ───────────────────────────────────
KEYWORDS = list(KEYWORD_WEIGHTS.keys())
TARGET_COMPANY_NAMES = {c["name"].lower() for c in TARGET_COMPANIES}
