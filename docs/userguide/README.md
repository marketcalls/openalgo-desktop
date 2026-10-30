# OpenAlgo Desktop User Guide

The step-by-step guide to OpenAlgo Desktop: from installing the app to placing
orders from TradingView, Amibroker, Chartink and your own code.

OpenAlgo Desktop is OpenAlgo web for one trader on one computer. The chapters
follow the OpenAlgo web user guide, chapter for chapter, and describe what the
desktop does. Where the desktop differs from the web, the chapter says so in
plain words; a feature the desktop does not have gets a short page that says
so and points to OpenAlgo web.

## Getting started (chapters 01-05)

- [01 - What is OpenAlgo Desktop](./01-what-is-openalgo/README.md)
- [02 - Why build with OpenAlgo](./02-why-build-with-openalgo/README.md)
- [03 - Key concepts](./03-key-concepts/README.md)
- [03 - System requirements](./03-system-requirements/README.md): supported systems, ports, and why Raspberry Pi users run OpenAlgo web
- [04 - Installation](./04-installation/README.md): installers, the first open of an unsigned app, where your data lives, running next to OpenAlgo web
- [05 - First-time setup](./05-first-time-setup/README.md): your account, what your password protects, two-factor sign-in

## Broker and interface (chapters 06-10)

- [06 - Broker connection](./06-broker-connection/README.md): keys per broker, redirect URL, one broker at a time, the daily 03:00 IST login
- [07 - Dashboard overview](./07-dashboard-overview/README.md)
- [08 - Understanding the interface](./08-understanding-interface/README.md)
- [09 - API key management](./09-api-key-management/README.md): the API key, Auto and Semi-Auto, tokens for AI clients
- [10 - Placing your first order](./10-placing-first-order/README.md)

## Order management (chapters 11-15)

- [11 - Order types](./11-order-types/README.md)
- [12 - Smart orders](./12-smart-orders/README.md)
- [13 - Basket orders](./13-basket-orders/README.md): baskets and split orders
- [14 - Positions and holdings](./14-positions-holdings/README.md)
- [15 - Analyzer mode (sandbox)](./15-analyzer-mode/README.md)

## Reference

- [Symbol format guide](./symbol-format/README.md): how OpenAlgo writes equities, futures, options and indices

## Platform integrations (chapters 16-19)

- [16 - TradingView integration](./16-tradingview-integration/README.md): webhooks, tunnels, and what reaching the desktop from the internet takes
- [17 - Amibroker integration](./17-amibroker-integration/README.md)
- [18 - ChartInk integration](./18-chartink-integration/README.md)
- [19 - GoCharting integration](./19-gocharting-integration/README.md)

## Strategy building (chapters 20-22)

- [20 - Python strategies](./20-python-strategies/README.md): the Python Strategy Host is not in the desktop; your scripts still run with the Python SDK
- [21 - Flow visual builder](./21-flow-visual-builder/README.md): drag-and-drop workflows, their triggers and webhooks, import and export with OpenAlgo web, and the Agent building them
- [22 - Action Center](./22-action-center/README.md)

## Monitoring and alerts (chapters 23-26)

- [23 - Telegram bot (and WhatsApp)](./23-telegram-bot/README.md)
- [24 - PnL tracker](./24-pnl-tracker/README.md)
- [25 - Latency monitor](./25-latency-monitor/README.md)
- [26 - Traffic logs](./26-traffic-logs/README.md)

## Security and support (chapters 27-30)

- [27 - Security settings](./27-security-settings/README.md)
- [28 - Two-factor authentication](./28-two-factor-auth/README.md)
- [29 - Troubleshooting](./29-troubleshooting/README.md)
- [30 - FAQs](./30-faqs/README.md)

## Analytics tools (chapter 31)

- [31 - Tools](./31-tools/README.md): option chain, Greeks, OI tools, volatility, straddles, strategy builder

## Charting (chapter 32)

- [32 - Charting terminal](./32-charting-terminal/README.md): trade from the chart, the order dock, drawing tools and shortcuts

## Quick navigation

| I want to... | Go to |
| --- | --- |
| Know what OpenAlgo Desktop does | [Chapter 01](./01-what-is-openalgo/README.md) |
| Move over from OpenAlgo web | [Moving from OpenAlgo web](../user/moving-from-openalgo-web.md) |
| Install it | [Chapter 04](./04-installation/README.md) |
| Fix "port 5000 is already used" | [Chapter 04](./04-installation/README.md#if-port-5000-is-already-in-use) |
| Understand why tools say "Sign in to OpenAlgo" | [Chapter 05](./05-first-time-setup/README.md#what-your-password-protects) |
| Connect my broker | [Chapter 06](./06-broker-connection/README.md) |
| Place my first order | [Chapter 10](./10-placing-first-order/README.md) |
| Test without real money | [Chapter 15](./15-analyzer-mode/README.md) |
| Find a symbol's OpenAlgo name | [Symbol format guide](./symbol-format/README.md) |
| Receive TradingView alerts | [Chapter 16](./16-tradingview-integration/README.md) |
| Automate a strategy without code | [Chapter 21](./21-flow-visual-builder/README.md) |
| Connect Claude Desktop or Claude Code | [MCP guide](../user/mcp.md) |
| Get Telegram or WhatsApp alerts | [Chapter 23](./23-telegram-bot/README.md) |
| Use the options tools | [Chapter 31](./31-tools/README.md) |
| Update the app | [Updating OpenAlgo Desktop](../user/updating.md) |

## Desktop guides

Short guides for things only the desktop has, or does differently. The
chapters link to them where they matter.

- [Moving from OpenAlgo web](../user/moving-from-openalgo-web.md)
- [Broker sign-in](../user/brokers.md): every broker's keys and login
- [Sandbox mode (analyzer mode)](../user/sandbox.md): how sandbox orders fill and what the simulation leaves out
- [Connecting Claude Desktop and Claude Code (MCP)](../user/mcp.md)
- [Updating OpenAlgo Desktop](../user/updating.md)
- [Troubleshooting](../user/troubleshooting.md)

## More

- The project [README](../../README.md): supported systems, downloads, data folder, building from source
- The REST API reference: [API documentation](../api/README.md)
- OpenAlgo documentation: [docs.openalgo.in](https://docs.openalgo.in)
- Community: [Discord](https://discord.com/invite/UPh7QPsNhP)
- Videos: [YouTube](https://www.youtube.com/@openalgo)

## Support

If something does not work:

1. Check [Troubleshooting](./29-troubleshooting/README.md) and the desktop
   [Troubleshooting guide](../user/troubleshooting.md).
2. Read the [FAQs](./30-faqs/README.md).
3. Ask on [Discord](https://discord.com/invite/UPh7QPsNhP).
4. Open a [GitHub issue](https://github.com/marketcalls/openalgo-desktop/issues).
