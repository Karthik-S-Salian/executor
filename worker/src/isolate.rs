use anyhow::{Result, anyhow};
use common::model::NatsSubmission;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

pub struct Sandbox {
    pub box_id: u32,
    pub workdir: PathBuf,
    pub boxdir: PathBuf,
    pub tmpdir: PathBuf,
    pub metadata_file: PathBuf,
    pub source_file: PathBuf,
    pub stdin_file: PathBuf,
    pub stdout_file: PathBuf,
    pub stderr_file: PathBuf,
}

use std::collections::HashMap;
use tokio::fs;

impl Sandbox {
    pub fn new(box_id: u32) -> Result<Self> {
        let isolate_cmd = format!("sudo isolate --cg -b {} --init", box_id);
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&isolate_cmd)
            .output()?;

        if !output.status.success() {
            return Err(anyhow!("Failed to initialize isolate"));
        }

        let workdir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let workdir = PathBuf::from(workdir);
        let boxdir = workdir.join("box");
        let tmpdir = workdir.join("tmp");

        Ok(Self {
            box_id,
            boxdir: boxdir.clone(),
            workdir: workdir.clone(),
            tmpdir: tmpdir.clone(),
            metadata_file: workdir.join("metadata.txt"),
            source_file: boxdir.join("source"),
            stdin_file: workdir.join("stdin.txt"),
            stdout_file: workdir.join("stdout.txt"),
            stderr_file: workdir.join("stderr.txt"),
        })
    }

    pub async fn compile(&self, submission: &NatsSubmission) -> Result<bool> {
        if let Some(compile_cmd) = &submission.language.compile_cmd {
            let compile_script = self.boxdir.join("compile.sh");
            let mut file = File::create(&compile_script)?;
            let sanitized = submission
                .compiler_options
                .clone()
                .unwrap_or_default()
                .replace(['$', '&', ';', '<', '>', '|', '`'], "");
            writeln!(file, "{}", compile_cmd.replace("%s", &sanitized))?;

            let output = Command::new("sudo")
                .args([
                    "isolate",
                    "--cg",
                    "-b",
                    &self.box_id.to_string(),
                    "-M",
                    self.metadata_file.to_str().unwrap(),
                    "--stderr-to-stdout",
                    "-i",
                    "/dev/null",
                    "-t",
                    "2",
                    "-x",
                    "0",
                    "-w",
                    "4",
                    "-k",
                    "67108864",
                    "-m",
                    "262144",
                    "-f",
                    "1024",
                    "--run",
                    "--",
                    "/bin/bash",
                    "compile.sh",
                ])
                .current_dir(&self.boxdir)
                .output()
                .await?;

            let success = output.status.success();
            Ok(success)
        } else {
            Ok(true)
        }
    }

    pub async fn run(&self, submission: &NatsSubmission) -> Result<()> {
        let run_script = self.boxdir.join("run.sh");
        let mut file = File::create(&run_script)?;
        let args = submission
            .command_line_arguments
            .clone()
            .unwrap_or_default()
            .replace(['$', '&', ';', '<', '>', '|', '`'], "");
        writeln!(file, "{} {}", submission.language.run_cmd, args)?;

        let mut command = Command::new("sudo");
        command.args([
            "isolate",
            "--cg",
            "--silent",
            "-b",
            &self.box_id.to_string(),
            "-M",
            self.metadata_file.to_str().unwrap(),
            "--stderr-to-stdout",
            "-t",
            &submission.cpu_time_limit.unwrap_or(2.0).to_string(),
            "-x",
            &submission.cpu_extra_time.unwrap_or(1.0).to_string(),
            "-w",
            &submission.wall_time_limit.unwrap_or(4.0).to_string(),
            "-k",
            &submission.stack_limit.unwrap_or(67108864).to_string(),
            &format!(
                "-p{}",
                submission.max_processes_and_or_threads.unwrap_or(50)
            ),
            "-m",
            &submission.memory_limit.unwrap_or(262144.0).to_string(),
            "-f",
            &submission.max_file_size.unwrap_or(1024).to_string(),
            "--run",
            "--",
            "/bin/bash",
            "run.sh",
        ]);

        command
            .stdin(Stdio::from(File::open(&self.stdin_file)?))
            .stdout(Stdio::from(File::create(&self.stdout_file)?))
            .stderr(Stdio::from(File::create(&self.stderr_file)?))
            .current_dir(&self.boxdir);

        command.kill_on_drop(true);

        let _status = command.output().await?;

        Ok(())
    }

    pub async fn read_metadata(&self) -> Result<HashMap<String, String>> {
        let contents = fs::read_to_string(&self.metadata_file).await?;
        let mut map = HashMap::new();

        for line in contents.lines() {
            if let Some((key, val)) = line.split_once(':') {
                map.insert(key.trim().to_string(), val.trim().to_string());
            }
        }

        Ok(map)
    }

    pub async fn read_output(&self) -> Result<ProgramOutput> {
        let stdout = tokio::fs::read_to_string(&self.stdout_file).await.ok();
        let stderr = tokio::fs::read_to_string(&self.stderr_file).await.ok();

        let stdout = stdout.and_then(|s| if s.trim().is_empty() { None } else { Some(s) });
        let stderr = stderr.and_then(|s| if s.trim().is_empty() { None } else { Some(s) });

        Ok(ProgramOutput { stdout, stderr })
    }

    pub async fn cleanup(&self) -> Result<()> {
        let _ = Command::new("sudo")
            .args(["rm", "-rf", self.boxdir.to_str().unwrap()])
            .status()
            .await;

        let _ = Command::new("sudo")
            .args(["rm", "-rf", self.tmpdir.to_str().unwrap()])
            .status()
            .await;

        let _ = Command::new("sudo")
            .args(["rm", "-rf", self.metadata_file.to_str().unwrap()])
            .status()
            .await;

        Command::new("sudo")
            .args(["isolate","--cg", "-b", &self.box_id.to_string(), "--cleanup"])
            .status()
            .await?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct ProgramOutput {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}
