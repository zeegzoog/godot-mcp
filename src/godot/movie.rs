//! Godot's Movie Maker for a managed process run: the engine options that start it, the output
//! files it writes, and what they hold once the run has ended.
//!
//! Godot picks the writer from the file extension: `.avi` is MJPEG video with the game's audio in
//! one file; `.png` is one numbered image per frame (`name00000000.png`, `name00000001.png`, ...)
//! and the audio in `name.wav` beside them.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The frame rate Godot's own `editor/movie_writer/fps` setting defaults to.
pub const DEFAULT_MOVIE_FPS: u32 = 60;
pub const MAX_MOVIE_FPS: u32 = 1000;
/// About 46 hours at 60 frames per second: far beyond any recording, small enough to stay exact.
pub const MAX_MOVIE_FRAMES: u64 = 10_000_000;
/// The digits Godot's PNG writer gives each frame number.
const PNG_FRAME_DIGITS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovieFormat {
    /// MJPEG video and audio in one `.avi`.
    Avi,
    /// One `.png` per frame, audio in a `.wav` beside them.
    Png,
}

impl MovieFormat {
    /// The writer Godot uses for `path`, from its extension in any case; `None` for any other.
    pub fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "avi" => Some(Self::Avi),
            "png" => Some(Self::Png),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Avi => "avi",
            Self::Png => "png",
        }
    }
}

/// One Movie Maker recording: where it goes, at which rate, and when the game quits.
#[derive(Clone, Debug)]
pub struct Movie {
    /// The absolute output path given to `--write-movie`.
    pub path: PathBuf,
    pub format: MovieFormat,
    pub fps: u32,
    /// `--quit-after`: the game quits after this many frames. `None` records until it ends.
    pub quit_after_frames: Option<u64>,
}

impl Movie {
    /// The engine options, placed before the `--` that starts the game's own arguments.
    pub fn engine_args(&self) -> Vec<String> {
        let mut args = vec![
            "--write-movie".to_string(),
            self.path.display().to_string(),
            "--fixed-fps".to_string(),
            self.fps.to_string(),
        ];
        if let Some(frames) = self.quit_after_frames {
            args.push("--quit-after".to_string());
            args.push(frames.to_string());
        }
        args
    }

    /// The `.wav` Godot writes beside a PNG sequence.
    fn audio_path(&self) -> Option<PathBuf> {
        (self.format == MovieFormat::Png).then(|| self.path.with_extension("wav"))
    }

    /// The files an earlier recording to the same path left: the video, or the frames and audio.
    pub fn existing_outputs(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        match self.format {
            MovieFormat::Avi => {
                if self.path.exists() {
                    found.push(self.path.clone());
                }
            }
            MovieFormat::Png => {
                found.extend(png_frames(&self.path));
                if let Some(audio) = self.audio_path().filter(|audio| audio.exists()) {
                    found.push(audio);
                }
            }
        }
        found
    }

    /// Creates the output folder and removes what an earlier recording to this path left, so the
    /// report counts this run's frames only. A folder this creates inside `project` gets a
    /// `.gdignore`, so the editor does not import every frame. Returns whether it made the folder.
    pub fn prepare(&self, project: &Path) -> std::io::Result<bool> {
        for old in self.existing_outputs() {
            std::fs::remove_file(&old)?;
        }
        let Some(folder) = self.path.parent() else {
            return Ok(false);
        };
        if folder.is_dir() {
            return Ok(false);
        }
        std::fs::create_dir_all(folder)?;
        let inside = match (folder.canonicalize(), project.canonicalize()) {
            (Ok(folder), Ok(project)) => folder.starts_with(&project) && folder != project,
            _ => false,
        };
        if inside {
            std::fs::write(folder.join(".gdignore"), "")?;
        }
        Ok(true)
    }

    /// What the recording holds now: the files, their size, and the frames written. `ended` says
    /// the game has exited, so the writer had its chance to finish the file.
    pub fn report(&self, ended: bool) -> Value {
        match self.format {
            MovieFormat::Avi => {
                let size = file_size(&self.path);
                let header_frames = avi_frames(&self.path);
                // Godot writes the frame count into the header when the recording ends; a game
                // that was killed leaves the count it started with.
                let finished = ended && header_frames.is_some_and(|frames| frames > 0);
                json!({
                    "path": self.path.display().to_string(),
                    "format": "avi",
                    "fps": self.fps,
                    "quit_after_frames": self.quit_after_frames,
                    "exists": size.is_some(),
                    "size_bytes": size,
                    "frames": if finished { header_frames } else { None },
                    "duration_seconds": if finished { header_frames.map(|frames| seconds(frames, self.fps)) } else { None },
                    "finished": finished,
                    "note": avi_note(ended, size.is_some(), finished),
                })
            }
            MovieFormat::Png => {
                let frames = png_frames(&self.path);
                let audio = self.audio_path().filter(|audio| audio.exists());
                let frame_bytes: u64 = frames.iter().filter_map(|frame| file_size(frame)).sum();
                let audio_bytes = audio.as_deref().and_then(file_size).unwrap_or(0);
                let count = frames.len() as u64;
                json!({
                    "path": self.path.display().to_string(),
                    "format": "png",
                    "fps": self.fps,
                    "quit_after_frames": self.quit_after_frames,
                    "first_frame": frames.first().map(|frame| frame.display().to_string()),
                    "last_frame": frames.last().map(|frame| frame.display().to_string()),
                    "frame_pattern": png_pattern(&self.path).map(|pattern| pattern.display().to_string()),
                    "audio": audio.as_ref().map(|audio| audio.display().to_string()),
                    "exists": count > 0,
                    "size_bytes": frame_bytes + audio_bytes,
                    "frames": count,
                    "duration_seconds": seconds(count, self.fps),
                    "finished": ended && count > 0,
                    "note": if ended { Value::Null } else { json!("Still recording: the counts grow until the game ends.") },
                })
            }
        }
    }
}

fn avi_note(ended: bool, exists: bool, finished: bool) -> Value {
    if !ended {
        return json!(
            "Still recording: Godot writes the frame count into the file when the game ends. Stop the run with godot_stop_project or let quit_after_frames end it."
        );
    }
    if !exists {
        return json!(
            "No file was written: read the run's output with godot_run_output for Movie Maker errors."
        );
    }
    if !finished {
        return json!(
            "The game ended before Godot finished the file (it was killed or crashed), so the header holds no frame count and some players refuse it. Record again and end it with quit_after_frames, or stop it with godot_stop_project, which asks the game to quit first."
        );
    }
    Value::Null
}

fn seconds(frames: u64, fps: u32) -> f64 {
    if fps == 0 {
        return 0.0;
    }
    // Two decimals are enough to say how long a clip is.
    ((frames as f64 / f64::from(fps)) * 100.0).round() / 100.0
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
}

/// `dwTotalFrames` of the AVI main header (`avih`), which Godot's MJPEG writer fills in when the
/// recording ends. `None` when the file is missing or is not an AVI.
pub fn avi_frames(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut head = Vec::with_capacity(512);
    std::fs::File::open(path)
        .ok()?
        .take(512)
        .read_to_end(&mut head)
        .ok()?;
    avi_frames_in(&head)
}

fn avi_frames_in(head: &[u8]) -> Option<u64> {
    if head.get(0..4)? != b"RIFF" || head.get(8..12)? != b"AVI " {
        return None;
    }
    let at = head.windows(4).position(|window| window == b"avih")?;
    // The chunk id and size come first; dwTotalFrames is the fifth field of the header.
    let field = at + 8 + 16;
    let bytes: [u8; 4] = head.get(field..field + 4)?.try_into().ok()?;
    Some(u64::from(u32::from_le_bytes(bytes)))
}

/// The pattern Godot's PNG writer names frames with, `name########.png`.
fn png_pattern(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    Some(path.with_file_name(format!("{stem}{}.png", "#".repeat(PNG_FRAME_DIGITS))))
}

/// The frames of a PNG sequence written for `path`, in frame order.
fn png_frames(path: &Path) -> Vec<PathBuf> {
    let (Some(folder), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str()))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut frames: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            is_png_frame(name, stem)
        })
        .map(|entry| entry.path())
        .collect();
    frames.sort();
    frames
}

fn is_png_frame(name: &str, stem: &str) -> bool {
    let Some(rest) = name.strip_prefix(stem) else {
        return false;
    };
    let Some(digits) = rest
        .strip_suffix(".png")
        .or_else(|| rest.strip_suffix(".PNG"))
    else {
        return false;
    };
    digits.len() == PNG_FRAME_DIGITS && digits.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avi_head(frames: u32) -> Vec<u8> {
        let mut head = Vec::new();
        head.extend_from_slice(b"RIFF");
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(b"AVI LIST");
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(b"hdrlavih");
        head.extend_from_slice(&56u32.to_le_bytes());
        for field in [16_666u32, 0, 0, 16] {
            head.extend_from_slice(&field.to_le_bytes());
        }
        head.extend_from_slice(&frames.to_le_bytes());
        head.extend_from_slice(&[0; 36]);
        head
    }

    #[test]
    fn the_writer_comes_from_the_extension_in_any_case() {
        assert_eq!(
            MovieFormat::of(Path::new("a/b.avi")),
            Some(MovieFormat::Avi)
        );
        assert_eq!(
            MovieFormat::of(Path::new("a/b.AVI")),
            Some(MovieFormat::Avi)
        );
        assert_eq!(MovieFormat::of(Path::new("b.png")), Some(MovieFormat::Png));
        assert_eq!(MovieFormat::of(Path::new("b.mp4")), None);
        assert_eq!(MovieFormat::of(Path::new("b")), None);
    }

    #[test]
    fn the_frame_count_is_read_from_the_avi_main_header() {
        assert_eq!(avi_frames_in(&avi_head(30)), Some(30));
        assert_eq!(avi_frames_in(&avi_head(0)), Some(0));
        assert_eq!(avi_frames_in(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(avi_frames_in(b"RIFF"), None);
    }

    #[test]
    fn only_numbered_frames_of_the_same_name_count() {
        assert!(is_png_frame("shot00000000.png", "shot"));
        assert!(is_png_frame("shot00000123.png", "shot"));
        assert!(!is_png_frame("shot.png", "shot"));
        assert!(!is_png_frame("shot0000001.png", "shot"));
        assert!(!is_png_frame("other00000001.png", "shot"));
        assert!(!is_png_frame("shot0000000a.png", "shot"));
    }

    #[test]
    fn the_engine_options_name_the_file_the_rate_and_the_frame_limit() {
        let movie = Movie {
            path: PathBuf::from("/p/movies/clip.avi"),
            format: MovieFormat::Avi,
            fps: 30,
            quit_after_frames: Some(90),
        };
        assert_eq!(
            movie.engine_args(),
            [
                "--write-movie",
                "/p/movies/clip.avi",
                "--fixed-fps",
                "30",
                "--quit-after",
                "90"
            ]
        );
        let open = Movie {
            quit_after_frames: None,
            ..movie
        };
        assert!(!open.engine_args().contains(&"--quit-after".to_string()));
    }

    #[test]
    fn a_png_report_counts_this_names_frames_and_adds_the_audio() {
        let dir = tempfile::tempdir().expect("dir");
        let movie = Movie {
            path: dir.path().join("clip.png"),
            format: MovieFormat::Png,
            fps: 30,
            quit_after_frames: Some(3),
        };
        for name in [
            "clip00000000.png",
            "clip00000001.png",
            "clip00000002.png",
            "other00000000.png",
        ] {
            std::fs::write(dir.path().join(name), b"1234").expect("frame");
        }
        std::fs::write(dir.path().join("clip.wav"), b"12").expect("audio");
        let report = movie.report(true);
        assert_eq!(report["frames"], 3);
        assert_eq!(report["size_bytes"], 14);
        assert_eq!(report["duration_seconds"], 0.1);
        assert_eq!(report["finished"], true);
        assert!(report["audio"].as_str().unwrap_or("").ends_with("clip.wav"));
    }

    #[test]
    fn an_avi_reports_its_frames_only_once_the_game_ended() {
        let dir = tempfile::tempdir().expect("dir");
        let movie = Movie {
            path: dir.path().join("clip.avi"),
            format: MovieFormat::Avi,
            fps: 30,
            quit_after_frames: None,
        };
        std::fs::write(&movie.path, avi_head(60)).expect("avi");
        let running = movie.report(false);
        assert_eq!(running["frames"], Value::Null);
        assert_eq!(running["finished"], false);
        let ended = movie.report(true);
        assert_eq!(ended["frames"], 60);
        assert_eq!(ended["duration_seconds"], 2.0);
        assert_eq!(ended["finished"], true);

        std::fs::write(&movie.path, avi_head(0)).expect("avi");
        let killed = movie.report(true);
        assert_eq!(killed["finished"], false);
        assert!(killed["note"].as_str().unwrap_or("").contains("killed"));
    }

    #[test]
    fn preparing_removes_an_earlier_recording_and_hides_a_new_folder_from_the_editor() {
        let project = tempfile::tempdir().expect("project");
        let movie = Movie {
            path: project.path().join("movies/clip.png"),
            format: MovieFormat::Png,
            fps: 60,
            quit_after_frames: None,
        };
        assert!(movie.prepare(project.path()).expect("prepare"));
        assert!(project.path().join("movies/.gdignore").is_file());
        std::fs::write(project.path().join("movies/clip00000000.png"), b"x").expect("frame");
        std::fs::write(project.path().join("movies/clip.wav"), b"x").expect("audio");
        std::fs::write(project.path().join("movies/keep.png"), b"x").expect("other");
        assert_eq!(movie.existing_outputs().len(), 2);
        assert!(!movie.prepare(project.path()).expect("prepare again"));
        assert!(movie.existing_outputs().is_empty());
        assert!(project.path().join("movies/keep.png").is_file());
    }
}
