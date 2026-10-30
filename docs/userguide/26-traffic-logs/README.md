# 26 - Traffic Logs

## Overview

OpenAlgo Desktop records one line for each request its server answers: when
it came, from where, what it asked for, how it was answered and how long it
took. Use it to see whether an alert or a program reached OpenAlgo at all, and
what OpenAlgo answered.

Open **Logs**, then **Traffic Monitor** (`/logs/traffic`). It is also on the
**Admin** page as **Traffic Dashboard**. The page title is **Traffic
Dashboard**.

## What is recorded

| Field | Meaning |
| --- | --- |
| Timestamp | When the request arrived |
| Method | GET, POST and so on |
| Path | The address asked for, without its query string |
| Status | The answer's status code (for example 200, 403, 503) |
| Duration | Time to answer, in milliseconds |
| Client IP | Who asked (below) |
| Host | The host name the request was addressed to |
| Error | Kept for the web's export format. The desktop leaves it empty: the status code says how the request ended |

Never recorded: request or response bodies, query strings, headers other than
Host, and your API key. The secret part of a strategy webhook address, of
a Chartink webhook address and of a Flow webhook address is replaced before
the path is stored (a Flow address keeps a symbol after it, as in
`/flow/webhook/<redacted>/SBIN`); a `?secret=` is never stored, as no query
string is.

Not recorded, to keep the log readable: the app's own files (scripts, styles,
images), the live-update connection the pages use, and the traffic page's own
requests.

### Who the Client IP is

| Client IP | Means |
| --- | --- |
| `127.0.0.1` | A program on this computer, including the OpenAlgo window itself |
| A network address (for example `192.168.1.20`) | A device on your network, when **Allow access from other devices** is on |
| `100::2` | Anyone who came through a tunnel (ngrok, Cloudflare Tunnel). OpenAlgo cannot see tunnel callers' real addresses, so they all share this one |

In the desktop, tunnel callers share one identity: OpenAlgo never trusts a
forwarding header as an address. See
[Troubleshooting: another device, a firewall, or a tunnel](../../user/troubleshooting.md#another-device-a-firewall-or-a-tunnel).

## The page

- **Total Requests**, **Error Requests** (answers of 400 and above) and
  **Avg Duration**, for all traffic or for the API only.
- **Traffic Logs**: the latest 100 requests, newest first. The tabs choose
  **All Traffic** or **API Traffic** (`/api/v1` only); the filter chooses
  **All**, **Errors Only** or **Success Only**.
- **API Endpoint Statistics**: per endpoint (placeorder, placesmartorder,
  modifyorder, cancelorder, quotes, history, depth, intervals, funds,
  orderbook, tradebook, positionbook, holdings, basketorder, splitorder,
  orderstatus, openposition): requests, errors, average duration and success
  rate.

The page refreshes every 30 seconds; **Refresh** reloads it at once.

## Export

**Export to CSV** downloads every stored request as `traffic_logs.csv`,
newest first, with the columns Timestamp, Client IP, Method, Path, Status
Code, Duration (ms), Host and Error. In the app window the file is saved like
any download.

## What is kept

The traffic log lives in `logs.db` in your data folder. Rows older than 30
days are removed, and the log never holds more than the newest 100,000
requests. Old rows are removed every ten minutes. There is no setting for
this in the desktop; OpenAlgo web's `TRAFFIC_LOG_RETENTION_DAYS` has no
equivalent here.

If the computer is so busy that the log cannot keep up, some lines are
dropped rather than slowing down trading. The count of dropped and unsaved
records is in the diagnostics report (Admin, Diagnostics, "Records not saved
since start").

## Using it

- **An alert did not trade.** Look for its path (for example
  `/api/v1/placeorder` or `/chartink/webhook/...`) at the alert's time. No
  line: the alert never reached OpenAlgo (tunnel down, wrong address). A line
  with an error status: OpenAlgo refused it; the status tells you why:

  | Status | Usual reason |
  | --- | --- |
  | 400 | A field is missing or malformed |
  | 403 | Wrong API key, no broker logged in, or a refused webhook |
  | 404 | Wrong address |
  | 429 | Too many requests from that caller |
  | 503 | OpenAlgo is locked: sign in after a restart |

- **Someone is probing your address.** Many 404s or 403s from one network
  address: see [27 - Security Settings](../27-security-settings/README.md) to
  ban it. Callers through a tunnel cannot be banned by address; protect your
  webhook addresses and API key instead.

---

**Previous**: [25 - Latency Monitor](../25-latency-monitor/README.md)

**Next**: [27 - Security Settings](../27-security-settings/README.md)
