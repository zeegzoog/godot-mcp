//! World probes: raycasts in the running game's 3D physics world, without moving any node.

use super::{params_without_run, runtime_error};
use crate::protocol::ToolResult;
use crate::tools::{ToolManager, str_arg};
use serde_json::Value;

/// Rays in one list; the runtime refuses more.
pub(super) const MAX_RAYS: usize = 1024;
/// Down rays in one grid; the runtime refuses more.
pub(super) const MAX_GRID: u64 = 4096;

pub(super) async fn probe(tools: &ToolManager, args: &Value) -> ToolResult {
    if let Err(result) = check(args) {
        return result;
    }
    match tools
        .runtime_call(
            str_arg(args, "run_id"),
            "game.probe",
            params_without_run(args),
        )
        .await
    {
        // Compact: a grid of heights printed one number per line would be thousands of lines.
        Ok(value) => ToolResult::text(value.to_string()),
        Err(error) => runtime_error(&error),
    }
}

/// The checks that need no game: one of rays or grid, and the size limits. The runtime checks
/// each ray and the grid's numbers again.
fn check(args: &Value) -> Result<(), ToolResult> {
    let rays = args.get("rays");
    let grid = args.get("grid");
    match (rays, grid) {
        (Some(_), Some(_)) | (None, None) => Err(ToolResult::error(
            "Pass rays or grid, not both and not neither",
            &[
                "rays: [{\"from\": [x, y, z], \"to\": [x, y, z]}] or [{\"x\": 4, \"z\": -2}] for a ray straight down",
                "grid: {\"min\": [x, z], \"max\": [x, z], \"step\": 0.5} for a table of ground heights",
            ],
        )),
        (Some(rays), None) => {
            let Some(list) = rays.as_array().filter(|list| !list.is_empty()) else {
                return Err(ToolResult::error(
                    "rays must be a non-empty list",
                    &[
                        "Pass rays: [{\"x\": 4, \"z\": -2}] or [{\"from\": [0, 5, 0], \"to\": [0, -5, 0]}]",
                    ],
                ));
            };
            if list.len() > MAX_RAYS {
                return Err(ToolResult::error(
                    format!("{} rays; at most {MAX_RAYS} in one call", list.len()),
                    &[
                        "Split the rays over several calls",
                        "Use grid for an area of ground heights",
                    ],
                ));
            }
            Ok(())
        }
        (None, Some(grid)) => check_grid(grid),
    }
}

fn check_grid(grid: &Value) -> Result<(), ToolResult> {
    let corner = |key: &str| -> Option<(f64, f64)> {
        let pair = grid.get(key)?.as_array()?;
        if pair.len() != 2 {
            return None;
        }
        Some((pair[0].as_f64()?, pair[1].as_f64()?))
    };
    let (Some(low), Some(high)) = (corner("min"), corner("max")) else {
        return Err(ToolResult::error(
            "grid.min and grid.max must be [x, z]",
            &["Pass grid: {\"min\": [-10, -10], \"max\": [10, 10], \"step\": 1}"],
        ));
    };
    if high.0 < low.0 || high.1 < low.1 {
        return Err(ToolResult::error(
            "grid.max is below grid.min on x or z",
            &["Swap the corners: min holds the smaller x and z"],
        ));
    }
    let points = match (grid.get("step"), grid.get("count")) {
        (Some(step), None) => {
            let Some(step) = step.as_f64().filter(|step| *step > 0.0 && step.is_finite()) else {
                return Err(ToolResult::error(
                    "grid.step must be a number above 0",
                    &["Pass the spacing in metres, such as step: 0.5"],
                ));
            };
            let columns = ((high.0 - low.0) / step + 1e-6).floor() + 1.0;
            let rows = ((high.1 - low.1) / step + 1e-6).floor() + 1.0;
            columns * rows
        }
        (None, Some(count)) => {
            let pair = count.as_array().filter(|pair| pair.len() == 2);
            let sizes = pair.and_then(|pair| Some((pair[0].as_u64()?, pair[1].as_u64()?)));
            let Some((columns, rows)) = sizes.filter(|(columns, rows)| *columns > 0 && *rows > 0)
            else {
                return Err(ToolResult::error(
                    "grid.count must be [columns, rows], two whole numbers above 0",
                    &["Pass count: [32, 32], or step for a spacing in metres"],
                ));
            };
            columns.saturating_mul(rows) as f64
        }
        _ => {
            return Err(ToolResult::error(
                "grid needs step or count, not both and not neither",
                &["Pass step: 0.5 for a spacing in metres, or count: [32, 32]"],
            ));
        }
    };
    if points > MAX_GRID as f64 {
        return Err(ToolResult::error(
            format!("This grid has {points} points; at most {MAX_GRID}"),
            &[
                "Use a larger step or a smaller area",
                "Split the area over several calls",
            ],
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refused(args: Value) -> String {
        match check(&args) {
            Ok(()) => panic!("{args} was accepted"),
            Err(result) => serde_json::to_string(&result).unwrap_or_default(),
        }
    }

    #[test]
    fn a_probe_takes_rays_or_a_grid() {
        assert!(refused(json!({})).contains("rays or grid"));
        assert!(refused(json!({"rays": [{"x": 0, "z": 0}], "grid": {}})).contains("rays or grid"));
        assert!(refused(json!({"rays": []})).contains("non-empty"));
        assert!(check(&json!({"rays": [{"x": 0, "z": 0}]})).is_ok());
        assert!(check(&json!({"grid": {"min": [0, 0], "max": [10, 10], "step": 1}})).is_ok());
        assert!(
            check(&json!({"grid": {"min": [0, 0], "max": [10, 10], "count": [64, 64]}})).is_ok()
        );
    }

    #[test]
    fn a_probe_is_bounded() {
        let rays: Vec<Value> = (0..=MAX_RAYS).map(|x| json!({"x": x, "z": 0})).collect();
        assert!(refused(json!({"rays": rays})).contains("at most 1024"));
        // 0..=64 with step 1 is 65 x 65 = 4225 points.
        let big = json!({"grid": {"min": [0, 0], "max": [64, 64], "step": 1}});
        assert!(refused(big).contains("at most 4096"));
        // 0..=63 is 64 x 64 = 4096 points, the limit itself.
        assert!(check(&json!({"grid": {"min": [0, 0], "max": [63, 63], "step": 1}})).is_ok());
        assert!(
            refused(json!({"grid": {"min": [0, 0], "max": [1, 1], "count": [65, 64]}}))
                .contains("4096")
        );
    }

    #[test]
    fn a_bad_grid_says_what_is_wrong() {
        assert!(
            refused(json!({"grid": {"min": [0], "max": [1, 1], "step": 1}})).contains("[x, z]")
        );
        assert!(
            refused(json!({"grid": {"min": [2, 0], "max": [1, 1], "step": 1}})).contains("below")
        );
        assert!(
            refused(json!({"grid": {"min": [0, 0], "max": [1, 1], "step": 0}})).contains("above 0")
        );
        assert!(refused(json!({"grid": {"min": [0, 0], "max": [1, 1]}})).contains("step or count"));
        assert!(
            refused(json!({"grid": {"min": [0, 0], "max": [1, 1], "count": [0, 2]}}))
                .contains("columns, rows")
        );
    }
}
