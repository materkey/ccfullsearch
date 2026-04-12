---
name: release
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
- Wait for CI: `gh run list --limit 1 --repo materkey/ccfullsearch`
- Poll until CI completes (check every 30s, max 5 min)
- If CI fails, report and abort — do NOT tag

### 6. Tag and release

- Only after CI is green:
- Tag: `git tag vX.Y.Z`
- Push tag: `git push origin vX.Y.Z`
- This triggers cargo-dist Release workflow automatically

### 7. Verify release

- Check Release workflow started: `gh run list --limit 1 --repo materkey/ccfullsearch`
- Report:
  ```
  Release v{version} published.
  
  CI: ✅ green
  Release workflow: started (cargo-dist builds for all platforms)
  Tag: v{version}
  
  Release page: https://github.com/materkey/ccfullsearch/releases/tag/v{version}
  Homebrew: `brew upgrade ccs` (updates automatically via tap materkey/homebrew-ccs)
  ```

## Important notes

- **Never tag before CI is green** — cargo-dist triggers on tag push
- **Use `--force-with-lease`** if force-pushing is needed (never plain `--force`)
- **cargo-dist config** is in `dist-workspace.toml` — targets: macOS (arm64, x86_64), Linux (gnu, musl for arm64 and x86_64)
- **Homebrew tap**: `materkey/homebrew-ccs`, formula name `ccs`
- **crates.io**: manual `cargo publish` if needed (not automated)
- **History must be linear** — rebase, not merge
