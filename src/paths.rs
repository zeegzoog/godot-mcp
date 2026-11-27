//! Path and identifier checks shared by the tools that touch the file system.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf, Prefix};

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

/// Whether Godot's editor file system leaves the entry at `path` out of the project, so a walk
/// of project files must skip it: a hidden entry (a name starting with `.`, which covers
/// `.godot/` and `.git/`), a directory holding a `.gdignore` file, or a directory holding its own
/// `project.godot` (a nested project). Apply it to the entries under the project root, never to
/// the root itself.
pub fn skipped_by_godot(path: &Path, is_dir: bool) -> bool {
    let hidden = path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'));
    hidden || (is_dir && (path.join(".gdignore").exists() || path.join("project.godot").exists()))
}

/// Whether two paths name the same directory: compared after resolving links where the
/// directory exists, with no trailing separator, no Windows verbatim prefix and, on Windows, `\`
/// read as `/` and no difference of case.
pub fn same_directory(a: &Path, b: &Path) -> bool {
    fn normalized(path: &Path) -> String {
        // A network path is compared by its spelling: resolving it would make Windows connect
        // to that host (and offer it the user's credentials) on behalf of whoever reported it.
        let resolved = if is_network_path(path) {
            path.to_path_buf()
        } else {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        };
        let mut text = host_text(&resolved);
        if let Some(rest) = text.strip_prefix("//?/") {
            // `\\?\UNC\host\share` is another spelling of `\\host\share`.
            text = match rest.get(..4) {
                Some(head) if head.eq_ignore_ascii_case("UNC/") => format!("//{}", &rest[4..]),
                _ => rest.to_string(),
            };
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

/// `path` as text with the separators of this system read as `/`: on Windows `\` too, while on
/// Unix a `\` is an ordinary character of a file name and stays one.
fn host_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

/// Whether `path` names a network share or a Windows device rather than a local file:
/// `//host/share` and, on Windows, `\\host\share`, `\\?\UNC\...` or `\\.\device`. A verbatim
/// local path (`\\?\C:\...`) is not one.
fn is_network_path(path: &Path) -> bool {
    let text = host_text(path);
    match text.strip_prefix("//?/") {
        Some(rest) => rest
            .get(..4)
            .is_some_and(|head| head.eq_ignore_ascii_case("UNC/")),
        None => text.starts_with("//"),
    }
}

/// The components of `path`, each with the key it is compared by without touching the file
/// system: a Windows prefix in one spelling (verbatim or not), the root as `/`, no `.` segments
/// and, on Windows, lower case. The second item is the component as written.
fn comparable_components(path: &Path) -> Vec<(String, OsString)> {
    let fold = |text: String| {
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => {
                let key = match prefix.kind() {
                    Prefix::Verbatim(name) => name.to_string_lossy().into_owned(),
                    Prefix::VerbatimUNC(host, share) | Prefix::UNC(host, share) => {
                        format!("//{}/{}", host.to_string_lossy(), share.to_string_lossy())
                    }
                    Prefix::VerbatimDisk(disk) | Prefix::Disk(disk) => {
                        format!("{}:", char::from(disk))
                    }
                    Prefix::DeviceNS(name) => format!("//./{}", name.to_string_lossy()),
                };
                Some((fold(key), prefix.as_os_str().to_os_string()))
            }
            Component::RootDir => Some(("/".to_string(), OsString::from("/"))),
            Component::CurDir => None,
            Component::ParentDir => Some(("..".to_string(), OsString::from(".."))),
            Component::Normal(name) => Some((
                fold(name.to_string_lossy().into_owned()),
                name.to_os_string(),
            )),
        })
        .collect()
}

/// The components of `path` below whichever of `roots` contains it, compared by spelling only;
/// `None` when no root contains it.
fn components_below(path: &Path, roots: &[&Path]) -> Option<Vec<OsString>> {
    let wanted = comparable_components(path);
    roots.iter().find_map(|root| {
        let root = comparable_components(root);
        let inside = wanted.len() >= root.len()
            && wanted
                .iter()
                .zip(&root)
                .all(|((key, _), (root_key, _))| key == root_key);
        inside.then(|| {
            wanted[root.len()..]
                .iter()
                .map(|(_, part)| part.clone())
                .collect()
        })
    })
}

/// Where an absolute path starts inside the project: the resolved project root and the
/// components below it when the path is spelled under the project, otherwise the resolved
/// deepest part of the path that exists and the rest (so a spelling of the project in another
/// case, or through a link to it, still counts). Never called for a network path. `None` when
/// the path is outside the project.
fn absolute_link_target(
    target: &Path,
    root: &Path,
    project: &Path,
) -> Option<(PathBuf, Vec<OsString>)> {
    if let Some(below) = components_below(target, &[root, project]) {
        return Some((root.to_path_buf(), below));
    }
    let existing = target.ancestors().find(|candidate| candidate.exists())?;
    let resolved = existing.canonicalize().ok()?;
    if !resolved.starts_with(root) {
        return None;
    }
    let below = components_below(target, &[existing])?;
    Some((resolved, below))
}

/// Whether `path` is a network path outside the project by its spelling. A project may itself lie
/// on a share the user chose (a WSL folder, a company share); every other share is foreign.
fn is_foreign_network_path(path: &Path, root: &Path, project: &Path) -> bool {
    is_network_path(path) && components_below(path, &[root, project]).is_none()
}

/// Whether a failed look at the entry `part` shows that nothing is there: it is missing, or its
/// name is one the system cannot give any entry (so it is no link either). A name that holds a
/// separator of this system is not judged: it is not one entry.
fn nothing_exists_at(part: &OsStr, kind: ErrorKind) -> bool {
    match kind {
        ErrorKind::NotFound | ErrorKind::NotADirectory => true,
        ErrorKind::InvalidFilename | ErrorKind::InvalidInput => {
            !part.to_string_lossy().chars().any(std::path::is_separator)
        }
        _ => false,
    }
}

/// How many symbolic links one path may pass through before it is refused, as on Linux.
const MAX_LINKS: usize = 40;

/// Follows `below` (components under `start`, a resolved folder inside the resolved project
/// `root`) the way the operating system would when the path is opened or created, and says
/// whether it stays inside `root`. A link is read, never followed blindly: its target is checked
/// by spelling before anything at it is looked at, so a link that points outside the project,
/// or at a network share the project does not lie on, is refused whether or not its target
/// exists.
fn stays_inside(
    root: &Path,
    project: &Path,
    start: PathBuf,
    below: Vec<OsString>,
) -> Result<(), &'static str> {
    let mut current = start;
    let mut pending: VecDeque<OsString> = below.into();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." || part.is_empty() {
            continue;
        }
        if part == ".." {
            if !current.pop() || !current.starts_with(root) {
                return Err("a link leads out of the project directory");
            }
            continue;
        }
        let next = current.join(&part);
        let meta = match std::fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(error) if nothing_exists_at(&part, error.kind()) => {
                // Nothing exists here: the rest is created under `current`, unless it climbs
                // back.
                return if pending.iter().any(|rest| rest == "..") {
                    Err("a link leads out of the project directory")
                } else {
                    Ok(())
                };
            }
            // What cannot be looked at may be a link: refuse rather than guess.
            Err(_) => return Err("a part of the path cannot be read"),
        };
        if !meta.file_type().is_symlink() {
            current = next;
            continue;
        }
        links += 1;
        if links > MAX_LINKS {
            return Err("too many levels of symbolic links");
        }
        let target = std::fs::read_link(&next).map_err(|_| "a symbolic link cannot be read")?;
        let absolute = target.has_root()
            || target
                .components()
                .any(|component| matches!(component, Component::Prefix(_)));
        let mut expanded: Vec<OsString> = if absolute {
            if is_foreign_network_path(&target, root, project) {
                return Err("a link points at a network path");
            }
            let (start, below) = absolute_link_target(&target, root, project)
                .ok_or("a link points outside the project directory")?;
            current = start;
            below
        } else {
            // Split by this system's own rules: on Unix `a\b` is one name.
            target
                .components()
                .filter_map(|component| match component {
                    Component::Normal(name) => Some(name.to_os_string()),
                    Component::ParentDir => Some(OsString::from("..")),
                    _ => None,
                })
                .collect()
        };
        expanded.extend(pending.drain(..));
        pending = expanded.into();
    }
    Ok(())
}

/// Resolves a `res://` path, a project-relative path, or an absolute path to a location
/// inside `project`.
///
/// Rejects `..` segments (a name merely containing two dots is fine) and anything that lands
/// outside the project, including through symlinks, whether their target exists or not. A
/// network path outside the project is refused by its spelling, before anything at it is looked
/// at; a local path spelled otherwise than the project (another case, a link to it) is compared
/// by the folder on disk it names.
pub fn resolve_in_project(project: &Path, requested: &str) -> Result<PathBuf, String> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err("Path is empty".into());
    }

    let relative = trimmed.strip_prefix(RES_PREFIX).unwrap_or(trimmed);
    let slashed = relative.replace('\\', "/");
    // A verbatim Windows path (`\\?\...`) knows only `\` as a separator: it is read as written.
    let normalized = if cfg!(windows) && relative.starts_with(r"\\?\") {
        relative.to_string()
    } else {
        slashed.clone()
    };
    let path = Path::new(&normalized);
    let outside = || {
        format!(
            "Path is outside the project directory {}: {requested}; pass a res:// path or a path inside the project",
            project.display()
        )
    };
    if trimmed.starts_with(RES_PREFIX) && slashed.starts_with("//") {
        return Err(outside());
    }

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
    // A foreign network path is refused by its spelling, before anything at it is looked at.
    if is_foreign_network_path(&resolved, &project_root, project) {
        return Err(outside());
    }
    let (start, below) =
        absolute_link_target(&resolved, &project_root, project).ok_or_else(outside)?;
    stays_inside(&project_root, project, start, below)
        .map(|()| resolved)
        .map_err(|why| format!("{} ({why})", outside()))
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
    fn what_godot_does_not_scan_is_skipped() {
        let dir = project();
        let root = dir.path();
        for folder in [".hidden", "docs", "nested", "assets"] {
            std::fs::create_dir(root.join(folder)).expect("mkdir");
        }
        std::fs::write(root.join("docs/.gdignore"), "").expect("write");
        std::fs::write(root.join("nested/project.godot"), "").expect("write");
        assert!(skipped_by_godot(&root.join(".hidden"), true));
        assert!(skipped_by_godot(&root.join(".godot"), true));
        assert!(skipped_by_godot(&root.join("docs"), true));
        assert!(skipped_by_godot(&root.join("nested"), true));
        assert!(skipped_by_godot(&root.join("assets/.x.png.import"), false));
        assert!(!skipped_by_godot(&root.join("assets"), true));
        assert!(!skipped_by_godot(&root.join("assets/x.png.import"), false));
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

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_cannot_lead_out_of_the_project() {
        let dir = project();
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir(dir.path().join("scripts")).expect("mkdir");
        let target = outside.path().join("evil.plist");
        std::os::unix::fs::symlink(&target, dir.path().join("scripts/x.gd")).expect("symlink");
        let err = resolve_in_project(dir.path(), "res://scripts/x.gd").expect_err("dangling link");
        assert!(err.contains("outside the project"), "{err}");

        // The same through a relative link and through a link to a directory that is missing.
        std::os::unix::fs::symlink(
            "../../../../../../../../../tmp/evil.gd",
            dir.path().join("rel.gd"),
        )
        .expect("symlink");
        assert!(resolve_in_project(dir.path(), "rel.gd").is_err());
        std::os::unix::fs::symlink(outside.path().join("gone"), dir.path().join("gone"))
            .expect("symlink");
        assert!(resolve_in_project(dir.path(), "gone/a.gd").is_err());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_network_path_is_refused() {
        let dir = project();
        std::os::unix::fs::symlink("//host/share/x.gd", dir.path().join("share.gd"))
            .expect("symlink");
        assert!(resolve_in_project(dir.path(), "share.gd").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_inside_the_project_still_resolve() {
        let dir = project();
        let root = dir.path();
        std::fs::create_dir_all(root.join("real/sub")).expect("mkdir");
        std::fs::write(root.join("real/a.gd"), "").expect("write");
        std::os::unix::fs::symlink(root.join("real"), root.join("abs")).expect("symlink");
        std::os::unix::fs::symlink("real", root.join("rel")).expect("symlink");
        std::os::unix::fs::symlink("../a.gd", root.join("real/sub/up.gd")).expect("symlink");
        // A dangling link whose target would be created inside the project.
        std::os::unix::fs::symlink("real/new.gd", root.join("new.gd")).expect("symlink");
        for path in [
            "abs/a.gd",
            "rel/a.gd",
            "rel/b.gd",
            "real/sub/up.gd",
            "new.gd",
        ] {
            assert_eq!(
                resolve_in_project(root, path).expect(path),
                root.join(path),
                "{path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_is_refused() {
        let dir = project();
        std::os::unix::fs::symlink("b", dir.path().join("a")).expect("symlink");
        std::os::unix::fs::symlink("a", dir.path().join("b")).expect("symlink");
        assert!(resolve_in_project(dir.path(), "a/x.gd").is_err());
    }

    /// On Unix a backslash is an ordinary character of a file name, so a link named `a\b` is
    /// one entry, never the folders `a` and `b`.
    #[cfg(unix)]
    #[test]
    fn a_backslash_in_a_link_name_does_not_hide_the_link() {
        let dir = project();
        let root = dir.path().canonicalize().expect("canonical project");
        let outside = tempfile::tempdir().expect("outside");
        let victim = outside.path().join("victim");
        std::os::unix::fs::symlink(&victim, root.join("a\\b")).expect("symlink");
        std::os::unix::fs::symlink("a\\b", root.join("L1")).expect("symlink");
        std::os::unix::fs::symlink(root.join("a\\b"), root.join("L2")).expect("symlink");
        for path in ["L1", "L2", "res://L1"] {
            let err = resolve_in_project(dir.path(), path).expect_err(path);
            assert!(err.contains("outside the project"), "{path}: {err}");
        }
        assert!(resolve_in_project(&root, "L2").is_err());
        assert!(!victim.exists());
    }

    /// A request may spell the project's folders in another case on a file system that ignores
    /// case (macOS, Windows): the folder on disk decides, as it did before links were walked.
    #[test]
    fn an_absolute_path_in_another_case_of_the_project_still_resolves() {
        let dir = project();
        let root = dir.path().canonicalize().expect("canonical project");
        let Some(name) = root
            .file_name()
            .map(|name| name.to_string_lossy().to_uppercase())
        else {
            return;
        };
        let shouted = root.with_file_name(&name);
        if shouted == root || !shouted.exists() {
            return; // A file system that tells case apart: nothing to check.
        }
        let requested = shouted.join("x.gd");
        let resolved =
            resolve_in_project(dir.path(), &requested.to_string_lossy()).expect("other case");
        assert_eq!(resolved, requested);

        std::fs::create_dir(root.join("sub")).expect("mkdir");
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().expect("outside");
            std::os::unix::fs::symlink(outside.path(), root.join("sub/out")).expect("symlink");
            let through = shouted.join("sub/out/x.gd");
            assert!(resolve_in_project(dir.path(), &through.to_string_lossy()).is_err());
        }
    }

    /// A part of the path that cannot be looked at is refused, not taken for a missing one.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = project();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).expect("mkdir");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let probe = std::fs::symlink_metadata(locked.join("x.gd"));
        let result = resolve_in_project(dir.path(), "locked/x.gd");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // Root reads every folder: then there is nothing to refuse.
        if probe
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        {
            assert!(result.is_err(), "{result:?}");
        }
    }

    /// A name the system cannot give a file (a NUL; on Windows a quote or a line break) cannot be
    /// a link either: the path resolves, and the tool that takes the name judges it.
    #[test]
    fn a_name_the_system_cannot_have_is_not_taken_for_a_link() {
        let dir = project();
        for requested in [
            "res://b\".gd",
            "res://b\n.gd",
            "res://b?.gd",
            "res://b\u{0}.gd",
            "res://dir\u{1}/b.gd",
        ] {
            let resolved = resolve_in_project(dir.path(), requested);
            assert!(resolved.is_ok(), "{requested:?}: {resolved:?}");
        }
    }

    #[test]
    fn network_and_device_paths_are_refused_by_their_spelling() {
        let dir = project();
        for path in [
            "res:////host/share/x.gd",
            "res://\\\\host\\share\\x.gd",
            "res://\\\\?\\UNC\\host\\share\\x.gd",
            "res://\\\\.\\pipe\\x",
            "\\\\host\\share\\x.gd",
            "//host/share/x.gd",
        ] {
            assert!(resolve_in_project(dir.path(), path).is_err(), "{path}");
        }
        // Without touching the share: nothing under it is ever below the project.
        assert!(components_below(Path::new("//host/share/x.gd"), &[dir.path()]).is_none());
        assert!(is_network_path(Path::new("//host/share")));
        assert!(!is_network_path(Path::new("/home/me/proj")));
        // On Unix a `\` is part of a file name, so only Windows reads these as shares.
        assert_eq!(is_network_path(Path::new("\\\\host\\share")), cfg!(windows));
        assert_eq!(
            is_network_path(Path::new("\\\\?\\UNC\\host\\share")),
            cfg!(windows)
        );
        assert!(!is_network_path(Path::new("\\\\?\\C:\\proj")));
    }

    /// A new project folder that is reached by a network path: inside the writable share named
    /// by `GODOT_MCP_TEST_SHARE` (for example a WSL folder), otherwise a local temporary folder
    /// spelled through the administrative share of its drive. `None` when neither can be used.
    #[cfg(windows)]
    fn shared_project() -> Option<(tempfile::TempDir, PathBuf)> {
        if let Some(share) = std::env::var_os("GODOT_MCP_TEST_SHARE") {
            let dir = tempfile::tempdir_in(share).expect("a folder in GODOT_MCP_TEST_SHARE");
            let path = dir.path().to_path_buf();
            return Some((dir, path));
        }
        let dir = project();
        let local = dir.path().canonicalize().expect("canonical project");
        let text = local.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        let (drive, rest) = text.split_once(":\\")?;
        let shared = PathBuf::from(format!(r"\\localhost\{drive}$\{rest}"));
        shared.is_dir().then_some((dir, shared))
    }

    /// A project may itself lie on a network share (a WSL folder, a company share): the user
    /// chose that place, so its own files resolve, while any other share stays refused.
    #[cfg(windows)]
    #[test]
    fn a_project_opened_by_a_network_path_keeps_its_own_files() {
        let Some((_keep, shared)) = shared_project() else {
            eprintln!("skipped: no network path to a writable folder");
            return;
        };
        let canonical = shared.canonicalize().expect("canonical share");
        for root in [&shared, &canonical] {
            for requested in [
                "res://a.gd".to_string(),
                "sub/a.gd".to_string(),
                root.join("a.gd").to_string_lossy().into_owned(),
            ] {
                let resolved = resolve_in_project(root, &requested)
                    .unwrap_or_else(|why| panic!("{}: {requested}: {why}", root.display()));
                assert!(resolved.ends_with("a.gd"), "{}", resolved.display());
            }
            for other in [
                "//other-host/share/x.gd",
                "res:////other-host/share/x.gd",
                "\\\\other-host\\share\\x.gd",
            ] {
                assert!(resolve_in_project(root, other).is_err(), "{other}");
            }
        }
        // The editor reports the project by one spelling, the server may hold the other.
        assert!(same_directory(&shared, &canonical));
    }

    #[test]
    fn a_network_path_is_compared_by_its_spelling() {
        assert!(same_directory(
            Path::new("//host/share/proj"),
            Path::new("//host/share/proj/")
        ));
        #[cfg(windows)]
        assert!(same_directory(
            Path::new("//host/share/proj"),
            Path::new("\\\\host\\share\\proj\\")
        ));
        assert!(!same_directory(
            Path::new("//host/share/proj"),
            Path::new("/host/share/proj")
        ));
    }
}
