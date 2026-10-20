# Implementation Plan

**Version:** 1.2.0
**Generated:** 2026-10-20
**Based on:** .design/main/INDEX.md v1.3.0
**Status:** Active

## Overview

The registry was bootstrapped from existing code (godot-mcp 0.3.0 plus unreleased changes at commit 94e4540), so Phases 0 and 1 describe what is already implemented and verified. Phase 2 closes the gaps the first analysis found: a shipped-artifact hygiene gap, a stale comment, an unverified security property, and contributor guidance for the specification-driven workflow. Phase 3 adds the agent feedback channel: tools that let an agent record what is missing or broken and hand the maintainers a scrubbed report as text or as an approved issue submission. Phase 4 splits the product into one release line per Godot minor version (4.5, 4.6, 4.7) with the development line on `master` targeting Godot 4.8, rewrites the release procedure around carrying changes between lines and releasing each line on its own, and fills the security policy and contributor guide. Roadmap concepts wait in the Backlog until their open questions are answered.

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

## Backlog

<!-- Registered specifications waiting for prioritization (Draft or non-critical Stable) -->
- [l1-runtime-observability.md](specifications/l1-runtime-observability.md) *(Parked — ownership of runtime access undecided; see its TBD markers)*
- [l1-content-authoring.md](specifications/l1-content-authoring.md) *(Parked — update format and headless support undecided; see its TBD markers)*
