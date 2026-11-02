//! Responsibility: Keep the state of every device of one owner the same through a sync server:
//! settings, to-dos, the current task, scheduled updates, protection, blocks and grants; and
//! gather each device's AI usage so the statistics add up.
//!
//! - [`envelope`]: every request and answer is encrypted and signed with the device's key, so
//!   only the paired service of a device (which runs as SYSTEM and keeps its key in the locked
//!   data folder) can read or change what the server holds.
//! - [`merge`]: when two devices change the same file between syncs, a three-way merge keeps
//!   both changes where it can; where both changed the same value, the server's wins.
//! - [`server`]: the server's document store and request handling.
//! - [`client`]: one device's side: which files sync, and one sync round.
//!
//! Walkthrough (Korean): `docs/sync.md`.

pub mod client;
pub mod envelope;
pub mod merge;
pub mod protocol;
pub mod server;
