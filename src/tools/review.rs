//! Reviewing assets in one call: the given scenes are laid out, lit and framed in a temporary
//! Godot process that renders (a small window, since a headless Godot draws nothing), and the
//! answer is one PNG with each asset's triangles, draw calls and bounds.
//!
//! The scene is built in memory by a fixed script (`review_assets.gd`) that runs from a scratch
//! directory outside the project and reads the checked request from a JSON file there, so no
//! argument reaches the command line or generated source, and nothing is saved into the project.

use super::ToolManager;
use crate::paths::{res_display, resolve_in_project};
use crate::protocol::{Tool, ToolContent, ToolResult};
use serde_json::{Map, Value, json};
use std::path::Path;

pub const NAME: &str = "godot_review_assets";

const SCRIPT: &str = include_str!("review_assets.gd");
const MARKER: &str = "GODOT_MCP_REVIEW=";
const MAX_ASSETS: usize = 64;
const MAX_PLANES: usize = 8;
const MAX_SIDE: i64 = 4096;
const MAX_PIXELS: i64 = 4096 * 2304;
const DEFAULT_TIMEOUT: u64 = 120;

pub fn handles(name: &str) -> bool {
    name == NAME
}

pub fn catalog() -> Vec<Tool> {
    let color = json!({"description": "\"#rrggbb\", \"#rrggbbaa\" or [r, g, b] / [r, g, b, a] from 0 to 1."});
    vec![super::tool(
        NAME,
        "Renders a set of the project's scenes together in one picture, the way an asset review beside its concept needs it, without writing any scene to the project: a temporary Godot process (a small window, since a headless Godot does not render; audio muted) builds the review in memory with the project's own settings, autoloads and imports, renders it and exits. Without positions the assets stand in a row along X in the given order, gap metres apart, centred on the origin. The environment can be borrowed from a level scene (its first WorldEnvironment and every DirectionalLight3D, copied without their scripts), from an Environment resource, or left to a neutral default; light sets one sun instead of the borrowed ones. The camera is orthogonal by default, at yaw -35 and pitch -30, and fits the assets' bounds unless a size (orthogonal) or distance (perspective) is given; camera.scene copies a Camera3D's projection, size, fov, angle, near/far and attributes (the current one, or camera.node), and fits its view on the assets unless keep_position is true. planes adds ground or water planes at a height under the assets. Returns the PNG and JSON: per asset its scene, placement, bounds (world AABB of its visible geometry), triangles, surfaces and mesh_instances counted from its meshes (a MultiMesh counts each visible instance), and rendered, Godot's render info for a frame with that asset alone and no planes (objects, primitives and draw_calls of the visible pass, and shadow_ ones of the shadow pass); totals sums the meshes and gives the rendered counts of the whole picture with its planes; camera is the camera that was used. Assets must be imported: stale imports are reimported through the editor when it has the project open, otherwise named in a warning. Needs GODOT_PATH and a display. The engine may update caches under the project's .godot folder, as any run does.",
        json!({
            "assets": {
                "type": "array",
                "description": "1 to 64 scenes to review, in order.",
                "items": {
                    "type": "object",
                    "properties": {
                        "scene": {"type": "string", "description": "res:// path of a .tscn or .scn file, such as res://assets/rock.tscn."},
                        "position": {"description": "[x, y, z] world position. Assets without one are laid out in a row.", "type": "array", "items": {"type": "number"}},
                        "rotation": {"description": "[x, y, z] rotation in degrees.", "type": "array", "items": {"type": "number"}},
                        "scale": {"description": "A number, or [x, y, z]."}
                    },
                    "required": ["scene"],
                    "additionalProperties": false
                }
            },
            "gap": {"type": "number", "minimum": 0, "maximum": 1000, "description": "Metres between the bounds of assets laid out in a row. Default 1."},
            "environment": {
                "type": "object",
                "description": "Lighting. scene and resource cannot be combined. Omitted: a grey background, soft ambient light and one sun.",
                "properties": {
                    "scene": {"type": "string", "description": "A level scene whose first WorldEnvironment and DirectionalLight3D nodes are copied, such as res://levels/old_pond.tscn."},
                    "resource": {"type": "string", "description": "An Environment resource (.tres or .res)."},
                    "light": {
                        "type": "object",
                        "description": "One sun, replacing the scene's lights: yaw and pitch in degrees (defaults -40 and -50), energy (default 1), color, shadows (default true).",
                        "properties": {
                            "yaw": {"type": "number"},
                            "pitch": {"type": "number"},
                            "energy": {"type": "number"},
                            "color": color,
                            "shadows": {"type": "boolean"}
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            },
            "camera": {
                "type": "object",
                "description": "The view. Every field is optional.",
                "properties": {
                    "projection": {"type": "string", "enum": ["orthogonal", "perspective"]},
                    "yaw": {"type": "number", "description": "Degrees about Y. Default -35."},
                    "pitch": {"type": "number", "description": "Degrees about X; negative looks down. Default -30."},
                    "size": {"type": "number", "description": "Orthogonal size in metres (the height, as Camera3D.size). Setting it turns fitting off."},
                    "fov": {"type": "number", "description": "Perspective vertical field of view in degrees, 1 to 179. Default 75."},
                    "distance": {"type": "number", "description": "Metres from the target to the camera. Setting it turns fitting off for a perspective camera."},
                    "target": {"description": "[x, y, z] the camera looks at. Default: the centre of the assets' bounds.", "type": "array", "items": {"type": "number"}},
                    "fit": {"type": "boolean", "description": "Size (orthogonal) or move back (perspective) so every asset is in view. Default true unless size, distance or a copied orthogonal camera sets the scale."},
                    "margin": {"type": "number", "description": "Free border on each side when fitting, as a fraction of the picture, 0 to 0.45. Default 0.05."},
                    "scene": {"type": "string", "description": "A scene with the Camera3D to copy, such as the game's player scene."},
                    "node": {"type": "string", "description": "Path of the Camera3D in camera.scene, relative to its root. Default: its current camera, else its first."},
                    "keep_position": {"type": "boolean", "description": "Keep the copied camera's own position instead of aiming it at the assets. Default false."}
                },
                "additionalProperties": false
            },
            "planes": {
                "type": "array",
                "description": "Up to 8 horizontal planes, centred under the assets.",
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "enum": ["ground", "water"], "description": "Default ground. Water is a translucent blue-green."},
                        "height": {"type": "number", "description": "Y of the plane. Default 0."},
                        "size": {"description": "A number or [x, z] in metres. Default three times the assets' extent, at least 10."},
                        "color": color,
                        "material": {"type": "string", "description": "A Material resource of the project to use instead of the plain one."}
                    },
                    "additionalProperties": false
                }
            },
            "width": {"type": "integer", "minimum": 16, "maximum": MAX_SIDE, "description": "Picture width in pixels. Default 1280."},
            "height": {"type": "integer", "minimum": 16, "maximum": MAX_SIDE, "description": "Picture height in pixels. Default 720. width * height is at most 4096 * 2304."},
            "settle_frames": {"type": "integer", "minimum": 1, "maximum": 240, "description": "Frames rendered before the picture is taken, for shadows and effects that build up over frames. Default 8."},
            "timeout_seconds": {"type": "integer", "minimum": 10, "maximum": 600, "description": "Limit for the whole run. Default 120."},
            "project_path": {"type": "string"}
        }),
        &["assets"],
    )]
}

pub async fn call(tools: &ToolManager, args: Value) -> ToolResult {
    let project = match tools.resolve_project(&args).await {
        Ok(project) => project,
        Err(result) => return result,
    };
    let request = match review_request(&project.dir, &args) {
        Ok(request) => request,
        Err(problem) => {
            return ToolResult::error(
                format!("Invalid review request: {problem}"),
                &["Check the arguments against the godot_review_assets inputSchema and retry"],
            );
        }
    };
    let timeout_seconds = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT);
    // The process loads the imported cache as it is, as godot_run_script does.
    let imports = tools.check_imports(&project.dir).await;
    let (run, image) = match tools
        .cli
        .review_assets(&project.dir, SCRIPT, request, timeout_seconds)
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            return ToolResult::error(
                format!("Failed to run the review: {error}"),
                &["Verify GODOT_PATH points to a working Godot executable"],
            );
        }
    };
    let answer = run
        .stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(MARKER))
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let Some(mut answer) = answer else {
        let detail = if run.timed_out {
            format!("Godot did not finish the review within {timeout_seconds}s")
        } else {
            format!(
                "Godot gave no answer (exit {})",
                run.exit_code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string())
            )
        };
        return ToolResult::error(
            format!("{detail}. Its output ends with:\n{}", output_tail(&run)),
            &[
                "Godot must be able to open a window: run the server in a desktop session (a headless Godot does not render)",
                "Raise timeout_seconds when the project takes long to load",
                "Run the project once (godot_run_project) to see whether an autoload fails on start",
            ],
        );
    };
    if let Some(problem) = answer.get("error").and_then(Value::as_str) {
        return ToolResult::error(
            format!("The review failed: {problem}"),
            &[
                "Check that each path names a scene or resource of the project and that it is imported (godot_resave_resources)",
                "Open the scene in the editor to see whether it loads",
            ],
        );
    }
    let Some(png) = image.filter(|bytes| bytes.starts_with(b"\x89PNG")) else {
        return ToolResult::error(
            "The review ran but saved no picture",
            &["Retry; if it repeats, run the project once to check that it renders"],
        );
    };
    if let Some(map) = answer.as_object_mut() {
        map.remove("image");
        add_run_warnings(map, &run, imports.as_ref());
    }
    ToolResult {
        content: vec![
            ToolContent::Text {
                text: serde_json::to_string_pretty(&answer).unwrap_or_default(),
            },
            ToolContent::Image {
                data: base64_encode(&png),
                mime_type: "image/png".into(),
            },
        ],
        is_error: false,
    }
}

/// Script errors printed while the assets loaded (their own scripts), and stale imports.
fn add_run_warnings(
    answer: &mut Map<String, Value>,
    run: &crate::cli_runner::ScriptRun,
    imports: Option<&super::run::StaleImports>,
) {
    let mut warnings: Vec<Value> = answer
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for error in run.script_errors.iter().take(10) {
        warnings.push(json!(format!(
            "script error in {}:{}: {}",
            error.file, error.line, error.message
        )));
    }
    if let Some(stale) = imports {
        warnings.push(json!(stale.sentence("review")));
    }
    answer.insert("warnings".into(), Value::Array(warnings));
}

fn output_tail(run: &crate::cli_runner::ScriptRun) -> String {
    let output = format!("{}\n{}", run.stdout.trim(), run.stderr.trim());
    let output = output.trim();
    let start = output
        .char_indices()
        .rev()
        .nth(1999)
        .map_or(0, |(index, _)| index);
    output[start..].to_string()
}

/// Checks the arguments and turns them into the request the review script reads: every path
/// becomes a res:// path of an existing file of the project, every vector three numbers.
pub(crate) fn review_request(project: &Path, args: &Value) -> Result<Value, String> {
    let assets = args
        .get("assets")
        .and_then(Value::as_array)
        .ok_or("assets must be a list of scenes")?;
    if assets.is_empty() || assets.len() > MAX_ASSETS {
        return Err(format!("assets must hold 1 to {MAX_ASSETS} scenes"));
    }
    let mut checked = Vec::with_capacity(assets.len());
    for (index, asset) in assets.iter().enumerate() {
        let field = format!("assets[{index}]");
        let asset = object(asset, &field, &["scene", "position", "rotation", "scale"])?;
        let mut entry = Map::new();
        let scene = asset
            .get("scene")
            .and_then(Value::as_str)
            .ok_or(format!("{field}.scene must be a res:// scene path"))?;
        entry.insert(
            "scene".into(),
            json!(project_file(
                project,
                scene,
                &format!("{field}.scene"),
                &["tscn", "scn"]
            )?),
        );
        for key in ["position", "rotation"] {
            if let Some(value) = asset.get(key) {
                entry.insert(key.into(), vector3(value, &format!("{field}.{key}"))?);
            }
        }
        if let Some(value) = asset.get("scale") {
            let scale = match value.as_f64() {
                Some(uniform) => json!([uniform, uniform, uniform]),
                None => vector3(value, &format!("{field}.scale"))?,
            };
            if scale
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part.as_f64() == Some(0.0)))
            {
                return Err(format!("{field}.scale must not be 0 on any axis"));
            }
            entry.insert("scale".into(), scale);
        }
        checked.push(Value::Object(entry));
    }

    let mut request = Map::new();
    request.insert("assets".into(), Value::Array(checked));
    request.insert(
        "gap".into(),
        json!(args.get("gap").and_then(Value::as_f64).unwrap_or(1.0)),
    );
    if let Some(environment) = args.get("environment") {
        request.insert(
            "environment".into(),
            environment_request(project, environment)?,
        );
    }
    if let Some(camera) = args.get("camera") {
        request.insert("camera".into(), camera_request(project, camera)?);
    }
    if let Some(planes) = args.get("planes") {
        request.insert("planes".into(), planes_request(project, planes)?);
    }
    let width = args.get("width").and_then(Value::as_i64).unwrap_or(1280);
    let height = args.get("height").and_then(Value::as_i64).unwrap_or(720);
    if !(16..=MAX_SIDE).contains(&width) || !(16..=MAX_SIDE).contains(&height) {
        return Err(format!("width and height must be from 16 to {MAX_SIDE}"));
    }
    if width * height > MAX_PIXELS {
        return Err(format!(
            "width * height is {}, more than {MAX_PIXELS} (4096 * 2304)",
            width * height
        ));
    }
    request.insert("width".into(), json!(width));
    request.insert("height".into(), json!(height));
    request.insert(
        "settle_frames".into(),
        json!(
            args.get("settle_frames")
                .and_then(Value::as_i64)
                .unwrap_or(8)
        ),
    );
    Ok(Value::Object(request))
}

fn environment_request(project: &Path, value: &Value) -> Result<Value, String> {
    let environment = object(value, "environment", &["scene", "resource", "light"])?;
    let mut out = Map::new();
    if environment.contains_key("scene") && environment.contains_key("resource") {
        return Err(
            "environment takes scene or resource, not both: both would bring a WorldEnvironment"
                .into(),
        );
    }
    if let Some(scene) = environment.get("scene") {
        out.insert(
            "scene".into(),
            json!(path_field(
                project,
                scene,
                "environment.scene",
                &["tscn", "scn"]
            )?),
        );
    }
    if let Some(resource) = environment.get("resource") {
        out.insert(
            "resource".into(),
            json!(path_field(
                project,
                resource,
                "environment.resource",
                &["tres", "res"]
            )?),
        );
    }
    if let Some(light) = environment.get("light") {
        let light = object(
            light,
            "environment.light",
            &["yaw", "pitch", "energy", "color", "shadows"],
        )?;
        let mut sun = Map::new();
        for key in ["yaw", "pitch", "energy"] {
            if let Some(value) = light.get(key) {
                sun.insert(
                    key.into(),
                    json!(number(value, &format!("environment.light.{key}"))?),
                );
            }
        }
        if let Some(energy) = sun.get("energy").and_then(Value::as_f64) {
            if !(0.0..=16.0).contains(&energy) {
                return Err("environment.light.energy must be from 0 to 16".into());
            }
        }
        if let Some(color) = light.get("color") {
            sun.insert(
                "color".into(),
                color_value(color, "environment.light.color")?,
            );
        }
        if let Some(shadows) = light.get("shadows") {
            sun.insert(
                "shadows".into(),
                json!(
                    shadows
                        .as_bool()
                        .ok_or("environment.light.shadows must be true or false")?
                ),
            );
        }
        out.insert("light".into(), Value::Object(sun));
    }
    Ok(Value::Object(out))
}

fn camera_request(project: &Path, value: &Value) -> Result<Value, String> {
    let camera = object(
        value,
        "camera",
        &[
            "projection",
            "yaw",
            "pitch",
            "size",
            "fov",
            "distance",
            "target",
            "fit",
            "margin",
            "scene",
            "node",
            "keep_position",
        ],
    )?;
    let mut out = Map::new();
    if let Some(projection) = camera.get("projection") {
        match projection.as_str() {
            Some(name @ ("orthogonal" | "perspective")) => {
                out.insert("projection".into(), json!(name));
            }
            _ => return Err("camera.projection must be orthogonal or perspective".into()),
        }
    }
    for (key, low, high) in [
        ("yaw", -3600.0, 3600.0),
        ("pitch", -90.0, 90.0),
        ("size", 0.001, 100_000.0),
        ("fov", 1.0, 179.0),
        ("distance", 0.01, 100_000.0),
        ("margin", 0.0, 0.45),
    ] {
        if let Some(value) = camera.get(key) {
            let number = number(value, &format!("camera.{key}"))?;
            if !(low..=high).contains(&number) {
                return Err(format!("camera.{key} must be from {low} to {high}"));
            }
            out.insert(key.into(), json!(number));
        }
    }
    if let Some(target) = camera.get("target") {
        out.insert("target".into(), vector3(target, "camera.target")?);
    }
    for key in ["fit", "keep_position"] {
        if let Some(flag) = camera.get(key) {
            let flag = flag
                .as_bool()
                .ok_or(format!("camera.{key} must be true or false"))?;
            out.insert(key.into(), json!(flag));
        }
    }
    if let Some(scene) = camera.get("scene") {
        out.insert(
            "scene".into(),
            json!(path_field(
                project,
                scene,
                "camera.scene",
                &["tscn", "scn"]
            )?),
        );
    }
    if let Some(node) = camera.get("node") {
        if !out.contains_key("scene") {
            return Err("camera.node needs camera.scene, the scene it is in".into());
        }
        let node = node
            .as_str()
            .filter(|node| !node.trim().is_empty())
            .ok_or("camera.node must be a node path such as Player/Camera3D")?;
        out.insert("node".into(), json!(node));
    }
    if out.contains_key("keep_position") && !out.contains_key("scene") {
        return Err("camera.keep_position needs camera.scene, the camera to copy".into());
    }
    Ok(Value::Object(out))
}

fn planes_request(project: &Path, value: &Value) -> Result<Value, String> {
    let planes = value.as_array().ok_or("planes must be a list")?;
    if planes.len() > MAX_PLANES {
        return Err(format!("planes holds at most {MAX_PLANES} planes"));
    }
    let mut out = Vec::with_capacity(planes.len());
    for (index, plane) in planes.iter().enumerate() {
        let field = format!("planes[{index}]");
        let plane = object(
            plane,
            &field,
            &["kind", "height", "size", "color", "material"],
        )?;
        let mut entry = Map::new();
        let kind = match plane.get("kind") {
            None => "ground",
            Some(kind) => match kind.as_str() {
                Some(name @ ("ground" | "water")) => name,
                _ => return Err(format!("{field}.kind must be ground or water")),
            },
        };
        entry.insert("kind".into(), json!(kind));
        if let Some(height) = plane.get("height") {
            entry.insert(
                "height".into(),
                json!(number(height, &format!("{field}.height"))?),
            );
        }
        if let Some(size) = plane.get("size") {
            let size = match size.as_f64() {
                Some(side) => vec![side, side],
                None => numbers(size, 2, &format!("{field}.size"))?,
            };
            if size.iter().any(|side| *side <= 0.0 || *side > 100_000.0) {
                return Err(format!("{field}.size must be above 0"));
            }
            entry.insert("size".into(), json!(size));
        }
        if let Some(color) = plane.get("color") {
            entry.insert(
                "color".into(),
                color_value(color, &format!("{field}.color"))?,
            );
        }
        if let Some(material) = plane.get("material") {
            entry.insert(
                "material".into(),
                json!(path_field(
                    project,
                    material,
                    &format!("{field}.material"),
                    &["tres", "res", "material"],
                )?),
            );
        }
        out.push(Value::Object(entry));
    }
    Ok(Value::Array(out))
}

/// `value` as an object holding only `allowed` keys.
fn object<'a>(
    value: &'a Value,
    field: &str,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let map = value
        .as_object()
        .ok_or(format!("{field} must be an object"))?;
    if let Some(unknown) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "{field} has no field '{unknown}'; it takes {}",
            allowed.join(", ")
        ));
    }
    Ok(map)
}

fn path_field(
    project: &Path,
    value: &Value,
    field: &str,
    extensions: &[&str],
) -> Result<String, String> {
    let path = value
        .as_str()
        .ok_or(format!("{field} must be a res:// path"))?;
    project_file(project, path, field, extensions)
}

/// The res:// path of an existing file of the project with one of `extensions`.
fn project_file(
    project: &Path,
    requested: &str,
    field: &str,
    extensions: &[&str],
) -> Result<String, String> {
    let file =
        resolve_in_project(project, requested).map_err(|problem| format!("{field}: {problem}"))?;
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !extensions.contains(&extension.as_str()) {
        return Err(format!(
            "{field} must be a .{} file: {requested}",
            extensions.join(" or .")
        ));
    }
    if !file.is_file() {
        return Err(format!(
            "{field}: no such file in the project: {}",
            res_display(project, &file)
        ));
    }
    Ok(res_display(project, &file))
}

fn number(value: &Value, field: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or(format!("{field} must be a number"))
}

fn numbers(value: &Value, count: usize, field: &str) -> Result<Vec<f64>, String> {
    let parts = value
        .as_array()
        .filter(|parts| parts.len() == count)
        .ok_or(format!("{field} must be a list of {count} numbers"))?;
    parts
        .iter()
        .map(|part| number(part, field))
        .collect::<Result<Vec<_>, _>>()
}

fn vector3(value: &Value, field: &str) -> Result<Value, String> {
    Ok(json!(numbers(value, 3, &format!("{field} ([x, y, z])"))?))
}

/// A colour as "#rrggbb" / "#rrggbbaa", or [r, g, b] / [r, g, b, a] from 0 to 1.
fn color_value(value: &Value, field: &str) -> Result<Value, String> {
    if let Some(text) = value.as_str() {
        let hex = text.strip_prefix('#').unwrap_or(text);
        if matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(json!(format!("#{hex}")));
        }
        return Err(format!(
            "{field} must be \"#rrggbb\" or \"#rrggbbaa\": {text}"
        ));
    }
    let parts = value
        .as_array()
        .filter(|parts| matches!(parts.len(), 3 | 4))
        .ok_or(format!("{field} must be \"#rrggbb\" or [r, g, b(, a)]"))?;
    let parts = parts
        .iter()
        .map(|part| number(part, field))
        .collect::<Result<Vec<_>, _>>()?;
    if parts.iter().any(|part| !(0.0..=1.0).contains(part)) {
        return Err(format!("{field} components must be from 0 to 1"));
    }
    Ok(json!(parts))
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for position in 0..4 {
            if position <= chunk.len() {
                let index = (triple >> (18 - 6 * position)) & 63;
                out.push(char::from(ALPHABET[index as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(dir.path().join("project.godot"), "config_version=5\n").expect("project");
        std::fs::create_dir_all(dir.path().join("kit")).expect("kit");
        for file in [
            "kit/rock.tscn",
            "kit/log.scn",
            "level.tscn",
            "sky.tres",
            "water.tres",
        ] {
            std::fs::write(dir.path().join(file), "").expect(file);
        }
        dir
    }

    #[test]
    fn a_minimal_request_gets_its_defaults_and_res_paths() {
        let dir = project();
        let request = review_request(dir.path(), &json!({"assets": [{"scene": "kit/rock.tscn"}]}))
            .expect("request");
        assert_eq!(request["assets"][0]["scene"], "res://kit/rock.tscn");
        assert_eq!(request["gap"], 1.0);
        assert_eq!(request["width"], 1280);
        assert_eq!(request["height"], 720);
        assert_eq!(request["settle_frames"], 8);
        assert!(request.get("camera").is_none(), "{request}");
        assert!(request.get("output").is_none(), "{request}");
    }

    #[test]
    fn placement_environment_camera_and_planes_are_passed_on_checked() {
        let dir = project();
        let request = review_request(
            dir.path(),
            &json!({
                "assets": [
                    {"scene": "res://kit/rock.tscn", "position": [1, 0, -2], "rotation": [0, 90, 0], "scale": 2},
                    {"scene": "res://kit/log.scn", "scale": [1, 2, 1]}
                ],
                "gap": 3,
                "environment": {"scene": "res://level.tscn", "light": {"yaw": 10, "color": "#ffcc88", "shadows": false}},
                "camera": {"scene": "res://level.tscn", "node": "Player/Camera3D", "pitch": -45, "margin": 0.1},
                "planes": [{"kind": "water", "height": -0.1, "size": 40, "color": [0.2, 0.5, 0.6, 0.7]}, {"material": "res://water.tres"}],
                "width": 640,
                "height": 480
            }),
        )
        .expect("request");
        assert_eq!(request["assets"][0]["scale"], json!([2.0, 2.0, 2.0]));
        assert_eq!(request["assets"][0]["position"], json!([1.0, 0.0, -2.0]));
        assert_eq!(request["assets"][1]["scene"], "res://kit/log.scn");
        assert_eq!(request["environment"]["scene"], "res://level.tscn");
        assert_eq!(request["environment"]["light"]["color"], "#ffcc88");
        assert_eq!(request["environment"]["light"]["shadows"], false);
        assert_eq!(request["camera"]["node"], "Player/Camera3D");
        assert_eq!(request["camera"]["pitch"], -45.0);
        assert_eq!(request["planes"][0]["size"], json!([40.0, 40.0]));
        assert_eq!(request["planes"][0]["kind"], "water");
        assert_eq!(request["planes"][1]["kind"], "ground");
        assert_eq!(request["planes"][1]["material"], "res://water.tres");
    }

    #[test]
    fn paths_must_stay_in_the_project_and_name_existing_files_of_the_right_kind() {
        let dir = project();
        let refused = |args: Value, needle: &str| {
            let problem = review_request(dir.path(), &args).expect_err("refused");
            assert!(problem.contains(needle), "{needle}: {problem}");
        };
        refused(
            json!({"assets": [{"scene": "res://../outside.tscn"}]}),
            "assets[0].scene",
        );
        refused(
            json!({"assets": [{"scene": "res://missing.tscn"}]}),
            "no such file",
        );
        refused(
            json!({"assets": [{"scene": "res://sky.tres"}]}),
            ".tscn or .scn",
        );
        refused(
            json!({"assets": [{"scene": "res://level.tscn"}], "environment": {"resource": "res://level.tscn"}}),
            ".tres or .res",
        );
        refused(
            json!({"assets": [{"scene": "res://level.tscn"}], "environment": {"scene": "res://level.tscn", "resource": "res://sky.tres"}}),
            "not both",
        );
        refused(
            json!({"assets": [{"scene": "res://level.tscn"}], "camera": {"scene": "/etc/passwd.tscn"}}),
            "camera.scene",
        );
    }

    #[test]
    fn nested_values_are_checked_with_their_field_names() {
        let dir = project();
        let refused = |args: Value, needle: &str| {
            let problem = review_request(dir.path(), &args).expect_err("refused");
            assert!(problem.contains(needle), "{needle}: {problem}");
        };
        let rock = json!({"scene": "res://kit/rock.tscn"});
        refused(json!({"assets": []}), "1 to 64");
        refused(
            json!({"assets": [{"scene": "res://kit/rock.tscn", "spin": 1}]}),
            "no field 'spin'",
        );
        refused(
            json!({"assets": [{"scene": "res://kit/rock.tscn", "position": [1, 2]}]}),
            "assets[0].position",
        );
        refused(
            json!({"assets": [{"scene": "res://kit/rock.tscn", "scale": 0}]}),
            "must not be 0",
        );
        refused(
            json!({"assets": [rock], "camera": {"projection": "fisheye"}}),
            "orthogonal or perspective",
        );
        refused(
            json!({"assets": [rock], "camera": {"fov": 200}}),
            "camera.fov",
        );
        refused(
            json!({"assets": [rock], "camera": {"node": "Camera3D"}}),
            "needs camera.scene",
        );
        refused(
            json!({"assets": [rock], "camera": {"keep_position": true}}),
            "needs camera.scene",
        );
        refused(
            json!({"assets": [rock], "planes": [{"kind": "lava"}]}),
            "ground or water",
        );
        refused(
            json!({"assets": [rock], "planes": [{"color": "blue"}]}),
            "#rrggbb",
        );
        refused(
            json!({"assets": [rock], "planes": [{"color": [2, 0, 0]}]}),
            "from 0 to 1",
        );
        refused(
            json!({"assets": [rock], "environment": {"light": {"energy": 99}}}),
            "energy",
        );
        refused(
            json!({"assets": [rock], "width": 4096, "height": 4096}),
            "4096 * 2304",
        );
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xff, 0xfe, 0x00]), "//4A");
    }
}
