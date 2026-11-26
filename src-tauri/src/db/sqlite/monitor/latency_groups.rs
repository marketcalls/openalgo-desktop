//! Which endpoint a latency record is for, and which group that endpoint
//! is in (web `database/latency_groups.py`, web c46a4b19a).
//!
//! Every `/api/v1` call is timed and stored in `order_latency` with a short
//! tag in its `order_type` column. The Latency page used to put all of them
//! into one average, one P99 and one "under 150 ms" figure on a page that
//! called them orders, so a chart loading its history decided what a trader
//! read as order speed (web issues #2421, #2286). This is the one place that
//! says what each tag is and where it belongs; the recorder
//! (`services::monitor::api_type`), the retention purge and the Latency
//! report all read it, and the page keeps no list of its own.
//!
//! The groups follow the sections of `docs/api/README.md`, apart where a
//! latency figure needs calls that cost roughly the same: the GTT order book
//! sits with the other books, and the symbol lookups answered from
//! OpenAlgo's own data are apart from the broker's market data.
//!
//! **Orders are the group that is kept**: order records stay (up to the
//! row cap) and every other record goes after [`RETENTION_DAYS`], so
//! [`order_tags`] is also the keep list.
//!
//! Desktop: an endpoint is named by its `/api/v1` path, not by a Flask-RESTX
//! namespace (`docs/api` paths: `placeorder`, `optiongreeks`), and a path
//! with more segments (`strategy/list`, `ticker/NSE:SBIN`) by its first
//! segment unless the whole path is listed (`market/holidays`). The tags and
//! labels are the web's. Adding an `/api/v1` route without giving it an
//! [`Endpoint`] here fails `every_api_v1_route_is_in_a_group`.
//!
//! No I/O here.

/// One kind of call: the tag it is stored under and the name a trader reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub tag: &'static str,
    pub label: &'static str,
    /// The `/api/v1` paths (first segment, or the whole path) recorded
    /// under this tag.
    pub paths: &'static [&'static str],
}

/// Endpoints whose latency can sensibly be read together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub endpoints: &'static [Endpoint],
    /// Whether the Excellent to Slow scale applies. It was drawn up for an
    /// order confirmation and says nothing about a history download.
    pub rated: bool,
    /// Whether these records are kept. Everything else goes after
    /// [`RETENTION_DAYS`].
    pub kept: bool,
}

/// How long a record outside the kept group stays.
pub const RETENTION_DAYS: i64 = 7;

pub const ORDERS: &str = "orders";
pub const ALL: &str = "all";
pub const OTHER: &str = "other";
pub const DEFAULT_GROUP: &str = ORDERS;

const fn ep(tag: &'static str, label: &'static str, paths: &'static [&'static str]) -> Endpoint {
    Endpoint { tag, label, paths }
}

pub const GROUPS: &[Group] = &[
    Group {
        key: ORDERS,
        label: "Orders",
        description: "Placing, changing and cancelling orders. These are the calls a trade waits on.",
        rated: true,
        kept: true,
        endpoints: &[
            ep("PLACE", "Place order", &["placeorder"]),
            ep("SMART", "Smart order", &["placesmartorder"]),
            ep("OPTIONS", "Options order", &["optionsorder"]),
            ep("OPTIONS_MULTI", "Options multi-leg order", &["optionsmultiorder"]),
            ep("BASKET", "Basket order", &["basketorder"]),
            ep("SPLIT", "Split order", &["splitorder"]),
            ep("MODIFY", "Modify order", &["modifyorder"]),
            ep("CANCEL", "Cancel order", &["cancelorder"]),
            ep("CANCEL_ALL", "Cancel all orders", &["cancelallorder"]),
            ep("CLOSE", "Close position", &["closeposition"]),
            ep("GTT_PLACE", "Place GTT", &["placegttorder"]),
            ep("GTT_MODIFY", "Modify GTT", &["modifygttorder"]),
            ep("GTT_CANCEL", "Cancel GTT", &["cancelgttorder"]),
        ],
    },
    Group {
        key: "account",
        label: "Account and books",
        description: "Order status, positions, holdings, funds and the order and trade books.",
        rated: false,
        kept: false,
        endpoints: &[
            ep("STATUS", "Order status", &["orderstatus"]),
            ep("POSITION", "Open position", &["openposition"]),
            ep("ORDERBOOK", "Order book", &["orderbook"]),
            ep("TRADEBOOK", "Trade book", &["tradebook"]),
            ep("POSITIONBOOK", "Position book", &["positionbook"]),
            ep("HOLDINGS", "Holdings", &["holdings"]),
            ep("FUNDS", "Funds", &["funds"]),
            ep("MARGIN", "Margin", &["margin"]),
            ep("CHARGES", "Charges", &["charges"]),
            ep("GTT_ORDERBOOK", "GTT order book", &["gttorderbook"]),
        ],
    },
    Group {
        key: "market_data",
        label: "Market data",
        description: "Quotes, depth and price history. A history call returns a large response and is expected to be the slowest.",
        rated: false,
        kept: false,
        endpoints: &[
            ep("QUOTES", "Quote", &["quotes"]),
            ep("MULTIQUOTES", "Multiple quotes", &["multiquotes"]),
            ep("DEPTH", "Market depth", &["depth"]),
            ep("HISTORY", "History", &["history"]),
            ep("TICKER", "Ticker", &["ticker"]),
        ],
    },
    Group {
        key: "options",
        label: "Options",
        description: "Option chain, Greeks and option symbol lookups.",
        rated: false,
        kept: false,
        endpoints: &[
            ep("OPTIONCHAIN", "Option chain", &["optionchain"]),
            ep("OPTIONGREEKS", "Option Greeks", &["optiongreeks"]),
            ep("MULTIOPTIONGREEKS", "Batch option Greeks", &["multioptiongreeks"]),
            ep("OPTIONSYMBOL", "Option symbol", &["optionsymbol"]),
            ep("SYNTHETICFUTURE", "Synthetic future", &["syntheticfuture"]),
        ],
    },
    Group {
        key: "reference",
        label: "Symbols and settings",
        description: "Symbol search, expiry dates, the market calendar and settings. These are answered from OpenAlgo's own data.",
        rated: false,
        kept: false,
        endpoints: &[
            ep("SYMBOL", "Symbol", &["symbol"]),
            ep("SEARCH", "Symbol search", &["search"]),
            ep("EXPIRY", "Expiry dates", &["expiry"]),
            ep("INSTRUMENTS", "Instruments", &["instruments"]),
            ep("INTERVALS", "Intervals", &["intervals"]),
            ep("MARKET_HOLIDAYS", "Market holidays", &["market/holidays"]),
            ep("MARKET_TIMINGS", "Market timings", &["market/timings"]),
            ep("ANALYZER", "Analyzer mode", &["analyzer"]),
            ep("PNL", "Sandbox P&L", &["pnl"]),
            ep("CHART", "Chart preferences", &["chart"]),
            ep("PING", "Ping", &["ping"]),
        ],
    },
    Group {
        key: "tools",
        label: "Tools and alerts",
        description: "Portfolio, SIP and sector analysis, the strategy module and alert messages.",
        rated: false,
        kept: false,
        endpoints: &[
            // The web's Portfolio and SIP endpoints are out of scope here
            // (pandas backtesters); their tags stay so the groups read alike.
            ep("PORTFOLIO", "Portfolio analysis", &["portfolio"]),
            ep("SIP", "SIP backtest", &["sip"]),
            ep("SECTORROTATION", "Sector rotation", &["sectorrotation"]),
            ep("STRATEGY", "Strategy module", &["strategy"]),
            ep("TELEGRAM", "Telegram", &["telegram"]),
            ep("WHATSAPP", "WhatsApp", &["whatsapp"]),
            ep("SLACK", "Slack", &["slack"]),
            ep("DISCORD", "Discord", &["discord"]),
        ],
    },
];

/// A record whose tag is in no group. Shown only when there is one.
pub const OTHER_GROUP: Group = Group {
    key: OTHER,
    label: "Other",
    description: "Calls that are in none of the groups above.",
    endpoints: &[],
    rated: false,
    kept: false,
};

/// Every record, whatever it is. Not rated: the scale means nothing across
/// unlike calls.
pub const ALL_GROUP: Group = Group {
    key: ALL,
    label: "All calls",
    description: "Every recorded call together. Orders and data calls take very different times, so read the groups for a figure you can act on.",
    endpoints: &[],
    rated: false,
    kept: false,
};

/// Tags rows were written under before the groups, read as what they are.
/// The first two are the web's (its old table missed the options orders'
/// namespaces); the rest are the desktop's own earlier spellings: the
/// options tools (`GREEKS`, `MULTI_GREEKS`, `OPTION_SYMBOL`, `SYNTHETIC`)
/// and the GTT order book (`GTTORDERBOOK`, its path upper-cased, where the
/// web already wrote `GTT_ORDERBOOK`). Without them a week of rows stored
/// before an update would read as Other until they are purged.
pub const LEGACY_TAGS: &[(&str, &str)] = &[
    ("OPTIONSORDER", "OPTIONS"),
    ("OPTIONSMULTIORDER", "OPTIONS_MULTI"),
    ("GREEKS", "OPTIONGREEKS"),
    ("MULTI_GREEKS", "MULTIOPTIONGREEKS"),
    ("OPTION_SYMBOL", "OPTIONSYMBOL"),
    ("SYNTHETIC", "SYNTHETICFUTURE"),
    ("GTTORDERBOOK", "GTT_ORDERBOOK"),
];

fn every_group() -> impl Iterator<Item = &'static Group> {
    GROUPS.iter().chain([&OTHER_GROUP, &ALL_GROUP])
}

fn endpoints() -> impl Iterator<Item = (&'static Group, &'static Endpoint)> {
    GROUPS
        .iter()
        .flat_map(|g| g.endpoints.iter().map(move |e| (g, e)))
}

/// The tag a call to `/api/v1/<path>` is stored under, if its path is in a
/// group: the whole path if listed, else its first segment.
pub fn tag_for_path(path: &str) -> Option<&'static str> {
    let first = path.split('/').next().unwrap_or(path);
    [path, first].into_iter().find_map(|p| {
        endpoints()
            .find(|(_, e)| e.paths.contains(&p))
            .map(|(_, e)| e.tag)
    })
}

/// The tag a stored value stands for.
pub fn canonical(tag: &str) -> &str {
    LEGACY_TAGS
        .iter()
        .find(|(old, _)| *old == tag)
        .map_or(tag, |(_, new)| new)
}

/// Every tag in a group or a legacy spelling of one.
pub fn known_tags() -> Vec<&'static str> {
    endpoints()
        .map(|(_, e)| e.tag)
        .chain(LEGACY_TAGS.iter().map(|(old, _)| *old))
        .collect()
}

/// Every stored value that belongs to a group, the legacy spellings
/// included. Empty for [`ALL`] and [`OTHER`], which no list of tags defines.
pub fn tags_in(group_key: &str) -> Vec<&'static str> {
    let Some(group) = get_group(group_key) else {
        return Vec::new();
    };
    let own: Vec<&'static str> = group.endpoints.iter().map(|e| e.tag).collect();
    let legacy = LEGACY_TAGS
        .iter()
        .filter(|(_, new)| own.contains(new))
        .map(|(old, _)| *old);
    own.iter().copied().chain(legacy).collect()
}

/// The records that are kept: the orders group (web `ORDER_TAGS`, also
/// `KEEP_FOREVER_TYPES`).
pub fn order_tags() -> Vec<&'static str> {
    tags_in(ORDERS)
}

pub fn get_group(group_key: &str) -> Option<&'static Group> {
    every_group().find(|g| g.key == group_key)
}

/// The group a request names (`group`, any case and spacing; `default`
/// when it names none), or the refusal for a name that is no group (web
/// `requested_group`). An unknown name is refused rather than answered
/// with the default: figures for the wrong group under the right heading
/// are what the page used to get wrong.
pub fn requested(raw: Option<&str>, default: &'static str) -> Result<&'static str, String> {
    let asked = raw.map(|r| r.trim().to_lowercase()).unwrap_or_default();
    let asked = if asked.is_empty() {
        default
    } else {
        asked.as_str()
    };
    match group_keys().find(|k| *k == asked) {
        Some(key) => Ok(key),
        None => {
            let choices: Vec<&str> = every_group().map(|g| g.label).collect();
            Err(format!(
                "That is not a latency group. The groups are: {}.",
                choices.join(", ")
            ))
        }
    }
}

/// Every key a caller may ask for, in the order the page shows them.
pub fn group_keys() -> impl Iterator<Item = &'static str> {
    every_group().map(|g| g.key)
}

/// The group a stored tag belongs to, [`OTHER`] for one in none (and for a
/// record with no tag).
pub fn group_key_of(tag: Option<&str>) -> &'static str {
    tag.map(canonical)
        .and_then(|t| endpoints().find(|(_, e)| e.tag == t))
        .map_or(OTHER, |(g, _)| g.key)
}

/// What to call a stored tag on the page.
pub fn label_of(tag: Option<&str>) -> String {
    match tag {
        Some(t) => endpoints()
            .find(|(_, e)| e.tag == canonical(t))
            .map_or_else(|| t.to_string(), |(_, e)| e.label.to_string()),
        None => "Unknown".into(),
    }
}

/// Position of each tag in its group, for listing endpoints in a fixed
/// order; for [`ALL`] and [`OTHER`] every tag's position across the groups.
pub fn endpoint_order(group_key: &str) -> Vec<&'static str> {
    match get_group(group_key) {
        Some(g) if !g.endpoints.is_empty() => g.endpoints.iter().map(|e| e.tag).collect(),
        _ => endpoints().map(|(_, e)| e.tag).collect(),
    }
}

#[cfg(test)]
mod tests;
