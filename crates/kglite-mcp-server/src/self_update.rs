//! Stale-build detection: tell the agent when the code this server runs was
//! replaced on disk (`pip install -U`, `cargo install`, a rebuilt dev binary).
//!
//! The framework (`mcp_methods::server::SelfCheck`) compares a stat snapshot of
//! a watched file on every tool call. This module decides *which* file that is,
//! what version is installed there, and which name and version this process
//! reports.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mcp_methods::server::SelfCheck;

/// Name and version a server reports in its stale notice, `ping` and
/// `serverInfo`. The default is this crate; an embedder that ships its own
/// package overrides it with [`crate::ServerExtensions::with_identity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) name: String,
    pub(crate) version: String,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            name: "kglite-mcp-server".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// How the watched file ships, which decides how its installed version is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Artifact {
    /// The running executable itself (cargo binary, `sonagram-mcp-server`).
    Binary(PathBuf),
    /// A shared object loaded into another host process: the wheel's extension.
    Extension(PathBuf),
}

impl Artifact {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Artifact::Binary(path) | Artifact::Extension(path) => path,
        }
    }
}

/// `current_exe()` with the ` (deleted)` suffix Linux appends once the file
/// was unlinked, so the path still names what a replacement will recreate.
fn current_exe_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let text = exe.to_string_lossy();
    Some(match text.strip_suffix(" (deleted)") {
        Some(stripped) => PathBuf::from(stripped),
        None => exe,
    })
}

/// The object file that contains this very function.
#[cfg(unix)]
fn loaded_object_path() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: `dladdr` only reads the address and fills `info`; `info` is
    // zero-initialised and `dli_fname` is read only when `dladdr` reported
    // success, as a NUL-terminated string owned by the loader.
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        let anchor = loaded_object_path as fn() -> Option<PathBuf>;
        if libc::dladdr(anchor as *const libc::c_void, &mut info) == 0 || info.dli_fname.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr(info.dli_fname);
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())))
    }
}

#[cfg(not(unix))]
fn loaded_object_path() -> Option<PathBuf> {
    None
}

fn canonical(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// Pure classification of the two candidate paths: the object containing this
/// code (`dladdr`) and the process executable.
pub(crate) fn classify(object: Option<PathBuf>, exe: Option<PathBuf>) -> Option<Artifact> {
    match (object.map(canonical), exe.map(canonical)) {
        (Some(object), Some(exe)) if object == exe => Some(Artifact::Binary(exe)),
        (Some(object), _) => Some(Artifact::Extension(object)),
        (None, Some(exe)) => Some(Artifact::Binary(exe)),
        (None, None) => None,
    }
}

pub(crate) fn watched_artifact() -> Option<Artifact> {
    classify(loaded_object_path(), current_exe_path())
}

/// `<site>/<pkg>-<version>.dist-info` next to the package directory that holds
/// the extension; the version of the newest one when pip left several.
pub(crate) fn dist_info_version(extension: &Path) -> Option<String> {
    let package_dir = extension.parent()?;
    let package = package_dir.file_name()?.to_str()?;
    let site = package_dir.parent()?;
    let wanted = normalise_dist_name(package);
    let mut found: Vec<(Vec<u64>, String)> = Vec::new();
    for entry in std::fs::read_dir(site).ok()?.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".dist-info")) else {
            continue;
        };
        // `<name>-<version>`: a normalised name holds no `-`, so the first one splits.
        let Some((dist, version)) = stem.split_once('-') else {
            continue;
        };
        if normalise_dist_name(dist) == wanted {
            found.push((version_key(version), version.to_string()));
        }
    }
    found.into_iter().max().map(|(_, version)| version)
}

fn normalise_dist_name(name: &str) -> String {
    name.to_ascii_lowercase().replace(['-', '.'], "_")
}

fn version_key(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// Version printed by `<program> --version` (`name X.Y.Z`), within `timeout`.
pub(crate) fn probe_binary_version(program: &Path, timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    parse_version_line(&out)
}

pub(crate) fn parse_version_line(output: &str) -> Option<String> {
    let token = output.lines().next()?.split_whitespace().last()?;
    token
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| token.to_string())
}

/// The framework check for this process, or `None` when no file can be named.
pub(crate) fn build_self_check(identity: &Identity) -> Option<SelfCheck> {
    let artifact = watched_artifact()?;
    let probe = artifact.clone();
    Some(
        SelfCheck::new(
            identity.name.clone(),
            artifact.path().to_path_buf(),
            identity.version.clone(),
        )
        .with_installed_version(move || match &probe {
            Artifact::Extension(path) => dist_info_version(path),
            Artifact::Binary(path) => probe_binary_version(path, Duration::from_secs(5)),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_dist_info_is_found_beside_the_package_directory() {
        let site = tempfile::tempdir().unwrap();
        let package = site.path().join("kglite");
        std::fs::create_dir_all(&package).unwrap();
        let so = package.join("kglite.abi3.so");
        std::fs::write(&so, b"x").unwrap();
        for dir in [
            "kglite-0.20.0.dist-info",
            "kglite-0.20.1.dist-info",
            "kglite-0.9.9.dist-info",
            "kglite_extra-9.0.0.dist-info",
            "other-5.0.dist-info",
        ] {
            std::fs::create_dir(site.path().join(dir)).unwrap();
        }
        assert_eq!(dist_info_version(&so).as_deref(), Some("0.20.1"));
    }

    #[test]
    fn dist_names_normalise_dashes_and_case() {
        let site = tempfile::tempdir().unwrap();
        let package = site.path().join("my_pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir(site.path().join("My_Pkg-1.2.3.dist-info")).unwrap();
        assert_eq!(
            dist_info_version(&package.join("ext.so")).as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn missing_dist_info_is_unknown() {
        let site = tempfile::tempdir().unwrap();
        let package = site.path().join("kglite");
        std::fs::create_dir_all(&package).unwrap();
        assert_eq!(dist_info_version(&package.join("kglite.abi3.so")), None);
    }

    #[test]
    fn version_line_parsing() {
        assert_eq!(
            parse_version_line("kglite-mcp-server 0.20.1\n").as_deref(),
            Some("0.20.1")
        );
        assert_eq!(parse_version_line("error: nope\n"), None);
        assert_eq!(parse_version_line(""), None);
    }

    #[test]
    fn object_equal_to_the_executable_is_a_binary_else_an_extension() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("server");
        let so = dir.path().join("pkg").join("ext.so");
        std::fs::create_dir_all(so.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"x").unwrap();
        std::fs::write(&so, b"x").unwrap();
        let same = classify(Some(exe.clone()), Some(exe.clone())).unwrap();
        assert!(matches!(same, Artifact::Binary(_)));
        let ext = classify(Some(so), Some(exe.clone())).unwrap();
        assert!(matches!(ext, Artifact::Extension(_)));
        assert!(matches!(
            classify(None, Some(exe)).unwrap(),
            Artifact::Binary(_)
        ));
        assert_eq!(classify(None, None), None);
    }

    #[test]
    fn this_test_binary_resolves_to_itself() {
        let artifact = watched_artifact().expect("resolves");
        assert!(matches!(artifact, Artifact::Binary(_)), "{artifact:?}");
        assert_eq!(
            artifact.path(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn default_identity_is_this_crate() {
        let identity = Identity::default();
        assert_eq!(identity.name, "kglite-mcp-server");
        assert_eq!(identity.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn an_embedder_identity_is_what_the_server_reports() {
        use rmcp::ServerHandler;
        let extensions = crate::ServerExtensions::new().with_identity("codingest", "9.8.7");
        assert_eq!(extensions.identity.name, "codingest");
        let check = build_self_check(&extensions.identity).expect("watched file");
        let options = mcp_methods::server::ServerOptions::default().with_self_check(check);
        let info = mcp_methods::server::McpServer::new(options).get_info();
        assert_eq!(info.server_info.version, "9.8.7");
    }
}
