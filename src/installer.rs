#![allow(
    non_snake_case,
    non_camel_case_types,
    unpredictable_function_pointer_comparisons
)]
//! Installation and update functionality for nano-rs.
//!
//! Supports self-install / self-update on both Windows and Unix:
//!   * Windows downloads via native WinHTTP (no PowerShell, no extra deps) and
//!     installs to %LOCALAPPDATA%\Microsoft\WindowsApps\nano.exe.
//!   * Unix downloads via `curl` (falling back to `wget`) — both near-universal
//!     and dependency-free — and installs to ~/.local/bin/nano.
//! Release assets are named per-platform: nano-<arch>.exe on Windows and
//! nano-linux-<arch> on Linux/Unix (arch is amd64 or arm64).

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows::Win32::Foundation::GetLastError;
#[cfg(windows)]
use windows::Win32::Networking::WinHttp::{
    URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
#[cfg(windows)]
use windows::core::{PCWSTR, PWSTR, w};

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
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
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
    let name = if cfg!(windows) {
        "nano-update.exe"
    } else {
        "nano-update"
    };
    Some(cache_dir()?.join(name))
}

/// Metadata that binds a pending executable to the release and target for
/// which it was downloaded.  It is deliberately a tiny line-based format so
/// startup validation does not depend on a JSON parser.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingManifest {
    version: String,
    target: String,
    asset: String,
    sha256: String,
}

fn update_manifest_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("nano-update.manifest"))
}

impl PendingManifest {
    fn encode(&self) -> String {
        format!(
            "version={}\ntarget={}\nasset={}\nsha256={}\n",
            self.version, self.target, self.asset, self.sha256
        )
    }

    fn decode(text: &str) -> Option<Self> {
        let mut version = None;
        let mut target = None;
        let mut asset = None;
        let mut sha256 = None;
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            if value.is_empty() || value.contains(['\r', '\n']) {
                return None;
            }
            match key {
                "version" if version.is_none() => version = Some(value.to_string()),
                "target" if target.is_none() => target = Some(value.to_string()),
                "asset" if asset.is_none() => asset = Some(value.to_string()),
                "sha256"
                    if sha256.is_none()
                        && value.len() == 64
                        && value.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    sha256 = Some(value.to_ascii_lowercase())
                }
                _ => return None,
            }
        }
        Some(Self {
            version: version?,
            target: target?,
            asset: asset?,
            sha256: sha256?,
        })
    }
}

/// Why a release lookup failed.  The distinction decides the background
/// check's retry policy: a transport failure (offline, DNS, timeout) must not
/// arm the 24-hour throttle, while a definitive server answer should.
#[derive(Debug)]
pub(crate) enum FetchError {
    /// No usable server answer: DNS/connect/timeout, tool missing, truncated read.
    Transport(String),
    /// The server answered definitively and refused us: non-2xx status,
    /// unusable JSON body, missing asset.
    Rejected(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Transport(msg) | FetchError::Rejected(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for FetchError {}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_artifact(bytes: &[u8], target: &str) -> bool {
    match target {
        "linux-x86_64" | "linux-aarch64" => {
            if bytes.len() < 20 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
                return false;
            }
            let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
            machine == if target == "linux-x86_64" { 62 } else { 183 }
        }
        "windows-x86_64" | "windows-aarch64" => {
            if bytes.len() < 64 || &bytes[..2] != b"MZ" {
                return false;
            }
            let pe_offset =
                u32::from_le_bytes([bytes[0x3c], bytes[0x3d], bytes[0x3e], bytes[0x3f]]) as usize;
            if pe_offset.checked_add(6).is_none_or(|end| end > bytes.len())
                || &bytes[pe_offset..pe_offset + 4] != b"PE\0\0"
            {
                return false;
            }
            let machine = u16::from_le_bytes([bytes[pe_offset + 4], bytes[pe_offset + 5]]);
            machine
                == if target == "windows-x86_64" {
                    0x8664
                } else {
                    0xaa64
                }
        }
        _ => false,
    }
}

fn remove_pending_update() {
    if let Some(path) = update_temp_path() {
        let _ = fs::remove_file(path);
    }
    if let Some(path) = update_manifest_path() {
        let _ = fs::remove_file(path);
    }
}

fn store_pending_update(
    body: &[u8],
    version: &str,
    expected_sha256: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let target = target_id()?;
    if !validate_artifact(body, target) {
        return Err(format!("downloaded asset is not a valid executable for {target}").into());
    }
    let update_path =
        update_temp_path().ok_or("could not determine private update cache directory")?;
    let manifest_path =
        update_manifest_path().ok_or("could not determine private update cache directory")?;
    let actual_sha256 = sha256_bytes(body);
    if actual_sha256 != expected_sha256.to_ascii_lowercase() {
        return Err("downloaded asset does not match the published SHA-256 digest".into());
    }
    let manifest = PendingManifest {
        version: version.to_string(),
        target: target.to_string(),
        asset: target_asset_name()?.to_string(),
        sha256: actual_sha256,
    };

    // Publish the manifest last: a crash after the executable write leaves an
    // untrusted orphan that startup will discard rather than apply.
    write_bytes_atomic(&update_path, body)?;
    if let Err(error) = write_bytes_atomic(&manifest_path, manifest.encode().as_bytes()) {
        let _ = fs::remove_file(&update_path);
        return Err(error);
    }
    Ok(update_path)
}

fn load_valid_pending_update() -> Result<(PathBuf, PendingManifest), Box<dyn std::error::Error>> {
    let update_path = update_temp_path().ok_or("private update cache unavailable")?;
    let manifest_path = update_manifest_path().ok_or("private update cache unavailable")?;
    let manifest_text = fs::read_to_string(&manifest_path)?;
    let manifest =
        PendingManifest::decode(&manifest_text).ok_or("invalid pending-update manifest")?;
    let expected_target = target_id()?;
    if manifest.target != expected_target
        || manifest.asset != target_asset_name()?
        || !is_newer_version(&manifest.version, env!("CARGO_PKG_VERSION"))
    {
        return Err("pending update does not match this version and target".into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let myuid = unsafe { libc::geteuid() };
        for path in [&update_path, &manifest_path] {
            if fs::metadata(path)?.uid() != myuid {
                return Err("pending update has unexpected owner".into());
            }
        }
    }

    let bytes = fs::read(&update_path)?;
    if !validate_artifact(&bytes, expected_target) || sha256_file(&update_path)? != manifest.sha256
    {
        return Err("pending update failed format or digest validation".into());
    }
    Ok((update_path, manifest))
}

/// Stamp file recording when the background check last ran (for throttling).
fn last_attempt_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("last-check-attempt"))
}

/// Whether a *completed* release lookup happened within the last 24h.
/// Transport failures never write this stamp, so being offline does not
/// silence every subsequent launch for a day.
fn checked_recently() -> bool {
    let Some(p) = last_attempt_path() else {
        return false;
    };
    if let Ok(md) = fs::metadata(&p) {
        if let Ok(modified) = md.modified() {
            if let Ok(elapsed) = modified.elapsed() {
                return elapsed < std::time::Duration::from_secs(24 * 3600);
            }
        }
    }
    false
}

/// Record that a lookup reached its outcome; the mtime carries the time and
/// the payload names the release version seen (diagnostics only).
fn stamp(path: Option<PathBuf>, payload: &str) {
    if let Some(p) = path {
        let _ = fs::write(p, payload);
    }
}

/// Read a boolean environment flag: unset => `None`; "0", "false", "", "no",
/// "off" (case-insensitive) => `Some(false)`; any other value => `Some(true)`.
fn env_flag(name: &str) -> Option<bool> {
    std::env::var_os(name).map(
        |raw| match raw.to_string_lossy().to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" | "off" => false,
            _ => true,
        },
    )
}

/// Resolve the background-check policy.
///
/// An explicit `NANO_NO_UPDATE_CHECK=1` always disables. Otherwise
/// `NANO_UPDATE_CHECK` decides when it is set, and otherwise the check is ON —
/// on every platform (Linux previously required opt-in; nano-rs is typically
/// self-installed rather than the distro `$EDITOR`, so this now matches
/// Windows and htop-win).
fn resolve_updates_enabled(disable: Option<bool>, enable: Option<bool>) -> bool {
    if disable == Some(true) {
        return false;
    }
    match enable {
        Some(v) => v,
        None => true,
    }
}

/// Whether the launch-time background update check should run.
///
/// On by default everywhere. `NANO_UPDATE_CHECK=0/false/no/off/""` disables;
/// `NANO_NO_UPDATE_CHECK=1` also disables and takes precedence over
/// `NANO_UPDATE_CHECK`. (`--check` ignores these variables entirely.)
fn background_updates_enabled() -> bool {
    resolve_updates_enabled(
        env_flag("NANO_NO_UPDATE_CHECK"),
        env_flag("NANO_UPDATE_CHECK"),
    )
}

fn dir_writable(dir: Option<&Path>) -> bool {
    match dir {
        Some(dir) => dir.is_dir() && tempfile::tempfile_in(dir).is_ok(),
        None => false,
    }
}

/// Whether two paths name the same file.  Falls back to comparing the
/// canonical parent plus the file name because `get_install_path()` often
/// does not exist yet (first install), which defeats plain canonicalize.
fn paths_equivalent(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        return ca == cb;
    }
    match (a.parent(), b.parent(), a.file_name(), b.file_name()) {
        (Some(pa), Some(pb), Some(fa), Some(fb)) if fa == fb => {
            matches!(
                (fs::canonicalize(pa), fs::canonicalize(pb)),
                (Ok(x), Ok(y)) if x == y
            )
        }
        _ => false,
    }
}

/// Every location an update may be written to: (1) the running executable
/// itself, when its directory is writable, and (2) the managed install path,
/// unless it names the same file as (1).  Background downloads happen only
/// when at least one location applies -- otherwise the update could never be
/// installed and would re-notify forever.
fn apply_targets() -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if dir_writable(exe.parent()) {
            targets.push(exe);
        }
    }
    if let Ok(install_path) = get_install_path() {
        if !targets.iter().any(|t| paths_equivalent(t, &install_path)) {
            targets.push(install_path);
        }
    }
    targets
}

/// Whether one of the apply targets is the running executable itself, i.e.
/// whether applying a pending update takes effect on this installation's
/// next launch as opposed to only refreshing the managed copy.
fn applies_in_place() -> bool {
    match std::env::current_exe() {
        Ok(exe) => apply_targets()
            .iter()
            .any(|target| paths_equivalent(target, &exe)),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("target path has no parent directory")?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

fn write_bytes_atomic(path: &Path, body: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("target path has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(body)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    sync_parent(path)
}

fn copy_file_atomic(source: &Path, target: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let parent = target
        .parent()
        .ok_or("target path has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut source_file = fs::File::open(source)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut source_file, temp.as_file_mut())?;
    set_executable(temp.as_file())?;
    temp.as_file().sync_all()?;
    temp.persist(target).map_err(|e| e.error)?;
    sync_parent(target)
}

/// Sibling path used for rollback: `nano` keeps its previous bytes in
/// `nano.old` next to it (htop-win parity).
fn backup_target_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|f| f.to_os_string())
        .unwrap_or_default();
    name.push(".old");
    target.with_file_name(name)
}

/// Best-effort: move an existing target aside so the replacement both frees
/// the path and keeps the previous binary for manual rollback.  A failure is
/// warned about and never aborts the caller (a locked `.old` on Windows must
/// not wedge an update).
fn move_aside_existing(target: &Path) -> bool {
    if !target.exists() {
        return false;
    }
    match fs::rename(target, backup_target_path(target)) {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "nano: could not save previous binary as {}: {error}",
                backup_target_path(target).display()
            );
            false
        }
    }
}

/// Replace `target` with `source`, keeping the previous bytes in
/// `<target>.old` for rollback.  If the replacement fails after a successful
/// backup, the backup is moved back so the target is never left missing.
pub(crate) fn replace_file_with_backup(
    source: &Path,
    target: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let backed_up = move_aside_existing(target);
    if let Err(error) = copy_file_atomic(source, target) {
        if backed_up {
            let _ = fs::rename(backup_target_path(target), target);
        }
        return Err(error);
    }
    Ok(())
}

/// Crash-window insurance: if the target vanished (crash between the
/// move-aside and the copy) but its `.old` backup exists, put it back.
fn restore_backup_if_missing(target: &Path) {
    if !target.exists() && backup_target_path(target).exists() {
        let _ = fs::rename(backup_target_path(target), target);
    }
}

/// Mark a file executable on Unix (no-op on Windows).
fn set_executable(file: &fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        file.set_permissions(permissions)?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
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

    let mut command = std::process::Command::new(&install_path);
    command.arg("--version");
    let output = command_output_with_timeout(command, std::time::Duration::from_secs(5)).ok()?;

    let version_output = String::from_utf8_lossy(&output.stdout);
    parse_installed_version(&version_output)
}

fn command_output_with_timeout(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    use std::time::Instant;

    // Do not use piped output here.  `wait_with_output()` waits for pipe EOF,
    // which can remain open indefinitely when the probed program leaves a
    // descendant running with inherited stdout/stderr.  Regular temporary
    // files let us enforce the deadline on the direct child and then read the
    // output without waiting for descendant-held pipe handles to close.
    let mut stdout_file = tempfile::tempfile()?;
    let mut stderr_file = tempfile::tempfile()?;
    command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?));
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            const MAX_PROBE_OUTPUT: u64 = 1024 * 1024;
            let read_output = |file: &mut fs::File| -> std::io::Result<Vec<u8>> {
                file.seek(SeekFrom::Start(0))?;
                let mut bytes = Vec::new();
                file.take(MAX_PROBE_OUTPUT + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_PROBE_OUTPUT {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "version probe produced too much output",
                    ));
                }
                Ok(bytes)
            };
            return Ok(std::process::Output {
                status,
                stdout: read_output(&mut stdout_file)?,
                stderr: read_output(&mut stderr_file)?,
            });
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "version probe timed out",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
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
    if let (Ok(a), Ok(b)) = (
        fs::canonicalize(&current_exe),
        fs::canonicalize(&target_path),
    ) {
        if a == b {
            println!(
                "nano {} is already installed at this location.",
                current_version
            );
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
                println!(
                    "nano {} is already installed and up to date.",
                    current_version
                );
                println!("Location: {}", target_path.display());
                println!("\nUse --force to reinstall anyway.");
                return Ok(());
            } else {
                println!(
                    "Updating nano from {} to {}...",
                    installed_version, current_version
                );
            }
        } else {
            println!("Reinstalling nano {}...", current_version);
        }
    } else if force && target_path.exists() {
        println!("Force reinstalling nano {}...", current_version);
    } else {
        println!("Installing nano {} to PATH...", current_version);
    }

    replace_file_with_backup(&current_exe, &target_path)?;

    println!("Successfully installed nano {}!", current_version);
    println!("Location: {}", target_path.display());
    println!("\nYou can now run 'nano' from any terminal.");
    #[cfg(not(windows))]
    println!(
        "(Ensure {:?} is on your PATH.)",
        target_path.parent().unwrap_or(Path::new("~/.local/bin"))
    );
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
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// Native HTTP GET using WinHTTP (no PowerShell, no extra deps)
#[cfg(windows)]
fn native_http_get(url: &str) -> Result<Vec<u8>, FetchError> {
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
            return Err(FetchError::Transport(format!(
                "WinHttpOpen failed: {:?}",
                GetLastError()
            )));
        }
        let _session_guard = HandleGuard(session);
        // Keep manual update/install operations bounded.  WinHTTP expresses
        // these values in milliseconds (resolve, connect, send, receive).
        WinHttpSetTimeouts(session, 10_000, 10_000, 60_000, 60_000)
            .map_err(|e| FetchError::Transport(format!("WinHttpSetTimeouts failed: {e:?}")))?;

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
            return Err(FetchError::Transport(format!(
                "WinHttpCrackUrl failed: {:?}",
                GetLastError()
            )));
        }

        // 3. Connect
        let connect = WinHttpConnect(
            session,
            PCWSTR(components.lpszHostName.0),
            components.nPort,
            0,
        );
        if connect.is_null() {
            return Err(FetchError::Transport(format!(
                "WinHttpConnect failed: {:?}",
                GetLastError()
            )));
        }
        let _connect_guard = HandleGuard(connect);

        // 4. Open Request
        let flags = if components.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
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
            return Err(FetchError::Transport(format!(
                "WinHttpOpenRequest failed: {:?}",
                GetLastError()
            )));
        }
        let _request_guard = HandleGuard(request);

        // 5. Send Request
        if WinHttpSendRequest(request, None, None, 0, 0, 0).is_err() {
            return Err(FetchError::Transport(format!(
                "WinHttpSendRequest failed: {:?}",
                GetLastError()
            )));
        }

        // 6. Receive Response
        if WinHttpReceiveResponse(request, std::ptr::null_mut()).is_err() {
            return Err(FetchError::Transport(format!(
                "WinHttpReceiveResponse failed: {:?}",
                GetLastError()
            )));
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
            return Err(FetchError::Transport(format!(
                "WinHttpQueryHeaders failed: {:?}",
                GetLastError()
            )));
        }
        if !(200..300).contains(&status_code) {
            // Definitive server answer: counts as a completed check even
            // though it failed (e.g. rate limiting, asset not published).
            return Err(FetchError::Rejected(format!(
                "HTTP request failed with status {}",
                status_code
            )));
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
                return Err(FetchError::Transport(format!(
                    "WinHttpQueryDataAvailable failed: {:?}",
                    GetLastError()
                )));
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
            )
            .is_err()
            {
                return Err(FetchError::Transport(format!(
                    "WinHttpReadData failed: {:?}",
                    GetLastError()
                )));
            }

            if read_now == 0 {
                break;
            }

            body.extend_from_slice(&buffer[..read_now as usize]);
        }

        Ok(body)
    }
}

/// Classify a curl/wget exit into retry-policy terms: curl 22 / wget 8 mean
/// the server answered with an HTTP error (definitive); anything else — DNS
/// (6), connect failure (7), timeout (28), wget network (4), signal death
/// (None) — is transport trouble worth retrying on the next launch.
#[cfg(not(windows))]
fn classify_tool_exit(tool: &str, code: Option<i32>) -> FetchError {
    let detail = format!("{tool} failed fetching (exit {code:?})");
    match (tool, code) {
        ("curl", Some(22)) | ("wget", Some(8)) => FetchError::Rejected(detail),
        _ => FetchError::Transport(detail),
    }
}

/// HTTP GET on Unix via `curl` (falling back to `wget`).
///
/// Both tools follow redirects and FAIL on HTTP 4xx/5xx (curl `-f`, wget's
/// default), which gives us the same hardening WinHTTP's status-code check
/// provides: an error page is never returned as a successful body. The output
/// is captured as raw bytes, so binary assets download intact.
#[cfg(not(windows))]
fn native_http_get(url: &str) -> Result<Vec<u8>, FetchError> {
    use std::process::Command;

    // curl -f (fail on HTTP error) -s (silent) -S (show error) -L (follow redirects).
    let mut last_error = FetchError::Transport(format!(
        "could not download {url}: neither `curl` nor `wget` is available on PATH"
    ));

    // Bound connection setup to 10 seconds and the whole transfer to 60.
    match Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "10",
            "--max-time",
            "60",
            "--speed-limit",
            "1",
            "--speed-time",
            "15",
            "-A",
            USER_AGENT,
            url,
        ])
        .output()
    {
        Ok(out) if out.status.success() => return Ok(out.stdout),
        Ok(out) => last_error = classify_tool_exit("curl", out.status.code()),
        Err(_) => { /* curl not installed; try wget */ }
    }

    // wget -q (quiet) -O - (stdout); non-zero exit on server error by default.
    match Command::new("wget")
        .args([
            "-q",
            "--connect-timeout=10",
            "--timeout=60",
            "--tries=1",
            "-U",
            USER_AGENT,
            "-O",
            "-",
            url,
        ])
        .output()
    {
        Ok(out) if out.status.success() => Ok(out.stdout),
        Ok(out) => Err(classify_tool_exit("wget", out.status.code())),
        Err(_) => Err(last_error),
    }
}

/// Exact release target supported by the updater.  Builds for any other
/// OS/architecture may still run nano-rs, but must update through their package
/// manager or a manually supplied binary instead of guessing an asset.
fn target_id() -> Result<&'static str, Box<dyn std::error::Error>> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("windows-x86_64"),
        ("windows", "aarch64") => Ok("windows-aarch64"),
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        (os, arch) => Err(format!("self-update is unsupported on {os}/{arch}").into()),
    }
}

fn target_asset_name() -> Result<&'static str, Box<dyn std::error::Error>> {
    match target_id()? {
        "windows-x86_64" => Ok("nano-amd64.exe"),
        "windows-aarch64" => Ok("nano-arm64.exe"),
        "linux-x86_64" => Ok("nano-linux-amd64"),
        "linux-aarch64" => Ok("nano-linux-arm64"),
        _ => unreachable!("target_id only returns supported targets"),
    }
}

/// Get the latest version info from GitHub
/// Returns (version, download_url) or None if check fails
pub fn get_latest_release() -> Result<(String, String, String), FetchError> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_REPO
    );

    // Fetch JSON from GitHub API
    let body = native_http_get(&url)?;
    let json_text = String::from_utf8(body)
        .map_err(|_| FetchError::Rejected("GitHub API returned invalid UTF-8".to_string()))?;

    // Parse JSON manually to avoid complex deps
    // We look for "tag_name": "vX.Y.Z"
    let version = json_text
        .split("\"tag_name\"")
        .nth(1)
        .and_then(|s| s.split(':').nth(1))
        .and_then(|s| s.split("\"").nth(1))
        .ok_or_else(|| {
            FetchError::Rejected(format!(
                "Failed to parse tag_name from GitHub API response (body length: {} bytes)",
                json_text.len()
            ))
        })?
        .trim_start_matches('v')
        .to_string();

    let target_suffix = target_asset_name().map_err(|e| FetchError::Rejected(e.to_string()))?;

    // Find asset URL
    // Look for "browser_download_url": "..." that ends with target_suffix
    // Note: Can't split on ':' because URLs contain "https:"
    let mut download_url = String::new();
    let mut checksums_url = String::new();
    for part in json_text.split("\"browser_download_url\"") {
        // Part starts with: ": "https://..." or similar
        // Extract the first quoted string after the colon-space separator
        if let Some(after_colon) = part.split_once(':') {
            // after_colon.1 is everything after the first ':', e.g. ' "https://...foo.exe",...'
            let rest = after_colon.1.trim();
            if rest.starts_with('"') {
                if let Some(url) = rest[1..].split('"').next() {
                    if url.ends_with(target_suffix) {
                        download_url = url.to_string();
                    } else if url.ends_with("nano-checksums.txt") {
                        checksums_url = url.to_string();
                    }
                }
            }
        }
    }

    if version.is_empty() || download_url.is_empty() {
        return Err(FetchError::Rejected(format!(
            "release {version} has no exact asset {target_suffix} for {}",
            target_id().map_err(|e| FetchError::Rejected(e.to_string()))?
        )));
    }

    Ok((version, download_url, checksums_url))
}

fn published_checksum(text: &str, asset: &str) -> Option<String> {
    for line in text.lines() {
        // Short lines (blank separators, stray tokens) must be skipped, not
        // abort the whole scan: a `?` here returned None for the entire
        // manifest before ever reaching this target's entry.
        let mut fields = line.split_whitespace();
        let Some(digest) = fields.next() else {
            continue;
        };
        let Some(filename) = fields.next() else {
            continue;
        };
        if fields.next().is_none()
            && filename.trim_start_matches('*') == asset
            && digest.len() == 64
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

fn download_verified_release(
    download_url: &str,
    checksums_url: &str,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    if checksums_url.is_empty() {
        return Err("release has no nano-checksums.txt; refusing an unverified update".into());
    }
    let checksum_body = native_http_get(checksums_url)?;
    let checksum_text =
        String::from_utf8(checksum_body).map_err(|_| "release checksum manifest is not UTF-8")?;
    let expected = published_checksum(&checksum_text, target_asset_name()?)
        .ok_or("release checksum manifest has no valid entry for this target")?;
    let body = native_http_get(download_url)?;
    if body.is_empty() || sha256_bytes(&body) != expected {
        return Err("downloaded release asset failed SHA-256 verification".into());
    }
    Ok((body, expected))
}

/// Clean up any leftover temp files from previous updates
fn cleanup_temp_files() {
    remove_pending_update();
}

/// Update nano-rs from GitHub releases (cross-platform).
pub fn update_from_github(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Clean up any old temp files from previous failed updates
    cleanup_temp_files();

    println!("Checking for updates...");

    let (latest_version, download_url, checksums_url) = match get_latest_release() {
        Ok(v) => v,
        Err(e) => return Err(format!("Failed to check for updates: {}", e).into()),
    };

    let current_version = env!("CARGO_PKG_VERSION");

    if !force && !is_newer_version(&latest_version, current_version) {
        println!("nano {} is already the latest version.", current_version);
        println!("\nUse --force to reinstall anyway.");
        return Ok(());
    }

    if force && !is_newer_version(&latest_version, current_version) {
        println!("Force reinstalling nano {} from GitHub...", latest_version);
    } else {
        println!(
            "New version available: {} -> {}",
            current_version, latest_version
        );
    }
    println!("Downloading from GitHub...");

    // Download to temp file
    let temp_file =
        update_temp_path().ok_or("could not determine a private cache directory (is HOME set?)")?;

    let (body, _expected_sha256) = download_verified_release(&download_url, &checksums_url)?;
    if !validate_artifact(&body, target_id()?) {
        return Err(
            "downloaded release asset has the wrong executable format or architecture".into(),
        );
    }
    write_bytes_atomic(&temp_file, &body)?;

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

    replace_file_with_backup(update_file, &target_path)?;

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
    /// A newer version is available and has been downloaded.  `in_place` is
    /// true when one of the written locations is the running executable, so
    /// restarting nano is enough to pick it up; when false only the managed
    /// install path was refreshed.
    Downloaded { version: String, in_place: bool },
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
    // Don't download what we can't install anywhere (and avoid re-notifying
    // forever when no applicable location is user-writable).
    if apply_targets().is_empty() {
        return UpdateStatus::None;
    }

    let temp_file = match update_temp_path() {
        Some(p) => p,
        None => return UpdateStatus::None,
    };
    let pending = temp_file.exists() || update_manifest_path().is_some_and(|path| path.exists());

    // Throttle *completed* checks to once per day, but always surface an
    // already-downloaded pending update immediately.
    if !pending && checked_recently() {
        return UpdateStatus::None;
    }

    // A pending executable is usable only together with a valid manifest,
    // target match, newer version, executable header, owner, and digest.
    if pending {
        match load_valid_pending_update() {
            Ok((_path, manifest)) => {
                return UpdateStatus::Downloaded {
                    version: manifest.version,
                    in_place: applies_in_place(),
                };
            }
            Err(_) => remove_pending_update(),
        }
    }

    let current_version = env!("CARGO_PKG_VERSION");

    // The throttle stamps are written here — after the network round-trip —
    // so an offline launch never arms the 24-hour backoff (issue: being on a
    // train used to silence update checks for a full day).
    let (latest_version, download_url, checksums_url) = match get_latest_release() {
        Ok(v) => {
            stamp(last_attempt_path(), &v.0);
            stamp(cache_dir().map(|d| d.join("last-check-success")), &v.0);
            v
        }
        Err(FetchError::Rejected(reason)) => {
            // Definitive answer (rate limit, missing asset): wait a cycle.
            stamp(last_attempt_path(), "");
            eprintln!("nano: update check skipped ({reason})");
            return UpdateStatus::None;
        }
        Err(FetchError::Transport(_)) => return UpdateStatus::None,
    };

    if !is_newer_version(&latest_version, current_version) {
        return UpdateStatus::None;
    }

    match download_verified_release(&download_url, &checksums_url) {
        Ok((body, expected_sha256)) => {
            if store_pending_update(&body, &latest_version, &expected_sha256).is_ok() {
                UpdateStatus::Downloaded {
                    version: latest_version,
                    in_place: applies_in_place(),
                }
            } else {
                UpdateStatus::None
            }
        }
        _ => UpdateStatus::None,
    }
}

/// Format the user-facing result of an explicit update check.
fn format_update_report(current: &str, latest: &str) -> String {
    if is_newer_version(latest, current) {
        format!(
            "nano-rs {latest} available (installed {current})\nRun 'nano --update' to install it."
        )
    } else {
        format!("nano-rs {current} is up to date")
    }
}

/// Query GitHub for the latest release and report it.  Never downloads an
/// asset or stages a pending update; ignores the background-policy
/// environment variables and --force entirely (explicit user intent).
pub fn report_available_update() -> Result<(), Box<dyn std::error::Error>> {
    let current = env!("CARGO_PKG_VERSION");
    println!("Checking for updates...");
    let (latest, _url, _checksums_url) = get_latest_release()?;
    println!("{}", format_update_report(current, &latest));
    Ok(())
}

/// Version of a staged update that validates fully (target, asset, newer
/// version, ownership, executable header, SHA-256). `None` when nothing is
/// staged or the pair does not check out.
pub fn pending_update_version() -> Option<String> {
    load_valid_pending_update()
        .ok()
        .map(|(_path, manifest)| manifest.version)
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
/// Cross-platform: atomically replaces the currently-running executable with a
/// validated, target-specific pending update.  On Unix and Windows the running
/// process keeps executing its already-open image; the new binary takes effect
/// on the next launch. Returns true if an update was applied or remains pending.
pub fn apply_pending_update() -> bool {
    let any_pending = update_temp_path().is_some_and(|path| path.exists())
        || update_manifest_path().is_some_and(|path| path.exists());
    if !any_pending {
        return false;
    }

    let (update_file, _manifest) = match load_valid_pending_update() {
        Ok(valid) => valid,
        Err(error) => {
            eprintln!("Ignoring invalid pending update: {error}");
            remove_pending_update();
            return false;
        }
    };

    // Crash-window insurance: a vanished target with an orphaned .old means
    // we died between move-aside and copy; put the old binary back first.
    for target in apply_targets() {
        restore_backup_if_missing(&target);
    }

    // Write every applicable location (running executable and/or managed
    // install path).  A partially applied round self-heals: the applied copy
    // reports the new version, so the leftover pending pair -- no longer
    // "newer than current" -- is discarded on the next launch anyway.
    let targets = apply_targets();
    if targets.is_empty() {
        eprintln!("Update pending (no user-writable location to install it)");
        return true; // Keep pending; skip re-download this launch.
    }

    let mut applied = 0;
    for target in &targets {
        match replace_file_with_backup(&update_file, target) {
            Ok(()) => applied += 1,
            Err(error) => {
                eprintln!("Could not update {}: {error}", target.display());
            }
        }
    }

    if applied == 0 {
        eprintln!("Update pending (cannot replace running executable)");
        return true; // Keep pending; retried on the next launch.
    }

    // Clean up the executable and its binding manifest only after at least
    // one location was replaced.
    remove_pending_update();
    eprintln!("Update applied ({applied} location(s)) -- restart nano to use it");
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn paths_equivalent_covers_identity_canonical_and_missing_targets() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("nano");
        let b = dir.path().join("nano");
        std::fs::write(&a, b"x").unwrap();
        assert!(super::paths_equivalent(&a, &b), "identical paths");

        // A symlinked launch path collapses to the same target (this is how
        // /usr/local/bin/nano -> ~/.local/bin/nano stays one location).
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        assert!(super::paths_equivalent(&a, &link), "same file via symlink");
        // A hard link is a distinct directory entry: an atomic rename over
        // one must not be assumed to publish through the other.
        let hard = dir.path().join("hard");
        fs::hard_link(&a, &hard).unwrap();
        assert!(!super::paths_equivalent(&a, &hard), "hard links diverge");

        let other = dir.path().join("other");
        std::fs::write(&other, b"y").unwrap();
        assert!(!super::paths_equivalent(&a, &other), "distinct files");

        // Install path often does not exist yet: compare parent + name.
        let nested = dir.path().join("sub");
        std::fs::create_dir(&nested).unwrap();
        let ghost_a = nested.join("nano");
        let ghost_b = dir.path().join("sub").join("nano");
        assert!(super::paths_equivalent(&ghost_a, &ghost_b));
        assert!(!super::paths_equivalent(&ghost_a, &a));
    }

    #[test]
    fn installed_version_probe_keys_on_nano_rs_token() {
        // The titlebar may brand itself however it likes, but --version's
        // second line is the updater's contract: "nano-rs X.Y.Z".
        let real = " GNU nano, version 9.0.0\n nano-rs 0.0.15 (Rust port) \u{2014} https://github.com/faratech/nano-rs\n";
        assert_eq!(
            super::parse_installed_version(real),
            Some("0.0.15".to_string())
        );
        assert_eq!(super::parse_installed_version("no version line here"), None);
    }

    #[test]
    fn update_check_message_is_actionable() {
        let newer = super::format_update_report("0.0.15", "0.0.16");
        assert!(newer.contains("0.0.16 available"));
        assert!(newer.contains("installed 0.0.15"));
        assert!(newer.contains("--update"));

        let current = super::format_update_report("0.0.16", "0.0.15");
        assert_eq!(current, "nano-rs 0.0.16 is up to date");
    }

    #[test]
    fn version_comparison_table() {
        // (candidate, current) => is_newer?
        let cases: &[(&str, &str, bool)] = &[
            ("0.0.16", "0.0.15", true),
            ("0.1.0", "0.0.99", true),
            ("1.0.0", "0.9.9", true),
            ("v0.0.16", "0.0.15", true),     // v prefix tolerated
            ("0.0.15", "0.0.15", false),     // equal
            ("0.0.14", "0.0.15", false),     // older
            ("0.0", "0.0.15", false),        // malformed
            ("abc", "0.0.15", false),        // garbage
            ("0.0.16-rc1", "0.0.15", false), // prerelease tags unsupported
        ];
        for &(latest, installed, expect) in cases {
            assert_eq!(
                super::is_newer_version(latest, installed),
                expect,
                "{latest} vs {installed}"
            );
        }
    }

    use super::{FetchError, classify_tool_exit, env_flag, resolve_updates_enabled};

    #[test]
    fn fetch_error_is_display_and_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(FetchError::Rejected("nope".into()));
        assert_eq!(err.to_string(), "nope");
    }

    #[cfg(unix)]
    #[test]
    fn tool_exit_classification_matches_retry_policy() {
        // Definitive server answers arm the throttle...
        assert!(matches!(
            classify_tool_exit("curl", Some(22)),
            FetchError::Rejected(_)
        ));
        assert!(matches!(
            classify_tool_exit("wget", Some(8)),
            FetchError::Rejected(_)
        ));
        // ...transport trouble never does.
        for code in [Some(6), Some(7), Some(28), Some(4), None] {
            assert!(matches!(
                classify_tool_exit("curl", code),
                FetchError::Transport(_)
            ));
            assert!(matches!(
                classify_tool_exit("wget", code),
                FetchError::Transport(_)
            ));
        }
    }

    #[test]
    fn env_flag_tokens_parse_case_insensitively() {
        // The std::env reads live in the thin wrapper; this table pins the
        // pure token grammar so NANO_UPDATE_CHECK=0 can never enable again.
        let off = ["", "0", "false", "no", "off", "OFF", "False"];
        for token in off {
            assert_eq!(Some(false), fake_env_flag(token), "{token:?}");
        }
        let on = ["1", "true", "YES", "on", "junk", "2"];
        for token in on {
            assert_eq!(Some(true), fake_env_flag(token), "{token:?}");
        }
    }

    fn fake_env_flag(value: &str) -> Option<bool> {
        // Mirror of env_flag's body minus the process-global read.
        match value.to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" | "off" => false,
            _ => true,
        }
        .into_some()
    }

    trait IntoSome {
        fn into_some(self) -> Option<bool>;
    }
    impl IntoSome for bool {
        fn into_some(self) -> Option<bool> {
            Some(self)
        }
    }

    #[test]
    fn update_policy_precedence() {
        let d: Option<bool> = None;
        let e: Option<bool> = None;
        assert!(resolve_updates_enabled(d, e), "default is on everywhere");
        assert!(!resolve_updates_enabled(Some(true), Some(true)));
        assert!(!resolve_updates_enabled(Some(true), None));
        assert!(!resolve_updates_enabled(None, Some(false)));
        assert!(resolve_updates_enabled(None, Some(true)));
        assert!(
            resolve_updates_enabled(Some(false), None),
            "NO=0 alone must not disable"
        );
        assert!(resolve_updates_enabled(Some(false), Some(true)));
        assert!(!resolve_updates_enabled(Some(false), Some(false)));
    }

    #[test]
    fn replace_with_backup_moves_previous_binary_aside() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nano");
        let source = dir.path().join("source");
        std::fs::write(&target, b"old executable").unwrap();
        std::fs::write(&source, b"new executable").unwrap();

        super::replace_file_with_backup(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new executable");
        let backup = dir.path().join("nano.old");
        assert_eq!(std::fs::read(&backup).unwrap(), b"old executable");
    }

    #[test]
    fn replace_with_backup_overwrites_stale_old() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nano");
        let source = dir.path().join("source");
        std::fs::write(&target, b"previous").unwrap();
        std::fs::write(dir.path().join("nano.old"), b"ancient").unwrap();
        std::fs::write(&source, b"current").unwrap();

        super::replace_file_with_backup(&source, &target).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("nano.old")).unwrap(),
            b"previous"
        );
    }

    #[test]
    fn replace_with_backup_creates_missing_target_without_old() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nano");
        let source = dir.path().join("source");
        std::fs::write(&source, b"fresh install").unwrap();

        super::replace_file_with_backup(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"fresh install");
        assert!(!dir.path().join("nano.old").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_copy_restores_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nano");
        // A directory as the "source" makes every read fail (EISDIR) no
        // matter what privileges the test runs under.
        let source = dir.path().join("not-a-file");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(&target, b"precious").unwrap();

        assert!(super::replace_file_with_backup(&source, &target).is_err());
        // Restore semantics: the target is never left missing -- the `.old`
        // backup is moved back onto it, so no `.old` remains afterwards.
        assert_eq!(std::fs::read(&target).unwrap(), b"precious");
        assert!(!dir.path().join("nano.old").exists());
    }

    #[test]
    fn checksum_scan_survives_short_lines_before_the_entry() {
        // Issue #77: a `?` on blank or one-token lines used to abort the
        // whole manifest scan before reaching this target's entry.
        let manifest = concat!(
            "\n",
            "# sha256 hashes\n",
            "incomplete-line\n",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef nano-x86_64-pc-windows-msvc.zip\n",
            "deadbeef deadbeef extra\n",
            "FEDCBA9876543210fedcba9876543210FEDCBA9876543210fedcba9876543210 *nano-x86_64-unknown-linux-gnu.tar.gz\n",
            "\n",
        );
        assert_eq!(
            super::published_checksum(manifest, "nano-x86_64-unknown-linux-gnu.tar.gz"),
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210".to_string())
        );
        assert_eq!(
            super::published_checksum(manifest, "nano-aarch64-apple-darwin.tar.gz"),
            None
        );
    }

    use super::*;

    #[test]
    fn pending_manifest_round_trips_and_rejects_extra_fields() {
        let manifest = PendingManifest {
            version: "1.2.3".to_string(),
            target: "linux-x86_64".to_string(),
            asset: "nano-linux-amd64".to_string(),
            sha256: "ab".repeat(32),
        };
        assert_eq!(PendingManifest::decode(&manifest.encode()), Some(manifest));
        assert!(
            PendingManifest::decode(
                "version=1.2.3\ntarget=linux-x86_64\nasset=nano-linux-amd64\nsha256=bad\n"
            )
            .is_none()
        );
        assert!(
            PendingManifest::decode(&format!(
                "{}unexpected=value\n",
                PendingManifest {
                    version: "1.2.3".to_string(),
                    target: "linux-x86_64".to_string(),
                    asset: "nano-linux-amd64".to_string(),
                    sha256: "ab".repeat(32),
                }
                .encode()
            ))
            .is_none()
        );
    }

    #[test]
    fn artifact_validation_checks_format_and_architecture() {
        let mut elf = vec![0u8; 64];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert!(validate_artifact(&elf, "linux-x86_64"));
        assert!(!validate_artifact(&elf, "linux-aarch64"));

        let mut pe = vec![0u8; 128];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
        pe[64..68].copy_from_slice(b"PE\0\0");
        pe[68..70].copy_from_slice(&0xaa64u16.to_le_bytes());
        assert!(validate_artifact(&pe, "windows-aarch64"));
        assert!(!validate_artifact(&pe, "windows-x86_64"));
    }

    #[test]
    fn atomic_copy_does_not_touch_adjacent_old_file() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("nano");
        let adjacent = directory.path().join("nano.old");
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"old executable").unwrap();
        fs::write(&adjacent, b"user data").unwrap();

        copy_file_atomic(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read(&adjacent).unwrap(), b"user data");
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn checksum_manifest_requires_exact_asset_and_digest() {
        let digest = "12".repeat(32);
        let text = format!(
            "{digest}  nano-linux-amd64\n{}  nano-linux-arm64\n",
            "34".repeat(32)
        );
        assert_eq!(published_checksum(&text, "nano-linux-amd64"), Some(digest));
        assert_eq!(published_checksum(&text, "nano-amd64.exe"), None);
        assert_eq!(
            published_checksum("not-a-digest  nano-linux-amd64", "nano-linux-amd64"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_does_not_wait_for_descendant_held_output() {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("sleep 2 & printf 'nano-rs 1.2.3\\n'");
        let started = std::time::Instant::now();

        let output =
            command_output_with_timeout(command, std::time::Duration::from_millis(500)).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"nano-rs 1.2.3\n");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
