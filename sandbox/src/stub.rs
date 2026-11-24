use crate::{Outcome, Result, RunSpec, SandboxConfig, SandboxError};
use std::path::{Path, PathBuf};

/// Capability report on non-Linux hosts.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub kernel: String,
    pub cgroup_v2: bool,
    pub cgroup_delegated: bool,
    pub userns: bool,
    pub seccomp: bool,
    pub landlock_abi: i32,
    pub usable: bool,
    pub problems: Vec<String>,
}

impl Capabilities {
    pub fn probe() -> Self {
        Self {
            kernel: "non-linux".into(),
            cgroup_v2: false,
            cgroup_delegated: false,
            userns: false,
            seccomp: false,
            landlock_abi: 0,
            usable: false,
            problems: vec![
                "the sandbox requires Linux namespaces, cgroup v2, seccomp and Landlock"
                    .to_string(),
            ],
        }
    }
}

pub(crate) struct PlatformSandbox {
    root: PathBuf,
}

impl PlatformSandbox {
    pub fn create(_config: SandboxConfig) -> Result<Self> {
        Err(SandboxError::Unsupported(
            "sandbox is only implemented on Linux".into(),
        ))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write_file(&self, _relative: &Path, _data: &[u8]) -> Result<()> {
        Err(SandboxError::Unsupported(
            "sandbox is only implemented on Linux".into(),
        ))
    }

    pub fn run(&self, _spec: &RunSpec<'_>) -> Result<Outcome> {
        Err(SandboxError::Unsupported(
            "sandbox is only implemented on Linux".into(),
        ))
    }

    pub fn cleanup(self) -> Result<()> {
        Ok(())
    }
}
