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

- Costs, a tab of its own: what the agents' tokens would cost at the API's prices, from the
  agents' own files (Claude Code and Claude Desktop's Code tab, Codex, Gemini CLI, OpenCode).
  Today, the last 7 days, the month and all time; Claude's five-hour window while it is open,
  with the cost an hour and where it is heading; each day by agent, then by model (input,
  output, cache written and read, each at its rate, the cache of an hour and long prompts
  dearer) and by folder. The prices are LiteLLM's list, read once a day; a copy comes with
  Clak for when there is no network. The files are only read, and only those that changed are
  read again.
- Protocol: `resource_changed`, for a document saved under another name while connected.
  The new title shows to the user and to agents at once, and their permissions stay; a
  different document is still a new connection, and agents ask again. In `clak-kit`,
  `LiveResource::set_title`. Hubs that don't know it ignore it: the protocol stays version 1.
- Import, in the MCP tab: the MCP servers Claude Desktop, Cursor and VS Code already have, to
  tick and add in one go, with their environment variables and their token (a Bearer header).
  Those already in Clak, and those Clak can't take as they are (a `${input:…}` VS Code fills
  in, another header, an environment file), are shown with the reason. The other apps' files
  are only read, and the panel sees the names of the variables, never their values.
- Conversations, in the Agents tab: the past Claude Code conversations, the latest first, with
  their folder, filtered by words, or grouped under their folder with a switch. Each one resumes
  in a terminal in the folder where it took place, authorized like a launch from Clak, gets a
  name of its own, as `/rename` gives it, or is deleted, after a confirmation. From its row
  it is also pinned at the top, its folder opens in Finder or File Explorer, and the command
  that resumes it by hand is copied.
- Claude Desktop's Code sessions, in the Agents tab: listed with the title and the folder the
  app shows, archived ones left out. Each one opens again in the app, authorized like a launch
  from Clak, or in a terminal with its command; the app keeps renaming and deleting them.
- Conversations of Codex, Gemini CLI and OpenCode too, each resumed in a terminal in its
  folder (`codex resume`, `gemini --resume`, `opencode --session`), with the names Codex gives
  and the titles Gemini makes. The filter also finds words said in a conversation, not only in
  its title. A conversation is saved as Markdown (what was asked, what was answered, the
  commands used), and Claude Code's show what they take on disk: Clean up… deletes in one go
  those not used for more than a week, a month, three, six months or a year, pinned ones
  excepted. A row's actions are in its More menu.
- Launch, in the Agents tab: the agents installed on this computer (Claude Code, Codex, Gemini
  CLI, OpenCode, Claude Desktop, VS Code, Cursor, Windsurf), launched from Clak and authorized
  without a question. A command-line agent opens in a terminal in the folder you pick, with a
  one-time secret valid for 12 hours; an app counts if its new session arrives within three
  minutes, signed or running inside it.
- Each agent in the Agents tab shows its version, and a command-line agent says when a newer
  one is out on npm: Update runs its own update in a terminal (`claude update`, `opencode
  upgrade`), or the command to type is copied (npm or Homebrew, as it was installed).
- Usage, in the Agents tab: how many commands each agent ran, today or in the last 7 days, and
  to which programs. Only names and counts, kept for a month in `usage.json`.
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
- Try it, on a command's page in Status: a field for each parameter (a list for the choices
  and yes/no, numbers, JSON for objects), Run, and what came back. It runs as you, for real,
  and only in the app where the hub runs. Commands… in an MCP server's or an API's menu opens
  that page's program.
- An MCP server's and an API's card say how they have been answering the agents: how many
  commands, how many failed, the average time, the last one, in the warning color when it
  failed.
- MCP servers and REST APIs can show their own icon in the MCP and APIs tabs, in Status, in
  the tray box and in the tray menu: the one an MCP server declares, otherwise the one of its
  website (SVG too). Server icons in Settings › Hub turns it on (off by default: it tells
  each site that Clak uses it); Clak fetches them, not the page, and keeps them for a week in
  `~/.clak/icons`.
- Settings › General › Configuration: Export… writes the presets, the MCP servers and the APIs
  in one file, Import… adds those of a file that aren't here yet, each with what happened. No
  token, key or sign-in goes in it (of a program's variables, only the names), and no
  permission: the allowed applications and the "Always allow" rules stay on this computer. A
  command line that looks like it carries a key is pointed out before exporting.
- An MCP server or an API that needs you (its sign-in expired, its token refused) says so
  before an agent finds out: a notification, a bar in Status with Sign in… or Edit…, and a red
  count on the MCP and APIs tabs.
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
- Always allow, in a consent window: ticked with Allow, that agent may use that program from
  then on without asking, in the folder it works in (anywhere, if it doesn't say). The agent is
  recognized as programs are in `apps.json` (same executable and signer or, unsigned, same
  content), the program as a preset names it; offered only for applications, MCP servers and
  APIs, never ticked from the start. The rules are listed in Applications, where Remove brings
  the questions back.
- Several consent windows at once say which they are: "1 of 3".
- In a permission's history, a click on a call shows what the agent sent and what came back.
  They stay only in memory, trimmed, for the latest 100 calls, and only the app hosting the
  hub sees them: never the control channel.
- Applications: an Open button, to start an allowed program.
- Mac releases are signed with a Developer ID and notarized by Apple.

### Changed

- `clak list` shows the programs the agent can use without asking: permission already given,
  covered by the preset it was launched with, or always allowed by the user, each saying
  which. It ends with how many other programs are connected; `clak list --others` lists them
  (their first command asks the user), `clak list --all` (`-a`) lists both. The same for MCP
  clients, with `programs {"which": "others" | "all"}`, and in the skill.
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

- Times read naturally: "yesterday" rather than "1 days ago", and the date past a week
  ("Oct 12", with the year when it isn't this one) rather than "240 days ago".
- From the keyboard, a page drawn again (an agent connecting, a minute gone by, a switch
  turned, a program ticked in a preset) no longer takes the focus away: the same control
  keeps it. The tray box no longer rebuilds its rows when nothing changed.
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
