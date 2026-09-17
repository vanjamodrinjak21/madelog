//! One file per command, per spec §4.

pub mod config;
pub mod export;
pub mod hide;
pub mod hook;
pub mod init;
pub mod log;
pub mod note;
pub mod onboard;
pub mod verify;

/// Commands that exist so later tasks have somewhere to land.
pub mod analyze;
pub mod ask;
pub mod model;
pub mod publish;
pub mod summarize;
