//! Real-kernel containment tests. They need Linux, unprivileged user
//! namespaces, a delegated cgroup v2 subtree and Landlock; run explicitly:
//!
//! ```sh
//! sudo -u executor cargo test -p sandbox --test linux -- --ignored --nocapture
//! ```

#![cfg(target_os = "linux")]

use std::time::{SystemTime, UNIX_EPOCH};

use sandbox::{Limits, RunSpec, Sandbox, SandboxConfig};

fn sandbox() -> Sandbox {
    let base = std::env::temp_dir().join(format!(
        "sandbox-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config = SandboxConfig {
        base_dir: base,
        require_landlock: true,
        ..SandboxConfig::default()
    };
    Sandbox::create(config).expect("host must support the sandbox")
}

fn limits() -> Limits {
    Limits {
        cpu_time_secs: 2.0,
        wall_time_secs: 5.0,
        memory_kb: 262_144,
        ..Limits::default()
    }
}

fn sh(sandbox: &Sandbox, script: &str, limits: Limits) -> sandbox::Outcome {
    sandbox
        .run(&RunSpec {
            program: "/bin/sh",
            args: &["-c".to_string(), script.to_string()],
            stdin: b"",
            limits,
        })
        .expect("run should not fail at the sandbox level")
}

#[test]
#[ignore = "requires Linux userns + delegated cgroup v2"]
fn runs_and_captures_stdout() {
    let sandbox = sandbox();
    let outcome = sh(&sandbox, "echo hello", limits());
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.stdout, b"hello\n");
    sandbox.cleanup().unwrap();
}

#[test]
#[ignore = "requires Linux userns + delegated cgroup v2"]
fn passes_stdin() {
    let sandbox = sandbox();
    let outcome = sandbox
        .run(&RunSpec {
            program: "/bin/cat",
            args: &[],
            stdin: b"abc\n",
            limits: limits(),
        })
        .unwrap();
    assert_eq!(outcome.stdout, b"abc\n");
    sandbox.cleanup().unwrap();
}

#[test]
#[ignore = "requires Linux userns + delegated cgroup v2"]
fn wall_timeout_kills_the_process() {
    let sandbox = sandbox();
    let mut limits = limits();
    limits.wall_time_secs = 0.5;
    let outcome = sh(&sandbox, "sleep 30", limits);
    assert!(outcome.timed_out, "expected wall-clock timeout");
    sandbox.cleanup().unwrap();
}

#[test]
#[ignore = "requires Linux userns + delegated cgroup v2"]
fn etc_is_read_only() {
    let sandbox = sandbox();
    let outcome = sh(&sandbox, "echo x > /etc/sandbox-test", limits());
    assert_ne!(outcome.exit_code, Some(0));
    assert!(!std::path::Path::new("/etc/sandbox-test").exists());
    sandbox.cleanup().unwrap();
}

#[test]
#[ignore = "requires Linux userns + delegated cgroup v2"]
fn write_file_refuses_symlinks() {
    use std::os::unix::fs::symlink;

    let sandbox = sandbox();
    let link = sandbox.root().join("link");
    symlink("/tmp/sandbox-escape-test", &link).unwrap();
    assert!(sandbox.write_file("link", b"x").is_err());

    let dir_link = sandbox.root().join("dir");
    symlink("/tmp", &dir_link).unwrap();
    assert!(sandbox.write_file("dir/x", b"x").is_err());
    sandbox.cleanup().unwrap();
}

#[test]
#[ignore = "requires Linux userns + delegated cgroup v2"]
fn memory_limit_is_enforced() {
    let sandbox = sandbox();
    let mut limits = limits();
    limits.memory_kb = 65_536;
    limits.wall_time_secs = 10.0;
    let outcome = sh(
        &sandbox,
        "python3 -c 'x = bytearray(256 * 1024 * 1024)'",
        limits,
    );
    assert!(
        outcome.memory_limit_hit || outcome.exit_code != Some(0),
        "expected the allocation to be killed or fail"
    );
    sandbox.cleanup().unwrap();
}
