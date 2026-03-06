pub mod fork;
pub mod launcher;
pub mod path_codec;

pub use fork::build_chain_from_tip;
pub use path_codec::encode_path_for_claude;

use crate::session::SessionSource;

/// Resume a Claude session based on its source.
/// If `message_uuid` is provided and the message is not on the latest chain,
/// creates a forked JSONL file and resumes from that instead.
pub fn resume(
    session_id: &str,
    file_path: &str,
    source: SessionSource,
    message_uuid: Option<&str>,
) -> Result<(), String> {
    if let Some(uuid) = message_uuid {
        if source == SessionSource::ClaudeCodeCLI && !fork::is_on_latest_chain(file_path, uuid) {
            let (fork_session_id, fork_file_path) = fork::create_fork(file_path, uuid)?;
            return launcher::resume_cli(&fork_session_id, &fork_file_path);
        }
    }

    match source {
        SessionSource::ClaudeCodeCLI => launcher::resume_cli(session_id, file_path),
        SessionSource::ClaudeDesktop => launcher::resume_desktop(),
    }
}
