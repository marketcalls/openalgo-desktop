# Updating OpenAlgo Desktop

- [How you hear about a new version](#how-you-hear-about-a-new-version)
- [Installing an update](#installing-an-update)
- [If something is running](#if-something-is-running)
- [Linux .deb installs](#linux-deb-installs)
- [If an update fails](#if-an-update-fails)
- [Installing by hand](#installing-by-hand)
- [Your data is backed up before an update](#your-data-is-backed-up-before-an-update)
- [Going back to an earlier version](#going-back-to-an-earlier-version)

## How you hear about a new version

Each time OpenAlgo Desktop starts, it asks GitHub whether a newer version has
been published. You can also ask at any time: open the profile menu (your
initial, top right) and choose **Check for Updates**.

When there is a newer version, a small notice appears in the bottom-right
corner. **See what is new** shows the version and its release notes;
**Later** hides the notice until OpenAlgo starts again.

Checking only reads the version and its notes. **Nothing is downloaded until
you choose Update**, and OpenAlgo never restarts on its own.

## Installing an update

Choose **Update** in the update window. OpenAlgo then:

1. downloads the new version;
2. checks its signature against the key built into OpenAlgo Desktop, and
   refuses anything that does not match;
3. installs it and restarts. Sign in again: OpenAlgo locks your keys whenever
   it closes, and your broker connection and Live trading resume once you
   sign in.

| System | What the update does |
| --- | --- |
| macOS | Replaces OpenAlgo Desktop in your Applications folder. If it lives where your account cannot write, macOS asks for an administrator password. |
| Windows | Runs the new installer in passive mode (a progress window, no questions) for your user only, then starts OpenAlgo again. |
| Linux AppImage (x64 or ARM64) | Replaces the `.AppImage` file you started, then starts it again. |
| Linux `.deb` | Not updated in place; see below. |

Your account, settings, broker keys, strategies, sandbox data and Historify
data are kept.

## If something is running

Restarting stops OpenAlgo from watching your trades until it opens again
(usually within a minute) and you sign in. Before it restarts, OpenAlgo lists
what would be interrupted and asks you to confirm:

- **Live trading** (shown as a red warning): running Live strategies (their
  stops, targets and exits are not checked until OpenAlgo opens; then they
  resume), Live OpenScript runs (paused; start them again after the update),
  Live positions under scalping protection, open orders at your broker (they
  stay at your broker; OpenAlgo follows them again once it opens) and orders
  whose outcome is not known yet. Choose **Update and restart anyway** only if
  that is acceptable; otherwise choose **Not now** and update after the market
  closes.
- **Sandbox activity only** (a lighter warning): sandbox strategies, runs,
  protection and open orders pause until OpenAlgo opens again. Nothing is sent
  to your broker, and sandbox orders and positions are kept.

If something new starts while the update downloads (a strategy enters, an
order is placed), OpenAlgo asks again before restarting.

## Linux .deb installs

A copy installed from a `.deb` package belongs to your package manager, so
OpenAlgo cannot replace it. It still tells you about new versions; the update
window says so and offers **Open the release page**. Download the new `.deb`
there and install it over the old one (for example by opening it with your
software installer). Your data is kept.

## If an update fails

OpenAlgo stays on the version you have and says why:

- **Could not reach the update server**: check your internet connection and
  try again.
- **Could not read the update information**: try again later, or install by
  hand (below).
- **Failed its signature check**: the download was not what OpenAlgo
  published, so it was not installed. Try again later; if it keeps happening,
  install by hand from the release page.
- **Could not be installed**: install by hand from the release page.

## Installing by hand

Every version is also on the
[releases page](https://github.com/marketcalls/openalgo-desktop/releases).
Download the installer for your system and install it over the old version,
as described in the README under "Download and install". Your data is kept.

## Your data is backed up before an update

However a new version arrives (in the app or by hand), the first time it
opens your data folder it backs up your data before changing anything:

- every database (`openalgo.db`, `logs.db`, `sandbox.db`,
  `historify.duckdb`) and the key file (`vault.json`, when you use one) is
  copied into `backups/<old version>-<date and time>/` inside the data folder
  (where the data folder is: README, "Where your data lives, and backups"),
  readable by your user account only;
- only then does the new version update the databases to its format;
- as soon as the new version has opened your data, the copy is deleted.

The copy is only a safety net for that one start. OpenAlgo keeps no older
copy of your account afterwards, so broker keys you remove, a broker session
that ended, an API key you regenerated or a password you changed or reset
cannot come back from a backup. If a copy is ever left behind (for example
because another program had a file open when OpenAlgo tried to delete it),
OpenAlgo deletes it the next time it starts, and also when you sign out of
your broker, forget a broker, reset your password or reset your account.

That first start can take a little longer when Historify holds a lot of
data, and the backup needs about as much free disk space as your data. If
it cannot be made (a full disk), OpenAlgo changes nothing and asks you to
free some space and start it again.

If the new version cannot open the data after the backup, it puts the
backup back, so everything is exactly as the old version left it, and says
so. Start OpenAlgo again to retry; if it happens again, install the previous
version from the release page: it opens the data as before. In the rare case
the copy cannot be put back, OpenAlgo keeps it, tells you where it is and how
to copy it back by hand. That copy is your data exactly as the previous
version left it a moment before.

OpenAlgo never offers to put back a copy at any other time.

## Going back to an earlier version

`data-version.json` in the data folder records the version that last opened
it. An older version will not open a folder a newer version has used: it
says "Your OpenAlgo data was last opened by version ..." and changes
nothing, because an older version could damage data in a newer format.

OpenAlgo does not keep the data of the earlier version once a newer one
has opened it (see above), so there is no copy to go back to. To use an
earlier version anyway, close OpenAlgo, rename the data folder (for example
to `com.openalgo.desktop-newer`), install the earlier version and start it.
It starts with a new data folder, where you create your account and add
your broker again.

A copy of the whole data folder that you made yourself can also be put
back (README, "Where your data lives, and backups"). It brings back
everything as it was when you made it, including broker keys you have
removed since and the API key of that time, so sign out of your broker and
generate a new API key once it opens if anything has changed since then.
