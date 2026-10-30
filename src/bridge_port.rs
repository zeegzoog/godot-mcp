//! The bridge port derived from the project path, used when no port is configured, so that
//! editors of different projects (for example git worktrees of one repository) never compete
//! for one port. The addon computes the same value (`addons/godot_mcp/core/bridge_port.gd`);
//! `tests/bridge-port-vectors.json` holds the cases both sides check.
//!
//! Canonical path: the project directory with symlinks resolved, without a trailing slash; on
//! Windows with `/` separators and ASCII letters in lower case. Port: FNV-1a 32 of its UTF-8
//! bytes, modulo [`PORT_SPAN`], plus [`FIRST_PORT`]. The range 10000–29999 lies below the
//! ephemeral ports of every supported OS and does not contain the legacy port 9333.

use std::path::Path;

pub const FIRST_PORT: u16 = 10000;
pub const PORT_SPAN: u16 = 20000;
const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

/// Canonical form of a project directory: symlinks resolved on Unix (Godot reads its project
/// directory back from `getcwd`, which resolves them too), made absolute on Windows.
pub fn canonical_project_path(dir: &Path) -> String {
    let resolved = if cfg!(windows) {
        std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
    } else {
        std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
    };
    normalize(&resolved.to_string_lossy(), cfg!(windows))
}

/// Text rules of the canonical path, applied after the file system resolved it.
pub fn normalize(path: &str, windows: bool) -> String {
    let mut text = path.to_string();
    if windows {
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            text = rest.to_string();
        }
        text = text.replace('\\', "/").to_ascii_lowercase();
    }
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    text
}

pub fn port_for(canonical_path: &str) -> u16 {
    let hash = canonical_path.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    });
    // The remainder is below PORT_SPAN, so the sum fits in a u16.
    FIRST_PORT + (hash % u32::from(PORT_SPAN)) as u16
}

/// The port the addon of the project in `dir` listens on when no port is configured.
pub fn derived_port(dir: &Path) -> u16 {
    port_for(&canonical_project_path(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const VECTORS: &str = include_str!("../tests/bridge-port-vectors.json");

    fn vectors() -> Value {
        serde_json::from_str(VECTORS).expect("valid vector file")
    }

    #[test]
    fn the_shared_vectors_hold_for_the_port_formula() {
        let vectors = vectors();
        let ports = vectors["ports"].as_array().expect("ports");
        assert!(ports.len() >= 5);
        for case in ports {
            let path = case["path"].as_str().expect("path");
            let port = case["port"].as_u64().expect("port");
            assert_eq!(u64::from(port_for(path)), port, "{path:?}");
        }
    }

    #[test]
    fn the_shared_vectors_hold_for_the_path_rules() {
        let vectors = vectors();
        for case in vectors["normalize"].as_array().expect("normalize") {
            let input = case["input"].as_str().expect("input");
            let windows = case["windows"].as_bool().expect("windows");
            let canonical = case["canonical"].as_str().expect("canonical");
            assert_eq!(normalize(input, windows), canonical, "{input:?}");
        }
    }

    #[test]
    fn every_derived_port_is_in_range_and_never_the_legacy_port() {
        for i in 0..5000 {
            let port = port_for(&format!("/work/project-{i}"));
            assert!(
                (FIRST_PORT..FIRST_PORT + PORT_SPAN).contains(&port),
                "{port}"
            );
            assert_ne!(port, 9333);
        }
    }

    #[test]
    fn worktrees_of_one_repository_get_different_ports() {
        let main = port_for("/Users/me/work/game");
        let worktree = port_for("/Users/me/work/game/.claude/worktrees/model-a");
        let other = port_for("/Users/me/work/game/.claude/worktrees/model-b");
        assert_ne!(main, worktree);
        assert_ne!(worktree, other);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_path_and_a_trailing_slash_give_the_same_port() {
        let root = tempfile::tempdir().expect("tempdir");
        let real = root.path().join("real");
        std::fs::create_dir(&real).expect("mkdir");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let canonical = canonical_project_path(&real);
        assert_eq!(canonical_project_path(&link), canonical);
        assert_eq!(
            canonical_project_path(Path::new(&format!("{}/", link.display()))),
            canonical
        );
        assert!(!canonical.ends_with('/'), "{canonical}");
        assert!(Path::new(&canonical).is_absolute(), "{canonical}");
        assert_eq!(derived_port(&link), derived_port(&real));
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_directory_still_gets_a_port() {
        let port = derived_port(Path::new("/no/such/godot/project/"));
        assert_eq!(port, port_for("/no/such/godot/project"));
    }
}
