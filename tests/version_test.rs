//! The version a running server reports: the manifest version, with the commit it was built from
//! added as build metadata, so builds between two releases can be told apart without a version
//! change in every pull request.

use godot_mcp::version::{build_version, with_build};

#[test]
fn a_build_commit_becomes_semver_build_metadata() {
    assert_eq!(
        with_build("0.12.1-dev+godot.4.8", Some("6295b51ab")),
        "0.12.1-dev+godot.4.8.g6295b51ab",
        "the commit joins the metadata the version already has"
    );
    assert_eq!(
        with_build("0.13.0", Some("abc1234")),
        "0.13.0+gabc1234",
        "a version without metadata gets some"
    );
}

#[test]
fn no_commit_leaves_the_version_alone() {
    for build in [None, Some(""), Some("  ")] {
        assert_eq!(
            with_build("0.13.0+godot.4.7", build),
            "0.13.0+godot.4.7",
            "build {build:?}"
        );
    }
}

#[test]
fn only_a_hex_commit_is_taken_as_build_metadata() {
    // The text comes from `git`, but it ends up in a version string that reports and clients
    // read: anything that is not a short hex id is dropped.
    for build in [
        "",
        "main",
        "6295b51 ",
        "g1\n2",
        "../x",
        "6295b51+1",
        "ABCDEF1",
    ] {
        assert_eq!(with_build("0.1.0", Some(build)), "0.1.0", "build {build:?}");
    }
}

#[test]
fn the_reported_version_starts_with_the_manifest_version() {
    let manifest = env!("CARGO_PKG_VERSION");
    let reported = build_version();
    assert!(
        reported.starts_with(manifest),
        "{reported} must start with {manifest}"
    );
    let extra = &reported[manifest.len()..];
    assert!(
        extra.is_empty() || extra.trim_start_matches(['.', '+']).starts_with('g'),
        "only a commit may follow the manifest version: {reported}"
    );
}

#[test]
fn a_checkout_names_the_commit_it_was_built_from() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let git_available = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    // A source archive has no .git and builds without a commit: nothing to name then.
    if !root.join(".git").exists() || !git_available {
        eprintln!("skipped: not a git checkout, or no git");
        return;
    }
    let head = std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--short=9", "HEAD"])
        .output()
        .expect("git rev-parse");
    let head = String::from_utf8(head.stdout).expect("utf8");
    let reported = build_version();
    // The last identifier of the build metadata is `g` and the commit. (Not a substring test:
    // "godot" in the line's metadata also contains a `g`.)
    let last = reported.rsplit(['.', '+']).next().unwrap_or_default();
    let commit = last
        .strip_prefix('g')
        .unwrap_or_else(|| panic!("a build from a checkout names its commit: {reported}"));
    // The commit checked out when the build script ran may be older than HEAD now, so only the
    // shape is compared.
    assert!(
        commit.chars().all(|c| c.is_ascii_hexdigit()) && commit.len() == head.trim().len(),
        "{reported} vs {head}"
    );
}
