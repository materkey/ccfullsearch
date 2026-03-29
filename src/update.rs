use semver::Version;
use std::path::Path;
use std::process::Command;

const REPO: &str = "materkey/ccfullsearch";
const BIN_NAME: &str = "ccs";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Map OS/arch to cargo-dist release artifact target triple.
fn target_triple() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", arch) => linux_target_triple(arch, cfg!(target_env = "musl")),
        (os, arch) => Err(format!("Unsupported platform: {os}/{arch}")),
    }
}

fn linux_target_triple(arch: &str, musl: bool) -> Result<&'static str, String> {
    match (arch, musl) {
        ("x86_64", false) => Ok("x86_64-unknown-linux-gnu"),
        ("x86_64", true) => Ok("x86_64-unknown-linux-musl"),
        ("aarch64", false) => Ok("aarch64-unknown-linux-gnu"),
        ("aarch64", true) => Ok("aarch64-unknown-linux-musl"),
        (arch, _) => Err(format!("Unsupported platform: linux/{arch}")),
    }
}

/// Check if the binary is managed by Homebrew.
fn is_homebrew_install(exe_path: &Path) -> bool {
    let path_str = exe_path.to_string_lossy();
    path_str.contains("/Cellar/")
}

/// Fetch the latest release tag from GitHub API using curl.
fn fetch_latest_version() -> Result<String, String> {
    let output = Command::new("curl")
        .args([
            "-sSf",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()
        .map_err(|e| format!("Failed to run curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to fetch latest release: {}", stderr.trim()));
    }

    let body: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse GitHub API response: {e}"))?;

    let tag = body["tag_name"]
        .as_str()
        .ok_or("No tag_name in GitHub API response")?;

    Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

/// Download a URL to a file path using curl.
fn download(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args([
            "-sSLf",
            "--connect-timeout",
            "10",
            "--max-time",
            "120",
            "-o",
        ])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("Failed to run curl: {e}"))?;

    if !status.success() {
        return Err(format!("Download failed: {url}"));
    }
    Ok(())
}

/// Extract a tar.gz archive into a directory.
fn extract_tar(archive: &Path, dest: &Path) -> Result<(), String> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .map_err(|e| format!("Failed to run tar: {e}"))?;

    if !status.success() {
        return Err("Failed to extract archive".to_string());
    }
    Ok(())
}

/// Compute SHA-256 hash of a file using system tools.
fn sha256_of(path: &Path) -> Result<String, String> {
    // Try sha256sum first (common on Linux)
    if let Ok(output) = Command::new("sha256sum").arg(path).output() {
        if output.status.success() {
            let out = String::from_utf8_lossy(&output.stdout);
            if let Some(hash) = out.split_whitespace().next() {
                return Ok(hash.to_string());
            }
        }
    }

    // Fall back to shasum -a 256 (macOS)
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|e| format!("Neither sha256sum nor shasum found: {e}"))?;

    if !output.status.success() {
        return Err("Checksum command failed".to_string());
    }

    let out = String::from_utf8_lossy(&output.stdout);
    out.split_whitespace()
        .next()
        .map(|s| s.to_string())
        .ok_or_else(|| "Could not parse checksum output".to_string())
}

/// Verify SHA-256 checksum of a file.
fn verify_checksum(file: &Path, expected_content: &str) -> Result<(), String> {
    let expected_hash = expected_content
        .split_whitespace()
        .next()
        .ok_or("Invalid checksum file format")?;

    let actual_hash = sha256_of(file)?;
    if actual_hash != expected_hash {
        return Err(format!(
            "Checksum mismatch!\n  Expected: {expected_hash}\n  Got:      {actual_hash}"
        ));
    }
    Ok(())
}

/// Replace the current binary with the new one, with rollback on failure.
fn replace_binary(new_binary: &Path, current_exe: &Path) -> Result<(), String> {
    let exe_dir = current_exe
        .parent()
        .ok_or("Could not determine binary directory")?;

    // Copy to destination directory to avoid EXDEV (cross-device rename)
    let staged = exe_dir.join(format!(".{BIN_NAME}.new"));
    std::fs::copy(new_binary, &staged).map_err(|e| format!("Failed to copy new binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set permissions: {e}"))?;
    }

    // Rename current -> .old, then staged -> current
    let backup = exe_dir.join(format!(".{BIN_NAME}.old"));
    std::fs::rename(current_exe, &backup)
        .map_err(|e| format!("Failed to move current binary aside: {e}"))?;

    if let Err(e) = std::fs::rename(&staged, current_exe) {
        // Rollback: restore the original
        let _ = std::fs::rename(&backup, current_exe);
        return Err(format!("Failed to install new binary (rolled back): {e}"));
    }

    // Cleanup
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

fn compare_versions(
    current_version: &str,
    latest_version: &str,
) -> Result<std::cmp::Ordering, String> {
    let current = Version::parse(current_version)
        .map_err(|e| format!("Invalid current version '{current_version}': {e}"))?;
    let latest = Version::parse(latest_version)
        .map_err(|e| format!("Invalid latest version '{latest_version}': {e}"))?;

    Ok(current.cmp(&latest))
}

/// Run the self-update process.
pub fn run() -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("Could not determine executable path: {e}"))?;

    // Guard: Homebrew-managed installs (canonicalize to resolve symlinks)
    let canonical_exe = std::fs::canonicalize(&current_exe).unwrap_or(current_exe.clone());
    if is_homebrew_install(&canonical_exe) {
        return Err("ccs is managed by Homebrew. Run `brew upgrade ccs` instead.".to_string());
    }

    let triple = target_triple()?;
    // cargo-dist artifact naming: ccfullsearch-{target}.tar.gz
    let artifact_name = format!("ccfullsearch-{triple}");

    eprintln!("Checking for updates...");

    let latest_version = fetch_latest_version()?;

    match compare_versions(CURRENT_VERSION, &latest_version)? {
        std::cmp::Ordering::Equal => {
            eprintln!("Already up to date (v{CURRENT_VERSION})");
            return Ok(());
        }
        std::cmp::Ordering::Greater => {
            eprintln!(
                "Current build v{CURRENT_VERSION} is newer than latest release v{latest_version}"
            );
            return Ok(());
        }
        std::cmp::Ordering::Less => {}
    }

    eprintln!("Downloading v{latest_version}...");

    let tmp = tempfile::tempdir().map_err(|e| format!("Failed to create temp directory: {e}"))?;
    let tar_path = tmp.path().join(format!("{artifact_name}.tar.gz"));
    let sha_path = tmp.path().join(format!("{artifact_name}.tar.gz.sha256"));

    let base_url = format!("https://github.com/{REPO}/releases/download/v{latest_version}");

    download(&format!("{base_url}/{artifact_name}.tar.gz"), &tar_path)?;
    download(
        &format!("{base_url}/{artifact_name}.tar.gz.sha256"),
        &sha_path,
    )?;

    eprintln!("Verifying checksum...");
    let sha_content = std::fs::read_to_string(&sha_path)
        .map_err(|e| format!("Failed to read checksum file: {e}"))?;
    verify_checksum(&tar_path, &sha_content)?;

    eprintln!("Installing...");
    let extract_dir = tmp.path().join("extract");
    std::fs::create_dir(&extract_dir).map_err(|e| format!("Failed to create extract dir: {e}"))?;
    extract_tar(&tar_path, &extract_dir)?;

    // cargo-dist extracts into a subdirectory named after the artifact
    let new_binary = extract_dir.join(&artifact_name).join(BIN_NAME);
    let new_binary = if new_binary.exists() {
        new_binary
    } else {
        // Fallback: binary directly in extract dir
        let flat = extract_dir.join(BIN_NAME);
        if flat.exists() {
            flat
        } else {
            return Err(format!(
                "Extracted archive does not contain '{BIN_NAME}' binary"
            ));
        }
    };

    replace_binary(&new_binary, &canonical_exe)?;

    eprintln!("Updated ccs v{CURRENT_VERSION} -> v{latest_version}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn target_triple_returns_known_value() {
        let triple = target_triple().unwrap();
        assert!(
            [
                "aarch64-apple-darwin",
                "x86_64-apple-darwin",
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
                "x86_64-unknown-linux-musl",
                "aarch64-unknown-linux-musl",
            ]
            .contains(&triple),
            "Unexpected triple: {triple}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn target_triple_is_unsupported_on_windows() {
        assert!(target_triple().is_err());
    }

    #[test]
    fn linux_target_triple_preserves_gnu_assets() {
        assert_eq!(
            linux_target_triple("x86_64", false).unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            linux_target_triple("aarch64", false).unwrap(),
            "aarch64-unknown-linux-gnu"
        );
    }

    #[test]
    fn linux_target_triple_selects_musl_assets() {
        assert_eq!(
            linux_target_triple("x86_64", true).unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            linux_target_triple("aarch64", true).unwrap(),
            "aarch64-unknown-linux-musl"
        );
    }

    #[test]
    fn is_homebrew_cellar() {
        assert!(is_homebrew_install(Path::new(
            "/opt/homebrew/Cellar/ccs/0.5.0/bin/ccs"
        )));
    }

    #[test]
    fn is_not_homebrew_cargo_home() {
        assert!(!is_homebrew_install(Path::new(
            "/Users/user/.cargo/bin/ccs"
        )));
    }

    #[test]
    fn is_not_homebrew_local_bin() {
        assert!(!is_homebrew_install(Path::new("/usr/local/bin/ccs")));
    }

    #[test]
    fn compare_versions_detects_equal_versions() {
        assert_eq!(
            compare_versions("0.5.0", "0.5.0").unwrap(),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn compare_versions_detects_newer_local_builds() {
        assert_eq!(
            compare_versions("0.5.1-dev.0", "0.5.0").unwrap(),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn compare_versions_detects_older_local_builds() {
        assert_eq!(
            compare_versions("0.5.0", "0.5.1").unwrap(),
            std::cmp::Ordering::Less
        );
    }
}
