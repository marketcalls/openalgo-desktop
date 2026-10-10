# Connecting Claude Desktop and Claude Code (MCP)

OpenAlgo Desktop has a built-in MCP server, so an AI assistant such as Claude
Desktop or Claude Code can read your account and market data and, if you allow
it, place orders. It offers the same tools as the OpenAlgo web MCP server.

Orders placed by an AI client follow the analyzer mode switch, exactly like the
API: with analyzer mode on they go to the sandbox, not to your broker. Try it
in sandbox mode first.

## 1. Create a token

1. In OpenAlgo Desktop open the **API Key** page (profile menu).
2. In the **AI clients (MCP)** card, enter a **Client name** (for example
   "Claude Desktop") and choose the **Access**:
   - **Read only**: account, books and market data, no orders.
   - **Read and place orders**: also places, modifies and cancels orders.
3. Choose **Create token**.
4. **Copy the token now. It is shown only once.** The card also shows a
   ready-made configuration for Claude Desktop and a ready-made command for
   Claude Code, both with this computer's path to OpenAlgo Desktop filled in.
   Copy those; they are the easiest way to get the setup right.

Create one token for each AI client, so you can revoke one without affecting
the others.

The token goes in the AI client's own configuration, in the environment
variable `OPENALGO_MCP_TOKEN`. It is never typed on a command line and never
saved in a file of OpenAlgo's.

## 2a. Claude Desktop

Open Claude Desktop's settings, go to **Developer**, and choose **Edit Config**.
This opens `claude_desktop_config.json`:

- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

Paste the configuration from the API Key page. It looks like this:

```json
{
  "mcpServers": {
    "openalgo": {
      "command": "<path to the OpenAlgo Desktop program>",
      "args": ["mcp", "--url", "http://127.0.0.1:5000"],
      "env": { "OPENALGO_MCP_TOKEN": "<your token>" }
    }
  }
}
```

If the file already has an `mcpServers` section, add the `"openalgo": {...}`
entry inside it rather than adding a second section. Save the file and restart
Claude Desktop.

## 2b. Claude Code

Run the command from the API Key page in a terminal, after replacing
`<MCP_TOKEN>` with your token. The page leaves the token out of the command
because your terminal keeps a history of the commands you run; if you prefer,
add the token to Claude Code's settings yourself instead. It looks like this:

```bash
claude mcp add openalgo -e OPENALGO_MCP_TOKEN=<MCP_TOKEN> -- "<path to the OpenAlgo Desktop program>" mcp --url http://127.0.0.1:5000
```

Then check it with `claude mcp list`.

## The path to OpenAlgo Desktop

The `command` is the OpenAlgo Desktop program itself, started with `mcp`. The
API Key page fills in the real path on this computer, so copy the
configuration from there rather than typing the path.

If you run the AppImage on Linux or Raspberry Pi, change that path to the
location of your `.AppImage` file. The page shows a temporary path that
changes every time the AppImage starts.

If you move or reinstall OpenAlgo Desktop somewhere else, create the
configuration again from the API Key page.

## How it works

The AI client starts `openalgo-desktop mcp` in the background. That small
process passes the client's requests to the running OpenAlgo Desktop over
`http://127.0.0.1:5000/mcp`, using your token.

- **OpenAlgo Desktop must be open and signed in**, and connected to your
  broker for market data and orders. If the app is closed, the AI client gets
  "OpenAlgo Desktop is not running, so this request could not be sent".
- If you changed the app port in Server Settings, change `--url` to match.
- `--url` may name another computer only with `https://` (for example a
  tunnel). Plain `http://` is accepted only for this computer
  (`127.0.0.1`, `localhost`), because the token would otherwise cross the
  network unencrypted.

## Managing tokens

The **Active tokens** list on the API Key page shows each token, its access
level and when it was last used. Revoke a token there to disconnect the AI
client using it at once.

If an AI client reports "OpenAlgo did not accept this AI client's token", the
token was revoked or copied incorrectly. Create a new one and update the
client's configuration.

Each token is limited in how fast it can call tools (order placement more
tightly than reading). A client that goes over the limit is asked to wait a
minute.

## AI clients on another computer

Clients on this computer always work. Clients on another computer, or hosted
AI services, connect over HTTP to `/mcp` and need **Remote MCP** turned on in
**Admin**, **Remote MCP**, as well as access from other devices in Server
Settings or a tunnel. Leave Remote MCP off unless you need it.
