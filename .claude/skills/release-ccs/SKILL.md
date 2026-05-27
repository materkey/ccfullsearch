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
- **Keep entries terse and user-facing.** One sentence per bullet describing observable behavior. No implementation details (struct names, internal modules, cache strategies, sentinel values, type signatures) — those belong in the commit body, not the changelog. If a commit body lists 5 follow-up fixes that harden one feature, the changelog gets one bullet for the feature itself.
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
- Confirm the release actually exists: `gh release view v{version} --repo materkey/ccfullsearch --json tagName,isDraft,isPrerelease` — `isDraft` and `isPrerelease` must both be `false`. Also check it's the latest: `gh api repos/materkey/ccfullsearch/releases/latest --jq '.tag_name'` must equal `v{version}`. (Note: `isLatest` is not a valid field on `gh release view` — use the API endpoint instead.)
- **Polish release notes.** cargo-dist copies the CHANGELOG entry verbatim into the GitHub release body, then appends auto-generated Install/Download sections. Fetch the current body (`gh release view v{version} --json body --jq '.body'`), then:
  - Add an "Update an existing install" section near the top of the Install block: ` ```sh\nccs update\n``` ` (above the shell-script and Homebrew snippets). Users who already have `ccs` are the most common audience.
  - Re-verify the Release Notes section reads cleanly — no internal jargon leaked through from the CHANGELOG.
  - Write the new body via `gh release edit v{version} --repo materkey/ccfullsearch --notes-file /tmp/release-notes-{version}.md`. If you also trim the CHANGELOG entry, edit `CHANGELOG.md` to match and commit it as a follow-up (`docs(changelog): trim v{version} entry`).

### 8. Wait for crates.io publication, then verify

- The `Publish to crates.io` workflow (`.github/workflows/publish-crates.yml`) is *supposed* to fire on the `release: published` event emitted by step 7. **It does NOT fire automatically** — known GitHub Actions limitation: releases created by cargo-dist using the default `GITHUB_TOKEN` don't emit events that trigger downstream workflows. Verified missing on v0.15.0 (2026-05-27).
- **Dispatch it manually instead** (don't wait): `gh workflow run publish-crates.yml --repo materkey/ccfullsearch -f tag=v{version}` — then `sleep 5` and find the run via `gh run list --workflow=publish-crates.yml --repo materkey/ccfullsearch --limit 3 --json databaseId,headBranch,status,event,createdAt` (look for the most recent `event=="workflow_dispatch"` entry).
- Block until it finishes: `gh run watch <run-id> --repo materkey/ccfullsearch --exit-status` (timeout 300s; typical ~1 min)
- Verify crates.io has the version: `curl -s -H "User-Agent: ccs-release-skill" https://crates.io/api/v1/crates/ccfullsearch | jq -r '.crate.newest_version'` — must equal `{version}`
- If the workflow failed because `CARGO_REGISTRY_TOKEN` is missing or expired, see the "crates.io token" note below — do NOT fall back to running `cargo publish` locally without checking with the user first (a local publish from a dirty working tree can ship unintended changes).
- Only after homebrew + crates checks pass, report:
  ```
  Release v{version} published.

  CI: ✅ green
  Release workflow: ✅ completed (build → host → publish-homebrew-formula → announce)
  Crates.io: ✅ v{version} live (https://crates.io/crates/ccfullsearch)
  Tag: v{version} (marked as Latest)

  Release page: https://github.com/materkey/ccfullsearch/releases/tag/v{version}
  Update: `ccs update` (existing installs)
  Homebrew: `brew upgrade ccs` (tap materkey/homebrew-ccs formula updated automatically)
  Cargo: `cargo install ccfullsearch --locked`
  ```
- If Release workflow fails, report the failing job and which stage — do NOT delete the tag; failures are usually re-runnable via `gh run rerun <run-id> --repo materkey/ccfullsearch`

## Important notes

- **Never tag before CI is green** — cargo-dist triggers on tag push
- **Use `--force-with-lease`** if force-pushing is needed (never plain `--force`)
- **cargo-dist config** is in `dist-workspace.toml` — targets: macOS (arm64, x86_64), Linux (gnu, musl for arm64 and x86_64). `release.yml` is autogenerated by `dist generate` — do not edit by hand.
- **Homebrew tap**: `materkey/homebrew-ccs`, formula name `ccs`
- **crates.io**: standalone workflow `.github/workflows/publish-crates.yml` (kept out of cargo-dist's `release.yml` so it survives `dist generate`). Listens on `release: published` *in theory*, but in practice cargo-dist's `GITHUB_TOKEN`-created release doesn't fire that event — always dispatch manually via `gh workflow run publish-crates.yml -f tag=vX.Y.Z`. Long-term options if the manual step gets tedious: (a) move publish into cargo-dist's announce job, (b) use a PAT in cargo-dist's create-release step, (c) leave as-is and dispatch each time.
- **crates.io token**: repo secret `CARGO_REGISTRY_TOKEN` (scope `publish-update`, crate-restricted to `ccfullsearch`). Generate at https://crates.io/settings/tokens, set with `gh secret set CARGO_REGISTRY_TOKEN --repo materkey/ccfullsearch`. If it expires, the publish-crates workflow will fail with 401 — rotate and re-run.
- **History must be linear** — rebase, not merge
