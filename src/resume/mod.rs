pub mod fork;
pub mod launcher;
pub mod path_codec;

pub use fork::build_chain_from_tip;
pub use path_codec::encode_path_for_claude;

use crate::session::{resolve_parent_session, SessionProvider, SessionSource};

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

/// Whether to replace the current process (exec) or spawn a child and wait.
#[derive(Clone, Copy)]
enum ResumeMode {
    /// Normal mode: replace the current process via exec.
    Exec,
    /// Overlay mode: spawn a child and return when it exits.
    Child,
}

/// Core resume logic shared by `resume()` and `resume_child()`.
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
fn resume_inner(
    session_id: &str,
    file_path: &str,
    source: SessionSource,
    message_uuid: Option<&str>,
    mode: ResumeMode,
) -> Result<(), String> {
    let label = match mode {
        ResumeMode::Exec => "resume",
        ResumeMode::Child => "resume_child",
    };
    ccs_debug!(
        "[ccs:{}] input: session_id={}, file_path={}, source={:?}, uuid={:?}",
        label,
        session_id,
        file_path,
        source,
        message_uuid
    );

    let (session_id, resolved_file_path) = resolve_parent_session(session_id, file_path);
    let provider = SessionProvider::from_path(&resolved_file_path);
    let file_changed = resolved_file_path != file_path;
    ccs_debug!(
        "[ccs:{}] resolved: provider={:?}, session_id={}, file_path={}, file_changed={}",
        label,
        provider,
        session_id,
        resolved_file_path,
        file_changed
    );

    // When resolve_parent_session changes the file, the message UUID belongs to
    // the original (auxiliary/agent) file and won't exist in the parent session.
    if let Some(uuid) = message_uuid {
        if !file_changed
            && provider == SessionProvider::Claude
            && source == SessionSource::ClaudeCodeCLI
            && fork::should_fork_for_resume(&resolved_file_path, uuid)
        {
            let (fork_session_id, fork_file_path) = fork::create_fork(&resolved_file_path, uuid)?;
            ccs_debug!(
                "[ccs:{}] forking: fork_session_id={}, fork_file_path={}",
                label,
                fork_session_id,
                fork_file_path
            );
            return match mode {
                ResumeMode::Exec => launcher::resume_cli(&fork_session_id, &fork_file_path),
                ResumeMode::Child => launcher::resume_cli_child(&fork_session_id, &fork_file_path),
            };
        }
    }

    match (provider, source, mode) {
        (SessionProvider::Codex, _, ResumeMode::Exec) => {
            launcher::resume_codex(&session_id, &resolved_file_path)
        }
        (SessionProvider::Codex, _, ResumeMode::Child) => {
            launcher::resume_codex_child(&session_id, &resolved_file_path)
        }
        (SessionProvider::Claude, SessionSource::ClaudeCodeCLI, ResumeMode::Exec) => {
            launcher::resume_cli(&session_id, &resolved_file_path)
        }
        (SessionProvider::Claude, SessionSource::ClaudeCodeCLI, ResumeMode::Child) => {
            launcher::resume_cli_child(&session_id, &resolved_file_path)
        }
        (SessionProvider::Claude, SessionSource::ClaudeDesktop, ResumeMode::Exec) => {
            launcher::resume_desktop()
        }
        (SessionProvider::Claude, SessionSource::ClaudeDesktop, ResumeMode::Child) => {
            launcher::resume_desktop_child()
        }
    }
}

/// Resume a session based on its provider and source.
/// If `message_uuid` is provided and the message is not the current resumable
/// tip, creates a forked JSONL file and resumes from that instead.
/// For subagent sessions, automatically resumes the parent session.
pub fn resume(
    session_id: &str,
    file_path: &str,
    source: SessionSource,
    message_uuid: Option<&str>,
) -> Result<(), String> {
    resume_inner(
        session_id,
        file_path,
        source,
        message_uuid,
        ResumeMode::Exec,
    )
}

/// Resume a session as a child process.
/// Used in overlay mode where TUI needs to regain control after the agent exits.
pub fn resume_child(
    session_id: &str,
    file_path: &str,
    source: SessionSource,
    message_uuid: Option<&str>,
) -> Result<(), String> {
    resume_inner(
        session_id,
        file_path,
        source,
        message_uuid,
        ResumeMode::Child,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
