# Changelog

What changes in Clak from one version to the next, for the people who use it and for those
who write extensions. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Every push to `main` publishes a release, `X.Y.<run number>`: `X.Y` comes from the root
`Cargo.toml` (see `.github/workflows/release.yml`). So the entries are grouped by `X.Y`:

- a pull request that changes something worth telling adds a line under **Unreleased**;
- every release puts the **Unreleased** section at the top of its GitHub notes;
- moving to a new `X.Y` in `Cargo.toml`, **Unreleased** becomes that version, with its date.

## [Unreleased]

### Added

- A brief clack animation when an agent connects, with an optional connection sound in Settings
  (off by default). The animation respects reduced motion and groups rapid connections.
- On the Mac, the app follows the accent color chosen in System Settings, and changes with it.
- The menu bar icon opens a box like the Windows one: each agent with the programs it may
  use under it, a Revoke button on each, and links into the panel. The right click opens it
  too.
- In a permission's history, a click on a call shows what the agent sent and what came back.
  They stay only in memory, trimmed, for the latest 100 calls, and only the app hosting the
  hub sees them: never the control channel.
- Applications: an Open button, to start an allowed program.
- Mac releases are signed with a Developer ID and notarized by Apple.

### Changed

- The consent window is shorter and easier to read: who asks with its real icon and who
  signed it, the commands as names to tap, the rest under Details. On the Mac it is no longer
  see-through.
- Applications show who signed the program without "signed by".
- The protocol's session diagram is a Mermaid diagram.

### Fixed

- The menu bar icon with a green or amber dot stayed black on a dark bar.
- "Sign in…" on an MCP server added from the command line closed its window at once.
- Mac windows could not be dragged by their top edge.
- Windows: updating while the app was open could leave it blank after the restart.

## [0.3] - 2026-10-17

### Added

- Clak in Rust and Tauri for Windows and Mac: the hub, the `clak` command line, the MCP shim
  and the app, with the tray or menu bar icon, notifications, consent windows and the panel.
- MCP servers (HTTP with a token or OAuth, or local programs over stdio) and REST APIs, from
  OpenAPI or by hand, as commands for the agents.
- The extension protocol, version 1, with examples in Python, JavaScript and C#.

[Unreleased]: https://github.com/giancarlov93/clak/compare/0f6ebde...HEAD
[0.3]: https://github.com/giancarlov93/clak/commit/0f6ebde
