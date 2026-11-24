# Developing Clak

How Clak is built, tried, packaged and released. What it does and how to install it are in
the [README](../README.md).

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
  clak-backup      backups locked with the user's password, and their restore after a check
  clak-kit         the program side, for writing programs in Rust
  clak-daemon      the hub on its own, without an interface
  clak-app         the Tauri app: tray, notifications, consent, panel
ui/                the app's pages (plain HTML, CSS and JS, no bundler)
docs/              the extension guide, the protocol, Linux, this page; the README's screenshots
examples/          the same minimal extension in Python, JavaScript and C#
scripts/           packaging, release notes and the version; the git hook
```

The hub, the app and the command line share only `daemon.json` (in `~/.clak`, or
`CLAK_HOME`), the control channel, the programs' WebSocket and the hub's files (`apps.json`,
`presets.json`, `connectors.json`, `apis.json` and their `.secrets`). Only the
app writes `backup.json`, the settings of the automatic backup.

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

The checks of CI (`.github/workflows/checks.yml`) run on every pull request that changes more
than the documentation: formatting, clippy without warnings, the tests on Windows, the Mac and
Linux (there without the app), a coverage floor (`COVERAGE_FLOOR`), and what the release does
besides compiling. The first ones, by hand:

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

Every merge into `main` is released at once, without approval (`.github/workflows/release.yml`):
it doesn't check again what passed on the pull request, and runs the checks only for files that
weren't checked there. A merge of documentation only (Markdown, `docs/`, `examples/`) releases
nothing. Installers for Windows x64 and the Mac with Apple silicon, the Linux archives for x64
and ARM, with notes that start from the
**Unreleased** section of `CHANGELOG.md`. The version is `X.Y` from `[workspace.package]` in
`Cargo.toml` plus the run number; `node scripts/version.mjs 0.4.0` moves to a new `X.Y`.

The Mac app is signed with a Developer ID and notarized. The installed apps update themselves
through Tauri's updater, which installs only files signed with Clak's key: its public half is
in `crates/clak-app/tauri.conf.json`, the private one in the `release` environment's secrets
(`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) or, for a local build, in
`.secrets/updater.key` and `.secrets/updater.password`. Without it the installers have no
updater files.

## The README's screenshots

The pictures in `docs/images` are the app's real pages (`ui/`) with example data, never a
real hub's: `docs/screenshots/demo.js` stands in for the app's backend, and Chrome without a
window photographs them in the light and the dark appearance.

```bash
node docs/screenshots/shoot.mjs
```

The terminal beside the panel in the first picture shows what `clak` prints for the same
data (`crates/clak-agent/src/render.rs`): it is written in `docs/screenshots/frame.html`, to
update by hand when that output changes.

Run it again when the pages change. It needs Google Chrome (`CHROME` for another path) and
Node 22 or later; on the Mac the example programs' real icons are used when they are
installed.
