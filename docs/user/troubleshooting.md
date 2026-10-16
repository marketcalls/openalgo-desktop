# Troubleshooting

- [The app says a port is already in use](#the-app-says-a-port-is-already-in-use)
- [Broker sign-in fails](#broker-sign-in-fails)
- [Master contract errors](#master-contract-errors)
- [Historify says some changes could not be recovered](#historify-says-some-changes-could-not-be-recovered)
- [Another device, a firewall, or a tunnel](#another-device-a-firewall-or-a-tunnel)
- [Your API client says the API key is invalid](#your-api-client-says-the-api-key-is-invalid)
- [OpenAlgo could not open your data](#openalgo-could-not-open-your-data)

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

1. **Keys saved for the right broker.** On **Broker Configuration**, choose
   your broker: the badge says **Configured** and each saved key shows a
   masked value. If a key is missing, enter it and save.
2. **Redirect URL matches exactly.** The redirect URL in your broker's
   developer portal must be the one Broker Configuration shows for that
   broker, usually `http://127.0.0.1:5000/<broker>/callback`. `localhost`
   instead of `127.0.0.1`, `https` instead of `http`, a different port or a
   trailing slash all count as different.
3. **Start from OpenAlgo.** "This broker sign-in was not started from OpenAlgo
   or has expired" means the broker sent you back after the sign-in timed out,
   or to a sign-in OpenAlgo did not start. On **Select Broker**, choose
   **Login with** again, and finish the broker's login within a few minutes.
   A failed login's reason shows on that broker's card.
4. **Client ID.** A message asking you to add your client ID (or User ID) on
   the Broker Configuration page means your broker needs the account OpenAlgo
   checks the sign-in against. See
   [Brokers bound to your client ID](brokers.md#brokers-bound-to-your-client-id).
5. **Same account.** "This sign-in is for a different account than the one set
   up in OpenAlgo" means you logged in to the broker with another account.
   Log in with the configured account, or, to switch accounts, log out and
   save the broker's keys again with the new account's details.
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
through a tunnel whose address is not entered there are refused. When you
stop using the tunnel, choose **Use automatic address** under the field (or
clear it) and save, so OpenAlgo goes back to its own address on this
computer, including after a later port change.

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
- If the answer is "Sign in to OpenAlgo to use the API", the key is not the
  problem: OpenAlgo protects its keys with your password, so after it starts,
  and after you log out, the API stays off until you sign in. Sign in to
  OpenAlgo and try again. The market data feed, webhooks and AI clients say
  the same.
- After several invalid-key attempts in a minute from another device on your
  network, OpenAlgo blocks further attempts from that device for a while. Fix
  the key, wait, and try again. Programs on this computer are never blocked
  this way, a web page open in your browser cannot cause it, and through a
  tunnel only the wrong key itself is blocked, never your correct one.

## Signing in, and a forgotten password

OpenAlgo Desktop keeps your saved broker keys, your API key and your
two-factor login secret encrypted, and locks the key that opens them with your
OpenAlgo password. Nothing is kept in your computer's keychain.

- **After OpenAlgo starts, sign in first.** Until you do, the API, the market
  data feed, TradingView and Chartink webhooks, AI clients (MCP) and your
  broker connection stay off. Your saved broker session comes back when you
  sign in, if it is still from today, and Telegram and WhatsApp bots you had
  switched on start again. Logging out (or quitting OpenAlgo) turns them off
  again and wipes the unlocked key from memory.
- **Forgot your password?** Choose **Forgot password?** on the sign-in page,
  in the OpenAlgo Desktop window on this computer. Without the old password
  nothing it locked can be opened, so setting a new password keeps your
  account, settings, strategies, watchlists and history, and then you:
  - enter your broker keys again on **Broker Configuration** and sign in to
    your broker;
  - copy the new API key from the **API Key** page into every tool that uses
    it (TradingView, Amibroker, Python scripts, Excel);
  - set up two-factor login again (**Profile**, **TOTP** tab), if you used it;
  - add your Telegram bot again and pair WhatsApp again, if you used them.

  Your AI client tokens, webhook addresses and Chartink addresses keep
  working.
- **First sign-in after upgrading from an earlier build.** Earlier builds kept
  the keys in your computer's keychain. The first sign-in in the OpenAlgo
  Desktop window reads them from there one last time, moves them into
  OpenAlgo's own database, checks them and deletes the keychain entries. On a Mac, choose **Allow** (or **Always
  Allow**) when macOS asks; on Linux, unlock your keyring if
  asked; on Windows, the Credential Manager does not usually ask. After this
  OpenAlgo never uses the keychain again.
- **"OpenAlgo needs to read your keys from the system keychain one last
  time".** The keychain was locked or the request was refused. Choose
  **Allow the keychain once and retry** and allow access when asked, or
  choose **Start fresh**, which sets a new password as described above.
- **"The keys for this account are not in this computer's keychain".** The
  data folder came from another computer, or from before an account reset.
  Restore the folder on the computer it came from, or choose **Start fresh**.
- **"Sign in once in the OpenAlgo Desktop window".** You signed in from a
  browser, another device or a tunnel before the keys of an earlier build
  were moved. Sign in once in the OpenAlgo Desktop window on the computer
  where it is installed; after that you can sign in from anywhere again.
- **"Invalid credentials" in a browser although the password is right.**
  Outside the OpenAlgo Desktop window every failed sign-in reads "Invalid
  credentials", so a failed sign-in never tells anyone whether a password was
  right. If the saved keys cannot be opened, sign in in the OpenAlgo Desktop
  window: it says what is wrong and offers the ways out. Do not choose
  **Forgot password?** until you have tried there.
- **Old keychain entries.** If OpenAlgo could not delete them after the move
  (or you started fresh because the keychain could not be read), they are left
  unused. You can remove them yourself: on a Mac, open **Keychain Access** and
  delete the entries named `com.openalgo.desktop`; on Windows, open
  **Credential Manager**, **Windows Credentials**; on Linux, use your
  keyring manager (for example **Passwords and Keys**).

## OpenAlgo could not open your data

OpenAlgo shows this page instead of starting when opening your data folder
would be unsafe. The message names the cause; nothing in the folder was
changed.

- **"Your OpenAlgo data was last opened by version X, which is newer than
  this copy"**: you started an older version after a newer one. Install
  version X or newer, or go back as described in
  [Going back to an earlier version](updating.md#going-back-to-an-earlier-version).
- **"OpenAlgo Desktop is already running"**: another OpenAlgo window has the
  data open. Close it (on Windows, check the taskbar; on a Mac, the Dock),
  then start OpenAlgo again.
- **"this backup could not be made"**: OpenAlgo backs up your data before an
  update changes it and could not, usually because the disk is full. Free
  some space and start it again.
- **"could not open your data ... was put back exactly as it was"**: the new
  version could not update your data, so it restored the backup it had just
  made. Start OpenAlgo again; if the same message comes back, install the
  previous version from the release page and report the problem.
- **"this backup could not be made"** can also mean `backups` inside the
  data folder is not an ordinary folder (a link, or a file): remove it and
  start OpenAlgo again.

