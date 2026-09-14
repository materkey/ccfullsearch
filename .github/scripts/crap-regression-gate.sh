#!/usr/bin/env bash
# PR and main-push gate: fail on a regressed function over threshold OR a new function over
# threshold. cargo-crap's --fail-regression catches only regressions (a new
# function is status "new", not "regressed") and counts any score increase
# past the epsilon even for trivial sub-threshold functions, so we read the
# JSON delta and apply both filters ourselves. threshold/epsilon come from
# .cargo-crap.toml (the epsilon slack absorbs run-to-run coverage jitter).
# Requires lcov.info and baseline/crap-current.json.
set -euo pipefail

if ! jq -e '(.version | type == "string") and (.entries | type == "array") and (.duplicates | type == "array")' baseline/crap-current.json >/dev/null 2>&1; then
  echo "::error::No usable CRAP baseline with duplicate detection available."
  exit 1
fi

# The jq filters below need the same threshold cargo-crap reads from the
# config; failing loudly on a broken config beats gating on a silent default.
threshold=$(python3 -c 'import tomllib; print(tomllib.load(open(".cargo-crap.toml", "rb"))["threshold"])')

cargo crap \
  --lcov lcov.info \
  --baseline baseline/crap-current.json \
  --format json \
  --output delta.json

# Missing duplicate output means detection was disabled or its schema changed.
jq -e '(.entries | type == "array") and (.duplicates | type == "array")' delta.json >/dev/null

# A pair is identified by its two (file, function) endpoints, regardless of
# order or source line shifts. Similarity jitter for an existing pair is not
# a new duplicate; renames/moves require review as newly identified pairs.
new_duplicates=$(jq --slurpfile baseline baseline/crap-current.json '
  def pair_key:
    [[.first_file, .first_function], [.second_file, .second_function]] | sort;
  ($baseline[0].duplicates | map(pair_key)) as $known |
  [.duplicates[] | (pair_key) as $key |
    select(any($known[]; . == $key) | not)]
' delta.json)
new_duplicate_count=$(jq 'length' <<< "$new_duplicates")
echo "New duplicate pairs: $new_duplicate_count"
if [ "$new_duplicate_count" -gt 0 ]; then
  jq -r '.[] | "  similarity \(.score): \(.first_function) (\(.first_file):\(.first_start_line)) <-> \(.second_function) (\(.second_file):\(.second_start_line))"' <<< "$new_duplicates"
fi

regressed=$(jq --argjson t "$threshold" \
  '[.entries[] | select(.status == "regressed" and .crap > $t)] | length' delta.json)
new_hotspots=$(jq --argjson t "$threshold" \
  '[.entries[] | select(.status == "new" and .crap > $t)] | length' delta.json)

echo "Regressed above $threshold: $regressed · New functions above $threshold: $new_hotspots"

if [ "$regressed" -gt 0 ] || [ "$new_hotspots" -gt 0 ] || [ "$new_duplicate_count" -gt 0 ]; then
  echo "::error::CRAP gate failed — $regressed regression(s), $new_hotspots new function(s) above $threshold, $new_duplicate_count new duplicate pair(s)."
  jq -r --argjson t "$threshold" \
    '.entries[] | select((.status == "regressed" or .status == "new") and .crap > $t) | "  \(.status)\tCRAP \(.crap)\t\(.function) (\(.file):\(.line))"' \
    delta.json
  exit 1
fi
echo "CRAP gate passed — no regressions, no new hot spots, no new duplicate pairs."
