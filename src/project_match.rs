//! Path rules shared by the picker's project filter and `ccs serve`.

use crate::session::SessionProvider;

pub fn normalize_path_for_prefix_check(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized.trim_end_matches(['/', '\\']).to_string()
}

pub fn path_is_within_project(file_path: &str, project_path: &str) -> bool {
    let file_path = normalize_path_for_prefix_check(file_path);
    let project_path = normalize_path_for_prefix_check(project_path);

    file_path == project_path
        || file_path
            .strip_prefix(&project_path)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `path` relative to `home`, compared by components. A path outside `home`
/// stays absolute, so it only matches the same absolute path elsewhere.
pub fn home_relative(path: &str, home: &str) -> String {
    let path = normalize_path_for_prefix_check(path);
    let home = normalize_path_for_prefix_check(home);
    if path == home {
        return String::new();
    }
    match path
        .strip_prefix(&home)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        Some(rest) if !home.is_empty() => rest.to_string(),
        _ => path,
    }
}

/// Whether a session recorded in `session_cwd` belongs to `project`, a path
/// relative to the serving machine's `home` (or absolute when outside it).
/// Claude sessions match only the project directory itself, as the local
/// Ctrl+A does; Codex and Opencode sessions also match from subdirectories.
pub fn remote_project_matches(
    provider: SessionProvider,
    session_cwd: &str,
    project: &str,
    home: &str,
) -> bool {
    let relative = home_relative(session_cwd, home);
    match provider {
        SessionProvider::Claude => {
            normalize_path_for_prefix_check(&relative) == normalize_path_for_prefix_check(project)
        }
        SessionProvider::Codex | SessionProvider::Opencode => {
            path_is_within_project(&relative, project)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_is_within_project_rejects_sibling_prefixes() {
        assert!(path_is_within_project(
            r"C:\Users\test\project\session.jsonl",
            r"C:/Users/test/project"
        ));
        assert!(!path_is_within_project(
            r"C:\Users\test\project-other\session.jsonl",
            r"C:/Users/test/project"
        ));
    }

    #[test]
    fn home_relative_when_inside_home_then_strips_home() {
        assert_eq!(home_relative("/home/m/projects/x", "/home/m"), "projects/x");
    }

    #[test]
    fn home_relative_when_home_itself_then_empty() {
        assert_eq!(home_relative("/home/m/", "/home/m"), "");
    }

    #[test]
    fn home_relative_when_sibling_prefix_then_unchanged() {
        assert_eq!(home_relative("/home/mx/a", "/home/m"), "/home/mx/a");
    }

    #[test]
    fn remote_project_matches_when_claude_in_subdir_then_false() {
        assert!(!remote_project_matches(
            SessionProvider::Claude,
            "/home/m/projects/x/sub",
            "projects/x",
            "/home/m"
        ));
    }

    #[test]
    fn remote_project_matches_when_claude_same_dir_then_true() {
        assert!(remote_project_matches(
            SessionProvider::Claude,
            "/home/m/projects/x",
            "projects/x/",
            "/home/m"
        ));
    }

    #[test]
    fn remote_project_matches_when_codex_in_subdir_then_true() {
        assert!(remote_project_matches(
            SessionProvider::Codex,
            "/home/m/projects/x/sub",
            "projects/x",
            "/home/m"
        ));
    }

    #[test]
    fn remote_project_matches_when_sibling_name_then_false() {
        assert!(!remote_project_matches(
            SessionProvider::Codex,
            "/home/m/projects/x-old",
            "projects/x",
            "/home/m"
        ));
    }

    #[test]
    fn remote_project_matches_when_outside_home_then_exact_abs() {
        assert!(remote_project_matches(
            SessionProvider::Opencode,
            "/srv/p",
            "/srv/p",
            "/home/m"
        ));
    }
}
