# Installation

The files to download for your Godot version are listed in the [README](../README.md#installation). The quickest way to install is the prompt for your AI assistant in the README; these are the steps by hand.

## Install by hand

**1. Download** the server for your platform and the addon from the [latest release](https://github.com/zeegzoog/godot-mcp/releases/latest), the files for your Godot version (see [the table above](../README.md#installation)). `M.N` is your Godot version, e.g. `4.6`.

| Platform | Asset |
| --- | --- |
| Windows x86_64 | `godot-mcp-x86_64-pc-windows-msvc-godotM.N.zip` |
| Linux x86_64 | `godot-mcp-x86_64-unknown-linux-gnu-godotM.N.tar.gz` |
| macOS Apple Silicon | `godot-mcp-aarch64-apple-darwin-godotM.N.tar.gz` |
| macOS Intel | `godot-mcp-x86_64-apple-darwin-godotM.N.tar.gz` |
| Godot addon (required, any platform) | `godot-mcp-addon-godotM.N.zip` |

The addon warns in the editor's Output panel when your Godot is newer than the release was made for, and refuses to start in an older Godot.

**2. Enable the plugin.** Copy `addons/godot_mcp` from the addon archive into your project's `addons/` folder, open the project, and enable **Godot MCP Bridge** in **Project → Project Settings → Plugins**. The bridge listens on a port derived from the project's path, which a server started in the project directory derives too (see [Bridge port](configuration.md#bridge-port)), so editors of several projects or git worktrees can run at once. The addon is editor-only and leaves itself out of exported games while the plugin is enabled; if you export with it disabled, add `addons/godot_mcp/*` to the export preset's exclude filter.

**3. Register the server** in your MCP client, pointing `command` at the extracted binary and `GODOT_PATH` at your Godot executable:

```json
{
  "mcpServers": {
    "godot": {
      "command": "/path/to/godot-mcp",
      "args": [],
      "env": {
        "GODOT_PATH": "/path/to/Godot_v4.x-stable"
      }
    }
  }
}
```

| Client | Config location |
| --- | --- |
| Claude Code | `.mcp.json` in the project root, or `claude mcp add godot -e GODOT_PATH=/path/to/godot -- /path/to/godot-mcp` |
| Claude Desktop | `claude_desktop_config.json` |
| Cursor | `.cursor/mcp.json` |
| VS Code | `.vscode/mcp.json` (uses a top-level `servers` key instead of `mcpServers`) |
| Antigravity / others | the client's MCP settings file |

On Windows, escape backslashes in JSON: `"C:\\Program Files\\godot-mcp\\godot-mcp.exe"`.

Both paths are machine-specific. Don't commit a `.mcp.json` with real paths: commit a `.mcp.json.example` with placeholders, add `.mcp.json` to `.gitignore`, and let each developer copy it locally.
