//! The export tools against a real headless Godot: a preset created and changed through Godot's
//! own ConfigFile, and a pack export of a small project. Ignored by default, like the other
//! end-to-end tests:
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test export_e2e_test -- --ignored --test-threads=1
//! ```
//!
//! A pack export needs no export templates, so no extra setup is required.

mod common;

use common::{free_port, tool_text};
use godot_mcp::bridge::BridgeClient;
use godot_mcp::cli_runner::CliRunner;
use godot_mcp::protocol::ToolResult;
use godot_mcp::tools::ToolManager;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAIN_SCENE: &str = r#"[gd_scene format=3]

[ext_resource type="Script" path="res://main.gd" id="1"]
[ext_resource type="Texture2D" path="res://icon.svg" id="2"]

[node name="Main" type="Sprite2D"]
texture = ExtResource("2")
script = ExtResource("1")
"#;

const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#4a8"/></svg>"##;

fn godot_binary() -> PathBuf {
    let _ = dotenvy::dotenv();
    std::env::var("GODOT_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .expect("set GODOT_PATH to run the export tests")
}

/// A project with a main scene, its script and a texture, never imported yet.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project dir");
    let root = dir.path();
    std::fs::write(
        root.join("project.godot"),
        "config_version=5\n\n[application]\n\nconfig/name=\"Export tools\"\n\
         run/main_scene=\"res://main.tscn\"\n",
    )
    .expect("project.godot");
    std::fs::write(root.join("main.gd"), "extends Sprite2D\n").expect("main.gd");
    std::fs::write(root.join("main.tscn"), MAIN_SCENE).expect("main.tscn");
    std::fs::write(root.join("icon.svg"), ICON).expect("icon.svg");
    dir
}

fn manager() -> Arc<ToolManager> {
    let cwd = std::env::temp_dir();
    Arc::new(
        ToolManager::new(
            BridgeClient::new(free_port()),
            CliRunner::new(godot_binary()),
        )
        .with_working_dir(cwd),
    )
}

fn text(result: &ToolResult) -> String {
    tool_text(&json!({"result": serde_json::to_value(result).expect("serialize result")}))
}

async fn call(manager: &Arc<ToolManager>, tool: &str, args: Value, project: &Path) -> ToolResult {
    let mut args = args;
    args["project_path"] = json!(project);
    Arc::clone(manager).call_tool(tool, args).await
}

fn report(result: &ToolResult) -> Value {
    assert!(!result.is_error, "{}", text(result));
    serde_json::from_str(&text(result)).expect("JSON report")
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn a_preset_is_created_changed_and_used_for_a_pack_export() {
    let dir = project();
    let root = dir.path();
    let tools = manager();

    let empty = report(&call(&tools, "godot_list_export_presets", json!({}), root).await);
    assert_eq!(empty["file_exists"], false);
    assert_eq!(empty["presets"], json!([]));

    let created = report(
        &call(
            &tools,
            "godot_set_export_preset",
            json!({
                "name": "webOS", "create": true, "platform": "Linux",
                "export_path": "build/game.pck", "exclude_filter": "tests/*",
                "options": {
                    "binary_format/architecture": "arm32",
                    "texture_format/etc2_astc": true,
                    "texture_format/s3tc_bptc": false
                }
            }),
            root,
        )
        .await,
    );
    assert_eq!(created["created"], true);
    assert_eq!(created["preset"]["platform"], "Linux");
    assert_eq!(
        created["preset"]["options"]["binary_format/architecture"],
        "arm32"
    );
    assert_eq!(
        created["preset"]["options"]["texture_format/etc2_astc"],
        true
    );
    let stored = std::fs::read_to_string(root.join("export_presets.cfg")).expect("presets file");
    assert!(stored.contains("[preset.0.options]"), "{stored}");
    assert!(stored.contains("texture_format/etc2_astc=true"), "{stored}");

    let again = report(
        &call(
            &tools,
            "godot_set_export_preset",
            json!({"name": "webOS", "options": {"binary_format/architecture": "arm32"}}),
            root,
        )
        .await,
    );
    assert_eq!(again["created"], false);
    assert_eq!(
        again["changed"],
        json!([]),
        "the same value changes nothing"
    );

    let wrong_type = call(
        &tools,
        "godot_set_export_preset",
        json!({"name": "webOS", "options": {"texture_format/etc2_astc": "yes"}}),
        root,
    )
    .await;
    assert!(wrong_type.is_error);
    assert!(
        text(&wrong_type).contains("holds a bool"),
        "{}",
        text(&wrong_type)
    );

    let listed = report(&call(&tools, "godot_list_export_presets", json!({}), root).await);
    assert_eq!(listed["presets"][0]["name"], "webOS");
    assert_eq!(listed["presets"][0]["exclude_filter"], "tests/*");

    let exported = report(
        &call(
            &tools,
            "godot_export_project",
            json!({"preset": "webOS", "timeout_seconds": 300}),
            root,
        )
        .await,
    );
    assert_eq!(exported["ok"], true, "{exported}");
    assert_eq!(exported["import"]["ran"], true);
    let size = exported["size_bytes"].as_u64().expect("size");
    let pack = root.join("build/game.pck");
    assert_eq!(std::fs::metadata(&pack).expect("pack").len(), size);
    let bytes = std::fs::read(&pack).expect("pack bytes");
    assert!(bytes.starts_with(b"GDPC"), "a Godot pack");
    let names = String::from_utf8_lossy(&bytes);
    assert!(names.contains("main.tscn"), "the main scene is in the pack");
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn an_unusable_release_template_is_an_error_with_the_log() {
    let dir = project();
    let root = dir.path();
    let tools = manager();
    report(
        &call(
            &tools,
            "godot_set_export_preset",
            json!({
                "name": "Linux", "create": true, "platform": "Linux",
                "options": {"custom_template/release": "/nonexistent/godot-template"}
            }),
            root,
        )
        .await,
    );
    let result = call(
        &tools,
        "godot_export_project",
        json!({"preset": "Linux", "mode": "release", "output_path": "build/game.x86_64",
               "timeout_seconds": 300}),
        root,
    )
    .await;
    assert!(result.is_error, "{}", text(&result));
    let message = text(&result);
    assert!(message.contains("failed (exit code"), "{message}");
    assert!(
        message.to_lowercase().contains("template not found"),
        "the reason after Godot's ERROR line is kept: {message}"
    );
    assert!(
        message.contains("\"errors\""),
        "the details carry the log: {message}"
    );
    assert!(!root.join("build/game.x86_64").exists());
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn import_params_are_read_set_copied_and_reimported_headless() {
    let dir = project();
    let root = dir.path();
    std::fs::write(root.join("far.svg"), ICON).expect("far.svg");
    let tools = manager();

    let unimported = call(
        &tools,
        "godot_get_import_params",
        json!({"path": "res://icon.svg"}),
        root,
    )
    .await;
    assert!(unimported.is_error);
    assert!(
        text(&unimported).contains("godot_resave_resources"),
        "{}",
        text(&unimported)
    );
    report(&call(&tools, "godot_resave_resources", json!({}), root).await);

    let read = report(
        &call(
            &tools,
            "godot_get_import_params",
            json!({"path": "res://icon.svg"}),
            root,
        )
        .await,
    );
    assert_eq!(read["source"], "headless");
    assert_eq!(read["importer"], "texture");
    assert_eq!(read["params"]["compress/mode"], 0);
    assert_eq!(read["types"]["compress/mode"], "int");
    assert_eq!(read["types"]["svg/scale"], "float");

    let set = report(
        &call(
            &tools,
            "godot_set_import_params",
            json!({"path": "res://icon.svg", "params": {
                "compress/mode": 2, "compress/high_quality": true, "svg/scale": 2,
                "not/an_option": 1
            }}),
            root,
        )
        .await,
    );
    assert_eq!(set["reimported"], true, "{set}");
    assert_eq!(set["new_options"], json!(["not/an_option"]));
    assert_eq!(set["ignored_options"], json!(["not/an_option"]));
    assert_eq!(set["after"]["params"]["compress/mode"], 2);
    assert_eq!(set["after"]["params"]["compress/high_quality"], true);
    assert_eq!(
        set["after"]["types"]["svg/scale"], "float",
        "a whole number keeps the float type: {set}"
    );
    let sidecar = std::fs::read_to_string(root.join("icon.svg.import")).expect("sidecar");
    assert!(
        sidecar.contains("\"vram_texture\": true"),
        "the reimport wrote VRAM-compressed data: {sidecar}"
    );
    assert!(!sidecar.contains("not/an_option"), "{sidecar}");

    let wrong_type = call(
        &tools,
        "godot_set_import_params",
        json!({"path": "res://icon.svg", "params": {"compress/mode": "two"}}),
        root,
    )
    .await;
    assert!(wrong_type.is_error);
    assert!(
        text(&wrong_type).contains("holds a int"),
        "{}",
        text(&wrong_type)
    );

    let copied = report(
        &call(
            &tools,
            "godot_set_import_params",
            json!({"path": "res://far.svg", "copy_from": "res://icon.svg"}),
            root,
        )
        .await,
    );
    assert_eq!(copied["reimported"], true, "{copied}");
    assert_eq!(copied["after"]["params"]["compress/mode"], 2);
    assert_eq!(copied["after"]["params"]["compress/high_quality"], true);
    assert_eq!(copied["after"]["params"]["svg/scale"], 2.0);
}

#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn an_image_is_imported_as_a_texture_3d_headless() {
    let dir = project();
    let root = dir.path();
    std::fs::write(root.join("strip.svg"), STRIP).expect("strip.svg");
    let tools = manager();
    report(&call(&tools, "godot_resave_resources", json!({}), root).await);

    let wrong = call(
        &tools,
        "godot_set_import_params",
        json!({"path": "res://strip.svg", "importer": "wav"}),
        root,
    )
    .await;
    assert!(wrong.is_error, "{}", text(&wrong));
    assert!(text(&wrong).contains("3d_texture"), "{}", text(&wrong));

    let set = report(
        &call(
            &tools,
            "godot_set_import_params",
            json!({"path": "res://strip.svg", "importer": "3d_texture",
                   "params": {"slices/horizontal": 4, "slices/vertical": 1}}),
            root,
        )
        .await,
    );
    assert_eq!(set["reimported"], true, "{set}");
    assert_eq!(set["importer"], "3d_texture");
    assert_eq!(set["previous_importer"], "texture");
    assert_eq!(set["type"], "CompressedTexture3D", "{set}");
    assert_eq!(set["after"]["params"]["slices/horizontal"], 4);
    assert_eq!(set["after"]["types"]["slices/horizontal"], "int");
    assert!(
        set["after"]["params"].get("svg/scale").is_none(),
        "the texture importer's options are gone: {set}"
    );
    let sidecar = std::fs::read_to_string(root.join("strip.svg.import")).expect("sidecar");
    assert!(sidecar.contains("importer=\"3d_texture\""), "{sidecar}");
    assert!(sidecar.contains(".ctex3d"), "{sidecar}");

    let fallback = call(
        &tools,
        "godot_set_import_params",
        json!({"path": "res://strip.svg", "importer": "no_such_importer"}),
        root,
    )
    .await;
    assert!(fallback.is_error, "{}", text(&fallback));
    assert!(
        text(&fallback).contains("imported it with"),
        "{}",
        text(&fallback)
    );
}

/// A 64×16 strip of four 16×16 slices.
const STRIP: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="16"><rect width="16" height="16" fill="#000"/><rect x="16" width="16" height="16" fill="#555"/><rect x="32" width="16" height="16" fill="#aaa"/><rect x="48" width="16" height="16" fill="#fff"/></svg>"##;

/// A glb with one triangle whose legs are `size` long: every size gives a file of the same length.
fn triangle_glb(size: f32) -> Vec<u8> {
    let json = format!(
        concat!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"#,
            r#""nodes":[{{"mesh":0,"name":"Tri"}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],"#,
            r#""accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[{size:.1},{size:.1},0]}}],"#,
            r#""bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}}],"buffers":[{{"byteLength":36}}]}}"#
        ),
        size = size
    );
    let mut json = json.into_bytes();
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    let mut bin = Vec::new();
    for value in [0.0, 0.0, 0.0, size, 0.0, 0.0, 0.0, size, 0.0] {
        bin.extend(f32::to_le_bytes(value));
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut glb = Vec::new();
    for word in [0x4654_6C67u32, 2, total as u32] {
        glb.extend(word.to_le_bytes());
    }
    glb.extend((json.len() as u32).to_le_bytes());
    glb.extend(0x4E4F_534Au32.to_le_bytes());
    glb.extend(json);
    glb.extend((bin.len() as u32).to_le_bytes());
    glb.extend(0x004E_4942u32.to_le_bytes());
    glb.extend(bin);
    glb
}

const MESH_SIZE_SCRIPT: &str = "extends SceneTree\n\nfunc _initialize() -> void:\n\
    \tvar scene: Node = (load(\"res://m.glb\") as PackedScene).instantiate()\n\
    \tvar mesh: MeshInstance3D = scene.find_child(\"*\", true, false) as MeshInstance3D\n\
    \tprint(\"MESH_SIZE=%.1f\" % mesh.get_aabb().size.x)\n\
    \tscene.free()\n\
    \tquit()\n";

async fn mesh_size(tools: &Arc<ToolManager>, root: &Path) -> String {
    let run = call(
        tools,
        "godot_run_script",
        json!({"script": "res://mesh_size.gd"}),
        root,
    )
    .await;
    let body = text(&run);
    body.split("MESH_SIZE=")
        .nth(1)
        .and_then(|rest| rest.get(..3))
        .unwrap_or_else(|| panic!("no MESH_SIZE in {body}"))
        .to_string()
}

/// A model rewritten twice with new bytes of the same size: each headless resave lists it with
/// its kind; one rewritten within the second Godot's last scan saw is skipped by Godot, named
/// under still_stale, and imported by force.
#[tokio::test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ editor executable"]
async fn a_rewritten_model_is_listed_reported_when_skipped_and_forced_headless() {
    let dir = project();
    let root = dir.path();
    let source = root.join("m.glb");
    std::fs::write(root.join("mesh_size.gd"), MESH_SIZE_SCRIPT).expect("script");
    std::fs::write(&source, triangle_glb(1.0)).expect("glb");
    let tools = manager();
    let paths = json!({"paths": ["res://m.glb"]});
    report(&call(&tools, "godot_resave_resources", json!({}), root).await);
    assert_eq!(mesh_size(&tools, root).await, "1.0");

    // A second second, so Godot's scan sees a new modification time and reimports.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&source, triangle_glb(2.0)).expect("glb");
    assert_eq!(triangle_glb(1.0).len(), triangle_glb(2.0).len());
    let listed = report(&call(&tools, "godot_resave_resources", paths.clone(), root).await);
    assert_eq!(
        listed["imported"],
        json!([{"path": "res://m.glb", "kind": "scene", "type": "PackedScene"}]),
        "{listed}"
    );
    assert_eq!(listed["still_stale"], json!([]), "{listed}");
    assert_eq!(mesh_size(&tools, root).await, "2.0");

    // New bytes whose modification time is in the same whole second as the one Godot recorded.
    let recorded = std::fs::metadata(&source)
        .and_then(|meta| meta.modified())
        .expect("mtime");
    let second = recorded
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs();
    std::fs::write(&source, triangle_glb(3.0)).expect("glb");
    std::fs::File::options()
        .write(true)
        .open(&source)
        .and_then(|file| {
            file.set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(second * 1000 + 900),
            )
        })
        .expect("same second");
    let skipped = call(&tools, "godot_resave_resources", paths.clone(), root).await;
    let body = text(&skipped);
    assert!(skipped.is_error, "{body}");
    assert!(body.contains("\"reason\": \"source_changed\""), "{body}");
    assert!(body.contains("force: true"), "{body}");
    assert_eq!(mesh_size(&tools, root).await, "2.0", "Godot skipped it");

    let mut forced_args = paths;
    forced_args["force"] = json!(true);
    let forced = report(&call(&tools, "godot_resave_resources", forced_args, root).await);
    assert_eq!(forced["forced"], json!(["res://m.glb"]), "{forced}");
    assert_eq!(forced["imported"][0]["path"], "res://m.glb", "{forced}");
    assert_eq!(forced["still_stale"], json!([]), "{forced}");
    assert_eq!(mesh_size(&tools, root).await, "3.0");
}
