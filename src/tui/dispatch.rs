use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// All possible user actions in the TUI.
/// Produced by `classify_key`, consumed by `App::handle_action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    // -- Global --
    Quit,

    // -- Search mode: navigation --
    Up,
    Down,
    Left,
    Right,
    Tab,
    Enter,

    // -- Search mode: editing --
    InputChar(char),
    Backspace,
    Delete,
    ClearInput,
    DeleteWordLeft,
    DeleteWordRight,

    // -- Search mode: cursor movement --
    MoveWordLeft,
    MoveWordRight,
    MoveHome,
    MoveEnd,

    // -- Search mode: toggles --
    ToggleRegex,
    ToggleProjectFilter,
    ToggleAutomationFilter,
    TogglePreview,
    ExitPreview,

    // -- Search mode: AI --
    EnterAiMode,
    ExitAiMode,

    // -- Search mode: tree entry --
    EnterTreeMode,
    EnterTreeModeRecent,

    // -- Tree mode --
    TreeUp,
    TreeDown,
    TreeLeft,
    TreeRight,
    TreeTab,
    TreeEnter,
    ExitTreeMode,

    // -- Fallthrough --
    Noop,
}

/// Subset of App state needed by `classify_key` to resolve ambiguous keys.
#[derive(Debug, Clone)]
pub struct KeyContext {
    pub tree_mode: bool,
    pub input_empty: bool,
    pub preview_mode: bool,
    pub in_recent_sessions_mode: bool,
    pub has_recent_sessions: bool,
    pub has_groups: bool,
    pub ai_mode: bool,
    pub ai_input_empty: bool,
}

/// Returns true if the key event is Ctrl+H (sent as Ctrl+Backspace on some terminals).
fn is_ctrl_h(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('h') | KeyCode::Backspace)
}

/// Map a raw key event to a `KeyAction`, given the current TUI context.
///
/// This function is pure — it reads no mutable state and has no side effects,
/// making every key combination trivially testable.
pub fn classify_key(key: KeyEvent, ctx: &KeyContext) -> KeyAction {
    if ctx.tree_mode {
        return classify_tree_key(key);
    }
    classify_search_key(key, ctx)
}

fn classify_tree_key(key: KeyEvent) -> KeyAction {
    // Ctrl+C always exits tree mode
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::ExitTreeMode;
    }

    match key.code {
        KeyCode::Esc => KeyAction::ExitTreeMode,
        KeyCode::Up => KeyAction::TreeUp,
        KeyCode::Down => KeyAction::TreeDown,
        KeyCode::Left => KeyAction::TreeLeft,
        KeyCode::Right => KeyAction::TreeRight,
        KeyCode::Tab => KeyAction::TreeTab,
        KeyCode::Enter => KeyAction::TreeEnter,
        KeyCode::Char('b') => KeyAction::ExitTreeMode,
        KeyCode::Char('q') => KeyAction::Quit,
        _ => KeyAction::Noop,
    }
}

fn classify_search_key(key: KeyEvent, ctx: &KeyContext) -> KeyAction {
    // --- Ctrl combinations (checked first, before plain keys) ---

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if ctx.ai_mode {
            return if ctx.ai_input_empty {
                KeyAction::ExitAiMode
            } else {
                KeyAction::ClearInput
            };
        }
        return if ctx.input_empty {
            KeyAction::Quit
        } else {
            KeyAction::ClearInput
        };
    }

    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::ToggleRegex;
    }

    if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return if ctx.in_recent_sessions_mode {
            if ctx.has_recent_sessions {
                KeyAction::EnterTreeModeRecent
            } else {
                KeyAction::Noop
            }
        } else if ctx.has_groups {
            KeyAction::EnterTreeMode
        } else {
            KeyAction::Noop
        };
    }

    // Word-movement: Alt+Left / Ctrl+Left / Alt+B
    if key.code == KeyCode::Left
        && key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
        || key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::ALT)
    {
        return KeyAction::MoveWordLeft;
    }

    // Word-movement: Alt+Right / Ctrl+Right / Alt+F
    if key.code == KeyCode::Right
        && key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
        || key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::ALT)
    {
        return KeyAction::MoveWordRight;
    }

    // Alt+Backspace -> delete word left
    if key.code == KeyCode::Backspace && key.modifiers.contains(KeyModifiers::ALT) {
        return KeyAction::DeleteWordLeft;
    }

    // Alt+D -> delete word right
    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::ALT) {
        return KeyAction::DeleteWordRight;
    }

    // Ctrl+W -> delete word left
    if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::DeleteWordLeft;
    }

    // Ctrl+A -> toggle project filter
    if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::ToggleProjectFilter;
    }

    // Ctrl+H / Ctrl+Backspace -> toggle automation filter
    if is_ctrl_h(key) {
        return KeyAction::ToggleAutomationFilter;
    }

    // Ctrl+V -> toggle preview (same as Tab)
    if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::TogglePreview;
    }

    // Ctrl+E -> move cursor to end
    if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::MoveEnd;
    }

    // Ctrl+G -> toggle AI search mode
    if key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return if ctx.ai_mode {
            KeyAction::ExitAiMode
        } else {
            KeyAction::EnterAiMode
        };
    }

    // --- Plain keys ---

    match key.code {
        KeyCode::Esc => {
            if ctx.ai_mode {
                KeyAction::ExitAiMode
            } else if ctx.preview_mode {
                KeyAction::ExitPreview
            } else {
                KeyAction::Quit
            }
        }
        KeyCode::Home => KeyAction::MoveHome,
        KeyCode::End => KeyAction::MoveEnd,
        KeyCode::Up => KeyAction::Up,
        KeyCode::Down => KeyAction::Down,
        KeyCode::Left => KeyAction::Left,
        KeyCode::Right => KeyAction::Right,
        KeyCode::Tab => KeyAction::Tab,
        KeyCode::Enter => KeyAction::Enter,
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Delete => KeyAction::Delete,
        KeyCode::Char(c) => KeyAction::InputChar(c),
        _ => KeyAction::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn search_ctx() -> KeyContext {
        KeyContext {
            tree_mode: false,
            input_empty: false,
            preview_mode: false,
            in_recent_sessions_mode: false,
            has_recent_sessions: false,
            has_groups: false,
            ai_mode: false,
            ai_input_empty: true,
        }
    }

    fn tree_ctx() -> KeyContext {
        KeyContext {
            tree_mode: true,
            ..search_ctx()
        }
    }

    // =====================================================================
    // Tree mode
    // =====================================================================

    #[test]
    fn tree_ctrl_c_exits_tree() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &tree_ctx()
            ),
            KeyAction::ExitTreeMode,
        );
    }

    #[test]
    fn tree_esc_exits_tree() {
        assert_eq!(
            classify_key(key(KeyCode::Esc), &tree_ctx()),
            KeyAction::ExitTreeMode,
        );
    }

    #[test]
    fn tree_b_exits_tree() {
        assert_eq!(
            classify_key(key(KeyCode::Char('b')), &tree_ctx()),
            KeyAction::ExitTreeMode,
        );
    }

    #[test]
    fn tree_q_quits() {
        assert_eq!(
            classify_key(key(KeyCode::Char('q')), &tree_ctx()),
            KeyAction::Quit,
        );
    }

    #[test]
    fn tree_navigation_keys() {
        assert_eq!(
            classify_key(key(KeyCode::Up), &tree_ctx()),
            KeyAction::TreeUp
        );
        assert_eq!(
            classify_key(key(KeyCode::Down), &tree_ctx()),
            KeyAction::TreeDown
        );
        assert_eq!(
            classify_key(key(KeyCode::Left), &tree_ctx()),
            KeyAction::TreeLeft
        );
        assert_eq!(
            classify_key(key(KeyCode::Right), &tree_ctx()),
            KeyAction::TreeRight
        );
    }

    #[test]
    fn tree_tab_and_enter() {
        assert_eq!(
            classify_key(key(KeyCode::Tab), &tree_ctx()),
            KeyAction::TreeTab
        );
        assert_eq!(
            classify_key(key(KeyCode::Enter), &tree_ctx()),
            KeyAction::TreeEnter
        );
    }

    #[test]
    fn tree_unknown_key_is_noop() {
        assert_eq!(
            classify_key(key(KeyCode::Char('x')), &tree_ctx()),
            KeyAction::Noop
        );
        assert_eq!(
            classify_key(key(KeyCode::F(1)), &tree_ctx()),
            KeyAction::Noop
        );
    }

    // =====================================================================
    // Search mode: Ctrl combinations
    // =====================================================================

    #[test]
    fn search_ctrl_c_empty_input_quits() {
        let ctx = KeyContext {
            input_empty: true,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL), &ctx),
            KeyAction::Quit,
        );
    }

    #[test]
    fn search_ctrl_c_nonempty_input_clears() {
        let ctx = KeyContext {
            input_empty: false,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL), &ctx),
            KeyAction::ClearInput,
        );
    }

    #[test]
    fn search_ctrl_r_toggles_regex() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::ToggleRegex,
        );
    }

    #[test]
    fn search_ctrl_b_with_groups_enters_tree() {
        let ctx = KeyContext {
            has_groups: true,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('b'), KeyModifiers::CONTROL), &ctx),
            KeyAction::EnterTreeMode,
        );
    }

    #[test]
    fn search_ctrl_b_recent_mode_with_sessions_enters_tree_recent() {
        let ctx = KeyContext {
            in_recent_sessions_mode: true,
            has_recent_sessions: true,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('b'), KeyModifiers::CONTROL), &ctx),
            KeyAction::EnterTreeModeRecent,
        );
    }

    #[test]
    fn search_ctrl_b_recent_mode_no_sessions_is_noop() {
        let ctx = KeyContext {
            in_recent_sessions_mode: true,
            has_recent_sessions: false,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('b'), KeyModifiers::CONTROL), &ctx),
            KeyAction::Noop,
        );
    }

    #[test]
    fn search_ctrl_b_no_groups_is_noop() {
        let ctx = KeyContext {
            has_groups: false,
            in_recent_sessions_mode: false,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('b'), KeyModifiers::CONTROL), &ctx),
            KeyAction::Noop,
        );
    }

    #[test]
    fn search_ctrl_a_toggles_project_filter() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::ToggleProjectFilter,
        );
    }

    #[test]
    fn search_ctrl_h_toggles_automation_filter() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('h'), KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::ToggleAutomationFilter,
        );
    }

    #[test]
    fn search_ctrl_backspace_toggles_automation_filter() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Backspace, KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::ToggleAutomationFilter,
        );
    }

    #[test]
    fn search_ctrl_v_toggles_preview() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('v'), KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::TogglePreview,
        );
    }

    #[test]
    fn search_ctrl_e_moves_end() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('e'), KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::MoveEnd,
        );
    }

    // =====================================================================
    // Search mode: word navigation
    // =====================================================================

    #[test]
    fn search_alt_left_moves_word_left() {
        assert_eq!(
            classify_key(key_mod(KeyCode::Left, KeyModifiers::ALT), &search_ctx()),
            KeyAction::MoveWordLeft,
        );
    }

    #[test]
    fn search_ctrl_left_moves_word_left() {
        assert_eq!(
            classify_key(key_mod(KeyCode::Left, KeyModifiers::CONTROL), &search_ctx()),
            KeyAction::MoveWordLeft,
        );
    }

    #[test]
    fn search_alt_b_moves_word_left() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('b'), KeyModifiers::ALT),
                &search_ctx()
            ),
            KeyAction::MoveWordLeft,
        );
    }

    #[test]
    fn search_alt_right_moves_word_right() {
        assert_eq!(
            classify_key(key_mod(KeyCode::Right, KeyModifiers::ALT), &search_ctx()),
            KeyAction::MoveWordRight,
        );
    }

    #[test]
    fn search_ctrl_right_moves_word_right() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Right, KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::MoveWordRight,
        );
    }

    #[test]
    fn search_alt_f_moves_word_right() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('f'), KeyModifiers::ALT),
                &search_ctx()
            ),
            KeyAction::MoveWordRight,
        );
    }

    // =====================================================================
    // Search mode: word deletion
    // =====================================================================

    #[test]
    fn search_alt_backspace_deletes_word_left() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Backspace, KeyModifiers::ALT),
                &search_ctx()
            ),
            KeyAction::DeleteWordLeft,
        );
    }

    #[test]
    fn search_alt_d_deletes_word_right() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('d'), KeyModifiers::ALT),
                &search_ctx()
            ),
            KeyAction::DeleteWordRight,
        );
    }

    #[test]
    fn search_ctrl_w_deletes_word_left() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('w'), KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::DeleteWordLeft,
        );
    }

    // =====================================================================
    // Search mode: Esc behavior
    // =====================================================================

    #[test]
    fn search_esc_in_preview_exits_preview() {
        let ctx = KeyContext {
            preview_mode: true,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key(KeyCode::Esc), &ctx),
            KeyAction::ExitPreview
        );
    }

    #[test]
    fn search_esc_without_preview_quits() {
        let ctx = KeyContext {
            preview_mode: false,
            ..search_ctx()
        };
        assert_eq!(classify_key(key(KeyCode::Esc), &ctx), KeyAction::Quit);
    }

    // =====================================================================
    // Search mode: plain navigation and editing
    // =====================================================================

    #[test]
    fn search_home_end() {
        assert_eq!(
            classify_key(key(KeyCode::Home), &search_ctx()),
            KeyAction::MoveHome
        );
        assert_eq!(
            classify_key(key(KeyCode::End), &search_ctx()),
            KeyAction::MoveEnd
        );
    }

    #[test]
    fn search_arrow_keys() {
        assert_eq!(classify_key(key(KeyCode::Up), &search_ctx()), KeyAction::Up);
        assert_eq!(
            classify_key(key(KeyCode::Down), &search_ctx()),
            KeyAction::Down
        );
        assert_eq!(
            classify_key(key(KeyCode::Left), &search_ctx()),
            KeyAction::Left
        );
        assert_eq!(
            classify_key(key(KeyCode::Right), &search_ctx()),
            KeyAction::Right
        );
    }

    #[test]
    fn search_tab_and_enter() {
        assert_eq!(
            classify_key(key(KeyCode::Tab), &search_ctx()),
            KeyAction::Tab
        );
        assert_eq!(
            classify_key(key(KeyCode::Enter), &search_ctx()),
            KeyAction::Enter
        );
    }

    #[test]
    fn search_backspace_and_delete() {
        assert_eq!(
            classify_key(key(KeyCode::Backspace), &search_ctx()),
            KeyAction::Backspace
        );
        assert_eq!(
            classify_key(key(KeyCode::Delete), &search_ctx()),
            KeyAction::Delete
        );
    }

    #[test]
    fn search_char_input() {
        assert_eq!(
            classify_key(key(KeyCode::Char('a')), &search_ctx()),
            KeyAction::InputChar('a')
        );
        assert_eq!(
            classify_key(key(KeyCode::Char('Z')), &search_ctx()),
            KeyAction::InputChar('Z')
        );
        assert_eq!(
            classify_key(key(KeyCode::Char('1')), &search_ctx()),
            KeyAction::InputChar('1')
        );
        assert_eq!(
            classify_key(key(KeyCode::Char('\u{0444}')), &search_ctx()),
            KeyAction::InputChar('\u{0444}'),
        );
    }

    #[test]
    fn search_unknown_key_is_noop() {
        assert_eq!(
            classify_key(key(KeyCode::F(5)), &search_ctx()),
            KeyAction::Noop
        );
        assert_eq!(
            classify_key(key(KeyCode::Insert), &search_ctx()),
            KeyAction::Noop
        );
        assert_eq!(
            classify_key(key(KeyCode::PageUp), &search_ctx()),
            KeyAction::Noop
        );
    }

    // =====================================================================
    // Edge cases: modifier combinations
    // =====================================================================

    #[test]
    fn ctrl_alt_left_moves_word_left() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::MoveWordLeft,
        );
    }

    #[test]
    fn ctrl_alt_right_moves_word_right() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::MoveWordRight,
        );
    }

    // =====================================================================
    // AI mode
    // =====================================================================

    #[test]
    fn search_ctrl_g_enters_ai_mode() {
        assert_eq!(
            classify_key(
                key_mod(KeyCode::Char('g'), KeyModifiers::CONTROL),
                &search_ctx()
            ),
            KeyAction::EnterAiMode,
        );
    }

    #[test]
    fn search_ctrl_g_in_ai_mode_exits() {
        let ctx = KeyContext {
            ai_mode: true,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('g'), KeyModifiers::CONTROL), &ctx),
            KeyAction::ExitAiMode,
        );
    }

    #[test]
    fn search_esc_in_ai_mode_exits_ai() {
        let ctx = KeyContext {
            ai_mode: true,
            ..search_ctx()
        };
        assert_eq!(classify_key(key(KeyCode::Esc), &ctx), KeyAction::ExitAiMode,);
    }

    #[test]
    fn search_ctrl_c_in_ai_mode_empty_query_exits_ai() {
        let ctx = KeyContext {
            ai_mode: true,
            ai_input_empty: true,
            input_empty: true,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL), &ctx),
            KeyAction::ExitAiMode,
        );
    }

    #[test]
    fn search_ctrl_c_in_ai_mode_nonempty_query_clears() {
        let ctx = KeyContext {
            ai_mode: true,
            ai_input_empty: false,
            input_empty: true,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL), &ctx),
            KeyAction::ClearInput,
        );
    }

    #[test]
    fn search_ctrl_c_in_ai_mode_ignores_main_input_empty() {
        // Even when the main search input is non-empty, Ctrl+C must act on the
        // AI query buffer while AI mode is active.
        let ctx = KeyContext {
            ai_mode: true,
            ai_input_empty: true,
            input_empty: false,
            ..search_ctx()
        };
        assert_eq!(
            classify_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL), &ctx),
            KeyAction::ExitAiMode,
        );
    }
}
