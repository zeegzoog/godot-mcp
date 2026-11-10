# Configuration

The server reads its settings from CLI flags, the environment (e.g. the MCP client's `env` block), or a `.env` file in its working directory or a parent directory, in that order of precedence.

| Variable | Flag | Default | Purpose |
| --- | --- | --- | --- |
| `GODOT_PATH` | `--godot-path` | auto-detect | Godot executable. If unset, the server tries a few common install locations, then `godot` on `PATH`; set it explicitly. A value that points to a missing file is not replaced: tools that run Godot fail and name the path. |
| `GODOT_DEBUG_PORT` | `--debug-port` | derived | Editor bridge port. When unset, derived from the path of the project the server was started in, the way the plugin derives it; `9333` when the server runs outside any project. See [Bridge port](#bridge-port). |
| `GODOT_MCP_LSP_PORT` | | `6005` | Port of a running editor's GDScript language server. The `godot_code_*` tools and `godot_validate_script` use it if an editor with the same project open listens there, and otherwise start a private headless editor of the project (started again when another script of the project changed). |
| `GODOT_MCP_TOKEN` | | generated | Shared secret for the bridge. Normally unnecessary, see [Security](security.md). |
| `GODOT_CONSOLE_PATH` | `--godot-console-path` | | Windows console wrapper. Unused by the server; the editor end-to-end tests use it for editor logs. |
| `GODOT_MCP_FEEDBACK` | `--feedback` | `full` | Feedback channel for the maintainers: `off`, `draft` (the AI can write a report but never send one) or `full`. A value the server does not recognize gives `draft`. See [Feedback](tools.md#feedback-to-the-maintainers). |
| `GODOT_MCP_GH_PATH` | `--gh-path` | `gh` on `PATH` | The [GitHub CLI](https://cli.github.com) used when you let the AI submit a report. Set it when your MCP client does not see `gh` on its `PATH`. |

An empty value counts as "not set", so placeholders in a copied `.env.example` or an MCP client's `env` block are harmless.

In a `.env` file, put **Windows paths in single quotes** (`GODOT_PATH='C:\Program Files\Godot\Godot.exe'`). In double quotes a backslash starts an escape sequence and the whole file fails to load; the server logs a warning when that happens. See [.env.example](../.env.example).

## Bridge port

The plugin picks its port from the project setting `godot_mcp/bridge_port` if you add one, otherwise from the editor's own `GODOT_DEBUG_PORT` environment variable, otherwise it derives one from the project's path. The server does the same: `--debug-port` or `GODOT_DEBUG_PORT` if set, otherwise the port derived from the project found from its working directory (the directory itself, a parent, or its only subdirectory with a `project.godot`). So a server started in a project directory finds that project's editor with no port setting at all, and editors of different projects, such as git worktrees of one repository, never compete for one port. An editor started by `godot_launch_editor` is given the server's port. Both log the port and where it came from (the editor in its Output panel, the server on stderr), and an unreachable or busy port is reported with that origin.

The derived port is FNV-1a 32 of the canonical project path (symlinks resolved, no trailing slash; on Windows `/` separators and ASCII letters in lower case) in UTF-8, modulo 20000, plus 10000: a port from 10000 to 29999. [tests/bridge-port-vectors.json](../tests/bridge-port-vectors.json) lists examples that both implementations are tested against. Two projects can, rarely, get the same port; the second editor then reports the busy port, and a `godot_mcp/bridge_port` setting (with the same `--debug-port` for the server) resolves it.

Setups made before 0.10.1 used `9333`. A plugin without a configured port therefore also listens on `9333` while no other editor holds it, so a server with `GODOT_DEBUG_PORT=9333` in its config, or one started outside any project, still reaches the first editor opened. Remove `GODOT_DEBUG_PORT` from the server's config to have each server talk to its own project's editor.

Set the environment variable `GODOT_MCP_NO_BRIDGE=1` for an editor that must not open the bridge at all; the server does this for the private editor it starts for the language server. Add the project setting `godot_mcp/verbose` = `true` to log every client connection in the editor's Output panel. Editor Play loads the running-game runtime unless `godot_mcp/runtime_on_play` is `false`.
