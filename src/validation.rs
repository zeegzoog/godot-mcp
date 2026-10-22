//! Minimal JSON-Schema checks for tool arguments: required keys, primitive types, enums and
//! the numeric `minimum` / `maximum` of a parameter.
//!
//! Catching wrong types here gives the caller an immediate, specific error instead of a
//! script error inside the editor that would only surface as a timeout.

use serde_json::{Map, Value};

pub fn validate_arguments(schema: &Value, arguments: &Value) -> Result<(), String> {
    let empty = Map::new();
    let arguments = match arguments {
        Value::Object(map) => map,
        Value::Null => &empty,
        other => {
            return Err(format!(
                "arguments must be an object, got {}",
                type_name(other)
            ));
        }
    };

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if arguments.get(name).is_none_or(Value::is_null) {
                return Err(format!("Missing required parameter: {name}"));
            }
        }
    }

    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    for (name, value) in arguments {
        if value.is_null() {
            continue;
        }
        let Some(property) = properties.get(name) else {
            continue;
        };
        check_value(name, value, property)?;
    }
    Ok(())
}

fn check_value(name: &str, value: &Value, property: &Value) -> Result<(), String> {
    if let Some(expected) = property.get("type").and_then(Value::as_str) {
        if !matches_type(value, expected) {
            return Err(format!(
                "Parameter '{name}' must be {} {expected}, got {}",
                article(expected),
                type_name(value)
            ));
        }
        if expected == "array" {
            if let (Some(items), Some(item_type)) = (
                value.as_array(),
                property
                    .get("items")
                    .and_then(|items| items.get("type"))
                    .and_then(Value::as_str),
            ) {
                if let Some(bad) = items.iter().find(|item| !matches_type(item, item_type)) {
                    return Err(format!(
                        "Every item of '{name}' must be {} {item_type}, got {}",
                        article(item_type),
                        type_name(bad)
                    ));
                }
            }
        }
    }

    if let Some(number) = value.as_f64() {
        if let Some(minimum) = property.get("minimum").and_then(Value::as_f64) {
            if number < minimum {
                return Err(format!(
                    "Parameter '{name}' must be at least {minimum}, got {number}"
                ));
            }
        }
        if let Some(maximum) = property.get("maximum").and_then(Value::as_f64) {
            if number > maximum {
                return Err(format!(
                    "Parameter '{name}' must be at most {maximum}, got {number}"
                ));
            }
        }
    }

    if let Some(allowed) = property.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            let options: Vec<String> = allowed.iter().map(Value::to_string).collect();
            return Err(format!(
                "Parameter '{name}' must be one of {}",
                options.join(", ")
            ));
        }
    }
    Ok(())
}

fn matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value.as_f64().is_some_and(|number| number.fract() == 0.0)
        }
        _ => true,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn article(type_name: &str) -> &'static str {
    if type_name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "count": {"type": "integer"},
                "flag": {"type": "boolean"},
                "map": {"type": "object"},
                "list": {"type": "array", "items": {"type": "string"}},
                "mode": {"type": "string", "enum": ["a", "b"]}
            },
            "required": ["path"]
        })
    }

    #[test]
    fn a_number_outside_its_documented_range_is_refused_with_the_range() {
        let schema = json!({
            "type": "object",
            "properties": {"line": {"type": "integer", "minimum": 1, "maximum": 10}}
        });
        assert!(validate_arguments(&schema, &json!({"line": 1})).is_ok());
        assert!(validate_arguments(&schema, &json!({"line": 10})).is_ok());
        let low = validate_arguments(&schema, &json!({"line": 0})).unwrap_err();
        assert!(
            low.contains("'line'") && low.contains("at least 1") && low.contains("got 0"),
            "{low}"
        );
        let high = validate_arguments(&schema, &json!({"line": 11})).unwrap_err();
        assert!(high.contains("at most 10"), "{high}");
        // A wrong type is still reported as a wrong type, not as a range.
        let wrong = validate_arguments(&schema, &json!({"line": "3"})).unwrap_err();
        assert!(wrong.contains("must be an integer"), "{wrong}");
    }

    #[test]
    fn accepts_valid_arguments() {
        let args =
            json!({"path": "x", "count": 3, "flag": true, "map": {}, "list": ["a"], "mode": "a"});
        assert_eq!(validate_arguments(&schema(), &args), Ok(()));
        assert_eq!(
            validate_arguments(&schema(), &json!({"path": "x", "count": 3.0})),
            Ok(())
        );
    }

    #[test]
    fn missing_or_null_required_argument_is_named() {
        let err = validate_arguments(&schema(), &json!({})).expect_err("missing");
        assert!(err.contains("path"), "{err}");
        let err = validate_arguments(&schema(), &json!({"path": null})).expect_err("null");
        assert!(err.contains("path"), "{err}");
    }

    #[test]
    fn wrong_types_name_the_parameter_and_both_types() {
        let err = validate_arguments(&schema(), &json!({"path": 5})).expect_err("type");
        assert!(
            err.contains("'path'") && err.contains("string") && err.contains("number"),
            "{err}"
        );

        let err =
            validate_arguments(&schema(), &json!({"path": "x", "count": 1.5})).expect_err("int");
        assert!(err.contains("integer"), "{err}");

        let err =
            validate_arguments(&schema(), &json!({"path": "x", "list": [1]})).expect_err("items");
        assert!(err.contains("list"), "{err}");
    }

    #[test]
    fn enum_violations_list_the_options() {
        let err =
            validate_arguments(&schema(), &json!({"path": "x", "mode": "c"})).expect_err("enum");
        assert!(err.contains("\"a\"") && err.contains("\"b\""), "{err}");
    }

    #[test]
    fn non_object_arguments_are_rejected_and_null_means_empty() {
        assert!(validate_arguments(&schema(), &json!("text")).is_err());
        let optional = json!({"type": "object", "properties": {}});
        assert_eq!(validate_arguments(&optional, &Value::Null), Ok(()));
    }

    #[test]
    fn unknown_properties_are_ignored() {
        assert_eq!(
            validate_arguments(&schema(), &json!({"path": "x", "extra": 1})),
            Ok(())
        );
    }
}
