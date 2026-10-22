//! Path and identifier checks shared by the tools that touch the file system.

use std::path::{Component, Path, PathBuf};

const RES_PREFIX: &str = "res://";

/// Matches `^[A-Za-z_][A-Za-z0-9_]*$`, the only shape accepted for class and node identifiers.
pub fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Checks that `dir` exists and is the root of a Godot project.
pub fn ensure_project_dir(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!(
            "Project directory does not exist: {}",
            dir.display()
        ));
    }
    if !dir.join("project.godot").is_file() {
        return Err(format!(
            "Not a Godot project directory (project.godot not found): {}",
            dir.display()
        ));
    }
    Ok(())
}

/// Finds the project the server was started in: the first of `cwd` and its ancestors that
/// contains `project.godot`, otherwise the only direct subdirectory of `cwd` that does (for a
/// repository that keeps the game in a folder such as `app/`).
pub fn find_project(cwd: &Path) -> Result<PathBuf, String> {
    if let Some(dir) = cwd
        .ancestors()
        .find(|dir| dir.join("project.godot").is_file())
    {
        return Ok(dir.to_path_buf());
    }

    let mut children: Vec<PathBuf> = std::fs::read_dir(cwd)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.join("project.godot").is_file())
                .collect()
        })
        .unwrap_or_default();
    children.sort();

    match children.as_slice() {
        [single] => Ok(single.clone()),
        [] => Err(format!(
            "No project.godot found in {} or its parent directories; pass project_path",
            cwd.display()
        )),
        several => Err(format!(
            "Several Godot projects found in {} ({}); pass project_path to choose one",
            cwd.display(),
            several
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A file of the project as the caller knows it, `res://relative/path`; the path as it is when
/// the file is not inside the project.
pub fn res_display(project: &Path, file: &Path) -> String {
    match file.strip_prefix(project) {
        Ok(relative) => format!("res://{}", relative.to_string_lossy().replace('\\', "/")),
        Err(_) => file.display().to_string(),
    }
}

/// Whether two paths name the same directory: compared after resolving links where the
/// directory exists, with `\` read as `/`, no trailing separator, no Windows verbatim prefix and,
/// on Windows, no difference of case.
pub fn same_directory(a: &Path, b: &Path) -> bool {
    fn normalized(path: &Path) -> String {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut text = resolved.to_string_lossy().replace('\\', "/");
        if let Some(rest) = text.strip_prefix("//?/") {
            text = rest.to_string();
        }
        while text.len() > 1 && text.ends_with('/') {
            text.pop();
        }
        if cfg!(windows) {
            text = text.to_lowercase();
        }
        text
    }
    normalized(a) == normalized(b)
}

/// Resolves a `res://` path, a project-relative path, or an absolute path to a location
/// inside `project`.
///
/// Rejects `..` segments (a name merely containing two dots is fine) and anything that lands
/// outside the project, including through symlinks of the part that already exists.
pub fn resolve_in_project(project: &Path, requested: &str) -> Result<PathBuf, String> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err("Path is empty".into());
    }

    let relative = trimmed.strip_prefix(RES_PREFIX).unwrap_or(trimmed);
    let normalized = relative.replace('\\', "/");
    let path = Path::new(&normalized);

    let is_absolute = path.has_root()
        || path
            .components()
            .any(|component| matches!(component, Component::Prefix(_)));
    let full = if is_absolute {
        path.to_path_buf()
    } else {
        project.join(path)
    };

    let mut resolved = PathBuf::new();
    for component in full.components() {
        match component {
            Component::ParentDir => {
                return Err(format!(
                    "Parent directory segments ('..') are not allowed: {requested}"
                ));
            }
            Component::CurDir => {}
            other => resolved.push(other.as_os_str()),
        }
    }

    let project_root = project.canonicalize().map_err(|err| {
        format!(
            "Cannot access project directory {}: {err}",
            project.display()
        )
    })?;
    let existing_ancestor = resolved
        .ancestors()
        .find(|candidate| candidate.exists())
        .and_then(|candidate| candidate.canonicalize().ok());
    match existing_ancestor {
        Some(ancestor) if ancestor.starts_with(&project_root) => Ok(resolved),
        _ => Err(format!(
            "Path is outside the project directory {}: {requested}",
            project.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("project.godot"), "config_version=5\n").expect("write");
        dir
    }

    #[test]
    fn the_same_directory_is_recognized_in_any_spelling() {
        let dir = project();
        let other = project();
        let plain = dir.path().to_path_buf();
        let slashed = PathBuf::from(format!("{}/", plain.to_string_lossy().replace('\\', "/")));
        assert!(same_directory(&plain, &slashed));
        assert!(same_directory(&plain, &plain.join(".")));
        assert!(!same_directory(&plain, other.path()));
        // A directory that does not exist is compared by its spelling.
        assert!(same_directory(
            Path::new("/no/such/dir"),
            Path::new("/no/such/dir/")
        ));
        assert!(!same_directory(
            Path::new("/no/such/a"),
            Path::new("/no/such/b")
        ));
    }

    #[test]
    fn identifiers_follow_the_strict_pattern() {
        for valid in ["Node", "_private", "Player2D", "a"] {
            assert!(is_valid_identifier(valid), "{valid}");
        }
        for invalid in ["", "2D", "Foo Bar", "Foo\nBar", "a-b", "Node;", "ä"] {
            assert!(!is_valid_identifier(invalid), "{invalid:?}");
        }
    }

    #[test]
    fn res_prefix_and_backslashes_resolve_under_the_project() {
        let dir = project();
        let resolved = resolve_in_project(dir.path(), "res://scripts\\player.gd").expect("resolve");
        assert_eq!(resolved, dir.path().join("scripts").join("player.gd"));
    }

    #[test]
    fn parent_segments_are_rejected_but_double_dots_in_names_are_not() {
        let dir = project();
        assert!(resolve_in_project(dir.path(), "a/../b.gd").is_err());
        assert!(resolve_in_project(dir.path(), "../b.gd").is_err());
        assert!(resolve_in_project(dir.path(), "my..file.gd").is_ok());
    }

    #[test]
    fn absolute_paths_must_stay_inside_the_project() {
        let dir = project();
        let outside = tempfile::tempdir().expect("outside");

        let inside_file = dir.path().join("sub").join("a.gd");
        assert!(resolve_in_project(dir.path(), &inside_file.to_string_lossy()).is_ok());

        let outside_file = outside.path().join("a.gd");
        assert!(resolve_in_project(dir.path(), &outside_file.to_string_lossy()).is_err());
    }

    #[test]
    fn empty_paths_are_rejected() {
        let dir = project();
        assert!(resolve_in_project(dir.path(), "  ").is_err());
    }

    #[test]
    fn project_directory_needs_project_godot() {
        let dir = project();
        assert!(ensure_project_dir(dir.path()).is_ok());

        let empty = tempfile::tempdir().expect("empty");
        let err = ensure_project_dir(empty.path()).expect_err("no project.godot");
        assert!(err.contains("project.godot"));

        let missing = empty.path().join("missing");
        assert!(ensure_project_dir(&missing).is_err());
    }

    #[test]
    fn find_project_walks_up_from_the_working_directory() {
        let dir = project();
        let deep = dir.path().join("scenes").join("levels");
        std::fs::create_dir_all(&deep).expect("mkdir");

        assert_eq!(find_project(dir.path()).expect("cwd"), dir.path());
        assert_eq!(find_project(&deep).expect("ancestor"), dir.path());
    }

    #[test]
    fn find_project_accepts_a_single_child_project() {
        let root = tempfile::tempdir().expect("root");
        let app = root.path().join("app");
        std::fs::create_dir_all(&app).expect("mkdir app");
        std::fs::write(app.join("project.godot"), "config_version=5\n").expect("write");
        std::fs::create_dir_all(root.path().join("docs")).expect("mkdir docs");

        assert_eq!(find_project(root.path()).expect("child"), app);
    }

    #[test]
    fn find_project_prefers_the_working_directory_over_children() {
        let dir = project();
        let nested = dir.path().join("demo");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("project.godot"), "config_version=5\n").expect("write");

        assert_eq!(find_project(dir.path()).expect("cwd"), dir.path());
    }

    #[test]
    fn find_project_without_a_project_asks_for_project_path() {
        let empty = tempfile::tempdir().expect("empty");
        let err = find_project(empty.path()).expect_err("no project");
        assert!(err.contains("project_path"), "{err}");
    }

    #[test]
    fn find_project_lists_several_child_projects_instead_of_guessing() {
        let root = tempfile::tempdir().expect("root");
        for name in ["game", "tools"] {
            let child = root.path().join(name);
            std::fs::create_dir_all(&child).expect("mkdir");
            std::fs::write(child.join("project.godot"), "config_version=5\n").expect("write");
        }

        let err = find_project(root.path()).expect_err("ambiguous");
        assert!(err.contains("game") && err.contains("tools"), "{err}");
        assert!(err.contains("project_path"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_be_used_to_leave_the_project() {
        let dir = project();
        let outside = tempfile::tempdir().expect("outside");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");
        assert!(resolve_in_project(dir.path(), "link/a.gd").is_err());
    }
}
