use std::fs;

use super::cgroup;

/// What this host can actually enforce. Callers must treat `usable == false` as
/// fatal: running untrusted code without these primitives is not containment.
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
        let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".into());

        let cgroup_v2 = cgroup::is_cgroup_v2();
        let (cgroup_delegated, cgroup_problem) = match cgroup::delegation_available() {
            Ok(()) => (true, None),
            Err(reason) => (false, Some(format!("cgroup delegation: {reason}"))),
        };

        let (userns, userns_problem) = probe_userns();
        let seccomp = probe_seccomp();
        let landlock_abi = super::landlock::abi();

        let mut problems = Vec::new();
        if !cgroup_v2 {
            problems.push("cgroup v2 is not mounted at /sys/fs/cgroup".into());
        }
        if let Some(problem) = cgroup_problem {
            problems.push(problem);
        }
        if let Some(problem) = userns_problem {
            problems.push(problem);
        }
        if !seccomp {
            problems.push("seccomp filter actions are unavailable".into());
        }
        if landlock_abi < 1 {
            problems.push("Landlock ABI < 1 (kernel 5.13+ required)".into());
        }

        let usable = cgroup_v2 && cgroup_delegated && userns && seccomp;

        Self {
            kernel,
            cgroup_v2,
            cgroup_delegated,
            userns,
            seccomp,
            landlock_abi,
            usable,
            problems,
        }
    }
}

fn probe_userns() -> (bool, Option<String>) {
    let mut problems: Vec<String> = Vec::new();

    if let Ok(value) = fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        && value.trim() != "1"
    {
        problems.push("unprivileged_userns_clone is disabled".into());
    }

    match fs::read_to_string("/proc/sys/user/max_user_namespaces") {
        Ok(value) => {
            if value.trim().parse::<u64>().unwrap_or(0) == 0 {
                problems.push("max_user_namespaces is 0".into());
            }
        }
        Err(_) => problems.push("cannot read max_user_namespaces".into()),
    }

    if let Ok(value) = fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        && value.trim() == "1"
    {
        problems.push(
            "AppArmor restricts unprivileged user namespaces; add an AppArmor profile \
             for the worker or set kernel.apparmor_restrict_unprivileged_userns=0"
                .into(),
        );
    }

    if problems.is_empty() {
        (true, None)
    } else {
        (false, Some(problems.join("; ")))
    }
}

fn probe_seccomp() -> bool {
    fs::read_to_string("/proc/sys/kernel/seccomp/actions_avail")
        .map(|value| value.split_whitespace().any(|a| a == "errno"))
        .unwrap_or(false)
}
