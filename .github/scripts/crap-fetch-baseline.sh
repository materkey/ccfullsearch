#!/usr/bin/env bash
# Fetch the baseline that main last published to the orphan `badges` branch
# (see crap-push-badge.sh) into baseline/crap-current.json. Replaces artifact
# transport: no 90-day expiry, no API scan for the right run — the branch tip
# holds the last accepted baseline. Missing data must fail closed so a fetch
# failure cannot silently accept regressions.
set -euo pipefail

mkdir -p baseline
if git fetch --no-tags --depth 1 origin badges 2>/dev/null \
    && git cat-file -e FETCH_HEAD:crap-current.json 2>/dev/null; then
  git show FETCH_HEAD:crap-current.json > baseline/crap-current.json
  echo "Baseline fetched from the badges branch."
else
  echo "::error::Cannot fetch CRAP baseline from the badges branch."
  exit 1
fi
