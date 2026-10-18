# Clak

Clak for Windows and Mac: Rust for the hub and the command line, Tauri 2
for the app. Each system uses its own things: the Windows tray and the Mac menu bar,
the system notifications, Mica on Windows 11 and glass (vibrancy) on the Mac, the named pipe and
DPAPI on Windows, the Unix socket and the keychain on the Mac.

The protocol lives in a few places: `~/.clak/daemon.json`, the control channel, the
programs' WebSocket, the files `apps.json`, `connectors.json`, `apis.json` and their
`.secrets`. A program, an agent or a panel talks to the hub only through those.

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
  clak-setup       skill, PATH, start with the system, clak://, MCP clients
  clak-kit         the program side, for writing programs in Rust
  clak-daemon      the hub on its own, without an interface
  clak-app         the Tauri app: tray, notifications, consent, panel
ui/                the app's pages (plain HTML, CSS and JS, no bundler)
docs/              the program protocol and the extension guide
examples/          the same minimal extension in Python, JavaScript and C#
scripts/           the installers
```

## Extensions

How a program connects to Clak, for those writing an extension:

- [Writing an extension](docs/extensions.md): the guide, with examples in four languages.
- [The Clak extension protocol](docs/protocol.md): the specification of every message.

The protocol version is `clak_wire::PROTOCOL`: it changes only when an extension
written for the previous one would stop working, and the specification must be updated with it.

## Building

The Rust version is fixed in `rust-toolchain.toml`: in this folder rustup uses it.

```bash
rustup toolchain install
```

```bash
cargo build
```

```bash
cargo test
```

The app (it is not among the default members, because it pulls in Tauri):

```bash
cargo build -p clak-app
```

The Mac check can also be run from Windows: `cargo check --target aarch64-apple-darwin`.
`clak-app` needs a C compiler for the Mac target; for `check` alone a fake one
that writes empty files is enough (`CC_aarch64_apple_darwin`, `AR_aarch64_apple_darwin`).

Before a pull request, the same checks as CI (`.github/workflows/checks.yml`): formatting and
clippy without warnings.

```bash
cargo fmt --all
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

A git hook runs them before every commit that touches Rust; turn it on once per clone:

```bash
git config core.hooksPath scripts/hooks
```

`.gitignore` already leaves out the files of macOS, Windows and the common editors (`.DS_Store`,
`.vscode/`…). Anything else of your own setup goes in your global ignore file
(`git config --global core.excludesFile`).

## Trying it

With a separate home, so as not to touch the real one:

```bash
CLAK_HOME="$LOCALAPPDATA/Temp/clak-try" "$CARGO_TARGET_DIR/debug/ClakApp.exe" --show
```

A test program, a notepad that agents read and extend:

```bash
CLAK_HOME="$LOCALAPPDATA/Temp/clak-try" cargo run -p clak-kit --example notes
```

```bash
CLAK_HOME="$LOCALAPPDATA/Temp/clak-try" "$CARGO_TARGET_DIR/debug/clak.exe" notes append --line "milk"
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

If a hub is already running (another copy, the daemon) the app watches it from the
control channel, read-only for connectors and APIs, and takes its place as soon as
it goes away.

## Installer

```bash
node scripts/bundle.mjs
```

On Windows a per-user NSIS installer (without administrator privileges) that after
copying calls `ClakApp --setup` and before removal `ClakApp --uninstall`; on the Mac
`.app` and `.dmg`. `clak` and `clak-shim` go next to the app.

## Release

On every push to `main` GitHub Actions (`.github/workflows/release.yml`) runs the tests, builds the
installers for Windows x64, Mac ARM and Mac Intel, and a release with notes generated from the
commits. The version is `X.Y` from `[workspace.package]` in `Cargo.toml` plus the run
number (`0.3.17`): to move to `0.4` change it there, by hand or with

```bash
node scripts/version.mjs 0.4.0
```

On the Mac the app is signed ad hoc, not by an Apple developer: the first time it must be opened with
right-click → Open.

## License

MIT: see [LICENSE](LICENSE). The dependency notices, which their licenses require to be
distributed with the binaries, are written by `scripts/notices.mjs` for every installer: they end up
next to the app as `THIRD-PARTY-NOTICES.txt`, together with `LICENSE.txt`.
