//! InputMap actions: listing them, and adding or removing their events through the editor.
//!
//! An action is stored in `project.godot` as a Dictionary holding serialized InputEvent objects,
//! which a plain setting edit cannot write. The editor builds the events, records the change in
//! its undo history and saves the file, so these tools have no offline fallback.

use super::{ToolManager, str_arg};
use crate::protocol::{Tool, ToolResult};
use serde_json::{Value, json};

pub const GET: &str = "godot_get_input_map";
pub const EDIT: &str = "godot_edit_input_action";

/// Characters the editor's Input Map panel refuses in an action name.
const FORBIDDEN_NAME_CHARS: &[char] = &['/', ':', '=', '\\', '"'];
const MAX_NAME_CHARS: usize = 128;
const MAX_EVENTS: usize = 32;
const MOUSE_BUTTONS: &[&str] = &[
    "left",
    "right",
    "middle",
    "wheel_up",
    "wheel_down",
    "wheel_left",
    "wheel_right",
    "xbutton1",
    "xbutton2",
];
/// Godot's mouse buttons are 1 to 9, joypad buttons 0 to 127 and joypad axes 0 to 9.
const MOUSE_BUTTON_RANGE: std::ops::RangeInclusive<i64> = 1..=9;
const JOY_BUTTON_RANGE: std::ops::RangeInclusive<i64> = 0..=127;
const JOY_AXIS_RANGE: std::ops::RangeInclusive<i64> = 0..=9;

pub fn handles(name: &str) -> bool {
    name == GET || name == EDIT
}

pub fn catalog() -> Vec<Tool> {
    let event = json!({
        "type": "object",
        "description": "One input event. type key: key (a key name such as E, Space, Up, F1), physical (default true: the key at that position on a QWERTY layout), shift, ctrl, alt, meta. type mouse_button: button (left, right, middle, wheel_up, wheel_down, wheel_left, wheel_right, xbutton1, xbutton2, or 1-9). type joypad_button: button (0-127), device (default -1, all devices). type joypad_motion: axis (0-9), axis_value (its sign picks the direction), device.",
        "properties": {
            "type": {"type": "string", "enum": ["key", "mouse_button", "joypad_button", "joypad_motion"]}
        },
        "required": ["type"]
    });
    vec![
        super::tool(
            GET,
            "Lists the project's InputMap actions with their deadzone and events, as the editor holds them. Godot's built-in ui_ actions are left out unless include_builtin is true. Requires the editor bridge.",
            json!({
                "action": {"type": "string", "description": "Only this action."},
                "include_builtin": {"type": "boolean", "description": "Also list the built-in ui_ actions. Default false."}
            }),
            &[],
        ),
        super::tool(
            EDIT,
            "Adds or removes events of one InputMap action, sets its deadzone, creates it (create true) or deletes it (delete true), and saves project.godot through the editor. An event is a key, a mouse button, a joypad button or a joypad axis. The change is one step in the editor's undo history (Edit > Undo). All or nothing: an event to remove that the action does not have, or a key name Godot does not know, changes nothing. An event the action already has is not added twice and is listed under already_present. Key events use the physical key by default. Built-in ui_ actions can be edited but not deleted. Requires the editor bridge; never edit the [input] section of project.godot by hand.",
            json!({
                "action": {"type": "string", "description": "Action name, for example jump."},
                "add_events": {"type": "array", "items": event, "description": "Events to add."},
                "remove_events": {"type": "array", "items": event, "description": "Events to remove, matched by type, key or button, modifiers and device."},
                "deadzone": {"type": "number", "minimum": 0, "maximum": 1, "description": "Deadzone from 0 to 1. A new action gets 0.2."},
                "create": {"type": "boolean", "description": "Create the action when it does not exist. Default false."},
                "delete": {"type": "boolean", "description": "Delete the action. Cannot be combined with other changes."}
            }),
            &["action"],
        ),
    ]
}

pub async fn call(tools: &ToolManager, name: &str, args: Value) -> ToolResult {
    let command = if name == GET {
        "get_input_map"
    } else {
        if let Err(problem) = check_edit(&args) {
            return ToolResult::error(
                problem,
                &[
                    "Pass action plus at least one of add_events, remove_events, deadzone, create or delete; the event shapes are in the tool's inputSchema",
                ],
            );
        }
        "edit_input_action"
    };
    match tools.bridge.send_command(command, args).await {
        Ok(value) => ToolResult::text(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(error) => super::bridge_error_result(&error),
    }
}

/// Refuses a request the editor could never apply, so a mistake is reported without an editor
/// round-trip. The editor checks the same rules again and also the key names.
fn check_edit(args: &Value) -> Result<(), String> {
    let action = str_arg(args, "action").unwrap_or("");
    check_action_name(action)?;
    let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
    let events = |key: &str| {
        args.get(key)
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
    };
    let (added, removed) = (events("add_events"), events("remove_events"));
    let deadzone = args.get("deadzone").filter(|value| !value.is_null());
    if flag("delete") {
        if !added.is_empty() || !removed.is_empty() || deadzone.is_some() || flag("create") {
            return Err(
                "delete cannot be combined with add_events, remove_events, deadzone or create"
                    .into(),
            );
        }
        if action.starts_with("ui_") {
            return Err(format!(
                "'{action}' is a built-in action: it can be edited but not deleted"
            ));
        }
        return Ok(());
    }
    if added.is_empty() && removed.is_empty() && deadzone.is_none() && !flag("create") {
        return Err(format!("nothing to change for action '{action}'"));
    }
    if let Some(value) = deadzone {
        match value.as_f64() {
            Some(number) if (0.0..=1.0).contains(&number) => {}
            _ => return Err("deadzone must be a number from 0 to 1".into()),
        }
    }
    if added.len() + removed.len() > MAX_EVENTS {
        return Err(format!("at most {MAX_EVENTS} events per call"));
    }
    for (index, event) in added.iter().chain(removed).enumerate() {
        check_event(event).map_err(|problem| format!("event {index}: {problem}"))?;
    }
    Ok(())
}

fn check_action_name(action: &str) -> Result<(), String> {
    if action.trim().is_empty() {
        return Err("action must be a non-empty name, like jump".into());
    }
    if action.chars().count() > MAX_NAME_CHARS
        || action
            .chars()
            .any(|c| c.is_control() || FORBIDDEN_NAME_CHARS.contains(&c))
    {
        return Err(format!(
            "action {action:?} is not a valid name: at most {MAX_NAME_CHARS} characters, without / : = \\ \" or control characters"
        ));
    }
    Ok(())
}

fn check_event(event: &Value) -> Result<(), String> {
    let Some(kind) = str_arg(event, "type") else {
        return Err(
            "each event needs a type: key, mouse_button, joypad_button or joypad_motion".into(),
        );
    };
    let whole = |key: &str| event.get(key).and_then(Value::as_i64);
    let device_ok = event
        .get("device")
        .is_none_or(|device| device.as_i64().is_some_and(|number| number >= -1));
    if !device_ok {
        return Err("device must be -1 (all devices) or a device number".into());
    }
    match kind {
        "key" => match str_arg(event, "key") {
            Some(key) if !key.trim().is_empty() => Ok(()),
            _ => Err("a key event needs key, a key name such as E or Space".into()),
        },
        "mouse_button" => {
            let named = str_arg(event, "button").is_some_and(|name| MOUSE_BUTTONS.contains(&name));
            let numbered =
                whole("button").is_some_and(|number| MOUSE_BUTTON_RANGE.contains(&number));
            if named || numbered {
                Ok(())
            } else {
                Err(format!(
                    "a mouse_button event needs button: one of {} or a number from 1 to 9",
                    MOUSE_BUTTONS.join(", ")
                ))
            }
        }
        "joypad_button" => match whole("button") {
            Some(number) if JOY_BUTTON_RANGE.contains(&number) => Ok(()),
            _ => Err("a joypad_button event needs button, a number from 0 to 127".into()),
        },
        "joypad_motion" => {
            if !whole("axis").is_some_and(|number| JOY_AXIS_RANGE.contains(&number)) {
                return Err("a joypad_motion event needs axis, a number from 0 to 9".into());
            }
            match event.get("axis_value").and_then(Value::as_f64) {
                Some(value) if value != 0.0 => Ok(()),
                _ => Err(
                    "a joypad_motion event needs axis_value, a non-zero number whose sign picks the direction"
                        .into(),
                ),
            }
        }
        other => Err(format!(
            "unknown event type '{other}': use key, mouse_button, joypad_button or joypad_motion"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_name_is_data_but_not_one_the_editor_refuses() {
        for good in ["jump", "move left", "ui_accept", "прыжок"] {
            assert!(check_action_name(good).is_ok(), "{good}");
        }
        for bad in ["", "  ", "a/b", "a:b", "a=b", "a\\b", "a\"b", "a\nb"] {
            assert!(check_action_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn events_of_every_type_are_checked() {
        for good in [
            json!({"type": "key", "key": "E"}),
            json!({"type": "mouse_button", "button": "wheel_up"}),
            json!({"type": "mouse_button", "button": 9}),
            json!({"type": "joypad_button", "button": 0, "device": -1}),
            json!({"type": "joypad_motion", "axis": 9, "axis_value": -0.5}),
        ] {
            assert!(check_event(&good).is_ok(), "{good}");
        }
        for bad in [
            json!({}),
            json!({"type": "key", "key": ""}),
            json!({"type": "mouse_button", "button": 0}),
            json!({"type": "joypad_button", "button": 128}),
            json!({"type": "joypad_button", "button": 1, "device": -2}),
            json!({"type": "joypad_motion", "axis": 10, "axis_value": 1}),
        ] {
            assert!(check_event(&bad).is_err(), "{bad}");
        }
    }
}
