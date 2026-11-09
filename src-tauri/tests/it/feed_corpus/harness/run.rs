//! A case's steps, in order, against its world: client messages to the
//! feed server, frames pushed from the fake broker, and every frame either
//! side receives compared exactly. At the end nothing may be left unread
//! on either side, and every subscription must be released once the
//! clients leave. A client frame that is not the expected one does not
//! stop the case: it fails at the end with all of them, and every frame a
//! client received is on the test's output (`CORPUS-RECORD`), so one run
//! is enough to pin a case again.

use super::case::{bad, Action, Case, Expect, Socket, Step};
use super::compare::{self, Receipts, Window};
use super::http::{self, HttpRequestExpect};
use super::invariants;
use super::json_text;
use super::upstream::{until, Received};
use super::web;
use super::world::{Env, Feeds, World};
use crate::feed_support::Client;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::Message;

/// How long a client waits for a frame it expects.
const RECV: Duration = Duration::from_secs(5);
/// How long the end of a case waits for frames nobody expected.
const SETTLE: Duration = Duration::from_millis(200);
/// How many frames nobody expected the end of a case reports per client.
const LEFTOVER_FRAMES: usize = 20;
/// Room for the wall clock either side of a receipt-time window.
const CLOCK_SLACK_MS: i64 = 5;

/// Run case `<broker>/<name>` with the feeds `feeds` builds, then check it
/// against the web's output when the case has one.
pub(crate) async fn run(broker: &str, name: &str, feeds: fn(&Env) -> Feeds) {
    let case = Case::load(broker, name);
    let world = World::start(&case, feeds).await;
    let mut r = Runner {
        case: &case,
        world: &world,
        clients: BTreeMap::new(),
        pushes: Vec::new(),
        invariants_applied: vec![0; case.invariants.len()],
        mismatches: Vec::new(),
    };
    for step in &case.steps {
        r.step(step).await;
    }
    r.finish().await;
    drop(r);
    world.stop(&case.id).await;
    web::check(&case);
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The client's next text frame, its numbers read exactly (`json_text`);
/// `None` once the socket closes or nothing comes within `d`.
async fn next_frame(c: &mut Client, d: Duration) -> Option<Value> {
    loop {
        let msg = tokio::time::timeout(d, c.ws.next()).await.ok()??.ok()?;
        match msg {
            Message::Text(t) => {
                return Some(json_text::parse(&t).unwrap_or_else(|e| {
                    panic!(
                        "the feed server sent a frame that is not JSON ({}): {}",
                        e, t
                    )
                }))
            }
            Message::Close(_) => return None,
            _ => continue,
        }
    }
}

/// Requests, one per line, for a failure message.
fn listed(items: &[impl std::fmt::Display]) -> String {
    if items.is_empty() {
        return "nothing".into();
    }
    items.iter().map(|r| format!("\n    {}", r)).collect()
}

fn list(frames: &[Received]) -> String {
    if frames.is_empty() {
        return "nothing".into();
    }
    frames
        .iter()
        .map(|r| format!("\n    connection {}: {}", r.connection, r.wire))
        .collect()
}

struct Runner<'a> {
    case: &'a Case,
    world: &'a World,
    clients: BTreeMap<String, Client>,
    /// Each push so far (or `http_serve`): its script line, and the wall
    /// clock just before it.
    pushes: Vec<(usize, i64)>,
    /// How many received frames each of the case's invariants applied to.
    invariants_applied: Vec<usize>,
    /// Received frames that were not the expected ones, by step.
    mismatches: Vec<String>,
}

impl Runner<'_> {
    async fn step(&mut self, s: &Step) {
        let at = format!("{} script.jsonl:{}", self.case.id, s.line);
        match &s.action {
            Action::Send {
                client,
                message,
                reply,
            } => self.send(&at, client, message, reply.as_ref()).await,
            Action::CloseClient { client } => {
                let mut c = self.take(&at, client);
                let _ = c.ws.close(None).await;
            }
            Action::Receive { client, frame, .. } => self.receive(&at, client, frame).await,
            Action::Silence { client, ms, .. } => {
                let c = self.known(&at, client);
                if let Some(v) = next_frame(c, Duration::from_millis(*ms)).await {
                    self.record(&at, &v);
                    self.mismatch(&at, format!("client {} received {}", client, v));
                }
            }
            Action::Push {
                socket,
                wire,
                connection,
            } => {
                self.pushes.push((s.line, now_ms()));
                if let Err(e) = self.world.upstream(*socket).push(wire, *connection).await {
                    bad(&at, format!("{} socket: {}", socket.name(), e));
                }
            }
            Action::UpstreamReceives {
                socket,
                frames,
                any_order,
                connection,
            } => {
                self.upstream_receives(&at, *socket, frames, *any_order, *connection)
                    .await
            }
            Action::UpstreamConnected {
                socket,
                connection,
                path,
                headers,
            } => {
                let hs = self
                    .world
                    .upstream(*socket)
                    .connection(*connection)
                    .await
                    .unwrap_or_else(|| {
                        bad(
                            &at,
                            format!(
                                "the {} feed never opened connection {}",
                                socket.name(),
                                connection
                            ),
                        )
                    });
                if let Some(p) = path {
                    if hs.path != *p {
                        bad(
                            &at,
                            format!("the feed asked for {}, expected {}", hs.path, p),
                        );
                    }
                }
                for (k, v) in headers {
                    let got = hs
                        .headers
                        .iter()
                        .find(|(name, _)| name == k)
                        .map(|(_, v)| v);
                    if got != Some(v) {
                        bad(&at, format!("header {}: {:?}, expected {}", k, got, v));
                    }
                }
            }
            Action::DropUpstream { socket, close } => {
                let up = self.world.upstream(*socket);
                if let Err(e) = up.drop_live(close.clone()).await {
                    bad(&at, format!("{} socket: {}", socket.name(), e));
                }
            }
            Action::Disconnect { socket } => self.world.disconnect(*socket).await,
            Action::RefuseUpgrades {
                socket,
                status,
                times,
            } => self.world.upstream(*socket).refuse(*status, *times),
            Action::UpstreamConnections {
                socket,
                count,
                refused,
                ms,
            } => {
                self.upstream_connections(&at, *socket, *count, *refused, *ms)
                    .await
            }
            Action::HttpServe(answer) => {
                // An order book the poller reads next is like a push: its
                // update's receipt window starts here.
                self.pushes.push((s.line, now_ms()));
                self.world.serve(answer);
            }
            Action::HttpPolled {
                method,
                path,
                times,
            } => {
                if !self
                    .world
                    .http()
                    .polled(method.as_deref(), path, *times)
                    .await
                {
                    bad(
                        &at,
                        format!("{} was not read {} times by the deadline", path, times),
                    );
                }
            }
            Action::HttpReceives { requests } => self.http_receives(&at, requests).await,
            Action::Note => {}
        }
    }

    /// The socket reaches exactly `count` connections (and `refused`
    /// refused upgrades) and stays there for `ms`: a feed that reconnects
    /// after a drop or a 429, or one that stops after a refused session.
    async fn upstream_connections(
        &self,
        at: &str,
        socket: Socket,
        count: usize,
        refused: Option<usize>,
        ms: u64,
    ) {
        let up = self.world.upstream(socket);
        let wanted = |(c, r): (usize, usize)| c == count && refused.is_none_or(|want| want == r);
        let reached = until(|| wanted(up.counts()).then_some(())).await.is_some();
        tokio::time::sleep(Duration::from_millis(ms)).await;
        let now = up.counts();
        if !reached || !wanted(now) {
            bad(
                at,
                format!(
                    "the {} socket has had {} connections and {} refused upgrades; expected {}{}",
                    socket.name(),
                    now.0,
                    now.1,
                    count,
                    refused.map_or_else(String::new, |r| format!(" and {} refused", r))
                ),
            );
        }
    }

    async fn http_receives(&self, at: &str, requests: &[HttpRequestExpect]) {
        let got = self
            .world
            .http()
            .take(requests.len())
            .await
            .unwrap_or_else(|got| {
                bad(
                    at,
                    format!(
                        "the broker API received {} of {} requests:{}",
                        got.len(),
                        requests.len(),
                        listed(&got)
                    ),
                )
            });
        if !requests.iter().zip(&got).all(|(w, g)| http::matches(w, g)) {
            bad(
                at,
                format!(
                    "the broker API expected:{}\n  received:{}",
                    listed(requests),
                    listed(&got)
                ),
            );
        }
    }

    /// The client `name`, connected and authenticated on first use.
    async fn client(&mut self, name: &str) -> &mut Client {
        if !self.clients.contains_key(name) {
            let mut c = Client::connect(&self.world.feed_url).await;
            c.auth().await;
            self.clients.insert(name.to_string(), c);
        }
        self.clients
            .get_mut(name)
            .unwrap_or_else(|| unreachable!("inserted above"))
    }

    /// A client an earlier step has already used.
    fn known(&mut self, at: &str, name: &str) -> &mut Client {
        self.clients
            .get_mut(name)
            .unwrap_or_else(|| bad(at, format!("client {} has sent nothing yet", name)))
    }

    fn take(&mut self, at: &str, name: &str) -> Client {
        self.clients
            .remove(name)
            .unwrap_or_else(|| bad(at, format!("client {} has sent nothing yet", name)))
    }

    async fn send(&mut self, at: &str, name: &str, message: &Value, reply: Option<&Value>) {
        let c = self.client(name).await;
        c.send(message).await;
        let got = next_frame(c, RECV)
            .await
            .unwrap_or_else(|| bad(at, format!("client {} got no reply", name)));
        match reply {
            Some(want) => {
                if let Err(e) = compare::exact(want, &got, None) {
                    bad(at, e);
                }
            }
            None if got["status"] == "success" => {}
            None => bad(at, format!("the reply is not a success: {}", got)),
        }
    }

    async fn receive(&mut self, at: &str, name: &str, want: &Value) {
        let c = self.known(at, name);
        let Some(got) = next_frame(c, RECV).await else {
            return self.mismatch(at, format!("client {} received nothing", name));
        };
        self.record(at, &got);
        let times = self.receipts(now_ms());
        if let Err(e) = compare::exact(want, &got, Some(&times)) {
            self.mismatch(at, e);
        }
        if let Some(frame) = got.as_object().filter(|f| f["type"] == "market_data") {
            let broken =
                invariants::broken(&self.case.invariants, frame, &mut self.invariants_applied);
            if !broken.is_empty() {
                self.mismatch(
                    at,
                    format!(
                        "the frame breaks invariants:\n  {}\n  received {}",
                        broken.join("\n  "),
                        got
                    ),
                );
            }
        }
    }

    /// A frame a client received, on the test's output as an expected
    /// frame would write it (`compare::as_expected`), so a case whose
    /// frames changed shape is pinned again from one run: nextest shows
    /// the output of a failed test.
    fn record(&self, at: &str, got: &Value) {
        let times = self.receipts(now_ms());
        eprintln!("CORPUS-RECORD {} {}", at, compare::as_expected(got, &times));
    }

    /// A received frame that is not the expected one, or a frame where
    /// none was expected: reported at once, the case goes on, and it fails
    /// at the end with every one of them, so one run shows every frame.
    fn mismatch(&mut self, at: &str, msg: impl std::fmt::Display) {
        let msg = format!("{}: {}", at, msg);
        eprintln!("CORPUS-MISMATCH {}", msg);
        self.mismatches.push(msg);
    }

    /// When each push's packets can have been stamped, as of `now`: from
    /// just before the push to just before the next one (the latest's, to
    /// now; before any push, any time up to now).
    fn receipts(&self, now: i64) -> Receipts {
        let window = |i: usize| Window {
            from_ms: self.pushes[i].1 - CLOCK_SLACK_MS,
            to_ms: self.pushes.get(i + 1).map_or(now, |next| next.1) + CLOCK_SLACK_MS,
        };
        let latest = self.pushes.len().checked_sub(1).map_or(
            Window {
                from_ms: -CLOCK_SLACK_MS,
                to_ms: now + CLOCK_SLACK_MS,
            },
            window,
        );
        let by_push = (0..self.pushes.len())
            .map(|i| (self.pushes[i].0, window(i)))
            .collect();
        Receipts { latest, by_push }
    }

    async fn upstream_receives(
        &self,
        at: &str,
        socket: Socket,
        frames: &[Expect],
        any_order: bool,
        connection: Option<usize>,
    ) {
        let up = self.world.upstream(socket);
        let got = up.take(frames.len()).await.unwrap_or_else(|got| {
            bad(
                at,
                format!(
                    "the {} socket received {} of {} frames:{}",
                    socket.name(),
                    got.len(),
                    frames.len(),
                    list(&got)
                ),
            )
        });
        if let Some(n) = connection {
            if got.iter().any(|r| r.connection != n) {
                bad(at, format!("expected on connection {}:{}", n, list(&got)));
            }
        }
        let matched = if any_order {
            let mut free: Vec<&Received> = got.iter().collect();
            frames
                .iter()
                .all(|e| match free.iter().position(|r| e.matches(&r.wire)) {
                    Some(i) => {
                        free.remove(i);
                        true
                    }
                    None => false,
                })
        } else {
            frames.iter().zip(&got).all(|(e, r)| e.matches(&r.wire))
        };
        if !matched {
            let want: String = frames.iter().map(|e| format!("\n    {}", e)).collect();
            bad(
                at,
                format!(
                    "the {} socket expected:{}\n  received:{}",
                    socket.name(),
                    want,
                    list(&got)
                ),
            );
        }
    }

    /// Nothing unread on either side, then every client leaves.
    async fn finish(&mut self) {
        let id = self.case.id.clone();
        let mut unexpected = Vec::new();
        for (name, c) in self.clients.iter_mut() {
            // Every frame left, up to a bound: a synthetic feed never stops.
            for _ in 0..LEFTOVER_FRAMES {
                let Some(v) = next_frame(c, SETTLE).await else {
                    break;
                };
                unexpected.push((name.clone(), v));
            }
        }
        for (name, v) in unexpected {
            let at = format!("{} (after the last step, client {})", id, name);
            self.record(&at, &v);
            self.mismatch(&at, format!("a frame no step expects: {}", v));
        }
        if !self.mismatches.is_empty() {
            bad(
                &id,
                format!(
                    "{} frames are not the expected ones (CORPUS-RECORD lines above give every frame received):\n  {}",
                    self.mismatches.len(),
                    self.mismatches.join("\n  ")
                ),
            );
        }
        for s in self.world.sockets().collect::<Vec<_>>() {
            let left = self.world.upstream(s).leftover();
            if !left.is_empty() {
                bad(
                    &id,
                    format!(
                        "the {} socket received frames no step expects:{}",
                        s.name(),
                        list(&left)
                    ),
                );
            }
        }
        let left = self.world.http().leftover();
        if !left.is_empty() {
            bad(
                &id,
                format!(
                    "the broker API received requests no step expects:{}",
                    listed(&left)
                ),
            );
        }
        for (_, mut c) in std::mem::take(&mut self.clients) {
            let _ = c.ws.close(None).await;
        }
        let idle: Vec<&str> = self
            .case
            .invariants
            .iter()
            .zip(&self.invariants_applied)
            .filter(|(_, n)| **n == 0)
            .map(|(inv, _)| inv.text.as_str())
            .collect();
        if !idle.is_empty() {
            bad(
                &id,
                format!("invariants no received frame has the key of: {:?}", idle),
            );
        }
    }
}
