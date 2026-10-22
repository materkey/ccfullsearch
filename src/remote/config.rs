//! Remote hosts from `[[hosts]]` in `config.toml` and `CCS_HOSTS`.

use crate::config::HostEntry;

/// The `ccs` binary used when a host entry does not name one.
const DEFAULT_CCS_PATH: &str = "ccs";

/// One remote host: its ssh alias and where `ccs` lives there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfig {
    pub name: String,
    pub ccs_path: String,
}

/// Pick the remote hosts. `CCS_HOSTS` wins (names, with `ccs_path` from the
/// config when it lists the same name); a test or tool that set
/// `CCFS_SEARCH_PATH` gets no hosts unless it also set `CCS_HOSTS`; otherwise
/// the config decides, and no `[[hosts]]` means no hosts.
pub fn resolve_hosts(
    ccs_hosts: Option<String>,
    search_path_overridden: bool,
    file_hosts: Option<Vec<HostEntry>>,
) -> Vec<HostConfig> {
    let from_file: Vec<HostConfig> = file_hosts
        .unwrap_or_default()
        .into_iter()
        .map(|entry| host(&entry.name, entry.ccs_path))
        .collect();
    let Some(names) = ccs_hosts else {
        return if search_path_overridden {
            Vec::new()
        } else {
            from_file
        };
    };
    names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            from_file
                .iter()
                .find(|h| h.name == name)
                .cloned()
                .unwrap_or_else(|| host(name, None))
        })
        .collect()
}

fn host(name: &str, ccs_path: Option<String>) -> HostConfig {
    HostConfig {
        name: name.to_string(),
        ccs_path: ccs_path.unwrap_or_else(|| DEFAULT_CCS_PATH.to_string()),
    }
}

/// Remote hosts for this process, from the environment and `config.toml`.
pub fn load_hosts() -> Result<Vec<HostConfig>, String> {
    let ccs_hosts = std::env::var("CCS_HOSTS").ok();
    let search_path_overridden = std::env::var_os("CCFS_SEARCH_PATH").is_some();
    // Tests and tools that pinned the search path stay hermetic: the user's
    // config is not even read.
    if search_path_overridden && ccs_hosts.is_none() {
        return Ok(Vec::new());
    }
    let file_hosts =
        match crate::config::config_path(std::env::var("XDG_CONFIG_HOME").ok(), dirs::home_dir()) {
            Some(path) => Some(crate::config::load_config(&path)?.hosts),
            None => None,
        };
    Ok(resolve_hosts(ccs_hosts, search_path_overridden, file_hosts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HostEntry;

    fn file() -> Option<Vec<HostEntry>> {
        Some(vec![HostEntry {
            name: "Z270XX".into(),
            ccs_path: None,
        }])
    }

    #[test]
    fn resolve_hosts_when_ccs_hosts_empty_then_none() {
        assert_eq!(resolve_hosts(Some(String::new()), false, file()), vec![]);
    }

    #[test]
    fn resolve_hosts_when_search_path_overridden_without_ccs_hosts_then_none() {
        assert_eq!(resolve_hosts(None, true, file()), vec![]);
    }

    #[test]
    fn resolve_hosts_when_no_file_and_no_env_then_none() {
        assert_eq!(resolve_hosts(None, false, None), vec![]);
    }

    #[test]
    fn resolve_hosts_when_file_has_host_then_default_ccs_path() {
        assert_eq!(
            resolve_hosts(None, false, file()),
            vec![HostConfig {
                name: "Z270XX".into(),
                ccs_path: "ccs".into()
            }]
        );
    }

    #[test]
    fn resolve_hosts_when_ccs_hosts_names_file_host_then_keeps_its_ccs_path() {
        let file = Some(vec![HostEntry {
            name: "Z270XX".into(),
            ccs_path: Some("/opt/ccs".into()),
        }]);
        assert_eq!(
            resolve_hosts(Some(" Z270XX, other ,".into()), true, file),
            vec![
                HostConfig {
                    name: "Z270XX".into(),
                    ccs_path: "/opt/ccs".into()
                },
                HostConfig {
                    name: "other".into(),
                    ccs_path: "ccs".into()
                },
            ]
        );
    }
}
