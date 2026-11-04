//! Godot's exit-time leak reports in a headless run's output: repeats collapsed and counted.
//!
//! A SceneTree script that creates nodes and never frees them ends with the lines Godot prints
//! while it shuts down (`Leaked instance dependency`, `N RID allocations of type ... were leaked
//! at exit`, ...), the same line hundreds of times. They bury the errors that matter, so each
//! repeated report is shown once with its count, every report is counted by kind, and the answer
//! says what causes them. No report is dropped: the leaks are the script's to fix.

use std::collections::{BTreeMap, HashMap};

/// What the answer says when exit-time leak reports appear.
pub(super) const EXIT_LEAK_HINT: &str = "Godot reported leaks at exit (exit_leaks): the script left objects unfreed. A Node made with instantiate() or new() that is not in the tree, or was removed from it, is not freed by quit(); call free() on it before quit(). Run Godot with --verbose to list the leaked instances.";

/// Whether the text of a message is one kind of exit-time report.
type Matcher = fn(&str) -> bool;

/// The exit-time reports, by the name `exit_leaks` gives them, and how each is recognised in the
/// text after Godot's `ERROR: ` or `WARNING: ` prefix.
const KINDS: &[(&str, Matcher)] = &[
    ("leaked_instance_dependency", |text| {
        text.starts_with("Leaked instance dependency:")
    }),
    ("rid_allocations", |text| {
        text.contains(" RID allocations of type ") && text.contains("were leaked at exit")
    }),
    ("objectdb_instances", |text| {
        text.contains(" ObjectDB instance") && text.contains("leaked at exit")
    }),
    ("resources_in_use", |text| {
        text.contains("resources still in use at exit")
    }),
    ("paged_allocator", |text| {
        text.starts_with("Pages in use exist at exit in PagedAllocator")
    }),
];

/// The kind of exit-time report `line` starts, if it starts one.
fn kind_of(line: &str) -> Option<&'static str> {
    let plain = strip_ansi(line);
    let trimmed = plain.trim();
    let text = trimmed
        .strip_prefix("ERROR:")
        .or_else(|| trimmed.strip_prefix("WARNING:"))?
        .trim_start();
    KINDS
        .iter()
        .find(|(_, matches)| matches(text))
        .map(|(kind, _)| *kind)
}

fn is_source_line(line: &str) -> bool {
    strip_ansi(line).trim_start().starts_with("at:")
}

/// One piece of the output: a line kept as it is, or an exit-time report (with the `at:` line
/// Godot prints under it) and how many times it was printed.
enum Piece<'a> {
    Line(&'a str),
    Report {
        line: &'a str,
        source: Option<&'a str>,
        times: usize,
    },
}

/// `text` with each exit-time report printed once, where it first appeared, and `×N` after it
/// when Godot printed the same report (and the same `at:` line) N times. Every other line is
/// kept exactly as it was. Each report printed is counted in `counts` by kind.
pub(super) fn collapse_exit_leaks(
    text: &str,
    counts: &mut BTreeMap<&'static str, usize>,
) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut pieces: Vec<Piece> = Vec::with_capacity(lines.len());
    let mut seen: HashMap<(String, Option<String>), usize> = HashMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        let Some(kind) = kind_of(line) else {
            pieces.push(Piece::Line(line));
            continue;
        };
        *counts.entry(kind).or_insert(0) += 1;
        let source = lines
            .get(index)
            .copied()
            .filter(|next| is_source_line(next));
        if source.is_some() {
            index += 1;
        }
        let key = (
            line.trim_end().to_string(),
            source.map(|at| at.trim_end().to_string()),
        );
        if let Some(Piece::Report { times, .. }) =
            seen.get(&key).and_then(|&first| pieces.get_mut(first))
        {
            *times += 1;
            continue;
        }
        seen.insert(key, pieces.len());
        pieces.push(Piece::Report {
            line,
            source,
            times: 1,
        });
    }
    let mut shown = String::with_capacity(text.len());
    for piece in pieces {
        match piece {
            Piece::Line(line) => shown.push_str(line),
            Piece::Report {
                line,
                source,
                times,
            } => {
                if times > 1 {
                    let body = line.trim_end_matches(['\n', '\r']);
                    shown.push_str(body);
                    shown.push_str(&format!(" ×{times}"));
                    shown.push_str(&line[body.len()..]);
                } else {
                    shown.push_str(line);
                }
                if let Some(source) = source {
                    shown.push_str(source);
                }
            }
        }
    }
    shown
}

fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        // CSI: ESC [ parameters, ended by a letter.
        if chars.next() == Some('[') {
            for end in chars.by_ref() {
                if end.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPENDENCY: &str = "WARNING: Leaked instance dependency: Bug - did not call instance_notify_deleted when freeing.\n     at: ~DependencyTracker (servers/rendering/renderer_rd/storage_rd/utilities.h:79)\n";

    const SHUTDOWN_TAIL: &str = "\
WARNING: 5 ObjectDB instances were leaked at exit (run with `--verbose` for details).
   at: cleanup (core/object/object.cpp:2609)
ERROR: 3 resources still in use at exit (run with --verbose for details).
   at: clear (core/io/resource.cpp:599)
ERROR: 7 RID allocations of type 'N12MeshStorage4MeshE' were leaked at exit.
ERROR: Pages in use exist at exit in PagedAllocator: N7Skeleton
   at: ~PagedAllocator (./core/templates/paged_allocator.h:170)
";

    #[test]
    fn shows_a_repeated_report_once_with_its_count_and_keeps_the_rest() {
        let mut counts = BTreeMap::new();
        let text = format!("placed 12 rocks\n{}{SHUTDOWN_TAIL}", DEPENDENCY.repeat(148));
        let expected = format!(
            "placed 12 rocks\nWARNING: Leaked instance dependency: Bug - did not call instance_notify_deleted when freeing. ×148\n     at: ~DependencyTracker (servers/rendering/renderer_rd/storage_rd/utilities.h:79)\n{SHUTDOWN_TAIL}"
        );
        assert_eq!(collapse_exit_leaks(&text, &mut counts), expected);
        assert_eq!(
            counts,
            BTreeMap::from([
                ("leaked_instance_dependency", 148),
                ("objectdb_instances", 1),
                ("paged_allocator", 1),
                ("resources_in_use", 1),
                ("rid_allocations", 1),
            ])
        );
    }

    #[test]
    fn keeps_every_other_line_even_when_it_repeats() {
        let other = "\
ERROR: Failed to load resource 'res://zones/missing.tscn'.
   at: _load (core/io/resource_loader.cpp:283)
ERROR: Failed to load resource 'res://zones/missing.tscn'.
   at: _load (core/io/resource_loader.cpp:283)
SCRIPT ERROR: Invalid call. Nonexistent function 'foo'.
   at: _initialize (res://place.gd:14)
leaked at exit, said the script
";
        let mut counts = BTreeMap::new();
        assert_eq!(collapse_exit_leaks(other, &mut counts), other);
        assert!(counts.is_empty());
    }

    #[test]
    fn collapses_repeats_that_other_lines_interrupt_at_the_first_one() {
        let mut counts = BTreeMap::new();
        let text = format!("{DEPENDENCY}between\n{DEPENDENCY}");
        let expected = format!(
            "{}between\n",
            DEPENDENCY.replacen("freeing.\n", "freeing. ×2\n", 1)
        );
        assert_eq!(collapse_exit_leaks(&text, &mut counts), expected);
        assert_eq!(counts, BTreeMap::from([("leaked_instance_dependency", 2)]));
    }

    #[test]
    fn different_reports_of_one_kind_stay_apart() {
        let mut counts = BTreeMap::new();
        let text = "ERROR: 2 RID allocations of type 'A' were leaked at exit.\nERROR: 4 RID allocations of type 'B' were leaked at exit.\nERROR: 2 RID allocations of type 'A' were leaked at exit.";
        assert_eq!(
            collapse_exit_leaks(text, &mut counts),
            "ERROR: 2 RID allocations of type 'A' were leaked at exit. ×2\nERROR: 4 RID allocations of type 'B' were leaked at exit.\n"
        );
        assert_eq!(counts, BTreeMap::from([("rid_allocations", 3)]));
    }

    #[test]
    fn recognises_colored_reports() {
        let mut counts = BTreeMap::new();
        let text = "\u{1b}[1;33mWARNING:\u{1b}[0;93m 1 ObjectDB instance was leaked at exit (run with `--verbose` for details).\u{1b}[0m\n\u{1b}[0;90m     at: cleanup (core/object/object.cpp:2577)\u{1b}[0m\nnext\n";
        assert_eq!(collapse_exit_leaks(text, &mut counts), text);
        assert_eq!(counts, BTreeMap::from([("objectdb_instances", 1)]));
    }
}
