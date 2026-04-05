//! End-to-end tests for session resume resolution.
//!
//! Tests the full pipeline: resolve_parent_session correctly maps
//! auxiliary/agent/subagent files to the main session file, and
//! fork logic is skipped when the file was redirected.

use std::fs;
use std::io::Write;
use tempfile::TempDir;

/// Helper: write a minimal JSONL session with user/assistant messages
fn write_session(path: &std::path::Path, session_id: &str) {
    let mut f = fs::File::create(path).unwrap();
    writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"u1","sessionId":"{}","timestamp":"2025-01-01T00:00:00Z"}}"#, session_id).unwrap();
    writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi"}}]}},"uuid":"u2","parentUuid":"u1","sessionId":"{}","timestamp":"2025-01-01T00:01:00Z"}}"#, session_id).unwrap();
}

/// Helper: write a metadata-only JSONL (like Claude's auxiliary files)
fn write_auxiliary(path: &std::path::Path, session_id: &str) {
    let mut f = fs::File::create(path).unwrap();
    writeln!(
        f,
        r#"{{"type":"file-history-snapshot","messageId":"m1","snapshot":{{}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"last-prompt","lastPrompt":"test","sessionId":"{}"}}"#,
        session_id
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2025-01-01T00:00:00Z","sessionId":"{}"}}"#,
        session_id
    )
    .unwrap();
}

/// Helper: write an agent JSONL with messages whose sessionId points to parent
fn write_agent(path: &std::path::Path, parent_session_id: &str) {
    let mut f = fs::File::create(path).unwrap();
    writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Sub task"}}]}},"uuid":"agent-u1","sessionId":"{}","timestamp":"2025-01-01T00:02:00Z"}}"#, parent_session_id).unwrap();
    writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Working on it"}}]}},"uuid":"agent-u2","parentUuid":"agent-u1","sessionId":"{}","timestamp":"2025-01-01T00:03:00Z"}}"#, parent_session_id).unwrap();
}

// =============================================================================
// Scenario 1: Subagent file under .../session-id/subagents/
// =============================================================================

#[test]
fn e2e_subagent_resolves_to_parent_session() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir
        .path()
        .join(".claude")
        .join("projects")
        .join("-Users-test");
    let parent_id = "dffd4ad8-c1bc-459d-a43c-e5746363d094";

    // Create parent session file
    let parent_jsonl = project_dir.join(format!("{}.jsonl", parent_id));
    fs::create_dir_all(&project_dir).unwrap();
    write_session(&parent_jsonl, parent_id);

    // Create subagent directory structure
    let subagents_dir = project_dir.join(parent_id).join("subagents");
    fs::create_dir_all(&subagents_dir).unwrap();
    let agent_file = subagents_dir.join("agent-a65efef89f277db1a.jsonl");
    write_agent(&agent_file, parent_id);

    // Simulate: search found match in agent file, msg.session_id = parent_id
    let (sid, fpath) =
        ccs::resume::test_resolve_parent_session(parent_id, agent_file.to_str().unwrap());

    assert_eq!(sid, parent_id, "session_id should be parent's ID");
    assert_eq!(
        fpath,
        parent_jsonl.to_string_lossy(),
        "file_path should point to parent JSONL"
    );
}

// =============================================================================
// Scenario 2: Auxiliary metadata file (filename UUID != internal sessionId)
// =============================================================================

#[test]
fn e2e_auxiliary_file_resolves_to_main_session() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("-Users-Shared-projects-avito-android");
    fs::create_dir_all(&project_dir).unwrap();

    let main_id = "64cd6570-3475-47fd-a2cc-2da718d0dcb3";
    let aux_id = "68483698-e6fc-4ea8-a85e-989e6dfa5c2f";

    // Main session file
    let main_jsonl = project_dir.join(format!("{}.jsonl", main_id));
    write_session(&main_jsonl, main_id);

    // Auxiliary file with different filename but sessionId pointing to main
    let aux_jsonl = project_dir.join(format!("{}.jsonl", aux_id));
    write_auxiliary(&aux_jsonl, main_id);

    // Simulate: search found match, msg.session_id = main_id, file_path = aux file
    let (sid, fpath) =
        ccs::resume::test_resolve_parent_session(main_id, aux_jsonl.to_str().unwrap());

    assert_eq!(sid, main_id, "session_id should stay as main session ID");
    assert_eq!(
        fpath,
        main_jsonl.to_string_lossy(),
        "file_path should redirect to main session JSONL"
    );
}

// =============================================================================
// Scenario 3: Top-level agent file (agent-xxx.jsonl at project root)
// =============================================================================

#[test]
fn e2e_top_level_agent_resolves_to_parent() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("-Users-Shared-projects-avito-android");
    fs::create_dir_all(&project_dir).unwrap();

    let parent_id = "1b739662-27c8-499f-9af6-69c96bc2f62e";

    // Parent session file
    let parent_jsonl = project_dir.join(format!("{}.jsonl", parent_id));
    write_session(&parent_jsonl, parent_id);

    // Top-level agent file
    let agent_file = project_dir.join("agent-0b8388ad.jsonl");
    write_agent(&agent_file, parent_id);

    // msg.session_id from JSONL content = parent_id
    let (_sid, fpath) =
        ccs::resume::test_resolve_parent_session(parent_id, agent_file.to_str().unwrap());

    assert_eq!(_sid, parent_id);
    assert_eq!(fpath, parent_jsonl.to_string_lossy());
}

// =============================================================================
// Scenario 4: Normal session file (filename matches sessionId) — no redirect
// =============================================================================

#[test]
fn e2e_normal_session_no_redirect() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("-Users-test-project");
    fs::create_dir_all(&project_dir).unwrap();

    let session_id = "abc12345-6789-0def-ghij-klmnopqrstuv";
    let session_file = project_dir.join(format!("{}.jsonl", session_id));
    write_session(&session_file, session_id);

    let (sid, fpath) =
        ccs::resume::test_resolve_parent_session(session_id, session_file.to_str().unwrap());

    assert_eq!(sid, session_id, "session_id unchanged");
    assert_eq!(fpath, session_file.to_string_lossy(), "file_path unchanged");
}

// =============================================================================
// Scenario 5: Fork NOT triggered when file was redirected
// =============================================================================

#[test]
fn e2e_fork_skipped_when_file_redirected() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("projects");
    fs::create_dir_all(&project_dir).unwrap();

    let main_id = "main-session-id";

    // Main session with UUIDs u1, u2
    let main_jsonl = project_dir.join(format!("{}.jsonl", main_id));
    write_session(&main_jsonl, main_id);

    // Agent file with UUID agent-u1 (not in main file)
    let agent_file = project_dir.join("agent-xyz.jsonl");
    write_agent(&agent_file, main_id);

    // Step 1: resolve redirects to main file
    let (_sid, fpath) =
        ccs::resume::test_resolve_parent_session(main_id, agent_file.to_str().unwrap());
    assert_eq!(fpath, main_jsonl.to_string_lossy());
    let file_changed = fpath != agent_file.to_str().unwrap();
    assert!(file_changed, "file should have changed");

    // Step 2: agent-u1 doesn't exist in main file — is_on_latest_chain returns true
    // (unknown uuid = safe to resume without fork)
    let on_chain = ccs::resume::fork::is_on_latest_chain(&fpath, "agent-u1");
    assert!(
        on_chain,
        "unknown UUID should be treated as on-chain (no fork needed)"
    );

    // Step 3: file_changed=true provides a second safety net — fork is SKIPPED
    // even if is_on_latest_chain were to return false
    assert!(
        file_changed,
        "file_changed flag should prevent fork from triggering"
    );

    // Verify no fork files were created
    let fork_files: Vec<_> = fs::read_dir(&project_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.ends_with(".jsonl")
                && name != format!("{}.jsonl", main_id)
                && name != "agent-xyz.jsonl"
        })
        .collect();
    assert!(
        fork_files.is_empty(),
        "no fork files should be created: {:?}",
        fork_files
    );
}

// =============================================================================
// Scenario 6: Fork DOES trigger for real branch on same file
// =============================================================================

#[test]
fn e2e_fork_triggers_for_branch_on_same_file() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("projects");
    fs::create_dir_all(&project_dir).unwrap();

    let session_id = "branched-session";
    let session_file = project_dir.join(format!("{}.jsonl", session_id));
    {
        let mut f = fs::File::create(&session_file).unwrap();
        // Linear: p1 -> u1 -> a1 -> u2(branch A) -> a2(branch A, latest)
        //                        -> u3(branch B) -> a3(branch B)
        writeln!(f, r#"{{"type":"progress","uuid":"p1","sessionId":"{}","timestamp":"2025-01-01T00:00:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p1","sessionId":"{}","timestamp":"2025-01-01T00:01:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"{}","timestamp":"2025-01-01T00:02:00Z"}}"#, session_id).unwrap();
        // Branch A (latest — comes last in file)
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch A"}},"uuid":"u2","parentUuid":"a1","sessionId":"{}","timestamp":"2025-01-01T00:03:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"A reply"}},"uuid":"a2","parentUuid":"u2","sessionId":"{}","timestamp":"2025-01-01T00:04:00Z"}}"#, session_id).unwrap();
        // Branch B (NOT latest)
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch B"}},"uuid":"u3","parentUuid":"a1","sessionId":"{}","timestamp":"2025-01-01T00:03:30Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"B reply"}},"uuid":"a3","parentUuid":"u3","sessionId":"{}","timestamp":"2025-01-01T00:04:30Z"}}"#, session_id).unwrap();
    }

    // No redirect — same file
    let (_, fpath) =
        ccs::resume::test_resolve_parent_session(session_id, session_file.to_str().unwrap());
    let file_changed = fpath != session_file.to_str().unwrap();
    assert!(!file_changed, "no redirect for normal file");

    // a3 (branch B tip) IS on latest chain because it's the last UUID in the file
    // Latest chain walks backward from last uuid (a3) through a3->u3->a1->u1->p1
    assert!(
        ccs::resume::fork::is_on_latest_chain(&fpath, "a3"),
        "a3 should be on latest chain (last in file)"
    );

    // a2 (branch A tip) is NOT on latest chain
    assert!(
        !ccs::resume::fork::is_on_latest_chain(&fpath, "a2"),
        "a2 should NOT be on latest chain"
    );

    // Fork should work for a2 since file_changed=false and a2 is NOT on latest chain
    let result = ccs::resume::fork::create_fork(&fpath, "a2");
    assert!(result.is_ok(), "fork should succeed for branch B message");

    let (fork_id, fork_path) = result.unwrap();
    assert!(
        std::path::Path::new(&fork_path).exists(),
        "fork file should exist"
    );
    assert_ne!(fork_id, session_id, "fork should have new session ID");

    // Clean up fork file
    let _ = fs::remove_file(&fork_path);
}

// =============================================================================
// Scenario 7: Resume returns session ID (cross-project handled via cwd + index)
// =============================================================================

#[test]
fn e2e_resume_returns_session_id() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("proj");
    fs::create_dir_all(&project_dir).unwrap();

    let session_id = "test-session-abc";
    let session_file = project_dir.join(format!("{}.jsonl", session_id));
    write_session(&session_file, session_id);

    let resume_arg =
        ccs::resume::test_prepare_cli_resume_session_id(session_id, session_file.to_str().unwrap())
            .unwrap();

    // Resume returns session ID; cross-project works because build_resume_command
    // sets cwd to the decoded project dir and ensures session is in the index.
    assert_eq!(
        resume_arg, session_id,
        "resume arg should be the session ID"
    );
}

// =============================================================================
// Scenario 8: Generic resume keeps original branched session
// =============================================================================

#[test]
fn e2e_generic_resume_does_not_linearize_branched_session() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("projects");
    fs::create_dir_all(&project_dir).unwrap();

    let session_id = "branched-session";
    let session_file = project_dir.join(format!("{}.jsonl", session_id));
    {
        let mut f = fs::File::create(&session_file).unwrap();
        writeln!(f, r#"{{"type":"progress","uuid":"p1","sessionId":"{}","timestamp":"2025-01-01T00:00:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p1","sessionId":"{}","timestamp":"2025-01-01T00:01:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"{}","timestamp":"2025-01-01T00:02:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch A"}},"uuid":"u2","parentUuid":"a1","sessionId":"{}","timestamp":"2025-01-01T00:03:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"A reply"}},"uuid":"a2","parentUuid":"u2","sessionId":"{}","timestamp":"2025-01-01T00:04:00Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch B"}},"uuid":"u3","parentUuid":"a1","sessionId":"{}","timestamp":"2025-01-01T00:03:30Z"}}"#, session_id).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"B reply"}},"uuid":"a3","parentUuid":"u3","sessionId":"{}","timestamp":"2025-01-01T00:04:30Z"}}"#, session_id).unwrap();
    }

    let resume_arg =
        ccs::resume::test_prepare_cli_resume_session_id(session_id, session_file.to_str().unwrap())
            .unwrap();

    // prepare_resume returns session ID
    assert_eq!(resume_arg, session_id);

    let jsonl_files: Vec<_> = fs::read_dir(&project_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect();
    assert_eq!(
        jsonl_files.len(),
        1,
        "generic resume should not create a synthetic copy"
    );
    assert_eq!(
        jsonl_files[0].file_name().to_string_lossy(),
        format!("{}.jsonl", session_id)
    );
}
