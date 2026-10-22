# Clak

Clak lets the AI agents on your computer (Claude Code, Codex, Gemini CLI, OpenCode…) use your
programs and services: the document open in a desktop program, an MCP server, a REST API.
Nothing happens without your say: you decide which programs connect, and which agent may use
each of them.

## How

- **Programs offer commands.** A desktop program connects to the Clak *hub* on your computer
  and offers commands for the document it has open: one connection per open document. MCP
  servers and REST APIs are connected the same way and become programs too.
- **Agents use one command line.** `clak list` shows what an agent can use, `clak <program> -h`
  its commands. Clak installs a skill that teaches agents this, and `clak-shim` serves the same
  over MCP for agents that prefer it.
- **You give the permissions.** A program connects only once you allow it. Each agent asks you
  the first time it uses each program, and its permission ends with the agent, the document,
  or when you revoke it. Who is asking comes from the system (the process, its executable and
  its signature), not from what a program says about itself.
- **Each system's own means.** The Windows tray and the Mac menu bar, the system
  notifications, Mica on Windows 11 and glass on the Mac; the hub listens on loopback only,
  behind a token; secrets are kept with DPAPI on Windows and in the keychain on the Mac.

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
tests on Windows and the Mac, and a coverage floor (`COVERAGE_FLOOR`):

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

## Installer and release

```bash
node scripts/bundle.mjs
```

On Windows a per-user NSIS installer, without administrator rights, that calls
`ClakApp --setup` after copying and `ClakApp --uninstall` before removal; on the Mac `.app` and
`.dmg`. `clak` and `clak-shim` go next to the app.

Every commit on `main` that passes the checks is released (`.github/workflows/release.yml`):
installers for Windows x64, Mac ARM and Mac Intel, with notes that start from the
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
every installer and go next to the app as `THIRD-PARTY-NOTICES.txt`, with `LICENSE.txt`.
