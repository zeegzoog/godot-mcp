//! Recordings: PNG frames below the temporary directory and the optional movie.

use super::{params_without_run, runtime_error};
use crate::paths::{resolve_in_project, same_directory};
use crate::protocol::ToolResult;
use crate::tools::{ToolManager, str_arg};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const MAX_RECORD_FRAMES: i64 = 240;

const MAX_RECORD_EVERY: i64 = 60;

const MAX_RECORD_SPAN: i64 = 480;

pub(super) async fn record(tools: &ToolManager, args: &Value) -> ToolResult {
    let frames = match bounded_int(args, "frames", 60, 1, MAX_RECORD_FRAMES) {
        Ok(value) => value,
        Err(result) => return result,
    };
    let every = match bounded_int(args, "every", 1, 1, MAX_RECORD_EVERY) {
        Ok(value) => value,
        Err(result) => return result,
    };
    if frames.saturating_mul(every) > MAX_RECORD_SPAN {
        return ToolResult::error(
            format!("frames * every must be at most {MAX_RECORD_SPAN} process frames"),
            &["Lower frames or every. 180 frames with every 1 is about three seconds at 60 fps"],
        );
    }
    let max_dimension = match bounded_int(args, "max_dimension", 768, 16, 4096) {
        Ok(value) => value,
        Err(result) => return result,
    };
    let format = str_arg(args, "format").unwrap_or("png");
    if !matches!(format, "png" | "gif" | "mp4") {
        return ToolResult::error(
            "format must be png, gif, or mp4",
            &["Omit format to keep the PNG files only"],
        );
    }
    let directory = match prepare_record_dir(str_arg(args, "directory")) {
        Ok(directory) => directory,
        Err(result) => return result,
    };
    let mut params = params_without_run(args);
    if let Some(map) = params.as_object_mut() {
        map.insert("directory".into(), json!(directory.display().to_string()));
        map.insert("frames".into(), json!(frames));
        map.insert("every".into(), json!(every));
        map.insert("max_dimension".into(), json!(max_dimension));
        map.remove("format");
    }
    let value = match tools
        .runtime_call(str_arg(args, "run_id"), "game.record", params)
        .await
    {
        Ok(value) => value,
        Err(error) => return runtime_error(&error),
    };
    let Some(reported) = value.get("frames").and_then(Value::as_array) else {
        return ToolResult::error(
            "The game did not return a frame list",
            &["Update the Godot MCP addon runtime"],
        );
    };
    let expected = usize::try_from(frames).unwrap_or(usize::MAX);
    if reported.len() != expected {
        return ToolResult::error(
            format!(
                "The recording saved {} frames, not {frames}",
                reported.len()
            ),
            &["Retry godot_game_record. A closed window stops the recording"],
        );
    }
    let mut frames_out = Vec::with_capacity(reported.len());
    for (offset, frame) in reported.iter().enumerate() {
        let index = offset + 1;
        let path = directory.join(format!("frame-{index:04}.png"));
        if !is_png(&path) {
            return ToolResult::error(
                format!("{} is not a PNG", path.display()),
                &["The game could not finish writing the frame"],
            );
        }
        let Some(object) = frame.as_object() else {
            return ToolResult::error(
                "A recorded frame was not an object",
                &["Update the Godot MCP addon runtime"],
            );
        };
        let mut item = object.clone();
        item.insert("index".into(), json!(index));
        item.insert("file".into(), json!(format!("frame-{index:04}.png")));
        item.insert("path".into(), json!(path.display().to_string()));
        frames_out.push(Value::Object(item));
    }
    let rate = recording_rate(&frames_out);
    let video = encode_video(&directory, format, frames, &rate).await;
    let mut body = value;
    if let Some(object) = body.as_object_mut() {
        object.insert("directory".into(), json!(directory.display().to_string()));
        object.insert("count".into(), json!(frames));
        object.insert("frames".into(), Value::Array(frames_out));
        object.insert("video".into(), video);
    }
    ToolResult::text(serde_json::to_string_pretty(&body).unwrap_or_default())
}

fn bounded_int(
    args: &Value,
    key: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, ToolResult> {
    let Some(value) = args.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let Some(number) = value.as_i64() else {
        return Err(ToolResult::error(
            format!("{key} must be an integer from {min} to {max}"),
            &["Pass a whole number"],
        ));
    };
    if !(min..=max).contains(&number) {
        return Err(ToolResult::error(
            format!("{key} must be from {min} to {max}"),
            &["Use a value inside the documented range"],
        ));
    }
    Ok(number)
}

fn prepare_record_dir(requested: Option<&str>) -> Result<PathBuf, ToolResult> {
    let temp_root = std::env::temp_dir();
    let directory = if let Some(text) = requested {
        let hint = format!(
            "Pass an absolute directory below {}, or omit directory",
            temp_root.display()
        );
        if text.is_empty() || text.chars().any(char::is_control) {
            return Err(ToolResult::error(
                "directory must be an absolute path",
                &[hint.as_str()],
            ));
        }
        let path = PathBuf::from(text);
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(ToolResult::error(
                "directory must be an absolute path and must not contain ..",
                &[hint.as_str()],
            ));
        }
        // A tool call must not write files anywhere on the machine: the recording goes below the
        // system temporary directory, links in the part that exists included.
        let inside = resolve_in_project(&temp_root, text)
            .ok()
            .filter(|resolved| !same_directory(resolved, &temp_root));
        inside.ok_or_else(|| {
            ToolResult::error(
                format!(
                    "directory must be a folder inside the system temporary directory {}",
                    temp_root.display()
                ),
                &[hint.as_str()],
            )
        })?
    } else {
        // Straight into the temporary directory, with an unguessable name: a shared parent folder
        // could be made by another user beforehand and then swapped under the recording.
        temp_root.join(format!(
            "godot-mcp-record-{}",
            uuid::Uuid::new_v4().simple()
        ))
    };
    if directory.is_file() {
        return Err(ToolResult::error(
            format!("directory is a file: {}", directory.display()),
            &["Pass a directory, or omit directory for a temporary one"],
        ));
    }
    if let Err(error) = create_private_dir(&directory) {
        return Err(ToolResult::error(
            format!("Could not create the recording directory: {error}"),
            &["Choose a directory you can write to"],
        ));
    }
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(ToolResult::error(
                format!("Could not read the recording directory: {error}"),
                &["Choose a directory you can write to"],
            ));
        }
    };
    let occupied = entries.flatten().any(|entry| {
        let name = entry.file_name();
        // Compared without case: a Windows or macOS volume treats Recording.GIF as recording.gif.
        let name = name.to_string_lossy().to_lowercase();
        (name.starts_with("frame-") && name.ends_with(".png")) || name.starts_with("recording.")
    });
    if occupied {
        return Err(ToolResult::error(
            "directory already contains frame PNG files or a recording",
            &["Pass an empty directory, or omit directory for a new one"],
        ));
    }
    Ok(directory)
}

/// Creates the folder and its missing parents readable by this user only (`0700` on Unix), so the
/// frames of a game are not listed to the other accounts of a shared machine.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

fn is_png(path: &Path) -> bool {
    let mut header = [0_u8; 8];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    std::io::Read::read_exact(&mut file, &mut header).is_ok()
        && header == [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
}

fn recording_rate(frames: &[Value]) -> String {
    let msec = |frame: &Value| frame.get("msec").and_then(Value::as_f64);
    let first = frames.first().and_then(msec).unwrap_or(0.0);
    let last = frames.last().and_then(msec).unwrap_or(first);
    let span = last - first;
    let rate = if frames.len() > 1 && span > 0.0 {
        (frames.len() - 1) as f64 * 1000.0 / span
    } else {
        10.0
    };
    format!("{:.2}", rate.clamp(1.0, 60.0))
}

async fn encode_video(directory: &Path, format: &str, count: i64, rate: &str) -> Value {
    encode_video_with(directory, format, count, rate, "ffmpeg").await
}

async fn encode_video_with(
    directory: &Path,
    format: &str,
    count: i64,
    rate: &str,
    program: &str,
) -> Value {
    if format == "png" {
        return json!({"format": "png", "ffmpeg": "not_requested"});
    }
    let video_path = directory.join(format!("recording.{format}"));
    let mut command = tokio::process::Command::new(program);
    command
        .current_dir(directory)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-framerate")
        .arg(rate)
        .arg("-start_number")
        .arg("1")
        .arg("-i")
        .arg("frame-%04d.png")
        .arg("-frames:v")
        .arg(count.to_string());
    if format == "mp4" {
        command.args([
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",
            "-pix_fmt",
            "yuv420p",
        ]);
    }
    command.arg(&video_path);
    let outcome = tokio::time::timeout(Duration::from_secs(60), command.output()).await;
    match outcome {
        Ok(Ok(output)) if output.status.success() && video_path.is_file() => json!({
            "format": format,
            "path": video_path.display().to_string(),
            "ffmpeg": "used",
        }),
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({"format": format, "ffmpeg": "not_found"})
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("ffmpeg failed")
                .trim();
            json!({"format": format, "ffmpeg": "failed", "error": detail})
        }
        Ok(Err(error)) => json!({"format": format, "ffmpeg": "failed", "error": error.to_string()}),
        Err(_) => json!({"format": format, "ffmpeg": "failed", "error": "ffmpeg timed out"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_ffmpeg_leaves_the_request_as_png_only() {
        let dir = tempfile::tempdir().expect("record dir");
        let video = encode_video_with(dir.path(), "gif", 1, "10", "godot-mcp-ffmpeg-missing").await;
        assert_eq!(video["ffmpeg"], "not_found");
        assert!(video.get("path").is_none());
    }

    #[test]
    fn the_recording_rate_follows_the_capture_times_and_stays_in_range() {
        let frames = |times: &[f64]| {
            times
                .iter()
                .map(|msec| json!({"msec": msec}))
                .collect::<Vec<_>>()
        };
        assert_eq!(recording_rate(&frames(&[0.0, 100.0, 200.0])), "10.00");
        assert_eq!(recording_rate(&frames(&[0.0, 1.0, 2.0])), "60.00");
        assert_eq!(recording_rate(&frames(&[0.0, 5000.0])), "1.00");
        assert_eq!(recording_rate(&frames(&[3.0, 3.0, 3.0])), "10.00");
        assert_eq!(recording_rate(&frames(&[7.0])), "10.00");
        assert_eq!(recording_rate(&[]), "10.00");
    }

    #[test]
    fn a_directory_with_frames_or_a_recording_is_refused_whatever_its_case() {
        for name in [
            "frame-0001.png",
            "FRAME-0001.PNG",
            "recording.mp4",
            "Recording.GIF",
        ] {
            let dir = tempfile::tempdir().expect("record dir");
            std::fs::write(dir.path().join(name), b"earlier").expect("write");
            assert!(prepare_record_dir(dir.path().to_str()).is_err(), "{name}");
            assert_eq!(
                std::fs::read(dir.path().join(name)).expect("read"),
                b"earlier"
            );
        }
    }

    #[test]
    fn the_temporary_directory_itself_is_not_a_recording_directory() {
        let root = std::env::temp_dir();
        assert!(prepare_record_dir(root.to_str()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_recording_directory_is_private_and_directly_in_the_temporary_directory() {
        use std::os::unix::fs::PermissionsExt;
        let Ok(directory) = prepare_record_dir(None) else {
            panic!("no default recording directory");
        };
        let mode = std::fs::metadata(&directory)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        let parent = directory.parent().map(Path::to_path_buf);
        let name = directory
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        std::fs::remove_dir(&directory).expect("remove");
        assert_eq!(mode, 0o700);
        assert_eq!(parent, Some(std::env::temp_dir()));
        assert!(name.starts_with("godot-mcp-record-"), "{name}");
    }

    /// Runs `encode_video_with` against a shell script that stands in for ffmpeg. A script written
    /// a moment ago can still be "busy" on Linux while another test forks, so that is retried.
    #[cfg(unix)]
    async fn encode_with_stub(dir: &Path, format: &str, count: i64, script: &str) -> Value {
        use std::os::unix::fs::PermissionsExt;
        let stub = dir.join("stub-ffmpeg.sh");
        std::fs::write(&stub, script).expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let program = stub.to_string_lossy().to_string();
        for _ in 0..40 {
            let video = encode_video_with(dir, format, count, "12.50", &program).await;
            let busy = video["error"]
                .as_str()
                .is_some_and(|text| text.to_lowercase().contains("busy"));
            if !busy {
                return video;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the stub stayed busy");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ffmpeg_is_given_the_frames_and_the_movie_is_reported() {
        let dir = tempfile::tempdir().expect("record dir");
        let script = "#!/bin/sh\necho \"$@\" > args.txt\nfor last; do :; done\n: > \"$last\"\n";
        let video = encode_with_stub(dir.path(), "mp4", 3, script).await;
        assert_eq!(video["ffmpeg"], "used", "{video}");
        assert_eq!(
            video["path"],
            dir.path().join("recording.mp4").display().to_string()
        );
        let arguments = std::fs::read_to_string(dir.path().join("args.txt")).expect("args");
        for expected in [
            "-framerate 12.50",
            "-start_number 1",
            "-i frame-%04d.png",
            "-frames:v 3",
            "yuv420p",
        ] {
            assert!(arguments.contains(expected), "{expected}: {arguments}");
        }
        assert!(
            arguments.trim_end().ends_with("recording.mp4"),
            "{arguments}"
        );

        let gif = encode_with_stub(dir.path(), "gif", 3, script).await;
        assert_eq!(gif["ffmpeg"], "used", "{gif}");
        let arguments = std::fs::read_to_string(dir.path().join("args.txt")).expect("args");
        assert!(!arguments.contains("yuv420p"), "{arguments}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_ffmpeg_is_reported_with_its_first_line_and_leaves_no_movie() {
        let dir = tempfile::tempdir().expect("record dir");
        let script = "#!/bin/sh\necho 'Unable to find a suitable output format' >&2\necho second >&2\nexit 1\n";
        let video = encode_with_stub(dir.path(), "gif", 2, script).await;
        assert_eq!(video["ffmpeg"], "failed", "{video}");
        assert_eq!(video["error"], "Unable to find a suitable output format");
        assert!(video.get("path").is_none());

        let silent = encode_with_stub(dir.path(), "gif", 2, "#!/bin/sh\nexit 0\n").await;
        assert_eq!(silent["ffmpeg"], "failed", "{silent}");
        assert!(silent.get("path").is_none());
    }
}
