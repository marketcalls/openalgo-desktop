//! The supervisor task that owns one broker socket: it prepares and
//! connects, sends the handshake and the registry's subscriptions, reads
//! frames through the broker's `BrokerFeed::parse`, applies the manager's
//! subscription changes, and reconnects with capped backoff after an
//! error, a close or a stall (see the manager's module docs).

use super::heartbeat::{Beat, Heartbeat};
use super::registry::{Delta, Registry};
use super::release::Written;
use super::stats::{DropCause, FeedStats};
use super::{Apply, Command, FeedConfig, FeedStatus};
use crate::brokers::common::ratelimit::backoff_delay;
use crate::brokers::common::redact::{url_safe_error, ws_error_kind};
use crate::brokers::common::streaming::{
    normalize_request, BrokerFeed, Ended, FeedEvent, FeedSubscription, MarketEvent, Message,
    PrepareError, Reconnect, ReleaseAnswer, StopKind,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Error as WsError;

/// Why one socket session ended.
pub(super) enum SessionEnd {
    Stopped,
    /// The connection ended (or was not made) for this cause; the manager
    /// connects again after a backoff.
    Dropped(DropCause),
    /// Dropped, and the broker said how long to wait before the next
    /// connection (`Reconnect::After`, audit P-12).
    DroppedFor(DropCause, Duration),
    AuthFailed(String),
    /// The broker ended the feed until the next sign-in for a reason other
    /// than the login (`Reconnect::Stop`), with the trader's message.
    Halted(StopKind, String),
    /// A write failed. Inside a session only: `session` reads what the
    /// broker sent last (its close code) and ends as the feed judges it.
    WriteFailed,
}

/// The broker's own answers to waited releases: each release's instruments
/// and where its answer comes.
type Later = Vec<(Vec<FeedSubscription>, ReleaseAnswer)>;

/// A session goes on, or ends with the reason (`?` in its steps).
type Sent = std::result::Result<(), SessionEnd>;

const CONNECT_FAILED: SessionEnd = SessionEnd::Dropped(DropCause::ConnectFailed);

/// How long a session whose write failed reads on for the broker's close
/// frame.
const CLOSE_AFTER_WRITE: Duration = Duration::from_millis(500);

/// Longest the feed's farewell frames may take before a deliberate close.
const FAREWELL_WAIT: Duration = Duration::from_secs(1);

/// A socket that ended without a close frame (a read error, the stream's
/// end): no code, no reason.
const CLOSED_UNHEARD: Ended<'static> = Ended::Closed {
    code: None,
    reason: "",
};

/// What a login acknowledgement that did not come in time means.
#[derive(Debug, Clone, Copy)]
enum AckLapse {
    /// It is optional: take the session as accepted.
    Assume,
    /// It is required: the session is lost.
    Refuse,
}

pub(super) struct Supervisor {
    pub(super) feed: Box<dyn BrokerFeed>,
    pub(super) cmd: mpsc::Receiver<Command>,
    pub(super) registry: Arc<Mutex<Registry>>,
    pub(super) events: broadcast::Sender<MarketEvent>,
    pub(super) status: watch::Sender<FeedStatus>,
    pub(super) config: FeedConfig,
    pub(super) stats: Arc<FeedStats>,
    /// Which of the manager's connections this is: it sends the
    /// registry's instruments placed on it, and only the first relays
    /// the broker's order updates (a broker that sends them on every
    /// socket is heard once).
    pub(super) conn: usize,
    /// The registry's change count when this session was last sent its
    /// whole set: a change made before it is already in that set, so it
    /// is not sent again (a subscription made while the login waited for
    /// its acknowledgement reached the broker twice; Kotak, run
    /// 42519352778).
    pub(super) synced: u64,
}

impl Supervisor {
    pub(super) async fn run(mut self) {
        let broker = self.feed.broker().to_string();
        let mut attempt: u32 = 0;
        loop {
            self.status.send_replace(FeedStatus::Connecting {
                broker: broker.clone(),
                attempt,
            });
            let started = Instant::now();
            let end = self.connect_once(&broker).await;
            let asked = match end {
                SessionEnd::Stopped => {
                    self.status.send_replace(FeedStatus::Disconnected);
                    return;
                }
                SessionEnd::AuthFailed(message) => {
                    tracing::warn!(broker = %broker, "Market data feed refused the session");
                    self.status.send_replace(FeedStatus::AuthFailed {
                        broker: broker.clone(),
                        message,
                    });
                    self.hold(false).await;
                    return;
                }
                SessionEnd::Halted(kind, message) => {
                    tracing::warn!(broker = %broker, ?kind, "The broker ended the market data feed");
                    self.status.send_replace(FeedStatus::Halted {
                        broker: broker.clone(),
                        kind,
                        message,
                    });
                    if !self.hold(true).await {
                        return;
                    }
                    // The trader cleared it (closed the other app): a fresh
                    // run, as the web's rebuilt adapter starts one.
                    self.feed.on_resume();
                    attempt = 0;
                    continue;
                }
                SessionEnd::Dropped(cause) => {
                    self.stats.dropped(cause);
                    None
                }
                SessionEnd::WriteFailed => {
                    self.stats.dropped(DropCause::Closed);
                    None
                }
                SessionEnd::DroppedFor(cause, wait) => {
                    self.stats.dropped(cause);
                    Some(wait)
                }
            };
            if started.elapsed() >= self.config.stable_after {
                attempt = 0;
            }
            let delay = asked.unwrap_or_else(|| {
                backoff_delay(attempt, self.config.backoff_base, self.config.backoff_max)
            });
            attempt = attempt.saturating_add(1);
            self.status.send_replace(FeedStatus::Reconnecting {
                broker: broker.clone(),
                attempt,
                delay_ms: delay.as_millis() as u64,
            });
            tracing::info!(broker = %broker, attempt, "Market data feed reconnecting in {:?}", delay);
            // Sleep, still answering commands (registry already holds the
            // state; a stop ends the loop).
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    cmd = self.cmd.recv() => match cmd {
                        None | Some(Command::Stop) => {
                            self.status.send_replace(FeedStatus::Disconnected);
                            return;
                        }
                        // Connect now rather than at the end of the backoff.
                        Some(Command::Reconnect) => break,
                        Some(Command::Apply(_)) => {}
                    }
                }
            }
        }
    }

    /// The broker will not take this session again: connect no more until
    /// a stop (a logout, or the next sign-in replacing this connection),
    /// so a dead token or a full connection limit is not hammered. A stop
    /// the trader can clear (`resumable`: too many connections on one
    /// login) ends at a reconnect, when a feed client signs in or the
    /// computer wakes (`WebSocketManager::resume`, `reconnect`); whether to
    /// connect again.
    async fn hold(&mut self, resumable: bool) -> bool {
        while let Some(cmd) = self.cmd.recv().await {
            match cmd {
                Command::Stop => return false,
                Command::Reconnect if resumable => return true,
                _ => {}
            }
        }
        false
    }

    /// How the session ends once the feed has judged how its socket ended
    /// (audit P-12): `cause` when it asks for the usual backoff.
    fn judged(&mut self, cause: DropCause, ended: Ended<'_>) -> SessionEnd {
        match self.feed.on_ended(ended) {
            Reconnect::Backoff => SessionEnd::Dropped(cause),
            Reconnect::After(wait) => SessionEnd::DroppedFor(cause, wait),
            Reconnect::Stop {
                kind: StopKind::Auth,
                message,
            } => SessionEnd::AuthFailed(message),
            Reconnect::Stop { kind, message } => SessionEnd::Halted(kind, message),
        }
    }

    async fn connect_once(&mut self, broker: &str) -> SessionEnd {
        match tokio::time::timeout(self.config.connect_timeout, self.feed.prepare()).await {
            Ok(Ok(())) => {}
            Ok(Err(PrepareError::AuthFailed(message))) => return SessionEnd::AuthFailed(message),
            Ok(Err(PrepareError::Unavailable)) => return CONNECT_FAILED,
            Err(_) => {
                tracing::debug!(broker, "Market data feed preparation timed out");
                return CONNECT_FAILED;
            }
        }
        let mut retried = false;
        let ws = loop {
            let request = match self.feed.ws_request() {
                Ok(r) => normalize_request(r),
                Err(e) => {
                    tracing::error!(
                        broker,
                        code = e.code(),
                        "Market data feed request could not be built"
                    );
                    return SessionEnd::AuthFailed(e.client_message());
                }
            };
            let connect = tokio_tungstenite::connect_async(request);
            match tokio::time::timeout(self.config.connect_timeout, connect).await {
                Ok(Err(e)) if !matches!(e, WsError::Http(_)) && !retried => {
                    if self.feed.on_connect_failed(&url_safe_error(&e)) {
                        retried = true;
                        continue;
                    }
                    break Ok(Err(e));
                }
                other => break other,
            }
        };
        let ws = match ws {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(WsError::Http(resp))) => {
                let code = resp.status().as_u16();
                tracing::warn!(broker, status = code, "Market data feed handshake refused");
                if self.feed.is_auth_failure(Some(code)) {
                    return SessionEnd::AuthFailed(
                        "The broker refused the live market data session. Log in to your broker again."
                            .into(),
                    );
                }
                return self.judged(DropCause::ConnectFailed, Ended::Refused(code));
            }
            Ok(Err(e)) => {
                tracing::debug!(
                    broker,
                    kind = ws_error_kind(&e),
                    "Market data feed connect failed"
                );
                return CONNECT_FAILED;
            }
            Err(_) => {
                tracing::debug!(broker, "Market data feed connect timed out");
                return CONNECT_FAILED;
            }
        };
        self.stats.opened();
        let (mut write, mut read) = ws.split();
        let end = self.session(broker, &mut write, &mut read).await;
        if matches!(end, SessionEnd::Stopped) {
            self.farewell(&mut write).await;
        }
        // Close before reconnecting so the descriptor is released on every
        // path; bounded so a dead peer cannot hold us.
        let _ = tokio::time::timeout(Duration::from_secs(2), write.close()).await;
        drop(write);
        drop(read);
        end
    }

    /// A deliberate close: the feed's farewell first (Motilal's logout),
    /// bounded, best effort; never after an error or the broker's close.
    async fn farewell<S>(&self, write: &mut S)
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        let frames = self.feed.farewell_frames();
        if frames.is_empty() {
            return;
        }
        let send = async {
            for f in frames {
                write.send(f).await?;
            }
            Ok::<(), WsError>(())
        };
        let _ = tokio::time::timeout(FAREWELL_WAIT, send).await;
    }

    /// Send `frames` in order, all within the write timeout. An error when
    /// the socket failed or did not take them in time (TIME-02): the
    /// session then ends and the socket is replaced.
    async fn send_all<S>(&self, write: &mut S, frames: Vec<Message>) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        if frames.is_empty() {
            return Ok(());
        }
        let send = async {
            for f in frames {
                write.send(f).await?;
            }
            Ok::<(), WsError>(())
        };
        match tokio::time::timeout(self.config.write_timeout, send).await {
            Ok(sent) => sent.map_err(|_| SessionEnd::WriteFailed),
            Err(_) => {
                tracing::warn!(
                    broker = self.feed.broker(),
                    "Market data feed stopped taking frames; reconnecting"
                );
                Err(SessionEnd::Dropped(DropCause::NotTakingFrames))
            }
        }
    }

    /// Sends the full subscription set and reports the feed connected.
    async fn subscribe_all<S>(&mut self, broker: &str, write: &mut S) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        self.resubscribe(write).await?;
        self.stats.mark_connected();
        self.status.send_replace(FeedStatus::Connected {
            broker: broker.to_string(),
        });
        Ok(())
    }

    /// One message from the broker: market events published, replies sent,
    /// and the subscriptions sent once the login is acknowledged. An `Err`
    /// ends the session.
    async fn on_message<S>(
        &mut self,
        broker: &str,
        write: &mut S,
        msg: Message,
        subscribed: &mut bool,
    ) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        if let Message::Close(frame) = &msg {
            let (code, reason) = frame
                .as_ref()
                .map_or((None, ""), |f| (Some(u16::from(f.code)), &*f.reason));
            return Err(self.judged(DropCause::Closed, Ended::Closed { code, reason }));
        }
        for ev in self.feed.parse(&msg) {
            match ev {
                FeedEvent::AuthFailed(m) => return Err(SessionEnd::AuthFailed(m)),
                FeedEvent::AuthOk => {
                    if !*subscribed {
                        self.subscribe_all(broker, write).await?;
                        *subscribed = true;
                    }
                }
                FeedEvent::Heartbeat => {}
                FeedEvent::Reply(m) => self.send_all(write, vec![m]).await?,
                FeedEvent::OrderUpdate(_) if self.conn > 0 => self.stats.mark_event(),
                e @ (FeedEvent::Tick(_) | FeedEvent::Depth(_) | FeedEvent::OrderUpdate(_)) => {
                    self.stats.mark_event();
                    // No receiver yet is normal; nothing is retained.
                    let _ = self.events.send(Arc::new(e));
                }
            }
        }
        Ok(())
    }

    /// Takes the commands already waiting into `batch`, so a burst of
    /// subscribe calls reaches the broker as batches. The end of the session
    /// when a stop or a reconnect was among them (the changes before it are
    /// in the registry, which the next socket is sent whole).
    fn coalesce(&mut self, batch: &mut Vec<Apply>) -> Option<SessionEnd> {
        while let Ok(more) = self.cmd.try_recv() {
            match more {
                Command::Apply(a) => batch.push(a),
                Command::Reconnect => return Some(self.reconnecting()),
                Command::Stop => return Some(SessionEnd::Stopped),
            }
        }
        None
    }

    /// A reconnect was asked for: the session ends, and the registry is
    /// sent again on the new socket.
    fn reconnecting(&self) -> SessionEnd {
        tracing::info!(
            broker = self.feed.broker(),
            "Market data feed reconnecting after the computer slept"
        );
        SessionEnd::Dropped(DropCause::Woke)
    }

    /// Whether the feed stalled: nothing received within the stall timeout,
    /// or no market data while the socket stayed open.
    fn stalled(&mut self, broker: &str, last_rx: Instant) -> Option<DropCause> {
        if last_rx.elapsed() >= self.config.stall_timeout {
            tracing::warn!(broker, "Market data feed stalled; reconnecting");
            return Some(DropCause::Silent);
        }
        if self.feed.data_stalled() {
            tracing::warn!(
                broker,
                "Market data stopped while the socket stayed open; reconnecting"
            );
            return Some(DropCause::DataStopped);
        }
        None
    }

    /// One socket session, to its end. A broker that closes with a code
    /// right after the upgrade (Sahi's 4429 when too many connections hold
    /// the login) can make a write fail before its close frame is read: the
    /// frame is read then, so the feed judges its code (audit P-12).
    pub(super) async fn session<S, R>(
        &mut self,
        broker: &str,
        write: &mut S,
        read: &mut R,
    ) -> SessionEnd
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
        R: futures_util::Stream<Item = std::result::Result<Message, WsError>> + Unpin,
    {
        match self.serve(broker, write, read).await {
            SessionEnd::WriteFailed => self.close_after_write(read).await,
            end => end,
        }
    }

    /// The broker's close frame after a failed write, read for a moment,
    /// judged by the feed; none heard is a close with no code.
    async fn close_after_write<R>(&mut self, read: &mut R) -> SessionEnd
    where
        R: futures_util::Stream<Item = std::result::Result<Message, WsError>> + Unpin,
    {
        let heard = tokio::time::timeout(CLOSE_AFTER_WRITE, async {
            while let Some(Ok(msg)) = read.next().await {
                if let Message::Close(frame) = msg {
                    return frame;
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        match &heard {
            Some(f) => {
                let ended = Ended::Closed {
                    code: Some(u16::from(f.code)),
                    reason: &f.reason,
                };
                self.judged(DropCause::Closed, ended)
            }
            None => self.judged(DropCause::Closed, CLOSED_UNHEARD),
        }
    }

    async fn serve<S, R>(&mut self, broker: &str, write: &mut S, read: &mut R) -> SessionEnd
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
        R: futures_util::Stream<Item = std::result::Result<Message, WsError>> + Unpin,
    {
        let hello = self.feed.on_connected();
        if let Err(end) = self.send_all(write, hello).await {
            return end;
        }
        let mut subscribed = false;
        if !self.feed.awaits_auth_ack() {
            if let Err(end) = self.subscribe_all(broker, write).await {
                return end;
            }
            subscribed = true;
        }
        let mut heartbeat = Heartbeat::new(&*self.feed);
        let check = (self.config.stall_timeout / 4).max(Duration::from_millis(10));
        let mut watchdog = tokio::time::interval_at(Instant::now() + check, check);
        let mut last_rx = Instant::now();
        let ack = self.ack_wait(subscribed);
        let ack_deadline =
            tokio::time::sleep(ack.map_or(Duration::from_secs(3600), |(after, _)| after));
        tokio::pin!(ack_deadline);
        loop {
            tokio::select! {
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            tracing::debug!(broker, kind = ws_error_kind(&e), "Market data feed read error");
                            return self.judged(DropCause::Closed, CLOSED_UNHEARD);
                        }
                        None => return self.judged(DropCause::Closed, CLOSED_UNHEARD),
                    };
                    last_rx = Instant::now();
                    heartbeat.heard();
                    if let Err(end) = self.on_message(broker, write, msg, &mut subscribed).await {
                        return end;
                    }
                }
                cmd = self.cmd.recv() => {
                    if let Err(end) = self.on_command(write, cmd, subscribed).await {
                        return end;
                    }
                }
                beat = heartbeat.next() => match beat {
                    Beat::Due(m) => {
                        if let Err(end) = self.send_all(write, vec![m]).await {
                            return end;
                        }
                        heartbeat.sent();
                    }
                    Beat::Unanswered => {
                        tracing::warn!(broker, "The broker did not answer the feed's heartbeat; reconnecting");
                        return SessionEnd::Dropped(DropCause::HeartbeatUnanswered);
                    }
                },
                _ = &mut ack_deadline, if ack.is_some() && !subscribed => {
                    let lapse = ack.map_or(AckLapse::Refuse, |(_, lapse)| lapse);
                    if let Err(end) = self.on_ack_lapse(broker, write, lapse).await {
                        return end;
                    }
                    subscribed = true;
                }
                _ = watchdog.tick() => {
                    if let Some(cause) = self.stalled(broker, last_rx) {
                        return SessionEnd::Dropped(cause);
                    }
                }
            }
        }
    }

    /// The login acknowledgement this session still waits for, and what
    /// its absence means once the wait is over; `None` when nothing is
    /// awaited.
    fn ack_wait(&self, subscribed: bool) -> Option<(Duration, AckLapse)> {
        if subscribed {
            return None;
        }
        // An optional acknowledgement (Groww: 2 s, like the web).
        if let Some(after) = self.feed.auth_ack_timeout() {
            return Some((after, AckLapse::Assume));
        }
        self.feed
            .auth_ack_deadline()
            .map(|after| (after, AckLapse::Refuse))
    }

    /// The broker did not acknowledge the login in time: take the session
    /// as accepted when the acknowledgement is optional, else end it so the
    /// capped backoff reconnects (TIME-05). Frames that keep arriving
    /// without an acknowledgement (heartbeats) must not keep a socket that
    /// never streams.
    async fn on_ack_lapse<S>(&mut self, broker: &str, write: &mut S, lapse: AckLapse) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        match lapse {
            AckLapse::Assume => self.subscribe_all(broker, write).await,
            AckLapse::Refuse => {
                tracing::warn!(
                    broker,
                    "The broker did not confirm the market data login in time; reconnecting"
                );
                Err(SessionEnd::Dropped(DropCause::LoginNotConfirmed))
            }
        }
    }

    /// One command from the manager; subscription changes wait for the
    /// login acknowledgement (they go out with the full set after it).
    async fn on_command<S>(&mut self, write: &mut S, cmd: Option<Command>, subscribed: bool) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        let mut batch = match cmd {
            Some(Command::Apply(a)) => vec![a],
            Some(Command::Reconnect) => return Err(self.reconnecting()),
            None | Some(Command::Stop) => return Err(SessionEnd::Stopped),
        };
        if let Some(end) = self.coalesce(&mut batch) {
            return Err(end);
        }
        if !subscribed {
            // A waiting caller's reply goes unanswered: nothing reached a
            // socket, and the full set follows the login acknowledgement.
            return Ok(());
        }
        let mut plain = Vec::new();
        for mut apply in batch {
            if apply.seq <= self.synced {
                // Made before this session was sent its whole set, which
                // carries it already.
                apply.deltas.clear();
            }
            let Some(reply) = apply.reply else {
                plain.extend(apply.deltas);
                continue;
            };
            let frames = self.delta_frames(std::mem::take(&mut plain), None);
            self.send_all(write, frames).await?;
            self.send_waited(write, apply.deltas, apply.undo, reply)
                .await?;
        }
        let frames = self.delta_frames(plain, None);
        self.send_all(write, frames).await
    }

    /// A release its caller waits on (audit C01), written apart so the
    /// caller hears what became of its own frames. What the socket did not
    /// take goes back into the registry before the session ends, so the
    /// next socket subscribes it again and the caller's "still subscribed"
    /// holds.
    async fn send_waited<S>(
        &mut self,
        write: &mut S,
        deltas: Vec<Delta>,
        undo: Vec<FeedSubscription>,
        reply: oneshot::Sender<Written>,
    ) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        let mut later = Vec::new();
        let frames = self.delta_frames(deltas, Some(&mut later));
        match self.send_all(write, frames).await {
            Ok(()) => {
                let _ = reply.send(Written::Sent(later));
                Ok(())
            }
            Err(end) => {
                {
                    let mut reg = self.registry.lock();
                    for s in undo {
                        reg.add(s, self.conn);
                    }
                }
                let _ = reply.send(Written::Failed);
                Err(end)
            }
        }
    }

    /// Frames for a run of deltas, in the web's order (`in_web_order`).
    /// Consecutive subscribes (and consecutive unsubscribes) go to the feed
    /// in one call, so feeds that can batch send one frame or request per
    /// run rather than one per instrument (web #2176: Firstock, Pocketful,
    /// RMoney batching). With
    /// `later`, unsubscribes are releases a caller waits on, and the
    /// broker's own answers to them are collected there.
    fn delta_frames(&mut self, deltas: Vec<Delta>, mut later: Option<&mut Later>) -> Vec<Message> {
        let deltas = super::registry::in_web_order(deltas);
        let mut frames = Vec::new();
        let mut subs: Vec<FeedSubscription> = Vec::new();
        let mut unsubs: Vec<FeedSubscription> = Vec::new();
        for d in deltas {
            match d {
                Delta::Subscribe(s) => {
                    if !unsubs.is_empty() {
                        frames.extend(
                            self.release_run(std::mem::take(&mut unsubs), later.as_deref_mut()),
                        );
                    }
                    subs.push(s);
                }
                Delta::Unsubscribe(s) => {
                    if !subs.is_empty() {
                        frames.extend(
                            self.subscribe_run(std::mem::take(&mut subs), later.as_deref_mut()),
                        );
                    }
                    unsubs.push(s);
                }
                Delta::ModeChange(old, new) => {
                    if !subs.is_empty() {
                        frames.extend(
                            self.subscribe_run(std::mem::take(&mut subs), later.as_deref_mut()),
                        );
                    }
                    if !unsubs.is_empty() {
                        frames.extend(
                            self.release_run(std::mem::take(&mut unsubs), later.as_deref_mut()),
                        );
                    }
                    frames.extend(self.feed.mode_change_frames(&old, &new));
                }
            }
        }
        if !subs.is_empty() {
            frames.extend(self.subscribe_run(subs, later.as_deref_mut()));
        }
        if !unsubs.is_empty() {
            frames.extend(self.release_run(unsubs, later));
        }
        frames
    }

    /// The frames of one run of unsubscribes; for a waited release, through
    /// `BrokerFeed::release_frames`, keeping the broker's later answer.
    /// The frames of one run of subscribes; for an admission its caller
    /// waits on, through `BrokerFeed::admission_frames`, keeping the
    /// broker's later answer (review R107-03).
    fn subscribe_run(
        &mut self,
        subs: Vec<FeedSubscription>,
        later: Option<&mut Later>,
    ) -> Vec<Message> {
        let Some(later) = later else {
            return self.feed.subscribe_frames(&subs);
        };
        let (frames, answer) = self.feed.admission_frames(&subs);
        if let Some(answer) = answer {
            later.push((subs, answer));
        }
        frames
    }

    fn release_run(
        &mut self,
        unsubs: Vec<FeedSubscription>,
        later: Option<&mut Later>,
    ) -> Vec<Message> {
        let Some(later) = later else {
            return self.feed.unsubscribe_frames(&unsubs);
        };
        let (frames, answer) = self.feed.release_frames(&unsubs);
        if let Some(answer) = answer {
            later.push((unsubs, answer));
        }
        frames
    }

    async fn resubscribe<S>(&mut self, write: &mut S) -> Sent
    where
        S: futures_util::Sink<Message, Error = WsError> + Unpin,
    {
        // The session was just accepted: its post-login frames go first.
        let mut frames = self.feed.on_authenticated();
        let (subs, synced) = {
            let reg = self.registry.lock();
            (reg.effective_on(self.conn), reg.changes)
        };
        self.synced = synced;
        if !subs.is_empty() {
            frames.extend(self.feed.subscribe_frames(&subs));
        }
        self.send_all(write, frames).await
    }
}
