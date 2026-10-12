# Frontend to backend contract

Every HTTP route and Socket.IO event the carried-over OpenAlgo web frontend
calls, grouped by page, with the web file and line that serves it. The Rust
server must implement each one with the web's request and response shapes.
Generated on 2026-10-03 from the frontend as merged at c1ce921. Line numbers
refer to the OpenAlgo web repository at that date.

Format: `path` METHOD :line (web file).

The raw market data WebSocket is separate: the market data manager,
`useMarketData`, the WebSocket test pages and the Playground connect to the
`websocket_url` from `/api/websocket/config` and speak the protocol recorded in
`tests/fixtures/web/websocket/`.

The Python Strategy Host and the Agent are not part of this version
(maintainer, 2026-10-12), so their pages and routes (`/python/*`,
`/agent/api/*`) are not listed.

## Shared shell

Used by main, App, layouts, Navbar, providers, AuthSync, SocketProvider,
useSocket, MarketDataManager, errorReporter and Footer.

- `/auth/csrf-token` GET :110 (blueprints/auth.py)
- `/auth/session-status` GET :1013 (blueprints/auth.py)
- `/auth/logout` POST :1287 (blueprints/auth.py)
- `/auth/analyzer-mode` GET :1120 (blueprints/auth.py)
- `/auth/analyzer-toggle` POST :1146 (blueprints/auth.py)
- `/auth/app-info` GET :1112 (blueprints/auth.py)
- `/admin/api/errors/client` POST :928 (blueprints/admin.py)
- `/api/broker/capabilities` GET :312 (blueprints/broker_credentials.py)
- `/api/websocket/apikey` GET :165 (blueprints/websocket_example.py)
- `/api/websocket/config` GET :186 (blueprints/websocket_example.py)
- `/api/v1/multiquotes` POST :23 (restx_api/multiquotes.py), polling fallback
- `/socket.io` connection

## Socket.IO events

| Event | Direction | Web emitter | Used by |
| --- | --- | --- | --- |
| `order_event` | listen | subscribers/socketio_subscriber.py:21 (also :137, :158, :179, :200) | shell, order refresh hook |
| `order_notification` | listen | subscribers/socketio_subscriber.py:46 | shell |
| `modify_order_event` | listen | subscribers/socketio_subscriber.py:61 | shell, order refresh hook |
| `cancel_order_event` | listen | subscribers/socketio_subscriber.py:82, :103 | shell, order refresh hook |
| `close_position_event` | listen | subscribers/socketio_subscriber.py:122 | shell, order refresh hook |
| `analyzer_update` | listen | subscribers/socketio_subscriber.py:273; services/orderstatus_service.py:27; services/openposition_service.py:26 | shell, order refresh hook |
| `order_update` | listen | subscribers/socketio_subscriber.py:248; websocket_proxy/server.py:1874 | /trading blotter |
| `force_logout` | listen | blueprints/auth.py:728, :832, :1336, :1463; utils/session.py:276 | shell |
| `active_sessions_update` | listen | blueprints/auth.py:1341; utils/session.py:285; utils/auth_utils.py:496 | shell |
| `master_contract_download` | listen | broker/*/database/master_contract_db.py (zerodha :443) | shell |
| `password_change` | listen | no web emitter (dead listener) | shell |
| `pending_order_created` | listen | services/order_router_service.py:124 | Action Center |
| `pending_order_updated` | listen | blueprints/orders.py:1063, :1072, :1115, :1139, :1219 | Action Center |
| `historify_progress`, `historify_job_complete`, `historify_job_paused`, `historify_job_cancelled` | listen | services/historify_service.py:1823, :1842, :1861, :1873 | Historify |
| `historify_schedule_execution_complete` | listen | services/historify_service.py:1797 | Historify |
| `historify_schedule_created`, `_updated`, `_deleted`, `_execution_started` | listen | services/historify_scheduler_service.py:294, :326, :350, :627 | Historify |
| `strategy_snapshot`, `strategy_delta`, `strategy_event`, `strategy_order_update`, `strategy_run_update`, `strategy_terminal` | listen | services/strategy_module/broadcast.py:122-127 | strategy pages |
| `strategy_subscribe`, `strategy_unsubscribe` | client emit | blueprints/strategy_module.py:1917, :1946 | strategy pages |
| `whatsapp_qr`, `whatsapp_pair_code`, `whatsapp_paired`, `whatsapp_pair_status`, `whatsapp_status` | listen | services/whatsapp_bot_service.py:602, :608, :725, :640, :910 | WhatsApp |
| `scalping_sl_update` | listen | services/scalping_risk_monitor_service.py:630 | Scalping |

The order refresh hook (`useOrderEventRefresh`) is used by Dashboard,
Positions, OrderBook, TradeBook, Holdings, Scalping and /trading; each chooses
which order events it listens to.

## Shared modules

**api/auth.ts** (Login, ResetPassword, Profile)
- `/auth/login` POST :276; `/auth/reset-password` POST :578; `/auth/change-password` POST :1422 (blueprints/auth.py)

**api/trading.ts** (Positions, OrderBook, TradeBook, Holdings, OptionChain, Scalping, StrategyBuilder, Arbitrage, Trading)
- `/api/v1/` placeorder :22, basketorder :26, quotes :23, multiquotes :23, depth :23, funds :24, orderbook :23, tradebook :23, positionbook :23, holdings :23, gttorderbook :19, all POST (restx_api/*)
- `/close_position` :504, `/close_all_positions` :650, `/cancel_all_orders` :696, `/cancel_order` :752, `/modify_gtt_order` :795, `/cancel_gtt_order` :853, `/modify_order` :890, all POST (blueprints/orders.py)

**hooks/useMarketStatus.ts** (Positions, Holdings, OptionChain, Trading)
- `/admin/api/holidays` GET :312; `/admin/api/timings` GET :476 (blueprints/admin.py)

**hooks/useOptionChainPolling.ts, api/option-chain.ts** (OptionChain, StrategyBuilder, Trading, strategy pages)
- `/api/v1/optionchain` POST :83 (restx_api/option_chain.py); `/api/v1/expiry` POST :23 (restx_api/expiry.py)

**api/admin.ts** (all blueprints/admin.py)
- `/admin/api/stats` GET :110
- `/admin/api/freeze` GET :131, POST :157; `/admin/api/freeze/<id>` PUT :203, DELETE :241; `/admin/api/freeze/upload` POST :263
- `/admin/api/holidays` GET :312, POST :369; `/admin/api/holidays/<id>` DELETE :448
- `/admin/api/timings` GET :476; `/admin/api/timings/<exchange>` PUT :517; `/admin/api/timings/check` POST :556
- `/admin/api/errors` GET :787; `/admin/api/errors/stats` GET :989; `/admin/api/errors/groups` GET :1086
- `/admin/api/system` GET :1690; `/admin/api/system/diagnostics` POST :1873; `/admin/api/system/report` GET :2081
- `/admin/api/oauth/clients` GET :2198; `/admin/api/oauth/clients/<client_id>/approve` POST :2242; `.../revoke` POST :2282
- `/admin/api/mcp/audit` GET :2337; `/admin/api/mcp/kill-switch` POST :2437; `/admin/api/mcp/settings` GET :2574, PUT :2586

**api/scalping.ts** (all blueprints/scalping.py, prefixed `/scalping/api/`)
- underlyings GET :134; history GET :149; all_underlyings GET :268; expiry GET :300; strikes GET :433; search GET :488; futures GET :511
- order POST :630; close_leg POST :799; close_all POST :838; cancel_all POST :902; tracked GET :919, DELETE :928; sl GET :937, POST :946, DELETE :1005

**api/oi-profile.ts, api/oi-tracker.ts**
- `/oiprofile/api/profile-data` POST :29; `/oiprofile/api/intervals` GET :98 (blueprints/oiprofile.py)
- `/oitracker/api/oi-data` POST :25; `/oitracker/api/maxpain` POST :77 (blueprints/oitracker.py)
- `/search/api/expiries` GET :227; `/search/api/underlyings` GET :248 (blueprints/search.py)

**api/strategy-portfolio.ts** (blueprints/strategy_portfolio.py)
- `/api/strategy-portfolio` GET :48, POST :73; `/api/strategy-portfolio/<id>` GET :63, PUT :95, DELETE :118

**api/strategy_module.ts** (blueprints/strategy_module.py, prefixed `/strategy/api/strategies`)
- GET :1193, POST :1213; `/<sid>` GET :1262, PATCH :1273, DELETE :1349
- `/<sid>/webhook/rotate` :1374; `/live` :1400; `/kill_switch` :1434; `/start` :1511; `/stop` :1545; `/close_all` :1553; `/legs/<leg_id>/close` :1606; `/unlock_webhook` :1640 (all POST)
- `/<sid>/runs` :1668; `/orders` :1679; `/events` :1694; `/webhook_events` :1727; `/orderbook` :1771; `/tradebook` :1787; `/positions` :1797; `/checkpoints` :1813 (all GET)
- plus `/api/v1/search`, `/api/v1/symbol`, `/api/v1/optionsymbol` POST

**api/chartink.ts** (blueprints/chartink.py)
- `/chartink/<id>/delete` POST :621; `/chartink/<id>/configure` POST :654; `/chartink/<id>/symbol/<mid>/delete` POST :773; `/chartink/search` GET :822
- `/chartink/api/strategies` GET :848; `/chartink/api/strategy/<id>` GET :878; `/chartink/api/strategy` POST :924; `/chartink/api/strategy/<id>/toggle` POST :992

**lib/trading/customIndicators.ts, lib/trading/openscriptFiles.ts**
- `/custom-indicators/index.json` GET :47; `/custom-indicators/<path>` GET :78, loaded as an ES module (blueprints/custom_indicators.py)
- `/openscript/index.json` GET :235; `/openscript/<filename>` GET :370, POST :389, DELETE :599 (blueprints/openscript.py)

## Pages

Calls beyond the shell and shared modules.

- **/, /faq, /download, /error, /rate-limited, 404, /platforms, /tools, /logs**: none (`/error` posts `/auth/logout`).
- **/setup**: `/setup` POST :19 (blueprints/core.py)
- **/login**: `/auth/check-setup` GET :156; `/auth/login/totp` POST :401 (blueprints/auth.py)
- **/broker**: `/auth/broker-config` GET :117 (auth.py); `/dhan/initiate-oauth` GET :1078 and `/<broker>/callback` GET :37 (brlogin.py). Desktop: the page calls `/<broker>/initiate-oauth`.
- **/broker/:broker/totp, /:broker/auth**: `/<broker>/callback` GET/POST :37 (brlogin.py)
- **/broker/samco/auth**: `/<broker>/callback` POST :37; `/samco/ip-status` GET :1159 (brlogin.py)
- **/dashboard**: `/auth/dashboard-data` GET :1184 (auth.py); `/api/master-contract/status` GET :33 (master_contract_status.py)
- **/search/token**: `/search/api/search` GET :133, expiries :227, underlyings :248 (search.py)
- **/search**: `/search/api/search` GET :133
- **/apikey**: `/apikey` GET/POST :45; `/apikey/mode` POST :103 (apikey.py)
- **/profile**: `/auth/profile-data` GET :1357; `/auth/2fa/status` GET :476; `/auth/2fa/configure` POST :495; `/auth/smtp-config` POST :848; `/auth/test-smtp` POST :913; `/auth/debug-smtp` POST :951 (auth.py); `/api/broker/credentials` GET :70, POST :132 (broker_credentials.py); `/api/system/permissions` GET :238; `/api/system/permissions/fix` POST :274 (system_permissions.py)
- **/master-contract**: `/api/cache/health` GET :96; `/api/cache/reload` POST :125; `/api/master-contract/download` POST :185; `/api/master-contract/smart-status` GET :233 (master_contract_status.py)
- **/action-center**: `/action-center/approve/<id>` POST :1021; `/action-center/reject/<id>` POST :1098; `/action-center/delete/<id>` DELETE :1124; `/action-center/approve-all` POST :1161; `/action-center/api/data` GET :1262 (orders.py)
- **/tradingview, /gocharting**: `/api/config/host` GET :680 (app.py); `/playground/api-key` GET :299 (playground.py); `/search/api/search` GET :133
- **/pnl-tracker**: `/pnltracker/api/pnl` POST :206 (pnltracker.py)
- **/sandbox**: `/sandbox/api/configs` GET :104; `/sandbox/update` POST :220; `/sandbox/reset` POST :315 (sandbox.py)
- **/sandbox/mypnl**: `/sandbox/mypnl/api/data` GET :550; `/sandbox/mypnl/export/{daily,positions,holdings,trades}` GET :1150, :1184, :1216, :1248 (sandbox.py)
- **/analyzer, /logs/sandbox**: `/analyzer/api/data` GET :224; `/analyzer/export` GET :334 (analyzer.py)
- **/websocket/test(/20|30|50), /websocket/order**: `/search/api/search` GET :133, plus the raw feed
- **/chart/test**: `/chart/test/api/history` GET :47 (chart_test.py)
- **/playground**: `/api/config/host` GET :680; `/playground/api-key` GET :299; `/playground/endpoints` GET :336 (playground.py); then any `/api/v1/*`
- **/trading**:
  - `/api/v1/` history, intervals, market/holidays, search, symbol, quotes, positionbook (POST)
  - `/api/v1/telegram/notify` POST :465; `/api/v1/whatsapp/notify` POST :106
  - `/alerts/fired` POST :52; `/alerts/log` GET :88, DELETE :99 (alerts.py)
  - `/openscript/instrument` GET :277 (openscript.py)
  - `/openscript/runner/` start/<f> POST :696; pause/<f> :792; close/<f> :831; status GET :862; config/<f> POST :966, DELETE :1064; schedule/<f> POST :1139, DELETE :1276; instruments GET :1364; intervals GET :1396; orderbook|tradebook|positions/<f> GET :1484, :1493, :1502 (openscript_runner.py)
  - `/watchlist/api/lists` GET :65, POST :72; `/watchlist/api/lists/<id>` PATCH :98, DELETE :114; `.../clear` POST :123; `.../items` POST :132; `.../items/<iid>` DELETE :153; `.../items/order` PUT :164 (watchlist.py)
  - `/strategybuilder/api/*` and `/historify/api/*` as listed under those pages
- **/historify** (historify.py, prefixed `/historify/api/`): watchlist GET :28, POST :42, DELETE :61; watchlist/bulk/delete :79; watchlist/bulk :99; catalog GET :237; export/bulk POST :411; export/bulk/download GET :564; intervals :625; historify-intervals :652; exchanges :666; stats :680; delete DELETE :694; delete/bulk POST :713; upload POST :741; sample/<format> :820; fno/underlyings :875; fno/expiries :891; fno/chain :914; jobs GET :1001, POST :1018; jobs/<id>/{cancel,pause,resume,retry} POST :1080, :1094, :1108, :1122; jobs/<id> DELETE :1149; schedules GET :1225, POST :1248; schedules/<id> GET :1342, PUT :1368, DELETE :1433; schedules/<id>/{enable,disable,pause,resume,trigger} POST :1453, :1473, :1493, :1513, :1533; schedules/<id>/executions GET :1553; plus `/search/api/search`
- **/historify/charts**: `/historify/api/data` GET :211; catalog :237; historify-intervals :652; catalog/metadata :1186
- **/optionchain, /oitracker, /oirange, /maxpain, /oiprofile, /scalping**: shared modules only
- **/ivchart**: `/ivchart/api/iv-data` POST :20; default-symbols POST :69; intervals GET :109 (ivchart.py)
- **/gammadensity**: `/gammadensity/api/gamma-data` POST :28 (gamma_density.py)
- **/straddle**: `/straddle/api/straddle-data` POST :20; intervals GET :69 (straddle_chart.py)
- **/straddlepnl**: `/straddlepnl/api/simulate` POST :21; lotsize GET :83; intervals GET :116 (custom_straddle.py)
- **/volsurface**: `/volsurface/api/surface-data` POST :19 (vol_surface.py)
- **/gex**: `/gex/api/gex-data` POST :24 (gex.py)
- **/ivsmile**: `/ivsmile/api/iv-smile-data` POST :24 (ivsmile.py)
- **/arbitrage**: `/arbitrage/api/universe` GET :24 (arbitrage.py)
- **/strategybuilder**: `/api/v1/margin` POST; `/strategybuilder/api/strategy-chart` POST :32; multi-strike-oi POST :107; intervals GET :182 (strategy_chart.py)
- **/strategybuilder/portfolio**: `/api/v1/multiquotes` plus strategy-portfolio module
- **/strategy pages**: shared modules only
- **/chartink pages**: `/api/config/host` GET :680 plus chartink module
- **/leverage**: `/leverage/api/current` GET :16; `/leverage/api/update` POST :26 (leverage.py)
- **/admin/***: api/admin.ts only
- **/telegram**: bot/start POST :90; bot/stop :121; broadcast :160; test-message :221; api/index GET :348; config POST :54; api/config GET :394; user/<id>/unlink POST :204; send-message :271; api/users GET :420; api/analytics GET :445 (telegram.py, prefixed `/telegram/`)
- **/whatsapp** (whatsapp.py, prefixed `/whatsapp/`): config GET :66, POST :89; pair POST :116; pair/status GET :162; unlink :169; bot/start :183; bot/stop :198; bot/status GET :207; users GET :237; user/<jid>/unlink :248; broadcast :270; test-message :299; send :344; stats GET :390
- **/logs/live**: `/logs/` GET :210; `/logs/export` GET :258 (log.py)
- **/logs/security** (security.py, prefixed `/security/`): ban POST :119; unban :161; ban-host :186; clear-404 :262; api/data GET :289; api/stats :375; api/settings POST :409; api/login-activity GET :475; api/login-activity/clear POST :492; api/active-sessions GET :507
- **/logs/traffic**: `/traffic/api/logs` GET :97; api/stats :125; export :191 (traffic.py)
- **/logs/latency**: `/latency/api/logs` GET :175; api/stats :208; export :247 (latency.py)
- **/health**: `/health/check` GET :102; api/current :256; api/history :309; api/stats :337; api/alerts :351; api/alerts/<id>/acknowledge POST :380; .../resolve POST :395; export GET :415 (health.py)
- **/settings/server** (desktop only): `/settings/api/server` GET returns `{status, data:{http_host, http_port, ws_host, ws_port, lan_enabled}}`; POST takes the same fields and returns `{status, message, data?}`; a refusal message is shown to the trader as-is.

Calls with no web route behind them: `authApi.getSession`, `getBrokers`,
`initiateBrokerAuth` (`/auth/session`, `/auth/brokers`, `/auth/broker/<b>`) and
`getSimpleHealth` (GET `/health`) exist in the frontend code but nothing calls
them.
