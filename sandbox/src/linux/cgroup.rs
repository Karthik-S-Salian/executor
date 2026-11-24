use crate::{Limits, Result, SandboxError};
use std::fs;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

#[derive(Debug, Default)]
pub struct Stats {
    pub cpu_secs: f64,
    pub memory_peak_kb: f64,
    pub memory_current_kb: f64,
    pub oom_killed: bool,
}

pub struct Cgroup {
    dir: PathBuf,
}

impl Cgroup {
    pub fn create(limits: &Limits) -> Result<Self> {
        let own = own_cgroup_path()?;
        let parent = own.parent().ok_or_else(|| {
            SandboxError::Cgroup(format!("cgroup path '{}' has no parent", own.display()))
        })?;

        // A cgroup that already contains processes cannot enable domain
        // controllers for its children (cgroup v2 no-internal-process rule).
        // For a systemd service, the parent unit directory is the right base;
        // see the README for Delegate=yes / DelegateSubgroup=supervisor.
        let base = if enable_controllers(parent) {
            parent.to_path_buf()
        } else if enable_controllers(&own) {
            own.clone()
        } else {
            return Err(SandboxError::Cgroup(format!(
                "no delegated cgroup v2 subtree under '{}'; enable delegation \
                 (systemd Delegate=yes / DelegateSubgroup=supervisor) or run the \
                 worker in a cgroup that has cpu, memory and pids controllers",
                own.display()
            )));
        };

        let name = format!(
            "executor-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = base.join(name);
        fs::create_dir(&dir)
            .map_err(|e| SandboxError::Cgroup(format!("cannot create '{}': {e}", dir.display())))?;

        let cg = Self { dir };
        cg.apply(limits)?;
        Ok(cg)
    }

    /// Apply limits again before a run so one sandbox can execute several cases
    /// with different limits.
    pub fn apply(&self, limits: &Limits) -> Result<()> {
        self.write("memory.max", &(limits.memory_kb * 1024).to_string())?;
        // Best effort: older kernels may not expose swap accounting.
        let _ = self.write("memory.swap.max", "0");
        self.write("memory.oom.group", "1")?;
        self.write("pids.max", &limits.max_processes.to_string())?;
        Ok(())
    }

    pub fn open_fd(&self) -> Result<OwnedFd> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&self.dir)
            .map_err(|e| {
                SandboxError::Cgroup(format!("cannot open '{}': {e}", self.dir.display()))
            })?;
        Ok(OwnedFd::from(file))
    }

    pub fn add_pid(&self, pid: libc::pid_t) -> Result<()> {
        self.write("cgroup.procs", &pid.to_string())
            .map_err(|e| SandboxError::Cgroup(format!("cannot move pid {pid}: {e}")))
    }

    pub fn kill(&self) {
        if self.write("cgroup.kill", "1").is_ok() {
            return;
        }
        // Pre-fallback: post-fallback kernels without cgroup.kill.
        if let Ok(procs) = fs::read_to_string(self.dir.join("cgroup.procs")) {
            for line in procs.lines() {
                if let Ok(pid) = line.trim().parse::<libc::pid_t>() {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }

    pub fn stats(&self) -> Stats {
        let mut stats = Stats::default();

        if let Ok(cpu) = fs::read_to_string(self.dir.join("cpu.stat")) {
            for line in cpu.lines() {
                if let Some(value) = line.strip_prefix("usage_usec ")
                    && let Ok(usec) = value.trim().parse::<f64>()
                {
                    stats.cpu_secs = usec / 1_000_000.0;
                }
            }
        }

        let peak = fs::read_to_string(self.dir.join("memory.peak"))
            .or_else(|_| fs::read_to_string(self.dir.join("memory.current")))
            .unwrap_or_default();
        if let Ok(bytes) = peak.trim().parse::<f64>() {
            stats.memory_peak_kb = bytes / 1024.0;
        }

        if let Ok(current) = fs::read_to_string(self.dir.join("memory.current"))
            && let Ok(bytes) = current.trim().parse::<f64>()
        {
            stats.memory_current_kb = bytes / 1024.0;
        }

        if let Ok(events) = fs::read_to_string(self.dir.join("memory.events")) {
            for line in events.lines() {
                if let Some(value) = line.strip_prefix("oom_kill ") {
                    stats.oom_killed = value.trim().parse::<u64>().unwrap_or(0) > 0;
                }
            }
        }

        stats
    }

    pub fn cleanup(&mut self) {
        self.kill();
        for _ in 0..50 {
            match fs::remove_dir(&self.dir) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        eprintln!("sandbox: could not remove cgroup '{}'", self.dir.display());
    }

    fn write(&self, file: &str, value: &str) -> std::io::Result<()> {
        fs::write(self.dir.join(file), value)
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn own_cgroup_path() -> Result<PathBuf> {
    let content = fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| SandboxError::Cgroup(format!("cannot read /proc/self/cgroup: {e}")))?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("0::") {
            return Ok(Path::new(CGROUP_ROOT).join(rest.trim_start_matches('/')));
        }
    }
    Err(SandboxError::Cgroup(
        "cgroup v2 is not mounted at /sys/fs/cgroup".into(),
    ))
}

fn enable_controllers(dir: &Path) -> bool {
    let controllers = match fs::read_to_string(dir.join("cgroup.controllers")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let available: Vec<&str> = controllers.split_whitespace().collect();
    if !available.contains(&"memory") || !available.contains(&"pids") {
        return false;
    }

    let current = fs::read_to_string(dir.join("cgroup.subtree_control")).unwrap_or_default();
    let mut additions = String::new();
    for controller in ["cpu", "memory", "pids"] {
        if available.contains(&controller) && !current.split_whitespace().any(|c| c == controller) {
            additions.push('+');
            additions.push_str(controller);
            additions.push(' ');
        }
    }
    if additions.is_empty() {
        return true;
    }
    fs::write(dir.join("cgroup.subtree_control"), additions).is_ok()
}

#[allow(dead_code)]
pub fn is_cgroup_v2() -> bool {
    Path::new(CGROUP_ROOT).join("cgroup.controllers").exists()
}

/// Check whether a delegated subtree exists that can host job cgroups. This is
/// a read-only approximation of what [`Cgroup::create`] will do: a usable base
/// must expose the memory and pids controllers, be writable, and must not
/// already contain processes (cgroup v2's no-internal-process rule).
pub fn delegation_available() -> std::result::Result<(), String> {
    let own = own_cgroup_path().map_err(|e| e.to_string())?;
    let mut parents = Vec::new();
    if let Some(parent) = own.parent() {
        parents.push(parent.to_path_buf());
    }
    parents.push(own);

    let mut reasons = Vec::new();
    for base in parents {
        let controllers = match fs::read_to_string(base.join("cgroup.controllers")) {
            Ok(value) => value,
            Err(e) => {
                reasons.push(format!("{}: {e}", base.display()));
                continue;
            }
        };
        if !controllers.split_whitespace().any(|c| c == "memory")
            || !controllers.split_whitespace().any(|c| c == "pids")
        {
            reasons.push(format!(
                "{}: memory/pids controllers not available",
                base.display()
            ));
            continue;
        }

        let procs = fs::read_to_string(base.join("cgroup.procs")).unwrap_or_default();
        if !procs.trim().is_empty() {
            reasons.push(format!(
                "{}: contains processes (use systemd DelegateSubgroup=supervisor)",
                base.display()
            ));
            continue;
        }

        let subtree = base.join("cgroup.subtree_control");
        let writable = {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            CString::new(subtree.as_os_str().as_bytes())
                .map(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
                .unwrap_or(false)
        };
        if !writable {
            reasons.push(format!("{}: not writable", subtree.display()));
            continue;
        }
        return Ok(());
    }

    Err(reasons.join("; "))
}
