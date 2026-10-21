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

- Conversations, in the Agents tab: the past Claude Code conversations, the latest first, with
  their folder, filtered by words. Each one resumes in a terminal in the folder where it took
  place, authorized like a launch from Clak, or gets a name of its own, as `/rename` gives it.
- Launch, in the Agents tab: the agents installed on this computer (Claude Code, Codex, Gemini
  CLI, OpenCode, Claude Desktop, VS Code, Cursor, Windsurf), launched from Clak and authorized
  without a question. A command-line agent opens in a terminal in the folder you pick, with a
  one-time secret valid for 12 hours; an app counts if its new session arrives within three
  minutes, signed or running inside it.
- Presets: an agent, a folder and the programs it may use without asking, all their commands
  or only some. A known application covers all its documents; the preset opens the ones that
  aren't running. Other programs and commands still ask.
- A preset can also start a command-line agent with a model and a system prompt of its own,
  added to the agent's or in its place, as far as each agent allows: Claude Code both, Codex
  and Gemini CLI in its place, OpenCode added. With a folder, it edits the instructions file
  the agent reads there (`CLAUDE.md`, `AGENTS.md`, `GEMINI.md`).
- Each agent's global instructions (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
  `~/.gemini/GEMINI.md`, OpenCode's `AGENTS.md`) open from its row in the Agents tab, to read
  and change; the text replaced is kept as `.clak-bak`.
- Show brings a connected agent to the front (in Status and in the tray box), and the eye a
  program it uses, when the program has a window: the app it runs in comes forward. On the
  Mac, Terminal and iTerm also select the agent's tab (macOS asks first whether Clak may
  control them); on Windows a console or Windows Terminal window comes forward.
- MCP servers and REST APIs can show their own icon in the MCP and APIs tabs, in Status, in
  the tray box and in the tray menu: the one an MCP server declares, otherwise the one of its
  website (SVG too). Server icons in Settings › Hub turns it on (off by default: it tells
  each site that Clak uses it); Clak fetches them, not the page, and keeps them for a week in
  `~/.clak/icons`.
- Clak's own settings show Clak's icon in Status, in the tray box and in the tray menu.
- Clak updates itself. When a newer version is out it says so (a notification, the tray box
  and Settings › About), and Install and restart downloads it, checks its signature and
  installs it. Install updates automatically (off by default) does it alone, when no agent is
  connected. Clak looks every hour, and Check now looks at once; Check for updates turns the
  hourly checks off, and so does the `CLAK_NO_UPDATE_CHECK` variable.
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

- Settings › Integration no longer confirms under a row what the row already says
  ("Installed · applies to new sessions", "Applies to new sessions"…): only what is left to
  do, such as restarting Claude Desktop, and the errors.
- The consent window is shorter and easier to read: who asks with its real icon and who
  signed it, the commands as names to tap, the rest under Details. On the Mac it is no longer
  see-through.
- A consent window plays Clak's own sound, of the same family as the connection sound, not
  the system one. Sound for consent windows in Settings still turns it off.
- Applications show who signed the program without "signed by".
- The Agents tab holds only the agents to launch and the presets. The command line, the
  skills and the MCP clients moved to Settings › Integration; Settings has three parts:
  General, Integration and About.
- Settings › About shows the version, where the updates stand and Check now together, as
  Windows Update does; the two switches stay in their own rows.
- On Windows too, Status lays the connected programs out in a grid, as on the Mac.
- The protocol's session diagram is a Mermaid diagram.

### Fixed

- On Windows, the first click on the icon opened the box and closed it at once: it took a
  second click. The box now opens when its page is ready.
- The icon's box was always as tall as with several agents connected: now it takes the height
  of what it shows, and scrolls past 440 points.
- Under the pointer, the main button (Open the panel, Allow, Save…) turned the grey of the
  others, as if it were no longer the main one. Now it stays its color, a little lighter.
- On Windows, OpenCode wasn't recognized as an agent: its launcher exits as soon as it starts
  it, and every command failed with "Cannot tell which agent launched this command". It is
  now recognized by its executable, and authorized when launched from Clak.
- A program that runs in a runtime (Java, Python, Node.js, .NET…) looked signed by whoever
  signed the runtime, as if it were their own. Applications and the consent window now name
  the runtime next to the signature.
- A newer Clak could go unnoticed for hours: Clak looked every six hours, a failed look left
  no trace, and turning off the notifications about programs turned this one off too.
  Settings now shows when Clak last looked, and why it could not.
- On Windows, Claude Code that had updated itself while running was no longer recognized
  (the update renames the running file to `claude.exe.old.…`): from Windows Terminal every
  command failed with "Cannot tell which agent launched this command". The name now comes
  from the file the program started from.
- On Windows, Clak started by an agent inside Claude Desktop ran inside its package: its
  updates went into Claude's own folder, and the PATH, the shortcuts and the notifications
  pointed to a Clak that nothing else could see. Now Clak opens outside it, and an installer
  opened from inside an app asks to be opened from File Explorer.
- On Windows, Open the panel in the tray box left a blank window, and Clak stopped
  responding until it was ended from Task Manager.
- The menu bar icon with a green or amber dot stayed black on a dark bar.
- "Sign in…" on an MCP server added from the command line closed its window at once.
- Mac windows could not be dragged by their top edge.
- Windows: updating while the app was open could leave it blank after the restart.
- Under the pointer, the programs in Status, the agents' rows with Revoke, the commands and
  the rows of the tray box light up whole, not only a part of them.

## [0.3] - 2026-10-17

### Added

- Clak in Rust and Tauri for Windows and Mac: the hub, the `clak` command line, the MCP shim
  and the app, with the tray or menu bar icon, notifications, consent windows and the panel.
- MCP servers (HTTP with a token or OAuth, or local programs over stdio) and REST APIs, from
  OpenAPI or by hand, as commands for the agents.
- The extension protocol, version 1, with examples in Python, JavaScript and C#.

[Unreleased]: https://github.com/giancarlov93/clak/compare/0f6ebde...HEAD
[0.3]: https://github.com/giancarlov93/clak/commit/0f6ebde
