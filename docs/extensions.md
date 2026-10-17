# Writing an extension

A Clak extension lets AI agents use your program: read the open document, change it,
render it, export it, always with the user's consent. This guide shows how to write one.
The details of every message are in the [protocol specification](protocol.md).

## How it works

1. The user opens a document in your program and chooses *Connect to Clak* (a button,
   a menu item, a setting).
2. Your program reads `~/.clak/daemon.json`, opens a WebSocket to the port written there
   and sends `register`: the program's name, the document's title and its commands.
3. The first time, Clak asks the user whether your program may connect. Clak remembers
   the answer.
4. Agents see the document in `clak list`. The first time an agent uses it, Clak asks the
   user again, for that agent.
5. Agents call your commands (`clak notes append --line milk`). Your program receives
   `call`, runs the command and answers with `result`.
6. When the document closes, your program sends `close`. The document disappears for
   agents, and their permissions end.

Your program never handles permissions: Clak asks, and tells you the outcome
(`granted`, `revoked`).

## Start from an example

The same small extension, a notepad with two commands (`read` and `append`), is written
in four languages. Each one is a single file with no dependencies beyond a WebSocket
client:

| Language | File | Run it |
| --- | --- | --- |
| Python | [examples/python/notes.py](../examples/python/notes.py) | `pip install websockets`, then `python notes.py` |
| JavaScript (Node 22+) | [examples/node/notes.mjs](../examples/node/notes.mjs) | `node notes.mjs` |
| C# (.NET 10+) | [examples/csharp/notes.cs](../examples/csharp/notes.cs) | `dotnet run notes.cs` |
| Rust | [crates/clak-kit/examples/notes.rs](../crates/clak-kit/examples/notes.rs) | `cargo run -p clak-kit --example notes` |

Rust has a library, `clak-kit`, which handles the connection, the messages and running
commands concurrently. In the other languages the example *is* the client: about a
hundred lines you copy into your program and adapt.

Try one with Clak running:

```bash
python notes.py
```

Clak asks whether `python` may connect; allow it. Then, from a terminal:

```bash
clak list
```

```bash
clak notes append --line "milk"
```

```bash
clak notes read
```

The first `clak notes …` asks whether the terminal may use the note. This is what an
agent does: Claude Code or Codex run the same commands.

## The essential code

Every client does the same five things. In Python, from the example:

```python
# 1. Find the hub.
hub = json.loads((HOME / "daemon.json").read_text("utf-8"))

# 2. Connect, on the loopback interface only.
async with websockets.connect(f"ws://127.0.0.1:{hub['ws_port']}") as socket:

    # 3. Introduce the open document and its commands.
    await socket.send(json.dumps({
        "type": "register",
        "protocol": 1,
        "token": hub["token"],
        "resource": {"app": "Notes", "title": "shopping.txt", "instance_id": uuid.uuid4().hex},
        "tools": TOOLS,
    }))

    # 4. Answer every call with a result.
    async for raw in socket:
        message = json.loads(raw)
        if message["type"] == "call":
            try:
                reply = {"content": run(message["tool"], message["arguments"])}
            except Exception as error:
                reply = {"content": str(error), "is_error": True}
            await socket.send(json.dumps({"type": "result", "call_id": message["call_id"], **reply}))

# 5. When the document closes: {"type": "close"}, then close the socket.
```

## Designing the integration

### One connection per open document

A connection is one opening of one document, not your application. Open two documents
and make two connections, each with its own `instance_id` (a fresh random id per opening)
and its own `title`. Agents then see both (`clak notes`, `clak notes-2`) and the user
grants them separately.

Keep `resource.app` the same across versions and documents. It is the name users know,
it is part of what Clak remembers when the user allows your program, and it gives agents
the command name.

### Connect only when the user asks

Connecting is the user's decision: offer a *Connect to Clak* command and let it stay on
for the document. Show the state, because the user needs to know what is happening:

| Event | Show |
| --- | --- |
| `daemon.json` missing, or connection refused | Clak isn't running |
| `waiting` | Waiting for permission in Clak… |
| `registered` | Connected, as `clak <name>` |
| `granted`, `revoked` | Which agents are using the document (`harness`) |
| `denied` | Not allowed. Don't retry on your own. |
| connection closed | Disconnected |

When Clak quits, the connection closes. Reconnect when the user asks, or when Clak is back
if the user turned the connection on. A new connection is a new registration, and agents
need permission again.

### Commands

Commands are what agents understand of your program; design them like an API for a
capable but literal colleague.

- **Few, meaningful commands.** `get_scene_summary`, `render`, `export_obj` work better
  than a hundred thin wrappers. A general `execute_python` command is powerful in a
  scripting host, but add the specific ones too: they are safer and easier to get right.
- **Precise descriptions.** Say what the command does, what it returns and any important
  limit, in one or two sentences. Put what applies to every command (units, coordinate
  systems, the usual workflow) in `instructions`.
- **Complete schemas.** A `type` and a `description` for every property, `required`,
  `enum` for fixed choices, `default` where there is one.
- **Stable text.** The user's approval covers each command's exact name, description and
  schema. Changing them asks every agent again, so keep counts, file names and other
  changing data out of descriptions.
- **Active commands.** Declare everything in `tools` and use `active` to say what makes
  sense now (no selection, no `delete_selection`). Update it with `tools_changed`.

### Results

- Return text when you can: it is what an agent reads best.
- Return images as `image` blocks (`render`, `screenshot`): the agent gets a file it can
  look at.
- Return other files as `resource` blocks with a `uri` whose last part is a good file name
  (`file:///exports/scene.obj`).
- For large outputs, write a file and return its path.
- On failure set `is_error` and say what went wrong and what to do about it:
  "no object named 'Cube'; get_scene_summary lists the objects" lets the agent fix it in
  one step.

### Threads

Most desktop applications only allow changes from their main thread. Read the socket on a
background thread, run each call on the main thread, and send the result back. Keep
reading messages while a command runs, so that pings, other calls and `cancel` still
arrive. If a call is cancelled before it starts, skip it; if your program can stop it
halfway, do so.

### Security

- Read the token from `daemon.json` each time you connect and never log it or show it.
- Connect only to `127.0.0.1`.
- Validate arguments as you would any external input: the agent is acting for the user,
  but it can make mistakes.
- Sign your executables if you can. Clak shows the user the signer ("signed by Example
  Ltd") instead of a bare path, and a signed program that updates is not asked about
  again.

## Testing

With Clak running, connect your extension and call it with `clak`, as above. Useful
commands:

```bash
clak <name> -h
```

shows exactly what agents see: your instructions and every command's usage. And with
`--json`, `clak` prints the raw result:

```bash
clak notes read --json
```

To test against a separate hub, without touching the user's own settings, set `CLAK_HOME`
to an empty folder for both the hub and your extension. With Clak built from source,
`clak-daemon --ask` runs a hub that asks its questions in the terminal:

```bash
CLAK_HOME=/tmp/clak-test clak-daemon --ask
```

```bash
CLAK_HOME=/tmp/clak-test python notes.py
```

## Checklist

- [ ] `register` sends `protocol`, a stable `app`, the document's `title` and a fresh
      `instance_id`.
- [ ] Every command has a clear description and a complete schema, with no changing data.
- [ ] Workflow and conventions are in `instructions`, not repeated in each description.
- [ ] Every `call` gets exactly one `result`; errors have `is_error` and an actionable
      message.
- [ ] Commands run where the host requires (main thread), without blocking the socket.
- [ ] `cancel` is honoured.
- [ ] The user sees the state: not running, waiting, connected, who is using the document.
- [ ] Closing the document sends `close`.
- [ ] The token is never logged.
- [ ] Unknown message types and fields are ignored.
