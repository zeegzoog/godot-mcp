//! The journal of the agent feedback channel: the notes an agent recorded, what the server
//! observed itself, and the drafts built from both. It lives in memory for the life of the
//! process; the text helpers (scrubbing, fingerprint, link) are in `feedback`.

use crate::feedback::{ScrubContext, build_link, fingerprint, scrub, shape};
use crate::util::lock;
use std::collections::BTreeMap;
use std::sync::Mutex;

pub const MAX_NOTES: usize = 20;
pub const MAX_DRAFTS: usize = 10;
pub const MAX_EVIDENCE: usize = 50;
pub const SUMMARY_LIMIT: usize = 120;
pub const DETAILS_LIMIT: usize = 2_000;
pub const SUGGESTION_LIMIT: usize = 500;
pub const TOOL_LIMIT: usize = 64;
pub const TITLE_LIMIT: usize = 100;
pub const EVIDENCE_MESSAGE_LIMIT: usize = 200;
pub const REPORT_LIMIT: usize = 24_000;
pub const CLIENT_FIELD_LIMIT: usize = 64;
pub const TITLE_PREFIX: &str = "[agent-feedback] ";

const INVALID_NAME: &str = "<invalid name>";
const MISSING_TOOLS_SHOWN: usize = 10;
const FAILURES_PER_TOOL: usize = 5;
const CELL_LIMIT: usize = 100;
const SHORTENED_NOTE: &str = "(details shortened)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bug,
    MissingFeature,
    Improvement,
    Docs,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "bug" => Some(Self::Bug),
            "missing_feature" => Some(Self::MissingFeature),
            "improvement" => Some(Self::Improvement),
            "docs" => Some(Self::Docs),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::MissingFeature => "missing_feature",
            Self::Improvement => "improvement",
            Self::Docs => "docs",
        }
    }

    /// The kind of a draft that mixes kinds is the one with the lowest rank.
    fn rank(self) -> u8 {
        match self {
            Self::Bug => 0,
            Self::MissingFeature => 1,
            Self::Improvement => 2,
            Self::Docs => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Impact {
    Blocked,
    Workaround,
    Minor,
}

impl Impact {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "blocked" => Some(Self::Blocked),
            "workaround" => Some(Self::Workaround),
            "minor" => Some(Self::Minor),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Workaround => "workaround",
            Self::Minor => "minor",
        }
    }

    fn severity(self) -> u8 {
        match self {
            Self::Blocked => 2,
            Self::Workaround => 1,
            Self::Minor => 0,
        }
    }
}

/// Why a call to the channel was refused, with the step that fixes it.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub message: String,
    pub hint: String,
}

impl Refusal {
    fn new(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hint: hint.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewNote {
    pub kind: Kind,
    pub summary: String,
    pub details: String,
    pub tool: Option<String>,
    pub impact: Option<Impact>,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone)]
struct Note {
    id: String,
    kind: Kind,
    summary: String,
    details: String,
    tool: Option<String>,
    impact: Option<Impact>,
    suggestion: Option<String>,
    drafted: bool,
}

/// A note that is not in any draft yet.
#[derive(Debug, Clone)]
pub struct PendingNote {
    pub id: String,
    pub kind: Kind,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    Error,
    InvalidArguments,
    UnknownTool,
}

impl EvidenceKind {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::InvalidArguments => "invalid arguments",
            Self::UnknownTool => "unknown tool",
        }
    }
}

/// Something the server observed: the same failure of the same tool is one entry with a count.
#[derive(Debug, Clone)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub tool: String,
    pub message: String,
    pub count: u32,
    last_seen: u64,
}

/// The facts of the running setup that a report quotes. Values are free text from other
/// processes, so the report builder sanitizes every one.
#[derive(Debug, Clone)]
pub struct Environment {
    pub server_version: String,
    pub addon_version: String,
    pub engine_version: String,
    pub platform: String,
    pub bridge_state: String,
    pub feedback_mode: String,
}

#[derive(Debug, Clone, Default)]
pub struct DraftRequest {
    pub note_ids: Option<Vec<String>>,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Created(String),
    Duplicate(String),
    NotCreated { reason: String, steps: Vec<String> },
    Unknown(String),
}

#[derive(Debug, Clone)]
pub enum DraftState {
    Pending,
    Submitting,
    Submitted(Outcome),
}

#[derive(Debug, Clone)]
pub struct Draft {
    pub id: String,
    pub title: String,
    pub body: String,
    pub fingerprint: String,
    /// The prefilled new-issue link, or the reason there is none.
    pub link: Result<String, String>,
    pub note_ids: Vec<String>,
    pub state: DraftState,
}

#[derive(Debug, Clone)]
pub struct BuiltDraft {
    pub draft: Draft,
    /// Notes that are still in no draft.
    pub left_out: Vec<PendingNote>,
}

#[derive(Debug)]
pub enum SubmitRefusal {
    UnknownDraft { known: Vec<String> },
    InProgress,
    AlreadyDone(Outcome),
}

#[derive(Default)]
struct State {
    notes: Vec<Note>,
    next_note: u32,
    evidence: Vec<Evidence>,
    sequence: u64,
    /// Per tool: calls, failures.
    counters: BTreeMap<String, (u32, u32)>,
    drafts: Vec<Draft>,
    next_draft: u32,
    client_name: Option<String>,
    client_version: Option<String>,
}

#[derive(Default)]
pub struct Journal {
    state: Mutex<State>,
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers which MCP client the session talks to (free text, sanitized).
    pub fn set_client(&self, name: Option<&str>, version: Option<&str>, context: &ScrubContext) {
        let clean = |value: Option<&str>| {
            value
                .map(|text| one_line(&scrub(text, context)))
                .map(|text| text.chars().take(CLIENT_FIELD_LIMIT).collect::<String>())
                .filter(|text| !text.is_empty())
        };
        let mut state = lock(&self.state);
        state.client_name = clean(name);
        state.client_version = clean(version);
    }

    /// Records one observation. Text is scrubbed first, then checked against its limit.
    pub fn add_note(
        &self,
        note: NewNote,
        context: &ScrubContext,
    ) -> Result<(String, Vec<PendingNote>), Refusal> {
        let summary = one_line(&scrub(&note.summary, context));
        if summary.is_empty() {
            return Err(Refusal::new(
                "summary is empty",
                "Describe the observation in one sentence of at most 120 characters",
            ));
        }
        check_length(
            "summary",
            &summary,
            SUMMARY_LIMIT,
            "Shorten it and put the rest in details",
        )?;
        let details = scrub(note.details.trim(), context);
        if details.is_empty() {
            return Err(Refusal::new(
                "details is empty",
                "Say what you tried, what came back and what you expected",
            ));
        }
        check_length(
            "details",
            &details,
            DETAILS_LIMIT,
            "Keep the essentials: what you tried, what came back, what you expected",
        )?;
        let suggestion = note
            .suggestion
            .as_deref()
            .map(|text| scrub(text.trim(), context))
            .filter(|text| !text.is_empty());
        if let Some(text) = &suggestion {
            check_length(
                "suggestion",
                text,
                SUGGESTION_LIMIT,
                "State the suggestion in a few sentences",
            )?;
        }
        if let Some(tool) = &note.tool
            && !is_tool_name(tool)
        {
            return Err(Refusal::new(
                format!(
                    "tool {tool:?} is not a tool name: use 1 to {TOOL_LIMIT} characters of a-z, 0-9 and underscore"
                ),
                "Pass the tool's name such as godot_add_node, or leave tool out",
            ));
        }

        let mut state = lock(&self.state);
        if state.notes.len() >= MAX_NOTES {
            return Err(Refusal::new(
                format!("the session already holds {MAX_NOTES} notes, which is the limit"),
                "Draft the notes with godot_draft_feedback and show the report to the human",
            ));
        }
        state.next_note += 1;
        let id = format!("n{}", state.next_note);
        state.notes.push(Note {
            id: id.clone(),
            kind: note.kind,
            summary,
            details,
            tool: note.tool,
            impact: note.impact,
            suggestion,
            drafted: false,
        });
        Ok((id, pending_of(&state)))
    }

    pub fn pending_notes(&self) -> Vec<PendingNote> {
        pending_of(&lock(&self.state))
    }

    /// Counts one finished call of a catalog tool.
    pub fn record_call(&self, tool: &str, failed: bool) {
        let mut state = lock(&self.state);
        let counter = state.counters.entry(tool.to_string()).or_insert((0, 0));
        counter.0 = counter.0.saturating_add(1);
        if failed {
            counter.1 = counter.1.saturating_add(1);
        }
    }

    /// Records something the server observed. Only the first line of `message` is kept, scrubbed
    /// and cut; a tool name that is not a plain name is not stored verbatim.
    pub fn record_evidence(
        &self,
        kind: EvidenceKind,
        tool: &str,
        message: &str,
        context: &ScrubContext,
    ) {
        let tool = if kind == EvidenceKind::UnknownTool && !is_plain_name(tool) {
            INVALID_NAME.to_string()
        } else {
            tool.to_string()
        };
        let scrubbed = scrub(message, context);
        let message: String = scrubbed
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(EVIDENCE_MESSAGE_LIMIT)
            .collect();

        let mut state = lock(&self.state);
        state.sequence += 1;
        let sequence = state.sequence;
        if let Some(event) = state
            .evidence
            .iter_mut()
            .find(|event| event.kind == kind && event.tool == tool && event.message == message)
        {
            event.count = event.count.saturating_add(1);
            event.last_seen = sequence;
            return;
        }
        if state.evidence.len() >= MAX_EVIDENCE
            && let Some(oldest) = state
                .evidence
                .iter()
                .enumerate()
                .min_by_key(|(_, event)| event.last_seen)
                .map(|(index, _)| index)
        {
            state.evidence.remove(oldest);
        }
        state.evidence.push(Evidence {
            kind,
            tool,
            message,
            count: 1,
            last_seen: sequence,
        });
    }

    /// Builds a draft from the chosen notes (default: every note not yet drafted) and the
    /// evidence that belongs to them, and stores it.
    pub fn build_draft(
        &self,
        request: DraftRequest,
        environment: &Environment,
        link_base: &str,
        context: &ScrubContext,
    ) -> Result<BuiltDraft, Refusal> {
        let mut state = lock(&self.state);
        if state.drafts.len() >= MAX_DRAFTS {
            return Err(Refusal::new(
                format!("the session already holds {MAX_DRAFTS} drafts, which is the limit"),
                "Submit or hand over an existing draft instead of drafting again",
            ));
        }
        let known_ids = || {
            state
                .notes
                .iter()
                .map(|note| note.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let ids: Vec<String> = match request.note_ids {
            Some(ids) if ids.is_empty() => {
                return Err(Refusal::new(
                    "note_ids is empty",
                    "Omit note_ids to draft every pending note, or list the ids to include",
                ));
            }
            Some(ids) => {
                let mut unique: Vec<String> = Vec::new();
                for id in ids {
                    if !unique.contains(&id) {
                        unique.push(id);
                    }
                }
                unique
            }
            None => state
                .notes
                .iter()
                .filter(|note| !note.drafted)
                .map(|note| note.id.clone())
                .collect(),
        };
        if ids.is_empty() {
            return Err(Refusal::new(
                "there are no notes to report",
                "Record an observation with godot_note_feedback first",
            ));
        }
        let unknown: Vec<&str> = ids
            .iter()
            .filter(|id| !state.notes.iter().any(|note| &note.id == *id))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(Refusal::new(
                format!("unknown note id(s): {}", unknown.join(", ")),
                format!("Known note ids: {}", known_ids()),
            ));
        }
        let chosen: Vec<Note> = ids
            .iter()
            .filter_map(|id| state.notes.iter().find(|note| &note.id == id).cloned())
            .collect();

        let kind = chosen
            .iter()
            .map(|note| note.kind)
            .min_by_key(|kind| kind.rank())
            .unwrap_or(Kind::Bug);
        let mut tools: Vec<String> = chosen.iter().filter_map(|note| note.tool.clone()).collect();
        tools.sort();
        tools.dedup();

        let missing = if kind == Kind::MissingFeature {
            let mut events: Vec<&Evidence> = state
                .evidence
                .iter()
                .filter(|event| event.kind == EvidenceKind::UnknownTool)
                .collect();
            events.sort_by_key(|event| std::cmp::Reverse((event.count, event.last_seen)));
            events.truncate(MISSING_TOOLS_SHOWN);
            events.into_iter().cloned().collect()
        } else {
            Vec::new()
        };
        let mut failures: Vec<Evidence> = Vec::new();
        for tool in &tools {
            let mut events: Vec<&Evidence> = state
                .evidence
                .iter()
                .filter(|event| event.kind != EvidenceKind::UnknownTool && &event.tool == tool)
                .collect();
            events.sort_by_key(|event| std::cmp::Reverse(event.last_seen));
            events.truncate(FAILURES_PER_TOOL);
            failures.extend(events.into_iter().cloned());
        }
        let counters: Vec<(String, (u32, u32))> = tools
            .iter()
            .filter_map(|tool| {
                state
                    .counters
                    .get(tool)
                    .map(|counts| (tool.clone(), *counts))
            })
            .collect();

        let fingerprint = fingerprint_of(kind, &tools, &failures, &missing, &chosen[0].summary);
        let title = draft_title(request.title.as_deref(), kind, &chosen[0].summary, context);
        let input = ReportInput {
            notes: &chosen,
            missing: &missing,
            failures: &failures,
            counters: &counters,
            environment,
            client: client_label(&state),
            fingerprint: &fingerprint,
            context,
        };
        let body = scrub(&render_within_limit(&input), context);
        let link = if link_base.is_empty() {
            Err("the project's repository address is unknown".to_string())
        } else {
            build_link(link_base, &title, &body)
        };

        state.next_draft += 1;
        let id = format!("d{}", state.next_draft);
        for note in state.notes.iter_mut().filter(|note| ids.contains(&note.id)) {
            note.drafted = true;
        }
        let draft = Draft {
            id,
            title,
            body,
            fingerprint,
            link,
            note_ids: ids,
            state: DraftState::Pending,
        };
        state.drafts.push(draft.clone());
        Ok(BuiltDraft {
            draft,
            left_out: pending_of(&state),
        })
    }

    pub fn draft_ids(&self) -> Vec<String> {
        lock(&self.state)
            .drafts
            .iter()
            .map(|draft| draft.id.clone())
            .collect()
    }

    /// Claims a draft for submission: `Pending` becomes `Submitting`. A draft that is already
    /// being submitted, or already has a final outcome, is refused.
    pub fn begin_submit(&self, id: &str) -> Result<Draft, SubmitRefusal> {
        let mut state = lock(&self.state);
        let known: Vec<String> = state.drafts.iter().map(|draft| draft.id.clone()).collect();
        let Some(draft) = state.drafts.iter_mut().find(|draft| draft.id == id) else {
            return Err(SubmitRefusal::UnknownDraft { known });
        };
        match &draft.state {
            DraftState::Pending => {
                draft.state = DraftState::Submitting;
                Ok(draft.clone())
            }
            DraftState::Submitting => Err(SubmitRefusal::InProgress),
            DraftState::Submitted(outcome) => Err(SubmitRefusal::AlreadyDone(outcome.clone())),
        }
    }

    /// Stores the result of a submission. Only a created issue, or one whose creation cannot be
    /// confirmed, is final; a duplicate or a failure leaves the draft ready to submit again.
    pub fn finish_submit(&self, id: &str, outcome: Outcome) {
        let mut state = lock(&self.state);
        if let Some(draft) = state
            .drafts
            .iter_mut()
            .find(|draft| draft.id == id && matches!(draft.state, DraftState::Submitting))
        {
            draft.state = match outcome {
                Outcome::Created(_) | Outcome::Unknown(_) => DraftState::Submitted(outcome),
                Outcome::Duplicate(_) | Outcome::NotCreated { .. } => DraftState::Pending,
            };
        }
    }

    /// Returns a draft that was being submitted to `Pending` (the call was cancelled).
    pub fn abort_submit(&self, id: &str) {
        let mut state = lock(&self.state);
        if let Some(draft) = state
            .drafts
            .iter_mut()
            .find(|draft| draft.id == id && matches!(draft.state, DraftState::Submitting))
        {
            draft.state = DraftState::Pending;
        }
    }

    #[cfg(test)]
    fn evidence_for_tests(&self) -> Vec<Evidence> {
        lock(&self.state).evidence.clone()
    }

    #[cfg(test)]
    fn counter_for_tests(&self, tool: &str) -> (u32, u32) {
        lock(&self.state)
            .counters
            .get(tool)
            .copied()
            .unwrap_or((0, 0))
    }

    #[cfg(test)]
    fn panic_while_locked_for_tests(&self) {
        let _guard = lock(&self.state);
        panic!("poisoning the journal on purpose");
    }
}

fn pending_of(state: &State) -> Vec<PendingNote> {
    state
        .notes
        .iter()
        .filter(|note| !note.drafted)
        .map(|note| PendingNote {
            id: note.id.clone(),
            kind: note.kind,
            summary: note.summary.clone(),
        })
        .collect()
}

fn client_label(state: &State) -> String {
    match (&state.client_name, &state.client_version) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name.clone(),
        (None, Some(version)) => version.clone(),
        (None, None) => "unknown".to_string(),
    }
}

fn check_length(field: &str, text: &str, limit: usize, hint: &str) -> Result<(), Refusal> {
    let length = text.chars().count();
    if length > limit {
        return Err(Refusal::new(
            format!("{field} is {length} characters; the limit is {limit}"),
            hint,
        ));
    }
    Ok(())
}

/// `[a-z0-9_]{1,64}`: the shape of a tool name given in a note.
fn is_tool_name(text: &str) -> bool {
    (1..=TOOL_LIMIT).contains(&text.chars().count())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `[A-Za-z0-9_.-]{1,64}`: the shape of a requested tool name worth quoting.
fn is_plain_name(text: &str) -> bool {
    (1..=TOOL_LIMIT).contains(&text.chars().count())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn draft_title(
    requested: Option<&str>,
    kind: Kind,
    first_summary: &str,
    context: &ScrubContext,
) -> String {
    let custom = requested
        .map(|title| one_line(&scrub(title, context)))
        .filter(|title| !title.is_empty());
    let core = custom.unwrap_or_else(|| format!("{}: {first_summary}", kind.as_str()));
    format!("{TITLE_PREFIX}{core}")
        .chars()
        .take(TITLE_LIMIT)
        .collect()
}

/// Derived from facts that stay the same when two people hit the same problem: the kind, the
/// named tools, the shape of the observed failures and, for a missing feature, the names of the
/// tools that were requested. Free wording counts only when none of those exist.
fn fingerprint_of(
    kind: Kind,
    tools: &[String],
    failures: &[Evidence],
    missing: &[Evidence],
    first_summary: &str,
) -> String {
    let mut shapes: Vec<String> = failures.iter().map(|event| shape(&event.message)).collect();
    shapes.sort();
    shapes.dedup();
    let mut missing_names: Vec<&str> = missing.iter().map(|event| event.tool.as_str()).collect();
    missing_names.sort_unstable();
    let fallback = if tools.is_empty() && shapes.is_empty() && missing_names.is_empty() {
        shape(first_summary)
    } else {
        String::new()
    };
    fingerprint(&format!(
        "v1\n{}\n{}\n{}\n{}\n{}",
        kind.as_str(),
        tools.join(","),
        shapes.join("\u{1f}"),
        missing_names.join(","),
        fallback
    ))
}

struct ReportInput<'a> {
    notes: &'a [Note],
    missing: &'a [Evidence],
    failures: &'a [Evidence],
    counters: &'a [(String, (u32, u32))],
    environment: &'a Environment,
    client: String,
    fingerprint: &'a str,
    context: &'a ScrubContext,
}

/// Renders the report; while it is over the size limit, halves the longest note details.
fn render_within_limit(input: &ReportInput<'_>) -> String {
    let mut details: Vec<String> = input
        .notes
        .iter()
        .map(|note| note.details.clone())
        .collect();
    let mut shortened = false;
    loop {
        let body = render_body(input, &details, shortened);
        if body.chars().count() <= REPORT_LIMIT {
            return body;
        }
        let longest = details
            .iter()
            .enumerate()
            .map(|(index, text)| (index, text.chars().count()))
            .max_by_key(|(_, length)| *length);
        match longest {
            Some((index, length)) if length > 0 => {
                details[index] = details[index].chars().take(length / 2).collect();
                shortened = true;
            }
            _ => return body.chars().take(REPORT_LIMIT).collect(),
        }
    }
}

const AGENT: &str = "*(written by the agent)*";
const SERVER: &str = "*(observed by the server)*";

fn render_body(input: &ReportInput<'_>, details: &[String], shortened: bool) -> String {
    let single = input.notes.len() == 1;
    let mut out = String::new();

    out.push_str(&format!("## Summary {AGENT}\n\n"));
    let summaries: Vec<String> = input
        .notes
        .iter()
        .map(|note| {
            if single {
                note.summary.clone()
            } else {
                format!("{}: {}", note.id, note.summary)
            }
        })
        .collect();
    out.push_str(&fenced(&summaries.join("\n")));

    out.push_str(&format!("\n\n## What happened {AGENT}\n\n"));
    if single {
        out.push_str(&fenced(&details[0]));
    } else {
        for (note, text) in input.notes.iter().zip(details) {
            let tool = note
                .tool
                .as_deref()
                .map(|tool| format!(" · {tool}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "### {} · {}{tool}\n\n",
                note.id,
                note.kind.as_str()
            ));
            out.push_str(&fenced(text));
            out.push_str("\n\n");
        }
        while out.ends_with("\n\n") {
            out.pop();
        }
    }

    let suggestions: Vec<String> = input
        .notes
        .iter()
        .filter_map(|note| {
            note.suggestion.as_ref().map(|text| {
                if single {
                    text.clone()
                } else {
                    format!("{}: {text}", note.id)
                }
            })
        })
        .collect();
    if !suggestions.is_empty() {
        out.push_str(&format!("\n\n## Expected / suggestion {AGENT}\n\n"));
        out.push_str(&fenced(&suggestions.join("\n")));
    }

    let impact = input
        .notes
        .iter()
        .filter_map(|note| note.impact)
        .max_by_key(|impact| impact.severity())
        .map_or("not stated", Impact::as_str);
    out.push_str(&format!("\n\n## Impact {AGENT}\n\n{impact}\n"));

    let cell = |value: &str| cell(value, input.context);
    out.push_str(&format!("\n## Environment {SERVER}\n\n"));
    out.push_str("| Item | Value |\n| --- | --- |\n");
    for (label, value) in [
        ("godot-mcp", input.environment.server_version.as_str()),
        ("Addon", input.environment.addon_version.as_str()),
        ("Godot", input.environment.engine_version.as_str()),
        ("Platform", input.environment.platform.as_str()),
        ("Editor bridge", input.environment.bridge_state.as_str()),
        ("MCP client", input.client.as_str()),
        ("Feedback mode", input.environment.feedback_mode.as_str()),
    ] {
        out.push_str(&format!("| {label} | {} |\n", cell(value)));
    }

    out.push_str(&format!("\n## Server-observed context {SERVER}\n\n"));
    let mut observed = false;
    if !input.missing.is_empty() {
        observed = true;
        out.push_str("Requested tools that do not exist:\n\n");
        for event in input.missing {
            out.push_str(&format!(
                "- `{}`: requested {} time{}\n",
                event.tool,
                event.count,
                plural(event.count)
            ));
        }
        out.push('\n');
    }
    if !input.failures.is_empty() {
        observed = true;
        out.push_str("Recent failures of the tools named in the notes:\n\n");
        let lines: Vec<String> = input
            .failures
            .iter()
            .map(|event| {
                let times = if event.count > 1 {
                    format!(" ×{}", event.count)
                } else {
                    String::new()
                };
                format!(
                    "{} [{}{times}]: {}",
                    event.tool,
                    event.kind.label(),
                    event.message
                )
            })
            .collect();
        out.push_str(&fenced(&lines.join("\n")));
        out.push_str("\n\n");
    }
    if !input.counters.is_empty() {
        observed = true;
        out.push_str("Calls in this session:\n\n");
        for (tool, (calls, failures)) in input.counters {
            out.push_str(&format!(
                "- {tool}: {calls} call{}, {failures} failure{}\n",
                plural(*calls),
                plural(*failures)
            ));
        }
        out.push('\n');
    }
    if !observed {
        out.push_str(
            "No related failures or requests for missing tools were observed in this session.\n\n",
        );
    }

    out.push_str("## Report details\n\n");
    out.push_str(&format!("- Fingerprint: {}\n", input.fingerprint));
    out.push_str("- Format: godot-mcp feedback report v1\n");
    out.push_str("- Drafted by an AI agent through the godot-mcp feedback channel.\n");
    if shortened {
        out.push_str(&format!("- {SHORTENED_NOTE}\n"));
    }
    out
}

fn plural(count: u32) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// One table cell: sanitized free text on a single line, with the pipe escaped.
fn cell(value: &str, context: &ScrubContext) -> String {
    one_line(&scrub(value, context))
        .replace('|', "\\|")
        .chars()
        .take(CELL_LIMIT)
        .collect()
}

/// Puts text in a fenced code block whose fence is longer than any run of backticks inside it,
/// so the text can neither close the block nor pass for a heading of the report.
fn fenced(text: &str) -> String {
    fenced_as(text, "text")
}

/// Same, with the given language tag after the opening fence.
pub(crate) fn fenced_as(text: &str, language: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{LINK_BUDGET, ScrubContext, scrub};

    const LINK_BASE: &str = "https://github.com/o/r/issues/new";

    fn ctx() -> ScrubContext {
        ScrubContext::default()
    }

    fn environment() -> Environment {
        Environment {
            server_version: "0.3.0".into(),
            addon_version: "0.3.0".into(),
            engine_version: "4.5.stable".into(),
            platform: "linux x86_64".into(),
            bridge_state: "connected".into(),
            feedback_mode: "full".into(),
        }
    }

    fn note(kind: Kind, summary: &str) -> NewNote {
        NewNote {
            kind,
            summary: summary.into(),
            details: "Tried it and it failed.".into(),
            tool: None,
            impact: None,
            suggestion: None,
        }
    }

    fn add(journal: &Journal, kind: Kind, summary: &str) -> String {
        journal
            .add_note(note(kind, summary), &ctx())
            .expect("note")
            .0
    }

    fn draft(journal: &Journal) -> Draft {
        journal
            .build_draft(DraftRequest::default(), &environment(), LINK_BASE, &ctx())
            .expect("draft")
            .draft
    }

    fn request(note_ids: &[&str]) -> DraftRequest {
        DraftRequest {
            note_ids: Some(note_ids.iter().map(|id| id.to_string()).collect()),
            title: None,
        }
    }

    #[test]
    fn notes_get_sequential_ids_and_are_listed_as_pending() {
        let journal = Journal::new();
        let (first, _) = journal
            .add_note(note(Kind::Bug, "one"), &ctx())
            .expect("note");
        let (second, pending) = journal
            .add_note(note(Kind::Docs, "two"), &ctx())
            .expect("note");
        assert_eq!((first.as_str(), second.as_str()), ("n1", "n2"));
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[1].kind, Kind::Docs);
        assert_eq!(pending[1].summary, "two");
    }

    #[test]
    fn the_twenty_first_note_is_refused_naming_the_limit() {
        let journal = Journal::new();
        for index in 0..MAX_NOTES {
            add(&journal, Kind::Bug, &format!("note {index}"));
        }
        let refusal = journal
            .add_note(note(Kind::Bug, "one too many"), &ctx())
            .expect_err("limit");
        assert!(refusal.message.contains("20"), "{}", refusal.message);
        assert!(!refusal.hint.is_empty());
    }

    #[test]
    fn every_text_field_has_a_named_limit() {
        let journal = Journal::new();
        let mut long_summary = note(Kind::Bug, &"s".repeat(SUMMARY_LIMIT + 1));
        let refusal = journal
            .add_note(long_summary.clone(), &ctx())
            .expect_err("summary");
        assert!(refusal.message.contains("summary") && refusal.message.contains("120"));
        long_summary.summary = "s".repeat(SUMMARY_LIMIT);
        assert!(journal.add_note(long_summary, &ctx()).is_ok());

        let mut long_details = note(Kind::Bug, "ok");
        long_details.details = "d".repeat(DETAILS_LIMIT + 1);
        let refusal = journal.add_note(long_details, &ctx()).expect_err("details");
        assert!(refusal.message.contains("details") && refusal.message.contains("2000"));

        let mut long_suggestion = note(Kind::Bug, "ok");
        long_suggestion.suggestion = Some("g".repeat(SUGGESTION_LIMIT + 1));
        let refusal = journal
            .add_note(long_suggestion, &ctx())
            .expect_err("suggestion");
        assert!(refusal.message.contains("suggestion") && refusal.message.contains("500"));
    }

    #[test]
    fn empty_summary_or_details_and_odd_tool_names_are_refused() {
        let journal = Journal::new();
        assert!(journal.add_note(note(Kind::Bug, "   "), &ctx()).is_err());
        let mut no_details = note(Kind::Bug, "ok");
        no_details.details = "  ".into();
        assert!(journal.add_note(no_details, &ctx()).is_err());
        let too_long = "t".repeat(TOOL_LIMIT + 1);
        for bad in [
            "Godot_Add",
            "has space",
            "semi;colon",
            "",
            too_long.as_str(),
        ] {
            let mut with_tool = note(Kind::Bug, "ok");
            with_tool.tool = Some(bad.to_string());
            let refusal = journal.add_note(with_tool, &ctx()).expect_err(bad);
            assert!(
                refusal.message.contains("tool"),
                "{bad}: {}",
                refusal.message
            );
        }
        let mut good = note(Kind::Bug, "ok");
        good.tool = Some("godot_add_node".into());
        assert!(journal.add_note(good, &ctx()).is_ok());
    }

    #[test]
    fn text_is_scrubbed_before_it_is_stored_and_summaries_stay_on_one_line() {
        let journal = Journal::new();
        let secret = ScrubContext {
            secret: Some("hunter2hunter2".into()),
            ..ScrubContext::default()
        };
        let mut sensitive = note(Kind::Bug, "first line\nsecond hunter2hunter2");
        sensitive.details = "see @maintainer and /home/bob/game/a.gd".into();
        let (_, pending) = journal.add_note(sensitive, &secret).expect("note");
        assert_eq!(pending[0].summary, "first line second {redacted}");
        let body = draft(&journal).body;
        assert!(body.contains("@\u{200B}maintainer"), "{body}");
        assert!(body.contains("{path}") && !body.contains("bob"), "{body}");
    }

    #[test]
    fn the_same_failure_is_stored_once_with_a_count() {
        let journal = Journal::new();
        for _ in 0..3 {
            journal.record_evidence(
                EvidenceKind::Error,
                "godot_add_node",
                "Node not found: A",
                &ctx(),
            );
        }
        journal.record_evidence(
            EvidenceKind::Error,
            "godot_add_node",
            "Other problem",
            &ctx(),
        );
        let events = journal.evidence_for_tests();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].count, 3);
    }

    #[test]
    fn evidence_beyond_the_limit_replaces_the_oldest_and_messages_are_cut() {
        let journal = Journal::new();
        for index in 0..=MAX_EVIDENCE {
            journal.record_evidence(
                EvidenceKind::Error,
                "godot_x",
                &format!("failure number {index}"),
                &ctx(),
            );
        }
        let events = journal.evidence_for_tests();
        assert_eq!(events.len(), MAX_EVIDENCE);
        assert!(
            events
                .iter()
                .all(|event| event.message != "failure number 0")
        );

        let journal = Journal::new();
        journal.record_evidence(
            EvidenceKind::Error,
            "godot_x",
            &format!("first line {}\nsecond line", "z".repeat(500)),
            &ctx(),
        );
        let event = &journal.evidence_for_tests()[0];
        assert_eq!(event.message.chars().count(), EVIDENCE_MESSAGE_LIMIT);
        assert!(!event.message.contains("second"));
    }

    #[test]
    fn an_unknown_tool_name_that_is_not_a_plain_name_is_not_stored_verbatim() {
        let journal = Journal::new();
        journal.record_evidence(EvidenceKind::UnknownTool, "ok.name-1_x", "", &ctx());
        journal.record_evidence(
            EvidenceKind::UnknownTool,
            "bad`name\n## Heading",
            "",
            &ctx(),
        );
        let names: Vec<String> = journal
            .evidence_for_tests()
            .into_iter()
            .map(|event| event.tool)
            .collect();
        assert_eq!(names, ["ok.name-1_x", "<invalid name>"]);
    }

    #[test]
    fn calls_and_failures_are_counted_per_tool() {
        let journal = Journal::new();
        journal.record_call("godot_add_node", false);
        journal.record_call("godot_add_node", true);
        journal.record_call("godot_add_node", true);
        journal.record_call("godot_get_version", false);
        assert_eq!(journal.counter_for_tests("godot_add_node"), (3, 2));
        assert_eq!(journal.counter_for_tests("godot_get_version"), (1, 0));
        assert_eq!(journal.counter_for_tests("never_called"), (0, 0));
    }

    #[test]
    fn a_draft_needs_at_least_one_note() {
        let journal = Journal::new();
        let refusal = journal
            .build_draft(DraftRequest::default(), &environment(), LINK_BASE, &ctx())
            .expect_err("nothing to draft");
        assert!(
            refusal.hint.contains("godot_note_feedback"),
            "{}",
            refusal.hint
        );
    }

    #[test]
    fn a_draft_takes_every_undrafted_note_by_default_and_marks_them() {
        let journal = Journal::new();
        add(&journal, Kind::Docs, "readme");
        add(&journal, Kind::Bug, "crash");
        let built = journal
            .build_draft(DraftRequest::default(), &environment(), LINK_BASE, &ctx())
            .expect("draft");
        assert_eq!(built.draft.id, "d1");
        assert_eq!(built.draft.note_ids, ["n1", "n2"]);
        assert!(built.left_out.is_empty());
        assert!(journal.pending_notes().is_empty());
    }

    #[test]
    fn explicit_notes_are_drafted_and_the_rest_are_reported_as_left_out() {
        let journal = Journal::new();
        add(&journal, Kind::Docs, "readme");
        add(&journal, Kind::Bug, "crash");
        let built = journal
            .build_draft(request(&["n2"]), &environment(), LINK_BASE, &ctx())
            .expect("draft");
        assert_eq!(built.draft.note_ids, ["n2"]);
        assert_eq!(built.left_out.len(), 1);
        assert_eq!(built.left_out[0].id, "n1");
        // An already drafted note may be selected again.
        let again = journal
            .build_draft(request(&["n2"]), &environment(), LINK_BASE, &ctx())
            .expect("redraft");
        assert_eq!(again.draft.id, "d2");
    }

    #[test]
    fn unknown_and_empty_selections_are_refused_with_the_known_ids() {
        let journal = Journal::new();
        add(&journal, Kind::Bug, "crash");
        let refusal = journal
            .build_draft(request(&["n9"]), &environment(), LINK_BASE, &ctx())
            .expect_err("unknown id");
        assert!(refusal.message.contains("n9") && refusal.hint.contains("n1"));
        let refusal = journal
            .build_draft(request(&[]), &environment(), LINK_BASE, &ctx())
            .expect_err("empty selection");
        assert!(refusal.message.contains("note_ids"));
    }

    #[test]
    fn the_eleventh_draft_is_refused_naming_the_limit() {
        let journal = Journal::new();
        add(&journal, Kind::Bug, "crash");
        for _ in 0..MAX_DRAFTS {
            journal
                .build_draft(request(&["n1"]), &environment(), LINK_BASE, &ctx())
                .expect("draft within the limit");
        }
        let refusal = journal
            .build_draft(DraftRequest::default(), &environment(), LINK_BASE, &ctx())
            .expect_err("limit");
        assert!(refusal.message.contains("10"), "{}", refusal.message);
    }

    #[test]
    fn the_title_is_prefixed_single_line_scrubbed_and_shortened() {
        let journal = Journal::new();
        add(&journal, Kind::MissingFeature, "no scene creation");
        assert_eq!(
            draft(&journal).title,
            "[agent-feedback] missing_feature: no scene creation"
        );

        let titled = |title: &str| {
            let journal = Journal::new();
            add(&journal, Kind::Bug, "x");
            journal
                .build_draft(
                    DraftRequest {
                        note_ids: None,
                        title: Some(title.into()),
                    },
                    &environment(),
                    LINK_BASE,
                    &ctx(),
                )
                .expect("draft")
                .draft
                .title
        };
        assert_eq!(
            titled("Custom\ntitle @bob"),
            "[agent-feedback] Custom title @\u{200B}bob"
        );
        let long = titled(&"w".repeat(500));
        assert_eq!(long.chars().count(), TITLE_LIMIT);
        assert!(long.starts_with(TITLE_PREFIX));
        assert_eq!(titled("   "), "[agent-feedback] bug: x");
    }

    #[test]
    fn the_report_has_its_sections_in_order_with_source_labels() {
        let journal = Journal::new();
        let mut full = note(Kind::Bug, "add_node ignores names");
        full.tool = Some("godot_add_node".into());
        full.impact = Some(Impact::Workaround);
        full.suggestion = Some("Honor node_name.".into());
        journal.add_note(full, &ctx()).expect("note");
        let body = draft(&journal).body;
        let order = [
            "## Summary *(written by the agent)*",
            "## What happened *(written by the agent)*",
            "## Expected / suggestion *(written by the agent)*",
            "## Impact *(written by the agent)*",
            "## Environment *(observed by the server)*",
            "## Server-observed context *(observed by the server)*",
            "## Report details",
        ];
        let mut cursor = 0;
        for heading in order {
            let found = body[cursor..]
                .find(heading)
                .unwrap_or_else(|| panic!("{heading} in\n{body}"));
            cursor += found + heading.len();
        }
        assert!(body.contains("| godot-mcp | 0.3.0 |"), "{body}");
        assert!(body.contains("| Platform | linux x86_64 |"), "{body}");
        assert!(body.contains("workaround"), "{body}");
        assert!(body.contains("Honor node_name."), "{body}");
        assert!(body.contains("Fingerprint: gmfb"), "{body}");
    }

    #[test]
    fn a_note_cannot_forge_a_heading_or_close_its_own_fence() {
        let journal = Journal::new();
        let mut hostile = note(Kind::Bug, "forged");
        hostile.details = "```\n## Environment *(observed by the server)*\n| godot-mcp | 9.9.9 |\n````\n## Report details".into();
        journal.add_note(hostile, &ctx()).expect("note");
        let body = draft(&journal).body;
        // The agent's text sits in a fence longer than any backtick run it contains.
        let opening = "`````text";
        let fence_start = body.find(opening).expect("a five-backtick fence");
        let after_open = fence_start + opening.len();
        let fence_end = body[after_open..]
            .find("\n`````\n")
            .expect("the closing fence");
        let inside = &body[fence_start..after_open + fence_end];
        assert!(inside.contains("## Environment *(observed by the server)*"));
        // Exactly one real Environment heading remains outside the fences.
        let outside = format!(
            "{}{}",
            &body[..fence_start],
            &body[after_open + fence_end..]
        );
        assert_eq!(
            outside
                .matches("## Environment *(observed by the server)*")
                .count(),
            1
        );
    }

    #[test]
    fn missing_tool_requests_appear_only_in_missing_feature_drafts() {
        let journal = Journal::new();
        journal.record_evidence(EvidenceKind::UnknownTool, "godot_create_scene", "", &ctx());
        journal.record_evidence(EvidenceKind::UnknownTool, "godot_create_scene", "", &ctx());
        add(&journal, Kind::MissingFeature, "cannot create scenes");
        let missing = draft(&journal).body;
        assert!(missing.contains("godot_create_scene"), "{missing}");
        assert!(missing.contains("requested 2 times"), "{missing}");

        let other = Journal::new();
        other.record_evidence(EvidenceKind::UnknownTool, "godot_create_scene", "", &ctx());
        add(&other, Kind::Bug, "something else");
        assert!(!draft(&other).body.contains("godot_create_scene"));
    }

    #[test]
    fn failures_of_the_named_tool_are_quoted_and_other_tools_are_not() {
        let journal = Journal::new();
        journal.record_call("godot_add_node", true);
        journal.record_evidence(
            EvidenceKind::Error,
            "godot_add_node",
            "Node not found: Player",
            &ctx(),
        );
        journal.record_evidence(
            EvidenceKind::Error,
            "godot_open_scene",
            "Scene missing",
            &ctx(),
        );
        let mut named = note(Kind::Bug, "add_node fails");
        named.tool = Some("godot_add_node".into());
        journal.add_note(named, &ctx()).expect("note");
        let body = draft(&journal).body;
        assert!(body.contains("Node not found: Player"), "{body}");
        assert!(!body.contains("Scene missing"), "{body}");
        assert!(body.contains("godot_add_node: 1 call"), "{body}");
    }

    #[test]
    fn a_body_over_the_limit_is_shortened_by_halving_the_longest_details() {
        let journal = Journal::new();
        for index in 0..12 {
            let mut big = note(Kind::Bug, &format!("note {index}"));
            big.details = "x".repeat(DETAILS_LIMIT);
            journal.add_note(big, &ctx()).expect("note");
        }
        let body = draft(&journal).body;
        assert!(
            body.chars().count() <= REPORT_LIMIT,
            "{}",
            body.chars().count()
        );
        assert!(body.contains("(details shortened)"));
        assert!(body.contains("note 11"), "every note keeps its summary");
    }

    #[test]
    fn the_environment_table_cannot_be_broken_by_the_values() {
        let journal = Journal::new();
        journal.set_client(Some("My|Client\nname"), Some("1.0"), &ctx());
        add(&journal, Kind::Bug, "x");
        let body = draft(&journal).body;
        assert!(
            body.contains("| MCP client | My\\|Client name 1.0 |"),
            "{body}"
        );
    }

    #[test]
    fn the_finished_body_holds_no_secret_and_scrubbing_it_again_changes_nothing() {
        let journal = Journal::new();
        let context = ScrubContext {
            secret: Some("topsecretvalue".into()),
            project_dir: Some("/home/alice/game".into()),
            home_dir: Some("/home/alice".into()),
        };
        journal.set_client(Some("client topsecretvalue"), None, &ctx());
        let mut sensitive = note(Kind::Bug, "x");
        sensitive.details = "path /home/alice/game/a.gd".into();
        journal.add_note(sensitive, &ctx()).expect("note");
        let built = journal
            .build_draft(DraftRequest::default(), &environment(), LINK_BASE, &context)
            .expect("draft");
        assert!(!built.draft.body.contains("topsecretvalue"));
        assert!(!built.draft.body.contains("alice"));
        assert_eq!(scrub(&built.draft.body, &context), built.draft.body);
    }

    #[test]
    fn the_draft_carries_a_link_within_the_budget() {
        let journal = Journal::new();
        add(&journal, Kind::Bug, "x");
        let link = draft(&journal).link.expect("link");
        assert!(link.starts_with(LINK_BASE) && link.len() <= LINK_BUDGET);
    }

    #[test]
    fn the_fingerprint_follows_stable_facts_not_wording() {
        let fingerprint_of = |kind: Kind, tool: &str, summary: &str, message: &str| {
            let journal = Journal::new();
            journal.record_evidence(EvidenceKind::Error, tool, message, &ctx());
            let mut named = note(kind, summary);
            named.tool = Some(tool.into());
            journal.add_note(named, &ctx()).expect("note");
            draft(&journal).fingerprint
        };
        let first = fingerprint_of(
            Kind::Bug,
            "godot_add_node",
            "wording one",
            "Node 12 not found in /a/b",
        );
        let second = fingerprint_of(
            Kind::Bug,
            "godot_add_node",
            "other words",
            "node 99 not found in /c/d",
        );
        assert_eq!(first, second);
        let other_kind = fingerprint_of(
            Kind::Docs,
            "godot_add_node",
            "wording one",
            "Node 12 not found in /a/b",
        );
        let other_tool = fingerprint_of(
            Kind::Bug,
            "godot_remove_node",
            "wording one",
            "Node 12 not found in /a/b",
        );
        let other_error = fingerprint_of(
            Kind::Bug,
            "godot_add_node",
            "wording one",
            "a different error",
        );
        assert_ne!(first, other_kind);
        assert_ne!(first, other_tool);
        assert_ne!(first, other_error);
    }

    #[test]
    fn with_no_tool_or_evidence_the_fingerprint_falls_back_to_the_summary_words() {
        let a = Journal::new();
        add(&a, Kind::Improvement, "Better  error MESSAGES");
        let b = Journal::new();
        add(&b, Kind::Improvement, "better error messages");
        let c = Journal::new();
        add(&c, Kind::Improvement, "something else entirely");
        let (from_a, from_b, from_c) = (
            draft(&a).fingerprint,
            draft(&b).fingerprint,
            draft(&c).fingerprint,
        );
        assert_eq!(from_a, from_b);
        assert_ne!(from_a, from_c);
    }

    #[test]
    fn a_missing_feature_fingerprint_uses_the_requested_tool_names() {
        let build = |tool: &str| {
            let journal = Journal::new();
            journal.record_evidence(EvidenceKind::UnknownTool, tool, "", &ctx());
            add(&journal, Kind::MissingFeature, "wording does not matter");
            draft(&journal).fingerprint
        };
        assert_eq!(build("godot_create_scene"), build("godot_create_scene"));
        assert_ne!(build("godot_create_scene"), build("godot_get_debug_log"));
    }

    #[test]
    fn a_draft_moves_through_its_states() {
        let journal = Journal::new();
        add(&journal, Kind::Bug, "x");
        let id = draft(&journal).id;

        let snapshot = journal.begin_submit(&id).expect("pending to submitting");
        assert_eq!(snapshot.id, id);
        assert!(matches!(
            journal.begin_submit(&id),
            Err(SubmitRefusal::InProgress)
        ));

        journal.finish_submit(
            &id,
            Outcome::NotCreated {
                reason: "offline".into(),
                steps: vec![],
            },
        );
        assert!(
            journal.begin_submit(&id).is_ok(),
            "a failed submission can be retried"
        );

        journal.finish_submit(
            &id,
            Outcome::Duplicate("https://github.com/o/r/issues/1".into()),
        );
        assert!(
            journal.begin_submit(&id).is_ok(),
            "a duplicate can be retried with allow_duplicate"
        );

        journal.abort_submit(&id);
        assert!(
            journal.begin_submit(&id).is_ok(),
            "a cancelled call leaves the draft retriable"
        );

        journal.finish_submit(
            &id,
            Outcome::Created("https://github.com/o/r/issues/2".into()),
        );
        match journal.begin_submit(&id) {
            Err(SubmitRefusal::AlreadyDone(Outcome::Created(address))) => {
                assert!(address.ends_with("/2"));
            }
            other => panic!("expected the stored outcome, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_draft_lists_the_known_ones() {
        let journal = Journal::new();
        add(&journal, Kind::Bug, "x");
        draft(&journal);
        match journal.begin_submit("d7") {
            Err(SubmitRefusal::UnknownDraft { known }) => assert_eq!(known, ["d1"]),
            other => panic!("expected an unknown draft, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_outcome_is_stored_so_a_repeat_cannot_create_twice() {
        let journal = Journal::new();
        add(&journal, Kind::Bug, "x");
        let id = draft(&journal).id;
        journal.begin_submit(&id).expect("submitting");
        journal.finish_submit(&id, Outcome::Unknown("no address printed".into()));
        assert!(matches!(
            journal.begin_submit(&id),
            Err(SubmitRefusal::AlreadyDone(Outcome::Unknown(_)))
        ));
    }

    #[test]
    fn the_journal_survives_a_panic_in_another_task() {
        let journal = std::sync::Arc::new(Journal::new());
        let poisoner = journal.clone();
        let _ = std::thread::spawn(move || poisoner.panic_while_locked_for_tests()).join();
        assert!(
            journal
                .add_note(note(Kind::Bug, "still works"), &ctx())
                .is_ok()
        );
    }
}
