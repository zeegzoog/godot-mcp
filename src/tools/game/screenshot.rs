//! Screenshots: the capture call and the text that tells the caller how to read the picture.

use super::{params_without_run, runtime_error};
use crate::protocol::ToolResult;
use crate::tools::{ToolManager, str_arg};
use serde_json::Value;

pub(super) async fn screenshot(tools: &ToolManager, args: &Value) -> ToolResult {
    let value = match tools
        .runtime_call(
            str_arg(args, "run_id"),
            "game.screenshot",
            params_without_run(args),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return runtime_error(&error),
    };
    let Some(data) = value.get("data").and_then(Value::as_str) else {
        return ToolResult::error(
            "The game did not return image data",
            &["Start a windowed game and retry godot_game_screenshot"],
        );
    };
    let bytes = match base64_decode(data) {
        Some(bytes) => bytes,
        None => {
            return ToolResult::error(
                "Screenshot payload was not valid base64",
                &["Update the Godot MCP addon runtime"],
            );
        }
    };
    let Some((width, height)) = png_dimensions(&bytes) else {
        return ToolResult::error(
            "Screenshot payload was not a PNG",
            &["The capture failed before a real image was produced"],
        );
    };
    let source = value
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("viewport");
    ToolResult::image_with_text(
        screenshot_note(&value, width, height, source),
        data,
        "image/png",
    )
}

fn screenshot_note(value: &Value, width: u32, height: u32, source: &str) -> String {
    let frames = value.get("frames").and_then(Value::as_u64).unwrap_or(1);
    let mut note = format!("Game screenshot {width}x{height} ({source})");
    if frames > 1 {
        let columns = value.get("columns").and_then(Value::as_u64).unwrap_or(1);
        note.push_str(&format!(", {frames} frames, {columns} columns"));
    }
    if let Some(mapping) = input_mapping(value, width, height, frames) {
        note.push_str(", ");
        note.push_str(&mapping);
    }
    match value
        .get("diff")
        .and_then(|diff| diff.get("compared"))
        .and_then(Value::as_bool)
    {
        Some(true) => {
            let changed = value["diff"]
                .get("changed")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            note.push_str(&format!(", changed {changed} px"));
            if let Some(rect) = value["diff"].get("rect").filter(|rect| rect.is_object()) {
                note.push_str(&format!(
                    " in {},{},{}x{}",
                    rect["x"].as_i64().unwrap_or(0),
                    rect["y"].as_i64().unwrap_or(0),
                    rect["w"].as_i64().unwrap_or(0),
                    rect["h"].as_i64().unwrap_or(0),
                ));
            }
            match value["diff"].get("exact").and_then(Value::as_bool) {
                Some(true) => note.push_str(", pixel count exact"),
                Some(false) => note.push_str(", pixel count sampled"),
                None => {}
            }
        }
        Some(false) => match value["diff"].get("reason").and_then(Value::as_str) {
            Some(reason) if reason != "no previous capture" => {
                note.push_str(&format!(", not compared: {reason}"));
            }
            _ => note.push_str(", no earlier capture to compare"),
        },
        None => {}
    }
    append_baseline(&mut note, value);
    append_render(&mut note, value, frames);
    note
}

/// Which drawn frame the image is, and a warning when the window drew nothing new before the
/// capture: the viewport texture then still holds an older frame than the game state.
fn append_render(note: &mut String, value: &Value, frames: u64) {
    let Some(render) = value.get("render").filter(|item| item.is_object()) else {
        return;
    };
    let drawn = json_number(render.get("frames_drawn"));
    let process = json_number(render.get("process_frame"));
    note.push_str(&format!(
        ", frames drawn {drawn} at process frame {process}"
    ));
    let stale = render
        .get("stale_frames")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if stale == 0 {
        return;
    }
    let waited = json_number(render.get("waited_frames"));
    if frames > 1 {
        note.push_str(&format!(
            ". STALE: {stale} of {frames} frames were taken while the window drew no new frame (each waited up to {waited} process frames), so they show older frames than the game state."
        ));
    } else {
        note.push_str(&format!(
            ". STALE: the window drew no new frame in the {waited} process frames before the capture, so the image shows an older frame than the game state."
        ));
    }
    if render.get("window_can_draw").and_then(Value::as_bool) == Some(false) {
        note.push_str(" The window reports that it cannot draw (minimized or hidden).");
    }
    note.push_str(
        " Bring the game window to the front and keep it uncovered (macOS stops drawing a covered or minimized window), then capture again; read state with godot_game_get instead of trusting this image.",
    );
}

/// How returned image pixels map onto the coordinate space `godot_game_input` uses.
///
/// Absent when the capture did not report an input size. A sheet's scale is per frame.
/// A capture of some other viewport names that viewport and scales to it.
fn input_mapping(value: &Value, image_w: u32, image_h: u32, frames: u64) -> Option<String> {
    let input_w = json_u32(value, "input_width")?;
    let input_h = json_u32(value, "input_height")?;
    let frame_w = json_u32(value, "frame_width").unwrap_or(image_w);
    let frame_h = json_u32(value, "frame_height").unwrap_or(image_h);
    if frame_w == 0 || frame_h == 0 {
        return None;
    }
    let captured_w = json_u32(value, "captured_width").unwrap_or(input_w);
    let captured_h = json_u32(value, "captured_height").unwrap_or(input_h);
    let same_viewport = captured_w == input_w && captured_h == input_h;
    let (scale_x, scale_y) = if same_viewport {
        (
            f64::from(input_w) / f64::from(frame_w),
            f64::from(input_h) / f64::from(frame_h),
        )
    } else {
        (
            f64::from(captured_w) / f64::from(frame_w),
            f64::from(captured_h) / f64::from(frame_h),
        )
    };
    let per_frame = if frames > 1 {
        format!(" per frame (frame {frame_w}x{frame_h})")
    } else {
        String::new()
    };
    let scale = scale_phrase(scale_x, scale_y);
    if same_viewport {
        Some(format!(
            "input space {input_w}x{input_h}, image scale {scale}{per_frame}"
        ))
    } else {
        Some(format!(
            "input space {input_w}x{input_h}, captured viewport {captured_w}x{captured_h}, image scale {scale} to the captured viewport{per_frame}"
        ))
    }
}

fn scale_phrase(scale_x: f64, scale_y: f64) -> String {
    if !scale_x.is_finite() || !scale_y.is_finite() {
        return "?".into();
    }
    if (scale_x - scale_y).abs() < 0.005 {
        format_scale(scale_x)
    } else {
        format!("{} by {}", format_scale(scale_x), format_scale(scale_y))
    }
}

fn format_scale(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "?".into();
    }
    let rounded = (value * 1000.0).round() / 1000.0;
    if (rounded - rounded.round()).abs() < 1e-9 {
        return format!("{}", rounded.round() as i64);
    }
    format!("{rounded:.3}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    let item = value.get(key)?;
    if let Some(number) = item.as_u64() {
        return u32::try_from(number).ok();
    }
    if let Some(number) = item.as_i64() {
        return u32::try_from(number).ok();
    }
    let number = item.as_f64()?;
    if number.is_finite() && number >= 0.0 && number <= f64::from(u32::MAX) {
        Some(number.round() as u32)
    } else {
        None
    }
}

fn append_baseline(note: &mut String, value: &Value) {
    let Some(baseline) = value.get("baseline").filter(|item| item.is_object()) else {
        return;
    };
    if let Some(saved) = baseline.get("saved").and_then(Value::as_str) {
        note.push_str(&format!(", saved baseline {saved}"));
    }
    if baseline.get("compared").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let name = baseline
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("baseline");
    let changed = json_number(baseline.get("changed"));
    let sampled = json_number(baseline.get("sampled"));
    let ratio = json_number(baseline.get("ratio"));
    let tolerance = json_number(baseline.get("tolerance"));
    let kind = if baseline.get("exact").and_then(Value::as_bool) == Some(true) {
        "exact"
    } else {
        "sampled"
    };
    note.push_str(&format!(
        ", baseline {name} changed {changed} px, sampled {sampled}, ratio {ratio}, tolerance {tolerance}, {kind}, roi_space returned_image"
    ));
    if let Some(roi) = baseline.get("roi").filter(|roi| roi.is_object()) {
        note.push_str(&format!(
            ", roi {},{},{}x{}",
            roi["x"].as_i64().unwrap_or(0),
            roi["y"].as_i64().unwrap_or(0),
            roi["w"].as_i64().unwrap_or(0),
            roi["h"].as_i64().unwrap_or(0),
        ));
    }
    if let Some(bounds) = baseline.get("bounds").filter(|bounds| bounds.is_object()) {
        note.push_str(&format!(
            ", bounds {},{},{}x{}",
            bounds["x"].as_i64().unwrap_or(0),
            bounds["y"].as_i64().unwrap_or(0),
            bounds["w"].as_i64().unwrap_or(0),
            bounds["h"].as_i64().unwrap_or(0),
        ));
    }
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    if width == 0 || height == 0 {
        None
    } else {
        Some((width, height))
    }
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 128] = &{
        let mut table = [255_u8; 128];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut index = 0;
        while index < alphabet.len() {
            table[alphabet[index] as usize] = index as u8;
            index += 1;
        }
        table
    };
    let mut out = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0;
    for byte in text.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        if byte > 127 || TABLE[byte as usize] == 255 {
            return None;
        }
        buffer = (buffer << 6) | u32::from(TABLE[byte as usize]);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn json_number(value: Option<&Value>) -> String {
    value
        .map(|item| serde_json::to_string(item).unwrap_or_else(|_| "0".into()))
        .unwrap_or_else(|| "0".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn screenshot_note_names_a_sheet_and_the_changed_rect() {
        let first = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 32,
                "input_height": 16,
                "frame_width": 32,
                "frame_height": 16,
                "diff": {"compared": false}
            }),
            32,
            16,
            "main window",
        );
        assert_eq!(
            first,
            "Game screenshot 32x16 (main window), input space 32x16, image scale 1, no earlier capture to compare"
        );
        let sheet = screenshot_note(
            &json!({
                "frames": 4,
                "columns": 2,
                "input_width": 64,
                "input_height": 64,
                "frame_width": 32,
                "frame_height": 32,
                "diff": {"compared": true, "changed": 12, "rect": {"x": 1, "y": 2, "w": 3, "h": 4}}
            }),
            64,
            64,
            "main window",
        );
        assert_eq!(
            sheet,
            "Game screenshot 64x64 (main window), 4 frames, 2 columns, input space 64x64, image scale 2 per frame (frame 32x32), changed 12 px in 1,2,3x4"
        );
    }

    #[test]
    fn screenshot_note_maps_image_pixels_onto_the_input_space() {
        let scaled = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 540,
                "input_height": 900,
                "captured_width": 540,
                "captured_height": 900,
                "frame_width": 360,
                "frame_height": 600,
                "diff": {"compared": false}
            }),
            360,
            600,
            "main window",
        );
        assert_eq!(
            scaled,
            "Game screenshot 360x600 (main window), input space 540x900, image scale 1.5, no earlier capture to compare"
        );
        let other = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 540,
                "input_height": 900,
                "captured_width": 200,
                "captured_height": 100,
                "frame_width": 100,
                "frame_height": 50
            }),
            100,
            50,
            "SubViewport",
        );
        assert!(
            other.contains(
                "input space 540x900, captured viewport 200x100, image scale 2 to the captured viewport"
            ),
            "{other}"
        );
        let uneven = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 100,
                "input_height": 40,
                "frame_width": 50,
                "frame_height": 10
            }),
            50,
            10,
            "main window",
        );
        assert!(
            uneven.contains("input space 100x40, image scale 2 by 4"),
            "{uneven}"
        );
    }

    #[test]
    fn screenshot_note_reports_a_named_baseline() {
        let note = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 32,
                "input_height": 16,
                "frame_width": 32,
                "frame_height": 16,
                "diff": {"compared": false},
                "baseline": {
                    "compared": true,
                    "name": "menu",
                    "changed": 4,
                    "sampled": 10,
                    "ratio": 0.4,
                    "tolerance": 8,
                    "exact": true,
                    "roi_space": "returned_image",
                    "roi": {"x": 1, "y": 2, "w": 3, "h": 4},
                    "bounds": {"x": 1, "y": 2, "w": 3, "h": 4},
                    "saved": "menu"
                }
            }),
            32,
            16,
            "main window",
        );
        assert_eq!(
            note,
            "Game screenshot 32x16 (main window), input space 32x16, image scale 1, no earlier capture to compare, saved baseline menu, baseline menu changed 4 px, sampled 10, ratio 0.4, tolerance 8, exact, roi_space returned_image, roi 1,2,3x4, bounds 1,2,3x4"
        );
        let sampled = screenshot_note(
            &json!({
                "frames": 1,
                "baseline": {
                    "compared": true,
                    "name": "wide",
                    "changed": 1,
                    "sampled": 2,
                    "ratio": 0.5,
                    "tolerance": 0,
                    "exact": false,
                    "roi": {"x": 0, "y": 0, "w": 2, "h": 2}
                }
            }),
            8,
            8,
            "main window",
        );
        assert!(
            sampled.contains("sampled") && !sampled.contains("exact"),
            "{sampled}"
        );
    }

    #[test]
    fn screenshot_note_names_the_drawn_frame() {
        let note = screenshot_note(
            &json!({
                "frames": 1,
                "diff": {"compared": false},
                "render": {"frames_drawn": 120, "process_frame": 124, "stale_frames": 0, "window_can_draw": true}
            }),
            8,
            8,
            "main window",
        );
        assert_eq!(
            note,
            "Game screenshot 8x8 (main window), no earlier capture to compare, frames drawn 120 at process frame 124"
        );
    }

    #[test]
    fn screenshot_note_warns_when_the_window_drew_nothing_new() {
        let note = screenshot_note(
            &json!({
                "frames": 1,
                "diff": {"compared": false},
                "render": {
                    "frames_drawn": 3,
                    "process_frame": 190,
                    "stale_frames": 1,
                    "waited_frames": 30,
                    "window_can_draw": true
                }
            }),
            8,
            8,
            "main window",
        );
        assert!(
            note.starts_with(
                "Game screenshot 8x8 (main window), no earlier capture to compare, frames drawn 3 at process frame 190. STALE: the window drew no new frame in the 30 process frames before the capture, so the image shows an older frame than the game state."
            ),
            "{note}"
        );
        assert!(
            note.contains("Bring the game window to the front"),
            "{note}"
        );
        assert!(!note.contains("cannot draw"), "{note}");

        let hidden = screenshot_note(
            &json!({
                "frames": 4,
                "columns": 2,
                "render": {
                    "frames_drawn": 3,
                    "process_frame": 190,
                    "stale_frames": 2,
                    "waited_frames": 30,
                    "window_can_draw": false
                }
            }),
            16,
            16,
            "main window",
        );
        assert!(
            hidden.contains("STALE: 2 of 4 frames were taken while the window drew no new frame"),
            "{hidden}"
        );
        assert!(
            hidden.contains("The window reports that it cannot draw (minimized or hidden)"),
            "{hidden}"
        );
    }

    #[test]
    fn screenshot_note_says_why_two_captures_were_not_compared() {
        let note = screenshot_note(
            &json!({
                "frames": 1,
                "input_width": 64,
                "input_height": 64,
                "diff": {"compared": false, "reason": "the previous capture had another size"}
            }),
            64,
            64,
            "main window",
        );
        assert_eq!(
            note,
            "Game screenshot 64x64 (main window), input space 64x64, image scale 1, not compared: the previous capture had another size"
        );
    }

    #[test]
    fn png_magic_is_rejected_when_absent() {
        assert!(base64_decode("aaaa").is_some());
        assert!(base64_decode("@@@").is_none());
        assert!(png_dimensions(b"not a png").is_none());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0; 8]);
        png.extend_from_slice(&32u32.to_be_bytes());
        png.extend_from_slice(&16u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), Some((32, 16)));
        png[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert!(png_dimensions(&png).is_none());
    }
}
