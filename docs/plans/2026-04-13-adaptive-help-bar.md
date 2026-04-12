# Adaptive help bar — threshold-per-item truncation

## Overview
- Fix bottom help/hotkey bar that gets clipped on narrow terminals (<108 cols). Apply the industry-standard "Pattern A: threshold per item" approach (used by Helix, mini.statusline) where each hint has a `min_width` threshold and is hidden when the terminal is too narrow.

## Context (from discovery)
- `src/tui/render_search.rs` lines 242–299: help bar with 5 conditional branches, hardcoded strings, no width check
- `src/tui/render_tree.rs` lines 55–62: simpler help bar, same problem
- `render_search.rs:554-567`: existing `truncate_to_width()` helper (not used for help bar)
- Filter label (`filter_label`) is a colored span injected between dim-styled text — must be bundled atomically with `[Ctrl+H]`
- ratatui has no built-in adaptive help bar widget

## Development Approach
- **Testing approach**: Regular — add unit tests for the new `build_help_line` function
- Complete each task fully before moving to the next

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Add HintItem struct and build_help_line function in render_search.rs
- [ ] Add `HintItem` struct with `spans: Vec<Span<'a>>` and `min_width: u16` near the top of render_search.rs (after imports)
- [ ] Add `fn build_help_line<'a>(hints: &[HintItem<'a>], available_width: u16) -> Line<'a>` as a module-level function — filters hints by min_width, joins with "  " separator
- [ ] Add unit tests for `build_help_line`: width 120 (all shown), width 50 (only min_width=0), width 85 (mid-range)

### Task 2: Refactor search mode help bar to use HintItem
- [ ] Replace all 5 branches in the help bar section (lines 260–296) with HintItem-based construction
- [ ] For each branch, define hints as a Vec<HintItem> with appropriate min_width thresholds:
  - AI mode: `[Enter] Rank` (0), `[↑↓] Navigate` (0), `[Esc/Ctrl+G] Cancel` (0)
  - Preview mode: `[Tab/Ctrl+V/Enter] Close preview` (0), `[Ctrl+A] Project` (70), `[Ctrl+H] <label>` (60), `[Ctrl+R] Regex` (90), `[Esc] Quit` (0)
  - Recent mode: `[↑↓] Navigate` (0), `[Enter] Resume` (0), `[Ctrl+G] AI` (80), `[Ctrl+A] Project` (70), `[Ctrl+H] <label>` (60), `[Ctrl+B] Tree` (90), `[Esc] Quit` (0)
  - Search results: `[↑↓] Navigate` (0), `[→←] Expand` (100), `[Tab/Ctrl+V] Preview` (90), `[Enter] Resume` (0), `[Ctrl+G] AI` (80), `[Ctrl+A] Project` (70), `[Ctrl+H] <label>` (60), `[Ctrl+B] Tree` (90), `[Ctrl+R] Regex` (100), `[Esc] Quit` (0)
  - Default (empty): `[↑↓] Navigate` (0), `[Tab/Ctrl+V] Preview` (80), `[Enter] Resume` (0), `[Ctrl+A] Project` (70), `[Ctrl+H] <label>` (60), `[Ctrl+R] Regex` (90), `[Esc] Quit` (0)
- [ ] Bundle `[Ctrl+H]` key text and colored `filter_label` into one atomic HintItem with `spans: vec![dim_span("[Ctrl+H] "), styled_span(label)]`
- [ ] Call `build_help_line(&hints, help_area.width)` and render result as Paragraph

### Task 3: Refactor tree mode help bar in render_tree.rs
- [ ] Apply same HintItem pattern to render_tree.rs lines 55–62
- [ ] Normal mode hints: `[↑↓] Navigate` (0), `[←→] Jump branches` (80), `[Tab] Preview` (70), `[Enter] Resume` (0), `[b/Esc] Back` (0)
- [ ] Preview mode hints: `[Tab/Enter] Close preview` (0), `[Esc] Back` (0)
- [ ] Import or inline `build_help_line` (reuse from render_search.rs via existing cross-import pattern)

### Task 4: Verify and commit
- [ ] Run `cargo fmt --check` and fix if needed
- [ ] Run `cargo clippy --all-targets --all-features -- -D warnings` and fix warnings
- [ ] Run `cargo test` — all tests pass including new help_line tests
- [ ] Run `cargo build` — clean build
- [ ] Commit changes with descriptive message

## Post-Completion
- Run `cargo run` and manually resize terminal to verify hints adapt at ~60, ~80, ~100, ~120 cols
- Check that filter label never appears orphaned (always bundled with [Ctrl+H])
