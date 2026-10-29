//! Whether a script can be the main loop of `godot -s`: it must extend `SceneTree` or
//! `MainLoop`, directly or through its base scripts. Godot 4.8 prints an error and exits with
//! code 0 when it cannot use the script, so the check runs before Godot is started.

use crate::paths::{res_display, resolve_in_project};
use std::path::Path;

const MAIN_LOOP_CLASSES: [&str; 2] = ["SceneTree", "MainLoop"];
/// Base scripts followed before giving up and letting Godot decide.
const MAX_BASE_DEPTH: usize = 16;
const GLOBAL_CLASS_CACHE: &str = ".godot/global_script_class_cache.cfg";

/// What a script's `extends` names.
enum Base {
    /// No `extends`: the script extends `RefCounted`.
    Missing,
    Class(String),
    Path(String),
    /// Something this check does not follow (an inner class, an unusual header); Godot decides.
    Unclear,
}

/// Why `godot -s` would not run `script`, or `None` when it extends `SceneTree` or `MainLoop`
/// or the check cannot tell (Godot's own load error is reported after the run then).
pub(super) fn main_loop_problem(project: &Path, script: &Path) -> Option<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut current = script.to_path_buf();
    for _ in 0..MAX_BASE_DEPTH {
        let source = std::fs::read_to_string(&current).ok()?;
        let shown = res_display(project, &current);
        match declared_base(&source) {
            Base::Unclear => return None,
            Base::Missing => {
                chain.push(format!(
                    "{shown} has no extends line, so it extends RefCounted"
                ));
                return Some(refusal(project, &chain, false));
            }
            Base::Class(name) if MAIN_LOOP_CLASSES.contains(&name.as_str()) => return None,
            Base::Class(name) => match global_class_script(project, &name) {
                Some(next) => {
                    chain.push(format!(
                        "{shown} extends {name} ({})",
                        res_display(project, &next)
                    ));
                    current = next;
                }
                None => {
                    chain.push(format!("{shown} extends {name}"));
                    return Some(refusal(project, &chain, true));
                }
            },
            Base::Path(path) => {
                let next = extends_path(project, &current, &path)?;
                chain.push(format!("{shown} extends \"{path}\""));
                current = next;
            }
        }
    }
    None
}

fn refusal(project: &Path, chain: &[String], named_class: bool) -> String {
    let mut text = format!(
        "{}, not SceneTree or MainLoop. godot -s runs only a script that extends SceneTree or MainLoop; Godot would print an error and exit with code 0 without running this one, so it was not started.",
        chain.join("; ")
    );
    if named_class && !project.join(GLOBAL_CLASS_CACHE).is_file() {
        text.push_str(&format!(
            " The project has no {GLOBAL_CLASS_CACHE} yet, so no class_name is known: if the base is a class of the project, import it first (godot_resave_resources, or godot --headless --import)."
        ));
    }
    text
}

/// What the script's header declares: annotations, `class_name` and `extends` come before any
/// other statement.
fn declared_base(source: &str) -> Base {
    for raw in source.lines() {
        let mut line = raw.trim();
        loop {
            if line.is_empty() || line.starts_with('#') {
                break;
            }
            if let Some(rest) = line.strip_prefix('@') {
                let mut after = &rest[identifier_len(rest)..];
                if after.starts_with('(') {
                    let Some(close) = after.find(')') else {
                        return Base::Unclear;
                    };
                    after = &after[close + 1..];
                }
                line = after.trim_start();
                continue;
            }
            if let Some(rest) = keyword(line, "class_name") {
                let rest = rest.trim_start();
                let length = identifier_len(rest);
                if length == 0 {
                    return Base::Unclear;
                }
                let rest = rest[length..].trim_start();
                line = rest.strip_prefix(';').unwrap_or(rest).trim_start();
                continue;
            }
            if let Some(rest) = keyword(line, "extends") {
                return extends_target(rest.trim_start());
            }
            return Base::Missing;
        }
    }
    Base::Missing
}

fn extends_target(text: &str) -> Base {
    if let Some(quote) = text.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let body = &text[1..];
        let Some(end) = body.find(quote) else {
            return Base::Unclear;
        };
        if body[end + 1..].trim_start().starts_with('.') {
            return Base::Unclear;
        }
        return Base::Path(body[..end].to_string());
    }
    let length = identifier_len(text);
    if length == 0 || text[length..].trim_start().starts_with('.') {
        return Base::Unclear;
    }
    Base::Class(text[..length].to_string())
}

fn keyword<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(word)?;
    (identifier_len(rest) == 0).then_some(rest)
}

fn identifier_len(text: &str) -> usize {
    text.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len())
}

/// The file an `extends "path"` names: a `res://` path, or one relative to the script.
fn extends_path(project: &Path, script: &Path, path: &str) -> Option<std::path::PathBuf> {
    let requested = if path.starts_with("res://") {
        path.to_string()
    } else {
        let directory = script.parent()?;
        res_display(project, &directory.join(path))
    };
    resolve_in_project(project, &requested)
        .ok()
        .filter(|file| file.is_file())
}

/// The script of a `class_name` listed in the project's global class cache.
fn global_class_script(project: &Path, name: &str) -> Option<std::path::PathBuf> {
    let cache = std::fs::read_to_string(project.join(GLOBAL_CLASS_CACHE)).ok()?;
    let path = cache.split('{').skip(1).find_map(|entry| {
        let entry = entry.split('}').next()?;
        if cache_value(entry, "class")? != name {
            return None;
        }
        cache_value(entry, "path")
    })?;
    resolve_in_project(project, path)
        .ok()
        .filter(|file| file.is_file())
}

/// A `"key": &"value"` or `"key": "value"` field of one cache entry.
fn cache_value<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    let rest = entry.split_once(&format!("\"{key}\":"))?.1.trim_start();
    let rest = rest.strip_prefix('&').unwrap_or(rest).strip_prefix('"')?;
    rest.split('"').next()
}
