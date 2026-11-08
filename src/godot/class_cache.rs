//! The project's global script class cache, `.godot/global_script_class_cache.cfg`.
//!
//! The editor writes this file whenever its file system scan sees a script with a
//! `class_name`. A headless run or a process run never scans: it registers exactly the classes
//! the cache lists, so a class written to disk without the editor is unknown to every other
//! script ("Could not find type") until something rewrites the cache. The script tools use
//! this module to register a class the way the editor would, and the validator uses it to tell
//! which declared classes the cache still lacks.
//!
//! Only the header of a script is read: the annotations, `class_name` and `extends` that come
//! before its first member, which is all the editor's own `get_global_class_name` looks at. A
//! header this reader is not sure about is reported as such, never guessed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The cache file, relative to the project directory.
pub const CACHE_FILE: &str = ".godot/global_script_class_cache.cfg";

/// The step that registers every class the way the editor does, named in answers.
pub const REGISTER_STEP: &str = "call godot_resave_resources (without the editor it runs `godot --headless --path <project> --import`, which rescans the project and rewrites the class cache), or open the project in the editor";

/// How deep `extends "res://…"` chains are followed before giving up.
const MAX_EXTENDS_DEPTH: usize = 32;

/// What a script header says about its global class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub class_name: Option<String>,
    pub extends: Extends,
    pub icon: Option<String>,
    pub is_tool: bool,
    pub is_abstract: bool,
}

/// The `extends` clause of a header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Extends {
    /// No `extends`: the script extends `RefCounted`.
    None,
    /// `extends Name`, a native or global class.
    Class(String),
    /// `extends "path"`.
    Path(String),
    /// A form the editor records no plain base for, such as `extends Outer.Inner`.
    Other,
}

/// A script's class as the cache stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassEntry {
    pub class: String,
    pub base: String,
    pub icon: String,
    pub is_abstract: bool,
    pub is_tool: bool,
    pub path: String,
}

/// What registering one script changed in the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registration {
    /// The script declares no class and the cache listed none for it.
    NoClass,
    /// The cache already listed this class for this script, unchanged.
    Unchanged(String),
    /// The class was added to the cache, or its entry was updated.
    Registered(String),
    /// The script no longer declares the class the cache listed for it; the entry was removed.
    Removed(String),
    /// The class name is already registered for another script, so nothing was written.
    Taken { class: String, other_path: String },
    /// The class could not be registered; the reason is for the answer.
    Failed(String),
}

impl Registration {
    /// One sentence for a tool's answer, or `None` when nothing is worth saying.
    pub fn note(&self, res_path: &str) -> Option<String> {
        match self {
            Self::NoClass => None,
            Self::Unchanged(class) => Some(format!(
                "Global class {class} is registered in {CACHE_FILE}."
            )),
            Self::Registered(class) => Some(format!(
                "Registered global class {class} ({res_path}) in {CACHE_FILE}, so headless runs and process runs know it."
            )),
            Self::Removed(class) => Some(format!(
                "Removed global class {class} from {CACHE_FILE}: {res_path} no longer declares it."
            )),
            Self::Taken { class, other_path } => Some(format!(
                "Warning: global class {class} is already registered for {other_path}, so {res_path} was not registered; Godot refuses two scripts with one class_name. Rename one of them."
            )),
            Self::Failed(reason) => Some(format!(
                "Warning: the class in {res_path} is not registered in {CACHE_FILE} yet ({reason}), so headless runs and process runs fail with \"Could not find type\" in scripts that use it. To register it, {REGISTER_STEP}."
            )),
        }
    }
}

/// Registers the class `text` declares for the script at `res_path` in the project's cache,
/// replacing whatever the cache listed for that path, the way the editor's scan does.
pub fn register(project: &Path, res_path: &str, text: &str) -> Registration {
    let header = match parse_header(text) {
        Ok(header) => header,
        Err(reason) => return Registration::Failed(reason),
    };
    let cache_path = project.join(CACHE_FILE);
    let mut cache = match std::fs::read_to_string(&cache_path) {
        Ok(text) => match Cache::parse(&text) {
            Ok(cache) => cache,
            Err(reason) => return Registration::Failed(format!("{CACHE_FILE}: {reason}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Cache::default(),
        Err(error) => return Registration::Failed(format!("cannot read {CACHE_FILE}: {error}")),
    };
    let previous = cache.class_of(res_path);
    let Some(class) = header.class_name.clone() else {
        let Some(previous) = previous else {
            return Registration::NoClass;
        };
        cache.remove_path(res_path);
        return match write_cache(&cache_path, &cache) {
            Ok(()) => Registration::Removed(previous),
            Err(reason) => Registration::Failed(reason),
        };
    };
    if let Some(other_path) = cache.path_of(&class).filter(|other| other != res_path) {
        // The editor drops an entry whose script is gone; a live one keeps the name.
        if project_file(project, &other_path).is_some_and(|file| file.is_file()) {
            return Registration::Taken { class, other_path };
        }
    }
    let entry = match entry_for(project, res_path, &header, &class, &cache) {
        Ok(entry) => entry,
        Err(reason) => return Registration::Failed(reason),
    };
    if cache.entry(res_path).as_ref() == Some(&entry) {
        return Registration::Unchanged(class);
    }
    cache.remove_path(res_path);
    cache.remove_class(&class);
    cache.insert(&entry);
    match write_cache(&cache_path, &cache) {
        Ok(()) => Registration::Registered(class),
        Err(reason) => Registration::Failed(reason),
    }
}

/// A class some script in the project declares that the cache does not list for that script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unregistered {
    pub class: String,
    pub path: String,
}

/// Every class declared by a `.gd` file in the project that the cache does not list for that
/// file. Walks the project as the editor's scan does ([`crate::paths::skipped_by_godot`]).
pub fn unregistered(project: &Path) -> Vec<Unregistered> {
    let cache = std::fs::read_to_string(project.join(CACHE_FILE))
        .ok()
        .and_then(|text| Cache::parse(&text).ok())
        .unwrap_or_default();
    let mut found = Vec::new();
    let mut pending = vec![project.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if crate::paths::skipped_by_godot(&path, kind.is_dir()) {
                continue;
            }
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && name.ends_with(".gd") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(Header {
                    class_name: Some(class),
                    ..
                }) = parse_header(&text)
                else {
                    continue;
                };
                let Some(res_path) = res_path(project, &path) else {
                    continue;
                };
                if cache.class_of(&res_path).as_deref() != Some(class.as_str()) {
                    found.push(Unregistered {
                        class,
                        path: res_path,
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| a.class.cmp(&b.class).then(a.path.cmp(&b.path)));
    found
}

/// Whether `text` mentions `word` as a whole identifier outside comments.
pub fn mentions(text: &str, word: &str) -> bool {
    text.lines().any(|line| {
        let code = strip_comment(line);
        code.match_indices(word).any(|(at, _)| {
            let before = code[..at].chars().next_back();
            let after = code[at + word.len()..].chars().next();
            !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char)
        })
    })
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The line without a trailing `#` comment; a `#` inside a string literal is kept.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return &line[..at],
            None => {}
        }
    }
    line
}

fn res_path(project: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(project).ok()?;
    Some(format!(
        "res://{}",
        relative.to_string_lossy().replace('\\', "/")
    ))
}

/// The file a `res://` path names inside the project, refusing anything that leaves it.
fn project_file(project: &Path, res_path: &str) -> Option<PathBuf> {
    let relative = res_path.strip_prefix("res://")?;
    let mut file = project.to_path_buf();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => file.push(part),
        }
    }
    Some(file)
}

/// A path written in a script (`extends "…"`, `@icon("…")`) as the `res://` path Godot reads:
/// a relative path is taken from the script's own directory.
fn resolve_script_relative(script_res_path: &str, written: &str) -> Option<String> {
    if written.starts_with("res://") {
        return Some(normalize_res(written));
    }
    if written.contains("://") || written.starts_with('/') {
        return None;
    }
    let dir = script_res_path
        .rsplit_once('/')
        .map_or("res:/", |(dir, _)| dir);
    Some(normalize_res(&format!("{dir}/{written}")))
}

fn normalize_res(path: &str) -> String {
    let rest = path.strip_prefix("res://").unwrap_or(path);
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("res://{}", parts.join("/"))
}

/// The entry the editor would store for `header`.
fn entry_for(
    project: &Path,
    res_path: &str,
    header: &Header,
    class: &str,
    cache: &Cache,
) -> Result<ClassEntry, String> {
    let base = base_of(project, res_path, &header.extends, cache, 0)?;
    let icon = match &header.icon {
        Some(icon) if !icon.is_empty() => resolve_script_relative(res_path, icon)
            .ok_or_else(|| format!("the @icon path {icon:?} is not a project path"))?,
        _ => String::new(),
    };
    Ok(ClassEntry {
        class: class.to_string(),
        base,
        icon,
        is_abstract: header.is_abstract,
        is_tool: header.is_tool,
        path: res_path.to_string(),
    })
}

/// The base class the editor records: the named class for `extends Name`, `RefCounted`
/// without `extends`, and for `extends "path"` the base that script records in turn.
fn base_of(
    project: &Path,
    res_path: &str,
    extends: &Extends,
    cache: &Cache,
    depth: usize,
) -> Result<String, String> {
    match extends {
        Extends::None => Ok("RefCounted".into()),
        Extends::Class(name) => Ok(name.clone()),
        Extends::Other => Err("its extends clause names an inner class".into()),
        Extends::Path(written) => {
            if depth >= MAX_EXTENDS_DEPTH {
                return Err("its extends chain is too long or loops".into());
            }
            let parent = resolve_script_relative(res_path, written)
                .ok_or_else(|| format!("extends {written:?} is not a project path"))?;
            if let Some(entry) = cache.entry(&parent) {
                return Ok(entry.base);
            }
            let file = project_file(project, &parent)
                .ok_or_else(|| format!("extends {written:?} leaves the project"))?;
            let text = std::fs::read_to_string(&file)
                .map_err(|error| format!("cannot read {parent}, which it extends: {error}"))?;
            let header = parse_header(&text).map_err(|reason| format!("{parent}: {reason}"))?;
            base_of(project, &parent, &header.extends, cache, depth + 1)
        }
    }
}

/// Writes the cache through a temporary file next to it, so a run never reads half of it.
fn write_cache(cache_path: &Path, cache: &Cache) -> Result<(), String> {
    let failure = |error: std::io::Error| format!("cannot write {CACHE_FILE}: {error}");
    if let Some(dir) = cache_path.parent() {
        std::fs::create_dir_all(dir).map_err(failure)?;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let temporary = cache_path.with_file_name(format!(
        ".global_script_class_cache.{:x}{nanos:x}.godot-mcp.tmp",
        std::process::id()
    ));
    std::fs::write(&temporary, cache.render()).map_err(failure)?;
    std::fs::rename(&temporary, cache_path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        failure(error)
    })
}

// ---------------------------------------------------------------------------------------------
// The cache file.

/// The parsed cache: each entry's fields as their literal text, in the order the file had,
/// so entries of other languages and fields this reader does not know are kept as written.
#[derive(Clone, Debug, Default)]
struct Cache {
    entries: Vec<Vec<(String, String)>>,
}

impl Cache {
    fn parse(text: &str) -> Result<Self, String> {
        let mut reader = Reader::new(text);
        reader.skip_space();
        if reader.at_end() {
            return Ok(Self::default());
        }
        if !reader.eat_word("list") {
            return Err("it does not start with list=".into());
        }
        reader.skip_space();
        reader.expect('=')?;
        reader.skip_space();
        reader.expect('[')?;
        let mut entries = Vec::new();
        loop {
            reader.skip_space();
            if reader.eat(']') {
                break;
            }
            reader.expect('{')?;
            let mut fields = Vec::new();
            loop {
                reader.skip_space();
                if reader.eat('}') {
                    break;
                }
                let key = reader.string()?;
                reader.skip_space();
                reader.expect(':')?;
                reader.skip_space();
                let value = reader.value()?;
                fields.push((key, value));
                reader.skip_space();
                if reader.eat(',') {
                    continue;
                }
                reader.skip_space();
                reader.expect('}')?;
                break;
            }
            entries.push(fields);
            reader.skip_space();
            if reader.eat(',') {
                continue;
            }
            reader.skip_space();
            reader.expect(']')?;
            break;
        }
        reader.skip_space();
        if !reader.at_end() {
            return Err("it has text after the class list".into());
        }
        Ok(Self { entries })
    }

    fn field(fields: &[(String, String)], key: &str) -> Option<String> {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| decode_string(value))
    }

    fn class_of(&self, path: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|fields| Self::field(fields, "path").as_deref() == Some(path))
            .and_then(|fields| Self::field(fields, "class"))
    }

    fn path_of(&self, class: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|fields| Self::field(fields, "class").as_deref() == Some(class))
            .and_then(|fields| Self::field(fields, "path"))
    }

    fn entry(&self, path: &str) -> Option<ClassEntry> {
        let fields = self
            .entries
            .iter()
            .find(|fields| Self::field(fields, "path").as_deref() == Some(path))?;
        let flag = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .is_some_and(|(_, value)| value.trim() == "true")
        };
        Some(ClassEntry {
            class: Self::field(fields, "class")?,
            base: Self::field(fields, "base").unwrap_or_default(),
            icon: Self::field(fields, "icon").unwrap_or_default(),
            is_abstract: flag("is_abstract"),
            is_tool: flag("is_tool"),
            path: path.to_string(),
        })
    }

    fn remove_path(&mut self, path: &str) {
        self.entries
            .retain(|fields| Self::field(fields, "path").as_deref() != Some(path));
    }

    fn remove_class(&mut self, class: &str) {
        self.entries
            .retain(|fields| Self::field(fields, "class").as_deref() != Some(class));
    }

    /// Adds the entry, keeping the list sorted by class name as the editor writes it.
    fn insert(&mut self, entry: &ClassEntry) {
        let fields = vec![
            ("base".to_string(), format!("&{}", quote(&entry.base))),
            ("class".to_string(), format!("&{}", quote(&entry.class))),
            ("icon".to_string(), quote(&entry.icon)),
            ("is_abstract".to_string(), entry.is_abstract.to_string()),
            ("is_tool".to_string(), entry.is_tool.to_string()),
            ("language".to_string(), "&\"GDScript\"".to_string()),
            ("path".to_string(), quote(&entry.path)),
        ];
        let at = self
            .entries
            .iter()
            .position(|other| {
                Self::field(other, "class").is_some_and(|name| name.as_str() > entry.class.as_str())
            })
            .unwrap_or(self.entries.len());
        self.entries.insert(at, fields);
    }

    /// The file text in the editor's own layout.
    fn render(&self) -> String {
        let entries: Vec<String> = self
            .entries
            .iter()
            .map(|fields| {
                let lines: Vec<String> = fields
                    .iter()
                    .map(|(key, value)| format!("{}: {value}", quote(key)))
                    .collect();
                format!("{{\n{}\n}}", lines.join(",\n"))
            })
            .collect();
        format!("list=[{}]\n", entries.join(", "))
    }
}

/// A Godot string literal for `text`, escaped as Godot's variant writer does.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The text of a string or StringName literal (`"…"` or `&"…"`), or `None` for other values.
fn decode_string(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.strip_prefix('&').unwrap_or(value);
    let mut reader = Reader::new(value);
    let text = reader.string().ok()?;
    reader.at_end().then_some(text)
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, at: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn at_end(&self) -> bool {
        self.at >= self.text.len()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn skip_space(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.at += c.len_utf8();
            } else if c == ';' {
                // A ConfigFile comment runs to the end of the line.
                let end = self
                    .rest()
                    .find('\n')
                    .map_or(self.text.len(), |n| self.at + n);
                self.at = end;
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.at += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}' at byte {}", self.at))
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if self.rest().starts_with(word) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

    /// A `"…"` literal, unescaped.
    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        while let Some((offset, c)) = chars.next() {
            match c {
                '"' => {
                    self.at += offset + 1;
                    return Ok(out);
                }
                '\\' => match chars.next() {
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, 'r')) => out.push('\r'),
                    Some((_, other)) => out.push(other),
                    None => break,
                },
                c => out.push(c),
            }
        }
        Err("a string literal is not closed".into())
    }

    /// One value as its literal text, up to the `,` or `}` that ends it.
    fn value(&mut self) -> Result<String, String> {
        let start = self.at;
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                '"' => {
                    self.string()?;
                    continue;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                '}' if depth == 0 => break,
                '}' => depth -= 1,
                ',' if depth == 0 => break,
                _ => {}
            }
            self.at += c.len_utf8();
        }
        let value = self.text[start..self.at].trim().to_string();
        if value.is_empty() {
            Err(format!("a value is missing at byte {start}"))
        } else {
            Ok(value)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Script headers.

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Annotation(String),
    Str(String),
    Punct(char),
    /// The end of a logical line (outside brackets).
    Newline,
}

/// Reads the header of a GDScript file: the annotations, `class_name` and `extends` before its
/// first member, as the editor's class scan does.
pub fn parse_header(text: &str) -> Result<Header, String> {
    let tokens = tokenize(text)?;
    let mut header = Header {
        class_name: None,
        extends: Extends::None,
        icon: None,
        is_tool: false,
        is_abstract: false,
    };
    let mut at = 0;
    let next_significant = |from: usize| {
        tokens[from..]
            .iter()
            .find(|token| !matches!(token, Token::Newline | Token::Punct(';')))
    };
    while at < tokens.len() {
        match &tokens[at] {
            Token::Newline | Token::Punct(';') | Token::Str(_) => at += 1,
            Token::Annotation(name) => match name.as_str() {
                "tool" => {
                    header.is_tool = true;
                    at += 1;
                }
                "static_unload" => at += 1,
                "abstract" => {
                    // `@abstract func` and `@abstract static func` annotate a member.
                    if matches!(next_significant(at + 1), Some(Token::Word(word)) if word != "class_name" && word != "extends")
                    {
                        break;
                    }
                    header.is_abstract = true;
                    at += 1;
                }
                "icon" => {
                    match (tokens.get(at + 1), tokens.get(at + 2), tokens.get(at + 3)) {
                        (
                            Some(Token::Punct('(')),
                            Some(Token::Str(path)),
                            Some(Token::Punct(')')),
                        ) => header.icon = Some(path.clone()),
                        _ => return Err("its @icon annotation is not a plain string".into()),
                    }
                    at += 4;
                }
                _ => break,
            },
            Token::Word(word) if word == "class_name" => {
                let Some(Token::Word(name)) = tokens.get(at + 1) else {
                    return Err("class_name is not followed by a name".into());
                };
                header.class_name = Some(name.clone());
                at += 2;
            }
            Token::Word(word) if word == "extends" => {
                at += 1;
                match tokens.get(at) {
                    Some(Token::Str(path)) => {
                        at += 1;
                        header.extends = if matches!(tokens.get(at), Some(Token::Punct('.'))) {
                            Extends::Other
                        } else {
                            Extends::Path(path.clone())
                        };
                    }
                    Some(Token::Word(name)) => {
                        at += 1;
                        header.extends = if matches!(tokens.get(at), Some(Token::Punct('.'))) {
                            Extends::Other
                        } else {
                            Extends::Class(name.clone())
                        };
                    }
                    _ => return Err("extends is not followed by a class or a path".into()),
                }
                while matches!(
                    tokens.get(at),
                    Some(Token::Punct('.')) | Some(Token::Word(_))
                ) {
                    at += 1;
                }
            }
            _ => break,
        }
    }
    Ok(header)
}

/// Tokens of the file's beginning; stops at the first token that cannot belong to a header
/// line, which is far enough for `parse_header`.
fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                if depth == 0 && tokens.last() != Some(&Token::Newline) {
                    tokens.push(Token::Newline);
                }
                i += 1;
            }
            '\\' if chars.get(i + 1) == Some(&'\n') => i += 2,
            c if c.is_whitespace() => i += 1,
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '"' | '\'' => {
                let (value, end) = read_string(&chars, i, false)?;
                tokens.push(Token::Str(value));
                i = end;
            }
            'r' if matches!(chars.get(i + 1), Some('"') | Some('\'')) => {
                let (value, end) = read_string(&chars, i + 1, true)?;
                tokens.push(Token::Str(value));
                i = end;
            }
            '@' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && is_identifier_char(chars[end]) {
                    end += 1;
                }
                tokens.push(Token::Annotation(chars[start..end].iter().collect()));
                i = end;
            }
            c if is_identifier_char(c) => {
                let start = i;
                while i < chars.len() && is_identifier_char(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                // The header never goes past the first member keyword.
                let stop = matches!(
                    word.as_str(),
                    "func" | "var" | "const" | "signal" | "enum" | "class" | "static" | "export"
                );
                tokens.push(Token::Word(word));
                if stop && depth == 0 {
                    break;
                }
            }
            '(' | '[' | '{' => {
                depth += 1;
                tokens.push(Token::Punct(c));
                i += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                tokens.push(Token::Punct(c));
                i += 1;
            }
            c => {
                tokens.push(Token::Punct(c));
                i += 1;
            }
        }
    }
    Ok(tokens)
}

/// A string literal starting at `chars[start]` (its opening quote): its value and the index
/// after it. Triple-quoted literals are read whole.
fn read_string(chars: &[char], start: usize, raw: bool) -> Result<(String, usize), String> {
    let quote = chars[start];
    let triple = chars.get(start + 1) == Some(&quote) && chars.get(start + 2) == Some(&quote);
    let mut i = start + if triple { 3 } else { 1 };
    let mut out = String::new();
    while i < chars.len() {
        let c = chars[i];
        if triple {
            if c == quote && chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote) {
                return Ok((out, i + 3));
            }
        } else if c == quote {
            return Ok((out, i + 1));
        } else if c == '\n' {
            break;
        }
        if c == '\\' && !raw {
            match chars.get(i + 1) {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(&other) => out.push(other),
                None => break,
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    Err("a string literal in its header is not closed".into())
}

/// Every class the cache lists, by class name: used by tests and diagnostics.
pub fn listed(project: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(project.join(CACHE_FILE)) else {
        return BTreeMap::new();
    };
    let Ok(cache) = Cache::parse(&text) else {
        return BTreeMap::new();
    };
    cache
        .entries
        .iter()
        .filter_map(|fields| {
            Some((
                Cache::field(fields, "class")?,
                Cache::field(fields, "path")?,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(text: &str) -> Header {
        parse_header(text).expect("header")
    }

    #[test]
    fn reads_the_header_forms_the_editor_registers() {
        let plain =
            header("extends RefCounted\nclass_name PartFlight\n\nfunc hi() -> int:\n\treturn 3\n");
        assert_eq!(plain.class_name.as_deref(), Some("PartFlight"));
        assert_eq!(plain.extends, Extends::Class("RefCounted".into()));

        let one_line =
            header("@tool\n@icon(\"res://icon.svg\")\nclass_name Kid extends PartFlight\n");
        assert_eq!(one_line.class_name.as_deref(), Some("Kid"));
        assert_eq!(one_line.extends, Extends::Class("PartFlight".into()));
        assert!(one_line.is_tool);
        assert_eq!(one_line.icon.as_deref(), Some("res://icon.svg"));

        let abstract_class = header("@abstract class_name Abs\nextends Node2D # comment\n");
        assert!(abstract_class.is_abstract);
        assert_eq!(abstract_class.extends, Extends::Class("Node2D".into()));

        let semicolon = header("class_name Two;extends Node3D\n");
        assert_eq!(semicolon.extends, Extends::Class("Node3D".into()));

        let doc = header("## Docs.\n\"\"\"doc\nmore\"\"\"\nclass_name AfterStr\n");
        assert_eq!(doc.class_name.as_deref(), Some("AfterStr"));
        assert_eq!(doc.extends, Extends::None);

        let by_path = header("extends 'res://part.gd'\nclass_name ByPath\n");
        assert_eq!(by_path.extends, Extends::Path("res://part.gd".into()));

        let inner = header("extends \"res://a.gd\".Inner\nclass_name InInner\n");
        assert_eq!(inner.extends, Extends::Other);
    }

    #[test]
    fn a_class_name_after_the_first_member_is_not_the_scripts_class() {
        let late = header("extends Node\n\nvar x := 1\nclass_name Late\n");
        assert_eq!(late.class_name, None);
        let abstract_member = header("extends Node\n@abstract func f() -> void\n");
        assert!(!abstract_member.is_abstract);
        let none = header("extends Node\n# class_name InComment\nfunc _ready():\n\tpass\n");
        assert_eq!(none.class_name, None);
    }

    #[test]
    fn mentions_whole_identifiers_outside_comments() {
        assert!(mentions("\tvar p := PartFlight.new()\n", "PartFlight"));
        assert!(!mentions("\tvar p := PartFlights.new()\n", "PartFlight"));
        assert!(!mentions("\t# PartFlight is gone\n", "PartFlight"));
        assert!(mentions("\tprint(\"#\", PartFlight)\n", "PartFlight"));
    }

    #[test]
    fn registers_in_the_editors_layout_and_keeps_other_entries() {
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        std::fs::create_dir_all(root.join(".godot")).expect(".godot");
        std::fs::write(
            root.join(CACHE_FILE),
            "list=[{\n\"base\": &\"Node\",\n\"class\": &\"Zed\",\n\"icon\": \"\",\n\"is_abstract\": false,\n\"is_tool\": false,\n\"language\": &\"CSharp\",\n\"path\": \"res://zed.cs\"\n}]\n",
        )
        .expect("cache");
        std::fs::write(root.join("zed.cs"), "").expect("zed");
        std::fs::write(root.join("part.gd"), "class_name PartFlight\n").expect("part");
        std::fs::create_dir_all(root.join("sub")).expect("sub");

        assert_eq!(
            register(root, "res://part.gd", "class_name PartFlight\n"),
            Registration::Registered("PartFlight".into())
        );
        assert_eq!(
            register(
                root,
                "res://sub/kid.gd",
                "@tool\n@icon('i.svg')\nextends \"../part.gd\"\nclass_name Kid\n"
            ),
            Registration::Registered("Kid".into())
        );
        let written = std::fs::read_to_string(root.join(CACHE_FILE)).expect("cache");
        assert_eq!(
            written,
            "list=[{\n\"base\": &\"RefCounted\",\n\"class\": &\"Kid\",\n\"icon\": \"res://sub/i.svg\",\n\"is_abstract\": false,\n\"is_tool\": true,\n\"language\": &\"GDScript\",\n\"path\": \"res://sub/kid.gd\"\n}, {\n\"base\": &\"RefCounted\",\n\"class\": &\"PartFlight\",\n\"icon\": \"\",\n\"is_abstract\": false,\n\"is_tool\": false,\n\"language\": &\"GDScript\",\n\"path\": \"res://part.gd\"\n}, {\n\"base\": &\"Node\",\n\"class\": &\"Zed\",\n\"icon\": \"\",\n\"is_abstract\": false,\n\"is_tool\": false,\n\"language\": &\"CSharp\",\n\"path\": \"res://zed.cs\"\n}]\n"
        );

        assert_eq!(
            register(root, "res://part.gd", "class_name PartFlight\n"),
            Registration::Unchanged("PartFlight".into())
        );
        assert_eq!(
            register(root, "res://other.gd", "class_name PartFlight\n"),
            Registration::Taken {
                class: "PartFlight".into(),
                other_path: "res://part.gd".into()
            }
        );
        assert_eq!(
            register(root, "res://part.gd", "class_name Renamed\n"),
            Registration::Registered("Renamed".into())
        );
        assert_eq!(
            register(root, "res://part.gd", "extends Node\n"),
            Registration::Removed("Renamed".into())
        );
        let names: Vec<String> = listed(root).into_keys().collect();
        assert_eq!(names, ["Kid", "Zed"]);
    }

    #[test]
    fn a_cache_it_cannot_read_is_left_alone() {
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        std::fs::create_dir_all(root.join(".godot")).expect(".godot");
        std::fs::write(root.join(CACHE_FILE), "garbage").expect("cache");
        assert!(matches!(
            register(root, "res://part.gd", "class_name PartFlight\n"),
            Registration::Failed(_)
        ));
        assert_eq!(
            std::fs::read_to_string(root.join(CACHE_FILE)).expect("cache"),
            "garbage"
        );
    }

    #[test]
    fn lists_declared_classes_the_cache_lacks() {
        let project = tempfile::tempdir().expect("project");
        let root = project.path();
        std::fs::create_dir_all(root.join("ign")).expect("ign");
        std::fs::write(root.join("ign/.gdignore"), "").expect("gdignore");
        std::fs::write(root.join("ign/x.gd"), "class_name Ignored\n").expect("ignored");
        std::fs::write(root.join("a.gd"), "class_name Alpha\n").expect("a");
        std::fs::write(root.join("b.gd"), "extends Node\n").expect("b");
        assert_eq!(
            unregistered(root),
            [Unregistered {
                class: "Alpha".into(),
                path: "res://a.gd".into()
            }]
        );
        register(root, "res://a.gd", "class_name Alpha\n");
        assert!(unregistered(root).is_empty());
    }
}
