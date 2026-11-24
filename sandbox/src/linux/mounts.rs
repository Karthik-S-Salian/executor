//! Mount namespace setup.
//!
//! The plan is built in the worker process (allocations, directory creation),
//! then executed in the cloned child using only pre-built C strings and raw
//! syscalls. A fork/clone child in a multithreaded process must not allocate:
//! another thread could hold the allocator lock at the moment of cloning.

use std::ffi::{CStr, CString};
use std::io;
use std::path::{Path, PathBuf};

const SYSTEM_DIRS: &[&str] = &["usr", "lib", "lib64", "bin", "sbin", "etc", "opt"];
const DEVICES: &[&str] = &["null", "zero", "full", "random", "urandom"];

pub enum Op {
    BindReadOnly { source: CString, target: CString },
    BindFile { source: CString, target: CString },
    Tmpfs { target: CString, options: CString },
    Proc { target: CString },
    BindSelf { path: CString },
    Pivot { root: CString },
}

pub struct Plan {
    ops: Vec<Op>,
}

impl Plan {
    pub fn build(root: &Path, extra_ro_paths: &[PathBuf]) -> io::Result<Self> {
        let mut ops = Vec::new();
        let root_c = cstring(root)?;
        // Make the root a mount point before anything is mounted under it, then
        // pivot at the end.
        ops.push(Op::BindSelf {
            path: root_c.clone(),
        });

        for dir in SYSTEM_DIRS {
            let source = Path::new("/").join(dir);
            if !source.exists() {
                continue;
            }
            let target = root.join(dir);
            std::fs::create_dir_all(&target)?;
            ops.push(Op::BindReadOnly {
                source: cstring(&source)?,
                target: cstring(&target)?,
            });
        }

        for source in extra_ro_paths {
            if !source.exists() {
                continue;
            }
            let relative = source.strip_prefix("/").unwrap_or(source);
            let target = root.join(relative);
            std::fs::create_dir_all(&target)?;
            ops.push(Op::BindReadOnly {
                source: cstring(source)?,
                target: cstring(&target)?,
            });
        }

        let dev = root.join("dev");
        std::fs::create_dir_all(&dev)?;
        ops.push(Op::Tmpfs {
            target: cstring(&dev)?,
            options: CString::new("size=1m,mode=755")?,
        });
        for device in DEVICES {
            let source = Path::new("/dev").join(device);
            if !source.exists() {
                continue;
            }
            let target = dev.join(device);
            ops.push(Op::BindFile {
                source: cstring(&source)?,
                target: cstring(&target)?,
            });
        }

        let proc_dir = root.join("proc");
        std::fs::create_dir_all(&proc_dir)?;
        ops.push(Op::Proc {
            target: cstring(&proc_dir)?,
        });

        let tmp = root.join("tmp");
        std::fs::create_dir_all(&tmp)?;
        ops.push(Op::Tmpfs {
            target: cstring(&tmp)?,
            options: CString::new("size=64m,mode=1777")?,
        });

        let shm = root.join("dev/shm");
        std::fs::create_dir_all(&shm)?;
        ops.push(Op::Tmpfs {
            target: cstring(&shm)?,
            options: CString::new("size=16m,mode=1777")?,
        });

        std::fs::create_dir_all(root.join(".old-root"))?;
        ops.push(Op::Pivot { root: root_c });

        Ok(Self { ops })
    }
}

fn cstring(path: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn mount(
    source: Option<&CStr>,
    target: &CStr,
    fstype: Option<&CStr>,
    flags: libc::c_ulong,
    data: Option<&CStr>,
) -> bool {
    let rc = unsafe {
        libc::mount(
            source.map_or(std::ptr::null(), |c| c.as_ptr()),
            target.as_ptr(),
            fstype.map_or(std::ptr::null(), |c| c.as_ptr()),
            flags,
            data.map_or(std::ptr::null(), |c| c.as_ptr() as *const libc::c_void),
        )
    };
    rc == 0
}

/// mkdir(2) ignoring EEXIST; the child may have to create mount targets that
/// were hidden by an earlier tmpfs mount (e.g. `/dev/shm` under `/dev`).
fn mkdir(path: &CStr) {
    unsafe {
        let _ = libc::mkdir(path.as_ptr(), 0o755);
    }
}

/// Create a regular file to receive a device bind mount. Never truncates: only
/// called on paths the child just created inside a fresh tmpfs.
fn touch(path: &CStr) {
    unsafe {
        let fd = libc::open(
            path.as_ptr(),
            libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
            0o600,
        );
        if fd >= 0 {
            libc::close(fd);
        }
    }
}

/// Execute the plan. Returns a static reason on the first failure; the child
/// reports it to the parent without formatting or allocating.
pub fn execute(plan: &Plan) -> std::result::Result<(), &'static [u8]> {
    if !mount(None, c"/", None, libc::MS_REC | libc::MS_PRIVATE, None) {
        return Err(b"making / private failed");
    }

    for op in &plan.ops {
        let ok = match op {
            Op::BindReadOnly { source, target } => {
                mount(
                    Some(source.as_c_str()),
                    target,
                    None,
                    libc::MS_BIND | libc::MS_REC,
                    None,
                ) && mount(
                    None,
                    target,
                    None,
                    libc::MS_BIND
                        | libc::MS_REMOUNT
                        | libc::MS_RDONLY
                        | libc::MS_NOSUID
                        | libc::MS_NODEV,
                    None,
                )
            }
            Op::BindFile { source, target } => {
                touch(target);
                mount(Some(source.as_c_str()), target, None, libc::MS_BIND, None)
            }
            Op::Tmpfs { target, options } => {
                mkdir(target);
                mount(
                    Some(c"tmpfs"),
                    target,
                    Some(c"tmpfs"),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    Some(options.as_c_str()),
                )
            }
            Op::Proc { target } => {
                mkdir(target);
                mount(
                    Some(c"proc"),
                    target,
                    Some(c"proc"),
                    libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                    None,
                )
            }
            Op::BindSelf { path } => mount(
                Some(path.as_c_str()),
                path,
                None,
                libc::MS_BIND | libc::MS_REC,
                None,
            ),
            Op::Pivot { root } => pivot(root),
        };
        if !ok {
            return Err(match op {
                Op::BindReadOnly { .. } => b"read-only bind mount failed",
                Op::BindFile { .. } => b"device bind mount failed",
                Op::Tmpfs { .. } => b"tmpfs mount failed",
                Op::Proc { .. } => b"proc mount failed",
                Op::BindSelf { .. } => b"making the sandbox root a mount point failed",
                Op::Pivot { .. } => b"pivot_root failed",
            });
        }
    }

    let name = b"sandbox";
    unsafe {
        libc::sethostname(name.as_ptr() as *const libc::c_char, name.len());
    }
    Ok(())
}

fn pivot(root: &CStr) -> bool {
    if unsafe { libc::chdir(root.as_ptr()) } != 0 {
        return false;
    }
    let rc = unsafe { libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".old-root".as_ptr()) };
    if rc != 0 {
        return false;
    }
    if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
        return false;
    }
    unsafe {
        libc::umount2(c"/.old-root".as_ptr(), libc::MNT_DETACH);
    }
    unsafe {
        libc::rmdir(c"/.old-root".as_ptr());
    }
    true
}
