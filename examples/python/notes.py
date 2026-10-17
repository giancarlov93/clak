"""A minimal Clak extension: a notepad that AI agents can read and append to.

    pip install websockets
    python notes.py [title]

The whole protocol is in docs/protocol.md; this file is the smallest complete client.
"""

import asyncio
import json
import os
import sys
import uuid
from pathlib import Path

import websockets

PROTOCOL = 1
HOME = Path(os.environ.get("CLAK_HOME") or Path.home() / ".clak")

TOOLS = [
    {
        "name": "read",
        "description": "The whole text of the note.",
        "input_schema": {"type": "object", "properties": {}},
    },
    {
        "name": "append",
        "description": "Adds a line at the end of the note.",
        "input_schema": {
            "type": "object",
            "properties": {"line": {"type": "string", "description": "The line to add."}},
            "required": ["line"],
        },
    },
]

lines: list[str] = []


def run(tool: str, arguments: dict) -> str:
    """Runs one command. An exception becomes an error the agent reads."""
    if tool == "read":
        return "\n".join(lines)
    if tool == "append":
        line = arguments.get("line")
        if not isinstance(line, str):
            raise ValueError("“line” is missing")
        lines.append(line)
        return "added"
    raise ValueError(f"no such command: {tool}")


async def main(title: str) -> None:
    try:
        hub = json.loads((HOME / "daemon.json").read_text("utf-8"))
    except OSError:
        sys.exit("Clak isn't running: open it first.")
    if hub.get("protocol", 1) != PROTOCOL:
        sys.exit(f"Clak speaks protocol {hub.get('protocol')}, this extension {PROTOCOL}.")

    async with websockets.connect(f"ws://127.0.0.1:{hub['ws_port']}", max_size=16 * 1024 * 1024) as hub_socket:
        await hub_socket.send(json.dumps({
            "type": "register",
            "protocol": PROTOCOL,
            "token": hub["token"],
            "resource": {"app": "Notes", "title": title, "instance_id": uuid.uuid4().hex},
            "tools": TOOLS,
            "instructions": "Call read before append, to see what is already there.",
        }))
        async for raw in hub_socket:
            message = json.loads(raw)
            kind = message["type"]
            if kind == "call":
                try:
                    reply = {"content": run(message["tool"], message["arguments"])}
                except Exception as error:
                    reply = {"content": str(error), "is_error": True}
                await hub_socket.send(json.dumps({"type": "result", "call_id": message["call_id"], **reply}))
            elif kind == "waiting":
                print("waiting for the user to allow this program in Clak…")
            elif kind == "registered":
                print(f"connected: agents call it with `clak {message['name']} …`")
            elif kind == "granted":
                print(f"{message['harness']} can use the note")
            elif kind == "revoked":
                print(f"{message['harness']} can no longer use the note")
            elif kind in ("denied", "error"):
                sys.exit(f"{kind}: {message.get('reason') or message.get('error')}")
    print("Clak closed the connection.")


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else "notes.txt"))
