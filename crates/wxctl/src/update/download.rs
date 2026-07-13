//! Self-update engine: resolve the host target, download the release archive +
//! SHA256SUMS, verify the checksum exactly as `install.sh` does, extract the
//! binary member, and install it. The binary swap sits behind an install seam:
//! `self-replace` against the running binary by default, or a direct write to
//! `WXCTL_UPDATE_INSTALL_PATH` (test-only) so the E2E never replaces the test
//! binary. Outbound hosts: only the release download host (AC 9 / invariant I1);
//! never `api.github.com`.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Public release-asset host (carve-out invariant I1: hardcoded, overridable
/// only by the documented `WXCTL_RELEASE_BASE_URL` test/dev env var).
const DEFAULT_RELEASE_BASE: &str = "https://github.com/randyphoa/wxctl/releases/download";

/// Manual-download fallback shown when no prebuilt asset applies.
pub const RELEASES_PAGE: &str = "https://github.com/randyphoa/wxctl/releases";

/// Upper bound on a downloaded archive (guards against a runaway body).
const MAX_ARCHIVE_BYTES: u64 = 100 * 1024 * 1024;

/// Typed self-update failure; `commands/update.rs` maps each to user guidance.
#[derive(Debug)]
pub enum UpdateError {
    UnsupportedPlatform { os: &'static str, arch: &'static str },
    NotWritable(PathBuf),
    Download(String),
    ChecksumMismatch { archive: String },
    ChecksumMissing { archive: String },
    Extract(String),
    Io(std::io::Error),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::UnsupportedPlatform { os, arch } => write!(f, "no prebuilt wxctl binary for {os}/{arch}"),
            UpdateError::NotWritable(p) => write!(f, "install directory is not writable: {}", p.display()),
            UpdateError::Download(m) => write!(f, "download failed: {m}"),
            UpdateError::ChecksumMismatch { archive } => write!(f, "checksum mismatch for {archive} — refusing to install"),
            UpdateError::ChecksumMissing { archive } => write!(f, "no SHA256SUMS entry for {archive} — refusing to install"),
            UpdateError::Extract(m) => write!(f, "could not extract the wxctl binary from the archive: {m}"),
            UpdateError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for UpdateError {}

/// Map the host OS/arch to one of the six `release.yml` target triples; `None`
/// on a genuinely unsupported combination.
pub fn target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => return None,
    })
}

/// Archive extension for the host OS (no leading dot).
#[cfg(unix)]
pub fn archive_ext() -> &'static str {
    "tar.gz"
}
#[cfg(windows)]
pub fn archive_ext() -> &'static str {
    "zip"
}

/// The binary member name inside the archive for the host OS.
#[cfg(unix)]
pub fn binary_member() -> &'static str {
    "wxctl"
}
#[cfg(windows)]
pub fn binary_member() -> &'static str {
    "wxctl.exe"
}

/// Release base URL: `WXCTL_RELEASE_BASE_URL` override (test/dev) else the const.
pub fn release_base() -> String {
    std::env::var("WXCTL_RELEASE_BASE_URL").unwrap_or_else(|_| DEFAULT_RELEASE_BASE.to_string())
}

/// `wxctl/<version>` User-Agent for asset requests.
fn user_agent() -> String {
    format!("wxctl/{}", env!("CARGO_PKG_VERSION"))
}

/// GET the archive + `SHA256SUMS` for `version`/`target`, verify the sha256
/// exactly as `install.sh` does, and return the archive bytes. A mismatch or a
/// missing `SHA256SUMS` line aborts **before** returning any bytes. `version`
/// may be bare (`0.2.0`) or `v`-prefixed; the asset path uses `v<version>`.
pub fn download_and_verify(version: &str, target: &str) -> Result<Vec<u8>, UpdateError> {
    let tag = format!("v{}", version.trim_start_matches('v'));
    let archive = format!("wxctl-{tag}-{target}.{}", archive_ext());
    let base = release_base();
    let archive_url = format!("{base}/{tag}/{archive}");
    let sums_url = format!("{base}/{tag}/SHA256SUMS");

    let bytes = http_get_bytes(&archive_url)?;
    let sums = http_get_string(&sums_url)?;

    // SHA256SUMS line format (from `sha256sum wxctl-* > SHA256SUMS`): "<hash>  <filename>".
    let want = sums
        .lines()
        .find_map(|line| {
            let mut it = line.split_whitespace();
            let hash = it.next()?;
            let name = it.next()?;
            (name == archive).then(|| hash.to_string())
        })
        .ok_or_else(|| UpdateError::ChecksumMissing { archive: archive.clone() })?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != want {
        return Err(UpdateError::ChecksumMismatch { archive });
    }
    Ok(bytes)
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>, UpdateError> {
    let ua = user_agent();
    let mut resp = ureq::get(url).header("User-Agent", ua.as_str()).call().map_err(|e| UpdateError::Download(format!("{url}: {e}")))?;
    resp.body_mut().with_config().limit(MAX_ARCHIVE_BYTES).read_to_vec().map_err(|e| UpdateError::Download(format!("{url}: {e}")))
}

fn http_get_string(url: &str) -> Result<String, UpdateError> {
    let ua = user_agent();
    let mut resp = ureq::get(url).header("User-Agent", ua.as_str()).call().map_err(|e| UpdateError::Download(format!("{url}: {e}")))?;
    resp.body_mut().read_to_string().map_err(|e| UpdateError::Download(format!("{url}: {e}")))
}

/// Return the `binary_member()` bytes from a `.tar.gz` archive (unix).
#[cfg(unix)]
pub fn extract_binary(archive: &[u8]) -> Result<Vec<u8>, UpdateError> {
    use flate2::read::GzDecoder;
    let member = binary_member();
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    for entry in tar.entries().map_err(|e| UpdateError::Extract(e.to_string()))? {
        let mut entry = entry.map_err(|e| UpdateError::Extract(e.to_string()))?;
        let is_member = entry.path().ok().and_then(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s == member)).unwrap_or(false);
        if is_member {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(|e| UpdateError::Extract(e.to_string()))?;
            return Ok(buf);
        }
    }
    Err(UpdateError::Extract(format!("no `{member}` entry in archive")))
}

/// Return the `binary_member()` bytes from a `.zip` archive (windows).
#[cfg(windows)]
pub fn extract_binary(archive: &[u8]) -> Result<Vec<u8>, UpdateError> {
    let member = binary_member();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|e| UpdateError::Extract(e.to_string()))?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| UpdateError::Extract(e.to_string()))?;
        let is_member = Path::new(f.name()).file_name().and_then(|n| n.to_str()).map(|s| s == member).unwrap_or(false);
        if is_member {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).map_err(|e| UpdateError::Extract(e.to_string()))?;
            return Ok(buf);
        }
    }
    Err(UpdateError::Extract(format!("no `{member}` entry in archive")))
}

/// Resolve the install target: the `WXCTL_UPDATE_INSTALL_PATH` test override, or
/// the running binary's path.
pub fn install_target() -> Result<PathBuf, UpdateError> {
    if let Some(p) = std::env::var_os("WXCTL_UPDATE_INSTALL_PATH") {
        return Ok(PathBuf::from(p));
    }
    std::env::current_exe().map_err(UpdateError::Io)
}

/// Fail with `NotWritable` if the install target's directory can't be written
/// (probe-create + remove a temp file). Runs **before** any download.
pub fn preflight_writable(target: &Path) -> Result<(), UpdateError> {
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let probe = dir.join(format!(".wxctl-update-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(UpdateError::NotWritable(dir.to_path_buf())),
    }
}

/// Install `bytes` as the new binary. Test override (`WXCTL_UPDATE_INSTALL_PATH`
/// set): write directly to that path (no self-replace). Default: write a temp
/// file beside the running binary, then atomically `self-replace` it (handles
/// Windows' locked running `.exe`).
pub fn install(bytes: &[u8]) -> Result<(), UpdateError> {
    let target = install_target()?;
    if std::env::var_os("WXCTL_UPDATE_INSTALL_PATH").is_some() {
        return write_executable(&target, bytes);
    }
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".wxctl-update-{}.tmp", std::process::id()));
    write_executable(&tmp, bytes)?;
    let res = self_replace::self_replace(&tmp).map_err(UpdateError::Io);
    let _ = std::fs::remove_file(&tmp);
    res
}

/// Write `bytes` to `path` and (on unix) mark it executable (0o755).
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    std::fs::write(path, bytes).map_err(UpdateError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(UpdateError::Io)?;
    }
    Ok(())
}
