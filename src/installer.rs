#![allow(unused, non_snake_case, dead_code, non_camel_case_types)]
//! Installation and update functionality for nano-rs.
//!
//! Supports self-install / self-update on both Windows and Unix:
//!   * Windows downloads via native WinHTTP (no PowerShell, no extra deps) and
//!     installs to %LOCALAPPDATA%\Microsoft\WindowsApps\nano.exe.
//!   * Unix downloads via `curl` (falling back to `wget`) — both near-universal
//!     and dependency-free — and installs to ~/.local/bin/nano.
//! Release assets are named per-platform: nano-<arch>.exe on Windows and
//! nano-linux-<arch> on Linux/Unix (arch is amd64 or arm64).

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows::core::{w, PCWSTR, PWSTR};
#[cfg(windows)]
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE,
    URL_COMPONENTS,
    WINHTTP_INTERNET_SCHEME_HTTPS,
    WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
#[cfg(windows)]
use windows::Win32::Foundation::GetLastError;

/// GitHub repository for releases
const GITHUB_REPO: &str = "faratech/nano-rs";

/// User-Agent used for GitHub requests (GitHub's API requires one).
const USER_AGENT: &str = "nano-rs-updater/1.0";

/// A user-private cache directory for pending downloads and the check stamp.
///
/// SECURITY: the pending-update file MUST NOT live in a world-writable location
/// like `/tmp`. `apply_pending_update()` copies this file over the running
/// executable; if another local user could create it, that would be a local
/// code-execution vector. We use a per-user directory created mode 0700 on Unix
/// (XDG cache) and %LOCALAPPDATA% on Windows (already per-user).
#[cfg(unix)]
fn cache_dir() -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("nano-rs");
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
    // Tighten perms in case the directory pre-existed with looser modes.
    let _ = fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    Some(dir)
}

#[cfg(windows)]
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let dir = base.join("nano-rs");
    let _ = fs::create_dir_all(&dir);
    Some(dir)
}

/// Path of the pending-update file, inside the per-user cache directory.
///
/// Returns None if no private cache directory is available (e.g. HOME unset).
/// We deliberately do NOT fall back to a world-writable location like /tmp: the
/// pending file is trusted and applied over the running executable, so it must
/// only ever come from a directory only this user can write.
fn update_temp_path() -> Option<PathBuf> {
    let name = if cfg!(windows) { "nano-update.exe" } else { "nano-update" };
    Some(cache_dir()?.join(name))
}

/// Stamp file recording when the background check last ran (for throttling).
fn last_check_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("last-check"))
}

/// Whether the background check ran within the last 24h.
fn checked_recently() -> bool {
    let Some(p) = last_check_path() else { return false };
    if let Ok(md) = fs::metadata(&p) {
        if let Ok(modified) = md.modified() {
            if let Ok(elapsed) = modified.elapsed() {
                return elapsed < std::time::Duration::from_secs(24 * 3600);
            }
        }
    }
    false
}

/// Record that the background check ran now.
fn stamp_check() {
    if let Some(p) = last_check_path() {
        let _ = fs::write(&p, b"");
    }
}

/// Whether the launch-time background update check should run.
///
/// Disabled entirely by `NANO_NO_UPDATE_CHECK`. On Windows it is on by default;
/// on Unix it is OPT-IN (set `NANO_UPDATE_CHECK`) because nano is commonly the
/// system `$EDITOR` and shouldn't phone home on every `git commit`.
fn background_updates_enabled() -> bool {
    if std::env::var_os("NANO_NO_UPDATE_CHECK").is_some() {
        return false;
    }
    if cfg!(windows) {
        return true;
    }
    std::env::var_os("NANO_UPDATE_CHECK").is_some()
}

/// Whether we can actually replace the running executable (its directory is
/// writable). Used to avoid downloading updates we could never apply — which
/// would otherwise re-notify "update downloaded" on every launch.
fn running_exe_replaceable() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    match exe.parent() {
        Some(dir) => tempfile::tempfile_in(dir).is_ok(),
        None => false,
    }
}

/// Backup path for the rename trick: `<path>.old` (keeps any existing extension,
/// e.g. nano.exe -> nano.exe.old, nano -> nano.old).
fn backup_path_for(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Mark a file executable on Unix (no-op on Windows).
fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            let mut perm = meta.permissions();
            perm.set_mode(perm.mode() | 0o755);
            let _ = fs::set_permissions(path, perm);
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Get the installation path for nano.
///
/// Windows: %LOCALAPPDATA%\Microsoft\WindowsApps\nano.exe (user-writable, on PATH).
/// Unix:    ~/.local/bin/nano (user-writable; on PATH for most modern setups).
pub fn get_install_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        let local_app_data = std::env::var("LOCALAPPDATA")?;
        Ok(PathBuf::from(&local_app_data)
            .join("Microsoft")
            .join("WindowsApps")
            .join("nano.exe"))
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(&home).join(".local").join("bin").join("nano"))
    }
}

/// Get version of installed nano (if any)
pub fn get_installed_version() -> Option<String> {
    let install_path = get_install_path().ok()?;
    if !install_path.exists() {
        return None;
    }

    let output = std::process::Command::new(&install_path)
        .arg("--version")
        .output()
        .ok()?;

    let version_output = String::from_utf8_lossy(&output.stdout);
    parse_installed_version(&version_output)
}

/// Parse nano-rs's own release version from its `--version` output.
///
/// The output has two version lines:
///   ` GNU nano, version 9.0.0`            (the upstream nano this port mirrors)
///   ` nano-rs 0.0.1 (Rust port) — <url>`  (the nano-rs release version)
/// The self-updater tracks the nano-rs version (it matches the release tags),
/// so we extract the token following "nano-rs". Returns None if not present.
fn parse_installed_version(version_output: &str) -> Option<String> {
    for line in version_output.lines() {
        let mut tokens = line.split_whitespace();
        while let Some(tok) = tokens.next() {
            if tok == "nano-rs" {
                if let Some(v) = tokens.next() {
                    // Only accept a version-looking token (starts with a digit),
                    // not e.g. "(Rust" if the line format changes.
                    if v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Install nano-rs to a PATH directory so it can be run from anywhere.
///
/// Cross-platform: copies the running executable to the managed location from
/// `get_install_path()` (creating the parent directory as needed) and marks it
/// executable on Unix.
pub fn install_to_path(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    let current_version = env!("CARGO_PKG_VERSION");
    let target_path = get_install_path()?;

    // Avoid copying a file onto itself (running the already-installed binary).
    if let (Ok(a), Ok(b)) = (fs::canonicalize(&current_exe), fs::canonicalize(&target_path)) {
        if a == b {
            println!("nano {} is already installed at this location.", current_version);
            println!("Location: {}", target_path.display());
            return Ok(());
        }
    }

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Check if already installed and compare versions (unless force)
    if target_path.exists() && !force {
        if let Some(installed_version) = get_installed_version() {
            if installed_version == current_version {
                println!("nano {} is already installed and up to date.", current_version);
                println!("Location: {}", target_path.display());
                println!("\nUse --force to reinstall anyway.");
                return Ok(())
            } else {
                println!("Updating nano from {} to {}...", installed_version, current_version);
            }
        } else {
            println!("Reinstalling nano {}...", current_version);
        }
    } else if force && target_path.exists() {
        println!("Force reinstalling nano {}...", current_version);
    } else {
        println!("Installing nano {} to PATH...", current_version);
    }

    // Copy the binary
    fs::copy(&current_exe, &target_path)?;
    set_executable(&target_path);

    println!("Successfully installed nano {}!", current_version);
    println!("Location: {}", target_path.display());
    println!("\nYou can now run 'nano' from any terminal.");
    #[cfg(not(windows))]
    println!("(Ensure {:?} is on your PATH.)", target_path.parent().unwrap_or(Path::new("~/.local/bin")));
    Ok(())
}

/// Parse version string to comparable tuple
fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = version.trim_start_matches('v').split('.').collect();
    if parts.len() >= 3 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    } else {
        None
    }
}

/// Compare two version strings, returns true if `a` is newer than `b`
pub fn is_newer_version(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(va), Some(vb)) => va > vb,
        _ => false,
    }
}

/// Helper struct to automatically close WinHTTP handles
#[cfg(windows)]
struct HandleGuard(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { let _ = WinHttpCloseHandle(self.0); }
        }
    }
}

/// Native HTTP GET using WinHTTP (no PowerShell, no extra deps)
#[cfg(windows)]
fn native_http_get(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use std::ffi::c_void;

    unsafe {
        // 1. Open Session
        let session = WinHttpOpen(
            w!("nano-rs-updater/1.0"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            None,
            None,
            0,
        );
        if session.is_null() {
            return Err(format!("WinHttpOpen failed: {:?}", GetLastError()).into());
        }
        let _session_guard = HandleGuard(session);

        // 2. Crack URL
        let mut host_name = vec![0u16; 256];
        let mut url_path = vec![0u16; 2048];

        let url_wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        let mut components = URL_COMPONENTS {
            dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
            dwHostNameLength: host_name.len() as u32,
            lpszHostName: PWSTR(host_name.as_mut_ptr()),
            dwUrlPathLength: url_path.len() as u32,
            lpszUrlPath: PWSTR(url_path.as_mut_ptr()),
            ..Default::default()
        };

        if WinHttpCrackUrl(&url_wide, 0, &mut components).is_err() {
             return Err(format!("WinHttpCrackUrl failed: {:?}", GetLastError()).into());
        }

        // 3. Connect
        let connect = WinHttpConnect(
            session,
            PCWSTR(components.lpszHostName.0),
            components.nPort,
            0,
        );
        if connect.is_null() {
            return Err(format!("WinHttpConnect failed: {:?}", GetLastError()).into());
        }
        let _connect_guard = HandleGuard(connect);

        // 4. Open Request
        let flags = if components.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = WinHttpOpenRequest(
            connect,
            w!("GET"),
            PCWSTR(components.lpszUrlPath.0),
            None,
            None,
            std::ptr::null(), // Accept types
            flags,
        );
        if request.is_null() {
            return Err(format!("WinHttpOpenRequest failed: {:?}", GetLastError()).into());
        }
        let _request_guard = HandleGuard(request);

        // 5. Send Request
        if WinHttpSendRequest(
            request,
            None,
            None,
            0,
            0,
            0,
        ).is_err() {
            return Err(format!("WinHttpSendRequest failed: {:?}", GetLastError()).into());
        }

        // 6. Receive Response
        if WinHttpReceiveResponse(request, std::ptr::null_mut()).is_err() {
            return Err(format!("WinHttpReceiveResponse failed: {:?}", GetLastError()).into());
        }

        // 6b. Check the HTTP status code. WinHTTP follows redirects by default, so this
        // is the FINAL status (e.g. after a GitHub asset URL redirects to its CDN).
        // Without this, a 404/403/5xx HTML error body would be read as "success" and,
        // for the asset download, written to disk and installed as the executable.
        let mut status_code: u32 = 0;
        let mut status_len = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(), // WINHTTP_HEADER_NAME_BY_INDEX
            Some(&mut status_code as *mut u32 as *mut c_void),
            &mut status_len,
            std::ptr::null_mut(), // WINHTTP_NO_HEADER_INDEX
        )
        .is_err()
        {
            return Err(format!("WinHttpQueryHeaders failed: {:?}", GetLastError()).into());
        }
        if !(200..300).contains(&status_code) {
            return Err(format!("HTTP request failed with status {}", status_code).into());
        }

        // 7. Read Data
        let mut body = Vec::new();
        let mut buffer = vec![0u8; 8192];
        let mut bytes_read = 0;

        loop {
            // Propagate read errors instead of returning a truncated body as Ok:
            // a mid-stream failure must NOT be reported as a complete download, or a
            // corrupt partial .exe could be installed over the working one.
            if WinHttpQueryDataAvailable(request, &mut bytes_read).is_err() {
                return Err(format!("WinHttpQueryDataAvailable failed: {:?}", GetLastError()).into());
            }
            if bytes_read == 0 {
                break;
            }

            let to_read = bytes_read.min(buffer.len() as u32);
            let mut read_now = 0;

            if WinHttpReadData(
                request,
                buffer.as_mut_ptr() as *mut c_void,
                to_read,
                &mut read_now,
            ).is_err() {
                return Err(format!("WinHttpReadData failed: {:?}", GetLastError()).into());
            }

            if read_now == 0 {
                break;
            }

            body.extend_from_slice(&buffer[..read_now as usize]);
        }

        Ok(body)
    }
}

/// HTTP GET on Unix via `curl` (falling back to `wget`).
///
/// Both tools follow redirects and FAIL on HTTP 4xx/5xx (curl `-f`, wget's
/// default), which gives us the same hardening WinHTTP's status-code check
/// provides: an error page is never returned as a successful body. The output
/// is captured as raw bytes, so binary assets download intact.
#[cfg(not(windows))]
fn native_http_get(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use std::process::Command;

    // curl -f (fail on HTTP error) -s (silent) -S (show error) -L (follow redirects)
    match Command::new("curl")
        .args(["-fsSL", "-A", USER_AGENT, url])
        .output()
    {
        Ok(out) if out.status.success() => return Ok(out.stdout),
        Ok(_) => { /* curl present but request failed; try wget */ }
        Err(_) => { /* curl not installed; try wget */ }
    }

    // wget -q (quiet) -O - (stdout); non-zero exit on server error by default.
    match Command::new("wget")
        .args(["-q", "-U", USER_AGENT, "-O", "-", url])
        .output()
    {
        Ok(out) if out.status.success() => Ok(out.stdout),
        Ok(_) => Err(format!("download failed (HTTP error fetching {})", url).into()),
        Err(_) => Err("could not download: neither `curl` nor `wget` is available on PATH".into()),
    }
}

/// The release asset name to look for, for the current OS + architecture.
/// Windows: nano-<arch>.exe   Unix: nano-linux-<arch>   (arch = amd64 | arm64)
fn target_asset_name() -> String {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
    if cfg!(windows) {
        format!("nano-{}.exe", arch)
    } else {
        format!("nano-linux-{}", arch)
    }
}

/// Whether `asset_url` is an acceptable fallback for this OS when the exact
/// arch-specific asset is not present.
fn is_os_asset_fallback(asset_url: &str) -> bool {
    if cfg!(windows) {
        asset_url.ends_with(".exe")
    } else {
        // Avoid grabbing a Windows .exe on Unix.
        asset_url.contains("nano-linux") && !asset_url.ends_with(".exe")
    }
}

/// Get the latest version info from GitHub
/// Returns (version, download_url) or None if check fails
pub fn get_latest_release() -> Result<(String, String), Box<dyn std::error::Error>> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);

    // Fetch JSON from GitHub API
    let body = native_http_get(&url)?;
    let json_text = String::from_utf8(body)
        .map_err(|_| "GitHub API returned invalid UTF-8")?;

    // Parse JSON manually to avoid complex deps
    // We look for "tag_name": "vX.Y.Z"
    let version = json_text.split("\"tag_name\"")
        .nth(1)
        .and_then(|s| s.split(':').nth(1))
        .and_then(|s| s.split("\"").nth(1))
        .ok_or_else(|| format!("Failed to parse tag_name from GitHub API response (body length: {} bytes)", json_text.len()))?
        .trim_start_matches('v')
        .to_string();

    let target_suffix = target_asset_name();

    // Find asset URL
    // Look for "browser_download_url": "..." that ends with target_suffix
    // Note: Can't split on ':' because URLs contain "https:"
    let mut download_url = String::new();
    for part in json_text.split("\"browser_download_url\"") {
        // Part starts with: ": "https://..." or similar
        // Extract the first quoted string after the colon-space separator
        if let Some(after_colon) = part.split_once(':') {
            // after_colon.1 is everything after the first ':', e.g. ' "https://...foo.exe",...'
            let rest = after_colon.1.trim();
            if rest.starts_with('"') {
                if let Some(url) = rest[1..].split('"').next() {
                    if url.ends_with(&target_suffix) {
                        download_url = url.to_string();
                        break;
                    }
                }
            }
        }
    }

    // Fallback: if specific arch not found, try any OS-appropriate asset.
    if download_url.is_empty() {
        for part in json_text.split("\"browser_download_url\"") {
            if let Some(after_colon) = part.split_once(':') {
                let rest = after_colon.1.trim();
                if rest.starts_with('"') {
                    if let Some(url) = rest[1..].split('"').next() {
                        if is_os_asset_fallback(url) {
                            download_url = url.to_string();
                            break;
                        }
                    }
                }
            }
        }
    }

    if version.is_empty() || download_url.is_empty() {
        return Err(format!("Could not find download URL for this platform (version={}, url_empty={})", version, download_url.is_empty()).into());
    }

    Ok((version, download_url))
}

/// Clean up any leftover temp files from previous updates
fn cleanup_temp_files() {
    if let Some(p) = update_temp_path() {
        let _ = fs::remove_file(p);
    }
}

/// Update nano-rs from GitHub releases (cross-platform).
pub fn update_from_github(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Clean up any old temp files from previous failed updates
    cleanup_temp_files();

    println!("Checking for updates...");

    let (latest_version, download_url) = match get_latest_release() {
        Ok(v) => v,
        Err(e) => return Err(format!("Failed to check for updates: {}", e).into()),
    };

    let current_version = env!("CARGO_PKG_VERSION");

    if !force && !is_newer_version(&latest_version, current_version) {
        println!("nano {} is already the latest version.", current_version);
        println!("\nUse --force to reinstall anyway.");
        return Ok(())
    }

    if force && !is_newer_version(&latest_version, current_version) {
        println!("Force reinstalling nano {} from GitHub...", latest_version);
    } else {
        println!("New version available: {} -> {}", current_version, latest_version);
    }
    println!("Downloading from GitHub...");

    // Download to temp file
    let temp_file = update_temp_path()
        .ok_or("could not determine a private cache directory (is HOME set?)")?;

    let body = native_http_get(&download_url)?;
    if body.is_empty() {
        return Err("Downloaded update file is empty".into());
    }
    fs::write(&temp_file, body)?;

    println!("Download complete. Installing...");

    // Install to the managed PATH location.
    do_install_update(&temp_file)
}

/// Install an update from a downloaded file into the managed PATH location.
pub fn do_install_update(update_file: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let target_path = get_install_path()?;

    // Ensure parent directory exists
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // If target exists, use the rename trick (both Windows and Unix allow
    // renaming/replacing a file that is currently being executed).
    if target_path.exists() {
        let backup_path = backup_path_for(&target_path);
        let _ = fs::remove_file(&backup_path); // Remove old backup if exists

        // Rename current binary to .old
        fs::rename(&target_path, &backup_path)?;

        // Copy new version
        if let Err(e) = fs::copy(update_file, &target_path) {
            // Failed - restore backup
            let _ = fs::rename(&backup_path, &target_path);
            return Err(e.into());
        }

        // Clean up backup - ignore errors as running process might lock it
        let _ = fs::remove_file(&backup_path);
    } else {
        // No existing file, just copy
        fs::copy(update_file, &target_path)?;
    }

    set_executable(&target_path);

    // Clean up temp file
    let _ = fs::remove_file(update_file);

    // Get version of newly installed binary
    let version = get_installed_version().unwrap_or_else(|| "unknown".to_string());

    println!("Successfully updated to nano {}!", version);
    println!("Location: {}", target_path.display());
    println!("\nRestart nano to use the new version.");
    Ok(())
}

/// Update status for background updates
#[derive(Clone)]
pub enum UpdateStatus {
    /// A newer version is available and has been downloaded
    Downloaded { version: String, path: PathBuf },
    /// No update available or error occurred
    None,
}

/// Check for updates and download if available (for background auto-update)
/// Returns UpdateStatus indicating what happened
pub fn check_and_download_update() -> UpdateStatus {
    // Respect the opt-in/opt-out policy (Unix is opt-in; see the fn).
    if !background_updates_enabled() {
        return UpdateStatus::None;
    }
    // Don't download what we can't install (and avoid re-notifying forever when
    // the binary lives in a non-writable location like /usr/bin).
    if !running_exe_replaceable() {
        return UpdateStatus::None;
    }

    let temp_file = match update_temp_path() {
        Some(p) => p,
        None => return UpdateStatus::None,
    };
    let pending = temp_file.exists()
        && fs::metadata(&temp_file).map(|m| m.len() > 0).unwrap_or(false);

    // Throttle network checks to once per day, but always surface an
    // already-downloaded pending update immediately.
    if !pending && checked_recently() {
        return UpdateStatus::None;
    }
    stamp_check();

    // If update already downloaded and pending, report it without re-downloading
    if temp_file.exists() {
        if let Ok(metadata) = fs::metadata(&temp_file) {
            if metadata.len() > 0 {
                // A non-empty pending update exists. Act only on a definitive answer
                // from GitHub: report it if newer, delete it only if CONFIRMED stale.
                // On a transient API failure, keep it — don't discard a valid,
                // already-downloaded update just because the check momentarily failed.
                match get_latest_release() {
                    Ok((latest_version, _)) => {
                        let current_version = env!("CARGO_PKG_VERSION");
                        if is_newer_version(&latest_version, current_version) {
                            return UpdateStatus::Downloaded {
                                version: latest_version,
                                path: temp_file,
                            };
                        }
                        // Confirmed not newer than current -- the pending file is stale.
                        let _ = fs::remove_file(&temp_file);
                    }
                    Err(_) => {
                        // Couldn't reach GitHub; preserve the pending update for retry.
                        return UpdateStatus::None;
                    }
                }
            } else {
                let _ = fs::remove_file(&temp_file);
            }
        }
    }

    let current_version = env!("CARGO_PKG_VERSION");

    let (latest_version, download_url) = match get_latest_release() {
        Ok(v) => v,
        Err(_) => return UpdateStatus::None,
    };

    if !is_newer_version(&latest_version, current_version) {
        return UpdateStatus::None;
    }

    match native_http_get(&download_url) {
        Ok(body) if !body.is_empty() => {
            if fs::write(&temp_file, body).is_ok() {
                UpdateStatus::Downloaded {
                    version: latest_version,
                    path: temp_file,
                }
            } else {
                UpdateStatus::None
            }
        },
        _ => UpdateStatus::None,
    }
}

/// Spawn a background thread to check and download updates
/// Returns a receiver that will receive the update status
pub fn spawn_update_check() -> std::sync::mpsc::Receiver<UpdateStatus> {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        // Small delay to not slow down startup
        std::thread::sleep(std::time::Duration::from_secs(3));
        let result = check_and_download_update();
        let _ = tx.send(result);
    });

    rx
}

/// Check for and apply a pending update on startup (call before the UI starts).
///
/// Cross-platform: replaces the currently-running executable using the rename
/// trick (rename the running file to `.old`, copy the new file into place). On
/// both Windows and Unix the running process keeps executing the old inode; the
/// new binary takes effect on the next launch. Returns true if an update was
/// applied or is pending.
pub fn apply_pending_update() -> bool {
    // Only ever trust a pending file from our private cache dir.
    let update_file = match update_temp_path() {
        Some(p) => p,
        None => return false,
    };

    // Get the currently running executable - this is what we need to update
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    if !update_file.exists() {
        // Clean up any old backup file from a previous update.
        let _ = fs::remove_file(backup_path_for(&current_exe));
        return false;
    }

    // Verify update file integrity
    if let Ok(metadata) = fs::metadata(&update_file) {
        if metadata.len() == 0 {
            let _ = fs::remove_file(&update_file);
            return false;
        }
    } else {
        return false;
    }

    // SECURITY (Unix): only trust an update file owned by the current user. The
    // file lives in our 0700 cache dir, but verify ownership too so we never
    // copy someone else's binary over our executable and mark it +x.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = fs::metadata(&update_file) {
            let myuid = unsafe { libc::geteuid() };
            if md.uid() != myuid {
                eprintln!("Ignoring pending update with unexpected owner.");
                let _ = fs::remove_file(&update_file);
                return false;
            }
        }
    }

    let install_path = current_exe;

    // If install path doesn't exist, just copy directly
    if !install_path.exists() {
        if fs::copy(&update_file, &install_path).is_ok() {
            set_executable(&install_path);
            let _ = fs::remove_file(&update_file);
            eprintln!("Update installed successfully!");
            return true;
        }
        return false;
    }

    // Rename current binary to .old (allowed while running on both platforms).
    let backup_path = backup_path_for(&install_path);
    let _ = fs::remove_file(&backup_path); // Remove old backup if exists

    if let Err(e) = fs::rename(&install_path, &backup_path) {
        // Can't rename - keep update file for retry on next restart
        eprintln!("Update pending (cannot rename running executable: {})", e);
        return true; // Return true to skip re-download
    }

    // Copy new version to install location
    if let Err(e) = fs::copy(&update_file, &install_path) {
        // Failed to copy, restore backup
        eprintln!("Update failed (copy error: {}), restoring backup", e);
        if let Err(e2) = fs::rename(&backup_path, &install_path) {
            eprintln!("CRITICAL: Failed to restore backup: {}. Working executable is at: {:?}", e2, backup_path);
        }
        // Keep update file for retry
        return true; // Return true to skip re-download
    }

    set_executable(&install_path);

    // Clean up update file ONLY on success
    let _ = fs::remove_file(&update_file);

    // Try to remove backup, but ignore error if locked (it's the running executable)
    let _ = fs::remove_file(&backup_path);

    eprintln!("Update applied successfully!");
    true
}
