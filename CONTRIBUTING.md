# Contributing to ccfullsearch

Thanks for your interest in `ccs`. This project is maintained in spare time; the
goal of this guide is to make PRs easier to review and safer to merge.

## Required local check

Before opening a PR, run:

```bash
make check
```

## Tests and fixtures

Please add or update tests for behavior changes.

## AI-assisted contributions

AI-assisted development is welcome — this project is itself an AI-tooling
project, and I use AI assistants on it daily. There are still clear
expectations for AI-assisted PRs.

- Contributors must review their own code before submitting, whether written
  by AI or not.

**What I will not accept:**

- Unreviewed AI output dumped for the maintainer to fix.
- Code without tests or with failing tests / linter.
- Changes that ignore project conventions after being pointed to them.
- PRs that do not respond to review feedback.

## Documentation

Update `README.md` when user-facing behavior changes (new CLI flags, new
session locations, new keys, new install instructions).

Update `CLAUDE.md` when internal architecture changes (new module, new data
flow, new error model). It is the source of truth for contributors and for
AI assistants used on the project.
