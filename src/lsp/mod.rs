//! Client for Godot's GDScript language server.
//!
//! Godot's server speaks LSP over TCP but rejects a `Content-Type` header: it parses
//! the length as every digit after `Content-Length: `. Frames are therefore only
//! `Content-Length: N\r\n\r\n<body>`.

mod client;
mod framing;

pub use client::LspHub;
pub use framing::{MessageReader, encode_message};
