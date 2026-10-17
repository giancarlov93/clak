//! The agent side: the `clak` command line and the MCP shim.
//!
//! Both talk to the hub (the Clak app) on the control channel, one request per
//! connection, in JSON lines. The library holds what the two executables have in
//! common, and what the app can reuse (the replies written for a model, in
//! [`render`]).
//!
//! No async runtime: the command line starts on every agent command, and must start
//! fast.

pub mod attachments;
pub mod cli;
pub mod hub;
pub mod invocation;
pub mod launcher;
pub mod output;
pub mod render;
pub mod shell;
pub mod shim;

mod clock;
mod util;

#[cfg(test)]
mod fake_hub;

pub use hub::{HubUnavailable, ask};
pub use invocation::Invocation;
pub use output::Exit;
pub use render::Style;
pub use shell::Shell;
