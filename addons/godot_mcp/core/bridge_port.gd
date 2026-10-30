@tool
extends RefCounted
## The bridge port derived from the project path, used when no port is configured. The Rust
## server computes the same value (src/bridge_port.rs); tests/bridge-port-vectors.json holds
## the cases both sides check.
##
## Canonical path: the project directory with symlinks resolved, without a trailing slash; on
## Windows with "/" separators and ASCII letters in lower case. Port: FNV-1a 32 of its UTF-8
## bytes, modulo PORT_SPAN, plus FIRST_PORT.

const FIRST_PORT: int = 10000
const PORT_SPAN: int = 20000
const FNV_OFFSET: int = 0x811c9dc5
const FNV_PRIME: int = 0x01000193
const LOW_32_BITS: int = 0xffffffff


## Canonical path of the open project.
static func project_path() -> String:
    var path: String = ProjectSettings.globalize_path("res://")
    # Changing into the directory resolves symlinks on Unix (the engine reads getcwd back).
    var dir: DirAccess = DirAccess.open(path)
    if dir != null:
        path = dir.get_current_dir()
    return normalize(path, OS.get_name() == "Windows")


## Text rules of the canonical path, applied after the file system resolved it.
static func normalize(path: String, windows: bool) -> String:
    var text: String = path
    if windows:
        if text.begins_with("\\\\?\\"):
            text = text.substr(4)
        text = text.replace("\\", "/")
        var bytes: PackedByteArray = text.to_utf8_buffer()
        for i: int in bytes.size():
            if bytes[i] >= 65 and bytes[i] <= 90:
                bytes[i] += 32
        text = bytes.get_string_from_utf8()
    while text.length() > 1 and text.ends_with("/"):
        text = text.substr(0, text.length() - 1)
    return text


static func port_for(canonical_path: String) -> int:
    var value: int = FNV_OFFSET
    for byte: int in canonical_path.to_utf8_buffer():
        value = ((value ^ byte) * FNV_PRIME) & LOW_32_BITS
    return FIRST_PORT + value % PORT_SPAN
