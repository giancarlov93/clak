# Clak on Linux, without an interface

For servers and containers: the hub on its own, with the questions answered on a web page or
in a terminal. The rest of Clak is in the [README](../README.md).

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
applications) and takes it away.

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

