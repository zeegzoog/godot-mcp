//! One held play-through: Input Map actions and pointers for a number of physics
//! ticks, then a snapshot. Builds on `godot_game_input`, `godot_game_physics`,
//! `game.body` and `godot_game_screenshot`. It does not evaluate GDScript.

use super::{ToolManager, str_arg, tool};
use crate::error::McpError;
use crate::godot::Run;
use crate::protocol::{Tool, ToolContent, ToolResult};
use serde_json::{Value, json};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

const MAX_STEPS: usize = 32;
const MAX_FRAMES: i64 = 600;
const MAX_TOTAL_FRAMES: i64 = 3600;
const SCENARIO_WAIT: Duration = Duration::from_secs(60);
const CLEANUP_WAIT: Duration = Duration::from_secs(3);
/// Per step and per call, so one call cannot ask for an unbounded number of round trips.
const MAX_INPUTS: usize = 32;
const MAX_READS: usize = 16;
const MAX_PROPERTIES: usize = 32;
const MAX_NAME_CHARS: usize = 256;
/// Touch indexes a game can tell apart in practice.
const MAX_TOUCH_INDEX: i64 = 9;

/// The runs a scenario is playing in. Two scenarios in one run would release each other's input
/// and fight over the physics step.
static ACTIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct RunLock(String);

impl RunLock {
    fn take(run_id: &str) -> Option<Self> {
        let mut active = ACTIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active.iter().any(|id| id == run_id) {
            return None;
        }
        active.push(run_id.to_string());
        Some(Self(run_id.to_string()))
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let mut active = ACTIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.retain(|id| *id != self.0);
    }
}

struct ReleaseInput {
    run: Option<Arc<Run>>,
    lock: Option<RunLock>,
}

impl Drop for ReleaseInput {
    fn drop(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        // The retry keeps the run locked until it is done, so it cannot wipe the input of the
        // next scenario. Outside a runtime there is nothing to retry on: the lock is freed, and
        // input still down stays down until the next release, which every scenario ends with.
        let lock = self.lock.take();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let _ = run
                .runtime("input.release_all", json!({}), CLEANUP_WAIT)
                .await;
            drop(lock);
        });
    }
}

pub fn definition() -> Tool {
    tool(
        "godot_game_scenario",
        "Plays one input scenario in a running game and returns that run's steps, snapshots, warnings and errors. Each step applies Input Map actions and pointer events, then advances `frames` physics ticks (0 to 600; the whole scenario at most 3600). phase is tap by default, as in godot_game_input: tap stays down for the step's frames and is released before the next step, so a step with a tap needs frames of at least 1. press stays down after the step until a later release, and release lets go at the start of the step. strength is the analog value, 0 to 1. pointers are mouse or touch; several touches may be down together, and touch phase move drags an index that is already down. pause sets SceneTree.paused for the following ticks; the runtime still answers, because a hold is not the tree's pause and pausable nodes simply skip their physics callback. bodies reads CharacterBody2D and CharacterBody3D (position, velocity, is_on_floor, is_on_wall, is_on_ceiling, up to 8 slide collisions). nodes reads public properties with game.get. A missing node or an unsupported body is an error that names the path. The game is held for the whole call (started with godot_run_project held true when the ticks must be exact) and left held, so nothing moves after the last snapshot; resume with godot_game_physics action resume. deterministic is true only for a game started held at a fixed frame rate equal to its tick rate. A press is flushed before the ticks, so _input sees it immediately and _process sees it on the following frame. Threads, loading and the physics solver are not made deterministic. screenshot true adds one capture of the last drawn frame (a headless game records a warning and no image). Every synthetic button, action and touch is released when the call finishes, when a step fails, and if the call is cancelled. output.lines and output.errors are only the warnings and errors recorded during this call. The call has a total 60-second budget, including 3 seconds reserved for input cleanup. It targets one managed process selected at the start; starting another process does not redirect it. A timed-out physics step may finish before the game holds again.",
        json!({
            "run_id": {"type": "string", "description": "Optional managed run id. Default: newest process run, pinned for the entire scenario."},
            "steps": {
                "type": "array",
                "maxItems": MAX_STEPS,
                "description": "1 to 32 steps. frames is the physics ticks to run after the input of that step.",
                "items": {
                    "type": "object",
                    "properties": {
                        "frames": {"type": "integer", "minimum": 0, "maximum": MAX_FRAMES, "description": "Physics ticks. 0 applies the input and takes the snapshot without stepping, and refuses a tap."},
                        "actions": {
                            "type": "array",
                            "maxItems": MAX_INPUTS,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "action": {"type": "string", "maxLength": 64},
                                    "phase": {"type": "string", "enum": ["press", "release", "tap"], "description": "Default tap, which needs frames of at least 1."},
                                    "strength": {"type": "number", "minimum": 0, "maximum": 1}
                                },
                                "required": ["action"]
                            }
                        },
                        "pointers": {
                            "type": "array",
                            "maxItems": MAX_INPUTS,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "pointer": {"type": "string", "enum": ["mouse", "touch"]},
                                    "phase": {"type": "string", "enum": ["press", "release", "move", "tap"], "description": "Default tap, which needs frames of at least 1."},
                                    "x": {"type": "number"},
                                    "y": {"type": "number"},
                                    "index": {"type": "integer", "minimum": 0, "maximum": MAX_TOUCH_INDEX, "description": "Touch index. Several indexes can be down at once."},
                                    "button": {"type": "string", "maxLength": MAX_NAME_CHARS, "description": "Mouse button. Default left."}
                                },
                                "required": ["pointer", "x", "y"]
                            }
                        },
                        "pause": {"type": "boolean", "description": "Set SceneTree.paused before this step's ticks."}
                    },
                    "required": ["frames"]
                }
            },
            "bodies": {"type": "array", "maxItems": MAX_READS, "items": {"type": "string", "maxLength": MAX_NAME_CHARS}, "description": "CharacterBody paths read after every step."},
            "nodes": {
                "type": "array",
                "maxItems": MAX_READS,
                "description": "Public properties read after every step. Names starting with _ are refused.",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "maxLength": MAX_NAME_CHARS},
                        "properties": {"type": "array", "maxItems": MAX_PROPERTIES, "items": {"type": "string", "maxLength": MAX_NAME_CHARS}}
                    },
                    "required": ["path", "properties"]
                }
            },
            "screenshot": {"type": "boolean", "description": "Capture one frame at the end. Default false."}
        }),
        &["steps"],
    )
}

struct Step {
    frames: i64,
    actions: Vec<Value>,
    pointers: Vec<Value>,
    pause: Option<bool>,
}

struct Spec {
    steps: Vec<Step>,
    bodies: Vec<String>,
    nodes: Vec<Value>,
    screenshot: bool,
}

pub async fn call(tools: &ToolManager, args: &Value) -> ToolResult {
    let spec = match parse(args) {
        Ok(spec) => spec,
        Err(message) => {
            return ToolResult::error(
                message,
                &[
                    "Pass steps: [{frames, actions, pointers, pause}]",
                    "frames is 0 to 600 physics ticks, at most 3600 in the whole call",
                    "See docs/game-checks.md",
                ],
            );
        }
    };
    // Select once: a new process must never change the target of an in-flight scenario.
    let run = match tools.games.select(str_arg(args, "run_id")) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return ToolResult::error(
                "A scenario needs a managed process run.",
                &["Start godot_run_project with mode process and held true, then retry."],
            );
        }
        Err(error) => {
            return ToolResult::error(
                error.to_string(),
                &["Pass the run_id returned by godot_run_project."],
            );
        }
    };
    let run_id = Some(run.id());
    let Some(lock) = RunLock::take(run.id()) else {
        return ToolResult::error(
            "A scenario is already playing in this run.",
            &[
                "Wait for the other godot_game_scenario call to finish, then retry",
                "Or start another process with godot_run_project and pass its run_id",
            ],
        );
    };
    let cursor = run.log_cursor();
    let mut guard = ReleaseInput {
        run: Some(Arc::clone(&run)),
        lock: Some(lock),
    };
    let deadline = Instant::now() + SCENARIO_WAIT;
    // Reserve cleanup time inside the total budget, rather than adding it afterwards.
    let work_deadline = deadline - CLEANUP_WAIT;
    let mut report = json!({
        "ok": true,
        "run_id": run.id(),
        "held": false,
        "deterministic": false,
        "deterministic_reason": "not_stepped",
        "paused": false,
        "ticks_run": 0,
        "steps": [],
        "warnings": [],
        "errors": [],
    });

    let mut image: Option<String> = None;
    let work = async {
        let status = ensure_held(tools, run_id).await?;
        report["held"] = json!(true);
        let mut ticks_run = 0_i64;
        let mut deterministic = status
            .get("deterministic")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut deterministic_reason = status
            .get("deterministic_reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut paused = status
            .get("paused")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        report["deterministic"] = json!(deterministic);
        report["deterministic_reason"] = json!(deterministic_reason);
        report["paused"] = json!(paused);
        for (index, step) in spec.steps.iter().enumerate() {
            if let Some(pause) = step.pause {
                match tools
                    .runtime_call(run_id, "game.pause", json!({"paused": pause}))
                    .await
                {
                    Ok(value) => {
                        paused = value
                            .get("paused")
                            .and_then(Value::as_bool)
                            .unwrap_or(pause);
                        report["paused"] = json!(paused);
                    }
                    Err(error) => {
                        push_error(&mut report, &error.to_string());
                        break;
                    }
                }
            }
            let mut applied = Vec::new();
            if let Err(error) = apply_input(tools, run_id, step, false, &mut applied).await {
                push_error(&mut report, &error.to_string());
                break;
            }
            let mut step_ticks = 0_i64;
            if step.frames > 0 {
                match tools
                    .runtime_call(
                        run_id,
                        "game.physics",
                        json!({"action": "step", "ticks": step.frames}),
                    )
                    .await
                {
                    Ok(value) => {
                        step_ticks = value.get("ticks_run").and_then(Value::as_i64).unwrap_or(0);
                        ticks_run += step_ticks;
                        report["ticks_run"] = json!(ticks_run);
                        if let Some(flag) = value.get("deterministic").and_then(Value::as_bool) {
                            deterministic = flag;
                        }
                        if let Some(reason) =
                            value.get("deterministic_reason").and_then(Value::as_str)
                        {
                            deterministic_reason = reason.to_string();
                        }
                        if let Some(flag) = value.get("paused").and_then(Value::as_bool) {
                            paused = flag;
                        }
                        report["deterministic"] = json!(deterministic);
                        report["deterministic_reason"] = json!(deterministic_reason);
                        report["paused"] = json!(paused);
                    }
                    Err(error) => {
                        push_error(&mut report, &error.to_string());
                        break;
                    }
                }
            }
            if let Err(error) = apply_input(tools, run_id, step, true, &mut applied).await {
                push_error(&mut report, &error.to_string());
                break;
            }
            let snapshot = match read_snapshot(tools, run_id, &spec).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    push_error(&mut report, &error.to_string());
                    break;
                }
            };
            if snapshot
                .get("errors")
                .and_then(Value::as_array)
                .is_some_and(|errors| !errors.is_empty())
            {
                report["ok"] = json!(false);
                if let Some(errors) = report["errors"].as_array_mut() {
                    if let Some(found) = snapshot.get("errors").and_then(Value::as_array) {
                        errors.extend(found.iter().cloned());
                    }
                }
            }
            if let Some(steps) = report["steps"].as_array_mut() {
                steps.push(json!({
                    "index": index,
                    "frames": step.frames,
                    "ticks_run": step_ticks,
                    "applied": applied,
                    "snapshot": snapshot,
                }));
            }
            if report["ok"] == false {
                break;
            }
        }

        report["deterministic"] = json!(deterministic);
        report["deterministic_reason"] = json!(deterministic_reason);
        report["paused"] = json!(paused);
        if spec.screenshot && report["ok"] == true {
            match tools
                .runtime_call(
                    run_id,
                    "game.screenshot",
                    json!({"frames": 1, "max_dimension": 512}),
                )
                .await
            {
                Ok(mut value) => {
                    image = value
                        .get("data")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(map) = value.as_object_mut() {
                        map.remove("data");
                    }
                    report["screenshot"] = value;
                }
                Err(error) => {
                    if let Some(warnings) = report["warnings"].as_array_mut() {
                        warnings.push(json!(error.to_string()));
                    }
                    report["screenshot"] = json!({"ok": false});
                }
            }
        }

        Ok(())
    };
    if let Err(error) = within_deadline(work_deadline, work).await {
        push_error(&mut report, &error.to_string());
    }
    match within_deadline(deadline, release(&mut guard, tools, run_id)).await {
        Ok(value) => report["released"] = value,
        Err(error) => {
            report["released"] = json!({"error": error.to_string()});
            push_error(&mut report, &format!("Input cleanup failed: {error}"));
        }
    }
    report["output"] = run.logs_from(cursor);
    if report["ok"] != true {
        report["recovery"] = json!([
            "godot_game_physics with action status shows whether the game is held and where it stopped",
            "godot_run_output shows the game's warnings and errors",
            "Lower frames or split the scenario into shorter calls; godot_game_input_release lets go of any input still down",
        ]);
    }
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string());
    let failed = report["ok"] != true;
    let mut content = vec![ToolContent::Text { text }];
    if let Some(data) = image.filter(|_| !failed) {
        content.push(ToolContent::Image {
            data,
            mime_type: "image/png".into(),
        });
    }
    ToolResult {
        content,
        is_error: failed,
    }
}

fn push_error(report: &mut Value, message: &str) {
    report["ok"] = json!(false);
    if let Some(errors) = report.get_mut("errors").and_then(Value::as_array_mut) {
        errors.push(json!(message));
    }
}

async fn ensure_held(tools: &ToolManager, run_id: Option<&str>) -> Result<Value, McpError> {
    let status = tools
        .runtime_call(run_id, "game.physics", json!({"action": "status"}))
        .await?;
    if status.get("held").and_then(Value::as_bool) == Some(true) {
        return Ok(status);
    }
    tools
        .runtime_call(run_id, "game.physics", json!({"action": "hold"}))
        .await
}

async fn apply_input(
    tools: &ToolManager,
    run_id: Option<&str>,
    step: &Step,
    taps_only: bool,
    applied: &mut Vec<Value>,
) -> Result<(), McpError> {
    let mut input = Vec::new();
    for action in &step.actions {
        let phase = action.get("phase").and_then(Value::as_str).unwrap_or("tap");
        let is_tap = phase == "tap";
        if taps_only && !is_tap {
            continue;
        }
        let mut event = json!({
            "type": "action",
            "action": action["action"],
            "phase": if is_tap && taps_only { "release" } else if is_tap { "press" } else { phase },
        });
        if let Some(strength) = action.get("strength") {
            event["strength"] = strength.clone();
        }
        input.push(event);
    }
    for pointer in &step.pointers {
        let phase = pointer
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("tap");
        let is_tap = phase == "tap";
        if taps_only && !is_tap {
            continue;
        }
        let sent = if is_tap && taps_only {
            "release"
        } else if is_tap {
            "press"
        } else {
            phase
        };
        let kind = if pointer["pointer"] == "mouse" && sent == "move" {
            "mouse_move"
        } else if pointer["pointer"] == "mouse" {
            "mouse_button"
        } else {
            "touch"
        };
        input.push(json!({
            "type": kind,
            "phase": if kind == "mouse_move" { "press" } else { sent },
            "x": pointer["x"],
            "y": pointer["y"],
            "index": pointer.get("index").cloned().unwrap_or(json!(0)),
            "button": pointer.get("button").cloned().unwrap_or(json!("left")),
        }));
    }
    if input.is_empty() {
        return Ok(());
    }
    let result = tools
        .runtime_call(run_id, "input.run", json!({"steps": input}))
        .await?;
    applied.push(result);
    Ok(())
}

async fn read_snapshot(
    tools: &ToolManager,
    run_id: Option<&str>,
    spec: &Spec,
) -> Result<Value, McpError> {
    let mut errors = Vec::new();
    let mut bodies = Value::Null;
    if !spec.bodies.is_empty() {
        let value = tools
            .runtime_call(run_id, "game.body", json!({"paths": spec.bodies}))
            .await?;
        if let Some(found) = value.get("errors").and_then(Value::as_array) {
            errors.extend(found.iter().cloned());
        }
        bodies = value.get("bodies").cloned().unwrap_or(json!([]));
    }
    let mut nodes = Vec::new();
    for node in &spec.nodes {
        match tools.runtime_call(run_id, "game.get", node.clone()).await {
            Ok(value) => nodes.push(value),
            Err(error) => errors.push(json!(error.to_string())),
        }
    }
    Ok(json!({"bodies": bodies, "nodes": nodes, "errors": errors}))
}

async fn release(
    guard: &mut ReleaseInput,
    tools: &ToolManager,
    run_id: Option<&str>,
) -> Result<Value, McpError> {
    disarm_after_success(
        &mut guard.run,
        tools.runtime_call(run_id, "input.release_all", json!({})),
    )
    .await
}

// Dropping a pending release must leave the guard armed, so Drop can retry on the pinned run.
async fn disarm_after_success<T>(
    guard: &mut Option<T>,
    response: impl Future<Output = Result<Value, McpError>>,
) -> Result<Value, McpError> {
    let value = response.await?;
    guard.take();
    Ok(value)
}

async fn within_deadline<T>(
    deadline: Instant,
    operation: impl Future<Output = Result<T, McpError>>,
) -> Result<T, McpError> {
    tokio::time::timeout_at(deadline, operation).await.map_err(|_| {
        McpError::Process("The scenario reached its deadline; input cleanup is attempted. An in-flight physics step may still finish before the game holds again.".into())
    })?
}

fn parse(args: &Value) -> Result<Spec, String> {
    let raw = args
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "steps must be a non-empty array.".to_string())?;
    if raw.is_empty() || raw.len() > MAX_STEPS {
        return Err(format!("steps must contain 1 to {MAX_STEPS} steps."));
    }
    let mut steps = Vec::with_capacity(raw.len());
    let mut total = 0_i64;
    for (index, step) in raw.iter().enumerate() {
        let step = step
            .as_object()
            .ok_or_else(|| format!("steps[{index}] must be an object."))?;
        let frames = whole(step.get("frames"))
            .filter(|frames| (0..=MAX_FRAMES).contains(frames))
            .ok_or_else(|| {
                format!("steps[{index}].frames must be an integer from 0 to {MAX_FRAMES}.")
            })?;
        total += frames;
        if total > MAX_TOTAL_FRAMES {
            return Err(format!(
                "a scenario may run at most {MAX_TOTAL_FRAMES} physics ticks."
            ));
        }
        let actions = match step.get("actions") {
            None => Vec::new(),
            Some(value) => parse_actions(index, value)?,
        };
        let pointers = match step.get("pointers") {
            None => Vec::new(),
            Some(value) => parse_pointers(index, value)?,
        };
        if actions.len() + pointers.len() > MAX_INPUTS {
            return Err(format!(
                "steps[{index}] has more than {MAX_INPUTS} actions and pointers."
            ));
        }
        // A tap is let go after the step's ticks; with none, no tick would see it down.
        if frames == 0
            && actions
                .iter()
                .chain(&pointers)
                .any(|input| input["phase"] == "tap")
        {
            return Err(format!(
                "steps[{index}] has frames 0, so a tap (the default phase) would be released before any tick sees it. Give the step frames of at least 1, or use phase press and a later release."
            ));
        }
        let pause = match step.get("pause") {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => return Err(format!("steps[{index}].pause must be a boolean.")),
        };
        steps.push(Step {
            frames,
            actions,
            pointers,
            pause,
        });
    }
    let bodies = string_array(args.get("bodies"), "bodies")?;
    if bodies.len() > MAX_READS {
        return Err(format!("bodies may name at most {MAX_READS} nodes."));
    }
    let nodes = match args.get("nodes") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut nodes = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let path = item
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|path| !path.is_empty() && path.chars().count() <= MAX_NAME_CHARS)
                    .ok_or_else(|| {
                        format!(
                            "nodes[{index}].path must be a node path of at most {MAX_NAME_CHARS} characters."
                        )
                    })?;
                let properties = item
                    .get("properties")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        format!("nodes[{index}].properties must be an array of names.")
                    })?;
                if properties.is_empty() || properties.iter().any(|name| name.as_str().is_none()) {
                    return Err(format!(
                        "nodes[{index}].properties must be a non-empty array of strings."
                    ));
                }
                if properties.len() > MAX_PROPERTIES {
                    return Err(format!(
                        "nodes[{index}].properties may name at most {MAX_PROPERTIES} properties."
                    ));
                }
                if let Some(name) = properties
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|name| !is_public_property(name))
                {
                    return Err(format!(
                        "nodes[{index}].properties: '{}' is not a public property name.",
                        name.chars().take(64).collect::<String>()
                    ));
                }
                nodes.push(json!({"path": path, "properties": properties}));
            }
            nodes
        }
        Some(_) => return Err("nodes must be an array.".into()),
    };
    if nodes.len() > MAX_READS {
        return Err(format!("nodes may name at most {MAX_READS} nodes."));
    }
    let screenshot = match args.get("screenshot") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("screenshot must be a boolean.".into()),
    };
    Ok(Spec {
        steps,
        bodies,
        nodes,
        screenshot,
    })
}

fn parse_actions(index: usize, value: &Value) -> Result<Vec<Value>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("steps[{index}].actions must be an array."))?;
    if items.len() > MAX_INPUTS {
        return Err(format!(
            "steps[{index}] has more than {MAX_INPUTS} actions and pointers."
        ));
    }
    let mut actions = Vec::new();
    for (action_index, item) in items.iter().enumerate() {
        let action = item
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|action| !action.is_empty() && action.len() <= 64)
            .ok_or_else(|| {
                format!("steps[{index}].actions[{action_index}].action must be a name of 1 to 64 characters.")
            })?;
        let phase = item.get("phase").and_then(Value::as_str).unwrap_or("tap");
        if !matches!(phase, "press" | "release" | "tap") {
            return Err(format!(
                "steps[{index}].actions[{action_index}].phase must be press, release, or tap."
            ));
        }
        let mut event = json!({"action": action, "phase": phase});
        if let Some(strength) = item.get("strength") {
            let number = strength.as_f64().ok_or_else(|| {
                format!(
                    "steps[{index}].actions[{action_index}].strength must be a number from 0 to 1."
                )
            })?;
            if !(0.0..=1.0).contains(&number) {
                return Err(format!(
                    "steps[{index}].actions[{action_index}].strength must be a number from 0 to 1."
                ));
            }
            event["strength"] = json!(number);
        }
        actions.push(event);
    }
    Ok(actions)
}

fn parse_pointers(index: usize, value: &Value) -> Result<Vec<Value>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("steps[{index}].pointers must be an array."))?;
    if items.len() > MAX_INPUTS {
        return Err(format!(
            "steps[{index}] has more than {MAX_INPUTS} actions and pointers."
        ));
    }
    let mut pointers = Vec::new();
    for (pointer_index, item) in items.iter().enumerate() {
        let kind = item.get("pointer").and_then(Value::as_str).unwrap_or("");
        if !matches!(kind, "mouse" | "touch") {
            return Err(format!(
                "steps[{index}].pointers[{pointer_index}].pointer must be mouse or touch."
            ));
        }
        let phase = item.get("phase").and_then(Value::as_str).unwrap_or("tap");
        if !matches!(phase, "press" | "release" | "move" | "tap") {
            return Err(format!(
                "steps[{index}].pointers[{pointer_index}].phase must be press, release, move, or tap."
            ));
        }
        let x = item.get("x").and_then(Value::as_f64).ok_or_else(|| {
            format!("steps[{index}].pointers[{pointer_index}] needs a numeric x and y.")
        })?;
        let y = item.get("y").and_then(Value::as_f64).ok_or_else(|| {
            format!("steps[{index}].pointers[{pointer_index}] needs a numeric x and y.")
        })?;
        let mut event = json!({"pointer": kind, "phase": phase, "x": x, "y": y});
        if let Some(touch_index) = item.get("index") {
            let touch_index = whole(Some(touch_index))
                .filter(|touch| (0..=MAX_TOUCH_INDEX).contains(touch))
                .ok_or_else(|| {
                    format!(
                        "steps[{index}].pointers[{pointer_index}].index must be an integer from 0 to {MAX_TOUCH_INDEX}."
                    )
                })?;
            event["index"] = json!(touch_index);
        }
        if let Some(button) = item.get("button").and_then(Value::as_str) {
            if button.chars().count() > MAX_NAME_CHARS {
                return Err(format!(
                    "steps[{index}].pointers[{pointer_index}].button is too long."
                ));
            }
            event["button"] = json!(button);
        }
        pointers.push(event);
    }
    Ok(pointers)
}

/// A property name the snapshot may read: no segment of a `/` or `:` path starts with `_`.
fn is_public_property(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && name
            .split(['/', ':'])
            .all(|segment| !segment.starts_with('_'))
}

fn string_array(value: Option<&Value>, name: &str) -> Result<Vec<String>, String> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|item| !item.is_empty() && item.chars().count() <= MAX_NAME_CHARS)
                    .map(str::to_string)
                    .ok_or_else(|| format!("{name} must be an array of node paths."))
            })
            .collect(),
        Some(_) => Err(format!("{name} must be an array of node paths.")),
    }
}

fn whole(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.fract() == 0.0 && number.is_finite())
            .map(|number| number as i64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(args: Value) -> String {
        match parse(&args) {
            Ok(_) => panic!("accepted: {args}"),
            Err(message) => message,
        }
    }

    #[test]
    fn limits_are_refused_before_a_game_is_touched() {
        let many = |count: usize| vec![json!({"action": "jump", "phase": "press"}); count];
        let too_many_steps = json!({"steps": vec![json!({"frames": 1}); MAX_STEPS + 1]});
        assert!(refused(too_many_steps).contains("steps"));
        let too_long = json!({"steps": vec![json!({"frames": 600}); 7]});
        assert!(refused(too_long).contains("3600"));
        let too_many_inputs = json!({"steps": [{"frames": 1, "actions": many(MAX_INPUTS + 1)}]});
        assert!(refused(too_many_inputs).contains("more than"));
        let nodes = vec![json!({"path": "Player", "properties": ["hp"]}); MAX_READS + 1];
        let message = refused(json!({"steps": [{"frames": 1}], "nodes": nodes}));
        assert!(message.contains("at most 16 nodes"), "{message}");
        let bodies = vec![json!("Player"); MAX_READS + 1];
        let message = refused(json!({"steps": [{"frames": 1}], "bodies": bodies}));
        assert!(message.contains("at most 16 nodes"), "{message}");
        let long_body =
            json!({"steps": [{"frames": 1}], "bodies": ["x".repeat(MAX_NAME_CHARS + 1)]});
        assert!(refused(long_body).contains("array of node paths"));
        let long_button = json!({"steps": [{"frames": 1, "pointers": [
            {"pointer": "mouse", "x": 1, "y": 1, "button": "x".repeat(MAX_NAME_CHARS + 1)}]}]});
        assert!(refused(long_button).contains("button is too long"));
        let negative = json!({"steps": [{"frames": 1, "pointers": [{"pointer": "touch", "x": 1, "y": 1, "index": -1}]}]});
        assert!(refused(negative).contains("from 0 to 9"));
        let properties = vec![json!("hp"); MAX_PROPERTIES + 1];
        let many_properties = json!({"steps": [{"frames": 1}],
            "nodes": [{"path": "Player", "properties": properties}]});
        assert!(refused(many_properties).contains("properties"));
        let long = "x".repeat(MAX_NAME_CHARS + 1);
        let long_path = json!({"steps": [{"frames": 1}],
            "nodes": [{"path": long, "properties": ["hp"]}]});
        assert!(refused(long_path).contains("path"));
    }

    #[test]
    fn input_arguments_are_validated() {
        let step = |extra: Value| {
            let mut step = json!({"frames": 1});
            for (key, value) in extra.as_object().into_iter().flatten() {
                step[key] = value.clone();
            }
            json!({"steps": [step]})
        };
        let bad_phase = step(json!({"actions": [{"action": "jump", "phase": "hold"}]}));
        assert!(refused(bad_phase).contains("phase"));
        assert!(refused(step(json!({"actions": [{"action": ""}]}))).contains("action"));
        let bad_pointer = step(json!({"pointers": [{"pointer": "pen", "x": 1, "y": 1}]}));
        assert!(refused(bad_pointer).contains("mouse or touch"));
        let no_y = step(json!({"pointers": [{"pointer": "touch", "x": 1}]}));
        assert!(refused(no_y).contains("numeric"));
        let bad_index =
            step(json!({"pointers": [{"pointer": "touch", "x": 1, "y": 1, "index": 10}]}));
        assert!(refused(bad_index).contains("index"));
        assert!(refused(step(json!({"pause": "yes"}))).contains("pause"));
    }

    #[test]
    fn private_properties_are_refused_whatever_the_spelling() {
        for name in ["_secret", "child/_secret", "material:_secret", "a/b:_c", ""] {
            assert!(!is_public_property(name), "{name}");
            let message = refused(json!({"steps": [{"frames": 1}],
                "nodes": [{"path": "Player", "properties": [name]}]}));
            assert!(message.contains("public property"), "{name}: {message}");
        }
        for name in ["jump_count", "position/x", "material:shader_parameter/a"] {
            assert!(is_public_property(name), "{name}");
        }
    }

    #[test]
    fn a_run_is_locked_for_one_scenario_at_a_time() {
        let first = RunLock::take("lock-test-run").expect("free run");
        assert!(
            RunLock::take("lock-test-run").is_none(),
            "second lock on one run"
        );
        assert!(RunLock::take("another-run").is_some());
        drop(first);
        assert!(
            RunLock::take("lock-test-run").is_some(),
            "lock not released"
        );
    }

    #[tokio::test]
    async fn cancellation_during_release_keeps_the_guard_armed() {
        let mut guard = Some(());
        let result = tokio::time::timeout(
            Duration::from_millis(10),
            disarm_after_success(&mut guard, std::future::pending()),
        )
        .await;
        assert!(result.is_err());
        assert!(guard.is_some(), "cancellation disarmed cleanup");
        let failed = disarm_after_success(&mut guard, async {
            Err(McpError::Process("disconnected".into()))
        })
        .await;
        assert!(failed.is_err());
        assert!(guard.is_some(), "failed cleanup disarmed retry");
        disarm_after_success(&mut guard, async { Ok(json!({"held": {}})) })
            .await
            .unwrap();
        assert!(guard.is_none(), "successful cleanup must disarm the retry");
    }

    // Windows shares one ~15ms timer tick, so two 20ms sleeps can finish inside a
    // 30ms deadline. Paused time checks the shared deadline without that race.
    #[tokio::test(start_paused = true)]
    async fn the_deadline_covers_all_awaits_instead_of_restarting_per_operation() {
        let deadline = Instant::now() + Duration::from_millis(30);
        let operation = within_deadline(deadline, async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(())
        });
        tokio::pin!(operation);

        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        assert!(
            operation.as_mut().poll(&mut context).is_pending(),
            "finished before the deadline"
        );

        tokio::time::advance(Duration::from_millis(30)).await;

        match operation.as_mut().poll(&mut context) {
            std::task::Poll::Ready(result) => {
                assert!(result.is_err(), "each await got a fresh budget");
            }
            std::task::Poll::Pending => panic!("the shared deadline did not fire"),
        }
    }
}
