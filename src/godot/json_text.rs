//! Reading the JSON text Godot writes.
//!
//! `JSON.stringify` escapes `"`, `\`, `\b`, `\t`, `\n`, `\f` and `\r`, but leaves every other
//! character below U+0020 raw inside a string and writes U+000B as `\v`. JSON allows neither,
//! so a reply that echoes such text (a node name, a file's contents, an error quoting the
//! caller's input) would not parse. [`repair`] rewrites both into `\u00XX` escapes first.

use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::fmt::Write as _;

/// The text with every control character inside a string literal written as a `\u00XX`
/// escape and every `\v` escape as `\u000b`. Text outside strings is left as it is: control
/// characters there are either JSON whitespace or already invalid. Clean text is returned
/// without a copy.
pub fn repair(text: &str) -> Cow<'_, str> {
    if !text.bytes().any(|byte| byte < 0x20) && !text.contains("\\v") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if !in_string {
            in_string = ch == '"';
            out.push(ch);
        } else if escaped {
            escaped = false;
            if ch == 'v' {
                out.push_str("u000b");
            } else {
                out.push(ch);
            }
        } else if ch == '\\' {
            escaped = true;
            out.push(ch);
        } else if ch == '"' {
            in_string = false;
            out.push(ch);
        } else if (ch as u32) < 0x20 {
            let _ = write!(out, "\\u{:04x}", ch as u32);
        } else {
            out.push(ch);
        }
    }
    Cow::Owned(out)
}

/// Parses JSON text written by Godot, after [`repair`].
pub fn parse<T: DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    serde_json::from_str(&repair(text))
}

/// The JSON after `marker` on the first line of `output` that starts with it, as a headless
/// script prints its answer, read with [`parse`]. `None` when no line has the marker or its
/// text is not JSON even after [`repair`].
pub fn marked<T: DeserializeOwned>(output: &str, marker: &str) -> Option<T> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(marker))
        .and_then(|answer| parse(answer.trim()).ok())
}

/// The first characters of a message, for a log line.
pub fn head(text: &str) -> String {
    const LIMIT: usize = 120;
    let mut cut: String = text.chars().take(LIMIT).collect();
    if text.chars().nth(LIMIT).is_some() {
        cut.push('…');
    }
    format!("{cut:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn a_marked_answer_is_read_from_its_line() {
        let output = "Godot Engine v4.8\nGODOT_MCP_X= {\"text\":\"a\u{7}b\"}\nGODOT_MCP_X={}\n";
        let answer: Value = marked(output, "GODOT_MCP_X=").expect("answer");
        assert_eq!(answer["text"], "a\u{7}b");
        assert!(marked::<Value>("GODOT_MCP_X={", "GODOT_MCP_X=").is_none());
        assert!(marked::<Value>("no answer\n", "GODOT_MCP_X=").is_none());
    }

    #[test]
    fn clean_text_is_not_copied() {
        let text = r#"{"a":"b\n\t\\","c":[1,2]}"#;
        assert!(matches!(repair(text), Cow::Borrowed(_)));
        let spaced = "{\n\t\"a\": 1\r\n}";
        assert_eq!(repair(spaced), spaced);
    }

    #[test]
    fn raw_control_characters_in_strings_are_escaped() {
        let text = "{\"name\":\"a\u{1}b\u{1f}c\",\"n\":\"\u{0}\"}";
        assert_eq!(repair(text), r#"{"name":"a\u0001b\u001fc","n":"\u0000"}"#);
        let value: Value = parse(text).expect("parses");
        assert_eq!(value["name"], "a\u{1}b\u{1f}c");
    }

    #[test]
    fn whitespace_between_tokens_stays_and_raw_whitespace_in_strings_is_escaped() {
        let text = "{\n\t\"a\": \"x\ty\nz\"\r\n}";
        assert_eq!(repair(text), "{\n\t\"a\": \"x\\u0009y\\u000az\"\r\n}");
        let value: Value = parse(text).expect("parses");
        assert_eq!(value["a"], "x\ty\nz");
    }

    #[test]
    fn a_v_escape_becomes_a_vertical_tab_but_an_escaped_backslash_before_v_does_not() {
        let text = r#"{"a":"1\v2","b":"3\\v4","c":"5\\\v6","\v":0}"#;
        let value: Value = parse(text).expect("parses");
        assert_eq!(value["a"], "1\u{b}2");
        assert_eq!(value["b"], "3\\v4");
        assert_eq!(value["c"], "5\\\u{b}6");
        assert_eq!(value["\u{b}"], 0);
    }

    #[test]
    fn quotes_and_non_ascii_text_keep_the_string_boundaries() {
        let text = "{\"a\":\"say \\\"hi\\\"\u{1}\",\"b\":\"\u{2028}é\u{2}\"}";
        let value: Value = parse(text).expect("parses");
        assert_eq!(value["a"], "say \"hi\"\u{1}");
        assert_eq!(value["b"], "\u{2028}é\u{2}");
    }

    #[test]
    fn invalid_text_stays_invalid() {
        assert!(parse::<Value>("{\"a\":1,\u{1}}").is_err());
        assert!(parse::<Value>("{\"a\":\"open").is_err());
    }
}
