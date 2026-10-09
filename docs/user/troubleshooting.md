# Troubleshooting

- [The app says a port is already in use](#the-app-says-a-port-is-already-in-use)
- [Broker sign-in fails](#broker-sign-in-fails)
- [Master contract errors](#master-contract-errors)
- [Historify says some changes could not be recovered](#historify-says-some-changes-could-not-be-recovered)
- [Another device, a firewall, or a tunnel](#another-device-a-firewall-or-a-tunnel)
- [Your API client says the API key is invalid](#your-api-client-says-the-api-key-is-invalid)

## The app says a port is already in use

OpenAlgo Desktop needs port 5000 for the app, the API and broker redirects, and
port 8765 for the market data WebSocket.

**Port 5000 (the app).** If another program holds it, the app opens on a page
that says "OpenAlgo could not start because port 5000 is already used by
another program", with a **Port** box and a **Try again** button.

- **On a Mac it is almost always AirPlay Receiver.** Open **System Settings**,
  **General**, **AirDrop & Handoff**, turn off **AirPlay Receiver**, then
  choose **Try again**.
- If OpenAlgo web, or a second copy of OpenAlgo Desktop, is running on this
  computer, close it and choose **Try again**.
- Or enter a free port (for example 5001) in the **Port** box and choose **Try
  again**. OpenAlgo remembers it. Then update:
  - the redirect URL in your broker's developer portal, to
    `http://127.0.0.1:5001/<broker>/callback`;
  - the base URL in your Python scripts, Amibroker, Excel and other tools, to
    `http://127.0.0.1:5001`;
  - the `--url` in your AI client's MCP configuration.

**Port 8765 (market data).** If another program holds it, the app still opens
but the market data WebSocket for your SDK and trading platforms does not start
until the port is free; OpenAlgo keeps trying in the background. Close the
other program, or choose another **Market data port** in **Server Settings**
(profile menu), save, restart the app, and update `ws_url` in your scripts.

To run OpenAlgo web and OpenAlgo Desktop on the same computer, give one of them
different ports. In the desktop, change **App port** and **Market data port**
in Server Settings and restart.

## Broker sign-in fails

Check these in order:

1. **Credentials saved for the right broker.** Profile, Broker tab, **Current
   Configuration** shows the broker and a masked API key. If the Broker page
   says to choose your broker and add its API key, do that first.
2. **Redirect URL matches exactly.** The redirect URL in your broker's
   developer portal must be the one shown under Current Configuration,
   usually `http://127.0.0.1:5000/<broker>/callback`. `localhost` instead of
   `127.0.0.1`, `https` instead of `http`, a different port or a trailing slash
   all count as different.
3. **Start from OpenAlgo.** "This broker sign-in was not started from OpenAlgo
   or has expired" means the broker sent you back after the sign-in timed out,
   or to a sign-in OpenAlgo did not start. Go to the Broker page and choose
   **Connect Account** again, and finish the broker's login within a few
   minutes.
4. **Client ID.** "Add your client id in Profile, Broker Configuration" means
   your broker needs the client ID OpenAlgo checks the account against. See
   [Brokers bound to your client ID](brokers.md#brokers-bound-to-your-client-id).
5. **Same account.** "This sign-in is for a different account than the one set
   up in OpenAlgo" means you logged in to the broker with another account.
   Log in with the configured account, or, to switch accounts, log out and
   save the broker settings again with the new account's details.
6. **TOTP rejected.** Wait for a fresh code and make sure your computer's clock
   is set automatically; a clock that is off by a minute makes every code
   wrong.
7. **Too many attempts.** After repeated failures OpenAlgo pauses sign-in
   attempts for a while. Wait and try again.
8. **Subscriptions and IP.** Zerodha needs a Kite Connect subscription and Dhan
   a Data API subscription for market data. Samco checks that your internet
   address is the static IP registered with Samco.

If it still fails, open **Logs** and look at **Live Logs** while you try
again; it shows where the sign-in stopped, which helps when you ask for help
on Discord or GitHub.

## Master contract errors

The master contract is your broker's list of tradable symbols. It downloads
after you connect to the broker. Until it has loaded, symbol search, option
chains and orders by symbol do not work.

- Open the **Master Contract** page (profile menu) to see its status, the
  number of symbols per exchange and when it was last downloaded.
- "The master contract is downloading right now" means a download is already
  running. Wait for it to finish.
- If the status shows an error, or symbols are missing, choose **Force
  Download**. If it keeps failing, connect to the broker again from the Broker
  page (an expired broker session is the usual cause) and download again.
- **Reload Cache** reloads the symbols already downloaded into memory, without
  downloading again.
- Normally OpenAlgo downloads once a day. If a download is skipped, the page
  says why.

## Historify says some changes could not be recovered

If OpenAlgo Desktop was closed suddenly (a crash, a forced quit, a power cut)
while Historify was saving data, you may see this alert on the **Health
Monitor** page (**Logs**, then **Health Monitor**):

> OpenAlgo did not close cleanly last time, and some unsaved Historify changes
> from that session could not be recovered. Historify has been reopened with
> the data saved before then. Check your Historify watchlist and download any
> recent data again if it is missing.

Your Historify data from before that session is intact. Open **Historify**,
check your watchlist, and download the most recent data again. The unsaved
changes are kept, not deleted, in a file named
`historify.duckdb.wal.unreplayable-<date and time>` in your data folder; you
can delete it once you are satisfied nothing is missing.

## Another device, a firewall, or a tunnel

**Using OpenAlgo from another device on your network.**

1. In **Server Settings**, turn on **Allow access from other devices**, save,
   and restart the app.
2. On the other device, open `http://<this computer's IP address>:5000`, for
   example `http://192.168.1.20:5000`. Use the IP address, not the computer's
   name: names other than `localhost` are refused.
3. Your firewall may ask whether to allow OpenAlgo Desktop to accept incoming
   connections. Allow it on private networks only. If you missed the prompt:
   - Windows: **Windows Security**, **Firewall & network protection**, **Allow
     an app through firewall**, and allow OpenAlgo Desktop on **Private**.
   - macOS: **System Settings**, **Network**, **Firewall**, **Options**, and
     allow OpenAlgo Desktop.
   - Linux with `ufw`: `sudo ufw allow from 192.168.1.0/24 to any port 5000`
     (and port 8765 for market data), using your own network's range.

Anyone on your network can then reach the login page, so use a strong
password and turn on two-factor login (**Profile**, **TOTP** tab, **Enable
2FA**). Leave this off on public or shared networks.

Broker sign-in still has to be done on the computer running OpenAlgo: the
broker sends you back to `127.0.0.1`, which on another device means that
device, not this computer.

**Using a tunnel (ngrok, Cloudflare Tunnel) for TradingView or Chartink
alerts.** Enter the tunnel's public address as **Host Server URL** under
**Server Configuration** on the Broker tab in Profile. Requests that arrive
through a tunnel whose address is not entered there are refused.

Everything that arrives through a tunnel or proxy on this computer is treated
as coming from the internet, never from this computer:

- **Remote MCP** must be on for AI clients to use OpenAlgo through the tunnel.
- Through a tunnel OpenAlgo cannot see the callers' addresses: they all share
  one identity. A strategy webhook's **IP allowlist** therefore applies to
  devices on your network only: it never matches a caller that comes through
  a tunnel (nor a program on this computer), and blocking an address on the
  Security page does not apply to tunnel callers. Protect your webhooks with
  their secret address, which OpenAlgo always requires: keep it private, and
  rotate it if it leaks.
- Invalid API keys and wrong webhook addresses that come through the tunnel
  are counted per key or address tried, so a stranger's attempts never block
  your own alerts or your correct key. Your own programs on this computer are
  not affected by what tunnel callers do.
- Signing in through the tunnel works only from the OpenAlgo page itself.
- A plain port forwarder that adds no forwarding header (for example `socat`
  or `ssh -R`) makes its callers look like programs on this computer. Use a
  tunnel that adds one, such as ngrok or Cloudflare Tunnel.

Signing in to OpenAlgo slows down after five wrong passwords or
authenticator codes from the same place (this computer, the tunnel, or one
device on your network): 30 seconds, then twice as long after each further
mistake, up to 5 minutes. Mistakes made elsewhere never slow down signing in
on this computer. Sign in with the right password once the wait is over.

## Your API client says the API key is invalid

- Use the key from the **API Key** page of OpenAlgo Desktop. A key from
  OpenAlgo web does not work here.
- If you regenerated the key, update every tool that uses it.
- On a computer with no system keychain (for example Raspberry Pi OS Lite),
  OpenAlgo protects its keys with your password. After the app starts, sign
  in once; until then every API request is refused as an invalid key.
- After several invalid-key attempts in a minute from another device on your
  network, OpenAlgo blocks further attempts from that device for a while. Fix
  the key, wait, and try again. Programs on this computer are never blocked
  this way, a web page open in your browser cannot cause it, and through a
  tunnel only the wrong key itself is blocked, never your correct one.
