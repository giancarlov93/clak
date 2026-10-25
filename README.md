# Clak

Clak lets the AI agents on your computer (Claude Code, Codex, Gemini CLI, OpenCode…) use your
programs and services: the document open in a desktop program, an MCP server, a REST API.
Nothing happens without your say: you decide which programs connect, and which agent may use
each of them.

## How

- **Programs offer commands.** An open program connects to the Clak *hub* on your computer and
  offers the commands that fit its context and its state: what is open, what is selected,
  what can be done right now. MCP servers and REST APIs are connected too and become programs.
- **Agents work from the command line.** Not through tools that fill their context and can't be
  chained by a program: `clak list` shows what an agent can use, `clak <program> -h` its
  commands, and their calls compose in scripts and pipes. What would take an agent several
  MCP turns takes one command line. Clak installs a skill that teaches agents this.
- **You give the permissions.** A program connects only once you allow it. Each agent asks you
  the first time it uses each program, and its permission ends with the agent, with the
  program's connection, or when you revoke it. Who is asking comes from the system (the
  process, its executable and its signature), not from what a program says about itself.
- **Each system's own means.** The Windows tray and the Mac menu bar, the system
  notifications, Mica on Windows 11 and glass on the Mac; the hub listens on loopback only,
  behind a token; secrets are kept with DPAPI on Windows and in the keychain on the Mac. On
  Linux, for servers and containers, there is the hub without an interface (see below).

The app around the hub shows what is connected and who is using it, asks the consent
questions, launches agents with programs already allowed (presets), and shows what the agents'
tokens would cost at the API's prices.

## The pieces

```text
crates/
  clak-wire        strict JSON, locations (home, lockfile, pipe/socket), IPC channel
  clak-platform    the real system: who the agent is, signatures, loopback ports, secrets
  clak-hub         the core: programs, agents, permissions, questions, the two channels
  clak-connectors  MCP servers: over HTTP, with a token or OAuth, or local programs over stdio
  clak-apis        REST APIs, from OpenAPI or by hand
  clak-settings    the settings as a program: `clak settings …`
  clak-host        everything together, with the real system
  clak-agent       `clak` (command line) and `clak-shim` (MCP over stdio)
  clak-setup       the skill, PATH, start with the system, clak://, MCP clients, the agents'
                   conversations, presets, token costs
  clak-kit         the program side, for writing programs in Rust
  clak-daemon      the hub on its own, without an interface
  clak-app         the Tauri app: tray, notifications, consent, panel
ui/                the app's pages (plain HTML, CSS and JS, no bundler)
docs/              the program protocol and the extension guide
examples/          the same minimal extension in Python, JavaScript and C#
scripts/           packaging, release notes and the version; the git hook
```

The hub, the app and the command line share only `daemon.json` (in `~/.clak`, or
`CLAK_HOME`), the control channel, the programs' WebSocket and the hub's files (`apps.json`,
`rules.json`, `presets.json`, `connectors.json`, `apis.json` and their `.secrets`).

## Extensions

- [Writing an extension](docs/extensions.md): the guide, with examples in four languages.
- [The Clak extension protocol](docs/protocol.md): every message.

The protocol version is `clak_wire::PROTOCOL`. It changes only when an extension written for
the previous one would stop working, and the specification changes with it.

## Building

The Rust version is fixed in `rust-toolchain.toml`.

```bash
cargo build
```

```bash
cargo test
```

The app isn't among the default members, because it pulls in Tauri:

```bash
cargo build -p clak-app
```

The checks of CI (`.github/workflows/checks.yml`) are formatting, clippy without warnings, the
tests on Windows, the Mac and Linux (there without the app), and a coverage floor
(`COVERAGE_FLOOR`):

```bash
cargo fmt --all
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo llvm-cov --workspace --summary-only
```

A git hook runs formatting and clippy before every commit that touches Rust:

```bash
git config core.hooksPath scripts/hooks
```

## Trying it

With a home of its own, so as not to touch the real one:

```bash
CLAK_HOME=/tmp/clak-try cargo run -p clak-app -- --show
```

A test program, a notepad that agents read and extend:

```bash
CLAK_HOME=/tmp/clak-try cargo run -p clak-kit --example notes
```

```bash
CLAK_HOME=/tmp/clak-try cargo run -p clak-agent --bin clak -- notes append --line "milk"
```

## The app

```text
ClakApp                       in the tray, with the hub (or watching the one already running)
ClakApp --show                and the panel open
ClakApp --no-broker           only watch, without starting a hub
ClakApp --quit [--update]     closes the running copy (and reopens it after the update)
ClakApp --setup | --uninstall from the installer
ClakApp clak://…              a link, to the running copy
```

If a hub is already running (another copy, the daemon), the app watches it from the control
channel, read-only for connectors and APIs, and takes its place as soon as it goes away.

## Linux, without an interface

On a server or in a container there is no app. Each release has an archive for Linux x64 and
ARM, linked statically so that it runs on any distribution, with `clak`, `clak-shim` and
`clak-daemon`, the hub on its own. Without an option every new permission is denied; the
questions can be answered on a web page, or in the daemon's terminal.

```bash
clak-daemon --web
```

The page listens on `127.0.0.1:7780` only (`--port` for another one) and asks for a password,
chosen the first time or with `clak-daemon --set-password`: only its hash is kept, in
`web.password` in the hub's folder. From another computer it is reached through a tunnel
(`ssh -L 7780:127.0.0.1:7780 server`) or Tailscale (`tailscale serve 7780`, then
`--url https://<server>.<tailnet>.ts.net` so the links point there). An agent that needs a
permission isn't kept waiting: it gets the link of its question at once, gives it to the user,
and tries again after the answer. The page also lists what is allowed now (permissions, agents,
"Always allow", applications) and takes it away.

The agents run as the same user and can reach the page too: what keeps them from answering
their own questions is the password, which they don't know. So it must be long, a phrase rather
than a PIN, since whoever reads the hash can try passwords one by one; and the daemon reads it
only when it starts. Against an agent set on getting around Clak (changing its files,
restarting the hub) only another user, or a container, protects: as on the other systems,
consent is there so that agents ask, not as a sandbox.

```bash
clak-daemon --ask
```

With `--ask` the questions are in the daemon's terminal instead, answered y or n: only a line
typed while a question is on screen answers it.

The agents reach it with `clak` in the PATH, or with `clak-shim` as an MCP server over stdio
(`claude mcp add clak -- clak-shim`): the skill and the MCP clients are set up by the app,
which Linux doesn't have yet.

Who is asking is read from `/proc`: the agent among the ancestors of the process, up to the
session (sshd, tmux, screen, the user's systemd). Linux has no signatures for executables:
an agent or a program is remembered by its content, and asked about again when it changes.
The connectors' tokens and the APIs' secrets are in `connectors.secrets` and `apis.secrets`
in the hub's folder, which only the user can read (0600), not encrypted: a server has no vault
to count on.

## Installer and release

```bash
node scripts/bundle.mjs
```

On Windows a per-user NSIS installer, without administrator rights, that calls
`ClakApp --setup` after copying and `ClakApp --uninstall` before removal; on the Mac `.app` and
`.dmg`. `clak` and `clak-shim` go next to the app.

Every commit on `main` that passes the checks is released (`.github/workflows/release.yml`):
installers for Windows x64, Mac ARM and Mac Intel, the Linux archives for x64 and ARM, with
notes that start from the
**Unreleased** section of `CHANGELOG.md`. The version is `X.Y` from `[workspace.package]` in
`Cargo.toml` plus the run number; `node scripts/version.mjs 0.4.0` moves to a new `X.Y`.

The Mac app is signed with a Developer ID and notarized. The installed apps update themselves
through Tauri's updater, which installs only files signed with Clak's key: its public half is
in `crates/clak-app/tauri.conf.json`, the private one in the `release` environment's secrets
(`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) or, for a local build, in
`.secrets/updater.key` and `.secrets/updater.password`. Without it the installers have no
updater files.

## License

MIT: see [LICENSE](LICENSE). The dependencies' notices are written by `scripts/notices.mjs` for
every installer and go next to the app as `THIRD-PARTY-NOTICES.txt`, with `LICENSE.txt`; in
the Linux archives, next to the binaries.
