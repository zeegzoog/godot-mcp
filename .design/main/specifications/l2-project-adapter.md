# Project Adapter Runtime

**Version:** 1.1.3
**Status:** Stable
**Layer:** implementation
**Implements:** l1-project-adapter.md

## Overview

How the addon's in-game component loads the project's adapter and answers the runtime methods `project.state` and `project.command`, and how the server offers them as `godot_game_state` and `godot_game_command`.

## Related Specifications

- [l1-project-adapter.md](l1-project-adapter.md) - Parent concept.
- [l2-game-runtime.md](l2-game-runtime.md) - The channel, the run selection and the value encoding these methods reuse.
- [l2-held-stepping.md](l2-held-stepping.md) - The held dispatch the two methods pass through.
- [l2-tool-handlers.md](l2-tool-handlers.md) - How the two tools are routed and annotated.
- [l2-test-suite.md](l2-test-suite.md) - Where their tests go.

## 1. Motivation

The runtime already has a channel into the game, a value encoding and an output capture. The adapter needs a registry with the registration calls, a loader, two runtime methods and two tools on top of them; nothing in the channel changes.

## 2. Constraints & Assumptions

- The setting is `godot_mcp/adapter`, read from `ProjectSettings` when the component starts. It holds a `res://` path or a `uid://` (the file picker of Project Settings stores a uid); anything else is reported as not loadable. An agent can set it with `godot_set_project_setting`; the addon does not register it in the Project Settings list, as it does not register its other settings.
- Only API present in the first release of the oldest supported Godot version (4.5.0) is used: `Callable.get_argument_count()` (since 4.3) and the output capture of `OS.add_logger` (since 4.5) already in `rt_output.gd`.
- Addon scripts declare no `class_name` and are loaded by path; an adapter's `register(registry)` receives the registry untyped.
- [DR] Two tools, `godot_game_state` and `godot_game_command`, not one tool with an action: the read is annotated read-only and repeatable, the command destructive and not repeatable, and one tool could carry only one of those annotations. Rejected: `godot_game_do` (says less than `command`); `godot_game_project` with an `action` (wrong annotations for one of the two uses). Override: `/magic.spec amend l2-project-adapter.md "one tool"`.
- [DR] The answer bound is 256 KiB of JSON text: small enough for an agent's context. Override: `/magic.spec amend l2-project-adapter.md "another bound"`.
- Engine facts, measured on 4.8-beta1, the build `.github/godot-version` named then, by the end-to-end test `logger_sees_a_script_error_of_a_called_handler_at_once`: (1) a script error raised inside a handler that the component calls through `Callable.call()` is in the logger by the time the call returns, on the main thread, with the engine's script error type; its text is in the entry's `code`, the `rationale` is empty, and the handler returns null while the caller goes on; (2) `push_error` in a handler is logged with the plain error type, so the two are told apart; (3) a handler that awaits, called without `await`, makes the engine log a script error that names the *calling* function and ends that function, which returns its type's default; the function above it goes on; (4) `await handler.call(args)` from a coroutine runs an awaiting handler and a plain one alike. PA-6 stands on (1) to (3); (4) is not used, see §7.

## 4. Invariant Compliance

| L1 Invariant | Implementation |
| --- | --- |
| PA-1 One entry point | `rt_project.gd` reads `godot_mcp/adapter` once in the component's `_ready`, before the hello is sent; an empty setting gives the status `{configured: false}`, and the tools' recovery names the setting and `godot_set_project_setting`. Neither the server nor the addon contains a project name. |
| PA-2 Two kinds of registration | `rt_registry.gd` `add_state(name, provider) -> bool` accepts a valid callable with `get_argument_count() == 0`; `add_command(name, handler, help = "") -> bool` accepts one with exactly one argument. A refused registration pushes a warning and is listed in the status under `refused` (`{name, kind, reason}`); it returns `false` and the adapter goes on. A second registration under a name already taken replaces the first. |
| PA-3 Only what was registered | `project.command` looks the name up in the registry only; there is no method-name path. An unknown name answers `NOT_FOUND` with `data.commands`, an unknown state name an `{error}` entry. `game.call` keeps its permit list and refusal list. |
| PA-4 The project decides | No refusal list is applied to a registered command. `godot_game_command` is annotated `[false, true, false]` (read-only, destructive, idempotent) like `godot_game_call`; `godot_game_state` `[true, false, true]` like `godot_game_get`. The tools' descriptions say that a command does what the project wrote, which may include files. |
| PA-5 Refusal is explicit | `refuse(message) -> Dictionary` returns `{"__bad_args__": str(message)}`; a result that is exactly that one key answers `BAD_PARAMS` with the message. |
| PA-6 Failures reported | The output logger gains a mark (its next sequence number) and a read of the entries since a mark. Around `register`, each provider and each handler the component takes a mark; a logged entry of the engine's script error type makes that call fail (`GODOT_ERROR` for a command, `{error}` for a provider, the status `error` for `register`), and the other error and warning entries come back as `errors`. Each call into the project's code sits in a function of its own in `rt_registry.gd` with a typed return (`call_provider`, `call_command`): a script error ends only the function it occurs in, which returns its type's default, so the caller finds the logged entry and an absent value and still answers, and nothing waits for a reply that would never come. The status `{configured, path, loaded, error, states, commands, refused}` (`states` lists the provider names, `commands` maps each command name to its help) is in the hello, in every `project.*` answer and in `godot_get_run_status` for each managed run (the server keeps the hello of a run it started; for editor Play the status is in the answers). |
| PA-7 Isolation | `read_state` calls each named provider apart; one entry's failure leaves the others. |
| PA-8 Conversion and bound | Values go through `rt.encode`, the encoder of `game.get`. When the JSON text of the states together exceeds 262 144 bytes `project.state` answers `BAD_PARAMS` with `data.sizes` (bytes per requested name) and a message to ask for fewer names. A command's value over the same bound answers `GODOT_ERROR` that says the command ran, with its size: asking again would run it again, so the message asks for a smaller value instead. |
| PA-9 Held games | `project.state` and `project.command` are added to the served list of the hold (`Hold.SERVED`; the hold refuses every method not on it) and need no held variant: they run inside the hold like `game.get` and `game.set`. A handler or provider that awaits is not supported: called plainly it makes the engine log a script error against the calling function and ends that function (fact 3 of §2), which PA-6 turns into a failed call; the answer says that handlers and providers must not await. What a handler defers (`call_deferred`, a timer, a signal) takes effect when the game next advances. |
| PA-10 Wherever the component runs | Loading lives in the component, so a managed run and editor Play load it alike; `RUNTIME_FILES` in `runs.rs` gains the two scripts, so a private runtime copy carries them too. An adapter that extends `Node` and is not in the tree is freed in `_exit_tree`. |

## 5. Detailed Design

### 5.1 Files

```plaintext
addons/godot_mcp/runtime/
├── mcp_runtime.gd        loads the adapter before the hello; the hello carries `adapter`
└── ops/
    ├── rt_registry.gd    the registry handed to register(registry)
    ├── rt_project.gd     adapter loading and status; project.state and project.command
    └── rt_output.gd      gains mark() and lines_since(mark) for PA-6
src/
├── godot/runs.rs         RUNTIME_FILES gains the two scripts; the run summary shows `adapter`
└── tools/game/
    ├── catalog.rs        the two tool definitions
    └── project.rs        their handlers
tests/
├── tools_test.rs         schemas, annotations, refusals before the game is touched
├── runtime_e2e_test.rs   a fixture adapter written into the test's temporary project
└── editor_e2e_test.rs    the same fixture adapter answering in editor Play
```

### 5.2 Runtime Methods

`project.state` takes `{names?: [string]}`; a missing or empty list reads every provider. It answers `{adapter, values: {name: value | {error}}, errors}`.

`project.command` takes `{name: string, args?: object}`; the arguments are the parsed JSON, numbers as floats. It answers `{adapter, name, value, errors}`, or an error:

| Case | Code | Recovery the server adds |
| --- | --- | --- |
| no adapter, or one that did not load | `UNSUPPORTED` | set `godot_mcp/adapter` with `godot_set_project_setting` and start the run again; read the status's `error` |
| unknown command | `NOT_FOUND` | the registered names, and `godot_game_state` for their help |
| refused arguments | `BAD_PARAMS` | the command's help |
| script error, an awaiting handler, or a handler no longer valid | `GODOT_ERROR` | `godot_run_output` level `error` for the stack; a handler must not await |
| states over the bound | `BAD_PARAMS` | ask for fewer names; `data.sizes` |
| a command's value over the bound | `GODOT_ERROR` | the command ran; have it return less |

### 5.3 Tools

| Tool | Parameters | Answer |
| --- | --- | --- |
| `godot_game_state` | `names` (0 to 64 strings, each 1 to 256 characters), `run_id` | `{run_id, adapter, values, errors}`; `adapter.commands` maps each command to its help, so one call shows everything the project offers |
| `godot_game_command` | `name` (1 to 256 characters, required), `args` (object), `run_id` | `{run_id, name, value, errors, adapter}` |

Both select the run as `godot_game_get` does: `run_id`, else the newest managed run or the editor's Play session. Both work on a held game. The descriptions tell the agent to look at `adapter` in `godot_get_run_status` and to prefer the project's states and commands to walking the tree when they exist.

### 5.4 Documentation

The README's tool list gains the two tools and a section on the adapter: the setting, the registry calls, the outcome table and a fixture-sized example. The agent guide's running-game protocol gains one line on the adapter. The GDScript syntax check lists in `AGENTS.md` and `CONTRIBUTING.md` gain the two scripts, together.

### 5.5 Supported Godot Versions

Built on `master` for every supported Godot version: nothing here needs API newer than 4.5.0, and CI runs the end-to-end tests with every supported Godot build.

## 6. Implementation Notes

1. Feasibility first, done: the end-to-end test `logger_sees_a_script_error_of_a_called_handler_at_once` pins the engine facts of §2 on the build of `.github/godot-version`; the awaiting handler turned out not to hand control back, which amended PA-9 here and in the concept.
2. `rt_output.gd` mark and read, `rt_registry.gd`, `rt_project.gd`, the hello and `RUNTIME_FILES`, with the fixture adapter's real-game tests (registration, refusals, isolation, script errors, the bound, a held game, editor Play).
3. The server tools, annotations and run status, with their protocol tests.
4. Documentation and the check lists.
5. Run the end-to-end tests on every supported Godot version (CI).

## 7. Drawbacks & Alternatives

- **Errors from other threads**: an error another thread logs while a handler runs is attributed to that call. Accepted: the window is one synchronous call on the main thread, and the entry keeps its own text.
- **Awaiting handlers**: supporting them would mean awaiting the call in a coroutine all the way up from the engine's message callback, and holding the reply until the handler ends, with a limit on how long. Not worth the machinery; the failure is reported instead (PA-9). Revisit when a project needs a handler that waits.
- **A typed registry argument**: rejected; it would need a `class_name` (forbidden in the addon) and would break adapters that declare `register(registry)` untyped.

## Canonical References

| Alias | Path | Purpose |
| --- | --- | --- |
| `[RUNTIME]` | `addons/godot_mcp/runtime/mcp_runtime.gd` | Component start, hello, dispatch |
| `[OUTPUT]` | `addons/godot_mcp/runtime/ops/rt_output.gd` | The logger PA-6 reads |
| `[HOLD]` | `addons/godot_mcp/runtime/ops/rt_hold.gd` | The held refusal list the methods stay out of |
| `[RUNS]` | `src/godot/runs.rs` | Runtime files and run summary |
| `[GAME]` | `src/tools/game/` | Tool definitions and handlers |
| `[HINTS]` | `src/tools.rs` | Tool annotations |
| `[E2E]` | `tests/runtime_e2e_test.rs` | Real-game tests with a fixture adapter |
| `[PLAY]` | `tests/editor_e2e_test.rs` | The fixture adapter in editor Play |

## Document History

| Version | Date | Author | Description |
| --- | --- | --- | --- |
| 1.0.0 | 2026-10-29 | Agent (magic.spec) | Created with the project adapter concept: the registry and loader in the in-game component, failures detected through the output logger, the two methods and tools, the answer bound, documentation and the release lines; the logger's view of script errors left to a feasibility test. Promoted to Stable after Post-Update Review. |
| 1.1.0 | 2026-10-29 | Agent (magic.run) | [MODIFIED] The engine facts measured by the feasibility test replace the premise (§2); PA-6 names the contained function per call; PA-9: an awaiting handler is not supported, with the measured reason, and §7 records the alternative. Stable → RFC → Stable after Post-Update Review. |
| 1.1.1 | 2026-10-29 | Agent (magic.run) | The two methods are added to the served list of the hold, which refuses everything not on it (the first text said they needed no change); a command's value over the bound is a `GODOT_ERROR` that says the command ran. No other requirement changed. |
| 1.1.2 | 2026-11-05 | Agent (magic.spec) | The mentions of a tool outside the technology scope are gone from the registry, refusal and call notes and from the rejected alternatives. No requirement changed. |
| 1.1.3 | 2026-11-20 | Agent (magic.spec) | §5.5 and the implementation steps speak of the supported Godot versions instead of carrying to release lines; the API floor is the first release of the oldest supported version. No requirement changed. |
