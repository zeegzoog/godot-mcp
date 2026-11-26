//! Drives the real `godot-mcp` binary over stdio against a fake editor bridge.

mod common;

use common::{MockBridge, Reply, Session, fake_godot, free_port, is_tool_error, project_dir};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Generous upper bound for waiting on a response; speed assertions use their own thresholds.
const FAST: Duration = Duration::from_secs(10);

fn no_godot() -> PathBuf {
    PathBuf::from("godot-binary-that-does-not-exist")
}

#[tokio::test]
async fn initialize_echoes_a_supported_protocol_version() {
    let mut session = Session::start(free_port(), &no_godot()).await;

    let reply = session
        .request("initialize", json!({"protocolVersion": "2025-06-18"}), FAST)
        .await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        reply["result"]["serverInfo"]["version"],
        godot_mcp::version::build_version(),
        "the server names the build, not only the manifest version"
    );

    let reply = session
        .request("initialize", json!({"protocolVersion": "1999-01-01"}), FAST)
        .await;
    assert_eq!(
        reply["result"]["protocolVersion"], "2025-06-18",
        "unsupported versions get the newest one the server speaks"
    );
}

#[tokio::test]
async fn malformed_json_is_a_parse_error() {
    let mut session = Session::start(free_port(), &no_godot()).await;
    session.send_raw("{this is not json").await;
    let reply = session.recv(FAST).await.expect("parse error response");
    assert_eq!(reply["error"]["code"], -32700);
}

#[tokio::test]
async fn request_without_jsonrpc_field_is_an_invalid_request_keeping_its_id() {
    let mut session = Session::start(free_port(), &no_godot()).await;
    session.send_raw(r#"{"id":77,"method":"ping"}"#).await;
    let reply = session.recv(FAST).await.expect("invalid request response");
    assert_eq!(reply["error"]["code"], -32600);
    assert_eq!(reply["id"], 77);
}

#[tokio::test]
async fn batch_requests_are_rejected_as_invalid_requests() {
    let mut session = Session::start(free_port(), &no_godot()).await;
    session
        .send_raw(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#)
        .await;
    let reply = session.recv(FAST).await.expect("batch rejection");
    assert_eq!(reply["error"]["code"], -32600);
}

#[tokio::test]
async fn unknown_tool_is_an_invalid_params_error() {
    let mut session = Session::start(free_port(), &no_godot()).await;
    let reply = session
        .call_tool("godot_does_not_exist", json!({}), FAST)
        .await;
    assert_eq!(reply["error"]["code"], -32602);
}

#[tokio::test]
async fn tool_call_without_a_name_is_an_invalid_params_error() {
    let mut session = Session::start(free_port(), &no_godot()).await;
    let reply = session.request("tools/call", json!({}), FAST).await;
    assert_eq!(reply["error"]["code"], -32602);
}

#[tokio::test]
async fn wrong_argument_type_fails_fast_without_reaching_the_editor() {
    let bridge = MockBridge::start(|_| Reply::Never).await;
    let mut session = Session::start(bridge.port, &no_godot()).await;

    let reply = session
        .call_tool("godot_get_node_properties", json!({"node_path": 5}), FAST)
        .await;

    assert!(is_tool_error(&reply));
    let text = common::tool_text(&reply);
    assert!(
        text.contains("node_path") && text.contains("string"),
        "{text}"
    );
    assert!(
        bridge.received().is_empty(),
        "invalid call must not be forwarded"
    );
}

#[tokio::test]
async fn missing_required_argument_comes_with_a_recovery_hint() {
    let mut session = Session::start(free_port(), &no_godot()).await;
    let reply = session
        .call_tool("godot_create_script", json!({}), FAST)
        .await;

    assert!(is_tool_error(&reply));
    let text = common::tool_text(&reply);
    assert!(text.contains("file_path"), "{text}");
    assert!(text.contains("Actionable recovery suggestions"), "{text}");
}

#[tokio::test]
async fn bridge_reply_is_passed_through_to_the_client() {
    let bridge = MockBridge::start(|_| {
        Reply::Ok(json!({"open_scenes": ["res://a.tscn"], "active_scene": "res://a.tscn"}))
    })
    .await;
    let mut session = Session::start(bridge.port, &no_godot()).await;

    let reply = session
        .call_tool("godot_get_open_scenes", json!({}), FAST)
        .await;

    assert!(!is_tool_error(&reply));
    assert!(common::tool_text(&reply).contains("res://a.tscn"));
}

#[tokio::test]
async fn ping_is_answered_while_a_tool_call_is_pending() {
    let bridge = MockBridge::start(|_| Reply::Never).await;
    let mut session = Session::start(bridge.port, &no_godot()).await;

    session
        .send(&json!({"jsonrpc":"2.0","id":100,"method":"tools/call",
            "params":{"name":"godot_get_open_scenes","arguments":{}}}))
        .await;
    let started = Instant::now();
    let reply = session.request("ping", json!({}), FAST).await;

    assert!(reply["result"].is_object());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "ping must not queue behind the hanging call"
    );
}

#[tokio::test]
async fn cancelled_request_never_gets_a_response() {
    let bridge = MockBridge::start(|_| Reply::Never).await;
    let mut session = Session::start(bridge.port, &no_godot()).await;

    session
        .send(&json!({"jsonrpc":"2.0","id":100,"method":"tools/call",
            "params":{"name":"godot_get_open_scenes","arguments":{}}}))
        .await;
    session
        .send(&json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":100,"reason":"user abort"}}))
        .await;
    session
        .send(&json!({"jsonrpc":"2.0","id":101,"method":"ping"}))
        .await;

    let mut ids = Vec::new();
    while let Some(frame) = session.recv(Duration::from_millis(1500)).await {
        ids.push(frame["id"].clone());
    }
    assert_eq!(ids, vec![json!(101)], "only the ping may be answered");
}

#[tokio::test]
async fn offline_bridge_fails_fast_with_an_actionable_error() {
    let mut session = Session::start(free_port(), &no_godot()).await;

    let started = Instant::now();
    let reply = session
        .call_tool("godot_get_scene_tree", json!({}), Duration::from_secs(10))
        .await;

    assert!(is_tool_error(&reply));
    assert!(common::tool_text(&reply).contains("Launch Godot Editor"));
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn editor_errors_are_reported_as_is_without_cli_fallback() {
    let bridge =
        MockBridge::start(|_| Reply::Err("Scene file does not exist: res://nope.tscn".into()))
            .await;
    let project = project_dir();
    let fake = fake_godot(project.path());
    let mut session = Session::start(bridge.port, &fake).await;

    let reply = session
        .call_tool(
            "godot_run_project",
            json!({"scene": "res://nope.tscn", "project_path": project.path()}),
            Duration::from_secs(10),
        )
        .await;

    assert!(is_tool_error(&reply));
    let text = common::tool_text(&reply);
    assert!(text.contains("Scene file does not exist"), "{text}");
    assert!(
        !text.contains("Spawned"),
        "must not fall back to the CLI: {text}"
    );
    assert!(
        !text.contains("Launch Godot Editor"),
        "an editor-reported error is not a connectivity problem: {text}"
    );
}

/// Runs a slowly printing script with the given call params, and returns every frame up to and
/// including the answer.
async fn frames_of_a_slow_script(meta: Option<serde_json::Value>) -> Vec<serde_json::Value> {
    let project = project_dir();
    std::fs::write(project.path().join("slow.gd"), "extends SceneTree\n").expect("script");
    let godot = common::slowly_printing_godot(project.path());
    let mut session = Session::start(free_port(), &godot).await;
    let mut params = json!({
        "name": "godot_run_script",
        "arguments": {"script": "slow.gd", "project_path": project.path()},
    });
    if let Some(meta) = meta {
        params["_meta"] = meta;
    }
    session
        .send(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":params}))
        .await;
    let mut frames = Vec::new();
    while let Some(frame) = session.recv(Duration::from_secs(30)).await {
        let answered = frame["id"] == json!(7);
        frames.push(frame);
        if answered {
            break;
        }
    }
    frames
}

#[tokio::test]
async fn run_script_streams_its_output_to_a_client_that_asks_for_progress() {
    let frames = frames_of_a_slow_script(Some(json!({"progressToken": "run-1"}))).await;
    let answer = frames.last().expect("frames");
    assert_eq!(answer["id"], json!(7), "{frames:?}");
    let notes: Vec<&serde_json::Value> = frames[..frames.len() - 1].iter().collect();
    assert!(notes.len() >= 2, "{frames:?}");
    let mut last_progress = 0.0;
    let mut messages = Vec::new();
    for note in &notes {
        assert_eq!(note["method"], "notifications/progress", "{note}");
        assert!(note.get("id").is_none(), "{note}");
        assert_eq!(note["params"]["progressToken"], "run-1", "{note}");
        let progress = note["params"]["progress"].as_f64().expect("progress");
        assert!(progress > last_progress, "progress must grow: {frames:?}");
        last_progress = progress;
        messages.push(note["params"]["message"].as_str().expect("message"));
    }
    assert_eq!(&messages[..2], ["first line", "second line"], "{frames:?}");
}

#[tokio::test]
async fn run_script_sends_no_progress_to_a_client_that_did_not_ask() {
    let frames = frames_of_a_slow_script(None).await;
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(frames[0]["id"], json!(7), "{frames:?}");
}

#[tokio::test]
async fn cli_fallback_keeps_stdout_clean() {
    let project = project_dir();
    let fake = fake_godot(project.path());
    let mut session = Session::start(free_port(), &fake).await;

    let reply = session
        .call_tool(
            "godot_run_project",
            json!({"project_path": project.path()}),
            Duration::from_secs(10),
        )
        .await;
    assert!(!is_tool_error(&reply), "{reply}");

    // `Session::recv` panics on any line that is not JSON, so draining proves stdout stayed clean.
    session
        .send(&json!({"jsonrpc":"2.0","id":900,"method":"ping"}))
        .await;
    let mut frames = 0;
    while session.recv(Duration::from_millis(1200)).await.is_some() {
        frames += 1;
    }
    assert_eq!(frames, 1, "only the ping response may appear on stdout");
}

#[tokio::test]
async fn bridge_commands_carry_the_shared_token() {
    let bridge = MockBridge::start(|_| Reply::Ok(json!({}))).await;
    let mut session =
        Session::start_with_env(bridge.port, &no_godot(), &[("GODOT_MCP_TOKEN", "s3cret")]).await;

    let reply = session
        .call_tool("godot_get_open_scenes", json!({}), FAST)
        .await;
    assert!(!is_tool_error(&reply));

    // The first call of a connection reads the addon's commands (get_version) first.
    let received = bridge.received();
    let types: Vec<&Value> = received.iter().map(|command| &command["type"]).collect();
    assert_eq!(types, [&json!("get_version"), &json!("get_open_scenes")]);
    for command in &received {
        assert_eq!(command["token"], "s3cret");
    }
}

#[tokio::test]
async fn empty_settings_are_treated_as_unset() {
    // MCP client configs and copied templates often contain empty placeholders.
    let mut session = Session::start_with_env(
        free_port(),
        std::path::Path::new(""),
        &[("GODOT_CONSOLE_PATH", ""), ("GODOT_DEBUG_PORT", "")],
    )
    .await;

    let reply = session.request("tools/list", json!({}), FAST).await;
    assert!(reply["result"]["tools"].is_array());
}

#[tokio::test]
async fn a_removed_setting_left_in_the_environment_is_ignored() {
    // GODOT_MCP_URL was a reserved placeholder until 0.3.0; old .env files may still set it.
    let mut session = Session::start_with_env(
        free_port(),
        std::path::Path::new(""),
        &[("GODOT_MCP_URL", "http://localhost:1234")],
    )
    .await;

    let reply = session.request("tools/list", json!({}), FAST).await;
    assert!(reply["result"]["tools"].is_array());
}

#[tokio::test]
async fn env_example_can_be_copied_as_is() {
    let mut session = Session::start_with_dotenv(include_str!("../.env.example")).await;

    let reply = session.request("tools/list", json!({}), FAST).await;
    assert!(reply["result"]["tools"].is_array());
}

#[tokio::test]
async fn unparsable_dotenv_is_ignored_instead_of_stopping_the_server() {
    // Double-quoted Windows paths are not valid in a .env file.
    let mut session = Session::start_with_dotenv(
        r#"GODOT_PATH="C:\Program Files\Godot\Godot.exe"
"#,
    )
    .await;

    let reply = session.request("ping", json!({}), FAST).await;
    assert!(reply["result"].is_object());
}

#[test]
fn an_unknown_feedback_mode_falls_back_with_a_warning_on_stderr_only() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let cwd = tempfile::tempdir().expect("working directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_godot-mcp"))
        .current_dir(cwd.path())
        .env_remove("GODOT_PATH")
        .env_remove("GODOT_DEBUG_PORT")
        .env("GODOT_MCP_FEEDBACK", "bogus")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the server");
    let mut stdin = child.stdin.take().expect("server stdin");
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).expect("send ping");
    drop(stdin);
    let output = child.wait_with_output().expect("server output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|_| panic!("non-JSON line on stdout: {line:?}"));
    }
    assert!(stdout.contains(r#""id":1"#), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("GODOT_MCP_FEEDBACK") && stderr.contains("bogus"),
        "{stderr}"
    );
    // Standard error is a pipe here, not a terminal: no colour codes in the text (ESC is byte 27).
    assert!(
        output.stderr.iter().all(|byte| *byte != 27),
        "colour codes in a piped log: {stderr}"
    );
}

#[tokio::test]
async fn an_unknown_feedback_mode_offers_drafting_but_never_submission() {
    assert_eq!(
        listed_feedback_tools(Some("drafts")).await,
        &FEEDBACK_TOOLS[..2]
    );
}

const FEEDBACK_TOOLS: [&str; 3] = [
    "godot_note_feedback",
    "godot_draft_feedback",
    "godot_submit_feedback",
];

async fn listed_feedback_tools(mode: Option<&str>) -> Vec<String> {
    let env: Vec<(&str, &str)> = mode
        .map(|mode| ("GODOT_MCP_FEEDBACK", mode))
        .into_iter()
        .collect();
    let mut session = Session::start_with_env(free_port(), &no_godot(), &env).await;
    let reply = session.request("tools/list", json!({}), FAST).await;
    reply["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .filter(|name| FEEDBACK_TOOLS.contains(name))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn the_feedback_tools_listed_follow_the_mode() {
    assert_eq!(listed_feedback_tools(None).await, FEEDBACK_TOOLS);
    assert_eq!(listed_feedback_tools(Some("full")).await, FEEDBACK_TOOLS);
    assert_eq!(
        listed_feedback_tools(Some("draft")).await,
        &FEEDBACK_TOOLS[..2]
    );
    assert!(listed_feedback_tools(Some("off")).await.is_empty());
}

#[tokio::test]
async fn a_tool_that_the_mode_removed_is_unknown() {
    let mut session =
        Session::start_with_env(free_port(), &no_godot(), &[("GODOT_MCP_FEEDBACK", "draft")]).await;
    let reply = session
        .call_tool(
            "godot_submit_feedback",
            json!({"draft_id": "d1", "confirmed": true}),
            FAST,
        )
        .await;
    assert_eq!(reply["error"]["code"], -32602);
}

async fn guidance(mode: Option<&str>) -> Option<String> {
    let env: Vec<(&str, &str)> = mode
        .map(|mode| ("GODOT_MCP_FEEDBACK", mode))
        .into_iter()
        .collect();
    let mut session = Session::start_with_env(free_port(), &no_godot(), &env).await;
    let reply = session
        .request("initialize", json!({"protocolVersion": "2025-06-18"}), FAST)
        .await;
    reply["result"]["instructions"].as_str().map(str::to_string)
}

#[tokio::test]
async fn initialize_carries_short_guidance_that_names_the_tools_of_the_mode() {
    for mode in [None, Some("full")] {
        let text = guidance(mode).await.expect("guidance in full mode");
        assert!(text.split_whitespace().count() <= 150, "{text}");
        for tool in FEEDBACK_TOOLS {
            assert!(text.contains(tool), "{tool} in {text}");
        }
        assert!(text.contains("explicit yes"), "{text}");
    }

    let text = guidance(Some("draft"))
        .await
        .expect("guidance in draft mode");
    assert!(text.split_whitespace().count() <= 150, "{text}");
    assert!(text.contains("godot_note_feedback") && text.contains("godot_draft_feedback"));
    assert!(!text.contains("godot_submit_feedback"), "{text}");

    assert!(guidance(Some("off")).await.is_none());
}

/// A stand-in for Godot that prints the arguments after `--`, waits two seconds and prints them
/// again, so overlapping runs are seen by their total time.
#[cfg(unix)]
fn echoing_slow_godot(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("echoing_slow_godot.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\nwhile [ \"$1\" != \"--\" ] && [ $# -gt 0 ]; do shift; done\nshift\necho \"start $*\"\nsleep 2\necho \"end $*\"\necho \"err $*\" >&2\n",
    )
    .expect("write echoing godot");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod echoing godot");
    path
}

#[cfg(unix)]
#[tokio::test]
async fn two_script_runs_sent_together_overlap_and_each_reports_its_own_output() {
    let project = project_dir();
    std::fs::write(project.path().join("zones.gd"), "extends SceneTree\n").expect("script");
    let godot = echoing_slow_godot(project.path());
    let mut session = Session::start(free_port(), &godot).await;
    let started = std::time::Instant::now();
    for (id, part) in [(41, "part-a"), (42, "part-b")] {
        session
            .send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                "name": "godot_run_script",
                "arguments": {"script": "zones.gd", "project_path": project.path(), "user_args": [part]},
            }}))
            .await;
    }
    let mut answers = std::collections::HashMap::new();
    while answers.len() < 2 {
        let frame = session
            .recv(Duration::from_secs(30))
            .await
            .expect("both runs answer");
        if let Some(id) = frame["id"].as_i64() {
            answers.insert(id, frame);
        }
    }
    let elapsed = started.elapsed();
    // Each run takes two seconds; one after the other they would take four.
    assert!(
        elapsed < Duration::from_millis(3500),
        "the runs did not overlap: {elapsed:?}"
    );
    for (id, part, other) in [(41, "part-a", "part-b"), (42, "part-b", "part-a")] {
        let answer = &answers[&id];
        assert!(!is_tool_error(answer), "{answer}");
        let text = common::tool_text(answer);
        assert!(text.contains(&format!("start {part}")), "{text}");
        assert!(text.contains(&format!("end {part}")), "{text}");
        assert!(text.contains(&format!("err {part}")), "{text}");
        assert!(!text.contains(other), "run {part} reports {other}: {text}");
    }
}

/// A stand-in for headless Godot that saves `<name>.tscn` in the project without its uid, as a
/// headless save does, prints `lines` lines naming it, waits `seconds` and exits. The arguments
/// after `--` are the name, the seconds and the line count.
#[cfg(unix)]
fn saving_godot(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("saving_godot.sh");
    std::fs::write(
        &path,
        "#!/bin/sh
project=.
while [ \"$1\" != \"--\" ] && [ $# -gt 0 ]; do
  if [ \"$1\" = \"--path\" ]; then project=\"$2\"; fi
  shift
done
shift
printf '[gd_scene format=3]\\n\\n[node name=\"%s\" type=\"Node\"]\\n' \"$1\" > \"$project/$1.tscn\"
i=0
while [ $i -lt \"$3\" ]; do echo \"line $i of $1 ................................................................\"; i=$((i+1)); done
sleep \"$2\"
",
    )
    .expect("write saving godot");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod saving godot");
    path
}

#[cfg(unix)]
fn call_run(id: i64, project: &std::path::Path, user_args: &[&str]) -> serde_json::Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name": "godot_run_script",
        "arguments": {"script": "zones.gd", "project_path": project, "user_args": user_args},
    }})
}

#[cfg(unix)]
async fn answers(
    session: &mut Session,
    count: usize,
) -> std::collections::HashMap<i64, (serde_json::Value, std::time::Instant)> {
    let mut answers = std::collections::HashMap::new();
    while answers.len() < count {
        let frame = session
            .recv(Duration::from_secs(30))
            .await
            .expect("every run answers");
        if let Some(id) = frame["id"].as_i64() {
            answers.insert(id, (frame, std::time::Instant::now()));
        }
    }
    answers
}

#[cfg(unix)]
fn body_of(answer: &serde_json::Value) -> serde_json::Value {
    let text = common::tool_text(answer);
    let start = text.find('{').expect("json body");
    serde_json::from_str(&text[start..]).unwrap_or_else(|_| panic!("{text}"))
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_that_ends_first_does_not_repair_the_files_a_parallel_run_is_writing() {
    let project = project_dir();
    std::fs::write(project.path().join("zones.gd"), "extends SceneTree\n").expect("script");
    for (name, uid) in [
        ("slow", "uid://b1slowzone0aa"),
        ("fast", "uid://b2fastzone0aa"),
    ] {
        std::fs::write(
            project.path().join(format!("{name}.tscn")),
            format!("[gd_scene format=3 uid=\"{uid}\"]\n\n[node name=\"{name}\" type=\"Node\"]\n"),
        )
        .expect("scene");
    }
    let godot = saving_godot(project.path());
    let mut session = Session::start(free_port(), &godot).await;
    let started = std::time::Instant::now();
    session
        .send(&call_run(51, project.path(), &["slow", "3", "1"]))
        .await;
    // Late enough that the slow run has already saved its scene without the uid.
    tokio::time::sleep(Duration::from_millis(700)).await;
    session
        .send(&call_run(52, project.path(), &["fast", "1", "1"]))
        .await;
    let answers = answers(&mut session, 2).await;
    let total = started.elapsed();
    assert!(
        total < Duration::from_millis(5500),
        "the runs did not overlap: {total:?}"
    );
    // The fast run's process ended first, but its repair waited for the slow one to finish.
    let fast_answered = answers[&52].1.duration_since(started);
    assert!(
        fast_answered >= Duration::from_millis(2900),
        "{fast_answered:?}"
    );
    for id in [51, 52] {
        let answer = &answers[&id].0;
        assert!(!is_tool_error(answer), "{answer}");
        let body = body_of(answer);
        assert!(
            body["overlapped"]
                .as_str()
                .is_some_and(|note| note.contains("same time")),
            "{body}"
        );
    }
    for (name, uid) in [
        ("slow", "uid://b1slowzone0aa"),
        ("fast", "uid://b2fastzone0aa"),
    ] {
        let text =
            std::fs::read_to_string(project.path().join(format!("{name}.tscn"))).expect("scene");
        assert!(text.contains(uid), "{name}.tscn lost its uid: {text}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn parallel_runs_keep_their_output_logs_apart() {
    let project = project_dir();
    std::fs::write(project.path().join("zones.gd"), "extends SceneTree\n").expect("script");
    let godot = saving_godot(project.path());
    let mut session = Session::start(free_port(), &godot).await;
    // About 100 bytes a line: 1500 lines outgrow the 64 KiB the answer holds.
    session
        .send(&call_run(61, project.path(), &["one", "1", "1500"]))
        .await;
    session
        .send(&call_run(62, project.path(), &["two", "1", "1500"]))
        .await;
    let answers = answers(&mut session, 2).await;
    let mut logs = Vec::new();
    for (id, name, other) in [(61, "one", "two"), (62, "two", "one")] {
        let body = body_of(&answers[&id].0);
        let log = body["stdout_log"]
            .as_str()
            .unwrap_or_else(|| panic!("{body}"))
            .to_string();
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(text.contains(&format!("line 0 of {name} ")), "{log}");
        assert!(text.contains(&format!("line 1499 of {name} ")), "{log}");
        assert!(
            !text.contains(&format!(" of {other} ")),
            "{log} holds the other run's output"
        );
        logs.push(log);
    }
    assert_ne!(logs[0], logs[1]);
    assert!(
        logs.iter()
            .any(|log| log.ends_with("script-run.stdout.log")),
        "{logs:?}"
    );
    assert!(
        logs.iter()
            .any(|log| log.ends_with("script-run-2.stdout.log")),
        "{logs:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_headless_import_waits_for_the_script_runs_in_its_project() {
    use std::os::unix::fs::PermissionsExt;
    let project = project_dir();
    std::fs::write(project.path().join("zones.gd"), "extends SceneTree\n").expect("script");
    let godot = project.path().join("import_or_sleep.sh");
    std::fs::write(
        &godot,
        "#!/bin/sh\nfor a in \"$@\"; do if [ \"$a\" = \"--import\" ]; then exit 0; fi; done\nsleep 2\n",
    )
    .expect("write fake godot");
    std::fs::set_permissions(&godot, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let other = project_dir();
    let mut session = Session::start(free_port(), &godot).await;
    let started = std::time::Instant::now();
    session.send(&call_run(71, project.path(), &[])).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    for (id, dir) in [(72, project.path()), (73, other.path())] {
        session
            .send(
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                    "name": "godot_resave_resources",
                    "arguments": {"project_path": dir},
                }}),
            )
            .await;
    }
    let answers = answers(&mut session, 3).await;
    for id in [71, 72, 73] {
        assert!(!is_tool_error(&answers[&id].0), "{}", answers[&id].0);
    }
    let at = |id: i64| answers[&id].1.duration_since(started);
    // The import in the same project starts once the script's process has ended.
    assert!(at(72) >= Duration::from_millis(1900), "{:?}", at(72));
    // Another project's import does not wait.
    assert!(at(73) < Duration::from_millis(1500), "{:?}", at(73));
}
