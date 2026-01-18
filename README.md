# OpenAlgo Desktop - Cross-Platform Algorithmic Trading Application

<div align="center">

[![GitHub Stars](https://img.shields.io/github/stars/marketcalls/openalgo?style=social)](https://github.com/marketcalls/openalgo)
[![X (formerly Twitter) Follow](https://img.shields.io/twitter/follow/openalgoHQ)](https://twitter.com/openalgoHQ)
[![YouTube Channel Subscribers](https://img.shields.io/youtube/channel/subscribers/UCw7eVneIEyiTApy4RtxrJsQ)](https://www.youtube.com/@openalgo)
[![Discord](https://img.shields.io/discord/1219847221055455263)](https://discord.com/invite/UPh7QPsNhP)

**Native Desktop Application | Zero Config | Secure by Design**

</div>

**OpenAlgo Desktop** is a native, cross-platform desktop application for algorithmic trading. Built with Tauri 2.0 and Rust, it provides the same powerful features as OpenAlgo web but runs entirely on your machine - no server setup required. Your credentials are stored securely in your operating system's keychain, not in configuration files.

## Why OpenAlgo Desktop?

| Feature | OpenAlgo Web | OpenAlgo Desktop |
|---------|--------------|------------------|
| Installation | Server setup required | One-click installer |
| Credentials | .env files | OS Keychain (secure) |
| Configuration | Manual setup | GUI-based |
| Deployment | Cloud/VPS | Your computer |
| Network | Internet required | Local-first |
| Privacy | Your server | 100% local |

## Quick Links

- **OpenAlgo Documentation**: [docs.openalgo.in](https://docs.openalgo.in)
- **Main Repository**: [github.com/marketcalls/openalgo](https://github.com/marketcalls/openalgo)
- **Discord Community**: [Join us](https://discord.com/invite/UPh7QPsNhP)

## Supported Platforms

| Platform | Architecture | Status |
|----------|--------------|--------|
| macOS | Intel (x64) | Supported |
| macOS | Apple Silicon (ARM64) | Supported |
| Windows | x64 | Supported |
| Windows Server | x64 | Supported |
| Linux | x64 | Supported |
| Linux | ARM64 | Supported |

## Supported Brokers

<details>
<summary>Initial Release (3 Brokers)</summary>

- **Angel One** - Full support with TOTP authentication
- **Zerodha** - Full support with request token flow
- **Fyers** - Full support with auth code flow

</details>

More brokers will be added in future releases to match the full OpenAlgo broker ecosystem.

## Core Features

### Same UI, Native Performance
- **1:1 Clone** of OpenAlgo web interface
- Same theme (OKLch colors), same components, same workflow
- Native performance with Rust backend
- Offline-capable (no internet required for UI)

### Zero Configuration
- **No .env files** - All configuration through GUI
- **No server setup** - Just install and run
- **No port forwarding** - Runs locally
- **Automatic database** - SQLite created on first run

### Enterprise-Grade Security
- **OS Keychain Storage** - Credentials stored in:
  - macOS: Keychain
  - Windows: Credential Manager
  - Linux: Secret Service (GNOME Keyring/KWallet)
  - Windows Server: DPAPI encrypted file (headless fallback)
- **AES-256-GCM** - Auth tokens encrypted before database storage
- **Argon2id** - Password hashing with hardware-resistant algorithm
- **Zero Data Collection** - Everything stays on your machine

### Compliance Features
- **Auto-Logout at 3:00 AM IST** - Automatic session cleanup for broker compliance
- **Token Refresh** - Fresh authentication each trading day
- **Audit Ready** - Complete local logs for compliance

### Trading Features
- **Order Management** - Place, modify, cancel orders
- **Portfolio Tracking** - Positions, holdings, P&L
- **Market Data** - Real-time quotes via WebSocket
- **Strategy Management** - TradingView webhook strategies
- **Sandbox Mode** - Paper trading with virtual capital
- **Historical Data** - DuckDB-powered Historify integration

## Tech Stack

### Frontend
- **React 19** - Modern UI with concurrent features
- **TypeScript** - Type-safe development
- **Zustand** - Lightweight state management
- **TanStack Query** - Server state and caching
- **shadcn/ui** - Beautiful, accessible components
- **Tailwind CSS v4** - Utility-first styling

### Backend (Rust)
- **Tauri 2.0** - Native app framework
- **rusqlite** - SQLite database
- **DuckDB** - Analytical queries (Historify)
- **keyring** - OS keychain integration
- **aes-gcm** - Encryption
- **argon2** - Password hashing
- **tokio** - Async runtime
- **reqwest** - HTTP client
- **tokio-tungstenite** - WebSocket client

## Installation

### Download Installers

Download the latest release for your platform:

| Platform | Download |
|----------|----------|
| macOS (Universal) | `OpenAlgo-Desktop-x.x.x-universal.dmg` |
| Windows | `OpenAlgo-Desktop-x.x.x-x64.msi` |
| Linux (Debian/Ubuntu) | `openalgo-desktop_x.x.x_amd64.deb` |
| Linux (AppImage) | `OpenAlgo-Desktop-x.x.x-x86_64.AppImage` |

### First Run

1. **Install** the application for your platform
2. **Launch** OpenAlgo Desktop
3. **Create Account** - Set up your local user (password stored with Argon2)
4. **Add Broker** - Enter your broker API credentials via GUI
5. **Start Trading** - Credentials securely stored in OS keychain

## Development

### Prerequisites

- **Node.js** 20+
- **Rust** 1.77+
- **Platform-specific requirements**:

<details>
<summary>macOS</summary>

```bash
xcode-select --install
```

</details>

<details>
<summary>Windows</summary>

- Visual Studio 2022 Build Tools with C++ workload
- WebView2 (usually pre-installed on Windows 10/11)

</details>

<details>
<summary>Linux (Debian/Ubuntu)</summary>

```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev \
  build-essential \
  curl \
  wget \
  file \
  libssl-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

</details>

### Development Commands

```bash
# Install dependencies
npm install

# Run in development mode (hot reload)
npm run tauri:dev

# Build for production
npm run tauri:build

# Type check
npm run typecheck

# Lint
npm run lint

# Run tests
npm run test
```

## Project Structure

```
openalgo-desktop/
├── src-tauri/                    # Rust backend
│   ├── Cargo.toml               # Rust dependencies
│   ├── tauri.conf.json          # Tauri configuration
│   └── src/
│       ├── main.rs              # Entry point
│       ├── lib.rs               # Library exports
│       ├── error.rs             # Error types
│       ├── state.rs             # App state
│       ├── commands/            # Tauri IPC commands
│       │   ├── auth.rs          # Login, logout
│       │   ├── broker.rs        # Broker login
│       │   ├── orders.rs        # Order management
│       │   ├── positions.rs     # Position tracking
│       │   ├── holdings.rs      # Holdings data
│       │   ├── funds.rs         # Account funds
│       │   ├── quotes.rs        # Market data
│       │   ├── symbols.rs       # Symbol search
│       │   ├── strategy.rs      # Strategy CRUD
│       │   ├── settings.rs      # App settings
│       │   ├── sandbox.rs       # Paper trading
│       │   └── historify.rs     # Historical data
│       ├── db/
│       │   ├── sqlite/          # SQLite tables
│       │   └── duckdb/          # DuckDB (Historify)
│       ├── brokers/
│       │   ├── mod.rs           # Broker trait
│       │   ├── types.rs         # Common types
│       │   ├── angel/           # Angel One adapter
│       │   ├── zerodha/         # Zerodha adapter
│       │   └── fyers/           # Fyers adapter
│       ├── security/
│       │   ├── keychain.rs      # OS keychain
│       │   ├── encryption.rs    # AES-256-GCM
│       │   └── hashing.rs       # Argon2id
│       └── websocket/
│           ├── manager.rs       # Connection manager
│           └── handlers.rs      # Binary protocols
├── src/                          # React frontend
│   ├── api/                     # API client (Tauri invoke)
│   │   ├── client.ts            # Tauri IPC wrapper
│   │   ├── trading.ts           # Trading operations
│   │   ├── strategy.ts          # Strategy management
│   │   └── auth.ts              # Authentication
│   ├── components/
│   │   ├── ui/                  # shadcn/ui components
│   │   └── layout/              # Layout components
│   ├── pages/                   # Page components
│   ├── stores/                  # Zustand stores
│   └── index.css                # Tailwind + theme
├── package.json
├── vite.config.ts
└── tsconfig.json
```

## Security Architecture

### Credential Storage

```
┌─────────────────────────────────────────────────────────────┐
│                     User's Computer                          │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  ┌─────────────────┐         ┌─────────────────────────┐   │
│  │  OpenAlgo       │         │   OS Keychain           │   │
│  │  Desktop        │◄───────►│   (macOS/Windows/Linux) │   │
│  │                 │         │                         │   │
│  │  - UI           │         │   - Broker API Keys     │   │
│  │  - Trading      │         │   - API Secrets         │   │
│  │  - Strategies   │         │   - Master Encryption   │   │
│  └────────┬────────┘         │     Key                 │   │
│           │                  └─────────────────────────┘   │
│           │                                                 │
│           ▼                                                 │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                    SQLite Database                   │   │
│  │  ~/Library/Application Support/com.openalgo.desktop │   │
│  │                                                      │   │
│  │  - User (Argon2id hashed password)                  │   │
│  │  - Auth Tokens (AES-256-GCM encrypted)              │   │
│  │  - Settings, Strategies, Symbols                    │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

### Data Directories

| Platform | Location |
|----------|----------|
| macOS | `~/Library/Application Support/com.openalgo.desktop/` |
| Windows | `C:\Users\<user>\AppData\Roaming\com.openalgo.desktop\` |
| Linux | `~/.config/com.openalgo.desktop/` |

## Auto-Logout (Compliance)

OpenAlgo Desktop automatically logs out broker sessions at **3:00 AM IST** daily:

- Broker auth tokens are valid for ~24 hours only
- 3:00 AM IST is well outside market hours (9:15 AM - 3:30 PM IST)
- Ensures fresh authentication each trading day
- **Compliance requirement** for Indian brokers

### Warning Schedule

| Time (IST) | Notification |
|------------|--------------|
| 2:30 AM | "Auto-logout in 30 minutes" |
| 2:55 AM | "Auto-logout in 5 minutes" |
| 3:00 AM | Session cleared, redirect to login |

## Key Benefits

- **Zero Config** - No server setup, no .env files, no port forwarding
- **Secure by Design** - OS keychain, not filesystem credentials
- **Cross-Platform** - Same experience on macOS, Windows, Linux
- **Privacy First** - Everything runs locally, zero data collection
- **Native Performance** - Rust backend, not Electron
- **Familiar UI** - Same interface as OpenAlgo web
- **Offline Capable** - UI works without internet
- **Auto-Updates** - Built-in update mechanism

## Comparison with OpenAlgo Web

| Aspect | OpenAlgo Web | OpenAlgo Desktop |
|--------|--------------|------------------|
| Setup | Clone repo, configure .env, run server | Download and install |
| Credentials | .env file | OS Keychain |
| Database | SQLite on server | SQLite local |
| Updates | git pull | Built-in updater |
| Hosting | Self-hosted server/cloud | Your computer |
| Multi-user | Yes | Single user |
| Multi-broker | Yes (simultaneous) | One broker at a time |
| Python Strategies | Yes | Not supported |
| ChartInk | Yes | Not supported |
| MCP Server | Yes | Not supported |

## Roadmap

- [ ] More brokers (matching full OpenAlgo ecosystem)
- [ ] TradingView webhook server (local)
- [ ] Strategy backtesting
- [ ] Multi-monitor support
- [ ] System tray with quick actions
- [ ] Auto-start on login
- [ ] Keyboard shortcuts

## Contributing

We welcome contributions! To contribute:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## Community & Support

- **Discord**: [Join our community](https://www.openalgo.in/discord)
- **Twitter/X**: [@openalgoHQ](https://twitter.com/openalgoHQ)
- **YouTube**: [@openalgo](https://www.youtube.com/@openalgo)
- **GitHub Issues**: [Report bugs or request features](https://github.com/marketcalls/openalgo/issues)

## License

OpenAlgo Desktop is released under the **AGPL V3.0 License**. See [LICENSE](LICENSE.md) for details.

## Credits

### Third-Party Libraries

**Frontend:**
- [React](https://react.dev) - MIT License
- [shadcn/ui](https://ui.shadcn.com) - MIT License
- [TanStack Query](https://tanstack.com/query) - MIT License
- [Zustand](https://zustand-demo.pmnd.rs) - MIT License
- [Tailwind CSS](https://tailwindcss.com) - MIT License

**Backend:**
- [Tauri](https://tauri.app) - MIT/Apache 2.0 License
- [Tokio](https://tokio.rs) - MIT License
- [rusqlite](https://github.com/rusqlite/rusqlite) - MIT License
- [DuckDB](https://duckdb.org) - MIT License

## Disclaimer

**This software is for educational purposes only. Do not risk money which you are afraid to lose. USE THE SOFTWARE AT YOUR OWN RISK. THE AUTHORS AND ALL AFFILIATES ASSUME NO RESPONSIBILITY FOR YOUR TRADING RESULTS.**

Always test your strategies in Sandbox Mode before deploying with real money. Past performance does not guarantee future results. Trading involves substantial risk of loss.

---

Built with love by traders, for traders. Part of the [OpenAlgo FOSS Ecosystem](https://docs.openalgo.in/mini-foss-universe).
