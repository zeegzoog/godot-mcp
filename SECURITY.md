# Security Policy

godot-mcp lets an AI assistant drive a live Godot editor, so a flaw in it can let someone else read or change your project. This page says which versions receive security fixes, how to report a vulnerability privately, and what happens after you report one.

## Supported Versions

Each godot-mcp release is built for one Godot minor version. Every supported Godot version has its own release line, kept on its own branch, and receives security fixes.

| Godot | Branch | Security fixes | Install |
| --- | --- | --- | --- |
| 4.7.x | `godot-v4.7` | Yes, in the next 4.7 release | The newest release whose tag ends with `+godot.4.7` |
| 4.6.x | `godot-v4.6` | Yes, in the next 4.6 release | The newest release whose tag ends with `+godot.4.6` |
| 4.5.x | `godot-v4.5` | Yes, in the next 4.5 release | The newest release whose tag ends with `+godot.4.5` |
| 4.8 (pre-release) | `master` | Fixed here first; no releases | Build from source |
| 0.4.0 and older | — | No | The release for your Godot version |

- A release for Godot 4.6 has a tag and a version that end in `+godot.4.6`, and is titled "godot-mcp X.Y.Z (Godot 4.6)".
- To find the version you run, read `version=` in `addons/godot_mcp/plugin.cfg` of your project.

## Reporting a Vulnerability

- Report it privately at <https://github.com/zeegzoog/godot-mcp/security/advisories/new>. Only the maintainers can see the report.
- Do not report a vulnerability in a public issue, a pull request, or through the godot-mcp feedback tools (`godot_note_feedback`, `godot_draft_feedback`, `godot_submit_feedback`): their reports become public issues.
- Include in the report:
  - the godot-mcp version (for example `0.5.0+godot.4.6`), the Godot version and the operating system;
  - what an attacker can do, and what they need for it (network access, a local account, a crafted project, a crafted prompt);
  - the steps to reproduce it, and a proof of concept if you have one;
  - whether the problem is already known publicly.

What happens after you report:

| Within | What happens |
| --- | --- |
| 7 days | We acknowledge the report. |
| 14 days | We tell you whether we accept it and how severe we consider it. |
| 90 days | A fix is released on every supported line it affects, or we agree on a disclosure date with you. |

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

The security measures themselves are described in the [Security section of the README](README.md#security).

## How Fixes Are Released

1. The fix lands first on the newest line it affects (`master`, when the development line is affected).
2. It is carried to every older supported line it affects.
3. Each affected line gets a new release, whose notes list the fix under "Security".
4. The advisory names the fixed release of each line.

## For AI Agents

If you are an AI agent and you find or suspect a vulnerability in godot-mcp while working:

1. Stop using the affected tool for the current task if continuing could expose data or change files outside the project.
2. Do not describe the vulnerability in an issue, a pull request, a commit message, or a godot-mcp feedback note or report.
3. Tell your user what you found, and ask them to report it privately at <https://github.com/zeegzoog/godot-mcp/security/advisories/new> as described in [Reporting a Vulnerability](#reporting-a-vulnerability).
