// A minimal Clak extension: a notepad that AI agents can read and append to.
//
//     node notes.mjs [title]          (Node 22 or later: WebSocket is built in)
//
// The whole protocol is in docs/protocol.md; this file is the smallest complete client.
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";

const PROTOCOL = 1;
const home = process.env.CLAK_HOME || join(homedir(), ".clak");

const tools = [
  {
    name: "read",
    description: "The whole text of the note.",
    input_schema: { type: "object", properties: {} },
  },
  {
    name: "append",
    description: "Adds a line at the end of the note.",
    input_schema: {
      type: "object",
      properties: { line: { type: "string", description: "The line to add." } },
      required: ["line"],
    },
  },
];

const lines = [];

// Runs one command. A thrown error becomes an error the agent reads.
async function run(tool, args) {
  if (tool === "read") return lines.join("\n");
  if (tool === "append") {
    lines.push(args.line);
    return "added";
  }
  throw new Error(`no such command: ${tool}`);
}

let hub;
try {
  hub = JSON.parse(readFileSync(join(home, "daemon.json"), "utf8"));
} catch {
  console.error("Clak isn't running: open it first.");
  process.exit(1);
}
if ((hub.protocol ?? 1) !== PROTOCOL) {
  console.error(`Clak speaks protocol ${hub.protocol}, this extension ${PROTOCOL}.`);
  process.exit(1);
}

const socket = new WebSocket(`ws://127.0.0.1:${hub.ws_port}`);
const send = (message) => socket.send(JSON.stringify(message));

socket.onopen = () =>
  send({
    type: "register",
    protocol: PROTOCOL,
    token: hub.token,
    resource: { app: "Notes", title: process.argv[2] ?? "notes.txt", instance_id: randomUUID() },
    tools,
    instructions: "Call read before append, to see what is already there.",
  });

socket.onmessage = async (event) => {
  const message = JSON.parse(event.data);
  switch (message.type) {
    case "call": {
      let reply;
      try {
        reply = { content: await run(message.tool, message.arguments) };
      } catch (error) {
        reply = { content: String(error.message ?? error), is_error: true };
      }
      send({ type: "result", call_id: message.call_id, ...reply });
      break;
    }
    case "waiting":
      console.log("waiting for the user to allow this program in Clak…");
      break;
    case "registered":
      console.log(`connected: agents call it with \`clak ${message.name} …\``);
      break;
    case "granted":
      console.log(`${message.harness} can use the note`);
      break;
    case "revoked":
      console.log(`${message.harness} can no longer use the note`);
      break;
    case "denied":
    case "error":
      console.error(`${message.type}: ${message.reason ?? message.error}`);
      break;
  }
};

socket.onclose = () => console.log("Clak closed the connection.");

// Closing the note: agents stop seeing it, and their permissions end.
process.on("SIGINT", () => {
  send({ type: "close", reason: "resource_closed" });
  socket.close();
});
