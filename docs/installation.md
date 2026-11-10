# Installation

The files to download for your Godot version are listed in the [README](../README.md#installation). Install them either by letting your AI assistant do it (Option A) or by hand (Option B).

## Option A: let your AI assistant install it

Open your Godot project in the assistant and paste:

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

## Option B: install manually

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
