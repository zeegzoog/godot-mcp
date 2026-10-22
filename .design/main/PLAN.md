# Implementation Plan

**Version:** 1.7.1
**Generated:** 2026-10-21
**Based on:** .design/main/INDEX.md v1.7.0
**Status:** Active

## Overview

The registry was bootstrapped from existing code (godot-mcp 0.3.0 plus unreleased changes at commit 94e4540), so Phases 0 and 1 describe what is already implemented and verified. Phase 2 closes the gaps the first analysis found: a shipped-artifact hygiene gap, a stale comment, an unverified security property, and contributor guidance for the specification-driven workflow. Phase 3 adds the agent feedback channel: tools that let an agent record what is missing or broken and hand the maintainers a scrubbed report as text or as an approved issue submission. Phase 4 splits the product into one release line per Godot minor version (4.5, 4.6, 4.7) with the development line on `master` targeting Godot 4.8, rewrites the release procedure around carrying changes between lines and releasing each line on its own, and fills the security policy and contributor guide. Phase 5 adds repository stewardship: a portable agent skill that triages, plans, fixes, answers, reviews, merges and closes issues and pull requests within autonomy limits, keeps a Kanban board, and announces when a release is due and what it holds for each Godot line, installed in this repository with its profile, workflows, forms and guide changes. Phase 6 records the series of contributor pull requests that gave the server a running-game channel, a debugger client, a language-server client and the authoring tools, and the specifications written for them. Phase 7 adds controlled time: holding a running game still at the start of a physics tick and stepping it by exact or conditional ticks over the existing debugger channel, specified before the code. Phase 8 adds script reading and fragment editing to the authoring tools, through the editor when it is connected and through the file when it is not, after a feasibility check of the script editor on a real engine. Phase 9 brings the product in line with the eleven specifications amended from the simulation QA of 2026-10-21: the named project wins over the editor, the running-game guards keep their promise, the file fallbacks accept what the editor accepts, and answers, hints and defaults say what happened. Roadmap concepts wait in the Backlog until their open questions are answered.

## Phase 0 — Requirements (Layer 1: Concept)

*Abstract specifications — technology-agnostic contracts. Implemented before the registry existed.*

- [x] **MCP Server Core** ([l1-mcp-server.md](specifications/l1-mcp-server.md)) [L1]
- [x] **Live Editor Bridge** ([l1-editor-bridge.md](specifications/l1-editor-bridge.md)) [L1]
- [x] **Tool Catalog** ([l1-tool-catalog.md](specifications/l1-tool-catalog.md)) [L1]
- [x] **Quality Assurance** ([l1-quality-assurance.md](specifications/l1-quality-assurance.md)) [L1]
- [x] **Release & Distribution** ([l1-distribution.md](specifications/l1-distribution.md)) [L1]

## Phase 1 — Baseline Implementation (Layer 2)

*Reverse-engineered from the code; compliance tables verified against the source.*

- [x] **Rust Stdio Server** ([l2-rust-stdio-server.md](specifications/l2-rust-stdio-server.md)) [L2]
- [x] **Bridge Client (Rust)** ([l2-bridge-client-rust.md](specifications/l2-bridge-client-rust.md)) [L2]
- [x] **Tool Handlers (Rust)** ([l2-tool-handlers.md](specifications/l2-tool-handlers.md)) [L2]
- [x] **Godot Editor Addon** ([l2-godot-editor-addon.md](specifications/l2-godot-editor-addon.md)) [L2]
- [x] **CI & Release Pipeline** ([l2-ci-release-pipeline.md](specifications/l2-ci-release-pipeline.md)) [L2]
- [x] **Test Suite** ([l2-test-suite.md](specifications/l2-test-suite.md)) [L2]

## Phase 2 — Alignment & Hardening

*Closes the gaps recorded by the first analysis. See [TASKS.md](TASKS.md).*

- [x] Keep the planning directory out of source archives (DS-8)
- [x] Prove the token file is owner-only on Unix (BR-2)
- [x] Correct the stale class_name note in the development project
- [x] ~~Document the specification-driven workflow for contributors~~ — moved to the contributor guide (Phase 4)

## Phase 3 — Agent Feedback Channel (Layers 1 and 2)

*Specified together: the concept first, then its Rust realization. See [TASKS.md](TASKS.md).*

- [x] **Agent Feedback Channel** ([l1-agent-feedback.md](specifications/l1-agent-feedback.md)) [L1]
- [x] **Agent Feedback Channel (Rust)** ([l2-agent-feedback-rust.md](specifications/l2-agent-feedback-rust.md)) [L2]
- [x] Record, scrub, draft and submit with the human's approval; documented for users and covered by tests

## Phase 4 — Godot Release Lines & Community Health (Layers 1 and 2)

*One release line per Godot minor version, the line-aware pipeline and release procedure, the security policy and the contributor guide. Requires Phase 2 (the cut inherits its fixes). See [TASKS.md](TASKS.md).*

- [x] **Godot Release Lines** ([l1-release-lines.md](specifications/l1-release-lines.md)) [L1]
- [x] **Community Health Files** ([l1-community-health.md](specifications/l1-community-health.md)) [L1]
- [x] **Release Branches & Backports** ([l2-release-branches.md](specifications/l2-release-branches.md)) [L2]
- [x] **Community Health Files (GitHub)** ([l2-community-health-files.md](specifications/l2-community-health-files.md)) [L2]
- [x] Amendments: Release & Distribution 1.1.0, Quality Assurance 1.1.0, CI & Release Pipeline 1.1.0, Test Suite 1.2.0, Godot Editor Addon 1.1.0
- [x] Gated on approval: repository settings, publishing the branches, the first line releases

## Phase 5 — Repository Stewardship (Layers 1 and 2)

*The stewardship concept first, then the portable skill and its installation here. Requires Phase 4 (release lines, release skill, contributor guide). See [TASKS.md](TASKS.md).*

- [x] **Issue & Pull Request Stewardship** ([l1-repository-stewardship.md](specifications/l1-repository-stewardship.md)) [L1]
- [x] **Repo Steward Skill (GitHub)** ([l2-repo-steward-skill.md](specifications/l2-repo-steward-skill.md)) [L2]
- [x] **Repo Steward Procedures (GitHub)** ([l2-repo-steward-procedures.md](specifications/l2-repo-steward-procedures.md)) [L2]
- [x] **Repo Steward for godot-mcp** ([l2-repo-steward-godot-mcp.md](specifications/l2-repo-steward-godot-mcp.md)) [L2]
- [x] Amendments: Community Health Files 1.1.0 (concept and GitHub), Test Suite 1.3.0
- [x] Gated on approval: pushing to `master`, labels and the board on GitHub, the first acting run; maintainer-only settings

## Phase 6 — Game Runtime, Authoring & Code Intelligence (Layers 1 and 2)

*Specified after the code merged; see [TASKS.md](TASKS.md).*

- [x] **Runtime Observability** ([l1-runtime-observability.md](specifications/l1-runtime-observability.md)) [L1]
- [x] **Code Intelligence** ([l1-code-intelligence.md](specifications/l1-code-intelligence.md)) [L1]
- [x] **Content Authoring** ([l1-content-authoring.md](specifications/l1-content-authoring.md)) [L1]
- [x] **Game Runtime Channel** ([l2-game-runtime.md](specifications/l2-game-runtime.md)) [L2]
- [x] **GDScript Language Server Client** ([l2-lsp-client.md](specifications/l2-lsp-client.md)) [L2]
- [x] Amendments: Tool Catalog 1.2.0, Tool Handlers 1.2.0, Godot Editor Addon 1.2.0, Test Suite 1.4.0

## Phase 7 — Controlled Time (Layers 1 and 2)

*Specified first; a feasibility test on a real game gates the rest. Requires Phase 6 (the runtime channel). See [TASKS.md](TASKS.md).*

- [x] **Controlled Time** ([l1-controlled-time.md](specifications/l1-controlled-time.md)) [L1]
- [x] **Held Stepping** ([l2-held-stepping.md](specifications/l2-held-stepping.md)) [L2]
- [x] Carried to `godot-v4.7`, `godot-v4.6`, `godot-v4.5` (one local commit each); pushing gated on approval

## Phase 8 — Script Reading and Editing (Layers 1 and 2)

*A feasibility check on a real engine comes first and gates the rest. Requires Phase 6 (the authoring tools). See [TASKS.md](TASKS.md).*

- [x] **Content Authoring** 1.1.0, CA-7 to CA-10 ([l1-content-authoring.md](specifications/l1-content-authoring.md)) [L1]
- [x] Specification notes for the two commands and the two tools in **Godot Editor Addon** ([l2-godot-editor-addon.md](specifications/l2-godot-editor-addon.md)) and **Tool Handlers** ([l2-tool-handlers.md](specifications/l2-tool-handlers.md)) [L2]
- [x] Editor handlers, host tools with a file fallback, user-facing text, validation
- [x] Carried to `godot-v4.7`, `godot-v4.6`, `godot-v4.5` (three local commits each); pushing gated on approval

## Phase 9 — Simulation QA Remediation (Layers 1 and 2)

*The specifications were amended first (all Stable); this phase makes the code match them. Requires Phase 6 (runtime, authoring, code intelligence) and Phase 8 (script tools). Decided in the specifications: no refusal of `@tool` edits, node names are data (TL-5), the fallback class check is one engine call. See [TASKS.md](TASKS.md).*

- [x] **Tool Catalog** 1.6.0, TL-5, TL-10 and TL-11 ([l1-tool-catalog.md](specifications/l1-tool-catalog.md)) [L1]
- [x] **Runtime Observability** 1.2.0, RO-3, RO-6, RO-11 ([l1-runtime-observability.md](specifications/l1-runtime-observability.md)) [L1]
- [x] **Content Authoring** 1.2.1, CA-3, CA-11 ([l1-content-authoring.md](specifications/l1-content-authoring.md)) [L1]
- [x] **Code Intelligence** 1.1.0, CI-7 ([l1-code-intelligence.md](specifications/l1-code-intelligence.md)) [L1]
- [x] **Agent Feedback** 1.1.0, FB-10 ([l1-agent-feedback.md](specifications/l1-agent-feedback.md)) [L1]
- [x] Host: **Tool Handlers** 1.5.0, **LSP Client** 1.2.1, **Agent Feedback (Rust)** 1.1.0, **Rust Stdio Server** 1.3.0 [L2]
- [x] Editor and runtime: **Godot Editor Addon** 1.6.0, **Game Runtime Channel** 1.4.0 [L2]
- [x] User-facing text and validation
- [x] Carried to `godot-v4.7`, `godot-v4.6`, `godot-v4.5` (one local commit each); pushing gated on approval

## Backlog

<!-- Registered specifications waiting for prioritization (Draft or non-critical Stable) -->
