//! The web's rate limit on each `/api/v1` endpoint, and the window each
//! one counts in.
//!
//! Flask-Limiter scopes a route's `@limiter.limit` by the route's endpoint
//! (`RuntimeLimit.scope_for` in Flask-Limiter 4.1, the web's pin), so every
//! endpoint has its own window per caller: ten places and ten cancels in one
//! second are two windows of ten, not one of twenty (docs-19). The limits
//! are the settings the web's decorators read (`restx_api/*.py`) at the
//! values `.sample.env` gives them, or the decorator's own default where
//! `.sample.env` has none.

/// The web's limit on an `/api/v1` endpoint, named by the web setting its
/// decorator reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiRate {
    /// `API_RATE_LIMIT`, 100 per second (`.sample.env`): every endpoint
    /// not listed below, the strategy routes and margin included.
    Api,
    /// `ORDER_RATE_LIMIT`, 10 per second: place, modify and cancel, the
    /// options orders and the GTT writes.
    Order,
    /// `SMART_ORDER_RATE_LIMIT`, 10 per second.
    SmartOrder,
    /// `GREEKS_RATE_LIMIT`, 30 per minute (`optiongreeks`; not in
    /// `.sample.env`, so the decorator's default).
    Greeks,
    /// `TELEGRAM_RATE_LIMIT`, 30 per minute (the Telegram routes but the
    /// broadcast and the webhook).
    Telegram,
    /// 5 per minute (`telegram/broadcast`): a script cannot flood the
    /// linked chats.
    TelegramBroadcast,
    /// `WHATSAPP_RATE_LIMIT`, 30 per minute (`whatsapp/notify`).
    WhatsApp,
}

impl ApiRate {
    /// Requests allowed per window, and the window, in seconds.
    pub(super) fn limit(self) -> (usize, u64) {
        match self {
            ApiRate::Api => (100, 1),
            ApiRate::Order | ApiRate::SmartOrder => (10, 1),
            ApiRate::Greeks | ApiRate::Telegram | ApiRate::WhatsApp => (30, 60),
            ApiRate::TelegramBroadcast => (5, 60),
        }
    }
}

/// One `/api/v1` endpoint's own window: the route, and the web's limit on
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Endpoint {
    /// The route below `/api/v1/` (`placeorder`, `telegram/notify`;
    /// `ticker` for every symbol, as the web's one `/<symbol>` route).
    name: &'static str,
    rate: ApiRate,
}

impl Endpoint {
    pub(super) fn rate(&self) -> ApiRate {
        self.rate
    }
}

/// Every `/api/v1` route the desktop serves, with the web's limit. The
/// Telegram webhook has no limit on the web; it keeps the general one here
/// as a resource guard (Telegram sends a handful of updates a second).
const ENDPOINTS: &[(&str, ApiRate)] = &[
    ("ping", ApiRate::Api),
    ("analyzer", ApiRate::Api),
    ("analyzer/toggle", ApiRate::Api),
    ("market/holidays", ApiRate::Api),
    ("market/timings", ApiRate::Api),
    ("funds", ApiRate::Api),
    ("orderbook", ApiRate::Api),
    ("tradebook", ApiRate::Api),
    ("positionbook", ApiRate::Api),
    ("holdings", ApiRate::Api),
    ("orderstatus", ApiRate::Api),
    ("openposition", ApiRate::Api),
    ("pnl/symbols", ApiRate::Api),
    ("placeorder", ApiRate::Order),
    ("placesmartorder", ApiRate::SmartOrder),
    ("modifyorder", ApiRate::Order),
    ("cancelorder", ApiRate::Order),
    ("cancelallorder", ApiRate::Api),
    ("closeposition", ApiRate::Api),
    ("basketorder", ApiRate::Api),
    ("splitorder", ApiRate::Api),
    ("optionsorder", ApiRate::Order),
    ("optionsmultiorder", ApiRate::Order),
    ("placegttorder", ApiRate::Order),
    ("modifygttorder", ApiRate::Order),
    ("cancelgttorder", ApiRate::Order),
    ("gttorderbook", ApiRate::Api),
    ("quotes", ApiRate::Api),
    ("multiquotes", ApiRate::Api),
    ("depth", ApiRate::Api),
    ("history", ApiRate::Api),
    ("intervals", ApiRate::Api),
    ("ticker", ApiRate::Api),
    ("symbol", ApiRate::Api),
    ("search", ApiRate::Api),
    ("expiry", ApiRate::Api),
    ("instruments", ApiRate::Api),
    ("margin", ApiRate::Api),
    ("optionsymbol", ApiRate::Api),
    ("optionchain", ApiRate::Api),
    ("syntheticfuture", ApiRate::Api),
    ("optiongreeks", ApiRate::Greeks),
    ("multioptiongreeks", ApiRate::Api),
    ("chart", ApiRate::Api),
    ("telegram/notify", ApiRate::Telegram),
    ("telegram/config", ApiRate::Telegram),
    ("telegram/start", ApiRate::Telegram),
    ("telegram/stop", ApiRate::Telegram),
    ("telegram/users", ApiRate::Telegram),
    ("telegram/broadcast", ApiRate::TelegramBroadcast),
    ("telegram/stats", ApiRate::Telegram),
    ("telegram/preferences", ApiRate::Telegram),
    ("telegram/webhook", ApiRate::Api),
    ("whatsapp/notify", ApiRate::WhatsApp),
    ("strategy/list", ApiRate::Api),
    ("strategy/status", ApiRate::Api),
    ("strategy/start", ApiRate::Api),
    ("strategy/stop", ApiRate::Api),
    ("strategy/close_all", ApiRate::Api),
    ("strategy/close_leg", ApiRate::Api),
    ("strategy/runs", ApiRate::Api),
    ("strategy/orders", ApiRate::Api),
    ("strategy/events", ApiRate::Api),
];

/// A path the router does not serve. The rate layer runs on matched
/// routes only, so nothing counts here in practice; one shared window
/// keeps the key set fixed whatever path is asked about.
const UNLISTED: Endpoint = Endpoint {
    name: "",
    rate: ApiRate::Api,
};

/// The endpoint an `/api/v1` request path counts against. The name comes
/// from the fixed table, never from the request, so the limiter's keys
/// stay a fixed set.
pub(super) fn endpoint_for(path: &str) -> Endpoint {
    let p = path.trim_start_matches("/api/v1/").trim_end_matches('/');
    let p = if p.starts_with("ticker/") {
        "ticker"
    } else {
        p
    };
    ENDPOINTS
        .iter()
        .find(|(name, _)| *name == p)
        .map(|&(name, rate)| Endpoint { name, rate })
        .unwrap_or(UNLISTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every web `/api/v1` route the desktop serves has its own entry
    /// (`tests/fixtures/web/route_inventory.json`, read from the web's
    /// source), so none falls into the unlisted window.
    #[test]
    fn every_web_route_has_its_own_endpoint() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/web/route_inventory.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut seen = 0;
        for route in doc["routes"].as_array().unwrap() {
            let (_, path) = route.as_str().unwrap().split_once(' ').unwrap();
            // The pandas backtesters are out of scope (api_v1_inventory).
            if !path.starts_with("/api/v1/")
                || path.starts_with("/api/v1/portfolio/")
                || path.starts_with("/api/v1/sip/")
            {
                continue;
            }
            let path = path.replace("<string:symbol>", "NSE:SBIN");
            assert_ne!(endpoint_for(&path), UNLISTED, "{}", path);
            seen += 1;
        }
        assert!(seen > 60, "read {} routes", seen);
    }

    #[test]
    fn a_path_is_one_endpoint_whatever_its_spelling() {
        assert_eq!(
            endpoint_for("/api/v1/placeorder/"),
            endpoint_for("/api/v1/placeorder")
        );
        assert_eq!(
            endpoint_for("/api/v1/ticker/NSE:SBIN"),
            endpoint_for("/api/v1/ticker/NFO:NIFTY28OCT2625000CE"),
            "the web's one ticker route"
        );
        assert_ne!(
            endpoint_for("/api/v1/placeorder"),
            endpoint_for("/api/v1/cancelorder")
        );
        assert_eq!(endpoint_for("/api/v1/nosuchroute"), UNLISTED);
    }

    /// The limits the web's decorators set (docs-1: optiongreeks and
    /// WhatsApp were counted at the general limit).
    #[test]
    fn limits_are_the_webs() {
        let rate = |p: &str| endpoint_for(p).rate();
        assert_eq!(rate("/api/v1/placeorder"), ApiRate::Order);
        assert_eq!(rate("/api/v1/cancelgttorder"), ApiRate::Order);
        assert_eq!(rate("/api/v1/placesmartorder"), ApiRate::SmartOrder);
        assert_eq!(rate("/api/v1/basketorder"), ApiRate::Api);
        assert_eq!(rate("/api/v1/optiongreeks"), ApiRate::Greeks);
        assert_eq!(rate("/api/v1/multioptiongreeks"), ApiRate::Api);
        assert_eq!(rate("/api/v1/whatsapp/notify"), ApiRate::WhatsApp);
        assert_eq!(rate("/api/v1/telegram/notify"), ApiRate::Telegram);
        assert_eq!(
            rate("/api/v1/telegram/broadcast"),
            ApiRate::TelegramBroadcast
        );
        assert_eq!(rate("/api/v1/strategy/start"), ApiRate::Api);
        assert_eq!(ApiRate::Greeks.limit(), (30, 60));
        assert_eq!(ApiRate::WhatsApp.limit(), (30, 60));
        assert_eq!(ApiRate::Order.limit(), (10, 1));
        assert_eq!(ApiRate::Api.limit(), (100, 1));
    }
}
