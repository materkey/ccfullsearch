use std::fs;
use std::path::Path;

/// Encode a path the same way Claude CLI does: replace any non-ASCII-alphanumeric
/// character (except `-`) with `-`.
pub fn encode_path_for_claude(path: &str) -> String {
    path.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Walk the filesystem to find the original directory whose Claude-encoded form
/// matches `remaining_encoded` (the full encoded directory name without leading `-`).
/// `full_target` is the original complete encoded name for round-trip validation.
pub fn walk_fs_for_path(current_dir: &str, remaining_encoded: &str) -> Option<String> {
    walk_fs_recursive(current_dir, remaining_encoded, remaining_encoded)
}

fn walk_fs_recursive(
    current_dir: &str,
    remaining_encoded: &str,
    full_target: &str,
) -> Option<String> {
    if remaining_encoded.is_empty() {
        // Validate: encoding this path must produce the original target
        let encoded = encode_path_for_claude(current_dir);
        if encoded.strip_prefix('-') == Some(full_target) {
            return Some(current_dir.to_string());
        }
        return None;
    }

    let entries = fs::read_dir(current_dir).ok()?;
    let mut dir_entries: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
    dir_entries.sort_by_key(|e| e.file_name());

    for entry in dir_entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let encoded = encode_path_for_claude(&name);

        // Exact match: this is the last path component
        if encoded == remaining_encoded {
            let candidate = entry.path().to_string_lossy().to_string();
            // Round-trip validation
            let candidate_encoded = encode_path_for_claude(&candidate);
            if candidate_encoded.strip_prefix('-') == Some(full_target) {
                return Some(candidate);
            }
        }

        // Prefix match: more components follow after a `-` separator (which represents `/`)
        if remaining_encoded.starts_with(&encoded) {
            let after = &remaining_encoded[encoded.len()..];
            if let Some(rest) = after.strip_prefix('-') {
                if let Some(result) = walk_fs_recursive(entry.path().to_str()?, rest, full_target) {
                    return Some(result);
                }
            }
        }
    }

    None
}

/// Decode the original project path from the .claude/projects folder name.
/// First tries walking the filesystem to find the exact directory (handles ambiguous
/// encodings like spaces, parentheses, dots all becoming `-`).
/// Falls back to naive string-based decoding.
pub fn decode_project_path(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let claude_project_dir = path.parent()?;
    let dir_name = claude_project_dir.file_name()?.to_str()?;

    // Strategy 1: Walk the filesystem to find the exact matching path.
    let remaining = dir_name.strip_prefix('-').unwrap_or(dir_name);
    if !remaining.is_empty() {
        if let Some(found) = walk_fs_for_path("/", remaining) {
            return Some(found);
        }
    }

    // Strategy 2: If there's a "-projects-" marker, use it
    if let Some(projects_idx) = dir_name.rfind("-projects-") {
        let path_prefix = if dir_name.starts_with('-') {
            &dir_name[1..projects_idx]
        } else {
            &dir_name[..projects_idx]
        };
        let path_prefix = path_prefix
            .replace("--", "\x00")
            .replace('-', "/")
            .replace('\x00', "/.");
        let project_name = &dir_name[projects_idx + 10..];
        return Some(format!("/{}/projects/{}", path_prefix, project_name));
    }

    // Strategy 3: Just convert dashes to slashes (handle -- as /. for hidden dirs)
    let stripped = dir_name.strip_prefix('-').unwrap_or(dir_name);
    let decoded = stripped
        .replace("--", "\x00")
        .replace('-', "/")
        .replace('\x00', "/.");
    Some(format!("/{}", decoded))
}

/// Extract the actual project path from the .claude/projects path (test helper).
#[cfg(test)]
pub fn extract_project_path(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let claude_project_dir = path.parent()?;
    let dir_name = claude_project_dir.file_name()?.to_str()?;

    if let Some(projects_idx) = dir_name.rfind("-projects-") {
        let path_prefix = if dir_name.starts_with('-') {
            &dir_name[1..projects_idx]
        } else {
            &dir_name[..projects_idx]
        };
        let path_prefix = path_prefix
            .replace("--", "\x00")
            .replace('-', "/")
            .replace('\x00', "/.");
        let project_name = &dir_name[projects_idx + 10..];
        let candidate = format!("/{}/projects/{}", path_prefix, project_name);
        if Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }

    let stripped = dir_name.strip_prefix('-').unwrap_or(dir_name);
    let decoded = stripped
        .replace("--", "\x00")
        .replace('-', "/")
        .replace('\x00', "/.");
    let candidate = format!("/{}", decoded);
    if Path::new(&candidate).exists() {
        return Some(candidate);
    }

    let parts: Vec<&str> = dir_name.split('-').collect();
    for split_point in (1..parts.len()).rev() {
        let path_part: String = parts[..split_point].join("/");
        let name_part: String = parts[split_point..].join("-");

        let candidate = if path_part.starts_with('/') {
            format!("{}/{}", path_part, name_part)
        } else {
            format!("/{}/{}", path_part, name_part)
        };

        let candidate = candidate.replace("//", "/.");

        if Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_encode_path_simple() {
        assert_eq!(encode_path_for_claude("/Users/user"), "-Users-user");
    }

    #[test]
    fn test_encode_path_hidden_dir() {
        assert_eq!(
            encode_path_for_claude("/Users/user/.claude"),
            "-Users-user--claude"
        );
    }

    #[test]
    fn test_encode_path_spaces_and_parens() {
        assert_eq!(
            encode_path_for_claude("/Users/user/Downloads/dc-vpn (1)"),
            "-Users-user-Downloads-dc-vpn--1-"
        );
    }

    #[test]
    fn test_encode_path_underscores() {
        assert_eq!(
            encode_path_for_claude("/Users/user/my_project"),
            "-Users-user-my-project"
        );
    }

    #[test]
    fn test_walk_fs_simple() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("alpha").join("beta");
        fs::create_dir_all(&sub).unwrap();

        let encoded = encode_path_for_claude(dir.path().to_str().unwrap());
        let remaining = encoded.strip_prefix('-').unwrap();
        let result = walk_fs_for_path("/", remaining);
        assert_eq!(result, Some(dir.path().to_string_lossy().to_string()));

        let full_encoded = encode_path_for_claude(sub.to_str().unwrap());
        let remaining = full_encoded.strip_prefix('-').unwrap();
        let result = walk_fs_for_path("/", remaining);
        assert_eq!(result, Some(sub.to_string_lossy().to_string()));
    }

    #[test]
    fn test_walk_fs_special_chars() {
        let dir = TempDir::new().unwrap();
        let special = dir.path().join("my dir (2)");
        fs::create_dir_all(&special).unwrap();

        let full_encoded = encode_path_for_claude(special.to_str().unwrap());
        let remaining = full_encoded.strip_prefix('-').unwrap();
        let result = walk_fs_for_path("/", remaining);
        assert_eq!(result, Some(special.to_string_lossy().to_string()));
    }

    #[test]
    fn test_walk_fs_dash_ambiguity() {
        let dir = TempDir::new().unwrap();
        let dashed = dir.path().join("a-b");
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&dashed).unwrap();
        fs::create_dir_all(&nested).unwrap();

        let encoded = encode_path_for_claude(dashed.to_str().unwrap());
        let remaining = encoded.strip_prefix('-').unwrap();
        let result = walk_fs_for_path("/", remaining);
        assert!(result.is_some());
        let found = result.unwrap();
        assert!(found == dashed.to_string_lossy() || found == nested.to_string_lossy());
    }

    #[test]
    fn test_decode_project_path_nonexistent_falls_back() {
        let file_path = "/Users/user/.claude/projects/-Users-user-projects-myapp/session.jsonl";
        let result = decode_project_path(file_path);
        assert_eq!(result, Some("/Users/user/projects/myapp".to_string()));
    }

    #[test]
    fn test_decode_project_path_with_projects_marker() {
        let file_path = "/fake/.claude/projects/-Users-user-projects-myapp/session.jsonl";
        let result = decode_project_path(file_path);
        assert_eq!(result, Some("/Users/user/projects/myapp".to_string()));
    }

    #[test]
    fn test_decode_project_path_hidden_dir_fallback() {
        let file_path = "/fake/.claude/projects/-Users-user--claude/session.jsonl";
        let result = decode_project_path(file_path);
        assert_eq!(result, Some("/Users/user/.claude".to_string()));
    }

    #[test]
    fn test_decode_roundtrip_with_tempdir() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("my project (v2)");
        fs::create_dir_all(&project).unwrap();

        let encoded_name = encode_path_for_claude(project.to_str().unwrap());
        let remaining = encoded_name.strip_prefix('-').unwrap();
        let result = walk_fs_for_path("/", remaining);
        assert_eq!(result, Some(project.to_string_lossy().to_string()));
    }

    #[test]
    fn test_extract_project_path_with_projects_marker() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("projects").join("myapp");
        fs::create_dir_all(&project).unwrap();

        let encoded = encode_path_for_claude(project.to_str().unwrap());
        let file_path = format!("/fake/.claude/projects/{}/session.jsonl", encoded);
        let result = extract_project_path(&file_path);
        assert_eq!(result, Some(project.to_string_lossy().to_string()));
    }

    #[test]
    fn test_extract_project_path_simple_dir() {
        let dir = TempDir::new().unwrap();
        let encoded = encode_path_for_claude(dir.path().to_str().unwrap());
        let file_path = format!("/fake/.claude/projects/{}/session.jsonl", encoded);
        let result = extract_project_path(&file_path);
        assert_eq!(result, Some(dir.path().to_string_lossy().to_string()));
    }

    #[test]
    fn test_extract_project_path_nonexistent_returns_none() {
        let file_path = "/fake/.claude/projects/-nonexistent-path-12345/session.jsonl";
        let result = extract_project_path(file_path);
        assert!(result.is_none() || Path::new(&result.unwrap()).exists());
    }
}
