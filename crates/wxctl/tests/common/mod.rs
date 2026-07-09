//! Shared HTTP stub + binary runner for the update-check E2E. Std-only
//! `TcpListener` accept loop (no extra deps); each `GET /check` increments a
//! counter. Env is passed to the CHILD `wxctl` via `Command::env` — never
//! mutated process-globally (avoids the cross-test env race:
//! docs/troubleshoot/archive/env-var-test-race-fix.md).
#![allow(dead_code)] // each integration-test crate compiles only the helpers it uses

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct Stub {
    pub url: String,
    hits: Arc<AtomicUsize>,
}

impl Stub {
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Serve `body` on every connection that requests `/check`; `delay` sleeps
/// before replying (timeout tests).
pub fn start(body: String, delay: Option<Duration>) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits2 = hits.clone();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            handle(&mut s, &body, delay, &hits2);
        }
    });
    Stub { url: format!("http://{addr}/check"), hits }
}

fn handle(s: &mut TcpStream, body: &str, delay: Option<Duration>, hits: &AtomicUsize) {
    let mut buf = [0u8; 1024];
    let _ = s.read(&mut buf);
    if String::from_utf8_lossy(&buf).contains("/check") {
        hits.fetch_add(1, Ordering::SeqCst);
    }
    if let Some(d) = delay {
        thread::sleep(d);
    }
    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    let _ = s.write_all(resp.as_bytes());
}

/// Bind+drop to obtain a definitely-refused endpoint (AC 7 unreachable).
pub fn dead_endpoint() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    format!("http://{addr}/check")
}

/// Run `wxctl resources` against a fresh child env. `force_tty` flips the
/// documented test-only gate so the notice path fires under captured stdout.
/// Inherited kill-switch env is cleared so the host CI/terminal can't skew it.
pub fn run_resources(home: &Path, endpoint: &str, force_tty: bool, extra: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wxctl"));
    // WXCTL_UPDATE_CACHE_DIR pins the child's `~/.wxctl` under the temp home even
    // on Windows, where `dirs::home_dir` ignores the HOME env var.
    cmd.arg("resources").args(extra).env("HOME", home).env("WXCTL_UPDATE_CACHE_DIR", home.join(".wxctl")).env("WXCTL_UPDATE_ENDPOINT", endpoint).env_remove("CI").env_remove("GITHUB_ACTIONS").env_remove("WXCTL_NO_UPDATE_CHECK").env_remove("DO_NOT_TRACK").stdin(Stdio::null());
    if force_tty {
        cmd.env("WXCTL_UPDATE_FORCE_TTY", "1");
    } else {
        cmd.env_remove("WXCTL_UPDATE_FORCE_TTY");
    }
    cmd.output().expect("spawn wxctl resources")
}

// ── Update-download E2E harness (Phase 3) ──
// All child env is set per-Command (never std::env::set_var): the in-process env
// race is documented in docs/troubleshoot/archive/env-var-test-race-fix.md.

/// Multi-route stub: `/check` JSON + release assets (`/v<ver>/<filename>` matched
/// by path suffix), recording every requested path for AC-9 / no-download checks.
pub struct UpdateServer {
    base: String, // http://127.0.0.1:PORT (no /check)
    checks: Arc<AtomicUsize>,
    assets: Arc<AtomicUsize>,
    paths: Arc<Mutex<Vec<String>>>,
}

impl UpdateServer {
    pub fn check_url(&self) -> String {
        format!("{}/check", self.base)
    }
    pub fn base_url(&self) -> &str {
        &self.base
    }
    pub fn check_hits(&self) -> usize {
        self.checks.load(Ordering::SeqCst)
    }
    pub fn asset_hits(&self) -> usize {
        self.assets.load(Ordering::SeqCst)
    }
    pub fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

/// Serve `check_json` at `/check` and each `(filename → bytes)` in `assets`
/// wherever the request path ends with that filename. Counts + logs every path.
pub fn start_update_server(check_json: String, assets: HashMap<String, Vec<u8>>) -> UpdateServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let asset_hits = Arc::new(AtomicUsize::new(0));
    let paths = Arc::new(Mutex::new(Vec::new()));
    let (c2, a2, p2) = (checks.clone(), asset_hits.clone(), paths.clone());
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            p2.lock().unwrap().push(path.clone());
            if path.starts_with("/check") {
                c2.fetch_add(1, Ordering::SeqCst);
                serve(&mut s, "application/json", check_json.as_bytes());
            } else if let Some((_, body)) = assets.iter().find(|(name, _)| path.ends_with(name.as_str())) {
                a2.fetch_add(1, Ordering::SeqCst);
                serve(&mut s, "application/octet-stream", body);
            } else {
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        }
    });
    UpdateServer { base: format!("http://{addr}"), checks, assets: asset_hits, paths }
}

fn serve(s: &mut TcpStream, ctype: &str, body: &[u8]) {
    let header = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = s.write_all(header.as_bytes());
    let _ = s.write_all(body);
}

/// Host target triple — mirrors `update::download::target_triple` (the binary-only
/// crate's internals aren't importable from an integration test).
pub fn host_target() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        (os, arch) => panic!("unsupported host {os}/{arch} for the update E2E"),
    }
    .to_string()
}

#[cfg(unix)]
pub fn host_archive_ext() -> &'static str {
    "tar.gz"
}
#[cfg(windows)]
pub fn host_archive_ext() -> &'static str {
    "zip"
}

#[cfg(unix)]
pub fn host_binary_member() -> &'static str {
    "wxctl"
}
#[cfg(windows)]
pub fn host_binary_member() -> &'static str {
    "wxctl.exe"
}

/// Build the host OS's release archive with `member` = `payload`.
#[cfg(unix)]
pub fn build_release_archive(member: &str, payload: &[u8]) -> Vec<u8> {
    use flate2::{Compression, write::GzEncoder};
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = tar::Builder::new(&mut enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, member, payload).unwrap();
        builder.finish().unwrap();
    }
    enc.finish().unwrap()
}

/// Build the host OS's release archive with `member` = `payload`.
#[cfg(windows)]
pub fn build_release_archive(member: &str, payload: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zw.start_file(member, SimpleFileOptions::default()).unwrap();
    zw.write_all(payload).unwrap();
    zw.finish().unwrap().into_inner()
}

/// Lowercase-hex sha256 (the column format `sha256sum` writes into SHA256SUMS).
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Spawn `wxctl update` with the install seam + release base pointed at the stub.
/// Clears inherited kill switches so the host env can't skew the run.
pub fn run_update(home: &Path, endpoint: &str, release_base: &str, install_path: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wxctl"))
        .arg("update")
        .args(extra)
        .env("HOME", home)
        .env("WXCTL_UPDATE_CACHE_DIR", home.join(".wxctl"))
        .env("WXCTL_UPDATE_ENDPOINT", endpoint)
        .env("WXCTL_RELEASE_BASE_URL", release_base)
        .env("WXCTL_UPDATE_INSTALL_PATH", install_path)
        .env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("WXCTL_DISABLE_UPDATES")
        .stdin(Stdio::null())
        .output()
        .expect("spawn wxctl update")
}
