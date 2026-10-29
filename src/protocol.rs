use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// Behaviour hints a host may use to decide whether to ask before a call.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub idempotent_hint: bool,
    pub open_world_hint: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

impl ToolResult {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::Text {
                text: content.into(),
            }],
            is_error: false,
        }
    }

    pub fn image(base64_data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::Image {
                data: base64_data.into(),
                mime_type: mime_type.into(),
            }],
            is_error: false,
        }
    }

    /// An image preceded by a short text note (e.g. its dimensions).
    pub fn image_with_text(
        note: impl Into<String>,
        base64_data: impl Into<String>,
        mime_type: impl Into<String>,
    ) -> Self {
        Self {
            content: vec![
                ToolContent::Text { text: note.into() },
                ToolContent::Image {
                    data: base64_data.into(),
                    mime_type: mime_type.into(),
                },
            ],
            is_error: false,
        }
    }

    pub fn error(message: impl Into<String>, recovery_hints: &[&str]) -> Self {
        let mut text = format!("Error: {}", message.into());
        if !recovery_hints.is_empty() {
            text.push_str("\n\nActionable recovery suggestions:\n");
            for hint in recovery_hints {
                text.push_str(&format!("- {hint}\n"));
            }
        }
        Self {
            content: vec![ToolContent::Text { text }],
            is_error: true,
        }
    }
}
