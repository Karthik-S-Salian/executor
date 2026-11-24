//! A small seccomp BPF deny-list, installed in the child before exec.
//!
//! This is deliberately a deny-list: a strict per-language allow-list is the
//! next step (Python/C/C++ first, Java later). Denying the namespace, mount,
//! ptrace and kernel-attack syscalls removes the obvious escape surface.

use std::collections::HashMap;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JGE_K: u16 = 0x35;
const BPF_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;

const RET_ALLOW: u32 = 0x7fff_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const RET_KILL_PROCESS: u32 = 0x8000_0000;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

const CLONE_NEWUSER: u32 = 0x1000_0000;

struct Asm {
    code: Vec<libc::sock_filter>,
    labels: HashMap<&'static str, usize>,
    fixups: Vec<(usize, Option<&'static str>, Option<&'static str>)>,
}

impl Asm {
    fn new() -> Self {
        Self {
            code: Vec::new(),
            labels: HashMap::new(),
            fixups: Vec::new(),
        }
    }

    fn stmt(&mut self, code: u16, k: u32) {
        self.code.push(libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        });
    }

    fn label(&mut self, name: &'static str) {
        self.labels.insert(name, self.code.len());
    }

    fn jump(&mut self, code: u16, k: u32, jt: Option<&'static str>, jf: Option<&'static str>) {
        let idx = self.code.len();
        self.code.push(libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        });
        self.fixups.push((idx, jt, jf));
    }

    fn finish(mut self) -> Vec<libc::sock_filter> {
        for (idx, jt, jf) in self.fixups {
            for (target, slot) in [(jt, true), (jf, false)] {
                let Some(name) = target else { continue };
                let target = *self
                    .labels
                    .get(name)
                    .unwrap_or_else(|| panic!("unknown BPF label {name}"));
                let offset = target as isize - idx as isize - 1;
                assert!((0..=255).contains(&offset), "BPF jump out of range: {name}");
                if slot {
                    self.code[idx].jt = offset as u8;
                } else {
                    self.code[idx].jf = offset as u8;
                }
            }
        }
        self.code
    }
}

fn denied() -> Vec<u32> {
    let mut calls: Vec<u32> = vec![
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_bpf,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_perf_event_open,
        libc::SYS_userfaultfd,
        libc::SYS_kexec_load,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_acct,
        libc::SYS_quotactl,
        libc::SYS_syslog,
        libc::SYS_name_to_handle_at,
        libc::SYS_open_by_handle_at,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_fsconfig,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_pidfd_getfd,
    ]
    .into_iter()
    .map(|n| n as u32)
    .collect();

    // mount_setattr is 442 on both x86_64 and aarch64; libc may not expose it.
    calls.push(442);

    calls
}

fn build() -> Vec<libc::sock_filter> {
    let mut asm = Asm::new();

    // Reject binaries built for another architecture (and x32).
    asm.stmt(BPF_LD_W_ABS, 4);
    asm.jump(BPF_JEQ_K, AUDIT_ARCH, Some("arch_ok"), Some("kill"));
    asm.label("kill");
    asm.stmt(BPF_RET_K, RET_KILL_PROCESS);
    asm.label("arch_ok");
    asm.stmt(BPF_LD_W_ABS, 0);
    asm.jump(BPF_JGE_K, 0x4000_0000, Some("kill"), Some("syscalls"));

    asm.label("syscalls");
    let calls = denied();
    for (index, call) in calls.iter().copied().enumerate() {
        let next: &'static str = if index + 1 == calls.len() {
            "clone_check"
        } else {
            // Labels must be static; use a fixed-size table instead.
            CONTINUE[index]
        };
        asm.jump(BPF_JEQ_K, call, Some("deny"), Some(next));
        asm.label(next);
    }

    asm.label("clone_check");
    asm.jump(
        BPF_JEQ_K,
        libc::SYS_clone as u32,
        Some("clone_args"),
        Some("clone3"),
    );
    asm.label("clone_args");
    asm.stmt(BPF_LD_W_ABS, 16); // args[0] of clone(2)
    asm.jump(BPF_JSET_K, CLONE_NEWUSER, Some("deny"), Some("clone3"));

    // glibc/Go probe clone3 and fall back on ENOSYS; returning ENOSYS keeps
    // threaded runtimes working while removing the struct-in-memory escape.
    asm.label("clone3");
    asm.stmt(BPF_LD_W_ABS, 0);
    asm.jump(
        BPF_JEQ_K,
        libc::SYS_clone3 as u32,
        Some("enosys"),
        Some("allow"),
    );

    asm.label("enosys");
    asm.stmt(BPF_RET_K, RET_ERRNO | libc::ENOSYS as u32);
    asm.label("deny");
    asm.stmt(BPF_RET_K, RET_ERRNO | libc::EPERM as u32);
    asm.label("allow");
    asm.stmt(BPF_RET_K, RET_ALLOW);

    asm.finish()
}

/// Labels used by the generated deny chain. Static so the assembler can resolve
/// them without allocating names.
static CONTINUE: [&str; 64] = [
    "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12", "c13", "c14",
    "c15", "c16", "c17", "c18", "c19", "c20", "c21", "c22", "c23", "c24", "c25", "c26", "c27",
    "c28", "c29", "c30", "c31", "c32", "c33", "c34", "c35", "c36", "c37", "c38", "c39", "c40",
    "c41", "c42", "c43", "c44", "c45", "c46", "c47", "c48", "c49", "c50", "c51", "c52", "c53",
    "c54", "c55", "c56", "c57", "c58", "c59", "c60", "c61", "c62", "c63",
];

/// The filter is built in the worker process; the child only installs it.
pub struct Prepared {
    filter: Vec<libc::sock_filter>,
}

impl Prepared {
    pub fn build() -> Self {
        Self { filter: build() }
    }

    /// Install with raw prctl calls; no allocation after clone. Returns false on
    /// failure so the child can report a static error.
    pub fn install(&self) -> bool {
        let mut program = libc::sock_fprog {
            len: self.filter.len() as u16,
            filter: self.filter.as_ptr() as *mut libc::sock_filter,
        };

        let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if rc != 0 {
            return false;
        }

        let rc = unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER as libc::c_ulong,
                &mut program as *mut libc::sock_fprog as libc::c_ulong,
                0,
                0,
            )
        };
        rc == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_builds_and_terminates() {
        let filter = Prepared::build().filter;
        assert!(filter.len() > 10);
        let last = filter.last().unwrap();
        assert_eq!(last.code, BPF_RET_K);
        assert_eq!(last.k, RET_ALLOW);
    }
}
