# Changelog

## v0.5.0 - 2026-03-20

### Fixed
- Fix stale search results remaining visible when query is deleted to empty

## v0.4.0 - 2026-03-09

### New Features
- Homebrew tap: `brew install materkey/ccs/ccs`
- Shell installer for macOS/Linux
- Published to crates.io as `ccfullsearch`
- cargo-binstall support

### Changed
- Rename package to `ccfullsearch` (binary stays `ccs`)
- Replace release workflow with cargo-dist v0.31.0
- Add Linux arm64 and musl targets (6 platforms total)
- Add MIT license

## v0.3.0 - 2026-03-06

### New Features
- CLI argument parsing with clap (`search`, `list` subcommands, `--tree` flag)
- Render snapshot tests
- Split TUI into separate modules (state, search_mode, tree_mode, render)

### Fixed
- Project filter search scope

## v0.2.0 - 2026-03-05

### New Features
- CLI mode with `search` and `list` subcommands (JSONL output)
- Claude Code skill for automatic session search
- Branch Tree Explorer mode for navigating transcript branches
- Branch-aware resume with automatic fork for non-latest chains
- Claude Desktop sessions support
- Regex search mode toggle
- Word-level cursor movement and deletion
- Ctrl-C support (clear input or quit)
- GitHub release workflow (macOS arm64/x86_64, Linux x86_64, Windows)

### Fixed
- Session resume for paths with spaces, parens, underscores
- Content overflow artifacts in tree view
- Search not finding messages with plain string content
- Desktop session format parsing
- UTF-8 boundary panics in preview and context extraction
- Terminal rendering glitches
- Windows build (conditional compilation for unix exec)

## v0.1.0 - 2026-03-02

### New Features
- Initial implementation of Claude Code session search TUI
- Full-text search across session JSONL files using ripgrep
- Session grouping with timestamps and project context
- Interactive preview with match navigation
- Session resume from search results
