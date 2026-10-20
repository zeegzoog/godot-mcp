use crate::error::{McpError, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct CliRunner {
    godot_binary: PathBuf,
}

impl CliRunner {
    pub fn new(godot_binary: PathBuf) -> Self {
        Self { godot_binary }
    }

    /// The Godot executable this runner was configured with.
    pub fn binary(&self) -> &Path {
        &self.godot_binary
    }

    /// Retrieve engine version directly via CLI
    pub async fn get_version(&self) -> Result<String> {
        let mut command = Command::new(&self.godot_binary);
        command
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(VERSION_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "'{} --version' did not finish within {}s",
                    self.godot_binary.display(),
                    VERSION_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!(
                    "Failed to invoke Godot executable at '{}': {e}",
                    self.godot_binary.display()
                ))
            })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(McpError::Process(format!(
                "Godot --version exited with error: {}",
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    /// Launch Godot Editor for a designated project directory
    pub async fn launch_editor(&self, project_path: &Path) -> Result<u32> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--editor").arg("--path").arg(project_path);
        spawn_detached(&mut cmd, "Godot Editor")
    }

    /// Run the game project or specific scene.
    ///
    /// The game's own output must never reach this process's stdout, which carries the MCP
    /// JSON-RPC stream, so all standard streams are detached.
    pub async fn run_project(&self, project_path: &Path, scene: Option<&str>) -> Result<u32> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--path").arg(project_path);
        if let Some(scene_file) = scene {
            cmd.arg(scene_file);
        }
        spawn_detached(&mut cmd, "Godot project")
    }

    /// Validate a GDScript file syntax using headless Godot check
    pub async fn validate_script(
        &self,
        project_path: Option<&Path>,
        script_path: &Path,
    ) -> Result<String> {
        let mut cmd = Command::new(&self.godot_binary);
        cmd.arg("--headless").arg("--check-only");

        if let Some(proj) = project_path {
            cmd.arg("--path").arg(proj);
        }

        cmd.arg("--script")
            .arg(script_path)
            .stdin(Stdio::null())
            .kill_on_drop(true);

        let output = timeout(VALIDATE_TIMEOUT, cmd.output())
            .await
            .map_err(|_| {
                McpError::Process(format!(
                    "Godot script validation did not finish within {}s",
                    VALIDATE_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                McpError::Process(format!("Failed to run Godot script validation: {e}"))
            })?;

        let stdout = strip_banner(&String::from_utf8_lossy(&output.stdout));
        let stderr = strip_banner(&String::from_utf8_lossy(&output.stderr));

        if output.status.success() && stderr.is_empty() {
            Ok("GDScript validation passed successfully with no errors.".into())
        } else {
            let mut report = Vec::new();
            if !stdout.is_empty() {
                report.push(format!("Stdout: {stdout}"));
            }
            if !stderr.is_empty() {
                report.push(format!("Stderr: {stderr}"));
            }
            if report.is_empty() {
                report.push(format!("Godot exited with {}", output.status));
            }
            Err(McpError::Validation(report.join("\n")))
        }
    }
}

fn spawn_detached(command: &mut Command, what: &str) -> Result<u32> {
    // Ctrl+C in the MCP client's terminal reaches every process attached to its console
    // (Windows) or in its foreground process group (Unix), which would close the editor with
    // unsaved work. No console and a process group of its own keep the child out of reach.
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    make_std_handles_private();

    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| McpError::Process(format!("Failed to spawn {what} process: {e}")))?;
    child
        .id()
        .ok_or_else(|| McpError::Process(format!("Failed to obtain {what} process ID")))
}

/// Windows lets a spawned process inherit every inheritable handle of its parent, whatever it
/// is given as its own standard streams. The server's stdin, stdout and stderr are pipes of the
/// MCP client, so a launched editor would keep them open for as long as it runs, and a client
/// that waits for the server's stderr to close would wait for the editor instead.
#[cfg(windows)]
pub(crate) fn make_std_handles_private() {
    use windows_sys::Win32::Foundation::SetHandleInformation;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: both calls only read or flag handles owned by this process. A missing or
        // invalid handle makes `SetHandleInformation` fail, which is harmless here: the
        // handle is then simply not protected, as before.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Drops the engine's start-up banner ("Godot Engine v4.x ...") from captured output.
fn strip_banner(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with("Godot Engine v"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_banner;

    #[test]
    fn banner_is_removed_and_the_rest_is_kept() {
        let raw = "Godot Engine v4.8.dev7.official - https://godotengine.org\n\nSCRIPT ERROR: x\n";
        assert_eq!(strip_banner(raw), "SCRIPT ERROR: x");
        assert_eq!(strip_banner("Godot Engine v4.5\n"), "");
    }

    /// Ctrl+C in the MCP client's terminal signals its whole process group; a launched editor
    /// must not be part of it.
    #[cfg(unix)]
    #[tokio::test]
    async fn launched_processes_get_their_own_process_group() {
        use super::CliRunner;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        fn process_group_of(pid: &str) -> String {
            let output = std::process::Command::new("ps")
                .args(["-o", "pgid=", "-p", pid])
                .output()
                .expect("run ps");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        let dir = tempfile::tempdir().expect("temp dir");
        let report = dir.path().join("pgid.txt");
        let script = dir.path().join("fake_godot.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nps -o pgid= -p $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'\n",
                report.display()
            ),
        )
        .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let pid = CliRunner::new(script)
            .launch_editor(dir.path())
            .await
            .expect("spawn");

        for _ in 0..100 {
            if report.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let child_group = std::fs::read_to_string(&report).expect("child reported its group");
        assert_eq!(child_group.trim(), pid.to_string());
        assert_ne!(
            child_group.trim(),
            process_group_of(&std::process::id().to_string())
        );
    }
}
