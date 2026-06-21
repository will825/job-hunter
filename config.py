# config.py — Job Hunter · Terra Echo Labs · v2.0
# Single source of truth. Edit this file to tune keywords, companies, and settings.

import os
from dotenv import load_dotenv

load_dotenv()

# ─── Settings ────────────────────────────────────────────────────────────────

DISCORD_WEBHOOK_URL = os.getenv("DISCORD_WEBHOOK_URL", "")
SCORE_THRESHOLD = 5       # Minimum score to trigger Discord alert (lower = more alerts, raise if too noisy)
DB_PATH = "data/jobs.db"

# ─── Title Blocklist ─────────────────────────────────────────────────────────
# Jobs whose titles match ANY of these patterns (case-insensitive) are SKIPPED
# entirely — not saved, not alerted. Prevents irrelevant alerts.
TITLE_BLOCKLIST = [
    "cloud infrastructure",
    "infrastructure engineer",
    "devops",
    "dev ops",
    "site reliability",
    "sre engineer",
    "data scientist",
    "data engineer",
    "machine learning engineer",  # generic ML, not audio ML
    "backend engineer",           # generic backend, not audio
    "frontend engineer",
    "full stack",
    "fullstack",
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
    "platform engineer",         # generic platform, not audio
    "solutions architect",       # generic cloud arch
    "cloud architect",
    "senior cloud",
    "staff engineer",            # too senior / generic
    "principal engineer",        # too senior unless audio
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
    "content creator",           # social media content, not audio
    "social media",
    "sales representative",
    "account executive",
    "business development",
    "legal counsel",
    "attorney",
    "paralegal",
]

# ─── Keywords & Weights ──────────────────────────────────────────────────────
# Format: "keyword phrase": weight
# Scorer matches case-insensitively against title + description + company name

KEYWORD_WEIGHTS = {

    # === Audio Engineering / Post-Production ===
    "audio engineer": 8,
    "audio software": 10,
    "audio developer": 6,
    "audio software engineer": 10,
    "audio software developer": 10,
    "sound designer": 8,
    "sound design": 7,
    "audio tools": 5,
    "audio plugin": 10,
    "plugin developer": 9,
    "music technology": 6,
    "music software": 6,
    "post production": 5,
    "audio post": 5,
    "dolby atmos": 9,
    "spatial audio": 9,
    "immersive audio": 8,
    "3d audio": 8,
    "ambisonics": 8,
    "audio middleware": 8,

    # === DSP / Software Development ===
    "audio dsp": 10,
    "dsp engineer": 10,
    "dsp developer": 10,
    "audio engine": 9,
    "real-time audio": 9,
    "audio signal processing": 9,
    "signal processing engineer": 9,
    "embedded audio": 8,
    "firmware audio": 7,
    "linux audio": 6,
    "juce": 9,
    "max/msp": 5,
    "machine learning audio": 9,
    "ml audio": 9,
    "audio machine learning": 9,
    "audio ai": 8,
    "python audio": 8,
    "fastapi": 4,
    "remote software engineer": 3,
    "remote developer": 3,

    # === Audio QA / Hardware QA / Testing ===
    "audio qa": 9,
    "audio quality assurance": 9,
    "audio quality engineer": 9,
    "audio testing": 8,
    "audio test engineer": 8,
    "qa engineer audio": 8,
    "hardware qa": 5,
    "hardware quality assurance": 5,
    "software qa": 5,
    "quality assurance engineer": 4,
    "test engineer": 3,
    "product validation": 4,
    "perceptual evaluation": 9,
    "subjective evaluation audio": 9,
    "audio hardware testing": 8,
    "acoustic testing": 7,

    # === IT / Technical Support / AV ===
    "it support": 3,
    "technical support": 3,
    "helpdesk": 3,
    "help desk": 3,
    "remote it": 4,
    "it administrator": 3,
    "systems administrator": 3,
    "av technician": 5,
    "audio visual technician": 6,
    "av support": 5,
    "it technician": 3,
    "desktop support": 3,
    "endpoint support": 3,
    "audio visual support": 5,

    # === Product / Project Management ===
    "associate product manager": 4,
    "audio product manager": 8,
    "product manager audio": 8,
    "product manager music": 7,
    "apm audio": 6,
    "technical project manager audio": 6,
    "program manager audio": 6,

    # === Bioacoustics / Field Recording / Environmental ===
    "bioacoustics": 10,
    "ecoacoustics": 10,
    "acoustic ecology": 10,
    "passive acoustic monitoring": 10,
    "wildlife acoustics": 9,
    "soundscape ecology": 10,
    "acoustic monitoring": 8,
    "field recording": 7,
    "environmental audio": 6,
    "environmental acoustics": 8,
    "hydroacoustics": 9,
    "underwater acoustics": 9,
    "bat acoustics": 8,
    "bird acoustics": 8,
    "biodiversity monitoring audio": 8,

    # === Environmental / Sustainability ===
    "environmental science": 4,
    "conservation biology": 4,
    "sustainability": 3,
    "climate tech": 3,
    "environmental monitoring": 5,
    "ecological research": 4,
    "field biology": 4,

    # === Mycology ===
    "mycology": 10,
    "mycologist": 10,
    "fungal biology": 9,
    "fungal research": 9,
    "fungal": 6,
    "mushroom research": 8,

    # === Company Name Keywords ===
    "izotope": 10,
    "avid": 9,
    "pro tools": 7,
    "native instruments": 9,
    "focusrite": 9,
    "shure": 7,
    "dolby": 8,
    "waves audio": 8,
    "ableton": 8,
    "steinberg": 6,
    "yamaha": 5,
    "sennheiser": 6,
    "beyerdynamic": 6,
    "universal audio": 9,
    "uad": 9,
    "audient": 6,
    "fender": 5,
    "minimal audio": 8,
    "focal": 5,
    "moog": 7,
    "arturia": 7,
    "elektron": 6,
    "korg": 5,
    "roland": 5,
    "teenage engineering": 8,
    "genelec": 8,
    "ssl": 8,
    "solid state logic": 8,
    "fabfilter": 8,
    "plugin alliance": 6,
    "splice": 5,
    "output": 5,
    "rupert neve": 8,
    "neve": 7,
    "rode": 5,
    "audio-technica": 5,
    "audeze": 6,
    "warm audio": 5,
    "wwise": 8,
    "audiokinetic": 8,
    "audinate": 8,
    "dante": 6,
    "biamp": 6,
    "extron": 6,
    "elevenlabs": 8,
    "deepgram": 7,
    "descript": 6,
    "pindrop": 6,
    "supertone": 6,
    "rockbot": 5,
    "voicemod": 5,
    "bose professional": 6,
    "qsc": 7,
}

# Convenience flat list for fast membership checks
KEYWORDS = list(KEYWORD_WEIGHTS.keys())

# ─── Target Companies ────────────────────────────────────────────────────────
# All jobs from these companies are saved with minimum score=1 regardless of keywords.
# Format: {"name": str, "ats": "greenhouse"|"lever", "token": str}

TARGET_COMPANIES = [

    # === GREENHOUSE — verified working 2026-03 ===
    {"name": "Splice",                  "ats": "greenhouse", "token": "splice"},
    {"name": "Fender",                  "ats": "greenhouse", "token": "fender"},
    {"name": "Descript",                "ats": "greenhouse", "token": "descript"},
    {"name": "Rockbot",                 "ats": "greenhouse", "token": "rockbot"},

    # === ASHBY — verified working 2026-03 ===
    {"name": "ElevenLabs",              "ats": "ashby", "token": "elevenlabs"},
    {"name": "Deepgram",                "ats": "ashby", "token": "deepgram"},

    # === GREENHOUSE — tokens may be stale, kept for floor scoring + future recovery ===
    # (404s are logged gracefully; jobs from these companies found via LinkedIn get score floor)
    {"name": "iZotope",                 "ats": "greenhouse", "token": "izotope"},
    {"name": "Avid Technology",         "ats": "greenhouse", "token": "avid"},
    {"name": "Native Instruments",      "ats": "greenhouse", "token": "nativeinstruments"},
    {"name": "Steinberg",               "ats": "greenhouse", "token": "steinberg"},
    {"name": "Output",                  "ats": "greenhouse", "token": "output"},
    {"name": "Sennheiser",              "ats": "greenhouse", "token": "sennheiser"},
    {"name": "Neumann",                 "ats": "greenhouse", "token": "neumann"},
    {"name": "Beyerdynamic",            "ats": "greenhouse", "token": "beyerdynamic"},
    {"name": "Sony Pro Audio",          "ats": "greenhouse", "token": "sony"},
    {"name": "Bose Professional",       "ats": "greenhouse", "token": "bose"},
    {"name": "Focusrite",               "ats": "greenhouse", "token": "focusrite"},
    {"name": "Universal Audio (UAD)",   "ats": "greenhouse", "token": "uaudio"},
    {"name": "PreSonus",                "ats": "greenhouse", "token": "presonus"},
    {"name": "SSL (Solid State Logic)", "ats": "greenhouse", "token": "solidstatelogic"},
    {"name": "Neve / AMS",              "ats": "greenhouse", "token": "neve"},
    {"name": "Audinate",                "ats": "greenhouse", "token": "audinate"},
    {"name": "QSC Audio",               "ats": "greenhouse", "token": "qsc"},
    {"name": "ClearOne",                "ats": "greenhouse", "token": "clearone"},
    {"name": "JBL Professional (Harman)","ats": "greenhouse", "token": "harman"},
    {"name": "Yamaha Pro Audio",        "ats": "greenhouse", "token": "yamaha"},
    {"name": "Meyer Sound",             "ats": "greenhouse", "token": "meyersound"},
    {"name": "Dolby",                   "ats": "greenhouse", "token": "dolby"},
    {"name": "Moog Music",              "ats": "greenhouse", "token": "moogmusic"},
    {"name": "Korg",                    "ats": "greenhouse", "token": "korg"},
    {"name": "Roland",                  "ats": "greenhouse", "token": "roland"},
    {"name": "Novation (Focusrite)",    "ats": "greenhouse", "token": "focusrite"},  # dedup'd by scraper
    {"name": "Gibson",                  "ats": "greenhouse", "token": "gibson"},
    {"name": "Music AI",                "ats": "greenhouse", "token": "musicai"},
    {"name": "Pindrop",                 "ats": "greenhouse", "token": "pindrop"},
    {"name": "Voicemod",                "ats": "greenhouse", "token": "voicemod"},
    {"name": "Microsoft (Xbox Audio)",  "ats": "greenhouse", "token": "microsoft"},
    {"name": "Sony Interactive (PS)",   "ats": "greenhouse", "token": "sonyinteractiveentertainment"},
    {"name": "Cornell Lab Ornithology", "ats": "greenhouse", "token": "cornelluniversity"},
    {"name": "EOS IT Solutions",        "ats": "greenhouse", "token": "eosits"},
    {"name": "ECRS",                    "ats": "greenhouse", "token": "ecrs"},

    # === LEVER — tokens may be stale, kept for floor scoring + future recovery ===
    {"name": "Ableton",                 "ats": "lever", "token": "ableton"},
    {"name": "Waves Audio",             "ats": "lever", "token": "waves"},
    {"name": "Plugin Alliance",         "ats": "lever", "token": "pluginalliance"},
    {"name": "Minimal Audio",           "ats": "lever", "token": "minimalaudio"},
    {"name": "Cradle",                  "ats": "lever", "token": "cradlemusic"},
    {"name": "Soundtoys",               "ats": "lever", "token": "soundtoys"},
    {"name": "FabFilter",               "ats": "lever", "token": "fabfilter"},
    {"name": "Sonible",                 "ats": "lever", "token": "sonible"},
    {"name": "Oeksound",                "ats": "lever", "token": "oeksound"},
    {"name": "Shure",                   "ats": "lever", "token": "shure"},
    {"name": "RODE Microphones",        "ats": "lever", "token": "rode"},
    {"name": "Audio-Technica",          "ats": "lever", "token": "audiotechnica"},
    {"name": "Earthworks Audio",        "ats": "lever", "token": "earthworksaudio"},
    {"name": "Warm Audio",              "ats": "lever", "token": "warmaudio"},
    {"name": "Lewitt Audio",            "ats": "lever", "token": "lewittaudio"},
    {"name": "Audeze",                  "ats": "lever", "token": "audeze"},
    {"name": "Focal",                   "ats": "lever", "token": "focal"},
    {"name": "Genelec",                 "ats": "lever", "token": "genelec"},
    {"name": "Adam Audio",              "ats": "lever", "token": "adamaudio"},
    {"name": "Eve Audio",               "ats": "lever", "token": "eveaudio"},
    {"name": "Audient",                 "ats": "lever", "token": "audient"},
    {"name": "RME Audio",               "ats": "lever", "token": "rmeaudio"},
    {"name": "MOTU",                    "ats": "lever", "token": "motu"},
    {"name": "Apogee Electronics",      "ats": "lever", "token": "apogeedigital"},
    {"name": "API Audio",               "ats": "lever", "token": "apiaudio"},
    {"name": "Rupert Neve Designs",     "ats": "lever", "token": "rupertnevedesigns"},
    {"name": "Bricasti Design",         "ats": "lever", "token": "bricasti"},
    {"name": "Dangerous Music",         "ats": "lever", "token": "dangerousmusic"},
    {"name": "Mackie",                  "ats": "lever", "token": "mackie"},
    {"name": "d&b audiotechnik",        "ats": "lever", "token": "dbaudiotechnik"},
    {"name": "Arturia",                 "ats": "lever", "token": "arturia"},
    {"name": "Elektron",                "ats": "lever", "token": "elektron"},
    {"name": "Sequential (DSI)",        "ats": "lever", "token": "sequential"},
    {"name": "Teenage Engineering",     "ats": "lever", "token": "teenageengineering"},
    {"name": "Make Noise",              "ats": "lever", "token": "makenoise"},
    {"name": "Intellijel",              "ats": "lever", "token": "intellijel"},
    {"name": "Strymon",                 "ats": "lever", "token": "strymon"},
    {"name": "Chase Bliss Audio",       "ats": "lever", "token": "chasebliss"},
    {"name": "Supertone",               "ats": "lever", "token": "supertone"},
    {"name": "Audiokinetic (Wwise)",    "ats": "lever", "token": "audiokinetic"},
    {"name": "Wildlife Acoustics",      "ats": "lever", "token": "wildlifeacoustics"},
    {"name": "Stallergenes Greer",      "ats": "lever", "token": "stallergenesgreer"},
]

# Set of target company names for fast O(1) lookup
TARGET_COMPANY_NAMES = {c["name"].lower() for c in TARGET_COMPANIES}

# ─── Score Tier Colors (Discord embed hex) ───────────────────────────────────
COLOR_EXCELLENT = 0x00FF00   # >= 20 — bright green
COLOR_STRONG    = 0xFFA500   # >= 10 — amber
COLOR_GOOD      = 0x3498DB   # >= 5  — blue
COLOR_TARGET    = 0x95A5A6   # >= 1  — grey (target company floor)

# ─── Job Board Reference (manual / bookmarks) ────────────────────────────────
JOB_BOARDS = [
    {"name": "AVIXA Job Board",     "url": "https://jobs.avixa.org",                         "priority": "TOP"},
    {"name": "ProAV Jobs",          "url": "https://proavjobs.com",                           "priority": "HIGH"},
    {"name": "Ziprecruiter Audio",  "url": "https://www.ziprecruiter.com/Jobs/Audio-Engineer", "priority": "HIGH"},
    {"name": "Indeed AV",          "url": "https://www.indeed.com/q-AV-Systems-Specialist-jobs.html", "priority": "HIGH"},
    {"name": "Science Careers",     "url": "https://jobs.sciencecareers.org/",                "priority": "MEDIUM"},
    {"name": "Conservation Jobs",   "url": "https://www.conservationjobboard.com",            "priority": "MEDIUM"},
    {"name": "Environmental Career","url": "https://www.environmentalcareer.com",              "priority": "MEDIUM"},
    {"name": "Remote Climate Jobs", "url": "https://www.remoteclimatejobs.com/",              "priority": "MEDIUM"},
    {"name": "NCWorks",             "url": "https://www.ncworks.gov",                         "priority": "MEDIUM"},
]

# ─── LinkedIn Search Queries ─────────────────────────────────────────────────
LINKEDIN_SEARCHES = [
    "Audio Software Engineer",
    "Solutions Engineer AV",
    "DSP Engineer",
    "Audio QA Engineer",
    "Bioacoustics",
    "Mycologist",
]
