# scorer.py — Job Hunter · Terra Echo Labs · v2.0
# Keyword relevance scoring. Returns (score, matched_keywords).

import re
import config


def score_job(title: str, description: str, company: str) -> tuple[int, list[str]]:
    """
    Match title + description + company name against all KEYWORD_WEIGHTS.
    Returns (total_score, list_of_matched_keywords).
    Case-insensitive. Each keyword matched at most once per job.

    If the job title matches any entry in TITLE_BLOCKLIST, returns (0, ['[BLOCKED: title match]'])
    immediately — these jobs will be skipped by the threshold check in run.py.
    """
    # ── Title blocklist check ────────────────────────────────────────────────
    title_lower = title.lower()
    for blocked_pattern in config.TITLE_BLOCKLIST:
        if blocked_pattern.lower() in title_lower:
            return 0, ["[BLOCKED: title match]"]

    # Combine all text into one searchable blob
    blob = f"{title} {description} {company}".lower()

    total_score = 0
    matched = []

    for keyword, weight in config.KEYWORD_WEIGHTS.items():
        # Use word-boundary-aware search to avoid substring false positives
        # e.g. "ssl" shouldn't match inside "fossil"
        pattern = re.compile(r'\b' + re.escape(keyword.lower()) + r'\b')
        if pattern.search(blob):
            total_score += weight
            matched.append(keyword)

    return total_score, matched
