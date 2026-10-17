//! Clak's wire, shared by hub, command line, shim and app: the protocol JSON, where the
//! hub lives and how to connect to its control channel.

pub mod ipc;
pub mod json;
pub mod location;
pub mod names;

pub use serde_json::{Map, Value, json};

/// The program protocol version: the hub writes it to `daemon.json` and repeats it in
/// `registered`, the program declares it in `register` (missing means 1). It changes only
/// when a program written for the previous version would stop working.
pub const PROTOCOL: u64 = 1;
