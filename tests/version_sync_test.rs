//! The server and the addon of a release line carry one version string, and every file that
//! names the line's Godot version agrees with it. The line is the Godot `major.minor` named by
//! the build metadata of the version: `0.5.0+godot.4.6` belongs to the Godot 4.6 line, and
//! `0.5.0-dev+godot.4.8` to the development line for Godot 4.8.

use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("cannot read {relative}: {err}"))
}

/// The Godot `(major, minor)` of this line, taken from the crate version's build metadata.
fn line() -> (u32, u32) {
    let version = env!("CARGO_PKG_VERSION");
    let parsed = version
        .split_once('+')
        .and_then(|(_, metadata)| metadata.strip_prefix("godot."))
        .and_then(|godot| godot.split_once('.'))
        .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)));
    parsed.unwrap_or_else(|| {
        panic!(
            "version {version} must end with +godot.M.N naming the Godot line \
             (Cargo.toml and addons/godot_mcp/plugin.cfg)"
        )
    })
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
fn version_names_the_godot_line_and_is_released_or_dev() {
    let (major, minor) = line();
    let version = env!("CARGO_PKG_VERSION");
    let core = version.split_once('+').map_or(version, |(core, _)| core);
    if let Some((_, pre_release)) = core.split_once('-') {
        assert_eq!(
            pre_release, "dev",
            "version {version}: the only pre-release allowed is -dev, on the development line \
             (Cargo.toml and addons/godot_mcp/plugin.cfg)"
        );
    }
    assert_eq!(
        version.split_once('+').map(|(_, metadata)| metadata),
        Some(format!("godot.{major}.{minor}").as_str()),
        "version {version}: the build metadata must be exactly godot.M.N"
    );
}

#[test]
fn plugin_minimum_godot_version_is_the_line() {
    let (major, minor) = line();
    let plugin_gd = read("addons/godot_mcp/plugin.gd");
    let constant = |name: &str| -> u32 {
        plugin_gd
            .lines()
            .find_map(|line| line.trim().strip_prefix(&format!("const {name}: int =")))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_else(|| panic!("addons/godot_mcp/plugin.gd has no `const {name}: int = N`"))
    };

    assert_eq!(
        (constant("MIN_MAJOR"), constant("MIN_MINOR")),
        (major, minor),
        "MIN_MAJOR/MIN_MINOR in addons/godot_mcp/plugin.gd must equal the line {major}.{minor}"
    );
}

#[test]
fn dev_project_features_name_the_line() {
    let (major, minor) = line();
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
        format!("{major}.{minor}"),
        "the first config/features entry in project.godot must be the line {major}.{minor}"
    );
}

#[test]
fn ci_godot_build_belongs_to_the_line() {
    let (major, minor) = line();
    let build = read(".github/godot-version");
    let build = build.trim();
    let prefix = format!("{major}.{minor}");
    let rest = build.strip_prefix(&prefix);

    assert!(
        matches!(rest, Some("")) || rest.is_some_and(|rest| rest.starts_with(['.', '-'])),
        ".github/godot-version names Godot build {build:?}, which is not part of the line {prefix}"
    );
}

#[test]
fn release_workflow_starts_on_release_tags_only() {
    // A release is published from a plain `vX.Y.Z` tag. The tags of the release lines
    // (`vX.Y.Z+godot.M.N`) only anchor each line's sources and must not start one.
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
            "release.yml tag filter \"v{pattern} would also match line tags"
        );
        for (at, _) in pattern.match_indices('+') {
            assert!(
                pattern[..at].ends_with(']'),
                "release.yml tag filter \"v{pattern} has a `+` that does not follow a character class"
            );
        }
    }
}

#[test]
fn supported_lines_are_listed_and_the_development_line_is_not() {
    let (major, minor) = line();
    let lines = read(".github/lines");
    let listed: Vec<&str> = lines
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .collect();

    assert!(
        !listed.is_empty(),
        ".github/lines lists no Godot line, so a release would contain nothing"
    );
    for entry in &listed {
        let valid = entry
            .split_once('.')
            .is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok());
        assert!(
            valid,
            ".github/lines has {entry:?}, which is not a Godot M.N"
        );
    }
    if env!("CARGO_PKG_VERSION").contains("-dev") {
        assert!(
            !listed.contains(&format!("{major}.{minor}").as_str()),
            "the development line {major}.{minor} must not be listed in .github/lines: it is never released"
        );
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
        .and_then(|rest| rest.split("4. **Release commit on every line.**").next())
        .expect("release skill step 3 (choose the version number)");
    assert!(
        step.contains("repo-steward/scripts/readiness.sh"),
        "step 3 must name the readiness script that computes the number:\n{step}"
    );
    assert!(
        skill.contains("## Between Releases: the Development Version"),
        "the release skill must say when master's -dev number moves"
    );
}

#[test]
fn development_version_is_newer_than_the_last_release() {
    // `master` is never released, but it must report a number above the newest release in the
    // changelog; otherwise a build of `master` claims to be an older release. A release commit
    // on `master` moves the development version to the next patch with `-dev`.
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
