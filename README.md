# Godot MCP Server

A fast, cross-platform [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server, written in Rust, that lets AI assistants inspect and edit a live Godot 4 editor.

- **One native binary** for Windows, Linux, and macOS (Intel and Apple Silicon). No Node.js or Python.
- **Live editor bridge.** A small GDScript plugin exposes the open scenes over a local WebSocket (loopback only, token-protected). Node edits go through `EditorUndoRedoManager`, so anything the AI changes can be undone with `Ctrl+Z`.
- **Headless fallback.** Without a running editor, the server calls the Godot binary directly to report the engine version, validate scripts, launch the editor, run the project as a managed game and ask Godot's language server about scripts. It writes scripts, scenes, resources and project settings as files.

## Installation

Requirements: **Godot 4.5 or newer**, the godot-mcp files made for your Godot version, and an MCP client such as Claude Code, Claude Desktop, Cursor, VS Code, or Antigravity. No Rust toolchain needed.

### Pick the files for your Godot version

Each godot-mcp release holds the files for every supported Godot minor version. Download the files whose names end in `-godotM.N`, where `M.N` is your Godot version (for example `-godot4.6`).

| Your Godot | Branch | Files to download |
| --- | --- | --- |
| 4.8 (pre-release) | `master` | none; build from source |
| 4.7.x | `godot-v4.7` | the files ending in `-godot4.7` |
| 4.6.x | `godot-v4.6` | the files ending in `-godot4.6` |
| 4.5.x | `godot-v4.5` | the files ending in `-godot4.5` |

Release 0.4.0 and older are one build for Godot 4.5 and newer, with file names that have no `-godotM.N` suffix.

Install the addon into your project and register the server in your MCP client: [docs/installation.md](docs/installation.md) has a prompt that lets your AI assistant do it, and the manual steps.

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
