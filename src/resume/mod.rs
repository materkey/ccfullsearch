pub mod fork;
pub mod launcher;
pub mod path_codec;

pub use fork::build_chain_from_tip;
pub use path_codec::encode_path_for_claude;

use crate::session::SessionSource;
use std::path::Path;

/// Resolve the correct session ID and file path for `claude --resume`.
///
/// Claude CLI matches sessions by filename: it looks for `<session-id>.jsonl`.
/// But search results may come from auxiliary files (agent files, metadata files)
/// whose filename doesn't match the `sessionId` in their content.
///
/// This function handles three cases:
/// 1. **Subagent file** (`../session-id/subagents/agent-xxx.jsonl`):
///    resolves to the parent `session-id.jsonl`
/// 2. **Mismatched filename** (file's stem != session_id):
///    looks for `<session_id>.jsonl` in the same directory
/// 3. **Normal file**: returns as-is
pub(crate) fn resolve_parent_session(session_id: &str, file_path: &str) -> (String, String) {
    let path = Path::new(file_path);

    // Case 1: subagent file under .../session-id/subagents/
    if let Some(parent) = path.parent() {
        if parent.file_name().and_then(|n| n.to_str()) == Some("subagents") {
            if let Some(session_dir) = parent.parent() {
                let parent_jsonl = session_dir.with_extension("jsonl");
                if parent_jsonl.exists() {
                    let dir_name = session_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(session_id);
                    return (
                        dir_name.to_string(),
                        parent_jsonl.to_string_lossy().to_string(),
                    );
                }
            }
        }
    }

    // Case 2: filename stem doesn't match session_id — find the correct file
    let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if file_stem != session_id {
        if let Some(parent_dir) = path.parent() {
            let correct_file = parent_dir.join(format!("{}.jsonl", session_id));
            if correct_file.exists() {
                return (
                    session_id.to_string(),
                    correct_file.to_string_lossy().to_string(),
                );
            }
        }
    }

    // Case 3: normal file
    (session_id.to_string(), file_path.to_string())
}

#[doc(hidden)]
pub fn test_resolve_parent_session(session_id: &str, file_path: &str) -> (String, String) {
    resolve_parent_session(session_id, file_path)
}

#[doc(hidden)]
pub fn test_prepare_cli_resume_session_id(
    session_id: &str,
    file_path: &str,
) -> Result<String, String> {
    launcher::prepare_resume(session_id, file_path)
}

/// Resume a Claude session based on its source.
/// If `message_uuid` is provided and the message is not on the latest chain,
/// creates a forked JSONL file and resumes from that instead.
/// For subagent sessions, automatically resumes the parent session.
///
/// # Why we use fork.rs instead of Claude's `--fork-session`
///
/// Claude Code CLI has `--fork-session` (creates a new session ID when resuming)
/// and `--resume-session-at <uuid>` (truncates history to a specific message).
/// Together they could theoretically replace our fork logic, but they can't:
///
/// 1. `--resume-session-at` is a hidden/internal flag ("use with --resume in print
///    mode") — it linearly truncates the loaded message list by index, not by DAG
///    structure. It doesn't walk `parentUuid` chains to extract a specific branch.
///
/// 2. `--fork-session` only changes the session ID — it doesn't select which branch
///    to fork from. Without branch-aware extraction, it forks from the latest leaf
///    (which is whatever Claude Code's own DAG resolver picks), not from the
///    arbitrary branch tip the user selected in our tree view.
///
/// 3. Claude Code's own `/branch` command (commands/branch/branch.ts) does
///    DAG-aware forking similar to our fork.rs — it walks the chain from the
///    current message, rewrites parentUuids, and creates a new JSONL file.
///    But it's an internal command, not exposed as a CLI flag.
///
/// Our fork.rs implements the same DAG-aware extraction: walk from selected tip
/// to root via parentUuid, write only those records into a new JSONL with a
/// rewritten sessionId. This is the correct approach for resuming from an
/// arbitrary branch tip that is not the latest leaf.
pub fn resume(
    session_id: &str,
    file_path: &str,
    source: SessionSource,
    message_uuid: Option<&str>,
) -> Result<(), String> {
    if std::env::var("CCS_DEBUG").is_ok() {
        eprintln!(
            "[ccs:resume] input: session_id={}, file_path={}, source={:?}, uuid={:?}",
            session_id, file_path, source, message_uuid
        );
    }
    let (session_id, resolved_file_path) = resolve_parent_session(session_id, file_path);
    let file_changed = resolved_file_path != file_path;
    if std::env::var("CCS_DEBUG").is_ok() {
        eprintln!(
            "[ccs:resume] resolved: session_id={}, file_path={}, file_changed={}",
            session_id, resolved_file_path, file_changed
        );
    }

    // Only attempt fork if the file wasn't redirected.
    // When resolve_parent_session changes the file, the message UUID belongs to the
    // original (auxiliary/agent) file and won't exist in the parent session file.
    if let Some(uuid) = message_uuid {
        if !file_changed
            && source == SessionSource::ClaudeCodeCLI
            && !fork::is_on_latest_chain(&resolved_file_path, uuid)
        {
            let (fork_session_id, fork_file_path) = fork::create_fork(&resolved_file_path, uuid)?;
            if std::env::var("CCS_DEBUG").is_ok() {
                eprintln!(
                    "[ccs:resume] forking: fork_session_id={}, fork_file_path={}",
                    fork_session_id, fork_file_path
                );
            }
            return launcher::resume_cli(&fork_session_id, &fork_file_path);
        }
    }

    match source {
        SessionSource::ClaudeCodeCLI => launcher::resume_cli(&session_id, &resolved_file_path),
        SessionSource::ClaudeDesktop => launcher::resume_desktop(),
    }
}

/// Resume a Claude session as a child process (returns when claude exits).
/// Same resolution logic as `resume()`, but uses `resume_cli_child()` instead of exec.
/// Used in overlay mode where TUI needs to regain control after claude exits.
pub fn resume_child(
    session_id: &str,
    file_path: &str,
    source: SessionSource,
    message_uuid: Option<&str>,
) -> Result<(), String> {
    if std::env::var("CCS_DEBUG").is_ok() {
        eprintln!(
            "[ccs:resume_child] input: session_id={}, file_path={}, source={:?}, uuid={:?}",
            session_id, file_path, source, message_uuid
        );
    }
    let (session_id, resolved_file_path) = resolve_parent_session(session_id, file_path);
    let file_changed = resolved_file_path != file_path;
    if std::env::var("CCS_DEBUG").is_ok() {
        eprintln!(
            "[ccs:resume_child] resolved: session_id={}, file_path={}, file_changed={}",
            session_id, resolved_file_path, file_changed
        );
    }

    if let Some(uuid) = message_uuid {
        if !file_changed
            && source == SessionSource::ClaudeCodeCLI
            && !fork::is_on_latest_chain(&resolved_file_path, uuid)
        {
            let (fork_session_id, fork_file_path) = fork::create_fork(&resolved_file_path, uuid)?;
            if std::env::var("CCS_DEBUG").is_ok() {
                eprintln!(
                    "[ccs:resume_child] forking: fork_session_id={}, fork_file_path={}",
                    fork_session_id, fork_file_path
                );
            }
            return launcher::resume_cli_child(&fork_session_id, &fork_file_path);
        }
    }

    match source {
        SessionSource::ClaudeCodeCLI => {
            launcher::resume_cli_child(&session_id, &resolved_file_path)
        }
        SessionSource::ClaudeDesktop => launcher::resume_desktop_child(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_parent_for_subagent_uses_parent_session_id() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("aaa-bbb-ccc");
        let subagents_dir = session_dir.join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();

        // Create parent JSONL
        let parent_jsonl = dir.path().join("aaa-bbb-ccc.jsonl");
        fs::write(&parent_jsonl, "{}").unwrap();

        // Create subagent JSONL
        let agent_file = subagents_dir.join("agent-xyz.jsonl");
        fs::write(&agent_file, "{}").unwrap();

        let (sid, fpath) = resolve_parent_session("wrong-id", agent_file.to_str().unwrap());
        assert_eq!(sid, "aaa-bbb-ccc");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_top_level_agent_uses_filename_session_id() {
        // Top-level agent file: .../project-dir/agent-xxx.jsonl
        // sessionId inside points to parent, but filename is agent-xxx
        // resume should use parent session, not agent filename
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        // Simulate .claude/projects/<encoded-dir>/
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-Users-proj");
        fs::create_dir_all(&project_dir).unwrap();

        // Parent session file
        let parent_jsonl = project_dir.join("64cd6570-parent.jsonl");
        fs::write(&parent_jsonl, r#"{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        // Top-level agent file with sessionId pointing to parent
        let agent_file = project_dir.join("agent-abc123.jsonl");
        fs::write(&agent_file, r#"{"type":"user","message":{"role":"user","content":"sub"},"sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        // When resolving, the session_id from JSONL is "64cd6570-parent" (correct)
        // file_path is ".../agent-abc123.jsonl"
        // resolve_parent_session should recognize this is NOT the main session file
        // and find the parent's JSONL by matching session_id to filenames in same dir
        let (sid, fpath) = resolve_parent_session("64cd6570-parent", agent_file.to_str().unwrap());
        assert_eq!(sid, "64cd6570-parent");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_exact_user_scenario() {
        // Exact reproduction: session file 68483698.jsonl contains
        // sessionId: 64cd6570 (parent). resume must use 64cd6570.
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("-Users-Shared-projects-avito-android");
        fs::create_dir_all(&project_dir).unwrap();

        // Main session file
        let main_file = project_dir.join("64cd6570-3475-47fd-a2cc-2da718d0dcb3.jsonl");
        fs::write(&main_file, r#"{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"64cd6570-3475-47fd-a2cc-2da718d0dcb3","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        // Auxiliary metadata file (different UUID filename, same sessionId inside)
        let aux_file = project_dir.join("68483698-e6fc-4ea8-a85e-989e6dfa5c2f.jsonl");
        fs::write(&aux_file, r#"{"type":"last-prompt","lastPrompt":"test","sessionId":"64cd6570-3475-47fd-a2cc-2da718d0dcb3"}"#).unwrap();

        let (sid, fpath) = resolve_parent_session(
            "64cd6570-3475-47fd-a2cc-2da718d0dcb3",
            aux_file.to_str().unwrap(),
        );
        assert_eq!(sid, "64cd6570-3475-47fd-a2cc-2da718d0dcb3");
        assert_eq!(fpath, main_file.to_string_lossy());
    }

    #[test]
    fn test_resolve_skips_fork_when_file_changed() {
        // When resolve_parent_session changes the file_path,
        // the message_uuid from the original file won't exist in the new file.
        // Fork should NOT be triggered in this case — just resume the parent session.
        use std::fs;
        use std::io::Write;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();

        // Parent session file with its own UUIDs
        let parent_jsonl = dir.path().join("64cd6570-parent.jsonl");
        {
            let mut f = fs::File::create(&parent_jsonl).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"uuid":"parent-uuid-1","sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
            writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"hello"}},"uuid":"parent-uuid-2","parentUuid":"parent-uuid-1","sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        }

        // Agent file with different UUIDs but same sessionId
        let agent_file = dir.path().join("agent-abc.jsonl");
        {
            let mut f = fs::File::create(&agent_file).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"sub task"}},"uuid":"agent-uuid-1","sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        }

        // resolve_parent_session changes file from agent to parent
        let (sid, fpath) = resolve_parent_session("64cd6570-parent", agent_file.to_str().unwrap());
        assert_eq!(sid, "64cd6570-parent");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());

        // agent-uuid-1 is NOT in parent file — is_on_latest_chain returns true
        // (unknown uuid = don't fork), so fork is correctly skipped
        assert!(fork::is_on_latest_chain(&fpath, "agent-uuid-1"));
        // The file_changed flag in resume() provides a second safety net
    }

    #[test]
    fn test_resolve_parent_for_auxiliary_file_finds_main_session() {
        // Auxiliary file: .../project-dir/1630cd72-xxx.jsonl
        // sessionId inside is "64cd6570-yyy" (parent session)
        // resume should use session_id="64cd6570-yyy" and file_path of parent
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-Users-proj");
        fs::create_dir_all(&project_dir).unwrap();

        // Parent session file
        let parent_jsonl = project_dir.join("64cd6570-parent.jsonl");
        fs::write(&parent_jsonl, "{}").unwrap();

        // Auxiliary file with different filename but sessionId pointing to parent
        let aux_file = project_dir.join("1630cd72-auxiliary.jsonl");
        fs::write(&aux_file, "{}").unwrap();

        let (sid, fpath) = resolve_parent_session("64cd6570-parent", aux_file.to_str().unwrap());
        assert_eq!(sid, "64cd6570-parent");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }
}
