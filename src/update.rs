use semver::Version;
use std::path::{Path, PathBuf};
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

/// Guard: refuse to self-update a Homebrew-managed install.
fn ensure_not_homebrew(exe_path: &Path) -> Result<(), String> {
    if is_homebrew_install(exe_path) {
        return Err("ccs is managed by Homebrew. Run `brew upgrade ccs` instead.".to_string());
    }
    Ok(())
}

/// Locate the running executable (canonicalized to resolve symlinks) and
/// verify it is safe to replace in place.
fn resolve_update_target() -> Result<PathBuf, String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("Could not determine executable path: {e}"))?;
    let canonical_exe = std::fs::canonicalize(&current_exe).unwrap_or(current_exe);
    ensure_not_homebrew(&canonical_exe)?;
    Ok(canonical_exe)
}

/// cargo-dist artifact naming: ccfullsearch-{target}.tar.gz
fn release_artifact_name() -> Result<String, String> {
    let triple = target_triple()?;
    Ok(format!("ccfullsearch-{triple}"))
}

/// Preflight checks before touching the network: executable path and artifact name.
fn preflight() -> Result<(PathBuf, String), String> {
    let canonical_exe = resolve_update_target()?;
    let artifact_name = release_artifact_name()?;
    Ok((canonical_exe, artifact_name))
}

/// Parse the release tag out of a GitHub API `releases/latest` response body.
fn parse_latest_tag(body: &[u8]) -> Result<String, String> {
    let body: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("Failed to parse GitHub API response: {e}"))?;

    let tag = body["tag_name"]
        .as_str()
        .ok_or("No tag_name in GitHub API response")?;

    Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

/// Fetch the latest release tag from GitHub API using curl.
fn fetch_latest_version() -> Result<String, String> {
    // Same retry knobs as download() — defensive against transient api.github.com failures.
    let output = Command::new("curl")
        .args([
            "-sSf",
            "--connect-timeout",
            "20",
            "--max-time",
            "30",
            "--retry",
            "3",
            "--retry-connrefused",
            "--retry-delay",
            "1",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()
        .map_err(|e| format!("Failed to run curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to fetch latest release: {}", stderr.trim()));
    }

    parse_latest_tag(&output.stdout)
}

/// Download a URL to a file path using curl.
fn download(url: &str, dest: &Path) -> Result<(), String> {
    // GitHub releases redirect to a Fastly CDN (release-assets.githubusercontent.com)
    // and individual IPs in 185.199.x.x occasionally stall at TLS handshake. Retry
    // lets curl pick a different IP from the DNS rotation instead of failing the run.
    let status = Command::new("curl")
        .args([
            "-sSLf",
            "--connect-timeout",
            "20",
            "--max-time",
            "120",
            "--retry",
            "3",
            "--retry-connrefused",
            "--retry-delay",
            "1",
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

/// Extract the hash field from `sha256sum`/`shasum` output ("<hash>  <file>").
fn parse_hash_output(stdout: &[u8]) -> Option<String> {
    let out = String::from_utf8_lossy(stdout);
    out.split_whitespace().next().map(|s| s.to_string())
}

/// Try `sha256sum` (common on Linux); None if unavailable or failed.
fn sha256_via_sha256sum(path: &Path) -> Option<String> {
    let output = Command::new("sha256sum").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_hash_output(&output.stdout)
}

/// Fall back to `shasum -a 256` (macOS).
fn sha256_via_shasum(path: &Path) -> Result<String, String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|e| format!("Neither sha256sum nor shasum found: {e}"))?;

    if !output.status.success() {
        return Err("Checksum command failed".to_string());
    }

    parse_hash_output(&output.stdout).ok_or_else(|| "Could not parse checksum output".to_string())
}

/// Compute SHA-256 hash of a file using system tools.
fn sha256_of(path: &Path) -> Result<String, String> {
    if let Some(hash) = sha256_via_sha256sum(path) {
        return Ok(hash);
    }
    sha256_via_shasum(path)
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

/// Read the downloaded checksum file and verify the archive against it.
fn verify_downloaded(tar_path: &Path, sha_path: &Path) -> Result<(), String> {
    let sha_content = std::fs::read_to_string(sha_path)
        .map_err(|e| format!("Failed to read checksum file: {e}"))?;
    verify_checksum(tar_path, &sha_content)
}

/// URLs for the release tarball and its checksum file.
fn release_asset_urls(latest_version: &str, artifact_name: &str) -> (String, String) {
    let base_url = format!("https://github.com/{REPO}/releases/download/v{latest_version}");
    (
        format!("{base_url}/{artifact_name}.tar.gz"),
        format!("{base_url}/{artifact_name}.tar.gz.sha256"),
    )
}

/// Download the release tarball and its checksum file into a temp directory.
fn download_release(
    latest_version: &str,
    artifact_name: &str,
    tmp_dir: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    let tar_path = tmp_dir.join(format!("{artifact_name}.tar.gz"));
    let sha_path = tmp_dir.join(format!("{artifact_name}.tar.gz.sha256"));
    let (tar_url, sha_url) = release_asset_urls(latest_version, artifact_name);

    download(&tar_url, &tar_path)?;
    download(&sha_url, &sha_path)?;
    Ok((tar_path, sha_path))
}

/// Find the extracted binary inside the extract directory.
fn locate_extracted_binary(extract_dir: &Path, artifact_name: &str) -> Result<PathBuf, String> {
    // cargo-dist extracts into a subdirectory named after the artifact
    let nested = extract_dir.join(artifact_name).join(BIN_NAME);
    if nested.exists() {
        return Ok(nested);
    }
    // Fallback: binary directly in extract dir
    let flat = extract_dir.join(BIN_NAME);
    if flat.exists() {
        return Ok(flat);
    }
    Err(format!(
        "Extracted archive does not contain '{BIN_NAME}' binary"
    ))
}

/// Extract the archive and locate the new binary inside it.
fn extract_and_locate(
    tar_path: &Path,
    tmp_dir: &Path,
    artifact_name: &str,
) -> Result<PathBuf, String> {
    let extract_dir = tmp_dir.join("extract");
    std::fs::create_dir(&extract_dir).map_err(|e| format!("Failed to create extract dir: {e}"))?;
    extract_tar(tar_path, &extract_dir)?;
    locate_extracted_binary(&extract_dir, artifact_name)
}

/// Download, verify, and extract the release; returns the path of the new binary.
fn fetch_and_verify(
    latest_version: &str,
    artifact_name: &str,
    tmp_dir: &Path,
) -> Result<PathBuf, String> {
    let (tar_path, sha_path) = download_release(latest_version, artifact_name, tmp_dir)?;

    eprintln!("Verifying checksum...");
    verify_downloaded(&tar_path, &sha_path)?;

    eprintln!("Installing...");
    extract_and_locate(&tar_path, tmp_dir, artifact_name)
}

/// Download the release into a temp directory and swap the binary in place.
fn download_and_install(
    latest_version: &str,
    artifact_name: &str,
    current_exe: &Path,
) -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("Failed to create temp directory: {e}"))?;
    let new_binary = fetch_and_verify(latest_version, artifact_name, tmp.path())?;
    replace_binary(&new_binary, current_exe)
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

/// Outcome of comparing the running build against the latest release.
#[derive(Debug, PartialEq)]
enum UpdateAction {
    /// No download needed; message explains why.
    Skip(String),
    Upgrade,
}

/// Decide whether an upgrade is needed based on version comparison.
fn decide_update(current: &str, latest: &str) -> Result<UpdateAction, String> {
    match compare_versions(current, latest)? {
        std::cmp::Ordering::Equal => Ok(UpdateAction::Skip(format!(
            "Already up to date (v{current})"
        ))),
        std::cmp::Ordering::Greater => Ok(UpdateAction::Skip(format!(
            "Current build v{current} is newer than latest release v{latest}"
        ))),
        std::cmp::Ordering::Less => Ok(UpdateAction::Upgrade),
    }
}

/// Perform the actual upgrade to the given release version.
fn perform_upgrade(
    latest_version: &str,
    artifact_name: &str,
    current_exe: &Path,
) -> Result<(), String> {
    eprintln!("Downloading v{latest_version}...");
    download_and_install(latest_version, artifact_name, current_exe)?;
    eprintln!("Updated ccs v{CURRENT_VERSION} -> v{latest_version}");
    Ok(())
}

/// Either report why no update is needed or upgrade to the latest release.
fn update_to(latest_version: &str, artifact_name: &str, current_exe: &Path) -> Result<(), String> {
    match decide_update(CURRENT_VERSION, latest_version)? {
        UpdateAction::Skip(message) => {
            eprintln!("{message}");
            Ok(())
        }
        UpdateAction::Upgrade => perform_upgrade(latest_version, artifact_name, current_exe),
    }
}

/// Run the self-update process.
pub fn run() -> Result<(), String> {
    let (canonical_exe, artifact_name) = preflight()?;

    eprintln!("Checking for updates...");
    let latest_version = fetch_latest_version()?;

    update_to(&latest_version, &artifact_name, &canonical_exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of the bytes "hello\n".
    const HELLO_SHA256: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

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
    fn ensure_not_homebrew_rejects_cellar_installs() {
        let err =
            ensure_not_homebrew(Path::new("/opt/homebrew/Cellar/ccs/0.5.0/bin/ccs")).unwrap_err();
        assert!(err.contains("brew upgrade"), "Unexpected error: {err}");
    }

    #[test]
    fn ensure_not_homebrew_allows_cargo_installs() {
        assert!(ensure_not_homebrew(Path::new("/Users/user/.cargo/bin/ccs")).is_ok());
    }

    #[test]
    fn resolve_update_target_finds_test_binary() {
        // The test binary lives in target/, never under a Homebrew Cellar.
        let path = resolve_update_target().unwrap();
        assert!(path.exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn release_artifact_name_uses_target_triple() {
        let name = release_artifact_name().unwrap();
        assert_eq!(name, format!("ccfullsearch-{}", target_triple().unwrap()));
    }

    #[cfg(not(windows))]
    #[test]
    fn preflight_returns_exe_and_artifact_name() {
        let (exe, artifact_name) = preflight().unwrap();
        assert!(exe.exists());
        assert!(artifact_name.starts_with("ccfullsearch-"));
    }

    #[test]
    fn parse_latest_tag_strips_v_prefix() {
        assert_eq!(
            parse_latest_tag(br#"{"tag_name": "v1.2.3"}"#).unwrap(),
            "1.2.3"
        );
    }

    #[test]
    fn parse_latest_tag_accepts_bare_version() {
        assert_eq!(
            parse_latest_tag(br#"{"tag_name": "1.2.3"}"#).unwrap(),
            "1.2.3"
        );
    }

    #[test]
    fn parse_latest_tag_rejects_missing_tag_name() {
        let err = parse_latest_tag(br#"{"message": "Not Found"}"#).unwrap_err();
        assert!(err.contains("No tag_name"), "Unexpected error: {err}");
    }

    #[test]
    fn parse_latest_tag_rejects_invalid_json() {
        assert!(parse_latest_tag(b"not json").is_err());
    }

    #[test]
    fn parse_hash_output_extracts_first_field() {
        assert_eq!(
            parse_hash_output(b"abc123  file.tar.gz\n").unwrap(),
            "abc123"
        );
    }

    #[test]
    fn parse_hash_output_returns_none_for_empty_output() {
        assert!(parse_hash_output(b"").is_none());
    }

    #[test]
    fn sha256_of_computes_known_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("payload");
        std::fs::write(&file, b"hello\n").unwrap();
        assert_eq!(sha256_of(&file).unwrap(), HELLO_SHA256);
    }

    #[test]
    fn sha256_of_fails_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(sha256_of(&tmp.path().join("does-not-exist")).is_err());
    }

    #[test]
    fn verify_checksum_accepts_matching_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("payload");
        std::fs::write(&file, b"hello\n").unwrap();
        verify_checksum(&file, &format!("{HELLO_SHA256}  payload")).unwrap();
    }

    #[test]
    fn verify_checksum_rejects_mismatched_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("payload");
        std::fs::write(&file, b"hello\n").unwrap();
        let err = verify_checksum(&file, "deadbeef  payload").unwrap_err();
        assert!(err.contains("Checksum mismatch"), "Unexpected error: {err}");
    }

    #[test]
    fn verify_checksum_rejects_empty_checksum_content() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("payload");
        std::fs::write(&file, b"hello\n").unwrap();
        let err = verify_checksum(&file, "  ").unwrap_err();
        assert!(
            err.contains("Invalid checksum file format"),
            "Unexpected error: {err}"
        );
    }

    #[test]
    fn verify_downloaded_accepts_valid_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let tar = tmp.path().join("a.tar.gz");
        let sha = tmp.path().join("a.tar.gz.sha256");
        std::fs::write(&tar, b"hello\n").unwrap();
        std::fs::write(&sha, format!("{HELLO_SHA256}  a.tar.gz")).unwrap();
        verify_downloaded(&tar, &sha).unwrap();
    }

    #[test]
    fn verify_downloaded_fails_without_checksum_file() {
        let tmp = tempfile::tempdir().unwrap();
        let tar = tmp.path().join("a.tar.gz");
        std::fs::write(&tar, b"hello\n").unwrap();
        let err = verify_downloaded(&tar, &tmp.path().join("missing.sha256")).unwrap_err();
        assert!(
            err.contains("Failed to read checksum file"),
            "Unexpected error: {err}"
        );
    }

    #[test]
    fn release_asset_urls_point_at_github_release() {
        let (tar_url, sha_url) = release_asset_urls("1.2.3", "ccfullsearch-x86_64-apple-darwin");
        assert_eq!(
            tar_url,
            "https://github.com/materkey/ccfullsearch/releases/download/v1.2.3/ccfullsearch-x86_64-apple-darwin.tar.gz"
        );
        assert_eq!(sha_url, format!("{tar_url}.sha256"));
    }

    #[test]
    fn locate_extracted_binary_finds_nested_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let nested_dir = tmp.path().join("ccfullsearch-test");
        std::fs::create_dir(&nested_dir).unwrap();
        std::fs::write(nested_dir.join(BIN_NAME), b"bin").unwrap();
        assert_eq!(
            locate_extracted_binary(tmp.path(), "ccfullsearch-test").unwrap(),
            nested_dir.join(BIN_NAME)
        );
    }

    #[test]
    fn locate_extracted_binary_finds_flat_layout() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(BIN_NAME), b"bin").unwrap();
        assert_eq!(
            locate_extracted_binary(tmp.path(), "ccfullsearch-test").unwrap(),
            tmp.path().join(BIN_NAME)
        );
    }

    #[test]
    fn locate_extracted_binary_reports_missing_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let err = locate_extracted_binary(tmp.path(), "ccfullsearch-test").unwrap_err();
        assert!(err.contains("does not contain"), "Unexpected error: {err}");
    }

    #[test]
    fn replace_binary_swaps_and_cleans_up() {
        let src_dir = tempfile::tempdir().unwrap();
        let exe_dir = tempfile::tempdir().unwrap();
        let new_binary = src_dir.path().join("ccs-new");
        let current_exe = exe_dir.path().join(BIN_NAME);
        std::fs::write(&new_binary, b"new-binary").unwrap();
        std::fs::write(&current_exe, b"old-binary").unwrap();

        replace_binary(&new_binary, &current_exe).unwrap();

        assert_eq!(std::fs::read(&current_exe).unwrap(), b"new-binary");
        assert!(!exe_dir.path().join(format!(".{BIN_NAME}.old")).exists());
        assert!(!exe_dir.path().join(format!(".{BIN_NAME}.new")).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&current_exe)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o755, 0o755);
        }
    }

    #[test]
    fn replace_binary_fails_for_missing_source() {
        let exe_dir = tempfile::tempdir().unwrap();
        let current_exe = exe_dir.path().join(BIN_NAME);
        std::fs::write(&current_exe, b"old-binary").unwrap();

        let err = replace_binary(&exe_dir.path().join("does-not-exist"), &current_exe).unwrap_err();
        assert!(
            err.contains("Failed to copy new binary"),
            "Unexpected error: {err}"
        );
        // Original binary must stay untouched
        assert_eq!(std::fs::read(&current_exe).unwrap(), b"old-binary");
    }

    #[test]
    fn replace_binary_requires_parent_dir() {
        let src_dir = tempfile::tempdir().unwrap();
        let new_binary = src_dir.path().join("ccs-new");
        std::fs::write(&new_binary, b"new-binary").unwrap();

        let err = replace_binary(&new_binary, Path::new("/")).unwrap_err();
        assert!(
            err.contains("Could not determine binary directory"),
            "Unexpected error: {err}"
        );
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

    #[test]
    fn decide_update_skips_when_already_current() {
        assert_eq!(
            decide_update("0.5.0", "0.5.0").unwrap(),
            UpdateAction::Skip("Already up to date (v0.5.0)".to_string())
        );
    }

    #[test]
    fn decide_update_skips_when_local_build_is_newer() {
        assert_eq!(
            decide_update("0.5.1-dev.0", "0.5.0").unwrap(),
            UpdateAction::Skip(
                "Current build v0.5.1-dev.0 is newer than latest release v0.5.0".to_string()
            )
        );
    }

    #[test]
    fn decide_update_upgrades_when_release_is_newer() {
        assert_eq!(
            decide_update("0.5.0", "0.5.1").unwrap(),
            UpdateAction::Upgrade
        );
    }

    #[test]
    fn decide_update_rejects_invalid_version() {
        assert!(decide_update("0.5.0", "not-a-version").is_err());
    }

    #[test]
    fn update_to_skips_when_already_current() {
        // Equal versions short-circuit before any network access.
        let exe = std::env::current_exe().unwrap();
        update_to(CURRENT_VERSION, "ccfullsearch-test", &exe).unwrap();
    }

    #[test]
    fn update_to_rejects_invalid_latest_version() {
        let exe = std::env::current_exe().unwrap();
        assert!(update_to("not-a-version", "ccfullsearch-test", &exe).is_err());
    }
}
