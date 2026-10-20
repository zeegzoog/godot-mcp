# Priority Examples for godot-mcp

The repo steward uses these examples together with its rubric when it sets an issue's priority. Choose the highest level that fits.

| Priority | Examples |
| --- | --- |
| P0 | The editor bridge accepts a command without its token or from another machine. A tool writes or reads outside the project directory. A release file is missing or does not start. CI fails on `master` or on a `godot-v*` branch. |
| P1 | A tool always fails on a supported Godot line. An editor change made through a tool cannot be undone. The server stops answering a client session. A regression since the last release. |
| P2 | A stale result that lasts until the editor restarts. An error message without a recovery step. A requested tool that fits the roadmap. Misleading documentation. |
| P3 | The wording of a message or of the documentation. A question about usage. |
