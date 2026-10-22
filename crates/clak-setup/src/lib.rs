//! Skill, PATH, start with the system, clak:// scheme, MCP clients, the agents to launch
//! and their presets.
//!
//! What the app's Agents and Settings tabs and the installer do on the user's computer,
//! with a single rule: we write only what is ours, we remove only what is ours, and what
//! belongs to others stays as it is.
//!
//! Every function receives the paths and keys to work on (the home, the folders, the
//! registry root): the tests run them on a temporary folder and a test key, without
//! touching the real PATH, startup entries or skills.

pub mod autostart;
pub mod clients;
pub mod conversations;
pub mod installation;
pub mod instructions;
pub mod launcher;
mod places;
pub mod preferences;
pub mod presets;
#[cfg(windows)]
pub mod registry;
pub mod shim;
pub mod skill;
pub mod uri_scheme;
pub mod user_path;
pub mod versions;

pub use places::{Places, Result, SetupError};
