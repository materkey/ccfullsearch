//! Remote hosts: where a session lives, and how ccs reaches other machines.

pub mod client;
pub mod config;
pub mod oneshot;
pub mod serve;
pub mod server;
pub mod ssh;
pub mod wire;

/// Which machines a CLI command asks, from its repeatable `--host` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSelection {
    pub local: bool,
    pub remote: Vec<config::HostConfig>,
}

/// `all` (local and every host), `local`, `remote` (every host) or a host
/// name; repeatable. An unknown name is an error rather than silently
/// searching less.
pub fn select_hosts(
    values: &[String],
    hosts: &[config::HostConfig],
) -> Result<HostSelection, String> {
    let mut local = false;
    let mut names: Vec<&str> = Vec::new();
    for value in values {
        match value.as_str() {
            "all" => {
                local = true;
                names.extend(hosts.iter().map(|h| h.name.as_str()));
            }
            "local" => local = true,
            "remote" => names.extend(hosts.iter().map(|h| h.name.as_str())),
            name if hosts.iter().any(|h| h.name == name) => names.push(name),
            name => {
                return Err(format!(
                    "unknown host {name} (not in [[hosts]] of config.toml or CCS_HOSTS)"
                ))
            }
        }
    }
    let remote = hosts
        .iter()
        .filter(|h| names.contains(&h.name.as_str()))
        .cloned()
        .collect();
    Ok(HostSelection { local, remote })
}

/// The machine a session file lives on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Host {
    #[default]
    Local,
    /// A host from `[[hosts]]` in `config.toml`, named by its ssh alias.
    Remote(String),
}

impl Host {
    pub fn is_local(&self) -> bool {
        matches!(self, Host::Local)
    }

    /// `"local"` or the host name, as shown in JSON output and pick output.
    pub fn label(&self) -> &str {
        match self {
            Host::Local => "local",
            Host::Remote(name) => name,
        }
    }
}

/// A session file on a given host. Code that reads session files goes
/// through [`SessionRef::local_path`], which is `None` for remote files, so
/// a path from another machine is never opened on this one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionRef {
    pub host: Host,
    pub file_path: String,
}

impl SessionRef {
    pub fn new(host: Host, file_path: impl Into<String>) -> Self {
        Self {
            host,
            file_path: file_path.into(),
        }
    }

    pub fn local(file_path: impl Into<String>) -> Self {
        Self::new(Host::Local, file_path)
    }

    pub fn local_path(&self) -> Option<&str> {
        self.host.is_local().then_some(self.file_path.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ref_when_remote_then_no_local_path() {
        let r = SessionRef::new(Host::Remote("Z".into()), "/x");
        assert_eq!(r.local_path(), None);
    }

    #[test]
    fn session_ref_when_local_then_local_path() {
        assert_eq!(SessionRef::local("/x").local_path(), Some("/x"));
    }

    fn hosts() -> Vec<config::HostConfig> {
        ["A", "B"]
            .map(|n| config::HostConfig {
                name: n.into(),
                ccs_path: "ccs".into(),
            })
            .to_vec()
    }

    #[test]
    fn select_hosts_when_all_then_local_and_every_host() {
        let s = select_hosts(&["all".into()], &hosts()).unwrap();
        assert!(s.local);
        assert_eq!(s.remote.len(), 2);
    }

    #[test]
    fn select_hosts_when_local_and_name_then_both() {
        let s = select_hosts(&["local".into(), "B".into()], &hosts()).unwrap();
        assert!(s.local);
        assert_eq!(
            s.remote.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["B"]
        );
    }

    #[test]
    fn select_hosts_when_unknown_name_then_error() {
        assert!(select_hosts(&["C".into()], &hosts()).is_err());
    }

    #[test]
    fn host_label_when_local_then_local() {
        assert_eq!(Host::Local.label(), "local");
        assert_eq!(Host::Remote("Z270XX".into()).label(), "Z270XX");
    }
}
