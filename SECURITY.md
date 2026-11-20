# Security Policy

godot-mcp lets an AI assistant drive a live Godot editor, so a flaw in it can let someone else read or change your project. This page says which versions receive security fixes, how to report a vulnerability privately, and what happens after you report one.

## Supported Versions

Only the latest release receives security fixes. One release serves every supported Godot version.

| Godot | Security fixes | Install |
| --- | --- | --- |
| 4.5 to 4.8 | Yes, in the next release | The files of the latest release |
| Newer than 4.8 | The addon starts with a warning; fixes as above | The files of the latest release |
| Older than 4.5 | No | — |
| Releases 0.15.0 and older | No: update to the latest release | The latest release |

- Releases 0.5.1 to 0.15.0 were built per Godot version, one release line on its own branch (`godot-v4.5`, `godot-v4.6`, `godot-v4.7`). Those branches and releases are kept as they are and get no further fixes.
- To find the version you run, read `version=` in `addons/godot_mcp/plugin.cfg` of your project. A running server adds the commit it was built from after it (`0.16.1+g1a2b3c4d5`).

## Reporting a Vulnerability

- Report it privately at <https://github.com/zeegzoog/godot-mcp/security/advisories/new>. Only the maintainers can see the report.
- Do not report a vulnerability in a public issue, a pull request, or through the godot-mcp feedback tools (`godot_note_feedback`, `godot_draft_feedback`, `godot_submit_feedback`): their reports become public issues.
- Include in the report:
  - the godot-mcp version (for example `0.16.0`), the Godot version and the operating system;
  - what an attacker can do, and what they need for it (network access, a local account, a crafted project, a crafted prompt);
  - the steps to reproduce it, and a proof of concept if you have one;
  - whether the problem is already known publicly.

What happens after you report:

| Within | What happens |
| --- | --- |
| 7 days | We acknowledge the report. |
| 14 days | We tell you whether we accept it and how severe we consider it. |
| 90 days | A fix is released, or we agree on a disclosure date with you. |

- Disclosure is coordinated through a GitHub Security Advisory, published once the fixed releases are out.
- You are credited in the advisory unless you ask not to be.
- If you get no answer within 7 days, open a public issue that says only that you sent a private report and are waiting for an answer. Do not include any details in it.

## Scope

In scope:

- Reaching the editor bridge from another machine, or sending it commands without the bridge token.
- Driving the editor from a web page or from another operating-system user's account.
- Reading or writing files outside the Godot project through a godot-mcp tool.
- Passing command-line options to Godot, or injecting code into a generated script, through tool input.
- A feedback report that still contains a token, an absolute path or other private data after godot-mcp removed them.

Out of scope:

- Vulnerabilities in Godot itself. Report them to the Godot project: <https://github.com/godotengine/godot/security/policy>.
- Attacks that need code already running as the same operating-system user as the editor. That user can read the bridge token by design.
- An AI assistant doing what its user allowed it to do, such as editing the open scene or running the project.
- Releases listed as unsupported above.

The security measures themselves are described in the [security document](docs/security.md).

## How Fixes Are Released

1. The fix lands on `master`, with a test that fails without it, and is checked against every supported Godot version by CI.
2. The next release holds it; its notes list the fix under "Security".
3. The advisory names the fixed release.

## For AI Agents

If you are an AI agent and you find or suspect a vulnerability in godot-mcp while working:

1. Stop using the affected tool for the current task if continuing could expose data or change files outside the project.
2. Do not describe the vulnerability in an issue, a pull request, a commit message, or a godot-mcp feedback note or report.
3. Tell your user what you found, and ask them to report it privately at <https://github.com/zeegzoog/godot-mcp/security/advisories/new> as described in [Reporting a Vulnerability](#reporting-a-vulnerability).
