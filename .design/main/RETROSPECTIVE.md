# SDD Retrospective

**Last Full Run:** 2026-10-21
**Full Sessions:** 1
**Snapshots:** 6

## Snapshots

Auto-collected after each phase completion. Lightweight metrics only — no analysis.

| Date | Phase | Specs (D/R/S) | Tasks (Done/Blocked/Cancelled) | Rules | Signal |
| --- | --- | --- | --- | --- | --- |
| 2026-10-16 | Phase 3 | 2/0/13 | 10/0/0 | 24 | 🟢 |
| 2026-10-21 | Phase 7 | 0/0/27 | 8/0/0 | 24 | 🟢 |
| 2026-10-21 | Phase 5 | 0/0/27 | 14/0/0 | 24 | 🟢 |
| 2026-10-21 | Phase 8 | 0/0/28 | 9/0/0 | 24 | 🟢 |
| 2026-11-02 | Phase 10 | 0/0/31 | 9/0/0 | 24 | 🟢 |
| 2026-11-02 | Phase 11 | 0/0/31 | 4/0/0 | 24 | 🟢 |

## Session 1 — 2026-10-21

**Scope:** Full system analysis, at the close of the plan (phases 2 to 7)
**Specs in registry:** 27 (Draft 0, RFC 0, Stable 27)
**Tasks total:** 60 (Done: 60, Blocked: 0, Cancelled: 0)
**RULES.md §7 entries:** 24

### 🚀 DORA Metrics (L2 Implementation)

| Metric | Value | Source | Details |
| --- | --- | --- | --- |
| **Deployment Frequency** | 4 releases in 8 days | Git tags and release list | v0.3.0 (10-14), v0.4.0 (10-16), v0.5.1 (10-20), v0.6.0 (10-21); the readiness notice calls v0.7.0 due |
| **Change Failure Rate** | n/a | none supplied | no CI hook or note records a failed release |

### 📊 Observations

| # | Severity | Area | Observation | Evidence |
| ---: | --- | --- | --- | --- |
| 1 | ✨ Positive | Spec first on a risky engine | The held-stepping specification listed its unverified engine facts and gave them a feasibility task first; that task found the one false premise (a vanished server cannot be detected by the game) before anything was built on it. | `l2-held-stepping.md` §2, `archives/tasks/phase-7.md` T-7A01 |
| 2 | ✨ Positive | Steward | A further steward run found real work (a released message owed on a closed issue) and a second pass posted nothing: 35 marked messages before and after. | `tasks/phase-5.md` T-5E04 |
| 3 | 🟡 Medium | Spec after code | The game-runtime concepts were specified after the pull requests were merged, so the specifications recorded what the code did rather than directing it. | `archives/tasks/phase-6.md` T-6S01 notes |
| 4 | 🟡 Medium | Spec revision | `l1-controlled-time.md` and `l2-held-stepping.md` were amended on the day they became Stable, once, for one invariant. The cost was one blocked validation and one extra planning round. | both specifications' Document History |
| 5 | 🟡 Medium | Commits | Two commits mixed planning files with product files, against the contributor guide; carrying them to the release lines needed a hand-built, product-only patch. | `CONTRIBUTING.md` § Planning and Specifications; commits `802aca4`, `26aaa6a` |
| 6 | 🟡 Medium | Addon hygiene | A new addon script reached `master` without its `.uid` file, which the three release lines have; no test checks that every addon script has one. | `addons/godot_mcp/runtime/ops/rt_hold.gd.uid` |
| 7 | 🟡 Medium | Settings | The protection of `master` and the project settings waited until the end of the plan; both are outside the agent's reach and were only read, then applied at the owner's word. | `tasks/phase-5.md` T-5E03 |

### 💡 Recommendations

| # | Refs Observation | Recommendation | Target File |
| --- | --- | --- | --- |
| R1 | #3 | For a contribution series that changes a runtime channel, write the concept first and name the engine facts each pull request must prove, as the held-stepping specification did. | `.design/main/specifications/l1-runtime-observability.md` |
| R2 | #5 | Commit `.design/` separately from product files, as the guide already says; a pre-commit check could refuse a commit that touches both. | `CONTRIBUTING.md` |
| R3 | #6 | Add a test that every script under `addons/godot_mcp/` has a `.uid` file next to it. | `tests/structure_test.rs` (new, product) |
| R4 | #7 | Put the protection of `master` on the checklist of the first plan phase that touches CI, so it is not left to the last task. | `.design/main/specifications/l2-ci-release-pipeline.md` |

### 📈 Trends (from Snapshots)

| Metric | Previous Snapshot | Current | Δ |
| --- | --- | --- | --- |
| Specs in registry | 15 | 27 | +12 |
| Blocked task rate | 0% | 0% | 0% |
| Signal | 🟢 | 🟢 | → |

**Signal framing (spec-quality lens):** green. Blocks happened twice in the last phase, and both came from the plan's order or from one measured engine fact, not from an ambiguous specification: the validation was ordered after the carrying, and the engine does not let a game see a closed debugger connection. No shadow logic: every tool in the catalog traces to a Stable specification, and the runtime concepts that were specified after the code (observation 3) now agree with it.

## Session 2 — 2026-11-02

**Scope:** Full system analysis, at the close of the plan (phases 10 and 11)
**Specs in registry:** 31 (Draft 0, RFC 0, Stable 31)
**Tasks total:** 13 in the two phases (Done: 13, Blocked: 0, Cancelled: 0)
**RULES.md §7 entries:** 24

### 🚀 DORA Metrics (L2 Implementation)

| Metric | Value | Source | Details |
| --- | --- | --- | --- |
| **Deployment Frequency** | n/a | none supplied | the last release is v0.12.0 (11-01); the commits of this plan are not released or pushed |
| **Change Failure Rate** | n/a | none supplied | no CI hook or note records a failed release |

### 📊 Observations

| # | Severity | Area | Observation | Evidence |
| ---: | --- | --- | --- | --- |
| 1 | 🔴 High | Spec after code | Seven pull requests (#58 to #101) added thirteen tools and four modules with no specification, and the specification still gave a bridge port the code had dropped. The ventilation of 2026-11-02 found it; one reality-sync pass wrote a concept and sixteen amendments. | `git log` of `.design/`; Document History of `l1-tool-catalog.md` 1.11.0 |
| 2 | 🟡 Medium | Stale block | The carry of Phase 10 stayed Blocked for five days on a cause that had gone away: the lines were released as 0.12.0 with the adapter already on them. Nothing re-checked the cause, and the engine named the first blocked task as the next step even when a later phase had open work. | `tasks/phase-10.md` T-10E01 Attempts and Changes |
| 3 | 🟡 Medium | Carry | Running the carried commits on the older engines found three gaps that the 4.8 editor hides: `get_base_editor` on the shader tab exists only from 4.8, a new text shader gets no `.uid` from the file system update, and a tab switch after a reload comes a frame late. A carry that had only been compiled would have shipped shader tools that cannot find their tab. | `tasks/phase-11.md` T-11E01 Changes; commit ffe4735 |
| 4 | 🟡 Medium | Parity | The file fallback and the editor path of project settings drifted apart (#98 changed one), against CA-11. A single table of inputs used by both tests now guards it. | `tests/tools_test.rs`, `tests/editor_e2e_test.rs` |
| 5 | 🟡 Medium | Environment | Three editor tests fail on this machine on every line and on `master` because the editor runs in Russian (a dialog title, an engine error text) or because of a wrapper PID. They are named in every report, and a real failure could hide among them. | `verify` results of T-11E01 |
| 6 | ✨ Positive | Test first | Every code change of Phase 11 had a test that failed first for the right reason (the old refusal, the missing label); the carry's failures were found by the existing suites, not by hand. | `tasks/phase-11.md` |

### 💡 Recommendations

| # | Refs Observation | Recommendation | Target File |
| --- | --- | --- | --- |
| R1 | #1 | Label a merged pull request that adds a tool or a module `status: planning-owed` until its specification is amended (done in Phase 11). | `.agents/skills/repo-steward/reference/pull-requests.md` |
| R2 | #3 | Name the oldest supported engine in the editor tests of a new editor-side feature and run them on every line before the carry is called done. | `.design/main/specifications/l2-test-suite.md` |
| R3 | #5 | Make the three tests locale-independent (match the engine's error by its structure, not its English words) so a failure on a Russian editor is a real one. | `tests/export_e2e_test.rs`, `tests/editor_e2e_test.rs` |
| R4 | #2 | Report to the engine's developers that `finalize` takes the next step from the first blocked task of any phase. | the bug report already drafted for `.magic/scripts/` |

### 📈 Trends (from Snapshots)

| Metric | Previous Snapshot | Current | Δ |
| --- | --- | --- | --- |
| Specs in registry | 27 | 31 | +4 |
| Blocked task rate | 0% | 0% | 0% |
| Signal | 🟢 | 🟢 | → |

**Signal framing (spec-quality lens):** green at the end, yellow in the middle. The specifications trailed the code by seven pull requests until the ventilation caught it, and both blocks of the plan came from facts nobody re-read, not from an ambiguous specification. At the close there is no shadow logic: every tool in the catalog traces to a Stable specification, and the three portability gaps are in the product commit history, not in a specification that promised otherwise.
