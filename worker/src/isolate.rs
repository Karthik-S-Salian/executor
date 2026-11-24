use anyhow::{Result, anyhow};
use common::model::NatsSubmission;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// The worker is expected to run as a service with only the privileges isolate
/// needs; no `sudo` in the call chain.
fn isolate_bin() -> String {
    std::env::var("ISOLATE_BIN").unwrap_or_else(|_| "isolate".to_string())
}

/// Quote one user-supplied argument so it is passed literally to the command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'\''"#))
}

/// User-supplied option/argument fragments are split on whitespace and quoted
/// individually. This removes the old shell-injection surface.
fn quote_args(raw: &Option<String>) -> String {
    raw.as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct Sandbox {
    pub box_id: u32,
    pub boxdir: PathBuf,
    pub metadata_file: PathBuf,
    pub compile_metadata_file: PathBuf,
    pub stdin_file: PathBuf,
    pub stdout_file: PathBuf,
    pub stderr_file: PathBuf,
}

pub struct CompileOutcome {
    pub success: bool,
    pub timed_out: bool,
    pub output: Option<String>,
    pub message: Option<String>,
}

impl Sandbox {
    pub async fn new(box_id: u32) -> Result<Self> {
        let output = Command::new(isolate_bin())
            .args(["--cg", "-b", &box_id.to_string(), "--init"])
            .output()
            .await?;

        if !output.status.success() {
            return Err(anyhow!(
                "Failed to initialize isolate box {}: {}",
                box_id,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        // isolate prints the box directory itself (mounted as /box inside the
        // sandbox), not its parent.
        let boxdir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let workdir = boxdir
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| boxdir.clone());

        Ok(Self {
            box_id,
            boxdir,
            metadata_file: workdir.join("metadata.txt"),
            compile_metadata_file: workdir.join("compile_metadata.txt"),
            stdin_file: workdir.join("stdin.txt"),
            stdout_file: workdir.join("stdout.txt"),
            stderr_file: workdir.join("stderr.txt"),
        })
    }

    pub async fn compile(&self, submission: &NatsSubmission) -> Result<CompileOutcome> {
        let Some(compile_cmd) = &submission.language.compile_cmd else {
            return Ok(CompileOutcome {
                success: true,
                timed_out: false,
                output: None,
                message: None,
            });
        };

        let compile_script = self.boxdir.join("compile.sh");
        let options = quote_args(&submission.compiler_options);
        let cmd = if compile_cmd.contains("%s") {
            compile_cmd.replace("%s", &options)
        } else if options.is_empty() {
            compile_cmd.clone()
        } else {
            format!("{compile_cmd} {options}")
        };
        tokio::fs::write(&compile_script, format!("{cmd}\n")).await?;

        let output = Command::new(isolate_bin())
            .args([
                "--cg",
                "--silent",
                "-b",
                &self.box_id.to_string(),
                "-M",
                self.compile_metadata_file.to_str().unwrap(),
                "-i",
                "/dev/null",
                "-t",
                "10",
                "-x",
                "1",
                "-w",
                "20",
                "-k",
                "65536",
                "-m",
                "1048576",
                "-f",
                "65536",
                "--run",
                "--",
                "/bin/bash",
                "compile.sh",
            ])
            .current_dir(&self.boxdir)
            .output()
            .await?;

        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let text = text.trim().to_string();
        let output = (!text.is_empty()).then_some(text);

        let success = output_success(&self.compile_metadata_file).await;
        let metadata = read_metadata(&self.compile_metadata_file)
            .await
            .unwrap_or_default();

        Ok(CompileOutcome {
            success: success.0,
            timed_out: success.1,
            output,
            message: metadata.get("message").cloned(),
        })
    }

    pub async fn run(&self, submission: &NatsSubmission) -> Result<()> {
        let run_script = self.boxdir.join("run.sh");
        let args = quote_args(&submission.command_line_arguments);
        let cmd = if args.is_empty() {
            submission.language.run_cmd.clone()
        } else {
            format!("{} {}", submission.language.run_cmd, args)
        };
        tokio::fs::write(&run_script, format!("{cmd}\n")).await?;

        let cpu_time = submission.cpu_time_limit.unwrap_or(2.0);
        let extra_time = submission.cpu_extra_time.unwrap_or(1.0);
        let wall_time = submission
            .wall_time_limit
            .unwrap_or(cpu_time + extra_time + 1.0);
        let stack = submission.stack_limit.unwrap_or(65536);
        let memory = submission.memory_limit.unwrap_or(262144.0);
        let file_size = submission.max_file_size.unwrap_or(1024);
        let processes = submission.max_processes_and_or_threads.unwrap_or(50);
        let enable_network = submission.enable_network.unwrap_or(false);

        let mut command = Command::new(isolate_bin());
        command.args([
            "--cg",
            "--silent",
            "-b",
            &self.box_id.to_string(),
            "-M",
            self.metadata_file.to_str().unwrap(),
            "-t",
            &cpu_time.to_string(),
            "-x",
            &extra_time.to_string(),
            "-w",
            &wall_time.to_string(),
            "-k",
            &stack.to_string(),
            "-m",
            &memory.to_string(),
            "-f",
            &file_size.to_string(),
            "-p",
            &processes.to_string(),
        ]);

        if submission.redirect_stderr_to_stdout.unwrap_or(false) {
            command.arg("--stderr-to-stdout");
        }
        if enable_network {
            // `-n` is --open-files, not network.
            command.arg("--share-net");
        }

        command
            .args(["--run", "--", "/bin/bash", "run.sh"])
            .stdin(Stdio::from(
                tokio::fs::File::open(&self.stdin_file)
                    .await?
                    .into_std()
                    .await,
            ))
            .stdout(Stdio::from(
                tokio::fs::File::create(&self.stdout_file)
                    .await?
                    .into_std()
                    .await,
            ))
            .stderr(Stdio::from(
                tokio::fs::File::create(&self.stderr_file)
                    .await?
                    .into_std()
                    .await,
            ))
            .current_dir(&self.boxdir)
            .kill_on_drop(true);

        let status = command.status().await?;

        // isolate returns non-zero for TLE/RE too; the metadata file is the
        // source of truth. Only a missing metadata file is an engine failure.
        if !status.success() && !self.metadata_file.exists() {
            return Err(anyhow!(
                "isolate exited with {status} and wrote no metadata"
            ));
        }

        Ok(())
    }

    pub async fn read_metadata(&self) -> Result<HashMap<String, String>> {
        read_metadata(&self.metadata_file).await
    }

    pub async fn read_output(&self) -> Result<ProgramOutput> {
        let stdout = tokio::fs::read_to_string(&self.stdout_file).await.ok();
        let stderr = tokio::fs::read_to_string(&self.stderr_file).await.ok();

        let stdout = stdout.and_then(|s| (!s.trim().is_empty()).then_some(s));
        let stderr = stderr.and_then(|s| (!s.trim().is_empty()).then_some(s));

        Ok(ProgramOutput { stdout, stderr })
    }

    pub async fn cleanup(&self) {
        // isolate owns /var/lib/isolate; let it remove the box.
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(isolate_bin())
                .args(["--cg", "-b", &self.box_id.to_string(), "--cleanup"])
                .status(),
        )
        .await;

        match result {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => {
                eprintln!("isolate cleanup for box {} returned {status}", self.box_id)
            }
            Ok(Err(e)) => eprintln!("isolate cleanup for box {} failed: {e}", self.box_id),
            Err(_) => eprintln!("isolate cleanup for box {} timed out", self.box_id),
        }
    }
}

async fn read_metadata(path: &PathBuf) -> Result<HashMap<String, String>> {
    let contents = tokio::fs::read_to_string(path).await?;
    let mut map = HashMap::new();

    for line in contents.lines() {
        if let Some((key, val)) = line.split_once(':') {
            map.insert(key.trim().to_string(), val.trim().to_string());
        }
    }

    Ok(map)
}

/// isolate's exit code alone does not distinguish a compile error from a
/// timeout, so consult the metadata it wrote.
async fn output_success(metadata_file: &PathBuf) -> (bool, bool) {
    let metadata = read_metadata(metadata_file).await.unwrap_or_default();
    let timed_out = metadata.get("status").map(String::as_str) == Some("TO");
    let box_error = metadata.get("status").map(String::as_str) == Some("XX");
    let success = !timed_out && !box_error && metadata.get("status").is_none();
    (success, timed_out)
}

#[derive(Debug)]
pub struct ProgramOutput {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::quote_args;

    #[test]
    fn quotes_each_argument() {
        let raw = Some("-O2 && rm -rf /".to_string());
        assert_eq!(quote_args(&raw), "'-O2' '&&' 'rm' '-rf' '/'");
    }

    #[test]
    fn empty_args_stay_empty() {
        assert_eq!(quote_args(&None), "");
        assert_eq!(quote_args(&Some("   ".to_string())), "");
    }

    #[test]
    fn single_quotes_are_escaped() {
        assert_eq!(super::shell_quote("it's"), r#"'it'\''s'"#);
    }
}
