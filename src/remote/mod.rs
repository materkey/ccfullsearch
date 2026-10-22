//! Remote hosts: where a session lives, and how ccs reaches other machines.

pub mod config;
pub mod ssh;
pub mod wire;

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

    #[test]
    fn host_label_when_local_then_local() {
        assert_eq!(Host::Local.label(), "local");
        assert_eq!(Host::Remote("Z270XX".into()).label(), "Z270XX");
    }
}
