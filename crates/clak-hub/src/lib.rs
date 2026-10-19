//! The core of the Clak hub: programs, agents, permissions, questions to the user, and the
//! two channels — the loopback WebSocket for programs and the control channel (named pipe
//! on Windows, Unix socket on the Mac) for agents and the panel.
//!
//! ```text
//!   program (WebSocket 127.0.0.1) ──▶ ┌─────┐ ◀── channel ── clak (command line) ◀─ agent
//!   program (WebSocket 127.0.0.1) ──▶ │ hub │ ◀── channel ── clak-shim (MCP) ◀─ client without a shell
//!   connectors, APIs, settings ─────▶ └─────┘ ◀── channel ── panel (watch)
//! ```
//!
//! The hub asks for permissions, with its own windows: not the agent, and not the program.

pub mod app_channel;
pub mod cli_names;
pub mod consent;
pub mod control_channel;
pub mod core;
pub mod flags;
pub mod hub;
pub mod launch;
pub mod permissions;
pub mod requests;
pub mod texts;
pub mod wire;

pub use consent::{
    AgentIdentity, ConnectorTiming, ConsentKind, ConsentLine, ConsentPrompt, ConsentRequest,
    ConsentTool, HubOptions, Platform, SettingsChange, Verdict,
};
pub use core::{AppConn, Broker, Core, Gone, Outbox, Progress, failure};
pub use hub::{Hub, LogSink};
pub use wire::{Malformed, ResourceDef, ToolDef};
