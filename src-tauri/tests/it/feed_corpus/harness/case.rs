//! A corpus case as it is on disk (`tests/fixtures/feed_corpus/README.md`):
//! `case.json`, `script.jsonl`, the raw frames in `frames/`, the expected
//! frames in `expected/` and, once computed, `web.json`. Everything is read
//! and checked here, so a mistake in a case is reported with its file and
//! line before any socket opens.

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
const DEVIATION_KINDS: &[&str] = &["web-defect", "desktop-rule", "desktop-decision", "open"];

/// How the expected values were found (README, "Provenance").
const METHODS: &[&str] = &["web-test", "web-script", "by-hand"];

/// One of the fake broker sockets a case's feeds connect to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Socket {
    /// The market data feed (Kite's order postbacks come on it too).
    Main,
    /// A broker's separate deep-book socket (Dhan 20, Fyers 50).
    Depth,
    /// A dedicated order-update socket.
    Orders,
}

impl Socket {
    pub(crate) const ALL: [Socket; 3] = [Socket::Main, Socket::Depth, Socket::Orders];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Socket::Main => "main",
            Socket::Depth => "depth",
            Socket::Orders => "orders",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Socket> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// A WebSocket data frame as it crosses the broker socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Wire {
    Text(String),
    Binary(Vec<u8>),
}

impl std::fmt::Display for Wire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Wire::Text(t) => write!(f, "text {}", t),
            Wire::Binary(b) => write!(f, "binary {}", hex::encode(b)),
        }
    }
}

/// A frame the fake broker expects from the feed.
#[derive(Debug, Clone)]
pub(crate) enum Expect {
    /// A text frame holding this JSON (key order and spacing free).
    Json(Value),
    Text(String),
    Binary(Vec<u8>),
}

impl Expect {
    pub(crate) fn matches(&self, wire: &Wire) -> bool {
        match (self, wire) {
            (Expect::Json(v), Wire::Text(t)) => {
                serde_json::from_str::<Value>(t).ok().as_ref() == Some(v)
            }
            (Expect::Text(s), Wire::Text(t)) => s == t,
            (Expect::Binary(b), Wire::Binary(x)) => b == x,
            _ => false,
        }
    }
}

impl std::fmt::Display for Expect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expect::Json(v) => write!(f, "json {}", v),
            Expect::Text(t) => write!(f, "text {}", t),
            Expect::Binary(b) => write!(f, "binary {}", hex::encode(b)),
        }
    }
}

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
            | Action::DropUpstream { socket } => Some(*socket),
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
    /// The `data` keys (or `frame`) on which the desktop may differ from
    /// the web.
    pub(crate) deviations: Vec<String>,
    pub(crate) steps: Vec<Step>,
    /// The fake sockets the script uses; a broker's order or deep-book
    /// feed is connected only when its socket is one of them.
    pub(crate) sockets: BTreeSet<Socket>,
    /// What the web's output is computed from, in order: the script, then
    /// each pushed frame file (`web.json` records its SHA-256).
    pub(crate) inputs: Vec<u8>,
}

/// A case mistake: which case and file, and what is wrong.
pub(crate) fn bad(at: &str, msg: impl std::fmt::Display) -> ! {
    panic!("feed corpus {}: {}", at, msg)
}

pub(crate) fn read_json(path: &Path) -> Value {
    let at = path.display().to_string();
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| bad(&at, e));
    serde_json::from_str(&text).unwrap_or_else(|e| bad(&at, format!("not JSON: {}", e)))
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
                "provenance",
                "deviations",
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
        let steps = read_script(&id, &dir, &script, &mut inputs);
        let sockets = steps.iter().filter_map(|s| s.action.socket()).collect();
        Case {
            depth_levels: depth_levels(&at, o.get("depth_levels")),
            upstream_ignore: strings(&at, "upstream_ignore", o.get("upstream_ignore")),
            web_sha: provenance(&at, o.get("provenance")),
            deviations: deviations(&at, o.get("deviations")),
            id,
            dir,
            broker: broker.to_string(),
            instruments,
            instruments_json,
            steps,
            sockets,
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
fn provenance(at: &str, v: Option<&Value>) -> String {
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
    sha.to_string()
}

/// The keys the deviations cover, after checking each is complete.
fn deviations(at: &str, v: Option<&Value>) -> Vec<String> {
    let Some(v) = v else {
        return Vec::new();
    };
    let list = v
        .as_array()
        .unwrap_or_else(|| bad(at, "\"deviations\" is an array"));
    let mut keys = Vec::new();
    for d in list {
        let o = d
            .as_object()
            .unwrap_or_else(|| bad(at, "a deviation is an object"));
        allow_keys(
            at,
            o,
            &["key", "kind", "reason", "ref", "approved", "web", "desktop"],
        );
        let key = o
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| bad(at, "a deviation names its \"key\""));
        let kind = o.get("kind").and_then(Value::as_str).unwrap_or_default();
        if !DEVIATION_KINDS.contains(&kind) {
            bad(
                at,
                format!(
                    "deviation \"{}\": kind is one of {}",
                    key,
                    DEVIATION_KINDS.join(", ")
                ),
            );
        }
        if !o
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|r| !r.trim().is_empty())
        {
            bad(at, format!("deviation \"{}\" gives no reason", key));
        }
        if o.get("approved").is_some_and(|a| !a.is_boolean()) {
            bad(
                at,
                format!("deviation \"{}\": approved is true or false", key),
            );
        }
        if keys.iter().any(|k| k == key) {
            bad(at, format!("deviation \"{}\" is listed twice", key));
        }
        keys.push(key.to_string());
    }
    keys
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
            serde_json::from_str(line).unwrap_or_else(|e| bad(&at, format!("not JSON: {}", e)));
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
        "silence" => &["client", "ms"],
        "push" => &["socket", "frame", "connection"],
        "upstream_receives" => &["socket", "frames", "any_order", "connection"],
        "upstream_connected" => &["socket", "connection", "path", "headers"],
        "drop_upstream" => &["socket"],
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
        Some(v) => v
            .as_str()
            .and_then(Socket::named)
            .unwrap_or_else(|| bad(at, "socket is \"main\", \"depth\" or \"orders\"")),
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
        "drop_upstream" => Action::DropUpstream { socket: socket() },
        _ => Action::Note,
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
    hex::decode(&digits).unwrap_or_else(|e| bad(at, format!("hex: {}", e)))
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
                serde_json::from_slice(&bytes)
                    .unwrap_or_else(|e| bad(at, format!("{}: not JSON: {}", name, e))),
            ),
            "txt" => Expect::Text(utf8(at, bytes)),
            _ => bad(
                at,
                format!("{}: a frame file is .hex, .bin, .json or .txt", name),
            ),
        };
    }
    match inline(at, v) {
        ("hex", s) => Expect::Binary(decode_hex(at, s.as_str().unwrap_or_default())),
        ("text", s) => Expect::Text(s.as_str().unwrap_or_default().to_string()),
        (_, json) => Expect::Json(json.clone()),
    }
}

/// `{"hex": "..."}`, `{"text": "..."}` or `{"json": ...}`.
fn inline<'a>(at: &str, v: &'a Value) -> (&'static str, &'a Value) {
    let o = v.as_object().filter(|o| o.len() == 1).unwrap_or_else(|| {
        bad(
            at,
            "a frame is a file name, {\"hex\"}, {\"text\"} or {\"json\"}",
        )
    });
    for kind in ["hex", "text", "json"] {
        if let Some(inner) = o.get(kind) {
            if kind != "json" && !inner.is_string() {
                bad(at, format!("\"{}\" holds a string", kind));
            }
            return (kind, inner);
        }
    }
    bad(
        at,
        "a frame is a file name, {\"hex\"}, {\"text\"} or {\"json\"}",
    )
}

/// A frame a client expects: a `.json` file of the case, or the frame
/// itself.
fn expected_frame(at: &str, dir: &Path, v: &Value) -> Value {
    match v.as_str() {
        Some(name) => {
            let (_, bytes) = case_file(at, dir, name);
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| bad(at, format!("{}: not JSON: {}", name, e)))
        }
        None if v.is_object() => v.clone(),
        None => bad(at, "\"frame\" is a file name or the frame"),
    }
}
