# Godot MCP Server

A fast, cross-platform [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server, written in Rust, that lets AI assistants inspect and edit a live Godot 4 editor.

- **One native binary** for Windows, Linux, and macOS (Intel and Apple Silicon). No Node.js or Python.
- **Live editor bridge.** A small GDScript plugin exposes the open scenes over a local WebSocket (loopback only, token-protected). Node edits go through `EditorUndoRedoManager`, so anything the AI changes can be undone with `Ctrl+Z`.
- **Headless fallback.** Without a running editor, the server calls the Godot binary directly to report the engine version, validate scripts, launch the editor, run the project as a managed game and ask Godot's language server about scripts. It writes scripts, scenes, resources and project settings as files.

## Installation

Requirements: **Godot 4.5 or newer**, the godot-mcp files made for your Godot version, and an MCP client such as Claude Code, Claude Desktop, Cursor, VS Code, or Antigravity. No Rust toolchain needed.

### Let your AI assistant install it

The quickest way. Open your Godot project in the assistant and paste:

```text
Install the Godot MCP server (https://github.com/zeegzoog/godot-mcp) for this Godot project. Use the GitHub release, not a local clone of that repository.

1. Detect the OS and CPU architecture (Windows, Linux, or macOS; x86_64 or arm64).
2. Ask me for the full path to my Godot executable. Never guess it: Windows builds are portable, version-named files (e.g. Godot_v4.5-stable_win64.exe) stored wherever I put them. Run `<path> --version` to confirm it works, and note its major.minor version (4.6.3.stable.official → 4.6).
3. Take the files for that Godot version from the latest release: `gh release view --repo zeegzoog/godot-mcp --json tagName,assets` lists them; use the assets whose names end in `-godot<major.minor>` (with a tag like `v0.5.1`). If the latest release has no such files, tell me and do not take the files of another Godot version. Download the server archive for this platform and the addon archive, e.g.:
   gh release download <tag> --repo zeegzoog/godot-mcp --pattern "godot-mcp-<target-triple>-godot<major.minor>*"
   gh release download <tag> --repo zeegzoog/godot-mcp --pattern "godot-mcp-addon-godot<major.minor>*"
   Extract the server to a permanent folder outside the project: %LOCALAPPDATA%\godot-mcp\ (Windows), ~/.local/share/godot-mcp/ (Linux), ~/Library/Application Support/godot-mcp/ (macOS).
4. Copy addons/godot_mcp from the addon archive into <project>/addons/godot_mcp and enable it in project.godot:
   [editor_plugins]
   enabled=PackedStringArray("res://addons/godot_mcp/plugin.cfg")
5. Register the server as "godot" in my MCP client config, with the binary from step 3 as the command and GODOT_PATH from step 2 in env. If the config is a project-scoped .mcp.json in a shared repository, do not commit real paths: write a .mcp.json.example with placeholders and add .mcp.json to .gitignore.
6. Ask me to restart the MCP client and (re)open the project in the Godot editor, then call godot_get_version and report the result.
```

### Pick the files for your Godot version

Each godot-mcp release holds the files for every supported Godot minor version. Download the files whose names end in `-godotM.N`, where `M.N` is your Godot version (for example `-godot4.6`).

| Your Godot | Branch | Files to download |
| --- | --- | --- |
| 4.8 (pre-release) | `master` | none; build from source |
| 4.7.x | `godot-v4.7` | the files ending in `-godot4.7` |
| 4.6.x | `godot-v4.6` | the files ending in `-godot4.6` |
| 4.5.x | `godot-v4.5` | the files ending in `-godot4.5` |

Release 0.4.0 and older are one build for Godot 4.5 and newer, with file names that have no `-godotM.N` suffix.

To install by hand, download the files for your Godot version, put the addon into your project and register the server in your MCP client: the steps are in [docs/installation.md](docs/installation.md).

## Documentation

| Document | What is in it |
| --- | --- |
| [Installation](docs/installation.md) | Install with an AI assistant or by hand; client configuration |
| [Configuration](docs/configuration.md) | Environment variables, flags, the bridge port |
| [Security](docs/security.md) | What the bridge and the tools allow and refuse |
| [Tools](docs/tools.md) | Every tool, exporting, the feedback channel |
| [Running game](docs/running-game.md) | Inspecting, holding and scripting a running game, the project adapter |
| [Game checks](docs/game-checks.md) | Repeatable play-throughs with `godot_game_scenario` |
| [Feature comparison](docs/feature-parity.md) | How this server compares with other Godot MCP servers |

## How it works

```
AI client <-stdio JSON-RPC-> godot-mcp <-WebSocket :<bridge port>-> addons/godot_mcp (inside the Godot editor)
                                 |
                                 +-> Godot binary (headless fallback)
```

Game tools reach a game through Godot's remote debugger: a managed process connects to this server on loopback, and the plugin relays editor Play.

## Contributing

Set up, choose the branch for your Godot version, run the checks and open a pull request: [CONTRIBUTING.md](CONTRIBUTING.md). AI agents start with [AGENTS.md](AGENTS.md). Releases are described in [CONTRIBUTING.md](CONTRIBUTING.md#releases). To report a vulnerability, follow [SECURITY.md](SECURITY.md).

## License

MIT. See [LICENSE](LICENSE).
