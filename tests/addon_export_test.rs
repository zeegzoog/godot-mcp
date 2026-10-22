//! The addon must stay out of exported games. Runs a real headless Godot, so the tests are
//! ignored by default, like the editor end-to-end tests:
//!
//! ```bash
//! GODOT_PATH=/path/to/godot cargo test --test addon_export_test -- --ignored --test-threads=1
//! ```
//!
//! A pack export needs no export templates, so no extra setup is required.

mod common;

use godot_mcp::bridge::token_path;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A script error in a `--script` run leaves Godot running instead of exiting.
const GODOT_TIMEOUT: Duration = Duration::from_secs(180);

/// Checks `should_skip` directly and writes "ok" or the first mismatch to the file named by
/// the first user argument. The function is static: an EditorExportPlugin can only be
/// instantiated by a running editor.
const SHOULD_SKIP_TEST: &str = r#"extends SceneTree

func _init() -> void:
    var out_path: String = OS.get_cmdline_user_args()[0]
    var plugin: Script = load("res://addons/godot_mcp/export_plugin.gd")
    var addon_dir: String = plugin.resource_path.get_base_dir()
    var cases: Dictionary = {
        "res://addons/godot_mcp/plugin.gd": true,
        "res://addons/godot_mcp/bridge_server.gd": true,
        "res://addons/godot_mcp/export_plugin.gd": true,
        "res://addons/godot_mcp/sub/data.tres": true,
        "res://addons/godot_mcp_other/plugin.gd": false,
        "res://addons/other/addons/godot_mcp/x.gd": false,
        "res://main.gd": false,
        "res://addons/godot_mcp": false,
    }
    var result: String = "ok"
    for path: String in cases:
        if plugin.call("should_skip", path, addon_dir) != cases[path]:
            result = "should_skip(%s, %s) != %s" % [path, addon_dir, cases[path]]
            break
    if plugin.call("should_skip", "res://main.gd", ""):
        result = "an unknown addon folder must not skip anything"
    var file: FileAccess = FileAccess.open(out_path, FileAccess.WRITE)
    file.store_string(result)
    file.close()
    quit()
"#;

/// Writes the names of the files in the zip named by the first user argument, one per line,
/// to the file named by the second, and the exported global class list to the third.
const LIST_ZIP: &str = r#"extends SceneTree

func _init() -> void:
    var args: PackedStringArray = OS.get_cmdline_user_args()
    var reader: ZIPReader = ZIPReader.new()
    var lines: String = ""
    var classes: String = ""
    if reader.open(args[0]) == OK:
        lines = "\n".join(reader.get_files())
        var class_list: String = ".godot/global_script_class_cache.cfg"
        if reader.file_exists(class_list):
            classes = reader.read_file(class_list).get_string_from_utf8()
        reader.close()
    var file: FileAccess = FileAccess.open(args[1], FileAccess.WRITE)
    file.store_string(lines)
    file.close()
    file = FileAccess.open(args[2], FileAccess.WRITE)
    file.store_string(classes)
    file.close()
    quit()
"#;

const MAIN_SCRIPT: &str = "extends Node\n";

const MAIN_SCENE: &str = r#"[gd_scene format=3]

[ext_resource type="Script" path="res://main.gd" id="1"]

[node name="Main" type="Node"]
script = ExtResource("1")
"#;

const EXPORT_PRESETS: &str = r#"[preset.0]

name="Pack"
platform="Linux"
runnable=true
export_filter="all_resources"
include_filter=""
exclude_filter=""
export_path=""

[preset.0.options]
"#;

fn godot_binary() -> PathBuf {
    let _ = dotenvy::dotenv();
    std::env::var("GODOT_PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .expect("set GODOT_PATH to run the addon export tests")
}

/// A project with a main scene and a copy of the addon, enabled.
struct Project {
    dir: tempfile::TempDir,
    port: u16,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("project dir");
        let root = dir.path();
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("addons/godot_mcp"),
            &root.join("addons/godot_mcp"),
        );
        std::fs::write(
            root.join("project.godot"),
            "config_version=5\n\n[application]\n\nconfig/name=\"Export check\"\n\
             run/main_scene=\"res://main.tscn\"\n\n[editor_plugins]\n\n\
             enabled=PackedStringArray(\"res://addons/godot_mcp/plugin.cfg\")\n",
        )
        .expect("project.godot");
        std::fs::write(root.join("main.gd"), MAIN_SCRIPT).expect("main.gd");
        std::fs::write(root.join("main.tscn"), MAIN_SCENE).expect("main.tscn");
        std::fs::write(root.join("export_presets.cfg"), EXPORT_PRESETS).expect("presets");
        std::fs::write(root.join("should_skip_test.gd"), SHOULD_SKIP_TEST).expect("test script");
        std::fs::write(root.join("list_zip.gd"), LIST_ZIP).expect("list script");
        Self {
            dir,
            // The import and the export start the editor, and with it the bridge.
            port: common::free_port(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn godot(&self, args: &[&str]) {
        let mut child = Command::new(godot_binary())
            .arg("--headless")
            .arg("--path")
            .arg(self.path())
            .args(args)
            .env("GODOT_DEBUG_PORT", self.port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("run godot");
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("wait for godot") {
                break status;
            }
            if started.elapsed() > GODOT_TIMEOUT {
                let _ = child.kill();
                panic!("godot {args:?} did not exit within {GODOT_TIMEOUT:?}");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(status.success(), "godot {args:?} failed: {status}");
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if let Some(path) = token_path(self.port) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

#[test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
fn should_skip_matches_the_addon_folder_and_nothing_else() {
    let project = Project::new();
    let out = project.path().join("should_skip.txt");

    project.godot(&[
        "--script",
        "res://should_skip_test.gd",
        "--",
        &out.to_string_lossy(),
    ]);

    let result = std::fs::read_to_string(&out).expect("the test script wrote its result");
    assert_eq!(result, "ok");
}

#[test]
#[ignore = "needs GODOT_PATH pointing at a Godot 4.5+ executable"]
fn exported_pack_contains_the_game_but_not_the_addon() {
    let project = Project::new();
    let pack = project.path().join("out.zip");
    let listing = project.path().join("listing.txt");
    let class_list = project.path().join("classes.txt");

    project.godot(&["--import"]);
    project.godot(&["--export-pack", "Pack", &pack.to_string_lossy()]);
    project.godot(&[
        "--script",
        "res://list_zip.gd",
        "--",
        &pack.to_string_lossy(),
        &listing.to_string_lossy(),
        &class_list.to_string_lossy(),
    ]);

    let files = std::fs::read_to_string(&listing).expect("listing written");
    assert!(
        files.lines().any(|file| file.starts_with("main.gd")),
        "the game itself must be exported:\n{files}"
    );
    let leaked: Vec<&str> = files
        .lines()
        .filter(|file| file.starts_with("addons/godot_mcp/"))
        .collect();
    assert!(leaked.is_empty(), "addon files were exported: {leaked:?}");

    // The class list is exported with every game; a class whose script was left out would
    // dangle in it.
    let classes = std::fs::read_to_string(&class_list).expect("class list written");
    assert!(
        !classes.contains("addons/godot_mcp"),
        "the exported class list still names the addon:\n{classes}"
    );
}
