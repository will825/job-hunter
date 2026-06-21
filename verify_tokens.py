#!/usr/bin/env python3
"""
verify_tokens.py — Checks every company token in config.py and reports status.
Green = jobs found, Yellow = page exists but 0 jobs, Red = 404 / broken.
"""

import json
import re
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, ".")
from config import TARGET_COMPANIES

GREENHOUSE_URL = "https://boards-api.greenhouse.io/v1/boards/{token}/jobs"
LEVER_URL      = "https://api.lever.co/v0/postings/{token}?mode=json"
ASHBY_URL      = "https://api.ashbyhq.com/posting-public/job-board/listJobPostings"

GREEN  = "\033[92m"
YELLOW = "\033[93m"
RED    = "\033[91m"
RESET  = "\033[0m"
BOLD   = "\033[1m"

def check_token(company):
    name  = company["name"]
    ats   = company["ats"]
    token = company["token"]

    try:
        if ats == "ashby":
            # Ashby API requires auth; parse __NEXT_DATA__ JSON embedded in the page
            page_url = f"https://jobs.ashbyhq.com/{token}"
            req = urllib.request.Request(page_url, headers={"User-Agent": "Mozilla/5.0"})
            with urllib.request.urlopen(req, timeout=10) as resp:
                html = resp.read().decode("utf-8", errors="replace")
            match = re.search(r'window\.__appData\s*=\s*(\{.*?\});', html, re.DOTALL)
            if not match:
                return "EMPTY", 0
            data = json.loads(match.group(1))
            postings = data.get("jobBoard", {}).get("jobPostings", [])
            count = len(postings)
            return ("OK", count) if count > 0 else ("EMPTY", 0)

        if ats == "greenhouse":
            url = GREENHOUSE_URL.format(token=token)
        else:
            url = LEVER_URL.format(token=token)

        req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
        with urllib.request.urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
            if ats == "greenhouse":
                count = len(data.get("jobs", []))
            else:
                count = len(data) if isinstance(data, list) else 0

            if count > 0:
                return "OK", count
            else:
                return "EMPTY", 0

    except urllib.error.HTTPError as e:
        return "HTTP_ERR", e.code
    except Exception as e:
        return "ERR", str(e)[:60]


def main():
    print(f"\n{BOLD}Job Hunter — Token Verifier{RESET}")
    print(f"Checking {len(TARGET_COMPANIES)} companies...\n")
    print(f"{'Company':<30} {'ATS':<12} {'Token':<28} {'Status'}")
    print("─" * 90)

    ok_list    = []
    empty_list = []
    broken_list = []

    for company in TARGET_COMPANIES:
        status, detail = check_token(company)
        name  = company["name"]
        ats   = company["ats"]
        token = company["token"]

        if status == "OK":
            color = GREEN
            label = f"✓  {detail} jobs"
            ok_list.append(company)
        elif status == "EMPTY":
            color = YELLOW
            label = "~  0 jobs (page exists)"
            empty_list.append(company)
        else:
            color = RED
            label = f"✗  {status} {detail}"
            broken_list.append(company)

        print(f"{color}{name:<30} {ats:<12} {token:<28} {label}{RESET}")
        time.sleep(0.15)   # be polite

    # ── Summary ──────────────────────────────────────────────────────────────
    print("\n" + "─" * 90)
    print(f"\n{BOLD}Summary{RESET}")
    print(f"  {GREEN}✓ Working with jobs  : {len(ok_list)}{RESET}")
    print(f"  {YELLOW}~ Exists but empty   : {len(empty_list)}{RESET}")
    print(f"  {RED}✗ Broken / 404       : {len(broken_list)}{RESET}")

    if broken_list:
        print(f"\n{BOLD}{RED}Broken tokens — need fixing:{RESET}")
        for c in broken_list:
            if c["ats"] == "greenhouse":
                print(f"  {c['name']:<30} → try: https://boards.greenhouse.io/{c['token']}")
            else:
                print(f"  {c['name']:<30} → try: https://jobs.lever.co/{c['token']}")

    print()


if __name__ == "__main__":
    main()
