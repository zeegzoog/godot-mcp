use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser};
use std::convert::Infallible;
use std::path::PathBuf;
use tracing::{debug, warn};

const DEFAULT_DEBUG_PORT: u16 = 9333;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "godot-mcp",
    about = "High-performance Model Context Protocol (MCP) server for the Godot Game Engine",
    version
)]
pub struct Config {
    /// Path to the primary Godot executable
    #[arg(long, env = "GODOT_PATH", value_parser = parse_path)]
    pub godot_path: Option<PathBuf>,

    /// Path to the Godot console wrapper executable. The server does not use it yet; the
    /// editor end-to-end tests read it to show editor logs on Windows.
    #[arg(long, env = "GODOT_CONSOLE_PATH", value_parser = parse_path)]
    pub godot_console_path: Option<PathBuf>,

    /// WebSocket port for Godot Editor bridge communication
    #[arg(long, env = "GODOT_DEBUG_PORT", default_value = "9333", value_parser = parse_port)]
    pub debug_port: u16,

    /// Agent feedback channel: `off`, `draft` (notes and drafts only) or `full` (adds
    /// submission after the human's approval)
    #[arg(long = "feedback", env = "GODOT_MCP_FEEDBACK", default_value = "full", value_parser = parse_feedback_mode)]
    pub feedback: FeedbackMode,

    /// Path of the GitHub command-line client used to submit feedback reports; `gh` on `PATH`
    /// when unset
    #[arg(long = "gh-path", env = "GODOT_MCP_GH_PATH", value_parser = parse_path)]
    pub gh_path: Option<PathBuf>,

    /// Where `debug_port` came from. Empty until `load`; tests that parse arguments directly
    /// leave it empty.
    #[arg(skip)]
    bridge_port_origin: String,
}

/// How much of the agent feedback channel the human allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedbackMode {
    /// No feedback tools and no guidance.
    Off,
    /// Notes and drafts; the agent can never publish.
    Draft,
    /// Adds submission, which the human still has to approve.
    #[default]
    Full,
}

impl FeedbackMode {
    /// Reads a mode name in any case; `None` for anything else.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "draft" => Some(Self::Draft),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Draft => "draft",
            Self::Full => "full",
        }
    }

    /// Whether the note and draft tools exist.
    pub fn enabled(self) -> bool {
        self != Self::Off
    }

    /// Whether the submit tool exists.
    pub fn submits(self) -> bool {
        self == Self::Full
    }
}

/// An empty value means "not set" and an unknown one is reported and read as drafting only: a
/// setting that is wrong must never keep the server from starting, and a typo of a restriction
/// (`drafts`, `of`) must never switch submission on (FB-10).
fn parse_feedback_mode(raw: &str) -> Result<FeedbackMode, Infallible> {
    if raw.trim().is_empty() {
        return Ok(FeedbackMode::default());
    }
    Ok(FeedbackMode::parse(raw).unwrap_or_else(|| {
        warn!(
            "Ignoring GODOT_MCP_FEEDBACK={raw:?}: use off, draft or full. Using {}.",
            FeedbackMode::Draft.as_str()
        );
        FeedbackMode::Draft
    }))
}

/// Accepts any text, including an empty one: placeholders such as `GODOT_PATH=""` in a
/// template or an MCP client's `env` block mean "not configured", not "invalid".
/// (clap's built-in path parser rejects empty values and the server would not start.)
/// An empty path never exists, so the Godot binary lookup falls through to auto-detection.
fn parse_path(raw: &str) -> Result<PathBuf, Infallible> {
    Ok(PathBuf::from(raw))
}

/// Names the port the server will dial and the place that value was read from.
pub fn describe_bridge_port(port: u16, source: Option<ValueSource>) -> String {
    match source {
        Some(ValueSource::CommandLine) => format!("port {port} from --debug-port"),
        Some(ValueSource::EnvVariable) => {
            format!("port {port} from the environment variable GODOT_DEBUG_PORT")
        }
        _ => format!("port {port}, the default when --debug-port and GODOT_DEBUG_PORT are unset"),
    }
}

/// Like `u16`, except that an empty value selects the default port.
fn parse_port(raw: &str) -> Result<u16, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(DEFAULT_DEBUG_PORT);
    }
    raw.parse::<u16>().map_err(|err| format!("{err}"))
}

/// Loads a `.env` file from the working directory (or a parent) if there is one.
///
/// A file that exists but cannot be parsed is reported instead of silently ignored, since
/// none of its settings would take effect. The usual cause on Windows is a path in double
/// quotes: `"C:\Godot\Godot.exe"` is read as escape sequences, `'C:\Godot\Godot.exe'` is not.
fn load_dotenv() {
    match dotenvy::dotenv() {
        Ok(path) => debug!("Loaded settings from {}", path.display()),
        Err(err) if err.not_found() => {}
        Err(err) => warn!(
            "Ignoring the .env file because it could not be parsed ({err}). \
             Put Windows paths in single quotes, e.g. GODOT_PATH='C:\\Godot\\Godot.exe'"
        ),
    }
}

impl Config {
    pub fn load() -> Self {
        load_dotenv();
        let matches = Self::command().get_matches();
        let mut config = Self::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
        config.bridge_port_origin =
            describe_bridge_port(config.debug_port, matches.value_source("debug_port"));
        config
    }

    /// The port the editor addon has to listen on, and the flag or variable that set it.
    pub fn bridge_port_origin(&self) -> String {
        if self.bridge_port_origin.is_empty() {
            format!("port {}", self.debug_port)
        } else {
            self.bridge_port_origin.clone()
        }
    }

    /// Resolve the effective Godot binary path.
    ///
    /// `GODOT_PATH` should always be set explicitly. Official Godot builds
    /// for Windows are portable, version-named executables (e.g.
    /// `Godot_v4.8.2-stable_win64.exe`) that users extract wherever they
    /// like, so there is no single standard install location to hardcode.
    /// The scan below is a best-effort convenience for a handful of common
    /// locations — it is not a substitute for configuring `GODOT_PATH`.
    ///
    /// A configured path is always used as given, even when the file is missing: replacing it
    /// with another installed Godot could silently run a different engine version than the one
    /// the user chose. Tools that need the engine then fail naming that path. An empty value is
    /// treated as unset.
    pub fn resolve_godot_path(&self) -> PathBuf {
        if let Some(path) = self
            .godot_path
            .as_ref()
            .filter(|p| !p.as_os_str().is_empty())
        {
            // A bare command name is looked up on PATH when it is run, so only a path with a
            // directory part can be judged missing here.
            let has_directory = path.parent().is_some_and(|dir| !dir.as_os_str().is_empty());
            if has_directory && !path.exists() {
                warn!(
                    "GODOT_PATH points to '{}', which does not exist. Tools that run Godot \
                     will fail until it is fixed.",
                    path.display()
                );
            }
            return path.clone();
        }

        if let Some(found) = Self::scan_for_godot_binary() {
            return found;
        }

        // Fallback to searching in PATH
        PathBuf::from("godot")
    }

    /// Scan a few common Windows install roots (Program Files, Local AppData)
    /// for an executable whose name starts with `Godot` and ends in `.exe`,
    /// looking one level into subdirectories to account for version-named
    /// folders produced by extracting the official zip release.
    #[cfg(target_os = "windows")]
    fn scan_for_godot_binary() -> Option<PathBuf> {
        let roots = [
            std::env::var("ProgramFiles").ok(),
            std::env::var("ProgramFiles(x86)").ok(),
            std::env::var("LOCALAPPDATA")
                .ok()
                .map(|p| format!("{p}\\Programs")),
        ];

        roots
            .into_iter()
            .flatten()
            .map(PathBuf::from)
            .find_map(|root| Self::find_godot_exe_in(&root, 2))
    }

    #[cfg(target_os = "windows")]
    fn find_godot_exe_in(dir: &std::path::Path, depth: u8) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        let mut subdirs = Vec::new();
        let mut matches = Vec::new();

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                subdirs.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_lowercase();
            if lower.starts_with("godot") && lower.ends_with(".exe") && !lower.contains("console") {
                matches.push(path);
            }
        }

        // Prefer the lexicographically last match: for versioned filenames
        // like `Godot_v4.8.2-stable_win64.exe` this tends to pick the newest.
        matches.sort();
        if let Some(found) = matches.pop() {
            return Some(found);
        }

        if depth == 0 {
            return None;
        }
        subdirs
            .into_iter()
            .find_map(|subdir| Self::find_godot_exe_in(&subdir, depth - 1))
    }

    /// macOS installs are conventionally a drag-and-dropped `.app` bundle,
    /// so a fixed path is a reasonable (if not guaranteed) default.
    #[cfg(target_os = "macos")]
    fn scan_for_godot_binary() -> Option<PathBuf> {
        [
            "/Applications/Godot.app/Contents/MacOS/Godot",
            "/Applications/Godot_mono.app/Contents/MacOS/Godot",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
    }

    /// Linux installs commonly land in one of these locations via a package
    /// manager or Flatpak.
    #[cfg(target_os = "linux")]
    fn scan_for_godot_binary() -> Option<PathBuf> {
        [
            "/usr/bin/godot",
            "/usr/local/bin/godot",
            "/var/lib/flatpak/exports/bin/org.godotengine.Godot",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    fn scan_for_godot_binary() -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, FeedbackMode, describe_bridge_port};
    use clap::parser::ValueSource;
    use clap::{CommandFactory, Parser};
    use std::path::PathBuf;

    #[test]
    fn the_feedback_mode_defaults_to_full_and_accepts_any_case() {
        assert_eq!(parse(&[]).feedback, FeedbackMode::Full);
        for (text, mode) in [
            ("off", FeedbackMode::Off),
            ("OFF", FeedbackMode::Off),
            ("Draft", FeedbackMode::Draft),
            (" full ", FeedbackMode::Full),
        ] {
            assert_eq!(parse(&["--feedback", text]).feedback, mode, "{text:?}");
        }
    }

    #[test]
    fn an_empty_feedback_mode_is_the_default_and_an_unknown_one_never_widens_the_channel() {
        assert_eq!(parse(&["--feedback", ""]).feedback, FeedbackMode::Full);
        // A typo of a restriction must not switch submission on.
        for typo in ["bogus", "drafts", "of", "fulll", "0", "no"] {
            assert_eq!(
                parse(&["--feedback", typo]).feedback,
                FeedbackMode::Draft,
                "{typo:?}"
            );
        }
    }

    #[test]
    fn the_feedback_modes_have_stable_names() {
        assert_eq!(FeedbackMode::Off.as_str(), "off");
        assert_eq!(FeedbackMode::Draft.as_str(), "draft");
        assert_eq!(FeedbackMode::Full.as_str(), "full");
        assert!(FeedbackMode::Full.submits() && !FeedbackMode::Draft.submits());
        assert!(FeedbackMode::Draft.enabled() && !FeedbackMode::Off.enabled());
    }

    #[test]
    fn the_gh_path_is_optional_and_an_empty_value_is_accepted() {
        assert_eq!(parse(&[]).gh_path, None);
        assert_eq!(
            parse(&["--gh-path", "/opt/gh"]).gh_path,
            Some(PathBuf::from("/opt/gh"))
        );
        assert_eq!(parse(&["--gh-path", ""]).gh_path, Some(PathBuf::from("")));
    }

    #[test]
    fn an_offline_bridge_is_told_the_port_the_server_actually_reads() {
        let from_env = describe_bridge_port(19333, Some(ValueSource::EnvVariable));
        assert!(from_env.contains("19333"), "{from_env}");
        assert!(
            from_env.contains("environment variable GODOT_DEBUG_PORT"),
            "{from_env}"
        );
        assert!(!from_env.contains("default: 9333"), "{from_env}");

        let from_flag = describe_bridge_port(19444, Some(ValueSource::CommandLine));
        assert!(
            from_flag.contains("19444") && from_flag.contains("--debug-port"),
            "{from_flag}"
        );
        assert!(!from_flag.contains("GODOT_DEBUG_PORT"), "{from_flag}");

        let fallback = describe_bridge_port(9333, None);
        assert!(
            fallback.contains("9333") && fallback.contains("default"),
            "{fallback}"
        );

        let matches = Config::command()
            .try_get_matches_from(["godot-mcp", "--debug-port", "19444"])
            .expect("args");
        assert_eq!(
            matches.value_source("debug_port"),
            Some(ValueSource::CommandLine)
        );
    }

    fn parse(args: &[&str]) -> Config {
        let mut full = vec!["godot-mcp"];
        full.extend_from_slice(args);
        Config::try_parse_from(full).expect("valid arguments")
    }

    #[test]
    fn a_configured_godot_path_is_used_even_when_the_file_is_missing() {
        // Silently substituting another installed Godot could run a different version than the
        // one the user chose; the tools report the missing file instead.
        let missing = std::env::temp_dir()
            .join("godot-mcp-test")
            .join("no-such-godot");
        let config = parse(&["--godot-path", missing.to_str().expect("utf-8 path")]);
        assert_eq!(config.resolve_godot_path(), missing);
    }

    #[test]
    fn a_bare_command_name_is_kept_so_the_operating_system_can_look_it_up_on_path() {
        let config = parse(&["--godot-path", "godot-4.6"]);
        assert_eq!(config.resolve_godot_path(), PathBuf::from("godot-4.6"));
    }

    #[test]
    fn an_empty_godot_path_means_unset_and_still_resolves_to_something_runnable_by_name() {
        let config = parse(&["--godot-path", ""]);
        assert_ne!(config.resolve_godot_path(), PathBuf::from(""));
    }

    #[test]
    fn the_unused_mcp_url_setting_is_gone() {
        assert!(Config::try_parse_from(["godot-mcp", "--mcp-url", "http://localhost:1"]).is_err());
        assert!(Config::try_parse_from(["godot-mcp", "--debug-port", "9333"]).is_ok());
    }
}
