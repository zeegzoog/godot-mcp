//! Godot remote-debugger client and the in-game runtime channel.
//!
//! Process runs speak Godot's own debugger protocol (`--remote-debug`). The server
//! does not implement a debugger; it is a client of that protocol.

mod debug_ctl;
mod debug_session;
mod runs;
mod variant;

pub use debug_ctl::{DebugCtl, StopInfo, split_breakpoint};
pub use debug_session::DebugSession;
pub use runs::{GameHub, RunInfo, json_id_eq};
pub use variant::{PacketReader, decode_variant, encode_variant, frame_packet};
