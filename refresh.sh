#!/usr/bin/env bash
# One-command test loop: fetch + score against your profile, rebuild the
# browsable snapshot, and open it. Run this after editing profile.toml to see
# your rankings change.
set -e
cd "$(dirname "$0")"

echo "▶ Scanning boards and scoring against profile.toml…"
cargo run --quiet

echo "▶ Building snapshot…"
python3 tools/build_snapshot.py

echo "▶ Opening…"
open jobs_snapshot.html
