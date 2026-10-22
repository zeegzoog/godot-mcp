//! The repo-steward skill: its release readiness script on a throwaway repository, and the
//! safety properties of the workflows it installs here — the agent workflow holds no write
//! access to the code, starts on no event an outsider can cause, and the reminder workflow
//! watches a label the skill creates.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const SKILL: &str = ".agents/skills/repo-steward";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("cannot read {relative}: {err}"))
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// `bash`, taken from Git for Windows on Windows: the `bash` found on `PATH` there can be the
/// WSL launcher, which runs in a different file system.
fn bash() -> Option<PathBuf> {
    if cfg!(windows) {
        let output = Command::new("git").arg("--exec-path").output().ok()?;
        let exec_path = String::from_utf8(output.stdout).ok()?;
        // <Git>/mingw64/libexec/git-core -> <Git>/bin/bash.exe
        let git_root = Path::new(exec_path.trim()).ancestors().nth(3)?;
        let candidate = git_root.join("bin").join("bash.exe");
        return candidate.is_file().then_some(candidate);
    }
    let output = Command::new("bash").arg("--version").output().ok()?;
    output.status.success().then(|| PathBuf::from("bash"))
}

/// Runs git in `dir` isolated from the user's configuration: no signing, no hooks, a fixed
/// identity, and the given date for both author and committer.
fn git(dir: &Path, args: &[&str], date: &str) {
    let status = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=no-hooks",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .status()
        .unwrap_or_else(|err| panic!("cannot run git {args:?}: {err}"));
    assert!(status.success(), "git {args:?} failed in the fixture");
}

fn write(dir: &Path, relative: &str, content: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture directory");
    }
    std::fs::write(path, content).expect("fixture file");
}

fn commit(dir: &Path, file: &str, content: &str, subject: &str, date: &str) {
    write(dir, file, content);
    git(dir, &["add", "--all"], date);
    git(dir, &["commit", "--quiet", "-m", subject], date);
}

/// A repository with one release (v0.1.0), two release targets on their own branches, and on
/// the default branch a feature, a fix, a documentation change, a fix that touches only an
/// ignored path and a release bookkeeping commit. Returns a clone, so `origin/*` refs exist as
/// they do in a real checkout.
fn readiness_fixture(base: &Path) -> PathBuf {
    let origin = base.join("origin");
    std::fs::create_dir_all(&origin).expect("fixture origin");
    let start = "2020-01-01T00:00:00Z";
    git(&origin, &["init", "--quiet"], start);
    write(
        &origin,
        ".github/steward.yml",
        "release:\n  targets_file: targets\n  target_branch: rel-{t}\n  \
         target_tag: v{version}+t.{t}\n  target_build_file: .ci/build.txt\n  \
         ignore_paths: notes\n  skip_subjects: chore(release) chore(line)\n",
    );
    write(&origin, "targets", "# release targets\na\nb\n");
    commit(&origin, "README.md", "fixture\n", "chore: start", start);
    git(&origin, &["tag", "-a", "v0.1.0", "-m", "v0.1.0"], start);

    for target in ["a", "b"] {
        let date = "2020-01-02T00:00:00Z";
        git(
            &origin,
            &["switch", "--quiet", "-c", &format!("rel-{target}"), "main"],
            date,
        );
        let build = format!("build-{target}-1\n");
        let subject = format!("chore(line): start {target}");
        commit(&origin, ".ci/build.txt", &build, &subject, date);
        let tag = format!("v0.1.0+t.{target}");
        git(&origin, &["tag", "-a", &tag, "-m", &tag], date);
    }
    git(&origin, &["switch", "--quiet", "rel-a"], start);
    commit(
        &origin,
        "a.txt",
        "a\n",
        "fix(a): line only",
        "2020-01-05T00:00:00Z",
    );

    git(&origin, &["switch", "--quiet", "main"], start);
    let changes = [
        (
            "src.txt",
            "1\n",
            "feat(core): add a thing",
            "2020-02-01T00:00:00Z",
        ),
        (
            "src.txt",
            "2\n",
            "fix(core): repair a thing",
            "2020-02-02T00:00:00Z",
        ),
        (
            "docs.md",
            "d\n",
            "docs: describe the thing",
            "2020-02-03T00:00:00Z",
        ),
        (
            "notes/n.md",
            "n\n",
            "fix(notes): only notes",
            "2020-02-04T00:00:00Z",
        ),
        (
            "CHANGELOG.md",
            "c\n",
            "chore(release): bookkeeping",
            "2020-02-05T00:00:00Z",
        ),
    ];
    for (file, content, subject, date) in changes {
        commit(&origin, file, content, subject, date);
    }

    let work = base.join("work");
    let origin_arg = origin.to_string_lossy().into_owned();
    let work_arg = work.to_string_lossy().into_owned();
    git(base, &["clone", "--quiet", &origin_arg, &work_arg], start);
    work
}

#[test]
fn readiness_counts_user_facing_changes_and_lists_every_target() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    if !git_available() {
        eprintln!("skipped: git is not installed");
        return;
    }
    let base = tempfile::tempdir().expect("fixture base");
    let work = readiness_fixture(base.path());
    let script = root().join(SKILL).join("scripts").join("readiness.sh");

    let output = Command::new(bash)
        .arg(&script)
        .current_dir(&work)
        .env("STEWARD_OFFLINE", "1")
        .output()
        .unwrap_or_else(|err| panic!("cannot run {}: {err}", script.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{SKILL}/scripts/readiness.sh failed:\n{stderr}"
    );

    assert_eq!(
        stdout.lines().next(),
        Some("state=due version=0.2.0"),
        "a feature and a fix older than the age threshold make a minor release due; \
         {SKILL}/scripts/readiness.sh printed:\n{stdout}"
    );
    for expected in [
        "feat(core): add a thing",
        "fix(core): repair a thing",
        "build-a-1",
        "build-b-1",
        "fix(a): line only",
    ] {
        assert!(
            stdout.contains(expected),
            "the notice must contain {expected:?}; {SKILL}/scripts/readiness.sh printed:\n{stdout}"
        );
    }
    for unexpected in [
        "describe the thing",
        "only notes",
        "bookkeeping",
        "chore(line): start",
    ] {
        assert!(
            !stdout.contains(unexpected),
            "the notice must not list {unexpected:?} (internal, ignored path or release \
             bookkeeping); {SKILL}/scripts/readiness.sh printed:\n{stdout}"
        );
    }
}

#[test]
fn reminder_workflow_watches_a_label_the_skill_creates() {
    let workflow = read(".github/workflows/steward-stale.yml");
    let watched = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("only-labels:"))
        .map(|value| {
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        })
        .expect(".github/workflows/steward-stale.yml must limit actions/stale with only-labels");
    assert_eq!(
        watched, "status: needs-info",
        ".github/workflows/steward-stale.yml must remind only on the needs-info label"
    );
    let labels = read(&format!("{SKILL}/scripts/labels.sh"));
    assert!(
        labels.contains(&watched),
        "{SKILL}/scripts/labels.sh must create the label {watched:?} the reminder workflow watches"
    );
}

/// The top-level keys under `on:` of a workflow file.
fn triggers(workflow: &str) -> Vec<String> {
    let mut lines = workflow.lines().skip_while(|line| line.trim_end() != "on:");
    lines.next();
    lines
        .take_while(|line| line.is_empty() || line.starts_with(' ') || line.starts_with('#'))
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .filter_map(|line| line.trim().split_once(':').map(|(key, _)| key.to_string()))
        .filter(|key| !key.starts_with('#'))
        .collect()
}

#[test]
fn agent_workflow_has_no_write_access_to_code_and_waits_for_the_switch() {
    let file = ".github/workflows/steward-agent.yml";
    let workflow = read(file);
    assert!(
        workflow.contains("vars.STEWARD_AGENT == 'on'"),
        "{file} must run only when the repository variable STEWARD_AGENT is on"
    );
    assert!(
        !workflow.contains("contents: write"),
        "{file} must not grant contents: write; the unattended agent never pushes or merges"
    );
    assert!(
        !workflow.contains("pull_request_target"),
        "{file} must not run on pull_request_target"
    );
    for head in [
        "github.event.pull_request.head",
        "refs/pull/",
        "github.head_ref",
    ] {
        assert!(
            !workflow.contains(head),
            "{file} must check out only the default branch, never a pull request head ({head})"
        );
    }
}

#[test]
fn agent_workflow_starts_only_on_schedule_or_by_hand() {
    let file = ".github/workflows/steward-agent.yml";
    let found = triggers(&read(file));
    assert!(!found.is_empty(), "{file} has no `on:` triggers");
    for trigger in &found {
        assert!(
            trigger == "schedule" || trigger == "workflow_dispatch",
            "{file} must start only on schedule or workflow_dispatch, found {trigger}"
        );
    }
}

/// The steps of a workflow job, each from its `- ` line (six spaces deep) to the next one.
fn steps(workflow: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in workflow.lines() {
        let depth = line.len() - line.trim_start().len();
        if depth == 6 && line.trim_start().starts_with("- ") {
            found.push(String::new());
        } else if !line.trim().is_empty() && depth < 6 {
            continue;
        }
        if let Some(step) = found.last_mut() {
            step.push_str(line);
            step.push('\n');
        }
    }
    found
}

#[test]
fn agent_workflow_keeps_every_token_out_of_the_agents_reach() {
    let file = ".github/workflows/steward-agent.yml";
    let workflow = read(file);
    let all = steps(&workflow);
    let checkout = all
        .iter()
        .find(|step| step.contains("actions/checkout"))
        .unwrap_or_else(|| panic!("{file} has no checkout step"));
    assert!(
        checkout.contains("persist-credentials: false"),
        "{file}: the checkout must not leave the workflow token in .git/config"
    );
    let agent = all
        .iter()
        .find(|step| step.contains("claude-code-action"))
        .unwrap_or_else(|| panic!("{file} has no claude-code-action step"));
    assert!(
        !agent.contains("STEWARD_PROJECT_TOKEN"),
        "{file}: the Projects token belongs to the step that applies the board queue, not to the agent"
    );
    for open in [
        "Bash(gh api",
        "Bash(gh issue:*)",
        "Bash(gh pr:*)",
        "--body-file",
    ] {
        assert!(
            !agent.contains(open),
            "{file}: the agent must not get {open}; it writes through the skill's act.sh"
        );
    }
    assert!(
        agent.contains("Read(//proc/**)"),
        "{file}: the agent must be denied reading /proc, where process environments are"
    );
}

#[test]
fn agent_workflow_gives_the_agent_no_way_to_write_or_run_its_own_files() {
    let file = ".github/workflows/steward-agent.yml";
    let workflow = read(file);
    let all = steps(&workflow);
    let agent_index = all
        .iter()
        .position(|step| step.contains("claude-code-action"))
        .unwrap_or_else(|| panic!("{file} has no claude-code-action step"));
    let agent = &all[agent_index];
    // git log and git show write files with --output; a wildcard over the scripts directory
    // would then run whatever was written there.
    for open in ["Bash(git ", "scripts/*)", "scripts/*:"] {
        assert!(
            !agent.contains(open),
            "{file}: the agent must not get {open}; name each script exactly"
        );
    }
    assert!(
        all[..agent_index]
            .iter()
            .any(|step| step.contains("chmod -R a-w .agents")),
        "{file}: the skill directory must be made read-only before the agent step"
    );
}

#[test]
fn readiness_takes_no_option_for_a_branch_name() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    let dir = tempfile::tempdir().expect("output directory");
    let target = dir.path().join("written-by-git");
    let option = format!("--output={}", target.display());
    let (ok, text) = run_script(&bash, "readiness.sh", &["--default-branch", &option], &[]);
    assert!(
        !ok,
        "readiness.sh must refuse a branch name that is an option:\n{text}"
    );
    assert!(
        !target.exists(),
        "readiness.sh passed a branch name to git as an option, and git wrote a file"
    );
}

#[test]
fn api_released_in_takes_only_a_commit_id() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    for bad in ["--output=x", "HEAD", "abc;id", "1234"] {
        let (ok, text) = run_script(&bash, "api.sh", &["released-in", bad], &[]);
        assert!(!ok, "api.sh released-in must refuse {bad:?}:\n{text}");
    }
}

#[test]
fn workflows_pin_third_party_actions_to_a_commit() {
    let mut names: Vec<String> = std::fs::read_dir(root().join(".github/workflows"))
        .expect("workflows directory")
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with(".yml"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no workflow files found");
    for name in names {
        let file = format!(".github/workflows/{name}");
        for line in read(&file).lines() {
            let Some(action) = line.trim().trim_start_matches("- ").strip_prefix("uses:") else {
                continue;
            };
            let action = action.trim();
            if action.starts_with("actions/") {
                continue;
            }
            let reference = action.split_once('@').map_or("", |(_, r)| r);
            let sha = reference.split_whitespace().next().unwrap_or("");
            assert!(
                sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
                "{file}: third-party action {action} must be pinned to a full commit SHA"
            );
        }
    }
}

fn run_script(bash: &Path, script: &str, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let output = Command::new(bash)
        .arg(root().join(SKILL).join("scripts").join(script))
        .args(args)
        .current_dir(root())
        .env("STEWARD_OFFLINE", "1")
        .envs(envs.iter().copied())
        .output()
        .unwrap_or_else(|err| panic!("cannot run {script}: {err}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[test]
fn act_posts_only_disclosed_messages_without_secrets() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    let ending = "_Written by an AI assistant on behalf of the maintainers._\n\
                  <!-- repo-steward:v1 kind=answer state=answered round=0 -->";
    let good = format!("The answer.\n\n{ending}");
    let (ok, text) = run_script(&bash, "act.sh", &["comment", "1", &good, "--dry-run"], &[]);
    assert!(ok, "act.sh must accept a disclosed message:\n{text}");
    assert!(
        text.contains("would run"),
        "act.sh --dry-run must only print:\n{text}"
    );

    let (ok, text) = run_script(
        &bash,
        "act.sh",
        &["comment", "1", "The answer.", "--dry-run"],
        &[],
    );
    assert!(
        !ok,
        "act.sh must refuse a message without the disclosure and marker:\n{text}"
    );

    let token = format!("ghs_{}", "a".repeat(36));
    let leaking = format!("The answer: {token}\n\n{ending}");
    let (ok, text) = run_script(
        &bash,
        "act.sh",
        &["comment", "1", &leaking, "--dry-run"],
        &[],
    );
    assert!(
        !ok,
        "act.sh must refuse a message that contains a token:\n{text}"
    );

    let (ok, text) = run_script(
        &bash,
        "act.sh",
        &["comment", "1;x", &good, "--dry-run"],
        &[],
    );
    assert!(
        !ok,
        "act.sh must refuse an item that is not a number:\n{text}"
    );
}

/// A directory holding the stand-in GitHub client under the name `gh`, for putting first on PATH.
fn gh_directory() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("fake gh directory");
    let name = if cfg!(windows) { "gh.exe" } else { "gh" };
    std::fs::copy(common::fake_gh(), dir.path().join(name)).expect("copy the fake gh");
    dir
}

#[test]
fn act_hands_gh_a_message_file_it_can_open() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    // A native gh on Windows cannot open the POSIX path bash's mktemp prints (/tmp/...).
    let gh = gh_directory();
    let record = tempfile::tempdir().expect("record directory");
    let body_record = record.path().join("body.txt");
    let path = std::env::join_paths(std::iter::once(gh.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let path = path.to_string_lossy().into_owned();
    let body_env = body_record.to_string_lossy().into_owned();
    let message = "The answer.\n\n_Written by an AI assistant on behalf of the maintainers._\n\
                   <!-- repo-steward:v1 kind=answer state=answered round=0 -->";
    let (ok, text) = run_script(
        &bash,
        "act.sh",
        &["comment", "1", message],
        &[("PATH", path.as_str()), ("FAKE_GH_BODY", body_env.as_str())],
    );
    assert!(ok, "act.sh comment failed with the stand-in gh:\n{text}");
    let posted = std::fs::read_to_string(&body_record).unwrap_or_default();
    assert_eq!(
        posted.trim_end(),
        message,
        "gh must receive the message through a file it can open"
    );
}

#[test]
fn board_queue_takes_only_this_repository_and_known_fields() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    let (ok, repo) = run_script(&bash, "context.sh", &["get", "repo"], &[]);
    assert!(ok, "context.sh get repo failed:\n{repo}");
    let url = format!("https://github.com/{}/issues/1", repo.trim());
    let dir = tempfile::tempdir().expect("queue directory");
    let queue = dir.path().join("queue.txt");
    let queue_env = queue.to_string_lossy().into_owned();
    let envs = [("STEWARD_BOARD_QUEUE", queue_env.as_str())];

    let (ok, text) = run_script(
        &bash,
        "board.sh",
        &["item", &url, "Status=Ready", "Priority=P2"],
        &envs,
    );
    assert!(ok, "board.sh must queue a valid change:\n{text}");
    let queued = std::fs::read_to_string(&queue).unwrap_or_default();
    assert!(
        queued.contains(&url) && queued.contains("Status=Ready"),
        "board.sh must write the change to STEWARD_BOARD_QUEUE, found:\n{queued}"
    );

    for (args, why) in [
        (
            vec![
                "item",
                "https://github.com/someone/else/issues/1",
                "Status=Ready",
            ],
            "another repository",
        ),
        (
            vec!["item", url.as_str(), "Status=Shipped"],
            "an unknown Status option",
        ),
        (vec!["item", url.as_str(), "Owner=me"], "an unknown field"),
        (
            vec!["item", url.as_str(), "Started=yesterday"],
            "a date that is not YYYY-MM-DD",
        ),
    ] {
        let (ok, text) = run_script(&bash, "board.sh", &args, &envs);
        assert!(!ok, "board.sh must refuse {why}:\n{text}");
    }
}

#[test]
fn skill_directory_is_tracked() {
    let skill = format!("{SKILL}/SKILL.md");
    assert!(root().join(&skill).is_file(), "{skill} is missing");
    if !git_available() {
        eprintln!("skipped the ignore check: git is not installed");
        return;
    }
    let status = Command::new("git")
        .current_dir(root())
        .args(["check-ignore", "-q", &skill])
        .status()
        .expect("git check-ignore runs");
    assert!(
        !status.success(),
        "{skill} is ignored; .gitignore needs the exception !/.agents/skills/repo-steward"
    );
}

#[test]
fn every_text_that_reaches_github_is_checked_for_credentials() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    let token = format!("ghs_{}", "a".repeat(36));
    let ending = "_Written by an AI assistant on behalf of the maintainers._\n\
                  <!-- repo-steward:v1 kind=answer state=answered round=0 -->";
    let good_body = format!("The answer.\n\n{ending}");

    // act.sh: the title of a pull request and the subject of a merge are public text too.
    let leaking_title = format!("fix: use {token}");
    let (ok, text) = run_script(
        &bash,
        "act.sh",
        &[
            "open-pr",
            "master",
            "topic",
            &leaking_title,
            &good_body,
            "--dry-run",
        ],
        &[],
    );
    assert!(!ok, "act.sh must refuse a title with a token:\n{text}");
    assert!(text.contains("credential"), "{text}");
    let (ok, text) = run_script(
        &bash,
        "act.sh",
        &["merge", "1", "squash", &leaking_title, "--dry-run"],
        &[],
    );
    assert!(
        !ok,
        "act.sh must refuse a merge subject with a token:\n{text}"
    );
    let (ok, text) = run_script(
        &bash,
        "act.sh",
        &[
            "open-pr",
            "master",
            "topic",
            "fix: a title",
            &good_body,
            "--dry-run",
        ],
        &[],
    );
    assert!(ok, "act.sh must accept a clean title:\n{text}");

    // board.sh: a status update and a field value, queued or applied.
    let (ok, repo) = run_script(&bash, "context.sh", &["get", "repo"], &[]);
    assert!(ok, "context.sh get repo failed:\n{repo}");
    let url = format!("https://github.com/{}/issues/1", repo.trim());
    let dir = tempfile::tempdir().expect("queue directory");
    let queue = dir.path().join("queue.txt");
    let queue_env = queue.to_string_lossy().into_owned();
    let envs = [("STEWARD_BOARD_QUEUE", queue_env.as_str())];

    let leaking_update = format!("Board: done\nRelease: {token}");
    let (ok, text) = run_script(
        &bash,
        "board.sh",
        &["status-update", &leaking_update],
        &envs,
    );
    assert!(
        !ok,
        "board.sh must refuse a status update with a token:\n{text}"
    );
    let blocked = format!("Blocked={token}");
    let (ok, text) = run_script(&bash, "board.sh", &["item", &url, &blocked], &envs);
    assert!(
        !ok,
        "board.sh must refuse a field value with a token:\n{text}"
    );
    assert!(
        !queue.exists()
            || std::fs::read_to_string(&queue)
                .unwrap_or_default()
                .is_empty(),
        "a refused text was queued"
    );

    let (ok, text) = run_script(
        &bash,
        "board.sh",
        &["status-update", "Board: 2 in review\nRelease: v0.7.0 due"],
        &envs,
    );
    assert!(ok, "board.sh must queue a clean status update:\n{text}");

    // The step that applies a queue checks every line again: a line written by hand is skipped.
    // `apply` asks GitHub who owns the repository before it reads the queue, so a stand-in `gh`
    // that answers only that question goes first on PATH; any other call it makes fails.
    let stub = tempfile::tempdir().expect("stub gh directory");
    std::fs::write(
        stub.path().join("gh"),
        "#!/bin/bash\ncase \"$1 $2\" in \"api users/\"*) echo Organization; exit 0 ;; esac\n\
         echo \"unexpected gh call: $*\" >&2\nexit 1\n",
    )
    .expect("write stub gh");
    // Git for Windows' bash runs a script only when bash itself marked it executable.
    let status = Command::new(&bash)
        .args(["-c", "chmod +x gh"])
        .current_dir(stub.path())
        .status()
        .expect("chmod the stub gh");
    assert!(status.success(), "cannot mark the stub gh executable");
    let path = std::env::join_paths(std::iter::once(stub.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let path = path.to_string_lossy().into_owned();
    let applied = dir.path().join("hand-written.txt");
    std::fs::write(&applied, format!("status\tBoard: {token}\n")).expect("write queue");
    let (_, text) = run_script(
        &bash,
        "board.sh",
        &["apply", &applied.to_string_lossy()],
        &[("PATH", path.as_str())],
    );
    assert!(
        text.contains("skipped") && !text.contains("status update posted"),
        "board.sh apply must skip a queued status update with a token:\n{text}"
    );
    assert!(
        !text.contains("unexpected gh call"),
        "a refused status update still reached GitHub:\n{text}"
    );
}

#[test]
fn delete_branch_refuses_what_is_not_a_finished_branch_before_asking_github() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    for (name, why) in [
        ("-D", "an option"),
        ("a..b", "a parent segment"),
        ("a b", "a space"),
        ("topic;rm", "a shell character"),
        ("/topic", "a leading slash"),
        ("godot-v4.7", "a release branch"),
        ("godot-v4.99", "a release branch of a later line"),
    ] {
        let (ok, text) = run_script(&bash, "act.sh", &["delete-branch", name, "--dry-run"], &[]);
        assert!(
            !ok,
            "act.sh delete-branch must refuse {why} ({name}):\n{text}"
        );
        assert!(
            !text.contains("would run"),
            "a refused branch reached the delete call ({name}):\n{text}"
        );
    }
}

#[test]
fn deleting_a_finished_branch_is_allowed_by_default() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash found (Git for Windows' bash on Windows)");
        return;
    };
    let (ok, value) = run_script(
        &bash,
        "profile.sh",
        &["get", "autonomy.delete_merged_branches"],
        &[],
    );
    assert!(ok, "profile.sh failed:\n{value}");
    assert_eq!(
        value.trim(),
        "allowed",
        "deleting a finished branch is allowed by default"
    );
}
