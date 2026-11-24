//! Landlock filesystem confinement (kernel 5.13+).
//!
//! The ruleset is prepared in the worker process and enforced in the child with
//! one raw syscall, so the child never allocates. One path-beneath rule allows
//! the pivoted sandbox root; nothing outside it is reachable. Read-only bind
//! mounts remain the primary write barrier for system paths.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const SYS_CREATE_RULESET: libc::c_long = 444;
const SYS_ADD_RULE: libc::c_long = 445;
const SYS_RESTRICT_SELF: libc::c_long = 446;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const ACCESS_FS_REFER: u64 = 1 << 13;
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Landlock ABI version, or 0 when unavailable.
pub fn abi() -> i32 {
    let rc = unsafe {
        libc::syscall(
            SYS_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if rc < 0 { 0 } else { rc as i32 }
}

fn handled_access(abi: i32) -> u64 {
    let mut access = ACCESS_FS_EXECUTE
        | ACCESS_FS_WRITE_FILE
        | ACCESS_FS_READ_FILE
        | ACCESS_FS_READ_DIR
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_MAKE_CHAR
        | ACCESS_FS_MAKE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_SOCK
        | ACCESS_FS_MAKE_FIFO
        | ACCESS_FS_MAKE_BLOCK
        | ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        access |= ACCESS_FS_REFER;
    }
    if abi >= 3 {
        access |= ACCESS_FS_TRUNCATE;
    }
    access
}

fn open_path(path: &Path) -> io::Result<OwnedFd> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub struct Prepared {
    ruleset: OwnedFd,
}

/// Create the ruleset and allow everything beneath `root`. Called in the worker
/// before cloning.
pub fn prepare(root: &Path) -> io::Result<Prepared> {
    let abi = abi();
    if abi < 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Landlock is not available",
        ));
    }

    let handled = handled_access(abi);
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    let ruleset = unsafe {
        libc::syscall(
            SYS_CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if ruleset < 0 {
        return Err(io::Error::last_os_error());
    }
    let ruleset = unsafe { OwnedFd::from_raw_fd(ruleset as i32) };
    // The ruleset fd must survive clone but not exec.
    unsafe {
        libc::fcntl(ruleset.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
    }

    let parent = open_path(root)?;
    let path_attr = PathBeneathAttr {
        allowed_access: handled,
        parent_fd: parent.as_raw_fd(),
    };
    let rc = unsafe {
        libc::syscall(
            SYS_ADD_RULE,
            ruleset.as_raw_fd(),
            LANDLOCK_RULE_PATH_BENEATH,
            &path_attr as *const PathBeneathAttr,
            0u32,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(Prepared { ruleset })
}

impl Prepared {
    /// Enforce the ruleset on the current (single-threaded) child.
    pub fn apply(&self) -> bool {
        let rc = unsafe { libc::syscall(SYS_RESTRICT_SELF, self.ruleset.as_raw_fd(), 0u32) };
        rc == 0
    }
}
