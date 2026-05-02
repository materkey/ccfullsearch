use ccs::search::{Message, RipgrepMatch, SessionGroup};
use ccs::session::SessionSource;
use ccs::tree::SessionTree;
use ccs::tui::{render, App};
use chrono::{TimeZone, Utc};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::fs;
use std::io::Write as _;
use tempfile::TempDir;

/// Extract visible text from a terminal buffer row.
fn buffer_line(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width;
    (0..width)
        .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Extract all visible lines from the terminal buffer.
fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let height = buffer.area.height;
    (0..height).map(|y| buffer_line(terminal, y)).collect()
}

#[test]
fn snapshot_empty_search_mode() {
    let backend = TestBackend::new(120, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    let app = App::new(vec!["/test".to_string()]);
    terminal.draw(|frame| render(frame, &app.view())).unwrap();

    let lines = buffer_lines(&terminal);

    // Header
    assert!(lines[0].contains("Claude Code Session Search"));
    // Input box with "Search" title
    assert!(
        lines.iter().any(|l| l.contains("Search")),
        "Should show Search input box"
    );
    // Help bar at bottom
    let last = lines.last().unwrap();
    assert!(
        last.contains("Navigate"),
        "Help bar should mention Navigate"
    );
    assert!(last.contains("Esc"), "Help bar should mention Esc");
}

#[test]
fn snapshot_search_with_results() {
    let backend = TestBackend::new(100, 15);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = App::new(vec!["/test".to_string()]);

    let msg = Message {
        session_id: "abc12345-def6-7890".to_string(),
        role: "user".to_string(),
        content: "How do I sort a list in Python?".to_string(),
        text_content: "How do I sort a list in Python?".to_string(),
        timestamp: Utc.with_ymd_and_hms(2025, 6, 15, 14, 30, 0).unwrap(),
        branch: Some("main".to_string()),
        line_number: 1,
        ..Default::default()
    };

    let m = RipgrepMatch {
        file_path: "/home/user/.claude/projects/-home-user-projects-myapp/session.jsonl"
            .to_string(),
        message: Some(msg),
        source: SessionSource::ClaudeCodeCLI,
    };

    app.search.groups = vec![SessionGroup {
        session_id: "abc12345-def6-7890".to_string(),
        file_path: m.file_path.clone(),
        matches: vec![m.clone()],
        automation: None,
        message_count: None,
        message_count_compacted: false,
    }];
    app.search.results_count = 1;
    app.search.results_query = "sort".to_string();

    terminal.draw(|frame| render(frame, &app.view())).unwrap();

    let lines = buffer_lines(&terminal);

    // Status line should show match count
    assert!(
        lines.iter().any(|l| l.contains("1 matches in 1 sessions")),
        "Should show match count. Lines: {:?}",
        lines
    );
    // Group header should contain the right-side agent/source badge, date, project
    assert!(
        lines
            .iter()
            .any(|l| l.contains("CC · cli") && l.contains("2025-06-15")),
        "Group header should contain source badge and date. Lines: {:?}",
        lines
    );
    // Group header should contain project name extracted from path
    assert!(
        lines.iter().any(|l| l.contains("myapp")),
        "Group header should contain project name. Lines: {:?}",
        lines
    );
}

#[test]
fn snapshot_search_status_indicators() {
    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    // Test "Typing..." status
    let mut app = App::new(vec!["/test".to_string()]);
    app.typing = true;
    terminal.draw(|frame| render(frame, &app.view())).unwrap();

    let lines = buffer_lines(&terminal);
    assert!(
        lines.iter().any(|l| l.contains("Typing...")),
        "Should show Typing indicator"
    );

    // Test "Searching..." status
    app.typing = false;
    app.set_searching_for_test(true);
    terminal.draw(|frame| render(frame, &app.view())).unwrap();
    let lines = buffer_lines(&terminal);
    assert!(
        lines.iter().any(|l| l.contains("Searching...")),
        "Should show Searching indicator"
    );

    // Test error status
    app.set_searching_for_test(false);
    app.search.error = Some("rg not found".to_string());
    terminal.draw(|frame| render(frame, &app.view())).unwrap();
    let lines = buffer_lines(&terminal);
    assert!(
        lines.iter().any(|l| l.contains("Error: rg not found")),
        "Should show error message"
    );
}

#[test]
fn snapshot_regex_and_project_filter_labels() {
    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = App::new(vec!["/test".to_string()]);

    // Regex mode
    app.regex_mode = true;
    terminal.draw(|frame| render(frame, &app.view())).unwrap();
    let lines = buffer_lines(&terminal);
    assert!(
        lines.iter().any(|l| l.contains("[Regex]")),
        "Should show [Regex] label in input title"
    );

    // Project filter mode
    app.regex_mode = false;
    app.project_filter = true;
    terminal.draw(|frame| render(frame, &app.view())).unwrap();
    let lines = buffer_lines(&terminal);
    assert!(
        lines.iter().any(|l| l.contains("[Project]")),
        "Should show [Project] label in input title"
    );

    // Both modes
    app.regex_mode = true;
    terminal.draw(|frame| render(frame, &app.view())).unwrap();
    let lines = buffer_lines(&terminal);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[Regex]") && l.contains("[Project]")),
        "Should show both [Regex] and [Project] labels"
    );
}

#[test]
fn snapshot_tree_mode_header() {
    let backend = TestBackend::new(100, 15);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = App::new(vec!["/test".to_string()]);

    // Create a simple session file
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("session.jsonl");
    {
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi there"}}]}},"uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
    }

    let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();
    app.tree_mode = true;
    app.tree.session_tree = Some(tree);

    terminal.draw(|frame| render(frame, &app.view())).unwrap();

    let lines = buffer_lines(&terminal);

    // Header should show Branch Tree with stats
    assert!(
        lines[0].contains("Branch Tree"),
        "Should show Branch Tree header"
    );
    assert!(
        lines[0].contains("2 messages"),
        "Should show message count. Line: {}",
        lines[0]
    );

    // Help bar should show tree-specific help
    let last = lines.last().unwrap();
    assert!(
        last.contains("Navigate") && last.contains("Resume"),
        "Help bar should show tree navigation hints"
    );
}

#[test]
fn snapshot_tree_mode_loading() {
    let backend = TestBackend::new(80, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = App::new(vec!["/test".to_string()]);
    app.tree_mode = true;
    app.tree.tree_loading = true;

    terminal.draw(|frame| render(frame, &app.view())).unwrap();

    let lines = buffer_lines(&terminal);

    assert!(
        lines[0].contains("Loading"),
        "Header should show loading state"
    );
    assert!(
        lines.iter().any(|l| l.contains("Loading session tree")),
        "Content should show loading message"
    );
}
