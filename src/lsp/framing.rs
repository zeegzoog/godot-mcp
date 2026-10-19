use serde_json::Value;

pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8 * 1024;

#[derive(Debug)]
pub struct FramingError(pub String);

impl std::fmt::Display for FramingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Frame one JSON-RPC message with only the Content-Length header.
pub fn encode_message(message: &Value) -> Result<Vec<u8>, FramingError> {
    let body = serde_json::to_vec(message)
        .map_err(|err| FramingError(format!("failed to encode LSP message: {err}")))?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(FramingError(format!(
            "LSP message of {} bytes exceeds the 4 MiB limit",
            body.len()
        )));
    }
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut framed = header.into_bytes();
    framed.extend_from_slice(&body);
    Ok(framed)
}

#[derive(Debug, Default)]
pub struct MessageReader {
    buf: Vec<u8>,
}

impl MessageReader {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, FramingError> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let header_end = self.buf.windows(4).position(|window| window == b"\r\n\r\n");
            let Some(header_end) = header_end else {
                if self.buf.len() > MAX_HEADER_BYTES {
                    return Err(FramingError(
                        "LSP header block is too long (no blank line)".into(),
                    ));
                }
                return Ok(out);
            };
            let header = String::from_utf8_lossy(&self.buf[..header_end]);
            let length = content_length(&header)?;
            if length > MAX_MESSAGE_BYTES {
                return Err(FramingError(format!(
                    "LSP message of {length} bytes exceeds the 4 MiB limit"
                )));
            }
            let start = header_end + 4;
            if self.buf.len() < start + length {
                return Ok(out);
            }
            let body = self.buf[start..start + length].to_vec();
            self.buf.drain(..start + length);
            let value = serde_json::from_slice(&body)
                .map_err(|err| FramingError(format!("LSP body is not JSON: {err}")))?;
            out.push(value);
        }
    }
}

fn content_length(header: &str) -> Result<usize, FramingError> {
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse()
                .map_err(|_| FramingError(format!("invalid Content-Length in {header:?}")));
        }
    }
    Err(FramingError(format!(
        "LSP message without Content-Length: {header:?}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_use_only_content_length() {
        let framed = encode_message(&json!({"jsonrpc":"2.0","id":1})).unwrap();
        let text = String::from_utf8(framed.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "));
        assert!(!text.to_ascii_lowercase().contains("content-type"));
        let mut reader = MessageReader::default();
        assert!(reader.push(&framed[..10]).unwrap().is_empty());
        let messages = reader.push(&framed[10..]).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["id"], 1);
    }
}
