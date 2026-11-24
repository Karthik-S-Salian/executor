//! Rootless confinement for untrusted code.
//!
//! The sandbox runs one program inside unprivileged user/mount/pid/net/ipc/uts
//! namespaces, a delegated cgroup v2 sub-tree, rlimits, Landlock (filesystem)
//! and a seccomp deny-list. It requires no root and no setuid helper.
//!
//! The containment is only as strong as the host's kernel and its userns/cgroup
//! configuration; [`Capabilities::probe`] reports whether the host can honour it.
//! Callers must fail closed when the probe reports unsupported.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Capabilities;
#[cfg(target_os = "linux")]
use linux::PlatformSandbox;

#[cfg(not(target_os = "linux"))]
mod stub;
#[cfg(not(target_os = "linux"))]
pub use stub::Capabilities;
#[cfg(not(target_os = "linux"))]
use stub::PlatformSandbox;

#[derive(Debug)]
pub enum SandboxError {
    /// The host cannot provide the isolation this crate promises.
    Unsupported(String),
    Io(io::Error),
    Setup(String),
    Cgroup(String),
    Runner(String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxError::Unsupported(m) => write!(f, "sandbox unsupported: {m}"),
            SandboxError::Io(e) => write!(f, "sandbox I/O: {e}"),
            SandboxError::Setup(m) => write!(f, "sandbox setup failed: {m}"),
            SandboxError::Cgroup(m) => write!(f, "cgroup setup failed: {m}"),
            SandboxError::Runner(m) => write!(f, "sandbox runner failed: {m}"),
        }
    }
}

impl std::error::Error for SandboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SandboxError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for SandboxError {
    fn from(e: io::Error) -> Self {
        SandboxError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, SandboxError>;

/// Resource limits for one execution.
#[derive(Debug, Clone)]
pub struct Limits {
    /// CPU seconds before the run is considered timed out.
    pub cpu_time_secs: f64,
    /// Grace period on top of `cpu_time_secs` (isolate's `-x`).
    pub cpu_extra_secs: f64,
    /// Wall-clock deadline.
    pub wall_time_secs: f64,
    pub memory_kb: u64,
    pub stack_kb: u64,
    pub max_processes: u64,
    pub max_file_size_kb: u64,
    pub max_open_files: u64,
    /// stdout/stderr are truncated to this many bytes.
    pub output_limit_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu_time_secs: 2.0,
            cpu_extra_secs: 1.0,
            wall_time_secs: 4.0,
            memory_kb: 262_144,
            stack_kb: 65_536,
            max_processes: 50,
            max_file_size_kb: 1024,
            max_open_files: 256,
            output_limit_bytes: 1024 * 1024,
        }
    }
}

/// Settings that apply to every run of one sandbox.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Parent directory for sandbox roots (e.g. `/run/executor`).
    pub base_dir: PathBuf,
    /// Give the program the host network namespace. Leave false for untrusted code.
    pub network: bool,
    /// Refuse to run if Landlock is unavailable (default true).
    pub require_landlock: bool,
    /// Explicit environment. Nothing is inherited from the worker process.
    pub env: Vec<(String, String)>,
    /// Extra read-only host paths to bind into the box (e.g. `/opt`).
    pub extra_ro_paths: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            base_dir: std::env::temp_dir().join("executor-sandbox"),
            network: false,
            require_landlock: true,
            env: Vec::new(),
            extra_ro_paths: Vec::new(),
        }
    }
}

/// One program invocation.
#[derive(Debug)]
pub struct RunSpec<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub stdin: &'a [u8],
    pub limits: Limits,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub memory_limit_hit: bool,
    pub output_truncated: bool,
    pub cpu_secs: f64,
    pub wall_secs: f64,
    pub memory_peak_kb: f64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A prepared sandbox. Files written with [`Sandbox::write_file`] are placed at
/// the root of the sandbox filesystem.
pub struct Sandbox {
    inner: PlatformSandbox,
    root: PathBuf,
}

impl Sandbox {
    pub fn create(config: SandboxConfig) -> Result<Self> {
        let inner = PlatformSandbox::create(config)?;
        let root = inner.root().to_path_buf();
        Ok(Self { inner, root })
    }

    /// Write a file relative to the sandbox root. Absolute paths and `..` are
    /// rejected, and symlinks are never followed: an earlier untrusted run
    /// could otherwise plant a link to a worker-writable host path.
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        let relative = safe_relative(path)?;
        self.inner.write_file(&relative, data)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run(&self, spec: &RunSpec<'_>) -> Result<Outcome> {
        self.inner.run(spec)
    }

    pub fn cleanup(self) -> Result<()> {
        self.inner.cleanup()
    }
}

fn safe_relative(path: &str) -> Result<PathBuf> {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(SandboxError::Setup(format!(
                    "path '{path}' escapes the sandbox root"
                )));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(SandboxError::Setup("empty sandbox path".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escaping_paths() {
        assert!(safe_relative("../etc/passwd").is_err());
        assert!(safe_relative("/etc/passwd").is_err());
        assert!(safe_relative("a/../../b").is_err());
    }

    #[test]
    fn accepts_nested_paths() {
        assert_eq!(
            safe_relative("src/main.py").unwrap(),
            PathBuf::from("src/main.py")
        );
        assert_eq!(safe_relative("./main.c").unwrap(), PathBuf::from("main.c"));
    }
}
