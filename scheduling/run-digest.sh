#!/usr/bin/env bash
# Wrapper the scheduler runs each morning. It cd's into the project (so the
# app finds .env, profile.toml, and jobs.db) and runs the daily digest.
#
# Build the release binary once first:  cargo build --release
set -e
# cd into the project root (this script lives in scheduling/), so the app finds
# .env, profile.toml, and jobs.db regardless of where it's checked out.
cd "$(dirname "$0")/.." || exit 1

BIN="./target/release/job_hunter"
if [ ! -x "$BIN" ]; then
  # Fall back to a debug run if the release binary isn't built yet.
  BIN="$HOME/.cargo/bin/cargo run --quiet --"
fi

echo "===== $(date) ====="
$BIN digest
