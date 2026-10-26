//! Input steps: the gestures the server turns into plain steps, and the compact reply.

use super::{params_without_run, runtime_error};
use crate::protocol::ToolResult;
use crate::tools::{ToolManager, str_arg};
use serde_json::{Value, json};

pub(super) async fn game_input(tools: &ToolManager, args: &Value) -> ToolResult {
    let verbose = args
        .get("verbose")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut params = params_without_run(args);
    if let Some(map) = params.as_object_mut() {
        map.remove("verbose");
    }
    if let Err(message) = prepare_input_steps(&mut params) {
        return ToolResult::error(
            message,
            &["Give path 2 to 256 points of [x, y], or a circle with a positive radius"],
        );
    }
    match tools
        .runtime_call(str_arg(args, "run_id"), "input.run", params)
        .await
    {
        Ok(mut value) => {
            compact_input_reply(&mut value, verbose);
            ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default())
        }
        Err(error) => runtime_error(&error),
    }
}

const MAX_PATH_POINTS: i64 = 256;

const COMPACT_AFTER: usize = 20;

/// Turns each `circle` step into a `path`. The game plays that one gesture.
fn prepare_input_steps(params: &mut Value) -> Result<(), String> {
    let Some(steps) = params.get_mut("steps").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (index, step) in steps.iter_mut().enumerate() {
        if step.get("type").and_then(Value::as_str) != Some("circle") {
            continue;
        }
        let points = circle_from_step(step, index)?;
        let Some(map) = step.as_object_mut() else {
            return Err(format!("steps[{index}] must be an object"));
        };
        map.insert("type".into(), json!("path"));
        map.insert("points".into(), json!(points));
        map.remove("center");
        map.remove("radius");
        map.remove("turns");
        map.remove("points_per_turn");
    }
    Ok(())
}

fn circle_from_step(step: &Value, index: usize) -> Result<Vec<[f64; 2]>, String> {
    let Some(pair) = step
        .get("center")
        .and_then(Value::as_array)
        .filter(|pair| pair.len() >= 2)
    else {
        return Err(format!("steps[{index}]: circle needs center [x, y]"));
    };
    let Some(x) = json_f64(&pair[0]) else {
        return Err(format!("steps[{index}]: circle center x must be a number"));
    };
    let Some(y) = json_f64(&pair[1]) else {
        return Err(format!("steps[{index}]: circle center y must be a number"));
    };
    let Some(radius) = step.get("radius").and_then(json_f64) else {
        return Err(format!("steps[{index}]: circle needs a radius"));
    };
    if !radius.is_finite() || radius <= 0.0 {
        return Err(format!(
            "steps[{index}]: circle radius must be greater than 0"
        ));
    }
    let turns = match step.get("turns") {
        Some(value) => json_whole(value)
            .ok_or_else(|| format!("steps[{index}]: turns must be a whole number from 1 to 8"))?,
        None => 1,
    };
    if !(1..=8).contains(&turns) {
        return Err(format!(
            "steps[{index}]: turns must be a whole number from 1 to 8"
        ));
    }
    let per = match step.get("points_per_turn") {
        Some(value) => json_whole(value).ok_or_else(|| {
            format!("steps[{index}]: points_per_turn must be a whole number from 4 to 64")
        })?,
        None => 16,
    };
    if !(4..=64).contains(&per) {
        return Err(format!(
            "steps[{index}]: points_per_turn must be a whole number from 4 to 64"
        ));
    }
    let count = turns.saturating_mul(per).saturating_add(1);
    if count > MAX_PATH_POINTS {
        return Err(format!(
            "steps[{index}]: a circle may have at most {MAX_PATH_POINTS} points"
        ));
    }
    Ok(circle_points(x, y, radius, turns, per))
}

fn circle_points(x: f64, y: f64, radius: f64, turns: i64, points_per_turn: i64) -> Vec<[f64; 2]> {
    let total = turns * points_per_turn;
    (0..=total)
        .map(|index| {
            let angle = std::f64::consts::TAU * (index as f64) / (points_per_turn as f64);
            [x + radius * angle.cos(), y + radius * angle.sin()]
        })
        .collect()
}

fn json_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
}

fn json_whole(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    value
        .as_f64()
        .filter(|number| number.fract() == 0.0 && number.is_finite())
        .map(|number| number as i64)
}

/// A long step list answers with a count. `verbose` keeps every step.
fn compact_input_reply(value: &mut Value, verbose: bool) {
    let Some(steps) = value.get("steps").and_then(Value::as_array).cloned() else {
        return;
    };
    if verbose || steps.len() <= COMPACT_AFTER {
        return;
    }
    let mut ok_count = 0_u64;
    let mut failed = Vec::new();
    for (index, step) in steps.into_iter().enumerate() {
        if step.get("ok").and_then(Value::as_bool) == Some(true) {
            ok_count += 1;
            continue;
        }
        let mut item = step;
        if let Some(map) = item.as_object_mut() {
            map.entry("index").or_insert_with(|| json!(index));
        }
        failed.push(item);
    }
    if let Some(map) = value.as_object_mut() {
        map.remove("steps");
        map.insert("ok_count".into(), json!(ok_count));
        map.insert("failed".into(), Value::Array(failed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_circle_becomes_a_closed_path_and_a_long_reply_shrinks() {
        let mut params = json!({
            "steps": [{
                "type": "circle",
                "center": [0, 0],
                "radius": 10,
                "turns": 1,
                "points_per_turn": 4,
                "pointer": "touch",
                "index": 2
            }]
        });
        prepare_input_steps(&mut params).expect("circle");
        let step = &params["steps"][0];
        assert_eq!(step["type"], "path");
        assert_eq!(step["pointer"], "touch");
        assert_eq!(step["index"], 2);
        assert!(step.get("radius").is_none());
        let points = step["points"].as_array().expect("points");
        assert_eq!(points.len(), 5);
        let first = &points[0];
        let last = &points[4];
        assert!((first[0].as_f64().unwrap() - 10.0).abs() < 1e-9);
        assert!(first[1].as_f64().unwrap().abs() < 1e-9);
        assert!(points[1][0].as_f64().unwrap().abs() < 1e-9);
        assert!((points[1][1].as_f64().unwrap() - 10.0).abs() < 1e-9);
        assert!((last[0].as_f64().unwrap() - first[0].as_f64().unwrap()).abs() < 1e-9);
        assert!((last[1].as_f64().unwrap() - first[1].as_f64().unwrap()).abs() < 1e-9);

        let mut too_big = json!({"steps": [{"type": "circle", "center": [0, 0], "radius": 1, "turns": 8, "points_per_turn": 64}]});
        let error = prepare_input_steps(&mut too_big).expect_err("too many points");
        assert!(error.contains("256"), "{error}");

        let mut reply = json!({
            "steps": (0..21).map(|index| json!({"type": "key", "ok": index != 3})).collect::<Vec<_>>(),
            "warp_mouse": false
        });
        compact_input_reply(&mut reply, false);
        assert!(reply.get("steps").is_none(), "{reply}");
        assert_eq!(reply["ok_count"], 20);
        assert_eq!(reply["failed"].as_array().map(Vec::len), Some(1));
        assert_eq!(reply["failed"][0]["index"], 3);
        assert_eq!(reply["warp_mouse"], false);

        let mut short = json!({"steps": [{"type": "key", "ok": true}]});
        compact_input_reply(&mut short, false);
        assert_eq!(short["steps"].as_array().map(Vec::len), Some(1));

        let mut listed = json!({
            "steps": (0..21).map(|_| json!({"type": "key", "ok": true})).collect::<Vec<_>>()
        });
        compact_input_reply(&mut listed, true);
        assert_eq!(listed["steps"].as_array().map(Vec::len), Some(21));
    }
}
