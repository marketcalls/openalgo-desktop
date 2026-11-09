//! A corpus case as it is on disk (`tests/fixtures/feed_corpus/README.md`):
//! `case.json`, `script.jsonl`, the raw frames in `frames/`, the expected
//! frames in `expected/` and, once computed, `web.json`. Everything is read
//! and checked here, so a mistake in a case is reported with its file and
//! line before any socket opens.

use super::approvals;
use super::http::HttpRequestExpect;
use super::invariants::{self, Invariant};
use super::json_text;
use openalgo_desktop_lib::brokers::common::symbols::SymToken;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where the corpus lives.
pub(crate) fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/feed_corpus")
}

/// What a case may say it covers (README, "Scenarios"); the parity matrix
/// is built from these and from the modes the script subscribes.
pub(crate) const SCENARIOS: &[&str] = &[
    "first-full-packet",
    "partial-packet",
    "absent-field",
    "depth-update",
    "book-clear",
    "index",
    "derivative-oi",
    "reconnect",
    "subscription-lifecycle",
    "mode-change",
    "order-open",
    "order-partial-fill",
    "order-complete",
    "order-rejected",
    "order-cancelled",
];

/// Why the desktop's frame may differ from the web's (README,
/// "Deviations"): the web is wrong, a desktop security or resource rule, a
/// desktop-wide wire decision already taken, or a desktop gap still open.
const DEVIATION_KINDS: &[&str] = &[
    "web-defect",
    "desktop-rule",
    "desktop-decision",
    "open",
    RANDOM_VALUES,
];
/// A deviation kind: the key's value is drawn at random (a synthetic
/// feed), so the web check compares its presence and JSON type, and the
/// case's invariants bound it.
pub(crate) const RANDOM_VALUES: &str = "random-values";
/// The brokers whose web feed is synthetic: only their values are drawn at
/// random, so only they may declare `random-values`.
const SYNTHETIC_FEEDS: &[&str] = &["dhan_sandbox"];

/// How the expected values were found (README, "Provenance").
const METHODS: &[&str] = &["web-test", "web-script", "by-hand"];

/// One of the fake broker sockets a case's feeds connect to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Socket {
    /// The market data feed's first connection (Kite's order postbacks
    /// come on it too).
    Main,
    /// A further connection of the market data pool, from 2: `main#2`,
    /// `main#3` (`WebSocketManager::connect_pool`).
    Pool(u8),
    /// A broker's separate deep-book socket (Dhan 20, Fyers 50).
    Depth,
    /// A dedicated order-update socket.
    Orders,
}

/// The names of the pool's further connections, `main#2` up.
const POOL_NAMES: [&str; 8] = [
    "main#2", "main#3", "main#4", "main#5", "main#6", "main#7", "main#8", "main#9",
];

impl Socket {
    /// The sockets every case has.
    pub(crate) const ALL: [Socket; 3] = [Socket::Main, Socket::Depth, Socket::Orders];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Socket::Main => "main",
            Socket::Pool(n) => POOL_NAMES[usize::from(n) - 2],
            Socket::Depth => "depth",
            Socket::Orders => "orders",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Socket> {
        let pool = POOL_NAMES
            .iter()
            .position(|p| *p == name)
            .and_then(|i| u8::try_from(i + 2).ok())
            .map(Socket::Pool);
        pool.or_else(|| Self::ALL.into_iter().find(|s| s.name() == name))
    }

    /// The pooled connection this socket is, from 1 (the market feed's
    /// first is `main`); `None` for the deep-book and order sockets.
    pub(crate) fn pool_slot(self) -> Option<u8> {
        match self {
            Socket::Main => Some(1),
            Socket::Pool(n) => Some(n),
            Socket::Depth | Socket::Orders => None,
        }
    }
}

/// A WebSocket data frame as it crosses the broker socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Wire {
    Text(String),
    Binary(Vec<u8>),
    /// The feed's close frame, with its code and reason if it gave them;
    /// recorded on a socket whose script expects a close or disconnects it
    /// (`Case::close_sockets`).
    Closed(Option<(u16, String)>),
    /// The connection ended without a close frame (on such a socket).
    Ended,
}

impl std::fmt::Display for Wire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Wire::Text(t) => write!(f, "text {}", t),
            Wire::Binary(b) => write!(f, "binary {}", hex::encode(b)),
            Wire::Closed(Some((code, reason))) => write!(f, "close {} {:?}", code, reason),
            Wire::Closed(None) => write!(f, "close without a code"),
            Wire::Ended => write!(f, "the connection ended without a close frame"),
        }
    }
}

/// In a JSON frame or request a case expects from the feed, a value no case
/// can know (Upstox's random `guid`): any value at that key matches. Never
/// where the value is deterministic.
pub(crate) const ANY: &str = "<any>";

/// In a JSON frame or request a case expects from the feed, the time the
/// feed stamped it when it sent it (Kotak's `conn_req_time`): an integer
/// epoch time, in ms or in s, from this run.
pub(crate) const NOW_MS: &str = "<now_ms>";
pub(crate) const NOW_S: &str = "<now_s>";

/// How far back a send time may lie: longer than any case runs.
const RUN_MS: i64 = 15 * 60 * 1000;

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Whether `got` is an integer time of this run in `unit` ms.
fn sent_now(got: &Value, unit: i64) -> bool {
    let now = epoch_ms();
    got.as_i64()
        .map(|t| t.saturating_mul(unit))
        .is_some_and(|t| t > now - RUN_MS && t <= now + unit.max(5))
}

/// `got` is `want`, `"<any>"` matching any value and `"<now_ms>"` /
/// `"<now_s>"` a send time of this run: the same keys, the same values with
/// the same JSON types, arrays of the same length.
pub(crate) fn json_matches(want: &Value, got: &Value) -> bool {
    match (want, got) {
        (Value::String(s), _) if s == ANY => true,
        (Value::String(s), _) if s == NOW_MS => sent_now(got, 1),
        (Value::String(s), _) if s == NOW_S => sent_now(got, 1000),
        (Value::Object(w), Value::Object(g)) => {
            w.len() == g.len()
                && w.iter()
                    .all(|(k, wv)| g.get(k).is_some_and(|gv| json_matches(wv, gv)))
        }
        (Value::Array(w), Value::Array(g)) => {
            w.len() == g.len() && w.iter().zip(g).all(|(a, b)| json_matches(a, b))
        }
        _ => want == got,
    }
}

/// A frame the fake broker expects from the feed.
#[derive(Debug, Clone)]
pub(crate) enum Expect {
    /// A text frame holding this JSON (key order and spacing free).
    Json(Value),
    /// A binary frame holding this JSON (Upstox's subscribe frames).
    BinaryJson(Value),
    Text(String),
    Binary(Vec<u8>),
    /// The feed's close frame: `{"close": null}` any, `{"close": 1000}`
    /// with that code, `{"close": {"code": 4001, "reason": "..."}}` with
    /// that code and reason.
    Close(CloseExpect),
    /// The connection ended without a close frame: `{"ended": null}` (a
    /// handshake the feed gave up on, as Groww's first one without its
    /// subprotocol).
    Ended,
}

/// What a close frame the feed sends must carry (`None`: anything).
#[derive(Debug, Clone, Default)]
pub(crate) struct CloseExpect {
    pub(crate) code: Option<u16>,
    pub(crate) reason: Option<String>,
}

impl CloseExpect {
    fn matches(&self, got: Option<&(u16, String)>) -> bool {
        let code = self.code.is_none_or(|c| got.is_some_and(|(g, _)| *g == c));
        let reason = self
            .reason
            .as_ref()
            .is_none_or(|r| got.is_some_and(|(_, g)| g == r));
        code && reason
    }
}

impl Expect {
    pub(crate) fn matches(&self, wire: &Wire) -> bool {
        match (self, wire) {
            (Expect::Json(v), Wire::Text(t)) => {
                json_text::parse(t).is_ok_and(|g| json_matches(v, &g))
            }
            (Expect::BinaryJson(v), Wire::Binary(b)) => {
                json_text::parse_slice(b).is_ok_and(|g| json_matches(v, &g))
            }
            (Expect::Text(s), Wire::Text(t)) => s == t,
            (Expect::Binary(b), Wire::Binary(x)) => b == x,
            (Expect::Close(want), Wire::Closed(got)) => want.matches(got.as_ref()),
            (Expect::Ended, Wire::Ended) => true,
            _ => false,
        }
    }
}

impl std::fmt::Display for Expect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expect::Json(v) => write!(f, "json {}", v),
            Expect::BinaryJson(v) => write!(f, "binary json {}", v),
            Expect::Text(t) => write!(f, "text {}", t),
            Expect::Binary(b) => write!(f, "binary {}", hex::encode(b)),
            Expect::Ended => write!(f, "the connection ended without a close frame"),
            Expect::Close(c) => {
                write!(f, "close")?;
                if let Some(code) = c.code {
                    write!(f, " {}", code)?;
                }
                if let Some(reason) = &c.reason {
                    write!(f, " {:?}", reason)?;
                }
                Ok(())
            }
        }
    }
}

/// A body the fake broker API answers with, or a request body a case
/// expects.
#[derive(Debug, Clone)]
pub(crate) enum HttpBody {
    Json(Value),
    Text(String),
}

impl HttpBody {
    /// The body as sent.
    pub(crate) fn text(&self) -> String {
        match self {
            HttpBody::Json(v) => v.to_string(),
            HttpBody::Text(t) => t.clone(),
        }
    }
}

/// An answer of the fake broker API: from `case.json`'s `http` or an
/// `http_serve` step. `{ws:main}`, `{ws:depth}`, `{ws:orders}` and
/// `{http}` in the body become the fake sockets' and API's addresses.
#[derive(Debug, Clone)]
pub(crate) struct HttpAnswer {
    /// `None`: any method.
    pub(crate) method: Option<String>,
    pub(crate) path: String,
    pub(crate) status: u16,
    pub(crate) body: HttpBody,
    /// Keep its requests for `http_receives` steps.
    pub(crate) record: bool,
}

/// A web publish named by its instrument and mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebFrame {
    pub(crate) symbol: String,
    pub(crate) exchange: String,
    pub(crate) mode: u8,
    /// The deviation that declares this frame: `missing-frame`, or
    /// `missing-frame:<label>` where a case has web-only frames of more
    /// than one cause, each with its own reason and approval.
    pub(crate) deviation: String,
}

/// The deviation key for a web frame the desktop does not send.
pub(crate) const MISSING_FRAME: &str = "missing-frame";

/// Which web output a received frame is compared with (`web.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebPair {
    /// The web's publish of this mode (`None`: the frame's own mode, else
    /// the lowest richer one, which is what the web delivers to a client
    /// at that mode).
    Mode(Option<u8>),
    /// The web sends nothing for this frame (a deviation names `frame`).
    Nothing,
}

#[derive(Debug, Clone)]
pub(crate) enum Action {
    Send {
        client: String,
        message: Value,
        reply: Option<Value>,
    },
    CloseClient {
        client: String,
    },
    Receive {
        client: String,
        frame: Value,
        web: WebPair,
    },
    Silence {
        client: String,
        ms: u64,
        /// The web publish this client would get here and the desktop does
        /// not send (a deviation names `missing-frame`).
        web: Option<WebFrame>,
    },
    Push {
        socket: Socket,
        wire: Wire,
        connection: Option<usize>,
    },
    UpstreamReceives {
        socket: Socket,
        frames: Vec<Expect>,
        any_order: bool,
        connection: Option<usize>,
    },
    UpstreamConnected {
        socket: Socket,
        connection: usize,
        path: Option<String>,
        headers: BTreeMap<String, String>,
    },
    DropUpstream {
        socket: Socket,
        /// The close frame's code and reason, if the broker sends one.
        close: Option<(u16, String)>,
    },
    /// The socket's manager closes it deliberately (a logout, a broker
    /// switch) while clients still hold subscriptions: the feed's farewell
    /// frames, then its close, reach the fake broker.
    Disconnect {
        socket: Socket,
    },
    /// Refuse the next `times` upgrades with HTTP `status`.
    RefuseUpgrades {
        socket: Socket,
        status: u16,
        times: usize,
    },
    /// The socket has had exactly `count` connections (and `refused`
    /// refused upgrades), and still has after `ms`.
    UpstreamConnections {
        socket: Socket,
        count: usize,
        refused: Option<usize>,
        ms: u64,
    },
    /// The fake broker API answers this from now on (an order book the
    /// poller reads next).
    HttpServe(HttpAnswer),
    /// Wait until the answer for `method` and `path` has been read `times`
    /// times since it was set.
    HttpPolled {
        method: Option<String>,
        path: String,
        times: usize,
    },
    /// The next requests the fake broker API kept are exactly these.
    HttpReceives {
        requests: Vec<HttpRequestExpect>,
    },
    Note,
}

impl Action {
    /// The fake socket a step acts on, if any.
    fn socket(&self) -> Option<Socket> {
        match self {
            Action::Push { socket, .. }
            | Action::UpstreamReceives { socket, .. }
            | Action::UpstreamConnected { socket, .. }
            | Action::DropUpstream { socket, .. }
            | Action::Disconnect { socket }
            | Action::RefuseUpgrades { socket, .. }
            | Action::UpstreamConnections { socket, .. } => Some(*socket),
            _ => None,
        }
    }
}

/// One line of `script.jsonl`.
#[derive(Debug, Clone)]
pub(crate) struct Step {
    pub(crate) line: usize,
    pub(crate) action: Action,
}

#[derive(Debug, Clone)]
pub(crate) struct Case {
    /// `<broker>/<case>`.
    pub(crate) id: String,
    pub(crate) dir: PathBuf,
    pub(crate) broker: String,
    pub(crate) instruments: Vec<SymToken>,
    /// `instruments` as written, which `web.json` repeats.
    pub(crate) instruments_json: Value,
    /// Depth levels the broker streams per exchange (default 5).
    pub(crate) depth_levels: BTreeMap<String, Vec<u8>>,
    /// Text frames the fake broker does not record (heartbeats).
    pub(crate) upstream_ignore: Vec<String>,
    /// The web commit the expected values were read at.
    pub(crate) web_sha: String,
    /// `provenance.method` is `by-hand`: the one case that may have no
    /// `web.json`.
    pub(crate) by_hand: bool,
    /// The `data` keys (or `frame`, `missing-frame`) on which the desktop
    /// may differ from the web.
    pub(crate) deviations: Vec<String>,
    /// The deviations of kind `random-values`.
    pub(crate) random: BTreeSet<String>,
    /// What must hold of the random values (`invariants.rs`).
    pub(crate) invariants: Vec<Invariant>,
    pub(crate) steps: Vec<Step>,
    /// The fake sockets the script uses; a broker's order or deep-book
    /// feed is connected only when its socket is one of them.
    pub(crate) sockets: BTreeSet<Socket>,
    /// The sockets whose script expects the feed's close (an
    /// `upstream_receives` naming `{"close": ..}`, or a `disconnect`): every
    /// close the feed makes on them is recorded from the start, as a frame
    /// it sent.
    pub(crate) close_sockets: BTreeSet<Socket>,
    /// `refuse_upgrades` steps before anything else in the script, applied
    /// before the feeds connect (a socket that never starts), by socket:
    /// the status and how many times.
    pub(crate) refused_at_start: BTreeMap<Socket, (u16, usize)>,
    /// `case.json` `deep_book_timeout_ms`: the broker's deep-book silence
    /// timeout (Dhan's 30 s, `broker_runtime::deep_book_timeout`) shortened
    /// so a case can pass it; only for a broker that has one.
    pub(crate) deep_book_timeout: Option<std::time::Duration>,
    /// `case.json` `deep_feed_not_made`: the broker's deeper-book feed
    /// could not be made this session (`Broker::create_depth_feed` failed),
    /// so Depth subscribes at its levels are served at the market feed's
    /// levels (`BrokerBridge::set_deep_unavailable`).
    pub(crate) deep_feed_not_made: bool,
    /// `case.json` `pool_per_connection`: the market data pool's instruments
    /// per connection, in place of the broker's (`broker_runtime::
    /// pool_limits`), so a case opens a further connection with a few
    /// instruments; only for a broker whose pool has more than one.
    pub(crate) pool_per_connection: Option<usize>,
    /// What the fake broker API answers from the start (`case.json`
    /// `http`).
    pub(crate) http: Vec<HttpAnswer>,
    /// Whether the script asks for order updates (`subscribe_orders`, or
    /// the `orders` socket).
    pub(crate) orders: bool,
    /// What the web's output is computed from, in order: the script, each
    /// file a push or an `http_serve` names, then each file `case.json`'s
    /// `http` names (`web.json` records its SHA-256).
    pub(crate) inputs: Vec<u8>,
}

/// A case mistake: which case and file, and what is wrong.
pub(crate) fn bad(at: &str, msg: impl std::fmt::Display) -> ! {
    panic!("feed corpus {}: {}", at, msg)
}

pub(crate) fn read_json(path: &Path) -> Value {
    let at = path.display().to_string();
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| bad(&at, e));
    json_text::parse(&text).unwrap_or_else(|e| bad(&at, format!("not JSON: {}", e)))
}

impl Case {
    /// Read and check `tests/fixtures/feed_corpus/<broker>/<name>`.
    pub(crate) fn load(broker: &str, name: &str) -> Case {
        let id = format!("{}/{}", broker, name);
        let dir = corpus_root().join(broker).join(name);
        let meta = read_json(&dir.join("case.json"));
        let at = format!("{} case.json", id);
        let o = meta
            .as_object()
            .unwrap_or_else(|| bad(&at, "not a JSON object"));
        allow_keys(
            &at,
            o,
            &[
                "broker",
                "summary",
                "scenarios",
                "instruments",
                "depth_levels",
                "upstream_ignore",
                "http",
                "provenance",
                "deviations",
                "invariants",
                "deep_book_timeout_ms",
                "deep_feed_not_made",
                "pool_per_connection",
            ],
        );
        if o.get("broker").and_then(Value::as_str) != Some(broker) {
            bad(
                &at,
                format!("\"broker\" is not \"{}\", the folder's", broker),
            );
        }
        if !o
            .get("summary")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
        {
            bad(&at, "no \"summary\"");
        }
        check_scenarios(&at, o.get("scenarios"));
        let instruments_json = o.get("instruments").cloned().unwrap_or(Value::Null);
        let instruments = instruments_json
            .as_array()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| bad(&at, "\"instruments\" is a non-empty array"))
            .iter()
            .map(|v| instrument(&at, v))
            .collect();
        let script_path = dir.join("script.jsonl");
        // Line endings as committed, whatever a checkout made of them.
        let script = std::fs::read_to_string(&script_path)
            .unwrap_or_else(|e| bad(&format!("{} script.jsonl", id), e))
            .replace("\r\n", "\n");
        let mut inputs = script.clone().into_bytes();
        let mut steps = read_script(&id, &dir, &script, &mut inputs);
        let sockets: BTreeSet<Socket> = steps.iter().filter_map(|s| s.action.socket()).collect();
        let refused_at_start = refusals_at_start(&mut steps);
        let close_sockets: BTreeSet<Socket> = steps
            .iter()
            .filter_map(|s| match &s.action {
                Action::Disconnect { socket } => Some(*socket),
                Action::UpstreamReceives { socket, frames, .. }
                    if frames
                        .iter()
                        .any(|f| matches!(f, Expect::Close(_) | Expect::Ended)) =>
                {
                    Some(*socket)
                }
                _ => None,
            })
            .collect();
        let orders = sockets.contains(&Socket::Orders)
            || steps.iter().any(|s| {
                matches!(&s.action, Action::Send { message, .. }
                    if message["action"] == "subscribe_orders")
            });
        let http = match o.get("http") {
            None => Vec::new(),
            Some(Value::Array(list)) => list
                .iter()
                .map(|v| {
                    let a = v
                        .as_object()
                        .unwrap_or_else(|| bad(&at, "an \"http\" answer is an object"));
                    allow_keys(&at, a, &["method", "path", "status", "body", "record"]);
                    http_answer(&at, &dir, a, &mut inputs)
                })
                .collect(),
            Some(_) => bad(&at, "\"http\" is an array of answers"),
        };
        let (web_sha, by_hand) = provenance(&at, o.get("provenance"));
        let deviations = deviations(&at, broker, o.get("deviations"));
        Case {
            http,
            orders,
            depth_levels: depth_levels(&at, o.get("depth_levels")),
            upstream_ignore: strings(&at, "upstream_ignore", o.get("upstream_ignore")),
            web_sha,
            by_hand,
            deviations: deviations.iter().map(|(k, _)| k.clone()).collect(),
            random: deviations
                .iter()
                .filter(|(_, kind)| kind == RANDOM_VALUES)
                .map(|(k, _)| k.clone())
                .collect(),
            invariants: invariants::parse(&at, o.get("invariants")),
            id,
            dir,
            broker: broker.to_string(),
            instruments,
            instruments_json,
            steps,
            sockets,
            close_sockets,
            refused_at_start,
            pool_per_connection: o.get("pool_per_connection").map(|v| {
                v.as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n >= 1)
                    .unwrap_or_else(|| bad(&at, "\"pool_per_connection\" is a number from 1"))
            }),
            deep_feed_not_made: match o.get("deep_feed_not_made") {
                None => false,
                Some(v) => v
                    .as_bool()
                    .unwrap_or_else(|| bad(&at, "\"deep_feed_not_made\" is true or false")),
            },
            deep_book_timeout: o.get("deep_book_timeout_ms").map(|v| {
                std::time::Duration::from_millis(
                    v.as_u64()
                        .unwrap_or_else(|| bad(&at, "\"deep_book_timeout_ms\" is a number")),
                )
            }),
            inputs,
        }
    }
}

fn allow_keys(at: &str, o: &Map<String, Value>, keys: &[&str]) {
    for k in o.keys() {
        if !keys.contains(&k.as_str()) {
            bad(
                at,
                format!("unknown key \"{}\" (known: {})", k, keys.join(", ")),
            );
        }
    }
}

fn strings(at: &str, key: &str, v: Option<&Value>) -> Vec<String> {
    match v {
        None => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|s| {
                s.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| bad(at, format!("\"{}\" holds strings", key)))
            })
            .collect(),
        Some(_) => bad(at, format!("\"{}\" is an array of strings", key)),
    }
}

fn check_scenarios(at: &str, v: Option<&Value>) {
    let names = strings(at, "scenarios", v);
    if names.is_empty() {
        bad(at, "\"scenarios\" names what the case covers");
    }
    for n in names {
        if !SCENARIOS.contains(&n.as_str()) {
            bad(
                at,
                format!(
                    "unknown scenario \"{}\" (known: {})",
                    n,
                    SCENARIOS.join(", ")
                ),
            );
        }
    }
}

/// The web commit, after checking the provenance is complete.
fn provenance(at: &str, v: Option<&Value>) -> (String, bool) {
    let o = v
        .and_then(Value::as_object)
        .unwrap_or_else(|| bad(at, "no \"provenance\" object"));
    allow_keys(at, o, &["web_sha", "method", "sources", "notes"]);
    let sha = o
        .get("web_sha")
        .and_then(Value::as_str)
        .filter(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or_else(|| bad(at, "provenance.web_sha is the web commit (7+ hex digits)"));
    let method = o.get("method").and_then(Value::as_str).unwrap_or_default();
    if !METHODS.contains(&method) {
        bad(
            at,
            format!("provenance.method is one of {}", METHODS.join(", ")),
        );
    }
    if strings(at, "provenance.sources", o.get("sources")).is_empty() {
        bad(
            at,
            "provenance.sources names the web file and function (or file:line)",
        );
    }
    strings(at, "provenance.notes", o.get("notes"));
    (sha.to_string(), method == "by-hand")
}

/// Take the `refuse_upgrades` steps that open the script (notes aside) out
/// of it: they apply before the feeds connect.
fn refusals_at_start(steps: &mut Vec<Step>) -> BTreeMap<Socket, (u16, usize)> {
    let lead = steps
        .iter()
        .take_while(|s| matches!(s.action, Action::RefuseUpgrades { .. } | Action::Note))
        .count();
    let rest = steps.split_off(lead);
    let mut out = BTreeMap::new();
    let mut kept = Vec::new();
    for s in std::mem::replace(steps, rest) {
        match s.action {
            Action::RefuseUpgrades {
                socket,
                status,
                times,
            } => {
                out.insert(socket, (status, times));
            }
            _ => kept.push(s),
        }
    }
    kept.append(steps);
    *steps = kept;
    out
}

/// The keys the deviations cover, with their kinds, after checking each
/// is complete (`deviation_problems`).
fn deviations(at: &str, broker: &str, v: Option<&Value>) -> Vec<(String, String)> {
    let problems = deviation_problems(broker, v);
    if !problems.is_empty() {
        bad(at, problems.join("\n  "));
    }
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|d| {
            let text = |k: &str| d[k].as_str().unwrap_or_default().to_string();
            (text("key"), text("kind"))
        })
        .collect()
}

/// The fields a deviation may have.
const DEVIATION_FIELDS: &[&str] = &[
    "key", "kind", "reason", "ref", "approved", "approval", "web", "desktop",
];

/// Every way a `case.json`'s `deviations` fall short (none when each names
/// its key once, a kind of `DEVIATION_KINDS`, a reason, `approved` as true
/// or false, and an `approval` that names a rule of `approvals.json`, never
/// on an open gap): what the parity matrix counts can be trusted only when
/// every case has none. A deviation with no `approval` is pending, which the
/// release gate fails on, not this check.
pub(crate) fn deviation_problems(broker: &str, v: Option<&Value>) -> Vec<String> {
    let Some(v) = v else {
        return Vec::new();
    };
    let Some(list) = v.as_array() else {
        return vec!["\"deviations\" is not an array".into()];
    };
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, d) in list.iter().enumerate() {
        let Some(o) = d.as_object() else {
            out.push(format!("deviation {} is not an object", i + 1));
            continue;
        };
        let key = o
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty());
        let name = key.map_or_else(
            || format!("deviation {}", i + 1),
            |k| format!("deviation \"{}\"", k),
        );
        if key.is_none() {
            out.push(format!("{} names no \"key\"", name));
        }
        for k in o.keys().filter(|k| !DEVIATION_FIELDS.contains(&k.as_str())) {
            out.push(format!("{}: \"{}\" is not a field of a deviation", name, k));
        }
        let kind = o.get("kind").and_then(Value::as_str).unwrap_or_default();
        if !DEVIATION_KINDS.contains(&kind) {
            out.push(format!(
                "{}: kind \"{}\" is not one of {}",
                name,
                kind,
                DEVIATION_KINDS.join(", ")
            ));
        }
        if kind == RANDOM_VALUES && !SYNTHETIC_FEEDS.contains(&broker) {
            out.push(format!(
                "{}: {} is only for a synthetic feed ({}), not {}",
                name,
                RANDOM_VALUES,
                SYNTHETIC_FEEDS.join(", "),
                broker
            ));
        }
        if !o
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|r| !r.trim().is_empty())
        {
            out.push(format!("{} gives no reason", name));
        }
        if o.get("approved").is_some_and(|a| !a.is_boolean()) {
            out.push(format!("{}: approved is true or false", name));
        }
        if let Some(a) = o.get("approval") {
            if let Some(p) = approvals::problem(a) {
                out.push(format!("{}: {}", name, p));
            }
            if kind == "open" {
                out.push(format!("{}: an open gap is never approved", name));
            }
        }
        if key.is_some_and(|k| !seen.insert(k)) {
            out.push(format!("{} is listed twice", name));
        }
    }
    out
}

fn depth_levels(at: &str, v: Option<&Value>) -> BTreeMap<String, Vec<u8>> {
    let Some(v) = v else {
        return BTreeMap::new();
    };
    let o = v
        .as_object()
        .unwrap_or_else(|| bad(at, "\"depth_levels\" maps an exchange to its levels"));
    o.iter()
        .map(|(exchange, levels)| {
            let levels = levels
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|n| {
                            n.as_u64()
                                .and_then(|n| u8::try_from(n).ok())
                                .unwrap_or_else(|| bad(at, "a depth level is a small number"))
                        })
                        .collect::<Vec<u8>>()
                })
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| bad(at, "depth_levels lists the levels per exchange"));
            (exchange.clone(), levels)
        })
        .collect()
}

/// A symbol master row; everything but symbol, exchange and token has the
/// default an equity row carries.
fn instrument(at: &str, v: &Value) -> SymToken {
    let o = v
        .as_object()
        .unwrap_or_else(|| bad(at, "an instrument is an object"));
    allow_keys(
        at,
        o,
        &[
            "symbol",
            "exchange",
            "token",
            "brsymbol",
            "brexchange",
            "name",
            "expiry",
            "strike",
            "lotsize",
            "instrumenttype",
            "tick_size",
        ],
    );
    let text = |k: &str| o.get(k).and_then(Value::as_str).map(String::from);
    let need =
        |k: &str| text(k).unwrap_or_else(|| bad(at, format!("an instrument without \"{}\"", k)));
    let symbol = need("symbol");
    let exchange = need("exchange");
    SymToken {
        brsymbol: text("brsymbol").unwrap_or_else(|| symbol.clone()),
        name: text("name").unwrap_or_else(|| symbol.clone()),
        brexchange: text("brexchange").unwrap_or_else(|| exchange.clone()),
        token: need("token"),
        expiry: text("expiry").unwrap_or_default(),
        strike: o.get("strike").and_then(Value::as_f64).unwrap_or(0.0),
        lot_size: o
            .get("lotsize")
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(1),
        instrument_type: text("instrumenttype").unwrap_or_else(|| "EQ".into()),
        tick_size: o.get("tick_size").and_then(Value::as_f64).unwrap_or(0.05),
        symbol,
        exchange,
    }
}

fn read_script(id: &str, dir: &Path, text: &str, inputs: &mut Vec<u8>) -> Vec<Step> {
    let mut steps = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let at = format!("{} script.jsonl:{}", id, i + 1);
        let v: Value =
            json_text::parse(line).unwrap_or_else(|e| bad(&at, format!("not JSON: {}", e)));
        let o = v
            .as_object()
            .unwrap_or_else(|| bad(&at, "a step is a JSON object"));
        let action = step(&at, dir, o, inputs);
        steps.push(Step {
            line: i + 1,
            action,
        });
    }
    if steps.is_empty() {
        bad(&format!("{} script.jsonl", id), "no steps");
    }
    steps
}

fn step(at: &str, dir: &Path, o: &Map<String, Value>, inputs: &mut Vec<u8>) -> Action {
    let kind = o
        .get("step")
        .and_then(Value::as_str)
        .unwrap_or_else(|| bad(at, "no \"step\""));
    let keys: &[&str] = match kind {
        "send" => &["client", "message", "reply"],
        "close_client" => &["client"],
        "receive" => &["client", "frame", "web", "web_mode"],
        "silence" => &["client", "ms", "web"],
        "push" => &["socket", "frame", "connection"],
        "upstream_receives" => &["socket", "frames", "any_order", "connection"],
        "upstream_connected" => &["socket", "connection", "path", "headers"],
        "drop_upstream" => &["socket", "code", "reason"],
        "disconnect" => &["socket"],
        "refuse_upgrades" => &["socket", "status", "times"],
        "upstream_connections" => &["socket", "count", "refused", "ms"],
        "http_serve" => &["method", "path", "status", "body", "record"],
        "http_polled" => &["method", "path", "times"],
        "http_receives" => &["requests"],
        "note" => &["text"],
        other => bad(at, format!("unknown step \"{}\"", other)),
    };
    for k in o.keys() {
        if k != "step" && k != "note" && !keys.contains(&k.as_str()) {
            bad(at, format!("\"{}\" is not a key of a \"{}\" step", k, kind));
        }
    }
    let client = || {
        o.get("client")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(String::from)
            .unwrap_or_else(|| bad(at, "no \"client\""))
    };
    let socket = || match o.get("socket") {
        None => Socket::Main,
        Some(v) => v.as_str().and_then(Socket::named).unwrap_or_else(|| {
            bad(
                at,
                "socket is \"main\", a pooled \"main#2\" to \"main#9\", \"depth\" or \"orders\"",
            )
        }),
    };
    let number = |k: &str| {
        o.get(k).map(|v| {
            v.as_u64()
                .filter(|n| *n >= 1)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or_else(|| bad(at, format!("\"{}\" is a number from 1", k)))
        })
    };
    let need = |k: &str| {
        o.get(k)
            .cloned()
            .unwrap_or_else(|| bad(at, format!("no \"{}\"", k)))
    };
    match kind {
        "send" => Action::Send {
            client: client(),
            message: need("message"),
            reply: o.get("reply").cloned(),
        },
        "close_client" => Action::CloseClient { client: client() },
        "receive" => Action::Receive {
            client: client(),
            frame: expected_frame(at, dir, &need("frame")),
            web: web_pair(at, o),
        },
        "silence" => Action::Silence {
            client: client(),
            ms: o.get("ms").map_or(300, |v| {
                v.as_u64().unwrap_or_else(|| bad(at, "\"ms\" is a number"))
            }),
            web: o.get("web").map(|w| web_frame(at, w)),
        },
        "push" => Action::Push {
            socket: socket(),
            wire: pushed_frame(at, dir, &need("frame"), inputs),
            connection: number("connection"),
        },
        "upstream_receives" => Action::UpstreamReceives {
            socket: socket(),
            frames: need("frames")
                .as_array()
                .unwrap_or_else(|| bad(at, "\"frames\" is an array"))
                .iter()
                .map(|f| upstream_frame(at, dir, f))
                .collect(),
            any_order: o.get("any_order").and_then(Value::as_bool).unwrap_or(false),
            connection: number("connection"),
        },
        "upstream_connected" => Action::UpstreamConnected {
            socket: socket(),
            connection: number("connection").unwrap_or(1),
            path: o.get("path").map(|p| {
                p.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| bad(at, "\"path\" is a string"))
            }),
            headers: o
                .get("headers")
                .map(|h| {
                    h.as_object()
                        .unwrap_or_else(|| bad(at, "\"headers\" is an object"))
                        .iter()
                        .map(|(k, v)| {
                            let v = v
                                .as_str()
                                .unwrap_or_else(|| bad(at, "a header value is a string"));
                            (k.to_ascii_lowercase(), v.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "disconnect" => Action::Disconnect { socket: socket() },
        "drop_upstream" => Action::DropUpstream {
            socket: socket(),
            close: match (o.get("code"), o.get("reason")) {
                (None, None) => None,
                (Some(c), r) => Some((
                    c.as_u64()
                        .and_then(|c| u16::try_from(c).ok())
                        .unwrap_or_else(|| bad(at, "\"code\" is a close code")),
                    r.map_or_else(String::new, |r| {
                        r.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| bad(at, "\"reason\" is a string"))
                    }),
                )),
                (None, Some(_)) => bad(at, "a close \"reason\" goes with its \"code\""),
            },
        },
        "refuse_upgrades" => Action::RefuseUpgrades {
            socket: socket(),
            status: need("status")
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .filter(|s| (400..600).contains(s))
                .unwrap_or_else(|| bad(at, "\"status\" is an HTTP error status")),
            times: number("times").unwrap_or(1),
        },
        "upstream_connections" => Action::UpstreamConnections {
            socket: socket(),
            count: need("count")
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or_else(|| bad(at, "\"count\" is a number")),
            refused: o.get("refused").map(|r| {
                r.as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or_else(|| bad(at, "\"refused\" is a number"))
            }),
            ms: o.get("ms").map_or(300, |v| {
                v.as_u64().unwrap_or_else(|| bad(at, "\"ms\" is a number"))
            }),
        },
        "http_serve" => Action::HttpServe(http_answer(at, dir, o, inputs)),
        "http_polled" => Action::HttpPolled {
            method: http_method(at, o),
            path: http_path(at, o),
            times: number("times").unwrap_or(1),
        },
        "http_receives" => Action::HttpReceives {
            requests: need("requests")
                .as_array()
                .unwrap_or_else(|| bad(at, "\"requests\" is an array"))
                .iter()
                .map(|r| http_request(at, r))
                .collect(),
        },
        _ => Action::Note,
    }
}

fn http_method(at: &str, o: &Map<String, Value>) -> Option<String> {
    o.get("method").map(|m| {
        m.as_str()
            .map(str::to_ascii_uppercase)
            .unwrap_or_else(|| bad(at, "\"method\" is a string"))
    })
}

fn http_path(at: &str, o: &Map<String, Value>) -> String {
    o.get("path")
        .and_then(Value::as_str)
        .filter(|p| p.starts_with('/'))
        .map(String::from)
        .unwrap_or_else(|| bad(at, "\"path\" is a path from /"))
}

/// An answer of the fake broker API: `method` (default any), `path`,
/// `status` (default 200), `body` (a file of the case, `{"json"}` or
/// `{"text"}`; default empty) and `record`. A body file joins `inputs`.
fn http_answer(at: &str, dir: &Path, o: &Map<String, Value>, inputs: &mut Vec<u8>) -> HttpAnswer {
    let status = o.get("status").map_or(200, |s| {
        s.as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| (100..600).contains(n))
            .unwrap_or_else(|| bad(at, "\"status\" is an HTTP status"))
    });
    let body = match o.get("body") {
        None => HttpBody::Text(String::new()),
        Some(Value::String(name)) => {
            let (path, bytes) = case_file(at, dir, name);
            inputs.extend_from_slice(
                String::from_utf8_lossy(&bytes)
                    .replace("\r\n", "\n")
                    .as_bytes(),
            );
            let text = utf8(at, bytes).replace("\r\n", "\n");
            match extension(&path) {
                "json" => HttpBody::Json(
                    json_text::parse(&text)
                        .unwrap_or_else(|e| bad(at, format!("{}: not JSON: {}", name, e))),
                ),
                _ => HttpBody::Text(text),
            }
        }
        Some(v) => match inline(at, v) {
            ("json", json) => HttpBody::Json(json.clone()),
            ("text", t) => HttpBody::Text(t.as_str().unwrap_or_default().to_string()),
            _ => bad(at, "an HTTP body is a file, {\"json\"} or {\"text\"}"),
        },
    };
    HttpAnswer {
        method: http_method(at, o),
        path: http_path(at, o),
        status,
        body,
        record: o.get("record").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// A request an `http_receives` step expects: `method`, `path` with its
/// query, and the body as `json` (compared as JSON, `"<any>"` allowed),
/// `text`, or neither for an empty body.
fn http_request(at: &str, v: &Value) -> HttpRequestExpect {
    let o = v
        .as_object()
        .unwrap_or_else(|| bad(at, "an expected request is an object"));
    allow_keys(at, o, &["method", "path", "json", "text"]);
    let body = match (o.get("json"), o.get("text")) {
        (Some(j), None) => Some(HttpBody::Json(j.clone())),
        (None, Some(t)) => Some(HttpBody::Text(
            t.as_str()
                .map(String::from)
                .unwrap_or_else(|| bad(at, "\"text\" is a string")),
        )),
        (None, None) => None,
        _ => bad(at, "an expected request has \"json\" or \"text\", not both"),
    };
    HttpRequestExpect {
        method: http_method(at, o)
            .unwrap_or_else(|| bad(at, "an expected request names its method")),
        path: http_path(at, o),
        body,
    }
}

fn web_frame(at: &str, v: &Value) -> WebFrame {
    let o = v.as_object().unwrap_or_else(|| {
        bad(
            at,
            "\"web\" names the web's frame: {\"symbol\", \"exchange\", \"mode\"}",
        )
    });
    allow_keys(at, o, &["symbol", "exchange", "mode", "deviation"]);
    let text = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| bad(at, format!("\"web\" names its \"{}\"", k)))
    };
    WebFrame {
        symbol: text("symbol"),
        exchange: text("exchange"),
        mode: o
            .get("mode")
            .and_then(Value::as_u64)
            .filter(|m| (1..=3).contains(m))
            .and_then(|m| u8::try_from(m).ok())
            .unwrap_or_else(|| bad(at, "\"web\" \"mode\" is 1, 2 or 3")),
        deviation: match o.get("deviation") {
            None => MISSING_FRAME.to_string(),
            Some(d) => d
                .as_str()
                .filter(|d| {
                    d.strip_prefix(MISSING_FRAME)
                        .is_some_and(|r| r.is_empty() || r.len() > 1 && r.starts_with(':'))
                })
                .map(String::from)
                .unwrap_or_else(|| {
                    bad(
                        at,
                        "\"web\" \"deviation\" is \"missing-frame\" or \"missing-frame:<label>\"",
                    )
                }),
        },
    }
}

fn web_pair(at: &str, o: &Map<String, Value>) -> WebPair {
    match (o.get("web"), o.get("web_mode")) {
        (Some(Value::Bool(false)), None) => WebPair::Nothing,
        (None, None) => WebPair::Mode(None),
        (None, Some(m)) => WebPair::Mode(Some(
            m.as_u64()
                .filter(|m| (1..=3).contains(m))
                .and_then(|m| u8::try_from(m).ok())
                .unwrap_or_else(|| bad(at, "\"web_mode\" is 1, 2 or 3")),
        )),
        _ => bad(at, "\"web\" is only ever false, and not with \"web_mode\""),
    }
}

/// A file of the case, named from the case folder.
fn case_file(at: &str, dir: &Path, name: &str) -> (PathBuf, Vec<u8>) {
    if name.contains("..") || Path::new(name).is_absolute() {
        bad(at, format!("\"{}\" is not a file in the case folder", name));
    }
    let path = dir.join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| bad(at, format!("{}: {}", name, e)));
    (path, bytes)
}

fn extension(path: &Path) -> &str {
    path.extension().and_then(|e| e.to_str()).unwrap_or("")
}

fn utf8(at: &str, bytes: Vec<u8>) -> String {
    let text = String::from_utf8(bytes).unwrap_or_else(|_| bad(at, "a text frame is UTF-8"));
    text.trim_end_matches(['\n', '\r']).to_string()
}

/// Hex digits with any spacing; `#` starts a comment to the end of the
/// line, so a packet can be written field by field.
fn decode_hex(at: &str, text: &str) -> Vec<u8> {
    let digits: String = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    hex::decode(digits).unwrap_or_else(|e| bad(at, format!("hex: {}", e)))
}

/// A frame the fake broker sends: a file (`.hex` and `.bin` binary,
/// `.json` and `.txt` text, sent as written) or `{"hex"}`, `{"text"}`,
/// `{"json"}`. A file's bytes join the case's inputs (a text file's with
/// LF line ends, so a Windows checkout's CRLF does not change them).
fn pushed_frame(at: &str, dir: &Path, v: &Value, inputs: &mut Vec<u8>) -> Wire {
    if let Some(name) = v.as_str() {
        let (path, bytes) = case_file(at, dir, name);
        match extension(&path) {
            "bin" => inputs.extend_from_slice(&bytes),
            _ => inputs.extend_from_slice(
                String::from_utf8_lossy(&bytes)
                    .replace("\r\n", "\n")
                    .as_bytes(),
            ),
        }
        return match extension(&path) {
            "hex" => Wire::Binary(decode_hex(at, &utf8(at, bytes))),
            "bin" => Wire::Binary(bytes),
            "json" | "txt" => Wire::Text(utf8(at, bytes)),
            _ => bad(
                at,
                format!("{}: a frame file is .hex, .bin, .json or .txt", name),
            ),
        };
    }
    match inline(at, v) {
        ("hex", s) => Wire::Binary(decode_hex(at, s.as_str().unwrap_or_default())),
        ("text", s) => Wire::Text(s.as_str().unwrap_or_default().to_string()),
        ("binary_json", json) => Wire::Binary(json.to_string().into_bytes()),
        (_, json) => Wire::Text(json.to_string()),
    }
}

/// A frame the fake broker expects, in the same forms; a `.json` file or
/// `{"json"}` is compared as JSON.
fn upstream_frame(at: &str, dir: &Path, v: &Value) -> Expect {
    if let Some(name) = v.as_str() {
        let (path, bytes) = case_file(at, dir, name);
        return match extension(&path) {
            "hex" => Expect::Binary(decode_hex(at, &utf8(at, bytes))),
            "bin" => Expect::Binary(bytes),
            "json" => Expect::Json(
                json_text::parse_slice(&bytes)
                    .unwrap_or_else(|e| bad(at, format!("{}: not JSON: {}", name, e))),
            ),
            "txt" => Expect::Text(utf8(at, bytes)),
            _ => bad(
                at,
                format!("{}: a frame file is .hex, .bin, .json or .txt", name),
            ),
        };
    }
    if let Some(code) = v
        .as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.get("close"))
    {
        return Expect::Close(close_expect(at, code));
    }
    if let Some(ended) = v
        .as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.get("ended"))
    {
        if !matches!(ended, Value::Null | Value::Bool(true)) {
            bad(
                at,
                "\"ended\" is null: a connection that ended without a close frame",
            );
        }
        return Expect::Ended;
    }
    match inline(at, v) {
        ("hex", s) => Expect::Binary(decode_hex(at, s.as_str().unwrap_or_default())),
        ("text", s) => Expect::Text(s.as_str().unwrap_or_default().to_string()),
        ("binary_json", json) => Expect::BinaryJson(json.clone()),
        (_, json) => Expect::Json(json.clone()),
    }
}

/// `{"close": ..}`: null (or true) for any close frame, a code, or
/// `{"code", "reason"}`.
fn close_expect(at: &str, v: &Value) -> CloseExpect {
    let code = |c: &Value| {
        c.as_u64()
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or_else(|| bad(at, "a close \"code\" is a number"))
    };
    match v {
        Value::Null | Value::Bool(true) => CloseExpect::default(),
        Value::Number(_) => CloseExpect {
            code: Some(code(v)),
            reason: None,
        },
        Value::Object(o) => {
            allow_keys(at, o, &["code", "reason"]);
            CloseExpect {
                code: o.get("code").map(code),
                reason: o.get("reason").map(|r| {
                    r.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| bad(at, "a close \"reason\" is a string"))
                }),
            }
        }
        _ => bad(
            at,
            "\"close\" is null (any), a code, or {\"code\", \"reason\"}",
        ),
    }
}

/// `{"hex": "..."}`, `{"text": "..."}`, `{"json": ...}` (a text frame) or
/// `{"binary_json": ...}` (a binary frame holding JSON).
fn inline<'a>(at: &str, v: &'a Value) -> (&'static str, &'a Value) {
    const FORMS: &str =
        "a frame is a file name, {\"hex\"}, {\"text\"}, {\"json\"} or {\"binary_json\"}";
    let o = v
        .as_object()
        .filter(|o| o.len() == 1)
        .unwrap_or_else(|| bad(at, FORMS));
    for kind in ["hex", "text", "json", "binary_json"] {
        if let Some(inner) = o.get(kind) {
            if matches!(kind, "hex" | "text") && !inner.is_string() {
                bad(at, format!("\"{}\" holds a string", kind));
            }
            return (kind, inner);
        }
    }
    bad(at, FORMS)
}

/// A frame a client expects: a `.json` file of the case, or the frame
/// itself.
fn expected_frame(at: &str, dir: &Path, v: &Value) -> Value {
    match v.as_str() {
        Some(name) => {
            let (_, bytes) = case_file(at, dir, name);
            json_text::parse_slice(&bytes)
                .unwrap_or_else(|e| bad(at, format!("{}: not JSON: {}", name, e)))
        }
        None if v.is_object() => v.clone(),
        None => bad(at, "\"frame\" is a file name or the frame"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A close the feed makes is matched by code and reason as named, and
    /// never by an end without a close frame.
    #[test]
    fn a_close_expectation_names_what_it_checks() {
        let got = Wire::Closed(Some((4001, "Invalid session".to_string())));
        let close = |v: Value| upstream_frame("t", Path::new("."), &json!({ "close": v }));
        assert!(close(Value::Null).matches(&got));
        assert!(close(json!(4001)).matches(&got));
        assert!(!close(json!(1000)).matches(&got));
        assert!(close(json!({"code": 4001, "reason": "Invalid session"})).matches(&got));
        assert!(!close(json!({"reason": "Expired"})).matches(&got));
        assert!(!close(json!({"code": 4001})).matches(&Wire::Closed(None)));
        assert!(close(Value::Null).matches(&Wire::Closed(None)));
        assert!(!close(Value::Null).matches(&Wire::Ended));
    }

    /// A connection that ended without a close frame has its own
    /// expectation, and a close frame does not meet it.
    #[test]
    fn an_ended_connection_is_expected_by_name() {
        let ended = upstream_frame("t", Path::new("."), &json!({"ended": null}));
        assert!(ended.matches(&Wire::Ended));
        assert!(!ended.matches(&Wire::Closed(None)));
        assert!(!ended.matches(&Wire::Closed(Some((1000, String::new())))));
        assert_eq!(
            ended.to_string(),
            "the connection ended without a close frame"
        );
    }
}
