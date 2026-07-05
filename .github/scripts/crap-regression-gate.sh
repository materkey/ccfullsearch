#!/usr/bin/env bash
# PR gate: fail on a regressed function over threshold OR a new function over
# threshold. cargo-crap's --fail-regression catches only regressions (a new
# function is status "new", not "regressed") and counts any score increase
# past the epsilon even for trivial sub-threshold functions, so we read the
# JSON delta and apply both filters ourselves. threshold/epsilon come from
# .cargo-crap.toml (the epsilon slack absorbs run-to-run coverage jitter).
# Requires lcov.info and baseline/crap-current.json.
set -euo pipefail

if ! jq -e .version baseline/crap-current.json >/dev/null 2>&1; then
  echo "No usable baseline available — skipping regression gate."
  exit 0
fi

# The jq filters below need the same threshold cargo-crap reads from the
# config; failing loudly on a broken config beats gating on a silent default.
threshold=$(python3 -c 'import tomllib; print(tomllib.load(open(".cargo-crap.toml", "rb"))["threshold"])')

cargo crap \
  --lcov lcov.info \
  --baseline baseline/crap-current.json \
  --format json \
  --output delta.json

regressed=$(jq --argjson t "$threshold" \
  '[.entries[] | select(.status == "regressed" and .crap > $t)] | length' delta.json)
new_hotspots=$(jq --argjson t "$threshold" \
  '[.entries[] | select(.status == "new" and .crap > $t)] | length' delta.json)

echo "Regressed above $threshold: $regressed · New functions above $threshold: $new_hotspots"

if [ "$regressed" -gt 0 ] || [ "$new_hotspots" -gt 0 ]; then
  echo "::error::CRAP gate failed — $regressed regression(s), $new_hotspots new function(s) above $threshold."
  jq -r --argjson t "$threshold" \
    '.entries[] | select((.status == "regressed" or .status == "new") and .crap > $t) | "  \(.status)\tCRAP \(.crap)\t\(.function) (\(.file):\(.line))"' \
    delta.json
  exit 1
fi
echo "CRAP gate passed — no regressions, no new hot spots."
