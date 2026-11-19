//! Starting, watching and stopping runs of the project: the editor's Play mode and managed
//! game processes.

use super::{
    ToolManager, bridge_error_result, game, list_paths, str_arg, string_list, user_args_of,
    with_notice,
};
use crate::error::McpError;
use crate::godot::movie::{DEFAULT_MOVIE_FPS, MAX_MOVIE_FPS, MAX_MOVIE_FRAMES, Movie, MovieFormat};
use crate::godot::{ProcessStart, stale_imports};
use crate::paths::{resolve_in_project, same_directory};
use crate::protocol::ToolResult;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Imports found stale before a run, and how reimporting them through the editor went: `None`
/// when no editor has the project open, so nothing was reimported.
pub(super) struct StaleImports {
    pub(super) paths: Vec<String>,
    reimport: Option<Result<(), String>>,
}

impl StaleImports {
    /// True when the editor reimported every stale file before the run.
    pub(super) fn reimported(&self) -> bool {
        matches!(self.reimport, Some(Ok(())))
    }

    /// One sentence for the answer; `loader` names what loads the import ("game", "script").
    pub(super) fn sentence(&self, loader: &str) -> String {
        let listed = list_paths(&self.paths);
        match &self.reimport {
            Some(Ok(())) => format!(
                "Reimported stale resources before the run: {listed}. The {loader} loads the new import."
            ),
            Some(Err(error)) => format!(
                "Warning: these imported resources are newer than the cache ({error}): {listed}. Call godot_resave_resources with those paths, then run again."
            ),
            None => format!(
                "Warning: these imported resources are newer than the cache the {loader} will load: {listed}. Call godot_resave_resources with those paths, then run again."
            ),
        }
    }
}

impl ToolManager {
    pub(super) async fn handle_run_project(&self, args: Value) -> ToolResult {
        let scene = str_arg(&args, "scene");
        if let Some(scene) = scene
            && (!scene.starts_with("res://") || scene.split(['/', '\\']).any(|part| part == ".."))
        {
            return ToolResult::error(
                format!("Invalid scene '{scene}': scene must be a res:// path inside the project"),
                &["Use a path such as res://scenes/Main.tscn"],
            );
        }
        for spec in string_list(args.get("breakpoints")) {
            if let Err(result) = game::check_breakpoint(&spec) {
                return result;
            }
        }
        let mode = str_arg(&args, "mode").unwrap_or("auto");
        if args.get("held").and_then(Value::as_bool).unwrap_or(false) && mode != "process" {
            return ToolResult::error(
                "held needs mode \"process\": only a game this server starts can run at a fixed frame rate",
                &[
                    "Pass mode: \"process\" with held: true",
                    "In the editor's Play mode a game can still be held later with godot_game_physics (action hold), but its steps are not exact",
                ],
            );
        }
        let user_args = match user_args_of(&args) {
            Ok(user_args) => user_args,
            Err(result) => return result,
        };
        if user_args.is_some() && mode != "process" {
            return ToolResult::error(
                "user_args is only passed to a managed process. Editor Play and mode \"auto\" cannot receive them.",
                &[
                    "Pass mode: \"process\" to start a game this server launches",
                    "The strings are placed after --, so the game reads them with OS.get_cmdline_user_args() and Godot does not treat them as engine options",
                ],
            );
        }
        let movie_asked = args.get("movie").is_some_and(|movie| !movie.is_null());
        if movie_asked && let Err(result) = movie_conflicts(&args, mode) {
            return result;
        }
        if mode == "process" {
            return self
                .spawn_managed(&args, scene, user_args.as_deref().unwrap_or(&[]))
                .await;
        }

        // Reimport before Play, so the editor's game does not keep the old cache.
        let notice = match self.resolve_project(&args).await {
            Ok(project) if project.from_editor => self.import_notice(&project.dir).await,
            _ => String::new(),
        };

        // Prefer the live editor: it can be stopped/queried later via
        // godot_stop_project / godot_get_run_status, unlike a spawned process.
        let bridge_params = json!({ "scene_path": scene.unwrap_or("") });
        match self.bridge.send_command("run_project", bridge_params).await {
            Ok(result) => {
                return with_notice(
                    ToolResult::text(format!(
                        "Project running via live editor:\n{}",
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    )),
                    &notice,
                );
            }
            // Only a missing editor justifies spawning a separate process.
            Err(McpError::BridgeUnavailable(_)) => {}
            Err(error) => return with_notice(bridge_error_result(&error), &notice),
        }

        self.spawn_managed(&args, scene, user_args.as_deref().unwrap_or(&[]))
            .await
    }

    async fn spawn_managed(
        &self,
        args: &Value,
        scene: Option<&str>,
        user_args: &[String],
    ) -> ToolResult {
        let path = match self.host_project(args) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let breakpoints = string_list(args.get("breakpoints"));
        let headless = args
            .get("headless")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let break_on_errors = args
            .get("break_on_errors")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let held = args.get("held").and_then(Value::as_bool).unwrap_or(false);
        let movie = match args.get("movie").filter(|movie| !movie.is_null()) {
            Some(request) => match movie_of(request, &path, self.movie_dir.as_deref()) {
                Ok(movie) => Some(movie),
                Err(result) => return result,
            },
            None => None,
        };
        let mut movie_folder_made = false;
        if let Some(movie) = &movie {
            match movie.prepare(&path) {
                Ok(made) => movie_folder_made = made,
                Err(error) => {
                    return ToolResult::error(
                        format!(
                            "Cannot prepare the Movie Maker output {}: {error}",
                            movie.path.display()
                        ),
                        &[
                            "Check that the folder can be written and that no player holds the old file open",
                            "Or pass another movie.path",
                        ],
                    );
                }
            }
        }
        let notice = self.import_notice(&path).await;
        self.gates.settle(&path).await;
        match self
            .games
            .start(
                &path,
                ProcessStart {
                    scene,
                    breakpoints: &breakpoints,
                    headless,
                    break_on_errors,
                    held,
                    user_args,
                    movie: movie.as_ref(),
                },
            )
            .await
        {
            Ok(info) => with_notice(
                ToolResult::text(format!(
                    "Spawned a managed Godot process (PID: {}, run_id: {}). Stop it with godot_stop_project. Project: '{}'.
{}{}",
                    info.pid,
                    info.id,
                    path.display(),
                    if held {
                        "Held at its first physics tick: advance it with godot_game_physics (step), and resume or stop the run when done."
                    } else {
                        "Not held: the game runs on its own."
                    },
                    movie
                        .as_ref()
                        .map(|movie| movie_sentence(movie, movie_folder_made))
                        .unwrap_or_default()
                )),
                &notice,
            ),
            Err(error) => with_notice(
                ToolResult::error(
                    format!("Failed to run project: {error}"),
                    &["Verify GODOT_PATH points to a working Godot executable"],
                ),
                &notice,
            ),
        }
    }

    /// Reimports stale imports through the editor when it has this project open, and otherwise
    /// returns a warning that names `godot_resave_resources`. Empty when nothing is stale.
    async fn import_notice(&self, project: &Path) -> String {
        match self.check_imports(project).await {
            Some(stale) => format!("\n{}", stale.sentence("game")),
            None => String::new(),
        }
    }

    /// Compares the files with a `.import` sidecar with their `.godot/imported` cache and
    /// reimports the stale ones when the editor has this project open. `None` when nothing is
    /// stale.
    pub(super) async fn check_imports(&self, project: &Path) -> Option<StaleImports> {
        let root = project.to_path_buf();
        // The walk reads every folder of the project, so it runs off the async workers.
        let paths = tokio::task::spawn_blocking(move || stale_imports(&root))
            .await
            .unwrap_or_default();
        if paths.is_empty() {
            return None;
        }
        let reimport = self.reimport_stale(project, &paths).await;
        Some(StaleImports { paths, reimport })
    }

    /// `Some` when the connected editor has this project open (`Ok` after a reimport). `None`
    /// when no editor is there, or it has a different project, so the caller warns instead.
    async fn reimport_stale(&self, project: &Path, paths: &[String]) -> Option<Result<(), String>> {
        let info = match self
            .bridge
            .send_command("get_project_info", json!({}))
            .await
        {
            Ok(info) => info,
            Err(McpError::BridgeUnavailable(_)) => return None,
            Err(error) => return Some(Err(error.to_string())),
        };
        let Some(dir) = info.get("project_dir").and_then(Value::as_str) else {
            return Some(Err("the editor did not report its project directory".into()));
        };
        if !same_directory(Path::new(dir), project) {
            return None;
        }
        match self
            .bridge
            .send_command("resave_resources", json!({ "paths": paths }))
            .await
        {
            Ok(_) => Some(Ok(())),
            Err(McpError::BridgeUnavailable(_)) => None,
            Err(error) => Some(Err(error.to_string())),
        }
    }

    pub(super) async fn handle_stop_project(&self, args: Value) -> ToolResult {
        if let Some(run_id) = str_arg(&args, "run_id") {
            return match self.games.stop(Some(run_id)).await {
                Ok(summary) => {
                    ToolResult::text(serde_json::to_string_pretty(&summary).unwrap_or_default())
                }
                Err(error) => ToolResult::error(
                    error.to_string(),
                    &["Check godot_get_run_status for live run ids"],
                ),
            };
        }
        // TL-11: stop everything that runs, and say what was stopped. The managed runs are
        // independent of the editor, so they are stopped first and an editor failure cannot
        // leave them running.
        let recently_ended = self.games.ended_runs();
        let runs = self.games.stop_all().await;
        let mut editor_play = false;
        let mut editor_error = None;
        match self.bridge.send_command("get_run_status", json!({})).await {
            Ok(status) if status.get("running").and_then(Value::as_bool) == Some(true) => {
                match self.bridge.send_command("stop_project", json!({})).await {
                    Ok(_) => editor_play = true,
                    Err(error) => editor_error = Some(error),
                }
            }
            Ok(_) | Err(McpError::BridgeUnavailable(_)) => {}
            Err(error) => editor_error = Some(error),
        }
        if let Some(error) = editor_error {
            if runs.is_empty() {
                return bridge_error_result(&error);
            }
            return ToolResult::error(
                format!(
                    "Stopped {} managed run(s), but the editor's Play mode could not be stopped: {error}",
                    runs.len()
                ),
                &["Call godot_stop_project again to stop the editor's Play mode"],
            );
        }
        let message = match (editor_play, runs.len()) {
            (false, 0) => format!("Nothing was running.{}", ended_sentence(&recently_ended)),
            (true, 0) => "Stopped the editor's Play mode.".to_string(),
            (false, count) => format!("Stopped {count} managed run(s)."),
            (true, count) => format!("Stopped the editor's Play mode and {count} managed run(s)."),
        };
        ToolResult::text(
            serde_json::to_string_pretty(&json!({
                "message": message,
                "stopped": {"editor_play": editor_play, "runs": runs},
                "recently_ended": recently_ended,
            }))
            .unwrap_or_default(),
        )
    }

    pub(super) async fn handle_run_status(&self) -> ToolResult {
        let processes = self.games.status();
        let ended = self.games.ended_runs();
        let answer = match self.bridge.send_command("get_run_status", json!({})).await {
            Ok(mut combined) => {
                if let Value::Object(map) = &mut combined {
                    map.insert("process_runs".into(), Value::Array(processes));
                }
                combined
            }
            // No editor is not a failure of this question: managed runs are still reported.
            Err(McpError::BridgeUnavailable(detail)) => {
                let nothing = processes.iter().all(|run| run["running"] != true);
                let mut answer = json!({
                    "running": false,
                    "editor": format!("not connected ({detail})"),
                    "process_runs": processes,
                });
                if nothing {
                    answer["message"] =
                        json!(format!("Nothing is running.{}", ended_sentence(&ended)));
                }
                answer
            }
            Err(error) => return bridge_error_result(&error),
        };
        ToolResult::text(serde_json::to_string_pretty(&answer).unwrap_or_default())
    }
}

/// How the newest ended run ended, for the sentence that says nothing runs. Empty without one.
fn ended_sentence(ended: &[Value]) -> String {
    let Some(run) = ended.first() else {
        return String::new();
    };
    format!(
        " Managed run {} ended: {}.",
        run["run_id"].as_str().unwrap_or("?"),
        run["how_ended"].as_str().unwrap_or("how is not known")
    )
}

/// Refuses a `movie` request that cannot be recorded, before anything starts.
fn movie_conflicts(args: &Value, mode: &str) -> Result<(), ToolResult> {
    if mode != "process" {
        return Err(ToolResult::error(
            "movie needs mode \"process\": Movie Maker is started with engine options, and only a game this server launches gets them",
            &["Pass mode: \"process\" with movie"],
        ));
    }
    if args.get("held").and_then(Value::as_bool).unwrap_or(false) {
        return Err(ToolResult::error(
            "movie and held cannot be combined: a held game draws no frames, and Movie Maker already runs at a fixed frame rate",
            &[
                "Drop held: Movie Maker gives every frame the same game time (1/fps), so the run is already repeatable",
                "Or drop movie and capture frames of a held game with godot_game_screenshot",
            ],
        ));
    }
    if args
        .get("headless")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ToolResult::error(
            "movie and headless cannot be combined: a headless game renders nothing, so Movie Maker would write empty frames",
            &["Drop headless: Movie Maker needs a window"],
        ));
    }
    Ok(())
}

/// Reads and checks a `movie` request against the project and the folder the person allowed.
fn movie_of(request: &Value, project: &Path, allowed: Option<&Path>) -> Result<Movie, ToolResult> {
    let example = "Pass movie: {\"path\": \"res://movies/clip.avi\", \"fps\": 60, \"quit_after_frames\": 600}";
    let Some(fields) = request.as_object() else {
        return Err(ToolResult::error("movie must be an object", &[example]));
    };
    if let Some(unknown) = fields
        .keys()
        .find(|key| !["path", "fps", "quit_after_frames", "overwrite"].contains(&key.as_str()))
    {
        return Err(ToolResult::error(
            format!(
                "movie has no field '{unknown}'; it takes path, fps, quit_after_frames and overwrite"
            ),
            &[
                example,
                "Set Movie Maker's quality, audio mix rate and vsync with godot_set_project_setting (editor/movie_writer/*)",
            ],
        ));
    }
    let Some(requested) = fields.get("path").and_then(Value::as_str).map(str::trim) else {
        return Err(ToolResult::error(
            "movie.path is required: the .avi file, or the .png that names an image sequence",
            &[example],
        ));
    };
    let path = movie_path(requested, project, allowed)?;
    let Some(format) = MovieFormat::of(&path) else {
        return Err(ToolResult::error(
            format!("movie.path must end in .avi or .png: {requested}"),
            &[
                "Use .avi for MJPEG video with the game's audio in one file",
                "Use .png for one image per frame (name00000000.png, ...) and the audio in name.wav",
            ],
        ));
    };
    if path.file_stem().is_none_or(|stem| stem.is_empty())
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
    {
        return Err(ToolResult::error(
            format!("movie.path needs a file name before its extension: {requested}"),
            &[example],
        ));
    }
    let fps = match fields.get("fps").filter(|fps| !fps.is_null()) {
        None => DEFAULT_MOVIE_FPS,
        Some(value) => match value
            .as_u64()
            .filter(|fps| (1..=u64::from(MAX_MOVIE_FPS)).contains(fps))
        {
            Some(fps) => u32::try_from(fps).unwrap_or(DEFAULT_MOVIE_FPS),
            None => {
                return Err(ToolResult::error(
                    format!("movie.fps must be a whole number from 1 to {MAX_MOVIE_FPS}"),
                    &["Pass fps: 60 (the default), or 30 for a smaller file"],
                ));
            }
        },
    };
    let quit_after_frames = match fields
        .get("quit_after_frames")
        .filter(|frames| !frames.is_null())
    {
        None => None,
        Some(value) => match value
            .as_u64()
            .filter(|frames| (1..=MAX_MOVIE_FRAMES).contains(frames))
        {
            Some(frames) => Some(frames),
            None => {
                return Err(ToolResult::error(
                    format!(
                        "movie.quit_after_frames must be a whole number from 1 to {MAX_MOVIE_FRAMES}"
                    ),
                    &[
                        "Pass the clip length in seconds times fps, such as 600 for 10 seconds at 60 fps",
                    ],
                ));
            }
        },
    };
    let overwrite = fields
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let movie = Movie {
        path,
        format,
        fps,
        quit_after_frames,
    };
    let existing = movie.existing_outputs();
    if !existing.is_empty() && !overwrite {
        return Err(ToolResult::error(
            format!(
                "An earlier recording is at {} ({} file(s)); this run would replace it",
                movie.path.display(),
                existing.len()
            ),
            &[
                "Pass movie.overwrite: true to replace it",
                "Or pick another movie.path",
            ],
        ));
    }
    Ok(movie)
}

/// Resolves the output inside the project, or inside the folder the person allowed for
/// recordings (`--movie-dir` / `GODOT_MCP_MOVIE_DIR`).
fn movie_path(
    requested: &str,
    project: &Path,
    allowed: Option<&Path>,
) -> Result<PathBuf, ToolResult> {
    let inside = resolve_in_project(project, requested);
    if let Ok(path) = inside {
        return Ok(path);
    }
    let absolute = Path::new(requested).is_absolute() && !requested.starts_with("res://");
    if absolute
        && let Some(allowed) = allowed
        && let Ok(path) = resolve_in_project(allowed, requested)
    {
        return Ok(path);
    }
    let reason = inside.err().unwrap_or_default();
    let allowed_sentence = match allowed {
        Some(dir) => format!("Or write it inside the recordings folder this server allows, {}", dir.display()),
        None => "To write outside the project, the person sets GODOT_MCP_MOVIE_DIR (or --movie-dir) in this server's configuration to the folder recordings may go to, and reconnects the server".to_string(),
    };
    Err(ToolResult::error(
        format!("movie.path cannot be used: {reason}"),
        &[
            "Use a path inside the project, such as res://movies/clip.avi",
            &allowed_sentence,
        ],
    ))
}

/// The sentence of the run's answer that says what is being recorded and how to finish it.
fn movie_sentence(movie: &Movie, folder_made: bool) -> String {
    let length = match movie.quit_after_frames {
        Some(frames) => format!(
            "The game quits by itself after {frames} frames ({:.2} s of game time).",
            frames as f64 / f64::from(movie.fps)
        ),
        None => "No frame limit: the recording ends when the game quits or godot_stop_project stops it (it asks the game to quit, so the file is finished).".to_string(),
    };
    let what = match movie.format {
        MovieFormat::Avi => "MJPEG video with audio".to_string(),
        MovieFormat::Png => format!(
            "a PNG sequence ({}########.png) with audio in {}",
            movie.path.with_extension("").display(),
            movie.path.with_extension("wav").display()
        ),
    };
    let ignored = if folder_made
        && movie
            .path
            .parent()
            .is_some_and(|folder| folder.join(".gdignore").is_file())
    {
        " The new output folder got a .gdignore, so the editor does not import the frames."
    } else {
        ""
    };
    format!(
        "\nMovie Maker records {what} to {} at {} fps. Each frame advances game time by exactly 1/{}, however long it takes to draw, so the game runs off the wall clock (usually slower than real time) and the same input gives the same frames; input tools (godot_game_input, godot_game_scenario) still work. {length} When it has ended, godot_get_run_status shows movie.size_bytes and movie.frames.{ignored}",
        movie.path.display(),
        movie.fps,
        movie.fps
    )
}
