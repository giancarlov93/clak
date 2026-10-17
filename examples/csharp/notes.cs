// A minimal Clak extension: a notepad that AI agents can read and append to.
//
//     dotnet run notes.cs [title]      (.NET 10 or later; on .NET 8 paste it into a console project)
//
// The whole protocol is in docs/protocol.md; this file is the smallest complete client.
// Only the standard library: System.Net.WebSockets and System.Text.Json.
using System.Net.WebSockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

const int Protocol = 1;
var home = Environment.GetEnvironmentVariable("CLAK_HOME") is { Length: > 0 } custom
    ? custom
    : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".clak");
var title = args.Length > 0 ? args[0] : "notes.txt";
var lines = new List<string>();

JsonNode hub;
try { hub = JsonNode.Parse(File.ReadAllText(Path.Combine(home, "daemon.json")))!; }
catch (IOException) { Console.Error.WriteLine("Clak isn't running: open it first."); return 1; }
if ((int?)hub["protocol"] is { } spoken && spoken != Protocol)
{
    Console.Error.WriteLine($"Clak speaks protocol {spoken}, this extension {Protocol}.");
    return 1;
}

var tools = new JsonArray(
    new JsonObject
    {
        ["name"] = "read",
        ["description"] = "The whole text of the note.",
        ["input_schema"] = new JsonObject { ["type"] = "object", ["properties"] = new JsonObject() },
    },
    new JsonObject
    {
        ["name"] = "append",
        ["description"] = "Adds a line at the end of the note.",
        ["input_schema"] = JsonNode.Parse("""
            {"type": "object",
             "properties": {"line": {"type": "string", "description": "The line to add."}},
             "required": ["line"]}
            """),
    });

// Runs one command. A thrown exception becomes an error the agent reads.
string Run(string tool, JsonObject arguments) => tool switch
{
    "read" => string.Join("\n", lines),
    "append" => Append((string?)arguments["line"] ?? throw new ArgumentException("“line” is missing")),
    _ => throw new ArgumentException($"no such command: {tool}"),
};
string Append(string line) { lines.Add(line); return "added"; }

using var socket = new ClientWebSocket();
await socket.ConnectAsync(new Uri($"ws://127.0.0.1:{(int)hub["ws_port"]!}"), default);

await Send(new JsonObject
{
    ["type"] = "register",
    ["protocol"] = Protocol,
    ["token"] = (string?)hub["token"],
    ["resource"] = new JsonObject { ["app"] = "Notes", ["title"] = title, ["instance_id"] = Guid.NewGuid().ToString("N") },
    ["tools"] = tools,
    ["instructions"] = "Call read before append, to see what is already there.",
});

while (await Receive() is { } message)
{
    switch ((string?)message["type"])
    {
        case "call":
            var reply = new JsonObject { ["type"] = "result", ["call_id"] = message["call_id"]?.DeepClone() };
            try { reply["content"] = Run((string)message["tool"]!, message["arguments"]!.AsObject()); }
            catch (Exception ex) { reply["content"] = ex.Message; reply["is_error"] = true; }
            await Send(reply);
            break;
        case "waiting": Console.WriteLine("waiting for the user to allow this program in Clak…"); break;
        case "registered": Console.WriteLine($"connected: agents call it with `clak {message["name"]} …`"); break;
        case "granted": Console.WriteLine($"{message["harness"]} can use the note"); break;
        case "revoked": Console.WriteLine($"{message["harness"]} can no longer use the note"); break;
        case "denied" or "error":
            Console.Error.WriteLine($"{message["type"]}: {message["reason"] ?? message["error"]}");
            return 1;
    }
}
Console.WriteLine("Clak closed the connection.");
return 0;

async Task Send(JsonObject message) =>
    await socket.SendAsync(Encoding.UTF8.GetBytes(message.ToJsonString()), WebSocketMessageType.Text, true, default);

// One message can span several frames: read until the end of the message.
async Task<JsonObject?> Receive()
{
    var buffer = new byte[64 * 1024];
    using var text = new MemoryStream();
    while (true)
    {
        WebSocketReceiveResult part;
        try { part = await socket.ReceiveAsync(buffer, default); }
        catch (WebSocketException) { return null; }
        if (part.MessageType == WebSocketMessageType.Close) return null;
        text.Write(buffer, 0, part.Count);
        if (part.EndOfMessage) return JsonNode.Parse(text.ToArray())?.AsObject();
    }
}
