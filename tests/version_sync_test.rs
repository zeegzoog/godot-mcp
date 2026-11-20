//! The server and the addon carry one version string, and the Godot versions the project supports
//! are stated once and agree everywhere: the addon refuses an editor older than `MIN_MINOR`,
//! `TESTED_MINOR` is the newest Godot the end-to-end tests run against, and CI runs them against
//! every minor version in between. A release is `X.Y.Z`; `master` between releases is `X.Y.Z-dev`.

use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("cannot read {relative}: {err}"))
}

/// The value of `const NAME: int = N` in the addon's plugin script.
fn plugin_constant(name: &str) -> u32 {
    read("addons/godot_mcp/plugin.gd")
        .lines()
        .find_map(|line| line.trim().strip_prefix(&format!("const {name}: int =")))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| panic!("addons/godot_mcp/plugin.gd has no `const {name}: int = N`"))
}

/// The minor versions of the Godot builds in `ci.yml`'s `godot-versions` matrix
/// (`godot: [4.5.2-stable, 4.6.3-stable]`).
fn matrix_minors() -> Vec<u32> {
    let workflow = read(".github/workflows/ci.yml");
    let list = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("godot: ["))
        .and_then(|rest| rest.split(']').next())
        .expect("ci.yml has no `godot: [...]` matrix of Godot builds");
    list.split(',')
        .map(|build| {
            build
                .trim()
                .split('.')
                .nth(1)
                .and_then(|minor| minor.parse().ok())
                .unwrap_or_else(|| panic!("ci.yml matrix entry {build:?} is not a Godot build"))
        })
        .collect()
}

/// The minor version of the build in `.github/godot-version`, the one CI's main job runs.
fn newest_build_minor() -> u32 {
    let build = read(".github/godot-version");
    build
        .trim()
        .split('.')
        .nth(1)
        .and_then(|minor| minor.split('-').next()?.parse().ok())
        .unwrap_or_else(|| panic!(".github/godot-version names {:?}", build.trim()))
}

#[test]
fn test_plugin_cfg_version_matches_cargo() {
    let plugin_cfg = include_str!("../addons/godot_mcp/plugin.cfg");
    let plugin_version = plugin_cfg
        .lines()
        .find_map(|line| line.trim().strip_prefix("version="))
        .map(|value| value.trim_matches('"'))
        .expect("plugin.cfg has no version= entry");

    assert_eq!(
        plugin_version,
        env!("CARGO_PKG_VERSION"),
        "addons/godot_mcp/plugin.cfg version must match Cargo.toml; bump both together"
    );
}

#[test]
fn version_is_a_release_or_dev() {
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        !version.contains('+'),
        "version {version}: a version carries no build metadata (the Godot version is not part \
         of it; a build adds its commit at run time). Cargo.toml and addons/godot_mcp/plugin.cfg"
    );
    if let Some((_, pre_release)) = version.split_once('-') {
        assert_eq!(
            pre_release, "dev",
            "version {version}: the only pre-release allowed is -dev, on master between releases"
        );
    }
    assert!(
        triple(version.split('-').next().unwrap_or(version)).is_some(),
        "version {version} is not X.Y.Z"
    );
}

#[test]
fn the_oldest_supported_godot_is_what_the_dev_project_and_ci_start_from() {
    let min = plugin_constant("MIN_MINOR");
    assert_eq!(
        plugin_constant("MIN_MAJOR"),
        4,
        "the project supports Godot 4 only"
    );
    let project = read("project.godot");
    let feature = project
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("config/features=PackedStringArray(")
        })
        .and_then(|list| list.split('"').nth(1))
        .unwrap_or_else(|| panic!("project.godot has no config/features entry"));
    assert_eq!(
        feature,
        format!("4.{min}"),
        "the dev project is saved with the oldest supported Godot, 4.{min}: a newer editor opens \
         it and an older one does not warn about a newer project"
    );
    assert!(
        matrix_minors().contains(&min),
        "CI's godot-versions matrix must run the oldest supported Godot, 4.{min}"
    );
}

#[test]
fn ci_runs_every_supported_godot_minor_version() {
    let min = plugin_constant("MIN_MINOR");
    let tested = plugin_constant("TESTED_MINOR");
    assert!(
        min <= tested,
        "MIN_MINOR {min} is above TESTED_MINOR {tested}"
    );
    assert_eq!(
        newest_build_minor(),
        tested,
        ".github/godot-version is the build of CI's main job: its minor version must be TESTED_MINOR {tested}"
    );
    let matrix = matrix_minors();
    for minor in min..tested {
        assert!(
            matrix.contains(&minor),
            "no Godot 4.{minor} build in ci.yml's godot-versions matrix: every minor version from \
             4.{min} up to the one before 4.{tested} is tested there"
        );
    }
    assert!(
        !matrix.contains(&tested),
        "Godot 4.{tested} is the main job; keep it out of the matrix so it runs once"
    );
}

#[test]
fn release_workflow_starts_on_release_tags_only() {
    // A release is published from a plain `vX.Y.Z` tag.
    // In a GitHub Actions filter pattern `+` is a quantifier of the preceding character, so it is
    // valid after a character class (`[0-9]+`) and invalid after `*`: GitHub then rejects the
    // whole workflow file and no tag ever starts a release.
    let workflow = read(".github/workflows/release.yml");
    let patterns: Vec<&str> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- \"v"))
        .collect();

    assert!(
        !patterns.is_empty(),
        "release.yml has no tag filter starting with v"
    );
    for pattern in patterns {
        assert!(
            !pattern.contains("godot"),
            "release.yml tag filter \"v{pattern} must match plain version tags only"
        );
        for (at, _) in pattern.match_indices('+') {
            assert!(
                pattern[..at].ends_with(']'),
                "release.yml tag filter \"v{pattern} has a `+` that does not follow a character class"
            );
        }
    }
}

/// Parses `X.Y.Z` into a comparable triple.
fn triple(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.split('.');
    let triple = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(triple)
}

#[test]
fn the_release_skill_takes_the_number_from_the_readiness_script() {
    // The number a release takes follows the Conventional Commit types since the last release,
    // and is computed by one script instead of being judged by hand in each release.
    let skill = read(".agents/skills/release/SKILL.md");
    assert!(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".agents/skills/repo-steward/scripts/readiness.sh")
            .is_file(),
        "the readiness script the release skill names does not exist"
    );
    let step = skill
        .split("3. **Choose the version number.**")
        .nth(1)
        .and_then(|rest| rest.split("4. **Release commit.**").next())
        .expect("release skill step 3 (choose the version number)");
    assert!(
        step.contains("repo-steward/scripts/readiness.sh"),
        "step 3 must name the readiness script that computes the number:\n{step}"
    );
    assert!(
        skill.contains("move master to the next development version"),
        "the release skill must say that master moves to -dev right after a release"
    );
}

#[test]
fn development_version_is_newer_than_the_last_release() {
    // `master` between releases must report a number above the newest release in the changelog;
    // otherwise a build of `master` claims to be an older release. The commit after a release
    // moves the version to the next patch with `-dev`.
    let version = env!("CARGO_PKG_VERSION");
    if !version.contains("-dev") {
        return;
    }
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let current = triple(core).unwrap_or_else(|| panic!("version {version} has no X.Y.Z core"));

    let changelog = read("CHANGELOG.md");
    let newest = changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## ["))
        .filter_map(|rest| triple(rest.split(']').next()?))
        .max()
        .expect("CHANGELOG.md has no `## [X.Y.Z]` release heading");

    assert!(
        current > newest,
        "development version {version} is not newer than the newest released {}.{}.{} in \
         CHANGELOG.md; set Cargo.toml and addons/godot_mcp/plugin.cfg to the next patch with \
         -dev and run cargo build",
        newest.0,
        newest.1,
        newest.2
    );
}
