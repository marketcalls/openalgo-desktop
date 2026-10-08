# Soak runs

Measured results of the leak soak tests. Both are `#[ignore]`d in the normal
suite and run weekly (and on demand) by `.github/workflows/soak.yml` on
ubuntu. Run locally with:

```
cd src-tauri
cargo test --test it soak -- --ignored --nocapture
```

## Broker session soak (`tests/it/soak.rs`)

One cycle, on the real `AppState`, HTTP server with Socket.IO, and 8765 feed
server, ephemeral ports only, `MockBroker` registered as `zerodha` and a fake
broker market socket:

1. Broker sign-in (OAuth callback with the server-issued `state`).
2. Master contract load (4 symbols).
3. Broker feed connected; an 8765 client authenticates, subscribes to SBIN
   and receives streaming ticks (one every 25 ms).
4. One live order (mock broker) and one sandbox order through
   `/api/v1/placeorder`.
5. The broker socket is dropped without a close frame; the feed reconnects,
   resubscribes and ticks resume.
6. 110 `/api/v1` requests over 14 endpoints (ping, funds, orderbook,
   tradebook, positionbook, holdings, quotes, depth, symbol, search,
   analyzer, intervals, multiquotes, openposition), every tenth on a fresh
   connection, paced under the 100 per second limit.
7. Two signed-in Socket.IO clients connect and leave; one without a session
   is refused.
8. The 8765 client leaves (unsubscribe and close frame, or a dropped socket,
   alternating).
9. Logout: session tasks, feed, registries, symbol cache and Socket.IO
   sockets all back to zero.

Samples are taken after 10 warm-up cycles and every 10 cycles after that.
Assertions: descriptors at the end at most the warm-up count plus two per
database pool connection added since (r2d2 refills `min_idle`; each holds the
database and its WAL) plus 4; live tokio tasks
(`RuntimeMetrics::num_alive_tasks`) at most the warm-up count plus 4; memory
growth at most 48 MiB, and the second half of the run growing no more than
half the first half plus 4 MiB (a plateau passes, a line fails). Memory is
the resident set on Linux and Windows and the physical footprint on macOS
(`proc_pid_rusage`, resident plus compressed): macOS compresses idle pages,
so its resident set alone falls and rises with no change in use, and an
earlier run asserting on it failed on that noise.

### 2026-10-08, macOS 14 (Apple Silicon), debug build

150 measured cycles after 10 warm-up cycles, 336 s.

| Measure | After warm-up (cycle 10) | End (cycle 160) |
| --- | --- | --- |
| Open descriptors | 37 | 33 |
| Database pool connections | 7 | 3 |
| RSS | 137,200 KiB | 138,048 KiB |
| Physical footprint | 30,626 KiB | 31,442 KiB |
| Live tokio tasks | 20 | 21 |

Footprint growth: 816 KiB in total, 672 KiB in the first half (cycles 10 to
90), 144 KiB in the second (90 to 160): a plateau. Descriptors read 37 at
every sample through cycle 150 and 33 at the end, once r2d2 had reaped four
idle pool connections (two descriptors each). Live tasks read 21 at every
sample from cycle 20 on.

No leak found in the app. Two test-harness defects were found and fixed:

- A broker sign-in reloads the settings, and in a debug build that load
  forces the development ports (5500 and 8766). Tests that set ephemeral
  ports only in memory then had the feed server's settings watcher move the
  listener to 8766, and a test that reloaded before starting its listeners
  bound 5500 and 8766. `AppState::pin_listener_ports` (test-support only) now
  keeps a test's ports through every reload. The soak asserts that it never
  binds 5000, 8765, 5500 or 8766 and that its listeners never move.
  `broker_session_e2e` has the same pin and a check after its sign-in.
- `MockBroker::calls` records every call without a bound. The soak clears
  it each cycle so the test's own bookkeeping does not show up as growth.

## Feed client soak (`tests/it/feed_soak.rs`)

1000 clients in waves of 100 connect, authenticate, subscribe, receive a
tick and leave. Descriptors at most the warm-up count plus 4, RSS growth at
most 16 MiB, and every registry entry and source subscription removed.
