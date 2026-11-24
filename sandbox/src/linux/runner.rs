use crate::{Limits, Outcome, Result, RunSpec, SandboxConfig, SandboxError};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{cgroup::Cgroup, landlock, mounts, seccomp};

const CLONE_INTO_CGROUP: u64 = 1 << 33;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

pub(crate) struct PlatformSandbox {
    dir: PathBuf,
    root: PathBuf,
    config: SandboxConfig,
    run_lock: std::sync::Mutex<()>,
    cleaned: bool,
}

impl PlatformSandbox {
    pub fn create(config: SandboxConfig) -> Result<Self> {
        let caps = super::Capabilities::probe();
        if !caps.usable {
            return Err(SandboxError::Unsupported(caps.problems.join("; ")));
        }
        if config.require_landlock && caps.landlock_abi < 1 {
            return Err(SandboxError::Unsupported(
                "Landlock is required but unavailable".into(),
            ));
        }

        fs::create_dir_all(&config.base_dir)?;
        let name = format!(
            "job-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = config.base_dir.join(name);
        fs::create_dir(&dir)?;
        let root = dir.join("root");
        fs::create_dir(&root)?;

        Ok(Self {
            dir,
            root,
            config,
            run_lock: std::sync::Mutex::new(()),
            cleaned: false,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write_file(&self, relative: &Path, data: &[u8]) -> Result<()> {
        use std::path::Component;

        let _guard = self
            .run_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut directory = self.root.clone();
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                let Component::Normal(part) = component else {
                    return Err(SandboxError::Setup("invalid sandbox path component".into()));
                };
                directory.push(part);
                match fs::symlink_metadata(&directory) {
                    Ok(metadata) if metadata.file_type().is_dir() => {}
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(SandboxError::Setup(format!(
                            "refusing to write through symlink '{}'",
                            directory.display()
                        )));
                    }
                    Ok(_) => {
                        return Err(SandboxError::Setup(format!(
                            "'{}' is not a directory",
                            directory.display()
                        )));
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        fs::create_dir(&directory)?;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }

        let target = self.root.join(relative);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&target)?;
        file.write_all(data)?;
        Ok(())
    }

    pub fn run(&self, spec: &RunSpec<'_>) -> Result<Outcome> {
        let _guard = self
            .run_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // One cgroup per execution keeps cpu/memory accounting (and limits)
        // independent between a compilation and each test case.
        let cgroup = Cgroup::create(&spec.limits)?;

        let stdin_path = self.dir.join("stdin");
        let stdout_path = self.dir.join("stdout");
        let stderr_path = self.dir.join("stderr");
        fs::write(&stdin_path, spec.stdin)?;
        File::create(&stdout_path)?;
        File::create(&stderr_path)?;

        let stdin = open_ro(&stdin_path)?;
        let stdout = OpenOptions::new().write(true).open(&stdout_path)?;
        let stderr = OpenOptions::new().write(true).open(&stderr_path)?;

        let (sync_read, sync_write) = pipe()?;
        let (err_read, err_write) = pipe()?;

        let mount_plan = mounts::Plan::build(&self.root, &self.config.extra_ro_paths)?;
        let landlock_plan = if self.config.require_landlock {
            Some(landlock::prepare(&self.root)?)
        } else {
            landlock::prepare(&self.root).ok()
        };
        let seccomp_plan = seccomp::Prepared::build();
        let exec = build_exec(spec, &self.config)?;

        let cgroup_fd = cgroup.open_fd()?;
        let (pid, into_cgroup) = spawn(
            &spec.limits,
            self.config.network,
            &mount_plan,
            landlock_plan.as_ref(),
            &seccomp_plan,
            &exec,
            stdin.as_raw_fd(),
            stdout.as_raw_fd(),
            stderr.as_raw_fd(),
            sync_read.as_raw_fd(),
            err_write.as_raw_fd(),
            Some(cgroup_fd.as_raw_fd()),
        )?;

        drop(sync_read);
        drop(err_write);
        drop(stdin);
        drop(stdout);
        drop(stderr);

        if let Err(e) = write_uid_maps(pid) {
            kill_and_reap(pid);
            return Err(e);
        }
        // clone3(CLONE_INTO_CGROUP) already placed the child. Otherwise the
        // write happens before the sync byte, so the child cannot run user code
        // before it is inside the limited cgroup.
        if !into_cgroup && let Err(e) = cgroup.add_pid(pid) {
            kill_and_reap(pid);
            return Err(e);
        }
        if let Err(e) = write_all(sync_write.as_raw_fd(), &[1]) {
            kill_and_reap(pid);
            return Err(e);
        }
        drop(sync_write);

        if let Some(message) = read_setup_error(err_read.as_raw_fd(), pid) {
            return Err(SandboxError::Setup(message));
        }

        self.wait(pid, &cgroup, &spec.limits, &stdout_path, &stderr_path)
    }

    fn wait(
        &self,
        pid: libc::pid_t,
        cgroup: &Cgroup,
        limits: &Limits,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<Outcome> {
        let start = Instant::now();
        let deadline = Duration::from_secs_f64(limits.wall_time_secs.max(0.05));
        let cpu_limit = limits.cpu_time_secs + limits.cpu_extra_secs;
        let mut timed_out = false;
        let mut status = 0;
        let mut rusage: libc::rusage = unsafe { std::mem::zeroed() };
        let mut last_stat = Instant::now();
        let mut reaped = false;
        let mut max_memory_kb: f64 = 0.0;

        loop {
            let waited = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut rusage) };
            if waited == pid {
                reaped = true;
                break;
            }
            if waited < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    return Err(SandboxError::Runner(
                        "child was reaped by another SIGCHLD handler".into(),
                    ));
                }
                return Err(SandboxError::Runner(error.to_string()));
            }

            if start.elapsed() >= deadline {
                timed_out = true;
                kill_tree(pid, cgroup);
                break;
            }

            if last_stat.elapsed() >= Duration::from_millis(10) {
                last_stat = Instant::now();
                let stats = cgroup.stats();
                max_memory_kb = max_memory_kb.max(stats.memory_current_kb);
                if stats.cpu_secs > cpu_limit {
                    timed_out = true;
                    kill_tree(pid, cgroup);
                    break;
                }
            }

            std::thread::sleep(Duration::from_millis(2));
        }

        if !reaped {
            let kill_start = Instant::now();
            loop {
                let waited = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut rusage) };
                if waited == pid {
                    break;
                }
                if waited < 0 {
                    return Err(SandboxError::Runner(format!(
                        "wait4 after kill: {}",
                        io::Error::last_os_error()
                    )));
                }
                if kill_start.elapsed() > Duration::from_secs(5) {
                    return Err(SandboxError::Runner(
                        "process did not die after SIGKILL".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let wall = start.elapsed().as_secs_f64();

        let stats = cgroup.stats();
        let memory_peak_kb = stats.memory_peak_kb.max(max_memory_kb);
        let (stdout, truncated) = read_capped(stdout_path, limits.output_limit_bytes)?;
        let (stderr, stderr_truncated) = read_capped(stderr_path, limits.output_limit_bytes)?;

        let (exit_code, signal) = if libc::WIFEXITED(status) {
            (Some(libc::WEXITSTATUS(status)), None)
        } else if libc::WIFSIGNALED(status) {
            (None, Some(libc::WTERMSIG(status)))
        } else {
            (None, None)
        };

        let rusage_cpu = rusage.ru_utime.tv_sec as f64
            + rusage.ru_utime.tv_usec as f64 / 1e6
            + rusage.ru_stime.tv_sec as f64
            + rusage.ru_stime.tv_usec as f64 / 1e6;
        let cpu = if stats.cpu_secs > 0.0 {
            stats.cpu_secs
        } else {
            rusage_cpu
        };

        Ok(Outcome {
            exit_code,
            signal,
            timed_out: timed_out || cpu > cpu_limit,
            memory_limit_hit: stats.oom_killed,
            output_truncated: truncated || stderr_truncated,
            cpu_secs: cpu,
            wall_secs: wall,
            memory_peak_kb: if memory_peak_kb > 0.0 {
                memory_peak_kb
            } else {
                rusage.ru_maxrss as f64
            },
            stdout,
            stderr,
        })
    }

    pub fn cleanup(mut self) -> Result<()> {
        self.cleanup_best_effort();
        Ok(())
    }

    pub fn cleanup_best_effort(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Drop for PlatformSandbox {
    fn drop(&mut self) {
        self.cleanup_best_effort();
    }
}

struct Exec {
    program: CString,
    // Owned to keep the CString allocations alive for the argv/envp pointers.
    #[allow(dead_code)]
    argv: Vec<CString>,
    #[allow(dead_code)]
    envp: Vec<CString>,
    argv_ptrs: Vec<*const libc::c_char>,
    envp_ptrs: Vec<*const libc::c_char>,
}

impl Exec {
    fn build(spec: &RunSpec<'_>, config: &SandboxConfig) -> Result<Self> {
        let mut argv = Vec::with_capacity(spec.args.len() + 1);
        argv.push(
            CString::new(spec.program.as_bytes())
                .map_err(|_| SandboxError::Setup("program path contains a NUL byte".into()))?,
        );
        for arg in spec.args {
            argv.push(
                CString::new(arg.as_bytes())
                    .map_err(|_| SandboxError::Setup("argument contains a NUL byte".into()))?,
            );
        }

        let mut env: Vec<(String, String)> = vec![
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("HOME".into(), "/tmp".into()),
            ("TMPDIR".into(), "/tmp".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
        ];
        for (key, value) in &config.env {
            if let Some(existing) = env.iter_mut().find(|(k, _)| k == key) {
                existing.1 = value.clone();
            } else {
                env.push((key.clone(), value.clone()));
            }
        }
        let envp = env
            .iter()
            .map(|(key, value)| {
                CString::new(format!("{key}={value}"))
                    .map_err(|_| SandboxError::Setup("environment contains a NUL byte".into()))
            })
            .collect::<Result<Vec<_>>>()?;

        let search_path = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.as_str())
            .unwrap_or("/usr/local/bin:/usr/bin:/bin");
        // Resolve in the worker so the child can call execve(2) directly: PATH
        // lookup and allocation must not happen after clone.
        let resolved = resolve_program(spec.program, search_path);
        let program = CString::new(resolved.as_bytes())
            .map_err(|_| SandboxError::Setup("program path contains a NUL byte".into()))?;

        let argv_ptrs = argv.iter().map(|s| s.as_ptr()).collect();
        let envp_ptrs = envp.iter().map(|s| s.as_ptr()).collect();

        Ok(Self {
            program,
            argv,
            envp,
            argv_ptrs,
            envp_ptrs,
        })
    }
}

fn resolve_program(program: &str, search_path: &str) -> String {
    if program.contains('/') {
        return program.to_string();
    }
    for dir in search_path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = format!("{dir}/{program}");
        if Path::new(&candidate).is_file() {
            return candidate;
        }
    }
    program.to_string()
}

fn build_exec(spec: &RunSpec<'_>, config: &SandboxConfig) -> Result<Exec> {
    Exec::build(spec, config)
}

#[allow(clippy::too_many_arguments)]
fn spawn(
    limits: &Limits,
    network: bool,
    mount_plan: &mounts::Plan,
    landlock_plan: Option<&landlock::Prepared>,
    seccomp_plan: &seccomp::Prepared,
    exec: &Exec,
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    sync_read: RawFd,
    err_write: RawFd,
    cgroup_fd: Option<RawFd>,
) -> Result<(libc::pid_t, bool)> {
    let mut flags = (libc::CLONE_NEWUSER
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWNET) as u64;
    if network {
        flags &= !(libc::CLONE_NEWNET as u64);
    }

    let mut attempts = vec![false];
    if cgroup_fd.is_some() {
        attempts.insert(0, true);
    }

    let mut last_error = None;
    for into_cgroup in attempts {
        let mut args = CloneArgs {
            flags: flags | if into_cgroup { CLONE_INTO_CGROUP } else { 0 },
            exit_signal: libc::SIGCHLD as u64,
            cgroup: if into_cgroup {
                cgroup_fd.unwrap() as u64
            } else {
                0
            },
            ..CloneArgs::default()
        };

        let rc = unsafe {
            libc::syscall(
                libc::SYS_clone3,
                &mut args as *mut CloneArgs,
                std::mem::size_of::<CloneArgs>(),
            )
        };

        if rc == 0 {
            child_main(
                limits,
                mount_plan,
                landlock_plan,
                seccomp_plan,
                exec,
                stdin,
                stdout,
                stderr,
                sync_read,
                err_write,
            );
        }
        if rc > 0 {
            return Ok((rc as libc::pid_t, into_cgroup));
        }

        let error = io::Error::last_os_error();
        if into_cgroup
            && matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
            )
        {
            last_error = Some(error);
            continue;
        }
        return Err(SandboxError::Runner(format!(
            "clone3 failed: {error} (user namespaces may be disabled or restricted)"
        )));
    }

    Err(SandboxError::Runner(format!(
        "clone3 failed: {}",
        last_error.unwrap_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))
    )))
}

#[allow(clippy::too_many_arguments)]
fn child_main(
    limits: &Limits,
    mount_plan: &mounts::Plan,
    landlock_plan: Option<&landlock::Prepared>,
    seccomp_plan: &seccomp::Prepared,
    exec: &Exec,
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    sync_read: RawFd,
    err_write: RawFd,
) -> ! {
    let mut byte = [0u8; 1];
    if read_some(sync_read, &mut byte) != 1 || byte[0] != 1 {
        unsafe { libc::_exit(125) };
    }

    // From here until execve(2) only raw syscalls and pre-built data are used:
    // this process was cloned from a multithreaded parent and must not allocate.
    let fail = |message: &'static [u8]| -> ! {
        unsafe {
            libc::write(
                err_write,
                message.as_ptr() as *const libc::c_void,
                message.len(),
            );
            libc::_exit(101);
        }
    };

    unsafe {
        libc::setpgid(0, 0);
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        libc::setgroups(0, std::ptr::null());
    }
    if unsafe { libc::setgid(0) } != 0 {
        fail(b"setgid failed");
    }
    if unsafe { libc::setuid(0) } != 0 {
        fail(b"setuid failed");
    }

    if !set_limits(limits) {
        fail(b"setrlimit failed");
    }

    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        fail(b"no_new_privs failed");
    }

    if let Err(reason) = mounts::execute(mount_plan) {
        fail(reason);
    }

    if let Some(plan) = landlock_plan
        && !plan.apply()
    {
        fail(b"Landlock enforcement failed");
    }

    if !seccomp_plan.install() {
        fail(b"seccomp installation failed");
    }

    unsafe {
        if libc::dup2(stdin, 0) < 0 || libc::dup2(stdout, 1) < 0 || libc::dup2(stderr, 2) < 0 {
            fail(b"dup2 failed");
        }
    }

    if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
        fail(b"chdir failed");
    }

    unsafe {
        libc::execve(
            exec.program.as_ptr(),
            exec.argv_ptrs.as_ptr(),
            exec.envp_ptrs.as_ptr(),
        );
    }
    fail(b"execve failed");
}

fn set_limits(limits: &Limits) -> bool {
    let cpu = (limits.cpu_time_secs + limits.cpu_extra_secs)
        .ceil()
        .max(1.0) as libc::rlim_t;
    if !set_rlimit(libc::RLIMIT_CPU as libc::c_int, cpu, cpu + 1) {
        return false;
    }
    let stack = (limits.stack_kb * 1024) as libc::rlim_t;
    if !set_rlimit(libc::RLIMIT_STACK as libc::c_int, stack, stack) {
        return false;
    }
    let file = (limits.max_file_size_kb * 1024) as libc::rlim_t;
    // stdout/stderr are regular files too; keep RLIMIT_FSIZE from turning a
    // chatty program into SIGXFSZ before the output cap is reached.
    let file = file.max(limits.output_limit_bytes as libc::rlim_t + 1024 * 1024);
    if !set_rlimit(libc::RLIMIT_FSIZE as libc::c_int, file, file) {
        return false;
    }
    if !set_rlimit(
        libc::RLIMIT_NOFILE as libc::c_int,
        limits.max_open_files as libc::rlim_t,
        limits.max_open_files as libc::rlim_t,
    ) {
        return false;
    }
    set_rlimit(libc::RLIMIT_CORE as libc::c_int, 0, 0)
}

fn set_rlimit(resource: libc::c_int, soft: libc::rlim_t, hard: libc::rlim_t) -> bool {
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    unsafe { libc::setrlimit(resource as _, &limit) == 0 }
}

fn write_uid_maps(pid: libc::pid_t) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let base = PathBuf::from(format!("/proc/{pid}"));

    // Must be written before gid_map; also removes setgroups(2) inside.
    let _ = fs::write(base.join("setgroups"), "deny");
    fs::write(base.join("uid_map"), format!("0 {uid} 1\n"))
        .map_err(|e| SandboxError::Setup(format!("cannot write uid_map: {e}")))?;
    fs::write(base.join("gid_map"), format!("0 {gid} 1\n"))
        .map_err(|e| SandboxError::Setup(format!("cannot write gid_map: {e}")))?;
    Ok(())
}

fn read_setup_error(fd: RawFd, pid: libc::pid_t) -> Option<String> {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout = SETUP_TIMEOUT.as_millis() as i32;
    let rc = unsafe { libc::poll(&mut pollfd, 1, timeout) };
    if rc == 0 {
        kill_and_reap(pid);
        return Some("sandbox setup timed out".into());
    }
    if rc < 0 {
        let error = io::Error::last_os_error();
        kill_and_reap(pid);
        return Some(format!("poll: {error}"));
    }

    let mut buffer = [0u8; 1024];
    let read = read_some(fd, &mut buffer);
    if read <= 0 {
        return None;
    }

    let _ = wait_blocking(pid);
    Some(String::from_utf8_lossy(&buffer[..read as usize]).into_owned())
}

fn kill_and_reap(pid: libc::pid_t) {
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
}

fn kill_tree(pid: libc::pid_t, cgroup: &Cgroup) {
    cgroup.kill();
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
}

fn wait_blocking(pid: libc::pid_t) -> io::Result<()> {
    let mut status = 0;
    loop {
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc == pid {
            return Ok(());
        }
        if rc < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
    }
}

fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }
    unsafe { Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))) }
}

fn open_ro(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)?)
}

fn read_some(fd: RawFd, buffer: &mut [u8]) -> libc::ssize_t {
    loop {
        let rc = unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len()) };
        if rc < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return rc;
    }
}

fn write_all(fd: RawFd, mut data: &[u8]) -> Result<()> {
    while !data.is_empty() {
        let written = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if written < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(SandboxError::Io(error));
        }
        data = &data[written as usize..];
    }
    Ok(())
}

fn read_capped(path: &Path, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let metadata = fs::metadata(path)?;
    let truncated = metadata.len() > limit as u64;
    let mut file = File::open(path)?;
    let mut buffer = Vec::with_capacity(limit.min(metadata.len() as usize));
    Read::by_ref(&mut file)
        .take(limit as u64)
        .read_to_end(&mut buffer)?;
    Ok((buffer, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_programs_from_path() {
        if Path::new("/bin/sh").is_file() {
            assert_eq!(resolve_program("sh", "/bin"), "/bin/sh");
        }
        assert_eq!(resolve_program("./main", "/bin"), "./main");
        assert_eq!(resolve_program("missing", "/nonexistent"), "missing");
    }
}
