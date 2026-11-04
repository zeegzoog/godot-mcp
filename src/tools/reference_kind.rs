//! Whether a GDScript file that names a file only writes it.
//!
//! A tool script that generates a scene names the scene's path to save it, not to use it, and
//! the script recreates the file when it runs again; deleting that file breaks nothing. A path
//! is "written" when every place it reaches in the script's code is a write (`ResourceSaver.save`,
//! `FileAccess.open` for writing, `DirAccess` removal, `Image.save_png` and the like), directly or
//! through a constant or variable that holds it, and at least one is. Printing the path, checking
//! that it exists or making its folder change nothing. Anything else, and anything the analysis
//! cannot follow, is "used". The addon classifies the same way (`file_ops.gd`), and
//! `tests/script-reference-vectors.json` holds the cases both sides check.

/// How far a path is followed through constants and variables that hold it.
const MAX_CARRIERS: usize = 4;
/// Calls that write the file named by an argument: receiver (empty for any), method, argument.
const WRITES: &[(&str, &str, usize)] = &[
    ("ResourceSaver", "save", 1),
    ("DirAccess", "remove_absolute", 0),
    ("DirAccess", "copy_absolute", 1),
    ("DirAccess", "rename_absolute", 1),
    ("OS", "move_to_trash", 0),
    ("ProjectSettings", "save_custom", 0),
    ("", "save_png", 0),
    ("", "save_jpg", 0),
    ("", "save_webp", 0),
    ("", "save_exr", 0),
];
/// `FileAccess` openers; they write when their second argument is one of `WRITE_MODES`.
const OPENERS: &[&str] = &[
    "open",
    "open_compressed",
    "open_encrypted",
    "open_encrypted_with_pass",
];
const WRITE_MODES: &[&str] = &["FileAccess.WRITE", "FileAccess.WRITE_READ"];
/// Calls that only look at the path: receiver (empty for any), method.
const LOOKS: &[(&str, &str)] = &[
    ("FileAccess", "file_exists"),
    ("ResourceLoader", "exists"),
    ("", "print"),
    ("", "prints"),
    ("", "printt"),
    ("", "printerr"),
    ("", "print_rich"),
    ("", "print_verbose"),
    ("", "push_warning"),
    ("", "push_error"),
];
/// Calls that only look at the folder of the path (`path.get_base_dir()`).
const FOLDER_LOOKS: &[(&str, &str)] = &[
    ("DirAccess", "make_dir_recursive_absolute"),
    ("DirAccess", "make_dir_absolute"),
    ("DirAccess", "dir_exists_absolute"),
];
/// Calls that hand back the same path in another form.
const SAME_PATH: &[(&str, &str)] = &[
    ("ProjectSettings", "globalize_path"),
    ("ProjectSettings", "localize_path"),
];

/// How a file names a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The file only writes it, so it can recreate it.
    Written,
    /// Anything else: the file may need it.
    Used,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Written => "written",
            Kind::Used => "used",
        }
    }
}

/// What one place in the code does with the path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Write,
    Look,
    Use,
}

fn combine(roles: impl IntoIterator<Item = Role>) -> Role {
    let mut result = Role::Look;
    for role in roles {
        match role {
            Role::Use => return Role::Use,
            Role::Write => result = Role::Write,
            Role::Look => {}
        }
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    Text(&'a str),
    Mark(&'a str),
    Newline,
}

/// How a GDScript file names `needle` (a `res://` path or `uid://`), which its text mentions.
pub fn classify_script(source: &str, needle: &str) -> Kind {
    let tokens = lex(source);
    let mut roles = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let Token::Text(value) = token else {
            continue;
        };
        if !super::files::mentions(value, needle) {
            continue;
        }
        if *value != needle {
            return Kind::Used;
        }
        roles.push(role(&tokens, at, at + 1, 0, false));
    }
    // A mention outside the code's strings (in a comment, say) is no place the analysis follows.
    if roles.is_empty() {
        return Kind::Used;
    }
    match combine(roles) {
        Role::Write => Kind::Written,
        Role::Look | Role::Use => Kind::Used,
    }
}

/// Splits GDScript into words, string contents, marks and line ends; comments are dropped, and
/// so are line ends inside brackets, as GDScript ignores them there.
fn lex(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut brackets = 0usize;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        match byte {
            b'\n' => {
                if brackets == 0 {
                    tokens.push(Token::Newline);
                }
                at += 1;
            }
            b' ' | b'\t' | b'\r' => at += 1,
            b'\\' if bytes.get(at + 1) == Some(&b'\n') => at += 2,
            b'#' => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'"' | b'\'' => {
                let triple = bytes.get(at + 1) == Some(&byte) && bytes.get(at + 2) == Some(&byte);
                let open = if triple { 3 } else { 1 };
                let start = at + open;
                let mut end = start;
                loop {
                    if end >= bytes.len() {
                        break;
                    }
                    if bytes[end] == b'\\' {
                        end += 2;
                        continue;
                    }
                    if triple {
                        if bytes[end] == byte
                            && bytes.get(end + 1) == Some(&byte)
                            && bytes.get(end + 2) == Some(&byte)
                        {
                            break;
                        }
                    } else if bytes[end] == byte || bytes[end] == b'\n' {
                        break;
                    }
                    end += 1;
                }
                let end = end.min(bytes.len());
                tokens.push(Token::Text(&source[start..end]));
                at = if end < bytes.len() && bytes[end] == byte {
                    end + open
                } else {
                    end
                };
            }
            _ if byte.is_ascii_alphanumeric() || byte == b'_' => {
                let start = at;
                while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                    at += 1;
                }
                tokens.push(Token::Word(&source[start..at]));
            }
            _ => {
                let pair = source.get(at..at + 2).unwrap_or("");
                let width = if [":=", "==", "!=", "<=", ">=", "->"].contains(&pair) {
                    2
                } else {
                    source[at..].chars().next().map_or(1, char::len_utf8)
                };
                match byte {
                    b'(' | b'[' | b'{' => brackets += 1,
                    b')' | b']' | b'}' => brackets = brackets.saturating_sub(1),
                    _ => {}
                }
                tokens.push(Token::Mark(&source[at..at + width]));
                at += width;
            }
        }
    }
    tokens
}

fn mark(tokens: &[Token], at: usize, text: &str) -> bool {
    tokens.get(at) == Some(&Token::Mark(text))
}

fn word<'a>(tokens: &[Token<'a>], at: usize) -> Option<&'a str> {
    match tokens.get(at) {
        Some(Token::Word(text)) => Some(text),
        _ => None,
    }
}

/// The call whose argument list holds `tokens[at]`: (receiver or "", method, opening index,
/// argument index). `at` is the first token of the argument.
fn enclosing_call<'a>(tokens: &[Token<'a>], at: usize) -> Option<(&'a str, &'a str, usize, usize)> {
    let mut nested = 0usize;
    let mut argument = 0usize;
    let mut index = at;
    while index > 0 {
        index -= 1;
        match tokens[index] {
            Token::Mark(")" | "]" | "}") => nested += 1,
            Token::Mark("(") if nested == 0 => {
                let method = word(tokens, index.checked_sub(1)?)?;
                let receiver = if index >= 3 && mark(tokens, index - 2, ".") {
                    word(tokens, index - 3).unwrap_or("")
                } else {
                    ""
                };
                return Some((receiver, method, index, argument));
            }
            Token::Mark("[" | "{") if nested == 0 => return None,
            Token::Mark("(" | "[" | "{") => nested -= 1,
            Token::Mark(",") if nested == 0 => argument += 1,
            _ => {}
        }
    }
    None
}

/// The tokens of argument `wanted` of the call opened at `open`, joined.
fn argument_text(tokens: &[Token], open: usize, wanted: usize) -> String {
    let mut nested = 0usize;
    let mut argument = 0usize;
    let mut text = String::new();
    for token in &tokens[open + 1..] {
        match token {
            Token::Mark("(" | "[" | "{") => nested += 1,
            Token::Mark(")" | "]" | "}") if nested == 0 => break,
            Token::Mark(")" | "]" | "}") => nested -= 1,
            Token::Mark(",") if nested == 0 => {
                argument += 1;
                continue;
            }
            _ => {}
        }
        if argument == wanted {
            match token {
                Token::Word(part) | Token::Mark(part) => text.push_str(part),
                Token::Text(_) => text.push('"'),
                Token::Newline => {}
            }
        }
    }
    text
}

fn listed(list: &[(&str, &str)], receiver: &str, method: &str) -> bool {
    list.iter()
        .any(|(class, name)| *name == method && (class.is_empty() || *class == receiver))
}

/// What the expression `tokens[start..end]` does with the path, or with its folder when `folder`.
fn role(tokens: &[Token], start: usize, end: usize, carriers: usize, folder: bool) -> Role {
    // path.get_base_dir() is the folder: making it or checking it changes nothing.
    if !folder
        && mark(tokens, end, ".")
        && word(tokens, end + 1) == Some("get_base_dir")
        && mark(tokens, end + 2, "(")
        && mark(tokens, end + 3, ")")
    {
        return role(tokens, start, end + 4, carriers, true);
    }
    let before = start.checked_sub(1).map(|at| tokens[at]);
    let whole = matches!(before, Some(Token::Mark("(" | ",")))
        && matches!(tokens.get(end), Some(Token::Mark(")" | ",")));

    if let Some((receiver, method, open, argument)) = enclosing_call(tokens, start) {
        if listed(LOOKS, receiver, method) {
            return Role::Look;
        }
        if !whole {
            return Role::Use;
        }
        if listed(SAME_PATH, receiver, method) && argument == 0 {
            let from = if receiver.is_empty() {
                open - 1
            } else {
                open - 3
            };
            return match argument_end(tokens, open) {
                Some(close) => role(tokens, from, close + 1, carriers, folder),
                None => Role::Use,
            };
        }
        if folder {
            return if listed(FOLDER_LOOKS, receiver, method) && argument == 0 {
                Role::Look
            } else {
                Role::Use
            };
        }
        let writes = WRITES.iter().any(|(class, name, index)| {
            *name == method && *index == argument && (class.is_empty() || *class == receiver)
        });
        let opens = receiver == "FileAccess"
            && OPENERS.contains(&method)
            && argument == 0
            && WRITE_MODES.contains(&argument_text(tokens, open, 1).as_str());
        return if writes || opens {
            Role::Write
        } else {
            Role::Use
        };
    }

    // `const NAME := <path>` or `var NAME: Type = <path>`: follow NAME.
    let ends_statement = matches!(
        tokens.get(end),
        None | Some(Token::Newline | Token::Mark(";"))
    );
    if !ends_statement || !matches!(before, Some(Token::Mark("=" | ":="))) {
        return Role::Use;
    }
    let assign = start - 1;
    let declared = [assign.checked_sub(2), assign.checked_sub(4)]
        .into_iter()
        .flatten()
        .find(|&keyword| {
            matches!(word(tokens, keyword), Some("var" | "const"))
                && word(tokens, keyword + 1).is_some()
                && (keyword + 2 == assign
                    || (mark(tokens, keyword + 2, ":")
                        && word(tokens, keyword + 3).is_some()
                        && keyword + 4 == assign))
        });
    let Some(keyword) = declared else {
        return Role::Use;
    };
    if carriers >= MAX_CARRIERS {
        return Role::Use;
    }
    let name = word(tokens, keyword + 1).unwrap_or_default();
    let uses: Vec<usize> = (0..tokens.len())
        .filter(|&at| {
            at != keyword + 1
                && word(tokens, at) == Some(name)
                && !(at > 0 && mark(tokens, at - 1, "."))
        })
        .collect();
    if uses.is_empty() {
        return Role::Use;
    }
    combine(
        uses.into_iter()
            .map(|at| role(tokens, at, at + 1, carriers + 1, folder)),
    )
}

/// The index of the `)` that closes the call opened at `open`.
fn argument_end(tokens: &[Token], open: usize) -> Option<usize> {
    let mut nested = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open + 1) {
        match token {
            Token::Mark("(" | "[" | "{") => nested += 1,
            Token::Mark(")" | "]" | "}") if nested == 0 => return Some(index),
            Token::Mark(")" | "]" | "}") => nested -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const VECTORS: &str = include_str!("../../tests/script-reference-vectors.json");

    #[test]
    fn the_shared_vectors_hold() {
        let vectors: Value = serde_json::from_str(VECTORS).expect("valid vector file");
        let cases = vectors["cases"].as_array().expect("cases");
        assert!(cases.len() >= 10);
        for case in cases {
            let script = case["script"].as_str().expect("script");
            let path = case["path"].as_str().expect("path");
            let kind = case["kind"].as_str().expect("kind");
            assert_eq!(
                classify_script(script, path).as_str(),
                kind,
                "{}:\n{script}",
                case["name"]
            );
        }
    }
}
