# database.py — Job Hunter · Terra Echo Labs · v2.0
# SQLite layer — append-only audit log. NEVER DELETE rows.

import hashlib
import sqlite3
import logging
from datetime import datetime, timezone
from pathlib import Path

import config

logger = logging.getLogger(__name__)


def _connect() -> sqlite3.Connection:
    Path(config.DB_PATH).parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(config.DB_PATH)
    conn.row_factory = sqlite3.Row
    return conn


def init_db() -> None:
    """Create tables if they don't exist. Safe to call on every run."""
    with _connect() as conn:
        conn.executescript("""
            CREATE TABLE IF NOT EXISTS jobs (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                company     TEXT NOT NULL,
                location    TEXT,
                url         TEXT NOT NULL,
                source      TEXT,
                score       INTEGER DEFAULT 0,
                keywords    TEXT,
                description TEXT,
                notified    INTEGER DEFAULT 0,
                seen_at     TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at  TEXT NOT NULL,
                finished_at TEXT,
                jobs_found  INTEGER DEFAULT 0,
                jobs_new    INTEGER DEFAULT 0,
                jobs_alerted INTEGER DEFAULT 0,
                errors      TEXT
            );
        """)
    logger.info("Database initialized at %s", config.DB_PATH)


def make_job_id(title: str, company: str, url: str) -> str:
    """SHA-1 hash of title+company+url, truncated to 16 chars."""
    raw = f"{title.lower()}|{company.lower()}|{url}"
    return hashlib.sha1(raw.encode()).hexdigest()[:16]


def save_job(
    title: str,
    company: str,
    location: str,
    url: str,
    source: str,
    score: int,
    keywords: list[str],
    description: str,
) -> bool:
    """
    Insert job if new. Returns True on insert, False if duplicate.
    Never raises — caller should handle exceptions.
    """
    job_id = make_job_id(title, company, url)
    seen_at = datetime.now(timezone.utc).isoformat()
    keywords_str = ", ".join(keywords)

    try:
        with _connect() as conn:
            existing = conn.execute(
                "SELECT id FROM jobs WHERE id = ?", (job_id,)
            ).fetchone()

            if existing:
                return False

            conn.execute(
                """INSERT INTO jobs
                   (id, title, company, location, url, source, score, keywords, description, notified, seen_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?)""",
                (job_id, title, company, location, url, source, score, keywords_str, description[:2000], seen_at),
            )
        return True
    except Exception as e:
        logger.error("save_job error for '%s' @ '%s': %s", title, company, e)
        return False


def mark_notified(job_id: str) -> None:
    """Mark a job as having been sent to Discord."""
    with _connect() as conn:
        conn.execute("UPDATE jobs SET notified = 1 WHERE id = ?", (job_id,))


def get_unnotified_jobs(min_score: int = 1) -> list[sqlite3.Row]:
    """Return all unsent jobs at or above min_score, ordered score ASC (lowest first for Discord)."""
    with _connect() as conn:
        return conn.execute(
            """SELECT * FROM jobs
               WHERE notified = 0 AND score >= ?
               ORDER BY score ASC""",
            (min_score,),
        ).fetchall()


def log_run(started_at: str, finished_at: str, jobs_found: int, jobs_new: int, jobs_alerted: int, errors: str = "") -> int:
    """Insert a run audit record. Returns the new run id."""
    with _connect() as conn:
        cur = conn.execute(
            """INSERT INTO runs (started_at, finished_at, jobs_found, jobs_new, jobs_alerted, errors)
               VALUES (?, ?, ?, ?, ?, ?)""",
            (started_at, finished_at, jobs_found, jobs_new, jobs_alerted, errors),
        )
        return cur.lastrowid
