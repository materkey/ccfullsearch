---
name: release-ccs
description: Publish a new ccfullsearch release. Bumps version, updates CHANGELOG, commits, tags, pushes, and waits for CI + cargo-dist. Use when user says "release", "publish", "new version", or "зарелизь".
---

# Release ccfullsearch

Publish a new version of ccfullsearch (`ccs`). Handles version bump, changelog, CI verification, and cargo-dist release.

**Input**: Optionally specify version bump type or explicit version. If omitted, infer from unreleased changes.

## Steps

### 1. Determine version

- Read current version from `Cargo.toml` (field `version`)
- Read latest tag: `git tag --sort=-v:refname | head -1`
- Check unreleased changes: `git log --oneline <latest-tag>...HEAD`
- If no unreleased changes, abort with "Nothing to release"
- Determine bump type from changes:
  - Breaking changes or major new features → **major** (x.0.0)
  - New features → **minor** (0.x.0)
  - Bug fixes only → **patch** (0.0.x)
- Use `AskUserQuestion` to confirm version (show suggested + alternatives)

### 2. Update CHANGELOG.md

- Read `CHANGELOG.md`
- Generate changelog entry from commits since last tag: `git log --oneline <latest-tag>...HEAD`
- Group by type: New Features, Fixed, Changed
- Insert new section after `# Changelog` header with format:
  ```
  ## vX.Y.Z - YYYY-MM-DD

  ### New Features
  - ...

  ### Fixed
  - ...

  ### Changed
  - ...
  ```
- Show the changelog entry to user for review before writing

### 3. Bump version

- Edit `Cargo.toml`: update `version = "X.Y.Z"`
- Run `cargo check` to update `Cargo.lock`

### 4. Commit and verify

- Stage: `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`
- Commit: `chore(release): prepare vX.Y.Z`
- Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- If tests fail, fix and re-commit

### 5. Push and verify CI

- Push: `git push origin main`
- Get run ID: `gh run list --limit 1 --repo materkey/ccfullsearch --json databaseId --jq '.[0].databaseId'`
- Block until CI finishes: `gh run watch <run-id> --repo materkey/ccfullsearch --exit-status` (timeout 420s; typical run ~1 min)
- If CI fails (non-zero exit), report and abort — do NOT tag

### 6. Tag and release

- Only after CI is green:
- Tag: `git tag vX.Y.Z`
- Push tag: `git push origin vX.Y.Z`
- This triggers cargo-dist Release workflow automatically

### 7. Wait for Release workflow to finish, then verify

- **Do NOT report "published" until cargo-dist actually publishes the release.** Tag push only *starts* the Release workflow — build, host, publish-homebrew-formula, announce jobs take ~3 min. Reporting success prematurely caused user confusion in a past run (they still saw the old release as Latest).
- Get run ID of the tag-triggered Release workflow: `gh run list --limit 5 --repo materkey/ccfullsearch --json databaseId,headBranch,name --jq '.[] | select(.headBranch=="v{version}" and .name=="Release") | .databaseId' | head -1` (may need a few seconds after tag push for the workflow to appear — if empty, wait 10s and retry)
- Block until it finishes: `gh run watch <run-id> --repo materkey/ccfullsearch --exit-status` (timeout 600s)
- Confirm the release actually exists: `gh release view v{version} --repo materkey/ccfullsearch --json tagName,isLatest --jq '{tag:.tagName,latest:.isLatest}'` — `latest` must be `true`
- Only after both checks pass, report:
  ```
  Release v{version} published.

  CI: ✅ green
  Release workflow: ✅ completed (build → host → publish-homebrew-formula → announce)
  Tag: v{version} (marked as Latest)

  Release page: https://github.com/materkey/ccfullsearch/releases/tag/v{version}
  Homebrew: `brew upgrade ccs` (tap materkey/homebrew-ccs formula updated automatically)
  ```
- If Release workflow fails, report the failing job and which stage — do NOT delete the tag; failures are usually re-runnable via `gh run rerun <run-id> --repo materkey/ccfullsearch`

## Important notes

- **Never tag before CI is green** — cargo-dist triggers on tag push
- **Use `--force-with-lease`** if force-pushing is needed (never plain `--force`)
- **cargo-dist config** is in `dist-workspace.toml` — targets: macOS (arm64, x86_64), Linux (gnu, musl for arm64 and x86_64)
- **Homebrew tap**: `materkey/homebrew-ccs`, formula name `ccs`
- **crates.io**: manual `cargo publish` if needed (not automated)
- **History must be linear** — rebase, not merge
