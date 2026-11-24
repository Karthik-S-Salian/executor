fn main() {
    let caps = sandbox::Capabilities::probe();
    println!("kernel:            {}", caps.kernel);
    println!("cgroup v2:          {}", caps.cgroup_v2);
    println!("cgroup delegated:   {}", caps.cgroup_delegated);
    println!("user namespaces:    {}", caps.userns);
    println!("seccomp:            {}", caps.seccomp);
    println!("Landlock ABI:       {}", caps.landlock_abi);
    println!("usable:             {}", caps.usable);
    for problem in &caps.problems {
        eprintln!("problem: {problem}");
    }
    if !caps.usable || caps.landlock_abi < 1 {
        std::process::exit(1);
    }
}
