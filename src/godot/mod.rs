//! Client for the protocol a Godot game speaks to its remote debugger.
//!
//! The server does not implement a debugger. Process runs connect with
//! `--remote-debug tcp://127.0.0.1:<port>`.

mod debug_ctl;
mod debug_session;
mod variant;

pub use debug_ctl::{DebugCtl, StopInfo, split_breakpoint};
pub use debug_session::DebugSession;
pub use variant::{PacketReader, decode_variant, encode_variant, frame_packet};
