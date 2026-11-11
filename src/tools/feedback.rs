//! The tools of the agent feedback channel: record a note, draft a report, submit it. What the
//! channel guarantees is specified in terms of the journal (`crate::journal`), the text helpers
//! (`crate::feedback`) and the GitHub client (`crate::issues`); this module connects them to the
//! tool manager.

use super::{ToolManager, tool};
use crate::config::FeedbackMode;
use crate::error::McpError;
use crate::feedback::ScrubContext;
use crate::issues::{IssueClient, RepoAddress, locate_gh};
use crate::journal::{
    BuiltDraft, Draft, DraftRequest, Environment, EvidenceKind, Impact, Journal, Kind, NewNote,
    Outcome, Refusal, SubmitRefusal, fenced_as,
};
use crate::paths::resolve_in_project;
use crate::protocol::{Tool, ToolContent, ToolResult};
use crate::util::lock;
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::timeout;

/// How long the editor and the engine have, together, to say which versions they are.
const ENVIRONMENT_BUDGET: Duration = Duration::from_secs(5);

pub(super) const NOTE: &str = "godot_note_feedback";
pub(super) const DRAFT: &str = "godot_draft_feedback";
pub(super) const SUBMIT: &str = "godot_submit_feedback";

pub(super) fn is_feedback_tool(name: &str) -> bool {
    matches!(name, NOTE | DRAFT | SUBMIT)
}

/// Everything the channel keeps beside the tool manager's own fields.
pub(super) struct FeedbackState {
    pub(super) mode: FeedbackMode,
    pub(super) journal: Journal,
    pub(super) gh_path: Option<PathBuf>,
    pub(super) repo: Option<RepoAddress>,
    /// The project directory this session last resolved, so it can be hidden in reports.
    project_dir: Mutex<Option<PathBuf>>,
}

impl FeedbackState {
    pub(super) fn new(mode: FeedbackMode, gh_path: Option<PathBuf>) -> Self {
        Self {
            mode,
            journal: Journal::new(),
            gh_path,
            repo: RepoAddress::manifest(),
            project_dir: Mutex::new(None),
        }
    }

    pub(super) fn remember_project(&self, dir: &Path) {
        *lock(&self.project_dir) = Some(dir.to_path_buf());
    }

    pub(super) fn known_project(&self) -> Option<PathBuf> {
        lock(&self.project_dir).clone()
    }
}

/// The catalog entries the mode allows.
pub(super) fn catalog(mode: FeedbackMode) -> Vec<Tool> {
    let mut tools = Vec::new();
    if mode.enabled() {
        tools.push(tool(
            NOTE,
            "Records one observation for the godot-mcp maintainers: something missing, broken or misleading in godot-mcp itself (a tool, its message or schema, the editor addon, the README). Not for your own mistakes or problems in the user's project. Write in English: what you tried, what came back, what you expected; one problem per note. The note stays in memory and nothing is sent. Later, godot_draft_feedback turns notes into a report for the human.",
            json!({
                "kind": {
                    "type": "string",
                    "enum": ["bug", "missing_feature", "improvement", "docs"],
                    "description": "bug: a tool misbehaves or fails for a reason it does not explain. missing_feature: you needed a capability no tool offers. improvement: it works but could be clearer, faster or safer. docs: a description, schema or README is wrong or unclear."
                },
                "summary": {
                    "type": "string",
                    "description": "One sentence, at most 120 characters."
                },
                "details": {
                    "type": "string",
                    "description": "What you tried, what came back, what you expected. At most 2000 characters. No secrets and no file contents."
                },
                "tool": {
                    "type": "string",
                    "description": "Name of the godot-mcp tool involved, such as godot_add_node. Lets the report quote that tool's recent failures."
                },
                "impact": {
                    "type": "string",
                    "enum": ["blocked", "workaround", "minor"],
                    "description": "blocked: you could not finish the task. workaround: you found another way. minor: a nuisance."
                },
                "suggestion": {
                    "type": "string",
                    "description": "What should change, at most 500 characters."
                }
            }),
            &["kind", "summary", "details"],
        ));
        let submission = if mode.submits() {
            "have it submitted with godot_submit_feedback, "
        } else {
            ""
        };
        tools.push(tool(
            DRAFT,
            &format!(
                "Builds one report for the godot-mcp maintainers from your notes (by default every note not yet in a report; pass note_ids to choose, one report per problem) plus what the server observed: versions, platform, failures of the tools your notes name and requests for tools that do not exist. It contacts no remote service. Call it when the user's request is completed or abandoned, or when the user asks for feedback, and only when notes exist. Then show the user the COMPLETE report, in the user's language: the issue title first, then the issue body. Then offer: copy it, open the link, {submission}or discard it."
            ),
            json!({
                "note_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ids of the notes to include, such as [\"n1\", \"n3\"]. Default: every note not yet in a report."
                },
                "title": {
                    "type": "string",
                    "description": "Optional title; by default it is built from the first note."
                }
            }),
            &[],
        ));
    }
    if mode.submits() {
        tools.push(tool(
            SUBMIT,
            "Publishes a draft from godot_draft_feedback as a PUBLIC GitHub issue of the godot-mcp project, through the user's own signed-in GitHub CLI. It takes only the draft id, so what is published is exactly the report the user saw. Call it ONLY after the user has seen the complete report and said an explicit yes in this conversation; then pass confirmed: true. It reports whether the issue was created, a duplicate already exists, or nothing was created.",
            json!({
                "draft_id": {
                    "type": "string",
                    "description": "The id returned by godot_draft_feedback, such as d1."
                },
                "confirmed": {
                    "type": "boolean",
                    "description": "true only when the user explicitly approved publishing this exact report."
                },
                "allow_duplicate": {
                    "type": "boolean",
                    "description": "Create the issue although one with the same fingerprint exists. Only when the user has looked at that issue and still wants a new one."
                }
            }),
            &["draft_id", "confirmed"],
        ));
    }
    tools
}

/// The guidance sent at session start, or `None` when the channel is off.
pub fn instructions(mode: FeedbackMode) -> Option<String> {
    if !mode.enabled() {
        return None;
    }
    let ending = if mode.submits() {
        "offer: copy it, open the link, submit it with godot_submit_feedback, or discard it. \
         Never call godot_submit_feedback without the user's explicit yes in this conversation."
    } else {
        "offer: copy it, open the link, or discard it. The report can only be handed over as text."
    };
    Some(format!(
        "godot-mcp feedback channel. While you work, if godot-mcp itself blocks or misleads you \
         (a tool fails for a reason its message does not explain, a needed capability has no tool, \
         a description or schema is wrong), record it with godot_note_feedback: English, what you \
         tried, what came back, what you expected, one problem per note. Do not note your own \
         mistakes or problems in the user's project. When the user's request is completed or \
         abandoned, or when asked for feedback, and notes exist, call godot_draft_feedback, show \
         the user the complete report (the issue title first, then the issue body), and {ending} \
         Notes stay in memory; nothing leaves this machine otherwise."
    ))
}

/// Returns a draft that was being submitted to `Pending` when the call that claimed it is
/// dropped before it stored an outcome. After an outcome is stored this does nothing.
struct SubmitGuard<'a> {
    journal: &'a Journal,
    id: String,
}

impl Drop for SubmitGuard<'_> {
    fn drop(&mut self) {
        self.journal.abort_submit(&self.id);
    }
}

/// What the agent is told about the outcome of a submission.
fn outcome_result(id: &str, outcome: &Outcome, notes: &[String], repeated: bool) -> ToolResult {
    let mut extra = String::new();
    for note in notes {
        extra.push_str(&format!(
            "
{note}"
        ));
    }
    let earlier = if repeated {
        " (submitted earlier in this session; nothing new was sent)"
    } else {
        ""
    };
    match outcome {
        Outcome::Created(address) => ToolResult::text(format!(
            "Created{earlier}: {address}

Draft {id} is now a PUBLIC issue under the user's GitHub account. Tell the user the address.{extra}"
        )),
        Outcome::Duplicate(address) => ToolResult::text(format!(
            "Not created: an issue with the same fingerprint already exists: {address}

Tell the user. If their problem differs from that issue, they can ask for a new one; then call again with allow_duplicate: true.{extra}"
        )),
        Outcome::NotCreated { reason, steps } => {
            let steps: Vec<&str> = steps.iter().map(String::as_str).collect();
            ToolResult::error(format!("Not created: {reason}"), &steps)
        }
        Outcome::Unknown(detail) => ToolResult::error(
            format!("Unknown whether the issue was created{earlier}: {detail}"),
            &[
                "Check the project's issues page for the new issue before submitting again",
                "Or file it by hand: give the user the report text, or the prefilled link from the draft",
            ],
        ),
    }
}

fn refusal_result(refusal: Refusal) -> ToolResult {
    ToolResult::error(refusal.message, &[refusal.hint.as_str()])
}

fn first_error_line(result: &ToolResult) -> &str {
    result
        .content
        .iter()
        .find_map(|content| match content {
            ToolContent::Text { text } => text.lines().next(),
            ToolContent::Image { .. } => None,
        })
        .map(|line| line.strip_prefix("Error: ").unwrap_or(line))
        .unwrap_or("")
}

impl ToolManager {
    /// The guidance for the `initialize` answer.
    pub fn guidance(&self) -> Option<String> {
        instructions(self.feedback.mode)
    }

    /// Remembers which client the session talks to.
    pub fn note_client(&self, name: Option<&str>, version: Option<&str>) {
        if self.feedback.mode.enabled() {
            self.feedback
                .journal
                .set_client(name, version, &self.scrub_context());
        }
    }

    /// A call for a tool name the server does not have is a request for a missing capability.
    pub fn record_unknown_tool(&self, name: &str) {
        if self.feedback.mode.enabled() {
            self.feedback.journal.record_evidence(
                EvidenceKind::UnknownTool,
                name,
                "",
                &self.scrub_context(),
            );
        }
    }

    pub(super) fn scrub_context(&self) -> ScrubContext {
        ScrubContext::from_environment(self.bridge.secret(), self.feedback.known_project())
    }

    /// Counts a finished call and keeps the first line of a failure. The result is not touched.
    pub(super) fn observe(&self, name: &str, result: &ToolResult, kind: EvidenceKind) {
        if !self.feedback.mode.enabled() || is_feedback_tool(name) {
            return;
        }
        self.feedback.journal.record_call(name, result.is_error);
        if result.is_error {
            self.feedback.journal.record_evidence(
                kind,
                name,
                first_error_line(result),
                &self.scrub_context(),
            );
        }
    }

    pub(super) async fn handle_note_feedback(&self, args: Value) -> ToolResult {
        let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_string);
        let Some(kind) = text("kind").and_then(|kind| Kind::parse(&kind)) else {
            return ToolResult::error(
                "kind must be one of bug, missing_feature, improvement, docs",
                &["Pick the kind that fits the observation and call again"],
            );
        };
        let note = NewNote {
            kind,
            summary: text("summary").unwrap_or_default(),
            details: text("details").unwrap_or_default(),
            tool: text("tool"),
            impact: text("impact").and_then(|impact| Impact::parse(&impact)),
            suggestion: text("suggestion"),
        };
        match self.feedback.journal.add_note(note, &self.scrub_context()) {
            Ok((id, pending)) => {
                let mut answer = format!(
                    "Noted as {id}. Notes not yet in a report ({}):\n",
                    pending.len()
                );
                for note in &pending {
                    answer.push_str(&format!(
                        "- {} ({}): {}\n",
                        note.id,
                        note.kind.as_str(),
                        note.summary
                    ));
                }
                answer.push_str(
                    "\nKeep working. When the user's request is completed or abandoned, call godot_draft_feedback.",
                );
                ToolResult::text(answer)
            }
            Err(refusal) => refusal_result(refusal),
        }
    }

    pub(super) async fn handle_draft_feedback(&self, args: Value) -> ToolResult {
        let note_ids = args.get("note_ids").and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string);

        // Without any note there is nothing to look up, so refuse before asking the editor.
        let nothing_to_draft =
            note_ids.is_none() && self.feedback.journal.pending_notes().is_empty();
        let environment = if nothing_to_draft {
            self.unknown_environment()
        } else {
            self.collect_environment().await
        };
        let link_base = self
            .feedback
            .repo
            .as_ref()
            .map(RepoAddress::new_issue_url)
            .unwrap_or_default();
        let built = self.feedback.journal.build_draft(
            DraftRequest { note_ids, title },
            &environment,
            &link_base,
            &self.scrub_context(),
        );
        match built {
            Ok(built) => ToolResult::text(self.draft_answer(&built)),
            Err(refusal) => refusal_result(refusal),
        }
    }

    fn unknown_environment(&self) -> Environment {
        let unknown = || "unknown".to_string();
        Environment {
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            addon_version: unknown(),
            engine_version: unknown(),
            platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            bridge_state: unknown(),
            feedback_mode: self.feedback.mode.as_str().to_string(),
        }
    }

    /// The facts a report quotes about this setup. The editor and the engine get a shared time
    /// budget; whatever has not answered by then is reported as unknown.
    async fn collect_environment(&self) -> Environment {
        let no_arguments = json!({});
        let lookups = async {
            let (engine_and_bridge, project) = tokio::join!(
                self.engine_version_and_bridge_state(),
                self.resolve_project(&no_arguments)
            );
            (engine_and_bridge, project.ok().map(|project| project.dir))
        };
        let ((engine_version, bridge_state), project_dir) =
            match timeout(ENVIRONMENT_BUDGET, lookups).await {
                Ok(found) => found,
                Err(_) => (
                    (
                        "unknown".to_string(),
                        format!(
                            "the editor did not answer within {} s",
                            ENVIRONMENT_BUDGET.as_secs()
                        ),
                    ),
                    self.feedback.known_project(),
                ),
            };
        let addon_version = match project_dir {
            Some(dir) => read_addon_version(&dir).await,
            None => "unknown".to_string(),
        };
        Environment {
            engine_version,
            addon_version,
            bridge_state,
            ..self.unknown_environment()
        }
    }

    async fn engine_version_and_bridge_state(&self) -> (String, String) {
        match self.bridge.send_command("get_version", json!({})).await {
            Ok(result) => (
                result
                    .get("version_string")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                "connected".to_string(),
            ),
            Err(McpError::BridgeUnavailable(_)) => (
                self.cli
                    .get_version()
                    .await
                    .ok()
                    .filter(|version| !version.is_empty())
                    .unwrap_or_else(|| "unknown".to_string()),
                "not reachable (editor closed or plugin disabled)".to_string(),
            ),
            Err(McpError::ToolExecution(message)) => (
                "unknown".to_string(),
                format!("the editor answered with an error: {message}"),
            ),
            Err(other) => (
                "unknown".to_string(),
                format!("connection problem: {other}"),
            ),
        }
    }

    fn gh_available(&self) -> bool {
        locate_gh(self.feedback.gh_path.as_deref()).is_some()
    }

    /// What the agent gets back for a new draft: the whole report, the ways to deliver it and
    /// what to do next.
    fn draft_answer(&self, built: &BuiltDraft) -> String {
        let draft = &built.draft;
        let submit = self.feedback.mode.submits() && self.gh_available();
        let mut out = format!(
            "Draft {} is ready. Nothing has been sent anywhere.\n\nIssue title: {}\nFingerprint: {}\n\n\
             Issue body (the complete text, exactly as it would be published):\n\n",
            draft.id, draft.title, draft.fingerprint
        );
        out.push_str(&fenced_as(&draft.body, "markdown"));
        out.push_str("\n\nWays to deliver it:\n");
        out.push_str(
            "- Copy: always available. The user copies the report text above into a new issue.\n",
        );
        match &draft.link {
            Ok(link) => out.push_str(&format!(
                "- Link: {link}\n  It opens a prefilled new-issue page; the user reviews it and presses the button.\n"
            )),
            Err(reason) => out.push_str(&format!("- Link: not available ({reason})\n")),
        }
        if self.feedback.mode.submits() {
            if submit {
                out.push_str(&format!(
                    "- Submit: available through godot_submit_feedback with draft_id \"{}\". It publishes the report publicly under the user's GitHub account.\n",
                    draft.id
                ));
            } else {
                out.push_str(
                    "- Submit: not available: the GitHub CLI was not found. Install it and sign in with `gh auth login`, or set GODOT_MCP_GH_PATH.\n",
                );
            }
        }
        if !built.left_out.is_empty() {
            out.push_str("\nNotes not in any report yet:\n");
            for note in &built.left_out {
                out.push_str(&format!(
                    "- {} ({}): {}\n",
                    note.id,
                    note.kind.as_str(),
                    note.summary
                ));
            }
        }
        out.push_str(
            "\nNext steps for you:\n1. Show the user the COMPLETE report above in your reply: the issue title first, then the issue body. Keep both texts as they are; explain around them in the user's language.\n",
        );
        if submit {
            out.push_str(
                "2. Offer these choices: copy the text, open the link, have you submit it, or discard it.\n3. Call godot_submit_feedback only after the user has explicitly said yes in this conversation.\n",
            );
        } else {
            out.push_str("2. Offer these choices: copy the text, open the link, or discard it.\n");
        }
        out
    }

    pub(super) async fn handle_submit_feedback(&self, args: Value) -> ToolResult {
        let id = args
            .get("draft_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
        if !flag("confirmed") {
            return ToolResult::error(
                "Nothing was sent: confirmed must be true, and only after the user approved publishing this report",
                &[
                    "Show the user the complete report from godot_draft_feedback",
                    "Ask for an explicit yes in this conversation, then call again with confirmed: true",
                    "Or hand the report over as text or as the prefilled link",
                ],
            );
        }

        let draft = match self.feedback.journal.begin_submit(id) {
            Ok(draft) => draft,
            Err(SubmitRefusal::UnknownDraft { known }) => {
                let hint = if known.is_empty() {
                    "No draft exists yet: record notes and call godot_draft_feedback first"
                        .to_string()
                } else {
                    format!("Known draft ids: {}", known.join(", "))
                };
                return ToolResult::error(format!("Unknown draft id {id:?}"), &[hint.as_str()]);
            }
            Err(SubmitRefusal::InProgress) => {
                return ToolResult::error(
                    format!("Draft {id} is being submitted by another call"),
                    &["Wait for that call's outcome instead of submitting again"],
                );
            }
            Err(SubmitRefusal::AlreadyDone(outcome)) => {
                return outcome_result(id, &outcome, &[], true);
            }
        };
        // From here the draft is `Submitting`; the guard returns it to `Pending` if this call is
        // cancelled before an outcome is stored (a repeat then finds an issue that was created).
        let _guard = SubmitGuard {
            journal: &self.feedback.journal,
            id: draft.id.clone(),
        };
        let (outcome, notes) = self.publish(&draft, flag("allow_duplicate")).await;
        self.feedback
            .journal
            .finish_submit(&draft.id, outcome.clone());
        outcome_result(&draft.id, &outcome, &notes, false)
    }

    /// Looks for an existing issue with the draft's fingerprint, then creates the issue.
    async fn publish(&self, draft: &Draft, allow_duplicate: bool) -> (Outcome, Vec<String>) {
        let not_created = |reason: &str, steps: &[&str]| Outcome::NotCreated {
            reason: reason.to_string(),
            steps: steps.iter().map(|step| step.to_string()).collect(),
        };
        let Some(repo) = self.feedback.repo.clone() else {
            return (
                not_created(
                    "the project's repository address is unknown",
                    &["Give the user the report text to file by hand"],
                ),
                Vec::new(),
            );
        };
        let Some(gh) = locate_gh(self.feedback.gh_path.as_deref()) else {
            return (
                not_created(
                    "the GitHub CLI was not found",
                    &[
                        "Install the GitHub CLI (https://cli.github.com) and sign in with `gh auth login`",
                        "Or set GODOT_MCP_GH_PATH to the full path of the gh executable",
                        "Or file it by hand: give the user the report text, or the prefilled link from the draft",
                    ],
                ),
                Vec::new(),
            );
        };
        let client = IssueClient::new(gh, repo);
        let mut notes = Vec::new();
        if allow_duplicate {
            notes.push("The duplicate check was skipped at the user's request.".to_string());
        } else {
            match client.find_duplicate(&draft.fingerprint).await {
                Ok(Some(address)) => return (Outcome::Duplicate(address), notes),
                Ok(None) => {}
                Err(problem) => notes.push(format!(
                    "The duplicate check failed ({problem}), so the issue was created without it."
                )),
            }
        }
        (client.create(&draft.title, &draft.body).await, notes)
    }
}

/// The version the addon's manifest in the project declares.
async fn read_addon_version(project: &Path) -> String {
    let Ok(manifest) = resolve_in_project(project, "res://addons/godot_mcp/plugin.cfg") else {
        return "unknown".to_string();
    };
    match tokio::fs::read_to_string(&manifest).await {
        Ok(text) => crate::addon::manifest_version(&text).unwrap_or_else(|| "unknown".to_string()),
        Err(err) if err.kind() == ErrorKind::NotFound => "not installed in the project".to_string(),
        Err(_) => "unknown".to_string(),
    }
}
