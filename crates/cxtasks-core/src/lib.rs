//! The platform-neutral half of CXTasks.
//!
//! Everything here compiles for Linux as well as macOS. The crate boundary is
//! what enforces that — see this crate's `Cargo.toml` for why it is a boundary
//! rather than a feature flag.
//!
//! `cxtasks_lib` re-exports every module below under its original path, so
//! `crate::paths::db_path()` keeps resolving from the Tauri side and the move
//! stays invisible to callers.
//!
//! What is deliberately NOT here: the Tauri commands, launchd, Ghostty, and the
//! three `*_macos` modules. Where this crate needs something from that side it
//! asks through a hook — [`notify`] for banners, [`platform`] for host facts.

pub mod api;
pub mod attachments;
pub mod checkpoint;
pub mod db;
pub mod dispatch;
pub mod error;
pub mod executor;
pub mod host;
pub mod intake;
pub mod intent;
pub mod jev;

// launchd is macOS-shaped but framework-free — plist files and `launchctl`
// subprocesses, no AppKit. It lives here rather than in `cxtasks_lib` so the
// helper BINARIES, which now live here too, can answer "is a drain loop
// installed" without depending on the Tauri crate. Deliberately NOT abstracted
// behind a `ServiceManager` trait spanning launchd and systemd: those two share
// a name and no semantics, and the fake symmetry would cost more than it saves.
#[cfg(target_os = "macos")]
pub mod launchd;

pub mod mcp;
pub mod mcp_registry;
pub mod mirror;
pub mod notify;
pub mod notify_helper;
pub mod paths;
pub mod platform;
pub mod prose;
pub mod recurrence;
pub mod routing;
pub mod sync;
pub mod tripwire;
pub mod runner;
