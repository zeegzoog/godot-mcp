//! The agent feedback channel, tested against the real server binary over stdio, with a
//! stand-in for the GitHub command-line client.

mod common;

use common::{FakeGh, fake_gh};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs the stand-in directly with the given arguments and standard input.
fn run_fake_gh(gh: &FakeGh, args: &[&str], input: &str) -> std::process::Output {
    let mut child = Command::new(fake_gh())
        .args(args)
        .envs(gh.env_pairs())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the stand-in");
    child
        .stdin
        .take()
        .expect("stand-in stdin")
        .write_all(input.as_bytes())
        .expect("write the body");
    child.wait_with_output().expect("stand-in output")
}

#[test]
fn the_stand_in_records_arguments_and_input_and_answers_as_scripted() {
    let gh = FakeGh::new()
        .script(
            "FAKE_GH_CREATE_STDOUT",
            "Creating issue\nhttps://github.com/o/r/issues/5\n",
        )
        .script("FAKE_GH_CREATE_STDERR", "a warning\n");
    let output = run_fake_gh(
        &gh,
        &["issue", "create", "--title=--odd title", "--body-file", "-"],
        "the body\nsecond line",
    );

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Creating issue\nhttps://github.com/o/r/issues/5\n"
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "a warning\n");
    assert_eq!(
        gh.calls(),
        vec![vec![
            "issue",
            "create",
            "--title=--odd title",
            "--body-file",
            "-"
        ]]
    );
    assert_eq!(gh.body().as_deref(), Some("the body\nsecond line"));
}

#[test]
fn the_stand_in_defaults_are_an_empty_search_and_a_created_issue() {
    let gh = FakeGh::new();
    let list = run_fake_gh(&gh, &["issue", "list"], "");
    assert_eq!(String::from_utf8_lossy(&list.stdout), "[]");
    let create = run_fake_gh(&gh, &["issue", "create", "--body-file", "-"], "x");
    assert!(
        String::from_utf8_lossy(&create.stdout)
            .trim()
            .ends_with("/issues/1")
    );
    assert_eq!(gh.calls().len(), 2);
}

#[test]
fn the_stand_in_can_fail_and_can_be_slow() {
    let gh = FakeGh::new()
        .script("FAKE_GH_CREATE_EXIT", "4")
        .script("FAKE_GH_CREATE_STDERR", "gh auth login\n")
        .script("FAKE_GH_DELAY_MS", "300");
    let started = Instant::now();
    let output = run_fake_gh(&gh, &["issue", "create", "--body-file", "-"], "x");
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "gh auth login\n");
}

#[test]
fn the_stand_in_refuses_commands_the_channel_never_issues() {
    let gh = FakeGh::new();
    let output = run_fake_gh(&gh, &["repo", "delete", "x"], "");
    assert_eq!(output.status.code(), Some(64));
}

use common::{Session, free_port, is_tool_error, tool_text};
use serde_json::{Value, json};
use std::path::PathBuf;

const FAST: Duration = Duration::from_secs(10);

fn no_godot() -> PathBuf {
    PathBuf::from("godot-binary-that-does-not-exist")
}

async fn session() -> Session {
    Session::start(free_port(), &no_godot()).await
}

fn note(kind: &str, summary: &str) -> Value {
    json!({"kind": kind, "summary": summary, "details": "Tried it; it did not work."})
}

#[tokio::test]
async fn a_note_is_stored_and_the_answer_lists_what_is_not_in_a_report_yet() {
    let mut session = session().await;
    let first = session
        .call_tool(
            "godot_note_feedback",
            note("bug", "add_node ignores names"),
            FAST,
        )
        .await;
    assert!(!is_tool_error(&first), "{first}");
    let text = tool_text(&first);
    assert!(text.contains("n1"), "{text}");

    let second = session
        .call_tool(
            "godot_note_feedback",
            note("docs", "README tool table is stale"),
            FAST,
        )
        .await;
    let text = tool_text(&second);
    assert!(text.contains("n2"), "{text}");
    assert!(
        text.contains("n1") && text.contains("add_node ignores names"),
        "{text}"
    );
    assert!(text.contains("godot_draft_feedback"), "{text}");
}

#[tokio::test]
async fn an_unknown_kind_is_rejected_by_the_schema_before_anything_is_stored() {
    let mut session = session().await;
    let reply = session
        .call_tool("godot_note_feedback", note("complaint", "x"), FAST)
        .await;
    assert!(is_tool_error(&reply));
    assert!(tool_text(&reply).contains("kind"), "{reply}");
}

#[tokio::test]
async fn over_long_text_is_refused_with_the_limit_and_the_way_to_fix_it() {
    let mut session = session().await;
    let reply = session
        .call_tool("godot_note_feedback", note("bug", &"s".repeat(121)), FAST)
        .await;
    assert!(is_tool_error(&reply));
    let text = tool_text(&reply);
    assert!(text.contains("summary") && text.contains("120"), "{text}");
    assert!(text.contains("Actionable recovery"), "{text}");
}

#[tokio::test]
async fn the_twenty_first_note_is_refused() {
    let mut session = session().await;
    for index in 0..20 {
        let reply = session
            .call_tool(
                "godot_note_feedback",
                note("bug", &format!("note {index}")),
                FAST,
            )
            .await;
        assert!(!is_tool_error(&reply), "{reply}");
    }
    let reply = session
        .call_tool("godot_note_feedback", note("bug", "one too many"), FAST)
        .await;
    assert!(is_tool_error(&reply));
    assert!(tool_text(&reply).contains("20"), "{reply}");
}

#[tokio::test]
async fn a_failing_tool_answers_identically_with_the_channel_on_and_off() {
    let failing = json!({"file_path": "not-a-script.txt"});
    let mut full =
        Session::start_with_env(free_port(), &no_godot(), &[("GODOT_MCP_FEEDBACK", "full")]).await;
    let mut off =
        Session::start_with_env(free_port(), &no_godot(), &[("GODOT_MCP_FEEDBACK", "off")]).await;
    let with_channel = full
        .call_tool("godot_create_script", failing.clone(), FAST)
        .await;
    let without = off.call_tool("godot_create_script", failing, FAST).await;
    assert!(is_tool_error(&with_channel));
    assert_eq!(with_channel["result"], without["result"]);
}

// ---- drafts ----

use common::{MockBridge, Reply, project_dir};

async fn note_with(session: &mut Session, args: Value) {
    let reply = session.call_tool("godot_note_feedback", args, FAST).await;
    assert!(!is_tool_error(&reply), "{reply}");
}

async fn draft_text(session: &mut Session, args: Value) -> String {
    let reply = session.call_tool("godot_draft_feedback", args, FAST).await;
    assert!(!is_tool_error(&reply), "{reply}");
    tool_text(&reply)
}

fn row(text: &str, label: &str) -> String {
    text.lines()
        .find(|line| line.starts_with(&format!("| {label} |")))
        .unwrap_or_else(|| panic!("no {label} row in\n{text}"))
        .to_string()
}

#[tokio::test]
async fn a_draft_without_notes_says_to_record_one_first() {
    let mut session = session().await;
    let reply = session
        .call_tool("godot_draft_feedback", json!({}), FAST)
        .await;
    assert!(is_tool_error(&reply));
    assert!(tool_text(&reply).contains("godot_note_feedback"), "{reply}");
}

#[tokio::test]
async fn a_draft_shows_the_whole_report_and_every_way_to_deliver_it() {
    let mut session = Session::start_with_env(
        free_port(),
        &no_godot(),
        &[("GODOT_MCP_GH_PATH", "no-such-gh-executable")],
    )
    .await;
    note_with(&mut session, note("bug", "add_node ignores names")).await;

    let text = draft_text(&mut session, json!({})).await;

    assert!(text.contains("Draft d1"), "{text}");
    assert!(text.contains("Nothing has been sent"), "{text}");
    assert!(
        text.contains("[agent-feedback] bug: add_node ignores names"),
        "{text}"
    );
    assert!(
        text.contains("## Summary *(written by the agent)*"),
        "{text}"
    );
    assert!(text.contains("## Report details"), "{text}");
    assert!(text.contains("gmfb"), "{text}");
    assert!(
        text.contains("Link: https://github.com/zeegzoog/godot-mcp/issues/new?title="),
        "{text}"
    );
    assert!(text.contains("Copy: always available"), "{text}");
    assert!(text.contains("Submit: not available"), "{text}");
    assert!(text.contains("COMPLETE report"), "{text}");
    assert!(
        !text.contains("explicitly said yes"),
        "submission is not offered when it is not available: {text}"
    );
}

#[tokio::test]
async fn submission_is_listed_as_available_when_the_client_is_found() {
    let gh = FakeGh::new();
    let env = gh.server_env();
    let mut session =
        Session::start_with_env(free_port(), &no_godot(), &common::as_pairs(&env)).await;
    note_with(&mut session, note("bug", "x")).await;
    let text = draft_text(&mut session, json!({})).await;
    assert!(
        text.contains("Submit: available through godot_submit_feedback"),
        "{text}"
    );
    assert!(text.contains(r#"draft_id "d1""#), "{text}");
    assert!(text.contains("explicitly said yes"), "{text}");
}

#[tokio::test]
async fn draft_mode_never_mentions_submission() {
    let mut session =
        Session::start_with_env(free_port(), &no_godot(), &[("GODOT_MCP_FEEDBACK", "draft")]).await;
    note_with(&mut session, note("bug", "x")).await;
    let text = draft_text(&mut session, json!({})).await;
    assert!(!text.to_lowercase().contains("submit"), "{text}");
    assert!(text.contains("Copy: always available"), "{text}");
}

#[tokio::test]
async fn the_environment_block_comes_from_the_editor_and_the_project() {
    let project = project_dir();
    let addon = project.path().join("addons").join("godot_mcp");
    std::fs::create_dir_all(&addon).expect("addon directory");
    std::fs::write(
        addon.join("plugin.cfg"),
        "[plugin]\n\nname=\"Godot MCP Bridge\"\nversion=\"0.3.0\"\nscript=\"plugin.gd\"\n",
    )
    .expect("plugin.cfg");
    let directory = project.path().to_string_lossy().into_owned();
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_version") => Reply::Ok(json!({"version_string": "4.5.stable.official"})),
        Some("get_project_info") => Reply::Ok(json!({"name": "Game", "project_dir": directory})),
        other => Reply::Err(format!("unexpected command {other:?}")),
    })
    .await;
    let mut session = Session::start(bridge.port, &no_godot()).await;
    // A failing call that resolves the project makes the server know its directory.
    let failed = session
        .call_tool(
            "godot_validate_script",
            json!({"script_path": "res://missing.gd"}),
            FAST,
        )
        .await;
    assert!(is_tool_error(&failed));
    let mut with_path = note("bug", "x");
    with_path["tool"] = json!("godot_validate_script");
    with_path["details"] = json!(format!("it failed in {}", project.path().display()));
    note_with(&mut session, with_path).await;

    let text = draft_text(&mut session, json!({})).await;

    assert_eq!(row(&text, "Godot"), "| Godot | 4.5.stable.official |");
    assert_eq!(row(&text, "Addon"), "| Addon | 0.3.0 |");
    assert_eq!(row(&text, "Editor bridge"), "| Editor bridge | connected |");
    assert_eq!(
        row(&text, "godot-mcp"),
        format!("| godot-mcp | {} |", env!("CARGO_PKG_VERSION"))
    );
    assert!(row(&text, "Platform").contains(std::env::consts::OS));
    assert!(text.contains("it failed in {project}"), "{text}");
    assert!(text.contains("Script file not found: {project}"), "{text}");
    assert!(
        !text.contains(&project.path().to_string_lossy().into_owned()),
        "{text}"
    );
}

#[tokio::test]
async fn without_an_editor_or_a_project_the_unknowns_are_said_so() {
    let mut session = session().await;
    note_with(&mut session, note("bug", "x")).await;
    let text = draft_text(&mut session, json!({})).await;
    assert_eq!(row(&text, "Godot"), "| Godot | unknown |");
    assert_eq!(row(&text, "Addon"), "| Addon | unknown |");
    assert!(
        row(&text, "Editor bridge").contains("not reachable"),
        "{text}"
    );
}

#[tokio::test]
async fn a_project_without_the_addon_is_reported_as_such() {
    let project = project_dir();
    let directory = project.path().to_string_lossy().into_owned();
    let bridge = MockBridge::start(move |command| match command["type"].as_str() {
        Some("get_version") => Reply::Ok(json!({"version_string": "4.5.stable"})),
        Some("get_project_info") => Reply::Ok(json!({"project_dir": directory})),
        _ => Reply::Err("unexpected".into()),
    })
    .await;
    let mut session = Session::start(bridge.port, &no_godot()).await;
    note_with(&mut session, note("bug", "x")).await;
    let text = draft_text(&mut session, json!({})).await;
    assert_eq!(
        row(&text, "Addon"),
        "| Addon | not installed in the project |"
    );
}

#[tokio::test]
async fn a_stalled_editor_costs_the_draft_no_more_than_its_budget() {
    let bridge = MockBridge::start(|_| Reply::Never).await;
    let mut session = Session::start(bridge.port, &no_godot()).await;
    note_with(&mut session, note("bug", "x")).await;

    let started = Instant::now();
    let text = draft_text(&mut session, json!({})).await;

    assert!(
        started.elapsed() < Duration::from_secs(9),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(row(&text, "Godot"), "| Godot | unknown |");
}

#[tokio::test]
async fn secrets_paths_and_mentions_never_reach_the_answer() {
    let mut session = Session::start_with_env(
        free_port(),
        &no_godot(),
        &[
            ("GODOT_MCP_TOKEN", "supersecrettoken1234567890abcdef"),
            ("HOME", "/home/tester"),
            ("USERPROFILE", r"C:\Users\tester"),
        ],
    )
    .await;
    let mut sensitive = note("bug", "leaky");
    sensitive["details"] = json!(
        "token supersecrettoken1234567890abcdef, files /home/tester/game/a.gd and \
         C:\\Users\\tester\\game\\a.gd, ping @maintainer, see #12"
    );
    note_with(&mut session, sensitive).await;

    let text = draft_text(&mut session, json!({})).await;

    assert!(!text.contains("supersecrettoken"), "{text}");
    assert!(!text.contains("tester"), "{text}");
    assert!(text.contains("@\u{200B}maintainer"), "{text}");
    assert!(text.contains("#\u{200B}12"), "{text}");
    assert!(text.contains("{redacted}"), "{text}");
}

#[tokio::test]
async fn failures_of_the_named_tool_are_quoted_and_other_failures_are_not() {
    let mut session = session().await;
    let failed = session
        .call_tool(
            "godot_create_script",
            json!({"file_path": "not-a-script.txt"}),
            FAST,
        )
        .await;
    assert!(is_tool_error(&failed));
    let other = session
        .call_tool(
            "godot_validate_script",
            json!({"script_path": "elsewhere.txt"}),
            FAST,
        )
        .await;
    assert!(is_tool_error(&other));
    let mut named = note("bug", "create_script message");
    named["tool"] = json!("godot_create_script");
    note_with(&mut session, named).await;

    let text = draft_text(&mut session, json!({})).await;

    assert!(
        text.contains("Script files must have the .gd extension"),
        "{text}"
    );
    assert!(
        text.contains("godot_create_script: 1 call, 1 failure"),
        "{text}"
    );
    assert!(
        !text.contains("Only .gd scripts can be validated"),
        "{text}"
    );
}

#[tokio::test]
async fn requests_for_missing_tools_become_evidence_for_a_missing_feature() {
    let mut session = session().await;
    for _ in 0..2 {
        let reply = session
            .call_tool("godot_export_project", json!({}), FAST)
            .await;
        assert_eq!(reply["error"]["code"], -32602);
    }
    note_with(
        &mut session,
        note("missing_feature", "no way to export the project"),
    )
    .await;

    let text = draft_text(&mut session, json!({})).await;

    assert!(text.contains("godot_export_project"), "{text}");
    assert!(text.contains("requested 2 times"), "{text}");
}

#[tokio::test]
async fn a_draft_marks_its_notes_and_reports_the_ones_left_out() {
    let mut session = session().await;
    note_with(&mut session, note("docs", "readme is stale")).await;
    note_with(&mut session, note("bug", "crash on open")).await;

    let first = draft_text(&mut session, json!({"note_ids": ["n2"]})).await;
    assert!(first.contains("Draft d1"), "{first}");
    assert!(first.contains("Notes not in any report yet"), "{first}");
    assert!(first.contains("n1 (docs): readme is stale"), "{first}");

    let second = draft_text(&mut session, json!({})).await;
    assert!(second.contains("Draft d2"), "{second}");
    assert!(second.contains("readme is stale"), "{second}");
    assert!(!second.contains("crash on open"), "{second}");
}

#[tokio::test]
async fn a_chosen_title_is_prefixed_and_a_bad_selection_names_the_known_ids() {
    let mut session = session().await;
    note_with(&mut session, note("bug", "x")).await;
    let text = draft_text(&mut session, json!({"title": "Custom title"})).await;
    assert!(text.contains("[agent-feedback] Custom title"), "{text}");

    let reply = session
        .call_tool("godot_draft_feedback", json!({"note_ids": ["n9"]}), FAST)
        .await;
    assert!(is_tool_error(&reply));
    let refusal = tool_text(&reply);
    assert!(
        refusal.contains("n9") && refusal.contains("n1"),
        "{refusal}"
    );
}

// ---- submission ----

use godot_mcp::issues::{IssueClient, RepoAddress};
use godot_mcp::journal::Outcome;

const ISSUE_PREFIX: &str = "https://github.com/zeegzoog/godot-mcp/issues/";

async fn started(gh: &FakeGh) -> Session {
    let env = gh.server_env();
    Session::start_with_env(free_port(), &no_godot(), &common::as_pairs(&env)).await
}

/// Records one note and drafts it; returns the draft answer.
async fn drafted(session: &mut Session) -> String {
    note_with(session, note("bug", "add_node ignores names")).await;
    draft_text(session, json!({})).await
}

fn line_after(text: &str, prefix: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} line in\n{text}"))
        .trim()
        .to_string()
}

/// The report between the outer fence of a draft answer.
fn report_in(text: &str) -> String {
    let mut lines = text.lines();
    let fence = lines
        .find(|line| {
            line.ends_with("markdown")
                && line.trim_end_matches("markdown").chars().all(|c| c == '`')
        })
        .expect("the opening fence")
        .trim_end_matches("markdown")
        .to_string();
    let mut body: Vec<&str> = Vec::new();
    for line in lines {
        if line == fence {
            break;
        }
        body.push(line);
    }
    body.join("\n")
}

async fn submit(session: &mut Session, args: Value) -> Value {
    session
        .call_tool("godot_submit_feedback", args, Duration::from_secs(30))
        .await
}

fn confirmed(draft: &str) -> Value {
    json!({"draft_id": draft, "confirmed": true})
}

#[tokio::test]
async fn without_confirmation_nothing_is_sent_and_the_reply_says_what_to_do() {
    let gh = FakeGh::new();
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    let reply = submit(&mut session, json!({"draft_id": "d1", "confirmed": false})).await;

    assert!(is_tool_error(&reply));
    let text = tool_text(&reply);
    assert!(
        text.contains("confirmed") && text.contains("explicit yes"),
        "{text}"
    );
    assert!(text.contains("Actionable recovery"), "{text}");
    assert!(gh.calls().is_empty(), "the client must not be started");

    let missing = submit(&mut session, json!({"draft_id": "d1"})).await;
    assert!(is_tool_error(&missing));
    assert!(tool_text(&missing).contains("confirmed"), "{missing}");
}

#[tokio::test]
async fn an_unknown_draft_lists_the_known_ones() {
    let gh = FakeGh::new();
    let mut session = started(&gh).await;
    drafted(&mut session).await;
    let reply = submit(&mut session, confirmed("d9")).await;
    assert!(is_tool_error(&reply));
    let text = tool_text(&reply);
    assert!(text.contains("d9") && text.contains("d1"), "{text}");
    assert!(gh.calls().is_empty());

    let mut fresh = started(&FakeGh::new()).await;
    let reply = submit(&mut fresh, confirmed("d1")).await;
    assert!(
        tool_text(&reply).contains("godot_draft_feedback"),
        "{reply}"
    );
}

#[tokio::test]
async fn a_confirmed_draft_is_published_exactly_as_shown_and_only_once() {
    let gh = FakeGh::new().script(
        "FAKE_GH_CREATE_STDOUT",
        format!("Creating issue in zeegzoog/godot-mcp\n\n{ISSUE_PREFIX}42\n").as_str(),
    );
    let mut session = started(&gh).await;
    let draft = drafted(&mut session).await;
    let fingerprint = line_after(&draft, "Fingerprint: ");
    let title = line_after(&draft, "Title: ");

    let reply = submit(&mut session, confirmed("d1")).await;

    assert!(!is_tool_error(&reply), "{reply}");
    let text = tool_text(&reply);
    assert!(text.contains(&format!("{ISSUE_PREFIX}42")), "{text}");
    assert!(text.to_lowercase().contains("public"), "{text}");
    assert_eq!(
        gh.calls(),
        vec![
            vec![
                "issue".to_string(),
                "list".into(),
                "--repo".into(),
                "zeegzoog/godot-mcp".into(),
                "--state".into(),
                "all".into(),
                "--search".into(),
                format!("{fingerprint} in:body"),
                "--json".into(),
                "number,title,url,body".into(),
                "--limit".into(),
                "5".into(),
            ],
            vec![
                "issue".to_string(),
                "create".into(),
                "--repo".into(),
                "zeegzoog/godot-mcp".into(),
                format!("--title={title}"),
                "--body-file".into(),
                "-".into(),
            ],
        ]
    );
    assert_eq!(gh.body(), Some(report_in(&draft)));

    let again = submit(&mut session, confirmed("d1")).await;
    assert!(
        tool_text(&again).contains(&format!("{ISSUE_PREFIX}42")),
        "{again}"
    );
    assert_eq!(
        gh.calls().len(),
        2,
        "a repeat must not start the client again"
    );
}

#[tokio::test]
async fn an_existing_issue_with_the_same_fingerprint_is_reported_instead_of_a_new_one() {
    let hit = format!("{ISSUE_PREFIX}7");
    let gh = FakeGh::new().script("FAKE_GH_LIST_HIT_URL", &hit);
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    let reply = submit(&mut session, confirmed("d1")).await;

    assert!(!is_tool_error(&reply), "{reply}");
    let text = tool_text(&reply);
    assert!(
        text.contains(&hit) && text.contains("already exists"),
        "{text}"
    );
    assert!(text.contains("allow_duplicate"), "{text}");
    assert_eq!(gh.calls().len(), 1, "only the look-up ran");
}

#[tokio::test]
async fn allow_duplicate_creates_the_issue_anyway_without_looking() {
    let gh = FakeGh::new().script("FAKE_GH_LIST_HIT_URL", &format!("{ISSUE_PREFIX}7"));
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    let reply = submit(
        &mut session,
        json!({"draft_id": "d1", "confirmed": true, "allow_duplicate": true}),
    )
    .await;

    assert!(!is_tool_error(&reply), "{reply}");
    assert!(
        tool_text(&reply).contains(&format!("{ISSUE_PREFIX}1")),
        "{reply}"
    );
    let calls = gh.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0][1], "create");
}

#[tokio::test]
async fn a_failed_look_up_does_not_stop_the_creation_and_is_reported() {
    let gh = FakeGh::new()
        .script("FAKE_GH_LIST_EXIT", "1")
        .script("FAKE_GH_LIST_STDERR", "search is down\n");
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    let reply = submit(&mut session, confirmed("d1")).await;

    assert!(!is_tool_error(&reply), "{reply}");
    let text = tool_text(&reply);
    assert!(text.contains(&format!("{ISSUE_PREFIX}1")), "{text}");
    assert!(
        text.contains("duplicate check failed") && text.contains("search is down"),
        "{text}"
    );
    assert_eq!(gh.calls().len(), 2);
}

#[tokio::test]
async fn a_client_that_is_not_signed_in_leaves_the_draft_ready_for_another_try() {
    let gh = FakeGh::new().script("FAKE_GH_CREATE_EXIT", "4").script(
        "FAKE_GH_CREATE_STDERR",
        "To get started with GitHub CLI, please run:  gh auth login\n",
    );
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    let reply = submit(&mut session, confirmed("d1")).await;

    assert!(is_tool_error(&reply));
    let text = tool_text(&reply);
    assert!(
        text.contains("Not created") && text.contains("not signed in"),
        "{text}"
    );
    assert!(
        text.contains("gh auth login") && text.contains("by hand"),
        "{text}"
    );

    let retry = submit(&mut session, confirmed("d1")).await;
    assert!(is_tool_error(&retry));
    assert_eq!(gh.calls().len(), 4, "the retry asked the client again");
}

#[tokio::test]
async fn a_missing_client_is_reported_with_the_way_to_install_it_and_the_hand_path() {
    let mut session = Session::start_with_env(
        free_port(),
        &no_godot(),
        &[("GODOT_MCP_GH_PATH", "no-such-gh-executable")],
    )
    .await;
    drafted(&mut session).await;
    let reply = submit(&mut session, confirmed("d1")).await;
    assert!(is_tool_error(&reply));
    let text = tool_text(&reply);
    assert!(text.contains("not found"), "{text}");
    assert!(
        text.contains("GODOT_MCP_GH_PATH") && text.contains("by hand"),
        "{text}"
    );
}

#[tokio::test]
async fn success_without_an_issue_address_is_unknown_and_is_never_retried() {
    let gh = FakeGh::new().script("FAKE_GH_CREATE_STDOUT", "Created!\n");
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    let reply = submit(&mut session, confirmed("d1")).await;

    assert!(is_tool_error(&reply));
    let text = tool_text(&reply);
    assert!(
        text.contains("Unknown whether the issue was created"),
        "{text}"
    );
    assert!(text.contains("before submitting again"), "{text}");

    let again = submit(&mut session, confirmed("d1")).await;
    assert!(tool_text(&again).contains("Unknown whether"), "{again}");
    assert_eq!(gh.calls().len(), 2, "no second creation");
}

#[tokio::test]
async fn gh_output_never_reaches_the_protocol_channel() {
    let gh = FakeGh::new()
        .script("FAKE_GH_LIST_STDOUT", "[]\nnoise from the look-up\n")
        .script("FAKE_GH_LIST_STDERR", "stderr noise\n")
        .script(
            "FAKE_GH_CREATE_STDOUT",
            format!("noise before\n{ISSUE_PREFIX}5\n").as_str(),
        )
        .script("FAKE_GH_CREATE_STDERR", "more stderr noise\n");
    let mut session = started(&gh).await;
    drafted(&mut session).await;
    // `Session::recv` fails the test on any line of standard output that is not JSON.
    let reply = submit(&mut session, confirmed("d1")).await;
    assert!(
        tool_text(&reply).contains(&format!("{ISSUE_PREFIX}5")),
        "{reply}"
    );
}

#[tokio::test]
async fn two_submissions_of_one_draft_at_the_same_time_create_one_issue() {
    let gh = FakeGh::new().script("FAKE_GH_DELAY_MS", "700");
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    for id in [9001, 9002] {
        session
            .send(&json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "godot_submit_feedback", "arguments": confirmed("d1")}
            }))
            .await;
    }
    let mut replies = Vec::new();
    while replies.len() < 2 {
        replies.push(
            session
                .recv(Duration::from_secs(30))
                .await
                .expect("a reply to each call"),
        );
    }

    let outcomes: Vec<String> = replies.iter().map(tool_text).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|text| text.contains(ISSUE_PREFIX))
            .count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|text| text.contains("being submitted"))
            .count(),
        1,
        "{outcomes:?}"
    );
    let creates = gh.calls().iter().filter(|call| call[1] == "create").count();
    assert_eq!(creates, 1);
}

#[tokio::test]
async fn a_cancelled_submission_leaves_the_draft_ready_for_another_try() {
    let gh = FakeGh::new().script("FAKE_GH_DELAY_MS", "1500");
    let mut session = started(&gh).await;
    drafted(&mut session).await;

    session
        .send(&json!({
            "jsonrpc": "2.0", "id": 9100, "method": "tools/call",
            "params": {"name": "godot_submit_feedback", "arguments": confirmed("d1")}
        }))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    session
        .send(&json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": 9100}
        }))
        .await;
    assert!(session.recv(Duration::from_millis(800)).await.is_none());

    let retry = submit(&mut session, confirmed("d1")).await;
    assert!(!is_tool_error(&retry), "{retry}");
    assert!(tool_text(&retry).contains(ISSUE_PREFIX), "{retry}");
}

#[tokio::test]
async fn a_title_that_looks_like_an_option_is_still_one_argument() {
    let gh = FakeGh::new();
    let mut session = started(&gh).await;
    note_with(&mut session, note("bug", "x")).await;
    draft_text(&mut session, json!({"title": "--web --repo evil/repo"})).await;
    submit(&mut session, confirmed("d1")).await;
    let create = gh
        .calls()
        .into_iter()
        .find(|call| call[1] == "create")
        .expect("a create call");
    assert_eq!(create.len(), 7, "{create:?}");
    assert_eq!(create[3], "zeegzoog/godot-mcp");
    assert_eq!(create[4], "--title=[agent-feedback] --web --repo evil/repo");
}

#[tokio::test]
async fn a_client_that_does_not_answer_in_time_is_unknown_for_a_create_and_a_failed_look_up() {
    let repo = RepoAddress::manifest().expect("repository");
    let client = IssueClient::new(fake_gh(), repo)
        .with_limits(Duration::from_millis(300), Duration::from_millis(300));

    let lookup = client.find_duplicate("FAKE_GH_SLEEP_3000").await;
    assert!(lookup.is_err(), "{lookup:?}");

    let created = client.create("FAKE_GH_SLEEP_3000", "body").await;
    assert!(matches!(created, Outcome::Unknown(_)), "{created:?}");
}

#[tokio::test]
async fn a_client_that_cannot_be_started_created_nothing() {
    let repo = RepoAddress::manifest().expect("repository");
    let client = IssueClient::new(PathBuf::from("no-such-gh-executable"), repo);
    match client.create("title", "body").await {
        Outcome::NotCreated { reason, steps } => {
            assert!(reason.contains("could not be started"), "{reason}");
            assert!(steps.iter().any(|step| step.contains("GODOT_MCP_GH_PATH")));
        }
        other => panic!("expected NotCreated, got {other:?}"),
    }
}
