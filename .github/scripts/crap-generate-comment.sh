#!/usr/bin/env bash
# Generate crap-comment.md (the sticky PR comment). Never fails the job, but
# must not leave a stale or PR-pre-seeded file behind: the privileged
# workflow_run bot posts whatever ends up here, so we keep only a file
# cargo-crap actually wrote. REPO_URL / COMMIT_REF come from the workflow env;
# threshold/epsilon come from .cargo-crap.toml, keeping the comment and the
# regression gate in agreement.
set -uo pipefail

# Drop anything the untrusted PR build may have planted under this name.
rm -f crap-comment.md

# This step runs under always(): with coverage generation failed there is
# nothing to report, so don't add a doomed cargo-crap error to the log.
[ -f lcov.info ] || exit 0

args=(--lcov lcov.info --format pr-comment --repo-url "$REPO_URL" --commit-ref "$COMMIT_REF")

have_baseline=false
if jq -e .version baseline/crap-current.json >/dev/null 2>&1; then
  have_baseline=true
  args+=(--baseline baseline/crap-current.json)
fi

if cargo crap "${args[@]}" --output crap-comment.md; then
  if [ "$have_baseline" = false ]; then
    printf '\n_No baseline available — showing absolute scores only._\n' >> crap-comment.md
  fi
else
  rm -f crap-comment.md
fi
