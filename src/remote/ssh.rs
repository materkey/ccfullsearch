//! The one place ssh command lines are built, for `serve` and for resume.

use super::config::HostConfig;
use std::process::Command;

/// Options both modes pass. With a live ControlMaster they are ignored for
/// the multiplexed session, which is why the client keeps its own deadlines.
const COMMON_OPTIONS: [&str; 8] = [
    "-o",
    "ConnectTimeout=3",
    "-o",
    "ServerAliveInterval=5",
    "-o",
    "ServerAliveCountMax=2",
    "-o",
    "Compression=yes",
];

pub enum SshMode<'a> {
    /// `ccs serve --stdio` over pipes; ssh must never prompt.
    Serve,
    /// `ccs resume` on a terminal. The session id travels too: an Opencode
    /// `<db>#<id>` path names no file the remote ccs could read it from.
    Resume {
        session_id: Option<&'a str>,
        file_path: &'a str,
        uuid: Option<&'a str>,
    },
}

/// ssh arguments for `mode` on `host`. ssh joins everything after the host
/// into one remote shell command, so values are quoted; `ccs_path` is the
/// user's own config and stays unquoted so `~` and `$HOME` work there.
pub fn ssh_args(host: &HostConfig, mode: &SshMode) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    match mode {
        SshMode::Serve => args.extend(["-o", "BatchMode=yes"].map(String::from)),
        SshMode::Resume { .. } => args.push("-t".to_string()),
    }
    args.extend(COMMON_OPTIONS.map(String::from));
    args.push(host.name.clone());
    match mode {
        SshMode::Serve => {
            args.extend([host.ccs_path.clone(), "serve".into(), "--stdio".into()]);
        }
        SshMode::Resume {
            session_id,
            file_path,
            uuid,
        } => {
            let mut remote = format!("{} resume", host.ccs_path);
            if let Some(id) = session_id {
                remote.push_str(&format!(" --session-id {}", shell_quote(id)));
            }
            remote.push_str(&format!(" --file {}", shell_quote(file_path)));
            if let Some(uuid) = uuid {
                remote.push_str(&format!(" --uuid {}", shell_quote(uuid)));
            }
            args.push(remote);
        }
    }
    args
}

/// The ssh program: `CCS_SSH` when set (tests point it at a fake), else `ssh`.
pub fn ssh_program() -> String {
    std::env::var("CCS_SSH")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ssh".to_string())
}

pub fn ssh_command(host: &HostConfig, mode: &SshMode) -> Command {
    let mut command = Command::new(ssh_program());
    command.args(ssh_args(host, mode));
    command
}

/// POSIX single-quoting: the value reaches the remote program unchanged.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z() -> HostConfig {
        HostConfig {
            name: "Z270XX".into(),
            ccs_path: "ccs".into(),
        }
    }

    #[test]
    fn ssh_args_when_serve_then_batchmode_and_serve_stdio() {
        let expected: Vec<String> = [
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
            "-o",
            "Compression=yes",
            "Z270XX",
            "ccs",
            "serve",
            "--stdio",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(ssh_args(&z(), &SshMode::Serve), expected);
    }

    #[test]
    fn ssh_args_when_resume_then_tty_without_batchmode() {
        let args = ssh_args(
            &z(),
            &SshMode::Resume {
                session_id: Some("ses_1"),
                file_path: "/a b/c.jsonl",
                uuid: Some("u1"),
            },
        );
        assert_eq!(args[0], "-t");
        assert!(!args.iter().any(|a| a == "BatchMode=yes"));
        assert_eq!(args[args.len() - 2], "Z270XX");
        assert_eq!(
            args.last().unwrap(),
            "ccs resume --session-id 'ses_1' --file '/a b/c.jsonl' --uuid 'u1'"
        );
    }

    #[test]
    fn ssh_args_when_resume_without_uuid_then_no_uuid_flag() {
        let args = ssh_args(
            &z(),
            &SshMode::Resume {
                session_id: None,
                file_path: "/s.jsonl",
                uuid: None,
            },
        );
        assert_eq!(args.last().unwrap(), "ccs resume --file '/s.jsonl'");
    }

    #[test]
    fn shell_quote_when_value_has_quote_and_space_then_roundtrips_through_sh() {
        let value = "it's a $x";
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("printf %s {}", shell_quote(value))])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), value);
    }
}
