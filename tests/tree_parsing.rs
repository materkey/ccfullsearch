use ccs::tree::SessionTree;

#[test]
fn linear_session_parses_all_messages() {
    let tree = SessionTree::from_file("tests/fixtures/linear_session.jsonl").unwrap();
    assert_eq!(tree.rows.len(), 4, "Linear session should have 4 messages");
}

#[test]
fn linear_session_preserves_order() {
    let tree = SessionTree::from_file("tests/fixtures/linear_session.jsonl").unwrap();

    let roles: Vec<&str> = tree.rows.iter().map(|r| r.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
}

#[test]
fn linear_session_all_on_latest_chain() {
    let tree = SessionTree::from_file("tests/fixtures/linear_session.jsonl").unwrap();

    for row in &tree.rows {
        assert!(
            row.is_on_latest_chain,
            "All linear messages should be on latest chain, but '{}' is not",
            row.content_preview
        );
    }
}

#[test]
fn branched_session_detects_branches() {
    let tree = SessionTree::from_file("tests/fixtures/branched_session.jsonl").unwrap();
    assert!(
        tree.branch_count() >= 1,
        "Branched session should have at least one branch point"
    );
}

#[test]
fn branched_session_has_fork_messages() {
    let tree = SessionTree::from_file("tests/fixtures/branched_session.jsonl").unwrap();

    let has_json = tree.rows.iter().any(|r| r.content_preview.contains("JSON"));
    let has_yaml = tree.rows.iter().any(|r| r.content_preview.contains("YAML"));

    assert!(has_json, "Should contain JSON branch message");
    assert!(has_yaml, "Should contain YAML branch message");
}

#[test]
fn branched_session_marks_fork_not_on_latest() {
    let tree = SessionTree::from_file("tests/fixtures/branched_session.jsonl").unwrap();

    let non_latest: Vec<&str> = tree
        .rows
        .iter()
        .filter(|r| !r.is_on_latest_chain)
        .map(|r| r.content_preview.as_str())
        .collect();

    assert!(
        !non_latest.is_empty(),
        "Branched session should have some messages not on latest chain"
    );
}

#[test]
fn compaction_session_summary_without_uuid_not_in_tree() {
    // Real Claude sessions have summary records without UUID — they should not
    // appear as tree nodes (only summary records with UUID would be visible).
    let tree = SessionTree::from_file("tests/fixtures/compaction_session.jsonl").unwrap();

    let has_compaction = tree.rows.iter().any(|r| r.is_compaction);
    assert!(
        !has_compaction,
        "Summary without UUID should not appear in tree"
    );
}

#[test]
fn compaction_session_preserves_messages_after_compaction() {
    let tree = SessionTree::from_file("tests/fixtures/compaction_session.jsonl").unwrap();

    let has_continue = tree
        .rows
        .iter()
        .any(|r| r.content_preview.contains("Continue after compaction"));
    assert!(has_continue, "Should have messages after compaction event");
}

#[test]
fn desktop_audit_session_parses() {
    let tree = SessionTree::from_file("tests/fixtures/desktop_audit_session.jsonl").unwrap();
    assert!(
        !tree.rows.is_empty(),
        "Desktop audit session should parse successfully"
    );
}

#[test]
fn empty_file_returns_empty_tree() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("empty.jsonl");
    std::fs::write(&path, "").unwrap();

    let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();
    assert!(tree.rows.is_empty(), "Empty file should produce empty tree");
}
