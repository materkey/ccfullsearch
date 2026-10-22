//! User configuration file: `$XDG_CONFIG_HOME/ccs/config.toml`, falling back
//! to `~/.config/ccs/config.toml` (also on macOS).

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_AGENT_MODEL: &str = "claude-haiku-5-5";

/// Reasoning effort passed to `claude --effort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// `[agent_search]` table: settings of the Ctrl+G agent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentSearchConfig {
    pub model: String,
    pub effort: Option<Effort>,
}

impl Default for AgentSearchConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_AGENT_MODEL.to_string(),
            effort: None,
        }
    }
}

/// One `[[hosts]]` entry: a remote machine searched over ssh.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEntry {
    /// ssh alias, also the label shown in results.
    pub name: String,
    /// `ccs` on that machine when it is not on the PATH of non-interactive ssh.
    pub ccs_path: Option<String>,
}

/// Whole config file. Unknown top-level tables are ignored, so a section
/// added by a newer ccs does not break an older one reading the same file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub agent_search: AgentSearchConfig,
    /// Remote hosts (`[[hosts]]`).
    pub hosts: Vec<HostEntry>,
}

/// Config file location. Env values are passed in so tests don't touch the
/// process env; an empty `XDG_CONFIG_HOME` counts as unset.
pub fn config_path(xdg_config_home: Option<String>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home?.join(".config"),
    };
    Some(base.join("ccs").join("config.toml"))
}

/// Read the config at `path`; a missing file gives the defaults.
pub fn load_config(path: &Path) -> Result<Config, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(format!("config {}: {}", path.display(), e)),
    };
    toml::from_str(&content).map_err(|e: toml::de::Error| {
        // `Display` adds a multi-line source snippet; the status bar has one line.
        let line = e
            .span()
            .map(|span| content[..span.start].matches('\n').count() + 1);
        match line {
            Some(line) => format!("config {}: line {line}: {}", path.display(), e.message()),
            None => format!("config {}: {}", path.display(), e.message()),
        }
    })
}

/// Read the config from its default location.
pub fn load_default_config() -> Result<Config, String> {
    match config_path(std::env::var("XDG_CONFIG_HOME").ok(), dirs::home_dir()) {
        Some(path) => load_config(&path),
        None => Ok(Config::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn xdg_set_config_path_uses_xdg_dir() {
        assert_eq!(
            config_path(Some("/x".into()), Some("/h".into())),
            Some(PathBuf::from("/x/ccs/config.toml"))
        );
    }

    #[test]
    fn xdg_empty_config_path_falls_back_to_home_dot_config() {
        assert_eq!(
            config_path(Some(String::new()), Some("/h".into())),
            Some(PathBuf::from("/h/.config/ccs/config.toml"))
        );
    }

    #[test]
    fn no_xdg_no_home_config_path_is_none() {
        assert_eq!(config_path(None, None), None);
    }

    #[test]
    fn missing_file_load_config_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_config(&dir.path().join("config.toml")).unwrap();
        assert_eq!(cfg.agent_search.model, "claude-haiku-5-5");
        assert_eq!(cfg.agent_search.effort, None);
    }

    #[test]
    fn model_and_effort_set_load_config_reads_both() {
        let (_dir, path) =
            write_config("[agent_search]\nmodel = \"claude-sonnet-5-5\"\neffort = \"high\"\n");
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.agent_search.model, "claude-sonnet-5-5");
        assert_eq!(cfg.agent_search.effort, Some(Effort::High));
        assert_eq!(Effort::High.as_str(), "high");
    }

    #[test]
    fn only_effort_set_load_config_keeps_default_model() {
        let (_dir, path) = write_config("[agent_search]\neffort = \"low\"\n");
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.agent_search.model, DEFAULT_AGENT_MODEL);
        assert_eq!(cfg.agent_search.effort, Some(Effort::Low));
    }

    #[test]
    fn unknown_effort_load_config_errors_with_path() {
        let (_dir, path) = write_config("[agent_search]\neffort = \"turbo\"\n");
        let err = load_config(&path).unwrap_err();
        assert!(
            err.starts_with(&format!("config {}:", path.display())),
            "{err}"
        );
        // The status bar is one line: the TOML snippet must not come along.
        assert!(!err.contains('\n'), "{err}");
        assert!(err.contains("turbo"), "{err}");
    }

    #[test]
    fn unknown_key_load_config_errors() {
        let (_dir, path) = write_config("[agent_search]\nmodle = \"x\"\n");
        assert!(load_config(&path).is_err());
    }

    #[test]
    fn hosts_tables_load_config_reads_names_and_ccs_path() {
        let (_dir, path) = write_config(
            "[[hosts]]\nname = \"Z270XX\"\n\n[[hosts]]\nname = \"devbox\"\nccs_path = \"/opt/ccs\"\n",
        );
        let cfg = load_config(&path).unwrap();
        assert_eq!(
            cfg.hosts,
            vec![
                HostEntry {
                    name: "Z270XX".into(),
                    ccs_path: None
                },
                HostEntry {
                    name: "devbox".into(),
                    ccs_path: Some("/opt/ccs".into())
                },
            ]
        );
    }

    #[test]
    fn host_without_name_load_config_errors() {
        let (_dir, path) = write_config("[[hosts]]\nccs_path = \"/opt/ccs\"\n");
        assert!(load_config(&path).is_err());
    }

    #[test]
    fn unknown_top_level_table_load_config_ignores_it() {
        let (_dir, path) = write_config("[future]\nx = 1\n[agent_search]\neffort = \"low\"\n");
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.agent_search.effort, Some(Effort::Low));
    }

    #[test]
    fn broken_toml_load_config_errors() {
        let (_dir, path) = write_config("[agent_search");
        assert!(load_config(&path).is_err());
    }
}
