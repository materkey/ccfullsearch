#!/usr/bin/env bash
# Fetch the baseline that main last published to the orphan `badges` branch
# (see crap-push-badge.sh) into baseline/crap-current.json. Replaces artifact
# transport: no 90-day expiry, no API scan for the right run — the branch tip
# always holds the newest baseline. Best-effort: a missing branch or file
# (bootstrap, badge job never succeeded yet) just means the gate skips.
set -uo pipefail

mkdir -p baseline
if git fetch --no-tags --depth 1 origin badges 2>/dev/null \
    && git cat-file -e FETCH_HEAD:crap-current.json 2>/dev/null; then
  git show FETCH_HEAD:crap-current.json > baseline/crap-current.json
  echo "Baseline fetched from the badges branch."
else
  echo "No baseline on the badges branch yet — regression gate will be skipped."
fi
