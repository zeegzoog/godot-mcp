//! A stand-in for the GitHub command-line client, used by the feedback tests. It records how it
//! was called and answers as its environment says. It is not part of the server and is not built
//! with it.
//!
//! Variables (all optional):
//! - `FAKE_GH_LOG`: file that receives one `[call]` block per run, one argument per line.
//! - `FAKE_GH_BODY`: file that receives standard input of `issue create`, or the content of the
//!   file named by `--body-file` for `issue comment`; a file it cannot open fails the run, as gh does.
//! - `FAKE_GH_LIST_STDOUT`, `FAKE_GH_LIST_STDERR`, `FAKE_GH_LIST_EXIT`, `FAKE_GH_LIST_DELAY_MS`.
//! - `FAKE_GH_CREATE_STDOUT`, `FAKE_GH_CREATE_STDERR`, `FAKE_GH_CREATE_EXIT`, `FAKE_GH_DELAY_MS`.
//! - `FAKE_GH_LIST_HIT_URL`: `issue list` answers with one hit at this address whose body holds the
//!   fingerprint it was asked to search for (unless `FAKE_GH_LIST_STDOUT` is set).
//!
//! An argument that contains `FAKE_GH_SLEEP_<milliseconds>` makes the run take that long, so a
//! test can slow down one call without touching the environment of the process under test.

use std::io::{Read, Write};
use std::time::Duration;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if let Some(log) = var("FAKE_GH_LOG") {
        let mut entry = String::from("[call]\n");
        for arg in &args {
            entry.push_str(arg);
            entry.push('\n');
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = file.write_all(entry.as_bytes());
        }
    }

    let (prefix, delay, default_stdout) = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("issue"), Some("list")) => ("FAKE_GH_LIST", "FAKE_GH_LIST_DELAY_MS", "[]"),
        (Some("issue"), Some("create")) => {
            let mut input = String::new();
            let _ = std::io::stdin().read_to_string(&mut input);
            if let Some(path) = var("FAKE_GH_BODY") {
                let _ = std::fs::write(path, input);
            }
            (
                "FAKE_GH_CREATE",
                "FAKE_GH_DELAY_MS",
                "https://github.com/zeegzoog/godot-mcp/issues/1\n",
            )
        }
        (Some("issue"), Some("comment")) => {
            let file = args
                .iter()
                .position(|arg| arg == "--body-file")
                .and_then(|index| args.get(index + 1));
            if let Some(file) = file {
                match std::fs::read_to_string(file) {
                    Ok(text) => {
                        if let Some(path) = var("FAKE_GH_BODY") {
                            let _ = std::fs::write(path, text);
                        }
                    }
                    Err(err) => {
                        eprintln!("open {file}: {err}");
                        std::process::exit(1);
                    }
                }
            }
            (
                "FAKE_GH_COMMENT",
                "FAKE_GH_DELAY_MS",
                "https://github.com/zeegzoog/godot-mcp/issues/1#issuecomment-1
",
            )
        }
        _ => {
            eprintln!("fake gh: unsupported arguments");
            std::process::exit(64);
        }
    };

    let marker = "FAKE_GH_SLEEP_";
    let requested = args.iter().find_map(|arg| {
        let digits: String = arg
            .split_once(marker)?
            .1
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse::<u64>().ok()
    });
    let configured = var(delay).and_then(|text| text.parse::<u64>().ok());
    if let Some(millis) = requested.or(configured) {
        std::thread::sleep(Duration::from_millis(millis));
    }
    let stdout = match (
        var(&format!("{prefix}_STDOUT")),
        var("FAKE_GH_LIST_HIT_URL"),
    ) {
        (Some(text), _) => text,
        (None, Some(url)) if prefix == "FAKE_GH_LIST" => {
            let fingerprint = args
                .iter()
                .position(|arg| arg == "--search")
                .and_then(|index| args.get(index + 1))
                .and_then(|query| query.split_whitespace().next())
                .unwrap_or_default();
            format!(
                r#"[{{"number":7,"title":"existing","url":"{url}","body":"Fingerprint: {fingerprint}"}}]"#
            )
        }
        _ => default_stdout.to_string(),
    };
    print!("{stdout}");
    eprint!("{}", var(&format!("{prefix}_STDERR")).unwrap_or_default());
    let code = var(&format!("{prefix}_EXIT"))
        .and_then(|text| text.parse::<i32>().ok())
        .unwrap_or(0);
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
