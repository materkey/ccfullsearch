#!/usr/bin/env bash
# Force-push the shields badge and the PR-gate baseline to the orphan `badges`
# branch as a single fresh commit (no history growth). The baseline lives here
# rather than in an expiring artifact so PRs get it with one git fetch — no
# API scan, no 90-day expiry, and fork PRs can't poison it (only this job has
# contents: write). Skips if main moved past this run, so an older run
# finishing late can't overwrite `badges` with stale data.
set -euo pipefail

latest_main=$(git ls-remote origin refs/heads/main | cut -f1)
if [ -n "$latest_main" ] && [ "$latest_main" != "$GITHUB_SHA" ]; then
  echo "main advanced past $GITHUB_SHA (now $latest_main) — newer run will publish; skipping stale badge."
  exit 0
fi

have_prev=false
if git fetch --no-tags --depth 1 origin badges 2>/dev/null; then
  have_prev=true
fi

# The crap job failed before generating a baseline: carry the previous one
# forward so PRs keep a comparison point across a broken main build.
if [ ! -f crap-current.json ] && [ "$have_prev" = true ] \
    && git cat-file -e FETCH_HEAD:crap-current.json 2>/dev/null; then
  git show FETCH_HEAD:crap-current.json > crap-current.json
  echo "Carrying the previous baseline forward."
fi

if [ ! -f crap-badge.json ]; then
  # Publish an explicit "unavailable" badge — silently keeping the previous
  # score would leave the README advertising a number main no longer has.
  echo "No badge artifact — crap job failed before generating it; publishing an 'unavailable' badge."
  printf '%s\n' '{"schemaVersion":1,"label":"CRAP","message":"unavailable","color":"red"}' > crap-badge.json
fi

git config user.name "github-actions[bot]"
git config user.email "github-actions[bot]@users.noreply.github.com"
tree=$({
  printf '100644 blob %s\tcrap-badge.json\n' "$(git hash-object -w crap-badge.json)"
  if [ -f crap-current.json ]; then
    printf '100644 blob %s\tcrap-current.json\n' "$(git hash-object -w crap-current.json)"
  fi
} | git mktree)
commit=$(git commit-tree "$tree" -m "chore: update CRAP badge and baseline")
git push --force origin "$commit:refs/heads/badges"
