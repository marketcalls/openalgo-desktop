//! A case's steps, in order, against its world: client messages to the
//! feed server, frames pushed from the fake broker, and every frame either
//! side receives compared exactly. At the end nothing may be left unread
//! on either side, and every subscription must be released once the
//! clients leave.

use super::case::{bad, Action, Case, Expect, Socket, Step};
use super::compare::{self, Window};
use super::upstream::Received;
use super::web;
use super::world::{Env, Feeds, World};
use crate::feed_support::Client;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a client waits for a frame it expects.
const RECV: Duration = Duration::from_secs(5);
/// How long the end of a case waits for frames nobody expected.
const SETTLE: Duration = Duration::from_millis(200);
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
        pushed_at: 0,
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
    /// Wall-clock time just before the latest push.
    pushed_at: i64,
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
            Action::Silence { client, ms } => {
                let c = self.known(&at, client);
                if let Some(v) = c.recv_within(Duration::from_millis(*ms)).await {
                    bad(&at, format!("client {} received {}", client, v));
                }
            }
            Action::Push {
                socket,
                wire,
                connection,
            } => {
                self.pushed_at = now_ms();
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
            Action::DropUpstream { socket } => {
                if let Err(e) = self.world.upstream(*socket).drop_live().await {
                    bad(&at, format!("{} socket: {}", socket.name(), e));
                }
            }
            Action::Note => {}
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
        let got = c
            .recv_within(RECV)
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
        let from = self.pushed_at - CLOCK_SLACK_MS;
        let c = self.known(at, name);
        let got = c
            .recv_within(RECV)
            .await
            .unwrap_or_else(|| bad(at, format!("client {} received nothing", name)));
        let window = Window {
            from_ms: from,
            to_ms: now_ms() + CLOCK_SLACK_MS,
        };
        if let Err(e) = compare::exact(want, &got, Some(window)) {
            bad(at, e);
        }
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
        for (name, c) in self.clients.iter_mut() {
            if let Some(v) = c.recv_within(SETTLE).await {
                bad(
                    &id,
                    format!("client {} received a frame no step expects: {}", name, v),
                );
            }
        }
        for s in Socket::ALL {
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
        for (_, mut c) in std::mem::take(&mut self.clients) {
            let _ = c.ws.close(None).await;
        }
    }
}
