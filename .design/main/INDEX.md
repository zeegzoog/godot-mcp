# Workspace Specifications Registry

**Version:** 1.4.0
**Status:** Active

## Overview

Local registry of specifications for this workspace.

## Domain Specifications

| File | Description | Status | Layer | Version |
| --- | --- | --- | --- | --- |
| [l1-mcp-server.md](specifications/l1-mcp-server.md) | What the server guarantees to an MCP client: framing, concurrency, cancellation, error contract and configuration. Open it before changing how requests are read, answered or configured. | Stable | concept | 1.1.1 |
| [l1-editor-bridge.md](specifications/l1-editor-bridge.md) | Guarantees of the server-to-editor channel: loopback, shared secret, one reply per command, undoable and scene-confined mutations. Open it before changing either side of the bridge. | Stable | concept | 1.0.0 |
| [l1-tool-catalog.md](specifications/l1-tool-catalog.md) | The tool set and the rules every tool follows: schema validation, live-first fallback, project resolution, write and launch safety. Open it before adding or changing a tool. | Stable | concept | 1.1.0 |
| [l1-distribution.md](specifications/l1-distribution.md) | What a release contains and the conditions for publishing one. Open it before changing versions, release notes, packaging or ignore rules. | Stable | concept | 1.2.0 |
| [l1-quality-assurance.md](specifications/l1-quality-assurance.md) | The checks every change must pass and which engine versions the editor code is proven against. Open it before changing the test strategy or CI gates. | Stable | concept | 1.1.0 |
| [l1-agent-feedback.md](specifications/l1-agent-feedback.md) | How an agent records what is missing, broken or misleading and gets it to the maintainers as copyable text or an approved issue submission. Open it before changing the feedback tools, their guidance or what a report may contain. | Stable | concept | 1.0.0 |
| [l1-release-lines.md](specifications/l1-release-lines.md) | One release line per Godot minor version plus a development line: routing changes between lines, release numbering, line lifecycle. Open it before deciding where a change lands, carrying a fix, releasing, or cutting or retiring a line. | Stable | concept | 1.1.0 |
| [l1-community-health.md](specifications/l1-community-health.md) | What the security policy and the contributor guide must say, for people and AI agents, and which changes must update them. Open it before editing either document or changing anything they describe. | Stable | concept | 1.1.0 |
| [l1-repository-stewardship.md](specifications/l1-repository-stewardship.md) | What an AI steward does with a repository's issues and pull requests on the maintainers' behalf, and where it stops. Open it before changing how issues or pull requests are handled, in this or any repository. | Stable | concept | 1.0.0 |
| [l1-runtime-observability.md](specifications/l1-runtime-observability.md) | Planned tools for runtime logs, stack traces and in-game camera capture. Open it when picking up that roadmap item. | Draft | concept | 0.1.0 |
| [l1-content-authoring.md](specifications/l1-content-authoring.md) | Planned tools for creating scenes and reading, updating and attaching scripts. Open it when picking up that roadmap item. | Draft | concept | 0.1.0 |
| [l2-rust-stdio-server.md](specifications/l2-rust-stdio-server.md) | How the Rust crate realizes the server core: session loop, configuration, error taxonomy. Open it when editing the entry point, session, protocol types or configuration code. | Stable | implementation | 1.2.0 |
| [l2-bridge-client-rust.md](specifications/l2-bridge-client-rust.md) | How the server keeps, multiplexes and recovers its editor connection. Open it when editing the bridge client or its timing. | Stable | implementation | 1.0.0 |
| [l2-godot-editor-addon.md](specifications/l2-godot-editor-addon.md) | How the GDScript addon implements the bridge, its handlers and export exclusion. Open it when editing anything in the addon. | Stable | implementation | 1.1.0 |
| [l2-tool-handlers.md](specifications/l2-tool-handlers.md) | How the Rust tool layer enforces the catalog rules, and the steps for adding a tool. Open it when editing tools, path checks or engine invocations. | Stable | implementation | 1.1.0 |
| [l2-ci-release-pipeline.md](specifications/l2-ci-release-pipeline.md) | Which workflow, script or manifest enforces each release rule. Open it when editing CI, release files or repository hygiene. | Stable | implementation | 1.2.1 |
| [l2-test-suite.md](specifications/l2-test-suite.md) | Map of the test files, shared helpers and opt-in editor tests. Open it to decide where a new test goes or how to run the editor tests. | Stable | implementation | 1.3.0 |
| [l2-agent-feedback-rust.md](specifications/l2-agent-feedback-rust.md) | How the Rust crate realizes the feedback channel: journal, scrubber, report and link, submission through the GitHub client, guidance and settings. Open it when editing the feedback tools or the code they call. | Stable | implementation | 1.0.2 |
| [l2-release-branches.md](specifications/l2-release-branches.md) | How the release lines map to git branches, version strings, identity files, workflows and the release skill. Open it when cutting, carrying, releasing or editing anything line-specific. | Stable | implementation | 1.1.0 |
| [l2-community-health-files.md](specifications/l2-community-health-files.md) | How SECURITY.md, CONTRIBUTING.md, their links and the reporting setting realize the community health concept on GitHub. Open it when editing either file or the links to them. | Stable | implementation | 1.1.0 |

| [l2-repo-steward-skill.md](specifications/l2-repo-steward-skill.md) | How the portable repo-steward skill realizes stewardship on GitHub. Open it when editing the skill or installing it in another repository. | Stable | implementation | 1.0.4 |
| [l2-repo-steward-godot-mcp.md](specifications/l2-repo-steward-godot-mcp.md) | How the steward is installed and configured in this repository, including how it treats the Godot lines. Open it when changing the steward's settings or files here. | Stable | implementation | 1.0.2 |
<!-- Add your specifications here -->
<!-- Description: what the specification is for and when to open it, in two sentences at most; no change history (that goes in the specification's Document History). -->

## Meta Information

- **Maintainer**: Core Team
- **Last Updated**: 2026-10-20
