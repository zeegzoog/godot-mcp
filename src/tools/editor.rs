//! Closing a Godot editor: through its bridge when it is connected to the project, else the
//! editor this server launched for the project, by its process. An editor that has another
//! project open is never touched.

use super::{ToolManager, bridge_error_result, str_arg};
use crate::error::McpError;
use crate::paths::same_directory;
use crate::protocol::ToolResult;
use crate::util::lock;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::process::Child;

const DEFAULT_WAIT_SECONDS: u64 = 30;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// An editor started by `godot_launch_editor`, kept so its exit can be seen and, without a
/// bridge, so it can be stopped.
struct LaunchedEditor {
    pid: u32,
    dir: PathBuf,
    child: Child,
}

/// The editors this server launched that may still run.
#[derive(Default)]
pub(super) struct LaunchedEditors(Mutex<Vec<LaunchedEditor>>);

impl LaunchedEditors {
    pub(super) fn add(&self, pid: u32, dir: PathBuf, child: Child) {
        lock(&self.0).push(LaunchedEditor { pid, dir, child });
    }

    /// The process id of a running editor launched for `dir`. Exited ones are forgotten.
    fn running_for(&self, dir: &Path) -> Option<u32> {
        let mut editors = lock(&self.0);
        editors.retain_mut(|editor| matches!(editor.child.try_wait(), Ok(None)));
        editors
            .iter()
            .find(|editor| same_directory(&editor.dir, dir))
            .map(|editor| editor.pid)
    }

    /// `Some(true)` once a launched editor with this id has exited, `None` when the server did
    /// not launch it. A launched editor is waited for here, so it never lingers as a zombie.
    fn exited(&self, pid: u32) -> Option<bool> {
        let mut editors = lock(&self.0);
        let editor = editors.iter_mut().find(|editor| editor.pid == pid)?;
        Some(!matches!(editor.child.try_wait(), Ok(None)))
    }

    /// Stops a launched editor: SIGTERM on Unix, so the engine can shut down, and a kill when
    /// `force` is set or on Windows.
    fn stop(&self, pid: u32, force: bool) {
        let mut editors = lock(&self.0);
        let Some(editor) = editors.iter_mut().find(|editor| editor.pid == pid) else {
            return;
        };
        // Once reaped, the id may already belong to another process.
        if !matches!(editor.child.try_wait(), Ok(None)) {
            return;
        }
        #[cfg(unix)]
        if !force {
            if let Ok(id) = i32::try_from(pid) {
                // SAFETY: kill only sends a signal to the editor this server spawned and still
                // holds unreaped, so the id cannot belong to another process.
                unsafe { libc::kill(id, libc::SIGTERM) };
                return;
            }
        }
        #[cfg(windows)]
        let _ = force;
        let _ = editor.child.start_kill();
    }
}

impl ToolManager {
    pub(super) async fn handle_close_editor(&self, args: Value) -> ToolResult {
        let discard = args
            .get("discard")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let wait = Duration::from_secs(
            args.get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_WAIT_SECONDS),
        );
        // The project the call names, else the one the server was started in. With neither,
        // the connected editor's own project.
        let target = match self.host_project(&args) {
            Ok(dir) => Some(dir),
            Err(_) if str_arg(&args, "project_path").is_none() => None,
            Err(result) => return result,
        };

        let other_editor = match self
            .bridge
            .send_command("get_project_info", json!({}))
            .await
        {
            Ok(info) => {
                let Some(dir) = info.get("project_dir").and_then(Value::as_str) else {
                    return ToolResult::error(
                        "The editor did not report which project it has open, so it was not closed",
                        &["Update the Godot MCP Bridge addon to the same version as the server"],
                    );
                };
                match &target {
                    Some(target) if !same_directory(target, Path::new(dir)) => {
                        Some(dir.to_string())
                    }
                    _ => return self.close_through_bridge(dir, discard, wait).await,
                }
            }
            Err(McpError::BridgeUnavailable(_)) => None,
            Err(error) => return bridge_error_result(&error),
        };

        let Some(target) = target else {
            return ToolResult::error(
                "No project_path was given, the server's working directory holds no project, and no editor is connected",
                &["Pass project_path: the directory of the project whose editor should close"],
            );
        };
        self.close_launched(&target, other_editor.as_deref(), discard, wait)
            .await
    }

    async fn close_through_bridge(&self, dir: &str, discard: bool, wait: Duration) -> ToolResult {
        let answer = match self
            .bridge
            .send_command("close_editor", json!({"discard": discard}))
            .await
        {
            Ok(answer) => answer,
            Err(error) => return bridge_error_result(&error),
        };
        let listed = |key: &str| -> Vec<String> {
            answer
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .map_or_else(|| item.to_string(), str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let scenes = listed("unsaved_scenes");
        let scripts = listed("unsaved_scripts");
        if answer.get("closing").and_then(Value::as_bool) != Some(true) {
            return ToolResult::error(
                format!(
                    "The editor for '{dir}' was not closed: it has unsaved changes.{}",
                    unsaved_listing(&scenes, &scripts)
                ),
                &[
                    "Save the scenes with godot_save_scene (it saves the edited scene) or ask the person to save them in the editor, then close again",
                    "Or pass discard: true to close the editor and lose these changes",
                ],
            );
        }
        let Some(pid) = answer
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
        else {
            return ToolResult::error(
                format!(
                    "The editor for '{dir}' was asked to quit but did not report its process id, so its exit could not be checked"
                ),
                &["Update the Godot MCP Bridge addon to the same version as the server"],
            );
        };
        let discarded = listed("discarded_scenes")
            .into_iter()
            .chain(listed("discarded_scripts"))
            .collect::<Vec<_>>();
        let started = Instant::now();
        if !self.wait_for_exit(pid, wait).await {
            return ToolResult::error(
                format!(
                    "The editor for '{dir}' (PID {pid}) was asked to quit but still runs after {}s",
                    wait.as_secs()
                ),
                &[
                    "Call godot_close_editor again with a larger timeout_seconds",
                    "Look at the editor window: a dialog may be holding it open",
                ],
            );
        }
        let mut text = format!(
            "Closed the Godot editor for '{dir}': PID {pid} exited after {:.1}s.",
            started.elapsed().as_secs_f64()
        );
        if !discarded.is_empty() {
            text.push_str(&format!(
                " Discarded unsaved changes in: {}.",
                discarded.join(", ")
            ));
        }
        ToolResult::text(text)
    }

    async fn close_launched(
        &self,
        target: &Path,
        other_editor: Option<&str>,
        discard: bool,
        wait: Duration,
    ) -> ToolResult {
        let shown = target.display();
        let Some(pid) = self.editors.running_for(target) else {
            let connected = match other_editor {
                Some(dir) => format!("the editor connected to the bridge has '{dir}' open"),
                None => "no editor is connected to the bridge".to_string(),
            };
            return ToolResult::error(
                format!(
                    "No Godot editor for '{shown}' was closed: {connected}, and this server did not launch one for it that still runs"
                ),
                &[
                    "If the editor runs, enable the Godot MCP Bridge plugin in it and make sure it listens on the port this server uses",
                    "Close the editor yourself; another project's editor is never closed by this tool",
                ],
            );
        };
        if !discard {
            return ToolResult::error(
                format!(
                    "The editor this server launched for '{shown}' (PID {pid}) is not connected to the bridge, so its unsaved changes cannot be checked. It was not closed."
                ),
                &[
                    "Pass discard: true to stop it; changes it has not saved are lost",
                    "Or enable the Godot MCP Bridge plugin in it, so godot_close_editor can check for unsaved changes",
                ],
            );
        }
        let started = Instant::now();
        self.editors.stop(pid, false);
        let mut forced = false;
        if !self.wait_for_exit(pid, wait).await {
            self.editors.stop(pid, true);
            forced = true;
            if !self.wait_for_exit(pid, Duration::from_secs(5)).await {
                return ToolResult::error(
                    format!("The editor for '{shown}' (PID {pid}) still runs after it was killed"),
                    &[
                        "Close the editor yourself, or check the process with the system's task manager",
                    ],
                );
            }
        }
        ToolResult::text(format!(
            "Stopped the Godot editor this server launched for '{shown}' without its bridge: PID {pid} exited after {:.1}s{}. Changes it had not saved are lost.",
            started.elapsed().as_secs_f64(),
            if forced {
                ", killed after it ignored the request to quit"
            } else {
                ""
            }
        ))
    }

    async fn wait_for_exit(&self, pid: u32, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let gone = self
                .editors
                .exited(pid)
                .unwrap_or_else(|| !process_runs(pid));
            if gone {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

fn unsaved_listing(scenes: &[String], scripts: &[String]) -> String {
    let mut text = String::new();
    if !scenes.is_empty() {
        text.push_str(&format!(" Unsaved scenes: {}.", scenes.join(", ")));
    }
    if !scripts.is_empty() {
        text.push_str(&format!(" Unsaved scripts: {}.", scripts.join(", ")));
    }
    text
}

/// Whether a process with this id runs. It only reads the process table and sends no signal.
#[cfg(unix)]
fn process_runs(pid: u32) -> bool {
    let Ok(id) = i32::try_from(pid) else {
        return false;
    };
    if id <= 0 {
        return false;
    }
    // SAFETY: signal 0 checks that the process exists and delivers nothing.
    if unsafe { libc::kill(id, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_runs(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, GetLastError, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: the handle is checked before use and closed once; nothing else is touched.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        running
    }
}
