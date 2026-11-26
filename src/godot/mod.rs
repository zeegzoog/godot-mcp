//! Godot remote-debugger client and the in-game runtime channel.
//!
//! Process runs speak Godot's own debugger protocol (`--remote-debug`). The server
//! does not implement a debugger; it is a client of that protocol.

mod binary_resource;
pub mod class_cache;
mod debug_ctl;
mod debug_session;
mod imports;
pub mod json_text;
pub mod movie;
mod resource_audit;
mod runs;
mod uid_cache;
mod variant;

pub use binary_resource::{BinaryUid, binary_uid};
pub use debug_ctl::{DebugCtl, StopInfo, split_breakpoint};
pub use debug_session::DebugSession;
pub use imports::{
    IMPORTED_EXTENSIONS, changed_imports, force_reimport, import_current, import_fingerprints,
    import_kind, md5_hex, stale_imports, stale_reason,
};
pub use resource_audit::{
    BinaryUidFix, ChangedText, ProjectChanges, ResourceDamage, ResourceRepair,
    TextResourceSnapshot, written_in,
};
pub use runs::{GameHub, ProcessStart, Run, RunInfo};
pub use uid_cache::{UidCacheUpdate, record as record_uids, uid_cache_file};
pub use variant::{PacketReader, decode_variant, encode_variant, frame_packet};
