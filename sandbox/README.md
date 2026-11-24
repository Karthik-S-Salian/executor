# `sandbox`

Rootless confinement for untrusted code. It replaces the `isolate` CLI with an
in-process Linux sandbox: user/mount/pid/net/ipc/uts namespaces, a per-run
cgroup v2 sub-cgroup, rlimits, Landlock and a seccomp deny-list. No setuid
binary, no root, no daemon.

> **Status: P0/P1, not yet exercised on a real Linux kernel.** The crate
> cross-compiles for `x86_64-unknown-linux-gnu` and
> `aarch64-unknown-linux-gnu`, and the host checks and unit tests pass, but the
> syscall paths have not been run on Linux in this workspace (the development
> machine is macOS). Run `sandbox-probe` and the ignored integration tests on
> the target worker image before routing submissions to it. The worker still
> uses the old isolate backend until that check passes.

## Host requirements

Fail closed: `Sandbox::create` refuses to run when any of these is missing.

- Linux kernel with unprivileged user namespaces (`max_user_namespaces > 0`,
  `unprivileged_userns_clone=1` where that sysctl exists).
- cgroup v2 mounted at `/sys/fs/cgroup` with `cpu`, `memory` and `pids`
  controllers delegated to the worker.
- Seccomp filter mode and Landlock ABI >= 1 (kernel 5.13+; `memory.peak` needs
  5.19+, `cgroup.kill` needs 5.14+ — older kernels fall back where possible).
- A writable base directory on a filesystem with enough space for source,
  compiler output and captured stdout/stderr.
- Native/rootless mode must run the worker as a non-root user. Mapping
  container uid 0 to host uid 0 would give the sandboxed program the host root
  identity (without host capabilities), so the namespace boundary alone is not
  the intended protection.

On Ubuntu 24.04, AppArmor restricts unprivileged user namespaces. The sandbox
reports this at probe time; allow the worker binary with an AppArmor profile
rather than disabling the kernel-wide restriction.

### systemd unit

The worker must not live in the cgroup that enables controllers for its
children (cgroup v2 no-internal-process rule). With systemd 254+:

```ini
[Service]
Delegate=yes
DelegateSubgroup=supervisor
ExecStart=/usr/local/bin/executor-worker
```

This puts the worker in `…/executor.service/supervisor` and leaves
`…/executor.service` free to be the cgroup parent. Older systemd: run the worker
via `systemd-run --user --scope --slice=...` and arrange a delegated, empty
parent cgroup, or use a privileged container for the convenience deployment.

### Containers

A privileged container on a dedicated VM works, but is **not rootless**: the
container gets host-level capabilities and the VM is the real boundary. gVisor,
Kata or Firecracker are the stronger options for container deployments and are
not implemented here. A worker container needs its own cgroup namespace and a
writable delegated subtree; see the probe output.

## Usage

```rust
use sandbox::{Limits, RunSpec, Sandbox, SandboxConfig};

let sandbox = Sandbox::create(SandboxConfig {
    base_dir: "/run/executor".into(),
    require_landlock: true,
    ..SandboxConfig::default()
})?;

sandbox.write_file("main.py", b"print(input())")?;

let outcome = sandbox.run(&RunSpec {
    program: "/usr/bin/python3",
    args: &["/main.py".to_string()],
    stdin: b"hello\n",
    limits: Limits {
        cpu_time_secs: 2.0,
        wall_time_secs: 4.0,
        memory_kb: 262_144,
        ..Limits::default()
    },
})?;

assert_eq!(outcome.exit_code, Some(0));
assert_eq!(outcome.stdout, b"hello\n");
```

One `Sandbox` is one root filesystem; each `run` gets a fresh cgroup, so a
compile step and each test case are accounted independently. Call `cleanup()`
(or drop it) to remove the root and cgroup.

Environment is explicit: nothing is inherited from the worker. Defaults are
`PATH`, `HOME=/tmp`, `TMPDIR=/tmp`, `LANG=C.UTF-8`. Network is a new empty net
namespace by default; `network: true` is only for trusted tooling and should
not be exposed through the API.

## Verification on the target

```sh
cargo build -p sandbox --release --bin sandbox-probe
sudo -u executor ./target/release/sandbox-probe   # must exit 0

sudo -u executor cargo test -p sandbox --test linux -- --ignored --nocapture
```

The integration tests cover stdout/stdin, wall-clock kill, read-only `/etc` and
the memory cgroup. They are ignored by default because they need a prepared
host, not because they are optional.

## Containment notes

- **Mounts**: system directories are bind-mounted read-only. The box root is
  writable. `/dev` is a tmpfs with only null/zero/full/random/urandom, `/proc`
  is a fresh procfs in the new pid namespace, `/tmp` and `/dev/shm` are tmpfs.
  `pivot_root` detaches the host root.
- **Credentials**: uid/gid 0 inside the user namespace maps to the worker's
  unprivileged uid/gid; no host capabilities are gained. All capability sets
  are effectively empty outside that namespace, `no_new_privs` is set and
  seccomp is installed before `execve`. The cloned child performs no allocation
  after clone (mount targets, argv/envp, seccomp filter and Landlock ruleset are
  built in the worker first), which avoids allocator-lock deadlocks in a
  multithreaded worker.
- **cgroups**: `memory.max`, `memory.swap.max=0`, `pids.max`,
  `memory.oom.group=1`. CPU is enforced by polling `cpu.stat` plus
  `RLIMIT_CPU`; wall time by a poll loop that kills the cgroup and process
  group.
- **seccomp**: a shared deny-list (mount, namespace, ptrace, BPF, keyring,
  io_uring, module loading, handle-based file access, …). `clone3` returns
  `ENOSYS` so glibc/Go fall back to `clone`, and `clone(CLONE_NEWUSER)` is
  rejected. This is not yet a per-language allow-list.
- **Landlock**: one path-beneath rule for `/`, so nothing outside the pivoted
  root is reachable even if a path escape is found. It also denies new
  filesystem access through inherited namespaces.
- **Output**: stdout/stderr are captured to regular files and truncated to
  `output_limit_bytes`.
- **Untrusted write paths**: source files are written with `O_NOFOLLOW` and each
  parent directory is checked for symlinks, so a file planted by an earlier run
  cannot redirect the worker into a host path.

## Known gaps before public exposure

1. Per-language seccomp allow-lists (Python/C/C++ first, then Java) and a
   landlock/seccomp escape test suite.
2. Disk quota for the writable root (per-file `RLIMIT_FSIZE` exists; total disk
   does not). Put `base_dir` on a size-limited filesystem or tmpfs.
3. Multi-case executor contract (`cases[]`, raw outputs/metrics) and the
   writer-side judging split; hidden inputs/outputs must not reach the worker.
4. Compile/run profiles per language. Java needs a larger `pids.max`, cgroup
   memory instead of `RLIMIT_AS` (already the case), `/tmp`, `/proc/self`,
   `/dev/urandom`, JIT `mprotect` and internally generated SIGSEGV — the shared
   deny-list is intended to allow these, but it must be tested.
5. Go and Node are expected to work with this profile but are not verified.
6. `/sys` is not mounted. Runtimes that probe cgroup/device details fall back;
   if one needs sysfs, bind only the specific files it needs, never all of
   `/sys`.
7. Cleanup of a crashed worker: job cgroups/directories under the base dir are
   not garbage-collected at startup yet.
