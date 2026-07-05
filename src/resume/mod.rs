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

/// Debug-log label for a resume mode.
fn resume_label(mode: ResumeMode) -> &'static str {
    match mode {
        ResumeMode::Exec => "resume",
        ResumeMode::Child => "resume_child",
    }
}

/// Launcher functions for one `ResumeMode`. Selecting the table by mode and
/// the entry by provider/source keeps the dispatch decision pure and testable
/// while the actual exec/spawn stays in `launcher`.
struct LauncherTable {
    codex: fn(&str, &str) -> Result<(), String>,
    opencode: fn(&str, &str) -> Result<(), String>,
    claude_cli: fn(&str, &str) -> Result<(), String>,
    claude_desktop: fn() -> Result<(), String>,
}

static EXEC_LAUNCHERS: LauncherTable = LauncherTable {
    codex: launcher::resume_codex,
    opencode: launcher::resume_opencode,
    claude_cli: launcher::resume_cli,
    claude_desktop: launcher::resume_desktop,
};

static CHILD_LAUNCHERS: LauncherTable = LauncherTable {
    codex: launcher::resume_codex_child,
    opencode: launcher::resume_opencode_child,
    claude_cli: launcher::resume_cli_child,
    claude_desktop: launcher::resume_desktop_child,
};

fn launcher_table(mode: ResumeMode) -> &'static LauncherTable {
    match mode {
        ResumeMode::Exec => &EXEC_LAUNCHERS,
        ResumeMode::Child => &CHILD_LAUNCHERS,
    }
}

/// Route a resume to the launcher matching the session's provider and source.
fn dispatch_launch(
    table: &LauncherTable,
    provider: SessionProvider,
    source: SessionSource,
    session_id: &str,
    file_path: &str,
) -> Result<(), String> {
    match (provider, source) {
        (SessionProvider::Codex, _) => (table.codex)(session_id, file_path),
        (SessionProvider::Opencode, _) => (table.opencode)(session_id, file_path),
        (SessionProvider::Claude, SessionSource::CLI) => (table.claude_cli)(session_id, file_path),
        (SessionProvider::Claude, SessionSource::ClaudeDesktop) => (table.claude_desktop)(),
    }
}

/// Pure decision: forking is only considered for Claude Code CLI sessions when
/// `resolve_parent_session` kept the original file. When the file changed, the
/// message UUID belongs to the original (auxiliary/agent) file and won't exist
/// in the parent session.
fn fork_applies(file_changed: bool, provider: SessionProvider, source: SessionSource) -> bool {
    !file_changed && provider == SessionProvider::Claude && source == SessionSource::CLI
}

/// Returns the UUID to fork from when resuming `message_uuid` requires a
/// branch-aware fork; `None` means resume the session tip directly.
fn fork_uuid<'a>(
    message_uuid: Option<&'a str>,
    file_changed: bool,
    provider: SessionProvider,
    source: SessionSource,
    resolved_file_path: &str,
) -> Option<&'a str> {
    let uuid = message_uuid?;
    if fork_applies(file_changed, provider, source)
        && fork::should_fork_for_resume(resolved_file_path, uuid)
    {
        Some(uuid)
    } else {
        None
    }
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
    let label = resume_label(mode);
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

    let table = launcher_table(mode);

    if let Some(uuid) = fork_uuid(
        message_uuid,
        file_changed,
        provider,
        source,
        &resolved_file_path,
    ) {
        let (fork_session_id, fork_file_path) = fork::create_fork(&resolved_file_path, uuid)?;
        ccs_debug!(
            "[ccs:{}] forking: fork_session_id={}, fork_file_path={}",
            label,
            fork_session_id,
            fork_file_path
        );
        return (table.claude_cli)(&fork_session_id, &fork_file_path);
    }

    dispatch_launch(table, provider, source, &session_id, &resolved_file_path)
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

    fn stub_codex(_: &str, _: &str) -> Result<(), String> {
        Err("stub-codex".to_string())
    }
    fn stub_opencode(_: &str, _: &str) -> Result<(), String> {
        Err("stub-opencode".to_string())
    }
    fn stub_cli(_: &str, _: &str) -> Result<(), String> {
        Err("stub-cli".to_string())
    }
    fn stub_desktop() -> Result<(), String> {
        Err("stub-desktop".to_string())
    }

    #[test]
    fn test_resume_label() {
        assert_eq!(resume_label(ResumeMode::Exec), "resume");
        assert_eq!(resume_label(ResumeMode::Child), "resume_child");
    }

    #[test]
    fn test_launcher_table_selects_table_by_mode() {
        assert!(std::ptr::eq(
            launcher_table(ResumeMode::Exec),
            &EXEC_LAUNCHERS
        ));
        assert!(std::ptr::eq(
            launcher_table(ResumeMode::Child),
            &CHILD_LAUNCHERS
        ));
    }

    #[test]
    fn test_dispatch_launch_routes_by_provider_and_source() {
        let table = LauncherTable {
            codex: stub_codex,
            opencode: stub_opencode,
            claude_cli: stub_cli,
            claude_desktop: stub_desktop,
        };
        let cases = [
            (SessionProvider::Codex, SessionSource::CLI, "stub-codex"),
            (
                SessionProvider::Codex,
                SessionSource::ClaudeDesktop,
                "stub-codex",
            ),
            (
                SessionProvider::Opencode,
                SessionSource::CLI,
                "stub-opencode",
            ),
            (
                SessionProvider::Opencode,
                SessionSource::ClaudeDesktop,
                "stub-opencode",
            ),
            (SessionProvider::Claude, SessionSource::CLI, "stub-cli"),
            (
                SessionProvider::Claude,
                SessionSource::ClaudeDesktop,
                "stub-desktop",
            ),
        ];
        for (provider, source, expected) in cases {
            let got = dispatch_launch(&table, provider, source, "sid", "path").unwrap_err();
            assert_eq!(got, expected, "provider={:?} source={:?}", provider, source);
        }
    }

    #[test]
    fn test_fork_applies_only_for_claude_cli_with_unchanged_file() {
        assert!(fork_applies(
            false,
            SessionProvider::Claude,
            SessionSource::CLI
        ));
        assert!(!fork_applies(
            true,
            SessionProvider::Claude,
            SessionSource::CLI
        ));
        assert!(!fork_applies(
            false,
            SessionProvider::Codex,
            SessionSource::CLI
        ));
        assert!(!fork_applies(
            false,
            SessionProvider::Opencode,
            SessionSource::CLI
        ));
        assert!(!fork_applies(
            false,
            SessionProvider::Claude,
            SessionSource::ClaudeDesktop
        ));
    }

    #[test]
    fn test_fork_uuid_decision() {
        use std::fs;
        use std::io::Write;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        {
            let mut f = fs::File::create(&jsonl).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"uuid":"uuid-1","sessionId":"s","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
            writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"hello"}},"uuid":"uuid-2","parentUuid":"uuid-1","sessionId":"s","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        }
        let path = jsonl.to_str().unwrap();
        let claude = SessionProvider::Claude;
        let cli = SessionSource::CLI;

        // No uuid selected → no fork.
        assert_eq!(fork_uuid(None, false, claude, cli, path), None);
        // File changed → uuid belongs to the original file, never fork.
        assert_eq!(fork_uuid(Some("uuid-1"), true, claude, cli, path), None);
        // Selected uuid is the current resumable tip → resume directly.
        assert_eq!(fork_uuid(Some("uuid-2"), false, claude, cli, path), None);
        // Ancestor uuid off the resumable tip → fork from it.
        assert_eq!(
            fork_uuid(Some("uuid-1"), false, claude, cli, path),
            Some("uuid-1")
        );
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
}
