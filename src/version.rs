//! The version the server reports: the manifest version, with the commit it was built from as
//! SemVer build metadata (`0.13.0-dev+godot.4.8.g6295b51ab`).
//!
//! The manifest version only changes in a release commit, so two builds between releases would
//! otherwise report the same number. The commit comes from `build.rs`; a build without git has
//! none and reports the manifest version alone. Release archives are built from a checkout, so
//! they name their commit too.

/// `version` with `build`, a short lowercase hex commit id, added as build metadata. Anything
/// else (no id, an id that is not hex) leaves `version` as it is: the id comes from a command's
/// output and ends up in a version string that clients and reports read.
pub fn with_build(version: &str, build: Option<&str>) -> String {
    let Some(build) = build else {
        return version.to_string();
    };
    let is_commit = (7..=40).contains(&build.len())
        && build
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !is_commit {
        return version.to_string();
    }
    let separator = if version.contains('+') { '.' } else { '+' };
    format!("{version}{separator}g{build}")
}

/// The version of this build: the manifest version and, when known, its commit.
pub fn build_version() -> String {
    with_build(env!("CARGO_PKG_VERSION"), option_env!("GODOT_MCP_BUILD"))
}
